// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! The planner's alignment graph and chosen cut, for logging and figures.

use super::graph::ProbeSet;
use super::plan::plan;
use super::{MAX_PATTERN_LEN, PrefilterAnalysis};
use crate::core::dictionary::CompactDictionaryView;
use crate::core::types::Token;
use crate::search::index::{TokenFrequencyIndex, TokenFrequencyIndexStorage};

/// What a scan compares a code against to catch one parse step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceLabel {
    /// One code.
    Code(Token),
    /// An explicit set of codes, ascending.
    CodeSet(Vec<Token>),
    /// An inclusive range of codes.
    CodeRange(Token, Token),
    /// A set too large to enumerate, which no cut may select.
    None,
}

/// One edge of the alignment graph. Nodes are needle offsets, `0` the start
/// and the needle length the end.
#[derive(Clone, Debug)]
pub struct TraceEdge {
    /// Offset the step leaves.
    pub from: u32,
    /// Offset the step reaches.
    pub to: u32,
    /// The probe for this step.
    pub label: TraceLabel,
    /// Whether the planner's chosen cut selects this step.
    pub in_cut: bool,
}

/// The alignment graph for a pattern, the cut the planner chose on it, and the
/// analysis [`analyze_prefilter`](super::analyze_prefilter) returns for it.
#[derive(Clone, Debug)]
pub struct PrefilterTrace {
    /// Every edge of the alignment graph.
    pub edges: Vec<TraceEdge>,
    /// The same analysis `analyze_prefilter` would return.
    pub analysis: PrefilterAnalysis,
}

/// Plan `pattern` as [`analyze_prefilter`](super::analyze_prefilter) does and
/// return the graph and cut behind the cover. The pattern must be non-empty.
/// The row count is ignored, as there.
///
/// # Panics
/// Panics on an empty pattern or one longer than [`MAX_PATTERN_LEN`].
pub fn trace_prefilter<S: TokenFrequencyIndexStorage>(
    pattern: &[u8],
    dict: CompactDictionaryView<'_>,
    frequencies: &TokenFrequencyIndex<S>,
    _row_count: usize,
) -> PrefilterTrace {
    assert!(!pattern.is_empty() && pattern.len() <= MAX_PATTERN_LEN);
    let (graph, cut, analysis) = plan(dict, pattern, frequencies.as_view());

    let mut in_cut = vec![false; graph.edges.len()];
    for &at in &cut {
        in_cut[at as usize] = true;
    }
    let edges = graph
        .edges
        .iter()
        .zip(in_cut)
        .map(|(edge, in_cut)| TraceEdge {
            from: edge.from,
            to: edge.to,
            label: match edge.probe() {
                ProbeSet::Point(id) => TraceLabel::Code(*id),
                ProbeSet::Range(range) => TraceLabel::CodeRange(range.begin, range.last),
                ProbeSet::Set(ids) => TraceLabel::CodeSet(ids.to_vec()),
                ProbeSet::SetTooBig => TraceLabel::None,
            },
            in_cut,
        })
        .collect();

    PrefilterTrace { edges, analysis }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::analyze_prefilter;
    use crate::search::index::build_token_frequency_index;
    use crate::{Column, DEFAULT_CONFIG, DictionaryView};

    #[test]
    fn trace_matches_analysis() {
        let rows: Vec<String> = (0..2000)
            .map(|i| format!("https://example.org/search?q={i}&page={}", i % 7))
            .collect();
        let bytes: Vec<u8> = rows.concat().into_bytes();
        let mut offsets = vec![0u32];
        for row in &rows {
            offsets.push(offsets.last().unwrap() + row.len() as u32);
        }
        let column = Column::compress(&bytes, &offsets, DEFAULT_CONFIG).unwrap();
        let view = column.view();
        let freq = build_token_frequency_index(view.codes, view.dict.num_tokens()).unwrap();
        for needle in [&b"search"[..], b"q=12", b"page=3", b"e", b"org/s"] {
            let trace = trace_prefilter(needle, view.dict, &freq, view.num_rows());
            let plain = analyze_prefilter(needle, view.dict, &freq, view.num_rows());
            let (a, b) = (trace.analysis.probe_cover(), plain.probe_cover());
            assert_eq!(a.points(), b.points());
            assert_eq!(a.ranges(), b.ranges());
            assert!(trace.edges.iter().any(|e| e.in_cut) || a.is_empty());
            assert!(
                trace
                    .edges
                    .iter()
                    .all(|e| e.from < e.to && e.to as usize <= needle.len())
            );
        }
    }
}
