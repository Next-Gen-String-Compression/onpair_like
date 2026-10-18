//! Glue crate for the C++ `fsst_prefilter_debug` candidate: `fsst_prefilter` with the
//! "sorted" and "uniform" switches, over its own copy of our FSST fork.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_fsst_prefilter_debug() -> *const LbCandidate;
}

/// The compressed-domain FSST prefilter with its two switches (exact).
pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_fsst_prefilter_debug() }
}
