// Unit tests for the pure reductions in prefilter.js.
//
// Run with the JavaScriptCore shell that ships with macOS:
//
//   /System/Library/Frameworks/JavaScriptCore.framework/Versions/A/Helpers/jsc \
//     tools/bench-viz/tests/prefilter.test.js
//
// prefilter.js returns early when there is no `document`, so loading it here
// gets the reductions and none of the rendering.

load("tools/bench-viz/prefilter.js");
const pf = globalThis.BenchVizPrefilter;

let failures = 0;
let checks = 0;

function check(name, condition, detail) {
  checks += 1;
  if (!condition) {
    failures += 1;
    print(`FAIL ${name}${detail === undefined ? "" : ` — ${detail}`}`);
  }
}

function near(name, actual, expected, tolerance) {
  const slack = tolerance === undefined ? 1e-9 : tolerance;
  check(name, Math.abs(actual - expected) <= slack, `got ${actual}, want ${expected}`);
}

function row(overrides) {
  return Object.assign({
    query_id: "q",
    latency_ns: 100,
    comparison_cost: 4,
    admitted_rows: 0.01,
    predicted_ns: 100,
    gbps: 10,
  }, overrides || {});
}

function baseline(entries) {
  return new Map(entries);
}

// ------------------------------------------------------------------- pairing

(() => {
  const rows = [row({query_id: "a", latency_ns: 50}), row({query_id: "b", latency_ns: 200})];
  const pairs = pf.pairedRatios(rows, baseline([["a", 100], ["b", 100]]));
  check("pairs both queries", pairs.length === 2);
  near("faster query reports a speedup above one", pairs[0].ratio, 2);
  near("slower query reports a ratio below one", pairs[1].ratio, 0.5);
})();

(() => {
  // A query the baseline never ran cannot be compared, and must be dropped
  // rather than silently compared against something else.
  const rows = [row({query_id: "a"}), row({query_id: "missing"})];
  const pairs = pf.pairedRatios(rows, baseline([["a", 100]]));
  check("a query absent from the baseline is skipped", pairs.length === 1);
  check("and it is the one that was present", pairs[0].row.query_id === "a");
})();

(() => {
  const rows = [row({query_id: "a", latency_ns: 0}), row({query_id: "b", latency_ns: NaN})];
  const pairs = pf.pairedRatios(rows, baseline([["a", 100], ["b", 100]]));
  check("zero and non-finite times are skipped, not divided by", pairs.length === 0);
})();

// --------------------------------------------------------------------- stats

(() => {
  const rows = [1, 2, 4, 8, 0.5].map((factor, i) =>
    row({query_id: `q${i}`, latency_ns: 100 / factor}));
  const times = rows.map(r => ["" + r.query_id, 100]);
  const stats = pf.ratioStats(pf.pairedRatios(rows, baseline(times)));
  check("counts every pair", stats.n === 5);
  check("counts only strict wins", stats.wins === 3, `got ${stats.wins}`);
  near("win rate is wins over pairs", stats.winRate, 3 / 5);
  near("median ratio", stats.median, 2);
  near("worst ratio is the minimum", stats.min, 0.5);
  near("best ratio is the maximum", stats.max, 8);
  // Total time weighs each query by its own duration, which the median cannot.
  near("total baseline time", stats.totalBase, 500);
  near("total self time", stats.totalSelf, 100 + 50 + 25 + 12.5 + 200);
})();

check("no pairs yields no statistics", pf.ratioStats([]) === null);

// ---------------------------------------------------------------- gate rules

(() => {
  const gate = {maxCost: 16, maxAdmitted: 0.1};
  check("a narrow cover admitting few rows is admitted",
    pf.gateAdmits(row({comparison_cost: 4, admitted_rows: 0.01}), gate) === true);
  check("the cost clause is inclusive at the boundary",
    pf.gateAdmits(row({comparison_cost: 16, admitted_rows: 0.01}), gate) === true);
  check("one comparison wider is rejected",
    pf.gateAdmits(row({comparison_cost: 17, admitted_rows: 0.0}), gate) === false);
  check("the verification clause is exclusive at the boundary",
    pf.gateAdmits(row({comparison_cost: 1, admitted_rows: 0.1}), gate) === false);
  check("just under the budget is admitted",
    pf.gateAdmits(row({comparison_cost: 1, admitted_rows: 0.0999}), gate) === true);
  check("a row with no cover facts is undecidable, not a default",
    pf.gateAdmits(row({comparison_cost: null}), gate) === null);
  check("the two clauses are independent",
    pf.gateAdmits(row({comparison_cost: 32, admitted_rows: 0.0}), gate) === false &&
    pf.gateAdmits(row({comparison_cost: 1, admitted_rows: 0.9}), gate) === false);
})();

(() => {
  const gate = {
    useModel: true, margin: 1.0,
    baselineFor: () => 100,
  };
  check("the model gate admits a prediction under the baseline",
    pf.gateAdmits(row({predicted_ns: 40}), gate) === true);
  check("and rejects one over it",
    pf.gateAdmits(row({predicted_ns: 140}), gate) === false);
  check("a row with no prediction is undecidable",
    pf.gateAdmits(row({predicted_ns: null}), gate) === null);
})();

// ------------------------------------------------------------ gate scoring

(() => {
  // Four queries: two the gate should take and does, one it should take and
  // does not, one it takes and should not.
  const rows = [
    row({query_id: "win-taken", latency_ns: 10, comparison_cost: 2, admitted_rows: 0.01}),
    row({query_id: "win-taken-2", latency_ns: 20, comparison_cost: 3, admitted_rows: 0.01}),
    row({query_id: "win-missed", latency_ns: 50, comparison_cost: 40, admitted_rows: 0.01}),
    row({query_id: "loss-taken", latency_ns: 400, comparison_cost: 2, admitted_rows: 0.01}),
  ];
  const times = baseline(rows.map(r => [r.query_id, 100]));
  const outcome = pf.gateOutcome(pf.pairedRatios(rows, times), {maxCost: 16, maxAdmitted: 0.1});
  check("selected count", outcome.selected === 3, `got ${outcome.selected}`);
  check("rejected count", outcome.rejected === 1, `got ${outcome.rejected}`);
  check("false positive is the query it took and lost", outcome.falsePositive === 1);
  check("false negative is the win it declined", outcome.falseNegative === 1);
  // policy = 10 + 20 + 400 (taken) + 100 (declined, so baseline)
  near("policy time sums the choices actually made", outcome.policyNs, 530);
  // oracle takes the best of each: 10 + 20 + 50 + 100
  near("oracle time takes the best of each", outcome.oracleNs, 180);
  near("regret is policy over oracle", outcome.regret, 530 / 180);
  near("always-on time", outcome.alwaysNs, 480);
  near("never-on time", outcome.neverNs, 400);
  near("worst regression is the largest single loss", outcome.worstRegression, 4);
  near("best missed win", outcome.missedBest, 2);
})();

(() => {
  // Undecidable queries must not be counted as either kind of error, and must
  // be charged at the baseline since the policy cannot run them.
  const rows = [row({query_id: "a", comparison_cost: null, latency_ns: 10})];
  const outcome = pf.gateOutcome(pf.pairedRatios(rows, baseline([["a", 100]])),
    {maxCost: 16, maxAdmitted: 0.1});
  check("undecided is its own count", outcome.undecided === 1);
  check("and is neither a false positive nor a false negative",
    outcome.falsePositive === 0 && outcome.falseNegative === 0);
  near("charged at the baseline", outcome.policyNs, 100);
})();

(() => {
  // A policy that never regresses reports a worst regression of exactly one,
  // so the column reads as "none" rather than as a suspiciously small number.
  const rows = [row({query_id: "a", latency_ns: 10, comparison_cost: 2, admitted_rows: 0.0})];
  const outcome = pf.gateOutcome(pf.pairedRatios(rows, baseline([["a", 100]])),
    {maxCost: 16, maxAdmitted: 0.1});
  check("no regression reports one", outcome.worstRegression === 1);
  near("a perfect policy has regret one", outcome.regret, 1);
})();

// ---------------------------------------------------------------- the sweep

(() => {
  const rows = [
    row({query_id: "cheap", latency_ns: 10, comparison_cost: 2, admitted_rows: 0.005}),
    row({query_id: "heavy", latency_ns: 400, comparison_cost: 2, admitted_rows: 0.05}),
  ];
  const pairs = pf.pairedRatios(rows, baseline(rows.map(r => [r.query_id, 100])));
  const sweep = pf.sweepAdmitted(pairs, 16, [0.001, 0.01, 0.1]);
  check("one row per threshold", sweep.length === 3);
  check("the tightest budget takes nothing", sweep[0].selected === 0);
  check("the middle budget takes only the cheap query", sweep[1].selected === 1);
  check("the loosest budget takes both", sweep[2].selected === 2);
  check("and pays for it with a false positive", sweep[2].falsePositive === 1);
  check("regret is monotone here", sweep[1].regret < sweep[0].regret &&
    sweep[1].regret < sweep[2].regret,
    `${sweep[0].regret} ${sweep[1].regret} ${sweep[2].regret}`);
})();

// ------------------------------------------------------------------ R-square

(() => {
  const xs = [1, 2, 3, 4, 5, 6];
  const exact = xs.map(x => 3 * x + 1);
  near("a perfect linear fit explains everything", pf.rsquared([xs], exact), 1, 1e-9);
  const flat = xs.map(() => 7);
  check("a constant target has no variance to explain",
    pf.rsquared([xs], flat) === null);
  const noisy = [4, 7, 9, 13, 16, 19];
  const score = pf.rsquared([xs], noisy);
  check("a good but imperfect fit lands below one", score > 0.98 && score < 1, `got ${score}`);
  // A second predictor that carries independent information must help.
  const second = [0, 1, 0, 1, 0, 1];
  const combined = xs.map((x, i) => 3 * x + 5 * second[i]);
  near("two predictors recover an exact two-term relation",
    pf.rsquared([xs, second], combined), 1, 1e-9);
  check("too few observations is reported, not extrapolated",
    pf.rsquared([[1, 2]], [1, 2]) === null);
})();

check("a singular system solves to nothing",
  pf.solve([[1, 2], [2, 4]], [1, 2]) === null);

(() => {
  const solved = pf.solve([[2, 1], [1, 3]], [5, 10]);
  near("solves a small system, first", solved[0], 1);
  near("solves a small system, second", solved[1], 3);
})();

// ------------------------------------------------------------------ binning

(() => {
  const rows = [1, 3, 10, 30, 100, 300].map((value, i) =>
    row({query_id: `q${i}`, comparison_cost: value}));
  const bins = pf.binBy(rows, "comparison_cost", 3);
  const total = bins.reduce((sum, bin) => sum + bin.rows.length, 0);
  check("every row lands in exactly one bin", total === rows.length, `got ${total}`);
  check("bins come back in ascending order",
    bins.every((bin, i) => i === 0 || bin.lo >= bins[i - 1].lo));
  check("the largest value is inside the last bin, not past it",
    bins[bins.length - 1].rows.some(r => r.comparison_cost === 300));
})();

(() => {
  // Zero and negative values have no place on a log axis; they are dropped
  // rather than silently floored into the first bin.
  const rows = [row({query_id: "a", comparison_cost: 0}),
                row({query_id: "b", comparison_cost: 4})];
  const bins = pf.binBy(rows, "comparison_cost", 4);
  const total = bins.reduce((sum, bin) => sum + bin.rows.length, 0);
  check("non-positive values are excluded from log bins", total === 1, `got ${total}`);
})();

check("binning an empty set yields no bins", pf.binBy([], "comparison_cost", 4).length === 0);

// ---------------------------------------------------------------- quantiles

(() => {
  const ordered = [1, 2, 3, 4];
  near("median interpolates between the middle pair", pf.quantileOf(ordered, 0.5), 2.5);
  near("the zeroth quantile is the minimum", pf.quantileOf(ordered, 0), 1);
  near("the first quantile is the maximum", pf.quantileOf(ordered, 1), 4);
  check("an empty set has no quantile", pf.quantileOf([], 0.5) === null);
})();

// ------------------------------------------- rank statistics and discrimination

// Ties must share a rank, or cover width -- which is nothing but ties -- gets a
// correlation driven by input order.
{
  const ranks = pf.ranksOf([10, 20, 20, 30]);
  near("tied values share the average rank", ranks[1], 2.5, 1e-12);
  near("and the tie does not shift what follows", ranks[3], 4, 1e-12);
}

// A textbook Spearman: monotone but bent, so Pearson on the raw values is
// below 1 while the rank correlation is exactly 1.
{
  const xs = [1, 2, 3, 4, 5];
  const ys = [1, 4, 9, 16, 25];
  near("spearman is 1 on any increasing map", pf.spearman(xs, ys), 1, 1e-12);
  near("pearson is not", pf.pearson(xs, ys), 0.9811, 5e-4);
  near("spearman is -1 when reversed", pf.spearman(xs, [...ys].reverse()), -1, 1e-12);
}

// AUC against a hand-checkable case: perfect separation, then a coin toss.
{
  const perfect = pf.auc([1, 2, 3, 4], [false, false, true, true]);
  near("perfect separation scores 1", perfect.value, 1, 1e-12);
  near("and reports the direction", perfect.direction, 1, 1e-12);
  const flipped = pf.auc([4, 3, 2, 1], [false, false, true, true]);
  near("a reversed variable is just as informative", flipped.value, 1, 1e-12);
  near("with the opposite direction", flipped.direction, -1, 1e-12);
  // Symmetric classes: one win either side of each loss, so no cut helps.
  const coin = pf.auc([1, 2, 3, 4], [true, false, false, true]);
  near("symmetric classes score a coin toss", coin.value, 0.5, 1e-12);
  // The reported value is max(auc, 1 - auc): a variable that separates the
  // classes backwards is just as useful once the cut is turned around, and
  // reporting 0.25 as "worse than a coin" would be wrong.
  const backwards = pf.auc([1, 2, 3, 4], [true, false, true, false]);
  near("a variable informative in reverse reports its strength", backwards.value, 0.75, 1e-12);
  near("with the direction that says so", backwards.direction, -1, 1e-12);
  const none = pf.auc([1, 2], [true, true]);
  check("one class alone has no AUC", none === null);
}

// The threshold search must find the cut that minimises *time*, not the one
// that classifies the most queries correctly.
{
  //         value  prefilter  baseline     one slow query dominates the clock
  const rows = [
    {value: 1, mine: 10, base: 100},
    {value: 2, mine: 10, base: 100},
    {value: 9, mine: 5000, base: 100},
  ];
  const pairs = rows.map(r => ({row: {v: r.value, latency_ns: r.mine}, base: r.base}));
  const found = pf.bestThreshold(pairs, row => row.v, -1, [1.5, 2.5, 9.5]);
  near("it cuts below the expensive query", found.cut, 2.5, 1e-12);
  near("keeping only the two wins", found.total, 10 + 10 + 100, 1e-9);
  near("and reports no regression", found.regressions, 0, 1e-12);
  near("regret against the oracle", found.regret, 120 / (10 + 10 + 100), 1e-9);
}

print(failures === 0
  ? `prefilter.js: ${checks} checks passed`
  : `prefilter.js: ${failures} of ${checks} checks FAILED`);
if (failures) throw new Error(`${failures} failing checks`);
