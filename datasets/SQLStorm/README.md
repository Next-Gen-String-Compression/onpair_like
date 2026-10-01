# SQLStorm v1.0

Queries from [SQL-Storm/SQLStorm](https://github.com/SQL-Storm/SQLStorm) (TUM),
converted to parquet.

SQLStorm is an LLM-generated analytical benchmark: GPT-4o-mini was shown a
schema and asked for realistic analytical SQL, and the queries that parsed and
ran on PostgreSQL 17 / Umbra 25.01 / DuckDB 1.2.0 were kept. v1.0 ships
**62,243 queries over four schemas** — StackOverflow, JOB, TPC-H, TPC-DS.

| | |
|---|---|
| Upstream | `vendor/SQLStorm` submodule, pinned at `b3bb0b9` (2025-10-08) |
| Paper | Schmidt et al. — <https://db.in.tum.de/~schmidt/papers/sqlstorm.pdf> |
| Licence | see `vendor/SQLStorm/LICENSE` |
| Clone size | 2.2 GB (queries are 72 MB of it; the rest is `v0.0` and result JSON) |

Two things here are of interest to a LIKE matcher, and they pull in opposite
directions — read "Needle quality" before building a suite from this.

## Usage

```bash
python3 datasets/SQLStorm/prepare.py
```

No download, no network — the submodule already holds every query as a `.sql`
file, and the script only reshapes them. Takes a few seconds. Existing output
parquets are left alone; delete them to rebuild. Both outputs are git-ignored
by `datasets/**/*.parquet`.

Only `v1.0` is read. `v0.0` is the parameterised official TPC-H/TPC-DS/JOB
query sets (152,000 files) and contains **no LIKE at all**.

## Tables

### `queries.parquet` — the SQL text corpus

One row per query file: `dataset`, `query_id` (the filename stem), `sql`
(verbatim). 12.3 MiB on disk.

| dataset | queries | `sql` bytes | mean | max | distinct |
|---|---:|---:|---:|---:|---:|
| stackoverflow | 18,251 | 22,833,187 | 1,251 | 3,295 | 17,314 |
| tpch | 17,036 | 17,977,912 | 1,055 | 3,122 | 17,030 |
| tpcds | 15,242 | 18,429,349 | 1,209 | 3,372 | 15,236 |
| job | 11,714 | 13,120,057 | 1,120 | 2,945 | 11,713 |
| **all** | **62,243** | **72,360,505** | 1,163 | 3,372 | 61,293 |

This is a shape the other corpora in `datasets/` don't cover: **long strings**
(mean 1.16 kB against 92 B for both log corpora and 32 B for `p_name`), so a
single row spans many SIMD blocks and per-row dispatch overhead is amortised
differently. Whole queries are 98.5% distinct, but the *lines* are not —
2,736,784 non-blank lines collapse to 425,967 distinct (15.6%), because the
generator reuses the same indented `FROM Posts p` / `LEFT JOIN Users u ON …`
fragments across thousands of queries. Keyword-heavy ASCII with a small
vocabulary and heavy internal repetition; expect FSST to do well.

```bash
bench ingest --source datasets/SQLStorm/queries.parquet --format parquet \
  --column sql --id sqlstorm-v1-sql --out datasets/sqlstorm-v1-sql
```

Ingested clean: 62,243 rows, 72,360,505 payload bytes, 0 nulls removed,
27–3,372 B, `xxh3:1fb000af6c04591c`.

Filter to one schema first if a 72 MB mixed corpus isn't wanted — the four are
stylistically distinct (`stackoverflow` has the CTE-heaviest queries, `tpch` the
shortest).

### `like_predicates.parquet` — the needle inventory

One row per LIKE/ILIKE predicate found by regex over the same files:
`dataset`, `query_id`, `operand`, `op`, `rhs_kind`, `pattern`, `shape`.
**6,710 predicates**, 47 KiB.

| rhs_kind | n | what it is |
|---|---:|---|
| `literal` | 5,345 | `p.Tags LIKE '%sql%'` — `pattern` holds the literal |
| `correlated` | 1,365 | `p.Tags LIKE CONCAT('%', t.TagName, '%')` — `pattern` is NULL |

Operators: `LIKE` 5,914, `ILIKE` 521, `NOT LIKE` 274, `NOT ILIKE` 1.

844 of the 5,345 literals are `'%'` — a match-all placeholder the generator
emits when it wants an optional filter, 727 of them in stackoverflow alone
(89% of that schema's literals). Excluding those leaves **4,501 non-trivial
predicates, 749 distinct patterns**, median needle 5 B, p90 9 B, max 20 B:

| dataset | non-trivial | distinct | contains | prefix | suffix | other |
|---|---:|---:|---:|---:|---:|---:|
| tpch | 2,361 | 379 | 1,642 | 695 | 6 | 18 |
| job | 1,657 | 260 | 1,291 | 342 | 2 | 22 |
| tpcds | 395 | 145 | 258 | 116 | 17 | 4 |
| stackoverflow | 88 | 37 | 66 | 11 | 10 | 1 |

Overwhelmingly single-needle `contains` and `prefix`. A `%` anywhere but at a
string end appears **16 times in 62,243 queries** — 9 with a trailing or
leading anchor, and 7 TPC-H `'Brand%G'` infix forms. The fully unanchored
`LIKE '%a%b%'` that `LB_MULTI_CONTAINS` implements occurs exactly **3** times.
`_` wildcards: 2.

## Needle quality: most of these match nothing

The literals are worth very little as a benchmark suite. TPC-H is the case that
can be checked exactly, because `datasets/tpch-pname-sf10` and
`datasets/tpch-ccomment-sf10` are the real columns those predicates address:

| column | rows | distinct needles | zero-match | uses that match nothing |
|---|---:|---:|---:|---:|
| `p_name` | 2,000,000 | 64 | **58 (91%)** | 313 / 400 |
| `c_comment` | 1,500,000 | 6 | **6 (100%)** | 7 / 7 |

The model invented a plausible parts vocabulary instead of using the generator's.
`'%widget%'` is the single most common `p_name` needle — **141 uses, 0 rows** —
alongside `'%Gadget%'`, `'%Aluminum%'`, `'%screws%'`, `'%special%'`. Every
`p_name` value is exactly five words drawn from a fixed 92-word colour list
(both counts confirmed against the 2M-row column), so the six needles that
do match are all colours (`%steel%` 72 uses, `%green%`, `%metal%`, `%wood%`)
and every one of them lands at ~5.4% selectivity — the same 1-in-92-per-word
draw. The measured selectivity range is 5.41%–9.78% with no spread below it.

So SQLStorm gives you needles with **two selectivity values** on the one corpus
where they can be verified, and no way to verify job/tpcds/stackoverflow at all
(no corpus here for those schemas). It is not a source of realistic selectivity
sweeps; `bench`'s generated suites remain better for that. What it does document
is the *pattern mix* an LLM writes when asked for analytical SQL — near-total
dominance of unanchored single-needle `contains`, which is the same conclusion
the Grafana o11y-bench trajectories reached from a completely different angle
(see `datasets/README.md`).

## The correlated form is the interesting part

1,365 predicates build the needle per row from a joined column rather than a
constant, and 1,210 of those are in stackoverflow:

```sql
FROM Tags t JOIN Posts p ON p.Tags LIKE CONCAT('%', t.TagName, '%')
```

`Posts.Tags` is a `<sql><postgresql><indexing>` blob, so this joins every post
against the whole tag table by substring — one haystack column against a
**needle set of tens of thousands**, not a single pattern. Top operands are
`p.Tags` (866), `rp.Tags` (85), `rp.Title` (44). 122 of the 1,365 (8.9%) spell
it `CONCAT('%<', t.TagName, '>%')`, anchoring on the delimiters.

That is a genuinely different workload from anything in `datasets/` — every
suite here is one needle against one column — and it is the reason to keep this
submodule around even though the literals are weak. Nothing implements it yet;
matching a corpus against a needle *set* is a different algorithm (Aho–Corasick
territory), not a loop over `LB_CONTAINS`.

## Curated suites from the GitHub crawl (`like_predicates.csv`)

`like_predicates.csv` (repo root) is the companion GitHub crawl: 324 LIKE
predicate uses from repos running SQLStorm-style queries, with the target
`table.column` resolved. `make_like_suites.py` turns the subset whose column
exists as an ingested dataset into curated suites:

| suite | dataset | queries | predicate uses covered |
|---|---|---:|---:|
| `imdb-keyword-sqlstorm` | `imdb-keyword` (134k rows) | 7 | 71 |
| `imdb-akaname-sqlstorm` | `imdb-akaname` (901k rows) | 6 | 50 |
| `tpch-pname-sf10-sqlstorm` | `tpch-pname-sf10` | 5 | 5 |

The remaining 198 uses target columns with no corpus (4-row dimension tables,
one-off toy schemas) and are dropped with an accounting line. Duplicated
predicates collapse into one query carrying `meta.use_count`; `ilike` is
folded to case-sensitive (the contract is byte-exact) with the fold recorded
in `meta.search_types`. Run via `specs/sqlstorm.toml`; this is a
workload-realism suite — for selectivity sweeps use the `gen` suites.

## Not in `datasets/sources.yaml`

Like `datasets/OpenRCA`, this stays a manual recipe. `sources.yaml` pins URLs by
sha256; here the pin is the submodule commit, and the queries are already in the
working tree — there is nothing to download or verify.
