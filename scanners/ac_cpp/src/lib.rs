//! Glue crate for the C++ `ac-cpp` scanner: the author's full-DFA byte-level
//! Aho-Corasick, benchmarked head-to-head against the `aho_corasick` and
//! `teddy` scanners for the multi-pattern ops.

use lb_abi::LbScanner;

extern "C" {
    fn lb_scanner_ac_cpp() -> *const LbScanner;
}

pub fn vtable() -> &'static LbScanner {
    unsafe { &*lb_scanner_ac_cpp() }
}
