# Substring prefilter for FSST code streams

Companion for porting onpair's Rust substring prefilter to C++ over FSST.
Two Rust sources are referenced:

| name | where | what it is |
| --- | --- | --- |
| **new** | `onpair` branch `feat/prune-factor-superset` (2026-09-15), `src/search/substring/prefilter/` | the clean version; u16 codes only; the primary reference for structure, graph, planner, walk, resolvers |
| **old** | `onpair` commit b081914, `src/search/prefilter/` | has what the port needs and the new branch dropped: u8-lane kernels, the universal `nibble` bitmap, u8 cost rows, FSST escape handling |

Paths below are relative to the new branch's `src/search/substring/prefilter/`
unless marked `old:`. Sections 3 and 9 hold what FSST adds that neither Rust
version handles.

```
needle bytes ──► graph ──► two min cuts ──► cover ──► scan ──► rows
                  │                                          ▲
                  └──────────────── walk ────────────────────┘  (exact check per hit)
```

The scan answers `LIKE '%needle%'` over a column of FSST-compressed strings
without decoding. The planner turns the needle into a *cover*: a set of codes
such that every row containing the needle holds at least one of them. The
scan finds the rows holding a covered code (stage one: codes to bit mask,
stage two: mask to rows). The walk checks each hit against the alignment
graph in the code domain, so the rows come out exact. `prefilter_superset`
is the same scan without the walk, for a caller that verifies by decode plus
memmem.

## 1. Terms

| term | meaning |
| --- | --- |
| needle | the pattern bytes, `n = needle.len()`; the empty needle matches every row and skips everything below |
| token, symbol | a dictionary entry, 1..MAX_TOKEN_SIZE bytes. onpair: 16. FSST: 1..8 |
| code | one stream element naming a token. onpair new: u16. FSST: one byte, 0..254 |
| row | one string, `codes[row_offsets[r]..row_offsets[r+1])` |
| cover | codes to probe for, as a point list plus a range list, disjoint |
| point, range | a single covered code, or an inclusive run `begin..=last` |
| hit | a stream index whose code is covered |
| K, R | points and ranges in the cover |
| layout | one way an occurrence lies across token boundaries |

## 2. File map

| file | holds | port |
| --- | --- | --- |
| `mod.rs` | `analyze_prefilter`, `prefilter_candidates` (exact), `prefilter_superset`, `PrefilterAnalysis`, empty-pattern handling | port; no pattern length limit, the walk's node ids are u32 |
| `graph.rs` | alignment DAG builder, candidate sweep, greedy parse, `ProbeCover::from_edge_cut` | port with FSST changes (§4, §9) |
| `mincut.rs` | Dinic max-flow and cut extraction, one `min_cut` per cut | port as is |
| `plan.rs` | the two cuts, `cheapest_cover`, `cover_frequency`, `plan` shared with the trace | port |
| `cover.rs` | `ProbeCover::from_runs` | port |
| `scan/mod.rs` | `BLOCK`, block driver, `Check`, `Facts` construction, `both_stages`, `Isa` | port |
| `scan/policy/mod.rs` | matcher selection, skip threshold, fitted cost rows, `scan_ns` | port; u8 rows from old |
| `scan/matcher/{eq_or,range,nibble_n8,shared}.rs` | u16 kernels on NEON, AVX2, AVX-512 | port the u8 counterparts from old; `table.rs` is not ported (§6.3) |
| `old: scan/novel/matcher/nibble.rs` | universal 16×16 bitmap for u8 codes | port; FSST keeps it |
| `old: scan/novel/matcher/{eq_or,range,nibble_n_8}.rs` | the same kernels with `u8` impls | port the u8 impls |
| `scan/resolver/mod.rs` | `GallopSeek`, the one resolver, generic over the offset width | port |
| `scan/walk.rs` | `Walk`, non-branching forward and backward | port with FSST changes (§7, §9) |
| `scan/dispatch.rs` | plan to template instantiation | port |
| `scan/README.md`, `old: scan/novel/README.md` | rates, model derivation, instruction counts | reference; the old one has the u8 tables |
| `tests.rs`, `scan/matcher/tests.rs`, `scan/resolver/tests.rs` | oracles | port the strategies (§10) |
| `scan/bench/` | sweeps and fits | skip |
| `old: src/fsst/encode.rs`, `old: src/fsst/tests.rs` | FSST symbol table sorted into a dictionary, stream remapped | replaced by `fsst_sort_codes` in the compressor (§3) |

Gone from new and not wanted in the port: the fused kernels (`old:
scan/{byte,aarch64,x86,generic}`, `sink.rs`), `naive`, `Pattern` and
`LOOKAHEAD`, sequence probes, `Exactness` and `PrefilterError`.

## 3. FSST as the dictionary

| planner needs | onpair | FSST |
| --- | --- | --- |
| token bytes and length by id | sorted `CompactDictionary` | `fsst_decoder_t::symbol[c]` (u64 little-endian), `len[c]`, `c < nSymbols` |
| `MAX_TOKEN_SIZE` | 16 | 8; the u128 `NeedlePrefix` in `walk.rs` becomes a u64 against the symbol word |
| tokens a byte string is a prefix of (`prefix_range`) | binary search, contiguous id range | linear scan over ≤255 symbols; contiguous `CodeRange` because the codes are sorted (`Dictionary::prefix_range`) |
| longest token that is a prefix of `needle[o..]` (`greedy_in_needle`) | narrowing binary search | linear scan, at most 255 × 8 compares |
| tokens ending with `needle[..k]`, tokens containing the needle (`alignment_candidates`) | one memmem sweep over the payload | linear scan; the 512 and 16 caps of §4 still apply |
| term frequency per token | cumulative `u32[num_tokens+1]` | `count[256]` plus `cum[257]` over the stream, one pass; a range's frequency is one subtraction |
| code width | u16 (new), u8 or u16 (old) | u8 |

**Codes.** A compressed FSST string is bytes 0..254 naming symbols, and 255
followed by one literal byte. `fsst_import` fills unused codes with an 8-byte
"corrupt" symbol and does not store the version word, so `nSymbols` comes
from byte 1 of the serialized header, or from summing `lenHisto` at header
bytes 9..16, or by recognising the corrupt fill. With `zeroTerminated`, code
0 is the one-byte symbol `\0`.

**Sorted id space, written by the compressor.** onpair sorts the symbols
and rewrites the code stream to sorted ids (`old: transcode_fsst_to_onpair`),
so a prefix search yields a contiguous id range. This branch gets the same
from the compressor: `fsst_sort_codes(encoder)` (call once after
`fsst_create`) fills a 256-entry permutation `perm[code]` = the symbol's rank
in byte order, a symbol before its extensions, 255 staying the escape.
`compressBulk` applies it at its write sites and nowhere else, so FSST's
internal numbering by length and the `suffixLim`/`byteLim` fast paths are
untouched. `fsst_export` sets bit 1 of header byte 8 and writes the symbols
in the internal order; `fsst_import` sorts them into rank order when the bit
is set, so `fsst_decoder_t::symbol[c]` is sorted by `c` and no separate
table travels with the data. The planner asserts `Dictionary::sorted()`; a
`Range` edge is one `CodeRange` and one range in the cut weight.

**Stream and rows.** `fsst_compress` writes strings back to back into
`output` with `strOut[i]` pointing at each; `row_offsets[i] = strOut[i] -
output`, `row_offsets[n] = total`, u32. Empty rows share an offset with
their successor. The new resolvers read u32 or u64 offsets in place.

**The parse the graph assumes.** The cover is sound only if the encoder's
parse of any occurrence is one of the graph's paths, which needs greedy
longest-match restricted to what the encoder can see. FSST (`compressBulk`
in `libfsst.cpp`) takes the symbol of length ≥ 3 from `hashTab` if its bytes
match, else the 2-byte symbol from `shortCodes`, else the 1-byte symbol,
else escape. Symbols of length ≥ 3 have distinct 3-byte prefixes (one per
hash bucket, `hashInsert` refuses collisions), so at most one can match at a
position and the result is greedy longest-match. The `noSuffixOpt` and
`avoidBranch` variants produce the same bytes. Two things differ from
onpair:

1. **Escapes.** FSST escapes any byte without a 1-byte symbol, and every
   table trained on real data lacks some bytes. The new graph panics on such
   a dead end (`build_state`, `IncompleteAlphabet`); the old one added a
   `Point(255)` step into the sink and its walk refused the graph. §9 has
   the escape-aware version the port uses.
2. **511-byte chunks, removed.** Upstream FSST cut every string into
   chunks of 511 bytes and parsed each chunk from scratch, so a token never
   spanned a chunk boundary and the parse of an occurrence straddling byte
   511·m was not greedy over the occurrence. This branch compresses each
   string whole and no longer dispatches to the AVX-512 compressor, whose
   output is chunked. Streams written by upstream FSST or by the AVX-512
   path are outside the prefilter's soundness guarantee.

**Literal bytes in the stream.** The byte after a 255 is raw text at a code
position. The matcher cannot tell it from a code, so a literal equal to a
covered code is a false hit. For the superset that only costs a candidate.
For the exact walk it is a correctness hole (§7, §9): the walk must know
whether a position is a token or a literal. A position `i` is a literal iff
the maximal run of consecutive 255s ending at `i-1` has odd length; a 255 at
`i` is an escape marker iff that run has even length. The run before a
non-255 code starts at a unit boundary either way, so parity decides.

## 4. Alignment graph (`graph.rs`)

Nodes are needle offsets: node `o` is the parse position at `needle[o..]`,
node 0 the source, node `n` the sink. `n + 1` nodes, at most `2n + 16`
edges. An edge `o -> o'` is one token covering `needle[o..o']` and carries a
probe set plus that set's term frequency.

| probe | edge | meaning |
| --- | --- | --- |
| `Point(t)` | `o -> o + len(t)`, `o + len(t) < n` | interior token of the greedy parse at `o` |
| `Range(r)` | `o -> sink` | tokens `needle[o..]` is a prefix of; the occurrence ends inside one |
| `Set(ids)` | `source -> k`, `k ≥ 1` | tokens whose last `k` bytes are `needle[..k]`; alignment `k` starts inside such a token |
| `Set(ids)` | `source -> sink` | tokens containing the whole needle at offset > 0 |
| `SetTooBig` | `source -> k` | more tokens qualified than the cap; never materialised; a cut may not select it |

Alignment 0 needs no entry edge; its layouts start at the source. Caps:
`PROBE_SET_SIZE_LIMIT = 512` for `k ≥ 2`, `PROBE_SET_SIZE_LIMIT_ALIGNMENT_1 = 16`
for `k = 1`, because nearly every byte ends more than 16 tokens. A k=1 pass
that passes 16 sets `count[1] = 513` so the set reads as too big. With ≤ 255
FSST symbols only `k = 1` can ever be too big.

```
kmax = min(n, MAX_TOKEN_SIZE)
(first[k] for k in 1..kmax, contained) = alignment_candidates(dict, needle)
for k in 0..kmax:
    if k != 0 and first[k] is empty: continue
    ensure_chain(k)
    if k != 0: add_edge(source, k, Set(first[k]) if count[k] <= 512 else SetTooBig)
if contained not empty: add_edge(source, sink, Set(contained))

ensure_chain(start):                      # iterative, memoised by built[o]
    o = start
    while not built[o]:
        built[o] = true
        next = build_state(o)
        if next >= n: break
        o = next

build_state(o):
    if n - o <= MAX_TOKEN_SIZE:
        r = prefix_range(dict, needle[o..])
        if r not empty: add_edge(o, sink, Range(r))
    g = greedy_in_needle(dict, needle[o..])         # longest token that is a prefix of the suffix
    if g = (token, len):
        if o + len < n: add_edge(o, o + len, Point(token))   # o + len == n is inside r
        return o + len
    else: add_edge(o, o + 1, Escape(byte = needle[o]))  # probe code 255; new panics here, §9
```

`alignment_candidates`, new version, two passes over the token payload:

- `alignment_k1_candidates`: tokens of length ≥ 2 whose last byte is
  `needle[0]`, stopping after 16 and marking too big. Skipped for `n == 1`.
- `sweep_for_candidates`: memmem for `needle[..min(2, n)]` over the payload.
  For each hit inside token `t`, with `tail = payload[hit..token_end]`:
  skip if `tail.len() < min(2, n)`; if `tail` starts with the needle and the
  hit is not at the token's start, push `t` to `contained` and resume the
  search at `token_end + 1 - n`; else if the hit is not at the token's
  start, `tail.len() < n` and the needle starts with `tail`, record
  alignment `k = tail.len()`. A hit at a token's start is alignment 0's or
  the terminal range's and is skipped.
- Counts saturate at 513.

`ProbeCover::from_edge_cut(cut)`: every point and set member becomes a run
of one, every range itself, then `from_runs` sorts by `begin`, merges runs
that overlap or abut, and files single-code runs as points and the rest as
ranges. No membership table exists any more.

**Pair probes (C++ only, `CutGraph::with_pairs` in `graph.hpp`).** The
min cut runs on an unrolled copy of the graph: every interior node is
copied once per edge entering it, so a copy knows the symbol that led to
it. A `Point(b)` edge out of a copy entered by `Point(a)` becomes two
edges in series, `Point(b)` then `Pair(a, b)`; a path through them is
blocked by either, so the cut picks the cheaper and every cut stays a
sound cover. Escape, Range and Set edges are copied as they are. A pair's
frequency is the independence estimate `count[a]·count[b]/total`
(first decision 2026-09-15), replaced the same day by the exact count
`Frequency::pairs[a·256+b]` after the estimate misled the cut; the balanced
cut weighs it at eq_or's fitted pair slope (§5). The cover gets `pairs`, the bit
lands on the first code, only `eq_or` compares pairs (`eq(v, a) & eq(v+1,
b)` with a lagged load, the driver keeps one code past every block
readable), and the walk is unchanged since the hit is a greedy step. The
stream's last code pairs with padding; `both_stages` resets that bit to
the plain membership test.

## 5. Min cut and the two cuts (`mincut.rs`, `plan.rs`)

**Min cut.** Dinic on `n + 1` nodes with CSR adjacency, built per cut. A
cuttable edge's capacity is its weight; `SetTooBig` gets `sum(finite
weights) + 1`. Max flow must stay below that (assert: otherwise a path has
no probe). The cut is every cuttable edge with `level[from] >= 0 &&
level[to] < 0` after the last BFS. Building the CSR costs about one solve,
so rebuilding it for the second cut adds 1% of plan time at 64 bytes and 4
to 6% at 4 KB (onpair, 2026-10-18).

**Two cuts.** A minimum cut is linear in its edge weights and the scan's
cost is not: a kernel takes eight codes for the price of one, and a range
costs the nibble table nothing. So two cuts stand in for it, one per end:

```
selective(e) = frequency(e)
balanced(e)  = VERIFY_NS_PER_HIT · frequency(e)
             + codes · (t · points(e) + q · ranges(e) + pq · pairs(e))     # picoseconds
               t, q, pq = eq_or's slopes per point, range and pair, read off ns_per_code
best = argmin scan_ns over the covers of both cuts
```

The selective cut wins for covers wide enough that a code costs the kernel
nothing; the balanced one for covers narrow enough that eq_or pays for
every code. A cover between the two, where nibble_n8 wins, can be missed. On
onpair's 239 drawn needles this loses 0.2% geomean against the full λ
ladder it replaced, worst 1.34x, by the model.

`price(cut)`: `cover = from_edge_cut(cut)`, `covered = Σ frequency over
points and ranges`, `ns = scan_ns(cover, covered, total_frequency)`. The row
count no longer enters the cost model; `analyze` keeps the parameter and
ignores it. A Set edge weighs the runs its codes merge to, as points and
ranges, in both implementations. `scan_ns` lives in `scan/policy/mod.rs`,
§6.5.

**Outputs (`mod.rs`).** `PrefilterAnalysis { cover, covered_frequency,
total_frequency, scan_ns, walk, matches_all }`. `plan` returns the analysis
together with the graph and the chosen cut, and `trace_prefilter` labels
those, so the trace and the analysis come from one planning path. The empty
pattern sets `matches_all` and both execute functions append every row
without a scan. The C++ walk's node ids are u32, so there is no pattern
length limit. `prefilter_candidates` runs the walk on every hit;
`prefilter_superset` runs the same plan with `Check::Superset`. An empty
cover on a non-empty pattern appends nothing. `prefilter_is_likely_profitable`
is gone from onpair; `scan_ns` is the number to compare against a decode
fallback.

## 6. Scan (`scan/`)

### 6.1 Driver (`scan/mod.rs`)

```
BLOCK = 4096 codes = 64 mask words;  Block = [Code; BLOCK];  Mask = [u64; 64]

both_stages<M>(cover, codes, row_offsets, check, out):
    matcher = M::new(cover); resolver = GallopSeek::new(row_offsets); bits = zero
    for (block, at, valid) in blocks(codes):
        if matcher.check(block, &mut bits):        # false promises an all-zero mask
            clear_from(bits, valid)
            resolver.rows(bits, at, check, out)

blocks(codes):
    at = 0
    while at + BLOCK <= codes.len(): yield (codes[at..], at, BLOCK); at += BLOCK
    if at < codes.len(): rest = codes.len() - at; tail = zeros; tail[..rest] = codes[at..]; yield (tail, at, rest)

clear_from(bits, valid): if valid < BLOCK: bits[valid/64] &= (1 << valid%64) - 1; zero bits[valid/64 + 1..]
```

No lookahead any more, since every probe is one code long. `facts(input,
covered, total)` returns `None` when the cover is empty or there are no
rows, and otherwise `Facts { expected_hits, code_count }` with
`expected_hits = covered · code_count / total` (exact when the region is
the indexed stream, 0 when `total == 0`).

### 6.2 Matcher contract (`scan/matcher/mod.rs`)

`Matcher::new(&cover)` and `check(block, bits) -> bool`: bit `i` set iff the
cover admits `codes[i]`; a `false` return promises an empty mask, `true`
promises nothing. Kernels do not check that they take the cover;
`policy::takes` does.

Shared driver `words<SKIP>` (`shared.rs`): per pair of mask words load 128
codes, compute `Hits` for each 64 (0xFF/0x00 byte lanes on NEON and AVX2, a
mask word on AVX-512), and if `SKIP && !any(a, b)` write two zero words and
continue, else `movemask`. Returns whether anything was written. NEON
movemask: AND each byte with `[1,2,4,…,128]` repeated, three `vpaddq_u8`
rounds, one 16-byte store for both words. AVX2: `vpmovmskb` per 32 lanes.
u16 lanes are narrowed with `vuzp1q_u8` first; u8 lanes (FSST) skip that.

### 6.3 Stage one kernels for u8 codes

The new branch compiles u16 lanes only (`Vectors = [uint16x8_t; 8]` on
NEON). FSST needs the u8 impls, which are in `old: scan/novel/matcher/` as
the `impl … for u8` halves of `eq_or.rs`, `range.rs`, `nibble_n_8.rs`, and
the whole of `nibble.rs`. Take the structure from new (matchers built from
`&ProbeCover`, no `Pattern`) and the lane code from old.

| kernel | takes | ops per 16 codes | what it does |
| --- | --- | --- | --- |
| `eq_or` | K ≥ 1, any R | `2K + 3R - 1` | `OR_k (codes == t_k)`, ranges folded in |
| `range` | K = 0, R ≥ 1 | `3R - 1` | `(codes - begin) <=u (last - begin)` per range, ORed |
| `nibble_n8`, 1 batch | 1 ≤ K ≤ 8, any R | `7 + 3R` | bit `k` set in `low[t_k & 0xf]` and `high[t_k >> 4]`; `tbl(low, c & 0xf) & tbl(high, c >> 4) != 0` |
| `nibble` | any K, any R | 8 | universal 16×16 bitmap; ranges are set into the tables and cost nothing |

The u16 `table` kernel is not ported: `nibble` takes every cover it would
and runs five times faster (23.6 against 4.8 GB/s), so no shape would ever
reach it. The tests keep a scalar definition of the mask as their oracle,
as the Rust tests do.

`nibble` build: for each covered code `c`, `row = c & 0x0f`, `bit = c >> 4`;
`bit < 8 → low[row] |= 1 << bit`, else `high[row] |= 1 << (bit - 8)`.
Kernel per 16 codes: `idx = c & 0x8f` (bit 7 kept so `vqtbl1q_u8` returns 0
for the wrong half), `hit = (tbl(low, idx) | tbl(high, idx ^ 0x80)) &
tbl(BIT, c >> 4) != 0` with `BIT = [1,2,4,…,128,1,2,…,128]`. The `0x80` is
a constant; the 0x80.pl page's `input & 0x80` is wrong. On AVX2 and
AVX-512 `vpshufb` also zeroes lanes whose index has bit 7 set, so the same
trick works with tables broadcast per 128-bit lane.

At u8 width `nibble_n8` runs one batch only (the bitmap is cheaper than a
second). `range` on AVX2 uses `min_epu8(x, width) == x` for the unsigned
compare.

### 6.4 Stage two resolver (`scan/resolver/`)

```
GallopSeek { row = 0, row_end = 0, resolved_code = 0 }

rows(bits, first_code, check, out):
    at = max(resolved_code - first_code, 0)
    while hit = next_set(bits, at):
        code = first_code + hit
        if code >= row_end:
            row = gallop(row, code); row_end = row_offsets[row + 1]
        if check.passes(code, row_offsets[row], row_end):
            out.push(row); resolved_code = row_end; at = row_end - first_code
        else:
            at = hit + 1

gallop(from, code): step = 1; while from + step < len && row_offsets[from + step] <= code: step *= 2
                    lo = from + step/2; hi = min(from + step, len)
                    return lo + partition_point(row_offsets[lo + 1 .. hi], <= code)
```

The linear seek and the choice between the two are gone. `Check` is
`Superset` or `Walk { walk, dict, codes }`. A row goes out on its first
passing hit; its other hits are never read. The row layer must cover every
code.

### 6.5 Policy and cost model (`scan/policy/mod.rs`)

Plan = `(matcher, skip)`.

- **matcher** = argmin `ns_per_code` over kernels that take the shape.
  `takes`: eq_or needs K > 0; range needs K = 0, R > 0; nibble_n8 needs
  K > 0 and `ceil(K/8) ≤ MAX_BATCHES` (3 on NEON and AVX-512, 2 on AVX2 at
  u16, 1 at u8); nibble takes everything, which is the role the u16 `table`
  had.
- **skip** = `d < SKIP_BELOW_DENSITY`, `d = expected_hits / code_count`. The
  flag adds a lane OR per group of 128 codes and saves the pack on each
  group with no match, an `exp(-128 · d)` share, so the break-even is
  `ln(pack / reduction) / 128`: 0.0023 on onpair's NEON and AVX2 pair
  (0.75, 1.01), never on AVX-512, where there is no pack.

```
scan_ns(cover, covered, codes):
    0 if cover empty or no codes
    codes · ns_per_code(select_matcher(shape), shape) + covered · VERIFY_NS_PER_HIT
```

Stage two, the resolve, is under 3% of the chosen cover's `scan_ns` on
onpair (median 0.4%, max 2.2%) and changes no plan, so the model leaves it
out; with it went the resolver sweep and its constants. `VERIFY_NS_PER_HIT`
is 8.0, onpair's 2026-09-08 median; a fused refit there measured 4.2 ns, and
the balanced cut picks the same covers anywhere from 2 to 64 ns.

`policy.hpp` carries the 2026-09-15 fit from §11. Its skip pair (3.3, 3.4 ns
per group) comes from the eq_or K = 1 ends of the same sweep, so
`SKIP_BELOW_DENSITY` is ln(3.4 / 3.3) / 128 = 0.00023 bits per code, and 0 on
AVX-512 as in onpair. Its
ladder: eq_or at K ≤ 2, nibble_n8k at 3..8, nibble from 9; ranges alone to
range up to R = 3; beside one token the compare carries two ranges; beside
eight tokens the batch carries one. `policy/tests.cpp` pins those boundaries
and runs every plan end to end against the scalar rows.

Fitted `ns_per_code` for **u8 codes**, from old (the kernels are unchanged;
K tokens, R ranges):

| kernel | NEON (M4 Pro) | AVX2 (Xeon 6975P-C) | AVX-512BW (same core) |
| --- | --- | --- | --- |
| eq_or | 0.0041 + 0.0070·K + 0.0119·R | 0.0124 + 0.0052·K + 0.0104·R | 0.0086 + 0.0041·K + 0.0078·R |
| range | 0.0032 + 0.0111·R | 0.0065 + 0.0104·R | 0.0050 + 0.0075·R |
| nibble_n8, 1 batch | 0.0243 + 0.0115·R | not measured | 0.0157 + 0.0075·R |
| nibble | 0.0386 | not built in Rust | not built in Rust |

The u16 rows (new: table 0.195, eq_or 0.0043 + 0.0144·K + 0.0205·R,
nibble_n8 0.0228 + 0.0325·B + 0.0213·R, range 0.0043 + 0.0212·R on NEON;
the AVX2 nibble_n8 row is marked not measured) matter only if a u16 stream
is ever scanned. NEON ladder at u8: eq_or at K ≤ 2, nibble_n8 at 3..8,
nibble from 9; ranges alone go to `range` up to R = 3 and to `nibble` from
R = 4; beside one token the compare carries two ranges and the bitmap takes
over from three.

### 6.6 Dispatch (`scan/dispatch.rs`)

`run(plan, cover, codes, row_offsets, check, out)` matches the matcher, then
the batch count for nibble_n8, then the skip flag (each vector kernel is
compiled with and without it), then the resolver. In C++: templates on
`<Matcher, Resolver, bool Skip>`, the resolver templated on the offset
width.

## 7. Walk (`scan/walk.rs`, new version)

A hit is a covered token at a code index. The graph lists the edges that
token can be; each fixes the node before and after it. Forward takes the
greedy step out of each node until a terminal range; backward takes the
step into each node until the source, or an entry set the planner did not
enumerate. Neither walk branches. No byte is decoded.

```
Node { greedy_step: Option<(token, to)>, terminal_range: Option<range>, entry: Option<NeedlePrefix> }
NeedlePrefix { len = k, bytes = needle[..k] little-endian, mask }      # only where the set into node k was too big
Edge { from: u16, to: u16 }
EdgesByToken: token -> edges; one Edge per token in a span array, NONE (0,0), LISTED (MAX,MAX) with a sorted side list

from_graph(graph, needle):
    Point(t) at from -> to:   nodes[from].greedy_step = (t, to);  edges[t] += (from, to)
    Range(r) at from -> sink: nodes[from].terminal_range = r;      edges[c] += (from, sink) for c in r
    Set(ids) at from -> to:   edges[c] += (from, to) for c in ids  # set entry is an ordinary edge
    SetTooBig at source -> k: nodes[k].entry = NeedlePrefix(needle[..k])

check(dict, codes, row_start, row_end, hit):
    for (from, to) in edges[codes[hit]]:
        if (to == sink || forward(to, hit + 1)) && backward(from, hit): return true
    false

forward(node, i):
    while i < row_end:
        c = codes[i]; at = nodes[node]
        if at.terminal_range contains c: return true
        if at.greedy_step == (c, next): node = next; i += 1
        else: return false
    false

backward(node, end):
    while node != SOURCE:
        if end <= row_start: return false
        c = codes[end - 1]
        step = the edge of c with to == node          # at most one exists
        if none: return nodes[node].entry is some and prefix.is_tail_of(dict, c)
        node = step.from; end -= 1
    true

is_tail_of(dict, c): len(c) >= k && (symbol(c) >> 8·(len(c) - k)) & mask == bytes
```

Backward is a loop because at most one edge of a token enters a node: a
greedy step's origin is `to` minus the token's length, and a token stepping
into `node` is shorter than the prefix `node` stands for, so it cannot also
be the set entry there. With 256 FSST codes `EdgesByToken` is a `[256]`
array of small lists.

For FSST the walk as written reads a literal byte after 255 as a token and
has no escape steps to follow. §9 has the fix, and `scan/walk/walk.hpp` is
the port: `Node` holds one step, `Point(code, next)` or `Escape(byte,
next)`, a 256-bit terminal set and `entry_len`; the steps of each code sit
in a CSR table indexed by code, escape steps under 255 with their byte.
`forward` also returns true the moment a step lands on the sink, which only
an escape step does (a greedy step that would land there is left out in
favour of the terminal set). Test oracle (`scan/walk/tests.cpp`): every
unit of every row as the hit, expected true iff the unit overlaps an
occurrence and is not an entry token of an unenumerated set. The weaker
"any hit in the row" oracle let a needle ending in a literal pass through
the marker hit while every token hit failed.

## 8. Everything else in the module

**Exactness.** `prefilter_candidates` is exact; `prefilter_superset` is a
sound superset for a caller that decodes candidates and runs memmem. In
FSST that verifier is `fsst_decompress` plus `memmem`.

**Empty needle.** Matches every row, including rows without codes; no
graph, no scan.

**Sequence probes and the fused kernels** were removed from new. Sequence
probes come back later (§11); the fused kernels do not.

## 9. FSST-specific changes to make

Neither Rust version handles FSST as the port needs it. Old had a partial
attempt (escape to sink, sorted remap); new has none. What to change,
against the new structure:

| place | new does | port |
| --- | --- | --- |
| `graph.rs` `build_state` | panics on a dead end | add edge `o -> o + 1` with probe `Point(255)` and the escaped byte `needle[o]` attached, so the chain continues and the cut may fall later; frequency of 255 is the escape count in the stream |
| `graph.rs` `build_alignment_graph` | `debug_assert frequencies.num_tokens() == dict.num_tokens()` | frequency array is `count[256]`; dictionary has ≤ 255 symbols |
| `graph.rs` `greedy_in_needle`, `terminal_range`, `alignment_candidates` | sorted-dictionary binary searches, memmem sweep | linear scans over ≤ 255 symbols; the terminal range is a contiguous `CodeRange` because `fsst_sort_codes` sorted the codes (§3) |
| `walk.rs` `from_graph` | `Point` edges only between interior nodes | an escape edge becomes `greedy_step = Escape(byte, to)` on `nodes[o]` and `edges[255] += (o, o + 1)`; several escape edges share token 255 and the byte disambiguates |
| `walk.rs` `forward` | one code per step | on `Escape(byte, next)`: need `codes[i] == 255 && i + 1 < row_end && codes[i + 1] == byte`, then `i += 2`; a 255 met on a unit boundary is always a marker; landing on the sink is success, since the escape edge into it has no terminal set standing in for it |
| `walk.rs` `backward` | unit ending at `end - 1` is a token | if the run of 255s ending at `end - 2` is odd, the unit is the literal `codes[end - 1]` and only the escape edge into `node` with that byte can match; then `end -= 2` |
| `walk.rs` `check` | every hit is a token | a hit at `i` whose preceding run of 255s is odd is a literal: skip it. A hit on code 255 with an even run before it is a marker: try the escape edges whose byte is `codes[i + 1]` |
| `walk.rs` `NeedlePrefix` | u128 window, `MAX_TOKEN_SIZE` 16 | u64 `symbol[c]`, `len[c]`, `MAX_TOKEN_SIZE` 8 |
| `mod.rs` `MAX_PATTERN_LEN` | 65535 | dropped: u32 node ids in the walk |
| `scan/matcher/*` | u16 lanes | u8 lanes from old, plus `nibble` from old |
| `scan/policy/mod.rs` | u16 cost rows, no `nibble` | u8 rows from old (§6.5), `nibble` in the kernel list with cost 0.0386 and `takes` always |
| `old: src/fsst/encode.rs` | sort symbols, remap stream, pass escapes through | the compressor writes sorted codes itself (`fsst_sort_codes`, §3); read `fsst_decoder_t` directly |
| nowhere | 511-byte chunk boundaries | removed from the compressor instead (§3) |

Frequencies for the literal bytes: the counting pass sees literals as codes
and inflates `count[b]` for every escaped byte `b`. That is an advisory
weight only; soundness does not depend on it.

## 10. Tests worth porting

Oracles the Rust holds every part to; `tests.rs`, `scan/matcher/tests.rs`,
`scan/resolver/tests.rs`, the tests inside `scan/walk.rs` and `scan/mod.rs`,
and for the policy `old: scan/novel/policy/tests.rs` (new has none):

- **Soundness end to end** (`check`, `covers_every_match`): compress rows,
  for each needle compare candidates against byte containment of the decoded
  rows; exact rows equal the true set; the superset contains it; candidates
  ascend without repeats. Corpora: URL-like text, repetitive text, random
  binary; needles of 1 byte to over 255 bytes; absent needles; the empty
  needle (`empty_pattern_appends_all_rows`); a non-empty needle with an empty
  cover (`empty_probe_cover_still_appends_nothing`);
  `superset_verified_by_memmem_matches_the_walk`. Port: `prefilter/tests.cpp`
  compresses its rows with `fsst_create`/`fsst_compress`, checks the graph
  invariants, the cover holding a code of every matching row and the exact rows through the walk on URL text,
  repetitive text, random bytes and text with escaped bytes inside the needle.
- **Graph invariants** (`check_graph`): `from < to` on every edge; the sink
  is unreachable when the cut's edges are blocked; the cut never contains
  `SetTooBig`; `contained` and `first[k]` agree with the brute-force scan,
  including the k=1 cap of 16.
- **Cover** (`cover_probes_the_maximal_runs_of_its_membership`,
  `analysis_reports_normalized_cover_frequency`): points and ranges disjoint,
  abutting runs merged, covered count equals the sum of frequencies.
- **False zero frequencies** cannot hide a match.
- **Walk**: for each row try every code as the hit and compare `any(check)`
  with byte containment. Cases: interior entry and exit layouts, whole
  needle inside one token, a token on two edges, no crossing of a row
  boundary, overlapping occurrences (`appappapple`), entry through a set the
  planner did not enumerate. Add for FSST: escapes inside and around the
  occurrence, literals equal to covered codes, runs of `255 255`, a hit on
  a marker 255, an occurrence whose first or last byte is escaped.
- **Matchers** against a scalar `expected(cover, block)`: needles the block
  holds and does not hold, a hit at the block seam, codes sharing a nibble
  with a token, ranges of 1 to 256 codes, the top of the code space (255),
  nibble table halves, padding sets no bit, `the_driver_scans_every_block`.
- **Resolvers** against `expected_rows` (partition_point per set bit), u32
  and u64 offsets: uniform and ragged layers with empty rows, empty masks,
  strided masks at several densities, a row hit in two blocks, hits at both
  stream ends, one hit after many rows.
- **Policy** (old): the planned matcher takes its cover; the plan is the
  cheapest kernel modelled; boundaries sit where the README table says; the
  resolver follows the two seek costs; skip only below the density
  threshold; the planned scan agrees with the scalar matcher plus either
  resolver.
- **Pricing**: `scan_cost_is_monotone_in_probes_and_coverage`,
  `planned_cover_prices_at_or_below_the_frequency_cut`,
  `split_scan_returns_exact_rows`.

## 11. Stage one sweep (`search/prefilter/scan/bench/`)

Mirrors onpair's `matcher_fit` so the paper's `plot_matcher.py` reads the CSV
unchanged (`--encoding fsst`). Data and output folders are gitignored.

1. Raw text: `bench_corpus`'s SQL against `~/ch.duckdb`, first-appearance
   distinct of the first 1,048,576 URLs, to `data/ch/hits/URL_1m.csv`.
2. `prefilter_corpus data/ch/hits/URL_1m.csv data/ch/hits/URL_1m.fsst.csv`:
   `fsst_create` over all rows, `fsst_compress`, one row per line with codes
   as decimals, marker and literal both as codes; the exported symbol table
   goes to `URL_1m.fsst.csv.symtab`. 296,161 rows, 23.4M codes, 0.2% escapes.
3. onpair's `cargo run --release --example bench_needles -- <that csv>`
   writes `URL_1m.fsst.needles.csv` beside it. A byte alphabet has about 217
   distinct codes here, so the K = 256 bin and many selectivity targets are
   unreachable and marked off target.
4. `prefilter_matcher_sweep`: sample-0 sets, ranges alone at R in {1..32} and
   widths {1, 16, 256} that fit 256 codes, width-16 ranges beside K in {1, 8,
   16} at target 0.01; kernels `eq_or`, `range`, `nibble_n8k` at
   `ceil(K/8)` batches up to 32, `nibble`, each vector kernel with and without
   the skip flag; fastest of five passes over a 4M-code prefix; rows checked
   against the cover over the first sixteenth. Writes
   `output/novel_mask_<stamp>.csv`, 970 rows in about 4 s. The timed loop
   carries an `asm volatile` barrier on the mask, since the compiler removed
   the fully inlined kernels otherwise.
5. `onpair_like/paper/gen-kernel-data-fsst.sh` wraps step 4 and copies the
   CSV to `paper/data/matcher_fsst_<arch>_<date>.csv`.

First numbers, M4 Pro, GB/s: `eq_or` 87 / 52 / 37 at K = 1 / 2 / 3,
`nibble_n8k` 37 flat to K = 8 then 22 at 16, `nibble` 23.6 at every K and R,
`range` 65 / 37 / 26 / 20 at R = 1..4, `table` 4.8. Within 10% of the old
branch's u8 rows in §6.5.

**Fit.** Both sweeps end by fitting the cost model to their rows and print
the block to paste into the policy; `--refit [csv]` does the same from the
newest CSV in `output/`, or the one named, without sweeping. The matcher
fit mirrors onpair's: a relative-error weighted line per kernel over its own
axis (K, batches, R; flat for `nibble`), the per-range slope
from the residual over the tokens-only line for `eq_or` and `nibble_n8k`,
the rate ceiling, and the skip flag's cost at each selectivity. First fit on
the M4 Pro, ns per code:

| kernel | fitted | old Rust u8 row |
| --- | --- | --- |
| eq_or | 0.0035 + 0.0078·K + 0.0115·R | 0.0041 + 0.0070·K + 0.0119·R |
| range | 0.0036 + 0.0117·R | 0.0032 + 0.0111·R |
| nibble_n8k | 0.0268 + 0.0124·R | 0.0243 + 0.0115·R |
| nibble | 0.0425 | 0.0386 |

Skip flag: saves 0.0006 (eq_or) to 0.003 (nibble_n8k) ns per code on a
stream nothing hits, costs 0.008 to 0.026 from target 0.001 up, so the
break-even sits below 1e-3 bits per code here, lower than the README's
2.3e-3. One numerical point: the flat-line test in `line_fit` is relative,
because clang contracts `sw·swxx - swx²` into an FMA and the exact zero
comes out as 1e-7, which turned the flat `nibble_n8k` line into a negative
intercept before the fix.

The stage two sweep (`resolver_fit.cpp`, `prefilter_resolver_sweep`) is
gone with stage two's place in the cost model, as onpair's `resolver_fit`
is; its last CSV stays in `output/`.

## 12. Decisions (2026-09-15)

| topic | decision |
| --- | --- |
| target | NEON, AVX-512BW and AVX2, one explicit body per kernel under `#if defined(__ARM_NEON)` / `#elif defined(__AVX512BW__)` / `#elif defined(__AVX2__)` in the same files, chosen at compile time by the build's flags (`-march=native` on the machine that runs the result; no runtime dispatch by the user's decision); on AVX-512 a compare answers into the mask word (`Hits = __mmask64`), the OR is `kor` and the movemask a store; AVX2 works in two `__m256i` with `vpmovmskb`, the unsigned range test as `max_epu8(x, w) == w`; the policy rows for both x86 sets are NEON's until `prefilter_matcher_sweep --refit` runs there; both x86 builds are compile-checked on the M4 by cross-compiling, their ctest runs are owed on x86 hosts |
| input | flat code buffer plus u32 row offsets; the caller works in blocks and the offsets are what makes that possible |
| chunking | removed from the compressor, AVX-512 compress path disabled (commit 3f46263 on this branch) |
| escapes | escape-aware graph and walk (§9) |
| kernels | port the u8 kernels from old; keep the universal `nibble` bitmap, which new dropped, because FSST codes are bytes and any cover is two 16-byte tables |
| order of work | stage one kernels, resolvers and the alignment graph first; the walk after; sequence probes later |
| frequency index | one counting pass over the stream at analysis time, not timed |
| location | new sources under `search/` in this repo, C++17, library target |
| pair probes | in, via the unrolled cut graph (§4), a one-symbol terminal range pairs too; pair frequency is counted exactly (`Frequency::pairs`, 256 KB, same pass; the independence estimate ranked `g`,`o` at 3.4k against 25k real and bought nothing); the sweep has pairs-alone rows at P in {1, 2, 4, 8} (`pairs` column) and `eq_or` is fitted at 0.01674 ns per code per pair |
| raw codes | sorted codes, written by the compressor: `fsst_sort_codes` permutes the codes at the write sites into byte order of their symbols (§3); a `Range` edge is one `CodeRange` weighing `(0, 1)`, a `Set` edge's weight counts the runs its codes merge to |
| C++ layout | header-only under `search/prefilter/`, one file per Rust module: `cover.hpp`, `scan/scan.hpp` (driver, `both_stages`), `scan/matcher/{shared,eq_or,range,nibble_n8,nibble}.hpp`, `scan/resolver/resolver.hpp` (`GallopSeek`), `scan/policy/policy.hpp` (selection, skip threshold and `scan_ns` with the fitted constants), `scan/dispatch.hpp`, `scan/execute.hpp` (facts and the planned run, the rest of Rust's `scan/mod.rs`), `dictionary.hpp` (symbol table as code to bytes, `MAX_TOKEN_SIZE`, `ESCAPE`), `frequency.hpp` (`count[256]`), `graph.hpp` (`Edge`, `Candidates`, `build_alignment_graph`, `from_edge_cut`), `mincut.hpp` (`min_cut`), `plan.hpp` (`Analysis` with its `Walk`, `cheapest_cover`, `plan`, which also returns the graphs and cut `tools/trace.hpp` labels), `prefilter.hpp` (`analyze`, `execute` over an `Analysis`, exact `candidate_rows`, `Finder`), `scan/walk/walk.hpp` (`Walk`, `WalkCheck`), `compress.hpp` (the FSST round trip the tests and tools share); tests as `tests.cpp` beside each module, registered with ctest from `search/CMakeLists.txt`; `prefilter/tests.cpp` links `fsst` and compresses its own rows |
