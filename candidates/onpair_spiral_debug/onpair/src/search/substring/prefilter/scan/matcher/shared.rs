// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Load, OR, any and movemask around each kernel's compare, in pairs of code
//! vectors: a pair's compares pack into one [`Hit`], 0xFF/0x00 byte lanes on
//! NEON and AVX2 and a mask word on AVX-512, which a build selects with
//! `-C target-feature=+avx512bw` or `-C target-cpu=native`. A mask word is
//! [`PAIRS`] pairs, and [`block`] and [`row`] are a [`Kernel`] as a
//! [`Matcher`](super::Matcher).

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::{Block, Kernel, Mask};
use crate::core::types::Token;

/// Eight codes.
#[cfg(target_arch = "aarch64")]
pub(super) type Vector = uint16x8_t;
/// A compare of one vector.
#[cfg(target_arch = "aarch64")]
pub(super) type Compare = uint16x8_t;
/// The compares of a pair, one byte per code.
#[cfg(target_arch = "aarch64")]
pub(super) type Hit = uint8x16_t;
#[cfg(target_arch = "aarch64")]
pub(super) const PAIRS: usize = 4;

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
pub(super) type Vector = __m256i;
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
pub(super) type Compare = __m256i;
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
pub(super) type Hit = __m256i;
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
pub(super) const PAIRS: usize = 2;

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) type Vector = __m512i;
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) type Compare = __mmask32;
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) type Hit = u64;
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) const PAIRS: usize = 1;

/// `P` pairs of code vectors; a block's 64 codes at [`PAIRS`].
pub(super) type Vectors<const P: usize> = [[Vector; 2]; P];
/// Compares of `P` pairs, ORed at this width and packed once.
pub(super) type Wide<const P: usize> = [[Compare; 2]; P];
/// Hits for 64 codes.
pub(super) type Hits = [Hit; PAIRS];

/// NEON is baseline; AVX2 and AVX-512 are not, so the planner asks before
/// naming a vector kernel.
pub(in crate::search::substring::prefilter::scan) fn available() -> bool {
    #[cfg(target_arch = "aarch64")]
    return true;
    #[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
    return std::arch::is_x86_feature_detected!("avx2");
    #[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
    return std::arch::is_x86_feature_detected!("avx512bw");
}

/// Per pair of mask words: load 128 codes, `hits` on each 64, movemask both in
/// one store. Answers whether any pair was written.
#[inline]
#[cfg_attr(target_arch = "aarch64", target_feature(enable = "neon"))]
#[cfg_attr(target_arch = "x86_64", target_feature(enable = "avx2"))]
pub(super) fn block<K: Kernel>(kernel: &K, codes: &Block, bits: &mut Mask) -> bool {
    let mut written = false;
    for (two, bits) in bits.chunks_exact_mut(2).enumerate() {
        let at = codes[two * 128..].as_ptr();
        // SAFETY: a block holds 128 codes for every pair of mask words; the
        // set, inherited from this function.
        let (a, b) = unsafe { (kernel.hits(load(at)), kernel.hits(load(at.add(64)))) };
        if K::SKIP && !any(a, b) {
            bits.fill(0);
            continue;
        }
        written = true;
        // SAFETY: the chunk is the two words the store writes.
        unsafe { movemask(bits, a, b) };
    }
    written
}

/// The per-row step: 64 codes, one word.
#[inline]
#[cfg_attr(target_arch = "aarch64", target_feature(enable = "neon"))]
#[cfg_attr(target_arch = "x86_64", target_feature(enable = "avx2"))]
pub(super) fn row<K: Kernel>(kernel: &K, codes: &[Token; 64]) -> u64 {
    // SAFETY: 64 codes are readable; the set, inherited from this function.
    row_word(unsafe { kernel.hits(load(codes.as_ptr())) })
}

/// `P` pairs of code vectors from `at`.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(super) unsafe fn load<const P: usize>(at: *const Token) -> Vectors<P> {
    std::array::from_fn(|p| unsafe { [vld1q_u16(at.add(16 * p)), vld1q_u16(at.add(16 * p + 8))] })
}

/// `vuzp1q_u8` keeps the low byte of each lane, which a compare wrote alike to the high.
#[inline]
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(super) fn pack<const P: usize>(hit: Wide<P>) -> [Hit; P] {
    std::array::from_fn(|p| {
        vuzp1q_u8(
            vreinterpretq_u8_u16(hit[p][0]),
            vreinterpretq_u8_u16(hit[p][1]),
        )
    })
}

/// Forced inline for the reason `range::inside` is.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub(super) fn or_wide<const P: usize>(mut hit: Wide<P>, with: Wide<P>) -> Wide<P> {
    for p in 0..P {
        for h in 0..2 {
            // SAFETY: neon is baseline on aarch64.
            hit[p][h] = unsafe { vorrq_u16(hit[p][h], with[p][h]) };
        }
    }
    hit
}

#[inline]
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(super) fn or<const P: usize>(hit: [Hit; P], with: [Hit; P]) -> [Hit; P] {
    std::array::from_fn(|p| vorrq_u8(hit[p], with[p]))
}

#[inline]
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
fn any(a: Hits, b: Hits) -> bool {
    let a = vorrq_u8(vorrq_u8(a[0], a[1]), vorrq_u8(a[2], a[3]));
    let b = vorrq_u8(vorrq_u8(b[0], b[1]), vorrq_u8(b[2], b[3]));
    vmaxvq_u8(vorrq_u8(a, b)) != 0
}

#[cfg(target_arch = "aarch64")]
const LANE_BIT: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];

/// One bit per lane, then two pairwise adds: a word's 64 hits as 16 nibbles.
#[inline]
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
fn nibbles(hit: Hits) -> uint8x16_t {
    // SAFETY: LANE_BIT is 16 bytes.
    let lane_bit = unsafe { vld1q_u8(LANE_BIT.as_ptr()) };
    let lo = vpaddq_u8(vandq_u8(hit[0], lane_bit), vandq_u8(hit[1], lane_bit));
    let hi = vpaddq_u8(vandq_u8(hit[2], lane_bit), vandq_u8(hit[3], lane_bit));
    vpaddq_u8(lo, hi)
}

/// The third pairwise add lands both words in one vector.
#[inline]
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn movemask(bits: &mut [u64], a: Hits, b: Hits) {
    let lanes = vpaddq_u8(nibbles(a), nibbles(b));
    // SAFETY: the caller's chunk is the two words this writes.
    unsafe { vst1q_u64(bits.as_mut_ptr(), vreinterpretq_u64_u8(lanes)) };
}

/// Code `i`'s hit at bit `i`, all 64.
#[inline]
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(super) fn row_word(hit: Hits) -> u64 {
    let lanes = nibbles(hit);
    vgetq_lane_u64::<0>(vreinterpretq_u64_u8(vpaddq_u8(lanes, lanes)))
}

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn load<const P: usize>(at: *const Token) -> Vectors<P> {
    std::array::from_fn(|p| unsafe {
        [
            _mm256_loadu_si256(at.add(32 * p).cast()),
            _mm256_loadu_si256(at.add(32 * p + 16).cast()),
        ]
    })
}

/// `packs_epi16` interleaves the 128-bit halves; the permute restores code order.
#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
pub(super) fn pack<const P: usize>(hit: Wide<P>) -> [Hit; P] {
    std::array::from_fn(|p| {
        _mm256_permute4x64_epi64::<0xD8>(_mm256_packs_epi16(hit[p][0], hit[p][1]))
    })
}

#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
pub(super) fn or_wide<const P: usize>(hit: Wide<P>, with: Wide<P>) -> Wide<P> {
    std::array::from_fn(|p| {
        [
            _mm256_or_si256(hit[p][0], with[p][0]),
            _mm256_or_si256(hit[p][1], with[p][1]),
        ]
    })
}

#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
pub(super) fn or<const P: usize>(hit: [Hit; P], with: [Hit; P]) -> [Hit; P] {
    std::array::from_fn(|p| _mm256_or_si256(hit[p], with[p]))
}

#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
fn any(a: Hits, b: Hits) -> bool {
    let or = _mm256_or_si256(or(a, b)[0], or(a, b)[1]);
    _mm256_testz_si256(or, or) == 0
}

#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
unsafe fn movemask(bits: &mut [u64], a: Hits, b: Hits) {
    bits[0] = row_word(a);
    bits[1] = row_word(b);
}

/// Code `i`'s hit at bit `i`, all 64.
#[inline]
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
pub(super) fn row_word(hit: Hits) -> u64 {
    let lo = _mm256_movemask_epi8(hit[0]) as u32;
    let hi = _mm256_movemask_epi8(hit[1]) as u32;
    u64::from(lo) | u64::from(hi) << 32
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
#[target_feature(enable = "avx512f,avx512bw")]
pub(super) unsafe fn load<const P: usize>(at: *const Token) -> Vectors<P> {
    std::array::from_fn(|p| unsafe {
        [
            _mm512_loadu_si512(at.add(64 * p).cast()),
            _mm512_loadu_si512(at.add(64 * p + 32).cast()),
        ]
    })
}

/// Compares land in mask registers, so a pair's join is the pack.
#[inline]
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) fn pack<const P: usize>(hit: Wide<P>) -> [Hit; P] {
    std::array::from_fn(|p| u64::from(hit[p][0]) | u64::from(hit[p][1]) << 32)
}

#[inline]
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) fn or_wide<const P: usize>(hit: Wide<P>, with: Wide<P>) -> Wide<P> {
    std::array::from_fn(|p| [hit[p][0] | with[p][0], hit[p][1] | with[p][1]])
}

#[inline]
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) fn or<const P: usize>(hit: [Hit; P], with: [Hit; P]) -> [Hit; P] {
    std::array::from_fn(|p| hit[p] | with[p])
}

#[inline]
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
fn any(a: Hits, b: Hits) -> bool {
    a[0] | b[0] != 0
}

#[inline]
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
unsafe fn movemask(bits: &mut [u64], a: Hits, b: Hits) {
    bits[0] = row_word(a);
    bits[1] = row_word(b);
}

/// Code `i`'s hit at bit `i`, all 64.
#[inline]
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
pub(super) fn row_word(hit: Hits) -> u64 {
    hit[0]
}
