//! Result bitmaps and the canonical truth hash (`bitmap-xxh3-v1`).
//!
//! Bit i (LSB-first within little-endian u64 words) is set iff row i
//! matches. The truth hash is xxh3-64 over the whole-dataset bitmap's
//! words serialized little-endian, padding bits zeroed — chunk-invariant
//! by construction (see contract/SEMANTICS.md).

use xxhash_rust::xxh3::Xxh3;

pub const TRUTH_ALGO: &str = "bitmap-xxh3-v1";

#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap {
    words: Vec<u64>,
    num_rows: u64,
}

impl Bitmap {
    pub fn new(num_rows: u64) -> Self {
        Self {
            words: vec![0u64; lb_abi::bitmap_words(num_rows)],
            num_rows,
        }
    }

    pub fn num_rows(&self) -> u64 {
        self.num_rows
    }

    pub fn words(&self) -> &[u64] {
        &self.words
    }

    pub fn words_mut(&mut self) -> &mut [u64] {
        &mut self.words
    }

    pub fn zero(&mut self) {
        self.words.fill(0);
    }

    #[inline]
    pub fn set(&mut self, i: u64) {
        debug_assert!(i < self.num_rows);
        self.words[(i >> 6) as usize] |= 1u64 << (i & 63);
    }

    #[inline]
    pub fn get(&self, i: u64) -> bool {
        (self.words[(i >> 6) as usize] >> (i & 63)) & 1 == 1
    }

    pub fn count(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }

    /// Zero any bits past `num_rows` so hashes are canonical even if a
    /// buggy plugin scribbled on padding.
    pub fn clear_padding(&mut self) {
        let tail = self.num_rows & 63;
        if tail != 0 {
            if let Some(last) = self.words.last_mut() {
                *last &= (1u64 << tail) - 1;
            }
        }
    }

    /// Canonical truth hash, `"xxh3:<16 hex>"` under `bitmap-xxh3-v1`.
    pub fn truth_hash(&self) -> String {
        debug_assert!(self.padding_is_zero());
        let mut h = Xxh3::new();
        for w in &self.words {
            h.update(&w.to_le_bytes());
        }
        format!("xxh3:{:016x}", h.digest())
    }

    fn padding_is_zero(&self) -> bool {
        let tail = self.num_rows & 63;
        tail == 0 || self.words.last().is_none_or(|w| w >> tail == 0)
    }

    /// Row index of the first bit where the two bitmaps differ.
    pub fn first_divergence(&self, other: &Bitmap) -> Option<u64> {
        for (i, (a, b)) in self.words.iter().zip(&other.words).enumerate() {
            let diff = a ^ b;
            if diff != 0 {
                return Some((i as u64) * 64 + diff.trailing_zeros() as u64);
            }
        }
        None
    }

    /// First `n` set-bit indices (debugging aid stored as truth samples).
    pub fn first_indices(&self, n: usize) -> Vec<u64> {
        let mut out = Vec::with_capacity(n);
        for i in 0..self.num_rows {
            if self.get(i) {
                out.push(i);
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_count() {
        let mut b = Bitmap::new(130);
        b.set(0);
        b.set(63);
        b.set(64);
        b.set(129);
        assert!(b.get(0) && b.get(63) && b.get(64) && b.get(129));
        assert!(!b.get(1) && !b.get(128));
        assert_eq!(b.count(), 4);
        assert_eq!(b.first_indices(3), vec![0, 63, 64]);
    }

    #[test]
    fn hash_is_deterministic_and_content_sensitive() {
        let mut a = Bitmap::new(100);
        let mut b = Bitmap::new(100);
        a.set(42);
        b.set(42);
        assert_eq!(a.truth_hash(), b.truth_hash());
        b.set(43);
        assert_ne!(a.truth_hash(), b.truth_hash());
        assert!(a.truth_hash().starts_with("xxh3:"));
    }

    #[test]
    fn divergence() {
        let mut a = Bitmap::new(200);
        let mut b = Bitmap::new(200);
        assert_eq!(a.first_divergence(&b), None);
        a.set(150);
        assert_eq!(a.first_divergence(&b), Some(150));
        b.set(150);
        b.set(3);
        assert_eq!(a.first_divergence(&b), Some(3));
    }

    #[test]
    fn padding_cleared() {
        let mut a = Bitmap::new(65);
        a.words_mut()[1] = u64::MAX; // scribble on padding
        a.clear_padding();
        assert_eq!(a.words()[1], 1);
    }
}
