import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("bench_viz", ROOT / "bench_viz.py")
bench_viz = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench_viz)


def query_row(**overrides):
    row = {
        "kind": "query",
        "status": "ok",
        "candidate": "onpair",
        "config": '{"bits":12}',
        "config_hash": "abc",
        "strategy": "decode",
        "scanner": "memmem",
        "dataset": "mini",
        "chunk_rows": 0,
        "op": "contains",
        "query_id": "mini.contains",
        "derived": {"selectivity": 0.125, "needle_len_total": 8},
        "latency": {"median_ns": 2000},
        "ns_per_row": 10.0,
        "gbps_raw": 4.0,
        "prefilter": {"decode_ns": {"ns": 500, "origin": "harness"}},
    }
    row.update(overrides)
    return row


class BenchVizTests(unittest.TestCase):
    def test_normalizes_decode_only_metrics(self):
        point = bench_viz.normalize_query(query_row(), "test-run")
        self.assertEqual(point["decode_gbps"], 16.0)
        self.assertEqual(point["decode_ns_per_row"], 2.5)
        self.assertEqual(point["selectivity"], 0.125)

    def test_non_decode_strategy_does_not_become_baseline(self):
        point = bench_viz.normalize_query(
            query_row(strategy="compressed"), "test-run"
        )
        self.assertIsNone(point["decode_gbps"])
        self.assertIsNone(point["decode_ns_per_row"])

    def test_loads_directory_and_ignores_non_query_rows(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory) / "demo"
            run.mkdir()
            rows = [
                {"kind": "build", "candidate": "onpair"},
                query_row(),
                query_row(status="unsupported"),
            ]
            (run / "results.jsonl").write_text(
                "\n".join(json.dumps(row) for row in rows) + "\n",
                encoding="utf-8",
            )
            points = bench_viz.load_results([run])
        self.assertEqual(len(points), 1)
        self.assertEqual(points[0]["source"], "demo")

    def test_html_is_self_contained_and_script_safe(self):
        point = bench_viz.normalize_query(
            query_row(query_id="</script><script>alert(1)</script>"), "test-run"
        )
        html = bench_viz.build_html([point], {"title": "Demo", "show": []})
        self.assertNotIn("__BENCH_VIZ_", html)
        self.assertNotIn("</script><script>alert", html)
        self.assertIn("\\u003c/script>", html)


if __name__ == "__main__":
    unittest.main()
