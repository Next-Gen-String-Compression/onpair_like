// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright the Vortex contributors

//! Builds a needle catalog for the encoded corpora written by `bench_corpus`.
//!
//!   `cargo run --release --example bench_needles -- <path>...`
//!
//! Each path is a code stream: comma-separated decimal codes, one row per
//! line. Needles are drawn from that stream, so they are per-encoding by
//! construction; the streams of one column are only comparable through the
//! selectivity a needle set achieves, not through the codes themselves.
//!
//! For every needle length `L`, needle count `N` and target row selectivity
//! `S`, the catalog holds three sets whose achieved selectivity sits as close
//! to `S` as the corpus allows. Selectivity is the share of rows containing at
//! least one of the `N` needles. It lands next to the stream as
//! `<stream>.needles.csv`, with the achieved value recorded alongside the
//! target: a bin the corpus cannot reach is visible rather than silent.

use std::path::Path;

use hashbrown::HashMap;

/// Needle lengths, in codes.
const LENGTHS: &[usize] = &[1, 2, 4];
/// Needles OR-ed together in one query.
const COUNTS: &[usize] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 32, 48, 64, 96, 128, 192, 256,
];
/// Target share of rows a set should match.
const TARGETS: &[f64] = &[0.0, 0.001, 0.01, 0.1, 0.2, 0.5];
/// Sets per bin.
const SAMPLES: usize = 3;
/// N-grams kept after sampling, before their exact row counts are known.
const SHORTLIST: usize = 1 << 15;
/// Candidates carried into set building, spread over exact row frequency.
const POOL: usize = 512;
/// Candidates whose union is evaluated at each step.
const WINDOW: usize = 32;
/// Never-occurring needles proposed per length, for the 0% bin.
const ZEROES: usize = 64;
/// Positions counted when shortlisting.
const SAMPLED_POSITIONS: usize = 4 << 20;
/// Relative distance from the target a set is allowed to keep.
const TOLERANCE: f64 = 0.02;
/// Frequency buckets per octave when spreading candidates.
const PER_OCTAVE: f64 = 4.0;

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    assert!(!paths.is_empty(), "usage: bench_needles <stream.csv>...");
    for path in &paths {
        catalog(Path::new(path));
    }
}

/// A code stream, rows delimited as they are in the file.
struct Stream {
    codes: Vec<u16>,
    row_offsets: Vec<u32>,
}

impl Stream {
    fn rows(&self) -> usize {
        self.row_offsets.len() - 1
    }

    fn row(&self, r: usize) -> &[u16] {
        &self.codes[self.row_offsets[r] as usize..self.row_offsets[r + 1] as usize]
    }
}

/// One candidate needle: its codes packed into a key, and the rows it occurs in.
struct Candidate {
    key: u64,
    rows: Bits,
    count: u32,
}

/// Row bitmap.
#[derive(Clone)]
struct Bits(Vec<u64>);

impl Bits {
    fn new(rows: usize) -> Self {
        Self(vec![0; rows.div_ceil(64)])
    }

    fn set(&mut self, row: usize) {
        self.0[row / 64] |= 1 << (row % 64);
    }

    fn count(&self) -> u32 {
        self.0.iter().map(|word| word.count_ones()).sum()
    }

    /// Rows in `self` or `other`, without materializing the union.
    fn union_count(&self, other: &Self) -> u32 {
        self.0
            .iter()
            .zip(&other.0)
            .map(|(a, b)| (a | b).count_ones())
            .sum()
    }

    fn union_with(&mut self, other: &Self) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a |= b;
        }
    }
}

fn catalog(path: &Path) {
    let stream = load(path);
    let rows = stream.rows();
    println!(
        "\n{}: {rows} rows, {} codes",
        path.file_name().unwrap().to_string_lossy(),
        stream.codes.len()
    );
    let mut csv = String::from("l,n,target,achieved,rows,sample,codes\n");
    for &l in LENGTHS {
        let pool = candidates(&stream, l);
        // Candidate indices by ascending row count, so a step can seek
        // straight to the marginal contribution it needs.
        let mut order: Vec<usize> = (0..pool.len()).collect();
        order.sort_by_key(|&i| pool[i].count);
        let zeroes = order.iter().take_while(|&&i| pool[i].count == 0).count();
        let reachable = pool.iter().map(|c| c.count).max().unwrap_or(0);
        let mut missed = 0;
        for &n in COUNTS {
            for &target in TARGETS {
                for sample in 0..SAMPLES {
                    let Some(set) = build(&pool, &order, n, target, rows, sample) else {
                        continue;
                    };
                    let achieved = f64::from(set.rows) / rows as f64;
                    missed += usize::from((achieved - target).abs() > 0.2 * target);
                    csv.push_str(&set.row(l, n, target, rows, sample));
                }
            }
        }
        println!(
            "  L={l}: {} candidates, {zeroes} never occur, most common in {:.2}% of rows, \
             {missed} sets off target",
            pool.len(),
            100.0 * f64::from(reachable) / rows as f64,
        );
    }
    let out = path.with_extension("needles.csv");
    std::fs::write(&out, csv).unwrap();
}

/// Parse `1,2,3\n4,5\n` into a stream. One line is one row.
fn load(path: &Path) -> Stream {
    let text = std::fs::read(path).unwrap();
    let mut codes = Vec::new();
    let mut row_offsets = vec![0u32];
    let (mut value, mut digits) = (0u32, false);
    for &byte in &text {
        match byte {
            b'0'..=b'9' => {
                value = value * 10 + u32::from(byte - b'0');
                digits = true;
            }
            b',' | b'\n' => {
                if digits {
                    codes.push(value as u16);
                }
                if byte == b'\n' {
                    row_offsets.push(codes.len() as u32);
                }
                value = 0;
                digits = false;
            }
            _ => panic!("unexpected byte in {}", path.display()),
        }
    }
    if digits {
        codes.push(value as u16);
        row_offsets.push(codes.len() as u32);
    }
    Stream { codes, row_offsets }
}

/// `l` codes packed into one key, first code in the low bits.
fn pack(window: &[u16]) -> u64 {
    window
        .iter()
        .enumerate()
        .fold(0u64, |key, (i, &code)| key | (u64::from(code) << (16 * i)))
}

fn unpack(key: u64, l: usize) -> Vec<u16> {
    (0..l).map(|i| (key >> (16 * i)) as u16).collect()
}

/// Keys spread evenly over frequency buckets, `PER_OCTAVE` to the octave, so a
/// pool reaches from the rarest n-gram to the most common one and stays dense
/// at the top, where a 50% target has to be met.
fn spread(counted: &[(u64, u32)], quota: usize) -> Vec<u64> {
    const BUCKETS: usize = 160;
    let mut buckets: Vec<Vec<u64>> = vec![Vec::new(); BUCKETS];
    for &(key, count) in counted {
        let bucket = (f64::from(count).log2() * PER_OCTAVE) as usize;
        buckets[bucket.min(BUCKETS - 1)].push(key);
    }
    for bucket in &mut buckets {
        bucket.sort_unstable();
    }
    let occupied = buckets.iter().filter(|b| !b.is_empty()).count().max(1);
    let per_bucket = quota.div_ceil(occupied);
    let mut keys = Vec::new();
    for bucket in &buckets {
        // Evenly spaced through the bucket rather than the first few, which
        // would all come from one corner of the key space.
        let step = bucket.len().div_ceil(per_bucket).max(1);
        keys.extend(bucket.iter().step_by(step).take(per_bucket));
    }
    keys
}

/// Candidates for one needle length: n-grams spread over the *row* frequency
/// spectrum, plus proposals for the 0% bin, each with the exact set of rows it
/// occurs in.
///
/// Three passes. Sampled positions shortlist the n-grams worth measuring, an
/// exact pass gives the shortlist true row counts (a position count is a poor
/// stand-in: an n-gram repeating inside one row looks far more selective than
/// it is), and the pool drawn from those counts gets bitmaps.
fn candidates(stream: &Stream, l: usize) -> Vec<Candidate> {
    let mut sampled: HashMap<u64, u32> = HashMap::new();
    let positions: usize = (0..stream.rows())
        .map(|r| stream.row(r).len().saturating_sub(l - 1))
        .sum();
    let stride = (positions / SAMPLED_POSITIONS).max(1);
    let mut at = 0usize;
    for r in 0..stream.rows() {
        for window in stream.row(r).windows(l) {
            if at.is_multiple_of(stride) {
                *sampled.entry(pack(window)).or_insert(0) += 1;
            }
            at += 1;
        }
    }
    let shortlist = spread(
        &sampled.iter().map(|(&k, &c)| (k, c)).collect::<Vec<_>>(),
        SHORTLIST,
    );

    let index: HashMap<u64, usize> = shortlist.iter().enumerate().map(|(i, &k)| (k, i)).collect();
    let mut counts = vec![0u32; shortlist.len()];
    let mut last = vec![u32::MAX; shortlist.len()];
    for r in 0..stream.rows() {
        for window in stream.row(r).windows(l) {
            if let Some(&i) = index.get(&pack(window))
                && last[i] != r as u32
            {
                last[i] = r as u32;
                counts[i] += 1;
            }
        }
    }
    let counted: Vec<(u64, u32)> = shortlist.iter().copied().zip(counts).collect();
    let mut keys = spread(&counted, POOL);
    keys.extend(never_occurring(stream, l, &sampled));

    let index: HashMap<u64, usize> = keys.iter().enumerate().map(|(i, &k)| (k, i)).collect();
    let mut bitmaps = vec![Bits::new(stream.rows()); keys.len()];
    for r in 0..stream.rows() {
        for window in stream.row(r).windows(l) {
            if let Some(&i) = index.get(&pack(window)) {
                bitmaps[i].set(r);
            }
        }
    }
    keys.into_iter()
        .zip(bitmaps)
        .map(|(key, rows)| Candidate {
            key,
            count: rows.count(),
            rows,
        })
        .collect()
}

/// Needles built from codes the stream does use but which never occur in this
/// order. Absence is proposed here and confirmed by the bitmap pass, which
/// leaves them with an empty row set.
fn never_occurring(stream: &Stream, l: usize, sampled: &HashMap<u64, u32>) -> Vec<u64> {
    let mut seen = vec![false; 1 << 16];
    for &code in &stream.codes {
        seen[code as usize] = true;
    }
    let alphabet: Vec<u16> = (0..1u32 << 16)
        .filter(|&code| seen[code as usize])
        .map(|code| code as u16)
        .collect();
    if l == 1 {
        // The only single codes that match nothing are the gaps in the
        // alphabet, if the encoder left any.
        let max = alphabet.last().copied().unwrap_or(0);
        return (0..=max)
            .filter(|&code| !seen[code as usize])
            .take(ZEROES)
            .map(|code| pack(&[code]))
            .collect();
    }
    // Codes drawn from spread-out points of the alphabet: a sequence of tokens
    // that share no context is the likeliest never to have been emitted.
    let mut keys = Vec::new();
    for i in 0..alphabet.len() {
        let window: Vec<u16> = (0..l)
            .map(|j| alphabet[(i + j * alphabet.len() / l + j) % alphabet.len()])
            .collect();
        let key = pack(&window);
        if !sampled.contains_key(&key) {
            keys.push(key);
        }
        if keys.len() == ZEROES {
            break;
        }
    }
    keys
}

/// One built needle set.
struct Set {
    chosen: Vec<u64>,
    rows: u32,
}

impl Set {
    fn row(&self, l: usize, n: usize, target: f64, rows: usize, sample: usize) -> String {
        let codes: Vec<String> = self
            .chosen
            .iter()
            .map(|&key| {
                unpack(key, l)
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        format!(
            "{l},{n},{target},{:.6},{},{sample},{}\n",
            f64::from(self.rows) / rows as f64,
            self.rows,
            codes.join("|")
        )
    }
}

/// Pick `n` distinct needles whose union covers as close to `target` of the
/// rows as the pool allows. `sample` picks the sample-th best choice at each
/// step, which is what separates the three sets of a bin.
fn build(
    pool: &[Candidate],
    order: &[usize],
    n: usize,
    target: f64,
    rows: usize,
    sample: usize,
) -> Option<Set> {
    let target_rows = (target * rows as f64).round() as u32;
    if target_rows == 0 {
        // Every needle has to match nothing, so the union does too.
        let zeroes: Vec<usize> = order
            .iter()
            .copied()
            .take_while(|&i| pool[i].count == 0)
            .collect();
        if zeroes.len() < n + sample {
            return None;
        }
        return Some(Set {
            chosen: zeroes[sample..sample + n]
                .iter()
                .map(|&i| pool[i].key)
                .collect(),
            rows: 0,
        });
    }

    let mut chosen: Vec<usize> = Vec::with_capacity(n);
    let mut union = Bits::new(rows);
    let mut covered = 0u32;
    for step in 0..n {
        // Aim at an even share of the target per needle, so the set grows into
        // it instead of one needle carrying it.
        let want = u32::try_from((u64::from(target_rows) * (step as u64 + 1)) / n as u64).unwrap();
        let marginal = want.saturating_sub(covered);
        let rank = |window: &[usize]| -> Vec<(u32, usize)> {
            let mut ranked: Vec<(u32, usize)> = window
                .iter()
                .filter(|&&i| pool[i].count > 0 && !chosen.contains(&i))
                .map(|&i| (union.union_count(&pool[i].rows).abs_diff(want), i))
                .collect();
            ranked.sort_unstable();
            ranked
        };
        // Fall back to the whole pool when the window is used up, which is how
        // a large `n` runs out of neighbours around the marginal it wants.
        let mut ranked = rank(&order[window_around(pool, order, marginal)]);
        if ranked.is_empty() {
            ranked = rank(order);
        }
        if ranked.is_empty() {
            return None;
        }
        let (_, best) = ranked[sample.min(ranked.len() - 1)];
        union.union_with(&pool[best].rows);
        covered = union.count();
        chosen.push(best);
    }
    covered = repair(pool, order, &mut chosen, target_rows, rows, covered, sample);
    Some(Set {
        chosen: chosen.iter().map(|&i| pool[i].key).collect(),
        rows: covered,
    })
}

/// The slice of `order` around the candidates that occur in `marginal` rows.
fn window_around(pool: &[Candidate], order: &[usize], marginal: u32) -> std::ops::Range<usize> {
    let at = order.partition_point(|&i| pool[i].count < marginal);
    at.saturating_sub(WINDOW / 2)..(at + WINDOW / 2).min(order.len())
}

/// Swap one needle at a time while the fit keeps improving. The greedy above
/// builds a set in a single pass and can only overshoot or fall short; this is
/// what closes the gap, and it is where the wide bins get their accuracy.
fn repair(
    pool: &[Candidate],
    order: &[usize],
    chosen: &mut [usize],
    target_rows: u32,
    rows: usize,
    mut covered: u32,
    sample: usize,
) -> u32 {
    let n = chosen.len();
    for _ in 0..8 {
        // Stop as soon as the set is close enough. Driving every set to the
        // exact optimum would collapse the three samples of a bin onto the
        // same needles, which is the opposite of what they are for.
        if f64::from(covered.abs_diff(target_rows)) <= TOLERANCE * f64::from(target_rows) {
            break;
        }
        // Union of everything but one needle, for each needle in turn.
        let mut prefix = vec![Bits::new(rows); n + 1];
        for i in 0..n {
            prefix[i + 1] = prefix[i].clone();
            prefix[i + 1].union_with(&pool[chosen[i]].rows);
        }
        let mut suffix = vec![Bits::new(rows); n + 1];
        for i in (0..n).rev() {
            suffix[i] = suffix[i + 1].clone();
            suffix[i].union_with(&pool[chosen[i]].rows);
        }
        let mut swaps: Vec<(u32, usize, usize)> = Vec::new();
        for i in 0..n {
            let mut rest = prefix[i].clone();
            rest.union_with(&suffix[i + 1]);
            let marginal = target_rows.saturating_sub(rest.count());
            for &c in &order[window_around(pool, order, marginal)] {
                if chosen.contains(&c) {
                    continue;
                }
                let error = rest.union_count(&pool[c].rows).abs_diff(target_rows);
                if error < covered.abs_diff(target_rows) {
                    swaps.push((error, i, c));
                }
            }
        }
        swaps.sort_unstable();
        // The sample-th best swap, for the same reason the greedy takes the
        // sample-th best needle: a bin the corpus cannot reach still owes
        // three different answers, not the same one three times.
        let Some(&(_, at, code)) = swaps.get(sample.min(swaps.len().saturating_sub(1))) else {
            break;
        };
        chosen[at] = code;
        let mut union = Bits::new(rows);
        for &i in chosen.iter() {
            union.union_with(&pool[i].rows);
        }
        covered = union.count();
    }
    covered
}
