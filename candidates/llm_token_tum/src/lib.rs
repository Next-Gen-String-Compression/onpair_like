//! Glue crate for the C++ `llm_token_tum` candidate (SGTT LLM-token-table
//! compression, adapted from TUM's token-vldb2026): build.rs compiles cpp/
//! via CMake; this file only re-exposes the vtable. Same copy-paste pattern
//! as cpp_identity/fsst.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_llm_token_tum() -> *const LbCandidate;
}

pub fn vtable() -> &'static LbCandidate {
    // The C++ side returns a pointer to a static vtable.
    unsafe { &*lb_candidate_llm_token_tum() }
}
