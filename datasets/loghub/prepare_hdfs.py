#!/usr/bin/env python3
"""Materialise the loghub HDFS_v1 log as a structured parquet table.

Pipeline: download HDFS_v1.zip (retry, sha256 recorded) -> stream-extract
HDFS.log out of the archive -> one row per log line, original file order ->
zstd parquet with the line split into its fields:

  ts         timestamp[ms] 2008-11-09 20:35:18   (the `081109 203518` prefix)
  pid        int32         143
  level      large_string  'INFO'
  component  large_string  'dfs.DataNode$DataXceiver'
  message    large_string  'Receiving block blk_-1608999687919862906 src: …'

This schema is the script's only output — there is no flag for an alternative
column layout. `message` is the string-matching workload; `bench ingest`
canonicalises one string column, so:

  bench ingest --source <parquet> --format parquet --column message --id hdfs-v1

`level` (2 distinct values) and `component` (9) are ingestable too but
dictionary-degenerate, and ts/pid are not string columns at all.

Emission rules follow datasets/prepare.py's extractors: trailing newline (and
CR) stripped, empty lines skipped, field text otherwise untouched.

Note that ts is a parsed value, not raw bytes: the two-digit year is read as
20xx (HDFS_v1 is a November 2008 capture) and the zero-padded `081109 203518`
text is not recoverable byte-for-byte from the column.

Usage:
  python3 datasets/loghub/prepare_hdfs.py                 # whole log
  python3 datasets/loghub/prepare_hdfs.py --rows 1000000  # first N lines
  python3 datasets/loghub/prepare_hdfs.py --keep-archive  # don't delete the zip

Idempotent: a present log / output parquet is reused, so a re-run after an
interrupted download only redoes the missing step.

Deps: pyarrow (see datasets/requirements.txt).
"""
from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import sys
import time
import urllib.request
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
RAW_DIR = REPO_ROOT / "datasets" / "raw" / "loghub"
OUT_DIR = Path(__file__).resolve().parent

URL = "https://zenodo.org/records/8196385/files/HDFS_v1.zip?download=1"
ARCHIVE = RAW_DIR / "HDFS_v1.zip"
LOG_MEMBER = "HDFS.log"          # the raw log inside the archive
BATCH_ROWS = 1 << 20

# `081109 203615 148 INFO dfs.DataNode$PacketResponder: <message>` — the
# loghub HDFS_v1 line format, captured field by field. All 11,175,629 lines
# match; any line that does not is skipped and counted (reported at the end),
# since the typed columns have no raw-text fallback.
LINE_RE = re.compile(
    r"^(\d{2})(\d{2})(\d{2}) (\d{2})(\d{2})(\d{2}) (\d+) (\w+) ([^:]+): (.*)$")


def log(msg: str) -> None:
    print(msg, flush=True)


def human(n: int) -> str:
    for unit in ("B", "KiB", "MiB", "GiB"):
        if n < 1024 or unit == "GiB":
            return f"{n:.1f} {unit}" if unit != "B" else f"{n} B"
        n /= 1024.0
    return f"{n} B"


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


def download(url: str, target: Path, retries: int = 5, backoff: float = 5.0) -> None:
    """Fetch url -> target unless already present as a readable zip."""
    if target.exists() and zipfile.is_zipfile(target):
        log(f"  archive present ({target.name}, {human(target.stat().st_size)}) — skipping download")
        return
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_suffix(target.suffix + ".part")
    for attempt in range(1, retries + 1):
        try:
            log(f"  downloading {url}")
            req = urllib.request.Request(
                url, headers={"User-Agent": "like-benchmark-prepare/1.0"})
            with urllib.request.urlopen(req, timeout=120) as resp, open(tmp, "wb") as out:
                total = int(resp.headers.get("Content-Length") or 0)
                seen, next_mark = 0, 1 << 28
                while True:
                    b = resp.read(1 << 20)
                    if not b:
                        break
                    out.write(b)
                    seen += len(b)
                    if seen >= next_mark:
                        pct = f" ({100 * seen / total:.0f}%)" if total else ""
                        log(f"    {human(seen)}{pct}")
                        next_mark += 1 << 28
            if total and seen != total:
                raise OSError(f"short read: {seen} of {total} bytes")
            break
        except Exception as e:  # noqa: BLE001 — transient network errors
            tmp.unlink(missing_ok=True)
            if attempt == retries:
                raise SystemExit(f"download failed after {retries} attempts: {e}")
            log(f"  attempt {attempt} failed ({e}); retrying in {backoff * attempt:.0f}s")
            time.sleep(backoff * attempt)
    if not zipfile.is_zipfile(tmp):
        raise SystemExit(f"{tmp}: not a zip archive — upstream layout changed?")
    tmp.rename(target)
    log(f"  downloaded {target.name} ({human(target.stat().st_size)}, "
        f"sha256 {sha256_of(target)[:16]}…)")


# ------------------------------------------------------------- extract


def extract_log(archive: Path, member: str, dest_dir: Path) -> Path:
    """Stream `member` out of the zip; returns the extracted path."""
    dest = dest_dir / Path(member).name
    with zipfile.ZipFile(archive) as zf:
        names = zf.namelist()
        if member not in names:
            hits = [n for n in names if n.endswith(f"/{member}")]
            if not hits:
                raise SystemExit(
                    f"{archive.name}: no member {member!r}; archive holds {names[:20]}")
            member = hits[0]
        info = zf.getinfo(member)
        if dest.exists() and dest.stat().st_size == info.file_size:
            log(f"  log present ({dest.name}, {human(dest.stat().st_size)}) — skipping extract")
            return dest
        log(f"  extracting {member} -> {dest.name} ({human(info.file_size)} uncompressed)")
        tmp = dest.with_suffix(dest.suffix + ".part")
        with zf.open(member) as src, open(tmp, "wb") as out:
            shutil.copyfileobj(src, out, length=1 << 22)
        tmp.rename(dest)
    return dest


# --------------------------------------------------------------- parse


def days_from_civil(y: int, m: int, d: int) -> int:
    """Days since 1970-01-01 (Howard Hinnant's algorithm) — the date32 /
    timestamp encoding, without paying for a datetime object per row."""
    y -= m <= 2
    era = (y if y >= 0 else y - 399) // 400
    yoe = y - era * 400
    doy = (153 * (m + (-3 if m > 2 else 9)) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def rows(path: Path, limit: int | None):
    """Yield (epoch_seconds, pid, level, component, message) per log line, in
    original file order. Unparsable lines are skipped; the caller counts them
    by comparing the yielded count against the lines read."""
    epoch_day: dict[tuple[str, str, str], int] = {}
    n = 0
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n").rstrip("\r")
            if not line:
                continue
            m = LINE_RE.match(line)
            if m is None:
                yield None
                continue
            yy, mm, dd, hh, mi, ss, pid, level, component, message = m.groups()
            key = (yy, mm, dd)
            day = epoch_day.get(key)
            if day is None:
                day = epoch_day[key] = days_from_civil(
                    2000 + int(yy), int(mm), int(dd))
            ts = day * 86400 + int(hh) * 3600 + int(mi) * 60 + int(ss)
            yield ts, int(pid), level, component, message
            n += 1
            if limit is not None and n >= limit:
                return


# ------------------------------------------------------------- parquet


def write_parquet(row_iter, out: Path) -> dict:
    """Stream parsed rows into the typed columns; returns a stats dict."""
    import pyarrow as pa
    import pyarrow.parquet as pq

    # Milliseconds, not seconds: Parquet's TIMESTAMP logical type has no
    # SECONDS unit, so a timestamp('s') schema is silently coerced to ms on
    # write and reads back as ms. Declaring ms keeps file and arrow schemas
    # equal. The log's resolution is still whole seconds.
    schema = pa.schema([
        ("ts", pa.timestamp("ms")),
        ("pid", pa.int32()),
        ("level", pa.large_string()),
        ("component", pa.large_string()),
        ("message", pa.large_string()),
    ])
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(out.suffix + ".part")
    n = message_bytes = unparsed = 0

    def flush(writer, cols):
        writer.write_table(pa.table([
            # epoch seconds -> s -> ms: the second cast converts the unit,
            # whereas casting int64 seconds straight to ms would read the
            # integer as milliseconds and land in 1970.
            pa.array(cols[0], pa.int64()).cast(pa.timestamp("s")).cast(pa.timestamp("ms")),
            pa.array(cols[1], pa.int32()),
            pa.array(cols[2], pa.large_string()),
            pa.array(cols[3], pa.large_string()),
            pa.array(cols[4], pa.large_string()),
        ], schema=schema))

    with pq.ParquetWriter(str(tmp), schema, compression="zstd") as writer:
        cols: tuple[list, ...] = ([], [], [], [], [])
        for row in row_iter:
            if row is None:
                unparsed += 1
                continue
            for col, value in zip(cols, row):
                col.append(value)
            message_bytes += len(row[4].encode("utf-8"))
            if len(cols[0]) >= BATCH_ROWS:
                flush(writer, cols)
                n += len(cols[0])
                for col in cols:
                    col.clear()
                log(f"    {n} rows")
        if cols[0]:
            flush(writer, cols)
            n += len(cols[0])
    tmp.rename(out)
    return {"rows": n, "message_bytes": message_bytes, "unparsed": unparsed}


# ---------------------------------------------------------------- main


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=None,
                    help="output parquet (default: datasets/loghub/<id>.parquet)")
    ap.add_argument("--rows", type=int, default=None, metavar="N",
                    help="stop after N rows (default: the whole log)")
    ap.add_argument("--keep-archive", action="store_true",
                    help="keep HDFS_v1.zip after extraction (default: delete it)")
    ap.add_argument("--force", action="store_true",
                    help="rewrite the parquet even if it already exists")
    args = ap.parse_args()

    ds_id = "hdfs-v1" + (f"-{args.rows}" if args.rows else "")
    out = args.out or OUT_DIR / f"{ds_id}.parquet"
    if out.exists() and not args.force:
        log(f"{out} present — nothing to do (--force to rewrite)")
        return

    log(f"loghub HDFS_v1 -> {out}")
    raw_log = RAW_DIR / LOG_MEMBER
    if raw_log.exists() and raw_log.stat().st_size > 0:
        # Already extracted (and the archive is deleted by default) — don't
        # re-fetch 178 MiB just to unpack a file we have.
        log(f"  log present ({raw_log.name}, {human(raw_log.stat().st_size)}) — "
            f"skipping download + extract")
    else:
        download(URL, ARCHIVE)
        raw_log = extract_log(ARCHIVE, LOG_MEMBER, RAW_DIR)

    log("  writing parquet (ts, pid, level, component, message)")
    st = write_parquet(rows(raw_log, args.rows), out)
    log(f"  {st['rows']} rows -> {out.name} ({human(out.stat().st_size)} on disk); "
        f"message payload {human(st['message_bytes'])}")
    if st["unparsed"]:
        print(f"  WARNING: {st['unparsed']} lines did not match the HDFS_v1 "
              f"format and were skipped", file=sys.stderr, flush=True)
    log(f"  ingest with: bench ingest --source {out} --format parquet "
        f"--column message --id {ds_id} --out datasets/{ds_id}")

    if not args.keep_archive and ARCHIVE.exists():
        ARCHIVE.unlink()
        log(f"  removed {ARCHIVE.name} (re-fetchable; {raw_log.name} kept)")


if __name__ == "__main__":
    main()
