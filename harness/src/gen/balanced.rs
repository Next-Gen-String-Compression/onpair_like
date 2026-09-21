//! Balanced needle selection with unique byte strings within each suite.
//!
//! Selection is deliberately stratified, not uniform over occurrences: each
//! length/selectivity cell keeps a bounded reservoir of interval spans, then
//! spreads its quota across their available byte lengths. All positive families
//! are visited, so shortages can be distinguished from bounded negative search.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::sampled::Rng;
use super::substrings::{Family, SubstringIndex};
use crate::suite::Result;

pub const SUBSTRING_GENERATOR_VERSION: &str = "sa-lcp-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LengthBucket {
    pub min: usize,
    pub max: usize,
}

/// Inclusive, disjoint bounds on distinct matching rows, never occurrences.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RowBucket {
    pub min: u64,
    pub max: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BalancedRequest {
    pub seed: u64,
    pub lengths: Vec<LengthBucket>,
    pub matching_rows: Vec<RowBucket>,
    pub per_cell: usize,
    /// Maximum attempted substitutions per zero-match cell.
    pub negative_attempts: usize,
}

impl BalancedRequest {
    /// Stable output name for one dataset and generation configuration. Other
    /// datasets, experiment order, cache location and timings do not enter it.
    pub fn suite_key(&self, dataset_checksum: &str) -> Result<String> {
        let identity = serde_json::to_vec(&(SUBSTRING_GENERATOR_VERSION, dataset_checksum, self))?;
        let hash = xxhash_rust::xxh3::xxh3_64(&identity);
        Ok(format!(
            "{SUBSTRING_GENERATOR_VERSION}-s{}-n{}-{hash:016x}",
            self.seed, self.per_cell
        ))
    }

    /// 1–256 bytes, 20 per cell. Zero and one row are separate buckets;
    /// remaining integer boundaries span rare matches through 100% of rows.
    pub fn new(num_rows: u64, seed: u64) -> Self {
        let mut matching_rows = vec![RowBucket { min: 0, max: 0 }];
        if num_rows > 0 {
            matching_rows.push(RowBucket { min: 1, max: 1 });
        }
        let mut previous = 1;
        for ppm in [
            1u64, 10, 100, 1_000, 10_000, 50_000, 100_000, 200_000, 500_000, 800_000, 1_000_000,
        ] {
            let upper = (num_rows as u128 * ppm as u128 / 1_000_000) as u64;
            if upper > previous {
                matching_rows.push(RowBucket {
                    min: previous + 1,
                    max: upper,
                });
                previous = upper;
            }
        }
        Self {
            seed,
            per_cell: 20,
            negative_attempts: 4_000,
            lengths: [
                (1, 4),
                (5, 8),
                (9, 16),
                (17, 32),
                (33, 64),
                (65, 128),
                (129, 256),
            ]
            .into_iter()
            .map(|(min, max)| LengthBucket { min, max })
            .collect(),
            matching_rows,
        }
    }

    fn validate(&self, index: &SubstringIndex<'_>) -> Result<()> {
        if self.per_cell == 0 || self.per_cell > 10_000 {
            return Err("per-cell quota must be in 1..=10000".into());
        }
        if self.lengths.is_empty() || self.matching_rows.is_empty() {
            return Err("empty bucket grid".into());
        }
        let mut previous = 0;
        for b in &self.lengths {
            if b.min <= previous || b.min > b.max || b.max > index.max_len {
                return Err("length buckets must be sorted, disjoint, nonempty and within the indexed length".into());
            }
            previous = b.max;
        }
        let mut previous = None;
        for b in &self.matching_rows {
            if b.min > b.max
                || b.max > index.num_rows()
                || previous.is_some_and(|p| b.min <= p)
                || (b.min == 0 && b.max != 0)
            {
                return Err(
                    "row-count buckets must be sorted and disjoint; zero must have its own bucket"
                        .into(),
                );
            }
            previous = Some(b.max);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedNeedle {
    pub bytes: Vec<u8>,
    pub matching_rows: u64,
    pub cell: usize,
    /// Witness substring in the original payload (before any mutation).
    pub source_position: usize,
    /// Negative needles differ from the witness at exactly this byte.
    pub mutation_position: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellReport {
    pub cell: usize,
    pub length: LengthBucket,
    pub matching_rows: RowBucket,
    pub requested: usize,
    pub generated: usize,
    /// Exact number of eligible positive needles in this dataset.
    /// Unknown for negatives: mutation search does not enumerate all absences.
    pub available: Option<u64>,
    pub attempts: usize,
    /// filled | exhausted (positive catalogue) | unresolved (negative search).
    pub status: String,
    pub lengths: Vec<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneratedNeedles {
    pub generator: String,
    pub dataset_checksum: String,
    pub total_rows: u64,
    pub request: BalancedRequest,
    pub cells: Vec<CellReport>,
    pub needles: Vec<GeneratedNeedle>,
}

impl SubstringIndex<'_> {
    /// Generate one independent suite. Uniqueness spans its cells, while other
    /// datasets and generation calls have no effect on its needles.
    pub fn generate(&self, request: &BalancedRequest) -> Result<GeneratedNeedles> {
        request.validate(self)?;
        let mut rng = Rng::from_seed(request.seed);
        let nrows = request.matching_rows.len();
        let mut pools: Vec<_> = (0..request.lengths.len() * nrows)
            .map(|_| Reservoir::new(request.per_cell * 4))
            .collect();
        let mut parent_pools: Vec<_> = request
            .lengths
            .iter()
            .map(|_| Reservoir::new(request.per_cell * 4))
            .collect();
        let mut parent_rng = Rng::from_seed(request.seed ^ 0x6e65676174697665);
        let mut available = vec![0u64; pools.len()];
        self.visit(|family| {
            let r = request
                .matching_rows
                .iter()
                .position(|b| b.min <= family.matching_rows && family.matching_rows <= b.max);
            for (l, length) in request.lengths.iter().enumerate() {
                let lo = family.min_len.max(length.min);
                let hi = family.max_len.min(length.max);
                if lo > hi {
                    continue;
                }
                parent_pools[l].add(
                    Family {
                        min_len: lo,
                        max_len: hi,
                        ..family
                    },
                    &mut parent_rng,
                );
                let Some(r) = r else {
                    continue;
                };
                let cell = l * nrows + r;
                available[cell] += (hi - lo + 1) as u64;
                pools[cell].add(
                    Family {
                        min_len: lo,
                        max_len: hi,
                        ..family
                    },
                    &mut rng,
                );
            }
        });

        let mut result = GeneratedNeedles {
            generator: SUBSTRING_GENERATOR_VERSION.into(),
            dataset_checksum: self.checksum.clone(),
            total_rows: self.num_rows(),
            request: request.clone(),
            cells: Vec::new(),
            needles: Vec::new(),
        };
        let mut used = HashSet::new();
        let mut present = [false; 256];
        for &b in self.payload {
            present[b as usize] = true;
        }
        let alphabet: Vec<_> = (0..=255u8).filter(|&b| present[b as usize]).collect();
        for (l, length) in request.lengths.iter().enumerate() {
            for (r, rows) in request.matching_rows.iter().enumerate() {
                let cell = l * nrows + r;
                let mut report = CellReport {
                    cell,
                    length: length.clone(),
                    matching_rows: rows.clone(),
                    requested: request.per_cell,
                    generated: 0,
                    available: (rows.max != 0).then_some(available[cell]),
                    attempts: 0,
                    status: String::new(),
                    lengths: Vec::new(),
                };
                if rows.max != 0 {
                    let mut choices = length_choices(&pools[cell].items, length);
                    let mut usage = vec![0; length.max + 1];
                    while report.generated < request.per_cell {
                        let Some(len) = next_length(
                            length,
                            report.generated,
                            request.per_cell,
                            &usage,
                            &choices,
                        ) else {
                            break;
                        };
                        let pick = rng.below(choices[len].len() as u64) as usize;
                        let family = pools[cell].items[choices[len].swap_remove(pick)];
                        let bytes = self.payload[family.position..family.position + len].to_vec();
                        if !used.insert(bytes.clone()) {
                            return Err(
                                "duplicate positive needle: catalogue invariant violated".into()
                            );
                        }
                        usage[len] += 1;
                        report.lengths.push(len);
                        report.generated += 1;
                        result.needles.push(GeneratedNeedle {
                            bytes,
                            matching_rows: family.matching_rows,
                            cell,
                            source_position: family.position,
                            mutation_position: None,
                        });
                    }
                } else {
                    // Mutation parents need not belong to a requested positive cell.
                    result.needles.extend(self.negatives(
                        &parent_pools[l].items,
                        &alphabet,
                        &mut report,
                        &mut used,
                        &mut rng,
                        request.negative_attempts,
                    ));
                }
                report.status = if report.generated == request.per_cell {
                    "filled"
                } else if rows.max == 0 {
                    "unresolved"
                } else {
                    "exhausted"
                }
                .into();
                result.cells.push(report);
            }
        }
        Ok(result)
    }

    fn negatives(
        &self,
        parents: &[Family],
        alphabet: &[u8],
        report: &mut CellReport,
        used: &mut HashSet<Vec<u8>>,
        rng: &mut Rng,
        attempt_limit: usize,
    ) -> Vec<GeneratedNeedle> {
        let mut needles = Vec::new();
        let choices = length_choices(parents, &report.length);
        let mut usage = vec![0; report.length.max + 1];
        if alphabet.len() < 2 {
            return needles;
        }
        let mut attempted = HashSet::new();
        while report.generated < report.requested && report.attempts < attempt_limit {
            // Rotate targets on failure as well, so an impossible length does
            // not prevent trying other lengths in the same bucket.
            let Some(len) = next_length(
                &report.length,
                report.attempts % report.requested,
                report.requested,
                &usage,
                &choices,
            ) else {
                break;
            };
            report.attempts += 1;
            usage[len] += 1;
            let parent = parents[choices[len][rng.below(choices[len].len() as u64) as usize]];
            let mut bytes = self.payload[parent.position..parent.position + len].to_vec();
            let position = rng.below(len as u64) as usize;
            let original = bytes[position];
            let mut replacement = alphabet[rng.below(alphabet.len() as u64) as usize];
            while replacement == original {
                replacement = alphabet[rng.below(alphabet.len() as u64) as usize];
            }
            bytes[position] = replacement;
            if used.contains(&bytes) || !attempted.insert(bytes.clone()) || self.contains(&bytes) {
                continue;
            }
            used.insert(bytes.clone());
            report.lengths.push(len);
            report.generated += 1;
            needles.push(GeneratedNeedle {
                bytes,
                matching_rows: 0,
                cell: report.cell,
                source_position: parent.position,
                mutation_position: Some(position),
            });
        }
        needles
    }
}

/// Uniform reservoir over interval spans, not over substring occurrences.
struct Reservoir {
    capacity: usize,
    seen: u64,
    items: Vec<Family>,
}
impl Reservoir {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            seen: 0,
            items: Vec::new(),
        }
    }
    fn add(&mut self, item: Family, rng: &mut Rng) {
        self.seen += 1;
        if self.items.len() < self.capacity {
            self.items.push(item);
        } else {
            let slot = rng.below(self.seen) as usize;
            if slot < self.capacity {
                self.items[slot] = item;
            }
        }
    }
}

fn length_choices(families: &[Family], bucket: &LengthBucket) -> Vec<Vec<usize>> {
    let mut choices = vec![Vec::new(); bucket.max + 1];
    for (i, f) in families.iter().enumerate() {
        for choice in &mut choices[f.min_len..=f.max_len] {
            choice.push(i);
        }
    }
    choices
}

fn next_length(
    bucket: &LengthBucket,
    draw: usize,
    quota: usize,
    usage: &[usize],
    choices: &[Vec<usize>],
) -> Option<usize> {
    let target = bucket.min + (2 * draw + 1) * (bucket.max - bucket.min + 1) / (2 * quota);
    (bucket.min..=bucket.max)
        .filter(|&len| !choices[len].is_empty())
        .min_by_key(|&len| (usage[len], len.abs_diff(target), len))
}
