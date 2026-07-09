//! Glue crate for the C++ `fsst_like` candidate: build.rs compiles cpp/ via
//! CMake (which fetches FSST-LIKE-Matching + calin2110/fsst + fmt, pinned);
//! this file only re-exposes the vtable. Same pattern as onpair/fsst.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_fsst_like() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_fsst_like() }
}
