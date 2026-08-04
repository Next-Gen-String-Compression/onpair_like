//! Glue crate for the C++ `fsst_decode_prefilter` and
//! `dict_fsst_decode_prefilter` candidates (Matcher-based; see
//! candidates/common/matcher). One FSST copy, two entry points.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_fsst_decode_prefilter() -> *const LbCandidate;
    fn lb_candidate_dict_fsst_decode_prefilter() -> *const LbCandidate;
}

/// The standalone FSST-storage decode-then-prefilter candidate.
pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_fsst_decode_prefilter() }
}

/// Its dictionary peer: the same matcher as a DictMatcher child (encoded domain).
pub fn vtable_dict() -> &'static LbCandidate {
    unsafe { &*lb_candidate_dict_fsst_decode_prefilter() }
}
