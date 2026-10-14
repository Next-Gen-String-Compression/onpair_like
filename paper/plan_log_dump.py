# /// script
# requires-python = ">=3.11"
# dependencies = ["pyarrow>=17"]
# ///
"""Dump a spec's columns and suite needles in the format the plan loggers read.

    uv run paper/plan_log_dump.py SPEC.toml OUT_DIR

Per dataset, OUT_DIR/<dataset>.col is u64 num_rows, u64 payload_len,
num_rows+1 u64 offsets, payload; OUT_DIR/<dataset>.ndl is u64 count, then u64
len and the bytes per needle, in suite order. Little-endian. A suite belongs to
the dataset its parent directory is named after.
"""
import base64
import json
import struct
import sys
import tomllib
from pathlib import Path

import pyarrow.ipc as ipc


def read_column(path):
    with path.open("rb") as fh:
        try:
            table = ipc.open_file(fh).read_all()
        except Exception:
            fh.seek(0)
            table = ipc.open_stream(fh).read_all()
    col = table.column(0).combine_chunks()
    if col.null_count:
        raise SystemExit(f"{path}: {col.null_count} nulls")
    return [r.encode() if isinstance(r, str) else r for r in col.to_pylist()]


def needles(path):
    out = []
    for line in path.read_text().splitlines():
        if line.strip():
            n = json.loads(line)["needles"][0]
            out.append(base64.b64decode(n["b64"]) if isinstance(n, dict) else n.encode())
    return out


def main():
    spec_path, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    spec = tomllib.loads(spec_path.read_text())
    suites = {(spec_path.parent / s["path"]).resolve().parent.name: (spec_path.parent / s["path"]).resolve()
              for s in spec["suites"]}
    for d in spec["datasets"]:
        ds_dir = (spec_path.parent / d["path"]).resolve()
        rows = read_column(ds_dir / "data.arrow")
        offsets = [0]
        for r in rows:
            offsets.append(offsets[-1] + len(r))
        with (out / f"{ds_dir.name}.col").open("wb") as fh:
            fh.write(struct.pack("<QQ", len(rows), offsets[-1]))
            fh.write(struct.pack(f"<{len(offsets)}Q", *offsets))
            fh.write(b"".join(rows))
        pool = needles(suites[ds_dir.name] / "queries.jsonl")
        with (out / f"{ds_dir.name}.ndl").open("wb") as fh:
            fh.write(struct.pack("<Q", len(pool)))
            for n in pool:
                fh.write(struct.pack("<Q", len(n)) + n)
        print(f"{ds_dir.name:<22} rows {len(rows):>8}  needles {len(pool):>5}")


if __name__ == "__main__":
    main()
