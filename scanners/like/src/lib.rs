//! The `like` scanner: SQL LIKE pattern evaluation over plaintext rows.
//!
//! This is the uncompressed baseline for `LB_LIKE` — and, through the
//! harness-composed `decode` strategy, the baseline for every decode-only
//! codec (`fsst`, `lz4`, `zstd`, `llm_token_tum`, `cpp_identity`) at no
//! per-candidate cost. Until a candidate declares `LB_LIKE` of its own, it is
//! the *only* thing in the roster that can answer a wildcard pattern, which
//! makes "what does a compressed engine save over decode-then-LIKE" a
//! measurable question rather than a blank column.
//!
//! It is emphatically **not** the oracle. `harness/src/oracle.rs` is the root
//! of trust and is deliberately naive; this shares no code with it and is
//! gated against it on every query like any other scanner. Two independent
//! implementations of the same semantics, one optimized and one obvious, is
//! the point.
//!
//! ## How it evaluates
//!
//! A pattern splits on `%` into fixed-width **segments**, each a run of
//! literal bytes and `_` holes. `%` is the only thing that can consume an
//! unknown number of bytes, so:
//!
//! ```text
//!   prefix%mid_le%suffix
//!   ^^^^^^ head     ^^^^^^ tail
//!          ^^^^^^ middles (found in order, left to right)
//! ```
//!
//! - the head must match at offset 0 and the tail at the very end, so both
//!   are one bounded compare;
//! - each middle is found at or after the previous match's end, greedy
//!   leftmost — which is complete here for the same reason it is for
//!   `multi_contains` (contract/SEMANTICS.md).
//!
//! A segment is located by SIMD-searching its **longest literal run** and
//! then verifying the holes around it, so `%speci_l%` still gets a real
//! `memmem` kernel over `speci` rather than degenerating to a byte loop.
//! A segment that is nothing but `_` has no literal to search and is matched
//! by its width alone.
//!
//! Portable by declaration: memchr dispatches internally, so `cpu_features`
//! is NULL for the same reason the `memmem` scanner's is.
//!
//! Only `LB_LIKE` is declared. The literal ops already have kernels tuned for
//! them; answering those here too would add a second, slower path to every
//! result table without adding a fact to it.

use core::ffi::c_void;

use lb_abi::*;
use memchr::memmem::Finder;

// ------------------------------------------------------------- segments

/// One `%`-free stretch of a pattern: a fixed number of byte positions, each
/// either a required literal or a `_` hole that any byte satisfies.
struct Seg {
    /// One entry per byte position; `None` is `_`.
    mask: Vec<Option<u8>>,
    /// Offset of the longest literal run within the segment, and a compiled
    /// finder for it. `None` when the segment is all holes.
    key: Option<(usize, Finder<'static>, usize)>,
}

impl Seg {
    fn new(mask: Vec<Option<u8>>) -> Seg {
        // Longest run of consecutive literals — the widest SIMD search key
        // this segment offers.
        let (mut best_at, mut best_len) = (0usize, 0usize);
        let (mut at, mut len) = (0usize, 0usize);
        for (i, m) in mask.iter().enumerate() {
            match m {
                Some(_) => {
                    if len == 0 {
                        at = i;
                    }
                    len += 1;
                    if len > best_len {
                        best_at = at;
                        best_len = len;
                    }
                }
                None => len = 0,
            }
        }
        let key = (best_len > 0).then(|| {
            let bytes: Vec<u8> = mask[best_at..best_at + best_len]
                .iter()
                .map(|m| m.unwrap())
                .collect();
            (best_at, Finder::new(&bytes).into_owned(), best_len)
        });
        Seg { mask, key }
    }

    #[inline]
    fn len(&self) -> usize {
        self.mask.len()
    }

    /// Does this segment match `row` starting at `at`?
    #[inline]
    fn matches_at(&self, row: &[u8], at: usize) -> bool {
        if at + self.mask.len() > row.len() {
            return false;
        }
        self.mask
            .iter()
            .zip(&row[at..])
            .all(|(m, &b)| match m {
                Some(want) => *want == b,
                None => true, // '_' takes any byte, but it does take one
            })
    }

    /// Earliest start `s >= from` with `s + len <= row.len()` at which this
    /// segment matches. `row` is already truncated to the tail boundary by
    /// the caller, so "fits in row" is the only bound to check.
    fn find_from(&self, row: &[u8], from: usize) -> Option<usize> {
        let Some((key_at, finder, key_len)) = &self.key else {
            // All holes: the only requirement is width.
            return (from + self.len() <= row.len()).then_some(from);
        };
        // A candidate segment start `s` puts the key at `s + key_at`, so hits
        // ascend exactly as `s` does and the first verified one is the
        // leftmost.
        let mut scan = from.checked_add(*key_at)?;
        while scan <= row.len().saturating_sub(*key_len) {
            let hit = finder.find(&row[scan..])? + scan;
            let s = hit - key_at;
            if self.matches_at(row, s) {
                return Some(s);
            }
            scan = hit + 1;
        }
        None
    }
}

/// A compiled pattern.
struct Plan {
    /// Anchored at offset 0; absent when the pattern starts with `%`.
    head: Option<Seg>,
    /// Anchored at the end; absent when the pattern ends with `%`.
    tail: Option<Seg>,
    /// Found in order between head and tail. Empty segments (from `%%`) are
    /// dropped at compile time — they match anywhere and consume nothing.
    middles: Vec<Seg>,
    /// No `%` anywhere: the row must equal the pattern, width included.
    exact: bool,
}

/// Split a pattern into `%`-separated segments, resolving `\` escapes.
///
/// A trailing lone `\` cannot occur — the suite loader rejects it
/// (`oracle::validate_like_pattern`) — but it is taken as a literal here
/// rather than panicking, so the scanner is total whatever reaches it.
fn compile(pat: &[u8]) -> Plan {
    let mut segs: Vec<Vec<Option<u8>>> = vec![Vec::new()];
    let mut i = 0;
    while i < pat.len() {
        match pat[i] {
            b'%' => {
                segs.push(Vec::new());
                i += 1;
            }
            b'_' => {
                segs.last_mut().unwrap().push(None);
                i += 1;
            }
            b'\\' if i + 1 < pat.len() => {
                segs.last_mut().unwrap().push(Some(pat[i + 1]));
                i += 2;
            }
            b => {
                segs.last_mut().unwrap().push(Some(b));
                i += 1;
            }
        }
    }

    if segs.len() == 1 {
        return Plan {
            head: Some(Seg::new(segs.pop().unwrap())),
            tail: None,
            middles: Vec::new(),
            exact: true,
        };
    }
    let tail_mask = segs.pop().unwrap();
    let head_mask = segs.remove(0);
    Plan {
        head: (!head_mask.is_empty()).then(|| Seg::new(head_mask)),
        tail: (!tail_mask.is_empty()).then(|| Seg::new(tail_mask)),
        middles: segs
            .into_iter()
            .filter(|m| !m.is_empty())
            .map(Seg::new)
            .collect(),
        exact: false,
    }
}

fn row_matches(plan: &Plan, row: &[u8]) -> bool {
    let mut lo = 0usize;
    let mut hi = row.len();

    if let Some(h) = &plan.head {
        if !h.matches_at(row, 0) {
            return false;
        }
        lo = h.len();
        if plan.exact {
            // No '%' to absorb anything: the pattern is the whole row.
            return lo == row.len();
        }
    } else if plan.exact {
        return row.is_empty();
    }

    if let Some(t) = &plan.tail {
        if t.len() > hi || hi - t.len() < lo {
            return false;
        }
        hi -= t.len();
        if !t.matches_at(row, hi) {
            return false;
        }
    }

    // Middles live strictly between head and tail, so search the bounded
    // window and never let one overlap the tail it must precede.
    let window = &row[..hi];
    for m in &plan.middles {
        match m.find_from(window, lo) {
            Some(s) => lo = s + m.len(),
            None => return false,
        }
    }
    true
}

// --------------------------------------------------------------- vtable

unsafe extern "C" fn prepare(query: *const LbQuery) -> *mut c_void {
    let q = &*query;
    if q.op != LB_LIKE || q.needle_count != 1 {
        return core::ptr::null_mut();
    }
    let needles = q.needles_vec();
    Box::into_raw(Box::new(compile(needles[0]))) as *mut c_void
}

unsafe extern "C" fn scan(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    _stats_or_null: *mut LbRunStats,
) -> i32 {
    let plan = &*(prepared as *const Plan);
    let v = &*view;
    let words = core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();
    for i in 0..v.num_rows as usize {
        let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
        if row_matches(plan, row) {
            set_bit(words, i);
        }
    }
    0
}

unsafe extern "C" fn release(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut Plan));
}

static VTABLE: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"like".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: op_bit(LB_LIKE),
    prepare: Some(prepare),
    scan: Some(scan),
    release: Some(release),
    // Every pattern the suite loader accepts is evaluable here: the plan is
    // a split on '%', which nothing can fail to do.
    supports_query: None,
};

pub fn vtable() -> &'static LbScanner {
    &VTABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force LIKE, written for this test module only: exponential
    /// recursive backtracking, the most obvious thing that could work. It
    /// shares nothing with the segment machinery above, which is the point.
    fn brute(pat: &[u8], row: &[u8]) -> bool {
        fn go(toks: &[Option<u8>], stars: &[bool], row: &[u8]) -> bool {
            // `stars[i]` marks a '%' occupying token slot i.
            match (toks.first(), stars.first()) {
                (None, None) => row.is_empty(),
                (_, Some(true)) => {
                    (0..=row.len()).any(|k| go(&toks[1..], &stars[1..], &row[k..]))
                }
                (Some(t), Some(false)) => {
                    !row.is_empty()
                        && t.map_or(true, |b| b == row[0])
                        && go(&toks[1..], &stars[1..], &row[1..])
                }
                _ => unreachable!(),
            }
        }
        // Tokenize into parallel arrays: token value, and "is this a '%'".
        let (mut toks, mut stars) = (Vec::new(), Vec::new());
        let mut i = 0;
        while i < pat.len() {
            match pat[i] {
                b'%' => {
                    toks.push(None);
                    stars.push(true);
                    i += 1;
                }
                b'_' => {
                    toks.push(None);
                    stars.push(false);
                    i += 1;
                }
                b'\\' if i + 1 < pat.len() => {
                    toks.push(Some(pat[i + 1]));
                    stars.push(false);
                    i += 2;
                }
                b => {
                    toks.push(Some(b));
                    stars.push(false);
                    i += 1;
                }
            }
        }
        go(&toks, &stars, row)
    }

    fn m(pat: &[u8], row: &[u8]) -> bool {
        let ours = row_matches(&compile(pat), row);
        assert_eq!(
            ours,
            brute(pat, row),
            "like scanner vs brute force diverge: pat={:?} row={:?}",
            String::from_utf8_lossy(pat),
            String::from_utf8_lossy(row),
        );
        ours
    }

    #[test]
    fn underscore_is_one_byte() {
        assert!(m(b"%ab_c%", b"zzabXczz"));
        assert!(m(b"%ab_c%", b"zzab_czz"));
        assert!(!m(b"%ab_c%", b"zzabczz"));
        assert!(!m(b"%ab_c%", b"zzabXYczz"));
    }

    #[test]
    fn anchoring_and_gaps() {
        assert!(m(b"abc%", b"abcdef"));
        assert!(!m(b"abc%", b"xabcdef"));
        assert!(m(b"%abc", b"defabc"));
        assert!(!m(b"%abc", b"defabcx"));
        assert!(m(b"a%c", b"abbbc"));
        assert!(!m(b"a%c", b"abbbcd"));
        assert!(m(b"%a%b%", b"xaybz"));
        assert!(!m(b"%b%a%", b"ab"));
        assert!(m(b"prefix%middle%suffix", b"prefix-middle-suffix"));
        assert!(!m(b"prefix%middle%suffix", b"xprefix-middle-suffixx"));
    }

    #[test]
    fn degenerate_patterns() {
        assert!(m(b"", b""));
        assert!(!m(b"", b"x"));
        assert!(m(b"%", b""));
        assert!(m(b"%", b"anything"));
        assert!(m(b"_", b"x"));
        assert!(!m(b"_", b""));
        assert!(!m(b"_", b"xy"));
        assert!(m(b"%%a%%", b"xax"));
        assert!(m(b"___", b"abc"));
        assert!(!m(b"___", b"ab"));
        assert!(m(b"%___%", b"abc"));
        assert!(!m(b"%____%", b"abc"));
    }

    #[test]
    fn escapes_and_binary() {
        assert!(m(b"%ab\\_c%", b"zzab_czz"));
        assert!(!m(b"%ab\\_c%", b"zzabXczz"));
        assert!(m(b"100\\%", b"100%"));
        assert!(m(b"a\\\\b", b"a\\b"));
        assert!(m(b"%\x00%", &[1u8, 0, 255]));
        assert!(m(b"%_%", &[0xffu8]));
    }

    /// The segment search keys off the *longest literal run*, so a pattern
    /// whose holes sit before that run exercises the `hit - key_at` rewind.
    #[test]
    fn key_offset_rewind() {
        assert!(m(b"%_bcdef%", b"xxabcdefyy"));
        assert!(!m(b"%_bcdef%", b"bcdefyy")); // nothing for '_' to take
        assert!(m(b"%__bcdef%", b"xxabcdefyy"));
        assert!(m(b"%ab_def_h%", b"zzabXdefYhzz"));
    }

    #[test]
    fn differential_random() {
        // Deterministic LCG; a tiny alphabet maximizes overlaps, which is
        // where a leftmost-match bug would hide.
        struct Lcg(u64);
        impl Lcg {
            fn next(&mut self) -> u64 {
                self.0 = self
                    .0
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                self.0
            }
            fn below(&mut self, n: u64) -> u64 {
                self.next() % n
            }
            fn bytes(&mut self, max_len: u64, alphabet: &[u8]) -> Vec<u8> {
                let len = self.below(max_len + 1);
                (0..len)
                    .map(|_| alphabet[self.below(alphabet.len() as u64) as usize])
                    .collect()
            }
        }
        let mut rng = Lcg(0x5CA9_2026_1EAF_D00D);
        for _ in 0..4000 {
            // No '\' in the pattern alphabet: a trailing lone escape is
            // rejected upstream and would only test unreachable behaviour.
            let pat = rng.bytes(8, b"ab%_");
            let row = rng.bytes(10, b"ab_%");
            m(&pat, &row); // asserts against brute force internally
        }
    }
}
