//! FSST (the Rust `fsst-rs` port) as a query-axis candidate: OnPair's substring
//! prefilter planned over an FSST symbol table, verified by decompressing the
//! survivors.
//!
//! Three FSST candidates are already on the roster and this is none of them.
//! `fsst` links the C++ cwida/fsst and decodes before evaluating; `fsst_prefilter`
//! prefilters in the compressed domain with the C++ mandatory-chain matcher;
//! `fsst_decode_prefilter` decodes and then prefilters. This one keeps the FSST
//! symbol table but plans its probe cover the way OnPair does — the minimum cut
//! of the alignment DAG — so the roster line it answers is *what does that
//! planner do with a vocabulary it did not build*.
//!
//! One build, one strategy, `LB_CONTAINS` only:
//!
//!   - `pf_memmem` — plan a cover over the symbol table, run it over the code
//!     stream with `prefilter_candidates`, then decompress each survivor and
//!     `memchr::memmem` it. Named to match `onpair_spiral`'s strategy of the same
//!     shape, so the two are a head-to-head: same planner, same verify, different
//!     dictionary.
//!
//! # How an FSST table becomes something OnPair can plan over
//! `transcode_fsst_to_onpair` sorts the symbols — OnPair's search binary-searches
//! the dictionary, so it has to be ordered — and rewrites the code stream to
//! index the sorted table. Codes stay one byte wide, so the dictionary holds the
//! symbols and nothing else: most single bytes have no token, and the stream
//! keeps FSST's escapes, a marker followed by the literal byte, both riding along
//! as codes. Two consequences the rest of this file is shaped by.
//!
//! The alphabet is incomplete, so a needle byte no symbol starts cannot be
//! tokenized. The planner is told the escape code and answers with it, which is
//! sound: a row holding that byte holds an escape marker.
//!
//! And the column is decodable by neither side as it stands. OnPair's decoder
//! rejects it — the escape markers and the literals behind them are codes past
//! the dictionary — and FSST's own decompressor cannot read it either, because
//! the transcode renumbered every code to index the *sorted* table. So the verify
//! decodes in the dictionary domain: a code is its token's bytes, an escape
//! marker means the byte after it is a literal. Ten lines, and it needs neither
//! the compressor nor a second copy of the stream.
//!
//! # A selectivity caveat worth reading before comparing numbers
//! The literal byte after an escape sits in the code stream as a raw `u8`, so it
//! aliases whatever token id happens to share its value: a probe for token 65
//! also matches an escaped `A`. That can only *add* candidates, never lose a
//! match, so the candidate stays sound — but its `prefilter_candidates` count is
//! inflated relative to a design that kept literals out of the code domain, and
//! that is a property of this encoding rather than of the planner being compared.

use core::ffi::{c_char, c_void};
use std::ffi::CStr;

use fsst::{Compressor, ESCAPE_CODE};
use onpair::DictionaryView;
use lb_abi::*;
use onpair::CompactDictionary;
use onpair::fsst::transcode_fsst_to_onpair;
use onpair::search::index::{TokenFrequencyIndex, build_token_frequency_index};
use onpair::search::{analyze_prefilter, prefilter_candidates};
use onpair::{Dictionary, Token};

/// FSST's escape marker, as a token id. It is not a dictionary token: it names a
/// byte the table could not cover, and the planner routes a parse it cannot
/// tokenize through it.
const ESCAPE: Token = ESCAPE_CODE as Token;

// ------------------------------------------------------------------ config

/// FSST training takes no knobs — the symbol table is whatever the corpus
/// yields — so the only thing to accept is nothing at all. `deny_unknown_fields`
/// turns a typo'd key into a build error rather than a silently-ignored default.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {}

fn parse_config(json: &str) -> Result<(), String> {
    let json = json.trim();
    let json = if json.is_empty() { "{}" } else { json };
    serde_json::from_str::<RawConfig>(json).map_err(|e| format!("invalid config: {e}"))?;
    Ok(())
}

// --------------------------------------------------------------- candidate

struct Handle {
    dict: CompactDictionary,
    /// The transcoded stream, one byte per code, escapes included.
    codes: Vec<u8>,
    row_offsets: Vec<u32>,
    frequencies: TokenFrequencyIndex,
    /// Longest row after decompression, so the verify buffer is sized once.
    widest_row: usize,
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
    parse_config(cfg_json)?;
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
    let rows: Vec<&[u8]> = (0..n)
        .map(|i| &bytes[offsets[i] as usize..offsets[i + 1] as usize])
        .collect();
    let widest_row = rows.iter().map(|row| row.len()).max().unwrap_or(0);

    let compressor = Compressor::train(&rows);
    let compressed = compressor.compress_bulk(&rows);
    // The transcode is one byte in, one byte out, so a row's code count is its
    // compressed byte count and the row layer survives it untouched.
    let mut row_offsets = Vec::with_capacity(n + 1);
    row_offsets.push(0u32);
    let mut end = 0u32;
    for row in &compressed {
        end = u32::try_from(row.len())
            .ok()
            .and_then(|len| end.checked_add(len))
            .ok_or("compressed chunk exceeds this candidate's u32 offsets")?;
        row_offsets.push(end);
    }

    let (dict, codes) = transcode_fsst_to_onpair(&compressor, &compressed.concat())
        .map_err(|e| format!("transcoding the FSST symbol table failed: {e}"))?;
    // Wide enough for the escape marker and for a literal byte behind one, both
    // of which appear in the stream as codes of their own. A code the index does
    // not cover cannot be weighed.
    let code_space = dict.num_tokens().max(ESCAPE as usize + 1);
    let frequencies = build_token_frequency_index(&codes, code_space)
        .map_err(|e| format!("frequency index failed: {e}"))?;

    Ok(Handle {
        dict,
        codes,
        row_offsets,
        frequencies,
        widest_row,
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
    // Codes are one byte each — that is FSST's whole point — against
    // `onpair_spiral`'s two, which is where the compression-axis difference
    // between the two candidates lives. The dictionary is the sorted symbol
    // table as a byte blob plus u32 offsets, and the frequency index is the
    // prefilter's resident cost, named so it is attributable (DESIGN.md §7).
    let components = [
        LbFootprintComponent::new("codes", h.codes.len() as u64),
        LbFootprintComponent::new(
            "row_offsets",
            (h.row_offsets.len() * core::mem::size_of::<u32>()) as u64,
        ),
        LbFootprintComponent::new("dict_bytes", h.dict.logical_len() as u64),
        LbFootprintComponent::new("dict_offsets", (h.dict.offsets().len() * 4) as u64),
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

/// Plan the cover, run it, decompress what survives and search it.
///
/// The empty needle matches every row and has no cover to plan — `analyze_prefilter`
/// asserts on it — so it short-circuits.
fn contains_rows(h: &Handle, needle: &[u8]) -> Result<(Vec<usize>, usize), String> {
    let n = h.num_rows as usize;
    if needle.is_empty() {
        return Ok(((0..n).collect(), n));
    }

    let analysis = analyze_prefilter(needle, h.dict.as_view(), &h.frequencies, Some(ESCAPE));
    let mut survivors = Vec::new();
    prefilter_candidates(&h.codes, &h.row_offsets, &analysis, &mut survivors)
        .map_err(|e| format!("prefilter failed: {e}"))?;
    let candidates = survivors.len();

    // Verify by decoding each survivor: a code is its token's bytes, an escape
    // marker means the byte after it stands for itself. One buffer, reused —
    // a row decodes to exactly the bytes it was built from.
    let dict = h.dict.as_view();
    let finder = memchr::memmem::Finder::new(needle);
    let mut decoded = Vec::with_capacity(h.widest_row);
    let mut rows = Vec::with_capacity(candidates);
    for &row in &survivors {
        let (from, to) = (h.row_offsets[row] as usize, h.row_offsets[row + 1] as usize);
        decoded.clear();
        let mut at = from;
        while at < to {
            let code = h.codes[at];
            if code == ESCAPE_CODE {
                let literal = *h
                    .codes
                    .get(at + 1)
                    .ok_or("code stream ends mid-escape")?;
                decoded.push(literal);
                at += 2;
            } else {
                decoded.extend_from_slice(dict.token(Token::from(code)));
                at += 1;
            }
        }
        if finder.find(&decoded).is_some() {
            rows.push(row);
        }
    }
    Ok((rows, candidates))
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
    if q.op != LB_CONTAINS {
        return 12; // CONTAINS only
    }
    if strategy_index != 0 {
        return 10;
    }
    if q.needle_count < 1 {
        return 14;
    }
    // CONTAINS has arity 1 (harness-validated); the single needle is at [0].
    let nd = &*q.needles;
    let needle = core::slice::from_raw_parts(nd.ptr, nd.len as usize);

    // The prefilter can panic on a genuinely malformed column — impossible for one
    // we just built — and the catch_unwind keeps any panic from crossing the FFI
    // boundary, turning it into an errored cell instead.
    let (rows, candidates) =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| contains_rows(h, needle))) {
            Ok(Ok(pair)) => pair,
            Ok(Err(_)) | Err(_) => return 13,
        };

    // The survivor count comes free with the answer here — unlike `onpair_spiral`,
    // which calls a library recipe and has to re-run the prefilter to report it —
    // so timed and instrumented runs do identical work either way.
    if !stats.is_null() {
        (*stats).prefilter_candidates = candidates as u64;
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
    name: c"pf_memmem".as_ptr(),
    supported_ops: op_bit(LB_CONTAINS),
}];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"fsst_rs".as_ptr(),
    version: c"0.1.0".as_ptr(),
    // The prefilter uses baseline NEON on aarch64 and dispatches between
    // SSE2/AVX2/AVX-512 on x86_64; it has no scalar fallback.
    cpu_features: core::ptr::null(),
    strategies: STRATEGIES.as_ptr(),
    strategy_count: 1,
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
