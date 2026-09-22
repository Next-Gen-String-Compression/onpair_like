# Datasets

Every column the benchmark reports on is pinned in `sources.yaml` and
materialised by `prepare.py` (download → sha256 → extract → parquet →
`bench ingest` → canonical `xxh3` checksum). The canonical checksum is a
dataset's identity: two machines that end with the same value have
bit-identical benchmark inputs, and a suite blessed against one checksum
refuses to load against another. Nothing materialised is ever committed.

This file records **why each column is in the roster** — from measured
distributions, not from column names — and what the two newer sources
(IMDb, and the LIKE workload's TPC-H columns) commit us to.

## The roster, and why

| id | rows | L_avg | U/N | what it exercises |
|---|---|---|---|---|
| `clickbench-url-1m` | 1 000 000 | 89 B | 0.28 | URLs: heavy prefix structure, high substring redundancy |
| `clickbench-referer-1m` | 921 225 | 86 B | 0.25 | URLs again, but a different host mix and prefix distribution — the second URL column, not a copy of the first |
| `clickbench-title-1m` | 935 718 | 148 B | **0.08** | the only long natural-language column with *heavy duplication*: a dictionary front-end's best case, unlike anything the URL columns do |
| `msmarco-query` | 1 010 916 | 36 B | — | short natural-language queries: the honest short-row case |
| `msmarco-url` | 3 210 000 | 65 B | — | URLs with a different host mix than ClickBench |
| `amazon-title` | 4 448 181 | 52 B | 0.87 | book titles, natural language |
| `dbpedia-abstract` | 1 000 000 | 338 B | 0.998 | long text, almost all distinct — the long-row showcase |
| `imdb-primary-title-1m` | 1 000 000 | 19 B | — | modern film titles: short proper-noun text (the TUM "films" stand-in) |
| `imdb-primary-name-1m` | 1 000 000 | 14 B | — | person names: very short rows, heavy first-name repetition (the TUM "actors" stand-in) |
| `imdb-plot-frozen-651k` | 650 918 | 557 B | — | plot summaries, 2017 freeze: the longest prose in the roster, and the TUM "plot" column itself |
| `imdb-quotes-frozen-1m` | 1 000 000 | 68 B | — | spoken lines, 2017 freeze: the TUM "quotes" column |
| `tpch-pname-sf10` | 2 000 000 | 33 B | 1.00 | synthetic vocabulary, every row distinct — the DB anchor |
| `tpch-ptype-sf10` | 2 000 000 | 21 B | **0.0001** | ~150 distinct values in 2M rows: the extreme low-cardinality LIKE target (TPC-H Q2/Q14/Q16) |
| `tpch-ccomment-sf10` | 1 500 000 | 72 B | 0.94 | grammar-generated comment text |
| `tpch-scomment-sf10` | **100 000** | 62 B | 0.996 | the Q16 `Customer%Complaints` column — small (supplier is 10k × SF), so timing noise is larger; it is here for pattern fidelity to the TUM workload |
| `tpch-ocomment-sf1` | 1 500 000 | 48 B | 0.53 | the Q13 `%special%requests%` column, at SF1 on purpose (see below) |

Non-default extras (`--dataset <id>`): `clickbench-searchphrase-69k`,
`tpch-ocomment-sf10`, `tpch-lcomment-sf10`, `tpch-caddress-sf10`,
`dblp-title`, `dblp-author`.

### ClickBench columns that were profiled and rejected

All from the same `hits_0.parquet` partition (1 000 000 rows), so adding any
of them costs no download:

| column | non-empty | L_avg | U/N | verdict |
|---|---|---|---|---|
| `SearchPhrase` | **6.9 %** | 51 B | 0.26 | only 69 k populated rows — kept as the non-default `clickbench-searchphrase-69k`, with the true row count in the id so no spec mistakes it for a million-row column |
| `OriginalURL` | 14.9 % | 187 B | 0.61 | a near-duplicate of `URL` on a seventh of the rows |
| `MobilePhoneModel` | 2.0 % | 4 B | 0.002 | degenerate |
| `Params` | 0.0 % | — | — | empty in this partition |

Empty rows are legal in the data model but a column that is 93 % empty
would be benchmarked mostly on rows no pattern can match; entries that want
the populated column say `drop_empty: true` and `prepare.py` filters before
ingest. The three ClickBench entries share one raw file: `prepare.py` reuses
any copy under `datasets/raw/` whose sha256 matches the pin.

### Why `o_comment` is SF1

`orders` scales as 1.5 M × SF: at SF10 the column is 15 M rows and 727 MB,
which would dominate every sweep it appeared in. SF1 gives 1.5 M rows and
73 MB with the same grammar. The scale factor is in the id because a silently
sampled column wearing an `sf10` name would be a different dataset with the
same label; `tpch-ocomment-sf10` exists as a non-default extra for
main-memory-scale runs. TPC-H is generated locally by DuckDB's `tpch`
extension (`CALL dbgen(sf=…)`), deterministic per scale factor; the DuckDB
version in `requirements.txt` is the effective dbgen pin.

## IMDb: two sources, two reproducibility stories

**`datasets.imdbws.com` is a rolling snapshot.** The TSVs are regenerated
daily (their `Last-Modified` moves every 24 h), so a permanent sha256 pin is
impossible. Those two entries record the sha256 **and `snapshot_date`** of the
copy `prepare.py` fetched, and pin the canonical `xxh3` of the extracted
column. That is enough: the canonical checksum is what binds suites to data,
so a newer snapshot fails *loudly* at suite load instead of quietly producing
incomparable numbers. Re-materialising on a later date will therefore require
re-blessing the IMDb suites; the numbers before and after are not comparable
and should not be pooled.

**`ftp.fu-berlin.de/pub/misc/movies/database/frozendata/` is frozen** at the
2017-12-22 IMDb list release and has not changed since. Those two entries are
fully pinned. They are also the corpus the DaMoN'26 FSST-LIKE paper used for
its plot and quotes columns, which is why they exist here at all.

The extraction granularity for the list files is an *adaptation*, documented
in `sources.yaml`: one row per plot summary (the joined `PL:` lines of a
block), and one row per spoken quote line with its `Speaker: ` prefix removed
and continuation lines joined. The latter was inferred from the shape of the
upstream patterns (most are head-anchored on the first spoken word); some
upstream quote patterns land at selectivity zero on our column because of it,
and `suites/tum_like/*/gen-report`-style counts in the blessed suites say
which.

**Terms.** Both IMDb sources are for personal, non-commercial use only under
IMDb's conditions (https://www.imdb.com/interfaces/). This benchmark uses them
for research and never redistributes them: the raw downloads and the
materialised columns live under gitignored paths and are re-derived, never
committed.

## Adding a column

1. Profile it first — row count, non-empty fraction, `L_avg`/p50/p95, U/N —
   and write down what distribution it adds that the roster lacks.
2. Add the `sources.yaml` entry with a public URL, a licence line, an honest
   id (row count or scale factor in the name when it is not what the source
   name implies), and `recorded-at-prepare` for anything not pinnable yet.
3. `python3 datasets/prepare.py --dataset <id> --update-checksums`, then
   commit the pinned checksums — not the data.
