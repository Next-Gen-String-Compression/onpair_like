//! The static registry of candidates and scanners, plus safe wrappers
//! around the C-ABI vtables. Adding a module = its directory + one
//! feature-gated line in `candidates()` / `scanners()` below.

use std::ffi::CString;

use lb_abi::*;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

// ------------------------------------------------------------ collection

pub fn candidates() -> Vec<Candidate> {
    let mut v: Vec<&'static LbCandidate> = Vec::new();
    #[cfg(feature = "cand-uncompressed")]
    v.push(lb_cand_uncompressed::vtable());
    #[cfg(feature = "cand-gate-canary")]
    v.push(lb_cand_gate_canary::vtable());
    #[cfg(feature = "cand-cpp-identity")]
    v.push(lb_cand_cpp_identity::vtable());
    #[cfg(feature = "cand-onpair")]
    {
        v.push(lb_cand_onpair::vtable());
        // OnPair as a plain codec (decompress-then-eval), a decode-only peer of
        // lz4/zstd/fsst — distinct from the compressed-domain `onpair` candidate.
        v.push(lb_cand_onpair::vtable_decode());
    }
    #[cfg(feature = "cand-onpair-spiral")]
    v.push(lb_cand_onpair_spiral::vtable());
    #[cfg(feature = "cand-lz4")]
    v.push(lb_cand_lz4::vtable());
    #[cfg(feature = "cand-zstd")]
    v.push(lb_cand_zstd::vtable());
    #[cfg(feature = "cand-fsst")]
    v.push(lb_cand_fsst::vtable());
    #[cfg(feature = "cand-fsst-like")]
    v.push(lb_cand_fsst_like::vtable());
    #[cfg(feature = "cand-fsst-spiral")]
    v.push(lb_cand_fsst_spiral::vtable());
    v.into_iter()
        .map(|vt| Candidate::validate(vt).expect("registered candidate failed validation"))
        .collect()
}

pub fn scanners() -> Vec<Scanner> {
    let mut v: Vec<&'static LbScanner> = Vec::new();
    #[cfg(feature = "scan-memmem")]
    {
        v.push(lb_scan_memmem::vtable());
        v.push(lb_scan_memmem::vtable_hay());
    }
    #[cfg(feature = "scan-cpp-std-find")]
    v.push(lb_scan_cpp_std_find::vtable());
    #[cfg(feature = "scan-classics")]
    v.extend(lb_scan_classics::vtables());
    #[cfg(feature = "scan-multi")]
    v.extend(lb_scan_multi::vtables());
    #[cfg(feature = "scan-composed")]
    v.extend(lb_scan_composed::vtables());
    #[cfg(feature = "scan-libc")]
    v.push(lb_scan_libc::vtable());
    #[cfg(feature = "scan-stringzilla")]
    v.push(lb_scan_stringzilla::vtable());
    v.into_iter()
        .map(|vt| Scanner::validate(vt).expect("registered scanner failed validation"))
        .collect()
}

pub fn find_candidate(name: &str) -> Result<Candidate> {
    candidates()
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| {
            format!(
                "unknown candidate {name:?}; registered: {:?}",
                candidates().iter().map(|c| c.name.clone()).collect::<Vec<_>>()
            )
            .into()
        })
}

pub fn find_scanner(name: &str) -> Result<Scanner> {
    scanners()
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            format!(
                "unknown scanner {name:?}; registered: {:?}",
                scanners().iter().map(|s| s.name.clone()).collect::<Vec<_>>()
            )
            .into()
        })
}

// ------------------------------------------------------------- candidate

#[derive(Clone)]
pub struct StrategyInfo {
    pub index: u32,
    pub name: String,
    pub supported_ops: u32,
}

#[derive(Clone)]
pub struct Candidate {
    pub vt: &'static LbCandidate,
    pub name: String,
    pub version: String,
    pub cpu_features: Option<String>,
    pub strategies: Vec<StrategyInfo>,
}

impl Candidate {
    fn validate(vt: &'static LbCandidate) -> Result<Candidate> {
        if vt.abi_version != LB_ABI_VERSION {
            return Err(format!(
                "candidate ABI version {} != harness {}",
                vt.abi_version, LB_ABI_VERSION
            )
            .into());
        }
        let name = unsafe { cstr(vt.name) }.ok_or("candidate name missing")?.to_string();
        let version = unsafe { cstr(vt.version) }.unwrap_or("0").to_string();
        let cpu_features = unsafe { cstr(vt.cpu_features) }
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty());
        if vt.build.is_none() || vt.footprint.is_none() || vt.destroy.is_none() {
            return Err(format!("candidate {name}: build/footprint/destroy are required").into());
        }
        let mut strategies = Vec::new();
        for i in 0..vt.strategy_count {
            let s = unsafe { &*vt.strategies.add(i as usize) };
            let sname = unsafe { cstr(s.name) }
                .ok_or_else(|| format!("candidate {name}: strategy {i} has no name"))?
                .to_string();
            if sname == "direct" || sname == "decode" {
                return Err(format!(
                    "candidate {name}: strategy name {sname:?} is reserved for \
                     harness-composed strategies (SEMANTICS.md)"
                )
                .into());
            }
            strategies.push(StrategyInfo {
                index: i,
                name: sname,
                supported_ops: s.supported_ops,
            });
        }
        if !strategies.is_empty() && vt.run.is_none() {
            return Err(format!("candidate {name}: declares strategies but run is NULL").into());
        }
        if strategies.is_empty() && vt.view.is_none() && vt.decode.is_none() {
            return Err(format!(
                "candidate {name}: must offer at least one of run/view/decode"
            )
            .into());
        }
        Ok(Candidate {
            vt,
            name,
            version,
            cpu_features,
            strategies,
        })
    }

    pub fn has_view(&self) -> bool {
        self.vt.view.is_some()
    }
    pub fn has_decode(&self) -> bool {
        self.vt.decode.is_some()
    }

    /// Build one chunk; harness-timed by the caller.
    pub fn build_chunk(&self, view: &LbChunkView, config_json: &str) -> Result<BuiltChunk> {
        let config = CString::new(config_json)?;
        let mut err_buf = [0u8; 512];
        let handle = unsafe {
            (self.vt.build.unwrap())(
                view,
                config.as_ptr(),
                err_buf.as_mut_ptr() as *mut _,
                err_buf.len() as u64,
            )
        };
        if handle.is_null() {
            let end = err_buf.iter().position(|&b| b == 0).unwrap_or(0);
            return Err(format!(
                "candidate {} build failed: {}",
                self.name,
                String::from_utf8_lossy(&err_buf[..end])
            )
            .into());
        }
        Ok(BuiltChunk {
            vt: self.vt,
            handle,
            num_rows: view.num_rows,
        })
    }
}

/// One built chunk handle; releases via destroy() on drop.
pub struct BuiltChunk {
    vt: &'static LbCandidate,
    handle: *mut core::ffi::c_void,
    pub num_rows: u64,
}

impl BuiltChunk {
    pub fn footprint(&self) -> Vec<(String, u64)> {
        let f = self.vt.footprint.unwrap();
        let mut buf = vec![LbFootprintComponent::new("", 0); 8];
        let mut n = unsafe { f(self.handle, buf.as_mut_ptr(), buf.len() as u32) };
        if n as usize > buf.len() {
            buf = vec![LbFootprintComponent::new("", 0); n as usize];
            n = unsafe { f(self.handle, buf.as_mut_ptr(), buf.len() as u32) };
        }
        buf[..n as usize]
            .iter()
            .map(|c| (c.name_str(), c.bytes))
            .collect()
    }

    /// # Safety-relevant contract
    /// `bitmap_words` must point at this chunk's pre-zeroed whole-word
    /// slice of the global bitmap.
    pub fn run(
        &self,
        strategy_index: u32,
        query: &LbQuery,
        bitmap_words: *mut u64,
        stats: Option<&mut LbRunStats>,
    ) -> i32 {
        unsafe {
            (self.vt.run.unwrap())(
                self.handle,
                strategy_index,
                query,
                bitmap_words,
                stats.map_or(std::ptr::null_mut(), |s| s as *mut _),
            )
        }
    }

    pub fn view(&self) -> Result<LbChunkView> {
        let mut out = LbChunkView {
            bytes: std::ptr::null(),
            offsets: std::ptr::null(),
            num_rows: 0,
        };
        let rc = unsafe { (self.vt.view.unwrap())(self.handle, &mut out) };
        if rc != 0 {
            return Err(format!("view() returned {rc}").into());
        }
        Ok(out)
    }

    pub fn decode(&self, bytes_out: &mut [u8], offsets_out: &mut [u64]) -> i32 {
        debug_assert!(offsets_out.len() as u64 == self.num_rows + 1);
        unsafe {
            (self.vt.decode.unwrap())(
                self.handle,
                bytes_out.as_mut_ptr(),
                bytes_out.len() as u64,
                offsets_out.as_mut_ptr(),
            )
        }
    }
}

impl Drop for BuiltChunk {
    fn drop(&mut self) {
        unsafe { (self.vt.destroy.unwrap())(self.handle) }
    }
}

// --------------------------------------------------------------- scanner

#[derive(Clone)]
pub struct Scanner {
    pub vt: &'static LbScanner,
    pub name: String,
    pub version: String,
    pub cpu_features: Option<String>,
    pub supported_ops: u32,
}

impl Scanner {
    fn validate(vt: &'static LbScanner) -> Result<Scanner> {
        if vt.abi_version != LB_ABI_VERSION {
            return Err(format!(
                "scanner ABI version {} != harness {}",
                vt.abi_version, LB_ABI_VERSION
            )
            .into());
        }
        let name = unsafe { cstr(vt.name) }.ok_or("scanner name missing")?.to_string();
        if vt.prepare.is_none() || vt.scan.is_none() || vt.release.is_none() {
            return Err(format!("scanner {name}: prepare/scan/release are required").into());
        }
        Ok(Scanner {
            vt,
            name,
            version: unsafe { cstr(vt.version) }.unwrap_or("0").to_string(),
            cpu_features: unsafe { cstr(vt.cpu_features) }
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty()),
            supported_ops: vt.supported_ops,
        })
    }

    /// Per-query capability probe (ABI v4). NULL callback => governed by
    /// `supported_ops` alone (always true here). A `false` result means the
    /// scanner declares this specific query out of its envelope.
    pub fn supports_query(&self, query: &LbQuery) -> bool {
        match self.vt.supports_query {
            None => true,
            Some(f) => unsafe { f(query) != 0 },
        }
    }

    pub fn prepare(&self, query: &LbQuery) -> Result<PreparedScan> {
        let handle = unsafe { (self.vt.prepare.unwrap())(query) };
        if handle.is_null() {
            return Err(format!("scanner {} prepare() failed", self.name).into());
        }
        Ok(PreparedScan {
            vt: self.vt,
            handle,
        })
    }
}

pub struct PreparedScan {
    vt: &'static LbScanner,
    handle: *mut core::ffi::c_void,
}

impl PreparedScan {
    pub fn scan(
        &self,
        view: &LbChunkView,
        bitmap_words: *mut u64,
        stats: Option<&mut LbRunStats>,
    ) -> i32 {
        unsafe {
            (self.vt.scan.unwrap())(
                self.handle,
                view,
                bitmap_words,
                stats.map_or(std::ptr::null_mut(), |s| s as *mut _),
            )
        }
    }
}

impl Drop for PreparedScan {
    fn drop(&mut self) {
        unsafe { (self.vt.release.unwrap())(self.handle) }
    }
}

// ----------------------------------------------------------- query views

/// Owns the C-ABI form of a query (needle descriptors must stay alive and
/// fixed while any run/scan call uses them).
pub struct QueryFfi {
    _needles: Vec<LbBytes>,
    pub query: LbQuery,
}

impl QueryFfi {
    pub fn new(op: u32, needles: &[Vec<u8>]) -> QueryFfi {
        let descriptors: Vec<LbBytes> = needles
            .iter()
            .map(|n| LbBytes {
                ptr: n.as_ptr(),
                len: n.len() as u64,
            })
            .collect();
        let query = LbQuery {
            op,
            needles: descriptors.as_ptr(),
            needle_count: descriptors.len() as u32,
        };
        QueryFfi {
            _needles: descriptors,
            query,
        }
    }
}
