//! `uncompressed_vectorscan` — the plaintext baseline with Vectorscan (the
//! Hyperscan fork, via `vectorscan-rs`) in place of `memchr::memmem`. Storage
//! is zero-copy, as in `uncompressed_memmem`. Contains only: the needle is
//! compiled per query as a literal block-mode database.
//!
//! Two strategies:
//!   - `vectorscan`: one `hs_scan` per row, terminated at the first match,
//!     over the selected rows, every row under `all`.
//!   - `vectorscan-hay`: one `hs_scan` over the whole payload, each match
//!     mapped to its row. Under a selection the harness ANDs the result.

use core::ffi::{c_char, c_void};

use lb_abi::*;
use vectorscan_rs::{BlockDatabase, Flag, Pattern, Scan};

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

/// The needle as a regex matching exactly its bytes.
fn literal(needle: &[u8]) -> Vec<u8> {
    needle.iter().flat_map(|b| format!("\\x{b:02x}").into_bytes()).collect()
}

unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    _stats_or_null: *mut LbRunStats,
) -> i32 {
    let h = &*(this as *mut Handle);
    let q = &*query;
    if q.op != LB_CONTAINS {
        return 11;
    }
    let needles = q.needles_vec();
    let n = needles[0];
    let v = &h.view;
    let words = core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();
    let rows = v.num_rows as usize;

    if n.is_empty() {
        (0..rows).for_each(|i| set_bit(words, i));
        return 0;
    }
    let Ok(db) = BlockDatabase::new(vec![Pattern::new(literal(n), Flag::empty(), None)]) else { return 12 };
    let Ok(mut scanner) = db.create_scanner() else { return 12 };

    match strategy_index {
        0 => {
            let mut row = |i: usize| {
                let r = &payload[offsets[i] as usize..offsets[i + 1] as usize];
                if matches!(scanner.scan(r, |_, _, _, _| Scan::Terminate), Ok(Scan::Terminate)) {
                    set_bit(words, i);
                }
            };
            let sel: Option<&[u32]> = (!q.selection.is_null()).then(|| (*q.selection).rows_slice());
            match sel {
                Some(sel) => sel.iter().for_each(|&i| row(i as usize)),
                None => (0..rows).for_each(row),
            }
        }
        1 => {
            if payload.len() > u32::MAX as usize {
                return 13;
            }
            // Matches arrive in order of end offset; `r` is the row holding the
            // last match's start, galloped forward.
            let m = n.len() as u64;
            let mut r = 0usize;
            let res = scanner.scan(payload, |_, _, to, _| {
                let start = to - m;
                let rest = &offsets[r + 1..rows + 1];
                let mut bound = 1;
                while bound < rest.len() && rest[bound - 1] <= start {
                    bound *= 2;
                }
                r += rest[..bound.min(rest.len())].partition_point(|&o| o <= start);
                if to <= offsets[r + 1] {
                    set_bit(words, r);
                }
                Scan::Continue
            });
            if res.is_err() {
                return 12;
            }
        }
        _ => return 10,
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 2] = [
    LbStrategy {
        name: c"vectorscan".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
        flags: LB_STRATEGY_TAKES_SELECTION,
    },
    LbStrategy { name: c"vectorscan-hay".as_ptr(), supported_ops: op_bit(LB_CONTAINS), flags: 0 },
];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"uncompressed_vectorscan".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 2,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: None,
    destroy: Some(destroy),
    decode_selected: None,
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
