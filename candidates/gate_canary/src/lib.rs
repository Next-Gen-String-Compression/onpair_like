//! The gate canary (DESIGN.md §8, §11 step 8): a candidate whose entire
//! purpose is proving the correctness gate fires. It declares
//! candidate-implemented strategies over a naive matcher:
//!
//! - `ok`    — correct; must pass every gate (also the only phase-1
//!             exerciser of the candidate-implemented run() path).
//! - `wrong` — flips row 0's bit; every cell over a non-empty chunk must
//!             fail loudly. A gate that has never fired is not known to work.
//!
//! ABI v8 adds two more, for the other thing that must be impossible — a
//! module quietly answering a pattern it cannot evaluate:
//!
//! - `like-declines` — declares `LB_LIKE` but returns 0 from `supports_query`
//!             for any pattern containing `_`. Every such cell must be
//!             recorded Unsupported, counted apart from the passes, and given
//!             no latency. This is the mechanism every compressed-domain
//!             engine will use for the shapes it cannot answer.
//! - `like-lowers` — declares `LB_LIKE`, accepts everything, and cheats
//!             exactly as the contract forbids: it drops the metacharacters
//!             and evaluates `%ab_c%` as `contains("abc")`. Every cell over a
//!             pattern where those differ must FAIL the gate. Without it,
//!             "no candidate silently transforms a pattern" would be a claim
//!             about code review rather than a property under test.
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
const STRATEGY_LIKE_DECLINES: u32 = 2;
const STRATEGY_LIKE_LOWERS: u32 = 3;

/// The forbidden transformation, in one function: throw the metacharacters
/// away and look for what is left. For `%abc%` this happens to be right,
/// which is why a canary that only ever ran on `%abc%` would prove nothing;
/// for `%ab_c%` it accepts `abc`, which the gate must catch.
fn strip_metacharacters(pattern: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        match pattern[i] {
            b'%' | b'_' => i += 1,
            b'\\' if i + 1 < pattern.len() => {
                out.push(pattern[i + 1]);
                i += 2;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Is this pattern exactly `%literal%` — a leading and a trailing `%`, and no
/// other metacharacter between them? That is the whole envelope of an engine
/// that only knows how to search for a literal substring, which is what every
/// contains-only candidate in the roster is.
fn is_plain_contains(pattern: &[u8]) -> bool {
    if pattern.len() < 2 || pattern[0] != b'%' {
        return false;
    }
    let mut i = 1;
    let mut last_was_percent = false;
    while i < pattern.len() {
        last_was_percent = false;
        match pattern[i] {
            b'\\' if i + 1 < pattern.len() => i += 2, // escaped: a literal byte
            b'_' => return false,
            b'%' => {
                // Only the final byte may be a '%'.
                if i + 1 != pattern.len() {
                    return false;
                }
                last_was_percent = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    last_was_percent
}

unsafe extern "C" fn supports_query(
    _this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
) -> i32 {
    if strategy_index != STRATEGY_LIKE_DECLINES {
        return 1;
    }
    let q = &*query;
    if q.op != LB_LIKE {
        return 1;
    }
    // Answer only what this strategy can answer exactly, and decline the
    // rest. Declining is always safe; answering wrongly never is — which is
    // precisely what the sibling `like-lowers` strategy demonstrates.
    is_plain_contains(q.needles_vec()[0]) as i32
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
    let needles = q.needles_vec();
    let words = core::slice::from_raw_parts_mut(
        out_bitmap_words,
        lb_abi::bitmap_words(h.view.num_rows),
    );

    // The two LIKE strategies never evaluate a pattern: one is only ever
    // handed patterns it declared itself able to take, the other cheats on
    // purpose. Both answer with `contains` over the stripped pattern.
    let stripped;
    let (op, needles) = if q.op == LB_LIKE
        && (strategy_index == STRATEGY_LIKE_DECLINES || strategy_index == STRATEGY_LIKE_LOWERS)
    {
        stripped = strip_metacharacters(needles[0]);
        (LB_CONTAINS, vec![stripped.as_slice()])
    } else {
        (q.op, needles)
    };

    for i in 0..h.view.num_rows {
        if matches(op, &needles, h.view.row(i as usize)) {
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

static STRATEGIES: [LbStrategy; 4] = [
    LbStrategy {
        name: c"ok".as_ptr(),
        supported_ops: LB_ALL_OPS,
    },
    LbStrategy {
        name: c"wrong".as_ptr(),
        supported_ops: LB_ALL_OPS,
    },
    // Both declare LB_LIKE and nothing else: they exist to exercise the
    // pattern path, and the literal ops are already covered above.
    LbStrategy {
        name: c"like-declines".as_ptr(),
        supported_ops: op_bit(LB_LIKE),
    },
    LbStrategy {
        name: c"like-lowers".as_ptr(),
        supported_ops: op_bit(LB_LIKE),
    },
];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"gate_canary".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 4,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: None,
    destroy: Some(destroy),
    query_facts: None,
    export_artifact: None,
    supports_query: Some(supports_query),
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
