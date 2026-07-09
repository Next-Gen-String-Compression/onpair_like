//! End-to-end vertical-slice tests (DESIGN.md §11 step 8): ingest → bless
//! → run → gated results, in-process and through the real CLI binary,
//! including chunk-size invariance and the gate-canary proof that the
//! correctness gate fires.

use std::path::{Path, PathBuf};

use lb_harness::dataset::{self, PreparedDataset};
use lb_harness::results::Writer;
use lb_harness::runner;
use lb_harness::spec::LoadedSpec;
use lb_harness::suite;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Ingest the fixture CSV and bless a copy of the smoke suite into `dir`.
fn prepare_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let ds_dir = dir.join("dataset");
    dataset::ingest(&dataset::IngestRequest {
        source: repo_root().join("datasets/fixtures/mini.csv"),
        format: "csv".into(),
        column: "data".into(),
        id: "mini".into(),
        out_dir: ds_dir.clone(),
    })
    .expect("ingest fixture");

    let suite_dir = dir.join("suite");
    std::fs::create_dir_all(&suite_dir).unwrap();
    for f in ["suite.json", "queries.jsonl"] {
        std::fs::copy(repo_root().join("suites/smoke").join(f), suite_dir.join(f)).unwrap();
    }
    let ds = PreparedDataset::load(&ds_dir, true).expect("load fixture");
    suite::bless(&suite_dir, &ds, false).expect("bless smoke suite");
    (ds_dir, suite_dir)
}

fn write_spec(dir: &Path, ds: &Path, suite: &Path, candidates: &[&str], chunk_rows: &str) -> PathBuf {
    let candidate_blocks: String = candidates
        .iter()
        .map(|c| format!("[[candidates]]\nname = \"{c}\"\n\n"))
        .collect();
    let spec = format!(
        r#"
[[datasets]]
path = "{}"

[[suites]]
path = "{}"

{candidate_blocks}
[[scanners]]
name = "memmem"

[[scanners]]
name = "cpp_std_find"

[measure]
warmup = 1
min_iters = 2
min_millis = 0
chunk_rows = {chunk_rows}
"#,
        ds.display(),
        suite.display(),
    );
    let path = dir.join("spec.toml");
    std::fs::write(&path, spec).unwrap();
    path
}

fn read_rows(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Every real candidate × scanner × chunk size passes the gate — including
/// the chunk-invariance requirement (identical gated results at 0/64/128).
#[test]
fn real_candidates_pass_all_gates_at_every_chunk_size() {
    let tmp = tempfile::tempdir().unwrap();
    let (ds_dir, suite_dir) = prepare_fixture(tmp.path());
    let spec_path = write_spec(
        tmp.path(),
        &ds_dir,
        &suite_dir,
        &["uncompressed", "cpp_identity"],
        "[0, 64, 128]",
    );
    let loaded = LoadedSpec::load(&spec_path).unwrap();

    for (idx, cand) in ["uncompressed", "cpp_identity"].iter().enumerate() {
        let out_path = tmp.path().join(format!("rows-{idx}.jsonl"));
        let mut writer = Writer::create(&out_path).unwrap();
        let summary = runner::run_worker(&loaded, cand, 0, 0, &mut writer, false).unwrap();
        writer.finish().unwrap();
        assert_eq!(summary.gate_failures, 0, "{cand}: gate failures");
        assert_eq!(summary.errors, 0, "{cand}: errors");

        let rows = read_rows(&out_path);
        let queries: Vec<_> = rows.iter().filter(|r| r["kind"] == "query").collect();
        // 28 queries × 2 scanners × 3 chunk sizes, one composed strategy each.
        assert_eq!(queries.len(), 28 * 2 * 3, "{cand}: cell count");
        assert!(queries.iter().all(|r| r["status"] == "ok"), "{cand}: all gated ok");
        let expected_strategy = if *cand == "uncompressed" { "direct" } else { "decode" };
        assert!(
            queries.iter().all(|r| r["strategy"] == expected_strategy),
            "{cand}: strategy"
        );
        // Compression axis: one build row per chunk size. Footprint is raw
        // plus 8·(num_chunks−1): every chunk owns its own num_rows+1 offsets,
        // and that per-chunk overhead appearing on the compression axis is
        // exactly what chunk sweeps exist to observe (DESIGN.md §6).
        let builds: Vec<_> = rows.iter().filter(|r| r["kind"] == "build").collect();
        assert_eq!(builds.len(), 3, "{cand}: build rows");
        for b in builds {
            let expected =
                b["raw_bytes"].as_u64().unwrap() + 8 * (b["num_chunks"].as_u64().unwrap() - 1);
            assert_eq!(
                b["footprint_total_bytes"].as_u64().unwrap(),
                expected,
                "{cand}: identity footprint + per-chunk offset overhead"
            );
        }
        // decode strategies must report harness-timed decode phase splits.
        if *cand == "cpp_identity" {
            assert!(
                queries
                    .iter()
                    .all(|r| r["prefilter"]["decode_ns"]["origin"] == "harness"),
                "cpp_identity: harness-timed decode split"
            );
        }
    }
}

/// OnPair, the first real compressed candidate: its "compressed" strategy
/// (token automata over the compressed stream) must pass every gate on the
/// ops it declares, its decode path must pass everywhere, and its
/// footprint must arrive as named store/dictionary components.
#[test]
fn onpair_gates_on_compressed_and_decode_strategies() {
    let tmp = tempfile::tempdir().unwrap();
    let (ds_dir, suite_dir) = prepare_fixture(tmp.path());
    let spec_path = write_spec(tmp.path(), &ds_dir, &suite_dir, &["onpair"], "[0, 64, 128]");
    let loaded = LoadedSpec::load(&spec_path).unwrap();

    let out_path = tmp.path().join("rows.jsonl");
    let mut writer = Writer::create(&out_path).unwrap();
    let summary = runner::run_worker(&loaded, "onpair", 0, 0, &mut writer, false).unwrap();
    writer.finish().unwrap();
    assert_eq!(summary.gate_failures, 0, "onpair: gate failures");
    assert_eq!(summary.errors, 0, "onpair: errors");

    let rows = read_rows(&out_path);
    let queries: Vec<_> = rows.iter().filter(|r| r["kind"] == "query").collect();
    // 28 queries × 3 chunk sizes × ("compressed" + decode × 2 scanners).
    assert_eq!(queries.len(), 28 * 3 * 3, "onpair: cell count");

    // The compressed strategy answers prefix/contains/contains_any (17 of
    // the 28 smoke queries) and honestly reports suffix/multi_contains
    // (the remaining 11) as unsupported.
    let compressed: Vec<_> =
        queries.iter().filter(|r| r["strategy"] == "compressed").collect();
    assert_eq!(compressed.len(), 28 * 3);
    let ok = compressed.iter().filter(|r| r["status"] == "ok").count();
    let unsupported =
        compressed.iter().filter(|r| r["status"] == "unsupported").count();
    assert_eq!((ok, unsupported), (17 * 3, 11 * 3), "onpair: compressed op split");
    assert!(
        compressed
            .iter()
            .filter(|r| r["status"] == "unsupported")
            .all(|r| r["op"] == "suffix" || r["op"] == "multi_contains"),
        "onpair: only suffix/multi_contains may be unsupported"
    );

    // The decode path covers every op, harness-timed at the decode joint.
    let decode: Vec<_> = queries.iter().filter(|r| r["strategy"] == "decode").collect();
    assert_eq!(decode.len(), 28 * 3 * 2);
    assert!(decode.iter().all(|r| r["status"] == "ok"), "onpair: decode all gated ok");
    assert!(
        decode.iter().all(|r| r["prefilter"]["decode_ns"]["origin"] == "harness"),
        "onpair: harness-timed decode split"
    );

    // Compression axis: named components, exact total.
    let builds: Vec<_> = rows.iter().filter(|r| r["kind"] == "build").collect();
    assert_eq!(builds.len(), 3, "onpair: build rows");
    for b in builds {
        let c = &b["footprint_components"];
        let sum: u64 = ["token_stream", "boundaries", "dict_bytes", "dict_offsets"]
            .iter()
            .map(|k| c[*k].as_u64().unwrap_or_else(|| panic!("missing component {k}")))
            .sum();
        assert_eq!(b["footprint_total_bytes"].as_u64().unwrap(), sum);
        assert!(b["build_ns"].as_u64().unwrap() > 0);
    }
}

/// The gate canary: its "ok" strategy passes everywhere, its "wrong"
/// strategy (bit flip on row 0) fails every cell loudly. A gate that has
/// never fired is not known to work.
#[test]
fn gate_canary_fires_the_gate() {
    let tmp = tempfile::tempdir().unwrap();
    let (ds_dir, suite_dir) = prepare_fixture(tmp.path());
    let spec_path = write_spec(tmp.path(), &ds_dir, &suite_dir, &["gate_canary"], "[0, 64]");
    let loaded = LoadedSpec::load(&spec_path).unwrap();

    let out_path = tmp.path().join("rows.jsonl");
    let mut writer = Writer::create(&out_path).unwrap();
    let summary = runner::run_worker(&loaded, "gate_canary", 0, 0, &mut writer, false).unwrap();
    writer.finish().unwrap();

    let rows = read_rows(&out_path);
    let by_strategy = |name: &str| -> Vec<&serde_json::Value> {
        rows.iter()
            .filter(|r| r["kind"] == "query" && r["strategy"] == name)
            .collect()
    };
    let ok_cells = by_strategy("ok");
    let wrong_cells = by_strategy("wrong");
    assert_eq!(ok_cells.len(), 28 * 2);
    assert_eq!(wrong_cells.len(), 28 * 2);
    assert!(ok_cells.iter().all(|r| r["status"] == "ok"), "ok strategy must pass");
    assert!(
        wrong_cells.iter().all(|r| r["status"] == "gate_failed"),
        "wrong strategy must fail every gate"
    );
    assert_eq!(summary.gate_failures, 28 * 2);
    // Failure reports name a divergent row and withhold all numbers.
    for r in &wrong_cells {
        assert!(r["gate"]["first_divergent_row"].is_u64());
        assert!(r["latency"].is_null());
    }
}

/// Through the real binary: parent process, per-candidate workers,
/// aggregation, manifest, and exit codes (0 clean, 3 on gate failure).
#[test]
fn cli_end_to_end_exit_codes_and_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let (ds_dir, suite_dir) = prepare_fixture(tmp.path());
    let bench = env!("CARGO_BIN_EXE_bench");

    // Clean run: all real modules, exit 0, results + manifest present.
    let spec_ok = write_spec(
        tmp.path(),
        &ds_dir,
        &suite_dir,
        &["uncompressed", "cpp_identity"],
        "[0, 64]",
    );
    let out_ok = tmp.path().join("results-ok");
    let status = std::process::Command::new(bench)
        .args(["run".as_ref(), spec_ok.as_os_str(), "--out".as_ref(), out_ok.as_os_str()])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let rows = read_rows(&out_ok.join("results.jsonl"));
    assert_eq!(
        rows.iter().filter(|r| r["kind"] == "query").count(),
        28 * 2 * 2 * 2 // queries × scanners × chunk sizes × candidates
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_ok.join("manifest.json")).unwrap())
            .unwrap();
    assert!(manifest["spec_hash"].as_str().unwrap().starts_with("xxh3:"));
    assert!(manifest["env"]["cpu_model"].is_string());

    // Canary run: exit code 3 and gate_failed rows in the aggregate.
    let canary_dir = tmp.path().join("canary");
    std::fs::create_dir_all(&canary_dir).unwrap();
    let spec_bad = write_spec(&canary_dir, &ds_dir, &suite_dir, &["gate_canary"], "[0]");
    let out_bad = tmp.path().join("results-bad");
    let status = std::process::Command::new(bench)
        .args(["run".as_ref(), spec_bad.as_os_str(), "--out".as_ref(), out_bad.as_os_str()])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3), "gate failure must fail the whole run");
    let rows = read_rows(&out_bad.join("results.jsonl"));
    assert!(rows
        .iter()
        .any(|r| r["kind"] == "query" && r["status"] == "gate_failed"));
}

/// §16 scanners & codecs: every new scanner passes the correctness gate on
/// the ops it supports (and is Unsupported elsewhere, never Error), across
/// both the uncompressed `direct` path and the lz4/zstd `decode` path. This
/// is the oracle-gated proof for the whole batch; the crate unit tests only
/// check the algorithms in isolation.
#[test]
fn section16_scanners_and_codecs_pass_all_gates() {
    let tmp = tempfile::tempdir().unwrap();
    let (ds_dir, suite_dir) = prepare_fixture(tmp.path());

    let scanners = [
        "memmem", "memmem-hay", "cpp_std_find", "libc-memmem", "stringzilla", "bndm", "kmp",
        "bmh", "aho_corasick", "teddy", "pf-none", "pf-first-byte", "pf-rare-byte",
        "pf-first-last", "pf-rare-pair",
    ];
    let candidates = ["uncompressed", "lz4", "zstd"];
    let scanner_blocks: String = scanners
        .iter()
        .map(|s| format!("[[scanners]]\nname = \"{s}\"\n\n"))
        .collect();
    let candidate_blocks: String = candidates
        .iter()
        .map(|c| format!("[[candidates]]\nname = \"{c}\"\n\n"))
        .collect();
    let spec = format!(
        "[[datasets]]\npath = \"{}\"\n\n[[suites]]\npath = \"{}\"\n\n{candidate_blocks}{scanner_blocks}\
         [measure]\nwarmup = 0\nmin_iters = 1\nmin_millis = 0\nchunk_rows = [0, 64]\n",
        ds_dir.display(),
        suite_dir.display(),
    );
    let spec_path = tmp.path().join("spec16.toml");
    std::fs::write(&spec_path, spec).unwrap();
    let loaded = LoadedSpec::load(&spec_path).unwrap();

    for (idx, cand) in candidates.iter().enumerate() {
        let out_path = tmp.path().join(format!("s16-{idx}.jsonl"));
        let mut writer = Writer::create(&out_path).unwrap();
        let summary = runner::run_worker(&loaded, cand, 0, 0, &mut writer, false).unwrap();
        writer.finish().unwrap();
        assert_eq!(summary.gate_failures, 0, "{cand}: gate failures");
        assert_eq!(summary.errors, 0, "{cand}: errors");

        let rows = read_rows(&out_path);
        let queries: Vec<_> = rows.iter().filter(|r| r["kind"] == "query").collect();
        // 28 queries × 15 scanners × 2 chunk sizes, one strategy each
        // (uncompressed → direct; lz4/zstd → decode).
        assert_eq!(queries.len(), 28 * 15 * 2, "{cand}: cell count");
        assert!(
            queries
                .iter()
                .all(|r| r["status"] == "ok" || r["status"] == "unsupported"),
            "{cand}: every cell must be ok or unsupported"
        );
        // Not all unsupported: contains is answered by every scanner.
        let ok = queries.iter().filter(|r| r["status"] == "ok").count();
        assert!(ok > 28, "{cand}: expected many ok cells, got {ok}");
        let expected_strategy = if *cand == "uncompressed" { "direct" } else { "decode" };
        assert!(
            queries.iter().all(|r| r["strategy"] == expected_strategy),
            "{cand}: strategy"
        );
    }
}

/// Truth is bound to the dataset checksum: a suite blessed against one
/// dataset refuses to run against another.
#[test]
fn stale_truth_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (_ds_dir, suite_dir) = prepare_fixture(tmp.path());

    // A different dataset (one row appended) with the same id.
    let altered_csv = tmp.path().join("altered.csv");
    let mut text = std::fs::read_to_string(repo_root().join("datasets/fixtures/mini.csv")).unwrap();
    text.push_str("one-extra-row\n");
    std::fs::write(&altered_csv, text).unwrap();
    let altered_dir = tmp.path().join("altered-dataset");
    dataset::ingest(&dataset::IngestRequest {
        source: altered_csv,
        format: "csv".into(),
        column: "data".into(),
        id: "mini".into(),
        out_dir: altered_dir.clone(),
    })
    .unwrap();

    let altered = PreparedDataset::load(&altered_dir, true).unwrap();
    let err = match suite::Suite::load_for_run(&suite_dir, &altered) {
        Ok(_) => panic!("stale truth was accepted"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("re-run `bench bless`"), "got: {err}");
}
