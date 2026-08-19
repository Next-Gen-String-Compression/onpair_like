//! Census of the probe-cover shapes the clickbench-url-1m contains suite
//! actually produces: for every needle, compress the column once, analyze the
//! prefilter, and print (points, ranges, covered_fraction, candidate rows).
//!
//! Usage: prefilter-census <payload.bin> <offsets.u32> <queries.jsonl> [dump_dir]

use base64::Engine;
use onpair::search::{analyze_prefilter, build_token_frequency_index, prefilter_candidates};
use onpair::{Column, DEFAULT_CONFIG};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let payload = std::fs::read(&args[1]).unwrap();
    let offsets_raw = std::fs::read(&args[2]).unwrap();
    let offsets: Vec<u32> = offsets_raw
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let queries = std::fs::read_to_string(&args[3]).unwrap();

    // Match the benchmark candidate's defaults: bits=16, threshold=0.15, seed=42
    // (DEFAULT_CONFIG uses bits=12, which produces a different dictionary).
    let mut cfg = DEFAULT_CONFIG;
    cfg.max_dict_bits = onpair::MaxDictBits::new(16).unwrap();
    cfg.seed = Some(42);
    let t = Instant::now();
    let col = Column::<u32>::compress(&payload, &offsets, cfg).unwrap();
    let freqs = build_token_frequency_index(&col.codes, col.dict.num_tokens()).unwrap();
    eprintln!(
        "compressed {} rows / {} codes in {:?}",
        offsets.len() - 1,
        col.codes.len(),
        t.elapsed()
    );

    // Optional 4th arg: directory to dump the compressed column (codes.bin as
    // LE u16, row_offsets.u32 as LE u32) plus points.tsv (token, covered
    // fraction, query id) for every query whose cover is exactly one point —
    // the inputs the point_scan_lab `real` mode consumes.
    let dump_dir = args.get(4).cloned();
    let mut points_tsv = String::new();
    if let Some(dir) = &dump_dir {
        std::fs::create_dir_all(dir).unwrap();
        let mut codes_bytes = Vec::with_capacity(col.codes.len() * 2);
        for &c in &col.codes {
            codes_bytes.extend_from_slice(&c.to_le_bytes());
        }
        std::fs::write(format!("{dir}/codes.bin"), codes_bytes).unwrap();
        let mut off_bytes = Vec::with_capacity(col.row_offsets.len() * 4);
        for &o in &col.row_offsets {
            off_bytes.extend_from_slice(&o.to_le_bytes());
        }
        std::fs::write(format!("{dir}/row_offsets.u32"), off_bytes).unwrap();
    }

    println!("query_id\tneedle_len\tpoints\tranges\tcovered_fraction\tcandidates\tscan_us");
    for line in queries.lines() {
        let q: serde_json::Value = serde_json::from_str(line).unwrap();
        let id = q["id"].as_str().unwrap();
        // A needle is either a plain JSON string (literal bytes) or {"b64": ...}.
        let n0 = &q["needles"][0];
        let needle: Vec<u8> = match n0.as_str() {
            Some(s) => s.as_bytes().to_vec(),
            None => base64::engine::general_purpose::STANDARD
                .decode(n0["b64"].as_str().unwrap())
                .unwrap(),
        };
        let analysis = analyze_prefilter(&needle, col.view().dict, &freqs);
        let cover = analysis.probe_cover();
        let mut out = Vec::new();
        let t = Instant::now();
        let scan = prefilter_candidates(&col.codes, &col.row_offsets, &analysis, &mut out);
        let dt = t.elapsed();
        println!(
            "{}\t{}\t{}\t{}\t{:.6}\t{}\t{:.1}{}",
            id,
            needle.len(),
            cover.points().len(),
            cover.ranges().len(),
            analysis.covered_fraction(),
            out.len(),
            dt.as_secs_f64() * 1e6,
            if scan.is_err() { "\tSCAN_ERR" } else { "" },
        );
        if cover.points().len() == 1 && cover.ranges().is_empty() {
            points_tsv.push_str(&format!(
                "{}\t{:.8}\t{}\n",
                cover.points()[0],
                analysis.covered_fraction(),
                id
            ));
        }
    }
    if let Some(dir) = &dump_dir {
        std::fs::write(format!("{dir}/points.tsv"), points_tsv).unwrap();
    }
}
