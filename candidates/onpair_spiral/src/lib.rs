//! SpiralDB's OnPair (github.com/spiraldb/onpair) as a query-axis candidate,
//! wrapping its `feat/search-prefilter` branch (pinned in Cargo.toml). This is
//! the Rust library; the sibling `onpair` candidate links the C++ onpair_cpp
//! and matches via compressed-domain automata. They are distinct storage
//! schemes and distinct roster lines.
//!
//! One build (train + encode) exposed through four `run()` strategies, all
//! answering `LB_CONTAINS`, and `pf_walk` also `LB_PREFIX` and `LB_SUFFIX`
//! through `AnchoredWalk`, the same walk run once per row from its first or
//! last code. The prefilter has two entry points, chosen by
//! which function is called and nothing else, and the three prefiltered
//! strategies price the difference between them:
//!
//!   - `pf_walk`   — `ColumnView::rows_containing_prefiltered`: the probe scan
//!     with the alignment walk, which checks every hit against the alignment
//!     graph in the compressed domain as it goes. Rows come back exact and
//!     nothing verifies behind it. Patterns up to `MAX_PATTERN_LEN`; a longer
//!     needle errors the cell (return 11).
//!   - `pf_memmem_superset` — `Verify::Superset` (the same probe scan, walk
//!     off, so the rows are a superset) then `BytesVerifier`: decode each
//!     candidate row and `memmem` its bytes. No library recipe pairs these, so
//!     it calls the two primitives directly.
//!   - `pf_kmp_superset`    — the same superset scan, verified in the
//!     compressed domain by a token-KMP `ContainsTable` instead. Its table
//!     caps patterns at 255 bytes.
//!   - `kmp`       — `ColumnView::rows_containing`: the token-KMP automaton over
//!     EVERY row, no prefilter. The un-prefiltered baseline — structurally the
//!     direct analog of the C++ `onpair` candidate's `compressed` path for
//!     contains, so the two are a head-to-head across the two libraries. Same
//!     255-byte cap.
//!
//! `pf_walk` and `kmp` are the library's own convenience recipes, called
//! verbatim so the benchmark measures exactly what OnPair ships (buffer sizing
//! included). In instrumented mode (once, outside the timing loop) every
//! prefiltered strategy runs the superset scan to report what the probes
//! admitted (`prefilter_candidates`, the denominator of the prune and
//! false-positive rates) as `prefilter_ns`. Every strategy then reports what
//! it costs to decide those candidate rows as `verify_ns`: the alignment walks
//! for `pf_walk`, over the hits located beforehand and untimed, and the
//! decode-and-memmem or token-automaton pass for the two superset strategies.
//! All three are a measurement of one pass, not a difference between two, and
//! all three are the fastest of five runs after a warm-up, so a phase shorter
//! than the clock's step or run second into a warm cache does not read as a
//! negative cost. `pf_walk` reports no `scan_ns`: its timed path is the fused
//! scan, so the harness's median is already that number. The prefilter's
//! resident cost is attributed on the compression axis, as the `prefilter`
//! footprint component.
//!
//! ABI note: the contract's offsets are u64; OnPair is generic over the offset
//! width and this candidate builds with u32 (the Arrow "binary" layout),
//! capping a chunk's payload at 4 GiB — `build()` rejects a larger chunk with a
//! message asking for a smaller `measure.chunk_rows`, mirroring the `onpair`
//! candidate.

use core::ffi::{c_char, c_void};
use std::cell::RefCell;
use std::ffi::CStr;
use std::mem::MaybeUninit;

use std::hint::black_box;
use std::time::Instant;

use lb_abi::*;
use onpair::search::index::build_token_frequency_index;
use onpair::search::{
    analyze_prefilter, contains, prefilter_hits, prefilter_walk_hits, Anchor, AnchoredWalk,
    BytesVerifier, ContainsTable, Finder, Verify, MAX_PATTERN_LEN,
};
use onpair::{
    decode_into, search::index::TokenFrequencyIndex, Column, Config, MaxDictBits, PruneFactor,
    Threshold, Token, DECODE_PADDING,
};

// ------------------------------------------------------------------ config

// Flat config, mirroring the `onpair` candidate's accepted keys minus
// `fixed_threshold` (the Rust crate's public `Config` exposes only the dynamic
// threshold). `deny_unknown_fields` turns a typo'd key into a build error
// rather than a silently-ignored default. `seed` defaults to 42 (NOT the
// library's non-deterministic default): a build must be reproducible from its
// recorded config.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default = "default_bits")]
    bits: u8,
    #[serde(default = "default_threshold")]
    threshold: f64,
    #[serde(default = "default_seed")]
    seed: u64,
    /// Post-training prune factor; absent keeps every token training found.
    #[serde(default)]
    prune: Option<f64>,
    /// Drop tokens the training sample matched fewer than this many times.
    #[serde(default)]
    min_hits: Option<u32>,
}

fn default_bits() -> u8 {
    16
}
fn default_threshold() -> f64 {
    0.15
}
fn default_seed() -> u64 {
    42
}

fn parse_config(json: &str) -> Result<Config, String> {
    let json = json.trim();
    let json = if json.is_empty() { "{}" } else { json };
    let raw: RawConfig = serde_json::from_str(json).map_err(|e| format!("invalid config: {e}"))?;
    let max_dict_bits = MaxDictBits::new(raw.bits)
        .map_err(|_| "\"bits\" must be an integer in [9, 16]".to_string())?;
    let threshold = Threshold::new(raw.threshold)
        .map_err(|_| "\"threshold\" must be in (0.0, 1.0]".to_string())?;
    let prune = raw
        .prune
        .map(|f| PruneFactor::new(f).map_err(|_| "\"prune\" must be finite and >= 0".to_string()))
        .transpose()?;
    Ok(Config {
        max_dict_bits,
        threshold,
        seed: Some(raw.seed),
        prune,
        min_hits: raw.min_hits,
    })
}

/// Timed passes per phase; the fastest is the one kept.
const PASSES: usize = 5;

/// A timed region shorter than this cannot be told from the clock's own step,
/// so it is repeated inside the timer until it can be.
const FLOOR_NS: u64 = 1_000;

/// Nanoseconds of the fastest of [`PASSES`] runs of `run`, after one untimed.
///
/// A phase measured once, cold, and read against another phase measured once,
/// warm, reports the order the two ran in as much as the work they did: the
/// second pass over the same codes finds them in cache, with the predictor
/// trained and the output vector already grown. Repeating each phase and
/// keeping the fastest removes that, and the repeat-to-the-floor removes the
/// case where the phase is shorter than the clock can see. Same discipline as
/// onpair's own scan bench.
fn best_ns(run: &mut dyn FnMut()) -> u64 {
    run();
    let mut reps: u64 = 1;
    loop {
        let mut best = u64::MAX;
        for _ in 0..PASSES {
            let started = Instant::now();
            for _ in 0..reps {
                run();
            }
            best = best.min(started.elapsed().as_nanos() as u64);
        }
        if best > FLOOR_NS || reps == 1 << 20 {
            return best / reps;
        }
        reps *= 2;
    }
}

/// Where an instrumented run's time went.
///
/// The timed path calls one library recipe, and calling it verbatim is the
/// point of this candidate — so these come from re-running the same public
/// primitives the recipe uses, outside the timing loop. They describe the same
/// work, not the same call, and they do not sum to the harness's median.
#[derive(Default)]
struct Phases {
    setup_ns: u64,
    prefilter_ns: u64,
    verify_ns: u64,
    scan_ns: u64,
    candidates: Option<u64>,
}

// --------------------------------------------------------------- candidate

struct Handle {
    col: Column<u32>,
    frequencies: TokenFrequencyIndex,
    num_rows: u64,
    /// `pf_walk`'s scratch for selections, reused by every query on the chunk.
    finder: RefCell<Finder>,
}

unsafe fn write_err(err_buf: *mut c_char, err_cap: u64, msg: &str) {
    if err_buf.is_null() || err_cap == 0 {
        return;
    }
    let cap = err_cap as usize;
    let dst = core::slice::from_raw_parts_mut(err_buf as *mut u8, cap);
    let bytes = msg.as_bytes();
    let n = bytes.len().min(cap - 1);
    dst[..n].copy_from_slice(&bytes[..n]);
    dst[n] = 0;
}

fn build_inner(v: &LbChunkView, cfg_json: &str) -> Result<Handle, String> {
    let cfg = parse_config(cfg_json)?;
    let n = v.num_rows as usize;
    // SAFETY: the harness guarantees the view's pointers are valid for the
    // duration of the build call.
    let offsets = unsafe { v.offsets_slice() };
    let payload_len = offsets[n];
    if payload_len > u32::MAX as u64 {
        return Err(format!(
            "chunk payload {payload_len} bytes exceeds this candidate's u32 offsets; \
             set a smaller measure.chunk_rows"
        ));
    }
    // SAFETY: as above; payload() derives its length from the same offsets.
    let bytes = unsafe { v.payload() };
    let offsets32: Vec<u32> = offsets.iter().map(|&o| o as u32).collect();
    let col = Column::<u32>::compress(bytes, &offsets32, cfg)
        .map_err(|e| format!("compress failed: {e}"))?;
    let frequencies = build_token_frequency_index(&col.codes, col.dict.num_tokens())
        .map_err(|e| format!("frequency index failed: {e}"))?;
    Ok(Handle {
        col,
        frequencies,
        num_rows: v.num_rows,
        finder: RefCell::new(Finder::new()),
    })
}

unsafe extern "C" fn build(
    view: *const LbChunkView,
    config_json: *const c_char,
    err_buf: *mut c_char,
    err_cap: u64,
) -> *mut c_void {
    let cfg_json = CStr::from_ptr(config_json).to_str().unwrap_or("");
    match build_inner(&*view, cfg_json) {
        Ok(h) => Box::into_raw(Box::new(h)) as *mut c_void,
        Err(msg) => {
            write_err(err_buf, err_cap, &msg);
            core::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn footprint(
    this: *mut c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32 {
    let h = &*(this as *const Handle);
    let dict = &h.col.dict;
    // Mirrors the `onpair` candidate's split. Codes are stored one u16 per token
    // (this port does not bit-pack the resident stream); the dictionary is a
    // byte blob plus u32 offsets; the frequency index is the prefilter's
    // per-token frequency prefix sums — the substring prefilter's resident cost, named so
    // it is attributable (DESIGN.md §7). The frequency index stores one u32
    // cumulative entry per token plus a sentinel.
    let components = [
        LbFootprintComponent::new(
            "codes",
            (h.col.codes.len() * core::mem::size_of::<Token>()) as u64,
        ),
        LbFootprintComponent::new(
            "row_offsets",
            (h.col.row_offsets.len() * core::mem::size_of::<u32>()) as u64,
        ),
        LbFootprintComponent::new("dict_bytes", dict.logical_len() as u64),
        LbFootprintComponent::new("dict_offsets", (dict.offsets().len() * 4) as u64),
        LbFootprintComponent::new(
            "prefilter",
            ((h.frequencies.num_tokens() + 1) * core::mem::size_of::<u32>()) as u64,
        ),
    ];
    for (i, c) in components.iter().take(capacity as usize).enumerate() {
        *out.add(i) = *c;
    }
    components.len() as u32
}

/// The superset scan plus a verify pass, the pairing no library recipe offers.
///
/// `Verify::Superset` runs the same probe cover as `Verify::Walk` with the
/// alignment walk off, so its rows are a superset and the verifier
/// above it actually runs. `decode` picks the byte-domain verifier
/// (`pf_memmem_superset`) over the token-KMP one (`pf_kmp_superset`).
fn superset_rows(
    view: onpair::ColumnView<'_, u32>,
    needle: &[u8],
    frequencies: &TokenFrequencyIndex,
    decode: bool,
) -> Vec<usize> {
    // The empty needle occurs at offset 0 of every row, which is what the
    // analysis short-circuits to as well.
    if needle.is_empty() {
        return (0..view.num_rows()).collect();
    }
    let analysis = analyze_prefilter(needle, view.dict, frequencies, view.num_rows());
    let mut rows = Vec::new();
    Finder::new().find(view, &analysis, Verify::Superset, None, &mut rows);
    if decode {
        BytesVerifier::new(needle).retain(view, &mut rows);
    } else {
        let table = ContainsTable::new(needle, view.dict);
        rows.retain(|&k| contains(view.row_codes(k), &table));
    }
    rows
}

// Strategy 0 = `pf_walk`, 1 = `pf_memmem_superset`, 2 = `pf_kmp_superset`,
// 3 = `kmp`; `pf_walk` also answers PREFIX and SUFFIX, the rest CONTAINS only. The timed samples pass a null stats pointer
// and run only the strategy's own path; instrumented mode (once, outside the
// timing loop) re-runs the primitives to report the phases and the survivor
// count. The library methods can panic on a genuinely malformed column —
// impossible for a column we just built — but the catch_unwind keeps any panic
// from crossing the FFI boundary and turns it into an errored cell.
unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    stats: *mut LbRunStats,
) -> i32 {
    let h = &*(this as *const Handle);
    let q = &*query;
    if q.needle_count < 1 {
        return 14;
    }
    // CONTAINS, PREFIX and SUFFIX have arity 1 (harness-validated); the single
    // needle is at [0].
    let nd = &*q.needles;
    let needle = core::slice::from_raw_parts(nd.ptr, nd.len as usize);
    if q.op == LB_PREFIX || q.op == LB_SUFFIX {
        return run_prefix_suffix(h, strategy_index, q, needle, out_bitmap_words, stats);
    }
    if q.op != LB_CONTAINS {
        return 12;
    }
    match strategy_index {
        // Every token-KMP ContainsTable caps the pattern at 255 B.
        2 | 3 if needle.len() > 255 => return 11,
        // The alignment walk holds one node id per needle offset in a u16.
        0 | 1 if needle.len() > MAX_PATTERN_LEN => return 11,
        0..=3 => {}
        _ => return 10,
    }
    let instrumented = !stats.is_null();
    let mut phases = Phases::default();
    let rows = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let view = h.col.view();
        let rows = match strategy_index {
            0 if q.selection.is_null() => view.rows_containing_prefiltered(needle, &h.frequencies),
            0 => {
                let analysis = analyze_prefilter(needle, view.dict, &h.frequencies, view.num_rows());
                let mut rows = Vec::new();
                h.finder.borrow_mut().find(
                    view,
                    &analysis,
                    Verify::Walk,
                    Some((*q.selection).rows_slice()),
                    &mut rows,
                );
                rows
            }
            1 => superset_rows(view, needle, &h.frequencies, true),
            2 => superset_rows(view, needle, &h.frequencies, false),
            _ => view.rows_containing(needle), // 3 = kmp, no prefilter
        };
        // The empty needle occurs at offset 0 of every row and every path
        // short-circuits it: no cover to plan, no scan to time.
        // The phases time the scan over every row; a selection scan is not split.
        if instrumented && !needle.is_empty() && q.selection.is_null() {
            if strategy_index == 3 {
                // `kmp`'s two phases: the table it builds, then the walk over
                // every row.
                phases.setup_ns = best_ns(&mut || {
                    black_box(ContainsTable::new(needle, view.dict));
                });
                let table = ContainsTable::new(needle, view.dict);
                let mut matched = 0usize;
                phases.scan_ns = best_ns(&mut || {
                    matched = (0..view.num_rows())
                        .filter(|&row| contains(view.row_codes(row), &table))
                        .count();
                });
                debug_assert_eq!(matched, rows.len());
            } else {
                phases.setup_ns = best_ns(&mut || {
                    black_box(analyze_prefilter(
                        needle,
                        view.dict,
                        &h.frequencies,
                        view.num_rows(),
                    ));
                });
                let analysis =
                    analyze_prefilter(needle, view.dict, &h.frequencies, view.num_rows());

                // The probe scan with the walk off: its survivors are what the
                // cover admits, the denominator of the prune and
                // false-positive rates, and its time is the scan the three
                // verifiers share.
                let mut survivors = Vec::new();
                phases.prefilter_ns = best_ns(&mut || {
                    survivors.clear();
                    Finder::new().find(view, &analysis, Verify::Superset, None, &mut survivors);
                });
                phases.candidates = Some(survivors.len() as u64);

                if strategy_index == 0 {
                    // The walk is this strategy's verify phase, reported in
                    // the same field as the other two and timed the same way:
                    // the hits the kernels would have produced are located
                    // first, untimed, and `verify_ns` is the alignment walks
                    // over them. The fused scan the timed path runs is the
                    // harness's own median, so it is not re-timed here.
                    let mut hits = Vec::new();
                    prefilter_hits(
                        view.codes,
                        view.row_offsets,
                        &analysis,
                        &survivors,
                        &mut hits,
                    );
                    let mut walked = Vec::new();
                    phases.verify_ns = best_ns(&mut || {
                        walked.clear();
                        prefilter_walk_hits(
                            view.codes,
                            view.row_offsets,
                            view.dict,
                            &analysis,
                            &hits,
                            &mut walked,
                        );
                    });
                    debug_assert_eq!(walked.len(), rows.len(), "the walk is not exact");
                } else {
                    // What this strategy's verifier costs over those rows, per
                    // row: the token automaton's fixed table build is charged
                    // to setup, where `kmp`'s is, so `verify_ns / candidates`
                    // is comparable across the two verifiers. The survivors are
                    // counted rather than retained, so the pass repeats without
                    // rebuilding the list it would have consumed.
                    let table = if strategy_index == 2 {
                        phases.setup_ns += best_ns(&mut || {
                            black_box(ContainsTable::new(needle, view.dict));
                        });
                        Some(ContainsTable::new(needle, view.dict))
                    } else {
                        None
                    };
                    let mut verifier = BytesVerifier::new(needle);
                    let mut matched = 0usize;
                    phases.verify_ns = best_ns(&mut || {
                        matched = match &table {
                            Some(t) => survivors
                                .iter()
                                .filter(|&&k| contains(view.row_codes(k), t))
                                .count(),
                            None => survivors
                                .iter()
                                .filter(|&&k| verifier.contains_row(view, k))
                                .count(),
                        };
                    });
                    debug_assert_eq!(matched, rows.len());
                }
            }
        }
        rows
    })) {
        Ok(rows) => rows,
        Err(_) => return 13,
    };
    if instrumented {
        if let Some(n) = phases.candidates {
            (*stats).prefilter_candidates = n;
        }
        for (field, ns) in [
            (&mut (*stats).setup_ns, phases.setup_ns),
            (&mut (*stats).prefilter_ns, phases.prefilter_ns),
            (&mut (*stats).verify_ns, phases.verify_ns),
            (&mut (*stats).scan_ns, phases.scan_ns),
        ] {
            if ns > 0 {
                *field = ns;
            }
        }
    }
    let bm = core::slice::from_raw_parts_mut(out_bitmap_words, bitmap_words(h.num_rows));
    for &r in &rows {
        set_bit(bm, r);
    }
    0
}

/// `pf_walk` on a PREFIX or SUFFIX: no cover to scan, the alignment walk of
/// the pattern's one position runs once per row, from the row's first code or
/// back from its last. Instrumented mode reports building the walk as
/// `setup_ns`.
unsafe fn run_prefix_suffix(
    h: &Handle,
    strategy_index: u32,
    q: &LbQuery,
    needle: &[u8],
    out_bitmap_words: *mut u64,
    stats: *mut LbRunStats,
) -> i32 {
    if strategy_index != 0 {
        return 12;
    }
    if needle.len() > MAX_PATTERN_LEN {
        return 11;
    }
    let anchor = if q.op == LB_PREFIX { Anchor::Start } else { Anchor::End };
    let view = h.col.view();
    let bm = core::slice::from_raw_parts_mut(out_bitmap_words, bitmap_words(h.num_rows));
    let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let walk = AnchoredWalk::new(needle, view.dict, anchor);
        let mut check = |row: usize| {
            if walk.matches(view.row_codes(row), view.dict) {
                set_bit(bm, row);
            }
        };
        if q.selection.is_null() {
            (0..view.num_rows()).for_each(&mut check);
        } else {
            (*q.selection).rows_slice().iter().for_each(|&row| check(row as usize));
        }
        if !stats.is_null() {
            (*stats).setup_ns = best_ns(&mut || {
                black_box(AnchoredWalk::new(needle, view.dict, anchor));
            });
        }
    }));
    if ok.is_err() {
        return 13;
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 4] = [
    LbStrategy {
        name: c"pf_walk".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS) | op_bit(LB_PREFIX) | op_bit(LB_SUFFIX),
        flags: LB_STRATEGY_TAKES_SELECTION,
    },
    LbStrategy {
        name: c"pf_memmem_superset".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
        flags: 0,
    },
    LbStrategy {
        name: c"pf_kmp_superset".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
        flags: 0,
    },
    LbStrategy {
        name: c"kmp".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
        flags: 0,
    },
];

/// The chunk decompressed row by row, one `decode_into` per row, so the
/// canonical offsets come out with the bytes. A row over-copies up to
/// `DECODE_PADDING` bytes past its end, which the next row overwrites and the
/// contract's `LB_DECODE_PAD` covers after the last.
unsafe extern "C" fn decode(
    this: *mut c_void,
    bytes_out: *mut u8,
    bytes_cap: u64,
    offsets_out: *mut u64,
) -> i32 {
    const { assert!(DECODE_PADDING <= LB_DECODE_PAD) };
    let h = &*(this as *const Handle);
    let view = h.col.view();
    let rows = view.num_rows();
    let out = core::slice::from_raw_parts_mut(bytes_out.cast::<MaybeUninit<u8>>(), bytes_cap as usize);
    let offsets = core::slice::from_raw_parts_mut(offsets_out, rows + 1);
    offsets[0] = 0;
    let mut at = 0;
    for r in 0..rows {
        // SAFETY: bytes_cap is the chunk's payload plus LB_DECODE_PAD >= DECODE_PADDING.
        at += decode_into(view.row_codes(r), view.dict, &mut out[at..]);
        offsets[r + 1] = at as u64;
    }
    0
}

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"onpair_spiral".as_ptr(),
    version: c"0.1.0+82cc11f".as_ptr(),
    // The prefilter uses baseline NEON on aarch64 and dispatches between
    // SSE2/AVX2/AVX-512 on x86_64; it has no scalar fallback.
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 4,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: Some(decode),
    destroy: Some(destroy),
    decode_selected: None,
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
