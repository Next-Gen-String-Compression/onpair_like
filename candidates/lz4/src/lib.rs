//! LZ4 decode-only candidate (DESIGN.md §16): a decompress-then-scan
//! baseline. `build()` LZ4-compresses the chunk payload as one block;
//! `decode()` inflates it back to canonical `(bytes, offsets)` layout, after
//! which the harness composes every registered scanner over it (the `decode`
//! strategy). No `run()`/`view()`: LZ4 has no compressed-domain matching.
//!
//! Offsets are stored uncompressed and counted in the footprint — an honest
//! cost of this representation (dominant on short-row columns), visible on
//! the compression axis.

use core::ffi::{c_char, c_void};

use lb_abi::*;

struct Handle {
    compressed: Vec<u8>,
    offsets: Vec<u64>,
    payload_len: usize,
}

unsafe extern "C" fn build(
    view: *const LbChunkView,
    _config_json: *const c_char,
    _err_buf: *mut c_char,
    _err_cap: u64,
) -> *mut c_void {
    let v = &*view;
    let offsets = v.offsets_slice().to_vec();
    let payload = v.payload();
    let compressed = lz4_flex::block::compress(payload);
    Box::into_raw(Box::new(Handle {
        compressed,
        offsets,
        payload_len: payload.len(),
    })) as *mut c_void
}

unsafe extern "C" fn footprint(
    this: *mut c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32 {
    let h = &*(this as *mut Handle);
    let components = [
        LbFootprintComponent::new("payload_lz4", h.compressed.len() as u64),
        LbFootprintComponent::new("offsets", 8 * h.offsets.len() as u64),
    ];
    for (i, c) in components.iter().take(capacity as usize).enumerate() {
        *out.add(i) = *c;
    }
    components.len() as u32
}

unsafe extern "C" fn decode(
    this: *mut c_void,
    bytes_out: *mut u8,
    bytes_cap: u64,
    offsets_out: *mut u64,
) -> i32 {
    let h = &*(this as *mut Handle);
    let out = core::slice::from_raw_parts_mut(bytes_out, bytes_cap as usize);
    match lz4_flex::block::decompress_into(&h.compressed, out) {
        Ok(n) if n == h.payload_len => {}
        _ => return 1,
    }
    let off = core::slice::from_raw_parts_mut(offsets_out, h.offsets.len());
    off.copy_from_slice(&h.offsets);
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"lz4".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: core::ptr::null(),
    strategy_count: 0,
    build: Some(build),
    footprint: Some(footprint),
    run: None,
    view: None,
    decode: Some(decode),
    destroy: Some(destroy),
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
