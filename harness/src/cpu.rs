//! Host CPU feature detection (hard capability gating, DESIGN.md §9),
//! core pinning, and environment capture for the results manifest.

use serde::Serialize;

/// Features the harness knows how to detect, in the vocabulary modules use
/// in `cpu_features`. Unknown names are conservatively treated as absent.
pub fn host_features() -> Vec<String> {
    let mut f: Vec<&str> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        // :tt so the std macro sees raw string-literal tokens (a :literal
        // metavariable would not match its internal rules).
        macro_rules! probe {
            ($($name:tt),+ $(,)?) => {
                $(if std::arch::is_x86_feature_detected!($name) { f.push($name); })+
            };
        }
        probe!(
            "sse2", "sse4.2", "popcnt", "pclmulqdq", "avx", "avx2", "bmi1", "bmi2",
            "avx512f", "avx512bw", "avx512vl", "avx512cd", "avx512dq", "avx512vbmi",
            "avx512vbmi2", "vaes", "gfni",
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        macro_rules! probe {
            ($($name:tt),+ $(,)?) => {
                $(if std::arch::is_aarch64_feature_detected!($name) { f.push($name); })+
            };
        }
        probe!("neon", "aes", "sha2", "sha3", "crc", "dotprod", "sve", "sve2");
    }
    f.into_iter().map(String::from).collect()
}

/// Check a module's comma-separated `cpu_features` requirement against the
/// host. `Ok(())` if all present; `Err(missing)` otherwise. NULL/"" (passed
/// here as None/empty) means portable. Unknown feature names count as
/// missing — the safe direction for a gate whose job is to prevent silent
/// scalar execution.
pub fn check_features(required: Option<&str>) -> Result<(), Vec<String>> {
    let required = required.unwrap_or("").trim();
    if required.is_empty() {
        return Ok(());
    }
    let host = host_features();
    let missing: Vec<String> = required
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .filter(|req| !host.iter().any(|h| h == req))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// Pin the current thread to `core`. Returns whether pinning took effect
/// (macOS offers no strict affinity; the result is recorded, not assumed).
pub fn pin_to_core(core: usize) -> bool {
    match core_affinity::get_core_ids() {
        Some(ids) if !ids.is_empty() => {
            let id = ids.get(core).copied().unwrap_or(ids[0]);
            core_affinity::set_for_current(id)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvCapture {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_features: Vec<String>,
    pub cores: usize,
    pub rustc_version: String,
    pub harness_version: String,
    pub cpu_governor: Option<String>,
}

pub fn capture_env() -> EnvCapture {
    EnvCapture {
        hostname: cmd_out("hostname", &[]).unwrap_or_default(),
        os: format!("{} {}", std::env::consts::OS, os_version()),
        arch: std::env::consts::ARCH.to_string(),
        cpu_model: cpu_model(),
        cpu_features: host_features(),
        cores: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        rustc_version: env!("LB_RUSTC_VERSION").to_string(),
        harness_version: env!("CARGO_PKG_VERSION").to_string(),
        cpu_governor: cpu_governor(),
    }
}

fn cmd_out(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn cpu_model() -> String {
    #[cfg(target_os = "macos")]
    {
        cmd_out("sysctl", &["-n", "machdep.cpu.brand_string"]).unwrap_or_default()
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|m| m.trim().to_string())
            })
            .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        String::new()
    }
}

fn os_version() -> String {
    cmd_out("uname", &["-r"]).unwrap_or_default()
}

/// Linux scaling governor; the runner warns when it isn't `performance`.
fn cpu_governor() -> Option<String> {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_modules_pass() {
        assert!(check_features(None).is_ok());
        assert!(check_features(Some("")).is_ok());
    }

    #[test]
    fn fictional_isa_is_gated() {
        let missing = check_features(Some("avx9999,definitely-not-real")).unwrap_err();
        assert_eq!(missing.len(), 2);
    }

    #[test]
    fn native_arch_baseline_detected() {
        #[cfg(target_arch = "aarch64")]
        assert!(check_features(Some("neon")).is_ok());
        #[cfg(target_arch = "x86_64")]
        assert!(check_features(Some("sse2")).is_ok());
    }
}
