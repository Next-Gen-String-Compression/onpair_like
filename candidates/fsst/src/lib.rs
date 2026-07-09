//! Glue crate for the C++ `fsst` candidate: build.rs compiles cpp/ via CMake
//! (which fetches cwida/fsst pinned to a commit); this file only re-exposes the
//! vtable. Same copy-paste pattern as cpp_identity/onpair.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_fsst() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_fsst() }
}
