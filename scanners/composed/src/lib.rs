//! Composed prefilter scanners (DESIGN.md §16): the uncompressed side's
//! prefilter ablation. Each scanner pairs one **prefilter** with a fixed
//! exact **verifier** (full-needle memcmp at the candidate position — the
//! position-level shape memchr uses internally), so the prefilter's
//! contribution is measurable in isolation. This is where §10's prefilter
//! instrumentation is finally exercised on the uncompressed side.
//!
//! Prefilters (all single-needle `contains` only; prefix/suffix are direct
//! compares, multi/any belong to the multi-pattern engines):
//!   * `pf-none`      — no prefilter; naive search every row (the honest
//!                      no-prefilter reference).
//!   * `pf-first-byte`— memchr on needle[0].
//!   * `pf-rare-byte` — memchr on the needle's statically-rarest byte.
//!   * `pf-first-last`— needle[0] via memchr, then require needle[m-1] at the
//!                      window end (the fixed first/last choice).
//!   * `pf-rare-pair` — the two statically-rarest bytes at their offsets
//!                      (isolates the rarity heuristic vs the fixed choice).
//!
//! Instrumented mode reports `prefilter_candidates` = the number of rows the
//! prefilter admitted to full verification (survivors); the harness derives
//! prune rate, false-positive rate, and per-survivor verify cost from it.
//! Timing mode (stats == NULL) is monomorphised to carry no counter
//! (SEMANTICS rule 5): `run::<false>` elides the survivor bookkeeping.
//!
//! Portable (memchr internal dispatch), so `cpu_features` is NULL.

use core::ffi::c_void;

use lb_abi::*;

// --- static byte-frequency heuristic (corpus-agnostic, like memchr's) -----
// Higher = more common; the prefilter scans for the needle's *lowest*-freq
// byte to maximise pruning. Deliberately static: the point of the ablation
// is to see when a fixed rarity guess helps and when it collapses (a needle
// whose "rarest" byte is common in *this* column).
const fn freq(b: u8) -> u16 {
    match b {
        b' ' => 700,
        b'e' => 1200,
        b't' => 900,
        b'a' => 800,
        b'o' => 750,
        b'i' => 700,
        b'n' => 670,
        b's' => 630,
        b'h' => 610,
        b'r' => 600,
        b'd' => 430,
        b'l' => 400,
        b'c' => 280,
        b'u' => 276,
        b'm' => 241,
        b'w' => 236,
        b'f' => 223,
        b'g' => 202,
        b'y' => 197,
        b'p' => 193,
        b'b' => 149,
        b'v' => 98,
        b'k' => 77,
        b'j' => 15,
        b'x' => 15,
        b'q' => 10,
        b'z' => 7,
        b'0'..=b'9' => 120,
        b'/' | b'.' | b'-' | b'_' | b':' => 150,
        b',' | b';' | b'\'' | b'"' => 80,
        b'A'..=b'Z' => 60,
        _ => 5,
    }
}

// ----------------------------------------------------------------- prefilter

#[derive(Clone, Copy)]
enum Pf {
    None,
    FirstByte,
    RareByte,
    FirstLast,
    RarePair,
}

struct Meta {
    needle: Vec<u8>,
    rare_byte: u8,
    rare_off: usize,
    rare2_byte: u8,
    rare2_off: usize,
}

struct Prepared {
    pf: Pf,
    /// Empty needle matches every row (SEMANTICS edge case): shortcut.
    match_all: bool,
    meta: Meta,
}

fn build_meta(needle: &[u8]) -> Meta {
    let m = needle.len();
    let mut rare_off = 0usize;
    for i in 1..m {
        if freq(needle[i]) < freq(needle[rare_off]) {
            rare_off = i;
        }
    }
    let mut rare2_off = rare_off;
    let mut best = u16::MAX;
    for i in 0..m {
        if i != rare_off && freq(needle[i]) < best {
            best = freq(needle[i]);
            rare2_off = i;
        }
    }
    Meta {
        needle: needle.to_vec(),
        rare_byte: if m > 0 { needle[rare_off] } else { 0 },
        rare_off,
        rare2_byte: if m > 0 { needle[rare2_off] } else { 0 },
        rare2_off,
    }
}

fn build(query: *const LbQuery, pf: Pf) -> *mut c_void {
    let q = unsafe { &*query };
    let needles = unsafe { q.needles_vec() };
    if needles.is_empty() {
        return core::ptr::null_mut();
    }
    let needle = needles[0];
    Box::into_raw(Box::new(Prepared {
        pf,
        match_all: needle.is_empty(),
        meta: build_meta(needle),
    })) as *mut c_void
}

// Each returns (matched, survived): matched = the needle occurs; survived =
// the prefilter admitted at least one candidate window (⇒ full verify ran).

#[inline]
fn row_none(row: &[u8], meta: &Meta) -> (bool, bool) {
    let n = &meta.needle;
    let m = n.len();
    if row.len() < m {
        return (false, false);
    }
    let matched = (0..=row.len() - m).any(|p| &row[p..p + m] == n.as_slice());
    (matched, true)
}

#[inline]
fn row_first_byte(row: &[u8], meta: &Meta) -> (bool, bool) {
    let n = &meta.needle;
    let m = n.len();
    if row.len() < m {
        return (false, false);
    }
    let first = n[0];
    let mut i = 0usize;
    let mut survived = false;
    while let Some(rel) = memchr::memchr(first, &row[i..]) {
        let p = i + rel;
        if p + m > row.len() {
            break;
        }
        survived = true;
        if &row[p..p + m] == n.as_slice() {
            return (true, true);
        }
        i = p + 1;
    }
    (false, survived)
}

#[inline]
fn row_rare_byte(row: &[u8], meta: &Meta) -> (bool, bool) {
    let n = &meta.needle;
    let m = n.len();
    if row.len() < m {
        return (false, false);
    }
    let mut i = 0usize;
    let mut survived = false;
    while let Some(rel) = memchr::memchr(meta.rare_byte, &row[i..]) {
        let hit = i + rel;
        i = hit + 1;
        if hit < meta.rare_off {
            continue;
        }
        let p = hit - meta.rare_off;
        if p + m > row.len() {
            continue;
        }
        survived = true;
        if &row[p..p + m] == n.as_slice() {
            return (true, true);
        }
    }
    (false, survived)
}

#[inline]
fn row_first_last(row: &[u8], meta: &Meta) -> (bool, bool) {
    let n = &meta.needle;
    let m = n.len();
    if row.len() < m {
        return (false, false);
    }
    let first = n[0];
    let last = n[m - 1];
    let mut i = 0usize;
    let mut survived = false;
    while let Some(rel) = memchr::memchr(first, &row[i..]) {
        let p = i + rel;
        i = p + 1;
        if p + m > row.len() {
            break;
        }
        if row[p + m - 1] != last {
            continue;
        }
        survived = true;
        if &row[p..p + m] == n.as_slice() {
            return (true, true);
        }
    }
    (false, survived)
}

#[inline]
fn row_rare_pair(row: &[u8], meta: &Meta) -> (bool, bool) {
    let n = &meta.needle;
    let m = n.len();
    if row.len() < m {
        return (false, false);
    }
    let mut i = 0usize;
    let mut survived = false;
    while let Some(rel) = memchr::memchr(meta.rare_byte, &row[i..]) {
        let hit = i + rel;
        i = hit + 1;
        if hit < meta.rare_off {
            continue;
        }
        let p = hit - meta.rare_off;
        if p + m > row.len() {
            continue;
        }
        if row[p + meta.rare2_off] != meta.rare2_byte {
            continue;
        }
        survived = true;
        if &row[p..p + m] == n.as_slice() {
            return (true, true);
        }
    }
    (false, survived)
}

// -------------------------------------------------------------------- run

#[inline]
unsafe fn row_loop<const INSTR: bool, F: Fn(&[u8], &Meta) -> (bool, bool)>(
    v: &LbChunkView,
    words: &mut [u64],
    meta: &Meta,
    f: F,
) -> u64 {
    let offsets = v.offsets_slice();
    let payload = v.payload();
    let mut survivors = 0u64;
    for i in 0..v.num_rows as usize {
        let row = &payload[offsets[i] as usize..offsets[i + 1] as usize];
        let (matched, survived) = f(row, meta);
        if matched {
            set_bit(words, i);
        }
        if INSTR && survived {
            survivors += 1;
        }
    }
    survivors
}

unsafe fn run<const INSTR: bool>(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    stats: *mut LbRunStats,
) -> i32 {
    let p = &*(prepared as *const Prepared);
    let v = &*view;
    let words =
        core::slice::from_raw_parts_mut(out_bitmap_words, lb_abi::bitmap_words(v.num_rows));

    if p.match_all {
        for i in 0..v.num_rows as usize {
            set_bit(words, i);
        }
        if INSTR && !stats.is_null() {
            (*stats).prefilter_candidates = v.num_rows;
        }
        return 0;
    }

    let survivors = match p.pf {
        Pf::None => row_loop::<INSTR, _>(v, words, &p.meta, row_none),
        Pf::FirstByte => row_loop::<INSTR, _>(v, words, &p.meta, row_first_byte),
        Pf::RareByte => row_loop::<INSTR, _>(v, words, &p.meta, row_rare_byte),
        Pf::FirstLast => row_loop::<INSTR, _>(v, words, &p.meta, row_first_last),
        Pf::RarePair => row_loop::<INSTR, _>(v, words, &p.meta, row_rare_pair),
    };
    if INSTR && !stats.is_null() {
        (*stats).prefilter_candidates = survivors;
    }
    0
}

unsafe extern "C" fn scan(
    prepared: *mut c_void,
    view: *const LbChunkView,
    out_bitmap_words: *mut u64,
    stats: *mut LbRunStats,
) -> i32 {
    // Monomorphise the timed path free of the survivor counter.
    if stats.is_null() {
        run::<false>(prepared, view, out_bitmap_words, core::ptr::null_mut())
    } else {
        run::<true>(prepared, view, out_bitmap_words, stats)
    }
}

unsafe extern "C" fn release(prepared: *mut c_void) {
    drop(Box::from_raw(prepared as *mut Prepared));
}

macro_rules! prep_fn {
    ($name:ident, $pf:expr) => {
        unsafe extern "C" fn $name(query: *const LbQuery) -> *mut c_void {
            build(query, $pf)
        }
    };
}
prep_fn!(prep_none, Pf::None);
prep_fn!(prep_first_byte, Pf::FirstByte);
prep_fn!(prep_rare_byte, Pf::RareByte);
prep_fn!(prep_first_last, Pf::FirstLast);
prep_fn!(prep_rare_pair, Pf::RarePair);

macro_rules! vtable {
    ($static:ident, $cname:expr, $prep:ident) => {
        static $static: LbScanner = LbScanner {
            abi_version: LB_ABI_VERSION,
            name: $cname.as_ptr(),
            version: c"0.1.0".as_ptr(),
            cpu_features: core::ptr::null(),
            supported_ops: op_bit(LB_CONTAINS),
            prepare: Some($prep),
            scan: Some(scan),
            release: Some(release),
            supports_query: None,
        };
    };
}
vtable!(VT_NONE, c"pf-none", prep_none);
vtable!(VT_FIRST_BYTE, c"pf-first-byte", prep_first_byte);
vtable!(VT_RARE_BYTE, c"pf-rare-byte", prep_rare_byte);
vtable!(VT_FIRST_LAST, c"pf-first-last", prep_first_last);
vtable!(VT_RARE_PAIR, c"pf-rare-pair", prep_rare_pair);

pub fn vtables() -> [&'static LbScanner; 5] {
    [
        &VT_NONE,
        &VT_FIRST_BYTE,
        &VT_RARE_BYTE,
        &VT_FIRST_LAST,
        &VT_RARE_PAIR,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_all(pf: &[fn(&[u8], &Meta) -> (bool, bool)], needle: &[u8], row: &[u8]) {
        let meta = build_meta(needle);
        let want = row_none(row, &meta).0;
        for f in pf {
            assert_eq!(f(row, &meta).0, want, "needle={needle:?} row={row:?}");
        }
    }

    #[test]
    fn prefilters_agree_with_naive() {
        let fns: Vec<fn(&[u8], &Meta) -> (bool, bool)> =
            vec![row_first_byte, row_rare_byte, row_first_last, row_rare_pair];
        let cases: &[(&[u8], &[u8])] = &[
            (b"abc", b"xxabcxx"),
            (b"abc", b"xxabxx"),
            (b"a", b"bbbab"),
            (b"zz", b"azzb"),
            (b"http", b"see http://x"),
            (b"needle", b"no match here"),
            (b"end", b"the end"),
        ];
        for (n, r) in cases {
            find_all(&fns, n, r);
        }
    }

    #[test]
    fn rarest_byte_is_lowest_freq() {
        let meta = build_meta(b"the");
        // 'h' (610) is rarer than 't'(900)/'e'(1200) in the table.
        assert_eq!(meta.rare_byte, b'h');
    }
}
