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
  python3 datasets/prepare.py --dataset msmarco-query --dataset amazon-title
  python3 datasets/prepare.py --all --update-checksums   # first-time pinning

Deps: pyyaml, pyarrow (and duckdb for the tpch-* entries) — see
datasets/requirements.txt. `bench` is found at target/release/bench or via
$BENCH_BIN.
"""
from __future__ import annotations

import argparse
import bz2
import gzip
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
import time
import urllib.request
import xml.sax
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
SOURCES = REPO_ROOT / "datasets" / "sources.yaml"
RAW_DIR = REPO_ROOT / "datasets" / "raw"
PARQUET_COLUMN = "data"
BATCH_ROWS = 1 << 20


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
                while True:
                    b = resp.read(1 << 20)
                    if not b:
                        break
                    out.write(b)
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


def rows_tar_tsv_field(raw: Path, params: dict):
    field = int(params["field"])
    with tarfile.open(str(raw), mode="r:gz") as tar:
        for member in tar.getmembers():
            if not member.name.endswith(".tsv"):
                continue
            f_in = tar.extractfile(member)
            if f_in is None:
                continue
            with io.TextIOWrapper(f_in, encoding="utf-8") as text:
                for line in text:
                    parts = line.split("\t", field)
                    if len(parts) >= field + 1:
                        row = parts[field].strip()
                        if row:
                            yield row


def rows_gzip_tsv_field(raw: Path, params: dict):
    field = int(params["field"])
    require = params.get("require_prefix")
    with gzip.open(str(raw), mode="rt", encoding="utf-8") as f:
        for line in f:
            parts = line.split("\t", field + 1)
            if len(parts) >= field + 1:
                row = parts[field]
                if require and not row.startswith(require):
                    continue
                if row:
                    yield row


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


class _DBLPHandler(xml.sax.ContentHandler):
    def __init__(self, element: str, sink):
        super().__init__()
        self.element = element
        self.sink = sink
        self._buf: list[str] = []
        self._capturing = False

    def startElement(self, name, attrs):  # noqa: N802
        if name == self.element:
            self._capturing = True
            self._buf = []

    def endElement(self, name):  # noqa: N802
        if name == self.element and self._capturing:
            text = "".join(self._buf).strip()
            if text:
                self.sink(text)
            self._capturing = False

    def characters(self, content):
        if self._capturing:
            self._buf.append(content)


def rows_dblp_xml(raw: Path, params: dict):
    # Same parse mode as the compression paper's extractor: external DTD not
    # fetched, so dblp.dtd entities are skipped by expat — identical row
    # bytes to the paper's columns.
    rows: list[str] = []
    handler = _DBLPHandler(params["element"], rows.append)
    parser = xml.sax.make_parser()
    parser.setFeature(xml.sax.handler.feature_external_ges, False)
    parser.setContentHandler(handler)
    with gzip.open(str(raw), "rb") as f:
        parser.parse(f)
    yield from rows


EXTRACTORS = {
    "tar-tsv-field": rows_tar_tsv_field,
    "gzip-tsv-field": rows_gzip_tsv_field,
    "jsonl-field": rows_jsonl_field,
    "ttl-en-literal": rows_ttl_en_literal,
    "dblp-xml": rows_dblp_xml,
}


# ------------------------------------------------------------- parquet


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


def tpch_to_parquet(params: dict, out: Path, db_dir: Path) -> None:
    """Generate TPC-H at the pinned SF with DuckDB (deterministic per SF) and
    dump one column to parquet, preserving dbgen row order."""
    import duckdb

    sf = params["sf"]
    table, column = params["table"], params["column"]
    db_path = db_dir / f"tpch-sf{sf}.duckdb"
    db_dir.mkdir(parents=True, exist_ok=True)
    con = duckdb.connect(str(db_path))
    try:
        con.execute("INSTALL tpch; LOAD tpch;")
        have = {r[0] for r in con.execute("SHOW TABLES").fetchall()}
        if table not in have:
            log(f"  dbgen(sf={sf}) — one-time generation into {db_path.name}")
            con.execute(f"CALL dbgen(sf={sf})")
        out.parent.mkdir(parents=True, exist_ok=True)
        con.execute(
            f"COPY (SELECT {column} AS {PARQUET_COLUMN} FROM {table}) "
            f"TO '{out}' (FORMAT PARQUET, COMPRESSION ZSTD)"
        )
    finally:
        con.close()


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
    writeback: dict = {}

    if kind == "tpch-duckdb":
        parquet = raw_dir / f"{entry['params']['table']}.{entry['params']['column']}.parquet"
        if not parquet.exists():
            tpch_to_parquet(entry["params"], parquet, RAW_DIR / "tpch")
        column = PARQUET_COLUMN
    elif kind == "parquet":
        url = entry["source"]["url"]
        parquet = raw_dir / url.rsplit("/", 1)[1]
        got_sha = download(url, parquet, _pinned_sha(entry))
        if update_checksums and not _pinned_sha(entry):
            writeback.setdefault(ds_id, {})["sha256"] = got_sha
        column = entry["params"]["column"]
    else:
        url = entry["source"]["url"]
        raw = raw_dir / url.rsplit("/", 1)[1]
        got_sha = download(url, raw, _pinned_sha(entry))
        if update_checksums and not _pinned_sha(entry):
            writeback.setdefault(ds_id, {})["sha256"] = got_sha
        parquet = raw_dir / "column.parquet"
        if not parquet.exists():
            log(f"  extracting ({kind}) -> {parquet.name}")
            n = write_parquet(EXTRACTORS[kind](raw, entry.get("params") or {}), parquet)
            log(f"  extracted {n} rows")
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
