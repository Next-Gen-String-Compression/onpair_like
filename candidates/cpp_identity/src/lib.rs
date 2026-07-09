//! Glue crate for the C++ `cpp_identity` candidate: build.rs compiles
//! cpp/ via CMake; this file only re-exposes the vtable. This is the
//! copy-paste pattern for every future C++ candidate — the author writes
//! zero Rust beyond it.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_cpp_identity() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_cpp_identity() }
}
