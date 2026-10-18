// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Paper measurement only: the walk timed apart from the fused scan, for the
//! phase-shares and micro figures. No query path calls this; delete the file
//! and its `pub use` for the final submission.

use super::PrefilterAnalysis;
#[cfg(doc)]
use super::{Finder, Verify};
use crate::core::dictionary::CompactDictionaryView;
use crate::core::offset::Offset;
use crate::core::types::Token;

/// A covered token occurrence: the row it falls in, and its index in the code
/// stream. What the scan kernels hand the walk, recorded so the walk can be
/// timed without them. See [`prefilter_hits`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct PrefilterHit {
    /// The row the occurrence falls in.
    pub row: usize,
    /// Its index in `codes`.
    pub code: usize,
}

/// Append every covered token occurrence inside `rows`, ascending.
///
/// `rows` are the candidate rows a [`Verify::Superset`] scan admitted. This locates
/// scalar-wise what the kernels locate, so that [`prefilter_walk_hits`] can be
/// measured with nothing in front of it; the executing paths never call it.
///
/// # Precondition
/// As [`Finder::find`], and every entry of `rows` is a valid row index.
pub fn prefilter_hits<O: Offset>(
    codes: &[Token],
    row_offsets: &[O],
    analysis: &PrefilterAnalysis,
    rows: &[usize],
    out: &mut Vec<PrefilterHit>,
) {
    if analysis.matches_all {
        return;
    }
    let cover = analysis.probe_cover();
    for &row in rows {
        let start = row_offsets[row].to_usize();
        let end = row_offsets[row + 1].to_usize();
        out.extend(
            (start..end)
                .filter(|&code| cover.contains(codes[code]))
                .map(|code| PrefilterHit { row, code }),
        );
    }
}

/// Walk `hits` and append the rows the pattern occurs in, ascending.
///
/// The exact half of a [`Verify::Walk`] scan with the scan taken out: given
/// the hits the kernels would have produced, this performs the same alignment
/// walks in the same order, stopping at a row's first passing hit as the
/// resolver does. Over the hits of a [`Verify::Superset`] scan's rows it
/// returns the [`Verify::Walk`] scan's rows exactly.
///
/// This exists to price the walk per candidate row against the verifiers it
/// replaces, which are themselves timed over a candidate list. It is slower
/// than the fused path it measures, which walks a hit the kernel has just
/// produced, and is not a faster way to answer the query.
///
/// # Precondition
/// `hits` is ascending by row, as [`prefilter_hits`] produces it, and the
/// preconditions of [`Finder::find`] hold.
pub fn prefilter_walk_hits<O: Offset>(
    codes: &[Token],
    row_offsets: &[O],
    dict: CompactDictionaryView<'_>,
    analysis: &PrefilterAnalysis,
    hits: &[PrefilterHit],
    out: &mut Vec<usize>,
) {
    if analysis.matches_all {
        out.extend(0..row_offsets.len().saturating_sub(1));
        return;
    }
    let mut i = 0;
    while i < hits.len() {
        let row = hits[i].row;
        let start = row_offsets[row].to_usize();
        let end = row_offsets[row + 1].to_usize();
        let mut passed = false;
        while i < hits.len() && hits[i].row == row {
            if analysis.walk.check(dict, codes, start, end, hits[i].code) {
                passed = true;
                break;
            }
            i += 1;
        }
        if passed {
            out.push(row);
            while i < hits.len() && hits[i].row == row {
                i += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prefilter_hits, prefilter_walk_hits};
    use crate::core::dictionary::DictionaryView;
    use crate::search::index::build_token_frequency_index;
    use crate::search::substring::prefilter::analyze_prefilter;
    use crate::search::substring::prefilter::tests::{compress_rows, scan_superset, scan_walk};

    #[test]
    fn walk_over_located_hits_matches_the_fused_walk() {
        use crate::test_corpus::user_strings;
        let corpus: Vec<Vec<u8>> = user_strings(50)
            .into_iter()
            .flat_map(|s| std::iter::repeat_n(s.into_bytes(), 4))
            .collect();
        let refs: Vec<&[u8]> = corpus.iter().map(Vec::as_slice).collect();
        let col = compress_rows(&refs);
        let view = col.view();
        let frequencies = build_token_frequency_index(view.codes, view.dict.num_tokens()).unwrap();
        for needle in [&b"a"[..], b"user", b"ing", b"e_", b"zzz", b""] {
            let analysis = analyze_prefilter(needle, view.dict, &frequencies, view.num_rows());
            let mut exact = Vec::new();
            scan_walk(
                view.codes,
                view.row_offsets,
                view.dict,
                &analysis,
                &mut exact,
            );
            let mut superset = Vec::new();
            scan_superset(view, &analysis, &mut superset);
            let mut hits = Vec::new();
            prefilter_hits(
                view.codes,
                view.row_offsets,
                &analysis,
                &superset,
                &mut hits,
            );
            assert!(
                hits.windows(2)
                    .all(|w| w[0].row <= w[1].row && w[0].code < w[1].code),
                "hits are not ascending for {needle:?}"
            );
            let mut walked = Vec::new();
            prefilter_walk_hits(
                view.codes,
                view.row_offsets,
                view.dict,
                &analysis,
                &hits,
                &mut walked,
            );
            assert_eq!(walked, exact, "the split walk disagrees on {needle:?}");
        }
    }
}
