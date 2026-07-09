//! The platform C library substring scanner (DESIGN.md §16): the honest
//! "what libc gives you" datum. Calls `memmem(3)` — the length-aware analog
//! of `strstr` (our rows are not NUL-terminated and `strstr` would need a
//! per-row copy). On Linux that is glibc's routine, which shares its
//! Two-Way core with `strstr` and adds an AVX2 fast path on x86; on macOS it
//! is Apple's libc implementation.
//!
//! `memmem` is a POSIX/BSD extension present on both target platforms
//! (Linux/glibc, macOS). Scope: single-pattern `contains` (via memmem) plus
//! `prefix`/`suffix` as direct compares — the clean single-literal baseline.

use core::ffi::c_void;

use lb_abi::*;

extern "C" {
    /// Locate needle within haystack; returns a pointer into haystack or NULL.
    fn memmem(
        haystack: *const c_void,
        haystacklen: usize,
        needle: *const c_void,
        needlelen: usize,
    ) -> *mut c_void;
}

#[inline]
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true; // empty needle matches every row (SEMANTICS)
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let r = unsafe {
        memmem(
            haystack.as_ptr() as *const c_void,
            haystack.len(),
            needle.as_ptr() as *const c_void,
            needle.len(),
        )
    };
    !r.is_null()
}

const OPS: u32 = op_bit(LB_PREFIX) | op_bit(LB_SUFFIX) | op_bit(LB_CONTAINS);

struct Prepared {
    op: u32,
    needle: Vec<u8>,
}

unsafe extern "C" fn prepare(query: *const LbQuery) -> *mut c_void {
    let q = &*query;
    let needles = q.needles_vec();
    if needles.is_empty() {
        return core::ptr::null_mut();
    }
    Box::into_raw(Box::new(Prepared {
        op: q.op,
        needle: needles[0].to_vec(),
    })) as *mut c_void
}

unsafe extern "C" fn scan(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    _stats: *mut LbRunStats,
) -> i32 {
    let p = &*(prepared as *const Prepared);
    let v = &*view;
    let words =
        core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));
    let offsets = v.offsets_slice();
    let payload = v.payload();
    let n = &p.needle;
    for i in 0..v.num_rows as usize {
        let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
        let hit = match p.op {
            LB_PREFIX => row.len() >= n.len() && &row[..n.len()] == n.as_slice(),
            LB_SUFFIX => row.len() >= n.len() && &row[row.len() - n.len()..] == n.as_slice(),
            LB_CONTAINS => contains(row, n),
            _ => false,
        };
        if hit {
            set_bit(words, i);
        }
    }
    0
}

unsafe extern "C" fn release(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut Prepared));
}

static VTABLE: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"libc-memmem".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: OPS,
    prepare: Some(prepare),
    scan: Some(scan),
    release: Some(release),
    supports_query: None,
};

pub fn vtable() -> &'static LbScanner {
    &VTABLE
}

#[cfg(test)]
mod tests {
    use super::contains;

    #[test]
    fn memmem_matches_reference() {
        assert!(contains(b"hello world", b"world"));
        assert!(contains(b"hello world", b"hello"));
        assert!(!contains(b"hello world", b"xyz"));
        assert!(contains(b"anything", b"")); // empty needle matches all
        assert!(!contains(b"ab", b"abc")); // needle longer than row
        assert!(contains(b"abc", b"abc"));
    }
}
