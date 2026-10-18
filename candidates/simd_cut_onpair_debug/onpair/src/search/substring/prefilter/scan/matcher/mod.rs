// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Stage one: codes in, bit mask out.

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod eq_or;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod nibble_n8;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod range;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
mod shared;
mod table;
#[cfg(test)]
mod tests;

use super::{Block, Mask};
use crate::core::types::Token;
use crate::search::substring::prefilter::ProbeCover;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::search::substring::prefilter::scan) use eq_or::{
    EqOr, EqOrK, EqOrKPoints, EqOrPoints,
};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::search::substring::prefilter::scan) use nibble_n8::{
    MAX_BATCHES, NibbleN8, PER_BATCH,
};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::search::substring::prefilter::scan) use range::Range;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::search::substring::prefilter::scan) use shared::available as simd_available;
pub(in crate::search::substring::prefilter::scan) use table::Table;

/// Bit `i` of `bits` is set iff the cover admits `codes[i]`. See `README.md`.
pub(in crate::search::substring::prefilter::scan) trait Matcher:
    Sized
{
    /// Callers check `policy::takes` first.
    fn new(cover: &ProbeCover) -> Self;

    /// Fills `bits` for one block. `false` promises an empty mask, so stage two
    /// is skipped; `true` promises nothing.
    fn check(&self, codes: &Block, bits: &mut Mask) -> bool;

    /// Bit `i` for each of the first `n <= 64` of `codes`; the bits past
    /// code `n` are unspecified.
    fn check_row(&self, codes: &[Token; 64], n: usize) -> u64;
}

/// A vector kernel: the compare alone, over any number of pairs of code
/// vectors. `shared` wraps it as a [`Matcher`] for the block and for the row.
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(in crate::search::substring::prefilter::scan) trait Kernel:
    Sized
{
    /// Callers check `policy::takes` first.
    fn new(cover: &ProbeCover) -> Self;

    /// Whether a pair of mask words with no hit skips its movemask: a const
    /// the kernel is compiled for rather than a branch it takes.
    const SKIP: bool;

    /// The hits of `P` pairs of code vectors.
    ///
    /// # Safety
    /// The instruction set is enabled, which [`simd_available`] answers.
    unsafe fn hits<const P: usize>(&self, codes: shared::Vectors<P>) -> [shared::Hit; P];
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
impl<K: Kernel> Matcher for K {
    fn new(cover: &ProbeCover) -> Self {
        <K as Kernel>::new(cover)
    }

    fn check(&self, codes: &Block, bits: &mut Mask) -> bool {
        // SAFETY: `policy::takes` answered for the set.
        unsafe { shared::block(self, codes, bits) }
    }

    fn check_row(&self, codes: &[Token; 64], _n: usize) -> u64 {
        // SAFETY: `policy::takes` answered for the set.
        unsafe { shared::row(self, codes) }
    }
}
