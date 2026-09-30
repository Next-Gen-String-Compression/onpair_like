//! Shared hit-to-row resolution for haystack scans.
//!
//! A haystack scan searches a chunk's concatenated payload in one pass instead
//! of restarting the search kernel on every row, then maps each occurrence back
//! to the row that holds it. Every such scan must resolve hits the same way, so
//! that differences between strategies come from their search kernels and not
//! from their bookkeeping; this module is that single definition.
//!
//! [`haystack_rows`] visits hits in ascending payload order and resolves each
//! one with a forward-only [`RowCursor`]. After a hit wholly inside a row, the
//! search resumes at the end of that row: the row is decided, so later
//! occurrences in it cannot change the result. A hit that straddles a row
//! boundary is rejected and the search resumes one byte later, because a valid
//! occurrence of a periodic needle (`"aa"` across `...a|a...`) may start there.

/// Forward-only cursor from payload positions to rows of one chunk.
///
/// `offsets` is the chunk's row layer: `num_rows + 1` non-decreasing entries
/// with `offsets[0] == 0` (SEMANTICS.md). Positions passed to
/// [`row_of`](Self::row_of) must be non-decreasing and below the payload length
/// `offsets[num_rows]`.
#[derive(Debug)]
pub struct RowCursor<'a> {
    offsets: &'a [u64],
    row: usize,
}

impl<'a> RowCursor<'a> {
    /// Start before the first row.
    #[inline]
    pub fn new(offsets: &'a [u64]) -> Self {
        Self { offsets, row: 0 }
    }

    /// The row holding payload byte `pos`, skipping empty rows.
    ///
    /// Gallops forward from the previous answer: a hit in the same or the next
    /// row costs one or two comparisons, and a jump across `g` rows costs
    /// `O(log g)`. A binary search over the whole chunk would pay `O(log n)` on
    /// every hit and touch cold offsets far from the cursor.
    #[inline]
    pub fn row_of(&mut self, pos: u64) -> usize {
        let o = self.offsets;
        debug_assert!(pos < o[o.len() - 1], "position past the payload");
        debug_assert!(o[self.row] <= pos, "positions must be non-decreasing");
        // Find the first j > row with o[j] > pos; the answer is j - 1.
        let mut lo = self.row + 1;
        if o[lo] > pos {
            return self.row;
        }
        // Invariant: o[lo] <= pos. o[len - 1] > pos, so lo < len - 1.
        let mut step = 1;
        let mut hi = lo + step;
        while hi < o.len() && o[hi] <= pos {
            lo = hi;
            step *= 2;
            hi = lo + step;
        }
        let hi = hi.min(o.len() - 1);
        // o[lo] <= pos < o[hi], so the first j with o[j] > pos lies in (lo, hi].
        let j = lo + 1 + o[lo + 1..=hi].partition_point(|&x| x <= pos);
        self.row = j - 1;
        self.row
    }
}

/// Call `mark(row)` once, in ascending order, for every row of the chunk that
/// contains a `needle_len`-byte needle.
///
/// `find(hay)` returns the offset of the first occurrence of the needle in
/// `hay`, as `memchr::memmem::Finder::find` does. `needle_len` must be nonzero:
/// an empty needle matches every row and has no occurrences to resolve.
#[inline]
pub fn haystack_rows(
    payload: &[u8],
    offsets: &[u64],
    needle_len: usize,
    mut find: impl FnMut(&[u8]) -> Option<usize>,
    mut mark: impl FnMut(usize),
) {
    debug_assert!(needle_len > 0, "an empty needle matches every row");
    debug_assert_eq!(payload.len() as u64, offsets[offsets.len() - 1]);
    let mut cursor = RowCursor::new(offsets);
    let mut start = 0usize;
    while start < payload.len() {
        let Some(rel) = find(&payload[start..]) else {
            break;
        };
        let pos = start + rel;
        let row = cursor.row_of(pos as u64);
        let end = offsets[row + 1] as usize;
        if pos + needle_len <= end {
            mark(row);
            start = end;
        } else {
            start = pos + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_find(needle: &[u8]) -> impl Fn(&[u8]) -> Option<usize> + '_ {
        move |hay: &[u8]| {
            if hay.len() < needle.len() {
                return None;
            }
            (0..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
        }
    }

    fn resolve(rows: &[&[u8]], needle: &[u8]) -> Vec<usize> {
        let payload: Vec<u8> = rows.concat();
        let mut offsets = vec![0u64];
        for r in rows {
            offsets.push(offsets.last().unwrap() + r.len() as u64);
        }
        let mut out = vec![];
        haystack_rows(&payload, &offsets, needle.len(), naive_find(needle), |r| {
            out.push(r)
        });
        out
    }

    fn reference(rows: &[&[u8]], needle: &[u8]) -> Vec<usize> {
        let find = naive_find(needle);
        (0..rows.len())
            .filter(|&i| find(rows[i]).is_some())
            .collect()
    }

    #[test]
    fn rejects_matches_that_straddle_rows() {
        assert_eq!(resolve(&[b"xa", b"ab"], b"aa"), Vec::<usize>::new());
    }

    #[test]
    fn keeps_a_valid_overlap_after_a_rejected_straddle() {
        // "aa" straddles rows 0|1 at the first hit; row 1 still holds "aa".
        assert_eq!(resolve(&[b"xa", b"aab"], b"aa"), vec![1]);
    }

    #[test]
    fn marks_each_row_once_and_skips_empty_rows() {
        let rows: [&[u8]; 6] = [b"", b"abab", b"", b"", b"zab", b""];
        assert_eq!(resolve(&rows, b"ab"), vec![1, 4]);
    }

    #[test]
    fn handles_first_and_last_rows() {
        assert_eq!(resolve(&[b"ab", b"xx", b"ab"], b"ab"), vec![0, 2]);
        assert_eq!(resolve(&[b"ab"], b"ab"), vec![0]);
    }

    #[test]
    fn cursor_gallops_across_long_gaps() {
        let offsets: Vec<u64> = (0..=1000u64).map(|i| i * 3).collect();
        let mut c = RowCursor::new(&offsets);
        for (pos, row) in [(0, 0), (2, 0), (3, 1), (700, 233), (701, 233), (2999, 999)] {
            assert_eq!(c.row_of(pos), row, "pos {pos}");
        }
    }

    #[test]
    fn matches_a_per_row_search_on_random_chunks() {
        // xorshift64*: deterministic, no dependency.
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            s.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        for _ in 0..2000 {
            let nrows = (next() % 12) as usize;
            let rows: Vec<Vec<u8>> = (0..nrows)
                .map(|_| {
                    (0..next() % 7)
                        .map(|_| b"ab"[(next() % 2) as usize])
                        .collect()
                })
                .collect();
            let needle: Vec<u8> = (0..1 + next() % 3)
                .map(|_| b"ab"[(next() % 2) as usize])
                .collect();
            let rows: Vec<&[u8]> = rows.iter().map(|r| r.as_slice()).collect();
            assert_eq!(
                resolve(&rows, &needle),
                reference(&rows, &needle),
                "{rows:?} {needle:?}"
            );
        }
    }
}
