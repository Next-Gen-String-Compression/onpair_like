//! Benchmark-only access to the local OnPair scanner.
//!
//! Injected into a source snapshot by benchmark.py, never into the user's checkout.
//! All variants use the same prepared cover, resolver and exact graph walker.

use super::{ContainsScan, plan, scan};
use crate::{Column, Dictionary};
use scan::{MatcherConfig, MatcherKind};

/// One eligible execution choice for an already prepared query.
pub struct Variant {
    config: MatcherConfig,
    current: bool,
    selected: bool,
}

/// Instruction set actually available to this build and CPU.
pub fn isa() -> String {
    format!("{:?}", scan::detect_isa())
}

fn kind_name(kind: MatcherKind) -> &'static str {
    match kind {
        MatcherKind::Table => "table",
        MatcherKind::EqOr => "eq_or",
        MatcherKind::Range => "range",
        MatcherKind::NibbleN8 => "nibble_n8",
    }
}

/// Production execution plus every eligible forced matcher.
/// Packing follows the production policy and is held fixed across vector matchers.
pub fn variants(query: &ContainsScan, code_count: usize) -> Vec<Variant> {
    let isa = scan::detect_isa();
    let cover = query.probe_cover();
    let density = plan::probe_density(
        query.covered_frequency() as usize,
        query.total_frequency() as usize,
        code_count,
    );
    let selected = plan::select_matcher_config(isa, cover, density);
    let mut out = vec![Variant {
        config: selected,
        current: true,
        selected: true,
    }];
    for kind in [
        MatcherKind::Table,
        MatcherKind::EqOr,
        MatcherKind::Range,
        MatcherKind::NibbleN8,
    ] {
        if !plan::is_eligible(isa, kind, cover) {
            continue;
        }
        out.push(Variant {
            config: MatcherConfig {
                kind,
                skip_empty_packing: kind != MatcherKind::Table && selected.skip_empty_packing,
            },
            current: false,
            selected: kind == selected.kind,
        });
    }
    out
}

impl Variant {
    /// Stable name used in measurement records.
    pub fn name(&self) -> &'static str {
        if self.current {
            "current"
        } else {
            kind_name(self.config.kind)
        }
    }

    /// Matcher selected by this execution choice.
    pub fn matcher(&self) -> &'static str {
        kind_name(self.config.kind)
    }

    /// Whether this is the algorithm selected by the production cost model.
    pub fn selected(&self) -> bool {
        self.selected
    }

    /// Whether empty vector groups skip packing.
    pub fn skip_empty_packing(&self) -> bool {
        self.config.skip_empty_packing
    }

    /// Run the normal scanner with this configuration. Setup and eligibility
    /// checks happen before timing; matchers are prepared per scan as in production.
    pub fn run(&self, query: &ContainsScan, column: &Column<u32>, out: &mut Vec<usize>) {
        if self.current
            || query.matches_all
            || column.codes.is_empty()
            || query.probe_cover.is_empty()
        {
            query.scan(
                &column.codes,
                &column.row_offsets,
                column.dict.as_view(),
                out,
            );
            return;
        }
        scan::matches(
            self.config,
            scan::ScanInput::new(&column.codes, &column.row_offsets, &query.probe_cover),
            column.dict.as_view(),
            &query.walk,
            out,
        );
    }
}
