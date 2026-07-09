# Contract semantics

Companion to `lb_candidate.h`. Everything here is binding on candidates,
scanners, and the harness oracle alike; the oracle's fixture tests encode
this document. ABI version: 4 (v2 added the `LB_DECODE_PAD` guarantee on
`decode()` output buffers; v3 added `lb_run_stats.setup_ns` — self-timed
per-query setup such as automaton compilation, instrumented mode only; v4
added the optional `lb_scanner.supports_query` per-query capability probe).

## Data model

- Rows are **byte strings** — no UTF-8 requirement, no case folding, no
  collation. Matching is byte-exact and case-sensitive.
- A chunk view is `(bytes, offsets, num_rows)`: row *i* is
  `bytes[offsets[i] .. offsets[i+1])`. `offsets` has `num_rows + 1` entries,
  is non-decreasing, and is rebased so `offsets[0] == 0`.
- Empty rows are legal. There are no nulls (removed at ingest).

## Operations

Needles are ordered byte strings; arity is validated by the harness before
any candidate sees the query.

| op | arity | row matches iff |
|---|---|---|
| `LB_PREFIX` | 1 | the row's first `len(n)` bytes equal `n` |
| `LB_SUFFIX` | 1 | the row's last `len(n)` bytes equal `n` |
| `LB_CONTAINS` | 1 | `n` occurs at some position in the row |
| `LB_MULTI_CONTAINS` | ≥ 1 | needles occur **in order, non-overlapping**: scanning left to right, needle *k*'s match begins at or after the end of needle *k−1*'s match (equivalently: greedy leftmost matching with the search position advancing past each match) |
| `LB_CONTAINS_ANY` | ≥ 1 | at least one needle occurs in the row |

### Edge cases (normative)

- An **empty needle matches every row**, including empty rows. In
  `LB_MULTI_CONTAINS` an empty needle matches at the current position and
  advances it by 0.
- A needle **longer than the row** matches nothing.
- **Duplicate needles** in `LB_MULTI_CONTAINS` require distinct sequential
  occurrences (`["ab","ab"]` needs `ab` twice, non-overlapping).
- Duplicate needles in `LB_CONTAINS_ANY` are equivalent to one occurrence
  of that needle.
- `LB_MULTI_CONTAINS` uses **greedy leftmost** semantics: each needle takes
  its earliest possible match. (For these patterns — `%a%b%c%` — greedy
  leftmost succeeds iff any assignment succeeds, so this is equivalent to
  SQL LIKE and merely pins down the reference algorithm.)

## Result bitmap

- The output of `run()`/`scan()` is a harness-owned, **pre-zeroed** bitmap
  slice of `ceil(num_rows / 64)` little-endian `uint64_t` words.
- Row *i* of the chunk ⇒ bit `i % 64` (LSB-first) of word `i / 64`.
- Set bits for matching rows only; never clear or read bits. Padding bits
  past `num_rows` must remain zero.
- Truth hashing (`bitmap-xxh3-v1`): xxh3-64 over the words of the
  **whole-dataset** bitmap serialized as little-endian bytes, padding bits
  zeroed. Chunking never changes the global bitmap, so truth is
  chunk-invariant.

## Candidate & scanner hygiene rules

1. **No memoization across calls.** Every `run()`/`decode()`/`scan()` call
   pays its full cost. Scratch *allocations* may persist between calls
   (steady-state realism); scratch *contents* must not carry information
   between calls.
2. **No threads.** Phase 1 measures single-threaded, core-pinned latency;
   spawning threads is a contract violation.
3. **Do not replace the global allocator.**
4. **Views are read-only.** Do not write through `lb_chunk_view` pointers;
   do not retain them past `destroy()` (candidates) or `release()`
   (scanners).
5. **Timing vs instrumented mode must be indistinguishable** except for the
   `stats` pointer: identical work, identical results. In timing mode
   (`stats == NULL`) do no bookkeeping whatsoever.
6. **Strategy names `direct` and `decode` are reserved** for
   harness-composed strategies; candidates must not declare them.
7. **Declare `cpu_features` honestly.** A module whose kernels require an
   ISA extension must list it; silently running scalar fallback code on a
   host that lacks the extension is a contract violation (the harness
   hard-gates instead).
8. **`decode()` headroom.** The harness guarantees `bytes_cap ≥ chunk
   payload + LB_DECODE_PAD`. A decoder may write anywhere in
   `[0, bytes_cap)` while it works (over-copy optimisations are welcome —
   that is what the pad is for), but only `[0, payload)` carries meaning
   when it returns, and it must never write past `bytes_cap`.
9. **`supports_query` is a pure capability gate.** A scanner may implement
   the optional `supports_query` probe to reject queries outside its
   algorithmic envelope (needle too long for a bit-parallel word, too many
   literals for a packed engine, an op it does not implement per-query).
   Returning 0 marks the cell `Unsupported`, never `Error`; a scanner must
   return 0 here rather than degrade to a different algorithm on the timed
   path (that would make the measurement not-that-scanner). It must not
   allocate or mutate state, and it must agree with `prepare()`: for any
   query it accepts, `prepare()` may fail only on genuine resource
   exhaustion, not on capability.

## Error convention

`build()`/`prepare()` return NULL on failure (`build` fills `err_buf` with a
NUL-terminated message). All `int` entry points return 0 on success,
nonzero on failure; a nonzero return marks that cell errored — it is not a
correctness failure, but the cell reports no numbers.
