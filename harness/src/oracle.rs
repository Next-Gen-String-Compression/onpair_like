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
        _ => unreachable!("op validated before reaching the oracle"),
    }
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
