//! The one timing loop every candidate is measured by (DESIGN.md §9).
//!
//! A sample is whatever the closure times internally (it returns the
//! measured Duration so per-sample setup like bitmap zeroing stays
//! untimed). Warmup runs first; then samples accumulate until both a
//! minimum iteration count and a minimum time budget are met.

use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct MeasureCfg {
    pub warmup: u32,
    pub min_iters: u32,
    pub min_time: Duration,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyStats {
    pub samples: usize,
    pub min_ns: u64,
    pub p25_ns: u64,
    pub median_ns: u64,
    pub p75_ns: u64,
    pub p99_ns: u64,
    pub max_ns: u64,
    pub mean_ns: f64,
    pub stddev_ns: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_ns: Option<Vec<u64>>,
}

/// Time one closure per the config. The closure performs one full sample
/// (e.g. needle prepare + the query over all chunks) and returns the
/// portion it measured with the harness-provided pattern:
/// `let t = Instant::now(); …work…; t.elapsed()`.
pub fn measure(cfg: &MeasureCfg, keep_raw: bool, mut sample: impl FnMut() -> Duration) -> LatencyStats {
    for _ in 0..cfg.warmup {
        let _ = sample();
    }
    let mut samples_ns: Vec<u64> = Vec::with_capacity(cfg.min_iters as usize);
    let started = Instant::now();
    loop {
        samples_ns.push(sample().as_nanos() as u64);
        if samples_ns.len() >= cfg.min_iters as usize && started.elapsed() >= cfg.min_time {
            break;
        }
        // Adaptive upper bound: never let one cell run away (a 100 ms query
        // stops at min_iters; a microsecond query gets thousands of samples).
        if samples_ns.len() >= 1_000_000 {
            break;
        }
    }
    stats_from(samples_ns, keep_raw)
}

fn stats_from(mut samples: Vec<u64>, keep_raw: bool) -> LatencyStats {
    let raw = keep_raw.then(|| samples.clone());
    samples.sort_unstable();
    let n = samples.len();
    let pct = |p: f64| -> u64 {
        // Nearest-rank percentile on the sorted samples.
        let rank = ((p / 100.0) * n as f64).ceil().max(1.0) as usize;
        samples[rank.min(n) - 1]
    };
    let mean = samples.iter().sum::<u64>() as f64 / n as f64;
    let var = samples
        .iter()
        .map(|&s| (s as f64 - mean).powi(2))
        .sum::<f64>()
        / n as f64;
    LatencyStats {
        samples: n,
        min_ns: samples[0],
        p25_ns: pct(25.0),
        median_ns: pct(50.0),
        p75_ns: pct(75.0),
        p99_ns: pct(99.0),
        max_ns: samples[n - 1],
        mean_ns: mean,
        stddev_ns: var.sqrt(),
        raw_ns: raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_minimums() {
        let cfg = MeasureCfg {
            warmup: 2,
            min_iters: 25,
            min_time: Duration::from_millis(0),
        };
        let mut calls = 0u32;
        let s = measure(&cfg, false, || {
            calls += 1;
            Duration::from_nanos(100)
        });
        assert_eq!(s.samples, 25);
        assert_eq!(calls, 27); // 2 warmup + 25 samples
        assert!(s.raw_ns.is_none());
    }

    #[test]
    fn stats_shape() {
        let s = stats_from((1..=100u64).collect(), true);
        assert_eq!(s.min_ns, 1);
        assert_eq!(s.max_ns, 100);
        assert_eq!(s.median_ns, 50);
        assert_eq!(s.p99_ns, 99);
        assert_eq!(s.mean_ns, 50.5);
        assert_eq!(s.raw_ns.unwrap().len(), 100);
    }
}
