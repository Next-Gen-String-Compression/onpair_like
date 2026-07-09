//! The gate canary (DESIGN.md §8, §11 step 8): a candidate whose entire
//! purpose is proving the correctness gate fires. It declares two
//! candidate-implemented strategies over a naive matcher:
//!
//! - `ok`    — correct; must pass every gate (also the only phase-1
//!             exerciser of the candidate-implemented run() path).
//! - `wrong` — flips row 0's bit; every cell over a non-empty chunk must
//!             fail loudly. A gate that has never fired is not known to work.
//!
//! The matcher is written here from scratch (naive loops, no memchr) so
//! the canary shares no machinery with the oracle it is judged against.

use core::ffi::{c_char, c_void};

use lb_abi::*;

struct Handle {
    view: LbChunkView,
}

fn find_from(row: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > row.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(from);
    }
    if needle.len() > row.len() - from {
        return None;
    }
    (from..=(row.len() - needle.len())).find(|&i| row[i..i + needle.len()] == *needle)
}

fn matches(op: u32, needles: &[&[u8]], row: &[u8]) -> bool {
    match op {
        LB_PREFIX => row.len() >= needles[0].len() && row[..needles[0].len()] == *needles[0],
        LB_SUFFIX => {
            row.len() >= needles[0].len() && row[row.len() - needles[0].len()..] == *needles[0]
        }
        LB_CONTAINS => find_from(row, needles[0], 0).is_some(),
        LB_MULTI_CONTAINS => {
            let mut pos = 0usize;
            for n in needles {
                match find_from(row, n, pos) {
                    Some(i) => pos = i + n.len(),
                    None => return false,
                }
            }
            true
        }
        LB_CONTAINS_ANY => needles.iter().any(|n| find_from(row, n, 0).is_some()),
        _ => false,
    }
}

unsafe extern "C" fn build(
    view: *const LbChunkView,
    _config_json: *const c_char,
    _err_buf: *mut c_char,
    _err_cap: u64,
) -> *mut c_void {
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

const STRATEGY_WRONG: u32 = 1;

unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    _stats_or_null: *mut LbRunStats,
) -> i32 {
    let h = &*(this as *mut Handle);
    let q = &*query;
    let needles = q.needles_vec();
    let words = core::slice::from_raw_parts_mut(
        out_bitmap_words,
        lb_abi::bitmap_words(h.view.num_rows),
    );
    for i in 0..h.view.num_rows {
        if matches(q.op, &needles, h.view.row(i as usize)) {
            set_bit(words, i as usize);
        }
    }
    if strategy_index == STRATEGY_WRONG && h.view.num_rows > 0 {
        words[0] ^= 1; // the deliberate off-by-one on row 0
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 2] = [
    LbStrategy {
        name: c"ok".as_ptr(),
        supported_ops: LB_ALL_OPS,
    },
    LbStrategy {
        name: c"wrong".as_ptr(),
        supported_ops: LB_ALL_OPS,
    },
];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"gate_canary".as_ptr(),
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
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
