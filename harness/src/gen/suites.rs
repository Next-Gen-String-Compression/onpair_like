//! Bridge shared generation results to the existing suite and oracle formats.

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::Path;

use base64::Engine;

use super::GeneratedNeedles;
use crate::dataset::PreparedDataset;
use crate::suite::{self, DatasetBinding, NeedleJson, QueryRecord, Result, Suite, SuiteManifest};

/// Write an unblessed suite and its complete coverage report. The generator's
/// counts are claims to verify, not benchmark truth.
pub fn write_balanced_suite(
    generated: &GeneratedNeedles,
    dataset: &PreparedDataset,
    directory: &Path,
    id: &str,
    force: bool,
) -> Result<()> {
    if generated.dataset_checksum != dataset.manifest.checksum
        || generated.total_rows != dataset.num_rows()
    {
        return Err("generated needles belong to a different dataset".into());
    }
    if directory.join(suite::QUERIES_FILE).exists() && !force {
        return Err("suite already exists; pass --force to replace it".into());
    }
    let mut unique = HashSet::new();
    for needle in &generated.needles {
        if !unique.insert(&needle.bytes) {
            return Err("duplicate needle bytes in generation result".into());
        }
    }
    let manifest = SuiteManifest {
        format_version: 1,
        id: id.into(),
        description: "SA + LCP length/selectivity-stratified CONTAINS needles".into(),
        dataset: DatasetBinding {
            id: dataset.manifest.id.clone(),
            checksum: None,
        },
        provenance: Some(serde_json::json!({"generator": generated.generator,
            "dataset_checksum": generated.dataset_checksum, "request": generated.request,
            "selection": "interval-span reservoir followed by balanced byte lengths"})),
        truth_algo: None,
        blessed_at: None,
    };
    std::fs::create_dir_all(directory)?;
    let mut queries = BufWriter::new(std::fs::File::create(directory.join(suite::QUERIES_FILE))?);
    for (i, needle) in generated.needles.iter().enumerate() {
        let encoded = match std::str::from_utf8(&needle.bytes) {
            Ok(text) => NeedleJson::Text(text.into()),
            Err(_) => NeedleJson::B64 {
                b64: base64::engine::general_purpose::STANDARD.encode(&needle.bytes),
            },
        };
        let record = QueryRecord {
            id: format!("{id}.{i:06}"),
            op: "contains".into(),
            needles: vec![encoded],
            truth: None,
            derived: None,
            meta: Some(serde_json::json!({"sa_lcp": {
                "cell": needle.cell, "length_bucket": generated.cells[needle.cell].length,
                "row_bucket": generated.cells[needle.cell].matching_rows,
                "matching_rows": needle.matching_rows, "source_position": needle.source_position,
                "mutation_position": needle.mutation_position,
            }})),
        };
        serde_json::to_writer(&mut queries, &record)?;
        queries.write_all(b"\n")?;
    }
    queries.flush()?;
    std::fs::write(
        directory.join(suite::SUITE_FILE),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    let report = serde_json::json!({"generator": generated.generator,
        "dataset_checksum": generated.dataset_checksum, "total_rows": generated.total_rows,
        "request": generated.request, "queries": generated.needles.len(),
        "cells": generated.cells});
    std::fs::write(
        directory.join("gen-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    Ok(())
}

/// After `bench bless`, verify SA counts, grid assignments and mutation witnesses
/// against the independently computed truth. No second oracle scan is needed.
pub fn verify_balanced_suite(directory: &Path, dataset: &PreparedDataset) -> Result<()> {
    let suite = Suite::load_for_run(directory, dataset)?;
    let provenance = suite
        .manifest
        .provenance
        .as_ref()
        .ok_or("missing generation provenance")?;
    if provenance["dataset_checksum"] != dataset.manifest.checksum {
        return Err("generation dataset checksum mismatch".into());
    }
    let mut seen = HashSet::new();
    for q in &suite.queries {
        if q.op != lb_abi::LB_CONTAINS || q.needles.len() != 1 {
            return Err("expected single-needle CONTAINS queries".into());
        }
        let bytes = &q.needles[0];
        if !seen.insert(bytes) {
            return Err(format!("duplicate needle in {}", q.record.id).into());
        }
        let meta = q
            .record
            .meta
            .as_ref()
            .and_then(|m| m.get("sa_lcp"))
            .ok_or("missing SA metadata")?;
        let expected = meta["matching_rows"].as_u64().ok_or("missing row count")?;
        let actual = q
            .record
            .truth
            .as_ref()
            .ok_or("suite must be blessed first")?
            .count;
        if actual != expected {
            return Err(format!(
                "{}: SA predicts {expected} matching rows, oracle finds {actual}",
                q.record.id
            )
            .into());
        }
        let length: super::LengthBucket = serde_json::from_value(meta["length_bucket"].clone())?;
        let rows: super::RowBucket = serde_json::from_value(meta["row_bucket"].clone())?;
        if bytes.len() < length.min
            || bytes.len() > length.max
            || actual < rows.min
            || actual > rows.max
        {
            return Err(format!("{}: oracle result outside generation cell", q.record.id).into());
        }
        let source = meta["source_position"]
            .as_u64()
            .ok_or("missing source witness")? as usize;
        let end = source.checked_add(bytes.len()).ok_or("witness overflow")?;
        let witness = dataset
            .payload()
            .get(source..end)
            .ok_or("witness out of bounds")?;
        let row = dataset
            .offsets_u64()
            .partition_point(|&p| p <= source as u64);
        if row >= dataset.offsets_u64().len() || end as u64 > dataset.offsets_u64()[row] {
            return Err("witness crosses row boundary".into());
        }
        match meta["mutation_position"].as_u64() {
            Some(p) => {
                let differences: Vec<_> = witness
                    .iter()
                    .zip(bytes)
                    .enumerate()
                    .filter_map(|(i, (a, b))| (a != b).then_some(i))
                    .collect();
                if actual != 0 || differences != [p as usize] {
                    return Err("invalid negative mutation witness".into());
                }
            }
            None if witness != bytes => return Err("invalid positive witness".into()),
            None => {}
        }
    }
    Ok(())
}
