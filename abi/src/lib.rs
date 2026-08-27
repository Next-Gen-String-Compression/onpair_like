//! Rust mirror of `contract/lb_candidate.h`.
//!
//! The C header is the source of truth; this crate mirrors it field for
//! field and must be updated in lockstep (any divergence is an ABI bug —
//! `LB_ABI_VERSION` must be bumped together). All function pointers are
//! `Option<…>` so NULL entries from C are represented safely; the harness
//! validates required entry points at registration.

use core::ffi::c_char;

pub const LB_ABI_VERSION: u32 = 7;

/// Guaranteed writable headroom past the decoded payload in `decode()`
/// output buffers (mirrors `LB_DECODE_PAD`; SEMANTICS.md rule 8).
pub const LB_DECODE_PAD: usize = 64;

// ------------------------------------------------------------------ ops

pub const LB_PREFIX: u32 = 0;
pub const LB_SUFFIX: u32 = 1;
pub const LB_CONTAINS: u32 = 2;
pub const LB_MULTI_CONTAINS: u32 = 3;
pub const LB_CONTAINS_ANY: u32 = 4;
pub const LB_OP_COUNT: u32 = 5;

#[inline]
pub const fn op_bit(op: u32) -> u32 {
    1u32 << op
}
pub const LB_ALL_OPS: u32 = (1u32 << LB_OP_COUNT) - 1;

pub fn op_name(op: u32) -> &'static str {
    match op {
        LB_PREFIX => "prefix",
        LB_SUFFIX => "suffix",
        LB_CONTAINS => "contains",
        LB_MULTI_CONTAINS => "multi_contains",
        LB_CONTAINS_ANY => "contains_any",
        _ => "unknown",
    }
}

pub fn op_from_name(name: &str) -> Option<u32> {
    Some(match name {
        "prefix" => LB_PREFIX,
        "suffix" => LB_SUFFIX,
        "contains" => LB_CONTAINS,
        "multi_contains" => LB_MULTI_CONTAINS,
        "contains_any" => LB_CONTAINS_ANY,
        _ => return None,
    })
}

// ----------------------------------------------------------------- data

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LbBytes {
    pub ptr: *const u8,
    pub len: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LbChunkView {
    pub bytes: *const u8,
    pub offsets: *const u64,
    pub num_rows: u64,
}

impl LbChunkView {
    /// # Safety
    /// The view's pointers must be valid for the lifetime of the returned
    /// slices (guaranteed by the harness while a candidate handle lives).
    pub unsafe fn offsets_slice<'a>(&self) -> &'a [u64] {
        core::slice::from_raw_parts(self.offsets, self.num_rows as usize + 1)
    }
    /// # Safety
    /// See [`Self::offsets_slice`].
    pub unsafe fn payload<'a>(&self) -> &'a [u8] {
        let off = self.offsets_slice();
        core::slice::from_raw_parts(self.bytes, off[self.num_rows as usize] as usize)
    }
    /// # Safety
    /// See [`Self::offsets_slice`]. `i < num_rows`.
    pub unsafe fn row<'a>(&self, i: usize) -> &'a [u8] {
        let off = self.offsets_slice();
        core::slice::from_raw_parts(
            self.bytes.add(off[i] as usize),
            (off[i + 1] - off[i]) as usize,
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LbQuery {
    pub op: u32,
    pub needles: *const LbBytes,
    pub needle_count: u32,
}

impl LbQuery {
    /// # Safety
    /// The query's needle pointers must outlive the returned slices
    /// (guaranteed by the harness for the duration of a run/scan call).
    pub unsafe fn needles_vec<'a>(&self) -> Vec<&'a [u8]> {
        (0..self.needle_count as usize)
            .map(|i| {
                let n = &*self.needles.add(i);
                core::slice::from_raw_parts(n.ptr, n.len as usize)
            })
            .collect()
    }
}

// ------------------------------------------------------------ reporting

pub const LB_FOOTPRINT_NAME_CAP: usize = 32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LbFootprintComponent {
    pub name: [c_char; LB_FOOTPRINT_NAME_CAP],
    pub bytes: u64,
}

impl LbFootprintComponent {
    pub fn new(name: &str, bytes: u64) -> Self {
        let mut buf = [0 as c_char; LB_FOOTPRINT_NAME_CAP];
        for (i, b) in name.bytes().take(LB_FOOTPRINT_NAME_CAP - 1).enumerate() {
            buf[i] = b as c_char;
        }
        Self { name: buf, bytes }
    }
    pub fn name_str(&self) -> String {
        let bytes: Vec<u8> = self
            .name
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

pub const LB_STAT_UNSET: u64 = u64::MAX;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LbRunStats {
    pub prefilter_candidates: u64,
    pub decode_ns: u64,
    pub prefilter_ns: u64,
    pub verify_ns: u64,
    /// Per-query setup (pattern/automaton compilation) before any row or
    /// token is examined (ABI v3).
    pub setup_ns: u64,
    /// The column values this strategy actually evaluated (ABI v5). UNSET
    /// means one per row; a reduced domain (a dictionary front-end
    /// evaluating once per unique value) declares its size here, and it is
    /// the denominator for the prefilter rates and `ns_per_domain_value`.
    /// A structural counter, not a measurement.
    pub eval_domain: u64,
    /// True matches within `eval_domain`.
    pub eval_domain_matches: u64,
}

impl LbRunStats {
    pub const fn unset() -> Self {
        Self {
            prefilter_candidates: LB_STAT_UNSET,
            decode_ns: LB_STAT_UNSET,
            prefilter_ns: LB_STAT_UNSET,
            verify_ns: LB_STAT_UNSET,
            setup_ns: LB_STAT_UNSET,
            eval_domain: LB_STAT_UNSET,
            eval_domain_matches: LB_STAT_UNSET,
        }
    }
}

// ----------------------------------------------------------- strategies

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LbStrategy {
    pub name: *const c_char,
    pub supported_ops: u32,
}

// ------------------------------------------------------------ candidate

pub type BuildFn = unsafe extern "C" fn(
    view: *const LbChunkView,
    config_json: *const c_char,
    err_buf: *mut c_char,
    err_cap: u64,
) -> *mut core::ffi::c_void;
pub type FootprintFn = unsafe extern "C" fn(
    this: *mut core::ffi::c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32;
pub type RunFn = unsafe extern "C" fn(
    this: *mut core::ffi::c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out_bitmap_words: *mut u64,
    stats_or_null: *mut LbRunStats,
) -> i32;
pub type ViewFn = unsafe extern "C" fn(this: *mut core::ffi::c_void, out: *mut LbChunkView) -> i32;
pub type DecodeFn = unsafe extern "C" fn(
    this: *mut core::ffi::c_void,
    bytes_out: *mut u8,
    bytes_cap: u64,
    offsets_out: *mut u64,
) -> i32;
pub type DestroyFn = unsafe extern "C" fn(this: *mut core::ffi::c_void);

/// Sentinel for an unpopulated [`LbQueryFacts`] field, matching
/// [`LB_STAT_UNSET`].
pub const LB_FACT_UNSET: u64 = u64::MAX;

/// Static, untimed description of what a strategy *would* do for one query
/// (ABI v6).
///
/// These are properties of the (column, needle) pair, not measurements, and
/// the harness collects them outside the measurement loop precisely so that
/// filling them cannot change what is timed — the reason `run` reports no
/// prefilter counters for candidates that call a shipped convenience method
/// (SEMANTICS.md rule 5).
///
/// Their purpose is analysis: throughput on a compressed-domain prefilter is
/// governed by how the needle tokenizes, not by how many rows match, and
/// without these fields a result set cannot explain its own spread.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LbQueryFacts {
    /// Equality probes in the compiled cover.
    pub cover_points: u64,
    /// Range probes in the compiled cover.
    pub cover_ranges: u64,
    /// Code positions the cover marks, and the population that count is over.
    /// Reported as a pair rather than a ratio so chunks compose by addition.
    pub covered_codes: u64,
    pub indexed_codes: u64,
    /// Whether the library's own profitability hint sanctions running the
    /// prefilter for this query: 1 yes, 0 no, `LB_FACT_UNSET` if the strategy
    /// has no such notion.
    pub profitable_hint: u64,
}

impl LbQueryFacts {
    pub const fn unset() -> Self {
        Self {
            cover_points: LB_FACT_UNSET,
            cover_ranges: LB_FACT_UNSET,
            covered_codes: LB_FACT_UNSET,
            indexed_codes: LB_FACT_UNSET,
            profitable_hint: LB_FACT_UNSET,
        }
    }
}

/// Optional per-query fact probe (ABI v6). Called once per cell, outside any
/// measurement, so it may do work `run` must not. Returns 0 on success.
pub type QueryFactsFn = unsafe extern "C" fn(
    this: *mut core::ffi::c_void,
    strategy_index: u32,
    query: *const LbQuery,
    out: *mut LbQueryFacts,
) -> i32;

/// Harness-owned artifact output descriptor used in post-run replay.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LbArtifact {
    pub format: *const c_char,
    pub bytes: *mut u8,
    pub capacity: u64,
    pub len: u64,
}

/// Optional post-run export (ABI v7). The harness calls this on a deterministic
/// replay build after the full measurement matrix. On the first call `bytes`
/// is NULL and `capacity` zero; the candidate sets `format` and required `len`.
/// On the second call it writes into the supplied harness buffer.
pub type ExportArtifactFn =
    unsafe extern "C" fn(this: *mut core::ffi::c_void, out: *mut LbArtifact) -> i32;

#[repr(C)]
pub struct LbCandidate {
    pub abi_version: u32,
    pub name: *const c_char,
    pub version: *const c_char,
    pub cpu_features: *const c_char,
    pub strategies: *const LbStrategy,
    pub strategy_count: u32,
    pub build: Option<BuildFn>,
    pub footprint: Option<FootprintFn>,
    pub run: Option<RunFn>,
    pub view: Option<ViewFn>,
    pub decode: Option<DecodeFn>,
    pub destroy: Option<DestroyFn>,
    /// ABI v6; `None` from candidates with nothing static to declare.
    pub query_facts: Option<QueryFactsFn>,
    /// ABI v7; invoked only in the post-run deterministic replay phase.
    pub export_artifact: Option<ExportArtifactFn>,
}

// -------------------------------------------------------------- scanner

pub type PrepareFn = unsafe extern "C" fn(query: *const LbQuery) -> *mut core::ffi::c_void;
pub type ScanFn = unsafe extern "C" fn(
    prepared: *mut core::ffi::c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    stats_or_null: *mut LbRunStats,
) -> i32;
pub type ReleaseFn = unsafe extern "C" fn(prepared: *mut core::ffi::c_void);
/// Optional per-query capability probe (ABI v4); returns nonzero if the
/// scanner will handle the query, 0 to declare a capability gap.
pub type SupportsQueryFn = unsafe extern "C" fn(query: *const LbQuery) -> i32;

#[repr(C)]
pub struct LbScanner {
    pub abi_version: u32,
    pub name: *const c_char,
    pub version: *const c_char,
    pub cpu_features: *const c_char,
    pub supported_ops: u32,
    pub prepare: Option<PrepareFn>,
    pub scan: Option<ScanFn>,
    pub release: Option<ReleaseFn>,
    pub supports_query: Option<SupportsQueryFn>,
}

// Vtables are immutable static descriptors (pointers to 'static data and
// function pointers); sharing them across threads is sound.
unsafe impl Sync for LbCandidate {}
unsafe impl Sync for LbScanner {}
unsafe impl Sync for LbStrategy {}
unsafe impl Send for LbCandidate {}
unsafe impl Send for LbScanner {}

// -------------------------------------------------------- small helpers

/// Read a NUL-terminated C string field from a vtable; None for NULL.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string with 'static
/// lifetime (vtable fields are required to be static data).
pub unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    core::ffi::CStr::from_ptr(p).to_str().ok()
}

/// Set bit `i` (LSB-first within little-endian u64 words) in a bitmap.
#[inline(always)]
pub fn set_bit(bitmap: &mut [u64], i: usize) {
    bitmap[i >> 6] |= 1u64 << (i & 63);
}

/// Number of u64 words needed for `num_rows` bits.
#[inline]
pub const fn bitmap_words(num_rows: u64) -> usize {
    num_rows.div_ceil(64) as usize
}
