// Cross-check: do the viewer's reductions reproduce the figures established
// independently in Python by
// experiments/optimize_prefilter/analysis/predict_profitability.py?
//
// Opt-in, not part of the default suite: it needs a `needle-sweep` run carrying
// ABI v6 cover facts and a decode baseline, which is a build product. Re-running
// the Python side additionally needs numpy. Without those the pinned figures
// below still serve as a regression pin on the JavaScript reductions.
//
//   python3 tools/bench-viz/tests/extract_payload.py <explorer.html> payload.json
//   jsc tools/bench-viz/tests/figures.test.js -- payload.json
//
// Two implementations of the same statistics, written against the same data in
// different languages, agreeing to four digits is much stronger evidence than
// either one passing its own unit tests. Skipped without a payload, because it
// depends on a specific run rather than on a fixture.
globalThis.document = undefined;
load("tools/bench-viz/prefilter.js");
const pf = globalThis.BenchVizPrefilter;
if (typeof arguments === "undefined" || !arguments.length) {
  print("figures.test.js: skipped (no payload given)");
} else {
run(arguments[0]);
}
function run(path) {
const payload = JSON.parse(readFile(path));
const DATA = payload.data;

const SEP = "";
function key(row) {
  return [row.candidate, row.config, row.strategy, row.scanner || ""].join(SEP);
}

const SPIRAL = ["onpair_spiral", '{"bits":16}', "pf_memmem", ""].join(SEP);
const DECODE = ["onpair_spiral_decode", '{"bits":16}', "decode", "memmem-hay"].join(SEP);

function slice(dataset, seriesKey) {
  return DATA.filter(row => row.source === "needle-sweep" && row.dataset === dataset
    && row.op === "contains" && key(row) === seriesKey);
}

let failures = 0;
function near(name, actual, expected, tol) {
  const ok = Math.abs(actual - expected) <= tol;
  if (!ok) failures += 1;
  print(`${ok ? "ok  " : "FAIL"} ${name}: ${actual.toFixed(4)} (expected ~${expected})`);
}

["clickbench-url-1m", "amazon-title", "dbpedia-abstract"].forEach(dataset => {
  const subject = slice(dataset, SPIRAL);
  const baseline = new Map(slice(dataset, DECODE).map(r => [r.query_id, r.latency_ns]));
  const pairs = pf.pairedRatios(subject, baseline);
  const stats = pf.ratioStats(pairs);
  print(`\n${dataset}: n=${stats.n} winRate=${(stats.winRate * 100).toFixed(1)}% ` +
        `median=${stats.median.toFixed(2)}x p90=${stats.p90.toFixed(2)}x`);
  const shipped = pf.gateOutcome(pairs, {maxCost: 16, maxAdmitted: 0.10});
  print(`  gate cost<=16 & rows<10%: selected ${shipped.selected} ` +
        `FP ${shipped.falsePositive} FN ${shipped.falseNegative} ` +
        `regret ${shipped.regret.toFixed(3)}x worst ${shipped.worstRegression.toFixed(2)}x`);
  const always = pf.gateOutcome(pairs, {maxCost: 1e9, maxAdmitted: 1e9});
  print(`  always on: regret ${always.regret.toFixed(3)}x ` +
        `worst ${always.worstRegression.toFixed(1)}x`);
});

// Pooled, which is what the conversation's headline numbers were.
const all = [];
["clickbench-url-1m", "amazon-title", "dbpedia-abstract"].forEach(dataset => {
  const baseline = new Map(slice(dataset, DECODE).map(r => [r.query_id, r.latency_ns]));
  all.push(...pf.pairedRatios(slice(dataset, SPIRAL), baseline));
});
const pooled = pf.ratioStats(all);
print(`\npooled n=${pooled.n}`);
near("pooled win rate vs decode+memmem", pooled.winRate, 0.933, 0.005);
// The SIMD path carries the wide-cover disasters the row table would rescue,
  // so its pooled median sits below the row-table build's.
  near("pooled median speedup", pooled.median, 10.96, 0.05);

const shipped = pf.gateOutcome(all, {maxCost: 16, maxAdmitted: 0.10});
near("selected by cost<=16 & rows<10%", shipped.selected, 2598, 4);
near("false positives", shipped.falsePositive, 0, 0);
near("regret", shipped.regret, 1.034, 0.004);
const always = pf.gateOutcome(all, {maxCost: 1e9, maxAdmitted: 1e9});
near("always-on regret", always.regret, 4.315, 0.02);
near("always-on worst regression", always.worstRegression, 55.28, 0.3);
const tight = pf.gateOutcome(all, {maxCost: 1e9, maxAdmitted: 0.10});
print(`\nno cost clause, rows<10%: FP ${tight.falsePositive} ` +
      `regret ${tight.regret.toFixed(3)}x worst ${tight.worstRegression.toFixed(2)}x`);
const noRows = pf.gateOutcome(all, {maxCost: 16, maxAdmitted: 1e9});
near("cost<=16 alone: regret", noRows.regret, 1.069, 0.004);
near("cost<=16 alone: worst regression", noRows.worstRegression, 5.30, 0.05);

// R^2 leaderboard, against the Python figures.
const cover = all.map(p => p.row).filter(r =>
  Number.isFinite(r.comparison_cost) && Number.isFinite(r.gbps) && r.gbps > 0);
const ln = (rows, k) => rows.map(r => Math.log(Math.max(r[k], 1e-12)));
const ys = ln(cover, "gbps");
near("R2 selectivity", pf.rsquared([ln(cover, "selectivity")], ys), 0.259, 0.01);
near("R2 cover width", pf.rsquared([ln(cover, "comparison_cost")], ys), 0.842, 0.01);
near("R2 covered_fraction", pf.rsquared([ln(cover, "covered_fraction")], ys), 0.703, 0.01);
// Two different predictor pairs, deliberately: covered_fraction is the axis the
// earlier analysis used, admitted rows the one the policy uses. They are not a
// reparametrization of each other once several columns are pooled, because
// codes-per-row differs between them.
near("R2 width + covered_fraction",
  pf.rsquared([ln(cover, "comparison_cost"), ln(cover, "covered_fraction")], ys), 0.910, 0.005);
near("R2 width + rows verified",
  pf.rsquared([ln(cover, "comparison_cost"), ln(cover, "admitted_rows")], ys), 0.897, 0.005);

print(failures ? `\n${failures} MISMATCHES` : "\nall cross-checks agree with Python");
if (failures) throw new Error("mismatch");
}
