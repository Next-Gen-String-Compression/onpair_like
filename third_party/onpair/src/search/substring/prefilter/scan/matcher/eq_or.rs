// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! One compare per token per vector, ORed, plus the ranges. L = 1, any K, any R.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::Kernel;
use super::range::{Held, hold, or_ranges};
use super::shared::{Hit, Vector, Vectors, Wide, pack};
use crate::core::types::{Token, TokenRange};
use crate::search::substring::prefilter::ProbeCover;

#[cfg(target_arch = "aarch64")]
fn broadcast(code: Token) -> Vector {
    // SAFETY: neon is baseline on aarch64.
    unsafe { vdupq_n_u16(code) }
}

/// Forced inline: left to the inliner, K = 1 pays a call per vector that costs
/// more than the compare. `inline(always)` and `target_feature` exclude each
/// other, and neon is baseline on aarch64, so the body is an unsafe block.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn compare<const P: usize>(tokens: &[Vector], codes: Vectors<P>) -> Wide<P> {
    // SAFETY: neon is baseline on aarch64.
    unsafe {
        let (first, rest) = tokens.split_first().unwrap();
        let mut hit = codes;
        for p in 0..P {
            for h in 0..2 {
                hit[p][h] = vceqq_u16(codes[p][h], *first);
            }
        }
        for token in rest {
            for p in 0..P {
                for h in 0..2 {
                    hit[p][h] = vorrq_u16(hit[p][h], vceqq_u16(codes[p][h], *token));
                }
            }
        }
        hit
    }
}

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
fn broadcast(code: Token) -> Vector {
    // SAFETY: avx2, checked by `available` before planning.
    unsafe { _mm256_set1_epi16(code as i16) }
}

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
unsafe fn compare<const P: usize>(tokens: &[Vector], codes: Vectors<P>) -> Wide<P> {
    let (first, rest) = tokens.split_first().unwrap();
    let mut hit = codes.map(|pair| pair.map(|codes| _mm256_cmpeq_epi16(codes, *first)));
    for token in rest {
        for p in 0..P {
            for h in 0..2 {
                hit[p][h] = _mm256_or_si256(hit[p][h], _mm256_cmpeq_epi16(codes[p][h], *token));
            }
        }
    }
    hit
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
fn broadcast(code: Token) -> Vector {
    // SAFETY: avx512bw, enabled by the build and checked by `available`.
    unsafe { _mm512_set1_epi16(code as i16) }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
#[target_feature(enable = "avx512f,avx512bw")]
unsafe fn compare<const P: usize>(tokens: &[Vector], codes: Vectors<P>) -> Wide<P> {
    let (first, rest) = tokens.split_first().unwrap();
    let mut hit = codes.map(|pair| pair.map(|codes| _mm512_cmpeq_epi16_mask(codes, *first)));
    for token in rest {
        for p in 0..P {
            for h in 0..2 {
                hit[p][h] |= _mm512_cmpeq_epi16_mask(codes[p][h], *token);
            }
        }
    }
    hit
}

/// The tokens a kernel holds: any count, or exactly `K` inline so a constant
/// count unrolls the compares and drops the loop over them.
pub(super) trait Points {
    fn of(points: &[Token]) -> Self;
    fn held(&self) -> &[Vector];
}

impl Points for Vec<Vector> {
    fn of(points: &[Token]) -> Self {
        points.iter().copied().map(broadcast).collect()
    }

    fn held(&self) -> &[Vector] {
        self
    }
}

impl<const K: usize> Points for [Vector; K] {
    fn of(points: &[Token]) -> Self {
        std::array::from_fn(|at| broadcast(points[at]))
    }

    fn held(&self) -> &[Vector] {
        self
    }
}

/// The ranges beside the tokens: some, or none at all, so that a cover
/// without ranges compiles to the compare alone. Sharing one body costs the
/// compare 0.025 ns per code at K = 1, the register pressure of a range loop
/// it never enters.
pub(super) trait Ranges {
    fn of(ranges: &[TokenRange]) -> Self;
    /// # Safety
    /// The instruction set is enabled.
    unsafe fn or_into<const P: usize>(&self, hit: Wide<P>, codes: Vectors<P>) -> Wide<P>;
}

impl Ranges for Vec<Held> {
    fn of(ranges: &[TokenRange]) -> Self {
        ranges.iter().copied().map(hold).collect()
    }

    #[inline(always)]
    unsafe fn or_into<const P: usize>(&self, hit: Wide<P>, codes: Vectors<P>) -> Wide<P> {
        // SAFETY: the set, the caller's promise.
        unsafe { or_ranges(hit, self, codes) }
    }
}

impl Ranges for () {
    fn of(ranges: &[TokenRange]) -> Self {
        debug_assert!(ranges.is_empty());
    }

    #[inline(always)]
    unsafe fn or_into<const P: usize>(&self, hit: Wide<P>, _codes: Vectors<P>) -> Wide<P> {
        hit
    }
}

pub(in crate::search::substring::prefilter::scan) struct Tokens<
    T,
    R,
    const SKIP_MOVEMASK_IF_NO_MATCH: bool,
> {
    tokens: T,
    ranges: R,
}

/// Any token count, with ranges.
pub(in crate::search::substring::prefilter::scan) type EqOr<const SKIP_MOVEMASK_IF_NO_MATCH: bool> =
    Tokens<Vec<Vector>, Vec<Held>, SKIP_MOVEMASK_IF_NO_MATCH>;

/// Any token count, no ranges.
pub(in crate::search::substring::prefilter::scan) type EqOrPoints<
    const SKIP_MOVEMASK_IF_NO_MATCH: bool,
> = Tokens<Vec<Vector>, (), SKIP_MOVEMASK_IF_NO_MATCH>;

/// Exactly `K` tokens, with ranges, for the per-row scan.
pub(in crate::search::substring::prefilter::scan) type EqOrK<const K: usize> =
    Tokens<[Vector; K], Vec<Held>, false>;

/// Exactly `K` tokens, no ranges, for the per-row scan.
pub(in crate::search::substring::prefilter::scan) type EqOrKPoints<const K: usize> =
    Tokens<[Vector; K], (), false>;

impl<T: Points, R: Ranges, const SKIP_MOVEMASK_IF_NO_MATCH: bool> Kernel
    for Tokens<T, R, SKIP_MOVEMASK_IF_NO_MATCH>
{
    const SKIP: bool = SKIP_MOVEMASK_IF_NO_MATCH;

    fn new(cover: &ProbeCover) -> Self {
        Self {
            tokens: T::of(cover.points()),
            ranges: R::of(cover.ranges()),
        }
    }

    #[inline(always)]
    unsafe fn hits<const P: usize>(&self, codes: Vectors<P>) -> [Hit; P] {
        // SAFETY: the set, the caller's promise.
        unsafe {
            pack(
                self.ranges
                    .or_into(compare(self.tokens.held(), codes), codes),
            )
        }
    }
}
