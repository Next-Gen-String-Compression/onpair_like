//! The run spec (`spec.toml`): what actually runs — candidates + configs ×
//! scanners × datasets × chunk sizes × suites. The spec file's hash is
//! recorded in the results manifest, so a run is reproducible from its spec.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetRef {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteRef {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateSel {
    pub name: String,
    /// Opaque JSON config strings passed to build(); each (candidate,
    /// config) pair is a distinct result row. Default: one empty config.
    #[serde(default = "default_configs")]
    pub configs: Vec<String>,
}

fn default_configs() -> Vec<String> {
    vec!["{}".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannerSel {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measure {
    #[serde(default = "default_warmup")]
    pub warmup: u32,
    #[serde(default = "default_min_iters")]
    pub min_iters: u32,
    #[serde(default = "default_min_millis")]
    pub min_millis: u64,
    /// Chunk sizes to sweep; 0 = single chunk over the whole dataset.
    /// Nonzero values must be multiples of 64.
    #[serde(default = "default_chunk_rows")]
    pub chunk_rows: Vec<u64>,
    /// Store raw latency samples on every row (large output).
    #[serde(default)]
    pub raw_samples: bool,
    /// Core to pin workers to.
    #[serde(default)]
    pub pin_core: usize,
    /// Skip dataset checksum verification at load (interactive iteration).
    #[serde(default)]
    pub skip_checksum_verify: bool,
}

fn default_warmup() -> u32 {
    3
}
fn default_min_iters() -> u32 {
    10
}
fn default_min_millis() -> u64 {
    200
}
fn default_chunk_rows() -> Vec<u64> {
    vec![0]
}

impl Default for Measure {
    fn default() -> Self {
        Self {
            warmup: default_warmup(),
            min_iters: default_min_iters(),
            min_millis: default_min_millis(),
            chunk_rows: default_chunk_rows(),
            raw_samples: false,
            pin_core: 0,
            skip_checksum_verify: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spec {
    pub datasets: Vec<DatasetRef>,
    pub suites: Vec<SuiteRef>,
    pub candidates: Vec<CandidateSel>,
    #[serde(default)]
    pub scanners: Vec<ScannerSel>,
    /// Optional allowlist of strategy names to run (candidate-declared
    /// names plus the reserved `direct` / `decode`). Empty = run every
    /// applicable strategy (the default). A shootout that only wants the
    /// uncompressed `direct` path sets `strategies = ["direct"]` so the
    /// runner does not re-run `decode` for every codec × scanner.
    #[serde(default)]
    pub strategies: Vec<String>,
    #[serde(default)]
    pub measure: Measure,
}

pub struct LoadedSpec {
    pub spec: Spec,
    pub path: PathBuf,
    /// xxh3 of the spec file bytes — recorded in the results manifest.
    pub hash: String,
}

impl LoadedSpec {
    pub fn load(path: &Path) -> Result<LoadedSpec> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut spec: Spec = toml::from_str(std::str::from_utf8(&bytes)?)?;
        let base = path.parent().unwrap_or(Path::new("."));
        // Paths in the spec are relative to the spec file.
        for d in &mut spec.datasets {
            d.path = base.join(&d.path);
        }
        for s in &mut spec.suites {
            s.path = base.join(&s.path);
        }
        if spec.candidates.is_empty() {
            return Err("spec selects no candidates".into());
        }
        if spec.datasets.is_empty() {
            return Err("spec selects no datasets".into());
        }
        if spec.suites.is_empty() {
            return Err("spec selects no suites".into());
        }
        for c in &spec.candidates {
            for cfg in &c.configs {
                serde_json::from_str::<serde_json::Value>(cfg)
                    .map_err(|e| format!("candidate {}: config {cfg:?} is not valid JSON: {e}", c.name))?;
            }
        }
        for &cr in &spec.measure.chunk_rows {
            if cr != 0 && cr % 64 != 0 {
                return Err(format!("measure.chunk_rows: {cr} is not a multiple of 64").into());
            }
        }
        Ok(LoadedSpec {
            spec,
            path: path.to_path_buf(),
            hash: format!("xxh3:{:016x}", xxhash_rust::xxh3::xxh3_64(&bytes)),
        })
    }
}

/// Short stable hash for a config string, used in row keys and file names.
pub fn config_hash(config: &str) -> String {
    format!("{:08x}", xxhash_rust::xxh3::xxh3_64(config.as_bytes()) as u32)
}
