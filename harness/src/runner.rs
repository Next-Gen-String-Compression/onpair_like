//! The worker: executes one (candidate, config, dataset) triple entirely
//! in-process — build per chunk, strategy × scanner × query cells, gate,
//! two-mode execution, timing — and writes its slice of results
//! (DESIGN.md §7–§10). The parent orchestrates workers; see main.rs.

use std::time::{Duration, Instant};

use lb_abi::{LbChunkView, LbRunStats, LB_STAT_UNSET};

use crate::bitmap::Bitmap;
use crate::chunks::{self, Chunks};
use crate::dataset::PreparedDataset;
use crate::registry::{self, BuiltChunk, Candidate, QueryFfi, Scanner};
use crate::results::{
    CellKey, GateReport, PhaseNs, PrefilterReport, Row, Status, Writer,
};
use crate::spec::{config_hash, LoadedSpec};
use crate::suite::{PreparedQuery, Suite};
use crate::timing::{measure, MeasureCfg};

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Default)]
pub struct WorkerSummary {
    pub cells_ok: u64,
    pub gate_failures: u64,
    pub errors: u64,
}

/// How a cell answers queries: a candidate-implemented strategy, or a
/// harness-composed pipeline over view()/decode() with a scanner.
enum Strat<'a> {
    Candidate { index: u32, name: String, supported_ops: u32 },
    Direct(&'a Scanner),
    Decode(&'a Scanner),
}

impl Strat<'_> {
    fn strategy_name(&self) -> String {
        match self {
            Strat::Candidate { name, .. } => name.clone(),
            Strat::Direct(_) => "direct".into(),
            Strat::Decode(_) => "decode".into(),
        }
    }
    fn scanner_name(&self) -> Option<String> {
        match self {
            Strat::Candidate { .. } => None,
            Strat::Direct(s) | Strat::Decode(s) => Some(s.name.clone()),
        }
    }
    fn supports(&self, op: u32) -> bool {
        let mask = match self {
            Strat::Candidate { supported_ops, .. } => *supported_ops,
            Strat::Direct(s) | Strat::Decode(s) => s.supported_ops,
        };
        mask & lb_abi::op_bit(op) != 0
    }
    /// Per-query capability probe (ABI v4). Candidate strategies are gated by
    /// their op mask only; scanner strategies may additionally decline a
    /// specific query (e.g. needle too long for a bit-parallel word).
    fn supports_query(&self, query: &lb_abi::LbQuery) -> bool {
        match self {
            Strat::Candidate { .. } => true,
            Strat::Direct(s) | Strat::Decode(s) => s.supports_query(query),
        }
    }
}

/// Aggregated instrumented-mode stats across chunks (fields sum over the
/// chunks that reported them; UNSET chunks contribute nothing).
#[derive(Default, Clone, Copy)]
struct StatsAgg {
    prefilter_candidates: Option<u64>,
    decode_ns: Option<u64>,
    prefilter_ns: Option<u64>,
    verify_ns: Option<u64>,
    setup_ns: Option<u64>,
}

impl StatsAgg {
    fn absorb(&mut self, s: &LbRunStats) {
        fn add(acc: &mut Option<u64>, v: u64) {
            if v != LB_STAT_UNSET {
                *acc = Some(acc.unwrap_or(0) + v);
            }
        }
        add(&mut self.prefilter_candidates, s.prefilter_candidates);
        add(&mut self.decode_ns, s.decode_ns);
        add(&mut self.prefilter_ns, s.prefilter_ns);
        add(&mut self.verify_ns, s.verify_ns);
        add(&mut self.setup_ns, s.setup_ns);
    }
}

/// Harness-clock phase splits at the pipeline joints of composed strategies.
#[derive(Default, Clone, Copy)]
struct HarnessPhases {
    decode_ns: u64,
    scan_ns: u64,
}

/// Pre-allocated decode destination. Allocations persist across calls
/// (steady-state realism); contents carry nothing (SEMANTICS.md rule 1).
struct Scratch {
    bytes: Vec<u8>,
    offsets: Vec<u64>,
}

enum Mode<'m> {
    Timing,
    Instrumented(&'m mut StatsAgg, &'m mut HarnessPhases),
}

/// One full sample: [scanner prepare +] the query over all chunks into the
/// global bitmap. Returns the harness-timed duration of exactly that work;
/// bitmap zeroing stays untimed. Module-visible call shape is identical in
/// both modes — a module cannot distinguish verification, instrumented, and
/// timing runs except by its stats pointer. Harness-side phase clocks (the
/// per-chunk decode/scan joints) run only in instrumented mode, so timed
/// samples carry no per-chunk clock overhead.
fn exec_once(
    strat: &Strat,
    handles: &[BuiltChunk],
    chunks: &Chunks,
    qffi: &QueryFfi,
    scratch: &mut Scratch,
    bitmap: &mut Bitmap,
    mut mode: Mode,
) -> Result<Duration> {
    bitmap.zero();
    let words = bitmap.words_mut().as_mut_ptr();
    let mut chunk_stats = LbRunStats::unset();

    let timed = Instant::now();
    match strat {
        Strat::Candidate { index, .. } => {
            for (chunk, handle) in chunks.chunks.iter().zip(handles) {
                let ptr = unsafe { words.add(chunk.bitmap_word_range().start) };
                let stats = match &mut mode {
                    Mode::Timing => None,
                    Mode::Instrumented(..) => {
                        chunk_stats = LbRunStats::unset();
                        Some(&mut chunk_stats)
                    }
                };
                let rc = handle.run(*index, &qffi.query, ptr, stats);
                if rc != 0 {
                    return Err(format!("run() returned {rc}").into());
                }
                if let Mode::Instrumented(agg, _) = &mut mode {
                    agg.absorb(&chunk_stats);
                }
            }
        }
        Strat::Direct(scanner) => {
            let prepared = scanner.prepare(&qffi.query)?;
            for (chunk, handle) in chunks.chunks.iter().zip(handles) {
                let ptr = unsafe { words.add(chunk.bitmap_word_range().start) };
                let view = handle.view()?;
                // Per-chunk phase clocks run only in instrumented mode:
                // timing-mode samples carry no harness clock reads beyond
                // the one outer pair.
                match &mut mode {
                    Mode::Timing => {
                        let rc = prepared.scan(&view, ptr, None);
                        if rc != 0 {
                            return Err(format!("scan() returned {rc}").into());
                        }
                    }
                    Mode::Instrumented(agg, phases) => {
                        chunk_stats = LbRunStats::unset();
                        let t0 = Instant::now();
                        let rc = prepared.scan(&view, ptr, Some(&mut chunk_stats));
                        let scan_ns = t0.elapsed().as_nanos() as u64;
                        if rc != 0 {
                            return Err(format!("scan() returned {rc}").into());
                        }
                        agg.absorb(&chunk_stats);
                        phases.scan_ns += scan_ns;
                    }
                }
            }
        }
        Strat::Decode(scanner) => {
            let prepared = scanner.prepare(&qffi.query)?;
            for (chunk, handle) in chunks.chunks.iter().zip(handles) {
                let ptr = unsafe { words.add(chunk.bitmap_word_range().start) };
                let n = chunk.num_rows as usize;
                // bytes_cap = payload + guaranteed pad (SEMANTICS.md rule 8).
                let cap = chunk.payload_bytes as usize + lb_abi::LB_DECODE_PAD;
                // Same discipline as Direct: the decode/scan joint clocks
                // exist only in instrumented mode.
                match &mut mode {
                    Mode::Timing => {
                        let rc = handle.decode(
                            &mut scratch.bytes[..cap],
                            &mut scratch.offsets[..n + 1],
                        );
                        if rc != 0 {
                            return Err(format!("decode() returned {rc}").into());
                        }
                        let view = LbChunkView {
                            bytes: scratch.bytes.as_ptr(),
                            offsets: scratch.offsets.as_ptr(),
                            num_rows: chunk.num_rows,
                        };
                        let rc = prepared.scan(&view, ptr, None);
                        if rc != 0 {
                            return Err(format!("scan() returned {rc}").into());
                        }
                    }
                    Mode::Instrumented(agg, phases) => {
                        let t0 = Instant::now();
                        let rc = handle.decode(
                            &mut scratch.bytes[..cap],
                            &mut scratch.offsets[..n + 1],
                        );
                        if rc != 0 {
                            return Err(format!("decode() returned {rc}").into());
                        }
                        let t1 = Instant::now();
                        let view = LbChunkView {
                            bytes: scratch.bytes.as_ptr(),
                            offsets: scratch.offsets.as_ptr(),
                            num_rows: chunk.num_rows,
                        };
                        chunk_stats = LbRunStats::unset();
                        let rc = prepared.scan(&view, ptr, Some(&mut chunk_stats));
                        let scan_ns = t1.elapsed().as_nanos() as u64;
                        if rc != 0 {
                            return Err(format!("scan() returned {rc}").into());
                        }
                        agg.absorb(&chunk_stats);
                        phases.decode_ns += (t1 - t0).as_nanos() as u64;
                        phases.scan_ns += scan_ns;
                    }
                }
            }
        }
    }
    let elapsed = timed.elapsed();
    bitmap.clear_padding();
    Ok(elapsed)
}

/// Execute the whole matrix slice owned by one worker.
pub fn run_worker(
    loaded: &LoadedSpec,
    candidate_name: &str,
    config_idx: usize,
    dataset_idx: usize,
    out: &mut Writer,
    fail_fast: bool,
) -> Result<WorkerSummary> {
    let spec = &loaded.spec;
    let measure_cfg = MeasureCfg {
        warmup: spec.measure.warmup,
        min_iters: spec.measure.min_iters,
        min_time: Duration::from_millis(spec.measure.min_millis),
    };
    if !crate::cpu::pin_to_core(spec.measure.pin_core) {
        eprintln!("warn: core pinning not effective on this platform");
    }

    let candidate = registry::find_candidate(candidate_name)?;
    // Defensive re-check; the parent already gates and records unavailability.
    if let Err(missing) = crate::cpu::check_features(candidate.cpu_features.as_deref()) {
        return Err(format!(
            "candidate {candidate_name} requires missing CPU features {missing:?}"
        )
        .into());
    }
    let config = spec
        .candidates
        .iter()
        .find(|c| c.name == candidate_name)
        .ok_or("candidate not in spec")?
        .configs
        .get(config_idx)
        .ok_or("config index out of range")?
        .clone();

    let ds_ref = spec.datasets.get(dataset_idx).ok_or("dataset index out of range")?;
    let ds = PreparedDataset::load(&ds_ref.path, !spec.measure.skip_checksum_verify)?;

    // Suites bound to this dataset (binding verified checksum-strict).
    let mut suites: Vec<Suite> = Vec::new();
    for s in &spec.suites {
        let manifest_id = Suite::load_unblessed(&s.path)?.manifest.dataset.id;
        if manifest_id == ds.manifest.id {
            suites.push(Suite::load_for_run(&s.path, &ds)?);
        }
    }
    if suites.is_empty() {
        return Ok(WorkerSummary::default());
    }

    // Scanners available on this host (parent recorded the gated-off ones).
    let scanners: Vec<Scanner> = spec
        .scanners
        .iter()
        .map(|s| registry::find_scanner(&s.name))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|s| crate::cpu::check_features(s.cpu_features.as_deref()).is_ok())
        .collect();

    let mut summary = WorkerSummary::default();

    for &chunk_rows in &spec.measure.chunk_rows {
        let key = CellKey {
            candidate: candidate.name.clone(),
            candidate_version: candidate.version.clone(),
            config: config.clone(),
            config_hash: config_hash(&config),
            strategy: None,
            scanner: None,
            dataset: ds.manifest.id.clone(),
            dataset_checksum: ds.manifest.checksum.clone(),
            chunk_rows,
        };
        let chunks = chunks::slice(&ds, chunk_rows)?;

        // ---- compression axis: build every chunk, harness-timed ----
        let mut handles: Vec<BuiltChunk> = Vec::with_capacity(chunks.chunks.len());
        let mut build_ns = 0u64;
        let mut build_err = None;
        for chunk in &chunks.chunks {
            let view = chunk.view();
            let t0 = Instant::now();
            match candidate.build_chunk(&view, &config) {
                Ok(h) => {
                    build_ns += t0.elapsed().as_nanos() as u64;
                    handles.push(h);
                }
                Err(e) => {
                    build_err = Some(e.to_string());
                    break;
                }
            }
        }
        if let Some(e) = build_err {
            eprintln!(
                "error: {} (config {}) failed to build chunk_rows={chunk_rows}: {e}",
                candidate.name, config
            );
            out.write(&Row::BuildFailed {
                key: key.clone(),
                error: e,
            })?;
            summary.errors += 1;
            continue;
        }

        let mut components: serde_json::Map<String, serde_json::Value> = Default::default();
        let mut footprint_total = 0u64;
        for h in &handles {
            for (name, bytes) in h.footprint() {
                footprint_total += bytes;
                let e = components.entry(name).or_insert(serde_json::json!(0u64));
                *e = serde_json::json!(e.as_u64().unwrap_or(0) + bytes);
            }
        }
        out.write(&Row::Build {
            key: key.clone(),
            num_chunks: chunks.chunks.len(),
            build_ns,
            footprint_total_bytes: footprint_total,
            footprint_components: components,
            raw_bytes: ds.raw_bytes(),
        })?;

        // ---- strategies over this one build ----
        let mut strats: Vec<Strat> = candidate
            .strategies
            .iter()
            .map(|s| Strat::Candidate {
                index: s.index,
                name: s.name.clone(),
                supported_ops: s.supported_ops,
            })
            .collect();
        if candidate.has_view() {
            strats.extend(scanners.iter().map(Strat::Direct));
        }
        if candidate.has_decode() {
            strats.extend(scanners.iter().map(Strat::Decode));
        }
        // Optional strategy allowlist (spec.strategies): keep only the named
        // strategies. Empty list = keep all (default). Scanner selection is
        // orthogonal (spec.scanners), so `["direct"]` keeps every scanner's
        // direct path while dropping the decode re-runs.
        if !spec.strategies.is_empty() {
            strats.retain(|s| spec.strategies.iter().any(|n| *n == s.strategy_name()));
        }

        let max_payload = chunks.chunks.iter().map(|c| c.payload_bytes).max().unwrap_or(0);
        let max_rows = chunks.chunks.iter().map(|c| c.num_rows).max().unwrap_or(0);
        let mut scratch = Scratch {
            // + LB_DECODE_PAD: the contract's guaranteed headroom for
            // over-copying decoders (SEMANTICS.md rule 8).
            bytes: vec![0u8; max_payload as usize + lb_abi::LB_DECODE_PAD],
            offsets: vec![0u64; max_rows as usize + 1],
        };
        let mut bitmap = Bitmap::new(ds.num_rows());

        for suite in &suites {
            for query in &suite.queries {
                for strat in &strats {
                    let row = run_cell(
                        &key, strat, query, &candidate, &handles, &chunks, &ds,
                        &mut scratch, &mut bitmap, &measure_cfg,
                        spec.measure.raw_samples,
                    )?;
                    let status = match &row {
                        Row::Query { status, .. } => *status,
                        _ => unreachable!(),
                    };
                    out.write(&row)?;
                    match status {
                        Status::Ok | Status::Unsupported => summary.cells_ok += 1,
                        Status::GateFailed => {
                            summary.gate_failures += 1;
                            if fail_fast {
                                return Err("gate failure with --fail-fast".into());
                            }
                        }
                        Status::Error => summary.errors += 1,
                    }
                }
            }
        }
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn run_cell(
    base_key: &CellKey,
    strat: &Strat,
    query: &PreparedQuery,
    candidate: &Candidate,
    handles: &[BuiltChunk],
    chunks: &Chunks,
    ds: &PreparedDataset,
    scratch: &mut Scratch,
    bitmap: &mut Bitmap,
    measure_cfg: &MeasureCfg,
    raw_samples: bool,
) -> Result<Row> {
    let key = CellKey {
        strategy: Some(strat.strategy_name()),
        scanner: strat.scanner_name(),
        ..base_key.clone()
    };
    let mk_row = |status, gate, latency, ns_per_row, gbps, prefilter, error| Row::Query {
        key: key.clone(),
        query_id: query.record.id.clone(),
        op: query.record.op.clone(),
        meta: query.record.meta.clone(),
        derived: query.record.derived.clone(),
        status,
        gate,
        latency,
        ns_per_row,
        gbps_raw: gbps,
        prefilter,
        error,
    };

    let qffi = QueryFfi::new(query.op, &query.needles);
    // Op-mask gate first, then the optional per-query capability probe: a
    // scanner that declares this specific query out of its envelope makes
    // the cell Unsupported, not Error.
    if !strat.supports(query.op) || !strat.supports_query(&qffi.query) {
        return Ok(mk_row(Status::Unsupported, None, None, None, None, None, None));
    }

    let truth = query.record.truth.as_ref().expect("suite loaded for run");

    // ---- 1. verification pass: gate before any number exists ----
    if let Err(e) = exec_once(strat, handles, chunks, &qffi, scratch, bitmap, Mode::Timing) {
        eprintln!("error: {}/{}: {e}", candidate.name, query.record.id);
        return Ok(mk_row(Status::Error, None, None, None, None, None, Some(e.to_string())));
    }
    let count = bitmap.count();
    let hash = bitmap.truth_hash();
    if count != truth.count || hash != truth.hash {
        // Loud, debuggable failure: recompute the oracle live and name the
        // first divergent row (never depends on stored indices).
        let needles: Vec<&[u8]> = query.needles.iter().map(|n| n.as_slice()).collect();
        let oracle_bm = crate::oracle::eval(query.op, &needles, ds.num_rows(), ds.rows());
        let div = bitmap.first_divergence(&oracle_bm);
        let (div_bytes, expected) = match div {
            Some(i) => {
                let raw = ds.row(i);
                let printable = String::from_utf8_lossy(&raw[..raw.len().min(128)]).into_owned();
                (Some(printable), Some(oracle_bm.get(i)))
            }
            None => (None, None),
        };
        eprintln!(
            "GATE FAILURE: candidate={} strategy={}{} query={} — expected count {} hash {}, \
             got count {count} hash {hash}; first divergent row: {div:?} \
             (oracle says match={expected:?}) bytes={div_bytes:?}",
            candidate.name,
            strat.strategy_name(),
            strat.scanner_name().map(|s| format!(" scanner={s}")).unwrap_or_default(),
            query.record.id,
            truth.count,
            truth.hash,
        );
        return Ok(mk_row(
            Status::GateFailed,
            Some(GateReport {
                expected_count: truth.count,
                actual_count: count,
                hash_ok: false,
                first_divergent_row: div,
                first_divergent_row_bytes: div_bytes,
                expected_match: expected,
            }),
            None, None, None, None, None,
        ));
    }
    let gate = GateReport {
        expected_count: truth.count,
        actual_count: count,
        hash_ok: true,
        first_divergent_row: None,
        first_divergent_row_bytes: None,
        expected_match: None,
    };

    // ---- 2. instrumented pass: once, outside the timing loop ----
    let mut agg = StatsAgg::default();
    let mut phases = HarnessPhases::default();
    if let Err(e) = exec_once(
        strat, handles, chunks, &qffi, scratch, bitmap,
        Mode::Instrumented(&mut agg, &mut phases),
    ) {
        return Ok(mk_row(Status::Error, Some(gate), None, None, None, None, Some(e.to_string())));
    }

    // ---- 3. timing loop: uninstrumented samples only ----
    let mut exec_error: Option<String> = None;
    let latency = measure(measure_cfg, raw_samples, || {
        if exec_error.is_some() {
            return Duration::ZERO;
        }
        match exec_once(strat, handles, chunks, &qffi, scratch, bitmap, Mode::Timing) {
            Ok(d) => d,
            Err(e) => {
                exec_error = Some(e.to_string());
                Duration::ZERO
            }
        }
    });
    if let Some(e) = exec_error {
        return Ok(mk_row(Status::Error, Some(gate), None, None, None, None, Some(e)));
    }

    // ---- derived attribution: harness-computed from the gated truth ----
    let median = latency.median_ns;
    let num_rows = ds.num_rows();
    let composed = matches!(strat, Strat::Direct(_) | Strat::Decode(_));
    let prefilter_candidates = agg.prefilter_candidates;
    let prefilter = {
        let prune_rate = prefilter_candidates.map(|c| 1.0 - c as f64 / num_rows as f64);
        let fp_rate = prefilter_candidates
            .filter(|&c| c > 0)
            .map(|c| (c as f64 - truth.count as f64) / c as f64);
        let verify_per_survivor = prefilter_candidates
            .filter(|&c| c > 0)
            .map(|c| median as f64 / c as f64);
        // Decode strategies: the harness owns the joint, so its measurement
        // is authoritative even when a tiny chunk quantizes to 0 ticks.
        let decode_ns = if matches!(strat, Strat::Decode(_)) {
            Some(PhaseNs { ns: phases.decode_ns, origin: "harness" })
        } else {
            agg.decode_ns.map(|ns| PhaseNs { ns, origin: "self_reported" })
        };
        let scan_ns = composed.then_some(PhaseNs { ns: phases.scan_ns, origin: "harness" });
        let prefilter_ns = agg.prefilter_ns.map(|ns| PhaseNs { ns, origin: "self_reported" });
        let verify_ns = agg.verify_ns.map(|ns| PhaseNs { ns, origin: "self_reported" });
        let setup_ns = agg.setup_ns.map(|ns| PhaseNs { ns, origin: "self_reported" });
        if prefilter_candidates.is_none()
            && decode_ns.is_none()
            && scan_ns.is_none()
            && prefilter_ns.is_none()
            && verify_ns.is_none()
            && setup_ns.is_none()
        {
            None
        } else {
            Some(PrefilterReport {
                prefilter_candidates,
                prune_rate,
                false_positive_rate: fp_rate,
                verify_ns_per_survivor: verify_per_survivor,
                decode_ns,
                scan_ns,
                prefilter_ns,
                verify_ns,
                setup_ns,
            })
        }
    };

    Ok(mk_row(
        Status::Ok,
        Some(gate),
        Some(latency),
        Some(median as f64 / num_rows as f64),
        Some(ds.manifest.payload_bytes as f64 / (median as f64 / 1e9) / 1e9),
        prefilter,
        None,
    ))
}
