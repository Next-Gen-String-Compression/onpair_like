//! Glue crate for the C++ `prefilter` scanner — a code-prefilter peer of the
//! memmem baseline over (uncompressed x direct). Same copy-paste pattern as
//! the other C++ scanners.

use lb_abi::LbScanner;

extern "C" {
    fn lb_scanner_prefilter() -> *const LbScanner;
}

pub fn vtable() -> &'static LbScanner {
    unsafe { &*lb_scanner_prefilter() }
}
