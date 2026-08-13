//! Glue crate for the C++ `llm_token_prefilter` and `dict_llm_token_prefilter`
//! candidates: compressed-domain prefiltering over the SGTT LLM-token stream
//! via the mandatory-chain translation (Matcher-based; see
//! candidates/common/llm_token). One token-table copy, two entry points —
//! same pattern as fsst_prefilter.

use lb_abi::LbCandidate;

extern "C" {
    fn lb_candidate_llm_token_prefilter() -> *const LbCandidate;
    fn lb_candidate_dict_llm_token_prefilter() -> *const LbCandidate;
}

/// Standalone compressed-domain token prefilter (chain-scan + per-survivor
/// decode verify).
pub fn vtable() -> &'static LbCandidate {
    unsafe { &*lb_candidate_llm_token_prefilter() }
}

/// Its dictionary peer: the same matcher as a DictMatcher child (encoded
/// domain).
pub fn vtable_dict() -> &'static LbCandidate {
    unsafe { &*lb_candidate_dict_llm_token_prefilter() }
}
