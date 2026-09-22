//! LIKE semantics, end to end over the adversarial wildcard fixture
//! (TODO_like_workload.md §7, T-FIX).
//!
//! The unit tests in `oracle` and `like` pin the semantics against a
//! differential twin and against each other. This file pins them against
//! *data*: the 39 hand-chosen rows of `datasets/fixtures/wildcards.csv`, each
//! of which exists to separate two readings of a pattern. The counts below
//! are worked out by hand from those rows, so a change in the matcher and a
//! change in the fixture cannot agree with each other and both be wrong.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lb_harness::dataset::{self, PreparedDataset};
use lb_harness::suite;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Ingest the checked-in wildcard fixture into a temp dir and bless a copy of
/// the suite against it. Returns query id -> blessed match count.
fn blessed_counts(dir: &Path) -> HashMap<String, u64> {
    let ds_dir = dir.join("dataset");
    dataset::ingest(&dataset::IngestRequest {
        source: repo_root().join("datasets/fixtures/wildcards.csv"),
        format: "csv".into(),
        column: "data".into(),
        id: "wildcards".into(),
        out_dir: ds_dir.clone(),
    })
    .expect("ingest wildcard fixture");
    let ds = PreparedDataset::load(&ds_dir, true).expect("load wildcard fixture");

    // Bless a scratch copy so the test never rewrites the tracked suite.
    let suite_dir = dir.join("suite");
    std::fs::create_dir_all(&suite_dir).unwrap();
    for f in [suite::SUITE_FILE, suite::QUERIES_FILE] {
        std::fs::copy(repo_root().join("suites/wildcards").join(f), suite_dir.join(f)).unwrap();
    }
    suite::bless(&suite_dir, &ds, false).expect("bless wildcard fixture");

    let s = suite::Suite::load_for_run(&suite_dir, &ds).expect("load blessed suite");
    s.queries
        .iter()
        .map(|q| (q.record.id.clone(), q.record.truth.as_ref().unwrap().count))
        .collect()
}

#[test]
fn wildcard_fixture_truth_is_exactly_what_the_rows_say() {
    let tmp = tempfile::tempdir().unwrap();
    let c = blessed_counts(tmp.path());
    let n = |id: &str| -> u64 {
        *c.get(id).unwrap_or_else(|| panic!("no such query: {id}"))
    };

    // '_' is exactly one byte. abxc / ab-c / ab_c / xxabxczz / xxab_czz match;
    // abc (zero bytes) and abXYc (two) do not.
    assert_eq!(n("wildcards.like.underscore.one"), 5);
    // The transformations a careless matcher might substitute, each landing
    // on a *different* set of rows — which is the whole point.
    assert_eq!(n("wildcards.like.contains.abc"), 4); // %abc%
    assert_eq!(n("wildcards.like.underscore.escaped"), 2); // %ab\_c%, literal
    assert_eq!(n("wildcards.like.underscore.two"), 2); // %ab__c%

    // Byte semantics: 'ó' is two UTF-8 bytes, so one '_' cannot cover it.
    assert_eq!(n("wildcards.like.multibyte.one"), 3); // Love, L_ve, Lxve
    assert_eq!(n("wildcards.like.multibyte.two"), 1); // Lóve alone

    // Anchoring is not a detail: these three patterns differ only in where
    // their '%' are, and each accepts a different set.
    assert_eq!(n("wildcards.like.anchored.gap.both"), 3);
    assert_eq!(n("wildcards.like.multigap.pattern"), 3);
    assert_eq!(n("wildcards.like.multigap.order"), 1); // ordering matters

    // Escapes.
    assert_eq!(n("wildcards.like.escape.percent.anchored"), 1); // the row "100%"
    assert_eq!(n("wildcards.like.escape.percent.contains"), 2); // + "100%off"
    assert_eq!(n("wildcards.like.escape.backslash"), 1);

    // Degenerate patterns.
    assert_eq!(n("wildcards.like.percent.only"), 39); // every row
    assert_eq!(n("wildcards.like.empty"), 1); // only the empty row
    assert_eq!(n("wildcards.like.any.nonempty"), 38); // every row but that one
    assert_eq!(n("wildcards.like.underscore.only"), 3); // the one-byte rows
    assert_eq!(n("wildcards.like.underscore.pair"), 4); // the two-byte rows
    assert_eq!(n("wildcards.like.nomatch"), 0);

    // Rows are byte strings.
    assert_eq!(n("wildcards.like.binary.nul"), 2);
    assert_eq!(n("wildcards.like.binary.ff"), 2);
}

#[test]
fn lowered_and_pattern_queries_bless_to_identical_truth() {
    let tmp = tempfile::tempdir().unwrap();
    let c = blessed_counts(tmp.path());
    // Each pair is the same predicate written twice: once with the literal op
    // the suite would lower it to, once as a pattern. Equal counts here are
    // the lowering equivalence observed on real data rather than argued.
    for (literal, pattern) in [
        ("wildcards.contains.contains.abc", "wildcards.like.contains.abc"),
        ("wildcards.prefix.prefix.ab", "wildcards.like.prefix.ab"),
        ("wildcards.suffix.suffix.c", "wildcards.like.suffix.c"),
        (
            "wildcards.multi_contains.multigap.literal",
            "wildcards.like.multigap.pattern",
        ),
    ] {
        assert_eq!(
            c[literal], c[pattern],
            "{literal} and {pattern} are the same predicate but blessed differently",
        );
    }
}

#[test]
fn invalid_patterns_are_rejected_at_load_not_matched() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(
        dir.join(suite::SUITE_FILE),
        r#"{"format_version":1,"id":"bad","dataset":{"id":"wildcards"}}"#,
    )
    .unwrap();
    // A trailing lone backslash has nothing to escape; upstream LIKE parsers
    // disagree about it, so the contract rejects it outright.
    std::fs::write(
        dir.join(suite::QUERIES_FILE),
        "{\"id\":\"bad.trailing\",\"op\":\"like\",\"needles\":[\"abc\\\\\"]}\n",
    )
    .unwrap();
    let msg = match suite::Suite::load_unblessed(dir) {
        Ok(_) => panic!("a pattern ending in a lone backslash must not load"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("invalid LIKE pattern"),
        "unexpected error: {msg}"
    );
}

/// T-CAP — the capability gate, end to end (TODO_like_workload.md §7).
///
/// Two failure modes must be impossible, and the gate canary's ABI-v8
/// strategies demonstrate both on the wildcard fixture:
///
/// - `like-declines` answers only `%literal%` and declines everything else
///   through `supports_query`. Declined cells must be `unsupported`, carry no
///   latency, and land in `cells_unsupported` rather than `cells_ok` — a
///   declared capability gap is not a correctness result.
/// - `like-lowers` declares the same op and cheats exactly as the contract
///   forbids, dropping metacharacters so `%ab_c%` becomes `contains("abc")`.
///   The gate must catch it.
#[test]
fn a_declined_pattern_is_skipped_and_a_lowered_one_is_caught() {
    use lb_harness::results::Writer;
    use lb_harness::runner;
    use lb_harness::spec::LoadedSpec;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Point a spec at the tracked fixture: already blessed, so this exercises
    // the real suite rather than a copy.
    let spec = format!(
        "[[datasets]]\npath = \"{}\"\n\n[[suites]]\npath = \"{}\"\n\n\
         [[candidates]]\nname = \"gate_canary\"\n\n\
         [measure]\nwarmup = 1\nmin_iters = 2\nmin_millis = 0\nchunk_rows = [0]\n",
        repo_root().join("datasets/wildcards").display(),
        repo_root().join("suites/wildcards").display(),
    );
    let spec_path = dir.join("spec.toml");
    std::fs::write(&spec_path, spec).unwrap();
    let loaded = LoadedSpec::load(&spec_path).unwrap();

    let out_path = dir.join("rows.jsonl");
    let mut writer = Writer::create(&out_path).unwrap();
    let summary = runner::run_worker(&loaded, "gate_canary", 0, 0, &mut writer, false).unwrap();
    writer.finish().unwrap();

    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&out_path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let cells = |strategy: &str, status: &str| -> Vec<&serde_json::Value> {
        rows.iter()
            .filter(|r| {
                r["kind"] == "query" && r["strategy"] == strategy && r["status"] == status
            })
            .collect()
    };

    // Declining, not answering: every underscore and anchored-gap pattern is
    // skipped, and nothing it skipped produced a number.
    let declined = cells("like-declines", "unsupported");
    assert!(
        declined.len() > 20,
        "expected the declining strategy to skip most of the corpus, got {}",
        declined.len()
    );
    for r in &declined {
        assert!(r["latency"].is_null(), "a declined cell must have no latency");
        assert!(r["gate"].is_null(), "a declined cell was never gated");
    }
    assert!(
        cells("like-declines", "gate_failed").is_empty(),
        "declining correctly must not fail a gate"
    );
    assert!(
        !cells("like-declines", "ok").is_empty(),
        "the strategy must still answer the patterns it accepted"
    );

    // Cheating, and caught. Note it is accidentally right on some patterns —
    // stripping '%' from `%abc%` gives the right answer — which is exactly
    // why a canary exercised only on `%abc%` would prove nothing.
    let caught = cells("like-lowers", "gate_failed");
    assert!(
        caught.len() > 10,
        "silently lowering a pattern must fail the gate, got {} failures",
        caught.len()
    );

    // A skip is not a pass: the two land in different counters.
    assert_eq!(
        summary.cells_unsupported as usize,
        rows.iter()
            .filter(|r| r["kind"] == "query" && r["status"] == "unsupported")
            .count()
    );
    assert_eq!(
        summary.cells_ok as usize,
        rows.iter()
            .filter(|r| r["kind"] == "query" && r["status"] == "ok")
            .count()
    );
    assert!(summary.gate_failures >= caught.len() as u64);
}
