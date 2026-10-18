// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Stage two: bit mask in, candidate rows out.

#[cfg(test)]
mod tests;

use super::{Check, Mask};
use crate::core::offset::Offset;

/// Appends the rows the set bits fall in, ascending and without repeats, one
/// block of mask at a time. Blocks arrive in stream order, only the non-empty
/// ones arrive at all, and a row can span several, so the cursor carries over
/// between calls. A hit's row is found by doubling the step from the cursor
/// until the offsets overshoot, then binary searching that bracket. A row is
/// emitted on its first hit that passes the check and the bit search resumes
/// at the row's end, so its further hits are never read. A hit that fails
/// moves the search on by one bit.
pub(in crate::search::substring::prefilter::scan) struct GallopSeek<'a, O> {
    /// Read in place at its stored width, never copied.
    row_offsets: &'a [O],
    row: usize,
    /// One past the last code of `row`, in stream indices. Zero before the
    /// first hit.
    row_end: usize,
    /// `row_end` of the last row emitted; a bit below it is a repeat of a row
    /// already out. Zero before the first emitted row.
    resolved_code: usize,
}

impl<'a, O: Offset> GallopSeek<'a, O> {
    pub(in crate::search::substring::prefilter::scan) fn new(row_offsets: &'a [O]) -> Self {
        Self {
            row_offsets,
            row: 0,
            row_end: 0,
            resolved_code: 0,
        }
    }

    /// `first_code` is the stream index of bit 0. The row layer covers every
    /// code a bit can be set for, so a set bit always has a row.
    #[inline]
    pub(in crate::search::substring::prefilter::scan) fn rows(
        &mut self,
        bits: &Mask,
        first_code: usize,
        check: Check<'_>,
        out: &mut Vec<usize>,
    ) {
        let mut at = self.resolved_code.saturating_sub(first_code);
        while let Some(hit) = next_set(bits, at) {
            let code = first_code + hit;
            if code >= self.row_end {
                self.row = gallop(self.row_offsets, self.row, code);
                self.row_end = self.row_offsets[self.row + 1].to_usize();
            }
            if check.passes(code, self.row_offsets[self.row].to_usize(), self.row_end) {
                out.push(self.row);
                self.resolved_code = self.row_end;
                at = self.row_end - first_code;
            } else {
                at = hit + 1;
            }
        }
    }
}

/// The row holding `code`, from a cursor at or before it.
#[inline]
fn gallop<O: Offset>(row_offsets: &[O], from: usize, code: usize) -> usize {
    let mut step = 1;
    while from + step < row_offsets.len() && row_offsets[from + step].to_usize() <= code {
        step *= 2;
    }
    let lo = from + step / 2;
    let hi = (from + step).min(row_offsets.len());
    lo + row_offsets[lo + 1..hi].partition_point(|&offset| offset.to_usize() <= code)
}

/// The set bit at or after `from`, `None` if the block holds none past it.
#[inline]
fn next_set(bits: &Mask, from: usize) -> Option<usize> {
    let mut word = from / 64;
    let mut set = bits.get(word)? & (u64::MAX << (from % 64));
    while set == 0 {
        word += 1;
        set = *bits.get(word)?;
    }
    Some(word * 64 + set.trailing_zeros() as usize)
}

/// The rows a mask names, worked out in one pass from the layer rather than
/// walked: what the resolver has to arrive at, without being one. The bench
/// and the tests check against this. Nothing outside them asks.
#[cfg(test)]
pub(in crate::search::substring::prefilter::scan) fn expected_rows<O: Offset>(
    mask: &[u64],
    row_offsets: &[O],
) -> Vec<usize> {
    let mut rows = Vec::new();
    for (word, &set) in mask.iter().enumerate() {
        let mut set = set;
        while set != 0 {
            let code = word * 64 + set.trailing_zeros() as usize;
            set &= set - 1;
            // The first offset past the code, less one, is the row holding it;
            // an empty row shares its start and loses the tie, as it should.
            let row = row_offsets.partition_point(|&offset| offset.to_usize() <= code) - 1;
            if rows.last() != Some(&row) {
                rows.push(row);
            }
        }
    }
    rows
}
