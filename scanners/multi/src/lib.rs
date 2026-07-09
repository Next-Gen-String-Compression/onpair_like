//! Multi-pattern scanners (DESIGN.md §16): Aho-Corasick and Teddy, both via
//! the `aho-corasick` crate — the canonical Rust homes of the classic
//! automaton and of Hyperscan's SIMD-fingerprint packed matcher.
//!
//! Scope: `contains` (one literal) and `contains_any` (OR of literals). The
//! order-sensitive `multi_contains` is not these engines' model and is left
//! to memmem's sequential loop; `prefix`/`suffix` are direct compares.
//!
//! `aho-corasick` handles any literal set, including an empty needle (which
//! matches every row per SEMANTICS — a fast all-set path). `teddy` (the
//! packed searcher) has a bounded envelope: it declines empty needles via
//! `supports_query` and, if the packed builder still cannot compile a set on
//! this host, `prepare()` fails and the cell is recorded as errored.
//!
//! Portable by declaration (`cpu_features` NULL): the packed module
//! dispatches internally (SSSE3/AVX2 on x86, NEON on aarch64), the same
//! internal-dispatch stance as memmem.

use core::ffi::c_void;

use aho_corasick::{packed, AhoCorasick};
use lb_abi::*;

const OPS: u32 = op_bit(LB_CONTAINS) | op_bit(LB_CONTAINS_ANY);

enum Engine {
    /// An empty needle is present: every row matches (SEMANTICS edge case).
    All,
    Ac(AhoCorasick),
    Teddy(packed::Searcher),
}

struct Prepared {
    engine: Engine,
}

fn owned_needles(q: &LbQuery) -> Vec<Vec<u8>> {
    unsafe { q.needles_vec() }.iter().map(|n| n.to_vec()).collect()
}

unsafe extern "C" fn prepare_ac(query: *const LbQuery) -> *mut c_void {
    let q = &*query;
    let needles = owned_needles(q);
    if needles.is_empty() {
        return core::ptr::null_mut();
    }
    let engine = if needles.iter().any(|n| n.is_empty()) {
        Engine::All
    } else {
        match AhoCorasick::new(&needles) {
            Ok(ac) => Engine::Ac(ac),
            Err(_) => return core::ptr::null_mut(),
        }
    };
    Box::into_raw(Box::new(Prepared { engine })) as *mut c_void
}

unsafe extern "C" fn prepare_teddy(query: *const LbQuery) -> *mut c_void {
    let q = &*query;
    let needles = owned_needles(q);
    // Empty needles are rejected by supports_query; guard anyway.
    if needles.is_empty() || needles.iter().any(|n| n.is_empty()) {
        return core::ptr::null_mut();
    }
    let mut builder = packed::Builder::new();
    for n in &needles {
        builder.add(n.as_slice());
    }
    match builder.build() {
        Some(s) => Box::into_raw(Box::new(Prepared {
            engine: Engine::Teddy(s),
        })) as *mut c_void,
        None => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn scan(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    _stats: *mut LbRunStats,
) -> i32 {
    let p = &*(prepared as *const Prepared);
    let v = &*view;
    let words =
        core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();
    // Engine dispatch is hoisted out of the row loop: constant per scan.
    match &p.engine {
        Engine::All => {
            for i in 0..v.num_rows as usize {
                set_bit(words, i);
            }
        }
        Engine::Ac(ac) => {
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if ac.is_match(row) {
                    set_bit(words, i);
                }
            }
        }
        Engine::Teddy(s) => {
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if s.find(row).is_some() {
                    set_bit(words, i);
                }
            }
        }
    }
    0
}

unsafe extern "C" fn release(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut Prepared));
}

/// Teddy fingerprints require non-empty literals; an empty needle (matches
/// everything) is outside its model.
unsafe extern "C" fn teddy_supports(query: *const LbQuery) -> i32 {
    let q = &*query;
    if q.needles_vec().iter().any(|n| n.is_empty()) {
        0
    } else {
        1
    }
}

static VT_AC: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"aho_corasick".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: OPS,
    prepare: Some(prepare_ac),
    scan: Some(scan),
    release: Some(release),
    supports_query: None,
};

static VT_TEDDY: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"teddy".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: OPS,
    prepare: Some(prepare_teddy),
    scan: Some(scan),
    release: Some(release),
    supports_query: Some(teddy_supports),
};

pub fn vtables() -> [&'static LbScanner; 2] {
    [&VT_AC, &VT_TEDDY]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Validates the exact aho-corasick API the scanners depend on.
    #[test]
    fn ac_and_teddy_agree_on_presence() {
        let needles: [&[u8]; 2] = [b"foo", b"bar"];

        let ac = AhoCorasick::new(needles).expect("build ac");
        assert!(ac.is_match(b"a foo b".as_slice()));
        assert!(ac.is_match(b"zzbarzz".as_slice()));
        assert!(!ac.is_match(b"nothing here".as_slice()));

        let mut builder = packed::Builder::new();
        for n in &needles {
            builder.add(n);
        }
        let searcher = builder.build().expect("packed searcher builds on this host");
        assert!(searcher.find(b"a bar b".as_slice()).is_some());
        assert!(searcher.find(b"nope".as_slice()).is_none());
    }
}
