#!/usr/bin/env python3
"""Compare eligible matchers using a snapshot of a local SpiralDB/OnPair checkout."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys

try:
    import tomllib
except ImportError:
    import tomli as tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def command_output(command, cwd=None):
    return subprocess.check_output(command, cwd=cwd, text=True).strip()


def git_info(path):
    try:
        return {"commit": command_output(["git", "rev-parse", "HEAD"], path),
                "status": command_output(["git", "status", "--short"], path)}
    except subprocess.CalledProcessError:
        return {"commit": None, "status": "not a git checkout"}


def snapshot(source):
    files = sorted([source / "Cargo.toml", *source.joinpath("src").rglob("*.rs"),
                    *source.joinpath("benches").rglob("*.rs")])
    hashes = {str(p.relative_to(source)): sha(p) for p in files}
    digest = hashlib.sha256(json.dumps(hashes, sort_keys=True).encode())
    digest.update((HERE / "adapter.rs").read_bytes())
    base = HERE / "build" / digest.hexdigest()[:20]
    target = base / "onpair"
    edits = {
        "src/search/substring/mod.rs": "\n/// Experimental matcher access, present only in the benchmark snapshot.\npub mod matcher_experiment;\n",
        "src/search/mod.rs": "\n#[doc(hidden)]\npub use substring::matcher_experiment;\n",
    }
    plan_name = "src/search/substring/plan/mod.rs"
    expected = "#[cfg(test)]\npub(super) use select::is_eligible;"
    if (source / plan_name).read_text().count(expected) != 1:
        raise ValueError("unsupported local OnPair layout: cannot expose matcher eligibility")
    # Compare cached files against the source plus exactly the declared edits.
    # No production functions, matcher bodies or optimizer attributes are replaced.
    for path in files:
        name = str(path.relative_to(source))
        content = path.read_bytes()
        if name in edits:
            content += edits[name].encode()
        elif name == plan_name:
            content = content.replace(expected.encode(), b"pub(super) use select::is_eligible;")
        dest = target / name
        if dest.exists() and dest.read_bytes() != content:
            raise ValueError(f"cached OnPair snapshot changed: {dest}")
        if not dest.exists():
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(content)
    adapter = target / "src/search/substring/matcher_experiment.rs"
    if adapter.exists() and adapter.read_bytes() != (HERE / "adapter.rs").read_bytes():
        raise ValueError(f"cached adapter changed: {adapter}")
    shutil.copy2(HERE / "adapter.rs", adapter)
    (base / "source-hashes.json").write_text(json.dumps(hashes, indent=2) + "\n")
    return base, target, hashes


def cases(args, parser):
    if args.case:
        if args.dataset:
            parser.error("use either --case or --dataset")
        chosen = [(Path(ds).resolve(), Path(suite).resolve()) for ds, suite in args.case]
    else:
        config_path = args.config.resolve()
        config = tomllib.loads(config_path.read_text())
        entries = config["datasets"]
        unknown = set(args.dataset) - {e["id"] for e in entries}
        if unknown:
            parser.error(f"unknown datasets: {sorted(unknown)}")
        chosen = []
        for entry in entries:
            if args.dataset and entry["id"] not in args.dataset:
                continue
            root = config_path.parent / config["output_root"] / entry["id"]
            suites = sorted(root.glob("*/suite.json"))
            if len(suites) != 1:
                parser.error(f"{entry['id']}: expected one generated suite, found {len(suites)}; generate it or use --case DATASET SUITE")
            chosen.append(((config_path.parent / entry["path"]).resolve(), suites[0].parent.resolve()))
    ids = set()
    validated = []
    for dataset, suite in chosen:
        manifest = json.loads((dataset / "manifest.json").read_text())
        binding = json.loads((suite / "suite.json").read_text())
        if binding["dataset"].get("checksum") != manifest["checksum"] or binding["dataset"]["id"] != manifest["id"]:
            parser.error(f"{suite}: suite and dataset identity mismatch; generate and bless the suite first")
        if not binding.get("blessed_at"):
            parser.error(f"{suite}: suite is not independently verified")
        name = manifest["id"]
        if not name or name in ids or Path(name).name != name or name in (".", ".."):
            parser.error(f"invalid or duplicate dataset ID: {name}")
        ids.add(name)
        validated.append({"id": name, "dataset": str(dataset), "suite": str(suite),
                          "checksum": manifest["checksum"],
                          "queries_sha256": sha(suite / "queries.jsonl")})
    if not validated:
        parser.error("no dataset/suite pairs selected")
    return validated


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--onpair", type=Path, required=True, help="local refactored SpiralDB/OnPair checkout")
    parser.add_argument("--config", type=Path, default=HERE / "config.toml")
    parser.add_argument("--dataset", action="append", default=[])
    parser.add_argument("--case", nargs=2, action="append", metavar=("DATASET_DIR", "SUITE_DIR"),
                        help="explicit prepared inputs, reusable across experiments")
    parser.add_argument("--out", type=Path, required=True, help="new result directory")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--min-millis", type=int, default=5)
    parser.add_argument("--min-iters", type=int, default=3)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--bits", type=int, default=16)
    parser.add_argument("--query-limit", type=int, help="explicit smoke-test limit; omit for all queries")
    parser.add_argument("--offline", action="store_true", help="build using already cached Cargo dependencies")
    args = parser.parse_args()
    selected = cases(args, parser)
    source = args.onpair.resolve()
    if not (source / "src/search/substring/plan/select.rs").exists():
        parser.error("--onpair must point to the refactored SpiralDB/OnPair checkout")
    if args.rounds < 1 or args.min_iters < 1 or args.min_millis < 0 or not 9 <= args.bits <= 16 or (args.query_limit is not None and args.query_limit < 1):
        parser.error("invalid measurement parameters")
    if args.out.exists():
        parser.error("output already exists; choose a new directory")
    base, target, hashes = snapshot(source)
    runner = base / "runner"
    runner.mkdir(exist_ok=True)
    manifest = runner / "Cargo.toml"
    manifest.write_text(f'''[package]
name = "matcher-selection-bench"
version = "0.1.0"
edition = "2024"
[workspace]
[dependencies]
onpair = {{ path = {json.dumps(str(target))} }}
lb-harness = {{ path = {json.dumps(str(ROOT / "harness"))}, default-features = false }}
clap = {{ version = "4.6.1", features = ["derive"] }}
serde_json = "1"
[[bin]]
name = "matcher-selection-bench"
path = {json.dumps(str(HERE / "benchmark.rs"))}
[profile.release]
debug = true
lto = "thin"
codegen-units = 1
''')
    if not (runner / "Cargo.lock").exists():
        # Seed resolution from the checked-in workspace dependency versions.
        shutil.copy2(ROOT / "Cargo.lock", runner / "Cargo.lock")
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(HERE / "build/target")
    offline = ["--offline"] if args.offline else []
    subprocess.run(["cargo", "build", "--release", "--manifest-path", str(manifest), *offline], env=env, check=True)
    args.out.mkdir(parents=True)
    # A concurrent build must not replace the executable between datasets.
    executable = args.out.resolve() / "matcher-selection-bench"
    shutil.copy2(HERE / "build/target/release/matcher-selection-bench", executable)
    provenance = {"status": "running", "started_at": datetime.now(timezone.utc).isoformat(),
        "onpair": {"path": str(source), **git_info(source), "source_sha256": hashes},
        "harness": git_info(ROOT), "adapter_sha256": sha(HERE / "adapter.rs"),
        "runner_sha256": sha(HERE / "benchmark.rs"), "binary_sha256": sha(executable),
        "launcher_sha256": sha(Path(__file__)), "report_sha256": sha(HERE / "report.py"),
        "rustc": command_output(["rustc", "-Vv"]), "platform": platform.platform(),
        "machine": platform.machine(), "processor": platform.processor(),
        "rustflags": env.get("RUSTFLAGS", ""), "cases": selected,
        "settings": {key: getattr(args, key) for key in ("rounds", "min_millis", "min_iters", "seed", "bits", "query_limit")}}
    if sys.platform == "darwin":
        cpu = subprocess.run(["sysctl", "-n", "machdep.cpu.brand_string"], capture_output=True, text=True)
        provenance["cpu"] = cpu.stdout.strip() if cpu.returncode == 0 else None
    shutil.copy2(runner / "Cargo.lock", args.out / "Cargo.lock")
    (args.out / "manifest.json").write_text(json.dumps(provenance, indent=2) + "\n")
    try:
        for case in selected:
            command = [str(executable), "--dataset", case["dataset"], "--suite", case["suite"],
                       "--out", str(args.out / f"{case['id']}.jsonl")]
            for key in ("rounds", "min_millis", "min_iters", "seed", "bits", "query_limit"):
                if getattr(args, key) is not None:
                    command += ["--" + key.replace("_", "-"), str(getattr(args, key))]
            subprocess.run(command, check=True)
        provenance["status"] = "complete"
    except BaseException:
        provenance["status"] = "incomplete"
        raise
    finally:
        provenance["finished_at"] = datetime.now(timezone.utc).isoformat()
        (args.out / "manifest.json").write_text(json.dumps(provenance, indent=2) + "\n")
    subprocess.run([sys.executable, str(HERE / "report.py"), str(args.out)], check=True)


if __name__ == "__main__":
    main()
