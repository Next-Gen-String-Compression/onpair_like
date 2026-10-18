//! Selection vectors: the rows an upstream predicate already admitted, as a
//! candidate strategy sees them (ABI v7). Membership is decided from the
//! global row index and a seed, so the same rows are selected at every
//! `chunk_rows`, and truth under a selection stays chunk-invariant like the
//! truth itself.
//!
//! Two shapes besides `all`:
//!
//! - `uniform`: row r is selected when `xxh3(r, seed) < fraction * 2^64`.
//!   Independent per row, geometric gaps, the hardest shape for anything
//!   that skips blocks.
//! - `runs`: one run per period of `avg_run / fraction` rows. Period k draws
//!   its run length uniformly over `run_rows_min..=run_rows_max` and places
//!   the run at a uniform offset inside the period, so runs never merge, the
//!   mean run length is the midpoint, and the fraction is exact up to the
//!   last partial period.

use lb_abi::LbSelection;
use xxhash_rust::xxh3::xxh3_64_with_seed;

use crate::bitmap::Bitmap;
use crate::spec::SelectionSpec;

pub struct Selection {
    /// Stable name for result keys: `all`, `uniform-f0.1-s7`,
    /// `runs-32-512-f0.1-s7`.
    pub id: String,
    /// Whole-dataset membership; `None` for `all`.
    pub bitmap: Option<Bitmap>,
}

/// One selection cut to one chunking: the chunk-local ascending lists and
/// the C descriptors over them. Empty for `all`.
pub struct ChunkSelections {
    pub lists: Vec<Vec<u32>>,
    pub ffi: Vec<LbSelection>,
}

fn unit(hash: u64) -> f64 {
    (hash >> 11) as f64 / (1u64 << 53) as f64
}

fn draw(seed: u64, k: u64) -> f64 {
    unit(xxh3_64_with_seed(&k.to_le_bytes(), seed))
}

/// Rows per period of a `runs` selection; a run never outgrows it because
/// the spec validation requires `fraction <= avg / run_rows_max`.
pub fn runs_period(run_rows_min: u64, run_rows_max: u64, fraction: f64) -> u64 {
    let avg = (run_rows_min + run_rows_max) as f64 / 2.0;
    ((avg / fraction).round() as u64).max(run_rows_max)
}

impl Selection {
    pub fn build(spec: &SelectionSpec, num_rows: u64) -> Selection {
        match spec.kind.as_str() {
            "all" => Selection {
                id: "all".into(),
                bitmap: None,
            },
            "uniform" => {
                let fraction = spec.fraction.unwrap();
                let seed = spec.seed;
                let mut bm = Bitmap::new(num_rows);
                for r in 0..num_rows {
                    if draw(seed, r) < fraction {
                        bm.set(r);
                    }
                }
                Selection {
                    id: format!("uniform-f{fraction}-s{seed}"),
                    bitmap: Some(bm),
                }
            }
            "runs" => {
                let (min, max) = (spec.run_rows_min.unwrap(), spec.run_rows_max.unwrap());
                let fraction = spec.fraction.unwrap();
                let seed = spec.seed;
                let period = runs_period(min, max, fraction);
                let mut bm = Bitmap::new(num_rows);
                for k in 0..num_rows.div_ceil(period) {
                    let len = (min + (draw(seed, 2 * k) * (max - min + 1) as f64) as u64).min(max);
                    let offset = ((draw(seed, 2 * k + 1) * (period - len + 1) as f64) as u64)
                        .min(period - len);
                    let start = k * period + offset;
                    for r in start..(start + len).min(num_rows) {
                        bm.set(r);
                    }
                }
                Selection {
                    id: format!("runs-{min}-{max}-f{fraction}-s{seed}"),
                    bitmap: Some(bm),
                }
            }
            other => unreachable!("spec validation admits no selection kind {other:?}"),
        }
    }

    /// Selected rows; `None` for `all`.
    pub fn rows(&self) -> Option<u64> {
        self.bitmap.as_ref().map(Bitmap::count)
    }

    /// `chunks` yields each chunk's `(start_row, num_rows)` in order.
    pub fn per_chunk(&self, chunks: impl Iterator<Item = (u64, u64)>) -> ChunkSelections {
        let Some(bm) = &self.bitmap else {
            return ChunkSelections {
                lists: Vec::new(),
                ffi: Vec::new(),
            };
        };
        let lists: Vec<Vec<u32>> = chunks
            .map(|(start, n)| {
                (start..start + n)
                    .filter(|&r| bm.get(r))
                    .map(|r| (r - start) as u32)
                    .collect()
            })
            .collect();
        let ffi = lists
            .iter()
            .map(|l| LbSelection {
                rows: l.as_ptr(),
                count: l.len() as u64,
            })
            .collect();
        ChunkSelections { lists, ffi }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: &str, fraction: f64, min: u64, max: u64, seed: u64) -> SelectionSpec {
        SelectionSpec {
            kind: kind.into(),
            fraction: Some(fraction),
            run_rows_min: Some(min),
            run_rows_max: Some(max),
            seed,
        }
    }

    #[test]
    fn uniform_hits_its_fraction_and_is_seeded() {
        let a = Selection::build(&spec("uniform", 0.1, 0, 0, 7), 1 << 20);
        let b = Selection::build(&spec("uniform", 0.1, 0, 0, 7), 1 << 20);
        let c = Selection::build(&spec("uniform", 0.1, 0, 0, 8), 1 << 20);
        let n = a.rows().unwrap() as f64 / (1u64 << 20) as f64;
        assert!((n - 0.1).abs() < 0.003, "fraction {n}");
        assert!(a.bitmap == b.bitmap, "same seed, same rows");
        assert!(a.bitmap != c.bitmap, "another seed, other rows");
        assert_eq!(a.id, "uniform-f0.1-s7");
    }

    #[test]
    fn runs_have_bounded_lengths_and_the_right_fraction() {
        let n = 1u64 << 20;
        let s = Selection::build(&spec("runs", 0.1, 32, 512, 3), n);
        let bm = s.bitmap.as_ref().unwrap();
        let frac = bm.count() as f64 / n as f64;
        assert!((frac - 0.1).abs() < 0.01, "fraction {frac}");
        let mut lens = Vec::new();
        let mut r = 0;
        while r < n {
            if bm.get(r) {
                let start = r;
                while r < n && bm.get(r) {
                    r += 1;
                }
                lens.push(r - start);
            } else {
                r += 1;
            }
        }
        assert!(lens.iter().all(|&l| (32..=512).contains(&l)), "{lens:?}");
        let mean = lens.iter().sum::<u64>() as f64 / lens.len() as f64;
        assert!((mean - 272.0).abs() < 30.0, "mean run {mean}");
        assert_eq!(s.id, "runs-32-512-f0.1-s3");
    }

    #[test]
    fn per_chunk_lists_are_local_ascending_and_complete() {
        let n = 1000u64;
        let s = Selection::build(&spec("uniform", 0.3, 0, 0, 1), n);
        // 128-row chunks, the last one ragged.
        let chunks: Vec<(u64, u64)> = (0..n).step_by(128).map(|s| (s, 128.min(n - s))).collect();
        let cs = s.per_chunk(chunks.iter().copied());
        let mut seen = 0u64;
        for (&(_, rows), list) in chunks.iter().zip(&cs.lists) {
            assert!(list.windows(2).all(|w| w[0] < w[1]));
            assert!(list.iter().all(|&r| (r as u64) < rows));
            seen += list.len() as u64;
        }
        assert_eq!(seen, s.rows().unwrap());
        assert_eq!(cs.ffi.len(), chunks.len());
        assert!(Selection::build(&SelectionSpec::all(), n).per_chunk(chunks.iter().copied()).ffi.is_empty());
    }
}
