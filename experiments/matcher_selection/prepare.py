#!/usr/bin/env python3
"""Prepare the configured columns using the repository's shared dataset recipes."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

try:
    import tomllib
except ImportError:
    import tomli as tomllib
import yaml

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_CONFIG = Path(__file__).with_name("config.toml")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    parser.add_argument("--dataset", action="append", default=[])
    parser.add_argument("--list", action="store_true", help="show index requirements without downloading")
    args = parser.parse_args()
    config_path = args.config.resolve()
    with config_path.open("rb") as file:
        config = tomllib.load(file)
    with (ROOT / "datasets/sources.yaml").open() as file:
        recipes = {entry["id"]: entry for entry in yaml.safe_load(file)["datasets"]}
    unknown = set(args.dataset) - {entry["id"] for entry in config["datasets"]}
    if unknown:
        parser.error(f"datasets not in config: {sorted(unknown)}")
    selected = [entry for entry in config["datasets"]
                if not args.dataset or entry["id"] in args.dataset]
    if not selected:
        parser.error("no datasets selected")
    # Validate everything before building or starting any downloads.
    for entry in selected:
        dataset = entry["id"]
        if dataset not in recipes:
            parser.error(f"{dataset}: no recipe in datasets/sources.yaml")
        if (config_path.parent / entry["path"]).resolve() != ROOT / "datasets" / dataset:
            parser.error(f"{dataset}: shared preparation writes to datasets/{dataset}; config path differs")
    if args.list:
        print("dataset\trows\tpayload MiB\tindex MiB\tsize source")
        for entry in selected:
            manifest = ROOT / "datasets" / entry["id"] / "manifest.json"
            if manifest.exists():
                data = json.loads(manifest.read_text())
                rows, payload, source = data["num_rows"], data["payload_bytes"], "prepared"
            else:
                data = recipes[entry["id"]]["approx"]
                rows, payload, source = data["rows"], data["payload_bytes"], "approximate"
            symbols = rows + payload
            mib = (16 * symbols + 64 * 1024**2 + 1024**2 - 1) // 1024**2
            budget = str(mib) if symbols <= 2**31 - 1 else "exceeds SA backend limit"
            print(f"{entry['id']}\t{rows}\t{payload / 1024**2:.1f}\t{budget}\t{source}")
        return
    env = os.environ.copy()
    if "BENCH_BIN" not in env:
        subprocess.run(["cargo", "build", "--locked", "--release", "-p", "lb-harness",
                        "--bin", "bench", "--no-default-features"], cwd=ROOT, check=True)
        metadata = subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps",
                                            "--format-version", "1"], cwd=ROOT, text=True)
        env["BENCH_BIN"] = str(Path(json.loads(metadata)["target_directory"]) / "release" / "bench")
    else:
        env["BENCH_BIN"] = str(Path(env["BENCH_BIN"]).resolve())
    command = [sys.executable, str(ROOT / "datasets/prepare.py")]
    for entry in selected:
        command.extend(["--dataset", entry["id"]])
    subprocess.run(command, cwd=ROOT, env=env, check=True)


if __name__ == "__main__":
    main()
