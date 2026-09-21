//! Reusable needle generation over canonical datasets.
//!
//! The sampled generator supports all LIKE operations. The SA + LCP generator
//! discovers row-bounded substrings and selects unique CONTAINS needles by
//! length and matching-row count. Both write ordinary, unblessed query suites;
//! the independent oracle remains the authority for benchmark truth.

mod balanced;
mod sampled;
mod substrings;
mod suites;

pub use balanced::{
    BalancedRequest, CellReport, GeneratedNeedle, GeneratedNeedles, LengthBucket, RowBucket,
    SUBSTRING_GENERATOR_VERSION,
};
pub use sampled::*;
pub use substrings::{IndexLimits, SubstringIndex};
pub use suites::{verify_balanced_suite, write_balanced_suite};

#[cfg(test)]
mod tests;
