#!/usr/bin/env python3
"""Download an OpenRCA system from Google Drive and convert its CSVs to parquet.

    python3 datasets/OpenRCA/download_and_transform.py [--system Bank]

Writes data/<system>/parquet/<table>.parquet, one table per distinct CSV name,
each the union of that CSV across all capture days. See README.md for the
schemas, the numbers, and the two upstream data quirks handled here.

The zip and every extracted CSV are deleted once the parquet is written (24.6 GB
for Bank) — parquet is the only representation that survives. Re-runs skip
whatever is already on disk.
"""
from __future__ import annotations

import argparse
import hashlib
import html
import re
import shutil
import time
import urllib.parse
import urllib.request
import zipfile
from http.cookiejar import CookieJar
from pathlib import Path

import duckdb

HERE = Path(__file__).resolve().parent
DATA = HERE / "data"

# Google Drive folder 1wGiEnu4OkWrjPxfx5ZTROnU37-5UDoPM. Drive is not
# content-addressed, so `bytes` (checked after the download) is the only pin
# available; the sha256 is logged for the record, not verified against anything.
DRIVE_FILES = {
    "Bank":    {"id": "1enBrdPT3wLG94ITGbSOwUFg9fkLR-16R", "bytes": 2829139841},
    "Telecom": {"id": "1cyOKpqyAP4fy-QiJ6a_cKuwR7D46zyVe", "bytes": None},
    "Market":  {"id": "1aDbqFHhYGgywLDfq54O7Ln8wgiv-BqGJ", "bytes": None},
}

UA = "openrca-prepare/1.0"
CHUNK = 1 << 20

# ------------------------------------------------------------------- sql

UTC8 = 8 * 3600
MS_CUTOFF = 100_000_000_000   # epoch value above this is ms, below it seconds

# The capture day, from the telemetry/{YYYY_MM_DD}/ directory the row came from.
DAY_SQL = ("strptime(regexp_extract(filename, "
           r"'telemetry/(\d{4}_\d{2}_\d{2})/', 1), '%Y_%m_%d')::DATE AS day")

# `timestamp` as UTC+8 wall clock. The CASE is not defensive padding:
# trace_span.csv stores 13-digit epoch MILLIseconds on 2021_03_04…2021_03_12 and
# 10-digit epoch seconds on 2021_03_23…2021_03_25, so a single-unit expression
# lands a quarter of the spans in 1970. log_service and both metric tables are
# seconds throughout and always take the first branch. 2021 is ~1.6e9 s /
# ~1.6e12 ms, so the 1e11 cutoff has three orders of magnitude either side.
TS_SQL = (f"make_timestamp(CASE WHEN timestamp < {MS_CUTOFF} "
          f"THEN (timestamp + {UTC8})::BIGINT * 1000000 "
          f"ELSE (timestamp + {UTC8 * 1000})::BIGINT * 1000 END) AS ts")

# quote/escape are pinned rather than sniffed. The CSVs are RFC4180 with doubled
# quotes inside quoted fields (apache_access_log values such as
# `"IPAddress ""POST /… HTTP/1.1"" 200 …"`), but DuckDB's sniffer settles on
# escape='' for log_service and then rejects those rows as having an unterminated
# quote. escape='"' reads `""` as one literal quote. Column names and types are
# left to the sniffer, which gets them right.
CSV_OPTS = "header = true, quote = '\"', escape = '\"'"


def log(msg: str) -> None:
    print(msg, flush=True)


def human(n: float) -> str:
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if n < 1024 or unit == "TiB":
            return f"{n:.0f} B" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024.0
    return f"{n} B"


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(CHUNK)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


# ------------------------------------------------------------- download
#
# Drive serves anything over ~100 MB behind a "can't scan this file for
# viruses" HTML interstitial: the real bytes need a `confirm` token plus a
# per-session `uuid`, both of which come out of that page's hidden form. The
# token is short-lived, so it is re-derived on every attempt rather than cached,
# and the partial file is resumed with a Range header.


def _opener() -> urllib.request.OpenerDirector:
    return urllib.request.build_opener(
        urllib.request.HTTPCookieProcessor(CookieJar()))


def _confirm_url(opener, file_id: str) -> str:
    """Resolve the interstitial into a direct-download URL for `file_id`."""
    base = ("https://drive.usercontent.google.com/download"
            f"?id={file_id}&export=download")
    req = urllib.request.Request(base, headers={"User-Agent": UA})
    with opener.open(req, timeout=60) as resp:
        ctype = resp.headers.get("Content-Type", "")
        if "text/html" not in ctype:
            # Small enough that Drive served the bytes directly.
            return base
        page = resp.read(1 << 20).decode("utf-8", "replace")
    form = re.search(r'<form[^>]+id="download-form"[^>]+action="([^"]+)"', page)
    if not form:
        snippet = re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", page))[:400]
        raise SystemExit(
            "Drive did not return the expected confirm form — the file may be "
            f"rate-limited or no longer shared. Page said: {snippet}")
    fields = {m.group(1): html.unescape(m.group(2)) for m in re.finditer(
        r'<input type="hidden" name="([^"]+)" value="([^"]*)">', page)}
    return html.unescape(form.group(1)) + "?" + urllib.parse.urlencode(fields)


def download(system: str, target: Path, retries: int = 6) -> None:
    """Fetch <system>.zip -> target, resuming a partial .part file."""
    if target.exists() and zipfile.is_zipfile(target):
        log(f"  archive present ({target.name}, {human(target.stat().st_size)})"
            " — skipping download")
        return
    meta = DRIVE_FILES[system]
    expect = meta["bytes"]
    target.parent.mkdir(parents=True, exist_ok=True)
    tmp = target.with_suffix(target.suffix + ".part")

    for attempt in range(1, retries + 1):
        have = tmp.stat().st_size if tmp.exists() else 0
        if expect and have == expect:
            break
        if expect and have > expect:
            log(f"  {tmp.name} is longer than expected — restarting")
            tmp.unlink()
            have = 0
        try:
            opener = _opener()
            url = _confirm_url(opener, meta["id"])
            headers = {"User-Agent": UA}
            if have:
                headers["Range"] = f"bytes={have}-"
                log(f"  resuming at {human(have)}")
            else:
                log(f"  downloading {system}.zip (drive id {meta['id']})")
            req = urllib.request.Request(url, headers=headers)
            with opener.open(req, timeout=120) as resp:
                if have and resp.status != 206:
                    # Server ignored the Range — start over rather than
                    # appending the whole file onto the partial one.
                    log("  server refused Range; restarting from 0")
                    have = 0
                total = expect or (
                    int(resp.headers.get("Content-Length") or 0) + have)
                mode = "ab" if have else "wb"
                seen, next_mark = have, have + (1 << 28)
                with open(tmp, mode) as out:
                    while True:
                        b = resp.read(CHUNK)
                        if not b:
                            break
                        out.write(b)
                        seen += len(b)
                        if seen >= next_mark:
                            pct = f" ({100 * seen / total:.0f}%)" if total else ""
                            log(f"    {human(seen)}{pct}")
                            next_mark += 1 << 28
            if expect and seen != expect:
                raise OSError(f"short read: {seen} of {expect} bytes")
            break
        except Exception as e:  # noqa: BLE001 — transient network/Drive errors
            if attempt == retries:
                raise SystemExit(f"download failed after {retries} attempts: {e}")
            wait = 5.0 * attempt
            log(f"  attempt {attempt} failed ({e}); retrying in {wait:.0f}s")
            time.sleep(wait)

    if not zipfile.is_zipfile(tmp):
        raise SystemExit(f"{tmp}: not a zip archive — Drive layout changed?")
    tmp.rename(target)
    log(f"  downloaded {target.name} ({human(target.stat().st_size)}, "
        f"sha256 {sha256_of(target)})")


# -------------------------------------------------------------- extract


def extract(archive: Path, out: Path, system: str) -> None:
    """Unzip `archive` into `out`, stripping its `<system>/` root component.

    The archive roots everything at `Bank/` (capitalised, as upstream names the
    system); `out` is data/bank, so the leading component is dropped rather than
    letting the archive dictate the on-disk case. macOS cruft (.DS_Store,
    __MACOSX) is skipped.
    """
    with zipfile.ZipFile(archive) as zf:
        members = [i for i in zf.infolist()
                   if not i.is_dir()
                   and Path(i.filename).name != ".DS_Store"
                   and not i.filename.startswith("__MACOSX/")]
        if not members:
            raise SystemExit(f"{archive.name}: no files to extract")
        roots = {Path(i.filename).parts[0] for i in members}
        if roots != {system}:
            raise SystemExit(
                f"{archive.name}: expected everything under {system}/, "
                f"found roots {sorted(roots)}")
        # relative path inside the system dir, i.e. Bank/x/y.csv -> x/y.csv
        rel = {i.filename: Path(*Path(i.filename).parts[1:]) for i in members}
        total = sum(i.file_size for i in members)
        log(f"  extracting {len(members)} files ({human(total)} uncompressed) "
            f"-> {out}")
        written = 0
        for i in members:
            dest = out / rel[i.filename]
            if dest.exists() and dest.stat().st_size == i.file_size:
                written += i.file_size
                continue
            dest.parent.mkdir(parents=True, exist_ok=True)
            tmp = dest.with_suffix(dest.suffix + ".part")
            with zf.open(i) as src, open(tmp, "wb") as dst:
                shutil.copyfileobj(src, dst, length=1 << 22)
            tmp.rename(dest)
            written += i.file_size
            log(f"    {rel[i.filename]} ({human(i.file_size)}) "
                f"[{100 * written / total:.0f}%]")


# ------------------------------------------------------------- duckdb


def sql_literal(p: str) -> str:
    return "'" + p.replace("'", "''") + "'"


def csv_tables(src: Path) -> dict[str, str]:
    """{output parquet stem: CSV glob relative to src}, discovered on disk.

    Two shapes exist: telemetry/{DATE}/{log,metric,trace}/*.csv, which is one
    logical table sharded by day, and the two CSVs at the system root. Reading
    the tree rather than hardcoding a file list is what lets --system work: each
    OpenRCA system ships a different set of CSVs.
    """
    tables = {}
    for p in sorted(src.glob("telemetry/*/*/*.csv")):
        tables.setdefault(p.stem, f"telemetry/*/{p.parent.name}/{p.name}")
    for p in sorted(src.glob("*.csv")):
        tables.setdefault(p.stem, p.name)
    return tables


def transform(src: Path, out_dir: Path) -> None:
    con = duckdb.connect()
    out_dir.mkdir(parents=True, exist_ok=True)
    tables = csv_tables(src)
    if not tables:
        raise SystemExit(f"{src}: no CSVs found")
    for name, pattern in tables.items():
        out = out_dir / f"{name}.parquet"
        if out.exists():
            log(f"  {out.name} present — skipping (delete it to rebuild)")
            continue
        matches = sorted(src.glob(pattern))
        csv_bytes = sum(p.stat().st_size for p in matches)
        log(f"  {name}: {len(matches)} csv ({human(csv_bytes)}) -> {out.name}")

        # Day-sharded telemetry gets the two derived columns; the root CSVs are
        # copied verbatim (record.csv already carries a parsed `datetime`, and
        # neither has a path to read a capture day out of).
        read = f"read_csv({sql_literal(str(src / pattern))}, {CSV_OPTS}"
        if pattern.startswith("telemetry/"):
            # `filename` feeds DAY_SQL and is then excluded from the output.
            select = f"SELECT * EXCLUDE (filename), {DAY_SQL}, {TS_SQL}"
            read += ", filename = true)"
        else:
            select = "SELECT *"
            read += ")"

        tmp = out.with_suffix(".parquet.part")
        tmp.unlink(missing_ok=True)
        # No ORDER BY: DuckDB preserves insertion order, and a glob is read in
        # sorted-filename order, which for telemetry/{YYYY_MM_DD}/ is date
        # order. Row order within a day is the CSV's own.
        con.execute(f"COPY ({select} FROM {read}) TO {sql_literal(str(tmp))} "
                    f"(FORMAT parquet, COMPRESSION zstd)")
        tmp.rename(out)
        n = con.execute(
            f"SELECT count(*) FROM {sql_literal(str(out))}").fetchone()[0]
        log(f"    {n:,} rows, {human(out.stat().st_size)} on disk "
            f"({csv_bytes / max(out.stat().st_size, 1):.1f}x vs csv)")


def database(out_dir: Path, db: Path) -> None:
    """Build <system>.duckdb: one entry point over every parquet in out_dir.

    Views, not tables — the parquet stays the single physical copy of the data.
    Telemetry lands in `main`; `record` and `query` land in `truth`, so a consumer
    that must not see the answers has one schema name to avoid, and the query log
    of a run can be checked for it afterwards. `main.tasks` is the safe projection
    of query: the task text without its `scoring_points`.

    Rebuilt from scratch on every run and cheap to rebuild, so whatever schemas
    consumers created in it are dropped. Paths are absolute and stored verbatim by
    the view: moving the repository means re-running this.
    """
    db.unlink(missing_ok=True)
    con = duckdb.connect(str(db))
    con.execute("CREATE SCHEMA truth")
    for p in sorted(out_dir.glob("*.parquet")):
        schema = "truth" if p.stem in ("record", "query") else "main"
        con.execute(f"CREATE VIEW {schema}.{p.stem} AS "
                    f"SELECT * FROM {sql_literal(str(p.resolve()))}")
    con.execute(
        "CREATE VIEW main.tasks AS SELECT row_number() OVER () AS task_id, "
        "task_index, instruction FROM truth.query")
    names = [r[0] for r in con.execute(
        "SELECT table_schema || '.' || table_name FROM information_schema.tables "
        "ORDER BY 1").fetchall()]
    con.close()
    log(f"  {db.name}: {', '.join(names)}")


# ----------------------------------------------------------------- main


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--system", default="Bank", choices=sorted(DRIVE_FILES),
                    help="OpenRCA system to fetch (default: Bank)")
    args = ap.parse_args()

    system = args.system
    src = DATA / system.lower()      # extracted CSVs
    archive = DATA / f"{system.lower()}.zip"
    out_dir = src / "parquet"        # parquet, alongside the telemetry/

    telemetry = src / "telemetry"

    log(f"OpenRCA {system} -> {out_dir}")
    if telemetry.is_dir():
        log(f"  extraction present ({src}) — skipping download + extract")
    elif any(out_dir.glob("*.parquet")):
        log(f"  parquet present and the telemetry CSVs are already discarded "
            f"— nothing to do")
        return
    else:
        download(system, archive)
        extract(archive, src, system)

    log(f"  duckdb {duckdb.__version__}")
    transform(src, out_dir)

    if archive.exists():
        archive.unlink()
        log(f"  removed {archive.name}")

    # Every CSV is an intermediate the parquet fully replaces — 24.6 GB for Bank.
    # The two at the root are tiny, but keeping them would leave query and record
    # readable two ways, so parquet/ is the single representation. Losing the CSVs
    # means a re-run re-downloads, which is why the parquet check above
    # short-circuits before the download.
    shutil.rmtree(telemetry)
    roots = sorted(src.glob("*.csv"))
    for p in roots:
        p.unlink()
    log(f"  removed {telemetry.name}/ and "
        f"{', '.join(p.name for p in roots)} (re-fetchable; parquet/ kept)")

    log(f"  done. ingest the log text with:\n"
        f"    bench ingest --source {out_dir / 'log_service.parquet'} "
        f"--format parquet --column value "
        f"--id openrca-{system.lower()}-log")


if __name__ == "__main__":
    main()
