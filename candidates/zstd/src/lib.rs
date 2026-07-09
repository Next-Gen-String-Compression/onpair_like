//! Zstandard decode-only candidate (DESIGN.md §16): the second
//! decompress-then-scan baseline, and the compression-ratio anchor. Config
//! `{"level": N}` selects the zstd level (default 3); each level is a
//! distinct result row, sweeping the ratio/decode-cost curve. Like `lz4`,
//! offsets are stored uncompressed and counted in the footprint.

use core::ffi::{c_char, c_void};

use lb_abi::*;

struct Handle {
    compressed: Vec<u8>,
    offsets: Vec<u64>,
    payload_len: usize,
}

fn parse_level(config_json: *const c_char) -> i32 {
    if config_json.is_null() {
        return 3;
    }
    let s = unsafe { core::ffi::CStr::from_ptr(config_json) }
        .to_str()
        .unwrap_or("{}");
    serde_json::from_str::<serde_json::Value>(s)
        .ok()
        .and_then(|v| v.get("level").and_then(|l| l.as_i64()))
        .unwrap_or(3) as i32
}

unsafe extern "C" fn build(
    view: *const LbChunkView,
    config_json: *const c_char,
    err_buf: *mut c_char,
    err_cap: u64,
) -> *mut c_void {
    let v = &*view;
    let offsets = v.offsets_slice().to_vec();
    let payload = v.payload();
    let level = parse_level(config_json);
    match zstd::bulk::compress(payload, level) {
        Ok(compressed) => Box::into_raw(Box::new(Handle {
            compressed,
            offsets,
            payload_len: payload.len(),
        })) as *mut c_void,
        Err(e) => {
            let msg = format!("zstd compress failed: {e}");
            let bytes = msg.as_bytes();
            let n = bytes.len().min(err_cap as usize - 1);
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf as *mut u8, n);
            *(err_buf.add(n)) = 0;
            core::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn footprint(
    this: *mut c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32 {
    let h = &*(this as *mut Handle);
    let components = [
        LbFootprintComponent::new("payload_zstd", h.compressed.len() as u64),
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
    match zstd::bulk::decompress_to_buffer(&h.compressed, out) {
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
    name: c"zstd".as_ptr(),
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
