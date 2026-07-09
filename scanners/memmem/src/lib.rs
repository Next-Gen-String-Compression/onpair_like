//! The `memmem` scanner (DESIGN.md §11 step 6): the phase-1 workhorse.
//!
//! `memchr::memmem` SIMD substring kernels for contains-family ops (with
//! per-needle `Finder`s compiled in prepare()), bounds-checked slice
//! compares for prefix/suffix, sequential memmem with position advance for
//! multi_contains, first-match-wins loop for contains_any. Aho-Corasick /
//! Teddy-style multi-pattern prefilters are future scanners, not this one.
//!
//! Portable by declaration: memchr dispatches internally (SSE2/AVX2/NEON)
//! and always has a correct path, so `cpu_features` is NULL — this is
//! internal dispatch, not the silent-scalar-fallback the platform policy
//! forbids for ISA-specific modules.
//!
//! No prefilter stage — instrumented mode reports nothing (appears as
//! "no prefilter" in results).

use core::ffi::c_void;

use lb_abi::*;
use memchr::memmem::Finder;

enum Prepared {
    Prefix(Vec<u8>),
    Suffix(Vec<u8>),
    Contains(Finder<'static>),
    /// (finder, needle length) per fragment, in order.
    Multi(Vec<(Finder<'static>, usize)>),
    Any(Vec<Finder<'static>>),
}

unsafe extern "C" fn prepare(query: *const LbQuery) -> *mut c_void {
    let q = &*query;
    let needles = q.needles_vec();
    let owned = |n: &[u8]| Finder::new(n).into_owned();
    let prepared = match q.op {
        LB_PREFIX => Prepared::Prefix(needles[0].to_vec()),
        LB_SUFFIX => Prepared::Suffix(needles[0].to_vec()),
        LB_CONTAINS => Prepared::Contains(owned(needles[0])),
        LB_MULTI_CONTAINS => {
            Prepared::Multi(needles.iter().map(|n| (owned(n), n.len())).collect())
        }
        LB_CONTAINS_ANY => Prepared::Any(needles.iter().map(|n| owned(n)).collect()),
        _ => return core::ptr::null_mut(),
    };
    Box::into_raw(Box::new(prepared)) as *mut c_void
}

unsafe extern "C" fn scan(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    _stats_or_null: *mut LbRunStats,
) -> i32 {
    let p = &*(prepared as *const Prepared);
    let v = &*view;
    let words =
        core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();

    for i in 0..v.num_rows as usize {
        let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
        let hit = match p {
            Prepared::Prefix(n) => row.len() >= n.len() && &row[..n.len()] == n.as_slice(),
            Prepared::Suffix(n) => {
                row.len() >= n.len() && &row[row.len() - n.len()..] == n.as_slice()
            }
            Prepared::Contains(f) => f.find(row).is_some(),
            Prepared::Multi(fragments) => {
                let mut pos = 0usize;
                let mut ok = true;
                for (f, len) in fragments {
                    match f.find(&row[pos..]) {
                        Some(rel) => pos += rel + len,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                ok
            }
            Prepared::Any(finders) => finders.iter().any(|f| f.find(row).is_some()),
        };
        if hit {
            set_bit(words, i);
        }
    }
    0
}

unsafe extern "C" fn release(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut Prepared));
}

static VTABLE: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"memmem".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: LB_ALL_OPS,
    prepare: Some(prepare),
    scan: Some(scan),
    release: Some(release),
    supports_query: None,
};

pub fn vtable() -> &'static LbScanner {
    &VTABLE
}

// ---------------------------------------------------------- haystack variant
//
// `memmem-hay` (DESIGN.md §16): one SIMD pass over the *concatenated payload*
// instead of a per-row loop, then map each hit back to its row. On short rows
// (msmarco-query ~36 B) the per-row loop restarts the kernel every few bytes
// and never gets a runway; the haystack pass keeps the SIMD kernel saturated
// for the whole column. A match is attributed only if it lies wholly within
// one row (`gpos + m <= offsets[r+1]`), which rejects the false positives
// that a needle straddling a row boundary would otherwise produce.
//
// `contains` only: prefix/suffix are position-anchored (no haystack win) and
// contains_any is the multi-pattern engines' territory.

struct PreparedHay {
    finder: Finder<'static>,
    needle_len: usize,
    match_all: bool,
}

unsafe extern "C" fn prepare_hay(query: *const LbQuery) -> *mut c_void {
    let q = &*query;
    if q.op != LB_CONTAINS {
        return core::ptr::null_mut();
    }
    let needles = q.needles_vec();
    if needles.is_empty() {
        return core::ptr::null_mut();
    }
    let n = needles[0];
    Box::into_raw(Box::new(PreparedHay {
        finder: Finder::new(n).into_owned(),
        needle_len: n.len(),
        match_all: n.is_empty(),
    })) as *mut c_void
}

unsafe extern "C" fn scan_hay(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    _stats_or_null: *mut LbRunStats,
) -> i32 {
    let p = &*(prepared as *const PreparedHay);
    let v = &*view;
    let words =
        core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();

    if p.match_all {
        for i in 0..v.num_rows as usize {
            set_bit(words, i);
        }
        return 0;
    }
    let m = p.needle_len;
    // Advance by 1 after each hit (not by m): a match that straddles a row
    // boundary is rejected but may overlap a *valid* within-row occurrence
    // of a periodic needle (e.g. "aa" across `...a|a...`); skipping by m
    // would drop it. Advancing by 1 enumerates every occurrence; the SIMD
    // kernel still scans the gaps between hits.
    let mut start = 0usize;
    while let Some(rel) = p.finder.find(&payload[start..]) {
        let gpos = start + rel;
        // Row containing byte gpos: first offset strictly greater, minus one.
        let r = offsets.partition_point(|&o| (o as usize) <= gpos) - 1;
        if gpos + m <= offsets[r + 1] as usize {
            set_bit(words, r);
        }
        start = gpos + 1;
    }
    0
}

unsafe extern "C" fn release_hay(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut PreparedHay));
}

static VTABLE_HAY: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"memmem-hay".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: op_bit(LB_CONTAINS),
    prepare: Some(prepare_hay),
    scan: Some(scan_hay),
    release: Some(release_hay),
    supports_query: None,
};

pub fn vtable_hay() -> &'static LbScanner {
    &VTABLE_HAY
}
