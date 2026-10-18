//! Glue crate for the C++ `simd_cut_fsst_debug` candidate: `simd_cut_fsst` with the
//! "sorted" and "uniform" switches, over its own copy of our FSST fork.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_simd_cut_fsst_debug() -> *const LbCandidate;
}

/// The compressed-domain FSST prefilter with its two switches (exact).
pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_simd_cut_fsst_debug() }
}
