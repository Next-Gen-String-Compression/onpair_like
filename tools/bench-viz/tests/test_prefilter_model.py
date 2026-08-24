import importlib.util
import math
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("prefilter_model", ROOT / "prefilter_model.py")
prefilter_model = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prefilter_model)


def build(**overrides):
    row = {
        "source": "run",
        "candidate": "onpair",
        "config": '{"bits":16}',
        "dataset": "mini",
        "chunk_rows": 0,
        "num_rows": 1000,
        "payload_bytes": 80_000,
        "footprint_components": {"codes": 20_000},  # u16 -> 10,000 code positions
    }
    row.update(overrides)
    return row


def query(**overrides):
    row = {
        "source": "run",
        "candidate": "onpair",
        "config": '{"bits":16}',
        "config_hash": "aaaa",
        "strategy": "pf_memmem",
        "scanner": None,
        "dataset": "mini",
        "chunk_rows": 0,
        "op": "contains",
        "query_id": "q1",
        "comparison_cost": 4.0,
        "covered_fraction": 0.001,
        "covered_codes": 10.0,
        "indexed_codes": 10_000.0,
        "candidate_row_fraction": 0.01,
        "latency_ns": 1_000_000.0,
        "gbps": 80.0,
        "ns_per_row": 1000.0,
        "selectivity": 0.001,
        "needle_len": 8.0,
    }
    row.update(overrides)
    return row


class SolverTests(unittest.TestCase):
    def test_solves_a_small_system(self):
        matrix = [[2.0, 1.0], [1.0, 3.0]]
        solved = prefilter_model.solve_symmetric(matrix, [5.0, 10.0])
        self.assertAlmostEqual(solved[0], 1.0)
        self.assertAlmostEqual(solved[1], 3.0)

    def test_a_term_with_no_observations_reports_zero(self):
        # A series whose covers never leave the specialized kernels has an
        # all-zero wide-cover column. That is not a failure, it is an absence.
        design = [[1.0, 0.0], [2.0, 0.0]]
        beta = prefilter_model.least_squares(design, [3.0, 6.0], [1.0, 1.0])
        self.assertAlmostEqual(beta[0], 3.0, places=6)
        self.assertEqual(beta[1], 0.0)

    def test_a_singular_system_is_reported_not_guessed(self):
        self.assertIsNone(prefilter_model.solve_symmetric([[1.0, 2.0], [2.0, 4.0]], [1.0, 2.0]))

    def test_least_squares_recovers_exact_coefficients(self):
        # y = 3*a + 0.5*b, sampled at four well-separated points.
        design = [[1.0, 0.0], [0.0, 1.0], [2.0, 1.0], [1.0, 4.0]]
        target = [3.0, 0.5, 6.5, 5.0]
        beta = prefilter_model.least_squares(design, target, [1.0] * 4)
        self.assertAlmostEqual(beta[0], 3.0, places=6)
        self.assertAlmostEqual(beta[1], 0.5, places=6)

    def test_a_negative_coefficient_is_clamped_to_zero(self):
        # y = -2*a: a cost cannot be negative, so zero is the honest answer.
        beta = prefilter_model.least_squares([[1.0], [2.0]], [-2.0, -4.0], [1.0, 1.0])
        self.assertEqual(beta, [0.0])


class ColumnFactTests(unittest.TestCase):
    def test_build_rows_supply_the_column_shape(self):
        facts = prefilter_model.column_facts([build()], [query()])
        entry = facts[("run", "mini", 0)]
        self.assertEqual(entry["num_rows"], 1000)
        self.assertEqual(entry["payload_bytes"], 80_000)
        self.assertEqual(entry["build_codes"], 10_000)

    def test_shape_is_recovered_when_the_run_predates_the_build_fields(self):
        # gbps_raw is payload bytes per nanosecond, and ns_per_row is the median
        # over the row count, so both are exactly recoverable from a query row.
        older = build()
        older.pop("num_rows")
        older.pop("payload_bytes")
        facts = prefilter_model.column_facts([older], [query()])
        entry = facts[("run", "mini", 0)]
        self.assertAlmostEqual(entry["payload_bytes"], 80.0 * 1_000_000.0)
        self.assertAlmostEqual(entry["num_rows"], 1000.0)

    def test_admitted_rows_prefers_the_reported_value(self):
        points = [query(candidate_row_fraction=0.25)]
        prefilter_model.annotate_shape(points, prefilter_model.column_facts([build()], points))
        self.assertEqual(points[0]["admitted_rows"], 0.25)

    def test_admitted_rows_is_derived_from_covered_codes_when_absent(self):
        points = [query(candidate_row_fraction=None, covered_codes=50.0)]
        prefilter_model.annotate_shape(points, prefilter_model.column_facts([build()], points))
        self.assertAlmostEqual(points[0]["admitted_rows"], 0.05)

    def test_admitted_rows_falls_back_to_coverage_times_codes_per_row(self):
        points = [query(candidate_row_fraction=None, covered_codes=None,
                        covered_fraction=0.002)]
        prefilter_model.annotate_shape(points, prefilter_model.column_facts([build()], points))
        # 10 codes per row on this fixture.
        self.assertAlmostEqual(points[0]["admitted_rows"], 0.02)

    def test_admitted_rows_never_exceeds_the_whole_column(self):
        points = [query(candidate_row_fraction=None, covered_codes=9_000.0)]
        prefilter_model.annotate_shape(points, prefilter_model.column_facts([build()], points))
        self.assertEqual(points[0]["admitted_rows"], 1.0)

    def test_shape_reports_the_ratios_the_cost_model_needs(self):
        points = [query()]
        prefilter_model.annotate_shape(points, prefilter_model.column_facts([build()], points))
        point = points[0]
        self.assertAlmostEqual(point["codes_per_row"], 10.0)
        self.assertAlmostEqual(point["bytes_per_code"], 8.0)
        # kappa is payload per byte of code stream, and codes are two bytes.
        self.assertAlmostEqual(point["kappa"], 4.0)

    def test_the_arm_boundary_is_the_specialized_kernel_ceiling(self):
        points = [query(comparison_cost=16.0), query(comparison_cost=17.0),
                  query(comparison_cost=None)]
        prefilter_model.annotate_shape(points, prefilter_model.column_facts([build()], points))
        self.assertEqual([p["arm"] for p in points], ["specialized", "wide", None])


class FitTests(unittest.TestCase):
    #: Two columns of different row length, so the per-row and per-byte verify
    #: costs are separable. On one column they are exact multiples and no fit
    #: can tell them apart -- see `test_one_column_cannot_separate_the_verify_terms`.
    COLUMNS = (
        {"dataset": "short", "rows": 1000.0, "payload": 80_000.0, "codes": 10_000.0},
        {"dataset": "long", "rows": 500.0, "payload": 200_000.0, "codes": 12_000.0},
    )

    def builds(self):
        return [build(dataset=column["dataset"], num_rows=column["rows"],
                      payload_bytes=column["payload"],
                      footprint_components={"codes": column["codes"] * 2})
                for column in self.COLUMNS]

    def synthetic(self, sigma0, sigma1, v_row, v_byte, n=60, columns=None, **extra):
        """Queries whose latency is exactly the model, so the fit must recover it."""
        columns = columns if columns is not None else self.COLUMNS
        points = []
        for i in range(n):
            column = columns[i % len(columns)]
            cost = 1 + (i % 8)
            admitted = 0.001 * (1 + i % 5)
            codes, rows, payload = column["codes"], column["rows"], column["payload"]
            latency = codes * (sigma0 + sigma1 * cost) + admitted * (rows * v_row + payload * v_byte)
            points.append(query(
                query_id=f"q{i}", dataset=column["dataset"], indexed_codes=codes,
                comparison_cost=float(cost), candidate_row_fraction=admitted,
                latency_ns=latency, gbps=payload / latency, ns_per_row=latency / rows,
                **extra,
            ))
        return points

    def fit(self, points):
        prefilter_model.annotate_shape(
            points, prefilter_model.column_facts(self.builds(), points))
        return prefilter_model.fit_cost_model(points)

    def fits(self, points):
        """Only the real fits, dropping the cross-run aliases."""
        return {key: value for key, value in self.fit(points).items()
                if "borrowed_from" not in value}

    def test_the_fit_recovers_the_constants_it_was_generated_from(self):
        models = self.fits(self.synthetic(0.0166, 0.0174, 35.7, 0.104))
        self.assertEqual(len(models), 1)
        constants = next(iter(models.values()))["constants"]
        self.assertAlmostEqual(constants["sigma0"], 0.0166, places=5)
        self.assertAlmostEqual(constants["sigma1"], 0.0174, places=5)
        self.assertAlmostEqual(constants["v_row"], 35.7, places=3)
        self.assertAlmostEqual(constants["v_byte"], 0.104, places=5)

    def test_one_column_cannot_separate_the_verify_terms(self):
        # Only one column, so `admitted * rows` and `admitted * bytes` are exact
        # multiples. The fit drops the per-byte term and lets the per-row term
        # absorb it, which keeps the prediction exact.
        column = self.COLUMNS[0]
        points = self.synthetic(0.02, 0.02, 30.0, 0.1, columns=(column,))
        model = next(iter(self.fits(points).values()))
        constants = model["constants"]
        self.assertEqual(constants["v_byte"], 0.0)
        self.assertAlmostEqual(
            constants["v_row"], 30.0 + 0.1 * column["payload"] / column["rows"], places=4)
        self.assertLess(model["median_abs_log_error"], 1e-9)

    def test_a_perfect_fit_reports_no_error(self):
        model = next(iter(self.fits(self.synthetic(0.02, 0.02, 30.0, 0.1)).values()))
        self.assertLess(model["median_abs_log_error"], 1e-6)
        # 2 bytes per code position at 0.02 ns is 100 GB/s of code stream.
        self.assertAlmostEqual(model["code_stream_gbps"], 100.0, places=3)

    def test_a_series_with_no_wide_covers_reports_no_wide_constant(self):
        model = next(iter(self.fits(self.synthetic(0.02, 0.02, 30.0, 0.1)).values()))
        self.assertEqual(model["constants"]["gamma1"], 0.0)
        self.assertEqual(model["constants"]["gamma0"], 0.0)

    def test_a_series_with_too_few_queries_is_not_fitted(self):
        points = self.synthetic(0.02, 0.02, 30.0, 0.1, n=prefilter_model.MIN_FIT_POINTS - 1)
        self.assertEqual(self.fits(points), {})

    def test_series_are_fitted_separately(self):
        # Two candidates whose scan cost differs by 4x. Pooling them would give
        # one slope belonging to neither.
        first = self.synthetic(0.01, 0.01, 30.0, 0.1)
        second = [dict(p, candidate="other") for p in self.synthetic(0.04, 0.04, 30.0, 0.1)]
        models = self.fits(first + second)
        self.assertEqual(len(models), 2)
        slopes = sorted(m["constants"]["sigma1"] for m in models.values())
        self.assertAlmostEqual(slopes[0], 0.01, places=5)
        self.assertAlmostEqual(slopes[1], 0.04, places=5)

    def test_a_wide_cover_gets_its_own_step_and_slope(self):
        # A planner that escapes wide covers to a membership table pays a roughly
        # flat cost per code. Without an intercept in the wide regime that shape
        # is unrepresentable and the slope absorbs it wrongly.
        points = self.synthetic(0.02, 0.02, 30.0, 0.1)
        flat_per_code = 0.5
        for i, column in enumerate(self.COLUMNS * 12):
            latency = column["codes"] * flat_per_code
            points.append(query(
                query_id=f"wide{i}", dataset=column["dataset"],
                indexed_codes=column["codes"], comparison_cost=float(100 + i),
                candidate_row_fraction=0.0, latency_ns=latency,
                gbps=column["payload"] / latency, ns_per_row=latency / column["rows"],
            ))
        constants = next(iter(self.fits(points).values()))["constants"]
        self.assertAlmostEqual(constants["sigma0"] + constants["gamma0"], flat_per_code, places=4)
        self.assertAlmostEqual(constants["gamma1"], 0.0, places=6)

    def test_a_run_too_small_to_fit_borrows_the_sibling_run(self):
        # A single-predicate run holds one query and can never be fitted, but it
        # is the same code on the same machine as the sweep beside it.
        sweep = self.synthetic(0.02, 0.02, 30.0, 0.1)
        for point in sweep:
            point["source"] = "sweep"
        lone = query(source="one-predicate", query_id="google",
                     dataset=self.COLUMNS[0]["dataset"],
                     indexed_codes=self.COLUMNS[0]["codes"])
        points = sweep + [lone]
        models = self.fit(points)
        prefilter_model.annotate_predictions(points, models)
        self.assertIsNotNone(lone["predicted_ns"])
        alias = models[prefilter_model.KEY_SEP.join(
            ("", "onpair", '{"bits":16}', "pf_memmem", ""))]
        self.assertEqual(alias["borrowed_from"], "sweep")

    def test_predictions_split_scan_from_verify(self):
        points = self.synthetic(0.02, 0.02, 30.0, 0.1)
        models = self.fit(points)
        prefilter_model.annotate_predictions(points, models)
        point = points[0]
        self.assertAlmostEqual(point["predicted_ns"], point["latency_ns"], places=3)
        self.assertAlmostEqual(
            point["predicted_scan_ns"] + point["predicted_verify_ns"],
            point["predicted_ns"], places=6)
        self.assertGreater(point["predicted_scan_ns"], 0.0)

    def test_a_point_without_cover_facts_gets_no_prediction(self):
        models = self.fit(self.synthetic(0.02, 0.02, 30.0, 0.1))
        bare = query(query_id="bare", comparison_cost=None)
        prefilter_model.annotate_shape([bare], prefilter_model.column_facts([build()], [bare]))
        prefilter_model.annotate_predictions([bare], models)
        self.assertIsNone(bare["predicted_ns"])


class NoiseFloorTests(unittest.TestCase):
    #: Two configs of one candidate that produced the same column. The harness
    #: hashes the config *string*, so these do not share a config_hash even
    #: though they parse to the same settings; the footprint is what betrays it.
    TWINS = ('{"bits":16}', '{"bits":16,"threshold":0.15}')

    def twin_builds(self):
        return [build(config=config) for config in self.TWINS]

    def test_configs_that_built_the_same_column_are_recognised(self):
        found = prefilter_model.twin_configs(self.twin_builds())
        self.assertEqual(list(found.values()), [sorted(self.TWINS)])

    def test_a_different_column_is_not_a_twin(self):
        builds = [build(config='{"bits":16}'),
                  build(config='{"bits":12}', footprint_components={"codes": 999})]
        self.assertEqual(prefilter_model.twin_configs(builds), {})

    def test_the_same_config_twice_is_not_a_twin(self):
        # One config string cannot be two worker processes.
        self.assertEqual(
            prefilter_model.twin_configs([build(), build()]), {})

    def test_the_spread_between_twins_is_the_floor(self):
        points = [query(config=self.TWINS[0], latency_ns=100.0),
                  query(config=self.TWINS[1], latency_ns=105.0)]
        noise = prefilter_model.noise_floor(points, self.twin_builds())
        self.assertEqual(noise["pairs"], 1)
        self.assertAlmostEqual(noise["median"], 0.05)
        self.assertEqual(noise["matched"][0]["configs"], sorted(self.TWINS))

    def test_no_twin_configs_means_no_floor(self):
        points = [query(latency_ns=100.0), query(latency_ns=140.0)]
        self.assertIsNone(prefilter_model.noise_floor(points, [build()]))

    def test_pairs_are_grouped_per_query_not_across_the_suite(self):
        points = [
            query(query_id="a", config=self.TWINS[0], latency_ns=100.0),
            query(query_id="a", config=self.TWINS[1], latency_ns=110.0),
            query(query_id="b", config=self.TWINS[0], latency_ns=200.0),
            query(query_id="b", config=self.TWINS[1], latency_ns=204.0),
        ]
        noise = prefilter_model.noise_floor(points, self.twin_builds())
        self.assertEqual(noise["pairs"], 2)
        self.assertAlmostEqual(noise["max"], 0.10)

    def test_a_third_config_that_built_a_different_column_is_excluded(self):
        builds = self.twin_builds() + [
            build(config='{"bits":12}', footprint_components={"codes": 999})]
        points = [
            query(config=self.TWINS[0], latency_ns=100.0),
            query(config=self.TWINS[1], latency_ns=101.0),
            query(config='{"bits":12}', latency_ns=500.0),
        ]
        noise = prefilter_model.noise_floor(points, builds)
        self.assertEqual(noise["pairs"], 1)
        self.assertAlmostEqual(noise["median"], 0.01)


class AnalyseTests(unittest.TestCase):
    def test_analyse_returns_everything_the_viewer_needs(self):
        points = [query(query_id=f"q{i}", comparison_cost=float(1 + i % 8),
                        candidate_row_fraction=0.001 * (1 + i % 4)) for i in range(40)]
        result = prefilter_model.analyse(points, [build()])
        key = prefilter_model.KEY_SEP.join(("run", "mini", "0"))
        self.assertIn(key, result["columns"])
        self.assertEqual(result["max_simd_comparisons"], prefilter_model.MAX_SIMD_COMPARISONS)
        self.assertEqual(result["max_candidate_row_fraction"],
                         prefilter_model.MAX_CANDIDATE_ROW_FRACTION)
        # Shape annotation happened in place, before the fit read it.
        self.assertEqual(points[0]["codes_per_row"], 10.0)
        self.assertIn("arm", points[0])

    def test_analyse_survives_a_run_with_no_cover_facts_at_all(self):
        points = [query(query_id=f"q{i}", comparison_cost=None, covered_fraction=None,
                        covered_codes=None, candidate_row_fraction=None) for i in range(5)]
        result = prefilter_model.analyse(points, [build()])
        self.assertEqual(result["models"], {})
        self.assertTrue(all(p["predicted_ns"] is None for p in points))


if __name__ == "__main__":
    unittest.main()
