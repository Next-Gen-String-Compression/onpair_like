# Upstream TUM pattern corpus — provenance

`patterns.json` is a byte-for-byte copy of `benchmark/patterns.json` from

- repository: https://github.com/calin2110/FSST-LIKE-Matching
- commit: `b1eb3ab9c63ea0199a381b92371a3154190b4406` (2026-06-25, upstream `main`
  at the time the `fsst_like_tum` candidate was pinned — DESIGN.md §17)
- sha256: `62b8ba01ac90d39ecd2199ae3727a94623886110752fce6632e83a370194b6d1`

verified identical to the copy inside the FetchContent tree the candidate
builds from (our fork `Hedi-Chehaidar/FSST-LIKE-Matching@09d89812`, which is
upstream plus two kernel fixes that do not touch the benchmark directory).

It is the workload of *Compression-Aware LIKE: Matching Patterns in the FSST
Domain* (Pop, Riedl, Neumann; DaMoN 2026): 462 patterns over four datasets,
grouped by the number of `_` wildcards (`0`, `1`, `>=2`), and for TPC-H
annotated with the query number a pattern originates from.

| upstream dataset | columns | `_`=0 | `_`=1 | `_`≥2 | imported here? |
|---|---|---|---|---|---|
| TPCH | part.p_type, part.p_name, orders.o_comment, supplier.s_comment | 23 | 27 | 28 | yes — all four columns are in `datasets/sources.yaml` |
| IMDB | films, actors, plot, quotes | 80 | 80 | 79 | yes — films→`imdb-primary-title-1m`, actors→`imdb-primary-name-1m`, plot/quotes→the frozen 2017 columns |
| StackOverflow | Posts, Comments, PostHistory | 16 | 15 | 18 | no — dataset not in the roster |
| PublicBI | six columns | 96 | — | — | no — dataset not in the roster |

The suites in this directory are produced from the file by `bench tum-import`
and blessed against **our** physical columns, which differ from the paper's
(modern IMDb TSVs and a DuckDB dbgen instead of their dumps). They are an
*adapted* TUM LIKE workload, never an exact reproduction: match counts are
recomputed by the oracle and never imported, and every query carries the
upstream dataset / table / column / underscore group / TPC-H query number in
`meta.tum` so the two can be reconciled.
