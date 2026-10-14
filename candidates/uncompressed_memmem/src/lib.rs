//! `uncompressed_memmem` — the plaintext memchr baseline as a self-contained
//! candidate. This is the fast `memchr::memmem` engine (the same one the
//! `memmem` scanner uses) promoted to a candidate with its own `run` strategy,
//! so it stands alone in the matrix as `uncompressed_memmem` rather than as an
//! (uncompressed × memmem × direct) composition.
//!
//! Storage is zero-copy (build retains the view; compression ratio 1.0). The
//! per-op match logic mirrors the `memmem` scanner exactly: per-needle
//! `Finder`s for contains-family, bounds-checked compares for prefix/suffix.
//!
//! Three strategies:
//!   - `memmem`: the predicate per row, over the selected rows, every row
//!     under `all`.
//!   - `hay`: contains as one search over the whole payload, each hit mapped to
//!     its row (the `memmem-hay` scanner's pass). Under a selection the harness
//!     ANDs the result: the no-pushdown baseline.
//!   - `row-hay`: contains under a selection, `memmem` below ROW_HAY_SHARE of
//!     the rows selected and `hay` ANDed with the selection from there on.

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

/// One row's predicate, compiled once per query.
enum Pred<'a> {
    Prefix(&'a [u8]),
    Suffix(&'a [u8]),
    Contains(Finder<'a>),
    /// (finder, needle length) per fragment, in order.
    Multi(Vec<(Finder<'a>, usize)>),
    Any(Vec<Finder<'a>>),
}

impl<'a> Pred<'a> {
    fn new(op: u32, needles: &'a [&'a [u8]]) -> Option<Self> {
        Some(match op {
            LB_PREFIX => Pred::Prefix(needles[0]),
            LB_SUFFIX => Pred::Suffix(needles[0]),
            LB_CONTAINS => Pred::Contains(Finder::new(needles[0])),
            LB_MULTI_CONTAINS => Pred::Multi(needles.iter().map(|n| (Finder::new(n), n.len())).collect()),
            LB_CONTAINS_ANY => Pred::Any(needles.iter().map(|n| Finder::new(n)).collect()),
            _ => return None,
        })
    }

    #[inline]
    fn hit(&self, row: &[u8]) -> bool {
        match self {
            Pred::Prefix(n) => row.starts_with(n),
            Pred::Suffix(n) => row.ends_with(n),
            Pred::Contains(f) => f.find(row).is_some(),
            Pred::Multi(fragments) => {
                let mut pos = 0usize;
                for (f, len) in fragments {
                    match f.find(&row[pos..]) {
                        Some(rel) => pos += rel + len,
                        None => return false,
                    }
                }
                true
            }
            Pred::Any(finders) => finders.iter().any(|f| f.find(row).is_some()),
        }
    }
}

/// `memchr` over the whole payload, each hit mapped to its row.
fn hay_all(finder: &Finder<'_>, m: usize, payload: &[u8], offsets: &[u64], words: &mut [u64]) {
    let mut r = 0usize;
    let mut at = 0usize;
    while let Some(rel) = finder.find(&payload[at..]) {
        let gpos = at + rel;
        let rest = &offsets[r + 1..];
        let mut bound = 1;
        while bound < rest.len() && rest[bound - 1] as usize <= gpos {
            bound *= 2;
        }
        r += rest[..bound.min(rest.len())].partition_point(|&o| o as usize <= gpos);
        let row_end = offsets[r + 1] as usize;
        if gpos + m <= row_end {
            set_bit(words, r);
            at = row_end;
        } else {
            at = gpos + 1;
        }
    }
}

/// Share of rows selected from which `row-hay` searches the whole payload.
const ROW_HAY_SHARE: f64 = 0.25;

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
    let v = &h.view;
    let words = core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();

    let sel: Option<&[u32]> = (!q.selection.is_null()).then(|| (*q.selection).rows_slice());
    match strategy_index {
        0 => {
            let Some(pred) = Pred::new(q.op, &needles) else { return 11 };
            let mut row = |i: usize| {
                if pred.hit(&payload[offsets[i] as usize..offsets[i + 1] as usize]) {
                    set_bit(words, i);
                }
            };
            match sel {
                Some(sel) => sel.iter().for_each(|&i| row(i as usize)),
                None => (0..v.num_rows as usize).for_each(row),
            }
        }
        1 | 2 => {
            if q.op != LB_CONTAINS {
                return 11;
            }
            // `hay` never sees a selection; `row-hay` under `all` neither.
            let n = needles[0];
            match sel {
                Some(sel) if (sel.len() as f64) < ROW_HAY_SHARE * v.num_rows as f64 => {
                    let finder = Finder::new(n);
                    for &i in sel {
                        let i = i as usize;
                        if finder.find(&payload[offsets[i] as usize..offsets[i + 1] as usize]).is_some() {
                            set_bit(words, i);
                        }
                    }
                }
                _ => {
                    if n.is_empty() {
                        (0..v.num_rows as usize).for_each(|i| set_bit(words, i));
                    } else {
                        hay_all(&Finder::new(n), n.len(), payload, offsets, words);
                    }
                    if let Some(sel) = sel {
                        let mut keep = vec![0u64; words.len()];
                        sel.iter().for_each(|&i| set_bit(&mut keep, i as usize));
                        words.iter_mut().zip(&keep).for_each(|(w, k)| *w &= k);
                    }
                }
            }
        }
        _ => return 10,
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 3] = [
    LbStrategy { name: c"memmem".as_ptr(), supported_ops: LB_ALL_OPS, flags: LB_STRATEGY_TAKES_SELECTION },
    LbStrategy { name: c"hay".as_ptr(), supported_ops: op_bit(LB_CONTAINS), flags: 0 },
    LbStrategy {
        name: c"row-hay".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
        flags: LB_STRATEGY_TAKES_SELECTION,
    },
];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"uncompressed_memmem".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 3,
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
