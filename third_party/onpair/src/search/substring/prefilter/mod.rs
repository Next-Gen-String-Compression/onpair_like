// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! SIMD prefilter for substring search. It compiles a sound probe cover from the
//! pattern and scans the code stream for the rows holding a probe token.
//!
//! The cover is sound because every occurrence either sits inside one token,
//! whose id is added unconditionally, or crosses a token boundary and so is a
//! path through the alignment DAG, which the cover cuts.

mod cover;
mod graph;
mod mincut;
mod paper;
mod plan;
mod scan;
mod trace;

#[cfg(test)]
mod tests;

pub use cover::ProbeCover;
pub use graph::Anchor;
pub use paper::{PrefilterHit, prefilter_hits, prefilter_walk_hits};
pub use trace::{PrefilterTrace, TraceEdge, TraceLabel, trace_prefilter};

use crate::ColumnView;
use crate::core::dictionary::CompactDictionaryView;
use crate::core::offset::Offset;
use crate::core::types::Token;
use crate::search::index::{TokenFrequencyIndex, TokenFrequencyIndexStorage};

/// The probe cover chosen for a pattern, with its frequency and cost.
#[derive(Debug, Clone)]
pub struct PrefilterAnalysis {
    probe_cover: ProbeCover,
    covered_frequency: u32,
    total_frequency: u32,
    scan_ns: f64,
    walk: scan::Walk,
    /// An empty pattern: every row matches, including rows without codes.
    matches_all: bool,
}

impl PrefilterAnalysis {
    /// The probe cover; empty for an empty pattern, which still matches every row.
    pub fn probe_cover(&self) -> &ProbeCover {
        &self.probe_cover
    }

    /// Modelled nanoseconds to scan the cover and walk its hits, the cost the cover was chosen by.
    pub fn expected_scan_ns(&self) -> f64 {
        self.scan_ns
    }

    /// Code positions whose token the cover holds.
    pub fn covered_frequency(&self) -> u32 {
        self.covered_frequency
    }

    /// Covered over total frequency, `0.0` for an empty stream.
    pub fn covered_fraction(&self) -> f64 {
        if self.total_frequency == 0 {
            0.0
        } else {
            f64::from(self.covered_frequency) / f64::from(self.total_frequency)
        }
    }

    /// Code positions the frequency index counts.
    pub fn total_frequency(&self) -> u32 {
        self.total_frequency
    }

    /// Expected share of `row_count` rows holding a covered code, an over-estimate
    /// when rows hold several.
    pub fn expected_candidate_row_fraction(&self, row_count: usize) -> f64 {
        if row_count == 0 {
            return 0.0;
        }
        if self.matches_all {
            return 1.0;
        }
        (f64::from(self.covered_frequency) / row_count as f64).min(1.0)
    }
}

/// The sound probe cover the cost model prices lowest for `pattern`.
///
/// `dict` is sorted, complete and unique, as [`Parser::train`](crate::Parser::train)
/// builds it, and `frequencies` counts its tokens over the scanned codes. The
/// frequencies steer the plan only; the cover stays sound whatever they are.
/// The row count no longer enters the cost model and is ignored.
/// Panics past [`MAX_PATTERN_LEN`] bytes.
pub fn analyze_prefilter<S: TokenFrequencyIndexStorage>(
    pattern: &[u8],
    dict: CompactDictionaryView<'_>,
    frequencies: &TokenFrequencyIndex<S>,
    _row_count: usize,
) -> PrefilterAnalysis {
    if pattern.is_empty() {
        return PrefilterAnalysis {
            probe_cover: ProbeCover::from_runs(Vec::new()),
            covered_frequency: 0,
            total_frequency: frequencies.total_frequency(),
            scan_ns: 0.0,
            walk: scan::Walk::default(),
            matches_all: true,
        };
    }
    assert!(
        pattern.len() <= MAX_PATTERN_LEN,
        "pattern of {} bytes exceeds the prefilter's {MAX_PATTERN_LEN}",
        pattern.len()
    );
    plan::plan(dict, pattern, frequencies.as_view()).2
}

/// A pattern prepared for the rows that start or end with it. The row fixes
/// where a prefix or suffix sits, so there is no cover to scan: the walk runs
/// once per row, forward from its first code or back from its last, over the
/// alignment graph of the layouts that position allows.
#[derive(Debug, Clone)]
pub struct AnchoredWalk {
    walk: scan::Walk,
    anchor: Anchor,
    /// An empty pattern: every row starts and ends with it.
    matches_all: bool,
}

impl AnchoredWalk {
    /// Prepares `pattern` against the sorted, complete `dict`.
    ///
    /// # Panics
    /// On [`Anchor::Anywhere`], which is [`analyze_prefilter`]'s, or past
    /// [`MAX_PATTERN_LEN`] bytes.
    pub fn new(pattern: &[u8], dict: CompactDictionaryView<'_>, anchor: Anchor) -> Self {
        assert!(
            anchor != Anchor::Anywhere,
            "a substring is analyze_prefilter's"
        );
        assert!(
            pattern.len() <= MAX_PATTERN_LEN,
            "pattern of {} bytes exceeds the walk's {MAX_PATTERN_LEN}",
            pattern.len()
        );
        if pattern.is_empty() {
            return Self {
                walk: scan::Walk::default(),
                anchor,
                matches_all: true,
            };
        }
        let graph = graph::build_alignment_graph(dict, pattern, None, anchor);
        Self {
            walk: scan::Walk::from_graph(&graph, pattern),
            anchor,
            matches_all: false,
        }
    }

    /// Whether the row `codes` starts (or ends) with the pattern.
    #[inline]
    pub fn matches(&self, codes: &[Token], dict: CompactDictionaryView<'_>) -> bool {
        if self.matches_all {
            return true;
        }
        match self.anchor {
            Anchor::End => self.walk.check_end(codes, dict),
            _ => self.walk.check_start(codes),
        }
    }
}

/// What a hit on the cover must pass before its row goes out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verify {
    /// Nothing: every row holding a covered token, which the caller verifies.
    Superset,
    /// The alignment walk: exactly the rows containing the pattern.
    Walk,
}

/// Scans of one row group, keeping the selection's scratch across queries.
#[derive(Debug, Default)]
pub struct Finder {
    /// One byte per row of the row group, 1 where the selection holds the row.
    selected: Vec<u8>,
}

impl Finder {
    /// A finder without scratch yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append the ascending rows of `view`, or of `sel` only, that `verify` admits.
    ///
    /// `view` is valid, `analysis` was built over `view.dict`, and `sel` holds
    /// ascending row indices without repeats, all inside the row group.
    pub fn find<O: Offset>(
        &mut self,
        view: ColumnView<'_, O>,
        analysis: &PrefilterAnalysis,
        verify: Verify,
        sel: Option<&[u32]>,
        out: &mut Vec<usize>,
    ) {
        let ColumnView {
            dict,
            codes,
            row_offsets,
        } = view;
        let num_rows = view.num_rows();
        if analysis.matches_all {
            match sel {
                None => out.extend(0..num_rows),
                Some(sel) => out.extend(sel.iter().map(|&r| r as usize)),
            }
            return;
        }
        let walk = (verify == Verify::Walk).then_some(scan::WalkCheck {
            walk: &analysis.walk,
            dict,
            codes,
        });
        let check = scan::Check { walk };
        let input = scan::ScanInput::full(codes, row_offsets, analysis.probe_cover());
        let plan = scan::plan(input, analysis);
        match sel {
            // A selected row costs the per-row scan its codes plus one word of
            // per-row overhead; the full scan costs every code once.
            Some(sel) if sel.len() * (codes.len() / num_rows + 64) < codes.len() => {
                scan::execute(plan, input, check, Some(sel), out);
            }
            Some(sel) => {
                let first = out.len();
                scan::execute(plan, input, check, None, out);
                self.keep_selected(num_rows, sel, out, first);
            }
            None => scan::execute(plan, input, check, None, out),
        }
    }

    /// Drops from `out[first..]` the rows `sel` does not hold, without branches:
    /// every row is written back, the write position advancing only past a
    /// selected one.
    fn keep_selected(&mut self, num_rows: usize, sel: &[u32], out: &mut Vec<usize>, first: usize) {
        self.selected.clear();
        self.selected.resize(num_rows, 0);
        for &r in sel {
            self.selected[r as usize] = 1;
        }
        let mut kept = first;
        for at in first..out.len() {
            let row = out[at];
            out[kept] = row;
            kept += usize::from(self.selected[row]);
        }
        out.truncate(kept);
    }
}

/// The longest pattern [`analyze_prefilter`] takes, bounded by the walk's `u16` node ids.
pub const MAX_PATTERN_LEN: usize = u16::MAX as usize;
