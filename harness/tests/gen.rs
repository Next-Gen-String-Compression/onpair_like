//! Generator integration tests (DESIGN.md §14 test plan): determinism,
//! band acceptance surviving bless, coverage accounting, truth protection,
//! and a gated end-to-end run over a generated suite.

use std::path::{Path, PathBuf};

use lb_harness::dataset::{self, PreparedDataset};
use lb_harness::gen::{self, Band, BandKind, GenParams, Profile};
use lb_harness::results::Writer;
use lb_harness::runner;
use lb_harness::spec::LoadedSpec;
use lb_harness::suite::{self, Suite};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn ingest_fixture(dir: &Path) -> PathBuf {
    let ds_dir = dir.join("dataset");
    dataset::ingest(&dataset::IngestRequest {
        source: repo_root().join("datasets/fixtures/mini.csv"),
        format: "csv".into(),
        column: "data".into(),
        id: "mini".into(),
        out_dir: ds_dir.clone(),
    })
    .expect("ingest fixture");
    ds_dir
}

fn quick_params(seed: u64) -> GenParams {
    GenParams {
        seed,
        profile: Profile::Quick,
        ops: None,
        suite_id: "genfix".into(),
    }
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Reconstruct a band from its report/meta label (the test-side inverse of
/// the generator's fixed label set).
fn band_from_label(label: &str) -> Band {
    let (target, kind) = match label {
        "0" => (0.0, BandKind::NoMatch),
        "1e-5" => (1e-5, BandKind::Decade),
        "1e-4" => (1e-4, BandKind::Decade),
        "1e-3" => (1e-3, BandKind::Decade),
        "1e-2" => (1e-2, BandKind::Decade),
        "1e-1" => (1e-1, BandKind::Decade),
        "0.3" => (0.3, BandKind::Dense),
        "0.5" => (0.5, BandKind::Dense),
        "0.8" => (0.8, BandKind::Dense),
        other => panic!("unknown band label {other:?}"),
    };
    Band { label: "", target, kind }
}

#[test]
fn same_seed_regenerates_byte_identical_suites() {
    let tmp = tempfile::tempdir().unwrap();
    let ds_dir = ingest_fixture(tmp.path());
    let ds = PreparedDataset::load(&ds_dir, true).unwrap();

    let out_a = tmp.path().join("a");
    let out_b = tmp.path().join("b");
    gen::generate(&ds, &out_a, &quick_params(7), false).unwrap();
    gen::generate(&ds, &out_b, &quick_params(7), false).unwrap();
    for f in ["queries.jsonl", "suite.json", "gen-report.json"] {
        assert_eq!(
            std::fs::read(out_a.join(f)).unwrap(),
            std::fs::read(out_b.join(f)).unwrap(),
            "{f}: same (dataset, seed, profile) must be byte-identical"
        );
    }

    let out_c = tmp.path().join("c");
    gen::generate(&ds, &out_c, &quick_params(8), false).unwrap();
    assert_ne!(
        std::fs::read(out_a.join("queries.jsonl")).unwrap(),
        std::fs::read(out_c.join("queries.jsonl")).unwrap(),
        "a different seed must produce a different suite"
    );
}

#[test]
fn coverage_report_accounts_for_every_grid_point() {
    let tmp = tempfile::tempdir().unwrap();
    let ds_dir = ingest_fixture(tmp.path());
    let ds = PreparedDataset::load(&ds_dir, true).unwrap();

    let out = tmp.path().join("suite");
    let outcome = gen::generate(&ds, &out, &quick_params(7), false).unwrap();

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("gen-report.json")).unwrap())
            .unwrap();
    let points = report["points"].as_array().unwrap();

    // Every grid point appears exactly once, in grid order.
    let expected_grid = gen::grid(Profile::Quick, None);
    assert_eq!(points.len(), expected_grid.len());
    assert_eq!(outcome.grid_points, expected_grid.len());

    // filled/partial/empty partition the grid; queries.jsonl length is the
    // sum of fills; no point exceeds its request or its budget silently.
    let queries = read_lines(&out.join("queries.jsonl"));
    let mut filled_sum = 0;
    let (mut full, mut partial, mut empty) = (0, 0, 0);
    for p in points {
        let filled = p["filled"].as_u64().unwrap() as usize;
        let requested = p["requested"].as_u64().unwrap() as usize;
        assert!(filled <= requested);
        assert_eq!(
            p["achieved_selectivities"].as_array().unwrap().len(),
            filled,
            "achieved list length matches fill"
        );
        match filled {
            f if f == requested => full += 1,
            0 => {
                empty += 1;
                assert!(p["reason"].is_string(), "empty point must carry a reason");
            }
            _ => {
                partial += 1;
                assert!(p["reason"].is_string(), "partial point must carry a reason");
            }
        }
        filled_sum += filled;
    }
    assert_eq!(filled_sum, queries.len());
    assert_eq!(full, outcome.filled_points);
    assert_eq!(partial, outcome.partial_points);
    assert_eq!(empty, outcome.empty_points);

    // The fixture is tiny but the cheap bands must be reachable.
    assert!(queries.len() >= 5, "expected some fills on the fixture, got {}", queries.len());
}

#[test]
fn blessed_truth_lands_inside_the_targeted_band() {
    let tmp = tempfile::tempdir().unwrap();
    let ds_dir = ingest_fixture(tmp.path());
    let ds = PreparedDataset::load(&ds_dir, true).unwrap();

    let out = tmp.path().join("suite");
    gen::generate(&ds, &out, &quick_params(7), false).unwrap();

    // The generator writes no truth; bless is the single truth authority.
    for q in read_lines(&out.join("queries.jsonl")) {
        assert!(q.get("truth").is_none(), "{}: generator must not write truth", q["id"]);
    }
    let outcome = suite::bless(&out, &ds, false).unwrap();
    assert_eq!(outcome.verified, 0);

    // After bless: measured selectivity (derived, the value analysis uses)
    // must satisfy the band each query targeted — the generator's exact
    // probe and the oracle must agree.
    let suite = Suite::load_for_run(&out, &ds).unwrap();
    assert!(!suite.queries.is_empty());
    for q in &suite.queries {
        let derived = q.record.derived.as_ref().unwrap();
        let sel = derived["selectivity"].as_f64().unwrap();
        let meta = q.record.meta.as_ref().unwrap();
        let band = band_from_label(meta["gen"]["band"].as_str().unwrap());
        assert!(
            band.accepts(sel),
            "{}: measured selectivity {sel} outside band {}",
            q.record.id,
            meta["gen"]["band"]
        );
        if band.kind == BandKind::NoMatch {
            assert_eq!(q.record.truth.as_ref().unwrap().count, 0);
        }
    }
}

#[test]
fn regeneration_refuses_to_clobber_unless_forced() {
    let tmp = tempfile::tempdir().unwrap();
    let ds_dir = ingest_fixture(tmp.path());
    let ds = PreparedDataset::load(&ds_dir, true).unwrap();

    let out = tmp.path().join("suite");
    gen::generate(&ds, &out, &quick_params(7), false).unwrap();
    suite::bless(&out, &ds, false).unwrap();

    let err = gen::generate(&ds, &out, &quick_params(7), false).unwrap_err();
    assert!(err.to_string().contains("--force"), "unexpected error: {err}");

    gen::generate(&ds, &out, &quick_params(7), true).unwrap();
    for q in read_lines(&out.join("queries.jsonl")) {
        assert!(q.get("truth").is_none(), "force regeneration must drop stale truth");
    }
}

/// The pipeline proof: a generated suite blesses and then runs fully gated
/// (uncompressed × memmem over two chunkings) with zero gate failures.
#[test]
fn generated_suite_runs_gated_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let ds_dir = ingest_fixture(tmp.path());
    let ds = PreparedDataset::load(&ds_dir, true).unwrap();

    let out = tmp.path().join("suite");
    gen::generate(&ds, &out, &quick_params(7), false).unwrap();
    suite::bless(&out, &ds, false).unwrap();
    let n_queries = read_lines(&out.join("queries.jsonl")).len();

    let spec = format!(
        r#"
[[datasets]]
path = "{}"

[[suites]]
path = "{}"

[[candidates]]
name = "uncompressed"

[[scanners]]
name = "memmem"

[measure]
warmup = 1
min_iters = 2
min_millis = 0
chunk_rows = [0, 64]
"#,
        ds_dir.display(),
        out.display(),
    );
    let spec_path = tmp.path().join("spec.toml");
    std::fs::write(&spec_path, spec).unwrap();
    let loaded = LoadedSpec::load(&spec_path).unwrap();

    let rows_path = tmp.path().join("rows.jsonl");
    let mut writer = Writer::create(&rows_path).unwrap();
    let summary = runner::run_worker(&loaded, "uncompressed", 0, 0, &mut writer, false).unwrap();
    writer.finish().unwrap();
    assert_eq!(summary.gate_failures, 0);
    assert_eq!(summary.errors, 0);

    let rows = read_lines(&rows_path);
    let queries: Vec<_> = rows.iter().filter(|r| r["kind"] == "query").collect();
    assert_eq!(queries.len(), n_queries * 2, "every query at both chunkings");
    assert!(queries.iter().all(|r| r["status"] == "ok"));
}
