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
