//! Glue crate for the C++ `cpp_std_find` scanner — proves the C++ scanner
//! path; the same copy-paste pattern as C++ candidates.

use lb_abi::LbScanner;

extern "C" {
    fn lb_scanner_cpp_std_find() -> *const LbScanner;
}

pub fn vtable() -> &'static LbScanner {
    unsafe { &*lb_scanner_cpp_std_find() }
}
