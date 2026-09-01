//! Regression test for the guarded compressed-stream layout shared by
//! `fsst_like_tum` and `dict_fsst_like_tum` (DESIGN.md §17.7): the upstream
//! FSST-LIKE kernels read one byte before the row they are handed during a
//! backward (suffix) scan, and a 0xFF there — the previous row's escaped 0xFF
//! literal in a contiguous layout — turns a match at the row start into a false
//! negative in EVERY backend. Both candidates now separate rows whenever any
//! compressed row ends in 0xFF; this test builds a corpus where that is the case
//! and drives every strategy of both through the correctness gate.
//!
//! `dict_fsst_like_tum` matches over the DEDUPLICATED unique values rather than
//! the rows, so most of the row adjacencies this corpus plants do not survive
//! dedup — but the `long_row(0xFF)` / `"e"` pair does (its successor is a first
//! occurrence), and that one is enough: with the separator suppressed the dict
//! candidate misses exactly one row on `suffix.e`. Verified by disabling
//! `sep` locally, 2026-09-01: fsst_like_tum failed 6 of 23 cells, the dict
//! candidate 1 of 23; both are clean with it enabled.
//!
//! The corpus is generated in-process (a portable LCG, no fixture file): FSST
//! trains on a ~16 KB line sample, so 0xFF is only escaped — the precondition
//! for the hazard — when the few rows carrying it are not sampled. Seed 1 over
//! 3000 short rows is a verified such corpus; the test asserts the precondition
//! through the candidate's own `stream_padding` footprint (separators active).

use std::path::{Path, PathBuf};

use lb_harness::dataset::{self, PreparedDataset};
use lb_harness::results::Writer;
use lb_harness::runner;
use lb_harness::spec::LoadedSpec;
use lb_harness::suite;

const SEED: u64 = 1;
const SHORT_ROWS: usize = 3000;
const GUARD_PAD: u64 = 64; // mirrors kGuardPad in the candidate
const STRATEGIES: &[&str] = &[
    "interp", "cpp", "cpp-simd", "llvm", "llvm-simd", // fsst_like_tum
    "dict+interp",                                    // dict_fsst_like_tum
];
/// (candidate, whether its padding is charged over rows or unique values)
const CANDIDATES: &[&str] = &["fsst_like_tum", "dict_fsst_like_tum"];
const WORDS: &[&str] = &[
    "the", "of", "and", "to", "in", "a", "is", "that", "for", "it", "as", "was", "with", "be", "by",
    "on", "not", "he", "this", "are", "or", "his", "from", "at", "which", "but", "have", "an", "had",
    "they", "you", "were", "their", "one", "all", "we", "can", "her", "has", "there", "been", "if",
    "more", "when", "will", "would", "who", "so", "no", "http", "https", "www", "com", "org", "index",
    "html", "query", "search", "user", "customer", "comment", "abstract", "wikipedia", "database",
    "compression", "symbol", "table", "string", "match", "pattern", "automaton", "fast", "static",
    "carefully", "regular", "requests", "special", "packages", "ironic", "furiously", "slyly",
    "blithely", "daringly", "pending", "final", "deposits", "accounts", "platelets", "foxes",
    "instructions", "theodolites", "excuses", "dolphins", "cat", "dog", "goose", "mouse", "the", "the",
];

struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(2654435761).wrapping_add(1))
    }
    fn below(&mut self, k: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % k as u64) as usize
    }
    fn sentence(&mut self, words: usize) -> Vec<u8> {
        (0..words)
            .map(|_| WORDS[self.below(WORDS.len())])
            .collect::<Vec<_>>()
            .join(" ")
            .into_bytes()
    }
    /// A row longer than one FSST 511-byte compression chunk, ending in `tail`.
    fn long_row(&mut self, tail: &[u8]) -> Vec<u8> {
        let mut row = self.sentence(90);
        while row.len() < 530 {
            row.push(b' ');
            row.extend(self.sentence(5));
        }
        row.extend(tail);
        row
    }
}

/// Short filler, the repeated short words that become whole FSST symbols, then
/// 0xFF-terminated long rows each immediately followed by a row a suffix query
/// matches at its very start (plus controls and the SEMANTICS.md edge rows).
fn corpus() -> Vec<Vec<u8>> {
    let mut rng = Lcg::new(SEED);
    let mut rows: Vec<Vec<u8>> = (0..SHORT_ROWS)
        .map(|_| {
            let words = 1 + rng.below(14);
            rng.sentence(words)
        })
        .collect();
    let common: [&[u8]; 5] = [b"the", b"cat", b"dog", b"goose", b"mouse"];
    for _ in 0..40 {
        rows.extend(common.iter().map(|r| r.to_vec()));
    }
    let specials: Vec<Vec<u8>> = vec![
        rng.long_row(b"\xff"), b"the".to_vec(),
        rng.long_row(b"abc\xff"), b"cat".to_vec(),
        rng.long_row(b"\xffz\xff"), b"goose".to_vec(),
        rng.long_row(b"x\xffy"), b"mouse".to_vec(), // 0xFF not at the end: control
        Vec::new(), b"the".to_vec(),                 // empty row before a match: control
        rng.long_row(b"\xff"), b"e".to_vec(),
        rng.long_row(b"\xff"), Vec::new(),
        rng.long_row(b"the\xff"), b"the".to_vec(),
        rng.long_row(b""), b"the".to_vec(),          // long row without 0xFF: control
        b"back\\slash".to_vec(), b"trailing\\".to_vec(), b"\\".to_vec(),
        b"50% off_now\\here".to_vec(), b"a\\%b".to_vec(),
        rng.long_row(b"\xff"),                       // 0xFF-terminated last row
    ];
    rows.extend(specials);
    rows
}

fn write_csv(rows: &[Vec<u8>], path: &Path) {
    let mut out = b"data\n".to_vec();
    for row in rows {
        if row.is_empty() {
            out.extend(b"\"\"\n");
        } else if row.iter().any(|b| matches!(b, b',' | b'"' | b'\n' | b'\r')) {
            out.push(b'"');
            for &b in row {
                if b == b'"' {
                    out.push(b'"');
                }
                out.push(b);
            }
            out.extend(b"\"\n");
        } else {
            out.extend(row);
            out.push(b'\n');
        }
    }
    std::fs::write(path, out).unwrap();
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn prepare(dir: &Path) -> (PathBuf, PathBuf, usize) {
    let rows = corpus();
    let csv = dir.join("fsst_like_guard.csv");
    write_csv(&rows, &csv);
    let ds_dir = dir.join("dataset");
    dataset::ingest(&dataset::IngestRequest {
        source: csv,
        format: "csv".into(),
        column: "data".into(),
        id: "fsst_like_guard".into(),
        out_dir: ds_dir.clone(),
    })
    .expect("ingest generated corpus");
    let suite_dir = dir.join("suite");
    std::fs::create_dir_all(&suite_dir).unwrap();
    for f in ["suite.json", "queries.jsonl"] {
        std::fs::copy(repo_root().join("suites/fsst_like_guard").join(f), suite_dir.join(f)).unwrap();
    }
    let ds = PreparedDataset::load(&ds_dir, true).expect("load corpus");
    // Printed so the corpus can be cross-checked against an external generator
    // (`cargo test -- --nocapture`); the dataset checksum is its identity.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(ds_dir.join("manifest.json")).unwrap()).unwrap();
    eprintln!("fsst_like_guard corpus: {} rows, checksum {}", rows.len(), manifest["checksum"]);
    suite::bless(&suite_dir, &ds, false).expect("bless guard suite");
    (ds_dir, suite_dir, rows.len())
}

fn write_spec(dir: &Path, ds: &Path, suite: &Path) -> PathBuf {
    let candidate_blocks: String = CANDIDATES
        .iter()
        .map(|name| format!("[[candidates]]\nname = \"{name}\"\n\n"))
        .collect();
    let spec = format!(
        "strategies = {}\n\n[measure]\nwarmup = 0\nmin_iters = 1\nmin_millis = 0\nchunk_rows = [0]\n\n[[datasets]]\npath = \"{}\"\n\n[[suites]]\npath = \"{}\"\n\n{candidate_blocks}[[scanners]]\nname = \"memmem\"\n",
        serde_json::to_string(STRATEGIES).unwrap(),
        ds.display(),
        suite.display(),
    );
    let path = dir.join("guard.toml");
    std::fs::write(&path, spec).unwrap();
    path
}

#[test]
fn fsst_like_backends_survive_rows_ending_in_escaped_0xff() {
    let tmp = tempfile::tempdir().unwrap();
    let (ds_dir, suite_dir, num_rows) = prepare(tmp.path());
    let loaded = LoadedSpec::load(&write_spec(tmp.path(), &ds_dir, &suite_dir)).unwrap();

    for (index, candidate) in CANDIDATES.iter().enumerate() {
        let out_path = tmp.path().join(format!("guard-{index}.jsonl"));
        let mut writer = Writer::create(&out_path).unwrap();
        let summary = runner::run_worker(&loaded, candidate, 0, 0, &mut writer, false).unwrap();
        writer.finish().unwrap();
        assert_eq!(summary.gate_failures, 0, "{candidate}: gate failures");
        assert_eq!(summary.errors, 0, "{candidate}: errors");

        let rows: Vec<serde_json::Value> = std::fs::read_to_string(&out_path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let queries: Vec<&serde_json::Value> =
            rows.iter().filter(|r| r["kind"] == "query").collect();
        assert!(!queries.is_empty(), "{candidate}: no query cells recorded");
        for q in &queries {
            assert_eq!(q["status"], "ok", "{candidate} {} / {}", q["strategy"], q["query_id"]);
        }
        // interp/dict+interp are always registered; cpp*/llvm* are host-gated
        // (runtime clang++, LLVM 14-16, x86-64) and simply absent elsewhere, so
        // report those rather than requiring them.
        let strategies: std::collections::BTreeSet<&str> =
            queries.iter().map(|q| q["strategy"].as_str().unwrap()).collect();
        assert!(!strategies.is_empty(), "{candidate}: no strategy ran");
        eprintln!("{candidate} strategies exercised: {strategies:?}");

        // The precondition actually held: some compressed row ends in 0xFF, so
        // the candidate switched on separators (one byte per row on top of the
        // two guard blocks). fsst_like_tum lays out every row; the dict wrapper
        // lays out the deduplicated unique values, so it charges fewer.
        let build = rows.iter().find(|r| r["kind"] == "build").unwrap();
        let padding = build["footprint_components"]["stream_padding"]
            .as_u64()
            .unwrap_or_else(|| panic!("{candidate}: no stream_padding component"));
        let laid_out = padding
            .checked_sub(2 * GUARD_PAD)
            .unwrap_or_else(|| panic!("{candidate}: padding {padding} below the guard blocks"));
        assert!(
            laid_out > 0 && laid_out <= num_rows as u64,
            "{candidate}: corpus no longer exercises the 0xFF-at-row-end hazard \
             (separators inactive: {laid_out} of at most {num_rows} entries)"
        );
        if *candidate == "fsst_like_tum" {
            assert_eq!(laid_out, num_rows as u64, "one separator per row");
        }
    }
}
