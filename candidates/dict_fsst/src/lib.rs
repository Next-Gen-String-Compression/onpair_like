//! Glue crate for the C++ `dict_fsst` candidate (FSST-compressed dictionary,
//! decode-only): build.rs compiles cpp/ via CMake (which fetches cwida/fsst
//! pinned to the same commit as the `fsst` candidate); this file only
//! re-exposes the vtable. Same copy-paste pattern as fsst.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_dict_fsst() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_dict_fsst() }
}
