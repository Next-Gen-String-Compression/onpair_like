//! Result rows (`results.jsonl`) and the run manifest. Rows are pure,
//! self-contained data — reporting consumes them later without re-running
//! anything (DESIGN.md §9).

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::cpu::EnvCapture;
use crate::timing::LatencyStats;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

/// Identifies one cell of the run matrix.
#[derive(Debug, Clone, Serialize)]
pub struct CellKey {
    pub candidate: String,
    pub candidate_version: String,
    pub config: String,
    pub config_hash: String,
    /// "compressed"/custom (candidate-implemented), or "direct"/"decode"
    /// (harness-composed). Absent on build rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Present only for harness-composed strategies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanner: Option<String>,
    pub dataset: String,
    pub dataset_checksum: String,
    pub chunk_rows: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Row {
    /// Compression axis: one per (candidate, config, dataset, chunk_rows);
    /// all strategies share it (DESIGN.md §9).
    Build {
        #[serde(flatten)]
        key: CellKey,
        num_chunks: usize,
        build_ns: u64,
        footprint_total_bytes: u64,
        footprint_components: serde_json::Map<String, serde_json::Value>,
        raw_bytes: u64,
    },
    /// Latency axis: one per gated (cell, query).
    Query {
        #[serde(flatten)]
        key: CellKey,
        query_id: String,
        op: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        derived: Option<serde_json::Value>,
        status: Status,
        #[serde(skip_serializing_if = "Option::is_none")]
        gate: Option<GateReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        latency: Option<LatencyStats>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ns_per_row: Option<f64>,
        /// Effective throughput over the *raw* payload bytes, so
        /// cross-candidate comparison ignores each one's compression.
        #[serde(skip_serializing_if = "Option::is_none")]
        gbps_raw: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prefilter: Option<PrefilterReport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// build() failed for this (candidate, config, dataset, chunk_rows);
    /// every cell it would have produced is withheld.
    BuildFailed {
        #[serde(flatten)]
        key: CellKey,
        error: String,
    },
    /// A module hard-gated off this host (missing CPU features) — recorded,
    /// never a silent absence (DESIGN.md §9).
    ModuleUnavailable {
        module: String,
        module_kind: String, // "candidate" | "scanner"
        required_cpu_features: String,
        missing_cpu_features: Vec<String>,
        dataset: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    GateFailed,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateReport {
    pub expected_count: u64,
    pub actual_count: u64,
    pub hash_ok: bool,
    /// On failure: first row where candidate and oracle disagree, with the
    /// row's bytes (lossy, truncated) — recomputed live from the oracle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_divergent_row: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_divergent_row_bytes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_match: Option<bool>,
}

/// Prefilter attribution (DESIGN.md §10). Counters come from the module in
/// instrumented mode; derived metrics are computed by the harness from the
/// gated truth. Self-timed phase splits are labelled by origin and never
/// mixed into headline latency.
#[derive(Debug, Clone, Serialize)]
pub struct PrefilterReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefilter_candidates: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub false_positive_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_ns_per_survivor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_ns: Option<PhaseNs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_ns: Option<PhaseNs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefilter_ns: Option<PhaseNs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify_ns: Option<PhaseNs>,
    /// Per-query setup (pattern/automaton compilation) — ABI v3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_ns: Option<PhaseNs>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseNs {
    pub ns: u64,
    /// "harness" (measured by the harness clock at a pipeline joint) or
    /// "self_reported" (module-provided, instrumented mode).
    pub origin: &'static str,
}

pub struct Writer {
    out: std::io::BufWriter<std::fs::File>,
}

impl Writer {
    pub fn create(path: &Path) -> Result<Writer> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Writer {
            out: std::io::BufWriter::new(std::fs::File::create(path)?),
        })
    }

    pub fn write(&mut self, row: &Row) -> Result<()> {
        serde_json::to_writer(&mut self.out, row)?;
        self.out.write_all(b"\n")?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.out.flush()?;
        Ok(())
    }
}

// -------------------------------------------------------------- manifest

#[derive(Debug, Serialize)]
pub struct RunManifest {
    pub spec_path: String,
    pub spec_hash: String,
    pub spec: crate::spec::Spec,
    pub started_at: String,
    pub finished_at: String,
    pub env: EnvCapture,
    pub pinned_core: usize,
    pub pinning_effective: bool,
    pub datasets: Vec<serde_json::Value>,
    pub suites: Vec<serde_json::Value>,
    pub candidates: Vec<serde_json::Value>,
    pub scanners: Vec<serde_json::Value>,
    /// Not a git repository => null; recorded so its absence is explicit.
    pub git_commit: Option<String>,
    pub git_dirty: Option<bool>,
}

pub fn git_state() -> (Option<String>, Option<bool>) {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let dirty = commit.is_some().then(|| {
        std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    });
    (commit, dirty)
}
