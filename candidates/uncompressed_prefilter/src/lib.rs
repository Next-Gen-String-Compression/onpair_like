//! Glue crate for the C++ `uncompressed_prefilter` candidate (Matcher-based; see
//! candidates/common/matcher). Same copy-paste pattern as the other C++
//! candidates.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_uncompressed_prefilter() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_uncompressed_prefilter() }
}
