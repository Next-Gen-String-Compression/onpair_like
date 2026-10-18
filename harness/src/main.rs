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
        /// Same dataset, generator configuration and seed produce identical needles.
        #[arg(long)]
        seed: u64,
        /// sampled supports all operations; suffix-array generates balanced CONTAINS needles.
        #[arg(long, default_value = "sampled", value_parser = ["sampled", "suffix-array"])]
        method: String,
        /// Unique needles per length/selectivity cell (suffix-array only).
        #[arg(long, default_value_t = 20)]
        per_cell: usize,
        /// Maximum needle length in bytes (suffix-array only).
        #[arg(long, default_value_t = 256)]
        max_needle_len: usize,
        /// Mutation attempts per zero-match cell (suffix-array only).
        #[arg(long, default_value_t = 4000)]
        negative_attempts: usize,
        /// Reuse dataset-bound SA/LCP arrays across seeds and quotas.
        #[arg(long)]
        index_cache: Option<PathBuf>,
        /// SA workspace admission budget, excluding the loaded dataset.
        #[arg(long, default_value_t = 4096)]
        index_memory_mib: u64,
        /// full | quick (quick: reduced grid for iteration).
        #[arg(long, default_value = "full")]
        profile: String,
        /// Comma-separated op filter (sampled: all five; suffix-array: contains).
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
        /// Override the spec's measure.min_iters; the manifest records the value used.
        #[arg(long)]
        min_iters: Option<u32>,
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
        Cmd::Gen { dataset: ds_dir, out, seed, method, per_cell, max_needle_len,
            negative_attempts, index_cache, index_memory_mib, profile, ops, id, force } => {
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
            if method == "suffix-array" {
                use lb_harness::gen::{BalancedRequest, IndexLimits, LengthBucket, SubstringIndex};
                if ops.as_ref().is_some_and(|ops| ops.as_slice() != [lb_abi::LB_CONTAINS]) {
                    return Err("suffix-array generation supports only --ops contains".into());
                }
                if profile != "full" { return Err("--profile applies to sampled generation; use --per-cell for suffix-array".into()); }
                if out.join(suite::QUERIES_FILE).exists() && !force { return Err("suite exists; pass --force to replace it".into()); }
                let limits = IndexLimits { max_needle_len,
                    memory_budget_bytes: index_memory_mib.checked_mul(1 << 20).ok_or("memory budget overflow")? };
                eprintln!("indexing {} rows, {} bytes (SA + LCP)", ds.num_rows(), ds.raw_bytes());
                let index = match index_cache {
                    Some(path) => SubstringIndex::cached(ds.payload(), ds.offsets_u64(), limits, &path)?,
                    None => SubstringIndex::new(ds.payload(), ds.offsets_u64(), limits)?,
                };
                let mut request = BalancedRequest::new(ds.num_rows(), seed);
                request.per_cell = per_cell;
                request.negative_attempts = negative_attempts;
                request.lengths.retain(|b| b.min <= max_needle_len);
                for bucket in &mut request.lengths { bucket.max = bucket.max.min(max_needle_len); }
                if max_needle_len > 256 { request.lengths.push(LengthBucket { min: 257, max: max_needle_len }); }
                eprintln!("selecting up to {per_cell} unique needles per cell");
                let generated = index.generate(&request)?;
                lb_harness::gen::write_balanced_suite(std::slice::from_ref(&generated), &ds, &out, &suite_id, force)?;
                let filled = generated.cells.iter().filter(|c| c.status == "filled").count();
                println!("generated {} unique queries; {filled}/{} cells filled; coverage: {}",
                    generated.needles.len(), generated.cells.len(), out.join("gen-report.json").display());
                println!("next: bench bless --suite {} --dataset {}", out.display(), ds_dir.display());
                return Ok(ExitCode::SUCCESS);
            }
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
            let blessed = Suite::load_for_run(&suite_dir, &ds)?;
            if blessed.manifest.provenance.as_ref().is_some_and(|p| p["generator"] == lb_harness::gen::SUBSTRING_GENERATOR_VERSION) {
                lb_harness::gen::verify_balanced_suite(&suite_dir, &ds)?;
                println!("verified SA row counts, bucket assignments, uniqueness and mutation witnesses");
            }
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
        Cmd::Run { spec, out, fail_fast, min_iters, worker_candidate, worker_config, worker_dataset } => {
            let mut loaded = LoadedSpec::load(&spec)?;
            if let Some(n) = min_iters {
                loaded.spec.measure.min_iters = n;
            }
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

/// Load this run into `results/bench.duckdb` (DESIGN.md §9): the JSONL stays
/// the source of truth, the DB is the derived index every analysis reads. The
/// measurement binary owns no DB code — it calls the loader, which is
/// idempotent, so a repeat is a no-op.
fn ingest_into_db(out_dir: &std::path::Path) -> Result<(), Error> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("harness crate has no parent directory")?
        .to_path_buf();
    // The DB indexes this repo's results/ tree. A run written elsewhere (a
    // scratch directory, an integration test's tempdir) is out of its scope —
    // say so rather than inserting rows nobody can locate again.
    // A fresh clone has no results/ until its first run writes one there.
    let results_root = root.join("results");
    let inside = std::fs::canonicalize(&results_root)
        .is_ok_and(|r| std::fs::canonicalize(out_dir).is_ok_and(|o| o.starts_with(r)));
    if !inside {
        println!(
            "not ingested: {} is outside {} (the DB indexes that tree only)",
            out_dir.display(),
            results_root.display()
        );
        return Ok(());
    }
    let python = root.join(".venv/bin/python");
    let loader = root.join("tools/results-db/load.py");
    if !python.exists() {
        // The run measured and wrote everything; the DB is derived from it, so a
        // missing interpreter is a note, not a failed run.
        eprintln!(
            "note: skipping ingest, {} not found — create it with `python3 -m venv \
             .venv && .venv/bin/pip install -r datasets/requirements.txt`, then \
             re-ingest with `.venv/bin/python tools/results-db/load.py {}`",
            python.display(),
            out_dir.display()
        );
        return Ok(());
    }
    let status = std::process::Command::new(&python)
        .arg(&loader)
        .arg(out_dir)
        .current_dir(&root)
        .status()?;
    if !status.success() {
        return Err(format!("ingest: {} exited with {status}", loader.display()).into());
    }
    Ok(())
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
                cmd.arg("--min-iters").arg(spec.measure.min_iters.to_string());
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
    let payload_bytes = manifest
        .datasets
        .iter()
        .filter_map(|ds| {
            let id = ds.get("id")?.as_str()?.to_string();
            Some((id, ds.get("payload_bytes")?.as_u64()?))
        })
        .collect();
    if let Err(e) = print_run_summary(&results_path, &payload_bytes) {
        eprintln!("warn: could not summarize {}: {e}", results_path.display());
    }

    if let Err(e) = ingest_into_db(out_dir) {
        eprintln!("error: {e}");
        eprintln!("       results.jsonl and manifest.json are intact; re-ingest with");
        eprintln!("       .venv/bin/python tools/results-db/load.py {}", out_dir.display());
        any_error = true;
    }

    if any_gate_failure {
        eprintln!("RUN FAILED: at least one correctness gate fired (exit {EXIT_GATE_FAILURE})");
        return Ok(ExitCode::from(EXIT_GATE_FAILURE));
    }
    if any_error {
        return Ok(ExitCode::from(EXIT_ERROR));
    }
    Ok(ExitCode::SUCCESS)
}

/// One measured cell, as the run summary prints it.
struct CellRow {
    candidate: String,
    config: String,
    strategy: String,
    gbps_raw: f64,
    median_ns: u64,
    /// Self-reported phase splits, absent for candidates that do not report them.
    prefilter_ns: Option<u64>,
    verify_ns: Option<u64>,
    setup_ns: Option<u64>,
    /// Code-stream bytes of the build this cell ran over, and payload bytes per
    /// one of them: `gbps_raw` is over the payload, so this is the factor
    /// between what the engine touched and what it is credited with.
    code_bytes: Option<u64>,
    payload_per_code_byte: Option<f64>,
}

/// Print what the run measured, fastest first, straight from the
/// `results.jsonl` just written.
///
/// One table per (dataset, chunk_rows, selection, query), because those are
/// the things a row is not comparable across: a table then holds one row per
/// system, which is the comparison worth reading.
///
/// The point of the last two columns is that `logical GB/s` is over the
/// *payload*, so two systems are only comparable through how much payload each
/// of their codes carries — a narrower dictionary can win on time per code and
/// still lose the cell because it needs more codes to say the same bytes.
fn print_run_summary(
    results_path: &std::path::Path,
    payload_bytes: &std::collections::BTreeMap<String, u64>,
) -> std::io::Result<()> {
    use std::collections::BTreeMap;
    use std::io::BufRead;

    let phase = |row: &serde_json::Value, name: &str| {
        row.pointer(&format!("/prefilter/{name}/ns"))
            .and_then(serde_json::Value::as_u64)
    };
    let text = |row: &serde_json::Value, name: &str| {
        row[name].as_str().unwrap_or("?").to_string()
    };
    let group_of = |row: &serde_json::Value| {
        (
            text(row, "dataset"),
            row["chunk_rows"].as_u64().unwrap_or(0),
            row["selection"].as_str().unwrap_or("all").to_string(),
            text(row, "query_id"),
        )
    };

    let mut builds: BTreeMap<String, Option<u64>> = BTreeMap::new();
    let mut groups: BTreeMap<(String, u64, String, String), Vec<CellRow>> = BTreeMap::new();

    let file = std::io::BufReader::new(std::fs::File::open(results_path)?);
    let rows: Vec<serde_json::Value> = file
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect();

    // Builds first: a query row names its build only by the same key fields.
    let build_key = |row: &serde_json::Value| {
        format!(
            "{}\u{1}{}\u{1}{}\u{1}{}",
            text(row, "candidate"),
            text(row, "config"),
            text(row, "dataset"),
            row["chunk_rows"].as_u64().unwrap_or(0)
        )
    };
    for row in rows.iter().filter(|row| row["kind"] == "build") {
        builds.insert(
            build_key(row),
            row["footprint_components"]["codes"].as_u64(),
        );
    }

    for row in rows
        .iter()
        .filter(|row| row["kind"] == "query" && row["status"] == "ok")
    {
        let code_bytes = builds.get(&build_key(row)).copied().flatten();
        let payload = payload_bytes.get(&text(row, "dataset")).copied();
        groups.entry(group_of(row)).or_default().push(CellRow {
            candidate: text(row, "candidate"),
            config: text(row, "config"),
            strategy: text(row, "strategy"),
            gbps_raw: row["gbps_raw"].as_f64().unwrap_or(0.0),
            median_ns: row
                .pointer("/latency/median_ns")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            prefilter_ns: phase(row, "prefilter_ns"),
            verify_ns: phase(row, "verify_ns"),
            setup_ns: phase(row, "setup_ns"),
            code_bytes,
            payload_per_code_byte: match (payload, code_bytes) {
                (Some(payload), Some(codes)) if codes != 0 => {
                    Some(payload as f64 / codes as f64)
                }
                _ => None,
            },
        });
    }

    for ((dataset, chunk_rows, selection, query), mut cells) in groups {
        cells.sort_by(|a, b| b.gbps_raw.total_cmp(&a.gbps_raw));
        let chunk = if chunk_rows == 0 {
            "whole column".to_string()
        } else {
            format!("{chunk_rows} rows/chunk")
        };
        println!("\n{query} · {dataset} · {chunk} · {selection} · {} systems", cells.len());
        println!(
            "{:<22} {:<24} {:<11} {:>12} {:>11} {:>10} {:>10} {:>9} {:>11} {:>8}",
            "candidate",
            "config",
            "strategy",
            "logical GB/s",
            "median ns",
            "prefilter",
            "verify",
            "setup",
            "code bytes",
            "payload/code"
        );
        let ns = |value: Option<u64>| {
            value.map_or_else(|| "-".to_string(), |ns| ns.to_string())
        };
        for cell in cells {
            println!(
                "{:<22} {:<24} {:<11} {:>12.2} {:>11} {:>10} {:>10} {:>9} {:>11} {:>8}",
                cell.candidate,
                cell.config,
                cell.strategy,
                cell.gbps_raw,
                cell.median_ns,
                ns(cell.prefilter_ns),
                ns(cell.verify_ns),
                ns(cell.setup_ns),
                ns(cell.code_bytes),
                cell.payload_per_code_byte
                    .map_or_else(|| "-".to_string(), |factor| format!("{factor:.3}")),
            );
        }
    }
    Ok(())
}
