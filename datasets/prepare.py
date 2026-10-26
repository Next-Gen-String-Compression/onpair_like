#!/usr/bin/env python3
"""Materialise benchmark datasets from datasets/sources.yaml.

Pipeline per entry: download (retry, sha256-verified) -> extract the string
column (stdlib parsers, same rules as the OnPair compression paper's
benchmark) -> intermediate parquet -> `bench ingest` (the only producer of
canonical artifacts) -> verify the canonical checksum pinned in sources.yaml.

Idempotent: a present raw download with a matching sha256 is not re-fetched;
a present canonical artifact with a matching checksum is not re-ingested.

Usage:
  python3 datasets/prepare.py --list
  python3 datasets/prepare.py --all                  # every default entry
  python3 datasets/prepare.py --dataset clickbench-url-1mi
  python3 datasets/prepare.py --all --update-checksums   # first-time pinning

Deps: pyyaml, pyarrow — see
datasets/requirements.txt. `bench` is found at target/release/bench or via
$BENCH_BIN.
"""
from __future__ import annotations

import argparse
import bz2
import csv
import gzip
import hashlib
import io
import itertools
import json
import re
import subprocess
import sys
import tarfile
import time
import urllib.request
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
SOURCES = REPO_ROOT / "datasets" / "sources.yaml"
RAW_DIR = REPO_ROOT / "datasets" / "raw"
PARQUET_COLUMN = "data"
BATCH_ROWS = 1 << 20
# A download slower than MIN_RATE bytes/s over STALL_SECONDS is as good as
# stalled: it starts again on a fresh connection, as a failed one does.
MIN_RATE = 100_000
STALL_SECONDS = 60


def log(msg: str) -> None:
    print(msg, flush=True)


# ------------------------------------------------------------ download


def sha256_of(path: Path, chunk: int = 1 << 20) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(chunk)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def download(url: str, target: Path, expected_sha256: str | None,
             retries: int = 5, backoff: float = 5.0) -> str:
    """Fetch url -> target unless already present and verified. Returns the
    observed sha256."""
    if target.exists() and target.stat().st_size > 0:
        got = sha256_of(target)
        if expected_sha256 and got != expected_sha256:
            raise SystemExit(
                f"{target}: sha256 {got} != pinned {expected_sha256} — "
                f"delete the file to re-download, or fix sources.yaml")
        log(f"  raw present ({target.name}, sha256 {got[:12]}…) — skipping download")
        return got
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_suffix(target.suffix + ".part")
    for attempt in range(1, retries + 1):
        try:
            log(f"  downloading {url}")
            req = urllib.request.Request(url, headers={"User-Agent": "like-benchmark-prepare/1.0"})
            with urllib.request.urlopen(req, timeout=60) as resp, open(tmp, "wb") as out:
                window, received = time.monotonic(), 0
                while True:
                    b = resp.read1(1 << 20)
                    if not b:
                        break
                    out.write(b)
                    received += len(b)
                    if time.monotonic() - window >= STALL_SECONDS:
                        if received < MIN_RATE * STALL_SECONDS:
                            raise TimeoutError(f"below {MIN_RATE // 1000} kB/s for {STALL_SECONDS} s")
                        window, received = time.monotonic(), 0
            break
        except Exception as e:  # noqa: BLE001 — transient network errors
            if attempt == retries:
                raise SystemExit(f"download failed after {retries} attempts: {e}")
            log(f"  attempt {attempt} failed ({e}); retrying in {backoff * attempt:.0f}s")
            time.sleep(backoff * attempt)
    got = sha256_of(tmp)
    if expected_sha256 and got != expected_sha256:
        raise SystemExit(f"{url}: sha256 {got} != pinned {expected_sha256}")
    tmp.rename(target)
    log(f"  downloaded {target.name} ({target.stat().st_size} bytes, sha256 {got[:12]}…)")
    return got


# ------------------------------------------------------- row extractors
# Each yields str rows in original file order — the same emission rules as
# the compression paper's tools/datasets/*.py, so both benchmarks see
# identical row streams.


def rows_jsonl_field(raw: Path, params: dict):
    field = params["field"]
    with gzip.open(str(raw), mode="rt", encoding="utf-8") as f:
        for line in f:
            try:
                row = json.loads(line).get(field, "").strip()
            except json.JSONDecodeError:
                continue
            if row:
                yield row


def rows_ttl_en_literal(raw: Path, params: dict):
    limit = int(params.get("limit", 0)) or None
    n = 0
    with bz2.open(str(raw), mode="rt", encoding="utf-8") as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            start = line.find('"')
            end = line.rfind('"@en')
            if start != -1 and end != -1 and end > start:
                text = line[start + 1:end].strip()
                if text:
                    yield text
                    n += 1
                    if limit and n >= limit:
                        return


def rows_tgz_csv_column(raw: Path, params: dict):
    """One CSV member of a .tgz, one column by index. Parsing matches the
    JOB load scripts (PostgreSQL COPY: FORMAT csv, ESCAPE '\\')."""
    member = params["member"]
    col = int(params["column_index"])
    # A NULL field is empty in these CSVs; keep_empty preserves it as an empty
    # row instead of dropping it, so a sparse column stays sparse.
    keep_empty = bool(params.get("keep_empty"))
    with tarfile.open(str(raw), mode="r:gz") as tar:
        names = [n for n in tar.getnames() if n == member or n.endswith("/" + member)]
        if not names:
            raise SystemExit(f"{raw}: no member named {member}")
        f_in = tar.extractfile(names[0])
        text = io.TextIOWrapper(f_in, encoding="utf-8", newline="")
        for parts in csv.reader(text, escapechar="\\", doublequote=False):
            if len(parts) > col:
                row = parts[col].strip()
                if row or keep_empty:
                    yield row


EXTRACTORS = {
    "jsonl-field": rows_jsonl_field,
    "ttl-en-literal": rows_ttl_en_literal,
    "tgz-csv-column": rows_tgz_csv_column,
}


# ------------------------------------------------------------- parquet


def parquet_column_to_parquet(files: list[Path], column: str, limit: int | None,
                              out: Path) -> int:
    """One column of several parquet files, in order, cut to `limit` rows."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    schema = pa.schema([(PARQUET_COLUMN, pa.large_string())])
    out.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    with pq.ParquetWriter(str(out), schema, compression="zstd") as writer:
        for f in files:
            for batch in pq.ParquetFile(f).iter_batches(batch_size=BATCH_ROWS,
                                                        columns=[column]):
                col = batch.column(0).cast(pa.large_string())
                if limit and n + len(col) > limit:
                    col = col.slice(0, limit - n)
                writer.write_table(pa.table({PARQUET_COLUMN: col}))
                n += len(col)
                if limit and n >= limit:
                    return n
    return n


def write_parquet(rows, out: Path) -> int:
    """Stream rows into a single large_string column; returns row count."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    schema = pa.schema([(PARQUET_COLUMN, pa.large_string())])
    out.parent.mkdir(parents=True, exist_ok=True)
    n = 0
    with pq.ParquetWriter(str(out), schema, compression="zstd") as writer:
        batch: list[str] = []
        for row in rows:
            batch.append(row)
            if len(batch) >= BATCH_ROWS:
                writer.write_table(pa.table({PARQUET_COLUMN: pa.array(batch, pa.large_string())}))
                n += len(batch)
                batch = []
        if batch:
            writer.write_table(pa.table({PARQUET_COLUMN: pa.array(batch, pa.large_string())}))
            n += len(batch)
    return n


# ----------------------------------------------------------- bench glue


def bench_bin() -> Path:
    import os

    env = os.environ.get("BENCH_BIN")
    if env:
        return Path(env)
    p = REPO_ROOT / "target" / "release" / "bench"
    if not p.exists():
        raise SystemExit(f"{p} not found — run `cargo build --release` first (or set $BENCH_BIN)")
    return p


def ingest(source: Path, column: str, ds_id: str, out_dir: Path) -> str:
    """Run `bench ingest`; returns the canonical checksum from the manifest."""
    subprocess.run(
        [str(bench_bin()), "ingest", "--source", str(source), "--format", "parquet",
         "--column", column, "--id", ds_id, "--out", str(out_dir)],
        check=True,
        cwd=REPO_ROOT,
    )
    manifest = json.loads((out_dir / "manifest.json").read_text())
    return manifest["checksum"]


# -------------------------------------------------------------- driver


def prepare_entry(entry: dict, update_checksums: bool) -> dict:
    ds_id = entry["id"]
    log(f"\n=== {ds_id} ===")
    out_dir = REPO_ROOT / "datasets" / ds_id
    expected = entry.get("expected_checksum")
    pinned = expected if isinstance(expected, str) and expected.startswith("xxh3:") else None

    # Idempotent resume: canonical artifact already present and matching.
    manifest_path = out_dir / "manifest.json"
    if manifest_path.exists():
        got = json.loads(manifest_path.read_text())["checksum"]
        if pinned and got == pinned:
            log(f"  canonical artifact present (checksum {got}) — nothing to do")
            return {}
        if not pinned:
            log(f"  canonical artifact present (checksum {got}, no pin to verify)")
            if update_checksums:
                return {ds_id: {"expected_checksum": got}}
            return {}
        raise SystemExit(
            f"{ds_id}: artifact checksum {got} != pinned {pinned} — delete "
            f"{out_dir} to re-materialise, or --update-checksums to re-pin")

    kind = entry["kind"]
    raw_dir = RAW_DIR / ds_id
    # Downloads land in datasets/raw/<source.download_dir> when the entry names
    # one, a folder other scripts read the same files from; extracts stay here.
    download_dir = RAW_DIR / entry["source"].get("download_dir", ds_id)
    writeback: dict = {}

    if kind == "parquet":
        params = entry.get("params") or {}
        limit = int(params.get("limit", 0)) or None
        extra = entry["source"].get("extra") or []
        files = []
        for i, src in enumerate([entry["source"], *extra]):
            f = download_dir / src["url"].rsplit("/", 1)[1]
            sha = src.get("sha256")
            pin = sha if isinstance(sha, str) and re.fullmatch(r"[0-9a-f]{64}", sha) else None
            got_sha = download(src["url"], f, pin)
            files.append(f)
            if update_checksums and not sha and i == 0:
                writeback.setdefault(ds_id, {})["sha256"] = got_sha
        if limit or len(files) > 1:
            parquet = raw_dir / "column.parquet"
            if not parquet.exists():
                log(f"  slicing {len(files)} parquet file(s), limit {limit} -> {parquet.name}")
                n = parquet_column_to_parquet(files, params["column"], limit, parquet)
                log(f"  extracted {n} rows")
            column = PARQUET_COLUMN
        else:
            parquet = files[0]
            column = params["column"]
    else:
        url = entry["source"]["url"]
        raw = download_dir / url.rsplit("/", 1)[1]
        got_sha = download(url, raw, _pinned_sha(entry))
        if update_checksums and not _pinned_sha(entry):
            writeback.setdefault(ds_id, {})["sha256"] = got_sha
        parquet = raw_dir / "column.parquet"
        if not parquet.exists():
            params = entry.get("params") or {}
            limit = int(params.get("limit", 0)) or None
            rows = EXTRACTORS[kind](raw, params)
            # `offset` picks the window: a column can be ordered by something
            # that correlates with its content, so row 0 is not always where a
            # representative slice starts.
            offset = int(params.get("offset", 0))
            if limit or offset:
                rows = itertools.islice(rows, offset, offset + limit if limit else None)
            log(f"  extracting ({kind}) -> {parquet.name}")
            n = write_parquet(rows, parquet)
            log(f"  extracted {n} rows")
            if limit and n != limit:
                parquet.unlink()
                raise SystemExit(
                    f"{ds_id}: extracted {n} rows, not the {limit} requested — "
                    f"check `spread_over` against the source's row count")
        column = PARQUET_COLUMN

    log("  ingesting -> canonical artifact")
    got = ingest(parquet, column, ds_id, out_dir)
    if pinned and got != pinned:
        raise SystemExit(
            f"{ds_id}: canonical checksum {got} != pinned {pinned} — the raw "
            f"source or extraction changed; do not trust results across this line")
    if not pinned:
        if update_checksums:
            writeback.setdefault(ds_id, {})["expected_checksum"] = got
            log(f"  pinning canonical checksum {got}")
        else:
            log(f"  NOTE: canonical checksum {got} is unpinned — rerun with "
                f"--update-checksums to record it in sources.yaml")
    return writeback


def _pinned_sha(entry: dict) -> str | None:
    s = entry["source"].get("sha256")
    return s if isinstance(s, str) and re.fullmatch(r"[0-9a-f]{64}", s) else None


def write_back(updates: dict) -> None:
    """Textual write-back that keeps comments intact: replaces the
    `sha256:`/`expected_checksum:` value lines inside each dataset block."""
    if not updates:
        return
    text = SOURCES.read_text().splitlines(keepends=True)
    current_id = None
    for i, line in enumerate(text):
        m = re.match(r"\s*-\s*id:\s*(\S+)", line)
        if m:
            current_id = m.group(1)
        if current_id in updates:
            up = updates[current_id]
            if "sha256" in up and re.match(r"\s*sha256:", line):
                text[i] = re.sub(r"sha256:.*", f"sha256: {up['sha256']}", line.rstrip()) + "\n"
            if "expected_checksum" in up and re.match(r"\s*expected_checksum:", line):
                text[i] = re.sub(
                    r"expected_checksum:.*",
                    f'expected_checksum: "{up["expected_checksum"]}"',
                    line.rstrip(),
                ) + "\n"
    SOURCES.write_text("".join(text))
    log(f"\nwrote pinned checksums back to {SOURCES}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--all", action="store_true", help="prepare every default entry")
    ap.add_argument("--dataset", action="append", default=[], help="prepare a specific entry (repeatable)")
    ap.add_argument("--list", action="store_true", help="list entries and exit")
    ap.add_argument("--update-checksums", action="store_true",
                    help="record observed sha256 / canonical checksums into sources.yaml")
    args = ap.parse_args()

    manifest = yaml.safe_load(SOURCES.read_text())
    entries = {e["id"]: e for e in manifest["datasets"]}

    if args.list:
        for e in manifest["datasets"]:
            dl = e["source"].get("download_bytes", 0)
            print(f"{'*' if e.get('default') else ' '} {e['id']:22s} "
                  f"~{e['approx']['rows']:>9,} rows  ~{e['approx']['payload_bytes']/1e6:>7.0f} MB payload  "
                  f"download ~{dl/1e6:,.0f} MB")
        print("\n* = default roster (--all)")
        return

    selected = [entries[d] for d in args.dataset if d in entries]
    missing = [d for d in args.dataset if d not in entries]
    if missing:
        raise SystemExit(f"unknown dataset(s): {missing}; see --list")
    if args.all:
        selected += [e for e in manifest["datasets"] if e.get("default") and e not in selected]
    if not selected:
        ap.error("nothing selected: pass --all or --dataset <id> (see --list)")

    updates: dict = {}
    for entry in selected:
        updates.update(prepare_entry(entry, args.update_checksums))
    if args.update_checksums:
        write_back(updates)
    log("\ndone.")


if __name__ == "__main__":
    main()
