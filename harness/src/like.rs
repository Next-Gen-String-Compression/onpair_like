//! Everything *about* a LIKE pattern that is not matching it.
//!
//! Matching semantics are the oracle's and live there — it is the root of
//! trust (DESIGN.md §8). This module owns the three derived questions:
//!
//! - **facts** — the shape numbers `bless` stamps into `derived` (wildcard
//!   counts, literal length, pattern class, the mandatory literal runs a
//!   prefilter would key on);
//! - **lowering** — the narrowest literal op that means exactly the same
//!   thing, so a pattern is stored under `contains` rather than `like`
//!   whenever the existing roster can answer it (contract/SEMANTICS.md,
//!   "Equivalence with the literal ops");
//! - **rendering** — the reverse: the canonical pattern text for a query
//!   written with the literal ops, so analysis can group every query by LIKE
//!   shape whatever op it is stored under.
//!
//! Lowering is a correctness claim, not a convenience: `harness/tests/
//! like_semantics.rs` asserts `row_matches(lowered) == row_matches_like(pat)`
//! over a randomized corpus.

/// One token of a pattern, escapes already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    /// `%` — zero or more bytes.
    Any,
    /// `_` — exactly one byte.
    One,
    /// A literal byte (possibly written escaped).
    Lit(u8),
}

/// Tokenize a pattern. A trailing lone `\` cannot occur in a loaded suite
/// (`oracle::validate_like_pattern` rejects it); here it is taken literally
/// so this function is total.
pub fn tokens(pat: &[u8]) -> Vec<Tok> {
    let mut out = Vec::with_capacity(pat.len());
    let mut i = 0;
    while i < pat.len() {
        match pat[i] {
            b'%' => {
                out.push(Tok::Any);
                i += 1;
            }
            b'_' => {
                out.push(Tok::One);
                i += 1;
            }
            b'\\' if i + 1 < pat.len() => {
                out.push(Tok::Lit(pat[i + 1]));
                i += 2;
            }
            b => {
                out.push(Tok::Lit(b));
                i += 1;
            }
        }
    }
    out
}

/// The `%`-shape of a pattern, orthogonal to its `_` count (which is
/// reported separately — the two axes cross).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// No `%` at all: the row must equal the pattern (modulo `_`).
    Exact,
    /// `lit%` — anchored at the head only.
    Prefix,
    /// `%lit` — anchored at the tail only.
    Suffix,
    /// `%lit%` — one literal run, unanchored.
    Contains,
    /// `%a%b%…%` — several runs, unanchored at both ends.
    MultiGap,
    /// `a%b…` — anchored at the head, with an interior gap.
    AnchoredGapHead,
    /// `…a%b` — anchored at the tail, with an interior gap.
    AnchoredGapTail,
    /// `a%…%b` — anchored at both ends, with an interior gap.
    AnchoredGapBoth,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Exact => "exact",
            Class::Prefix => "prefix",
            Class::Suffix => "suffix",
            Class::Contains => "contains",
            Class::MultiGap => "multi_gap",
            Class::AnchoredGapHead => "anchored_gap_head",
            Class::AnchoredGapTail => "anchored_gap_tail",
            Class::AnchoredGapBoth => "anchored_gap_both",
        }
    }
}

/// The shape of one pattern. Computed once at bless time; never a claim
/// written by hand.
#[derive(Debug, Clone)]
pub struct Facts {
    pub class: Class,
    pub percent_count: u64,
    pub underscore_count: u64,
    /// Literal bytes only — metacharacters excluded. This is the "needle
    /// length" the L-bands mean for a pattern.
    pub literal_len_total: u64,
    /// Maximal runs of consecutive literal bytes, in order. Every run is
    /// mandatory in any matching row whatever separates them, which is what
    /// makes them the unit a prefilter keys on.
    pub literal_runs: Vec<Vec<u8>>,
    /// The pattern starts with a literal or `_` (no leading `%`).
    pub anchored_head: bool,
    /// The pattern ends with a literal or `_` (no trailing `%`).
    pub anchored_tail: bool,
}

/// Describe a pattern's shape.
pub fn facts(pat: &[u8]) -> Facts {
    let toks = tokens(pat);
    let percent_count = toks.iter().filter(|t| **t == Tok::Any).count() as u64;
    let underscore_count = toks.iter().filter(|t| **t == Tok::One).count() as u64;

    let mut literal_runs: Vec<Vec<u8>> = Vec::new();
    let mut run: Vec<u8> = Vec::new();
    for t in &toks {
        match t {
            Tok::Lit(b) => run.push(*b),
            _ => {
                if !run.is_empty() {
                    literal_runs.push(std::mem::take(&mut run));
                }
            }
        }
    }
    if !run.is_empty() {
        literal_runs.push(run);
    }
    let literal_len_total = literal_runs.iter().map(|r| r.len() as u64).sum();

    let anchored_head = !matches!(toks.first(), Some(Tok::Any) | None);
    let anchored_tail = !matches!(toks.last(), Some(Tok::Any) | None);

    // Gap groups: runs of tokens separated by `%`. `_` does not separate —
    // it is a fixed-width hole inside one group, not a free gap.
    let gap_groups = 1 + percent_count_separating(&toks);
    let class = match (anchored_head, anchored_tail, percent_count, gap_groups) {
        (_, _, 0, _) => Class::Exact,
        (true, true, _, _) => Class::AnchoredGapBoth,
        (true, false, _, _) if gap_groups <= 2 => Class::Prefix,
        (true, false, _, _) => Class::AnchoredGapHead,
        (false, true, _, _) if gap_groups <= 2 => Class::Suffix,
        (false, true, _, _) => Class::AnchoredGapTail,
        (false, false, _, g) if g <= 3 => Class::Contains,
        (false, false, _, _) => Class::MultiGap,
    };

    Facts {
        class,
        percent_count,
        underscore_count,
        literal_len_total,
        literal_runs,
        anchored_head,
        anchored_tail,
    }
}

/// `%` tokens that actually separate content — consecutive `%` collapse, so
/// `%%a%%` separates exactly as `%a%` does.
fn percent_count_separating(toks: &[Tok]) -> usize {
    let mut n = 0;
    let mut prev_was_any = false;
    for t in toks {
        let is_any = *t == Tok::Any;
        if is_any && !prev_was_any {
            n += 1;
        }
        prev_was_any = is_any;
    }
    n
}

/// The narrowest literal op that means exactly this pattern, or `None` when
/// only `LB_LIKE` will do (any `_`, or an anchored gap).
///
/// The equivalences are normative — see contract/SEMANTICS.md.
pub fn lower(pat: &[u8]) -> Option<(u32, Vec<Vec<u8>>)> {
    let toks = tokens(pat);
    if toks.iter().any(|t| *t == Tok::One) {
        return None; // `_` has no literal-op equivalent, ever
    }
    if toks.is_empty() {
        return None; // the empty pattern matches only the empty row
    }
    let f = facts(pat);
    match (f.anchored_head, f.anchored_tail, f.literal_runs.len()) {
        // `lit%`
        (true, false, 1) => Some((lb_abi::LB_PREFIX, f.literal_runs)),
        // `%lit`
        (false, true, 1) => Some((lb_abi::LB_SUFFIX, f.literal_runs)),
        // `%lit%`
        (false, false, 1) => Some((lb_abi::LB_CONTAINS, f.literal_runs)),
        // a pattern of nothing but `%`: matches every row, as does an empty
        // `contains` needle.
        (false, false, 0) => Some((lb_abi::LB_CONTAINS, vec![Vec::new()])),
        // `%a%b%…%`
        (false, false, _) => Some((lb_abi::LB_MULTI_CONTAINS, f.literal_runs)),
        // anchored gaps: `a%b`, `a%b%`… have no literal-op equivalent
        _ => None,
    }
}

// --------------------------------------------------------------- buckets

/// Which selectivity stratum a blessed result falls in.
///
/// A true partition, unlike the generator's *target* bands (which carry
/// acceptance tolerances and can overlap): every query lands in exactly one
/// bucket, so a report grouped by bucket adds up. Ranges are half-open
/// `[lo, hi)`, and `ultra_rare` is defined on the **count** rather than the
/// ratio because "a handful of rows" does not scale with the column.
pub fn selectivity_bucket(match_count: u64, selectivity: f64) -> &'static str {
    match (match_count, selectivity) {
        (0, _) => "zero",
        (1..=10, _) => "ultra_rare",
        (_, s) if s < 1e-4 => "1e-5",
        (_, s) if s < 1e-3 => "1e-4",
        (_, s) if s < 1e-2 => "1e-3",
        (_, s) if s < 1e-1 => "1e-2",
        (_, s) if s < 0.3 => "1e-1",
        _ => "broad",
    }
}

/// Which literal-length band a pattern falls in: the bytes a matcher
/// actually has to compare, metacharacters excluded. Boundaries follow the
/// existing `L…` naming and straddle the 8/16/32-byte token and SIMD widths.
pub fn length_bucket(literal_len_total: u64) -> &'static str {
    match literal_len_total {
        0 => "L0",
        1..=3 => "L1-3",
        4..=7 => "L4-7",
        8..=16 => "L8-16",
        17..=32 => "L17-32",
        33..=64 => "L33-64",
        _ => "L65+",
    }
}

/// Escape the metacharacters in a literal needle so it is matched verbatim.
pub fn escape_literal(needle: &[u8], out: &mut Vec<u8>) {
    for &b in needle {
        if b == b'%' || b == b'_' || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
}

/// The canonical LIKE pattern for a query written with the literal ops —
/// the inverse of [`lower`]. `None` for `LB_CONTAINS_ANY`, which is a
/// disjunction of patterns rather than one pattern.
pub fn render(op: u32, needles: &[&[u8]]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match op {
        lb_abi::LB_PREFIX => {
            escape_literal(needles[0], &mut out);
            out.push(b'%');
        }
        lb_abi::LB_SUFFIX => {
            out.push(b'%');
            escape_literal(needles[0], &mut out);
        }
        lb_abi::LB_CONTAINS => {
            out.push(b'%');
            escape_literal(needles[0], &mut out);
            out.push(b'%');
        }
        lb_abi::LB_MULTI_CONTAINS => {
            out.push(b'%');
            for n in needles {
                escape_literal(n, &mut out);
                out.push(b'%');
            }
        }
        lb_abi::LB_LIKE => out.extend_from_slice(needles[0]),
        _ => return None, // contains_any
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(pat: &[u8]) -> Vec<String> {
        facts(pat)
            .literal_runs
            .iter()
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect()
    }

    #[test]
    fn facts_count_wildcards_and_literals() {
        let f = facts(b"%speci_l%requ_sts%");
        assert_eq!(f.percent_count, 3);
        assert_eq!(f.underscore_count, 2);
        assert_eq!(f.literal_len_total, 13); // speci(5) + l(1) + requ(4) + sts(3)
        assert_eq!(runs(b"%speci_l%requ_sts%"), ["speci", "l", "requ", "sts"]);
        assert!(!f.anchored_head && !f.anchored_tail);
        assert_eq!(f.class, Class::MultiGap);
    }

    #[test]
    fn facts_treat_escaped_metacharacters_as_literals() {
        let f = facts(b"100\\%\\_off");
        assert_eq!(f.percent_count, 0);
        assert_eq!(f.underscore_count, 0);
        assert_eq!(f.literal_len_total, 8); // "100%_off"
        assert_eq!(runs(b"100\\%\\_off"), ["100%_off"]);
        assert_eq!(f.class, Class::Exact);
    }

    #[test]
    fn facts_classify_shapes() {
        assert_eq!(facts(b"abc%").class, Class::Prefix);
        assert_eq!(facts(b"%abc").class, Class::Suffix);
        assert_eq!(facts(b"%abc%").class, Class::Contains);
        assert_eq!(facts(b"%a%b%").class, Class::MultiGap);
        assert_eq!(facts(b"a%b%").class, Class::AnchoredGapHead);
        assert_eq!(facts(b"%a%b").class, Class::AnchoredGapTail);
        assert_eq!(facts(b"a%b").class, Class::AnchoredGapBoth);
        assert_eq!(facts(b"abc").class, Class::Exact);
        // consecutive '%' collapse, so this is still a plain contains
        assert_eq!(facts(b"%%abc%%").class, Class::Contains);
        // '_' is a fixed-width hole, not a gap: shape is unchanged by it
        assert_eq!(facts(b"%ab_c%").class, Class::Contains);
        assert_eq!(facts(b"ab_c%").class, Class::Prefix);
    }

    #[test]
    fn lowering_picks_the_narrowest_op() {
        let lowered = |p: &[u8]| {
            lower(p).map(|(op, ns)| {
                (
                    lb_abi::op_name(op),
                    ns.iter()
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .collect::<Vec<_>>(),
                )
            })
        };
        assert_eq!(lowered(b"abc%"), Some(("prefix", vec!["abc".into()])));
        assert_eq!(lowered(b"%abc"), Some(("suffix", vec!["abc".into()])));
        assert_eq!(lowered(b"%abc%"), Some(("contains", vec!["abc".into()])));
        assert_eq!(
            lowered(b"%a%b%"),
            Some(("multi_contains", vec!["a".into(), "b".into()]))
        );
        assert_eq!(lowered(b"%"), Some(("contains", vec!["".into()])));
        // escaped metacharacters lower fine — they are ordinary bytes
        assert_eq!(lowered(b"%100\\%%"), Some(("contains", vec!["100%".into()])));
    }

    #[test]
    fn lowering_refuses_what_the_literal_ops_cannot_express() {
        assert_eq!(lower(b"%ab_c%"), None); // any '_'
        assert_eq!(lower(b"_"), None);
        assert_eq!(lower(b"a%b"), None); // anchored gaps
        assert_eq!(lower(b"a%b%"), None);
        assert_eq!(lower(b"%a%b"), None);
        assert_eq!(lower(b"abc"), None); // exact match is not a prefix
        assert_eq!(lower(b""), None);
    }

    /// T-EQ — the load-bearing claim of this module: a lowered query and the
    /// pattern it came from accept exactly the same rows. Storing `%a%b%` as
    /// `multi_contains` is only sound because of this, and the comment in
    /// SEMANTICS.md asserting it is not evidence — this is.
    #[test]
    fn lowering_is_semantically_exact() {
        // Deterministic LCG; quality is irrelevant, reproducibility is not.
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

        let mut rng = Lcg(0x10E8_2026_C0FF_EE01);
        // No '_' in the pattern alphabet: those never lower, and the test
        // would spend its whole budget on `None`.
        let pat_alphabet = b"ab%\\";
        let row_alphabet = b"ab%\\_";
        let (mut lowered, mut refused) = (0u64, 0u64);
        for _ in 0..3000 {
            let pat = rng.bytes(7, pat_alphabet);
            if crate::oracle::validate_like_pattern(&pat).is_err() {
                continue;
            }
            let Some((op, needles)) = lower(&pat) else {
                refused += 1;
                continue;
            };
            lowered += 1;
            let refs: Vec<&[u8]> = needles.iter().map(|n| n.as_slice()).collect();
            for _ in 0..12 {
                let row = rng.bytes(10, row_alphabet);
                assert_eq!(
                    crate::oracle::row_matches(op, &refs, &row),
                    crate::oracle::row_matches_like(&pat, &row),
                    "lowering changed the answer: pattern {:?} lowered to {} {:?}, row {:?}",
                    String::from_utf8_lossy(&pat),
                    lb_abi::op_name(op),
                    refs.iter()
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .collect::<Vec<_>>(),
                    String::from_utf8_lossy(&row),
                );
            }
        }
        assert!(lowered > 500, "too few patterns lowered: {lowered}");
        assert!(refused > 0, "expected some patterns to refuse lowering");
    }

    #[test]
    fn buckets_partition_their_axis() {
        // count-defined stratum first: 10 rows in 1M is 1e-5, but "a handful
        // of rows" is the interesting fact, so ultra_rare wins.
        assert_eq!(selectivity_bucket(0, 0.0), "zero");
        assert_eq!(selectivity_bucket(1, 1e-6), "ultra_rare");
        assert_eq!(selectivity_bucket(10, 1e-5), "ultra_rare");
        assert_eq!(selectivity_bucket(11, 1.1e-5), "1e-5");
        assert_eq!(selectivity_bucket(1_000, 1e-4), "1e-4");
        assert_eq!(selectivity_bucket(1_000, 9.99e-4), "1e-4");
        assert_eq!(selectivity_bucket(1_000, 1e-3), "1e-3");
        assert_eq!(selectivity_bucket(1_000, 5e-2), "1e-2");
        assert_eq!(selectivity_bucket(1_000, 0.29), "1e-1");
        assert_eq!(selectivity_bucket(1_000, 0.3), "broad");
        assert_eq!(selectivity_bucket(1_000, 1.0), "broad");

        assert_eq!(length_bucket(0), "L0");
        assert_eq!(length_bucket(3), "L1-3");
        assert_eq!(length_bucket(4), "L4-7");
        assert_eq!(length_bucket(8), "L8-16");
        assert_eq!(length_bucket(16), "L8-16");
        assert_eq!(length_bucket(17), "L17-32");
        assert_eq!(length_bucket(64), "L33-64");
        assert_eq!(length_bucket(65), "L65+");
    }

    #[test]
    fn render_is_the_inverse_of_lower() {
        for (op, needles) in [
            (lb_abi::LB_PREFIX, vec![&b"abc"[..]]),
            (lb_abi::LB_SUFFIX, vec![&b"abc"[..]]),
            (lb_abi::LB_CONTAINS, vec![&b"abc"[..]]),
            (lb_abi::LB_MULTI_CONTAINS, vec![&b"a"[..], &b"b"[..]]),
            // a needle that itself contains metacharacters must round-trip
            (lb_abi::LB_CONTAINS, vec![&b"50%_x"[..]]),
        ] {
            let pat = render(op, &needles).expect("renderable");
            let (lop, lns) = lower(&pat).expect("lowerable");
            assert_eq!(lop, op, "op round-trip for {:?}", String::from_utf8_lossy(&pat));
            let lns: Vec<&[u8]> = lns.iter().map(|n| n.as_slice()).collect();
            assert_eq!(lns, needles, "needle round-trip for {:?}", String::from_utf8_lossy(&pat));
        }
        assert_eq!(render(lb_abi::LB_CONTAINS_ANY, &[&b"a"[..]]), None);
    }
}
