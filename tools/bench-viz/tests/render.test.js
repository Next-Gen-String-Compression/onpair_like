// Exercises every render function in prefilter.js against a real payload.
//
//   /System/Library/Frameworks/JavaScriptCore.framework/Versions/A/Helpers/jsc \
//     tools/bench-viz/tests/render.test.js -- <payload.json>
//
// where payload.json is `{"data": [...], "analysis": {...}}` extracted from a
// built explorer (tests/extract_payload.py does that). Falls back to a small
// synthetic payload when none is given, so it runs with no prior build.
//
// The point is not to check pixels. It is that the drawing code runs at all:
// a mistyped element id, a helper that does not exist, or a null that is not
// guarded are all invisible to a syntax check and all fatal in a browser.

// ---------------------------------------------------------------- DOM stub

const registry = new Map();

class Node {
  constructor(tag) {
    this.tagName = tag;
    this.children = [];
    this.attributes = {};
    this.dataset = {};
    this.style = {};
    this.classList = {
      _set: new Set(),
      add(name) { this._set.add(name); },
      remove(name) { this._set.delete(name); },
      toggle(name, on) { if (on) this._set.add(name); else this._set.delete(name); },
      contains(name) { return this._set.has(name); },
    };
    this._textContent = "";
    this._listeners = {};
    this.hidden = false;
    this.disabled = false;
    this.value = "";
  }

  get textContent() { return this._textContent; }

  set textContent(value) {
    this._textContent = String(value);
    this.children = [];
  }

  set innerHTML(value) {
    if (value !== "") throw new Error("stub only supports innerHTML = ''");
    this.children = [];
  }

  get innerHTML() { return ""; }

  get firstElementChild() { return this.children[0] || null; }

  appendChild(child) {
    if (!child) throw new Error(`appendChild(${child}) on <${this.tagName}>`);
    this.children.push(child);
    return child;
  }

  setAttribute(name, value) {
    if (value === undefined || value === null || (typeof value === "number" && !Number.isFinite(value))) {
      throw new Error(`<${this.tagName} ${name}="${value}"> is not renderable`);
    }
    this.attributes[name] = String(value);
  }

  getAttribute(name) { return this.attributes[name]; }

  addEventListener(kind, handler) { this._listeners[kind] = handler; }

  querySelectorAll() { return []; }

  get viewBox() {
    const parts = String(this.attributes.viewBox || "0 0 1080 460").split(" ").map(Number);
    return {baseVal: {x: parts[0], y: parts[1], width: parts[2], height: parts[3]}};
  }

  // Depth-first text, for asserting that a panel produced something.
  allText() {
    return this._textContent + this.children.map(child => child.allText()).join(" ");
  }

  countTag(tag) {
    return (this.tagName === tag ? 1 : 0)
      + this.children.reduce((sum, child) => sum + child.countTag(tag), 0);
  }
}

function register(id) {
  const node = new Node("div");
  node.id = id;
  registry.set(id, node);
  return node;
}

// Every id prefilter.js reaches for. A missing one is a bug in this list or in
// the template, and either way the test should say so.
[
  "panel-prefilter", "pf-source", "pf-source-field", "pf-dataset",
  "pf-baseline", "pf-subject",
  "pf-max-cost", "pf-max-rows", "pf-gate-reset", "pf-gate-best",
  "pf-gate-safe", "pf-policy-note", "pf-columns-body",
  "pf-verdict-plot", "pf-verdict-body", "pf-correlation-plot", "pf-correlation-body",
  "pf-predictor", "pf-frontier-plot", "pf-frontier-status",
].forEach(register);

globalThis.document = {
  getElementById: id => registry.get(id) || null,
  createElement: tag => new Node(tag),
  createElementNS: (ns, tag) => new Node(tag),
  querySelectorAll: () => [],
};
globalThis.navigator = {};
globalThis.setTimeout = () => 0;

// ------------------------------------------------------------------ payload

const args = typeof arguments === "undefined" ? [] : arguments;
let payload;
if (args.length) {
  payload = JSON.parse(readFile(args[0]));
} else {
  const rows = [];
  for (let i = 0; i < 80; i += 1) {
    const cost = 1 + (i % 20);
    rows.push({
      source: "run", candidate: i % 2 ? "prefilter" : "fallback", config: "{}",
      strategy: i % 2 ? "pf" : "decode", scanner: null, dataset: "mini",
      chunk_rows: 0, op: "contains", query_id: `q${i >> 1}`,
      latency_ns: i % 2 ? 1000 + cost * 50 : 8000,
      gbps: i % 2 ? 40 : 5, ns_per_row: 1, selectivity: 0.001 * (1 + (i % 7)),
      needle_len: 4 + (i % 30), comparison_cost: i % 2 ? cost : null,
      covered_fraction: 0.0001 * (1 + i % 9), cover_points: 1, cover_ranges: 0,
      admitted_rows: i % 2 ? 0.001 * (1 + i % 9) : null,
      codes: 10000, column_rows: 1000, column_bytes: 80000,
      codes_per_row: 10, bytes_per_code: 8, kappa: 4,
      arm: i % 2 ? (cost <= 16 ? "specialized" : "wide") : null,
      predicted_ns: i % 2 ? 1000 + cost * 55 : null,
      predicted_scan_ns: i % 2 ? 900 : null,
      predicted_verify_ns: i % 2 ? 100 + cost * 55 : null,
      display: null, dataset_display: null, config_hash: "h",
    });
  }
  // Two more substring operations, neither of which compiles a cover -- which
  // is the real state of things: the fact probe answers for `contains` only.
  // Different sizes, so pooling them in would show up in any count.
  [["multi_contains", 60], ["contains_any", 40]].forEach(([op, take]) => {
    rows.slice(0, take).forEach(row => {
      rows.push(Object.assign({}, row, {
        op, query_id: `${op}-${row.query_id}`,
        comparison_cost: null, cover_points: null, cover_ranges: null,
        covered_fraction: null, admitted_rows: null,
      }));
    });
  });
  payload = {
    data: rows,
    analysis: {
      columns: {"runmini0": {num_rows: 1000, payload_bytes: 80000, build_codes: 10000}},
      models: {}, noise: null, max_simd_comparisons: 16, max_candidate_row_fraction: 0.1,
    },
  };
}

// ------------------------------------------------------------------- host

const DATA = payload.data;
const first = DATA[0];
const state = {
  source: first.source, dataset: first.dataset, op: first.op, chunk: first.chunk_rows,
  xMetric: "comparison_cost", yMetric: "gbps",
  visibleSeries: new Set(), visibleDecode: new Set(),
  ranges: {comparison_cost: {min: null, max: null}},
};

function finite(value) { return typeof value === "number" && Number.isFinite(value); }
function seriesKey(row) {
  return [row.candidate, row.config, row.strategy, row.scanner || ""].join("");
}
function seriesMeta(row) {
  return {
    id: seriesKey(row), candidateId: row.candidate, candidate: row.candidate,
    label: row.display || `${row.candidate} · ${row.strategy}`,
  };
}
function makeScale(values, kind, start, end) {
  const clean = values.filter(finite).filter(v => kind !== "log" || v > 0);
  const low = clean.length ? Math.min(...clean) : 0;
  const high = clean.length ? Math.max(...clean) : 1;
  const span = high - low || 1;
  return {
    map: value => start + ((Math.max(low, Math.min(high, value)) - low) / span) * (end - start),
    ticks: [low, (low + high) / 2, high],
    kind, hasZero: false,
  };
}

const host = {
  DATA, DEFAULTS: {}, ANALYSIS: payload.analysis, state,
  helpers: {
    finite,
    unique: values => [...new Set(values)],
    svgElement(name, attributes, textValue) {
      const node = document.createElementNS("svg", name);
      Object.entries(attributes || {}).forEach(([key, value]) => node.setAttribute(key, value));
      if (textValue !== undefined && textValue !== null) node.textContent = textValue;
      return node;
    },
    makeScale,
    formatSignificant: value => String(Number(value.toPrecision(3))),
    percentText: fraction => `${(fraction * 100).toFixed(3)}%`,
    seriesMeta, seriesKey,
    colorFor: () => "#0064a4",
    setSelection(patch) { Object.assign(state, patch); },
    sourceValues: () => [...new Set(DATA.map(row => row.source))].sort(),
    datasetValues: () => [...new Set(DATA.map(row => row.dataset))].sort(),
    datasetLabel: value => value,
    bindExportButtons: () => {},
    exportSvg: () => {},
    filteredRows: () => {
      throw new Error("the prefilter section must scope its own rows, not "
        + "borrow the plot's: filteredRows applies the plot's Focus X range");
    },
    contextRows: () => DATA,
    attachTooltip: (node, lines) => {
      if (!node) throw new Error("attachTooltip on a missing node");
      lines.forEach(line => {
        if (line === undefined) throw new Error("tooltip line is undefined");
      });
    },
    refs: {},
  },
  onRender: [],
};
globalThis.benchViz = host;

// ------------------------------------------------------------------- run it

let failures = 0;
function check(name, condition, detail) {
  if (!condition) {
    failures += 1;
    print(`FAIL ${name}${detail === undefined ? "" : ` — ${detail}`}`);
  }
}

// What the section itself will look at: only operations that carry cover facts,
// preferring contains. Mirrored here so the checks below scope the same way.
const coverOps = [...new Set(DATA.filter(row => finite(row.comparison_cost))
  .map(row => row.op))].sort();
const activeOp = coverOps.includes("contains") ? "contains" : coverOps[0] || null;

// Hidden panel: render must be a no-op rather than an error.
registry.get("panel-prefilter").hidden = true;
load("tools/bench-viz/prefilter.js");
check("a hidden panel renders nothing",
  registry.get("pf-columns-body").children.length === 0);

registry.get("panel-prefilter").hidden = false;
check("a renderer registered itself", host.onRender.length === 1);

// Nothing visible yet: the panel should fall back to describing every series
// rather than drawing an empty page.
host.onRender[0]();
check("the column panel produced content",
  registry.get("pf-columns-body").children.length > 0);
check("the verdict panel totalled the suite",
  registry.get("pf-verdict-body").children.length > 0);
check("the verdict panel offers the what-if policies",
  registry.get("pf-verdict-body").countTag("table") >= 3,
  `${registry.get("pf-verdict-body").countTag("table")} tables`);
check("the what-if tables mark the policy in force",
  /this gate/.test(registry.get("pf-verdict-body").allText()));
{
  // The search needs at least eight queries carrying both cover facts before it
  // will name an optimum, so the precondition is counted from the data rather
  // than scraped from the rendered text.
  const perSeries = new Map();
  DATA.filter(row => finite(row.comparison_cost) && finite(row.admitted_rows))
    .forEach(row => {
      const id = seriesKey(row);
      perSeries.set(id, (perSeries.get(id) || 0) + 1);
    });
  const searchable = Math.max(0, ...perSeries.values()) >= 8;
  const shown = registry.get("pf-verdict-body").allText();
  check("the panel reports a searched optimum where it can search",
    !searchable || /fastest gate here/.test(shown),
    `${Math.max(0, ...perSeries.values())} rows carry both cover facts, `
    + `yet no searched optimum was reported`);
}
check("the panel scores selectivity for comparison",
  /selectivity < 1%/.test(registry.get("pf-verdict-body").allText()));
check("the panel names selectivity as unusable for a decision",
  /selectivity is a result of the scan/.test(registry.get("pf-verdict-body").allText()));
{
  // A rule reading a field this run never records must be reported as unscored,
  // not shown as a policy that admits nothing. Forced by taking the field away.
  const saved = DATA.map(row => row.selectivity);
  DATA.forEach(row => { row.selectivity = null; });
  host.onRender[0]();
  const shown = registry.get("pf-verdict-body").allText();
  check("a rule whose field is absent is named rather than scored as zero",
    /Not scored/.test(shown) && /selectivity/.test(shown),
    "no 'Not scored' note after removing selectivity from every row");
  DATA.forEach((row, index) => { row.selectivity = saved[index]; });
  host.onRender[0]();
  check("and the rule returns once the field is back",
    !/Not scored/.test(registry.get("pf-verdict-body").allText()));
}

// Adopting the searched optimum must land in the boxes the gate reads from.
{
  const costBox = registry.get("pf-max-cost");
  const before = costBox.value;
  registry.get("pf-gate-best")._listeners.click();
  check("adopting the fastest gate rewrote the threshold boxes",
    costBox.value !== "" && registry.get("pf-max-rows").value !== "",
    `cost ${costBox.value} (was ${before})`);
}
{
  // Every query counted once: the panel compares one subject to one baseline,
  // and a total that double-counts is the failure mode worth guarding.
  const shown = registry.get("pf-verdict-body").allText();
  const pairCount = DATA.filter(row =>
    row.strategy === "pf" && row.op === activeOp).length;
  const pooled = DATA.filter(row => row.strategy === "pf").length;
  check("the verdict counts each query once",
    !pairCount || shown.includes(String(pairCount)),
    `expected a count of ${pairCount} in: ${shown.slice(0, 200)}`);
  // Operations that compile no cover must not be pooled in. They would not
  // change a single decision -- the gate cannot rule on a query with no cover
  // facts -- but they would dilute every total the panel reports. Anchored on
  // the "selected of total" phrasing, because a bare number search matches
  // things like "p90".
  const total = n => new RegExp(`of ${n.toLocaleString("en-GB")}\\b`);
  check("the verdict totals the operation that compiles a cover",
    !pairCount || total(pairCount).test(shown),
    `no "of ${pairCount}" in: ${shown.slice(0, 200)}`);
  check("and pools nothing else in",
    pooled === pairCount || !total(pooled).test(shown),
    `"of ${pooled}" appeared, pooling every operation together`);
}
check("the verdict panel drew the time bars",
  registry.get("pf-verdict-plot").children.length > 0);
check("the predictor panel said something",
  registry.get("pf-correlation-body").children.length > 0);
{
  // On a payload too small for the statistics the panel must decline in words
  // rather than render blank -- a fixture run is a real case, not a broken one.
  // Where it does compute, the columns and the explanation have to be there.
  const shown = registry.get("pf-correlation-body").allText();
  const declined = /Not enough measurements|No predictor is reported/.test(shown);
  check("the predictor panel reports a rank correlation, or says why not",
    declined || /correlation with throughput/.test(shown), shown.slice(0, 120));
  check("the predictor panel explains the policy's own quantity",
    declined || /Covered codes ÷ rows is the share of rows/.test(shown));
  check("the policy is one of the scored quantities",
    declined || /the policy itself/.test(shown));
}

{
  // The policy quantity claims "admits exactly when below 1". A cap that admits
  // at the boundary and a budget that excludes at it make that easy to get
  // wrong by one query, so it is checked against the rule on every row.
  const margin = row => {
    if (!Number.isFinite(row.comparison_cost)) return null;
    return Math.max(row.comparison_cost / (Math.floor(16) + 1),
      Number.isFinite(row.admitted_rows) ? row.admitted_rows / 0.1 : 0);
  };
  const disagreements = DATA.filter(row => {
    const value = margin(row);
    if (value === null) return false;
    const rule = row.comparison_cost <= 16
      && (Number.isFinite(row.admitted_rows) ? row.admitted_rows < 0.1 : true);
    return (value < 1) !== rule;
  });
  check("below 1 means admitted, on every row",
    disagreements.length === 0,
    `${disagreements.length} rows where the margin and the rule disagree`);
}
{
  // Both the scatter and its selector depend on there being a quantity worth
  // plotting, so they stand or fall together.
  const plotted = registry.get("pf-correlation-plot").children.length > 0;
  check("a drawn scatter comes with a filled selector",
    !plotted || registry.get("pf-predictor").children.length > 0);
  check("the selector offers only quantities whose axis stands alone",
    registry.get("pf-predictor").children.every(option => option.value !== "policy_margin"),
    "policy_margin is offered as an x-axis");
}
check("the frontier drew marks", registry.get("pf-frontier-plot").children.length > 0);
check("frontier reported a status",
  registry.get("pf-frontier-status").textContent.length > 0);


// The candidates card belongs to the throughput tab, so ticking series must not
// change what this panel shows.
const beforeSelection = registry.get("pf-columns-body").allText();
DATA.forEach(row => state.visibleSeries.add(seriesKey(row)));
host.onRender[0]();
check("the panel ignores the throughput tab's candidate selection",
  registry.get("pf-columns-body").allText() === beforeSelection);
check("the column table names the shape fields",
  /codes\/row/.test(registry.get("pf-columns-body").allText()));

// Every control must be bound, and changing one must not throw.
["pf-source", "pf-dataset", "pf-baseline", "pf-subject",
 "pf-max-cost", "pf-max-rows",
 "pf-gate-reset", "pf-predictor"].forEach(id => {
    const node = registry.get(id);
    const kinds = Object.keys(node._listeners);
    check(`${id} is bound`, kinds.length > 0, "no listener attached");
  });

const costInput = registry.get("pf-max-cost");
costInput.value = "8";
costInput._listeners.input();
check("tightening the cover-width gate re-rendered",
  registry.get("pf-frontier-plot").children.length > 0);

const rowsInput = registry.get("pf-max-rows");
rowsInput.value = "1";
rowsInput._listeners.input();
check("tightening the verification budget re-rendered",
  registry.get("pf-frontier-plot").children.length > 0);

registry.get("pf-gate-reset")._listeners.click();
check("resetting restored the shipped policy", costInput.value === "16");
check("the policy bar says the rule is the shipped one",
  /shipped rule/.test(registry.get("pf-policy-note").textContent),
  registry.get("pf-policy-note").textContent);

// One quantity, one name. It had five across the panels once, which is how a
// wrong axis label survived unnoticed.
{
  const labels = registry.get("pf-frontier-plot").allText();
  check("the frontier names the quantity it plots",
    /covered codes ÷ rows/.test(labels),
    "the y-axis label does not name the quantity it draws");
}

// A selection with no rows at all must not throw.
state.dataset = "no-such-dataset";
host.onRender[0]();
state.dataset = first.dataset;
host.onRender[0]();

// Every x-metric the throughput panel offers must survive this panel too.
["selectivity", "needle_len"].forEach(metric => {
    state.xMetric = metric;
    state.ranges[metric] = {min: null, max: null};
    host.onRender[0]();
  });

print(failures === 0 ? "prefilter.js render: all checks passed"
                     : `prefilter.js render: ${failures} checks FAILED`);
if (failures) throw new Error(`${failures} failing checks`);
