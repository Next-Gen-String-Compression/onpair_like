//! Experiment policy lives here; substring discovery and selection live in the
//! harness. This command prepares suites, not fitted coefficients or timings.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use lb_harness::{
    dataset::{DatasetManifest, PreparedDataset},
    gen, suite,
};
use serde::Deserialize;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "experiments/matcher_selection/config.toml")]
    config: PathBuf,
    /// Restrict preparation to named datasets, preserving manifest order.
    #[arg(long)]
    dataset: Vec<String>,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    #[arg(long)]
    per_cell: Option<usize>,
    /// Override the configured index workspace budget (excludes the dataset).
    #[arg(long)]
    index_memory_mib: Option<u64>,
    /// Run the independent oracle and verify counts and bucket assignments.
    #[arg(long)]
    bless: bool,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    list: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    output_root: PathBuf,
    cache_root: PathBuf,
    per_cell: usize,
    index_memory_mib: u64,
    negative_attempts: usize,
    datasets: Vec<Dataset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dataset {
    id: String,
    path: PathBuf,
}

fn validate(config: &Config) -> suite::Result<()> {
    let mut ids = HashSet::new();
    for ds in &config.datasets {
        if !ids.insert(&ds.id) {
            return Err(format!("duplicate dataset id {}", ds.id).into());
        }
        if ds.id.is_empty() || ds.id.contains(['/', '\\']) || ds.id == "." || ds.id == ".." {
            return Err("dataset IDs must be nonempty path components".into());
        }
    }
    Ok(())
}

fn main() -> suite::Result<()> {
    run(Args::parse())
}

fn run(args: Args) -> suite::Result<()> {
    let config: Config = toml::from_str(&std::fs::read_to_string(&args.config)?)?;
    validate(&config)?;
    let base = args
        .config
        .parent()
        .ok_or("config has no parent directory")?;
    for id in &args.dataset {
        if !config.datasets.iter().any(|ds| &ds.id == id) {
            return Err(format!("unknown dataset {id}").into());
        }
    }
    let selected: Vec<_> = config
        .datasets
        .iter()
        .filter(|ds| args.dataset.is_empty() || args.dataset.contains(&ds.id))
        .collect();
    if args.list {
        println!("dataset\trows\tpayload MiB\tindex MiB\tstatus");
        for ds in selected {
            let path = base.join(&ds.path);
            if path.join("data.arrow").exists() {
                let manifest = DatasetManifest::load(&path)?;
                let bytes = gen::IndexLimits::required_memory_bytes(
                    manifest.payload_bytes,
                    manifest.num_rows,
                )?;
                let mib = bytes.div_ceil(1 << 20);
                let status = if mib > args.index_memory_mib.unwrap_or(config.index_memory_mib) {
                    "over budget"
                } else {
                    "prepared"
                };
                println!(
                    "{}\t{}\t{:.1}\t{mib}\t{status}",
                    ds.id,
                    manifest.num_rows,
                    manifest.payload_bytes as f64 / (1 << 20) as f64
                );
            } else {
                println!("{}\t-\t-\t-\tmissing", ds.id);
            }
        }
        return Ok(());
    }
    if selected.is_empty() {
        return Err("no datasets selected".into());
    }
    let output = base.join(&config.output_root);
    let index_memory_mib = args.index_memory_mib.unwrap_or(config.index_memory_mib);
    let limits = gen::IndexLimits {
        max_needle_len: 256,
        memory_budget_bytes: index_memory_mib
            .checked_mul(1 << 20)
            .ok_or("memory budget overflow")?,
    };
    // Check the whole request before loading columns or generating any suites.
    for ds in &selected {
        let path = base.join(&ds.path);
        if !path.join("data.arrow").exists() {
            return Err(format!("{} is not prepared; run experiments/matcher_selection/prepare.py or select available datasets with --dataset", ds.id).into());
        }
        let manifest = DatasetManifest::load(&path)?;
        if manifest.id != ds.id {
            return Err(format!("{}: configured ID differs from dataset manifest", ds.id).into());
        }
        let required =
            gen::IndexLimits::required_memory_bytes(manifest.payload_bytes, manifest.num_rows)?;
        if required > limits.memory_budget_bytes {
            return Err(format!("{} needs an index budget of at least {} MiB (configured: {index_memory_mib}); pass --index-memory-mib or change the config", ds.id, required.div_ceil(1 << 20)).into());
        }
    }
    for entry in selected {
        let ds = PreparedDataset::load(&base.join(&entry.path), true)?;
        if ds.manifest.id != entry.id {
            return Err(
                format!("{}: configured ID differs from dataset manifest", entry.id).into(),
            );
        }
        let mut request = gen::BalancedRequest::new(ds.num_rows(), args.seed);
        request.per_cell = args.per_cell.unwrap_or(config.per_cell);
        request.negative_attempts = config.negative_attempts;
        let key = request.suite_key(&ds.manifest.checksum)?;
        let out = output.join(&entry.id).join(&key);
        if out.join(suite::QUERIES_FILE).exists() && !args.force {
            return Err(format!(
                "{} already generated; pass --force to replace",
                out.display()
            )
            .into());
        }
        eprintln!(
            "{}: indexing {} rows, {} payload bytes",
            entry.id,
            ds.num_rows(),
            ds.manifest.payload_bytes
        );
        let start = Instant::now();
        let index = gen::SubstringIndex::cached(
            ds.payload(),
            ds.offsets_u64(),
            limits,
            &base.join(&config.cache_root),
        )?;
        let index_seconds = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let generated = index.generate(&request)?;
        let generation_seconds = start.elapsed().as_secs_f64();
        drop(index);
        gen::write_balanced_suite(
            &generated,
            &ds,
            &out,
            &format!("{}.{key}", entry.id),
            args.force,
        )?;
        let start = Instant::now();
        if args.bless {
            suite::bless(&out, &ds, args.force)?;
            gen::verify_balanced_suite(&out, &ds)?;
        }
        let filled = generated
            .cells
            .iter()
            .filter(|c| c.status == "filled")
            .count();
        println!(
            "{}: {} unique needles, {filled}/{} cells filled; index {:.3}s, generation {:.3}s -> {}",
            entry.id,
            generated.needles.len(),
            generated.cells.len(),
            index_seconds,
            generation_seconds,
            out.display()
        );
        let report = serde_json::json!({"id": entry.id,
            "suite_key": key,
            "index_memory_mib": index_memory_mib,
            "checksum": ds.manifest.checksum, "queries": generated.needles.len(), "cells": generated.cells,
            "index_seconds": index_seconds, "generation_seconds": generation_seconds,
            "oracle_seconds": args.bless.then(|| start.elapsed().as_secs_f64()), "blessed": args.bless});
        std::fs::write(
            out.join("preparation.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn prepare_config(base: &Path, name: &str, ids: &[&str]) -> PathBuf {
        let mut text = format!("output_root = {name:?}\ncache_root = \"cache\"\nper_cell = 2\nindex_memory_mib = 128\nnegative_attempts = 40\n");
        for id in ids {
            text.push_str(&format!("\n[[datasets]]\nid = {id:?}\npath = {id:?}\n"));
        }
        let path = base.join(format!("{name}.toml"));
        std::fs::write(&path, text).unwrap();
        path
    }

    fn generate(config: PathBuf, seed: u64, per_cell: Option<usize>) -> suite::Result<()> {
        run(Args {
            config,
            dataset: Vec::new(),
            seed,
            per_cell,
            index_memory_mib: None,
            bless: false,
            force: false,
            list: false,
        })
    }

    fn suites_in(base: &Path, output: &str, id: &str) -> Vec<PathBuf> {
        let mut paths: Vec<_> = std::fs::read_dir(base.join(output).join(id))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        paths
    }

    fn prepare_fixture(base: &Path, id: &str) {
        lb_harness::dataset::ingest(&lb_harness::dataset::IngestRequest {
            source: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../datasets/fixtures/mini.csv"),
            format: "csv".into(),
            column: "data".into(),
            id: id.into(),
            out_dir: base.join(id),
        })
        .unwrap();
    }

    #[test]
    fn preflight_rejects_missing_inputs_and_small_budgets_before_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        prepare_fixture(base, "a");
        let missing = prepare_config(base, "missing-input", &["a", "missing"]);
        assert!(generate(missing, 42, None)
            .unwrap_err()
            .to_string()
            .contains("not prepared"));
        assert!(!base.join("missing-input").exists());
        let config = prepare_config(base, "small-budget", &["a"]);
        let text = std::fs::read_to_string(&config)
            .unwrap()
            .replace("index_memory_mib = 128", "index_memory_mib = 1");
        std::fs::write(&config, text).unwrap();
        assert!(generate(config.clone(), 42, None)
            .unwrap_err()
            .to_string()
            .contains("needs an index budget"));
        assert!(!base.join("small-budget").exists());
        run(Args::parse_from([
            "matcher-selection",
            "--config",
            config.to_str().unwrap(),
            "--index-memory-mib",
            "128",
        ]))
        .unwrap();
        assert!(base.join("small-budget/a").exists());
    }

    #[test]
    fn configured_datasets_have_preparation_recipes() {
        let mut config: Config = toml::from_str(include_str!("../config.toml")).unwrap();
        validate(&config).unwrap();
        let sources = include_str!("../../../datasets/sources.yaml");
        let recipes: HashSet<_> = sources
            .lines()
            .filter_map(|line| line.strip_prefix("  - id: "))
            .collect();
        assert!(config
            .datasets
            .iter()
            .all(|ds| recipes.contains(ds.id.as_str())));
        config.datasets[1].id = config.datasets[0].id.clone();
        assert!(validate(&config).is_err());
    }

    #[test]
    fn suites_are_independent_and_different_settings_coexist() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        // Identical data under two IDs deliberately exercises overlapping
        // needles: the second suite must keep them, not remove the first's.
        for id in ["a", "b"] {
            prepare_fixture(base, id);
        }
        let alone = prepare_config(base, "alone", &["b"]);
        generate(alone.clone(), 42, None).unwrap();
        generate(prepare_config(base, "together", &["a", "b"]), 42, None).unwrap();
        generate(prepare_config(base, "reversed", &["b", "a"]), 42, None).unwrap();
        let original = suites_in(base, "alone", "b").pop().unwrap();
        for output in ["together", "reversed"] {
            let other = suites_in(base, output, "b").pop().unwrap();
            assert_eq!(original.file_name(), other.file_name());
            for file in ["queries.jsonl", "suite.json", "gen-report.json"] {
                assert_eq!(
                    std::fs::read(original.join(file)).unwrap(),
                    std::fs::read(other.join(file)).unwrap()
                );
            }
        }
        let bytes = std::fs::read(original.join("queries.jsonl")).unwrap();
        generate(alone.clone(), 43, None).unwrap();
        generate(alone.clone(), 42, Some(3)).unwrap();
        assert_eq!(suites_in(base, "alone", "b").len(), 3);
        assert_eq!(
            bytes,
            std::fs::read(original.join("queries.jsonl")).unwrap()
        );
        // An exact repeat requires explicit permission to replace its own files.
        assert!(generate(alone, 42, None).is_err());
    }
}
