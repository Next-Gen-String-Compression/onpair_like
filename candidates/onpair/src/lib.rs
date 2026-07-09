//! Glue crate for the C++ `onpair` candidate: build.rs compiles cpp/ via
//! CMake (which fetches OnPair pinned to a commit); this file only
//! re-exposes the vtable.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_onpair() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_onpair() }
}
