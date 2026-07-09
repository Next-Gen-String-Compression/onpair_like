//! The StringZilla scanner (DESIGN.md §16): Ash Vardanian's SIMD string
//! library, a modern competitor to memchr's memmem (NEON on aarch64, up to
//! AVX-512 on x86). Its own dynamic dispatch picks the best kernel for the
//! host, so `cpu_features` is NULL (internal dispatch, like memmem — not the
//! forbidden silent scalar fallback).
//!
//! Scope: single-pattern `contains` (via `sz::find`) plus `prefix`/`suffix`
//! as direct compares — the clean single-literal SIMD counterpoint to
//! `memmem` and `libc-memmem`.

use core::ffi::c_void;

use lb_abi::*;

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

#[inline]
fn contains(row: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true; // empty needle matches every row (SEMANTICS)
    }
    stringzilla::sz::find(row, needle).is_some()
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
    name: c"stringzilla".as_ptr(),
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
    fn matches_reference() {
        assert!(contains(b"hello world", b"world"));
        assert!(!contains(b"hello world", b"xyz"));
        assert!(contains(b"anything", b""));
        assert!(!contains(b"ab", b"abc"));
    }
}
