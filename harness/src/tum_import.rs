//! `bench tum-import` — the upstream DaMoN'26 FSST-LIKE pattern corpus as
//! suites (TODO_like_workload.md §10, `suites/tum_like/PROVENANCE.md`).
//!
//! `patterns.json` groups patterns by upstream dataset, then by the number
//! of `_` wildcards (`"0"`, `"1"`, `">=2"`), then by table (and, for TPC-H,
//! column); TPC-H entries also name the benchmark query a pattern comes
//! from. One import selects one (dataset, table[, column]) and writes one
//! unblessed suite bound to one of *our* datasets, with every pattern
//! stored under the narrowest equivalent op (`crate::like::lower`) and the
//! full upstream coordinates in `meta.tum`.
//!
//! Truth is never imported: `bench bless` computes it against our physical
//! column, which is not the paper's. That is why the suite description says
//! *adapted*, and why the counts a pattern gets here are not a claim about
//! the paper's.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::dataset::PreparedDataset;
use crate::like;
use crate::oracle;
use crate::suite::{NeedleJson, QueryRecord, SuiteManifest, QUERIES_FILE, SUITE_FILE};

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

/// Where the corpus came from, recorded on every query.
pub struct Provenance {
    pub repo: String,
    pub commit: String,
    pub sha256: String,
}

impl Provenance {
    /// The pin documented in `suites/tum_like/PROVENANCE.md`.
    pub fn pinned() -> Provenance {
        Provenance {
            repo: "https://github.com/calin2110/FSST-LIKE-Matching".into(),
            commit: "b1eb3ab9c63ea0199a381b92371a3154190b4406".into(),
            sha256: "62b8ba01ac90d39ecd2199ae3727a94623886110752fce6632e83a370194b6d1".into(),
        }
    }
}

pub struct ImportRequest<'a> {
    pub patterns_json: &'a Path,
    /// Upstream top-level key: `TPCH`, `IMDB`, `StackOverflow`, `PublicBI`.
    pub upstream_dataset: String,
    /// Upstream table (`part`, `films`, …).
    pub table: String,
    /// Upstream column — TPC-H only; the other datasets have one text
    /// column per table.
    pub column: Option<String>,
    pub suite_id: String,
    pub provenance: Provenance,
}

/// One upstream entry, after flattening the JSON's two shapes: TPC-H lists
/// `{query, pattern}` objects, everything else lists bare strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamPattern {
    pub pattern: Vec<u8>,
    /// `"0"`, `"1"` or `"2plus"` — upstream's grouping, kept as data rather
    /// than recounted, so the suite is auditable against the file.
    pub underscore_group: &'static str,
    /// TPC-H query number the pattern originates from, when upstream says.
    pub tpch_query: Option<u32>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Entry {
    Annotated {
        query: Option<u32>,
        pattern: String,
    },
    Bare(String),
}

fn group_label(key: &str) -> Result<&'static str> {
    Ok(match key {
        "0" => "0",
        "1" => "1",
        ">=2" => "2plus",
        other => return Err(format!("unknown underscore group {other:?} in patterns.json").into()),
    })
}

/// Read every pattern for one (dataset, table[, column]) in file order —
/// groups `0`, `1`, `>=2`, each in its listed order — so ids are stable.
pub fn select(
    json: &serde_json::Value,
    upstream_dataset: &str,
    table: &str,
    column: Option<&str>,
) -> Result<Vec<UpstreamPattern>> {
    let ds = json
        .get(upstream_dataset)
        .ok_or_else(|| format!("no upstream dataset {upstream_dataset:?} in patterns.json"))?;
    let groups = ds
        .as_object()
        .ok_or("dataset value is not an object of underscore groups")?;
    let mut out = Vec::new();
    for key in ["0", "1", ">=2"] {
        let Some(group) = groups.get(key) else { continue };
        let Some(by_table) = group.get(table) else { continue };
        let list = match column {
            Some(c) => by_table
                .get(c)
                .ok_or_else(|| format!("no column {c:?} under {upstream_dataset}/{key}/{table}"))?,
            None => by_table,
        };
        let entries: Vec<Entry> = serde_json::from_value(list.clone()).map_err(|e| {
            format!("{upstream_dataset}/{key}/{table}: entries are neither strings nor {{query, pattern}}: {e}")
        })?;
        let label = group_label(key)?;
        for e in entries {
            let (tpch_query, pattern) = match e {
                Entry::Annotated { query, pattern } => (query, pattern),
                Entry::Bare(pattern) => (None, pattern),
            };
            out.push(UpstreamPattern {
                pattern: pattern.into_bytes(),
                underscore_group: label,
                tpch_query,
            });
        }
    }
    if out.is_empty() {
        return Err(format!(
            "no patterns for {upstream_dataset}/{table}{} — check the table/column spelling against patterns.json",
            column.map(|c| format!("/{c}")).unwrap_or_default()
        )
        .into());
    }
    Ok(out)
}

pub struct ImportOutcome {
    pub queries: usize,
    /// How many patterns landed under each stored op — the lowering split.
    pub by_op: BTreeMap<String, usize>,
}

fn needle_json(bytes: &[u8]) -> NeedleJson {
    use base64::Engine;
    match std::str::from_utf8(bytes) {
        Ok(s) => NeedleJson::Text(s.to_string()),
        Err(_) => NeedleJson::B64 {
            b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    }
}

/// Turn selected upstream patterns into suite records: validate, lower to
/// the narrowest op, and attach provenance. Pure; no I/O.
pub fn to_records(
    patterns: &[UpstreamPattern],
    req: &ImportRequest,
) -> Result<(Vec<QueryRecord>, BTreeMap<String, usize>)> {
    let mut records = Vec::with_capacity(patterns.len());
    let mut by_op: BTreeMap<String, usize> = BTreeMap::new();
    let mut seq: BTreeMap<&str, usize> = BTreeMap::new();
    for p in patterns {
        oracle::validate_like_pattern(&p.pattern).map_err(|e| {
            format!(
                "upstream pattern {:?} is not a valid LIKE pattern: {e}",
                String::from_utf8_lossy(&p.pattern)
            )
        })?;
        let (op, needles) = match like::lower(&p.pattern) {
            Some((op, needles)) => (op, needles),
            None => (lb_abi::LB_LIKE, vec![p.pattern.clone()]),
        };
        let op_name = lb_abi::op_name(op).to_string();
        *by_op.entry(op_name.clone()).or_default() += 1;
        let n = seq.entry(p.underscore_group).or_default();
        let id = format!(
            "{}.u{}.{:03}",
            req.suite_id, p.underscore_group, *n
        );
        *n += 1;
        let facts = like::facts(&p.pattern);
        records.push(QueryRecord {
            id,
            op: op_name,
            needles: needles.iter().map(|n| needle_json(n)).collect(),
            meta: Some(serde_json::json!({
                "source": "TUM",
                "like": { "pattern": String::from_utf8_lossy(&p.pattern) },
                "tum": {
                    "repo": req.provenance.repo,
                    "commit": req.provenance.commit,
                    "patterns_sha256": req.provenance.sha256,
                    "upstream_dataset": req.upstream_dataset,
                    "table": req.table,
                    "column": req.column,
                    "underscore_group": p.underscore_group,
                    "tpch_query": p.tpch_query,
                    // Recounted from the pattern so a disagreement with the
                    // upstream group is visible rather than silently trusted.
                    "underscore_count": facts.underscore_count,
                },
            })),
            truth: None,
            derived: None,
        });
    }
    Ok((records, by_op))
}

/// Write one adapted suite. Refuses to overwrite an existing `queries.jsonl`
/// unless `force`, exactly as `bench gen` does — a blessed suite is never
/// silently clobbered.
pub fn import(req: &ImportRequest, ds: &PreparedDataset, out_dir: &Path, force: bool) -> Result<ImportOutcome> {
    if out_dir.join(QUERIES_FILE).exists() && !force {
        return Err(format!(
            "{} already exists — pass --force to replace it (this discards blessed truth)",
            out_dir.join(QUERIES_FILE).display()
        )
        .into());
    }
    let text = std::fs::read_to_string(req.patterns_json)
        .map_err(|e| format!("reading {}: {e}", req.patterns_json.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let patterns = select(&json, &req.upstream_dataset, &req.table, req.column.as_deref())?;
    let (records, by_op) = to_records(&patterns, req)?;

    let column_note = req
        .column
        .as_ref()
        .map(|c| format!(".{c}"))
        .unwrap_or_default();
    let manifest = SuiteManifest {
        format_version: 1,
        id: req.suite_id.clone(),
        description: format!(
            "Adapted TUM LIKE workload: the {} patterns for upstream {}/{}{} from the DaMoN'26 \
             FSST-LIKE benchmark ({}@{}), stored under the narrowest equivalent op and blessed \
             against OUR column {} — not an exact reproduction of the paper's data. Underscore \
             groups 0/1/>=2 are kept in meta.tum.",
            records.len(),
            req.upstream_dataset,
            req.table,
            column_note,
            req.provenance.repo,
            &req.provenance.commit[..12],
            ds.manifest.id,
        ),
        dataset: crate::suite::DatasetBinding {
            id: ds.manifest.id.clone(),
            checksum: None,
        },
        provenance: Some(serde_json::json!({
            "importer": { "name": "tum-import", "version": "v1" },
            "upstream": {
                "repo": req.provenance.repo,
                "commit": req.provenance.commit,
                "file": "benchmark/patterns.json",
                "sha256": req.provenance.sha256,
                "dataset": req.upstream_dataset,
                "table": req.table,
                "column": req.column,
            },
            "lowering": by_op,
        })),
        truth_algo: None,
        blessed_at: None,
    };

    std::fs::create_dir_all(out_dir)?;
    let mut lines = String::new();
    for r in &records {
        lines.push_str(&serde_json::to_string(r)?);
        lines.push('\n');
    }
    std::fs::write(out_dir.join(QUERIES_FILE), lines)?;
    std::fs::write(
        out_dir.join(SUITE_FILE),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    Ok(ImportOutcome {
        queries: records.len(),
        by_op,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "TPCH": {
        "0":   { "part": { "p_type": [ {"query": 2, "pattern": "%BRASS%"}, {"query": null, "pattern": "%BRASS"} ] } },
        "1":   { "part": { "p_type": [ {"query": 2, "pattern": "%BR_SS%"} ] } },
        ">=2": { "part": { "p_type": [ {"query": 16, "pattern": "MED_UM POLISH_D%"} ] } }
      },
      "IMDB": {
        "0":   { "films": [ "The %", "% (1) - % Berlin" ], "actors": [ "Fred %" ] },
        "1":   { "films": [ "_he %" ] },
        ">=2": { "films": [ "__e %" ] }
      }
    }"#;

    fn req(ds: &str, table: &str, column: Option<&str>) -> ImportRequest<'static> {
        ImportRequest {
            patterns_json: Path::new("unused"),
            upstream_dataset: ds.into(),
            table: table.into(),
            column: column.map(String::from),
            suite_id: "t".into(),
            provenance: Provenance::pinned(),
        }
    }

    #[test]
    fn selects_both_json_shapes_in_group_then_file_order() {
        let json: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        let tpch = select(&json, "TPCH", "part", Some("p_type")).unwrap();
        let pats: Vec<&str> = tpch
            .iter()
            .map(|p| std::str::from_utf8(&p.pattern).unwrap())
            .collect();
        assert_eq!(pats, ["%BRASS%", "%BRASS", "%BR_SS%", "MED_UM POLISH_D%"]);
        assert_eq!(tpch[0].tpch_query, Some(2));
        assert_eq!(tpch[1].tpch_query, None);
        assert_eq!(tpch[3].underscore_group, "2plus");

        let films = select(&json, "IMDB", "films", None).unwrap();
        assert_eq!(films.len(), 4);
        assert!(films.iter().all(|p| p.tpch_query.is_none()));
        assert!(select(&json, "IMDB", "quotes", None).is_err());
        assert!(select(&json, "TPCH", "part", Some("nope")).is_err());
    }

    #[test]
    fn records_lower_to_the_narrowest_op_and_keep_provenance() {
        let json: serde_json::Value = serde_json::from_str(SAMPLE).unwrap();
        let r = req("TPCH", "part", Some("p_type"));
        let pats = select(&json, "TPCH", "part", Some("p_type")).unwrap();
        let (records, by_op) = to_records(&pats, &r).unwrap();
        let ops: Vec<&str> = records.iter().map(|q| q.op.as_str()).collect();
        // %BRASS% -> contains, %BRASS -> suffix, %BR_SS% -> like, MED_UM… -> like
        assert_eq!(ops, ["contains", "suffix", "like", "like"]);
        assert_eq!(by_op["contains"], 1);
        assert_eq!(by_op["like"], 2);
        // ids are stable per group and the pattern text is always carried
        assert_eq!(records[0].id, "t.u0.000");
        assert_eq!(records[2].id, "t.u1.000");
        let meta = records[2].meta.as_ref().unwrap();
        assert_eq!(meta["like"]["pattern"], "%BR_SS%");
        assert_eq!(meta["tum"]["underscore_group"], "1");
        assert_eq!(meta["tum"]["underscore_count"], 1);
        assert_eq!(meta["tum"]["tpch_query"], 2);
        assert_eq!(meta["tum"]["commit"], Provenance::pinned().commit);
        // the film pattern with an anchored tail cannot lower
        let films = select(&json, "IMDB", "films", None).unwrap();
        let (fr, _) = to_records(&films, &req("IMDB", "films", None)).unwrap();
        assert_eq!(fr[0].op, "prefix"); // The %
        assert_eq!(fr[1].op, "like"); // % (1) - % Berlin
    }
}
