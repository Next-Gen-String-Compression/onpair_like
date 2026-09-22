//! Fixed-cover matcher experiment. Dataset loading, compression, cover planning,
//! correctness checks and JSON output are outside the scan timing.

use std::fs::OpenOptions;
use std::hint::black_box;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Parser;
use lb_harness::{
    bitmap::Bitmap,
    dataset::PreparedDataset,
    suite::{Result, Suite},
};
use onpair::search::{ContainsScan, index::build_token_frequency_index, matcher_experiment};
use onpair::{Column, Config, Dictionary, MaxDictBits, Threshold};
use serde_json::{Value, json};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dataset: PathBuf,
    #[arg(long)]
    suite: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = 5)]
    rounds: usize,
    #[arg(long, default_value_t = 5)]
    min_millis: u64,
    #[arg(long, default_value_t = 3)]
    min_iters: usize,
    #[arg(long, default_value_t = 16)]
    bits: u8,
    #[arg(long, default_value_t = 42)]
    seed: u64,
    /// Explicit smoke-test limit; normal experiments always use the entire suite.
    #[arg(long)]
    query_limit: Option<usize>,
}

fn write_json(out: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *out, value)?;
    writeln!(out)?;
    Ok(())
}

/// Validate ordered row results against the independently blessed bitmap.
fn check(rows: &[usize], total: u64, count: u64, hash: &str) -> Result<()> {
    if rows.iter().any(|&r| r as u64 >= total) || rows.windows(2).any(|w| w[0] >= w[1]) {
        return Err("scan returned invalid, unsorted or duplicate rows".into());
    }
    let mut bitmap = Bitmap::new(total);
    for &row in rows {
        bitmap.set(row as u64);
    }
    if rows.len() as u64 != count || bitmap.truth_hash() != hash {
        return Err("scan differs from independently verified truth".into());
    }
    Ok(())
}

/// Seeded ordering avoids consistently giving one matcher a colder cache.
fn shuffle<T>(items: &mut [T], state: &mut u64) {
    for i in (1..items.len()).rev() {
        *state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^= z >> 31;
        items.swap(i, z as usize % (i + 1));
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.rounds == 0 || args.min_iters == 0 || args.query_limit == Some(0) {
        return Err("rounds, min-iters and query-limit must be positive".into());
    }
    let dataset = PreparedDataset::load(&args.dataset, true)?;
    let suite = Suite::load_for_run(&args.suite, &dataset)?;
    if suite
        .queries
        .iter()
        .any(|q| q.record.op != "contains" || q.needles.len() != 1)
    {
        return Err("matcher selection requires single-needle CONTAINS queries".into());
    }
    // The file is created exclusively: a repeated run must choose a new output.
    let mut out = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&args.out)?,
    );
    let start = Instant::now();
    let offsets = dataset
        .offsets_u64()
        .iter()
        .map(|&p| u32::try_from(p))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let column = Column::compress(
        dataset.payload(),
        &offsets,
        Config {
            max_dict_bits: MaxDictBits::new(args.bits)?,
            threshold: Threshold::new(0.15)?,
            seed: Some(args.seed),
        },
    )?;
    let frequencies = build_token_frequency_index(&column.codes, column.dict.num_tokens())?;
    let count = args
        .query_limit
        .unwrap_or(suite.queries.len())
        .min(suite.queries.len());
    let setup = json!({"type":"setup", "dataset":dataset.manifest.id,
        "checksum":dataset.manifest.checksum, "rows":dataset.num_rows(),
        "payload_bytes":dataset.manifest.payload_bytes, "codes":column.codes.len(),
        "dictionary_tokens":column.dict.num_tokens(), "isa":matcher_experiment::isa(),
        "suite":suite.manifest.id, "queries":count, "suite_queries":suite.queries.len(),
        "rounds":args.rounds, "min_millis":args.min_millis, "min_iters":args.min_iters,
        "bits":args.bits, "threshold":0.15, "seed":args.seed,
        "compression_and_index_seconds":start.elapsed().as_secs_f64(),
        "scope":"prepared full-column scan; matcher setup, resolution and exact verification included"});
    write_json(&mut out, &setup)?;
    out.flush()?;
    eprintln!(
        "{}: {} queries, {} codes, {}",
        dataset.manifest.id,
        count,
        column.codes.len(),
        matcher_experiment::isa()
    );

    let mut order: Vec<_> = (0..count).collect();
    let mut random = args.seed;
    shuffle(&mut order, &mut random);
    for (at, index) in order.into_iter().enumerate() {
        let q = &suite.queries[index];
        let truth = q.record.truth.as_ref().ok_or("missing truth")?;
        let prepared_at = Instant::now();
        let scan = ContainsScan::new(&q.needles[0], column.dict.as_view(), &frequencies)?;
        let preparation_ns = prepared_at.elapsed().as_nanos();
        let variants = matcher_experiment::variants(&scan, column.codes.len());
        let mut rows = Vec::with_capacity(truth.count as usize);
        // Every variant must match truth before any latency is recorded.
        for variant in &variants {
            rows.clear();
            variant.run(&scan, &column, &mut rows);
            check(&rows, dataset.num_rows(), truth.count, &truth.hash)
                .map_err(|e| format!("{} / {}: {e}", q.record.id, variant.name()))?;
        }
        let mut samples: Vec<Vec<Value>> = vec![Vec::new(); variants.len()];
        let mut variant_order: Vec<_> = (0..variants.len()).collect();
        for _ in 0..args.rounds {
            shuffle(&mut variant_order, &mut random);
            for &v in &variant_order {
                let variant = &variants[v];
                // Warm up the exact execution path immediately before timing it.
                rows.clear();
                variant.run(black_box(&scan), black_box(&column), &mut rows);
                black_box(&rows);
                let started = Instant::now();
                let mut iterations = 0;
                loop {
                    rows.clear();
                    variant.run(black_box(&scan), black_box(&column), &mut rows);
                    black_box(&rows);
                    iterations += 1;
                    if iterations >= args.min_iters
                        && started.elapsed() >= Duration::from_millis(args.min_millis)
                    {
                        break;
                    }
                }
                let elapsed = started.elapsed();
                samples[v].push(
                    json!({"iterations":iterations, "elapsed_ns":elapsed.as_nanos(),
                    "ns_per_scan":elapsed.as_nanos() as f64 / iterations as f64}),
                );
                check(&rows, dataset.num_rows(), truth.count, &truth.hash)?;
            }
        }
        let measurements: Vec<_> = variants.iter().zip(samples).map(|(variant, samples)| {
            let mut times: Vec<_> = samples.iter().map(|s| s["ns_per_scan"].as_f64().unwrap()).collect();
            times.sort_by(f64::total_cmp);
            let mid = times.len()/2;
            let median = if times.len()%2==0 {(times[mid-1]+times[mid])/2.0} else {times[mid]};
            json!({"name":variant.name(), "matcher":variant.matcher(), "selected":variant.selected(),
                "skip_empty_packing":variant.skip_empty_packing(), "median_ns":median, "samples":samples})
        }).collect();
        let cover = scan.probe_cover();
        write_json(
            &mut out,
            &json!({"type":"query", "id":q.record.id, "suite_index":index,
            "needle":q.record.needles[0], "needle_len":q.needles[0].len(), "meta":q.record.meta,
            "matching_rows":truth.count, "row_selectivity":truth.count as f64/dataset.num_rows().max(1) as f64,
            "points":cover.n_points(), "ranges":cover.n_ranges(),
            "cover_points":cover.points(),
            "cover_ranges":cover.ranges().iter().map(|r| [r.begin,r.last]).collect::<Vec<_>>(),
            "covered_frequency":scan.covered_frequency(), "probe_density":scan.covered_fraction(),
            "preparation_ns":preparation_ns, "correct":true, "measurements":measurements}),
        )?;
        out.flush()?;
        if (at + 1) % 50 == 0 || at + 1 == count {
            eprintln!("{}: {}/{} queries", dataset.manifest.id, at + 1, count);
        }
    }
    write_json(&mut out, &json!({"type":"complete", "queries":count}))?;
    out.flush()?;
    Ok(())
}
