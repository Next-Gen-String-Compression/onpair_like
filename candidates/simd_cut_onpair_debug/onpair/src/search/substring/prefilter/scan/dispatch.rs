// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! A plan as the type parameters [`scan`] and [`scan_rows`] want.

use super::matcher::{self, Matcher};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use super::matcher::{MAX_BATCHES, PER_BATCH};
use super::policy::{Match, Plan, Resolve};
use super::{Check, resolver, scan, scan_rows};
use crate::core::offset::Offset;
use crate::core::types::Token;
use crate::search::substring::prefilter::ProbeCover;

/// The planned kernels over every row, or over `sel` alone.
pub(super) fn run<O: Offset>(
    plan: Plan,
    cover: &ProbeCover,
    codes: &[Token],
    row_offsets: &[O],
    check: Check<'_>,
    sel: Option<&[u32]>,
    out: &mut Vec<usize>,
) {
    macro_rules! with {
        ($m:ty) => {
            with::<O, $m>(plan, cover, codes, row_offsets, check, sel, out)
        };
    }
    // The same kernel compiled with the pack skipped, `S`, and without, `P`.
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    macro_rules! skip {
        ($s:ty, $p:ty) => {
            if plan.skip { with!($s) } else { with!($p) }
        };
    }
    match plan.matcher {
        Match::Table => with!(matcher::Table),
        // Few tokens over selected rows: a constant count of them.
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        Match::EqOr if sel.is_some() && cover.points().len() <= 8 => {
            macro_rules! at {
                ($($k:literal)*) => {
                    match cover.points().len() {
                        $($k => if cover.ranges().is_empty() {
                            with!(matcher::EqOrKPoints<$k>)
                        } else {
                            with!(matcher::EqOrK<$k>)
                        },)*
                        _ => unreachable!("EqOr takes a cover with a token"),
                    }
                };
            }
            at!(1 2 3 4 5 6 7 8)
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        Match::EqOr if cover.ranges().is_empty() => {
            skip!(matcher::EqOrPoints<true>, matcher::EqOrPoints<false>)
        }
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        Match::EqOr => skip!(matcher::EqOr<true>, matcher::EqOr<false>),
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        Match::Range => skip!(matcher::Range<true>, matcher::Range<false>),
        #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
        Match::NibbleN8K => {
            macro_rules! at {
                ($($n:literal)*) => {
                    match cover.points().len().div_ceil(PER_BATCH) {
                        $($n => skip!(matcher::NibbleN8<$n, true>, matcher::NibbleN8<$n, false>),)*
                        _ => unreachable!("`takes` admits at most MAX_BATCHES batches"),
                    }
                };
            }
            const _: () = assert!(MAX_BATCHES == 16, "one arm below per batch count");
            at!(1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16)
        }
    }
}

/// `M` over `sel` alone, or over every block under the planned resolver.
fn with<O: Offset, M: Matcher>(
    plan: Plan,
    cover: &ProbeCover,
    codes: &[Token],
    row_offsets: &[O],
    check: Check<'_>,
    sel: Option<&[u32]>,
    out: &mut Vec<usize>,
) {
    match (sel, plan.resolver) {
        (Some(sel), _) => scan_rows::<M, O>(cover, codes, row_offsets, sel, check, out),
        (None, Resolve::LinearSeek) => {
            scan::<M, resolver::LinearSeek<'_, O>>(cover, codes, row_offsets, check, out)
        }
        (None, Resolve::GallopSeek) => {
            scan::<M, resolver::GallopSeek<'_, O>>(cover, codes, row_offsets, check, out)
        }
    }
}
