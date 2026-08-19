//! Selectivity-stratified, deterministic CONTAINS query generation.
//!
//! Candidate discovery is intentionally separate from seed-based selection:
//! a bounded catalogue is expensive to build but reusable, while changing the
//! experiment seed cheaply chooses another stable subset from every cell.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use aho_corasick::{AhoCorasickBuilder, AhoCorasickKind, MatchKind};
use base64::Engine as _;
use lb_harness::bitmap::{Bitmap, TRUTH_ALGO};
use lb_harness::dataset::PreparedDataset;
use lb_harness::suite::{
    DatasetBinding, NeedleJson, QueryRecord, SuiteManifest, Truth, QUERIES_FILE, SUITE_FILE,
};
use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

pub const GENERATOR_VERSION: &str = "optimize-prefilter-v3";
// Bump this whenever discovery semantics change so an older cache cannot
// silently preserve a different query population.
const CATALOG_VERSION: &str = "optimize-prefilter-catalog-v2";
pub const REPORT_FILE: &str = "gen-report.json";
pub const QUERY_DOCUMENT: &str = "queries.md";
pub const QUERY_CSV: &str = "queries.csv";
pub const COVERAGE_CSV: &str = "coverage.csv";

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub format_version: u32,
    pub default_profile: String,
    pub experiment: ExperimentConfig,
    pub profiles: BTreeMap<String, ProfileConfig>,
    pub sampling: SamplingConfig,
    pub benchmark: BenchmarkConfig,
    pub datasets: Vec<DatasetConfig>,
    #[serde(skip)]
    config_path: PathBuf,
    #[serde(skip)]
    config_dir: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentConfig {
    pub output_root: PathBuf,
    pub cache_root: PathBuf,
    pub min_needle_len: usize,
    pub max_needle_len: usize,
    pub log_bands_per_decade: u32,
    pub low_selectivity_cutoff: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub description: String,
    pub zero_replicates: usize,
    pub low_selectivity_replicates: usize,
    pub other_replicates: usize,
    /// Strict percentage ceiling. `Some(2.0)` admits only queries whose exact
    /// row selectivity is less than 2%.
    pub max_selectivity_percent: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EffectiveProfile {
    pub name: String,
    pub description: String,
    pub zero_replicates: usize,
    pub low_selectivity_replicates: usize,
    pub other_replicates: usize,
    pub max_selectivity_percent: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingConfig {
    pub candidate_draws: usize,
    pub anchor_rows: usize,
    pub zero_probe_candidates: usize,
    pub catalog_entries_per_cell: usize,
    pub truth_batch_size: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfig {
    pub result_root: PathBuf,
    pub onpair_configs: Vec<String>,
    pub strategies: Vec<String>,
    pub warmup: u32,
    pub min_iters: u32,
    pub min_millis: u64,
    pub chunk_rows: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetConfig {
    pub id: String,
    pub path: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("reading configuration {}: {e}", path.display()))?;
        let mut cfg: Config = toml::from_str(std::str::from_utf8(&bytes)?)?;
        if cfg.format_version != 1 {
            return Err(format!("unsupported config format_version {}", cfg.format_version).into());
        }
        let config_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        cfg.config_dir = config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        cfg.config_path = config_path;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        let e = &self.experiment;
        if e.min_needle_len == 0 || e.min_needle_len > e.max_needle_len {
            return Err("needle length range must be non-empty and start at 1 or later".into());
        }
        if e.log_bands_per_decade == 0 {
            return Err("log_bands_per_decade must be positive".into());
        }
        if !(0.0 < e.low_selectivity_cutoff && e.low_selectivity_cutoff < 1.0) {
            return Err("low_selectivity_cutoff must lie in (0, 1)".into());
        }
        if self.profiles.is_empty() {
            return Err("at least one profile must be configured".into());
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(format!(
                "default_profile {:?} is not present in [profiles]",
                self.default_profile
            )
            .into());
        }
        for (name, profile) in &self.profiles {
            if !valid_profile_name(name) {
                return Err(format!(
                    "profile name {name:?} must contain only ASCII letters, digits, '-' or '_', and start with a letter or digit"
                )
                .into());
            }
            if profile.zero_replicates == 0
                || profile.low_selectivity_replicates == 0
                || profile.other_replicates == 0
            {
                return Err(format!("profile {name:?} replicate counts must be positive").into());
            }
            if profile
                .max_selectivity_percent
                .is_some_and(|pct| !(pct > 0.0 && pct <= 100.0))
            {
                return Err(format!(
                    "profile {name:?} max_selectivity_percent must lie in (0, 100]"
                )
                .into());
            }
        }
        if self.sampling.candidate_draws == 0
            || self.sampling.catalog_entries_per_cell == 0
            || self.sampling.truth_batch_size == 0
        {
            return Err("sampling sizes must be positive".into());
        }
        for cfg in &self.benchmark.onpair_configs {
            serde_json::from_str::<serde_json::Value>(cfg)
                .map_err(|e| format!("benchmark OnPair config {cfg:?}: {e}"))?;
        }
        Ok(())
    }

    pub fn profile(&self, name: Option<&str>) -> Result<EffectiveProfile> {
        let name = name.unwrap_or(&self.default_profile);
        let profile = self.profiles.get(name).ok_or_else(|| {
            format!(
                "unknown profile {name:?}; available profiles: {}",
                self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
        Ok(EffectiveProfile {
            name: name.to_string(),
            description: profile.description.clone(),
            zero_replicates: profile.zero_replicates,
            low_selectivity_replicates: profile.low_selectivity_replicates,
            other_replicates: profile.other_replicates,
            max_selectivity_percent: profile.max_selectivity_percent,
        })
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.config_dir.join(path)
        }
    }
}

fn valid_profile_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Band {
    pub index: usize,
    pub label: String,
    pub min_matches: u64,
    pub max_matches_exclusive: u64,
}

impl Band {
    fn contains(&self, count: u64) -> bool {
        self.min_matches <= count && count < self.max_matches_exclusive
    }
}

/// Exact integer-count bands: zero, four logarithmic intervals per decade
/// from one matching row through 2%, then increasingly broad real-world
/// intervals through 100%. Integer bounds eliminate floating ambiguity.
pub fn selectivity_bands(rows: u64, per_decade: u32, low_cutoff: f64) -> Vec<Band> {
    assert!(rows > 0);
    let mut edges = vec![1u64];
    let step = 1.0 / per_decade as f64;
    let first_exp = ((1.0 / rows as f64).log10() / step).ceil() * step;
    let mut exp = first_exp;
    while 10f64.powf(exp) < low_cutoff {
        edges.push(((rows as f64 * 10f64.powf(exp)).ceil() as u64).clamp(1, rows));
        exp += step;
    }
    edges.push(((rows as f64 * low_cutoff).ceil() as u64).clamp(1, rows));
    for fraction in [0.03, 0.05, 0.08, 0.12, 0.20, 0.35, 0.50, 0.75, 1.0] {
        if fraction > low_cutoff {
            edges.push(((rows as f64 * fraction).ceil() as u64).clamp(1, rows));
        }
    }
    edges.push(rows + 1);
    edges.sort_unstable();
    edges.dedup();

    let mut bands = vec![Band {
        index: 0,
        label: "zero matches".to_string(),
        min_matches: 0,
        max_matches_exclusive: 1,
    }];
    for pair in edges.windows(2) {
        let index = bands.len();
        let lo = pair[0];
        let hi = pair[1];
        bands.push(Band {
            index,
            label: format!(
                "[{lo},{hi}) rows ({:.6}%..{:.6}%)",
                100.0 * lo as f64 / rows as f64,
                100.0 * hi.min(rows) as f64 / rows as f64
            ),
            min_matches: lo,
            max_matches_exclusive: hi,
        });
    }
    bands
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CatalogEntry {
    needle_b64: String,
    match_count: u64,
    provenance: String,
}

impl CatalogEntry {
    fn bytes(&self) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.needle_b64)
            .expect("cache entries were encoded by this generator")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CatalogBand {
    band: Band,
    sampled_eligible: usize,
    entries: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LengthCatalog {
    generator: String,
    dataset_id: String,
    dataset_checksum: String,
    needle_len: usize,
    catalogue_fingerprint: String,
    real_candidates_counted: usize,
    synthetic_candidates_counted: usize,
    bands: Vec<CatalogBand>,
}

#[derive(Clone)]
struct Selected {
    needle: Vec<u8>,
    match_count: u64,
    len: usize,
    band: Band,
    provenance: String,
    requested: usize,
}

#[derive(Clone, Debug, Serialize)]
struct CoverageRow {
    band_index: usize,
    band_label: String,
    min_matches: u64,
    max_matches_exclusive: u64,
    needle_len: usize,
    requested: usize,
    emitted: usize,
    sampled_eligible: usize,
    status: String,
}

#[derive(Debug, Serialize)]
struct GenReport<'a> {
    generator: &'a str,
    seed: u64,
    profile: &'a EffectiveProfile,
    dataset: serde_json::Value,
    methodology: serde_json::Value,
    queries: usize,
    unique_needles: usize,
    full_cells: usize,
    partial_cells: usize,
    not_observed_cells: usize,
    bands: &'a [Band],
    coverage: &'a [CoverageRow],
}

pub struct ExperimentOutcome {
    pub seed_dir: PathBuf,
    pub profile: String,
    pub datasets: usize,
    pub queries: usize,
}

pub fn generate_experiment(
    cfg: &Config,
    profile_name: Option<&str>,
    seed: u64,
    dataset_overrides: &[PathBuf],
    force: bool,
    rebuild_cache: bool,
) -> Result<ExperimentOutcome> {
    let profile = cfg.profile(profile_name)?;
    let output_root = cfg.resolve(&cfg.experiment.output_root);
    let seed_dir = output_root.join(&profile.name).join(format!("seed-{seed}"));
    let datasets: Vec<(Option<&str>, PathBuf)> = if dataset_overrides.is_empty() {
        cfg.datasets
            .iter()
            .map(|d| (Some(d.id.as_str()), cfg.resolve(&d.path)))
            .collect()
    } else {
        dataset_overrides
            .iter()
            .map(|p| {
                (
                    None,
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        std::env::current_dir().unwrap().join(p)
                    },
                )
            })
            .collect()
    };
    if datasets.is_empty() {
        return Err("no datasets selected".into());
    }
    if seed_dir.join("benchmark.toml").exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to regenerate this seed",
            seed_dir.display()
        )
        .into());
    }
    std::fs::create_dir_all(&seed_dir)?;

    let mut generated = Vec::new();
    let mut total_queries = 0usize;
    for (expected_id, dataset_path) in datasets {
        eprintln!("loading dataset {}", dataset_path.display());
        let ds = PreparedDataset::load(&dataset_path, true).map_err(|e| {
            format!(
                "{}: {e}. Prepare configured datasets with `python3 datasets/prepare.py --dataset <id>`",
                dataset_path.display()
            )
        })?;
        if let Some(expected) = expected_id {
            if ds.manifest.id != expected {
                return Err(format!(
                    "{} contains dataset id {:?}, configuration expected {:?}",
                    dataset_path.display(),
                    ds.manifest.id,
                    expected
                )
                .into());
            }
        }
        let out = seed_dir.join(&ds.manifest.id);
        let n = generate_dataset(cfg, &profile, &ds, &out, seed, force, rebuild_cache)?;
        total_queries += n;
        generated.push((dataset_path, out, ds.manifest.id.clone()));
    }
    write_benchmark_spec(cfg, &seed_dir, &generated)?;
    write_experiment_readme(cfg, &profile, &seed_dir, seed, &generated, total_queries)?;
    Ok(ExperimentOutcome {
        seed_dir,
        profile: profile.name,
        datasets: generated.len(),
        queries: total_queries,
    })
}

fn generate_dataset(
    cfg: &Config,
    profile: &EffectiveProfile,
    ds: &PreparedDataset,
    out: &Path,
    seed: u64,
    force: bool,
    rebuild_cache: bool,
) -> Result<usize> {
    if out.join(QUERIES_FILE).exists() && !force {
        return Err(format!("{} already exists; pass --force", out.display()).into());
    }
    let bands = selectivity_bands(
        ds.num_rows(),
        cfg.experiment.log_bands_per_decade,
        cfg.experiment.low_selectivity_cutoff,
    );
    let cache_base = cache_dir(cfg, ds)?;
    std::fs::create_dir_all(&cache_base)?;

    let mut selected = Vec::new();
    let mut coverage = Vec::new();
    let mut globally_seen = HashSet::<Vec<u8>>::new();
    for len in cfg.experiment.min_needle_len..=cfg.experiment.max_needle_len {
        eprintln!("{}: catalogue length {len}", ds.manifest.id);
        let catalog = load_or_discover_catalog(cfg, ds, len, &bands, &cache_base, rebuild_cache)?;
        for catalog_band in &catalog.bands {
            let Some(target_band) = profile_band(profile, ds.num_rows(), &catalog_band.band) else {
                continue;
            };
            let requested = quota(cfg, profile, ds.num_rows(), &target_band);
            let limit = profile_match_limit(profile, ds.num_rows());
            let mut entries: Vec<_> = catalog_band
                .entries
                .iter()
                .filter(|entry| limit.map_or(true, |limit| entry.match_count < limit))
                .cloned()
                .collect();
            let eligible_entries = entries.len();
            entries.sort_by(|a, b| {
                let ab = a.bytes();
                let bb = b.bytes();
                selection_score(seed, &ds.manifest.checksum, &ab)
                    .cmp(&selection_score(seed, &ds.manifest.checksum, &bb))
                    .then_with(|| ab.cmp(&bb))
            });
            let before = selected.len();
            for entry in entries {
                let bytes = entry.bytes();
                if globally_seen.insert(bytes.clone()) {
                    selected.push(Selected {
                        len,
                        needle: bytes,
                        match_count: entry.match_count,
                        band: target_band.clone(),
                        provenance: entry.provenance,
                        requested,
                    });
                    if selected.len() - before == requested {
                        break;
                    }
                }
            }
            let emitted = selected.len() - before;
            coverage.push(CoverageRow {
                band_index: target_band.index,
                band_label: target_band.label.clone(),
                min_matches: target_band.min_matches,
                max_matches_exclusive: target_band.max_matches_exclusive,
                needle_len: len,
                requested,
                emitted,
                sampled_eligible: if target_band.max_matches_exclusive
                    == catalog_band.band.max_matches_exclusive
                {
                    catalog_band.sampled_eligible
                } else {
                    eligible_entries
                },
                status: if emitted == requested {
                    "full".to_string()
                } else if emitted == 0 {
                    "not_observed".to_string()
                } else {
                    "partial".to_string()
                },
            });
        }
    }
    if selected.is_empty() {
        return Err("candidate discovery produced no queries".into());
    }
    selected.sort_by(|a, b| {
        a.match_count
            .cmp(&b.match_count)
            .then_with(|| a.len.cmp(&b.len))
            .then_with(|| a.needle.cmp(&b.needle))
    });

    eprintln!(
        "{}: computing exact bitmap truth for {} unique queries",
        ds.manifest.id,
        selected.len()
    );
    let records = build_query_records(cfg, profile, ds, seed, &selected)?;
    std::fs::create_dir_all(out)?;
    let manifest = SuiteManifest {
        format_version: 1,
        id: format!(
            "optimize-prefilter-{}-{}-s{seed}",
            profile.name, ds.manifest.id
        ),
        description: format!(
            "{} exact-selectivity, unique CONTAINS queries over {} using profile {}. Sorted by selectivity then needle length.",
            records.len(), ds.manifest.id, profile.name
        ),
        dataset: DatasetBinding {
            id: ds.manifest.id.clone(),
            checksum: Some(ds.manifest.checksum.clone()),
        },
        provenance: Some(serde_json::json!({
            "generator": {
                "name": GENERATOR_VERSION,
                "seed": seed,
                "profile": profile,
                "config": cfg.config_path.display().to_string(),
                "selection": "xxh3 rank over dataset checksum, seed, and exact needle bytes",
            }
        })),
        truth_algo: Some(TRUTH_ALGO.to_string()),
        // Deliberately absent: otherwise wall-clock time breaks byte-identical regeneration.
        blessed_at: None,
    };
    write_suite(out, &manifest, &records)?;
    write_reports(cfg, profile, ds, out, seed, &bands, &coverage, &records)?;
    Ok(records.len())
}

fn cache_dir(cfg: &Config, ds: &PreparedDataset) -> Result<PathBuf> {
    let serialized = serde_json::to_vec(&serde_json::json!({
        "generator": CATALOG_VERSION,
        "checksum": ds.manifest.checksum,
        "rows": ds.num_rows(),
        "experiment": {
            "log_bands_per_decade": cfg.experiment.log_bands_per_decade,
            "low_selectivity_cutoff": cfg.experiment.low_selectivity_cutoff,
        },
        "sampling": cfg.sampling,
    }))?;
    let fingerprint = format!("{:016x}", xxh3_64(&serialized));
    let checksum = ds.manifest.checksum.replace(':', "-");
    Ok(cfg
        .resolve(&cfg.experiment.cache_root)
        .join(checksum)
        .join(fingerprint))
}

fn catalog_fingerprint(cfg: &Config, ds: &PreparedDataset) -> Result<String> {
    Ok(cache_dir(cfg, ds)?
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned())
}

fn load_or_discover_catalog(
    cfg: &Config,
    ds: &PreparedDataset,
    len: usize,
    bands: &[Band],
    cache_base: &Path,
    rebuild: bool,
) -> Result<LengthCatalog> {
    let path = cache_base.join(format!("length-{len:03}.json"));
    if path.exists() && !rebuild {
        let catalog: LengthCatalog = serde_json::from_slice(&std::fs::read(&path)?)?;
        if catalog.generator == CATALOG_VERSION
            && catalog.dataset_checksum == ds.manifest.checksum
            && catalog.needle_len == len
            && catalog.bands.iter().map(|b| &b.band).eq(bands.iter())
        {
            return Ok(catalog);
        }
    }
    let catalog = discover_catalog(cfg, ds, len, bands)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&catalog)?)?;
    std::fs::rename(tmp, path)?;
    Ok(catalog)
}

#[derive(Clone, Copy)]
enum CandidateKind {
    Real(u8),
    Synthetic(u8),
}

fn discover_catalog(
    cfg: &Config,
    ds: &PreparedDataset,
    len: usize,
    bands: &[Band],
) -> Result<LengthCatalog> {
    let mut candidates = HashMap::<Vec<u8>, CandidateKind>::new();
    sample_real_candidates(cfg, ds, len, &mut candidates);
    let real_candidates_counted = candidates.len();
    add_synthetic_candidates(cfg, ds, len, &mut candidates);
    let synthetic_candidates_counted = candidates.len() - real_candidates_counted;

    let mut ordered: Vec<_> = candidates.into_iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    let patterns: Vec<&[u8]> = ordered.iter().map(|(p, _)| p.as_slice()).collect();
    let counts = document_counts(ds, &patterns)?;

    let mut bucket_entries: Vec<Vec<CatalogEntry>> = vec![Vec::new(); bands.len()];
    let mut totals = vec![0usize; bands.len()];
    for ((needle, kind), count) in ordered.into_iter().zip(counts) {
        let (eligible, provenance) = match kind {
            CandidateKind::Real(flags) if count > 0 => (true, real_provenance(flags).to_string()),
            CandidateKind::Synthetic(mutations) if count == 0 => (
                true,
                format!("synthetic_negative_{mutations}_observed_byte_mutation"),
            ),
            _ => (false, String::new()),
        };
        if !eligible {
            continue;
        }
        let Some(index) = bands.iter().position(|b| b.contains(count)) else {
            continue;
        };
        totals[index] += 1;
        bucket_entries[index].push(CatalogEntry {
            needle_b64: base64::engine::general_purpose::STANDARD.encode(&needle),
            match_count: count,
            provenance,
        });
    }
    let fixed_seed = xxh3_64(ds.manifest.checksum.as_bytes()) ^ len as u64;
    for entries in &mut bucket_entries {
        entries.sort_by(|a, b| {
            let ab = a.bytes();
            let bb = b.bytes();
            xxh3_64_with_seed(&ab, fixed_seed)
                .cmp(&xxh3_64_with_seed(&bb, fixed_seed))
                .then_with(|| ab.cmp(&bb))
        });
        entries.truncate(cfg.sampling.catalog_entries_per_cell);
    }
    Ok(LengthCatalog {
        generator: CATALOG_VERSION.to_string(),
        dataset_id: ds.manifest.id.clone(),
        dataset_checksum: ds.manifest.checksum.clone(),
        needle_len: len,
        catalogue_fingerprint: catalog_fingerprint(cfg, ds)?,
        real_candidates_counted,
        synthetic_candidates_counted,
        bands: bands
            .iter()
            .cloned()
            .enumerate()
            .map(|(i, band)| CatalogBand {
                band,
                sampled_eligible: totals[i],
                entries: std::mem::take(&mut bucket_entries[i]),
            })
            .collect(),
    })
}

fn sample_real_candidates(
    cfg: &Config,
    ds: &PreparedDataset,
    len: usize,
    out: &mut HashMap<Vec<u8>, CandidateKind>,
) {
    let mut eligible_rows = Vec::<u64>::new();
    let mut window_prefix = Vec::<u64>::with_capacity(ds.num_rows() as usize + 1);
    window_prefix.push(0);
    for row_id in 0..ds.num_rows() {
        let row_len = ds.row(row_id).len();
        let windows = if row_len >= len {
            (row_len - len + 1) as u64
        } else {
            0
        };
        if windows > 0 {
            eligible_rows.push(row_id);
        }
        window_prefix.push(window_prefix.last().copied().unwrap() + windows);
    }
    let total_windows = *window_prefix.last().unwrap();
    if eligible_rows.is_empty() || total_windows == 0 {
        return;
    }
    let base_seed = xxh3_64(ds.manifest.checksum.as_bytes())
        ^ (len as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ 0x4341_5441_4c4f_4731;
    let mut rng = Rng::from_seed(base_seed);
    let row_draws = cfg.sampling.candidate_draws / 2;
    for _ in 0..row_draws {
        let row_id = eligible_rows[rng.below(eligible_rows.len() as u64) as usize];
        let row = ds.row(row_id);
        let offset = rng.below((row.len() - len + 1) as u64) as usize;
        insert_real(out, row[offset..offset + len].to_vec(), 1);
    }
    for _ in row_draws..cfg.sampling.candidate_draws {
        let ordinal = rng.below(total_windows);
        let row_id = window_prefix.partition_point(|&v| v <= ordinal) - 1;
        let offset = (ordinal - window_prefix[row_id]) as usize;
        let row = ds.row(row_id as u64);
        insert_real(out, row[offset..offset + len].to_vec(), 2);
    }

    let anchors = cfg.sampling.anchor_rows.min(eligible_rows.len());
    for i in 0..anchors {
        let at = i * eligible_rows.len() / anchors;
        let row = ds.row(eligible_rows[at]);
        let max_offset = row.len() - len;
        for offset in [0, max_offset / 2, max_offset] {
            insert_real(out, row[offset..offset + len].to_vec(), 4);
        }
    }
}

fn insert_real(out: &mut HashMap<Vec<u8>, CandidateKind>, bytes: Vec<u8>, flag: u8) {
    out.entry(bytes)
        .and_modify(|kind| {
            if let CandidateKind::Real(flags) = kind {
                *flags |= flag;
            }
        })
        .or_insert(CandidateKind::Real(flag));
}

fn add_synthetic_candidates(
    cfg: &Config,
    ds: &PreparedDataset,
    len: usize,
    candidates: &mut HashMap<Vec<u8>, CandidateKind>,
) {
    // An absent byte makes a zero-result query trivially rejectable. Restrict
    // mutations to the dataset's observed alphabet, then let exact counting
    // prove that the complete mutated needle is absent.
    let alphabet: Vec<u8> = ds
        .manifest
        .byte_freq
        .iter()
        .enumerate()
        .filter_map(|(byte, &count)| (count > 0).then_some(byte as u8))
        .collect();
    if alphabet.len() < 2 {
        return;
    }
    let mut bases: Vec<Vec<u8>> = candidates
        .iter()
        .filter_map(|(bytes, kind)| matches!(kind, CandidateKind::Real(_)).then_some(bytes.clone()))
        .collect();
    let seed = xxh3_64(ds.manifest.checksum.as_bytes()) ^ len as u64 ^ 0x5a45_524f_5631;
    bases.sort_by(|a, b| {
        xxh3_64_with_seed(a, seed)
            .cmp(&xxh3_64_with_seed(b, seed))
            .then_with(|| a.cmp(b))
    });
    let mut added = 0usize;
    for (base_index, base) in bases.iter().enumerate() {
        for mutations in 1..=2u8 {
            let mut mutated = base.clone();
            for m in 0..mutations as usize {
                let h = xxh3_64_with_seed(
                    base,
                    seed ^ (base_index as u64).rotate_left(17) ^ (m as u64 + 1),
                );
                let pos = (h as usize + m) % len;
                let mut alphabet_index = ((h >> 32) as usize) % alphabet.len();
                let mut replacement = alphabet[alphabet_index];
                if replacement == mutated[pos] {
                    alphabet_index = (alphabet_index + 1) % alphabet.len();
                    replacement = alphabet[alphabet_index];
                }
                mutated[pos] = replacement;
            }
            if !candidates.contains_key(&mutated) {
                candidates.insert(mutated, CandidateKind::Synthetic(mutations));
                added += 1;
                if added == cfg.sampling.zero_probe_candidates {
                    return;
                }
            }
        }
    }
}

fn real_provenance(flags: u8) -> &'static str {
    match flags {
        1 => "real_substring_row_uniform",
        2 => "real_substring_occurrence_uniform",
        4 => "real_substring_anchor",
        _ => "real_substring_mixed_sampling",
    }
}

fn document_counts(ds: &PreparedDataset, patterns: &[&[u8]]) -> Result<Vec<u64>> {
    if patterns.is_empty() {
        return Ok(Vec::new());
    }
    let ac = AhoCorasickBuilder::new()
        .match_kind(MatchKind::Standard)
        .kind(Some(AhoCorasickKind::ContiguousNFA))
        .build(patterns)?;
    let mut counts = vec![0u64; patterns.len()];
    let mut last_row = vec![u64::MAX; patterns.len()];
    for (row_id, row) in ds.rows().enumerate() {
        for found in ac.find_overlapping_iter(row) {
            let id = found.pattern().as_usize();
            if last_row[id] != row_id as u64 {
                last_row[id] = row_id as u64;
                counts[id] += 1;
            }
        }
    }
    Ok(counts)
}

fn profile_match_limit(profile: &EffectiveProfile, rows: u64) -> Option<u64> {
    profile
        .max_selectivity_percent
        .map(|percent| ((rows as f64 * percent / 100.0).ceil() as u64).clamp(1, rows))
}

fn profile_band(profile: &EffectiveProfile, rows: u64, band: &Band) -> Option<Band> {
    let Some(limit) = profile_match_limit(profile, rows) else {
        return Some(band.clone());
    };
    if band.min_matches >= limit {
        return None;
    }
    if band.max_matches_exclusive <= limit {
        return Some(band.clone());
    }
    Some(Band {
        index: band.index,
        label: format!(
            "[{}, {}) rows ({:.6}%..{:.6}%)",
            band.min_matches,
            limit,
            100.0 * band.min_matches as f64 / rows as f64,
            100.0 * limit as f64 / rows as f64,
        ),
        min_matches: band.min_matches,
        max_matches_exclusive: limit,
    })
}

fn quota(cfg: &Config, profile: &EffectiveProfile, rows: u64, band: &Band) -> usize {
    if band.min_matches == 0 {
        profile.zero_replicates
    } else {
        let cutoff = (rows as f64 * cfg.experiment.low_selectivity_cutoff).ceil() as u64;
        if band.max_matches_exclusive <= cutoff {
            profile.low_selectivity_replicates
        } else {
            profile.other_replicates
        }
    }
}

fn selection_score(seed: u64, checksum: &str, needle: &[u8]) -> u64 {
    xxh3_64_with_seed(needle, seed ^ xxh3_64(checksum.as_bytes()))
}

fn build_query_records(
    cfg: &Config,
    profile: &EffectiveProfile,
    ds: &PreparedDataset,
    seed: u64,
    selected: &[Selected],
) -> Result<Vec<QueryRecord>> {
    let mut truths: Vec<Option<Truth>> = vec![None; selected.len()];
    let empty = Bitmap::new(ds.num_rows());
    for (i, query) in selected.iter().enumerate() {
        if query.match_count == 0 {
            truths[i] = Some(Truth {
                count: 0,
                hash: empty.truth_hash(),
                algo: TRUTH_ALGO.to_string(),
                sample_indices: Vec::new(),
            });
        }
    }
    let positive_indices: Vec<usize> = selected
        .iter()
        .enumerate()
        .filter_map(|(i, q)| (q.match_count > 0).then_some(i))
        .collect();
    for (batch_no, chunk) in positive_indices
        .chunks(cfg.sampling.truth_batch_size)
        .enumerate()
    {
        eprintln!(
            "{}: truth batch {}/{}",
            ds.manifest.id,
            batch_no + 1,
            positive_indices
                .len()
                .div_ceil(cfg.sampling.truth_batch_size)
        );
        let patterns: Vec<&[u8]> = chunk
            .iter()
            .map(|&i| selected[i].needle.as_slice())
            .collect();
        let mut bitmaps: Vec<Bitmap> = patterns
            .iter()
            .map(|_| Bitmap::new(ds.num_rows()))
            .collect();
        let ac = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            .kind(Some(AhoCorasickKind::ContiguousNFA))
            .build(patterns)?;
        for (row_id, row) in ds.rows().enumerate() {
            for found in ac.find_overlapping_iter(row) {
                bitmaps[found.pattern().as_usize()].set(row_id as u64);
            }
        }
        for (local, bitmap) in bitmaps.into_iter().enumerate() {
            let selected_index = chunk[local];
            let actual = bitmap.count();
            if actual != selected[selected_index].match_count {
                return Err(format!(
                    "internal exact-count disagreement for needle {}: catalogue {}, truth {}",
                    display_needle(&selected[selected_index].needle),
                    selected[selected_index].match_count,
                    actual
                )
                .into());
            }
            truths[selected_index] = Some(Truth {
                count: actual,
                hash: bitmap.truth_hash(),
                algo: TRUTH_ALGO.to_string(),
                sample_indices: bitmap.first_indices(8),
            });
        }
    }

    selected
        .iter()
        .enumerate()
        .map(|(i, query)| {
            let truth = truths[i].clone().ok_or("missing computed truth")?;
            let selectivity = truth.count as f64 / ds.num_rows() as f64;
            let rarest = query
                .needle
                .iter()
                .map(|&b| ds.manifest.byte_freq[b as usize])
                .min()
                .map(|n| n as f64 / ds.manifest.payload_bytes.max(1) as f64);
            Ok(QueryRecord {
                id: format!("{}.contains.q{:06}", ds.manifest.id, i + 1),
                op: "contains".to_string(),
                needles: vec![needle_json(&query.needle)],
                meta: Some(serde_json::json!({
                    "optimize_prefilter": {
                        "generator": GENERATOR_VERSION,
                        "seed": seed,
                        "profile": profile.name,
                        "max_selectivity_percent": profile.max_selectivity_percent,
                        "band_index": query.band.index,
                        "band_label": query.band.label,
                        "band_min_matches": query.band.min_matches,
                        "band_max_matches_exclusive": query.band.max_matches_exclusive,
                        "target_len": query.len,
                        "cell_requested": query.requested,
                        "provenance": query.provenance,
                    }
                })),
                truth: Some(truth),
                derived: Some(serde_json::json!({
                    "selectivity": selectivity,
                    "match_count": query.match_count,
                    "needle_lens": [query.len],
                    "needle_len_total": query.len,
                    "rarest_byte_freq": rarest,
                })),
            })
        })
        .collect()
}

fn needle_json(bytes: &[u8]) -> NeedleJson {
    match std::str::from_utf8(bytes) {
        Ok(text) => NeedleJson::Text(text.to_string()),
        Err(_) => NeedleJson::B64 {
            b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    }
}

fn write_suite(out: &Path, manifest: &SuiteManifest, records: &[QueryRecord]) -> Result<()> {
    let mut lines = String::new();
    for record in records {
        serde_json::to_writer(&mut lines_as_writer(&mut lines), record)?;
        lines.push('\n');
    }
    std::fs::write(out.join(QUERIES_FILE), lines)?;
    std::fs::write(
        out.join(SUITE_FILE),
        format!("{}\n", serde_json::to_string_pretty(manifest)?),
    )?;
    Ok(())
}

// serde_json writes bytes; this adapter lets us retain one allocation for JSONL.
fn lines_as_writer(target: &mut String) -> impl std::io::Write + '_ {
    struct Writer<'a>(&'a mut String);
    impl std::io::Write for Writer<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let text = std::str::from_utf8(buf)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            self.0.push_str(text);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    Writer(target)
}

fn write_reports(
    cfg: &Config,
    profile: &EffectiveProfile,
    ds: &PreparedDataset,
    out: &Path,
    seed: u64,
    bands: &[Band],
    coverage: &[CoverageRow],
    records: &[QueryRecord],
) -> Result<()> {
    let active_bands: Vec<_> = bands
        .iter()
        .filter_map(|band| profile_band(profile, ds.num_rows(), band))
        .collect();
    let full = coverage.iter().filter(|r| r.status == "full").count();
    let partial = coverage.iter().filter(|r| r.status == "partial").count();
    let missing = coverage
        .iter()
        .filter(|r| r.status == "not_observed")
        .count();
    let report = GenReport {
        generator: GENERATOR_VERSION,
        seed,
        profile,
        dataset: serde_json::json!({
            "id": ds.manifest.id,
            "checksum": ds.manifest.checksum,
            "num_rows": ds.num_rows(),
            "payload_bytes": ds.manifest.payload_bytes,
        }),
        methodology: serde_json::json!({
            "candidate_discovery": "bounded deterministic row-uniform + occurrence-uniform real-substring sampling with structural anchors",
            "candidate_draws_per_length": cfg.sampling.candidate_draws,
            "truth": "exact full-dataset Aho-Corasick scan; canonical bitmap-xxh3-v1",
            "uniqueness": "decoded needle bytes, globally within the dataset suite",
            "ordering": ["exact match_count/selectivity", "needle length", "needle bytes"],
            "caveat": "sampled_eligible is catalogue coverage, not proof that an unobserved cell is mathematically impossible",
        }),
        queries: records.len(),
        unique_needles: records.len(),
        full_cells: full,
        partial_cells: partial,
        not_observed_cells: missing,
        bands: &active_bands,
        coverage,
    };
    std::fs::write(
        out.join(REPORT_FILE),
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;
    write_coverage_csv(out, coverage)?;
    write_query_csv(out, records)?;
    write_query_markdown(cfg, profile, ds, out, seed, coverage, records)?;
    Ok(())
}

fn write_coverage_csv(out: &Path, coverage: &[CoverageRow]) -> Result<()> {
    let mut text = String::from(
        "band_index,band_label,min_matches,max_matches_exclusive,needle_len,requested,emitted,sampled_eligible,status\n",
    );
    for row in coverage {
        writeln!(
            text,
            "{},{},{},{},{},{},{},{},{}",
            row.band_index,
            csv_field(&row.band_label),
            row.min_matches,
            row.max_matches_exclusive,
            row.needle_len,
            row.requested,
            row.emitted,
            row.sampled_eligible,
            row.status
        )?;
    }
    std::fs::write(out.join(COVERAGE_CSV), text)?;
    Ok(())
}

fn write_query_csv(out: &Path, records: &[QueryRecord]) -> Result<()> {
    let mut text = String::from(
        "rank,query_id,selectivity,selectivity_percent,match_count,needle_len,needle_display,needle_base64,band_index,band_label,provenance,sample_indices\n",
    );
    for (rank, record) in records.iter().enumerate() {
        let needle = record.needles[0].decode()?;
        let truth = record.truth.as_ref().unwrap();
        let derived = record.derived.as_ref().unwrap();
        let meta = &record.meta.as_ref().unwrap()["optimize_prefilter"];
        writeln!(
            text,
            "{},{},{:.12},{:.9},{},{},{},{},{},{},{},{}",
            rank + 1,
            csv_field(&record.id),
            derived["selectivity"].as_f64().unwrap(),
            100.0 * derived["selectivity"].as_f64().unwrap(),
            truth.count,
            needle.len(),
            csv_field(&display_needle(&needle)),
            base64::engine::general_purpose::STANDARD.encode(&needle),
            meta["band_index"].as_u64().unwrap(),
            csv_field(meta["band_label"].as_str().unwrap()),
            csv_field(meta["provenance"].as_str().unwrap()),
            csv_field(&serde_json::to_string(&truth.sample_indices)?),
        )?;
    }
    std::fs::write(out.join(QUERY_CSV), text)?;
    Ok(())
}

fn write_query_markdown(
    cfg: &Config,
    profile: &EffectiveProfile,
    ds: &PreparedDataset,
    out: &Path,
    seed: u64,
    coverage: &[CoverageRow],
    records: &[QueryRecord],
) -> Result<()> {
    let full = coverage.iter().filter(|r| r.status == "full").count();
    let partial = coverage.iter().filter(|r| r.status == "partial").count();
    let missing = coverage
        .iter()
        .filter(|r| r.status == "not_observed")
        .count();
    let scope = profile
        .max_selectivity_percent
        .map(|pct| format!("exact selectivity < {pct}%"))
        .unwrap_or_else(|| "full selectivity space".to_string());
    let mut text = format!(
        "# Query catalogue: {} / {} (seed {})\n\n\
         This is the complete generated query set, ordered by **exact row selectivity first** and \
         **needle length second** (with needle bytes as the stable final tie-breaker). Every needle is \
         unique by decoded bytes. Selectivity and witness rows come from a full-dataset scan.\n\n\
         - Dataset: `{}` ({} rows, {} payload bytes)\n\
         - Dataset checksum: `{}`\n\
         - Profile: `{}` — {}\n\
         - Selectivity scope: {}\n\
         - Replicates per cell (zero / below {:.6}% / other): {} / {} / {}\n\
         - Benchmark queries: {} (every one is executed)\n\
         - Coverage-grid cells: {} total; {} full, {} partial, {} not observed\n\
         - Machine-readable companions: `{}`, `{}`, `{}`\n\n\
         `coverage.csv` has one row per `(selectivity band, needle length)` cell; those rows are not \
         queries. Each cell emits up to its requested replicate quota, and those emitted records are \
         the benchmark queries in `queries.jsonl`. This suite is balanced to map the performance \
         surface, rather than weighted to imitate one production application's query frequencies.\n\n\
         “Not observed” means the bounded candidate catalogue did not find an eligible needle; it is \
         intentionally not presented as proof that the cell is mathematically impossible.\n\n\
         ## Coverage gaps\n\n",
        ds.manifest.id,
        profile.name,
        seed,
        ds.manifest.id,
        ds.num_rows(),
        ds.manifest.payload_bytes,
        ds.manifest.checksum,
        profile.name,
        profile.description,
        scope,
        100.0 * cfg.experiment.low_selectivity_cutoff,
        profile.zero_replicates,
        profile.low_selectivity_replicates,
        profile.other_replicates,
        records.len(),
        coverage.len(),
        full,
        partial,
        missing,
        QUERY_CSV,
        COVERAGE_CSV,
        REPORT_FILE,
    );
    let gaps: Vec<_> = coverage.iter().filter(|row| row.status != "full").collect();
    if gaps.is_empty() {
        text.push_str("Every requested `(band, length)` cell is full.\n\n");
    } else {
        text.push_str(
            "These are the cells to inspect first when judging suite completeness. `sampled eligible` \
             counts exact-selectivity candidates observed during bounded discovery.\n\n\
             | Status | Needle length | Band | Requested | Emitted | Sampled eligible |\n\
             |---|---:|---|---:|---:|---:|\n",
        );
        for row in gaps {
            writeln!(
                text,
                "| {} | {} | {} | {} | {} | {} |",
                row.status,
                row.needle_len,
                row.band_label.replace('|', "\\|"),
                row.requested,
                row.emitted,
                row.sampled_eligible,
            )?;
        }
        text.push('\n');
    }
    text.push_str(
        "## All queries\n\n\
         | Rank | Query ID | Exact selectivity | Matches | Length | Needle (JSON text or `0x` bytes) | Band | Provenance | Witness rows |\n\
         |---:|---|---:|---:|---:|---|---|---|---|\n",
    );
    for (rank, record) in records.iter().enumerate() {
        let needle = record.needles[0].decode()?;
        let truth = record.truth.as_ref().unwrap();
        let derived = record.derived.as_ref().unwrap();
        let meta = &record.meta.as_ref().unwrap()["optimize_prefilter"];
        let display = display_needle(&needle).replace('|', "\\|");
        writeln!(
            text,
            "| {} | `{}` | {:.9}% | {} | {} | {} | B{}: {} | {} | `{}` |",
            rank + 1,
            record.id,
            100.0 * derived["selectivity"].as_f64().unwrap(),
            truth.count,
            needle.len(),
            display,
            meta["band_index"].as_u64().unwrap(),
            meta["band_label"].as_str().unwrap().replace('|', "\\|"),
            meta["provenance"].as_str().unwrap(),
            truth
                .sample_indices
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", "),
        )?;
    }
    std::fs::write(out.join(QUERY_DOCUMENT), text)?;
    Ok(())
}

fn display_needle(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if text.chars().all(|c| !c.is_control()) => serde_json::to_string(text).unwrap(),
        _ => format!(
            "0x{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ),
    }
}

fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn write_benchmark_spec(
    cfg: &Config,
    seed_dir: &Path,
    generated: &[(PathBuf, PathBuf, String)],
) -> Result<()> {
    let mut text =
        String::from("# Generated by optimize-prefilter. Paths are relative to this file.\n");
    writeln!(
        text,
        "strategies = {}\n",
        serde_json::to_string(&cfg.benchmark.strategies)?
    )?;
    writeln!(text, "[measure]")?;
    writeln!(text, "warmup = {}", cfg.benchmark.warmup)?;
    writeln!(text, "min_iters = {}", cfg.benchmark.min_iters)?;
    writeln!(text, "min_millis = {}", cfg.benchmark.min_millis)?;
    writeln!(
        text,
        "chunk_rows = {}\n",
        serde_json::to_string(&cfg.benchmark.chunk_rows)?
    )?;
    for (dataset, _, _) in generated {
        let relative = pathdiff::diff_paths(dataset, seed_dir).unwrap_or_else(|| dataset.clone());
        writeln!(text, "[[datasets]]")?;
        writeln!(
            text,
            "path = {}\n",
            serde_json::to_string(&relative.display().to_string())?
        )?;
    }
    for (_, suite, _) in generated {
        let relative = pathdiff::diff_paths(suite, seed_dir).unwrap_or_else(|| suite.clone());
        writeln!(text, "[[suites]]")?;
        writeln!(
            text,
            "path = {}\n",
            serde_json::to_string(&relative.display().to_string())?
        )?;
    }
    writeln!(text, "[[candidates]]")?;
    writeln!(text, "name = \"uncompressed_memmem\"")?;
    writeln!(text, "configs = [\"{{}}\"]\n")?;
    writeln!(text, "[[candidates]]")?;
    writeln!(text, "name = \"fsst\"\n")?;
    writeln!(text, "[[candidates]]")?;
    writeln!(text, "name = \"onpair_spiral\"")?;
    writeln!(
        text,
        "configs = {}\n",
        serde_json::to_string(&cfg.benchmark.onpair_configs)?
    )?;
    writeln!(text, "[[candidates]]")?;
    writeln!(text, "name = \"onpair_spiral_decode\"")?;
    writeln!(
        text,
        "configs = {}",
        serde_json::to_string(&cfg.benchmark.onpair_configs)?
    )?;
    writeln!(text, "\n[[scanners]]")?;
    writeln!(text, "name = \"memmem\"")?;
    writeln!(text, "\n[[scanners]]")?;
    writeln!(text, "name = \"memmem-hay\"")?;
    let spec_path = seed_dir.join("benchmark.toml");
    std::fs::write(&spec_path, text)?;
    // Fail generation immediately if path construction or TOML layout ever
    // stops matching the harness's benchmark-spec contract.
    lb_harness::spec::LoadedSpec::load(&spec_path)?;
    Ok(())
}

fn write_experiment_readme(
    cfg: &Config,
    profile: &EffectiveProfile,
    seed_dir: &Path,
    seed: u64,
    generated: &[(PathBuf, PathBuf, String)],
    queries: usize,
) -> Result<()> {
    let result_root = cfg
        .resolve(&cfg.benchmark.result_root)
        .join(&profile.name)
        .join(format!("seed-{seed}"));
    let mut datasets = String::new();
    for (_, suite, id) in generated {
        writeln!(
            datasets,
            "- `{id}`: [`{id}/queries.md`]({}/queries.md)",
            suite.file_name().unwrap().to_string_lossy()
        )?;
    }
    let text = format!(
        "# Prefilter experiment: {} / seed {seed}\n\n\
         {}\n\n\
         Selectivity scope: {}. Generated {queries} unique queries. Each dataset catalogue is sorted by exact selectivity, \
         then needle length.\n\n{datasets}\n\
         Benchmark with:\n\n\
         ```sh\n./experiments/optimize_prefilter/benchmark.sh --profile {} --seed {seed}\n```\n\n\
         The command writes a self-contained Benchmark Explorer to \
         `{}/explorer.html`.\n",
        profile.name,
        profile.description,
        profile
            .max_selectivity_percent
            .map(|pct| format!("exactly < {pct}%"))
            .unwrap_or_else(|| "full space".to_string()),
        profile.name,
        result_root.display()
    );
    std::fs::write(seed_dir.join("README.md"), text)?;
    Ok(())
}

// splitmix64-seeded xoshiro256**. Kept local so catalogue determinism does not
// depend on an external RNG implementation or release.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Rng([u64; 4]);

impl Rng {
    fn from_seed(mut seed: u64) -> Self {
        Self([
            splitmix64(&mut seed),
            splitmix64(&mut seed),
            splitmix64(&mut seed),
            splitmix64(&mut seed),
        ])
    }

    fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    fn below(&mut self, n: u64) -> u64 {
        debug_assert!(n > 0);
        loop {
            let value = self.next_u64();
            let remainder = value % n;
            if value - remainder <= u64::MAX - (n - 1) {
                return remainder;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lb_harness::dataset::{self, PreparedDataset};
    use lb_harness::suite::Suite;

    #[test]
    fn bands_partition_every_integer_count() {
        for rows in [1, 7, 200, 1_000_000] {
            let bands = selectivity_bands(rows, 4, 0.02);
            assert_eq!(bands[0].min_matches, 0);
            assert_eq!(bands[0].max_matches_exclusive, 1);
            for count in 0..=rows {
                assert_eq!(
                    bands.iter().filter(|band| band.contains(count)).count(),
                    1,
                    "rows={rows}, count={count}"
                );
            }
        }
    }

    #[test]
    fn million_rows_has_dense_low_selectivity_coverage() {
        let bands = selectivity_bands(1_000_000, 4, 0.02);
        let low = bands
            .iter()
            .filter(|band| band.min_matches > 0 && band.max_matches_exclusive <= 20_000)
            .count();
        assert!(low >= 17, "found only {low} positive bands below 2%");
    }

    #[test]
    fn seed_ranking_is_stable_and_changes() {
        let bytes = b"needle";
        assert_eq!(
            selection_score(42, "xxh3:a", bytes),
            selection_score(42, "xxh3:a", bytes)
        );
        assert_ne!(
            selection_score(42, "xxh3:a", bytes),
            selection_score(43, "xxh3:a", bytes)
        );
    }

    #[test]
    fn needle_display_is_unambiguous() {
        assert_eq!(display_needle(b"hello"), "\"hello\"");
        assert_eq!(display_needle(&[0xff, 0]), "0xff00");
    }

    #[test]
    fn fixture_suite_is_unique_sorted_and_harness_loadable() {
        let tmp = tempfile::tempdir().unwrap();
        let experiment = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dataset_dir = tmp.path().join("dataset");
        dataset::ingest(&dataset::IngestRequest {
            source: experiment.join("tests/fixtures/mini.csv"),
            format: "csv".to_string(),
            column: "data".to_string(),
            id: "mini".to_string(),
            out_dir: dataset_dir.clone(),
        })
        .unwrap();
        let ds = PreparedDataset::load(&dataset_dir, true).unwrap();
        let profile = EffectiveProfile {
            name: "test".to_string(),
            description: "fixture profile".to_string(),
            zero_replicates: 2,
            low_selectivity_replicates: 2,
            other_replicates: 2,
            max_selectivity_percent: None,
        };
        let cfg = Config {
            format_version: 1,
            default_profile: profile.name.clone(),
            experiment: ExperimentConfig {
                output_root: tmp.path().join("generated"),
                cache_root: tmp.path().join("cache"),
                min_needle_len: 1,
                max_needle_len: 8,
                log_bands_per_decade: 4,
                low_selectivity_cutoff: 0.02,
            },
            profiles: BTreeMap::from([(
                profile.name.clone(),
                ProfileConfig {
                    description: profile.description.clone(),
                    zero_replicates: profile.zero_replicates,
                    low_selectivity_replicates: profile.low_selectivity_replicates,
                    other_replicates: profile.other_replicates,
                    max_selectivity_percent: profile.max_selectivity_percent,
                },
            )]),
            sampling: SamplingConfig {
                candidate_draws: 2_000,
                anchor_rows: 200,
                zero_probe_candidates: 256,
                catalog_entries_per_cell: 16,
                truth_batch_size: 64,
            },
            benchmark: BenchmarkConfig {
                result_root: tmp.path().join("results"),
                onpair_configs: vec!["{}".to_string()],
                strategies: vec![
                    "memmem".to_string(),
                    "memmem-hay".to_string(),
                    "decode".to_string(),
                    "pf_memmem".to_string(),
                    "kmp".to_string(),
                ],
                warmup: 0,
                min_iters: 1,
                min_millis: 0,
                chunk_rows: vec![0],
            },
            datasets: Vec::new(),
            config_path: tmp.path().join("config.toml"),
            config_dir: tmp.path().to_path_buf(),
        };
        let out = tmp.path().join("suite");
        let count = generate_dataset(&cfg, &profile, &ds, &out, 42, false, false).unwrap();
        let suite = Suite::load_for_run(&out, &ds).unwrap();
        assert_eq!(suite.queries.len(), count);
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join(REPORT_FILE)).unwrap()).unwrap();
        let emitted: u64 = report["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|cell| cell["emitted"].as_u64().unwrap())
            .sum();
        assert_eq!(
            emitted as usize, count,
            "every emitted query is benchmarked"
        );

        let mut seen = HashSet::new();
        let mut previous = None;
        let mut saw_zero_probe = false;
        for query in &suite.queries {
            assert!(
                seen.insert(query.needles[0].clone()),
                "duplicate decoded needle"
            );
            let truth = query.record.truth.as_ref().unwrap();
            if truth.count == 0 {
                saw_zero_probe = true;
                assert!(
                    query.needles[0]
                        .iter()
                        .all(|&byte| ds.manifest.byte_freq[byte as usize] > 0),
                    "zero probe contains a byte absent from the dataset"
                );
            }
            let key = (
                truth.count,
                query.needles[0].len(),
                query.needles[0].clone(),
            );
            if let Some(prior) = previous {
                assert!(prior <= key, "queries are not in promised report order");
            }
            previous = Some(key);
        }
        assert!(
            saw_zero_probe,
            "fixture should exercise synthetic negatives"
        );
        for file in [QUERY_DOCUMENT, QUERY_CSV, COVERAGE_CSV, REPORT_FILE] {
            assert!(out.join(file).is_file(), "missing {file}");
        }

        let second = tmp.path().join("suite-second");
        generate_dataset(&cfg, &profile, &ds, &second, 42, false, false).unwrap();
        for file in [
            SUITE_FILE,
            QUERIES_FILE,
            QUERY_DOCUMENT,
            QUERY_CSV,
            COVERAGE_CSV,
            REPORT_FILE,
        ] {
            assert_eq!(
                std::fs::read(out.join(file)).unwrap(),
                std::fs::read(second.join(file)).unwrap(),
                "same dataset/config/seed must reproduce {file} byte-for-byte"
            );
        }

        let capped_profile = EffectiveProfile {
            name: "under-50pct".to_string(),
            description: "strict fixture ceiling".to_string(),
            max_selectivity_percent: Some(50.0),
            ..profile
        };
        let capped_out = tmp.path().join("suite-capped");
        generate_dataset(&cfg, &capped_profile, &ds, &capped_out, 42, false, false).unwrap();
        let capped = Suite::load_for_run(&capped_out, &ds).unwrap();
        assert!(capped.queries.iter().all(|query| {
            query.record.truth.as_ref().unwrap().count * 100 < ds.num_rows() * 50
        }));

        let outcome = generate_experiment(
            &cfg,
            Some("test"),
            7,
            std::slice::from_ref(&dataset_dir),
            false,
            false,
        )
        .unwrap();
        assert!(outcome.seed_dir.ends_with("test/seed-7"));
        let benchmark_path = outcome.seed_dir.join("benchmark.toml");
        assert!(benchmark_path.is_file());
        let benchmark = lb_harness::spec::LoadedSpec::load(&benchmark_path).unwrap();
        assert_eq!(
            benchmark
                .spec
                .candidates
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>(),
            [
                "uncompressed_memmem",
                "fsst",
                "onpair_spiral",
                "onpair_spiral_decode",
            ]
        );
        assert_eq!(
            benchmark
                .spec
                .scanners
                .iter()
                .map(|scanner| scanner.name.as_str())
                .collect::<Vec<_>>(),
            ["memmem", "memmem-hay"]
        );
        assert_eq!(outcome.profile, "test");
    }
}
