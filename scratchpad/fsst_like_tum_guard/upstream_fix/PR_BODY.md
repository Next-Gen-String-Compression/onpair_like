## Summary

Two commits, one root cause: the matchers decide whether a byte is a code or the escaped literal of an escape pair by looking at **one** byte, the byte before it. That is wrong in two ways, and both produce false negatives on data that contains a raw `0xFF` (which FSST escapes as `255 255`).

### 1. The backward scan reads the byte before the row (`fix: stop the backward scan at the row start ...`)

The suffix loop condition is `*strIdx + 1 >= level` in all three kernels (interpreted `parse<false>`, C++ codegen, LLVM codegen). For a level-0 pseudo-end state it still holds at `strIdx == -1`, so the loop reads `data[-1]`. Rows are stored back to back by `file::dumpBinary`, so that is the previous row's last byte; when the previous row ends in an escaped `0xFF` the scan treats the match's first code as escaped and reports *no match*. The reference oracle in `test/EndPatternMatchingTest.cpp` already guards with `backwardsStrIdx >= 0`; the kernels now do the same.

### 2. One byte cannot tell a marker from an escaped `255` (`fix: decide escapes by the parity of the 255 run ...`)

`255` is the escape marker unless it is itself the escaped `255` literal of the pair before it. Every non-`255` byte ends a token, so what decides it is the parity of the run of `255` bytes in front of the byte: even = code, odd = escaped literal.

- **Suffix automaton** (`percentage.cpp`): each pseudo-end went to error on the first `255`. It now alternates with an "odd" state on `255` (even run: accept as before, odd run: error). Fixes `"x\xFFy" LIKE '%y'`, `"\xFFe" LIKE '%e'`, and a needle of only `0xFF` bytes against a `255` run, on every backend.
- **Middle-start scan loops** (C++ and LLVM code generators): the scalar loop skipped a candidate when `prevByte == 255`, the SIMD path when `compressed[i-1] == 255`, and on re-entry `prevByte` was seeded from `data[strIdx-1]`, which after the sink state consumed a literal `255` is that literal. The loops now track whether they stand on a literal, seeded by walking back over the `255` run (`isEscapedLiteral`, emitted once per kernel/module), and the SIMD candidate check uses the same helper. Fixes `"\xFF\xFF\xFFthe" LIKE '%he%'` on the four code-generated backends. The interpreter's forward path already handled escapes through the sink state and is unchanged.

## When it triggers

The corpus must contain raw `0xFF` bytes that FSST escapes (no `0xFF` in its training sample). Valid UTF-8 never contains `0xFF`, so ASCII/UTF-8 columns are immune; binary or Latin-1 data is not. None of the corpora under `data/` contain a raw `0xFF`, which is why the tests never see it.

## Validation

A standalone driver compresses a synthetic corpus, places every compressed row flush against `PROT_NONE` guard pages, sweeps the byte before/after the row, and checks every backend (interpreted, cpp, cpp-simd, llvm, llvm-simd) against a substring oracle.

| corpus | metric | upstream `b1eb3ab9` | this branch |
|---|---|---|---|
| synthetic, 82 patterns × 5 backends | reads before the row | 42 cells | 0 |
| synthetic | answer flipped by a `0xFF` before the row | 50 cells | 0 |
| synthetic | false negatives from escaped `0xFF` (suffix) | 15 cells | 0 |
| in-row corpus (`\xFFe`, `q\xFFe`, `\xFF\xFFe`, `\xFF\xFF\xFFthe` before `%e`/`%the`/`%he%` …) | problem cells | 39 | 0 |

No false positives anywhere; per-pattern match counts are identical to the oracle's. The cells still reported on the synthetic corpus are patterns ending in a literal backslash before `%` (a pattern-parser limitation, unrelated) and the one below.

## Not covered

The LLVM emitter loads `data[strIdx]` at the top of the forward loop body before switching on the state, so a scan that reaches an accept/error state at `strIdx == len` still loads one byte past the row. The value is unused, so it is memory safety only (invisible on an `mmap`'d file with the offsets array behind the buffer).
