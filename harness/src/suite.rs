//! Suites: suite.json + queries.jsonl, validation, and `bench bless`
//! (DESIGN.md §5).
//!
//! The harness treats only `id`, `op`, `needles`, `truth` as semantics.
//! `meta` is declared metadata — opaque, carried verbatim into results.
//! `derived` is computed and stamped by the harness at bless time and is
//! the only metadata analysis should trust.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::bitmap::{Bitmap, TRUTH_ALGO};
use crate::dataset::PreparedDataset;
use crate::oracle;

pub const SUITE_FILE: &str = "suite.json";
pub const QUERIES_FILE: &str = "queries.jsonl";
const TRUTH_SAMPLE_INDICES: usize = 8;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------- shapes

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetBinding {
    pub id: String,
    /// Stamped at bless time; loading rejects a suite whose checksum does
    /// not match the dataset it is pointed at (truth is never silently
    /// reused across dataset versions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteManifest {
    pub format_version: u32,
    pub id: String,
    #[serde(default)]
    pub description: String,
    pub dataset: DatasetBinding,
    /// Curated-by, or generator name/version/seed/config — opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth_algo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blessed_at: Option<String>,
}

/// A needle in JSON: a plain string (UTF-8 text, the common case) or
/// `{"b64": "..."}` for arbitrary bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NeedleJson {
    Text(String),
    B64 { b64: String },
}

impl NeedleJson {
    pub fn decode(&self) -> Result<Vec<u8>> {
        use base64::Engine;
        Ok(match self {
            NeedleJson::Text(s) => s.as_bytes().to_vec(),
            NeedleJson::B64 { b64 } => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("invalid b64 needle: {e}"))?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Truth {
    pub count: u64,
    pub hash: String,
    pub algo: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample_indices: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRecord {
    pub id: String,
    pub op: String,
    pub needles: Vec<NeedleJson>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth: Option<Truth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived: Option<serde_json::Value>,
}

/// A validated query with needles decoded to bytes.
pub struct PreparedQuery {
    pub record: QueryRecord,
    pub op: u32,
    pub needles: Vec<Vec<u8>>,
}

pub struct Suite {
    pub dir: PathBuf,
    pub manifest: SuiteManifest,
    pub queries: Vec<PreparedQuery>,
}

// ---------------------------------------------------------------- loading

fn validate_arity(op: u32, n: usize) -> std::result::Result<(), String> {
    let ok = match op {
        lb_abi::LB_PREFIX | lb_abi::LB_SUFFIX | lb_abi::LB_CONTAINS => n == 1,
        lb_abi::LB_MULTI_CONTAINS | lb_abi::LB_CONTAINS_ANY => n >= 1,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "op {} takes {} needle(s), got {n}",
            lb_abi::op_name(op),
            if op <= lb_abi::LB_CONTAINS { "exactly 1" } else { ">= 1" }
        ))
    }
}

fn parse_queries(dir: &Path) -> Result<Vec<PreparedQuery>> {
    let path = dir.join(QUERIES_FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut queries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (lineno, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: QueryRecord = serde_json::from_str(line)
            .map_err(|e| format!("{}:{}: {e}", path.display(), lineno + 1))?;
        let op = lb_abi::op_from_name(&record.op)
            .ok_or_else(|| format!("{}: unknown op {:?}", record.id, record.op))?;
        let needles: Vec<Vec<u8>> = record
            .needles
            .iter()
            .map(|n| n.decode())
            .collect::<Result<_>>()?;
        validate_arity(op, needles.len()).map_err(|e| format!("{}: {e}", record.id))?;
        if !seen.insert(record.id.clone()) {
            return Err(format!("duplicate query id {:?}", record.id).into());
        }
        queries.push(PreparedQuery {
            record,
            op,
            needles,
        });
    }
    if queries.is_empty() {
        return Err(format!("{} contains no queries", path.display()).into());
    }
    Ok(queries)
}

impl Suite {
    /// Load without truth requirements (what `bless` starts from).
    pub fn load_unblessed(dir: &Path) -> Result<Suite> {
        let text = std::fs::read_to_string(dir.join(SUITE_FILE))
            .map_err(|e| format!("reading {}: {e}", dir.join(SUITE_FILE).display()))?;
        let manifest: SuiteManifest = serde_json::from_str(&text)?;
        Ok(Suite {
            dir: dir.to_path_buf(),
            manifest,
            queries: parse_queries(dir)?,
        })
    }

    /// Load for running: every query must carry blessed truth, and the
    /// suite must be bound to exactly this dataset (id + checksum).
    pub fn load_for_run(dir: &Path, ds: &PreparedDataset) -> Result<Suite> {
        let suite = Self::load_unblessed(dir)?;
        let m = &suite.manifest;
        if m.dataset.id != ds.manifest.id {
            return Err(format!(
                "suite {} is bound to dataset {:?}, not {:?}",
                m.id, m.dataset.id, ds.manifest.id
            )
            .into());
        }
        match &m.dataset.checksum {
            None => {
                return Err(format!(
                    "suite {} has no dataset checksum — run `bench bless` first",
                    m.id
                )
                .into())
            }
            Some(c) if *c != ds.manifest.checksum => {
                return Err(format!(
                    "suite {} was blessed against dataset checksum {c} but the loaded dataset \
                     hashes to {} — re-run `bench bless`",
                    m.id, ds.manifest.checksum
                )
                .into())
            }
            _ => {}
        }
        if m.truth_algo.as_deref() != Some(TRUTH_ALGO) {
            return Err(format!(
                "suite {} uses truth algo {:?}, harness implements {TRUTH_ALGO:?}",
                m.id, m.truth_algo
            )
            .into());
        }
        for q in &suite.queries {
            match &q.record.truth {
                None => {
                    return Err(format!(
                        "query {} has no truth — run `bench bless` first",
                        q.record.id
                    )
                    .into())
                }
                Some(t) if t.algo != TRUTH_ALGO => {
                    return Err(format!(
                        "query {} truth uses algo {:?}, expected {TRUTH_ALGO:?}",
                        q.record.id, t.algo
                    )
                    .into())
                }
                _ => {}
            }
        }
        Ok(suite)
    }
}

// ----------------------------------------------------------------- bless

pub struct BlessOutcome {
    pub blessed: usize,
    pub verified: usize,
}

/// Run the oracle over every query, fill `truth` + `derived`, stamp the
/// dataset binding, and rewrite the suite files atomically.
///
/// Existing truth is verified rather than overwritten (a generator may have
/// computed its own); a mismatch is an error unless `force` re-blesses.
pub fn bless(dir: &Path, ds: &PreparedDataset, force: bool) -> Result<BlessOutcome> {
    let mut suite = Suite::load_unblessed(dir)?;
    if suite.manifest.dataset.id != ds.manifest.id {
        return Err(format!(
            "suite {} is bound to dataset {:?}, not {:?}",
            suite.manifest.id, suite.manifest.dataset.id, ds.manifest.id
        )
        .into());
    }

    let (mut blessed, mut verified) = (0usize, 0usize);
    for q in &mut suite.queries {
        let needles: Vec<&[u8]> = q.needles.iter().map(|n| n.as_slice()).collect();
        let bm = oracle::eval(q.op, &needles, ds.num_rows(), ds.rows());
        let truth = Truth {
            count: bm.count(),
            hash: bm.truth_hash(),
            algo: TRUTH_ALGO.to_string(),
            sample_indices: bm.first_indices(TRUTH_SAMPLE_INDICES),
        };
        match (&q.record.truth, force) {
            (Some(existing), false) => {
                if existing.count != truth.count || existing.hash != truth.hash {
                    return Err(format!(
                        "query {}: existing truth (count={}, {}) disagrees with the oracle \
                         (count={}, {}) — fix the suite or re-bless with --force",
                        q.record.id, existing.count, existing.hash, truth.count, truth.hash
                    )
                    .into());
                }
                verified += 1;
            }
            _ => blessed += 1,
        }
        q.record.truth = Some(truth);
        q.record.derived = Some(derived_metadata(q, &bm, ds));
    }

    suite.manifest.dataset.checksum = Some(ds.manifest.checksum.clone());
    suite.manifest.truth_algo = Some(TRUTH_ALGO.to_string());
    suite.manifest.blessed_at =
        Some(humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string());

    // Atomic rewrite: write to temp files in the same directory, then rename.
    let write_atomic = |name: &str, contents: &str| -> Result<()> {
        let tmp = dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, dir.join(name))?;
        Ok(())
    };
    let mut lines = String::new();
    for q in &suite.queries {
        lines.push_str(&serde_json::to_string(&q.record)?);
        lines.push('\n');
    }
    write_atomic(QUERIES_FILE, &lines)?;
    write_atomic(
        SUITE_FILE,
        &format!("{}\n", serde_json::to_string_pretty(&suite.manifest)?),
    )?;
    Ok(BlessOutcome { blessed, verified })
}

/// Harness-computed, trusted query metadata (DESIGN.md §5): the values
/// weak-region analysis joins against — never hand-written claims.
fn derived_metadata(q: &PreparedQuery, bm: &Bitmap, ds: &PreparedDataset) -> serde_json::Value {
    let needle_lens: Vec<u64> = q.needles.iter().map(|n| n.len() as u64).collect();
    // Rarity of the needle's rarest byte, measured against this dataset.
    let payload = ds.manifest.payload_bytes.max(1);
    let rarest = q
        .needles
        .iter()
        .flat_map(|n| n.iter())
        .map(|&b| ds.manifest.byte_freq[b as usize])
        .min()
        .map(|c| c as f64 / payload as f64);
    serde_json::json!({
        "selectivity": bm.count() as f64 / ds.num_rows() as f64,
        "match_count": bm.count(),
        "needle_lens": needle_lens,
        "needle_len_total": needle_lens.iter().sum::<u64>(),
        "rarest_byte_freq": rarest,
    })
}
