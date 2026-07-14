//! SpiralDB's OnPair (github.com/spiraldb/onpair) as a query-axis candidate,
//! wrapping its `feat/search-prefilter` branch (pinned in Cargo.toml). This is
//! the Rust library; the sibling `onpair` candidate links the C++ onpair_cpp
//! and matches via compressed-domain automata. They are distinct storage
//! schemes and distinct roster lines.
//!
//! One build (train + encode) exposed through three `run()` strategies, all
//! answering `LB_CONTAINS` only:
//!
//!   - `pf_kmp`    — `ColumnView::rows_containing_prefiltered`: the SIMD
//!     substring prefilter, then verify each survivor in the compressed domain
//!     with a token-level KMP automaton. The `ContainsTable` caps patterns at
//!     255 bytes; a longer needle errors the cell (return 11).
//!   - `pf_memmem` — `ColumnView::rows_containing_prefiltered_memmem`: the SAME
//!     prefilter, but verify each survivor by decoding its bytes and running
//!     `memchr::memmem`. No pattern-length cap.
//!   - `kmp`       — `ColumnView::rows_containing`: the token-KMP automaton over
//!     EVERY row, no prefilter. The un-prefiltered baseline — structurally the
//!     direct analog of the C++ `onpair` candidate's `compressed` path for
//!     contains, so the two are a head-to-head across the two libraries. Same
//!     255-byte cap as `pf_kmp`.
//!
//! All three are the library's own convenience recipes, called verbatim so the
//! benchmark measures exactly what OnPair ships (buffer sizing included). No
//! `lb_run_stats` are filled: filling `prefilter_candidates` would mean
//! re-implementing the prefilter/verify split here rather than calling the
//! shipped method, so timing and instrumented modes stay identical work
//! (SEMANTICS.md rule 5). The prefilter's resident cost is still attributed on
//! the compression axis, as the `prefilter` footprint component.
//!
//! ABI note: the contract's offsets are u64; OnPair is generic over the offset
//! width and this candidate builds with u32 (the Arrow "binary" layout),
//! capping a chunk's payload at 4 GiB — `build()` rejects a larger chunk with a
//! message asking for a smaller `measure.chunk_rows`, mirroring the `onpair`
//! candidate.

use core::ffi::{c_char, c_void};
use std::ffi::CStr;

use lb_abi::*;
use onpair::{Column, Config, MaxDictBits, Threshold, Token};

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
    let raw: RawConfig =
        serde_json::from_str(json).map_err(|e| format!("invalid config: {e}"))?;
    let max_dict_bits =
        MaxDictBits::new(raw.bits).map_err(|_| "\"bits\" must be an integer in [9, 16]".to_string())?;
    let threshold = Threshold::new(raw.threshold)
        .map_err(|_| "\"threshold\" must be in (0.0, 1.0]".to_string())?;
    Ok(Config {
        max_dict_bits,
        threshold,
        seed: Some(raw.seed),
    })
}

// --------------------------------------------------------------- candidate

struct Handle {
    col: Column<u32>,
    num_rows: u64,
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
    Ok(Handle {
        col,
        num_rows: v.num_rows,
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
    // byte blob plus u32 offsets; `cum_token_freq` is the prefilter's per-token
    // frequency prefix sums — the substring prefilter's resident cost, named so
    // it is attributable (DESIGN.md §7).
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
        LbFootprintComponent::new("prefilter", (h.col.cum_token_freq.len() * 8) as u64),
    ];
    for (i, c) in components.iter().take(capacity as usize).enumerate() {
        *out.add(i) = *c;
    }
    components.len() as u32
}

// Strategy 0 = `pf_kmp`, 1 = `pf_memmem`, 2 = `kmp`; all CONTAINS-only. Timing
// and instrumented modes do identical work (stats is ignored either way). The
// library methods can panic on a genuinely malformed column — impossible for a
// column we just built — but the catch_unwind keeps any panic from crossing the
// FFI boundary and turns it into an errored cell.
unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    _stats: *mut LbRunStats,
) -> i32 {
    let h = &*(this as *const Handle);
    let q = &*query;
    if q.op != LB_CONTAINS {
        return 12; // both strategies declare CONTAINS only
    }
    if q.needle_count < 1 {
        return 14;
    }
    // CONTAINS has arity 1 (harness-validated); the single needle is at [0].
    let nd = &*q.needles;
    let needle = core::slice::from_raw_parts(nd.ptr, nd.len as usize);
    match strategy_index {
        // pf_kmp (0) and kmp (2) both build a token-KMP ContainsTable, capped at 255 B.
        0 | 2 if needle.len() > 255 => return 11,
        0 | 1 | 2 => {}
        _ => return 10,
    }
    let rows = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let view = h.col.view();
        match strategy_index {
            0 => view.rows_containing_prefiltered(needle),
            1 => view.rows_containing_prefiltered_memmem(needle),
            _ => view.rows_containing(needle), // 2 = kmp, no prefilter
        }
    })) {
        Ok(rows) => rows,
        Err(_) => return 13,
    };
    let bm = core::slice::from_raw_parts_mut(out_bitmap_words, bitmap_words(h.num_rows));
    for &r in &rows {
        set_bit(bm, r);
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static STRATEGIES: [LbStrategy; 3] = [
    LbStrategy {
        name: c"pf_kmp".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
    },
    LbStrategy {
        name: c"pf_memmem".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
    },
    LbStrategy {
        name: c"kmp".as_ptr(),
        supported_ops: op_bit(LB_CONTAINS),
    },
];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"onpair_spiral".as_ptr(),
    version: c"0.1.0+39180b1".as_ptr(),
    // Prefilter kernels dispatch on x86 runtime feature detection with a scalar
    // fallback, so no host feature is required.
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 3,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: None,
    destroy: Some(destroy),
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
