//! Glue crate for the C++ `dict_fsst_like_tum` candidate: the dictionary
//! front-end over the FSST-LIKE compressed-domain matcher (Matcher-based; see
//! candidates/common/matcher). Same copy-paste pattern as the other C++
//! candidates.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_dict_fsst_like_tum() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_dict_fsst_like_tum() }
}
