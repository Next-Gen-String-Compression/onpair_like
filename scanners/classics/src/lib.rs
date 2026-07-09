//! Classic single-pattern substring scanners (DESIGN.md §16): BNDM, KMP,
//! Boyer-Moore-Horspool. These are the textbook reference points — no SIMD,
//! no internal prefilter — that anchor the "why the SIMD engines win"
//! narrative and give the needle-length axis a scalar baseline.
//!
//! Scope: single-pattern ops only (`prefix`, `suffix`, `contains`).
//! Prefix/suffix are direct slice compares in every algorithm; the named
//! algorithm governs only the `contains` search. Multi/any are left to the
//! multi-pattern engines (aho-corasick, teddy) and to memmem's loops.
//!
//! Portable scalar code, so `cpu_features` is NULL. BNDM's bit-parallel word
//! is 64 bits wide, so it declines `contains` queries whose needle exceeds
//! 64 bytes via `supports_query` (reported Unsupported, never Error); KMP
//! and BMH have no length limit.

use core::ffi::c_void;

use lb_abi::*;

// --------------------------------------------------------------- searches

/// Backward Nondeterministic DAWG Matching. Needle length must be ≤ 64
/// (the machine word); callers gate longer needles via `supports_query`.
/// The `> 64` branch is a defensive naive fallback and is never hit in a
/// gated run.
fn bndm_find(pat: &[u8], text: &[u8]) -> Option<usize> {
    let m = pat.len();
    if m == 0 {
        return Some(0);
    }
    if m > text.len() {
        return None;
    }
    if m > 64 {
        return naive_find(pat, text);
    }
    let mut b = [0u64; 256];
    for (i, &c) in pat.iter().enumerate() {
        b[c as usize] |= 1u64 << (m - 1 - i);
    }
    let n = text.len();
    let top = 1u64 << (m - 1);
    let mut i = 0usize;
    while i + m <= n {
        let mut j = m;
        let mut last = m;
        let mut d = if m == 64 { u64::MAX } else { (1u64 << m) - 1 };
        while d != 0 {
            d &= b[text[i + j - 1] as usize];
            j -= 1;
            if d & top != 0 {
                if j > 0 {
                    last = j;
                } else {
                    return Some(i);
                }
            }
            d <<= 1;
        }
        i += last;
    }
    None
}

/// Knuth-Morris-Pratt: linear-time, failure-function driven.
fn kmp_find(pat: &[u8], text: &[u8]) -> Option<usize> {
    let m = pat.len();
    if m == 0 {
        return Some(0);
    }
    if m > text.len() {
        return None;
    }
    let mut fail = vec![0usize; m];
    let mut k = 0usize;
    for i in 1..m {
        while k > 0 && pat[i] != pat[k] {
            k = fail[k - 1];
        }
        if pat[i] == pat[k] {
            k += 1;
        }
        fail[i] = k;
    }
    let mut q = 0usize;
    for (i, &c) in text.iter().enumerate() {
        while q > 0 && c != pat[q] {
            q = fail[q - 1];
        }
        if c == pat[q] {
            q += 1;
        }
        if q == m {
            return Some(i + 1 - m);
        }
    }
    None
}

/// Boyer-Moore-Horspool: bad-character shift table, sublinear on average.
fn bmh_find(pat: &[u8], text: &[u8]) -> Option<usize> {
    let m = pat.len();
    if m == 0 {
        return Some(0);
    }
    let n = text.len();
    if m > n {
        return None;
    }
    let mut shift = [m; 256];
    for i in 0..m - 1 {
        shift[pat[i] as usize] = m - 1 - i;
    }
    let mut pos = 0usize;
    while pos + m <= n {
        let mut j = m;
        while j > 0 && text[pos + j - 1] == pat[j - 1] {
            j -= 1;
        }
        if j == 0 {
            return Some(pos);
        }
        pos += shift[text[pos + m - 1] as usize];
    }
    None
}

fn naive_find(pat: &[u8], text: &[u8]) -> Option<usize> {
    let m = pat.len();
    if m == 0 {
        return Some(0);
    }
    if m > text.len() {
        return None;
    }
    (0..=text.len() - m).find(|&i| &text[i..i + m] == pat)
}

// ----------------------------------------------------------------- glue

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

unsafe extern "C" fn release(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut Prepared));
}

/// Monomorphised per algorithm: `find` is a zero-sized fn item, so the
/// `contains` loop inlines the chosen search with no indirect call.
unsafe fn scan_with<F: Fn(&[u8], &[u8]) -> Option<usize>>(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    find: F,
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
            LB_CONTAINS => find(n, row).is_some(),
            _ => false,
        };
        if hit {
            set_bit(words, i);
        }
    }
    0
}

unsafe extern "C" fn scan_bndm(
    p: *mut c_void,
    v: *const LbChunkView,
    o: *mut u64,
    _s: *mut LbRunStats,
) -> i32 {
    scan_with(p, v, o, bndm_find)
}
unsafe extern "C" fn scan_kmp(
    p: *mut c_void,
    v: *const LbChunkView,
    o: *mut u64,
    _s: *mut LbRunStats,
) -> i32 {
    scan_with(p, v, o, kmp_find)
}
unsafe extern "C" fn scan_bmh(
    p: *mut c_void,
    v: *const LbChunkView,
    o: *mut u64,
    _s: *mut LbRunStats,
) -> i32 {
    scan_with(p, v, o, bmh_find)
}

/// BNDM declines `contains` needles that overflow its 64-bit word.
unsafe extern "C" fn bndm_supports(query: *const LbQuery) -> i32 {
    let q = &*query;
    if q.op == LB_CONTAINS && q.needles_vec().iter().any(|n| n.len() > 64) {
        0
    } else {
        1
    }
}

static VT_BNDM: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"bndm".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: OPS,
    prepare: Some(prepare),
    scan: Some(scan_bndm),
    release: Some(release),
    supports_query: Some(bndm_supports),
};

static VT_KMP: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"kmp".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: OPS,
    prepare: Some(prepare),
    scan: Some(scan_kmp),
    release: Some(release),
    supports_query: None,
};

static VT_BMH: LbScanner = LbScanner {
    abi_version: LB_ABI_VERSION,
    name: c"bmh".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    supported_ops: OPS,
    prepare: Some(prepare),
    scan: Some(scan_bmh),
    release: Some(release),
    supports_query: None,
};

pub fn vtables() -> [&'static LbScanner; 3] {
    [&VT_BNDM, &VT_KMP, &VT_BMH]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<fn(&[u8], &[u8]) -> Option<usize>> {
        vec![bndm_find, kmp_find, bmh_find, naive_find]
    }

    #[test]
    fn agree_on_presence() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"", b"abc"),
            (b"a", b"abc"),
            (b"c", b"abc"),
            (b"abc", b"abc"),
            (b"bc", b"abc"),
            (b"x", b"abc"),
            (b"abcd", b"abc"),
            (b"aa", b"aaaaa"),
            (b"ab", b"xabxab"),
            (b"needle", b"haystack with a needle inside"),
        ];
        for (pat, text) in cases {
            let want = naive_find(pat, text).is_some();
            for f in all() {
                assert_eq!(f(pat, text).is_some(), want, "pat={pat:?} text={text:?}");
            }
        }
    }

    #[test]
    fn bndm_long_needle_falls_back() {
        let pat = vec![b'a'; 100];
        let mut text = vec![b'b'; 200];
        text[50..150].copy_from_slice(&pat);
        assert_eq!(bndm_find(&pat, &text), Some(50));
    }
}
