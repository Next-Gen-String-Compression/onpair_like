//! `bench gen --method like` integration tests (TODO_like_workload.md §7,
//! T-GEN2-*): determinism, bucket accounting, truth protection, and the
//! generator's own counts surviving an independent bless.
//!
//! Runs over the checked-in mini fixture (200 rows) with the mining pool set
//! to every row, so the suffix-array index and the exact probes both stay
//! well under a second; the point is the contract, not the coverage.

use std::path::{Path, PathBuf};

use lb_harness::dataset::{self, PreparedDataset};
use lb_harness::gen::{generate_like, write_like_suite, LikeRequest, LIKE_CLASSES};
use lb_harness::{like, suite};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn ingest_fixture(dir: &Path) -> PreparedDataset {
    let ds_dir = dir.join("dataset");
    dataset::ingest(&dataset::IngestRequest {
        source: repo_root().join("datasets/fixtures/mini.csv"),
        format: "csv".into(),
        column: "data".into(),
        id: "mini".into(),
        out_dir: ds_dir.clone(),
    })
    .expect("ingest fixture");
    PreparedDataset::load(&ds_dir, true).expect("load fixture")
}

fn quick(seed: u64) -> LikeRequest {
    let mut r = LikeRequest::new(seed);
    r.per_cell = 2;
    r.mining_modulus = 1; // 200 rows: mine from all of them
    r.probe_budget_per_class = 60;
    r.pool_per_cell = 6;
    r.max_literal_len = 32;
    r
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn same_seed_regenerates_byte_identical_suites() {
    let tmp = tempfile::tempdir().unwrap();
    let ds = ingest_fixture(tmp.path());
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    for out in [&a, &b] {
        let g = generate_like(&ds, &quick(42)).unwrap();
        write_like_suite(&g, &ds, out, "g2", false).unwrap();
    }
    for f in [suite::QUERIES_FILE, "gen-report.json"] {
        assert_eq!(
            std::fs::read(a.join(f)).unwrap(),
            std::fs::read(b.join(f)).unwrap(),
            "{f} differs between two runs with the same seed"
        );
    }
    // and a different seed is a different suite
    let c = tmp.path().join("c");
    let g = generate_like(&ds, &quick(43)).unwrap();
    write_like_suite(&g, &ds, &c, "g2", false).unwrap();
    assert_ne!(
        std::fs::read(a.join(suite::QUERIES_FILE)).unwrap(),
        std::fs::read(c.join(suite::QUERIES_FILE)).unwrap()
    );
}

#[test]
fn every_pattern_is_valid_lowered_correctly_and_counted_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let ds = ingest_fixture(tmp.path());
    let g = generate_like(&ds, &quick(7)).unwrap();
    assert!(g.queries.len() > 20, "expected a real corpus, got {}", g.queries.len());

    let mut classes_seen = std::collections::BTreeSet::new();
    for q in &g.queries {
        classes_seen.insert(q.class.clone());
        // well-formed, and stored under exactly the op the lowering rules say
        assert!(lb_harness::oracle::validate_like_pattern(&q.pattern).is_ok());
        let expect = like::lower(&q.pattern)
            .map(|(op, _)| op)
            .unwrap_or(lb_abi::LB_LIKE);
        assert_eq!(q.op, expect, "pattern {:?}", String::from_utf8_lossy(&q.pattern));
        // the generator's count IS the oracle's count over the full column
        let refs: Vec<&[u8]> = q.needles.iter().map(|n| n.as_slice()).collect();
        let bm = lb_harness::oracle::eval(q.op, &refs, ds.num_rows(), ds.rows());
        assert_eq!(bm.count(), q.match_count);
        // the buckets are the measured ones
        assert_eq!(q.selectivity_bucket, like::selectivity_bucket(q.match_count, q.selectivity));
        let f = like::facts(&q.pattern);
        assert_eq!(q.length_bucket, like::length_bucket(f.literal_len_total));
        // a hole never lands inside a multi-byte sequence: every '_' replaced
        // an ASCII byte, so the pattern is still valid UTF-8 whenever its
        // sources were.
        if q.source_literals.iter().all(|l| std::str::from_utf8(l).is_ok()) {
            assert!(std::str::from_utf8(&q.pattern).is_ok());
        }
        // underscore classes really carry underscores, and only they do
        let has_hole = f.underscore_count > 0;
        let wants_hole = matches!(q.class.as_str(), "underscore_1" | "underscore_2plus" | "mixed");
        assert_eq!(has_hole, wants_hole, "class {} pattern {:?}", q.class, String::from_utf8_lossy(&q.pattern));
    }
    // the fixture is tiny, but the literal-op classes and the wildcard
    // classes must all be reachable on it
    for c in ["contains", "prefix", "suffix", "underscore_1"] {
        assert!(classes_seen.contains(c), "class {c} never generated; saw {classes_seen:?}");
    }
    assert!(LIKE_CLASSES.iter().all(|c| g.cells.iter().any(|cell| cell.class == *c)));
    // every cell is accounted for, filled or not, with a reason when not
    assert_eq!(g.cells.len(), LIKE_CLASSES.len() * 6 * 8);
    for cell in &g.cells {
        match cell.status.as_str() {
            "filled" => assert_eq!(cell.filled, cell.requested),
            "partial" | "empty" => assert!(cell.reason.is_some()),
            other => panic!("unknown cell status {other}"),
        }
    }
}

#[test]
fn bless_verifies_the_generator_and_regeneration_refuses_to_clobber() {
    let tmp = tempfile::tempdir().unwrap();
    let ds = ingest_fixture(tmp.path());
    let out = tmp.path().join("s");
    let g = generate_like(&ds, &quick(42)).unwrap();
    write_like_suite(&g, &ds, &out, "g2", false).unwrap();

    // The independent oracle agrees with every generator_count.
    suite::bless(&out, &ds, false).unwrap();
    for q in read_lines(&out.join(suite::QUERIES_FILE)) {
        assert_eq!(q["truth"]["count"], q["meta"]["gen"]["generator_count"]);
        // and bless stamped the LIKE facts for every record, lowered or not
        assert!(q["derived"]["pattern"].is_string());
        assert_eq!(q["derived"]["pattern"], q["meta"]["like"]["pattern"]);
        assert!(q["derived"]["selectivity_bucket"].is_string());
    }
    // A blessed suite is never silently regenerated.
    assert!(write_like_suite(&g, &ds, &out, "g2", false).is_err());
    write_like_suite(&g, &ds, &out, "g2", true).unwrap();
}
