//! The correctness oracle — the single root of trust (DESIGN.md §8).
//!
//! Deliberately naive, allocation-free byte loops: no memchr, no SIMD, no
//! shared machinery with any candidate or scanner, so a bug in a fast
//! kernel cannot also hide in the judge. Semantics are normative in
//! contract/SEMANTICS.md; the fixture tests below encode that document.

use crate::bitmap::Bitmap;
use lb_abi::*;

/// Naive byte-equality (no memcmp so even libc SIMD stays out of the judge).
#[inline]
fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i] != b[i] {
            return false;
        }
    }
    true
}

/// First occurrence of `needle` in `row` at or after `from`.
/// An empty needle matches at `from` itself (SEMANTICS.md edge cases).
#[inline]
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
    for i in from..=(row.len() - needle.len()) {
        if eq(&row[i..i + needle.len()], needle) {
            return Some(i);
        }
    }
    None
}

/// Does `row` match `op` with `needles`? The normative reference.
pub fn row_matches(op: u32, needles: &[&[u8]], row: &[u8]) -> bool {
    match op {
        LB_PREFIX => {
            let n = needles[0];
            n.len() <= row.len() && eq(&row[..n.len()], n)
        }
        LB_SUFFIX => {
            let n = needles[0];
            n.len() <= row.len() && eq(&row[row.len() - n.len()..], n)
        }
        LB_CONTAINS => find_from(row, needles[0], 0).is_some(),
        LB_MULTI_CONTAINS => {
            // Greedy leftmost, position advances past each match.
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
        LB_LIKE => row_matches_like(needles[0], row),
        _ => unreachable!("op validated before reaching the oracle"),
    }
}

// --------------------------------------------------------------- LB_LIKE

/// Why a LIKE pattern is not a pattern at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LikeError {
    /// A backslash with nothing after it.
    TrailingEscape,
    /// A backslash before a byte that is not `%`, `_` or `\`. Both readings
    /// (literal backslash, or escape of an ordinary byte) are defensible, so
    /// the contract rejects rather than guesses. Carries the offending byte.
    UnknownEscape(u8),
}

impl std::fmt::Display for LikeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LikeError::TrailingEscape => {
                write!(f, "pattern ends in a lone '\\' with nothing to escape")
            }
            LikeError::UnknownEscape(b) => write!(
                f,
                "'\\' escapes byte 0x{b:02x} ({:?}), but only '%', '_' and '\\' may be escaped",
                *b as char
            ),
        }
    }
}

impl std::error::Error for LikeError {}

/// Is this a well-formed LIKE pattern? Called at suite load, so an invalid
/// pattern never reaches a matcher and `row_matches_like` stays total.
pub fn validate_like_pattern(pat: &[u8]) -> std::result::Result<(), LikeError> {
    let mut i = 0usize;
    while i < pat.len() {
        if pat[i] == b'\\' {
            match pat.get(i + 1) {
                None => return Err(LikeError::TrailingEscape),
                Some(&b'%') | Some(&b'_') | Some(&b'\\') => i += 2,
                Some(&b) => return Err(LikeError::UnknownEscape(b)),
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// The LIKE token at `p`: its kind and how many pattern bytes it spans.
/// `Lit` covers both a plain byte and an escaped metacharacter.
enum LikeTok {
    Any,
    One,
    Lit(u8, usize),
}

#[inline]
fn like_token(pat: &[u8], p: usize) -> LikeTok {
    match pat[p] {
        b'%' => LikeTok::Any,
        b'_' => LikeTok::One,
        // A lone trailing backslash cannot occur — validate_like_pattern
        // rejects it at load — but matching stays total either way and
        // treats it as the literal byte.
        b'\\' if p + 1 < pat.len() => LikeTok::Lit(pat[p + 1], 2),
        b => LikeTok::Lit(b, 1),
    }
}

/// Does `row` match the SQL LIKE `pat`? The normative reference for
/// `LB_LIKE` (contract/SEMANTICS.md).
///
/// Backtracking two-pointer scan: `%` records a resume point and is retried
/// one byte later on failure, so the whole thing is a pair of indices and no
/// allocation — same discipline as the literal matchers above. Worst case is
/// O(len(pat) · len(row)); the judge is never on a timed path.
pub fn row_matches_like(pat: &[u8], row: &[u8]) -> bool {
    let (mut p, mut s) = (0usize, 0usize);
    // Pattern index just past the last '%' seen, and the row index that '%'
    // was first tried at. `None` until a '%' has been seen.
    let mut star: Option<usize> = None;
    let mut star_s = 0usize;

    while s < row.len() {
        if p < pat.len() {
            match like_token(pat, p) {
                LikeTok::Any => {
                    star = Some(p + 1);
                    star_s = s;
                    p += 1;
                    continue;
                }
                LikeTok::One => {
                    p += 1;
                    s += 1;
                    continue;
                }
                LikeTok::Lit(b, width) if b == row[s] => {
                    p += width;
                    s += 1;
                    continue;
                }
                LikeTok::Lit(..) => {}
            }
        }
        // Mismatch, or the pattern ran out with row bytes left: let the last
        // '%' swallow one more byte and retry from just after it.
        match star {
            Some(resume) => {
                star_s += 1;
                s = star_s;
                p = resume;
            }
            None => return false,
        }
    }
    // Row exhausted: only a run of '%' may remain.
    while p < pat.len() && pat[p] == b'%' {
        p += 1;
    }
    p == pat.len()
}

/// Evaluate a query over rows yielded by `rows`, producing the canonical
/// whole-dataset bitmap.
pub fn eval<'a>(
    op: u32,
    needles: &[&[u8]],
    num_rows: u64,
    rows: impl Iterator<Item = &'a [u8]>,
) -> Bitmap {
    let mut bm = Bitmap::new(num_rows);
    for (i, row) in rows.enumerate() {
        if row_matches(op, needles, row) {
            bm.set(i as u64);
        }
    }
    bm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Differential twin: an independent second implementation built on
    /// std iterators, used only to cross-check the oracle (DESIGN.md §8).
    fn twin_matches(op: u32, needles: &[&[u8]], row: &[u8]) -> bool {
        fn occurs_at(row: &[u8], n: &[u8], from: usize) -> Option<usize> {
            if n.is_empty() {
                return (from <= row.len()).then_some(from);
            }
            if row.len() < n.len() || from + n.len() > row.len() {
                return None;
            }
            row[from..]
                .windows(n.len())
                .position(|w| w == n)
                .map(|p| p + from)
        }
        match op {
            LB_PREFIX => row.starts_with(needles[0]),
            LB_SUFFIX => row.ends_with(needles[0]),
            LB_CONTAINS => occurs_at(row, needles[0], 0).is_some(),
            LB_MULTI_CONTAINS => {
                let mut pos = 0usize;
                for n in needles {
                    match occurs_at(row, n, pos) {
                        Some(i) => pos = i + n.len(),
                        None => return false,
                    }
                }
                true
            }
            LB_CONTAINS_ANY => needles.iter().any(|n| occurs_at(row, n, 0).is_some()),
            _ => unreachable!(),
        }
    }

    fn m(op: u32, needles: &[&[u8]], row: &[u8]) -> bool {
        let ours = row_matches(op, needles, row);
        assert_eq!(
            ours,
            twin_matches(op, needles, row),
            "oracle vs twin diverge: op={op} needles={needles:?} row={row:?}"
        );
        ours
    }

    // ---- fixtures encoding SEMANTICS.md, op by op ----

    #[test]
    fn prefix() {
        assert!(m(LB_PREFIX, &[b"foo"], b"foobar"));
        assert!(m(LB_PREFIX, &[b"foo"], b"foo"));
        assert!(!m(LB_PREFIX, &[b"foo"], b"fob"));
        assert!(!m(LB_PREFIX, &[b"foo"], b"xfoo"));
        assert!(!m(LB_PREFIX, &[b"foo"], b"fo")); // needle longer than row
        assert!(m(LB_PREFIX, &[b""], b"anything")); // empty needle
        assert!(m(LB_PREFIX, &[b""], b"")); // empty needle, empty row
        assert!(!m(LB_PREFIX, &[b"x"], b""));
    }

    #[test]
    fn suffix() {
        assert!(m(LB_SUFFIX, &[b"bar"], b"foobar"));
        assert!(m(LB_SUFFIX, &[b"bar"], b"bar"));
        assert!(!m(LB_SUFFIX, &[b"bar"], b"barx"));
        assert!(!m(LB_SUFFIX, &[b"bar"], b"ar"));
        assert!(m(LB_SUFFIX, &[b""], b"anything"));
        assert!(m(LB_SUFFIX, &[b""], b""));
        assert!(!m(LB_SUFFIX, &[b"x"], b""));
    }

    #[test]
    fn contains() {
        assert!(m(LB_CONTAINS, &[b"oob"], b"foobar"));
        assert!(m(LB_CONTAINS, &[b"foobar"], b"foobar"));
        assert!(!m(LB_CONTAINS, &[b"foobarx"], b"foobar"));
        assert!(!m(LB_CONTAINS, &[b"oxb"], b"foobar"));
        assert!(m(LB_CONTAINS, &[b""], b"foobar"));
        assert!(m(LB_CONTAINS, &[b""], b""));
        assert!(m(LB_CONTAINS, &[b"aa"], b"aaa")); // overlapping occurrences
        assert!(m(LB_CONTAINS, &[&[0u8, 255u8][..]], &[1u8, 0, 255, 2])); // binary
    }

    #[test]
    fn multi_contains_ordering() {
        // in order, non-overlapping
        assert!(m(LB_MULTI_CONTAINS, &[b"a", b"b"], b"a_b"));
        assert!(!m(LB_MULTI_CONTAINS, &[b"b", b"a"], b"a_b")); // order matters
        assert!(m(LB_MULTI_CONTAINS, &[b"ab", b"cd"], b"abcd")); // adjacent ok
        assert!(!m(LB_MULTI_CONTAINS, &[b"ab", b"bc"], b"abc")); // overlap not ok
        assert!(m(LB_MULTI_CONTAINS, &[b"ab", b"bc"], b"ababc"));
    }

    #[test]
    fn multi_contains_duplicates_and_empties() {
        // duplicates need distinct sequential occurrences
        assert!(!m(LB_MULTI_CONTAINS, &[b"ab", b"ab"], b"ab"));
        assert!(!m(LB_MULTI_CONTAINS, &[b"ab", b"ab"], b"aab"));
        assert!(m(LB_MULTI_CONTAINS, &[b"ab", b"ab"], b"abab"));
        assert!(m(LB_MULTI_CONTAINS, &[b"aa", b"aa"], b"aaaa"));
        assert!(!m(LB_MULTI_CONTAINS, &[b"aa", b"aa"], b"aaa")); // would overlap
        // empty needles match at current position, advance 0
        assert!(m(LB_MULTI_CONTAINS, &[b"", b""], b""));
        assert!(m(LB_MULTI_CONTAINS, &[b"", b"x", b""], b"x"));
        assert!(m(LB_MULTI_CONTAINS, &[b"x", b""], b"x")); // empty at end of row
        assert!(!m(LB_MULTI_CONTAINS, &[b"x", b"", b"y"], b"x"));
        // single needle degenerates to contains
        assert!(m(LB_MULTI_CONTAINS, &[b"oob"], b"foobar"));
    }

    #[test]
    fn multi_contains_greedy_leftmost_is_complete() {
        // greedy leftmost must succeed whenever any assignment succeeds
        assert!(m(LB_MULTI_CONTAINS, &[b"a", b"ab"], b"aab"));
        assert!(m(LB_MULTI_CONTAINS, &[b"ba", b"ab"], b"babab"));
    }

    #[test]
    fn contains_any() {
        assert!(m(LB_CONTAINS_ANY, &[b"x", b"oob"], b"foobar"));
        assert!(!m(LB_CONTAINS_ANY, &[b"x", b"y"], b"foobar"));
        assert!(m(LB_CONTAINS_ANY, &[b"x", b""], b"foobar")); // empty matches all
        assert!(m(LB_CONTAINS_ANY, &[b"oob", b"oob"], b"foobar")); // dup = same
        assert!(!m(LB_CONTAINS_ANY, &[b"x"], b""));
        assert!(m(LB_CONTAINS_ANY, &[b""], b""));
    }

    // ---- LB_LIKE: the wildcard semantics of SEMANTICS.md ----

    /// Differential twin for LIKE: a dynamic-programming sweep over
    /// (pattern token × row byte). Shares no code with the backtracking
    /// matcher it checks, and is allowed to allocate — the oracle is not.
    fn twin_like(pat: &[u8], row: &[u8]) -> bool {
        #[derive(PartialEq)]
        enum T {
            Any,
            One,
            Lit(u8),
        }
        let mut toks = Vec::new();
        let mut i = 0;
        while i < pat.len() {
            match pat[i] {
                b'%' => {
                    toks.push(T::Any);
                    i += 1;
                }
                b'_' => {
                    toks.push(T::One);
                    i += 1;
                }
                b'\\' if i + 1 < pat.len() => {
                    toks.push(T::Lit(pat[i + 1]));
                    i += 2;
                }
                b => {
                    toks.push(T::Lit(b));
                    i += 1;
                }
            }
        }
        // dp[j]: the first j tokens match everything consumed so far.
        let mut dp = vec![false; toks.len() + 1];
        dp[0] = true;
        for j in 0..toks.len() {
            dp[j + 1] = dp[j] && toks[j] == T::Any; // leading '%' match nothing
        }
        for &b in row {
            let mut next = vec![false; toks.len() + 1];
            for (j, tok) in toks.iter().enumerate() {
                let consumed = match tok {
                    T::Any => dp[j + 1] || dp[j],
                    T::One => dp[j],
                    T::Lit(l) => dp[j] && *l == b,
                };
                if consumed {
                    next[j + 1] = true;
                }
            }
            // '%' may also match zero further bytes at this position.
            for j in 0..toks.len() {
                if next[j] && toks[j] == T::Any {
                    next[j + 1] = true;
                }
            }
            dp = next;
        }
        dp[toks.len()]
    }

    fn l(pat: &[u8], row: &[u8]) -> bool {
        let ours = row_matches_like(pat, row);
        assert_eq!(
            ours,
            twin_like(pat, row),
            "LIKE oracle vs twin diverge: pat={:?} row={:?}",
            String::from_utf8_lossy(pat),
            String::from_utf8_lossy(row)
        );
        // The op dispatcher must agree with the direct entry point.
        assert_eq!(ours, row_matches(LB_LIKE, &[pat], row));
        ours
    }

    #[test]
    fn like_underscore_is_exactly_one_byte() {
        assert!(l(b"%ab_c%", b"zzabXczz"));
        assert!(l(b"%ab_c%", b"zzab-czz"));
        assert!(l(b"%ab_c%", b"zzab_czz"));
        assert!(!l(b"%ab_c%", b"zzabczz")); // '_' cannot match zero bytes
        assert!(!l(b"%ab_c%", b"zzabXYczz")); // nor two
        // The transformations that must never be substituted for it.
        assert!(!l(b"%ab_c%", b"abc"));
        assert!(l(b"%abc%", b"abc"));
        assert!(l(b"%ab%c%", b"abXYc"));
        assert!(!l(b"%ab_c%", b"abXYc"));
    }

    #[test]
    fn like_underscore_on_multibyte_is_bytewise() {
        // 'ó' is two bytes in UTF-8: one '_' cannot cover it, two can.
        assert!(!l("%L_ve%".as_bytes(), "Lóve".as_bytes()));
        assert!(l("%L__ve%".as_bytes(), "Lóve".as_bytes()));
    }

    #[test]
    fn like_anchoring() {
        assert!(l(b"abc%", b"abcdef"));
        assert!(!l(b"abc%", b"xabcdef"));
        assert!(l(b"%abc", b"defabc"));
        assert!(!l(b"%abc", b"defabcx"));
        assert!(l(b"%abc%", b"xabcx"));
        assert!(l(b"abc", b"abc"));
        assert!(!l(b"abc", b"abcd"));
        // anchored gaps — the shapes no literal op can express
        assert!(l(b"a%c", b"abbbc"));
        assert!(!l(b"a%c", b"abbbcd"));
        assert!(l(b"%a%c", b"xxabbbc"));
        assert!(!l(b"%a%c", b"xxabbbcd"));
        assert!(l(b"a%c%", b"abbbcd"));
        assert!(!l(b"a%c%", b"xabbbcd"));
    }

    #[test]
    fn like_edge_cases() {
        assert!(l(b"", b"")); // empty pattern matches only the empty row
        assert!(!l(b"", b"x"));
        assert!(l(b"%", b"")); // '%' matches everything
        assert!(l(b"%", b"anything"));
        assert!(!l(b"_", b"")); // '_' needs exactly one byte
        assert!(l(b"_", b"x"));
        assert!(!l(b"_", b"xy"));
        assert!(l(b"%%a%%", b"xax")); // consecutive '%' collapse
        assert!(l(b"_%", b"x"));
        assert!(l(b"%_", b"x"));
        assert!(!l(b"_%", b""));
        assert!(!l(b"%_", b""));
        assert!(l(b"%\x00%", &[1u8, 0, 255])); // binary rows
        assert!(l(b"%_%", &[0xffu8]));
    }

    #[test]
    fn like_escapes() {
        assert!(l(b"%ab\\_c%", b"zzab_czz")); // escaped '_' is literal
        assert!(!l(b"%ab\\_c%", b"zzabXczz"));
        assert!(l(b"100\\%", b"100%")); // escaped '%' is literal
        assert!(!l(b"100\\%", b"100abc"));
        assert!(l(b"a\\\\b", b"a\\b")); // escaped backslash
        assert!(!l(b"a\\\\b", b"ab"));
    }

    #[test]
    fn like_pattern_validation() {
        assert!(validate_like_pattern(b"%ab_c%").is_ok());
        assert!(validate_like_pattern(b"a\\%b").is_ok());
        assert!(validate_like_pattern(b"a\\_b").is_ok());
        assert!(validate_like_pattern(b"a\\\\b").is_ok());
        assert!(validate_like_pattern(b"").is_ok());
        // trailing lone backslash: nothing to escape
        assert!(validate_like_pattern(b"abc\\").is_err());
        // backslash before an ordinary byte: two readings, so neither
        assert!(validate_like_pattern(b"a\\bc").is_err());
    }

    #[test]
    fn like_differential_random() {
        let mut rng = Lcg(0xA11C_E0FF_1CE5_2026);
        // Metacharacter-heavy alphabets so escapes, adjacency and
        // backtracking all get hit hard.
        let pat_alphabet = b"ab%_\\";
        let row_alphabet = b"ab_%\\\x00\xff";
        let mut checked = 0u64;
        for _ in 0..4000 {
            let pat = rng.bytes(8, pat_alphabet);
            if validate_like_pattern(&pat).is_err() {
                continue; // invalid patterns never reach a matcher
            }
            let row = rng.bytes(10, row_alphabet);
            l(&pat, &row); // asserts oracle == twin internally
            checked += 1;
        }
        assert!(checked > 1000, "too few valid patterns drawn: {checked}");
    }

    #[test]
    fn eval_bitmap() {
        let rows: Vec<&[u8]> = vec![b"foo", b"bar", b"foobar", b"", b"oof"];
        let bm = eval(LB_PREFIX, &[b"foo"], 5, rows.into_iter());
        assert_eq!(bm.count(), 2);
        assert!(bm.get(0) && bm.get(2));
    }

    // ---- randomized differential test (deterministic seed, no rand dep) ----

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            // Numerical Recipes LCG constants; quality is irrelevant here.
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
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

    #[test]
    fn differential_random() {
        // Tiny alphabet maximizes repeats/overlaps — the hard cases.
        let mut rng = Lcg(0x5EED_1BAD_F00D_2026);
        let alphabet = b"abAB\x00\xff";
        let mut checked = 0u64;
        for _ in 0..4000 {
            let row = rng.bytes(24, alphabet);
            let op = (rng.below(5)) as u32;
            let count = match op {
                LB_MULTI_CONTAINS | LB_CONTAINS_ANY => 1 + rng.below(4) as usize,
                _ => 1,
            };
            let needles: Vec<Vec<u8>> = (0..count).map(|_| rng.bytes(6, alphabet)).collect();
            let refs: Vec<&[u8]> = needles.iter().map(|n| n.as_slice()).collect();
            m(op, &refs, &row); // asserts oracle == twin internally
            checked += 1;
        }
        assert_eq!(checked, 4000);
    }
}
