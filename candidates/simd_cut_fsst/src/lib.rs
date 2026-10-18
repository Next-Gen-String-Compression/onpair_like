//! Glue crate for the C++ `simd_cut_fsst` and `dict_simd_cut_fsst`
//! candidates: exact contains in the FSST-compressed domain through our FSST
//! fork's (third_party/fsst) min-cut prefilter and alignment walk (Matcher-based;
//! see candidates/common/matcher). One FSST copy, two entry points.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_simd_cut_fsst() -> *const LbCandidate;
    fn lb_candidate_dict_simd_cut_fsst() -> *const LbCandidate;
}

/// Standalone compressed-domain FSST prefilter (planned cover scan + alignment walk, exact).
pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_simd_cut_fsst() }
}

/// Its dictionary peer: the same matcher as a DictMatcher child (encoded domain).
pub fn vtable_dict() -> &'static LbCandidate {
    unsafe { &*lb_candidate_dict_simd_cut_fsst() }
}
