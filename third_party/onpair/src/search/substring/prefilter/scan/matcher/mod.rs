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

use super::{Block, Mask, RowBlock};
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
pub(in crate::search::substring::prefilter::scan) use shared::PAIRS;
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

    /// The hits among the first `n` codes of one block of the per-row scan;
    /// `n` may run past the block.
    fn step(&self, codes: &RowBlock, n: usize) -> StepHits;
}

/// The hits of one [`RowBlock`] as code offsets, ascending. On NEON a code
/// is four bits of the word, `uzp1` and `shrn` without a movemask; on x86 it
/// is one, and only this type knows which.
#[derive(Clone, Copy)]
pub(in crate::search::substring::prefilter::scan) struct StepHits {
    word: u64,
    /// log2 of the bits per code.
    shift: u32,
}

impl StepHits {
    /// Four bits per code, all four set on a hit.
    #[cfg(target_arch = "aarch64")]
    fn nibbles(word: u64) -> Self {
        Self {
            word: word & 0x1111_1111_1111_1111,
            shift: 2,
        }
    }

    /// One bit per code.
    fn bits(word: u64) -> Self {
        Self { word, shift: 0 }
    }

    /// Drops the codes from `n` on: the block ran past the row's end.
    fn keep(self, n: usize) -> Self {
        let bits = n << self.shift;
        Self {
            word: if bits < 64 {
                self.word & ((1 << bits) - 1)
            } else {
                self.word
            },
            ..self
        }
    }
}

impl Iterator for StepHits {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        (self.word != 0).then(|| {
            let at = self.word.trailing_zeros() >> self.shift;
            self.word &= self.word - 1;
            at as usize
        })
    }
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

    #[inline]
    fn step(&self, codes: &RowBlock, n: usize) -> StepHits {
        // SAFETY: `policy::takes` answered for the set.
        unsafe { shared::step(self, codes) }.keep(n)
    }
}
