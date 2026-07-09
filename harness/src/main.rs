//! `bench` — the LIKE-benchmark CLI.
//!
//! Subcommands: ingest (source → canonical artifact), bless (oracle →
//! truth), run (spec → results.jsonl + manifest), check (validate a
//! suite/dataset pairing). `run` re-executes itself with hidden --worker-*
//! flags: one child process per (candidate, config, dataset), so candidates
//! are isolated from each other with no IPC in the timing path.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use lb_harness::dataset::{self, PreparedDataset};
use lb_harness::registry;
use lb_harness::results::{self, Row, Writer};
use lb_harness::runner;
use lb_harness::spec::LoadedSpec;
use lb_harness::suite::{self, Suite};

const EXIT_ERROR: u8 = 2;
const EXIT_GATE_FAILURE: u8 = 3;

#[derive(Parser)]
#[command(name = "bench", version, about = "LIKE-family predicate benchmark")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Convert a raw source column into a canonical dataset artifact.
    Ingest {
        /// Source file (parquet, csv, or tsv).
        #[arg(long)]
        source: PathBuf,
        /// Source format: parquet | csv | tsv.
        #[arg(long)]
        format: String,
        /// Column name (or 0-based index for csv/tsv).
        #[arg(long)]
        column: String,
        /// Dataset id (its human name; the checksum is its identity).
        #[arg(long)]
        id: String,
        /// Output directory (will contain data.arrow + manifest.json).
        #[arg(long, short)]
        out: PathBuf,
    },
    /// Generate a parameterized query suite from a dataset (DESIGN.md §14).
    Gen {
        #[arg(long)]
        dataset: PathBuf,
        /// Output suite directory (suite.json + queries.jsonl + gen-report.json).
        #[arg(long, short)]
        out: PathBuf,
        /// Determinism root: same dataset + seed + profile + ops => byte-identical suite.
        #[arg(long)]
        seed: u64,
        /// full | quick (quick: reduced grid for iteration).
        #[arg(long, default_value = "full")]
        profile: String,
        /// Comma-separated op filter, e.g. "contains,prefix" (default: all five).
        #[arg(long)]
        ops: Option<String>,
        /// Suite id (default: the out directory's basename).
        #[arg(long)]
        id: Option<String>,
        /// Overwrite an existing generated suite (discards blessed truth).
        #[arg(long)]
        force: bool,
    },
    /// Run the oracle over a suite and cache ground truth in it.
    Bless {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        dataset: PathBuf,
        /// Re-bless even if existing truth disagrees with the oracle.
        #[arg(long)]
        force: bool,
    },
    /// Validate a dataset artifact and a suite's binding to it.
    Check {
        #[arg(long)]
        suite: PathBuf,
        #[arg(long)]
        dataset: PathBuf,
        /// Also recompute the oracle for every query and verify truth.
        #[arg(long)]
        verify_truth: bool,
    },
    /// Execute a run spec: all selected cells, gated, into results.jsonl.
    Run {
        spec: PathBuf,
        /// Output directory for results.jsonl, manifest.json, partials/.
        #[arg(long, short)]
        out: PathBuf,
        /// Abort at the first gate failure (debugging aid).
        #[arg(long)]
        fail_fast: bool,
        // Hidden worker-mode flags: the parent re-executes itself with
        // these to run one (candidate, config, dataset) in a child.
        #[arg(long, hide = true)]
        worker_candidate: Option<String>,
        #[arg(long, hide = true)]
        worker_config: Option<usize>,
        #[arg(long, hide = true)]
        worker_dataset: Option<usize>,
    },
    /// List registered candidates and scanners with their capabilities.
    List,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

type Error = Box<dyn std::error::Error>;

fn run(cli: Cli) -> Result<ExitCode, Error> {
    match cli.cmd {
        Cmd::Ingest { source, format, column, id, out } => {
            let manifest = dataset::ingest(&dataset::IngestRequest {
                source,
                format,
                column,
                id,
                out_dir: out.clone(),
            })?;
            println!(
                "ingested {}: {} rows, {} payload bytes, checksum {} -> {}",
                manifest.id,
                manifest.num_rows,
                manifest.payload_bytes,
                manifest.checksum,
                out.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Gen { dataset: ds_dir, out, seed, profile, ops, id, force } => {
            let ds = PreparedDataset::load(&ds_dir, true)?;
            let ops = ops
                .map(|s| {
                    s.split(',')
                        .map(|name| {
                            lb_abi::op_from_name(name.trim())
                                .ok_or_else(|| format!("unknown op {name:?}"))
                        })
                        .collect::<Result<Vec<u32>, _>>()
                })
                .transpose()?;
            let suite_id = match id {
                Some(id) => id,
                None => out
                    .file_name()
                    .ok_or("--out has no basename; pass --id explicitly")?
                    .to_string_lossy()
                    .into_owned(),
            };
            let params = lb_harness::gen::GenParams {
                seed,
                profile: lb_harness::gen::Profile::parse(&profile)?,
                ops,
                suite_id,
            };
            let outcome = lb_harness::gen::generate(&ds, &out, &params, force)?;
            println!(
                "generated {}: {} queries; grid {}/{} filled, {} partial, {} empty \
                 (coverage: {})",
                out.display(),
                outcome.queries,
                outcome.filled_points,
                outcome.grid_points,
                outcome.partial_points,
                outcome.empty_points,
                out.join(lb_harness::gen::REPORT_FILE).display(),
            );
            println!(
                "next: bench bless --suite {} --dataset {}",
                out.display(),
                ds_dir.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Bless { suite: suite_dir, dataset: ds_dir, force } => {
            let ds = PreparedDataset::load(&ds_dir, true)?;
            let outcome = suite::bless(&suite_dir, &ds, force)?;
            println!(
                "blessed {} ({} newly blessed, {} verified against existing truth)",
                suite_dir.display(),
                outcome.blessed,
                outcome.verified
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Check { suite: suite_dir, dataset: ds_dir, verify_truth } => {
            let ds = PreparedDataset::load(&ds_dir, true)?;
            let suite = Suite::load_for_run(&suite_dir, &ds)?;
            println!(
                "ok: dataset {} ({} rows, checksum verified); suite {} ({} queries, binding + truth present)",
                ds.manifest.id,
                ds.num_rows(),
                suite.manifest.id,
                suite.queries.len()
            );
            if verify_truth {
                for q in &suite.queries {
                    let needles: Vec<&[u8]> = q.needles.iter().map(|n| n.as_slice()).collect();
                    let bm = lb_harness::oracle::eval(q.op, &needles, ds.num_rows(), ds.rows());
                    let t = q.record.truth.as_ref().unwrap();
                    if bm.count() != t.count || bm.truth_hash() != t.hash {
                        return Err(format!(
                            "query {}: stored truth disagrees with the oracle — re-bless",
                            q.record.id
                        )
                        .into());
                    }
                }
                println!("ok: all {} truths verified against the oracle", suite.queries.len());
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Run { spec, out, fail_fast, worker_candidate, worker_config, worker_dataset } => {
            let loaded = LoadedSpec::load(&spec)?;
            match worker_candidate {
                Some(candidate) => run_worker_process(
                    &loaded,
                    &candidate,
                    worker_config.ok_or("--worker-config required")?,
                    worker_dataset.ok_or("--worker-dataset required")?,
                    &out,
                    fail_fast,
                ),
                None => run_parent(&loaded, &out, fail_fast),
            }
        }
        Cmd::List => {
            for c in registry::candidates() {
                let strategies: Vec<&str> =
                    c.strategies.iter().map(|s| s.name.as_str()).collect();
                println!(
                    "candidate {} v{} (cpu: {}) strategies={:?} view={} decode={}",
                    c.name,
                    c.version,
                    c.cpu_features.as_deref().unwrap_or("portable"),
                    strategies,
                    c.has_view(),
                    c.has_decode(),
                );
            }
            for s in registry::scanners() {
                println!(
                    "scanner   {} v{} (cpu: {}) ops={:#07b}",
                    s.name,
                    s.version,
                    s.cpu_features.as_deref().unwrap_or("portable"),
                    s.supported_ops
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn partial_name(candidate: &str, config_idx: usize, dataset_idx: usize) -> String {
    format!("{candidate}.{config_idx}.{dataset_idx}.jsonl")
}

fn run_worker_process(
    loaded: &LoadedSpec,
    candidate: &str,
    config_idx: usize,
    dataset_idx: usize,
    out_dir: &std::path::Path,
    fail_fast: bool,
) -> Result<ExitCode, Error> {
    let path = out_dir
        .join("partials")
        .join(partial_name(candidate, config_idx, dataset_idx));
    let mut writer = Writer::create(&path)?;
    let summary =
        runner::run_worker(loaded, candidate, config_idx, dataset_idx, &mut writer, fail_fast)?;
    writer.finish()?;
    if summary.gate_failures > 0 {
        eprintln!(
            "worker {candidate}#{config_idx} on dataset #{dataset_idx}: {} gate failure(s)",
            summary.gate_failures
        );
        return Ok(ExitCode::from(EXIT_GATE_FAILURE));
    }
    if summary.errors > 0 {
        return Ok(ExitCode::from(EXIT_ERROR));
    }
    Ok(ExitCode::SUCCESS)
}

fn run_parent(loaded: &LoadedSpec, out_dir: &std::path::Path, fail_fast: bool) -> Result<ExitCode, Error> {
    let spec = &loaded.spec;
    let started_at = humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string();
    std::fs::create_dir_all(out_dir.join("partials"))?;

    // ---- validate everything up front: fail before any child runs ----
    let env = lb_harness::cpu::capture_env();
    if let Some(gov) = &env.cpu_governor {
        if gov != "performance" {
            eprintln!("warn: CPU governor is {gov:?}, not \"performance\" — latency will be noisy");
        }
    }

    let mut datasets_meta = Vec::new();
    let mut dataset_ids = Vec::new();
    for d in &spec.datasets {
        let m = dataset::DatasetManifest::load(&d.path)?;
        dataset_ids.push(m.id.clone());
        datasets_meta.push(serde_json::json!({
            "id": m.id, "path": d.path.display().to_string(),
            "checksum": m.checksum, "num_rows": m.num_rows,
            "payload_bytes": m.payload_bytes,
        }));
    }
    let mut suites_meta = Vec::new();
    for s in &spec.suites {
        let su = Suite::load_unblessed(&s.path)?;
        if !dataset_ids.contains(&su.manifest.dataset.id) {
            return Err(format!(
                "suite {} is bound to dataset {:?}, which the spec does not select",
                su.manifest.id, su.manifest.dataset.id
            )
            .into());
        }
        suites_meta.push(serde_json::json!({
            "id": su.manifest.id, "path": s.path.display().to_string(),
            "dataset": su.manifest.dataset.id, "queries": su.queries.len(),
        }));
    }

    // ---- hard capability gating (DESIGN.md §9): record, never silent ----
    let mut parent_rows = Writer::create(&out_dir.join("partials").join("_parent.jsonl"))?;
    let mut available_candidates = Vec::new();
    let mut candidates_meta = Vec::new();
    for sel in &spec.candidates {
        let c = registry::find_candidate(&sel.name)?;
        let gate = lb_harness::cpu::check_features(c.cpu_features.as_deref());
        candidates_meta.push(serde_json::json!({
            "name": c.name, "version": c.version,
            "cpu_features": c.cpu_features,
            "strategies": c.strategies.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            "view": c.has_view(), "decode": c.has_decode(),
            "available": gate.is_ok(),
            "configs": sel.configs.clone(),
        }));
        match gate {
            Ok(()) => available_candidates.push(sel.clone()),
            Err(missing) => {
                eprintln!(
                    "note: candidate {} unavailable on this host (missing CPU features: {})",
                    c.name,
                    missing.join(",")
                );
                for ds in &dataset_ids {
                    parent_rows.write(&Row::ModuleUnavailable {
                        module: c.name.clone(),
                        module_kind: "candidate".into(),
                        required_cpu_features: c.cpu_features.clone().unwrap_or_default(),
                        missing_cpu_features: missing.clone(),
                        dataset: ds.clone(),
                    })?;
                }
            }
        }
    }
    let mut scanners_meta = Vec::new();
    for sel in &spec.scanners {
        let s = registry::find_scanner(&sel.name)?;
        let gate = lb_harness::cpu::check_features(s.cpu_features.as_deref());
        scanners_meta.push(serde_json::json!({
            "name": s.name, "version": s.version,
            "cpu_features": s.cpu_features, "available": gate.is_ok(),
        }));
        if let Err(missing) = gate {
            eprintln!(
                "note: scanner {} unavailable on this host (missing CPU features: {})",
                s.name,
                missing.join(",")
            );
            for ds in &dataset_ids {
                parent_rows.write(&Row::ModuleUnavailable {
                    module: s.name.clone(),
                    module_kind: "scanner".into(),
                    required_cpu_features: s.cpu_features.clone().unwrap_or_default(),
                    missing_cpu_features: missing.clone(),
                    dataset: ds.clone(),
                })?;
            }
        }
    }
    parent_rows.finish()?;

    // ---- one child per (candidate, config, dataset), sequential ----
    let exe = std::env::current_exe()?;
    let mut any_gate_failure = false;
    let mut any_error = false;
    let mut partials = vec![out_dir.join("partials").join("_parent.jsonl")];
    'jobs: for sel in &available_candidates {
        for config_idx in 0..sel.configs.len() {
            for dataset_idx in 0..spec.datasets.len() {
                eprintln!(
                    "running: candidate={} config#{config_idx} dataset={}",
                    sel.name, dataset_ids[dataset_idx]
                );
                let mut cmd = std::process::Command::new(&exe);
                cmd.arg("run")
                    .arg(&loaded.path)
                    .arg("--out")
                    .arg(out_dir)
                    .arg("--worker-candidate")
                    .arg(&sel.name)
                    .arg("--worker-config")
                    .arg(config_idx.to_string())
                    .arg("--worker-dataset")
                    .arg(dataset_idx.to_string());
                if fail_fast {
                    cmd.arg("--fail-fast");
                }
                let status = cmd.status()?;
                partials.push(
                    out_dir
                        .join("partials")
                        .join(partial_name(&sel.name, config_idx, dataset_idx)),
                );
                match status.code() {
                    Some(0) => {}
                    Some(c) if c == EXIT_GATE_FAILURE as i32 => {
                        any_gate_failure = true;
                        if fail_fast {
                            break 'jobs;
                        }
                    }
                    other => {
                        eprintln!(
                            "error: worker for candidate {} exited with {:?} (crash = one failed \
                             matrix cell, run continues)",
                            sel.name, other
                        );
                        any_error = true;
                    }
                }
            }
        }
    }

    // ---- aggregate partials into results.jsonl + manifest.json ----
    let results_path = out_dir.join("results.jsonl");
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&results_path)?);
        for p in &partials {
            if p.exists() {
                std::io::copy(&mut std::fs::File::open(p)?, &mut out)?;
            }
        }
    }
    let pinning_effective = lb_harness::cpu::pin_to_core(spec.measure.pin_core);
    let (git_commit, git_dirty) = results::git_state();
    let manifest = results::RunManifest {
        spec_path: loaded.path.display().to_string(),
        spec_hash: loaded.hash.clone(),
        spec: spec.clone(),
        started_at,
        finished_at: humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string(),
        env,
        pinned_core: spec.measure.pin_core,
        pinning_effective,
        datasets: datasets_meta,
        suites: suites_meta,
        candidates: candidates_meta,
        scanners: scanners_meta,
        git_commit,
        git_dirty,
    };
    std::fs::write(
        out_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    println!("results: {}", results_path.display());

    if any_gate_failure {
        eprintln!("RUN FAILED: at least one correctness gate fired (exit {EXIT_GATE_FAILURE})");
        return Ok(ExitCode::from(EXIT_GATE_FAILURE));
    }
    if any_error {
        return Ok(ExitCode::from(EXIT_ERROR));
    }
    Ok(ExitCode::SUCCESS)
}
