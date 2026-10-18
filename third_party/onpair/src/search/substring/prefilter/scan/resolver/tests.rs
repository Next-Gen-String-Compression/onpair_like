// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! The resolver against [`expected_rows`], at both offset widths.

use super::{GallopSeek, Mask, expected_rows};
use crate::core::offset::Offset;
use crate::search::substring::prefilter::scan::{BLOCK_SIZE_SCAN_BLOCKS, Check, WORDS};

/// Blocks per case: enough that a row can span one whole block and still
/// start and end inside the stream.
const BLOCKS: usize = 4;
const CODES: usize = BLOCKS * BLOCK_SIZE_SCAN_BLOCKS;

/// The resolver over a whole mask, block by block, skipping the empty blocks
/// the matcher's flag would have skipped.
fn resolve<O: Offset>(mask: &[u64], row_offsets: &[O]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut resolver = GallopSeek::new(row_offsets);
    for (block, bits) in mask.chunks_exact(WORDS).enumerate() {
        let bits: &Mask = bits.try_into().unwrap();
        if bits.iter().any(|&set| set != 0) {
            resolver.rows(
                bits,
                block * BLOCK_SIZE_SCAN_BLOCKS,
                Check::SUPERSET,
                &mut out,
            );
        }
    }
    out
}

/// The rows the mask names, first checked for the properties the contract
/// puts on them: ascending, without repeats, and one row per hit at least.
fn expected(mask: &[u64], row_offsets: &[u32]) -> Vec<usize> {
    let rows = expected_rows(mask, row_offsets);
    assert!(
        rows.windows(2).all(|pair| pair[0] < pair[1]),
        "the expectation is not ascending and unique: {rows:?}"
    );
    let bits: u32 = mask.iter().map(|set| set.count_ones()).sum();
    assert_eq!(rows.is_empty(), bits == 0, "{bits} hits and no rows");
    rows
}

fn resolves(case: &str, mask: &[u64], row_offsets: &[u32]) {
    let expected = expected(mask, row_offsets);
    assert_eq!(resolve(mask, row_offsets), expected, "{case}");
    // The same layer at the wide offset width, which the resolver reads in
    // place rather than narrowing.
    let wide: Vec<u64> = row_offsets
        .iter()
        .map(|&offset| u64::from(offset))
        .collect();
    assert_eq!(resolve(mask, &wide), expected, "u64 on {case}");
}

/// A row layer over the whole stream: rows of `codes` codes, the last one
/// stretched to the end so every code has a row.
fn uniform(codes: usize) -> Vec<u32> {
    let mut offsets: Vec<u32> = (0..CODES as u32).step_by(codes).collect();
    offsets.push(CODES as u32);
    offsets
}

/// Rows of wildly uneven length, a third of them empty. Empty rows are the
/// ones a cursor has to walk past without emitting.
fn ragged() -> Vec<u32> {
    let mut offsets = vec![0u32];
    let mut state = 0x2545_F491u32;
    while *offsets.last().unwrap() < CODES as u32 {
        state = state.wrapping_mul(0x0019_660D).wrapping_add(0x3C6E_F35F);
        let len = match state >> 30 {
            0 => 0,
            1 => 1 + state % 8,
            2 => 1 + state % 512,
            _ => 1 + state % 4096,
        };
        offsets.push((offsets.last().unwrap() + len).min(CODES as u32));
    }
    offsets
}

fn empty_mask() -> Vec<u64> {
    vec![0u64; CODES / 64]
}

/// A mask with a bit every `stride` codes.
fn strided(stride: usize) -> Vec<u64> {
    let mut mask = empty_mask();
    for code in (0..CODES).step_by(stride) {
        mask[code / 64] |= 1 << (code % 64);
    }
    mask
}

fn layers() -> Vec<(String, Vec<u32>)> {
    let mut layers = vec![
        ("ragged".to_string(), ragged()),
        ("one row".to_string(), vec![0, CODES as u32]),
    ];
    for codes in [
        1,
        3,
        8,
        64,
        65,
        1024,
        BLOCK_SIZE_SCAN_BLOCKS,
        BLOCK_SIZE_SCAN_BLOCKS + 7,
        3 * BLOCK_SIZE_SCAN_BLOCKS,
    ] {
        layers.push((format!("{codes} codes per row"), uniform(codes)));
    }
    layers
}

#[test]
fn agree_on_every_density() {
    for (name, row_offsets) in layers() {
        resolves(&format!("{name}, empty"), &empty_mask(), &row_offsets);
        resolves(
            &format!("{name}, full"),
            &vec![u64::MAX; CODES / 64],
            &row_offsets,
        );
        for stride in [
            1,
            2,
            7,
            64,
            97,
            BLOCK_SIZE_SCAN_BLOCKS,
            5 * BLOCK_SIZE_SCAN_BLOCKS / 4,
        ] {
            resolves(
                &format!("{name}, every {stride}th code"),
                &strided(stride),
                &row_offsets,
            );
        }
    }
}

/// The bits the resolver has to read but must not emit twice: two hits in one
/// row, on either side of a block boundary.
#[test]
fn agree_on_a_row_hit_in_two_blocks() {
    let row_offsets = vec![
        0,
        (BLOCK_SIZE_SCAN_BLOCKS / 4) as u32,
        (BLOCK_SIZE_SCAN_BLOCKS + BLOCK_SIZE_SCAN_BLOCKS / 2) as u32,
        CODES as u32,
    ];
    for pair in [
        [BLOCK_SIZE_SCAN_BLOCKS - 1, BLOCK_SIZE_SCAN_BLOCKS],
        [
            BLOCK_SIZE_SCAN_BLOCKS - 1,
            BLOCK_SIZE_SCAN_BLOCKS + BLOCK_SIZE_SCAN_BLOCKS / 2 - 1,
        ],
        [
            BLOCK_SIZE_SCAN_BLOCKS / 4 + BLOCK_SIZE_SCAN_BLOCKS / 8,
            BLOCK_SIZE_SCAN_BLOCKS + BLOCK_SIZE_SCAN_BLOCKS / 2 - 1,
        ],
    ] {
        let mut mask = empty_mask();
        for code in pair {
            mask[code / 64] |= 1 << (code % 64);
        }
        assert_eq!(
            expected(&mask, &row_offsets),
            vec![1],
            "row 1 not emitted once for {pair:?}"
        );
        resolves(&format!("hits at {pair:?}"), &mask, &row_offsets);
    }
}

/// A hit in the first and the last code of the stream, the positions where a
/// cursor or a bracket can fall off its end.
#[test]
fn agree_on_the_ends_of_the_stream() {
    for layer in [uniform(1), uniform(64), ragged()] {
        for code in [0, CODES - 1] {
            let mut mask = empty_mask();
            mask[code / 64] |= 1 << (code % 64);
            resolves(&format!("hit at {code}"), &mask, &layer);
        }
    }
}

/// A single hit far into the stream: what the gallop's doubling is for, and
/// the one case where it must not overshoot the row it lands on.
#[test]
fn agree_on_one_hit_after_many_rows() {
    let row_offsets = uniform(3);
    for code in [
        1,
        2,
        3,
        4,
        8,
        BLOCK_SIZE_SCAN_BLOCKS - 1,
        BLOCK_SIZE_SCAN_BLOCKS,
        BLOCK_SIZE_SCAN_BLOCKS + 1,
        CODES - 2,
    ] {
        let mut mask = empty_mask();
        mask[code / 64] |= 1 << (code % 64);
        let rows = expected(&mask, &row_offsets);
        assert_eq!(rows, vec![code / 3], "lost the hit at {code}");
        resolves(&format!("one hit at {code}"), &mask, &row_offsets);
    }
}
