//! `bench gen` — the parameterized query generator (DESIGN.md §14).
//!
//! Turns a dataset into a systematic sweep suite: cost-model-derived axes
//! (op × selectivity band × needle length × k/f × mix), needles sampled
//! from the actual column, oracle-probed band acceptance, one seed →
//! byte-identical output. The generator never writes truth — `bench bless`
//! remains the single truth authority — and never hides a gap: every grid
//! point lands in gen-report.json as filled, partial, or empty-with-reason.
//!
//! The grid is code, versioned by GEN_VERSION; changing any axis value is
//! a version bump. The CLI exposes only narrowing knobs (ops, profile), so
//! (dataset checksum, GEN_VERSION, seed, profile, ops) names one exact
//! suite.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use crate::dataset::PreparedDataset;
use crate::oracle;
use crate::suite::{NeedleJson, QueryRecord, QUERIES_FILE, SUITE_FILE};

pub const GEN_VERSION: &str = "gen1";
pub const REPORT_FILE: &str = "gen-report.json";

/// Candidate draws per grid point before giving up (DESIGN.md §14).
const FULL_BUDGET: usize = 32;
const QUICK_BUDGET: usize = 12;
/// contains_any pool of individually-probed len-8 needles.
const FULL_ANY_POOL: usize = 256;
const QUICK_ANY_POOL: usize = 64;
const ANY_NEEDLE_LEN: usize = 8;
/// Estimation sample: every ceil(n/SAMPLE_ROWS)-th row (deterministic).
const SAMPLE_ROWS: u64 = 65_536;
/// Mutation attempts per drawn window in the no-match band.
const MUTATE_TRIES: usize = 8;

pub type Error = Box<dyn std::error::Error>;
pub type Result<T> = std::result::Result<T, Error>;

// ------------------------------------------------------------------- prng
// splitmix64-seeded xoshiro256** — implemented here so determinism does not
// depend on an external crate's version.

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub struct Rng([u64; 4]);

impl Rng {
    pub fn from_seed(seed: u64) -> Self {
        let mut s = seed;
        Rng([
            splitmix64(&mut s),
            splitmix64(&mut s),
            splitmix64(&mut s),
            splitmix64(&mut s),
        ])
    }

    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.0;
        let result = s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Uniform in [0, n) via rejection sampling (unbiased, deterministic).
    pub fn below(&mut self, n: u64) -> u64 {
        debug_assert!(n > 0);
        loop {
            let v = self.next_u64();
            let r = v % n;
            if v - r <= u64::MAX - (n - 1) {
                return r;
            }
        }
    }
}

// ------------------------------------------------------------------ bands

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BandKind {
    /// Exactly zero matches (mutated-absent needles).
    NoMatch,
    /// Log-scale target: accept within ±0.25 decades.
    Decade,
    /// Linear target: accept within ±20% relative.
    Dense,
}

#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub label: &'static str,
    pub target: f64,
    pub kind: BandKind,
}

impl Band {
    const fn no_match() -> Band {
        Band { label: "0", target: 0.0, kind: BandKind::NoMatch }
    }

    /// Final acceptance, on an exact oracle count.
    pub fn accepts(&self, sel: f64) -> bool {
        match self.kind {
            BandKind::NoMatch => sel == 0.0,
            BandKind::Decade => {
                sel > 0.0 && (sel.log10() - self.target.log10()).abs() <= 0.25
            }
            BandKind::Dense => (sel - self.target).abs() <= 0.20 * self.target,
        }
    }

    /// Cheap pre-filter on a sampled estimate: generous margins so sampling
    /// noise never rejects a truly in-band candidate outright, only the
    /// clearly hopeless ones (the exact probe is authoritative).
    fn plausible(&self, est: f64) -> bool {
        match self.kind {
            BandKind::NoMatch => est == 0.0,
            BandKind::Decade => {
                if est == 0.0 {
                    // Nothing in the sample: true selectivity is likely
                    // below sample resolution — plausible only for the
                    // rarest decades.
                    self.target <= 3e-5
                } else {
                    (est.log10() - self.target.log10()).abs() <= 0.75
                }
            }
            BandKind::Dense => est >= 0.5 * self.target && est <= 1.5 * self.target,
        }
    }
}

const DECADES: [Band; 5] = [
    Band { label: "1e-5", target: 1e-5, kind: BandKind::Decade },
    Band { label: "1e-4", target: 1e-4, kind: BandKind::Decade },
    Band { label: "1e-3", target: 1e-3, kind: BandKind::Decade },
    Band { label: "1e-2", target: 1e-2, kind: BandKind::Decade },
    Band { label: "1e-1", target: 1e-1, kind: BandKind::Decade },
];
const DENSE: [Band; 3] = [
    Band { label: "0.3", target: 0.3, kind: BandKind::Dense },
    Band { label: "0.5", target: 0.5, kind: BandKind::Dense },
    Band { label: "0.8", target: 0.8, kind: BandKind::Dense },
];

// ------------------------------------------------------------------- grid

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Profile {
    Full,
    Quick,
}

impl Profile {
    pub fn parse(s: &str) -> Result<Profile> {
        match s {
            "full" => Ok(Profile::Full),
            "quick" => Ok(Profile::Quick),
            other => Err(format!("unknown profile {other:?} (expected full | quick)").into()),
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Profile::Full => "full",
            Profile::Quick => "quick",
        }
    }
    fn budget(&self) -> usize {
        match self {
            Profile::Full => FULL_BUDGET,
            Profile::Quick => QUICK_BUDGET,
        }
    }
    fn any_pool(&self) -> usize {
        match self {
            Profile::Full => FULL_ANY_POOL,
            Profile::Quick => QUICK_ANY_POOL,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mix {
    Balanced,
    Skewed,
}

impl Mix {
    fn name(&self) -> &'static str {
        match self {
            Mix::Balanced => "balanced",
            Mix::Skewed => "skewed",
        }
    }
}

/// One cell of the sweep: op + the axes that apply to it.
#[derive(Clone, Debug)]
pub struct GridPoint {
    pub op: u32,
    pub band: Band,
    /// Needle length: per-needle for single-needle ops and contains_any,
    /// total across fragments for multi_contains.
    pub len: usize,
    /// contains_any only.
    pub k: Option<usize>,
    pub mix: Option<Mix>,
    /// multi_contains only.
    pub fragments: Option<usize>,
    pub replicates: usize,
}

/// The versioned grid (DESIGN.md §14 per-op table). Op order, and the axis
/// orders within each op, are fixed: they define query ids and output order.
pub fn grid(profile: Profile, ops_filter: Option<&[u32]>) -> Vec<GridPoint> {
    let wanted = |op: u32| ops_filter.map_or(true, |ops| ops.contains(&op));
    let mut points = Vec::new();

    let (single_bands, single_r): (Vec<Band>, usize) = match profile {
        Profile::Full => {
            let mut b = vec![Band::no_match()];
            b.extend(DECADES);
            b.extend(DENSE);
            (b, 5)
        }
        Profile::Quick => {
            let mut b = vec![Band::no_match()];
            b.extend(DECADES);
            (b, 2)
        }
    };
    let single_lens: &[usize] = match profile {
        Profile::Full => &[1, 2, 4, 8, 16, 32, 64],
        Profile::Quick => &[2, 8, 32],
    };
    let prefix_extra: &[usize] = match profile {
        Profile::Full => &[128],
        Profile::Quick => &[],
    };

    for &op in &[lb_abi::LB_PREFIX, lb_abi::LB_SUFFIX, lb_abi::LB_CONTAINS] {
        if !wanted(op) {
            continue;
        }
        let mut lens = single_lens.to_vec();
        if op == lb_abi::LB_PREFIX {
            lens.extend(prefix_extra);
        }
        for &len in &lens {
            for &band in &single_bands {
                points.push(GridPoint {
                    op,
                    band,
                    len,
                    k: None,
                    mix: None,
                    fragments: None,
                    replicates: single_r,
                });
            }
        }
    }

    if wanted(lb_abi::LB_MULTI_CONTAINS) {
        let (fs, totals, bands, r): (&[usize], &[usize], &[Band], usize) = match profile {
            Profile::Full => (&[2, 3, 4, 8], &[8, 16, 32], &DECADES, 3),
            Profile::Quick => (&[2, 3], &[8], &DECADES[3..5], 2),
        };
        for &f in fs {
            for &total in totals {
                for &band in bands {
                    points.push(GridPoint {
                        op: lb_abi::LB_MULTI_CONTAINS,
                        band,
                        len: total,
                        k: None,
                        mix: None,
                        fragments: Some(f),
                        replicates: r,
                    });
                }
            }
        }
    }

    if wanted(lb_abi::LB_CONTAINS_ANY) {
        let (ks, bands, r): (&[usize], Vec<Band>, usize) = match profile {
            Profile::Full => (
                &[2, 4, 8, 16, 64],
                vec![DECADES[1], DECADES[2], DECADES[3], DECADES[4], DENSE[1]],
                3,
            ),
            Profile::Quick => (&[2, 8], vec![DECADES[3], DECADES[4]], 2),
        };
        for &k in ks {
            for &band in &bands {
                for mix in [Mix::Balanced, Mix::Skewed] {
                    points.push(GridPoint {
                        op: lb_abi::LB_CONTAINS_ANY,
                        band,
                        len: ANY_NEEDLE_LEN,
                        k: Some(k),
                        mix: Some(mix),
                        fragments: None,
                        replicates: r,
                    });
                }
            }
        }
    }

    points
}

// ---------------------------------------------------------------- probing

/// Oracle-backed selectivity probes: a stride-sampled estimate to discard
/// hopeless candidates cheaply, then an exact full-column count for
/// acceptance. Both use oracle::row_matches — no fast-scanner shortcut
/// (DESIGN.md §14: single root of trust; bless re-derives regardless).
struct Prober<'a> {
    ds: &'a PreparedDataset,
    sample: Vec<u64>,
}

impl<'a> Prober<'a> {
    fn new(ds: &'a PreparedDataset) -> Self {
        let n = ds.num_rows();
        let stride = n.div_ceil(SAMPLE_ROWS).max(1);
        let sample = (0..n).step_by(stride as usize).collect();
        Prober { ds, sample }
    }

    fn estimate(&self, op: u32, needles: &[&[u8]]) -> f64 {
        let hits = self
            .sample
            .iter()
            .filter(|&&i| oracle::row_matches(op, needles, self.ds.row(i)))
            .count();
        hits as f64 / self.sample.len() as f64
    }

    fn exact(&self, op: u32, needles: &[&[u8]]) -> (u64, f64) {
        let mut count = 0u64;
        for row in self.ds.rows() {
            if oracle::row_matches(op, needles, row) {
                count += 1;
            }
        }
        (count, count as f64 / self.ds.num_rows() as f64)
    }
}

/// Row indices sorted by length descending, so "all rows with len >= L" is
/// a prefix slice — uniform witness sampling with no rejection loop.
struct RowsByLen {
    idx: Vec<u64>,
    lens: Vec<u64>,
}

impl RowsByLen {
    fn new(ds: &PreparedDataset) -> Self {
        let off = ds.offsets_u64();
        let mut idx: Vec<u64> = (0..ds.num_rows()).collect();
        let len = |i: u64| off[i as usize + 1] - off[i as usize];
        // Stable tie-break by row index keeps this fully deterministic.
        idx.sort_by_key(|&i| (std::cmp::Reverse(len(i)), i));
        let lens = idx.iter().map(|&i| len(i)).collect();
        RowsByLen { idx, lens }
    }

    fn eligible(&self, min_len: usize) -> &[u64] {
        let cut = self.lens.partition_point(|&l| l >= min_len as u64);
        &self.idx[..cut]
    }
}

// ------------------------------------------------------------- generation

#[derive(Serialize)]
pub struct PointReport {
    pub op: String,
    pub band: String,
    pub len: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub k: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragments: Option<usize>,
    pub requested: usize,
    pub filled: usize,
    pub achieved_selectivities: Vec<f64>,
    pub draws: usize,
    /// Present iff filled < requested: no_candidates_in_band |
    /// length_infeasible | budget_exhausted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub struct GenReport {
    pub generator: String,
    pub seed: u64,
    pub profile: String,
    pub ops: Vec<String>,
    pub dataset: serde_json::Value,
    pub grid_points: usize,
    pub filled_points: usize,
    pub partial_points: usize,
    pub empty_points: usize,
    pub queries: usize,
    pub estimate_probes: u64,
    pub exact_probes: u64,
    pub points: Vec<PointReport>,
}

pub struct GenParams {
    pub seed: u64,
    pub profile: Profile,
    /// None = all five ops (grid order).
    pub ops: Option<Vec<u32>>,
    pub suite_id: String,
}

struct Gen<'a> {
    ds: &'a PreparedDataset,
    prober: Prober<'a>,
    rows_by_len: RowsByLen,
    rng: Rng,
    budget: usize,
    dedup: HashSet<(u32, Vec<Vec<u8>>)>,
    estimate_probes: u64,
    exact_probes: u64,
    /// Individually estimated len-8 needles for contains_any assembly.
    any_pool: Vec<(Vec<u8>, f64)>,
}

fn op_label(op: u32) -> &'static str {
    match op {
        lb_abi::LB_PREFIX => "prefix",
        lb_abi::LB_SUFFIX => "suffix",
        lb_abi::LB_CONTAINS => "contains",
        lb_abi::LB_MULTI_CONTAINS => "multi",
        lb_abi::LB_CONTAINS_ANY => "any",
        _ => unreachable!(),
    }
}

fn needle_json(bytes: &[u8]) -> NeedleJson {
    use base64::Engine;
    match std::str::from_utf8(bytes) {
        Ok(s) => NeedleJson::Text(s.to_string()),
        Err(_) => NeedleJson::B64 {
            b64: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    }
}

impl<'a> Gen<'a> {
    /// A slice of a matching row: the window recipe per op.
    fn sample_window(&mut self, op: u32, len: usize) -> Option<(u64, Vec<u8>)> {
        let eligible = self.rows_by_len.eligible(len);
        if eligible.is_empty() {
            return None;
        }
        let row_i = eligible[self.rng.below(eligible.len() as u64) as usize];
        let row = self.ds.row(row_i);
        let window = match op {
            lb_abi::LB_PREFIX => &row[..len],
            lb_abi::LB_SUFFIX => &row[row.len() - len..],
            _ => {
                let at = self.rng.below((row.len() - len + 1) as u64) as usize;
                &row[at..at + len]
            }
        };
        Some((row_i, window.to_vec()))
    }

    /// No-match needles: a sampled window with 1–2 seeded byte mutations —
    /// realistic byte statistics, absence verified by the exact probe.
    fn mutate(&mut self, window: &mut [u8]) {
        let muts = 1 + self.rng.below(2) as usize;
        for _ in 0..muts.min(window.len()) {
            let pos = self.rng.below(window.len() as u64) as usize;
            window[pos] = self.rng.below(256) as u8;
        }
    }

    /// One replicate attempt for a single-needle op. Returns the accepted
    /// needle with its witness row and exact selectivity.
    fn draw_single(&mut self, p: &GridPoint) -> Option<(Vec<Vec<u8>>, Option<u64>, f64)> {
        let (row_i, mut needle) = self.sample_window(p.op, p.len)?;
        let witness = if p.band.kind == BandKind::NoMatch {
            let mut ok = false;
            for _ in 0..MUTATE_TRIES {
                self.mutate(&mut needle);
                self.estimate_probes += 1;
                if self.prober.estimate(p.op, &[&needle]) == 0.0 {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return None;
            }
            None
        } else {
            Some(row_i)
        };
        self.accept(p, vec![needle]).map(|(n, sel)| (n, witness, sel))
    }

    /// multi_contains: cut one window of a sampled row into `f` ordered
    /// fragments with seeded gaps — the row is a witness by construction.
    fn draw_multi(&mut self, p: &GridPoint) -> Option<(Vec<Vec<u8>>, Option<u64>, f64)> {
        let f = p.fragments.unwrap();
        let total = p.len;
        let eligible = self.rows_by_len.eligible(total);
        if eligible.is_empty() {
            return None;
        }
        let row_i = eligible[self.rng.below(eligible.len() as u64) as usize];
        let row = self.ds.row(row_i);
        let slack = row.len() - total;
        let gap_budget = self.rng.below((slack.min(32) + 1) as u64) as usize;
        let start = self.rng.below((slack - gap_budget + 1) as u64) as usize;

        // Fragment lengths: a random composition of `total` into f parts >= 1.
        let mut cuts: Vec<usize> = Vec::with_capacity(f - 1);
        while cuts.len() < f - 1 {
            let c = 1 + self.rng.below((total - 1) as u64) as usize;
            if !cuts.contains(&c) {
                cuts.push(c);
            }
        }
        cuts.sort_unstable();
        let mut frag_lens = Vec::with_capacity(f);
        let mut prev = 0;
        for &c in &cuts {
            frag_lens.push(c - prev);
            prev = c;
        }
        frag_lens.push(total - prev);

        // Walk the row: fragment, gap, fragment, ...
        let mut needles = Vec::with_capacity(f);
        let mut cur = start;
        let mut gap_left = gap_budget;
        for (i, &fl) in frag_lens.iter().enumerate() {
            needles.push(row[cur..cur + fl].to_vec());
            cur += fl;
            if i + 1 < f && gap_left > 0 {
                let g = self.rng.below((gap_left + 1) as u64) as usize;
                cur += g;
                gap_left -= g;
            }
        }
        self.accept(p, needles).map(|(n, sel)| (n, Some(row_i), sel))
    }

    /// contains_any: assemble k needles from the pre-probed pool so the
    /// per-needle selectivities follow the mix profile, then probe the union.
    fn draw_any(&mut self, p: &GridPoint) -> Option<(Vec<Vec<u8>>, Option<u64>, f64)> {
        let (k, t) = (p.k.unwrap(), p.band.target);
        let pick_distinct = |rng: &mut Rng, pool: &[usize], n: usize| -> Option<Vec<usize>> {
            if pool.len() < n {
                return None;
            }
            let mut chosen: Vec<usize> = Vec::with_capacity(n);
            while chosen.len() < n {
                let c = pool[rng.below(pool.len() as u64) as usize];
                if !chosen.contains(&c) {
                    chosen.push(c);
                }
            }
            Some(chosen)
        };
        let sel_of = |i: usize| self.any_pool[i].1;
        let indices: Vec<usize> = (0..self.any_pool.len()).collect();
        let chosen = match p.mix.unwrap() {
            Mix::Balanced => {
                // Each needle contributes ~t/k to the union.
                let per = t / k as f64;
                let fit: Vec<usize> = indices
                    .iter()
                    .copied()
                    .filter(|&i| sel_of(i) >= per / 3.0 && sel_of(i) <= (per * 3.0).min(1.0))
                    .collect();
                pick_distinct(&mut self.rng, &fit, k)?
            }
            Mix::Skewed => {
                // One needle dominates the union; the rest are near-noise.
                let commons: Vec<usize> = indices
                    .iter()
                    .copied()
                    .filter(|&i| sel_of(i) >= 0.5 * t && sel_of(i) <= 1.5 * t)
                    .collect();
                let rare_cut = (t / (10.0 * k as f64)).max(1e-6);
                let rares: Vec<usize> = indices
                    .iter()
                    .copied()
                    .filter(|&i| sel_of(i) <= rare_cut)
                    .collect();
                let mut v = pick_distinct(&mut self.rng, &commons, 1)?;
                v.extend(pick_distinct(&mut self.rng, &rares, k - 1)?);
                v
            }
        };
        let needles: Vec<Vec<u8>> = chosen.iter().map(|&i| self.any_pool[i].0.clone()).collect();
        self.accept(p, needles).map(|(n, sel)| (n, None, sel))
    }

    /// Dedup + estimate gate + exact probe + band acceptance.
    fn accept(&mut self, p: &GridPoint, needles: Vec<Vec<u8>>) -> Option<(Vec<Vec<u8>>, f64)> {
        // contains_any is order-insensitive: dedup on a sorted key.
        let mut key_needles = needles.clone();
        if p.op == lb_abi::LB_CONTAINS_ANY {
            key_needles.sort();
        }
        let key = (p.op, key_needles);
        if self.dedup.contains(&key) {
            return None;
        }
        let refs: Vec<&[u8]> = needles.iter().map(|n| n.as_slice()).collect();
        self.estimate_probes += 1;
        if !p.band.plausible(self.prober.estimate(p.op, &refs)) {
            return None;
        }
        self.exact_probes += 1;
        let (_, sel) = self.prober.exact(p.op, &refs);
        if !p.band.accepts(sel) {
            return None;
        }
        self.dedup.insert(key);
        Some((needles, sel))
    }

    /// Build the contains_any pool: deduped len-8 windows, each with an
    /// estimated individual selectivity, in draw order (deterministic).
    fn build_any_pool(&mut self, size: usize) {
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        // Bounded draws so a tiny dataset cannot loop forever.
        for _ in 0..size * 4 {
            if self.any_pool.len() >= size {
                break;
            }
            let Some((_, w)) = self.sample_window(lb_abi::LB_CONTAINS, ANY_NEEDLE_LEN) else {
                break;
            };
            if !seen.insert(w.clone()) {
                continue;
            }
            self.estimate_probes += 1;
            let est = self.prober.estimate(lb_abi::LB_CONTAINS, &[&w]);
            self.any_pool.push((w, est));
        }
    }
}

#[derive(Debug)]
pub struct GenOutcome {
    pub queries: usize,
    pub filled_points: usize,
    pub partial_points: usize,
    pub empty_points: usize,
    pub grid_points: usize,
}

/// Generate a suite into `out_dir` (suite.json + queries.jsonl +
/// gen-report.json). Refuses to overwrite an existing queries.jsonl unless
/// `force` — a blessed suite is never silently clobbered.
pub fn generate(
    ds: &PreparedDataset,
    out_dir: &Path,
    params: &GenParams,
    force: bool,
) -> Result<GenOutcome> {
    if out_dir.join(QUERIES_FILE).exists() && !force {
        return Err(format!(
            "{} already exists — pass --force to regenerate (this discards blessed truth)",
            out_dir.join(QUERIES_FILE).display()
        )
        .into());
    }

    let points = grid(params.profile, params.ops.as_deref());
    if points.is_empty() {
        return Err("ops filter selected no grid points".into());
    }

    let mut g = Gen {
        ds,
        prober: Prober::new(ds),
        rows_by_len: RowsByLen::new(ds),
        rng: Rng::from_seed(params.seed),
        budget: params.profile.budget(),
        dedup: HashSet::new(),
        estimate_probes: 0,
        exact_probes: 0,
        any_pool: Vec::new(),
    };
    if points.iter().any(|p| p.op == lb_abi::LB_CONTAINS_ANY) {
        g.build_any_pool(params.profile.any_pool());
    }

    let mut records: Vec<QueryRecord> = Vec::new();
    let mut reports: Vec<PointReport> = Vec::new();

    for p in &points {
        let mut achieved: Vec<f64> = Vec::new();
        let mut draws = 0usize;
        let mut infeasible = false;

        while achieved.len() < p.replicates && draws < g.budget {
            draws += 1;
            let drawn = match p.op {
                lb_abi::LB_MULTI_CONTAINS => g.draw_multi(p),
                lb_abi::LB_CONTAINS_ANY => g.draw_any(p),
                _ => g.draw_single(p),
            };
            // A None from an empty eligible set / empty pool will repeat
            // forever: record infeasibility and stop this point early.
            let Some((needles, witness, sel)) = drawn else {
                if g.rows_by_len.eligible(p.len).is_empty()
                    || (p.op == lb_abi::LB_CONTAINS_ANY && g.any_pool.len() < p.k.unwrap())
                {
                    infeasible = true;
                    break;
                }
                continue;
            };

            let r = achieved.len();
            let id = match p.op {
                lb_abi::LB_MULTI_CONTAINS => format!(
                    "{}.multi.f{}.L{}.s{}.r{r}",
                    params.suite_id,
                    p.fragments.unwrap(),
                    p.len,
                    p.band.label
                ),
                lb_abi::LB_CONTAINS_ANY => format!(
                    "{}.any.k{}.{}.s{}.r{r}",
                    params.suite_id,
                    p.k.unwrap(),
                    p.mix.unwrap().name(),
                    p.band.label
                ),
                op => format!(
                    "{}.{}.L{}.s{}.r{r}",
                    params.suite_id,
                    op_label(op),
                    p.len,
                    p.band.label
                ),
            };
            let mut gen_meta = serde_json::json!({
                "version": GEN_VERSION,
                "seed": params.seed,
                "profile": params.profile.name(),
                "band": p.band.label,
                "target_selectivity": p.band.target,
                "target_len": p.len,
            });
            let m = gen_meta.as_object_mut().unwrap();
            if let Some(k) = p.k {
                m.insert("k".into(), k.into());
                m.insert("mix".into(), p.mix.unwrap().name().into());
            }
            if let Some(f) = p.fragments {
                m.insert("fragments".into(), f.into());
            }
            if let Some(w) = witness {
                m.insert("witness_row".into(), w.into());
            }
            records.push(QueryRecord {
                id,
                op: lb_abi::op_name(p.op).to_string(),
                needles: needles.iter().map(|n| needle_json(n)).collect(),
                meta: Some(serde_json::json!({ "gen": gen_meta })),
                truth: None,
                derived: None,
            });
            achieved.push(sel);
        }

        let filled = achieved.len();
        reports.push(PointReport {
            op: lb_abi::op_name(p.op).to_string(),
            band: p.band.label.to_string(),
            len: p.len,
            k: p.k,
            mix: p.mix.map(|m| m.name().to_string()),
            fragments: p.fragments,
            requested: p.replicates,
            filled,
            achieved_selectivities: achieved,
            draws,
            reason: if filled == p.replicates {
                None
            } else if infeasible {
                Some("length_infeasible".to_string())
            } else if filled == 0 {
                Some("no_candidates_in_band".to_string())
            } else {
                Some("budget_exhausted".to_string())
            },
        });
    }

    if records.is_empty() {
        return Err(
            "generation produced 0 queries — every grid point was infeasible on this dataset"
                .into(),
        );
    }

    let filled_points = reports.iter().filter(|r| r.filled == r.requested).count();
    let empty_points = reports.iter().filter(|r| r.filled == 0).count();
    let partial_points = reports.len() - filled_points - empty_points;

    let ops_named: Vec<String> = {
        let all = [
            lb_abi::LB_PREFIX,
            lb_abi::LB_SUFFIX,
            lb_abi::LB_CONTAINS,
            lb_abi::LB_MULTI_CONTAINS,
            lb_abi::LB_CONTAINS_ANY,
        ];
        all.iter()
            .filter(|op| params.ops.as_ref().map_or(true, |sel| sel.contains(op)))
            .map(|&op| lb_abi::op_name(op).to_string())
            .collect()
    };

    let report = GenReport {
        generator: GEN_VERSION.to_string(),
        seed: params.seed,
        profile: params.profile.name().to_string(),
        ops: ops_named.clone(),
        dataset: serde_json::json!({
            "id": ds.manifest.id,
            "checksum": ds.manifest.checksum,
            "num_rows": ds.num_rows(),
        }),
        grid_points: reports.len(),
        filled_points,
        partial_points,
        empty_points,
        queries: records.len(),
        estimate_probes: g.estimate_probes,
        exact_probes: g.exact_probes,
        points: reports,
    };

    let manifest = crate::suite::SuiteManifest {
        format_version: 1,
        id: params.suite_id.clone(),
        description: format!(
            "Generated sweep over dataset {} ({} rows): {} queries across ops {:?} \
             (DESIGN.md §14; {}/{} grid points filled). Regenerate with `bench gen --seed {} \
             --profile {}`.",
            ds.manifest.id,
            ds.num_rows(),
            records.len(),
            ops_named,
            filled_points,
            report.grid_points,
            params.seed,
            params.profile.name(),
        ),
        dataset: crate::suite::DatasetBinding {
            id: ds.manifest.id.clone(),
            checksum: None, // stamped by bless
        },
        provenance: Some(serde_json::json!({
            "generator": {
                "name": GEN_VERSION,
                "seed": params.seed,
                "profile": params.profile.name(),
                "ops": ops_named,
            }
        })),
        truth_algo: None,
        blessed_at: None,
    };

    std::fs::create_dir_all(out_dir)?;
    let mut lines = String::new();
    for r in &records {
        lines.push_str(&serde_json::to_string(r)?);
        lines.push('\n');
    }
    std::fs::write(out_dir.join(QUERIES_FILE), lines)?;
    std::fs::write(
        out_dir.join(SUITE_FILE),
        format!("{}\n", serde_json::to_string_pretty(&manifest)?),
    )?;
    std::fs::write(
        out_dir.join(REPORT_FILE),
        format!("{}\n", serde_json::to_string_pretty(&report)?),
    )?;

    Ok(GenOutcome {
        queries: records.len(),
        filled_points,
        partial_points,
        empty_points,
        grid_points: report.grid_points,
    })
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_acceptance() {
        let nm = Band::no_match();
        assert!(nm.accepts(0.0));
        assert!(!nm.accepts(1e-6));

        let d = DECADES[2]; // 1e-3, ±0.25 decades = [10^-3.25, 10^-2.75]
        assert!(d.accepts(1e-3));
        assert!(d.accepts(0.00057)); // 10^-3.24
        assert!(d.accepts(0.00177)); // 10^-2.75
        assert!(!d.accepts(0.00055)); // 10^-3.26
        assert!(!d.accepts(0.0018)); // 10^-2.74
        assert!(!d.accepts(0.0));

        let dense = DENSE[1]; // 0.5 ± 20%
        assert!(dense.accepts(0.5));
        assert!(dense.accepts(0.41));
        assert!(dense.accepts(0.59));
        assert!(!dense.accepts(0.39));
        assert!(!dense.accepts(0.61));
    }

    #[test]
    fn plausibility_is_wider_than_acceptance() {
        for band in DECADES.iter().chain(DENSE.iter()) {
            // Anything the band accepts must never be pre-filtered away
            // when the estimate is exact.
            for mul in [0.6, 0.8, 1.0, 1.25, 1.7] {
                let sel = band.target * mul;
                if band.accepts(sel) {
                    assert!(band.plausible(sel), "{} rejects in-band {sel}", band.label);
                }
            }
        }
        // Sample-resolution zero stays plausible only for the rarest decades.
        assert!(DECADES[0].plausible(0.0));
        assert!(!DECADES[2].plausible(0.0));
    }

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let mut a = Rng::from_seed(42);
        let mut b = Rng::from_seed(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = Rng::from_seed(7);
        for n in [1u64, 2, 3, 10, 1000, u64::MAX] {
            for _ in 0..100 {
                assert!(c.below(n) < n);
            }
        }
        // Different seeds diverge immediately.
        assert_ne!(Rng::from_seed(1).next_u64(), Rng::from_seed(2).next_u64());
    }

    #[test]
    fn full_grid_matches_design_table() {
        // §14 table counts *targets* (cells × replicates):
        // contains 315 + prefix 360 + suffix 315 + multi 180 + any 150 = 1320.
        let g = grid(Profile::Full, None);
        let targets = |op: u32| -> usize {
            g.iter().filter(|p| p.op == op).map(|p| p.replicates).sum()
        };
        assert_eq!(targets(lb_abi::LB_CONTAINS), 315);
        assert_eq!(targets(lb_abi::LB_PREFIX), 360);
        assert_eq!(targets(lb_abi::LB_SUFFIX), 315);
        assert_eq!(targets(lb_abi::LB_MULTI_CONTAINS), 180);
        assert_eq!(targets(lb_abi::LB_CONTAINS_ANY), 150);
        assert_eq!(g.iter().map(|p| p.replicates).sum::<usize>(), 1320);
    }

    #[test]
    fn ops_filter_narrows_the_grid() {
        let g = grid(Profile::Full, Some(&[lb_abi::LB_CONTAINS]));
        assert_eq!(g.iter().map(|p| p.replicates).sum::<usize>(), 315);
        assert!(g.iter().all(|p| p.op == lb_abi::LB_CONTAINS));
    }
}
