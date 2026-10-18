//! `onpair_spiral`'s `pf_walk` with the code stream stored as onpair's
//! `Split12`, 12 bits a code in runs of 64 high bytes and 64 nibbles, and
//! scanned in place by `Finder::find_split12`. Instrumented phases and
//! `decode()` unpack the whole chunk, outside the timing. Only `bits = 12`
//! builds; offsets are u32, as in `onpair_spiral`.

use core::ffi::{c_char, c_void};
use std::cell::RefCell;
use std::ffi::CStr;
use std::mem::MaybeUninit;

use std::hint::black_box;
use std::time::Instant;

use lb_abi::*;
use onpair::search::index::build_token_frequency_index;
use onpair::search::{
    analyze_prefilter, prefilter_hits, prefilter_walk_hits, Finder, Verify, MAX_PATTERN_LEN,
};
use onpair::{
    decode_into, search::index::TokenFrequencyIndex, Column, ColumnView, Config, MaxDictBits,
    PruneFactor, Split12, Threshold, Token, DECODE_PADDING,
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
    if raw.bits != 12 {
        return Err("\"bits\" must be 12: the codes are stored 12-bit packed".to_string());
    }
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

/// Where an instrumented run's time went, re-run over the whole unpacked
/// chunk outside the timing loop; they do not sum to the harness's median.
#[derive(Default)]
struct Phases {
    setup_ns: u64,
    prefilter_ns: u64,
    verify_ns: u64,
    candidates: Option<u64>,
}

// --------------------------------------------------------------- candidate

struct Handle {
    col: Column<u32>,
    frequencies: TokenFrequencyIndex,
    num_rows: u64,
    /// The scratch for selections, reused by every query on the chunk.
    finder: RefCell<Finder>,
    /// The code stream; `col.codes` is empty.
    packed: Split12,
}

/// The rows containing `needle`. The empty needle is every row, as every
/// library path short-circuits it.
fn packed_rows(h: &Handle, needle: &[u8], sel: Option<&[u32]>) -> Vec<usize> {
    let num_rows = h.num_rows as usize;
    if needle.is_empty() {
        return match sel {
            Some(sel) => sel.iter().map(|&r| r as usize).collect(),
            None => (0..num_rows).collect(),
        };
    }
    let dict = h.col.view().dict;
    let analysis = analyze_prefilter(needle, dict, &h.frequencies, num_rows);
    let mut rows = Vec::new();
    h.finder.borrow_mut().find_split12(
        dict,
        h.packed.view(),
        &h.col.row_offsets,
        &analysis,
        Verify::Walk,
        sel,
        &mut rows,
    );
    rows
}

impl Handle {
    /// The whole chunk unpacked into `codes`, viewed as the column.
    fn view<'a>(&'a self, codes: &'a mut Vec<Token>) -> ColumnView<'a, u32> {
        let packed = self.packed.view();
        codes.clear();
        codes.extend((0..packed.len()).map(|i| packed.get(i)));
        ColumnView {
            dict: self.col.view().dict,
            codes,
            row_offsets: &self.col.row_offsets,
        }
    }
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
    let mut col = Column::<u32>::compress(bytes, &offsets32, cfg)
        .map_err(|e| format!("compress failed: {e}"))?;
    let frequencies = build_token_frequency_index(&col.codes, col.dict.num_tokens())
        .map_err(|e| format!("frequency index failed: {e}"))?;
    let packed = Split12::pack(&std::mem::take(&mut col.codes));
    Ok(Handle {
        col,
        frequencies,
        num_rows: v.num_rows,
        finder: RefCell::new(Finder::new()),
        packed,
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
    // `onpair_spiral`'s split, with the packed stream as the codes.
    let components = [
        LbFootprintComponent::new("codes", h.packed.size_bytes() as u64),
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

// The one strategy, `pf_walk`, on CONTAINS. Instrumented mode (once, outside
// the timing loop) reports planning as `setup_ns`, the probe scan with the walk
// off as `prefilter_ns` with its survivors, and the walks over its hits as
// `verify_ns`. The catch_unwind keeps a library panic from crossing the FFI
// boundary and turns it into an errored cell.
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
    if q.op != LB_CONTAINS {
        return 12;
    }
    if strategy_index != 0 {
        return 10;
    }
    let nd = &*q.needles;
    let needle = core::slice::from_raw_parts(nd.ptr, nd.len as usize);
    // The alignment walk holds one node id per needle offset in a u16.
    if needle.len() > MAX_PATTERN_LEN {
        return 11;
    }
    let mut phases = Phases::default();
    let rows = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let sel = (!q.selection.is_null()).then(|| (*q.selection).rows_slice());
        let rows = packed_rows(h, needle, sel);
        if !stats.is_null() && !needle.is_empty() && sel.is_none() {
            let mut codes = Vec::new();
            let view = h.view(&mut codes);
            phases.setup_ns = best_ns(&mut || {
                black_box(analyze_prefilter(needle, view.dict, &h.frequencies, view.num_rows()));
            });
            let analysis = analyze_prefilter(needle, view.dict, &h.frequencies, view.num_rows());
            let mut survivors = Vec::new();
            phases.prefilter_ns = best_ns(&mut || {
                survivors.clear();
                Finder::new().find(view, &analysis, Verify::Superset, None, &mut survivors);
            });
            phases.candidates = Some(survivors.len() as u64);
            let mut hits = Vec::new();
            prefilter_hits(view.codes, view.row_offsets, &analysis, &survivors, &mut hits);
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
        }
        rows
    })) {
        Ok(rows) => rows,
        Err(_) => return 13,
    };
    if !stats.is_null() {
        if let Some(n) = phases.candidates {
            (*stats).prefilter_candidates = n;
        }
        for (field, ns) in [
            (&mut (*stats).setup_ns, phases.setup_ns),
            (&mut (*stats).prefilter_ns, phases.prefilter_ns),
            (&mut (*stats).verify_ns, phases.verify_ns),
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

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 1] = [LbStrategy {
    name: c"pf_walk".as_ptr(),
    supported_ops: op_bit(LB_CONTAINS),
    flags: LB_STRATEGY_TAKES_SELECTION,
}];

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
    let mut codes = Vec::new();
    let view = h.view(&mut codes);
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
    name: c"onpair_spiral_packed12".as_ptr(),
    version: c"0.1.0+82cc11f".as_ptr(),
    // The prefilter uses baseline NEON on aarch64 and dispatches between
    // SSE2/AVX2/AVX-512 on x86_64; it has no scalar fallback.
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 1,
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
