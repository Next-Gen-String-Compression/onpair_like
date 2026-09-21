#!/usr/bin/env python3
"""Render numbered length/selectivity grids from SA/LCP gen-report.json files."""
from __future__ import annotations

import argparse
import csv
import json
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.backends.backend_pdf import PdfPages
from matplotlib.patches import Patch, Rectangle
import numpy as np


def load_report(path: Path):
    report = json.loads(path.read_text())
    suite = json.loads(path.with_name("suite.json").read_text())
    lengths = report["request"]["lengths"]
    buckets = report["request"]["matching_rows"]
    counts = np.zeros((len(lengths), len(buckets)), dtype=int)
    seen = set()
    for cell in report["cells"]:
        index = cell["cell"]
        if index in seen or not 0 <= index < counts.size:
            raise ValueError(f"{path}: invalid or duplicate cell {index}")
        seen.add(index)
        i, j = divmod(index, len(buckets))
        if cell["length"] != lengths[i] or cell["matching_rows"] != buckets[j]:
            raise ValueError(f"{path}: inconsistent cell bounds")
        counts[i, j] = cell["generated"]
    if len(seen) != counts.size or int(counts.sum()) != report["queries"]:
        raise ValueError(f"{path}: incomplete coverage report")
    # Generation records identity before blessing adds the oracle's binding.
    checksum = report["dataset_checksum"]
    if checksum != suite.get("provenance", {}).get("dataset_checksum"):
        raise ValueError(f"{path}: suite and report refer to different datasets")
    blessed_checksum = suite["dataset"].get("checksum")
    if (blessed_checksum is not None or suite.get("blessed_at")) and blessed_checksum != checksum:
        raise ValueError(f"{path}: suite and report refer to different datasets")
    return path, suite, report, counts


def row_labels(report):
    labels = []
    previous = -1
    for bucket in report["request"]["matching_rows"]:
        lo, hi = bucket["min"], bucket["max"]
        if lo == hi == 0:
            label = "0% (none)"
        elif lo == hi == 1:
            label = "1 row"
        elif lo == previous + 1:
            label = f"≤{100 * hi / report['total_rows']:.3g}%"
        else:
            label = f"{lo:,}–{hi:,} rows"
        labels.append(label)
        previous = hi
    return labels


def draw_page(suites):
    single = len(suites) == 1
    fig, axes = plt.subplots(1, len(suites), squeeze=False,
                             figsize=(8.5, 7.6) if single else (5.8 * len(suites), 6.7))
    fig.suptitle("SA + LCP needle coverage", fontsize=17, y=.97)
    for ax, (_, suite, report, counts) in zip(axes[0], suites):
        quota = report["request"]["per_cell"]
        ax.imshow(counts / max(quota, 1), cmap="Blues", vmin=0, vmax=1,
                  interpolation="nearest", aspect="auto")
        for cell in report["cells"]:
            i, j = divmod(cell["cell"], counts.shape[1])
            if cell["available"] == 0:
                ax.add_patch(Rectangle((j-.5, i-.5), 1, 1, facecolor="#eef1f5",
                                      edgecolor="#bcc5d0", hatch="///", linewidth=0))
            elif cell["status"] == "unresolved":
                ax.add_patch(Rectangle((j-.46, i-.46), .92, .92, fill=False,
                                      edgecolor="#d48213", linewidth=2))
            ax.text(j, i, str(cell["generated"]), ha="center", va="center", fontsize=9,
                    color="white" if counts[i, j] >= .6 * max(quota, 1) else "#293442")
        lengths = report["request"]["lengths"]
        ax.set_yticks(range(len(lengths)), [f"{b['min']}–{b['max']}" for b in lengths])
        ax.set_xticks(range(counts.shape[1]), row_labels(report), rotation=55, ha="right")
        ax.set_ylabel("Needle length (bytes)")
        ax.tick_params(length=0, labelsize=9)
        ax.set_xticks(np.arange(-.5, counts.shape[1], 1), minor=True)
        ax.set_yticks(np.arange(-.5, counts.shape[0], 1), minor=True)
        ax.grid(which="minor", color="white", linewidth=1)
        ax.tick_params(which="minor", bottom=False, left=False)
        for spine in ax.spines.values():
            spine.set_visible(False)
        blessed = "blessed" if suite.get("blessed_at") else "unblessed"
        ax.set_title(f"{suite['dataset']['id']} · {report['total_rows']:,} rows\n"
                     f"{report['queries']:,} needles · quota {quota} · seed {report['request']['seed']}\n"
                     f"{blessed} · {report['dataset_checksum']}", fontsize=10, pad=12)
    fig.subplots_adjust(left=.13 if single else .07, right=.99, top=.77,
                        bottom=.36 if single else .32, wspace=.3)
    separator = "\n" if single else " "
    fig.text(.5, .19 if single else .145,
             f"Selectivity = rows containing the needle / all rows.{separator}Adjacent buckets show their upper bound.",
             ha="center", fontsize=9)
    fig.legend(handles=[Patch(facecolor=plt.get_cmap("Blues")(1.), label="Quota filled"),
                        Patch(facecolor="#eef1f5", edgecolor="#bcc5d0", hatch="///",
                              label="No positive substring in bucket"),
                        Patch(facecolor="none", edgecolor="#d48213", label="Negative search incomplete")],
               loc="lower center", bbox_to_anchor=(.5, .065), ncol=1 if single else 3,
               frameon=False, fontsize=9)
    fig.text(.5, .018, "Numbers count unique byte strings per suite. Exact bucket bounds and availability are in the CSV.",
             ha="center", fontsize=8)
    return fig


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inputs", type=Path, nargs="+", help="suite directories, reports, or generated roots")
    parser.add_argument("--out", type=Path, required=True, help="output directory for PNG pages, PDF and CSV")
    parser.add_argument("--panels-per-page", type=int, choices=(1, 2, 3), default=3,
                        help="use 1 for a separate PNG per suite (default: 3)")
    args = parser.parse_args()
    paths = {}
    for path in args.inputs:
        found = [path] if path.is_file() else sorted(path.rglob("gen-report.json"))
        if not found:
            parser.error(f"no reports in {path}")
        # Preserve input order, without plotting the same report twice.
        paths.update((p.resolve(), None) for p in found)
    suites = [load_report(path) for path in paths]
    args.out.mkdir(parents=True, exist_ok=True)
    with (args.out / "coverage.csv").open("w", newline="") as file:
        fields = ["dataset", "checksum", "suite", "total_rows", "seed", "length_min", "length_max",
                  "matching_rows_min", "matching_rows_max", "requested", "generated", "available", "status"]
        writer = csv.DictWriter(file, fields)
        writer.writeheader()
        for path, suite, report, _ in suites:
            for cell in report["cells"]:
                writer.writerow(dict(dataset=suite["dataset"]["id"], checksum=report["dataset_checksum"],
                                     suite=path.parent.name, total_rows=report["total_rows"],
                                     seed=report["request"]["seed"], length_min=cell["length"]["min"],
                                     length_max=cell["length"]["max"], matching_rows_min=cell["matching_rows"]["min"],
                                     matching_rows_max=cell["matching_rows"]["max"],
                                     **{key: cell[key] for key in ("requested", "generated", "available", "status")}))
    with PdfPages(args.out / "coverage.pdf") as pdf:
        for start in range(0, len(suites), args.panels_per_page):
            fig = draw_page(suites[start:start + args.panels_per_page])
            fig.savefig(args.out / f"coverage-{start // args.panels_per_page + 1}.png", dpi=180)
            pdf.savefig(fig)
            plt.close(fig)
    print(f"Plotted {len(suites)} suites -> {args.out}")


if __name__ == "__main__":
    main()
