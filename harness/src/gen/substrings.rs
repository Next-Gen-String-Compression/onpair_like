//! A reusable index of substrings that occur within individual rows.
//!
//! Bytes are lifted to 1..=256 in a u16 alphabet, leaving zero for row ends.
//! This preserves binary data and prevents matches across rows. SA + LCP
//! intervals describe whole ranges of lengths with identical occurrences;
//! visiting those families avoids materializing every distinct substring.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use libsais_rs::libsais16::{libsais16, libsais16_plcp_gsa};
use xxhash_rust::xxh3::Xxh3;

use crate::suite::Result;

const CACHE_MAGIC: &[u8; 8] = b"LBSA0001";

/// Explicit limits; exceeding either returns an error, never a sampled index.
#[derive(Clone, Copy, Debug)]
pub struct IndexLimits {
    pub max_needle_len: usize,
    /// Conservative workspace admission estimate, excluding the borrowed dataset.
    /// This is not an operating-system RSS limit.
    pub memory_budget_bytes: u64,
}

impl Default for IndexLimits {
    fn default() -> Self {
        Self {
            max_needle_len: 256,
            memory_budget_bytes: 4 << 30,
        }
    }
}

impl IndexLimits {
    /// Conservative index workspace estimate, excluding the loaded dataset.
    /// Also checks the suffix-array backend's encoded-symbol limit.
    pub fn required_memory_bytes(payload_bytes: u64, num_rows: u64) -> Result<u64> {
        let symbols = payload_bytes
            .checked_add(num_rows)
            .ok_or("index size overflow")?;
        if symbols > i32::MAX as u64 {
            return Err("SA backend supports at most i32::MAX encoded symbols".into());
        }
        Ok(symbols * 16 + 64 * 1024 * 1024)
    }
}

/// Index construction is independent of sampling seed, buckets, and quota.
pub struct SubstringIndex<'a> {
    pub(super) payload: &'a [u8],
    pub(super) offsets: &'a [u64],
    pub(super) max_len: usize,
    pub(super) checksum: String,
    // SA includes separator suffixes first, one per row. Keeping them avoids
    // copying the large array just to remove that prefix.
    sa: Vec<i32>,
    lcp: Vec<u16>,
    boundaries: RowBoundaries,
}

/// One distinct substring at each length in min_len..=max_len. All share the
/// same occurrence interval and hence the same distinct matching-row count.
#[derive(Clone, Copy, Debug)]
pub(super) struct Family {
    pub position: usize,
    pub min_len: usize,
    pub max_len: usize,
    pub matching_rows: u64,
}

impl<'a> SubstringIndex<'a> {
    pub fn new(payload: &'a [u8], offsets: &'a [u64], limits: IndexLimits) -> Result<Self> {
        let mut index = Self::empty(payload, offsets, limits)?;
        index.build()?;
        Ok(index)
    }

    /// Reuse an SA/LCP cache bound to the logical dataset, maximum length, and
    /// index format. Seeds and quotas intentionally do not enter the cache key.
    pub fn cached(
        payload: &'a [u8],
        offsets: &'a [u64],
        limits: IndexLimits,
        directory: &Path,
    ) -> Result<Self> {
        let mut index = Self::empty(payload, offsets, limits)?;
        let key = index.checksum.replace(':', "-");
        let path = directory.join(format!("sa1-{key}-L{}.bin", limits.max_needle_len));
        if path.exists() {
            index.read_cache(&path)?;
        } else {
            index.build()?;
            std::fs::create_dir_all(directory)?;
            // create_new avoids two generators sharing a partially written file.
            let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
            let file = File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            let result = index.write_cache(file).and_then(|()| {
                std::fs::rename(&temporary, &path)?;
                Ok(())
            });
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            result?;
        }
        Ok(index)
    }

    pub fn num_rows(&self) -> u64 {
        (self.offsets.len() - 1) as u64
    }

    fn empty(payload: &'a [u8], offsets: &'a [u64], limits: IndexLimits) -> Result<Self> {
        if offsets.len() < 2
            || offsets.first() != Some(&0)
            || offsets.last() != Some(&(payload.len() as u64))
            || offsets.windows(2).any(|w| w[0] > w[1])
        {
            return Err(
                "expected one or more rows with monotone offsets spanning the payload".into(),
            );
        }
        if !(1..=u16::MAX as usize).contains(&limits.max_needle_len) {
            return Err("maximum needle length must be in 1..=65535 bytes".into());
        }
        let n = payload
            .len()
            .checked_add(offsets.len() - 1)
            .ok_or("index size overflow")?;
        // Includes SA, text, PLCP/LCP, boundary ranks, row counters and headroom
        // for the backend. Sampling has its own separately bounded reservoirs.
        let estimate =
            IndexLimits::required_memory_bytes(payload.len() as u64, (offsets.len() - 1) as u64)?;
        if estimate > limits.memory_budget_bytes {
            return Err(format!("SA workspace estimate {estimate} bytes exceeds budget {}; increase --index-memory-mib explicitly", limits.memory_budget_bytes).into());
        }
        let mut hash = Xxh3::new();
        for w in offsets.windows(2) {
            hash.update(&(w[1] - w[0]).to_le_bytes());
            hash.update(&payload[w[0] as usize..w[1] as usize]);
        }
        Ok(Self {
            payload,
            offsets,
            max_len: limits.max_needle_len,
            checksum: format!("xxh3:{:016x}", hash.digest()),
            sa: Vec::new(),
            lcp: Vec::new(),
            boundaries: RowBoundaries::new(offsets, n),
        })
    }

    fn build(&mut self) -> Result<()> {
        let n = self.payload.len() + self.num_rows() as usize;
        let mut text = Vec::with_capacity(n);
        for w in self.offsets.windows(2) {
            text.extend(
                self.payload[w[0] as usize..w[1] as usize]
                    .iter()
                    .map(|&b| b as u16 + 1),
            );
            text.push(0);
        }
        self.sa = vec![0; n];
        // Ordinary SA permits consecutive separators (empty rows). Because zero
        // sorts below every lifted byte, nonempty row suffixes remain ordered;
        // bytes after a separator only break ties between equal row suffixes.
        // The GSA PLCP routine stops comparison at the first separator.
        let status = libsais16(&text, &mut self.sa, 0, None);
        if status != 0 {
            return Err(format!("generalized suffix-array construction failed ({status})").into());
        }
        let mut plcp = vec![0; n];
        if libsais16_plcp_gsa(&text, &self.sa, &mut plcp) != 0 {
            return Err("row-bounded LCP construction failed".into());
        }
        self.lcp = self
            .sa
            .iter()
            .map(|&p| (plcp[p as usize] as usize).min(self.max_len) as u16)
            .collect();
        Ok(())
    }

    /// Byte offset and row of a non-separator suffix at a rank in the full SA.
    fn location(&self, rank: usize) -> (usize, usize) {
        let encoded = self.sa[rank] as usize;
        let row = self.boundaries.row(encoded);
        (encoded - row, row)
    }

    fn suffix(&self, rank: usize) -> &[u8] {
        let (p, row) = self.location(rank);
        &self.payload[p..self.offsets[row + 1] as usize]
    }

    /// Exact existence check, including row boundaries, without scanning rows.
    pub fn contains(&self, needle: &[u8]) -> bool {
        self.first_occurrence(needle).is_some()
    }

    fn first_occurrence(&self, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        let (mut lo, mut hi) = (self.num_rows() as usize, self.sa.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.suffix(mid) < needle {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        (lo < self.sa.len() && self.suffix(lo).starts_with(needle)).then_some(lo)
    }

    /// Visit every nonempty substring family once, up to the indexed length.
    ///
    /// Active LCP intervals are nested. A row contributes to exactly those
    /// intervals whose left boundary is after its previous suffix. A small
    /// Fenwick tree increments that suffix of the interval stack; this counts
    /// distinct rows without storing a row set at every node. Its size is
    /// bounded by the maximum needle length, not the dataset size.
    pub(super) fn visit(&self, mut emit: impl FnMut(Family)) {
        struct Interval {
            depth: usize,
            left: usize,
            base: i64,
        }
        let first = self.num_rows() as usize;
        let mut stack = vec![Interval {
            depth: 0,
            left: first,
            base: 0,
        }];
        let mut counts = SuffixCounts(vec![0; self.max_len + 2]);
        let mut previous = vec![-1i32; first];
        let mut previous_len = 0;
        for i in first..=self.sa.len() {
            let depth = if i == self.sa.len() {
                0
            } else {
                self.lcp[i] as usize
            };
            let mut left = i.saturating_sub(1);
            let mut child_rows = 1;
            if i > first {
                let parent = depth.max(stack.last().unwrap().depth);
                if previous_len > parent {
                    emit(Family {
                        position: self.location(left).0,
                        min_len: parent + 1,
                        max_len: previous_len,
                        matching_rows: 1,
                    });
                }
            }
            while stack.last().unwrap().depth > depth {
                let slot = stack.len() - 1;
                let node = stack.pop().unwrap();
                child_rows = counts.at(slot) - node.base;
                left = node.left;
                emit(Family {
                    position: self.location(left).0,
                    min_len: depth.max(stack.last().unwrap().depth) + 1,
                    max_len: node.depth,
                    matching_rows: child_rows as u64,
                });
            }
            if stack.last().unwrap().depth < depth {
                stack.push(Interval {
                    depth,
                    left,
                    base: counts.at(stack.len()) - child_rows,
                });
            }
            if i == self.sa.len() {
                break;
            }
            let (position, row) = self.location(i);
            let already_seen = previous[row];
            let from = stack.partition_point(|node| node.left as i64 <= already_seen as i64);
            counts.increment_from(from);
            previous[row] = i as i32;
            previous_len = (self.offsets[row + 1] as usize - position).min(self.max_len);
        }
    }

    fn write_cache(&self, file: File) -> Result<()> {
        let mut out = BufWriter::new(file);
        out.write_all(CACHE_MAGIC)?;
        out.write_all(&(self.sa.len() as u64).to_le_bytes())?;
        out.write_all(&(self.max_len as u64).to_le_bytes())?;
        out.write_all(self.checksum.as_bytes())?;
        let mut hash = Xxh3::new();
        for &x in &self.sa {
            let b = x.to_le_bytes();
            hash.update(&b);
            out.write_all(&b)?;
        }
        for &x in &self.lcp {
            let b = x.to_le_bytes();
            hash.update(&b);
            out.write_all(&b)?;
        }
        out.write_all(&hash.digest().to_le_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn read_cache(&mut self, path: &Path) -> Result<()> {
        let file = File::open(path)?;
        let n = self.payload.len() + self.num_rows() as usize;
        let expected_len = 24 + self.checksum.len() as u64 + n as u64 * 6 + 8;
        if file.metadata()?.len() != expected_len {
            return Err("invalid SA cache size; remove cache and rebuild".into());
        }
        let mut input = BufReader::new(file);
        let mut header = [0; 24];
        input.read_exact(&mut header)?;
        let mut identity = vec![0; self.checksum.len()];
        input.read_exact(&mut identity)?;
        if &header[..8] != CACHE_MAGIC
            || header[8..16] != (n as u64).to_le_bytes()
            || header[16..24] != (self.max_len as u64).to_le_bytes()
            || identity != self.checksum.as_bytes()
        {
            return Err("SA cache identity mismatch; remove cache and rebuild".into());
        }
        let mut hash = Xxh3::new();
        self.sa = Vec::with_capacity(n);
        for _ in 0..n {
            let mut b = [0; 4];
            input.read_exact(&mut b)?;
            hash.update(&b);
            let p = i32::from_le_bytes(b);
            if p < 0 || p as usize >= n {
                return Err("invalid cached suffix position".into());
            }
            self.sa.push(p);
        }
        self.lcp = Vec::with_capacity(n);
        for _ in 0..n {
            let mut b = [0; 2];
            input.read_exact(&mut b)?;
            hash.update(&b);
            let depth = u16::from_le_bytes(b);
            if depth as usize > self.max_len {
                return Err("invalid cached LCP depth".into());
            }
            self.lcp.push(depth);
        }
        let mut checksum = [0; 8];
        input.read_exact(&mut checksum)?;
        if u64::from_le_bytes(checksum) != hash.digest() {
            return Err("SA cache checksum mismatch; remove cache and rebuild".into());
        }
        Ok(())
    }
}

/// Rank of row separators before an encoded position. About 3 bits/symbol,
/// avoiding a full row-ID array or a binary search for every suffix.
struct RowBoundaries {
    bits: Vec<u64>,
    before: Vec<u32>,
}

impl RowBoundaries {
    fn new(offsets: &[u64], n: usize) -> Self {
        let mut bits = vec![0u64; n.div_ceil(64)];
        for (row, &end) in offsets[1..].iter().enumerate() {
            let p = end as usize + row;
            bits[p / 64] |= 1 << (p % 64);
        }
        let mut count = 0;
        let before = bits
            .iter()
            .map(|&word| {
                let old = count;
                count += word.count_ones();
                old
            })
            .collect();
        Self { bits, before }
    }

    fn row(&self, p: usize) -> usize {
        (self.before[p / 64] + (self.bits[p / 64] & ((1u64 << (p % 64)) - 1)).count_ones()) as usize
    }
}

/// Difference Fenwick tree: increment a suffix of stack slots, query one slot.
/// New intervals subtract the accumulated value when occupying a reused slot.
struct SuffixCounts(Vec<i64>);
impl SuffixCounts {
    fn increment_from(&mut self, slot: usize) {
        let mut i = slot + 1;
        while i < self.0.len() {
            self.0[i] += 1;
            i += i & i.wrapping_neg();
        }
    }
    fn at(&self, slot: usize) -> i64 {
        let (mut i, mut sum) = (slot + 1, 0);
        while i > 0 {
            sum += self.0[i];
            i &= i - 1;
        }
        sum
    }
}
