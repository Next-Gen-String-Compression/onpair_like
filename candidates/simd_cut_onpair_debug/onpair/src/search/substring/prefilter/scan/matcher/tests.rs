// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Every matcher against `ProbeCover::contains`.

use super::{Block, Mask, Matcher, Table};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use super::{EqOr, EqOrK, EqOrKPoints, EqOrPoints, Range};
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use super::{NibbleN8, PER_BATCH};
use crate::core::types::{Token, TokenRange};
use crate::search::substring::prefilter::ProbeCover;
use crate::search::substring::prefilter::scan::policy::{Match, Shape, takes};
use crate::search::substring::prefilter::scan::{BLOCK, Check, WORDS, resolver, scan};

fn mask<M: Matcher>(cover: &ProbeCover, codes: &Block) -> Mask {
    let matcher = M::new(cover);
    let mut bits = [0u64; WORDS];
    // A cleared flag promises an empty mask. The other way round is allowed:
    // a kernel that packs regardless never asked the question.
    let any_hit = matcher.check(codes, &mut bits);
    assert!(
        any_hit || bits.iter().all(|&set| set == 0),
        "a set bit under a cleared empty-block flag"
    );
    // As the per-row scan reads it: `n` codes from every 16-aligned start
    // that leaves 64 codes readable.
    for start in (0..=BLOCK - 64).step_by(16) {
        let block: &[Token; 64] = codes[start..start + 64].try_into().unwrap();
        for n in 1..=64 {
            let row = matcher.check_row(block, n);
            for code in 0..n {
                let at = start + code;
                let want = bits[at / 64] >> (at % 64) & 1;
                let got = row >> code & 1;
                assert_eq!(got, want, "code {at} differs as a row of {n} from {start}");
            }
        }
    }
    bits
}

/// The mask the cover defines: bit `i` iff the probe matches at `codes[i]`.
/// One code at a time, straight off `ProbeCover::contains`, so a kernel is checked
/// against the contract and never against a sibling.
fn expected(cover: &ProbeCover, codes: &Block) -> Mask {
    let mut bits = [0u64; WORDS];
    for (at, &code) in codes.iter().enumerate() {
        if cover.contains(code) {
            bits[at / 64] |= 1 << (at % 64);
        }
    }
    bits
}

/// Bit for bit with the definition, where the policy hands the kernel the
/// cover at all.
fn agrees<M: Matcher>(kind: Match, name: &str, cover: &ProbeCover, codes: &Block) {
    if !takes(kind, Shape::of(cover)) {
        return;
    }
    assert_eq!(
        mask::<M>(cover, codes),
        expected(cover, codes),
        "{name} differs from the cover it was built from"
    );
}

/// A block whose codes are all distinct: 37 is invertible modulo 50021, so
/// no value repeats over a block and a needle has exactly one hit.
fn block() -> Block {
    let mut codes = [0 as Token; BLOCK];
    for (at, code) in codes.iter_mut().enumerate() {
        *code = (at * 37 % 50021) as Token;
    }
    codes
}

/// Every matcher on one cover and one block.
fn every_matcher(cover: &ProbeCover, codes: &Block) {
    agrees::<Table>(Match::Table, "table", cover, codes);
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    {
        agrees::<EqOr<false>>(Match::EqOr, "eq_or", cover, codes);
        agrees::<EqOr<true>>(Match::EqOr, "eq_or_skip_empty", cover, codes);
        if cover.ranges().is_empty() {
            agrees::<EqOrPoints<false>>(Match::EqOr, "eq_or_points", cover, codes);
            agrees::<EqOrPoints<true>>(Match::EqOr, "eq_or_points_skip_empty", cover, codes);
        }
        match cover.points().len() {
            1 => agrees::<EqOrK<1>>(Match::EqOr, "eq_or_k1", cover, codes),
            2 => agrees::<EqOrK<2>>(Match::EqOr, "eq_or_k2", cover, codes),
            3 => agrees::<EqOrK<3>>(Match::EqOr, "eq_or_k3", cover, codes),
            8 => agrees::<EqOrK<8>>(Match::EqOr, "eq_or_k8", cover, codes),
            _ => {}
        }
        if cover.ranges().is_empty() {
            match cover.points().len() {
                1 => agrees::<EqOrKPoints<1>>(Match::EqOr, "eq_or_k1_points", cover, codes),
                2 => agrees::<EqOrKPoints<2>>(Match::EqOr, "eq_or_k2_points", cover, codes),
                8 => agrees::<EqOrKPoints<8>>(Match::EqOr, "eq_or_k8_points", cover, codes),
                _ => {}
            }
        }
        agrees::<Range<false>>(Match::Range, "range", cover, codes);
        agrees::<Range<true>>(Match::Range, "range_skip_empty", cover, codes);
        // At the batch count the dispatch compiles for K.
        let n8k = Match::NibbleN8K;
        match cover.points().len().div_ceil(PER_BATCH) {
            1 => {
                agrees::<NibbleN8<1, false>>(n8k, "nibble_n8", cover, codes);
                agrees::<NibbleN8<1, true>>(n8k, "nibble_n8_skip_empty", cover, codes);
            }
            2 => {
                agrees::<NibbleN8<2, false>>(n8k, "nibble_n16", cover, codes);
                agrees::<NibbleN8<2, true>>(n8k, "nibble_n16_skip_empty", cover, codes);
            }
            3 => {
                agrees::<NibbleN8<3, false>>(n8k, "nibble_n24", cover, codes);
                agrees::<NibbleN8<3, true>>(n8k, "nibble_n24_skip_empty", cover, codes);
            }
            _ => {}
        }
    }
}

/// One cover as it comes, then with one range beside it and with two, so
/// a kernel that stops after the first is caught.
fn every_matcher_ranged(cover: ProbeCover, lo: Token, hi: Token, codes: &Block) {
    every_matcher(&cover, codes);
    every_matcher(
        &ProbeCover::new(
            cover.points().to_vec(),
            vec![TokenRange {
                begin: lo,
                last: hi,
            }],
        ),
        codes,
    );
    let two = [
        TokenRange {
            begin: lo,
            last: hi,
        },
        TokenRange {
            begin: 60000,
            last: 60100,
        },
    ];
    every_matcher(
        &ProbeCover::new(cover.points().to_vec(), two.to_vec()),
        codes,
    );
}

#[test]
fn agree_on_needles_the_block_holds() {
    let codes = block();
    for count in [1, 2, 3, 8, 9, 12, 16, 17, 24, 64] {
        let needles: Vec<Token> = (0..count).map(|k| codes[k * (BLOCK / 64)]).collect();
        let cover = ProbeCover::new(needles.to_vec(), vec![]);
        assert_eq!(
            expected(&cover, &codes)
                .iter()
                .map(|word| word.count_ones())
                .sum::<u32>(),
            count as u32,
            "wrong hits at K = {count}"
        );
        every_matcher_ranged(cover, 1000, 2000, &codes);
    }
}

#[test]
fn agree_on_needles_the_block_does_not_hold() {
    let mut codes = block();
    codes.fill(7);
    let needles: Vec<Token> = (0..4).map(|k| 60000 + k).collect();
    every_matcher(&ProbeCover::new(needles.to_vec(), vec![]), &codes);
}

/// A hit at the last position of a block, which for L = 1 is the last code
/// the mask covers rather than anything the lookahead reaches.
#[test]
fn agree_on_a_hit_at_the_seam() {
    let mut codes = [7 as Token; BLOCK];
    codes[BLOCK - 1] = 60000;
    let cover = ProbeCover::new(vec![60000], vec![]);
    assert_eq!(
        expected(&cover, &codes)[BLOCK / 64 - 1],
        1 << 63,
        "the definition missed the seam hit"
    );
    every_matcher(&cover, &codes);
}

/// Narrowing keeps the low byte of each lane, so two codes that share one
/// must not be confused for each other.
#[test]
fn agree_on_codes_sharing_a_low_byte() {
    let mut codes = [0x0102 as Token; BLOCK];
    let at = BLOCK - 58;
    codes[at] = 0x0202;
    let cover = ProbeCover::new(vec![0x0202], vec![]);
    assert_eq!(
        expected(&cover, &codes)[at / 64],
        1 << (at % 64),
        "the definition missed the planted code"
    );
    // A range whose codes share the low byte of every code in the block but
    // are none of them: narrowing its compare has to keep the high byte.
    every_matcher_ranged(cover, 0x0302, 0x0402, &codes);
}

/// A range at both ends of the code space, and over a run the block holds
/// nothing of: it holds nothing at or above 50021.
#[test]
fn agree_on_ranges_of_wide_codes() {
    let codes = block();
    for (lo, hi) in [
        (0 as Token, Token::MAX),
        (0, 0),
        (37, 37),
        (1000, 2000),
        (50000, 50020),
        (50021, Token::MAX),
    ] {
        let ranges = [TokenRange {
            begin: lo,
            last: hi,
        }];
        every_matcher(&ProbeCover::new(vec![], ranges.to_vec()), &codes);
        every_matcher(
            &ProbeCover::new(vec![0 as Token, 30000, 50020], ranges.to_vec()),
            &codes,
        );
    }
    let all = expected(
        &ProbeCover::new(
            vec![],
            vec![TokenRange {
                begin: 0,
                last: Token::MAX,
            }],
        ),
        &codes,
    );
    assert!(
        all.iter().all(|&word| word == !0),
        "a code outside the width"
    );
    let none = expected(
        &ProbeCover::new(
            vec![],
            vec![TokenRange {
                begin: 50021,
                last: Token::MAX,
            }],
        ),
        &codes,
    );
    assert!(
        none.iter().all(|&word| word == 0),
        "a code at or above 50021"
    );
}

/// Probes on both sides of 0x8000 and a range across it. AVX2 has no
/// unsigned 16-bit compare, so a range written with the signed one would
/// put 0x8000 below 0x7fff; the kernels must not.
#[test]
fn agree_on_the_sign_boundary() {
    let mut codes = [0 as Token; BLOCK];
    for (at, code) in codes.iter_mut().enumerate() {
        *code = [0x7ffe, 0x7fff, 0x8000, 0x8001, 0x8002, 7][at % 6];
    }
    for needles in [
        vec![0x7fff],
        vec![0x8000],
        vec![0x8001],
        vec![0x7fff, 0x8000, 0x8001],
    ] {
        every_matcher(&ProbeCover::new(needles, vec![]), &codes);
    }
    for (lo, hi) in [
        (0x7ffe, 0x8001),
        (0x7fff, 0x7fff),
        (0x8000, 0x8000),
        (0, 0x7fff),
        (0x8000, Token::MAX),
    ] {
        let range = TokenRange {
            begin: lo,
            last: hi,
        };
        every_matcher(&ProbeCover::new(vec![], vec![range]), &codes);
        every_matcher(&ProbeCover::new(vec![7], vec![range]), &codes);
    }
    let across = expected(
        &ProbeCover::new(
            vec![],
            vec![TokenRange {
                begin: 0x7ffe,
                last: 0x8001,
            }],
        ),
        &codes,
    );
    assert_eq!(
        across[0] & 0x3f,
        0b001111,
        "the definition misplaced 0x8000"
    );
}

/// Probes at the top of the code space, in a block that holds those codes:
/// a range ending at `Token::MAX` has nothing above it to reject, and the
/// width arithmetic must not wrap.
#[test]
fn agree_at_the_top_of_the_code_space() {
    let mut codes = [0 as Token; BLOCK];
    for (at, code) in codes.iter_mut().enumerate() {
        *code = [65527, 65531, 65532, 65533, 65534, Token::MAX, 7][at % 7];
    }
    for needles in [vec![65527], vec![Token::MAX], vec![65527, Token::MAX]] {
        every_matcher(&ProbeCover::new(needles, vec![]), &codes);
    }
    for (lo, hi) in [
        (65532, Token::MAX),
        (Token::MAX, Token::MAX),
        (65528, 65531),
    ] {
        let range = TokenRange {
            begin: lo,
            last: hi,
        };
        every_matcher(&ProbeCover::new(vec![], vec![range]), &codes);
        every_matcher(&ProbeCover::new(vec![65527], vec![range]), &codes);
    }
}

/// The codes just outside either end of what the cover admits, with a
/// range setting one end.
#[test]
fn table_rejects_the_codes_around_its_own() {
    let mut codes = [0 as Token; BLOCK];
    for (at, code) in codes.iter_mut().enumerate() {
        *code = [999, 1000, 3000, 3001, 0, Token::MAX][at % 6];
    }
    let ranges = [TokenRange {
        begin: 2500 as Token,
        last: 3000,
    }];
    every_matcher(
        &ProbeCover::new(vec![1000 as Token], ranges.to_vec()),
        &codes,
    );
    every_matcher(&ProbeCover::new(vec![1000 as Token, 3000], vec![]), &codes);
    let bits = mask::<Table>(&ProbeCover::new(vec![1000 as Token, 3000], vec![]), &codes);
    assert_eq!(bits[0] & 0x3f, 0b000110);
}

/// One planted code, one hit and no more, through the kernel every target
/// compiles.
#[test]
fn the_table_masks_one_code() {
    let codes = block();
    let at = BLOCK - 28;
    let bits = mask::<Table>(&ProbeCover::new(codes[at..at + 1].to_vec(), vec![]), &codes);
    assert_eq!(bits[at / 64] >> (at % 64) & 1, 1, "missed the code");
    assert_eq!(
        bits.iter().map(|word| word.count_ones()).sum::<u32>(),
        1,
        "extra hits"
    );
}

/// And the same through the block driver, whose tail is zero-padded and
/// trimmed to the codes the stream holds.
#[test]
fn the_driver_scans_every_block() {
    // Two and a bit blocks, so both the whole-block and the padded tail path
    // run, with the planted hit inside the tail.
    let codes: Vec<Token> = (0..2 * BLOCK + 100)
        .map(|at| (at * 37 % 50021) as Token)
        .collect();
    let row_offsets: Vec<u32> = (0..=codes.len() as u32).step_by(64).collect();
    let at = 2 * BLOCK + 40;
    let cover = ProbeCover::new(codes[at..at + 1].to_vec(), vec![]);
    let mut found = Vec::new();
    scan::<Table, resolver::LinearSeek<'_, u32>>(
        &cover,
        &codes,
        &row_offsets,
        Check::SUPERSET,
        &mut found,
    );
    assert_eq!(found, vec![at / 64]);

    // The same layer at the wide offset width.
    let wide: Vec<u64> = row_offsets.iter().map(|&o| u64::from(o)).collect();
    found.clear();
    scan::<Table, resolver::GallopSeek<'_, u64>>(
        &cover,
        &codes,
        &wide,
        Check::SUPERSET,
        &mut found,
    );
    assert_eq!(found, vec![at / 64]);
}

/// The tail is padded with zeros, so a zero the padding carries must not
/// reach a resolver, whatever the stream length leaves in the last block.
#[test]
fn padding_makes_no_candidate() {
    for length in [1, 2, 3, 5, BLOCK, BLOCK + 1, BLOCK + 2, BLOCK + 3] {
        // No zero in the stream, so every zero a token matches is padding.
        let codes: Vec<Token> = (0..length).map(|at| (at % 255 + 1) as Token).collect();
        let row_offsets = vec![0, length as u32];
        let mut found = Vec::new();
        scan::<Table, resolver::LinearSeek<'_, u32>>(
            &ProbeCover::new(vec![0 as Token], vec![]),
            &codes,
            &row_offsets,
            Check::SUPERSET,
            &mut found,
        );
        assert!(found.is_empty(), "over {length} codes: {found:?}");
    }
}
