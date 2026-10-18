// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! `code - begin <= last - begin` unsigned, so the wrap rejects codes below
//! `begin`; ORed over R ranges, folded into every kernel and run alone here.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::Kernel;
use super::shared::{Hit, Vector, Vectors, Wide, or, or_wide, pack};
use crate::core::types::TokenRange;
use crate::search::substring::prefilter::ProbeCover;

/// `begin` and `last - begin`, each broadcast.
pub(in crate::search::substring::prefilter::scan) type Held = (Vector, Vector);

#[cfg(target_arch = "aarch64")]
pub(in crate::search::substring::prefilter::scan) fn hold(range: TokenRange) -> Held {
    // SAFETY: neon is baseline on aarch64.
    unsafe {
        (
            vdupq_n_u16(range.begin),
            vdupq_n_u16(range.last - range.begin),
        )
    }
}

/// Forced inline: left to the inliner, the compare and nibble kernels put the
/// eight code vectors through the stack once per range, 3.5x what a range
/// costs here. `inline(always)` and `target_feature` exclude each other, and
/// neon is baseline on aarch64. A loop over a copy, not `array::map`, for the
/// same reason, it lowering to an out-of-line `drain_array_with`.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
unsafe fn inside<const P: usize>((lo, width): Held, codes: Vectors<P>) -> Wide<P> {
    // SAFETY: neon is baseline on aarch64.
    unsafe {
        let mut inside = codes;
        for p in 0..P {
            for h in 0..2 {
                inside[p][h] = vcleq_u16(vsubq_u16(codes[p][h], lo), width);
            }
        }
        inside
    }
}

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
pub(in crate::search::substring::prefilter::scan) fn hold(range: TokenRange) -> Held {
    // SAFETY: avx2, checked by `available` before planning.
    unsafe {
        (
            _mm256_set1_epi16(range.begin as i16),
            _mm256_set1_epi16((range.last - range.begin) as i16),
        )
    }
}

/// No unsigned u16 compare below AVX-512: `min(x, width) == x` is `x <= width`.
#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
unsafe fn inside<const P: usize>((lo, width): Held, codes: Vectors<P>) -> Wide<P> {
    codes.map(|pair| {
        pair.map(|codes| {
            let inside = _mm256_sub_epi16(codes, lo);
            _mm256_cmpeq_epi16(_mm256_min_epu16(inside, width), inside)
        })
    })
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(in crate::search::substring::prefilter::scan) fn hold(range: TokenRange) -> Held {
    // SAFETY: avx512bw, enabled by the build and checked by `available`.
    unsafe {
        (
            _mm512_set1_epi16(range.begin as i16),
            _mm512_set1_epi16((range.last - range.begin) as i16),
        )
    }
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
#[target_feature(enable = "avx512f,avx512bw")]
unsafe fn inside<const P: usize>((lo, width): Held, codes: Vectors<P>) -> Wide<P> {
    codes.map(|pair| pair.map(|codes| _mm512_cmple_epu16_mask(_mm512_sub_epi16(codes, lo), width)))
}

/// `hit` with every range ORed in, before the one `pack`.
#[inline]
#[cfg_attr(target_arch = "aarch64", target_feature(enable = "neon"))]
#[cfg_attr(target_arch = "x86_64", target_feature(enable = "avx2"))]
pub(super) unsafe fn or_ranges<const P: usize>(
    mut hit: Wide<P>,
    held: &[Held],
    codes: Vectors<P>,
) -> Wide<P> {
    for &held in held {
        // SAFETY: the set, inherited from this function.
        hit = unsafe { or_wide(hit, inside(held, codes)) };
    }
    hit
}

/// For a kernel whose hits are bytes already.
#[inline]
#[cfg_attr(target_arch = "aarch64", target_feature(enable = "neon"))]
#[cfg_attr(target_arch = "x86_64", target_feature(enable = "avx2"))]
pub(super) unsafe fn check_ranges<const P: usize>(
    hit: [Hit; P],
    held: &[Held],
    codes: Vectors<P>,
) -> [Hit; P] {
    match held.split_first() {
        None => hit,
        // SAFETY: the set, inherited from this function.
        Some((&first, rest)) => unsafe {
            or(hit, pack(or_ranges(inside(first, codes), rest, codes)))
        },
    }
}

/// For a cover with no tokens.
pub(in crate::search::substring::prefilter::scan) struct Range<
    const SKIP_MOVEMASK_IF_NO_MATCH: bool,
>(Vec<Held>);

impl<const SKIP_MOVEMASK_IF_NO_MATCH: bool> Kernel for Range<SKIP_MOVEMASK_IF_NO_MATCH> {
    const SKIP: bool = SKIP_MOVEMASK_IF_NO_MATCH;

    fn new(cover: &ProbeCover) -> Self {
        Self(cover.ranges().iter().copied().map(hold).collect())
    }

    #[inline(always)]
    unsafe fn hits<const P: usize>(&self, codes: Vectors<P>) -> [Hit; P] {
        let (&first, rest) = self.0.split_first().expect("a range, per policy::takes");
        // SAFETY: the set, the caller's promise.
        unsafe { pack(or_ranges(inside(first, codes), rest, codes)) }
    }
}
