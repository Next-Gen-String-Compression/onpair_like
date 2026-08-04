//! `uncompressed_memmem` — the plaintext memchr baseline as a self-contained
//! candidate. This is the fast `memchr::memmem` engine (the same one the
//! `memmem` scanner uses) promoted to a candidate with its own `run` strategy,
//! so it stands alone in the matrix as `uncompressed_memmem` rather than as an
//! (uncompressed × memmem × direct) composition.
//!
//! Storage is zero-copy (build retains the view; compression ratio 1.0). The
//! per-op match logic mirrors the `memmem` scanner exactly: per-needle
//! `Finder`s for contains-family, bounds-checked compares for prefix/suffix.

use core::ffi::{c_char, c_void};

use lb_abi::*;
use memchr::memmem::Finder;

struct Handle {
    view: LbChunkView,
}

unsafe extern "C" fn build(
    view: *const LbChunkView,
    _config_json: *const c_char,
    _err_buf: *mut c_char,
    _err_cap: u64,
) -> *mut c_void {
    // Zero-copy: the harness guarantees the view outlives destroy().
    Box::into_raw(Box::new(Handle { view: *view })) as *mut c_void
}

unsafe extern "C" fn footprint(
    this: *mut c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32 {
    let h = &*(this as *mut Handle);
    let offsets = h.view.offsets_slice();
    let components = [
        LbFootprintComponent::new("payload", offsets[h.view.num_rows as usize]),
        LbFootprintComponent::new("offsets", 8 * (h.view.num_rows + 1)),
    ];
    for (i, c) in components.iter().take(capacity as usize).enumerate() {
        *out.add(i) = *c;
    }
    components.len() as u32
}

unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    _stats_or_null: *mut LbRunStats,
) -> i32 {
    if strategy_index != 0 {
        return 10;
    }
    let h = &*(this as *mut Handle);
    let q = &*query;
    let needles = q.needles_vec();
    let v = &h.view;
    let words = core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();

    // Compile needle Finders once (per-query setup), then loop the rows.
    match q.op {
        LB_PREFIX => {
            let n = needles[0];
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if row.len() >= n.len() && &row[..n.len()] == n {
                    set_bit(words, i);
                }
            }
        }
        LB_SUFFIX => {
            let n = needles[0];
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if row.len() >= n.len() && &row[row.len() - n.len()..] == n {
                    set_bit(words, i);
                }
            }
        }
        LB_CONTAINS => {
            let f = Finder::new(needles[0]);
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if f.find(row).is_some() {
                    set_bit(words, i);
                }
            }
        }
        LB_MULTI_CONTAINS => {
            let fragments: Vec<(Finder, usize)> =
                needles.iter().map(|n| (Finder::new(n), n.len())).collect();
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                let mut pos = 0usize;
                let mut ok = true;
                for (f, len) in &fragments {
                    match f.find(&row[pos..]) {
                        Some(rel) => pos += rel + len,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    set_bit(words, i);
                }
            }
        }
        LB_CONTAINS_ANY => {
            let finders: Vec<Finder> = needles.iter().map(|n| Finder::new(n)).collect();
            for i in 0..v.num_rows as usize {
                let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if finders.iter().any(|f| f.find(row).is_some()) {
                    set_bit(words, i);
                }
            }
        }
        _ => return 11,
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 1] = [LbStrategy {
    name: c"memmem".as_ptr(),
    supported_ops: LB_ALL_OPS,
}];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"uncompressed_memmem".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 1,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: None,
    destroy: Some(destroy),
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
