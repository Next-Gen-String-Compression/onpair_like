//! Glue crate for the C++ `onpair` candidate: build.rs compiles cpp/ via
//! CMake (which fetches OnPair pinned to a commit); this file only
//! re-exposes the vtable(s).

use std::sync::OnceLock;

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_onpair() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_onpair() }
}

// A `'static` home for the derived decode-only vtable. LbCandidate holds raw
// pointers (not Sync); the pointers here are all `'static` (a byte-string name
// and fields copied from the C++ static vtable), so sharing is sound.
struct StaticVt(LbCandidate);
unsafe impl Sync for StaticVt {}

/// A decode-only view of the SAME OnPair codec: it exposes `build`/`decode`/
/// `footprint` but declares no native strategies and no zero-copy view, so the
/// harness composes only the reserved `decode` strategy (decompress-then-eval).
///
/// This makes "OnPair used as a plain block codec" a first-class roster
/// candidate — a peer of `lz4`/`zstd`/`fsst` — distinct from the `onpair`
/// candidate's compressed-domain `compressed` strategy (cf. `fsst` vs
/// `fsst_like`, DESIGN §17.1). Same handle/config semantics as `onpair`.
pub fn vtable_decode() -> &'static LbCandidate {
    static VT: OnceLock<StaticVt> = OnceLock::new();
    let s = VT.get_or_init(|| {
        let base = unsafe { &*lb_candidate_onpair() };
        StaticVt(LbCandidate {
            abi_version: base.abi_version,
            name: b"onpair_decode\0".as_ptr() as *const core::ffi::c_char,
            version: base.version,
            cpu_features: base.cpu_features,
            strategies: core::ptr::null(), // no native (compressed-domain) strategies
            strategy_count: 0,
            build: base.build,
            footprint: base.footprint,
            run: None,  // unused: no native strategies to dispatch
            view: None, // no zero-copy view -> harness won't compose `direct`
            decode: base.decode, // -> harness composes `decode` (decompress + eval)
            destroy: base.destroy,
        })
    });
    &s.0
}
