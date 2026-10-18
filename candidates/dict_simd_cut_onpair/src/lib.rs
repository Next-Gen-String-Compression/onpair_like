//! Generic Rust dictionary front-end (the Rust peer of the C++ DictMatcher in
//! candidates/common/matcher/dict_matcher.hpp), plus the candidate it
//! produces: `dict_simd_cut_onpair`.
//!
//! `DictWrapper` deduplicates a chunk into its unique row values + one code per
//! row, builds an *inner* candidate over the UNIQUE values, and at query time
//! runs the inner once per unique value into a small unique-bitmap, then
//! scatters that verdict to the row bitmap through the codes. The inner is any
//! registered candidate reached through its C-ABI vtable — here
//! `simd_cut_onpair` (simd-cut) — so `dict_<inner>` composes with no language barrier.
//!
//! Encoded domain: a `LIKE '%x%'` predicate is evaluated n_unique times, not
//! n_rows times. Footprint = the inner's footprint over the unique dictionary +
//! per-row codes bit-packed to ceil(log2(n_unique)) bits/row.

use core::ffi::{c_char, c_void, CStr};
use std::collections::HashMap;

use lb_abi::*;

struct Handle {
    inner_vt: &'static LbCandidate,
    /// Inner strategy index per exposed strategy, positionally: outer strategy
    /// `i` runs `inner_strats[i]`. A list rather than one value because a
    /// wrapper may expose several of the inner's strategies (`dict+simd-cut` and
    /// `dict+pf_memmem` over the same build).
    inner_strats: Vec<u32>,
    inner_handle: *mut c_void,
    // Unique dictionary + per-row codes. `blob`/`offsets` back the LbChunkView
    // handed to the inner build; they must outlive `inner_handle`, so the Handle
    // owns them and frees them only after the inner is destroyed.
    _blob: Vec<u8>,
    _offsets: Vec<u64>,
    codes: Vec<u32>,
    num_rows: u64,
    num_unique: u32,
}

/// Index of the inner strategy named `want`, or `None` if it has none.
///
/// Exact, with no fallback: an exposed strategy is named after the inner one it
/// runs (`dict+pf_memmem` runs `pf_memmem`), so silently substituting a
/// different one would mislabel every row it produced.
unsafe fn resolve_strategy(vt: &LbCandidate, want: &str) -> Option<u32> {
    if vt.strategies.is_null() || vt.strategy_count == 0 {
        return None;
    }
    let strats = core::slice::from_raw_parts(vt.strategies, vt.strategy_count as usize);
    strats
        .iter()
        .position(|s| CStr::from_ptr(s.name).to_str().unwrap_or("") == want)
        .map(|i| i as u32)
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

/// ceil(log2(n_unique)) bits, minimum 1 — the per-row code width.
fn code_bits(num_unique: u32) -> u64 {
    let mut bits = 1u64;
    while (1u64 << bits) < num_unique as u64 {
        bits += 1;
    }
    bits
}

/// `inner_names[i]` is the inner strategy the wrapper's strategy `i` runs.
unsafe fn dict_build(
    inner_vt: &'static LbCandidate,
    inner_names: &[&str],
    view: *const LbChunkView,
    config: *const c_char,
    err_buf: *mut c_char,
    err_cap: u64,
) -> *mut c_void {
    let mut inner_strats = Vec::with_capacity(inner_names.len());
    for want in inner_names {
        match resolve_strategy(inner_vt, want) {
            Some(i) => inner_strats.push(i),
            None => {
                let inner = CStr::from_ptr(inner_vt.name).to_string_lossy();
                write_err(
                    err_buf,
                    err_cap,
                    &format!("inner candidate {inner} has no strategy {want:?}"),
                );
                return core::ptr::null_mut();
            }
        }
    }
    let v = &*view;
    let offsets = v.offsets_slice();
    let payload = v.payload();
    let n = v.num_rows as usize;

    let mut map: HashMap<&[u8], u32> = HashMap::with_capacity(n);
    let mut blob: Vec<u8> = Vec::new();
    let mut uoffsets: Vec<u64> = vec![0];
    let mut codes: Vec<u32> = Vec::with_capacity(n);
    for i in 0..n {
        let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
        let code = match map.get(row) {
            Some(&c) => c,
            None => {
                let c = (uoffsets.len() - 1) as u32;
                blob.extend_from_slice(row);
                uoffsets.push(blob.len() as u64);
                map.insert(row, c);
                c
            }
        };
        codes.push(code);
    }
    let num_unique = (uoffsets.len() - 1) as u32;

    // Build the inner over the unique values. Moving blob/uoffsets into the
    // Handle afterwards does not relocate their heap buffers, so pointers the
    // inner may have retained stay valid.
    let uview = LbChunkView {
        bytes: blob.as_ptr(),
        offsets: uoffsets.as_ptr(),
        num_rows: num_unique as u64,
    };
    let inner_handle = (inner_vt.build.unwrap())(&uview, config, err_buf, err_cap);
    if inner_handle.is_null() {
        return core::ptr::null_mut();
    }
    Box::into_raw(Box::new(Handle {
        inner_vt,
        inner_strats,
        inner_handle,
        _blob: blob,
        _offsets: uoffsets,
        codes,
        num_rows: n as u64,
        num_unique,
    })) as *mut c_void
}

unsafe extern "C" fn footprint(
    this: *mut c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32 {
    let h = &*(this as *mut Handle);
    // Inner footprint over the unique dictionary (retry if it needs more slots).
    let f = h.inner_vt.footprint.unwrap();
    let mut buf = vec![LbFootprintComponent::new("", 0); 8];
    let mut n = f(h.inner_handle, buf.as_mut_ptr(), buf.len() as u32);
    if n as usize > buf.len() {
        buf = vec![LbFootprintComponent::new("", 0); n as usize];
        n = f(h.inner_handle, buf.as_mut_ptr(), buf.len() as u32);
    }
    let mut comps: Vec<LbFootprintComponent> = buf[..n as usize].to_vec();
    let code_bytes = (h.num_rows * code_bits(h.num_unique) + 7) / 8;
    comps.push(LbFootprintComponent::new("codes", code_bytes));
    for (i, c) in comps.iter().take(capacity as usize).enumerate() {
        *out.add(i) = *c;
    }
    comps.len() as u32
}

unsafe extern "C" fn run(
    this: *mut c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    stats_or_null: *mut LbRunStats,
) -> i32 {
    let h = &*(this as *mut Handle);
    let Some(&inner_strat) = h.inner_strats.get(strategy_index as usize) else {
        return 10;
    };
    // Evaluate the inner over the unique values into a unique-bitmap.
    let mut ubits = vec![0u64; lb_abi::bitmap_words(h.num_unique as u64)];
    let rc = (h.inner_vt.run.unwrap())(
        h.inner_handle,
        inner_strat,
        query,
        ubits.as_mut_ptr(),
        stats_or_null,
    );
    if rc != 0 {
        return rc;
    }
    // Scatter: row i matches iff its unique value matched.
    let words = core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(h.num_rows));
    for (w, codes) in h.codes.chunks(64).enumerate() {
        let mut acc = 0u64;
        for (j, &c) in codes.iter().enumerate() {
            acc |= ((ubits[(c >> 6) as usize] >> (c & 63)) & 1) << j;
        }
        words[w] |= acc;
    }
    // Instrumented mode: declare the reduced evaluation domain (SEMANTICS
    // rule 10). The inner ran once per UNIQUE value, so anything it counted is
    // unique-domain and the harness needs the matching denominator.
    // Unconditional — the domain is a property of the dictionary, not of
    // whether the inner prefilters. simd_cut_onpair's pf_* strategies do, and
    // report `prefilter_candidates` through the same stats pointer we passed
    // down, so their survivor counts are already unique-domain and score
    // against the denominator set here.
    if !stats_or_null.is_null() {
        (*stats_or_null).eval_domain = h.num_unique as u64;
        (*stats_or_null).eval_domain_matches =
            ubits.iter().map(|w| w.count_ones() as u64).sum();
    }
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    let h = Box::from_raw(this as *mut Handle);
    // Destroy the inner BEFORE `h` (and its blob/offsets) is freed.
    (h.inner_vt.destroy.unwrap())(h.inner_handle);
}

// -------------------------------------------------------- dict_simd_cut_onpair

unsafe extern "C" fn build_simd_cut_onpair(
    view: *const LbChunkView,
    config: *const c_char,
    err_buf: *mut c_char,
    err_cap: u64,
) -> *mut c_void {
    dict_build(
        lb_cand_simd_cut_onpair::vtable(),
        &["simd-cut"],
        view,
        config,
        err_buf,
        err_cap,
    )
}

// The inner's one prefiltered strategy, over the unique values. It verifies
// inside its own scan — hence `simd-cut`, not `pf_kmp` — so there is no second
// verifier to expose (the `dict+pf_memmem` line went with `pf_memmem`). The inner's `kmp`
// (no prefilter) is not exposed either; the dictionary already collapses the
// row domain, so an unprefiltered scan over the unique values is a different
// question from the one this wrapper asks.
static STRATS: [LbStrategy; 1] = [LbStrategy {
    name: c"dict+simd-cut".as_ptr(),
    supported_ops: op_bit(LB_CONTAINS),
        flags: 0,
    }];

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"dict_simd_cut_onpair".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: STRATS.as_ptr(),
    strategy_count: 1,
    build: Some(build_simd_cut_onpair),
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
