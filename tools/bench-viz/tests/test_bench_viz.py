import importlib.util
import json
import re
import sys
import tempfile
import types
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
# bench_viz is a script, not a package, so its sibling modules are only
# importable once its directory is on the path. Running pytest from the
# repository root does not put it there.
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
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
        "dataset_checksum": "xxh3:mini",
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
                query_row(op="prefix", query_id="mini.prefix"),
                query_row(op="suffix", query_id="mini.suffix"),
            ]
            (run / "results.jsonl").write_text(
                "\n".join(json.dumps(row) for row in rows) + "\n",
                encoding="utf-8",
            )
            points, builds, ignored = bench_viz.load_results([run])
        self.assertEqual(len(points), 1)
        self.assertEqual(points[0]["source"], "demo")
        # Anchored matches are not this tool's subject, and are reported rather
        # than dropped in silence.
        self.assertEqual(ignored, {"prefix": 1, "suffix": 1})
        self.assertEqual(bench_viz.summarize_ignored(ignored),
                         "2 rows: prefix 1, suffix 1")

    def test_the_scope_is_substring_search(self):
        """Every operation is either in scope or reported as out of it.

        A prefilter serves LIKE '%n%' and its multi-needle forms; nothing it
        measures says anything about an anchored match. Pooling those in would
        dilute every summary the panels report, so they never enter the payload.
        """
        for op in ("contains", "multi_contains", "contains_any"):
            self.assertTrue(bench_viz.is_substring_search({"op": op}), op)
        for op in ("prefix", "suffix", None, "equals"):
            self.assertFalse(bench_viz.is_substring_search({"op": op}), op)

    def test_a_run_of_only_anchored_matches_says_so(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory) / "anchored"
            run.mkdir()
            (run / "results.jsonl").write_text(
                json.dumps(query_row(op="prefix")) + "\n", encoding="utf-8")
            with self.assertRaises(ValueError) as caught:
                bench_viz.load_results([run])
        self.assertIn("no substring-search query rows", str(caught.exception))
        self.assertIn("prefix 1", str(caught.exception))

    def test_html_is_self_contained_and_script_safe(self):
        point = bench_viz.normalize_query(
            query_row(query_id="</script><script>alert(1)</script>"), "test-run"
        )
        html = bench_viz.build_html([point], {"title": "Demo", "show": []})
        self.assertNotIn("__BENCH_VIZ_", html)
        self.assertNotIn("</script><script>alert", html)
        self.assertIn("\\u003c/script>", html)
        self.assertIn("Benchmark Explorer 3000™", html)
        self.assertIn('<span class="section-kicker">CANDIDATES</span>', html)
        self.assertIn('id="query-details"', html)
        self.assertNotIn('id="mincut-graph"', html)
        self.assertIn('id="bench-viz-mincut-graphs"', html)
        self.assertIn('"version":2,"archives":{}', html)

    def test_html_embeds_a_compressed_mincut_archive(self):
        point = bench_viz.normalize_query(query_row(), "run")
        archive = {"version": 2, "archives": {
            "mincut-demo": {
                "encoding": "gzip+base64", "data": "H4sIAAAAA", "queries": 1,
                "raw_bytes": 100, "compressed_bytes": 20,
            }
        }}
        html = bench_viz.build_html(
            [point], {"title": "Demo", "show": []}, mincut_archives=archive)
        self.assertIn('"encoding":"gzip+base64"', html)
        self.assertIn('"data":"H4sIAAAAA"', html)

    def test_normalizes_query_inspection_facts(self):
        row = query_row(
            candidate_version="v7",
            derived={
                "selectivity": 0.125,
                "needle_len_total": 8,
                "needle_lens": [8],
                "match_count": 25,
                "rarest_byte_freq": 0.01,
            },
            gate={"expected_count": 25, "actual_count": 25, "hash_ok": True},
            latency={
                "min_ns": 1800, "p25_ns": 1900, "median_ns": 2000,
                "p75_ns": 2100, "p99_ns": 2200, "max_ns": 2250,
                "mean_ns": 2005, "stddev_ns": 80, "samples": 31,
            },
            prefilter={
                "cover_points": 2, "cover_ranges": 3, "comparison_cost": 8,
                "covered_codes": 123, "indexed_codes": 456,
                "covered_fraction": 123 / 456, "candidate_row_fraction": 0.2,
                "profitable_hint": True,
                "prefilter_ns": {"ns": 400, "origin": "self_reported"},
                "verify_ns": {"ns": 600, "origin": "self_reported"},
                "decode_ns": {"ns": 500, "origin": "harness"},
            },
        )
        point = bench_viz.normalize_query(row, "run")
        self.assertEqual(point["candidate_version"], "v7")
        self.assertEqual(point["needle_lens"], [8.0])
        self.assertEqual(point["match_count"], 25.0)
        self.assertEqual(point["covered_codes"], 123.0)
        self.assertEqual(point["prefilter_ns"], 400.0)
        self.assertEqual(point["latency_samples"], 31.0)
        self.assertTrue(point["gate_hash_ok"])


class QueryCatalogTests(unittest.TestCase):
    def test_adds_text_and_binary_needles_to_every_measurement(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "queries.jsonl"
            path.write_text(json.dumps({
                "id": "mini.contains",
                "op": "contains",
                "needles": ["hello", {"b64": "/wA="}],
                "truth": {"hash": "xxh3:abc", "sample_indices": [3, 9]},
                "meta": {"generator": "fixture"},
            }) + "\n", encoding="utf-8")
            catalog = bench_viz.load_query_catalog([path])
        points = [
            bench_viz.normalize_query(query_row(candidate="one"), "run"),
            bench_viz.normalize_query(query_row(candidate="two"), "run"),
        ]
        matched = bench_viz.apply_query_catalog(points, catalog)
        self.assertEqual(matched, 1)
        self.assertEqual(points[0]["needles"][0]["display"], "hello")
        self.assertEqual(points[0]["needles"][1]["display"], "\\xff\\x00")
        self.assertEqual(points[1]["needles"], points[0]["needles"])

    def test_explicit_suite_directory_is_discovered(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            suite = root / "suite"
            suite.mkdir()
            (suite / "queries.jsonl").write_text("{}\n", encoding="utf-8")
            self.assertEqual(
                bench_viz.discover_query_paths([], [suite]),
                [suite / "queries.jsonl"],
            )

    def mincut_point(self, *, candidate="onpair_spiral", config='{"bits":12}',
                     version="v1", fingerprint=None, chunk_rows=0, source="run"):
        prefilter = {
            "cover_points": 2, "cover_ranges": 1, "comparison_cost": 4,
            "covered_codes": 123, "indexed_codes": 456,
        }
        if fingerprint is not None:
            prefilter["dictionary_fingerprint"] = fingerprint
        point = bench_viz.normalize_query(query_row(
            candidate=candidate, candidate_version=version, config=config,
            config_hash=f"hash-{config}", chunk_rows=chunk_rows,
            prefilter=prefilter,
        ), source)
        point["needles"] = [{"display": "needle", "byte_len": 6, "b64": None}]
        return point

    def test_mixed_dictionary_sizes_get_distinct_archives(self):
        twelve = self.mincut_point(config='{"bits":12}')
        sixteen = self.mincut_point(config='{"bits":16}')
        groups = bench_viz.mincut_archive_groups([twelve, sixteen], None)
        self.assertEqual(len(groups), 2)
        self.assertNotEqual(twelve["mincut_archive_id"], sixteen["mincut_archive_id"])
        self.assertEqual({group["bits"] for group in groups.values()}, {12, 16})

    def test_exact_fingerprint_deduplicates_candidates(self):
        fingerprint = "onpair-mincut-v1:0123456789abcdef"
        first = self.mincut_point(candidate="onpair_spiral", fingerprint=fingerprint)
        second = self.mincut_point(candidate="future_onpair", fingerprint=fingerprint,
                                   version="v9", source="other-run")
        groups = bench_viz.mincut_archive_groups([first, second], None)
        self.assertEqual(len(groups), 1)
        self.assertEqual(first["mincut_archive_id"], second["mincut_archive_id"])
        self.assertEqual(len(next(iter(groups.values()))["profiles"]), 2)

    def test_no_onpair_candidate_does_not_synthesize_a_graph(self):
        point = self.mincut_point(candidate="fsst_prefilter")
        self.assertEqual(bench_viz.mincut_archive_groups([point], None), {})
        self.assertIsNone(point["mincut_archive_id"])

    def test_chunked_candidate_is_not_misrepresented_by_one_dictionary(self):
        point = self.mincut_point(chunk_rows=1000)
        self.assertEqual(bench_viz.mincut_archive_groups([point], None), {})

    def test_exact_sidecar_is_authoritative_without_retraining_config(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            artifact_dir = run / "artifacts"
            artifact_dir.mkdir()
            sidecar = artifact_dir / "exact.lbartifact"
            sidecar.write_bytes(b"LBOPMC01-fixture")
            artifact_row = {
                "kind": "artifact",
                "candidate": "future_onpair",
                "candidate_version": "v9",
                "config": '{"candidate_specific":true}',
                "config_hash": "future-hash",
                "dataset": "mini",
                "dataset_checksum": "xxh3:mini",
                "chunk_rows": 0,
                "chunk_index": 0,
                "artifact_format": "onpair-mincut-sidecar-v1",
                "artifact_path": "artifacts/exact.lbartifact",
                "artifact_bytes": sidecar.stat().st_size,
                "export_phase": "post_run_replay",
            }
            (run / "results.jsonl").write_text(
                json.dumps(artifact_row) + "\n", encoding="utf-8")
            artifacts = bench_viz.discover_mincut_artifacts([run])

            point = self.mincut_point(
                candidate="future_onpair", version="v9",
                config='{"candidate_specific":true}', source=run.name)
            point["config_hash"] = "future-hash"
            for key in ("cover_points", "cover_ranges", "comparison_cost", "covered_codes"):
                point[key] = None
            groups = bench_viz.mincut_archive_groups([point], None, artifacts)

        self.assertEqual(len(groups), 1)
        group = next(iter(groups.values()))
        self.assertEqual(group["artifact_path"], sidecar.resolve())
        self.assertIn("artifact_format", group["profiles"][0])
        self.assertIsNotNone(point["mincut_archive_id"])

    def test_declared_sidecar_cannot_silently_fall_back_when_missing(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            row = {
                "kind": "artifact", "candidate": "onpair_spiral",
                "candidate_version": "v1", "config": '{"bits":12}',
                "config_hash": "hash", "dataset": "mini",
                "dataset_checksum": "xxh3:mini", "chunk_rows": 0,
                "chunk_index": 0, "artifact_format": "onpair-mincut-sidecar-v1",
                "artifact_path": "artifacts/missing.lbartifact", "artifact_bytes": 10,
                "export_phase": "post_run_replay",
            }
            (run / "results.jsonl").write_text(json.dumps(row) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(FileNotFoundError, "sidecar is missing"):
                bench_viz.discover_mincut_artifacts([run])

    def test_sidecar_must_come_from_the_isolated_replay_phase(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            (run / "artifact.lbartifact").write_bytes(b"LBOPMC01-fixture")
            row = {
                "kind": "artifact", "artifact_format": "onpair-mincut-sidecar-v1",
                "artifact_path": "artifact.lbartifact", "artifact_bytes": 18,
                "export_phase": "post_measurement",
            }
            (run / "results.jsonl").write_text(json.dumps(row) + "\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "not produced by the isolated"):
                bench_viz.discover_mincut_artifacts([run])

    def test_archive_builder_passes_the_sidecar_to_graph_viz(self):
        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory) / "run"
            (run / "artifacts").mkdir(parents=True)
            sidecar = run / "artifacts" / "exact.lbartifact"
            sidecar.write_bytes(b"LBOPMC01-fixture")
            artifact_row = {
                "kind": "artifact", "candidate": "future_onpair",
                "candidate_version": "v9", "config": '{"candidate_specific":true}',
                "config_hash": "future-hash", "dataset": "mini",
                "dataset_checksum": "xxh3:mini", "chunk_rows": 0,
                "chunk_index": 0, "artifact_format": "onpair-mincut-sidecar-v1",
                "artifact_path": "artifacts/exact.lbartifact",
                "artifact_bytes": sidecar.stat().st_size,
                "export_phase": "post_run_replay",
            }
            (run / "results.jsonl").write_text(
                json.dumps(artifact_row) + "\n", encoding="utf-8")
            point = self.mincut_point(
                candidate="future_onpair", version="v9",
                config='{"candidate_specific":true}', source="run")
            point["config_hash"] = "future-hash"
            for key in ("cover_points", "cover_ranges", "comparison_cost", "covered_codes"):
                point[key] = None

            commands = []

            def fake_graph_viz(command, **_kwargs):
                commands.append(command)
                bundle_path = Path(command[command.index("--bundle") + 1])
                bundle_path.write_text(json.dumps({
                    "dictionary_bits": 8,
                    "dictionary_fingerprint": "onpair-mincut-v1:0123456789abcdef",
                    "graphs": {"mini.contains": [{
                        "needle_index": 0, "states": 1, "svg": "<svg/>", "error": None,
                    }]},
                }), encoding="utf-8")
                return types.SimpleNamespace(returncode=0)

            with mock.patch.object(bench_viz.subprocess, "run", side_effect=fake_graph_viz):
                archive, count = bench_viz.build_mincut_archives(
                    [point], [run], [], None, 128)

        self.assertEqual(count, 1)
        self.assertEqual(len(archive["archives"]), 1)
        self.assertIn("--artifact", commands[0])
        self.assertNotIn("--dataset", commands[0])
        self.assertEqual(
            next(iter(archive["archives"].values()))["verification"],
            "exact_post_run_replay_sidecar+embedded_dictionary_fingerprint",
        )

    def test_mincut_bits_is_a_filter_not_an_override(self):
        twelve = self.mincut_point(config='{"bits":12}')
        sixteen = self.mincut_point(config='{"bits":16}')
        groups = bench_viz.mincut_archive_groups([twelve, sixteen], 16)
        self.assertEqual([group["bits"] for group in groups.values()], [16])
        self.assertIsNone(twelve["mincut_archive_id"])
        with self.assertRaises(ValueError):
            bench_viz.mincut_archive_groups([twelve], 8)

    def test_generated_cover_facts_must_match_the_result(self):
        point = self.mincut_point()
        group = next(iter(bench_viz.mincut_archive_groups([point], None).values()))
        bundle = {"graphs": {"mini.contains": [{
            "cover_points": 2, "cover_ranges": 1, "comparison_cost": 4,
            "covered_codes": 123,
        }]}}
        self.assertEqual(
            bench_viz.validate_mincut_bundle(bundle, group),
            (1, "all_recorded_cover_facts"))
        bundle["graphs"]["mini.contains"][0]["comparison_cost"] = 3
        with self.assertRaisesRegex(ValueError, "comparison_cost"):
            bench_viz.validate_mincut_bundle(bundle, group)

    def test_generated_dictionary_fingerprint_must_match_the_result(self):
        fingerprint = "onpair-mincut-v1:0123456789abcdef"
        point = self.mincut_point(fingerprint=fingerprint)
        group = next(iter(bench_viz.mincut_archive_groups([point], None).values()))
        graph = {
            "cover_points": 2, "cover_ranges": 1, "comparison_cost": 4,
            "covered_codes": 123,
        }
        bundle = {
            "dictionary_fingerprint": fingerprint,
            "graphs": {"mini.contains": [graph]},
        }
        self.assertEqual(
            bench_viz.validate_mincut_bundle(bundle, group),
            (1, "dictionary_fingerprint+recorded_cover_facts"))
        bundle["dictionary_fingerprint"] = "onpair-mincut-v1:ffffffffffffffff"
        with self.assertRaisesRegex(ValueError, "fingerprint mismatch"):
            bench_viz.validate_mincut_bundle(bundle, group)

    def test_explicit_mincut_dataset_requires_a_prepared_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "data.arrow").touch()
            self.assertEqual(
                bench_viz.parse_dataset_specs([f"mini={path}"]), {"mini": path})
            with self.assertRaises(FileNotFoundError):
                bench_viz.parse_dataset_specs([f"missing={path / 'other'}"])

    def test_prepared_dataset_checksum_is_read_from_its_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "manifest.json").write_text(
                json.dumps({"checksum": "xxh3:dataset"}), encoding="utf-8")
            self.assertEqual(
                bench_viz.prepared_dataset_checksum(path), "xxh3:dataset")



class LabelTests(unittest.TestCase):
    def points(self, *specs):
        rows = [
            bench_viz.normalize_query(query_row(candidate="onpair_spiral_neontable",
                                                strategy="pf_memmem", scanner=None,
                                                config='{"bits":16}'), "run"),
            bench_viz.normalize_query(query_row(candidate="onpair_spiral_neontable",
                                                strategy="pf_memmem", scanner=None,
                                                config='{"bits":16,"threshold":0.15}'), "run"),
            bench_viz.normalize_query(query_row(candidate="fsst", strategy="decode",
                                                scanner="memmem-hay"), "run"),
        ]
        bench_viz.apply_labels(rows, list(specs))
        return rows

    def test_display_name_replaces_the_harness_name(self):
        rows = self.points("fsst/decode/memmem-hay=FSST decompress + memmem")
        self.assertEqual(rows[2]["display"], "FSST decompress + memmem")

    def test_one_name_over_several_configs_keeps_them_apart(self):
        # Both neontable configs match; collapsing them to one label would show
        # two identical legend entries for measurably different series.
        rows = self.points("onpair_spiral_neontable=onpair")
        self.assertEqual(rows[0]["display"], "onpair [bits=16]")
        self.assertEqual(rows[1]["display"], "onpair [bits=16, threshold=0.15]")

    def test_a_single_config_needs_no_disambiguation(self):
        rows = self.points("fsst=FSST")
        self.assertEqual(rows[2]["display"], "FSST")

    def test_most_specific_selector_wins(self):
        rows = self.points("fsst=broad", "fsst/decode/memmem-hay=narrow")
        self.assertEqual(rows[2]["display"], "narrow")

    def test_unmatched_rows_keep_the_harness_name(self):
        rows = self.points("fsst=FSST")
        self.assertIsNone(rows[0]["display"])

    def test_malformed_selector_is_rejected(self):
        with self.assertRaises(ValueError):
            bench_viz.parse_label_specs(["fsst"])
        with self.assertRaises(ValueError):
            bench_viz.parse_label_specs(["a/b/c/d=x"])

if __name__ == "__main__":
    unittest.main()


class RunDiscoveryTests(unittest.TestCase):
    """Finding runs on disk and naming them apart."""

    def test_a_tree_of_runs_is_found_to_any_depth(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            for relative in ("a", "group/b", "group/deep/c"):
                target = root / relative
                target.mkdir(parents=True)
                (target / "results.jsonl").write_text("{}\n", encoding="utf-8")
            (root / "not-a-run").mkdir()
            found = bench_viz.resolve_results_paths(root)
            self.assertEqual(
                sorted(path.parent.name for path in found), ["a", "b", "c"])

    def test_a_run_directory_resolves_to_itself_and_is_not_searched(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "results.jsonl").write_text("{}\n", encoding="utf-8")
            nested = root / "sub"
            nested.mkdir()
            (nested / "results.jsonl").write_text("{}\n", encoding="utf-8")
            self.assertEqual(bench_viz.resolve_results_paths(root),
                             [root / "results.jsonl"])

    def test_colliding_run_names_grow_until_they_differ(self):
        paths = [Path("results/figures/like-google/results.jsonl"),
                 Path("results/campaign/like-google/results.jsonl"),
                 Path("results/campaign/needle-sweep/results.jsonl")]
        labels = bench_viz.source_labels(paths)
        self.assertEqual(len(set(labels)), 3, labels)
        self.assertIn("figures/like-google", labels)
        self.assertIn("campaign/like-google", labels)
        # A name that was already unique is left short.
        self.assertIn("needle-sweep", labels)


class MarkupTests(unittest.TestCase):
    """Static checks on the template and stylesheet.

    These exist because of a bug the render tests structurally cannot see: the
    Prefilter section is toggled with the `hidden` attribute, but it also
    carries `display: flex` from its class. An author `display` rule outranks
    the user-agent `[hidden] { display: none }`, so the section stayed on screen
    while its renderer -- correctly seeing `hidden` -- drew nothing into it. The
    result was a visible, empty panel, and neither a syntax check nor a DOM stub
    can notice it.
    """

    TEMPLATE = (ROOT / "template.html").read_text(encoding="utf-8")
    CSS = (ROOT / "app.css").read_text(encoding="utf-8")

    def toggled_selectors(self):
        """CSS selectors for the elements the tab switch shows and hides.

        A panel is addressed by class or by id, and both are equally able to
        carry a `display` rule that defeats `hidden`, so both are collected.
        """
        found = set()
        for element in re.findall(r"<\w+[^>]*data-panel=[^>]*>", self.TEMPLATE):
            for group in re.findall(r'class="([^"]*)"', element):
                found.update(f".{name}" for name in group.split())
            for name in re.findall(r'id="([^"]*)"', element):
                found.add(f"#{name}")
        return found

    def declares_display(self, selector):
        pattern = rf"{re.escape(selector)}\s*(?:,[^{{]*)?{{[^}}]*\bdisplay\s*:"
        return re.search(pattern, self.CSS) is not None

    def hides_when_hidden(self, selector):
        if re.search(r"(?<![.\w#-])\[hidden\]\s*{[^}]*display\s*:\s*none", self.CSS):
            return True  # a global rule covers every element
        return re.search(rf"{re.escape(selector)}\[hidden\]", self.CSS) is not None

    def test_there_is_something_to_toggle(self):
        self.assertTrue(self.toggled_selectors(),
                        "no [data-panel] elements found; the tab switch has nothing to show")

    def test_a_toggled_panel_that_sets_display_also_overrides_hidden(self):
        for selector in sorted(self.toggled_selectors()):
            if not self.declares_display(selector):
                continue
            self.assertTrue(
                self.hides_when_hidden(selector),
                f"{selector} sets `display`, which outranks the user-agent "
                f"`[hidden]` rule, so it needs `{selector}[hidden] {{ display: none }}` "
                f"or the tab switch leaves it on screen and empty")

    def test_every_element_the_main_panel_reaches_for_exists(self):
        # app.js resolves its whole control set by id up front, and a missing one
        # is an undefined that only fails when that control is first touched.
        source = (ROOT / "app.js").read_text(encoding="utf-8")
        block = re.search(r"const refs = Object\.fromEntries\(\[(.*?)\]\.map",
                          source, re.S)
        self.assertIsNotNone(block, "could not find the refs list in app.js")
        wanted = set(re.findall(r'"([a-z0-9-]+)"', block.group(1)))
        present = set(re.findall(r'id="([a-z0-9-]+)"', self.TEMPLATE))
        self.assertTrue(wanted, "no ids found in the refs list")
        self.assertEqual(wanted - present, set(),
                         "app.js reads element ids the template does not define")

    # Void and self-closing elements never carry a closing tag.
    VOID = frozenset({
        "meta", "br", "hr", "img", "input", "link", "source", "path", "rect",
        "circle", "use", "area", "base", "col", "embed", "param", "track", "wbr",
    })

    def test_the_template_nests_correctly(self):
        """Every tag closes the element it actually opened.

        Counting opens against closes is not enough: moving a panel by slicing
        from its heading rather than its `<article>` leaves the opening behind
        and takes the closing along, and the totals still match. Only walking
        the tags in order catches it.
        """
        stack = []
        problems = []
        pattern = re.compile(r"<(/?)([a-zA-Z][\w-]*)([^>]*?)(/?)>")
        for match in pattern.finditer(self.TEMPLATE):
            closing, name, self_closing = match.group(1), match.group(2).lower(), match.group(4)
            if name in self.VOID or self_closing:
                continue
            line = self.TEMPLATE.count("\n", 0, match.start()) + 1
            if not closing:
                stack.append((name, line))
            elif not stack:
                problems.append(f"line {line}: </{name}> with nothing open")
            elif stack[-1][0] != name:
                problems.append(
                    f"line {line}: </{name}> closes <{stack[-1][0]}> from line {stack[-1][1]}")
                stack.pop()
            else:
                stack.pop()
        problems += [f"<{name}> on line {line} never closed" for name, line in stack]
        self.assertEqual(problems, [], "\n".join(problems))

    def test_every_element_the_prefilter_panel_reaches_for_exists(self):
        # prefilter.js resolves these by id. A typo on either side is silent:
        # the panel renders nothing and reports no error.
        wanted = set(re.findall(r'el\("([a-z0-9-]+)"\)',
                                (ROOT / "prefilter.js").read_text(encoding="utf-8")))
        present = set(re.findall(r'id="([a-z0-9-]+)"', self.TEMPLATE))
        self.assertTrue(wanted, "no element lookups found in prefilter.js")
        self.assertEqual(wanted - present, set(),
                         "prefilter.js reads element ids the template does not define")

    def test_the_template_has_a_slot_for_every_injected_asset(self):
        for marker in ("__BENCH_VIZ_CSS__", "__BENCH_VIZ_DATA__", "__BENCH_VIZ_DEFAULTS__",
                       "__BENCH_VIZ_ANALYSIS__", "__BENCH_VIZ_MINCUT_GRAPHS__",
                       "__BENCH_VIZ_JS__",
                       "__BENCH_VIZ_PREFILTER_JS__"):
            self.assertIn(marker, self.TEMPLATE)
