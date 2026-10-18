// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Four nibble-table shuffles per code, ANDed: a token bit that survives all
//! four matched the whole code. Eight tokens per batch, batches ORed, ranges
//! beside them. L = 1. See `README.md`.

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use super::Kernel;
use super::range::{Held, check_ranges, hold};
use super::shared::{Hit, Vectors};
use crate::core::types::Token;
use crate::search::substring::prefilter::ProbeCover;

/// The bits in a table byte.
pub(in crate::search::substring::prefilter::scan) const PER_BATCH: usize = 8;

/// Batch counts the dispatch compiles an arm for. Not a register budget: the
/// sweep prices a batch the same at the thirty-second as at the first, and the
/// byte table is the cheaper kernel past about eight on every fitted set, so
/// the planner stops well short of this.
pub(in crate::search::substring::prefilter::scan) const MAX_BATCHES: usize = 16;

/// A 16-byte shuffle row, broadcast into every 128-bit lane on x86 since
/// `vpshufb` indexes within lanes.
#[cfg(target_arch = "aarch64")]
type Table = uint8x16_t;
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
type Table = __m256i;
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
type Table = __m512i;

/// One table per nibble of a code, low byte first, low nibble first.
pub(in crate::search::substring::prefilter::scan) struct Batch([Table; 4]);

/// Token `k` owns bit `1 << k` in the row each of its nibbles indexes.
fn batch(tokens: &[Token]) -> Batch {
    let mut rows = [[0u8; 16]; 4];
    for (k, &token) in tokens.iter().enumerate() {
        for (shift, row) in rows.iter_mut().enumerate() {
            row[usize::from(token >> (4 * shift) & 0x0f)] |= 1 << k;
        }
    }
    Batch(rows.map(table))
}

#[cfg(target_arch = "aarch64")]
fn table(row: [u8; 16]) -> Table {
    // SAFETY: neon is baseline on aarch64; the row is the load's 16 bytes.
    unsafe { vld1q_u8(row.as_ptr()) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn probe<const BATCHES: usize, const P: usize>(
    batches: &[Batch; BATCHES],
    codes: Vectors<P>,
) -> [Hit; P] {
    let nibble = vdupq_n_u8(0x0f);
    std::array::from_fn(|at| {
        let lo = vreinterpretq_u8_u16(codes[at][0]);
        let hi = vreinterpretq_u8_u16(codes[at][1]);
        let (low_byte, high_byte) = (vuzp1q_u8(lo, hi), vuzp2q_u8(lo, hi));
        let nibbles = [
            vandq_u8(low_byte, nibble),
            vshrq_n_u8::<4>(low_byte),
            vandq_u8(high_byte, nibble),
            vshrq_n_u8::<4>(high_byte),
        ];
        let pair = |batch: &Batch| {
            (
                vandq_u8(
                    vqtbl1q_u8(batch.0[0], nibbles[0]),
                    vqtbl1q_u8(batch.0[1], nibbles[1]),
                ),
                vandq_u8(
                    vqtbl1q_u8(batch.0[2], nibbles[2]),
                    vqtbl1q_u8(batch.0[3], nibbles[3]),
                ),
            )
        };
        let (low, high) = pair(&batches[0]);
        if BATCHES == 1 {
            return vtstq_u8(low, high);
        }
        let mut hit = vandq_u8(low, high);
        for batch in &batches[1..] {
            let (low, high) = pair(batch);
            hit = vorrq_u8(hit, vandq_u8(low, high));
        }
        vtstq_u8(hit, hit)
    })
}

#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
fn table(row: [u8; 16]) -> Table {
    // SAFETY: avx2, checked by `available`; the row is the load's 16 bytes.
    unsafe { _mm256_broadcastsi128_si256(_mm_loadu_si128(row.as_ptr().cast())) }
}

/// No deinterleave on x86: saturating pack of the cleared halves, then a qword
/// permute back into code order. No byte test either: a surviving lane is
/// nonzero, not 0xFF, and the pack reads bit 7, so two compares widen it.
#[cfg(all(target_arch = "x86_64", not(target_feature = "avx512bw")))]
#[target_feature(enable = "avx2")]
unsafe fn probe<const BATCHES: usize, const P: usize>(
    batches: &[Batch; BATCHES],
    codes: Vectors<P>,
) -> [Hit; P] {
    let byte = _mm256_set1_epi16(0x00ff);
    let nibble = _mm256_set1_epi8(0x0f);
    let zero = _mm256_setzero_si256();
    std::array::from_fn(|at| {
        let (lo, hi) = (codes[at][0], codes[at][1]);
        let order = |packed| _mm256_permute4x64_epi64::<0xD8>(packed);
        let low_byte = order(_mm256_packus_epi16(
            _mm256_and_si256(lo, byte),
            _mm256_and_si256(hi, byte),
        ));
        let high_byte = order(_mm256_packus_epi16(
            _mm256_srli_epi16::<8>(lo),
            _mm256_srli_epi16::<8>(hi),
        ));
        let nibbles = [
            _mm256_and_si256(low_byte, nibble),
            _mm256_and_si256(_mm256_srli_epi16::<4>(low_byte), nibble),
            _mm256_and_si256(high_byte, nibble),
            _mm256_and_si256(_mm256_srli_epi16::<4>(high_byte), nibble),
        ];
        let pair = |batch: &Batch| {
            (
                _mm256_and_si256(
                    _mm256_shuffle_epi8(batch.0[0], nibbles[0]),
                    _mm256_shuffle_epi8(batch.0[1], nibbles[1]),
                ),
                _mm256_and_si256(
                    _mm256_shuffle_epi8(batch.0[2], nibbles[2]),
                    _mm256_shuffle_epi8(batch.0[3], nibbles[3]),
                ),
            )
        };
        let (low, high) = pair(&batches[0]);
        let mut hit = _mm256_and_si256(low, high);
        for batch in &batches[1..] {
            let (low, high) = pair(batch);
            hit = _mm256_or_si256(hit, _mm256_and_si256(low, high));
        }
        _mm256_cmpeq_epi8(_mm256_cmpeq_epi8(hit, zero), zero)
    })
}

#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
fn table(row: [u8; 16]) -> Table {
    // SAFETY: the row is the load's 16 bytes.
    unsafe { _mm512_broadcast_i32x4(_mm_loadu_si128(row.as_ptr().cast())) }
}

/// No deinterleave on x86: saturating pack of the cleared halves, then a qword
/// permute back into code order.
#[cfg(all(target_arch = "x86_64", target_feature = "avx512bw"))]
#[target_feature(enable = "avx512f,avx512bw")]
unsafe fn probe<const BATCHES: usize, const P: usize>(
    batches: &[Batch; BATCHES],
    codes: Vectors<P>,
) -> [Hit; P] {
    std::array::from_fn(|p| {
        let byte = _mm512_set1_epi16(0x00ff);
        let order = _mm512_setr_epi64(0, 2, 4, 6, 1, 3, 5, 7);
        let low_byte = _mm512_permutexvar_epi64(
            order,
            _mm512_packus_epi16(
                _mm512_and_si512(codes[p][0], byte),
                _mm512_and_si512(codes[p][1], byte),
            ),
        );
        let high_byte = _mm512_permutexvar_epi64(
            order,
            _mm512_packus_epi16(
                _mm512_srli_epi16::<8>(codes[p][0]),
                _mm512_srli_epi16::<8>(codes[p][1]),
            ),
        );
        let nibble = _mm512_set1_epi8(0x0f);
        let nibbles = [
            _mm512_and_si512(low_byte, nibble),
            _mm512_and_si512(_mm512_srli_epi16::<4>(low_byte), nibble),
            _mm512_and_si512(high_byte, nibble),
            _mm512_and_si512(_mm512_srli_epi16::<4>(high_byte), nibble),
        ];
        let pair = |batch: &Batch| {
            (
                _mm512_and_si512(
                    _mm512_shuffle_epi8(batch.0[0], nibbles[0]),
                    _mm512_shuffle_epi8(batch.0[1], nibbles[1]),
                ),
                _mm512_and_si512(
                    _mm512_shuffle_epi8(batch.0[2], nibbles[2]),
                    _mm512_shuffle_epi8(batch.0[3], nibbles[3]),
                ),
            )
        };
        let (low, high) = pair(&batches[0]);
        if BATCHES == 1 {
            return _mm512_test_epi8_mask(low, high);
        }
        let mut hit = _mm512_and_si512(low, high);
        for batch in &batches[1..] {
            let (low, high) = pair(batch);
            hit = _mm512_or_si512(hit, _mm512_and_si512(low, high));
        }
        _mm512_test_epi8_mask(hit, hit)
    })
}

pub(in crate::search::substring::prefilter::scan) struct NibbleN8<
    const BATCHES: usize,
    const SKIP_MOVEMASK_IF_NO_MATCH: bool,
> {
    batches: [Batch; BATCHES],
    ranges: Vec<Held>,
}

impl<const BATCHES: usize, const SKIP_MOVEMASK_IF_NO_MATCH: bool> Kernel
    for NibbleN8<BATCHES, SKIP_MOVEMASK_IF_NO_MATCH>
{
    const SKIP: bool = SKIP_MOVEMASK_IF_NO_MATCH;

    fn new(cover: &ProbeCover) -> Self {
        let codes = cover.points();
        Self {
            batches: std::array::from_fn(|at| {
                let from = (at * PER_BATCH).min(codes.len());
                let to = (from + PER_BATCH).min(codes.len());
                batch(&codes[from..to])
            }),
            ranges: cover.ranges().iter().copied().map(hold).collect(),
        }
    }

    #[inline(always)]
    unsafe fn hits<const P: usize>(&self, codes: Vectors<P>) -> [Hit; P] {
        // SAFETY: the set, the caller's promise.
        unsafe { check_ranges(probe(&self.batches, codes), &self.ranges, codes) }
    }
}
