# Shared needle generation

`lb_harness::gen` provides two generators. `generate` preserves the existing
sampled, multi-operation generator. `SubstringIndex` discovers row-bounded
substrings with a suffix array and LCP array, then generates balanced CONTAINS
suites. Experiments depend on the harness with `default-features = false` to
avoid building unrelated codecs and scanners.

```rust,ignore
use lb_harness::gen::{BalancedRequest, IndexLimits, SubstringIndex};

let index = SubstringIndex::new(
    dataset.payload(), dataset.offsets_u64(), IndexLimits::default(),
)?;
let mut request = BalancedRequest::new(dataset.num_rows(), 42);
request.per_cell = 20;
let generated = index.generate(&request)?;
```

Use `SubstringIndex::cached` to persist SA/LCP arrays. Changing the seed, grid,
or quota reuses the index; changing row boundaries, content, maximum indexed
length, or format invalidates it. The cache checksum detects damaged arrays.
Caching the index does not skip the subsequent traversal and row counting.

## Semantics and coverage

Selectivity is **matching rows / total rows**: repeated occurrences within one
row count once, duplicate rows each count, and empty rows remain in the
denominator. Buckets store inclusive integer row-count bounds so rounding cannot
create overlaps. The defaults separate zero and one match, then span logarithmic
rare-match buckets and broader buckets through 100%.

The default length buckets are 1–4, 5–8, 9–16, 17–32, 33–64, 65–128 and
129–256 **bytes**, with 20 distinct needles per cell. `BalancedRequest` can supply
different disjoint buckets and quotas. Bytes need not form valid UTF-8.

Every positive substring belongs to exactly one SA/LCP family and one length
within that family. Singleton suffixes and lengths between LCP branch depths
are included. The traversal counts all eligible substrings, while selection
retains at most four times the quota in interval spans per cell. It then prefers
underrepresented lengths near evenly spaced targets. This is diagnostic,
stratified sampling, **not occurrence-uniform or substring-uniform sampling**;
the retained spans can limit the length diversity within an otherwise full cell.

No duplicate byte strings are emitted anywhere in a suite, including across
cells and the negative bucket. Each suite depends only on its dataset, generator
version, request and seed. Generating another dataset or experiment has no effect:
the same needle may legitimately occur in multiple independent suites.

`request.suite_key(dataset_checksum)` provides a name containing the generator
version, seed, quota and a fingerprint of the complete request and dataset.
Experiments can use it to keep different generations in separate directories;
the `bench gen --out` command continues to use the explicit path supplied by
the caller. An existing suite is replaced only when `--force` is requested.

The zero-match bucket uses single-byte substitutions of observed substrings,
with replacement bytes drawn from the dataset's alphabet. Every mutation is
checked for exact absence in the row-bounded index. Source offsets and mutation
positions are retained as witnesses. Near-misses encourage realistic work, but
do not guarantee expensive verification for a particular codec or prefilter.
One-byte negatives cannot exist under this observed-alphabet restriction.

Each requested cell is reported as:

- `filled`: the requested number of distinct needles was emitted;
- `exhausted`: the full positive catalogue contains fewer eligible needles;
- `unresolved`: bounded negative search did not fill the quota.

There is no implicit dataset sampling or duplication to fill missing cells.
Resource failures return an error instead of claiming a bucket is impossible.

## Index implementation

The index uses the pinned `libsais-rs` implementation. Bytes map to u16 symbols
1..=256 and row separators to zero, supporting all 256 byte values. Ordinary SA
construction permits consecutive separators for empty rows; suffixes tied up to
their row end may be ordered by subsequent rows, which does not affect nonempty
substring intervals. The PLCP computation stops at separators.

An LCP stack emits ranges of lengths with the same occurrences. Distinct-row
counts use the previous suffix rank of each row: that row is new to precisely
the active intervals whose left boundary follows that rank. A difference
Fenwick tree updates that suffix of the stack in logarithmic time in the maximum
needle length. This avoids storing a document set at every interval. Tiny-case
tests compare the complete catalogue with independent exhaustive enumeration.

The backend supports at most `i32::MAX` encoded symbols (payload bytes + rows).
LCP depths are capped at the requested maximum, up to 65535. The default index
admission budget is 4 GiB, using a conservative `16 * encoded_symbols + 64 MiB`
workspace estimate; this excludes the loaded Arrow dataset and is not an OS RSS
cap. Retained SA/LCP arrays occupy roughly 6 bytes per encoded symbol, plus row
boundary ranks. Large datasets require explicitly increasing the budget; caches
also require roughly 6 bytes per encoded symbol on disk.

`IndexLimits::required_memory_bytes(payload_bytes, num_rows)` exposes the same
admission estimate and backend-size check without allocating an index. The
matcher-selection experiment uses a separate, configurable 16 GiB budget.

## CLI and independent verification

From the repository root:

```sh
cargo build -p lb-harness --release --no-default-features --bin bench
target/release/bench gen --method suffix-array --dataset datasets/my-dataset \
  --out /tmp/my-suite --seed 42 --per-cell 20 --max-needle-len 256 \
  --index-cache /tmp/needle-index-cache
target/release/bench bless --suite /tmp/my-suite --dataset datasets/my-dataset
```

Generated suites contain no benchmark truth. `bench bless` runs the existing
independent row oracle, then checks SA counts, bucket assignments, uniqueness,
and mutation witnesses. Library consumers call `suite::bless` followed by
`gen::verify_balanced_suite`. The report is `gen-report.json`; the regular
`suite.json` and `queries.jsonl` work with other experiments and `bench run`.

Background: [Yamamoto and Church, 1998](https://aclanthology.org/W98-1104.pdf)
describes suffix-array substring families and the distinction between occurrence
frequency and document frequency. The backend is
[libsais-rs](https://github.com/henriksson-lab/libsais-rs).
