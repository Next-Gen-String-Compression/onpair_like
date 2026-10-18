// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Scans of the code stream against a compiled cover: codes to bit mask,
//! bit mask to rows. See README.md for the contract between the halves.
//!
//! The scan takes any cover with something in it - the points as tokens, the
//! ranges beside them, or either alone - and picks its matcher from a cost
//! model. Every hit is verified against the alignment walk in the compressed
//! domain, so the rows are exact.
//!
//! Vector kernels are compiled for NEON, AVX2 and AVX-512; a target with
//! none, or an x86 without AVX2, runs the byte table instead, which is
//! scalar and portable. The scalar routine under `cfg(test)` is the
//! correctness oracle.

#[cfg(test)]
mod bench;
mod dispatch;
mod matcher;
mod policy;
mod resolver;
mod walk;

use matcher::Matcher;
pub(in crate::search::substring::prefilter) use policy::{
    Facts, Match, Shape, VERIFY_NS_PER_HIT, ns_per_code, scan_ns,
};
use resolver::GallopSeek;
pub(in crate::search::substring::prefilter) use walk::Walk;

use super::PrefilterAnalysis;
use super::cover::ProbeCover;
use crate::core::dictionary::CompactDictionaryView;
use crate::core::offset::Offset;
use crate::core::types::Token;

/// Borrowed buffers for one scan region.
#[derive(Clone, Copy)]
pub(super) struct ScanInput<'a, O> {
    pub(super) codes: &'a [Token],
    pub(super) row_offsets: &'a [O],
    pub(super) cover: &'a ProbeCover,
}

impl<'a, O> ScanInput<'a, O> {
    pub(super) const fn full(
        codes: &'a [Token],
        row_offsets: &'a [O],
        cover: &'a ProbeCover,
    ) -> Self {
        Self {
            codes,
            row_offsets,
            cover,
        }
    }
}

/// What the scan will be handed, without inspecting code values. `None` where
/// the region admits nothing whatever runs over it: an empty cover proves no
/// row matches, and a region with no rows has nothing to emit.
pub(super) type ScanPlan = Option<Facts>;

/// Derive an ephemeral plan without inspecting code values.
#[inline]
pub(super) fn plan<O: Offset>(input: ScanInput<'_, O>, analysis: &PrefilterAnalysis) -> ScanPlan {
    facts(
        input,
        analysis.covered_frequency() as usize,
        analysis.total_frequency() as usize,
    )
}

/// The region the scan will see and the hits it can expect there. Which
/// kernel runs over it is `policy`'s business, decided once the probe
/// is built.
fn facts<O: Offset>(
    input: ScanInput<'_, O>,
    covered_frequency: usize,
    total_frequency: usize,
) -> ScanPlan {
    let row_count = input.row_offsets.len().saturating_sub(1);
    if input.cover.is_empty() || row_count == 0 {
        return None;
    }
    Some(Facts {
        expected_hits: expected_hits(covered_frequency, total_frequency, input.codes.len()),
        code_count: input.codes.len(),
    })
}

/// Covered codes expected in this region: exact for the indexed population,
/// and a projection of it for a subset. Planning only, never correctness.
fn expected_hits(covered_frequency: usize, total_frequency: usize, code_count: usize) -> usize {
    if total_frequency == code_count {
        return covered_frequency;
    }
    if total_frequency == 0 {
        return 0;
    }
    let projected =
        (covered_frequency as u128).saturating_mul(code_count as u128) / total_frequency as u128;
    usize::try_from(projected).unwrap_or(usize::MAX)
}

/// Compatibility entry for tests that exercise dispatch with a synthetic
/// cover, without constructing a complete analysis.
#[cfg(test)]
pub(super) fn scan_cover<O: Offset>(
    codes: &[Token],
    row_offsets: &[O],
    cover: &ProbeCover,
    covered_frequency: usize,
    out: &mut Vec<usize>,
) {
    let input = ScanInput::full(codes, row_offsets, cover);
    execute(
        facts(input, covered_frequency, codes.len()),
        input,
        Check::SUPERSET,
        None,
        out,
    );
}

/// Execute a previously derived plan. This is the first stage that inspects
/// code values: `check` is what a hit passes before its row goes out, and
/// `sel` the rows scanned, every row without it.
pub(super) fn execute<O: Offset>(
    plan: ScanPlan,
    input: ScanInput<'_, O>,
    check: Check<'_>,
    sel: Option<&[u32]>,
    out: &mut Vec<usize>,
) {
    let Some(facts) = plan else {
        return;
    };
    dispatch::run(
        policy::select(input.cover, facts),
        input.cover,
        input.codes,
        input.row_offsets,
        check,
        sel,
        out,
    );
}

/// Test oracle for the split scan.
#[cfg(test)]
pub(super) fn scan_scalar<O: Offset>(
    codes: &[Token],
    row_offsets: &[O],
    pf: &ProbeCover,
    out: &mut Vec<usize>,
) {
    for row in 0..row_offsets.len().saturating_sub(1) {
        let a = row_offsets[row].to_usize();
        let b = row_offsets[row + 1].to_usize();
        if codes[a..b].iter().any(|&code| pf.contains(code)) {
            out.push(row);
        }
    }
}

/// Mask words per block.
pub(in crate::search::substring::prefilter) const WORDS: usize = 8;
/// Codes per block. Kernels rely on the multiple of 128.
pub(in crate::search::substring::prefilter) const BLOCK_SIZE_SCAN_BLOCKS: usize = 64 * WORDS;
/// Codes per block of the per-row scan: one pair of code vectors, the SIMD
/// width, so a short row costs one kernel call.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::search::substring::prefilter) const BLOCK_SIZE_SCAN_ROWS: usize = 64 / matcher::PAIRS;
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(in crate::search::substring::prefilter) const BLOCK_SIZE_SCAN_ROWS: usize = 64;

/// The instruction set this build's kernels run on. The other thing a cost
/// is per: the same kernel is a different cost on each, since what a range
/// or a pack costs is the set's business. `Scalar` is a target with no
/// vector kernels at all, where only the byte table is compiled.
///
/// Total by design though a build compiles one of them: a fit reads other
/// machines' numbers out of a CSV and has to name their sets.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::search::substring::prefilter) enum Isa {
    Neon,
    Avx2,
    Avx512Bw,
    Scalar,
}

impl Isa {
    /// The one this build compiled. A const, so the dispatch over it folds
    /// away and the other sets' costs are never asked for.
    #[cfg(target_arch = "aarch64")]
    pub(in crate::search::substring::prefilter) const BUILT: Self = Self::Neon;
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
    pub(in crate::search::substring::prefilter) const BUILT: Self = Self::Avx512Bw;
    #[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
    pub(in crate::search::substring::prefilter) const BUILT: Self = Self::Avx2;
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    pub(in crate::search::substring::prefilter) const BUILT: Self = Self::Scalar;
}

/// One block of codes, exactly what a matcher is handed.
type Block = [Token; BLOCK_SIZE_SCAN_BLOCKS];
/// One block of mask, bit `i` for code `i` of the block.
type Mask = [u64; WORDS];
/// One block of the per-row scan.
type RowBlock = [Token; BLOCK_SIZE_SCAN_ROWS];

/// What stage two asks of a hit before its row goes out.
#[derive(Clone, Copy)]
pub(super) struct Check<'a> {
    /// The alignment walk a hit must pass; `None` passes every hit.
    pub(super) walk: Option<WalkCheck<'a>>,
}

/// The walk and what it reads.
#[derive(Clone, Copy)]
pub(super) struct WalkCheck<'a> {
    pub(super) walk: &'a Walk,
    pub(super) dict: CompactDictionaryView<'a>,
    pub(super) codes: &'a [Token],
}

impl Check<'_> {
    /// Every hit's row is a candidate.
    #[cfg(test)]
    pub(super) const SUPERSET: Check<'static> = Check { walk: None };

    /// Whether the hit at stream index `code`, inside the row spanning
    /// `[row_start, row_end)`, passes.
    #[inline]
    fn passes(&self, code: usize, row_start: usize, row_end: usize) -> bool {
        self.walk
            .is_none_or(|w| w.walk.check(w.dict, w.codes, row_start, row_end, code))
    }
}

/// Both stages over a whole stream. Rows come out ascending and unique.
fn scan_blocks<M: Matcher, O: Offset>(
    cover: &ProbeCover,
    codes: &[Token],
    row_offsets: &[O],
    check: Check<'_>,
    out: &mut Vec<usize>,
) {
    let matcher = M::new(cover);
    let mut resolver = GallopSeek::new(row_offsets);
    let mut bits = [0u64; WORDS];
    blocks(codes, &mut |block, at, valid| {
        // A block the matcher reports empty costs stage two nothing.
        if matcher.check(block, &mut bits) {
            clear_from(&mut bits, valid);
            resolver.rows(&bits, at, check, out);
        }
    });
}

/// Walks the stream in blocks, straight out of `codes` while a whole [`Block`]
/// remains and through a zero-padded copy at the end. `step(block, at, valid)`
/// gets the stream index of the block and how many of its codes are the
/// stream's; bits past `valid` are the padding's and must go.
fn blocks(codes: &[Token], step: &mut dyn FnMut(&Block, usize, usize)) {
    let mut at = 0;
    while at + BLOCK_SIZE_SCAN_BLOCKS <= codes.len() {
        step(
            codes[at..at + BLOCK_SIZE_SCAN_BLOCKS].try_into().unwrap(),
            at,
            BLOCK_SIZE_SCAN_BLOCKS,
        );
        at += BLOCK_SIZE_SCAN_BLOCKS;
    }
    if at < codes.len() {
        let rest = codes.len() - at;
        let mut tail = [Token::default(); BLOCK_SIZE_SCAN_BLOCKS];
        tail[..rest].copy_from_slice(&codes[at..]);
        step(&tail, at, rest);
    }
}

/// The matcher over each row of `sel` from its first code, a
/// [`RowBlock`] at a time that runs past the row's end: the hits past it are
/// dropped, and every other hit is the row's own, so no resolver is asked.
/// The row goes out on its first hit that passes `check`, and the rest of it
/// is never read. `sel` is ascending row indices below the row count.
fn scan_rows<M: Matcher, O: Offset>(
    cover: &ProbeCover,
    codes: &[Token],
    row_offsets: &[O],
    sel: &[u32],
    check: Check<'_>,
    out: &mut Vec<usize>,
) {
    let matcher = M::new(cover);
    // Rows whose blocks all lie inside the stream read it in place; the few
    // at its end read a copy padded by a block.
    let inside = sel.partition_point(|&r| {
        row_offsets[r as usize + 1].to_usize() + BLOCK_SIZE_SCAN_ROWS <= codes.len()
    });
    // SAFETY: the partition's promise.
    unsafe { rows(&matcher, codes, 0, row_offsets, &sel[..inside], check, out) };
    if let Some(&first) = sel.get(inside) {
        let base = row_offsets[first as usize].to_usize();
        let mut padded = codes[base..].to_vec();
        padded.resize(padded.len() + BLOCK_SIZE_SCAN_ROWS, Token::default());
        // SAFETY: the padding covers the last block of every row.
        unsafe {
            rows(
                &matcher,
                &padded,
                base,
                row_offsets,
                &sel[inside..],
                check,
                out,
            )
        };
    }
}

/// [`scan_rows`] over rows `sel`, with `codes` the stream from index `base`
/// on, a block at least of which lies at and past every row's start.
/// One block per pass, the check at its end, so a short row loops once.
///
/// # Safety
/// For every row of `sel`, `codes` holds a block from each of its block
/// starts, and from its start when it is empty.
#[inline(always)]
unsafe fn rows<M: Matcher, O: Offset>(
    matcher: &M,
    codes: &[Token],
    base: usize,
    row_offsets: &[O],
    sel: &[u32],
    check: Check<'_>,
    out: &mut Vec<usize>,
) {
    'rows: for &r in sel {
        let r = r as usize;
        let (start, end) = (row_offsets[r].to_usize(), row_offsets[r + 1].to_usize());
        let mut at = start;
        loop {
            // SAFETY: the caller's promise.
            let block = unsafe { &*codes.as_ptr().add(at - base).cast::<RowBlock>() };
            for offset in matcher.step(block, end - at) {
                if check.passes(at + offset, start, end) {
                    out.push(r);
                    continue 'rows;
                }
            }
            at += BLOCK_SIZE_SCAN_ROWS;
            if at >= end {
                break;
            }
        }
    }
}

/// Clears the bits from position `valid` on.
fn clear_from(bits: &mut Mask, valid: usize) {
    if valid < BLOCK_SIZE_SCAN_BLOCKS {
        bits[valid / 64] &= (1u64 << (valid % 64)) - 1;
        bits[valid / 64 + 1..].fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::TokenRange;

    /// What the scan is handed, and what it declines.
    #[test]
    fn the_scan_takes_a_cover_it_can_probe() {
        let codes = [0 as Token; 8];
        let rows = [0u32, 4, 8];
        let takes = |points: Vec<Token>, ranges: Vec<TokenRange>| {
            let cover = ProbeCover { points, ranges };
            facts(ScanInput::full(&codes, &rows, &cover), 1, codes.len()).is_some()
        };
        assert!(takes(vec![7], Vec::new()), "a point");
        assert!(
            takes(Vec::new(), vec![TokenRange { begin: 1, last: 9 }]),
            "a range"
        );
        assert!(
            takes(vec![7], vec![TokenRange { begin: 1, last: 9 }]),
            "both"
        );
        assert!(!takes(Vec::new(), Vec::new()), "a cover with nothing in it");

        let cover = ProbeCover {
            points: vec![7],
            ranges: Vec::new(),
        };
        let rowless: &[u32] = &[0];
        assert!(facts(ScanInput::full(&codes, rowless, &cover), 1, 8).is_none());
    }

    /// Rows but no codes: the block driver has nothing to hand the matcher,
    /// and the empty rows must come out as no candidates rather than a panic.
    #[test]
    fn rows_without_codes_make_no_candidate() {
        let cover = ProbeCover {
            points: vec![7],
            ranges: Vec::new(),
        };
        let mut out = Vec::new();
        scan_cover(&[], &[0u32, 0, 0, 0], &cover, 0, &mut out);
        assert!(out.is_empty());
        scan_cover(&[], &[0u64, 0, 0, 0], &cover, 0, &mut out);
        assert!(out.is_empty());
    }

    /// The facts the scan plans its own halves from.
    #[test]
    fn the_scan_is_handed_the_region_it_will_see() {
        let codes = [0 as Token; 2_000];
        let rows: Vec<u32> = (0..=200).map(|row| row * 10).collect();
        let cover = ProbeCover {
            points: vec![7],
            ranges: Vec::new(),
        };
        assert_eq!(
            facts(ScanInput::full(&codes, &rows, &cover), 500, 10_000),
            Some(Facts {
                // A twentieth of the indexed codes are covered, so a
                // twentieth of the region's.
                expected_hits: 100,
                code_count: 2_000,
            })
        );
    }

    /// The projection at its ends: the whole index, none of it, and a region
    /// wider than the index it was measured over.
    #[test]
    fn the_hit_estimate_is_a_projection_of_the_index() {
        assert_eq!(expected_hits(7, 100, 100), 7);
        assert_eq!(expected_hits(500, 10_000, 2_000), 100);
        assert_eq!(expected_hits(1, 0, 100), 0);
        assert_eq!(expected_hits(1, 1, usize::MAX), usize::MAX);
    }
}
