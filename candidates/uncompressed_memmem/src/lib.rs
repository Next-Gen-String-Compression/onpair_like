//! `uncompressed_memmem` — the plaintext memchr baselines as a self-contained
//! candidate. These are the fast `memchr::memmem` engines (the same ones the
//! `memmem` and `memmem-hay` scanners use) promoted to candidate strategies,
//! so they stand alone in a mixed native/composed benchmark matrix.
//!
//! Storage is zero-copy (build retains the view; compression ratio 1.0). The
//! `memmem` searches each row separately and supports every operation;
//! `memmem-hay` makes one pass over the concatenated payload for `contains`
//! and rejects occurrences that cross a row boundary.

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
    if strategy_index > 1 {
        return 10;
    }
    let h = &*(this as *mut Handle);
    let q = &*query;
    let needles = q.needles_vec();
    let v = &h.view;
    let words = core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();

    if strategy_index == 1 {
        if q.op != LB_CONTAINS {
            return 11;
        }
        let needle = needles[0];
        if needle.is_empty() {
            for i in 0..v.num_rows as usize {
                set_bit(words, i);
            }
            return 0;
        }

        let finder = Finder::new(needle);
        let mut start = 0usize;
        while let Some(relative) = finder.find(&payload[start..]) {
            let position = start + relative;
            // Find the row containing `position`. The suite never supplies an
            // empty needle here, so a hit always names a payload byte.
            let row = offsets.partition_point(|&offset| offset as usize <= position) - 1;
            if position + needle.len() <= offsets[row + 1] as usize {
                set_bit(words, row);
            }
            // Preserve overlapping hits. A rejected boundary-spanning match
            // may overlap a valid occurrence at the next byte.
            start = position + 1;
        }
        return 0;
    }

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

static STRATEGIES: [LbStrategy; 2] = [
    LbStrategy {
        name: c"memmem".as_ptr(),
        supported_ops: LB_ALL_OPS,
    },
    LbStrategy {
        name: c"memmem-hay".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
    },
];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"uncompressed_memmem".as_ptr(),
    version: c"0.2.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: STRATEGIES.len() as u32,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: None,
    destroy: Some(destroy),
    query_facts: None,
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haystack_strategy_rejects_cross_row_matches() {
        let payload = b"xaab";
        let offsets = [0u64, 2, 4];
        let view = LbChunkView {
            bytes: payload.as_ptr(),
            offsets: offsets.as_ptr(),
            num_rows: 2,
        };
        let handle = unsafe { build(&view, c"{}".as_ptr(), core::ptr::null_mut(), 0) };

        let needle = b"aa";
        let needle_ffi = LbBytes {
            ptr: needle.as_ptr(),
            len: needle.len() as u64,
        };
        let query = LbQuery {
            op: LB_CONTAINS,
            needles: &needle_ffi,
            needle_count: 1,
        };
        let mut bitmap = [0u64; 1];
        assert_eq!(
            unsafe {
                run(
                    handle,
                    1,
                    &query,
                    bitmap.as_mut_ptr(),
                    core::ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(bitmap, [0]);
        unsafe { destroy(handle) };
    }
}
