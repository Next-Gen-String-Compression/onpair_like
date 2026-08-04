//! Glue crate for the C++ `fsst_prefilter` and `dict_fsst_prefilter`
//! candidates: compressed-domain prefiltering over FSST via the mandatory-chain
//! code translation (Matcher-based; see candidates/common/matcher). One FSST
//! copy, two entry points.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_fsst_prefilter() -> *const LbCandidate;
    fn lb_candidate_dict_fsst_prefilter() -> *const LbCandidate;
}

/// Standalone compressed-domain FSST prefilter (chain-scan + per-survivor decode verify).
pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_fsst_prefilter() }
}

/// Its dictionary peer: the same matcher as a DictMatcher child (encoded domain).
pub fn vtable_dict() -> &'static LbCandidate {
    unsafe { &*lb_candidate_dict_fsst_prefilter() }
}
