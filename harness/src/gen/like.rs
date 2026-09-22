//! `bench gen --method like` — the LIKE-shape generator ("gen2",
//! TODO_like_workload.md §10).
//!
//! The sampled generator draws literal needles for the five literal ops; the
//! suffix-array generator draws exactly-counted CONTAINS literals. Neither
//! produces a *pattern*. This one does: it takes the SA generator's literal
//! pool and synthesizes every LIKE shape the paper needs from it —
//!
//! ```text
//!   contains            %lit%            underscore_1     %l_t%
//!   prefix              lit%             underscore_2plus %l_t_r%
//!   suffix              %lit             mixed            %spec_al%requ_sts%
//!   multi_gap           %a%b%            anchored_gap_*   pre%lit% / %lit%suf / pre%suf
//! ```
//!
//! — stratified by pattern class × literal-length bucket × *measured*
//! selectivity bucket, and stored under the narrowest equivalent op
//! (`crate::like::lower`) so the literal-op roster keeps competing.
//!
//! ## Held-out mining
//!
//! Literals are mined only from the **mining pool**: rows whose index hashes
//! to `0 mod modulus` (default 8, a stable ~12.5% split), recorded in
//! `meta.gen.mining_pool`. Truth, and every timed scan, still use the whole
//! column. This keeps pattern discovery off the rows a compressor's symbol
//! table is most likely trained on; it is a modest guard against
//! overfitting, not a proof, and it is cheap.
//!
//! ## Two-stage probing, like the sampled generator
//!
//! The SA index over the pool gives each literal an exact row count *within
//! the pool* — a cheap, unbiased estimate of its full-column selectivity used
//! only to spread candidates across buckets. Every synthesized pattern is
//! then probed **exactly** against the full column with the oracle before it
//! is accepted, and the bucket it lands in is the measured one. An
//! underscore mutation is re-probed from scratch; it never inherits its
//! parent's count. `bench bless` re-derives everything regardless.
//!
//! ## Honesty about gaps
//!
//! Every (class, length, selectivity) cell is reported filled, partial or
//! empty with a reason, exactly as the other generators do. Nothing is
//! sampled twice or padded to fill a cell that the column cannot populate.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufWriter, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::sampled::Rng;
use super::{BalancedRequest, IndexLimits, LengthBucket, SubstringIndex};
use crate::dataset::PreparedDataset;
use crate::like;
use crate::oracle;
use crate::suite::{self, DatasetBinding, NeedleJson, QueryRecord, SuiteManifest};

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

pub const LIKE_GENERATOR_VERSION: &str = "like-gen2-v1";

/// The pattern classes, in the order they are generated and reported.
pub const CLASSES: [&str; 10] = [
    "prefix",
    "suffix",
    "contains",
    "multi_gap",
    "anchored_gap_head",
    "anchored_gap_tail",
    "anchored_gap_both",
    "underscore_1",
    "underscore_2plus",
    "mixed",
];

/// The literal-length strata, matching `crate::like::length_bucket`.
pub const LENGTH_BUCKETS: [(usize, usize); 6] = [(1, 3), (4, 7), (8, 16), (17, 32), (33, 64), (65, 128)];

/// The selectivity strata, matching `crate::like::selectivity_bucket`.
pub const SELECTIVITY_BUCKETS: [&str; 8] =
    ["zero", "ultra_rare", "1e-5", "1e-4", "1e-3", "1e-2", "1e-1", "broad"];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LikeRequest {
    pub seed: u64,
    /// Target patterns per (class, length bucket, selectivity bucket).
    pub per_cell: usize,
    /// Mining pool: rows with `xxh3(index) % modulus == 0`. 1 = every row.
    pub mining_modulus: u64,
    /// Longest literal (per run) the SA index is asked for.
    pub max_literal_len: usize,
    /// Exact full-column probes a class may spend before giving up on its
    /// remaining cells. The bound on generation time.
    pub probe_budget_per_class: usize,
    /// Literals the SA generator is asked for per (length, row) cell of the
    /// pool — the raw material every class is cut from.
    pub pool_per_cell: usize,
}

impl LikeRequest {
    pub fn new(seed: u64) -> Self {
        LikeRequest {
            seed,
            per_cell: 3,
            mining_modulus: 8,
            max_literal_len: 128,
            probe_budget_per_class: 240,
            pool_per_cell: 12,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LikeCell {
    pub class: String,
    pub length_bucket: String,
    pub selectivity_bucket: String,
    pub requested: usize,
    pub filled: usize,
    /// filled | partial | empty
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LikeQuery {
    pub pattern: Vec<u8>,
    pub class: String,
    pub op: u32,
    pub needles: Vec<Vec<u8>>,
    pub match_count: u64,
    pub selectivity: f64,
    pub length_bucket: String,
    pub selectivity_bucket: String,
    /// Full-column row the pattern was cut from (absent for negatives).
    pub witness_row: Option<u64>,
    /// The pool literal(s) the pattern was built from, before mutation.
    pub source_literals: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GeneratedLike {
    pub generator: String,
    pub dataset_checksum: String,
    pub total_rows: u64,
    pub request: LikeRequest,
    pub mining_pool: serde_json::Value,
    pub exact_probes: u64,
    pub cells: Vec<LikeCell>,
    pub queries: Vec<LikeQuery>,
}

// ------------------------------------------------------------ the pool

/// The held-out rows, laid out as a chunk of their own so the SA index can
/// be built over them alone, plus the map back to full-column row indices.
struct Pool {
    payload: Vec<u8>,
    offsets: Vec<u64>,
    rows: Vec<u64>,
}

fn build_pool(ds: &PreparedDataset, modulus: u64) -> Pool {
    let mut payload = Vec::new();
    let mut offsets = vec![0u64];
    let mut rows = Vec::new();
    for i in 0..ds.num_rows() {
        if modulus > 1 && xxhash_rust::xxh3::xxh3_64(&i.to_le_bytes()) % modulus != 0 {
            continue;
        }
        payload.extend_from_slice(ds.row(i));
        offsets.push(payload.len() as u64);
        rows.push(i);
    }
    Pool { payload, offsets, rows }
}

impl Pool {
    /// The full-column row a pool byte offset lies in.
    fn row_of(&self, pos: usize) -> u64 {
        let k = self.offsets.partition_point(|&o| o <= pos as u64) - 1;
        self.rows[k]
    }
}

/// A mined literal with where it came from.
#[derive(Clone)]
struct Lit {
    bytes: Vec<u8>,
    witness: Option<u64>,
}

// ------------------------------------------------------ pattern building

/// Escape a literal and punch `_` holes at the given byte positions.
fn with_holes(lit: &[u8], holes: &[usize]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lit.len() + 2 + holes.len());
    for (i, &b) in lit.iter().enumerate() {
        if holes.contains(&i) {
            out.push(b'_');
        } else {
            like::escape_literal(&[b], &mut out);
        }
    }
    out
}

/// Positions where a `_` may go: ASCII alphanumerics only, so a hole never
/// splits a multi-byte sequence and the pattern stays readable.
fn hole_candidates(lit: &[u8]) -> Vec<usize> {
    lit.iter()
        .enumerate()
        .filter(|(_, b)| b.is_ascii_alphanumeric())
        .map(|(i, _)| i)
        .collect()
}

fn pick_holes(rng: &mut Rng, lit: &[u8], want: usize) -> Option<Vec<usize>> {
    let mut cands = hole_candidates(lit);
    if cands.len() < want {
        return None;
    }
    let mut holes = Vec::with_capacity(want);
    for _ in 0..want {
        let k = rng.below(cands.len() as u64) as usize;
        holes.push(cands.swap_remove(k));
    }
    holes.sort_unstable();
    Some(holes)
}

fn wrap(parts: &[Vec<u8>], head_anchored: bool, tail_anchored: bool) -> Vec<u8> {
    let mut out = Vec::new();
    if !head_anchored {
        out.push(b'%');
    }
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(b'%');
        }
        out.extend_from_slice(p);
    }
    if !tail_anchored {
        out.push(b'%');
    }
    out
}

fn escaped(lit: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(lit.len());
    like::escape_literal(lit, &mut out);
    out
}

// -------------------------------------------------------------- generator

struct Gen<'a> {
    ds: &'a PreparedDataset,
    req: &'a LikeRequest,
    rng: Rng,
    seen: HashSet<Vec<u8>>,
    exact_probes: u64,
    /// (class, length bucket, selectivity bucket) -> filled so far.
    filled: BTreeMap<(String, &'static str, &'static str), usize>,
    queries: Vec<LikeQuery>,
}

impl<'a> Gen<'a> {
    fn exact_count(&mut self, op: u32, needles: &[&[u8]]) -> u64 {
        self.exact_probes += 1;
        self.ds
            .rows()
            .filter(|row| oracle::row_matches(op, needles, row))
            .count() as u64
    }

    /// Probe a synthesized pattern and keep it if its measured cell wants it.
    fn offer(
        &mut self,
        class: &str,
        pattern: Vec<u8>,
        witness: Option<u64>,
        sources: Vec<Vec<u8>>,
    ) -> bool {
        if oracle::validate_like_pattern(&pattern).is_err() || self.seen.contains(&pattern) {
            return false;
        }
        let facts = like::facts(&pattern);
        let length_bucket = like::length_bucket(facts.literal_len_total);
        let (op, needles) = match like::lower(&pattern) {
            Some((op, needles)) => (op, needles),
            None => (lb_abi::LB_LIKE, vec![pattern.clone()]),
        };
        let refs: Vec<&[u8]> = needles.iter().map(|n| n.as_slice()).collect();
        let count = self.exact_count(op, &refs);
        let selectivity = count as f64 / self.ds.num_rows().max(1) as f64;
        let sel_bucket = like::selectivity_bucket(count, selectivity);
        let key = (class.to_string(), length_bucket, sel_bucket);
        let slot = self.filled.entry(key).or_default();
        if *slot >= self.req.per_cell {
            return false;
        }
        *slot += 1;
        self.seen.insert(pattern.clone());
        self.queries.push(LikeQuery {
            pattern,
            class: class.into(),
            op,
            needles,
            match_count: count,
            selectivity,
            length_bucket: length_bucket.into(),
            selectivity_bucket: sel_bucket.into(),
            witness_row: witness,
            source_literals: sources,
        });
        true
    }

    /// A window of a full-column row: its prefix, its suffix, or a random
    /// interior slice, of a length drawn from a random length bucket.
    fn window(&mut self, row: u64, kind: u8) -> Option<Vec<u8>> {
        let r = self.ds.row(row);
        let (lo, hi) = LENGTH_BUCKETS[self.rng.below(LENGTH_BUCKETS.len() as u64) as usize];
        let hi = hi.min(self.req.max_literal_len).min(r.len());
        if lo > hi {
            return None;
        }
        let len = lo + self.rng.below((hi - lo + 1) as u64) as usize;
        Some(match kind {
            0 => r[..len].to_vec(),
            1 => r[r.len() - len..].to_vec(),
            _ => {
                let at = self.rng.below((r.len() - len + 1) as u64) as usize;
                r[at..at + len].to_vec()
            }
        })
    }

    /// A second literal from the same row, strictly after `first` ends.
    fn following(&mut self, row: u64, first: &[u8]) -> Option<Vec<u8>> {
        let r = self.ds.row(row);
        let end = r.windows(first.len()).position(|w| w == first)? + first.len();
        let rest = &r[end..];
        if rest.len() < 2 {
            return None;
        }
        let (lo, hi) = LENGTH_BUCKETS[self.rng.below(LENGTH_BUCKETS.len() as u64) as usize];
        let hi = hi.min(rest.len()).min(self.req.max_literal_len);
        if lo > hi {
            return None;
        }
        let len = lo + self.rng.below((hi - lo + 1) as u64) as usize;
        let at = self.rng.below((rest.len() - len + 1) as u64) as usize;
        Some(rest[at..at + len].to_vec())
    }

    fn synthesize(&mut self, class: &str, lit: &Lit) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
        let w = lit.witness;
        Some(match class {
            "contains" => (wrap(&[escaped(&lit.bytes)], false, false), vec![lit.bytes.clone()]),
            "prefix" => {
                let p = self.window(w?, 0)?;
                (wrap(&[escaped(&p)], true, false), vec![p])
            }
            "suffix" => {
                let s = self.window(w?, 1)?;
                (wrap(&[escaped(&s)], false, true), vec![s])
            }
            "multi_gap" => {
                let b = self.following(w?, &lit.bytes)?;
                (wrap(&[escaped(&lit.bytes), escaped(&b)], false, false), vec![lit.bytes.clone(), b])
            }
            "anchored_gap_head" => {
                let p = self.window(w?, 0)?;
                let b = self.following(w?, &p)?;
                (wrap(&[escaped(&p), escaped(&b)], true, false), vec![p, b])
            }
            "anchored_gap_tail" => {
                let s = self.window(w?, 1)?;
                let r = self.ds.row(w?);
                // the literal must end before the suffix starts
                let cut = r.len().saturating_sub(s.len());
                let head = &r[..cut];
                let pos = head.windows(lit.bytes.len()).position(|x| x == lit.bytes.as_slice())?;
                let _ = pos;
                (wrap(&[escaped(&lit.bytes), escaped(&s)], false, true), vec![lit.bytes.clone(), s])
            }
            "anchored_gap_both" => {
                let p = self.window(w?, 0)?;
                let s = self.window(w?, 1)?;
                let r = self.ds.row(w?);
                if p.len() + s.len() > r.len() {
                    return None;
                }
                (wrap(&[escaped(&p), escaped(&s)], true, true), vec![p, s])
            }
            "underscore_1" => {
                let holes = pick_holes(&mut self.rng, &lit.bytes, 1)?;
                (wrap(&[with_holes(&lit.bytes, &holes)], false, false), vec![lit.bytes.clone()])
            }
            "underscore_2plus" => {
                let n = 2 + self.rng.below(2) as usize;
                let holes = pick_holes(&mut self.rng, &lit.bytes, n)?;
                (wrap(&[with_holes(&lit.bytes, &holes)], false, false), vec![lit.bytes.clone()])
            }
            "mixed" => {
                let b = self.following(w?, &lit.bytes)?;
                let h1 = pick_holes(&mut self.rng, &lit.bytes, 1)?;
                let h2 = pick_holes(&mut self.rng, &b, 1)?;
                (
                    wrap(&[with_holes(&lit.bytes, &h1), with_holes(&b, &h2)], false, false),
                    vec![lit.bytes.clone(), b],
                )
            }
            _ => return None,
        })
    }
}

/// Mine the literal pool from the held-out rows with the SA generator.
fn mine_pool(pool: &Pool, req: &LikeRequest) -> Result<Vec<Lit>> {
    if pool.rows.is_empty() {
        return Err("mining pool is empty — lower --mining-modulus".into());
    }
    let limits = IndexLimits {
        max_needle_len: req.max_literal_len,
        ..IndexLimits::default()
    };
    let index = SubstringIndex::new(&pool.payload, &pool.offsets, limits)?;
    let mut breq = BalancedRequest::new(pool.rows.len() as u64, req.seed);
    breq.per_cell = req.pool_per_cell;
    breq.lengths = LENGTH_BUCKETS
        .iter()
        .filter(|(lo, _)| *lo <= req.max_literal_len)
        .map(|&(min, max)| LengthBucket { min, max: max.min(req.max_literal_len) })
        .collect();
    let generated = index.generate(&breq)?;
    Ok(generated
        .needles
        .into_iter()
        .map(|n| Lit {
            witness: (n.mutation_position.is_none()).then(|| pool.row_of(n.source_position)),
            bytes: n.bytes,
        })
        .collect())
}

pub fn generate_like(ds: &PreparedDataset, req: &LikeRequest) -> Result<GeneratedLike> {
    if req.per_cell == 0 {
        return Err("per_cell must be at least 1".into());
    }
    let pool = build_pool(ds, req.mining_modulus.max(1));
    let lits = mine_pool(&pool, req)?;

    let mut g = Gen {
        ds,
        req,
        rng: Rng::from_seed(req.seed ^ 0x11CE_2026),
        seen: HashSet::new(),
        exact_probes: 0,
        filled: BTreeMap::new(),
        queries: Vec::new(),
    };

    for class in CLASSES {
        // A fresh deterministic order per class, so classes do not all cut
        // from the same first few literals.
        let mut order: Vec<usize> = (0..lits.len()).collect();
        for i in (1..order.len()).rev() {
            let j = g.rng.below(i as u64 + 1) as usize;
            order.swap(i, j);
        }
        let mut probes = 0usize;
        let cells = LENGTH_BUCKETS.len() * SELECTIVITY_BUCKETS.len();
        for &k in &order {
            if probes >= req.probe_budget_per_class {
                break;
            }
            let full = g
                .filled
                .iter()
                .filter(|((c, _, _), n)| c == class && **n >= req.per_cell)
                .count();
            if full >= cells {
                break;
            }
            let Some((pattern, sources)) = g.synthesize(class, &lits[k]) else { continue };
            probes += 1;
            g.offer(class, pattern, lits[k].witness, sources);
        }
    }

    let mut cells = Vec::new();
    for class in CLASSES {
        for &(lo, hi) in &LENGTH_BUCKETS {
            let lb = like::length_bucket(lo as u64);
            debug_assert_eq!(lb, like::length_bucket(hi as u64));
            for sb in SELECTIVITY_BUCKETS {
                let filled = g
                    .filled
                    .get(&(class.to_string(), lb, sb))
                    .copied()
                    .unwrap_or(0);
                let status = if filled >= req.per_cell {
                    "filled"
                } else if filled > 0 {
                    "partial"
                } else {
                    "empty"
                };
                cells.push(LikeCell {
                    class: class.into(),
                    length_bucket: lb.into(),
                    selectivity_bucket: sb.into(),
                    requested: req.per_cell,
                    filled,
                    status: status.into(),
                    reason: (filled < req.per_cell)
                        .then(|| "no_candidates_in_cell_within_probe_budget".into()),
                });
            }
        }
    }

    Ok(GeneratedLike {
        generator: LIKE_GENERATOR_VERSION.into(),
        dataset_checksum: ds.manifest.checksum.clone(),
        total_rows: ds.num_rows(),
        request: req.clone(),
        mining_pool: serde_json::json!({
            "rule": format!("xxh3_64(row_index) % {} == 0", req.mining_modulus.max(1)),
            "rows": pool.rows.len(),
            "literals_mined": lits.len(),
        }),
        exact_probes: g.exact_probes,
        cells,
        queries: g.queries,
    })
}

// ------------------------------------------------------------- the suite

fn needle_json(bytes: &[u8]) -> NeedleJson {
    use base64::Engine;
    match std::str::from_utf8(bytes) {
        Ok(s) => NeedleJson::Text(s.to_string()),
        Err(_) => NeedleJson::B64 {
            b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    }
}

/// Write the generated patterns as an unblessed suite, one record per
/// pattern under its narrowest op, with the generator's own count in `meta`
/// for `bless` to verify — never as truth.
pub fn write_like_suite(
    generated: &GeneratedLike,
    ds: &PreparedDataset,
    dir: &Path,
    id: &str,
    force: bool,
) -> Result<()> {
    if generated.dataset_checksum != ds.manifest.checksum {
        return Err("generated patterns belong to a different dataset".into());
    }
    if dir.join(suite::QUERIES_FILE).exists() && !force {
        return Err(format!(
            "{} already exists — pass --force to regenerate (this discards blessed truth)",
            dir.join(suite::QUERIES_FILE).display()
        )
        .into());
    }
    let manifest = SuiteManifest {
        format_version: 1,
        id: id.into(),
        description: format!(
            "LIKE-shape sweep over {} ({} rows): {} patterns across {} classes, stratified by \
             literal length x measured selectivity, literals mined from a held-out {} of rows and \
             every pattern probed exactly against the full column. Regenerate with \
             `bench gen --method like --seed {}`.",
            ds.manifest.id,
            ds.num_rows(),
            generated.queries.len(),
            CLASSES.len(),
            generated.mining_pool["rule"],
            generated.request.seed,
        ),
        dataset: DatasetBinding {
            id: ds.manifest.id.clone(),
            checksum: None,
        },
        provenance: Some(serde_json::json!({
            "generator": generated.generator,
            "seed": generated.request.seed,
            "request": generated.request,
            "mining_pool": generated.mining_pool,
            "classes": CLASSES,
        })),
        truth_algo: None,
        blessed_at: None,
    };
    std::fs::create_dir_all(dir)?;
    let mut out = BufWriter::new(std::fs::File::create(dir.join(suite::QUERIES_FILE))?);
    let mut per_class: BTreeMap<&str, usize> = BTreeMap::new();
    for q in &generated.queries {
        let n = per_class.entry(q.class.as_str()).or_default();
        let record = QueryRecord {
            id: format!("{id}.{}.{}.{}.{:03}", q.class, q.length_bucket, q.selectivity_bucket, *n),
            op: lb_abi::op_name(q.op).into(),
            needles: q.needles.iter().map(|n| needle_json(n)).collect(),
            meta: Some(serde_json::json!({
                "source": "generated",
                "like": { "pattern": String::from_utf8_lossy(&q.pattern) },
                "gen": {
                    "version": generated.generator,
                    "seed": generated.request.seed,
                    "class": q.class,
                    "length_bucket": q.length_bucket,
                    "selectivity_bucket": q.selectivity_bucket,
                    "mining_pool": generated.mining_pool["rule"],
                    "generator_count": q.match_count,
                    "witness_row": q.witness_row,
                    "source_literals": q.source_literals.iter()
                        .map(|l| String::from_utf8_lossy(l).into_owned()).collect::<Vec<_>>(),
                },
            })),
            truth: None,
            derived: None,
        };
        *n += 1;
        serde_json::to_writer(&mut out, &record)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    std::fs::write(
        dir.join(suite::SUITE_FILE),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    let populated: BTreeMap<&str, Vec<&str>> = SELECTIVITY_BUCKETS
        .iter()
        .map(|sb| {
            (
                *sb,
                CLASSES
                    .iter()
                    .copied()
                    .filter(|c| generated.queries.iter().any(|q| q.class == *c && q.selectivity_bucket == *sb))
                    .collect(),
            )
        })
        .collect();
    let report = serde_json::json!({
        "generator": generated.generator,
        "dataset": { "id": ds.manifest.id, "checksum": ds.manifest.checksum, "num_rows": ds.num_rows() },
        "request": generated.request,
        "mining_pool": generated.mining_pool,
        "exact_probes": generated.exact_probes,
        "queries": generated.queries.len(),
        "per_class": per_class,
        "populated_selectivity_buckets": populated,
        "cells": generated.cells,
    });
    std::fs::write(dir.join("gen-report.json"), serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holes_are_ascii_only_and_escaped_literals_survive() {
        // 'ó' must never be split; '%' and '_' in the data must stay literal.
        let lit = "a%ób_c".as_bytes();
        let cands = hole_candidates(lit);
        assert_eq!(cands, vec![0, 4, 6]); // a, b, c — not '%', not the two bytes of ó, not '_'
        let pat = with_holes(lit, &[4]);
        assert_eq!(pat, "a\\%ó_\\_c".as_bytes());
        assert!(oracle::validate_like_pattern(&pat).is_ok());
        let unanchored = wrap(&[pat], false, false);
        assert!(oracle::row_matches_like(&unanchored, "xxa%óZ_cyy".as_bytes())); // hole takes 'Z'
        assert!(oracle::row_matches_like(&unanchored, "xxa%ób_cyy".as_bytes())); // and the original
        assert!(!oracle::row_matches_like(&unanchored, "xxa%ó_cyy".as_bytes())); // but not nothing
    }

    #[test]
    fn wrap_places_percents_by_anchoring() {
        assert_eq!(wrap(&[b"ab".to_vec()], false, false), b"%ab%");
        assert_eq!(wrap(&[b"ab".to_vec()], true, false), b"ab%");
        assert_eq!(wrap(&[b"ab".to_vec()], false, true), b"%ab");
        assert_eq!(wrap(&[b"a".to_vec(), b"b".to_vec()], true, true), b"a%b");
        assert_eq!(wrap(&[b"a".to_vec(), b"b".to_vec()], false, false), b"%a%b%");
    }

    #[test]
    fn length_buckets_agree_with_the_partition() {
        for (lo, hi) in LENGTH_BUCKETS {
            assert_eq!(like::length_bucket(lo as u64), like::length_bucket(hi as u64));
        }
    }
}
