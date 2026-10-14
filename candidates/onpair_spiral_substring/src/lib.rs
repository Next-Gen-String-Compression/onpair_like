//! Upstream OnPair on the `refactor/substring-search` branch (pinned in
//! Cargo.toml): `ContainsScan::new` plans the probe cover and compiles the
//! walk, `ContainsScan::scan` runs it, so the rows are exact.
//!
//! One strategy, `pf_walk`, `LB_CONTAINS` only, without a selection: the
//! harness ANDs its result with one. Instrumented mode reports planning,
//! `ContainsScan::new`, as `setup_ns` and the scan as `scan_ns`, each the
//! fastest of five passes, as `onpair_spiral` does.
//!
//! The branch's `Config` has no prune factor, so a build here keeps every token
//! training found. Offsets are u32, capping a chunk's payload at 4 GiB.

use core::ffi::{c_char, c_void};
use std::ffi::CStr;
use std::hint::black_box;
use std::time::Instant;

use lb_abi::*;
use onpair::search::index::{build_token_frequency_index, TokenFrequencyIndex};
use onpair::search::ContainsScan;
use onpair::{Column, Config, MaxDictBits, Threshold, Token};

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
    let raw: RawConfig = serde_json::from_str(json).map_err(|e| format!("invalid config: {e}"))?;
    let max_dict_bits = MaxDictBits::new(raw.bits)
        .map_err(|_| "\"bits\" must be an integer in [9, 16]".to_string())?;
    let threshold = Threshold::new(raw.threshold)
        .map_err(|_| "\"threshold\" must be in (0.0, 1.0]".to_string())?;
    Ok(Config {
        max_dict_bits,
        threshold,
        seed: Some(raw.seed),
    })
}

struct Handle {
    col: Column<u32>,
    frequencies: TokenFrequencyIndex,
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
    // SAFETY: the harness keeps the view's pointers valid for the build call.
    let offsets = unsafe { v.offsets_slice() };
    if offsets[n] > u32::MAX as u64 {
        return Err(format!(
            "chunk payload {} bytes exceeds this candidate's u32 offsets; set a smaller measure.chunk_rows",
            offsets[n]
        ));
    }
    // SAFETY: as above; payload() derives its length from the same offsets.
    let bytes = unsafe { v.payload() };
    let offsets32: Vec<u32> = offsets.iter().map(|&o| o as u32).collect();
    let col = Column::<u32>::compress(bytes, &offsets32, cfg).map_err(|e| format!("compress failed: {e}"))?;
    let frequencies = build_token_frequency_index(&col.codes, col.dict.num_tokens())
        .map_err(|e| format!("frequency index failed: {e}"))?;
    Ok(Handle {
        col,
        frequencies,
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

unsafe extern "C" fn footprint(this: *mut c_void, out: *mut LbFootprintComponent, capacity: u32) -> u32 {
    let h = &*(this as *const Handle);
    let dict = &h.col.dict;
    let components = [
        LbFootprintComponent::new("codes", (h.col.codes.len() * core::mem::size_of::<Token>()) as u64),
        LbFootprintComponent::new("row_offsets", (h.col.row_offsets.len() * core::mem::size_of::<u32>()) as u64),
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

/// Fastest of five passes of `f`, in nanoseconds.
fn best_ns(f: &mut dyn FnMut()) -> u64 {
    (0..5)
        .map(|_| {
            let started = Instant::now();
            f();
            started.elapsed().as_nanos() as u64
        })
        .min()
        .unwrap()
}

unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    stats: *mut LbRunStats,
) -> i32 {
    let h = &*(this as *const Handle);
    let q = &*query;
    if strategy_index != 0 {
        return 10;
    }
    if q.op != LB_CONTAINS {
        return 12;
    }
    if q.needle_count < 1 {
        return 14;
    }
    let nd = &*q.needles;
    let needle = core::slice::from_raw_parts(nd.ptr, nd.len as usize);
    if needle.len() > ContainsScan::MAX_PATTERN_LEN {
        return 11;
    }
    let rows = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let view = h.col.view();
        let scan = ContainsScan::new(needle, view.dict, &h.frequencies)?;
        let mut rows = Vec::new();
        scan.scan(view.codes, view.row_offsets, view.dict, &mut rows);
        if !stats.is_null() && !needle.is_empty() {
            (*stats).setup_ns = best_ns(&mut || {
                black_box(ContainsScan::new(needle, view.dict, &h.frequencies).ok());
            });
            (*stats).scan_ns = best_ns(&mut || {
                let mut again = Vec::new();
                scan.scan(view.codes, view.row_offsets, view.dict, &mut again);
                black_box(again);
            });
        }
        Ok::<_, onpair::search::ContainsError>(rows)
    })) {
        Ok(Ok(rows)) => rows,
        Ok(Err(_)) | Err(_) => return 13,
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

static STRATEGIES: [LbStrategy; 1] = [LbStrategy {
    name: c"pf_walk".as_ptr(),
    supported_ops: op_bit(LB_CONTAINS),
    flags: 0,
}];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"onpair_spiral_substring".as_ptr(),
    version: c"0.1.0+940f77c".as_ptr(),
    // Baseline NEON on aarch64, SSE2/AVX2/AVX-512 dispatch on x86_64.
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 1,
    build: Some(build),
    footprint: Some(footprint),
    run: Some(run),
    view: None,
    decode: None,
    destroy: Some(destroy),
    decode_selected: None,
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
