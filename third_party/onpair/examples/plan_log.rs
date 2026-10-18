// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Logs the prefilter's alignment graph and min cut for every needle in every
//! row group of a column, as three CSVs appended to `--out`:
//!
//!   `cargo run --release --example plan_log -- --col C.col --needles N.ndl
//!    --dataset NAME --chunk-rows 524288 --bits 12 [--prune 1.0] --out DIR`
//!
//! `queries.csv`: uuid, dataset, row_group, needle, algorithm, rows,
//! needle_selectivity, cut_selectivity_estimated, cut_selectivity_measured.
//! `symbols.csv`: dataset, row_group, algorithm, token_id, content, frequency.
//! `graph.csv`: uuid, from, to, label_type, label, cut.
//! Needles and token contents are base64. The uuid is a hash of
//! (dataset, row_group, needle, algorithm), the same one the FSST logger writes.
//!
//! `.col` is u64 num_rows, u64 payload_len, num_rows+1 u64 offsets, payload.
//! `.ndl` is u64 count, then u64 len and the bytes per needle. Little-endian.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use onpair::search::index::build_token_frequency_index;
use onpair::search::{Finder, TraceLabel, Verify, trace_prefilter};
use onpair::{Column, Config, DictionaryView, MaxDictBits, PruneFactor, Threshold};

fn u64_at(bytes: &[u8], at: &mut usize) -> u64 {
    let v = u64::from_le_bytes(bytes[*at..*at + 8].try_into().unwrap());
    *at += 8;
    v
}

fn read_col(path: &Path) -> (Vec<u8>, Vec<u64>) {
    let raw = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut at = 0;
    let rows = u64_at(&raw, &mut at) as usize;
    let len = u64_at(&raw, &mut at) as usize;
    let offsets: Vec<u64> = (0..=rows).map(|_| u64_at(&raw, &mut at)).collect();
    (raw[at..at + len].to_vec(), offsets)
}

fn read_needles(path: &Path) -> Vec<Vec<u8>> {
    let raw = fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut at = 0;
    let count = u64_at(&raw, &mut at);
    (0..count)
        .map(|_| {
            let len = u64_at(&raw, &mut at) as usize;
            at += len;
            raw[at - len..at].to_vec()
        })
        .collect()
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn fnv1a(bytes: &[u8], basis: u64) -> u64 {
    bytes.iter().fold(basis, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// Two FNV-1a passes over `dataset \x1f row_group \x1f needle_b64 \x1f algorithm`,
/// the second seeded with the first, printed as a uuid.
fn uuid(dataset: &str, row_group: usize, needle_b64: &str, algorithm: &str) -> String {
    let key = format!("{dataset}\x1f{row_group}\x1f{needle_b64}\x1f{algorithm}");
    let hi = fnv1a(key.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv1a(key.as_bytes(), hi);
    let h = format!("{hi:016x}{lo:016x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

fn csv(dir: &Path, name: &str, header: &str) -> BufWriter<File> {
    let path = dir.join(name);
    let fresh = fs::metadata(&path).map_or(true, |m| m.len() == 0);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut out = BufWriter::new(file);
    if fresh {
        writeln!(out, "{header}").unwrap();
    }
    out
}

fn list(ids: impl Iterator<Item = u32>) -> String {
    let items: Vec<String> = ids.map(|id| id.to_string()).collect();
    format!("\"[{}]\"", items.join(","))
}

fn main() {
    let (mut col, mut needles, mut dataset, mut out) = (None, None, None, None);
    let (mut chunk_rows, mut bits, mut prune) = (0usize, 16u8, None);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} needs a value"));
        match flag.as_str() {
            "--col" => col = Some(PathBuf::from(value)),
            "--needles" => needles = Some(PathBuf::from(value)),
            "--dataset" => dataset = Some(value),
            "--out" => out = Some(PathBuf::from(value)),
            "--chunk-rows" => chunk_rows = value.parse().expect("--chunk-rows"),
            "--bits" => bits = value.parse().expect("--bits"),
            "--prune" => prune = Some(value.parse::<f64>().expect("--prune")),
            other => panic!("unknown argument {other}"),
        }
    }
    let (col, needles, dataset, out) = (
        col.expect("--col"),
        needles.expect("--needles"),
        dataset.expect("--dataset"),
        out.expect("--out"),
    );
    // The onpair_spiral candidate's config: threshold 0.15 and seed 42.
    let config = Config {
        max_dict_bits: MaxDictBits::new(bits).expect("--bits in [9, 16]"),
        threshold: Threshold::new(0.15).unwrap(),
        seed: Some(42),
        prune: prune.map(|f| PruneFactor::new(f).expect("--prune >= 0")),
    };
    let algorithm = format!("onpair{bits}");

    let (payload, offsets) = read_col(&col);
    let needles = read_needles(&needles);
    let total_rows = offsets.len() - 1;
    let step = if chunk_rows == 0 {
        total_rows
    } else {
        chunk_rows
    };

    fs::create_dir_all(&out).unwrap();
    let mut queries = csv(
        &out,
        "queries.csv",
        "uuid,dataset,row_group,needle,algorithm,rows,needle_selectivity,\
         cut_selectivity_estimated,cut_selectivity_measured",
    );
    let mut symbols = csv(
        &out,
        "symbols.csv",
        "dataset,row_group,algorithm,token_id,content,frequency",
    );
    let mut graph = csv(&out, "graph.csv", "uuid,from,to,label_type,label,cut");

    for (row_group, start) in (0..total_rows).step_by(step).enumerate() {
        let end = (start + step).min(total_rows);
        let base = offsets[start];
        let bytes = &payload[base as usize..offsets[end] as usize];
        let local: Vec<u32> = offsets[start..=end]
            .iter()
            .map(|&o| (o - base) as u32)
            .collect();
        let rows = end - start;

        let column = Column::<u32>::compress(bytes, &local, config).expect("compress");
        let view = column.view();
        let freq = build_token_frequency_index(view.codes, view.dict.num_tokens()).unwrap();

        for id in 0..view.dict.num_tokens() {
            let token = id as onpair::Token;
            writeln!(
                symbols,
                "{dataset},{row_group},{algorithm},{id},{},{}",
                base64(view.dict.token(token)),
                freq.frequency(token)
            )
            .unwrap();
        }

        for needle in &needles {
            assert!(!needle.is_empty(), "empty needle");
            let finder = memchr::memmem::Finder::new(needle);
            let matching: Vec<usize> = (0..rows)
                .filter(|&r| {
                    finder
                        .find(&bytes[local[r] as usize..local[r + 1] as usize])
                        .is_some()
                })
                .collect();

            let trace = trace_prefilter(needle, view.dict, &freq, rows);
            let mut admitted = Vec::new();
            Finder::new().find(view, &trace.analysis, Verify::Superset, None, &mut admitted);
            let mut at = admitted.iter().peekable();
            let sound = matching.iter().all(|row| {
                while at.next_if(|&&a| a < *row).is_some() {}
                at.next_if_eq(&row).is_some()
            });
            assert!(
                sound,
                "{dataset} row group {row_group}: cover misses a match for {needle:?}"
            );

            let needle_b64 = base64(needle);
            let id = uuid(&dataset, row_group, &needle_b64, &algorithm);
            writeln!(
                queries,
                "{id},{dataset},{row_group},{needle_b64},{algorithm},{rows},{},{},{}",
                matching.len() as f64 / rows as f64,
                trace.analysis.expected_candidate_row_fraction(rows),
                admitted.len() as f64 / rows as f64
            )
            .unwrap();

            for edge in &trace.edges {
                let (kind, label) = match &edge.label {
                    TraceLabel::Code(c) => ("code", c.to_string()),
                    TraceLabel::CodeSet(ids) => ("code-set", list(ids.iter().map(|&c| c as u32))),
                    TraceLabel::CodeRange(lo, hi) => {
                        ("code-range", list([*lo as u32, *hi as u32].into_iter()))
                    }
                    TraceLabel::None => ("none", String::new()),
                };
                let cut = if edge.in_cut { "yes" } else { "no" };
                writeln!(graph, "{id},{},{},{kind},{label},{cut}", edge.from, edge.to).unwrap();
            }
        }
        eprintln!(
            "{dataset} {algorithm}: row group {row_group}, {rows} rows, {} needles",
            needles.len()
        );
    }
}
