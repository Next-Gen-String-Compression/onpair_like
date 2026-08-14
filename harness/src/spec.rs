//! The run spec (`spec.toml`): what actually runs — candidates + configs ×
//! scanners × datasets × chunk sizes × suites. The spec file's hash is
//! recorded in the results manifest, so a run is reproducible from its spec.
//!
//! ## Writing a spec: put root-level keys FIRST
//!
//! `strategies` and `measure` belong to the document root, and TOML assigns
//! every bare key-value pair to the most recently opened table. So this
//!
//! ```toml
//! [[scanners]]
//! name = "memmem"
//!
//! strategies = ["direct"]   # NOT a root key — a key of that [[scanners]] entry
//! ```
//!
//! silently makes `strategies` a field of the last scanner instead of the spec.
//! Every struct here therefore sets `deny_unknown_fields`, so a misplaced or
//! misspelled key is a load error naming the key, not a silently ignored line.
//! (Before that, a misplaced `strategies` allowlist parsed fine and ran the
//! full unfiltered strategy set — an 83-minute surprise on one sweep.)
//!
//! Write root keys above the first `[[table]]` header:
//!
//! ```toml
//! strategies = ["direct"]
//!
//! [[scanners]]
//! name = "memmem"
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRef {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteRef {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ScannerSel {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = r#"
[[datasets]]
path = "d"
[[suites]]
path = "s"
[[candidates]]
name = "c"
"#;

    #[test]
    fn root_strategies_before_tables_is_an_allowlist() {
        let spec: Spec = toml::from_str(&format!(
            "strategies = [\"direct\"]\n{HEAD}\n[[scanners]]\nname = \"memmem\"\n"
        ))
        .unwrap();
        assert_eq!(spec.strategies, ["direct"]);
        assert_eq!(spec.scanners.len(), 1);
    }

    /// TOML attaches a bare key to the last table opened, so a `strategies`
    /// line after `[[scanners]]` is a scanner field, not a root key. Silently
    /// ignoring it ran the full unfiltered strategy set; it must be an error.
    #[test]
    fn strategies_after_a_table_header_is_rejected() {
        let err = toml::from_str::<Spec>(&format!(
            "{HEAD}\n[[scanners]]\nname = \"memmem\"\nstrategies = [\"direct\"]\n"
        ))
        .expect_err("misplaced root key must not be silently dropped");
        let msg = err.to_string();
        assert!(msg.contains("strategies"), "error should name the key: {msg}");
    }

    /// Same hazard one table further down: after `[measure]`, a root key looks
    /// like a measure field.
    #[test]
    fn strategies_after_measure_is_rejected() {
        let err = toml::from_str::<Spec>(&format!(
            "{HEAD}\n[measure]\nchunk_rows = [0]\nstrategies = [\"direct\"]\n"
        ))
        .expect_err("misplaced root key must not be silently dropped");
        assert!(err.to_string().contains("strategies"));
    }

    #[test]
    fn a_misspelled_measure_field_is_rejected() {
        let err = toml::from_str::<Spec>(&format!("{HEAD}\n[measure]\nmin_iter = 3\n"))
            .expect_err("typo must not fall back to the default");
        assert!(err.to_string().contains("min_iter"));
    }

    /// Every spec checked into specs/ must load, so the guard cannot land
    /// while a committed spec still trips it.
    #[test]
    fn committed_specs_all_load() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("specs");
        let mut checked = 0;
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("specs/ is readable") {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "toml") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    toml::from_str::<Spec>(&text)
                        .unwrap_or_else(|e| panic!("{} does not load: {e}", path.display()));
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no specs found under {}", root.display());
    }
}
