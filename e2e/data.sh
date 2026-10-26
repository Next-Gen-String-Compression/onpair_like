#!/bin/bash
# Builds the files the end-to-end benchmark reads, into $E2E_DATA (default
# e2e/data). A file that exists is kept, and a download only happens when a
# file derived from it is missing, so pointing E2E_DATA at a directory that
# already holds them skips every step.
#
#   E2E_DATA=/data [E2E_CLICKBENCH=1] e2e/data.sh
#
# The paper's end-to-end figures read JOB and the micro columns only. The full
# ClickBench table, 14 GB of downloads and about 76 GB more of copies, feeds
# figures the paper does not include, so its files (hits_*, ch.*, hits.*) are
# built only with E2E_CLICKBENCH=1.
#
#   ../datasets/raw/clickbench/hits_<i>.parquet  ClickBench, the 100 athena_partitioned
#                              files, in the folder datasets/prepare.py and
#                              paper/gen-kernel.sh share (hits_0 and hits_1)
#   raw/job/job_<t>.parquet    IMDb (JOB), duckdb-data v1.0
#   ch.duckdb                  hits, by DuckDB's benchmark/clickbench load.sql, written by stock DuckDB
#   imdb.duckdb                the 21 JOB tables, by benchmark/imdb_plan_cost/init, written by stock DuckDB
#   ch.contains.duckdb         hits, FSST forced, storage v1.2.0, written by the fork
#   imdb.pf.duckdb             the JOB tables, storage v1.2.0, written by the fork
#   hits.0861.vortex           hits, written by duckdb-vortex 0.86.1
#   hits.onpair.vortex         hits, written by the OnPair build, FSST skipped
#   imdb.{0861,onpair}.vortex/<table>.vortex   the same for each JOB table
#   micro.duckdb               the micro benchmark's four columns, FSST forced, by the fork
#   micro.uncompressed.duckdb  the same columns stored uncompressed, by the fork
#   micro.{0861,onpair}.vortex/<table>.vortex  the same for each of them
#
# The fork writes FSST segments with sorted symbol tables, which the
# compressed contains needs; storage v1.2.0 is the oldest that carries them.
# The Vortex builds, stock DuckDB 1.5.5, can read neither those segments nor
# the fork's default storage version, so the two source databases they copy
# hits and the JOB tables from are written by stock DuckDB, the 0.86.1 build's.
set -euo pipefail
E2E=$(cd "$(dirname "$0")" && pwd)
DATA=${E2E_DATA:-$E2E/data}
CLICKBENCH=${E2E_CLICKBENCH:-}
BIN=$E2E/builds/bin
SRC=$E2E/../third_party/duckdb
RAW=$DATA/raw
HITS=$E2E/../datasets/raw/clickbench
mkdir -p "$DATA"
DUCKDB=$BIN/duckdb
STOCK=$BIN/duckdb-vortex-0.86.1

# download <url> <file>: a connection below 100 kB/s for a minute is cut and
# retried, resuming; when resuming keeps failing the file starts over once.
download() {
  [ -s "$2" ] && return
  mkdir -p "$(dirname "$2")"
  curl --fail --location --retry 3 --speed-limit 100000 --speed-time 60 --continue-at - --output "$2.part" "$1" || {
    rm -f "$2.part"
    curl --fail --location --retry 3 --speed-limit 100000 --speed-time 60 --output "$2.part" "$1"
  }
  mv "$2.part" "$2"
}

# The JOB tables in schema order.
TABLES=$(sed -n 's/^CREATE TABLE \([a-z_]*\).*/\1/p' "$SRC/benchmark/imdb_plan_cost/init/schema.sql")

need() { [ ! -e "$DATA/$1" ]; }

if [ -n "$CLICKBENCH" ] && need ch.duckdb && { need ch.contains.duckdb || need hits.0861.vortex || need hits.onpair.vortex; }; then
  url=https://datasets.clickhouse.com/hits_compatible/athena_partitioned
  for i in $(seq 0 99); do download "$url/hits_$i.parquet" "$HITS/hits_$i.parquet"; done
  echo "== ch.duckdb"
  rm -f "$DATA/ch.duckdb.tmp"
  { echo "PRAGMA disable_progress_bar;"; sed "s|$url/|$HITS/|" "$SRC/benchmark/clickbench/queries/load.sql"; } |
    "$STOCK" "$DATA/ch.duckdb.tmp"
  mv "$DATA/ch.duckdb.tmp" "$DATA/ch.duckdb"
fi

if need imdb.duckdb && { need imdb.pf.duckdb || need imdb.0861.vortex || need imdb.onpair.vortex; }; then
  url=https://github.com/duckdb/duckdb-data/releases/download/v1.0
  for t in $TABLES; do download "$url/job_$t.parquet" "$RAW/job/job_$t.parquet"; done
  echo "== imdb.duckdb"
  rm -f "$DATA/imdb.duckdb.tmp"
  { echo "PRAGMA disable_progress_bar;"; cat "$SRC/benchmark/imdb_plan_cost/init/schema.sql"
    sed "s|$url/|$RAW/job/|" "$SRC/benchmark/imdb_plan_cost/init/load.sql"; } |
    "$STOCK" "$DATA/imdb.duckdb.tmp"
  mv "$DATA/imdb.duckdb.tmp" "$DATA/imdb.duckdb"
fi

# copy_duckdb <source db> <target> <tables> <settings>: CREATE TABLE AS per table
copy_duckdb() {
  local src=$1 dst=$2 tables=$3 settings=$4 sql=""
  echo "== $dst"
  rm -f "$DATA/$dst.tmp"
  for t in $tables; do sql+="CREATE TABLE dst.$t AS SELECT * FROM src.$t;"$'\n'; done
  "$DUCKDB" <<SQL
SET threads=4;
PRAGMA disable_progress_bar;
$settings
ATTACH '$DATA/$src' AS src (READ_ONLY);
ATTACH '$DATA/$dst.tmp' AS dst;
$sql
CHECKPOINT dst;
SQL
  mv "$DATA/$dst.tmp" "$DATA/$dst"
}

[ -n "$CLICKBENCH" ] && need ch.contains.duckdb && copy_duckdb ch.duckdb ch.contains.duckdb hits \
  "SET storage_compatibility_version='v1.2.0'; SET force_compression='fsst';"
need imdb.pf.duckdb && copy_duckdb imdb.duckdb imdb.pf.duckdb "$TABLES" \
  "SET storage_compatibility_version='v1.2.0';"

# write_vortex <binary> <source db> <target> <tables> [env]: one file per table,
# or a single file when the target ends in .vortex and there is one table
write_vortex() {
  local bin=$1 src=$2 dst=$3 tables=$4 env=${5:-} sql="" out
  echo "== $dst"
  rm -rf "$DATA/$dst.tmp"
  if [ "$tables" = hits ]; then
    sql="COPY (SELECT * FROM src.hits) TO '$DATA/$dst.tmp' (FORMAT vortex);"
  else
    mkdir -p "$DATA/$dst.tmp"
    for t in $tables; do
      sql+="COPY (SELECT * FROM src.$t) TO '$DATA/$dst.tmp/$t.vortex' (FORMAT vortex);"$'\n'
    done
  fi
  env $env "$bin" <<SQL
SET threads=4;
PRAGMA disable_progress_bar;
ATTACH '$DATA/$src' AS src (READ_ONLY);
$sql
SQL
  mv "$DATA/$dst.tmp" "$DATA/$dst"
}

[ -n "$CLICKBENCH" ] && need hits.0861.vortex && write_vortex "$BIN/duckdb-vortex-0.86.1" ch.duckdb hits.0861.vortex hits
[ -n "$CLICKBENCH" ] && need hits.onpair.vortex && write_vortex "$BIN/duckdb-vortex-onpair" ch.duckdb hits.onpair.vortex hits VORTEX_SKIP_FSST=1
need imdb.0861.vortex && write_vortex "$BIN/duckdb-vortex-0.86.1" imdb.duckdb imdb.0861.vortex "$TABLES"
need imdb.onpair.vortex && write_vortex "$BIN/duckdb-vortex-onpair" imdb.duckdb imdb.onpair.vortex "$TABLES" VORTEX_SKIP_FSST=1

# The micro benchmark's four 2^20-row columns (datasets/prepare.py), one table
# each with the column `data`, for e2e/micro.py.
MICRO="clickbench_url amazon_title dbpedia_abstract imdb_charname"
# write_micro_duckdb <target> <compression>: the columns written by the fork
write_micro_duckdb() {
  local dst=$1 compression=$2 sql=""
  echo "== $dst"
  rm -f "$DATA/$dst.tmp"
  for t in $MICRO; do
    sql+="CREATE TABLE dst.$t AS SELECT data FROM '$E2E/../datasets/raw/${t//_/-}-1mi/column.parquet';"$'\n'
  done
  # Attached after the settings: a file opened on the command line keeps the
  # default storage version and is written uncompressed.
  "$DUCKDB" <<SQL
SET threads=4;
PRAGMA disable_progress_bar;
SET storage_compatibility_version='v1.2.0';
SET force_compression='$compression';
ATTACH '$DATA/$dst.tmp' AS dst;
$sql
CHECKPOINT dst;
SQL
  mv "$DATA/$dst.tmp" "$DATA/$dst"
}
need micro.duckdb && write_micro_duckdb micro.duckdb fsst
# Figure 15's uncompressed baseline: the same columns stored as plain strings.
need micro.uncompressed.duckdb && write_micro_duckdb micro.uncompressed.duckdb uncompressed
# write_micro_vortex <binary> <target> [env]: from the parquet columns, since the
# Vortex builds cannot open a file the fork wrote
write_micro_vortex() {
  local bin=$1 dst=$2 env=${3:-} sql=""
  echo "== $dst"
  rm -rf "$DATA/$dst.tmp"
  mkdir -p "$DATA/$dst.tmp"
  for t in $MICRO; do
    sql+="COPY (SELECT data FROM '$E2E/../datasets/raw/${t//_/-}-1mi/column.parquet') TO '$DATA/$dst.tmp/$t.vortex' (FORMAT vortex);"$'\n'
  done
  env $env "$bin" <<SQL
SET threads=4;
PRAGMA disable_progress_bar;
$sql
SQL
  mv "$DATA/$dst.tmp" "$DATA/$dst"
}
need micro.0861.vortex && write_micro_vortex "$BIN/duckdb-vortex-0.86.1" micro.0861.vortex
need micro.onpair.vortex && write_micro_vortex "$BIN/duckdb-vortex-onpair" micro.onpair.vortex VORTEX_SKIP_FSST=1

# Row counts of every table in every copy against its source.
echo "== verify"
for t in $TABLES; do
  want=$("$DUCKDB" -readonly "$DATA/imdb.duckdb" -noheader -list -c "SELECT count(*) FROM $t")
  for got in \
    "$("$DUCKDB" -readonly "$DATA/imdb.pf.duckdb" -noheader -list -c "SELECT count(*) FROM $t")" \
    "$("$BIN/duckdb-vortex-0.86.1" -noheader -list -c "SELECT count(*) FROM '$DATA/imdb.0861.vortex/$t.vortex'")" \
    "$("$BIN/duckdb-vortex-onpair" -noheader -list -c "SELECT count(*) FROM '$DATA/imdb.onpair.vortex/$t.vortex'")"; do
    [ "$got" = "$want" ] || { echo "$t: $got rows, $want in imdb.duckdb"; exit 1; }
  done
done
if [ -n "$CLICKBENCH" ]; then
  want=$("$DUCKDB" -readonly "$DATA/ch.duckdb" -noheader -list -c "SELECT count(*) FROM hits")
  for got in \
    "$("$DUCKDB" -readonly "$DATA/ch.contains.duckdb" -noheader -list -c "SELECT count(*) FROM hits")" \
    "$("$BIN/duckdb-vortex-0.86.1" -noheader -list -c "SELECT count(*) FROM '$DATA/hits.0861.vortex'")" \
    "$("$BIN/duckdb-vortex-onpair" -noheader -list -c "SELECT count(*) FROM '$DATA/hits.onpair.vortex'")"; do
    [ "$got" = "$want" ] || { echo "hits: $got rows, $want in ch.duckdb"; exit 1; }
  done
fi
for t in $MICRO; do
  want=$("$DUCKDB" -noheader -list -c "SELECT count(*) FROM '$E2E/../datasets/raw/${t//_/-}-1mi/column.parquet'")
  for got in \
    "$("$DUCKDB" -readonly "$DATA/micro.duckdb" -noheader -list -c "SELECT count(*) FROM $t")" \
    "$("$DUCKDB" -readonly "$DATA/micro.uncompressed.duckdb" -noheader -list -c "SELECT count(*) FROM $t")" \
    "$("$BIN/duckdb-vortex-0.86.1" -noheader -list -c "SELECT count(*) FROM '$DATA/micro.0861.vortex/$t.vortex'")" \
    "$("$BIN/duckdb-vortex-onpair" -noheader -list -c "SELECT count(*) FROM '$DATA/micro.onpair.vortex/$t.vortex'")"; do
    [ "$got" = "$want" ] || { echo "$t: $got rows, $want in its parquet"; exit 1; }
  done
done
echo "every copy has its source's row counts"
