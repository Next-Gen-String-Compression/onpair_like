//! Chunking: chunk size is a prepare-time run-spec parameter (DESIGN.md §6).
//!
//! The canonical artifact stays one contiguous column; at prepare time it is
//! sliced into contiguous runs of `chunk_rows` rows (last chunk ragged),
//! materializing per-chunk views with rebased offsets — one-time setup cost,
//! outside all measurement. `chunk_rows` must be a multiple of 64 so every
//! chunk owns whole words of the global result bitmap; `chunk_rows == 0`
//! means a single chunk over the whole dataset (the default).

use crate::dataset::PreparedDataset;
use lb_abi::LbChunkView;

pub struct Chunk<'a> {
    pub start_row: u64,
    pub num_rows: u64,
    pub payload_bytes: u64,
    /// Rebased offsets, materialized only when start_row > 0; the first
    /// chunk points straight into the dataset's own offsets (zero-copy).
    rebased: Option<Vec<u64>>,
    borrowed: &'a PreparedDataset,
}

impl<'a> Chunk<'a> {
    /// The C-ABI view of this chunk. Pointers are valid as long as the
    /// `Chunks` value (and the dataset) live.
    pub fn view(&self) -> LbChunkView {
        let off = self.offsets();
        LbChunkView {
            bytes: unsafe {
                self.borrowed
                    .payload()
                    .as_ptr()
                    .add(self.borrowed.offsets_u64()[self.start_row as usize] as usize)
            },
            offsets: off.as_ptr(),
            num_rows: self.num_rows,
        }
    }

    pub fn offsets(&self) -> &[u64] {
        match &self.rebased {
            Some(v) => v,
            None => &self.borrowed.offsets_u64()[..self.num_rows as usize + 1],
        }
    }

    /// This chunk's whole-word slice of the global bitmap.
    pub fn bitmap_word_range(&self) -> std::ops::Range<usize> {
        debug_assert_eq!(self.start_row % 64, 0);
        let start = (self.start_row / 64) as usize;
        start..start + lb_abi::bitmap_words(self.num_rows)
    }
}

pub struct Chunks<'a> {
    pub chunk_rows_param: u64,
    pub chunks: Vec<Chunk<'a>>,
}

pub fn slice(ds: &PreparedDataset, chunk_rows: u64) -> Result<Chunks<'_>, String> {
    let n = ds.num_rows();
    let per = if chunk_rows == 0 { n } else { chunk_rows };
    if chunk_rows != 0 && chunk_rows % 64 != 0 {
        return Err(format!(
            "chunk_rows must be a multiple of 64 (got {chunk_rows}) so chunks own whole bitmap words"
        ));
    }
    let global = ds.offsets_u64();
    let mut chunks = Vec::new();
    let mut start = 0u64;
    while start < n {
        let rows = per.min(n - start);
        let base = global[start as usize];
        let rebased = if start == 0 {
            None // offsets already start at 0: zero-copy
        } else {
            Some(
                global[start as usize..=(start + rows) as usize]
                    .iter()
                    .map(|&o| o - base)
                    .collect::<Vec<u64>>(),
            )
        };
        chunks.push(Chunk {
            start_row: start,
            num_rows: rows,
            payload_bytes: global[(start + rows) as usize] - base,
            rebased,
            borrowed: ds,
        });
        start += rows;
    }
    Ok(Chunks {
        chunk_rows_param: chunk_rows,
        chunks,
    })
}

#[cfg(test)]
mod tests {
    // Chunk slicing is exercised end-to-end (including the chunk-invariance
    // gate check) in tests/pipeline.rs, which builds a real dataset.
}
