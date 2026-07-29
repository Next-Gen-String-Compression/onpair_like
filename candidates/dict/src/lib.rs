//! Glue crate for the C++ `dict` candidate (plain dictionary, decode-only):
//! build.rs compiles cpp/ via CMake; this file only re-exposes the vtable.
//! Same copy-paste pattern as cpp_identity/fsst.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_dict() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_dict() }
}
