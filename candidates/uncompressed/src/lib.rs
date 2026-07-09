//! The uncompressed baseline candidate (DESIGN.md §11 step 6).
//!
//! Deliberately trivial storage: build() retains the chunk view zero-copy,
//! view() exposes it, footprint = raw size (compression ratio 1.0 by
//! construction). All scan smarts live in scanners — this candidate has no
//! strategies of its own; (uncompressed × memmem × direct) is the brief's
//! reference baseline.

use core::ffi::{c_char, c_void};

use lb_abi::*;

struct Handle {
    view: LbChunkView,
}

unsafe extern "C" fn build(
    view: *const LbChunkView,
    _config_json: *const c_char,
    _err_buf: *mut c_char,
    _err_cap: u64,
) -> *mut c_void {
    // Zero-copy: the harness guarantees the view outlives destroy().
    Box::into_raw(Box::new(Handle { view: *view })) as *mut c_void
}

unsafe extern "C" fn footprint(
    this: *mut c_void,
    out: *mut LbFootprintComponent,
    capacity: u32,
) -> u32 {
    let h = &*(this as *mut Handle);
    let offsets = h.view.offsets_slice();
    let components = [
        LbFootprintComponent::new("payload", offsets[h.view.num_rows as usize]),
        LbFootprintComponent::new("offsets", 8 * (h.view.num_rows + 1)),
    ];
    for (i, c) in components.iter().take(capacity as usize).enumerate() {
        *out.add(i) = *c;
    }
    components.len() as u32
}

unsafe extern "C" fn view(this: *mut c_void, out: *mut LbChunkView) -> i32 {
    *out = (*(this as *mut Handle)).view;
    0
}

unsafe extern "C" fn destroy(this: *mut c_void) {
    drop(Box::from_raw(this as *mut Handle));
}

static VTABLE: LbCandidate = LbCandidate {
    abi_version: LB_ABI_VERSION,
    name: c"uncompressed".as_ptr(),
    version: c"0.1.0".as_ptr(),
    cpu_features: core::ptr::null(),
    strategies: core::ptr::null(),
    strategy_count: 0,
    build: Some(build),
    footprint: Some(footprint),
    run: None,
    view: Some(view),
    decode: None,
    destroy: Some(destroy),
};

pub fn vtable() -> &'static LbCandidate {
    &VTABLE
}
