//! Glue crate for the C++ `fsst_like_utn` candidate: build.rs compiles cpp/ via
//! CMake, straight from the vendored `vendor/fsst-like` submodule (the repo's
//! own alexandervanrenen FSST fork included). This file only re-exposes the
//! vtable. Same pattern as fsst_like_tum/fsst.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_fsst_like_utn() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_fsst_like_utn() }
}
