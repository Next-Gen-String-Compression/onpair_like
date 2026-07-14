# Upstream bug: Vortex FSST Teddy scan — false positives on short contains needles

**Status:** blocks the `fsst_spiral` candidate (held, not wired into any gated spec).

## Summary

On the Vortex branch `ji/fsst-like-paper-2-work-clean`
(`346dfdf3cc342ff47b86b40aeb1e4703925427db`), `FsstMatcher::scan_to_bitbuf`
returns **false positives** for short `%needle%` contains patterns: it reports
matches on rows that do **not** contain the literal needle. The per-row
`FsstMatcher::matches()` on the *same* codes returns the correct answer, so the
bug is in the SIMD streaming scan (the `FoldedContains` Teddy pair-anchor path),
not in the DFA itself.

- Encoder: `fsst-rs 0.5.10` (the version Vortex's own `Cargo.lock` pins).
- Reproduced independently of this benchmark harness (unit test below).

## Evidence

Needle `" l"` (space + `l`) over the msmarco-query column (1,010,916 rows):

```
scan_to_bitbuf got_count = 152022
literal truth    count   = 151985
false_positives          = 37      (0 false negatives)
```

Every false-positive row has `scan_to_bitbuf = true` **but `matches() = false`**
(matches() is correct). All are near-misses where `l` is preceded by a byte
other than space:

| row | bytes | why it's a false positive |
|----:|-------|---------------------------|
| 242 | `what is process improvement (lean six sigma)` | `(l`, not ` l` |
| 7247 | `what is a wetland? (landforms)` | `(l` |
| 11917 | `what age will my baby weigh 33lbs` | `3l` |
| 17876 | `...organic carbon compounds (like glucose)...` | `(l` |
| 19961 | `victoza (liraglutide)` | `(l` |
| 42276 | `how to cook a 3lb pork shoulder roast` | `3l` |
| 84424 | `define oxbow (lake)` | `(l` |

The prefilter fires on a candidate (`(l`, `3l`) whose first byte differs from the
needle's first byte, and the streaming verify fails to reject it — while the
per-row DFA verify (`matches()`) rejects it correctly.

## Minimal reproduction

`candidates/fsst_spiral/src/lib.rs` module `isolation` (both `#[ignore]`d):

```sh
cargo test -p lb-cand-fsst-spiral --release -- --ignored --nocapture
```

- `roundtrip_matches_symbol_table` — **passes**: `compress()` round-trips through
  a `Decompressor` built from the same `symbol_table()`/`symbol_lengths()` handed
  to the matcher. Rules out any inconsistency in how codes/symbols are produced.
- `contains_space_l_reproduces_false_positives` — **fails**: 37 false positives,
  each with `scan_to_bitbuf=true` / `matches()=false`.

Standalone sketch (no harness):

```rust
use fsst::Compressor;
use vortex_fsst::dfa::FsstMatcher;

let comp = Compressor::train(&rows);                 // rows: &[&[u8]] of the column
let symbols = comp.symbol_table().to_vec();
let lengths = comp.symbol_lengths().to_vec();

let mut all = Vec::new();
let mut offs = vec![0u32];
for r in &rows { all.extend_from_slice(&comp.compress(r)); offs.push(all.len() as u32); }

let m = FsstMatcher::try_new(&symbols, &lengths, b"% l%").unwrap().unwrap();
let bits = m.scan_to_bitbuf(rows.len(), &offs, &all, false);
// bits.value(i) == true for rows containing "(l"/"3l" but NOT " l";
// m.matches(&comp.compress(rows[i])) == false for those same rows.
```

## Likely area

`encodings/fsst/src/dfa/folded_contains.rs` (`scan_to_bitbuf` / Teddy-2 bucketed
pair scan) + `encodings/fsst/src/dfa/anchor_scan.rs`. The candidate position from
the SIMD pair prefilter appears to be verified incorrectly (wrong offset, or the
verify is skipped), so a first-byte mismatch survives.

## Impact on the benchmark

`fsst_spiral`'s `teddy` strategy cannot pass the correctness gate on short-needle
(L2) contains cells until this is fixed. The candidate crate, harness wiring
(feature `cand-fsst-spiral`), specs, and this repro are kept so it can be
re-validated against a fixed Vortex ref by re-pinning the `rev` in `Cargo.toml`.
