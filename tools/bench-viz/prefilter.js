// Prefilter analysis section.
//
// The throughput panel answers "how fast was this series". None of the
// questions a compressed-domain prefilter actually raises are of that shape:
// they are all comparisons against the fallback the engine would otherwise
// run, on the same needle. So everything here is *paired* — a chosen baseline
// series supplies the alternative time for each query, and every statistic is
// computed over those pairs.
//
// The pure reductions are exported on `BenchVizPrefilter` so they can be
// exercised without a browser (see tests/prefilter.test.js, run under jsc).

(function () {
  "use strict";

  // -------------------------------------------------------------- reductions

  function finite(value) {
    return typeof value === "number" && Number.isFinite(value);
  }

  function sorted(values) {
    return [...values].sort((a, b) => a - b);
  }

  function quantileOf(ordered, fraction) {
    if (!ordered.length) return null;
    const position = (ordered.length - 1) * fraction;
    const lower = Math.floor(position);
    const upper = Math.ceil(position);
    if (lower === upper) return ordered[lower];
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower);
  }

  /// Per-query speedup of `rows` over the baseline time for the same query.
  ///
  /// Pairing is by query id, not by position: a series may be missing queries
  /// the baseline has, and averaging two differently-shaped sets would compare
  /// different work.
  function pairedRatios(rows, baselineByQuery) {
    const out = [];
    rows.forEach(row => {
      const base = baselineByQuery.get(row.query_id);
      if (!finite(base) || !finite(row.latency_ns) || row.latency_ns <= 0) return;
      out.push({row, base, ratio: base / row.latency_ns});
    });
    return out;
  }

  function ratioStats(pairs) {
    if (!pairs.length) return null;
    const ratios = sorted(pairs.map(p => p.ratio));
    return {
      n: pairs.length,
      wins: pairs.filter(p => p.ratio > 1).length,
      winRate: pairs.filter(p => p.ratio > 1).length / pairs.length,
      min: ratios[0],
      p10: quantileOf(ratios, 0.1),
      median: quantileOf(ratios, 0.5),
      p90: quantileOf(ratios, 0.9),
      max: ratios[ratios.length - 1],
      // Total time both ways, which is the only summary that weighs a query by
      // how long it takes. A median speedup of 12x means nothing if the three
      // queries that dominate the wall clock are the ones that regressed.
      totalSelf: pairs.reduce((sum, p) => sum + p.row.latency_ns, 0),
      totalBase: pairs.reduce((sum, p) => sum + p.base, 0),
    };
  }

  /// Should this query be prefiltered? `gate` is either a threshold pair or the
  /// fitted cost model.
  function gateAdmits(row, gate) {
    // An arbitrary rule, for scoring policies that are not two thresholds --
    // including ones that could never be implemented, which is the only way to
    // show what they would have been worth.
    if (typeof gate.admits === "function") return gate.admits(row);
    if (gate.useModel) {
      if (!finite(row.predicted_ns)) return null;
      const base = gate.baselineFor ? gate.baselineFor(row) : null;
      if (!finite(base)) return null;
      return row.predicted_ns < base * gate.margin;
    }
    if (!finite(row.comparison_cost)) return null;
    if (row.comparison_cost > gate.maxCost) return false;
    if (!finite(row.admitted_rows)) return true;
    return row.admitted_rows < gate.maxAdmitted;
  }

  /// Score a gate the way it will actually be paid for: by total time.
  ///
  /// Accuracy is the wrong headline. A gate that is right about 2800 cheap
  /// queries and wrong about the one 50x regression looks excellent and is not.
  /// `regret` is total time under the policy divided by total time under an
  /// oracle that knows every runtime, and `worstRegression` is the single
  /// query the policy hurt most.
  function gateOutcome(pairs, gate) {
    let selected = 0;
    let falsePositive = 0;
    let falseNegative = 0;
    let undecided = 0;
    let policy = 0;
    let oracle = 0;
    let always = 0;
    let never = 0;
    let worstRegression = 1;
    let missedBest = 1;
    pairs.forEach(({row, base, ratio}) => {
      const admits = gateAdmits(row, gate);
      const faster = ratio > 1;
      oracle += Math.min(row.latency_ns, base);
      always += row.latency_ns;
      never += base;
      if (admits === null) {
        undecided += 1;
        policy += base;
        return;
      }
      policy += admits ? row.latency_ns : base;
      if (admits) {
        selected += 1;
        if (!faster) {
          falsePositive += 1;
          worstRegression = Math.max(worstRegression, 1 / ratio);
        }
      } else if (faster) {
        falseNegative += 1;
        missedBest = Math.max(missedBest, ratio);
      }
    });
    return {
      n: pairs.length,
      selected,
      rejected: pairs.length - selected - undecided,
      undecided,
      falsePositive,
      falseNegative,
      accuracy: pairs.length ? (pairs.length - falsePositive - falseNegative - undecided) / pairs.length : null,
      policyNs: policy,
      oracleNs: oracle,
      alwaysNs: always,
      neverNs: never,
      regret: oracle > 0 ? policy / oracle : null,
      alwaysRegret: oracle > 0 ? always / oracle : null,
      neverRegret: oracle > 0 ? never / oracle : null,
      worstRegression,
      missedBest,
    };
  }

  /// Regret and false positives as the verification budget is varied.
  ///
  /// The point of showing the sweep rather than one number is that the shipped
  /// constant was calibrated on one suite; this is how you re-derive it on
  /// yours without asking anybody.
  function sweepAdmitted(pairs, maxCost, thresholds) {
    return thresholds.map(threshold => {
      const outcome = gateOutcome(pairs, {maxCost, maxAdmitted: threshold});
      return {
        threshold,
        regret: outcome.regret,
        falsePositive: outcome.falsePositive,
        falseNegative: outcome.falseNegative,
        selected: outcome.selected,
        worstRegression: outcome.worstRegression,
      };
    });
  }

  /// Ordinary least squares R^2 of `ys` on one or more predictors.
  ///
  /// Used to put candidate x-axes side by side on the same target. A predictor
  /// that explains a quarter of the variance is not a weaker version of one
  /// that explains all of it — it is a different question, and seeing the two
  /// numbers together is the whole argument.
  function rsquared(columns, ys) {
    const n = ys.length;
    if (n < 3 || !columns.length) return null;
    const width = columns.length + 1;
    const design = [];
    for (let i = 0; i < n; i += 1) {
      const row = [1];
      for (let c = 0; c < columns.length; c += 1) row.push(columns[c][i]);
      design.push(row);
    }
    const normal = [];
    const rhs = [];
    for (let i = 0; i < width; i += 1) {
      normal.push(new Array(width).fill(0));
      rhs.push(0);
    }
    for (let r = 0; r < n; r += 1) {
      for (let i = 0; i < width; i += 1) {
        rhs[i] += design[r][i] * ys[r];
        for (let j = 0; j < width; j += 1) normal[i][j] += design[r][i] * design[r][j];
      }
    }
    const beta = solve(normal, rhs);
    if (!beta) return null;
    const mean = ys.reduce((a, b) => a + b, 0) / n;
    let residual = 0;
    let total = 0;
    for (let r = 0; r < n; r += 1) {
      let fit = 0;
      for (let i = 0; i < width; i += 1) fit += beta[i] * design[r][i];
      residual += (ys[r] - fit) ** 2;
      total += (ys[r] - mean) ** 2;
    }
    return total > 0 ? 1 - residual / total : null;
  }

  function solve(matrix, rhs) {
    const n = rhs.length;
    const aug = matrix.map((row, i) => [...row, rhs[i]]);
    for (let col = 0; col < n; col += 1) {
      let pivot = col;
      for (let r = col + 1; r < n; r += 1) {
        if (Math.abs(aug[r][col]) > Math.abs(aug[pivot][col])) pivot = r;
      }
      if (Math.abs(aug[pivot][col]) < 1e-12) return null;
      const swap = aug[col];
      aug[col] = aug[pivot];
      aug[pivot] = swap;
      for (let r = col + 1; r < n; r += 1) {
        const factor = aug[r][col] / aug[col][col];
        if (!factor) continue;
        for (let k = col; k <= n; k += 1) aug[r][k] -= factor * aug[col][k];
      }
    }
    const out = new Array(n).fill(0);
    for (let col = n - 1; col >= 0; col -= 1) {
      let acc = aug[col][n];
      for (let k = col + 1; k < n; k += 1) acc -= aug[col][k] * out[k];
      out[col] = acc / aug[col][col];
    }
    return out;
  }

  /// Log-spaced bins over a positive metric, each carrying its own count.
  ///
  /// `n` per bin is reported rather than implied: a bin holding one query and a
  /// bin holding four hundred look identical on a median line, and the first
  /// one is noise.
  /// Average ranks, ties shared, so a rank correlation is not distorted by
  /// repeated values -- and cover width is nothing but repeated values.
  function ranksOf(values) {
    const order = values.map((value, index) => [value, index])
      .sort((a, b) => a[0] - b[0]);
    const ranks = new Array(values.length);
    let index = 0;
    while (index < order.length) {
      let end = index;
      while (end + 1 < order.length && order[end + 1][0] === order[index][0]) end += 1;
      const shared = (index + end) / 2 + 1;
      for (let k = index; k <= end; k += 1) ranks[order[k][1]] = shared;
      index = end + 1;
    }
    return ranks;
  }

  function pearson(xs, ys) {
    const n = xs.length;
    if (n < 3) return null;
    const mx = xs.reduce((a, b) => a + b, 0) / n;
    const my = ys.reduce((a, b) => a + b, 0) / n;
    let sxy = 0;
    let sxx = 0;
    let syy = 0;
    for (let i = 0; i < n; i += 1) {
      const dx = xs[i] - mx;
      const dy = ys[i] - my;
      sxy += dx * dy;
      sxx += dx * dx;
      syy += dy * dy;
    }
    return sxx === 0 || syy === 0 ? null : sxy / Math.sqrt(sxx * syy);
  }

  /// Rank correlation: monotone association with no assumed functional form.
  ///
  /// R^2 from a log-log fit assumes a power law, so it penalises a variable
  /// whose relationship with throughput is monotone but bent -- and the kernel
  /// cliff at the specialized-cover boundary is exactly such a bend. This says
  /// whether the variable orders queries correctly, which is all a threshold
  /// rule ever uses.
  function spearman(xs, ys) {
    if (xs.length !== ys.length) return null;
    return pearson(ranksOf(xs), ranksOf(ys));
  }

  /// Probability that a query the prefilter wins outranks one it loses.
  ///
  /// The gate question is binary, and this is the threshold-free measure of it:
  /// 0.5 is a coin toss and no cut on this variable can help, 1.0 separates the
  /// two classes perfectly. `direction` is +1 when larger values mark wins.
  function auc(values, wins) {
    const positives = [];
    const negatives = [];
    values.forEach((value, index) => {
      (wins[index] ? positives : negatives).push(value);
    });
    if (!positives.length || !negatives.length) return null;
    const ranks = ranksOf(values);
    let rankSum = 0;
    values.forEach((_value, index) => {
      if (wins[index]) rankSum += ranks[index];
    });
    const n1 = positives.length;
    const n2 = negatives.length;
    const raw = (rankSum - (n1 * (n1 + 1)) / 2) / (n1 * n2);
    return {value: Math.max(raw, 1 - raw), direction: raw >= 0.5 ? 1 : -1, wins: n1, losses: n2};
  }

  /// The best a single threshold on one quantity can do, scored by total time.
  ///
  /// The decision-theoretic answer, and the only one that weighs a query by how
  /// long it takes. `direction` -1 admits below the cut, +1 above it.
  function bestThreshold(pairs, pick, direction, candidates) {
    let oracle = 0;
    const usable = [];
    pairs.forEach(pair => {
      const mine = pair.row.latency_ns;
      const base = pair.base;
      if (!finite(mine) || !finite(base)) return;
      oracle += Math.min(mine, base);
      const value = pick(pair.row);
      if (finite(value)) usable.push({value, mine, base});
    });
    if (!usable.length || oracle <= 0) return null;
    // Pairs the quantity is missing on are declined, so their baseline cost is
    // fixed and belongs in every candidate's total.
    const fixed = pairs.reduce((sum, pair) => {
      const value = pick(pair.row);
      return finite(value) || !finite(pair.base) ? sum : sum + pair.base;
    }, 0);
    let best = null;
    candidates.forEach(cut => {
      let total = fixed;
      let regressions = 0;
      for (let i = 0; i < usable.length; i += 1) {
        const admit = direction < 0 ? usable[i].value < cut : usable[i].value > cut;
        if (admit) {
          total += usable[i].mine;
          if (usable[i].mine > usable[i].base) regressions += 1;
        } else {
          total += usable[i].base;
        }
      }
      if (!best || total < best.total) best = {cut, total, regressions};
    });
    return best ? {...best, regret: best.total / oracle} : null;
  }

  function binBy(rows, metric, binCount) {
    const values = rows.map(r => r[metric]).filter(v => finite(v) && v > 0);
    if (!values.length) return [];
    const low = Math.log10(Math.min(...values));
    const high = Math.log10(Math.max(...values));
    const span = high - low || 1;
    const buckets = new Map();
    rows.forEach(row => {
      const value = row[metric];
      if (!finite(value) || value <= 0) return;
      const slot = Math.min(binCount - 1,
        Math.floor(((Math.log10(value) - low) / span) * binCount));
      if (!buckets.has(slot)) buckets.set(slot, []);
      buckets.get(slot).push(row);
    });
    return [...buckets.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([slot, mine]) => ({
        lo: 10 ** (low + (span * slot) / binCount),
        hi: 10 ** (low + (span * (slot + 1)) / binCount),
        rows: mine,
      }));
  }

  const exported = {
    pairedRatios, ratioStats, gateAdmits, gateOutcome, sweepAdmitted,
    rsquared, solve, binBy, quantileOf, ranksOf, pearson, spearman, auc,
    bestThreshold,
  };
  if (typeof module === "object" && module.exports) module.exports = exported;
  else if (typeof globalThis === "object") globalThis.BenchVizPrefilter = exported;

  // ------------------------------------------------------------------ render
  //
  // Everything below needs a document, and is skipped when this file is loaded
  // for testing.
  if (typeof document === "undefined") return;
  const host = globalThis.benchViz;
  if (!host) return;

  const {state, DATA, ANALYSIS, helpers} = host;
  const {svgElement, makeScale, formatSignificant, percentText, seriesMeta,
         colorFor, attachTooltip, unique} = helpers;
  const NS = "http://www.w3.org/2000/svg";

  const ui = {
    baseline: null,
    maxCost: ANALYSIS.max_simd_comparisons || 16,
    maxAdmitted: ANALYSIS.max_candidate_row_fraction || 0.1,
    subject: null,
    xScale: "log",
    yScale: "log",
    corrXScale: "log",
    corrYScale: "log",
    predictor: null,
    op: null,
    best: null,
  };

  function el(id) {
    return document.getElementById(id);
  }

  function text(tag, value, className) {
    const node = document.createElement(tag);
    node.textContent = value;
    if (className) node.className = className;
    return node;
  }

  function table(headers, rows, className) {
    const node = document.createElement("table");
    node.className = `pf-table${className ? ` ${className}` : ""}`;
    const head = document.createElement("thead");
    const headRow = document.createElement("tr");
    headers.forEach(header => headRow.appendChild(text("th", header)));
    head.appendChild(headRow);
    node.appendChild(head);
    const body = document.createElement("tbody");
    rows.forEach(entry => {
      // A row is either its cells, or {cells, className} when it needs marking
      // out -- the policy currently in force, say.
      const cells = Array.isArray(entry) ? entry : entry.cells;
      const tr = document.createElement("tr");
      if (!Array.isArray(entry) && entry.className) tr.className = entry.className;
      cells.forEach(cell => {
        if (cell && typeof cell === "object" && cell.node) {
          const td = document.createElement("td");
          if (cell.className) td.className = cell.className;
          td.appendChild(cell.node);
          tr.appendChild(td);
        } else {
          tr.appendChild(text("td", cell === null || cell === undefined ? "—" : String(cell)));
        }
      });
      body.appendChild(tr);
    });
    node.appendChild(body);
    return node;
  }

  function swatch(color) {
    const dot = document.createElement("span");
    dot.className = "pf-swatch";
    dot.style.background = color;
    return dot;
  }


  function msText(ns) {
    if (!finite(ns)) return null;
    if (ns >= 1e9) return `${(ns / 1e9).toFixed(2)} s`;
    if (ns >= 1e6) return `${(ns / 1e6).toFixed(1)} ms`;
    if (ns >= 1e3) return `${(ns / 1e3).toFixed(1)} µs`;
    return `${ns.toFixed(0)} ns`;
  }

  function ratioText(value) {
    if (!finite(value)) return "—";
    if (value >= 100) return `${Math.round(value)}×`;
    if (value >= 10) return `${value.toFixed(1)}×`;
    return `${value.toFixed(2)}×`;
  }


  // Series present in the current selection, in plot order.
  function allSeries(rows) {
    const seen = new Map();
    rows.forEach(row => {
      const meta = seriesMeta(row);
      if (!seen.has(meta.id)) seen.set(meta.id, {meta, rows: []});
      seen.get(meta.id).rows.push(row);
    });
    return [...seen.values()];
  }

  function baselineMap(series) {
    const map = new Map();
    if (!series) return map;
    series.rows.forEach(row => {
      if (finite(row.latency_ns) && row.latency_ns > 0) map.set(row.query_id, row.latency_ns);
    });
    return map;
  }

  function medianLatency(group) {
    const times = group.rows.map(row => row.latency_ns).filter(finite);
    return times.length ? exported.quantileOf(sorted(times), 0.5) : null;
  }

  function fillSeriesSelect(id, groups) {
    const select = el(id);
    if (!select) return;
    const signature = groups.map(group => group.meta.id).join("\u001f");
    if (select.dataset.signature === signature) return;
    select.dataset.signature = signature;
    select.innerHTML = "";
    groups.forEach(group => {
      const option = document.createElement("option");
      option.value = group.meta.id;
      option.textContent = group.meta.label;
      select.appendChild(option);
    });
  }

  // The panel compares exactly one series against one other. Totalling several
  // subjects at once would count every query more than once, which is the one
  // way to make a suite total say nothing.
  function syncSeriesOptions(groups) {
    fillSeriesSelect("pf-baseline", groups);
    fillSeriesSelect("pf-subject", groups);
    const ids = groups.map(group => group.meta.id);

    if (!ids.includes(ui.baseline)) {
      // The slowest series by median latency: on these runs that is the
      // decode-then-scan fallback, which is the comparison that matters.
      let slowest = null;
      groups.forEach(group => {
        const mid = medianLatency(group);
        if (mid !== null && (slowest === null || mid > slowest.value)) {
          slowest = {id: group.meta.id, value: mid};
        }
      });
      ui.baseline = slowest ? slowest.id : ids[0] || null;
    }
    if (!ids.includes(ui.subject) || ui.subject === ui.baseline) {
      // A series that reports cover facts is a prefilter; prefer the fastest of
      // those, and fall back to anything that is not the baseline.
      const candidates = groups.filter(group =>
        group.meta.id !== ui.baseline
        && group.rows.some(row => finite(row.comparison_cost)));
      const pool = candidates.length
        ? candidates : groups.filter(group => group.meta.id !== ui.baseline);
      let fastest = null;
      pool.forEach(group => {
        const mid = medianLatency(group);
        if (mid !== null && (fastest === null || mid < fastest.value)) {
          fastest = {id: group.meta.id, value: mid};
        }
      });
      ui.subject = fastest ? fastest.id : (pool[0] ? pool[0].meta.id : null);
    }

    const baselineSelect = el("pf-baseline");
    if (baselineSelect && ui.baseline) baselineSelect.value = ui.baseline;
    const subjectSelect = el("pf-subject");
    if (subjectSelect && ui.subject) subjectSelect.value = ui.subject;
    return {
      baseline: groups.find(group => group.meta.id === ui.baseline) || null,
      subject: groups.find(group => group.meta.id === ui.subject) || null,
    };
  }




  // ------------------------------------------------------- panel: the column

  function bytesText(value) {
    if (!finite(value) || value <= 0) return null;
    if (value >= 1e9) return `${(value / 1e9).toFixed(2)} GB`;
    if (value >= 1e6) return `${(value / 1e6).toFixed(1)} MB`;
    return `${(value / 1e3).toFixed(0)} kB`;
  }

  function countText(value) {
    return finite(value) && value > 0
      ? Math.round(value).toLocaleString("en-GB") : null;
  }

  function renderColumns(prefilterRows) {
    const target = el("pf-columns-body");
    if (!target) return;
    target.innerHTML = "";

    // Exactly the column on screen. `columns` is keyed the way the builder
    // keyed it, so this is a lookup rather than a search.
    const key = [state.source || "", state.dataset || "", String(state.chunk || 0)]
      .join("\u001f");
    const facts = (ANALYSIS.columns || {})[key];

    if (!prefilterRows.length) {
      target.appendChild(text("p",
        "Nothing in this selection went through a prefilter.", "pf-note"));
      return;
    }
    if (!facts) {
      target.appendChild(text("p",
        "This run reports no build record for the selected column.", "pf-note"));
      return;
    }

    const rows = facts.num_rows;
    const payload = facts.payload_bytes;
    const codes = facts.build_codes;
    const compressed = facts.compressed_bytes;

    const compressedCell = document.createElement("span");
    compressedCell.textContent = bytesText(compressed) || "—";
    const excludes = facts.compressed_excludes || [];
    if (excludes.length) {
      // The number is a choice about what counts, so it says which parts it
      // left out rather than making the reader guess.
      attachTooltip(compressedCell, [
        "dictionary and code stream",
        `excludes ${excludes.join(" and ")}`,
        finite(facts.footprint_total_bytes)
          ? `${bytesText(facts.footprint_total_bytes)} with everything included` : null,
      ].filter(Boolean));
      compressedCell.className = "pf-has-note";
    }

    target.appendChild(table(
      ["rows", "payload", "compressed", "codes/row", "bytes/code"],
      [[
        countText(rows),
        bytesText(payload),
        {node: compressedCell},
        finite(codes) && finite(rows) && rows > 0 ? (codes / rows).toFixed(2) : null,
        finite(codes) && finite(payload) && codes > 0 ? (payload / codes).toFixed(2) : null,
      ]]));
  }

  // -------------------------------------------------- panel: is it worth it

  function speedupText(from, to) {
    return finite(from) && finite(to) && to > 0 ? `${(from / to).toFixed(2)}x` : null;
  }

  // Where the baseline's time goes under one gate. Kept separate from
  // `gateOutcome` because that scores a policy, while this describes coverage:
  // how much of the work the policy actually reaches.
  function coverage(pairs, gate) {
    const t = {
      baseline: 0, admittedBaseline: 0, admittedSubject: 0, skippedBaseline: 0,
      wonBaseline: 0, lostBaseline: 0, lostExtra: 0,
      n: 0, admitted: 0, won: 0, lost: 0,
    };
    pairs.forEach(pair => {
      const base = pair.base;
      const mine = pair.row.latency_ns;
      if (!finite(base) || !finite(mine)) return;
      t.n += 1;
      t.baseline += base;
      if (gateAdmits(pair.row, gate) === true) {
        t.admitted += 1;
        t.admittedBaseline += base;
        t.admittedSubject += mine;
        if (mine < base) {
          t.won += 1;
          t.wonBaseline += base;
        } else {
          t.lost += 1;
          t.lostBaseline += base;
          t.lostExtra += mine - base;
        }
      } else {
        t.skippedBaseline += base;
      }
    });
    return t;
  }

  // -------------------------------------------------------- the best gate
  //
  // The two thresholds are searched directly rather than reasoned about: the
  // candidate cuts are the values present in the data, so the grid contains
  // every distinct policy this selection can express and nothing else.
  //
  // Two answers, because they are different questions. `fastest` minimises
  // total time and may accept queries it makes slower; `safest` is the fastest
  // gate that makes no query slower at all, which is the rule the shipped
  // constants were chosen under.
  function searchGates(pairs) {
    const usable = pairs.filter(pair =>
      finite(pair.row.comparison_cost) && finite(pair.row.admitted_rows)
      && finite(pair.base) && finite(pair.row.latency_ns));
    if (usable.length < 8) return null;

    const costs = usable.map(pair => pair.row.comparison_cost);
    const admits = usable.map(pair => pair.row.admitted_rows);
    const bases = usable.map(pair => pair.base);
    const mines = usable.map(pair => pair.row.latency_ns);
    const undecidedBase = pairs
      .filter(pair => !finite(pair.row.comparison_cost) && finite(pair.base))
      .reduce((sum, pair) => sum + pair.base, 0);

    // Cover width is small and discrete, so every distinct value is a cut.
    const widths = unique(costs).sort((a, b) => a - b);
    // The budget is continuous, so the cuts are the values themselves, thinned
    // to keep the grid bounded. A cut just above a value admits it.
    const sortedAdmits = unique(admits).sort((a, b) => a - b);
    const stride = Math.max(1, Math.ceil(sortedAdmits.length / 120));
    const budgets = sortedAdmits.filter((_value, index) => index % stride === 0)
      .map(value => value * (1 + 1e-9));
    budgets.push(Infinity);
    widths.push(Infinity);

    let fastest = null;
    let safest = null;
    widths.forEach(width => {
      budgets.forEach(budget => {
        let total = undecidedBase;
        let regressions = 0;
        let worst = 1;
        for (let index = 0; index < usable.length; index += 1) {
          if (costs[index] <= width && admits[index] < budget) {
            total += mines[index];
            if (mines[index] > bases[index]) {
              regressions += 1;
              worst = Math.max(worst, mines[index] / bases[index]);
            }
          } else {
            total += bases[index];
          }
        }
        const found = {width, budget, total, regressions, worst};
        if (!fastest || total < fastest.total) fastest = found;
        if (!regressions && (!safest || total < safest.total)) safest = found;
      });
    });
    return {fastest, safest, grid: widths.length * budgets.length};
  }

  // Searching does not depend on the gate being scored, so it must not rerun
  // every time a threshold box is touched.
  const gateSearchCache = new Map();
  function bestGates(pairs, signature) {
    if (!gateSearchCache.has(signature)) {
      gateSearchCache.set(signature, searchGates(pairs));
    }
    return gateSearchCache.get(signature);
  }

  function policyRow(label, pairs, gate, current, late) {
    const outcome = gateOutcome(pairs, gate);
    const cells = [
      label,
      `${outcome.selected.toLocaleString("en-GB")} of ${outcome.n.toLocaleString("en-GB")}`,
      msText(outcome.policyNs),
      speedupText(outcome.neverNs, outcome.policyNs),
      outcome.regret === null ? null : `${outcome.regret.toFixed(3)}x`,
      outcome.falsePositive.toLocaleString("en-GB"),
      outcome.worstRegression > 1.0005 ? `${outcome.worstRegression.toFixed(2)}x` : "none",
    ];
    if (current) return {cells, className: "pf-row-current"};
    return late ? {cells, className: "pf-row-late"} : cells;
  }

  // The boxes and the searched optima are two ways of setting the same pair of
  // numbers, so adopting one has to be visible in the other.
  function syncGateInputs() {
    const cost = el("pf-max-cost");
    if (cost) cost.value = String(ui.maxCost);
    const admitted = el("pf-max-rows");
    if (admitted) {
      admitted.value = String(Number((ui.maxAdmitted * 100).toPrecision(4)));
    }
    const note = el("pf-policy-note");
    if (note) {
      const shippedCost = ANALYSIS.max_simd_comparisons || 16;
      const shippedRows = ANALYSIS.max_candidate_row_fraction || 0.1;
      const isShipped = ui.maxCost === shippedCost
        && Math.abs(ui.maxAdmitted - shippedRows) < 1e-12;
      note.textContent = isShipped
        ? `= the shipped rule: SIMD cost <= ${shippedCost} `
          + `and covered codes ÷ rows < ${percentText(shippedRows)}`
        : `edited — the shipped rule is SIMD cost <= ${shippedCost}, `
          + `covered codes ÷ rows < ${percentText(shippedRows)}`;
      note.classList.toggle("is-edited", !isShipped);
    }
  }

  const POLICY_HEADERS = ["policy", "prefiltered", "suite total", "end to end",
                          "regret vs oracle", "wrongly admitted", "worst regression"];

  function renderVerdict(pairs, baseline, subject, gate, signature) {
    const target = el("pf-verdict-body");
    const svg = el("pf-verdict-plot");
    if (!target || !svg) return;
    target.innerHTML = "";
    svg.innerHTML = "";
    if (!baseline || !subject) {
      target.appendChild(text("p",
        "Pick a subject and a baseline series to compare.", "pf-note"));
      return;
    }
    if (!pairs.length) {
      target.appendChild(text("p",
        "No query is measured by both series in this selection.", "pf-note"));
      return;
    }

    const stats = ratioStats(pairs);
    const t = coverage(pairs, gate);
    const outcome = gateOutcome(pairs, gate);
    if (!t.n || !t.baseline) {
      target.appendChild(text("p", "No paired timings to total up.", "pf-note"));
      return;
    }

    // ---- 1. the distribution, which is what a "median speedup" claim rests on
    target.appendChild(text("h4", "Per query, against the baseline", "pf-subhead"));
    const ratios = pairs.map(pair => pair.ratio).sort((a, b) => a - b);
    const at = fraction => quantileOf(ratios, fraction);
    const QUANTILES = [0.01, 0.05, 0.10, 0.25, 0.50, 0.75, 0.90, 0.95, 0.99];
    target.appendChild(table(
      ["queries", "faster", "worst"]
        .concat(QUANTILES.map(q => `p${(q * 100).toFixed(0)}`))
        .concat(["best"]),
      [[
        stats.n.toLocaleString("en-GB"),
        percentText(stats.winRate),
        `${stats.min.toFixed(2)}x`,
      ].concat(QUANTILES.map(q => `${at(q).toFixed(2)}x`))
       .concat([`${stats.max.toFixed(2)}x`])]));
    target.appendChild(text("p",
      `Unweighted, so every query counts the same however long it takes. `
      + `Weighing them by time instead gives ${speedupText(stats.totalBase, stats.totalSelf)} `
      + `across the whole suite with the prefilter always on -- the gap between those two `
      + `numbers is how much the cheap queries flatter the median.`, "pf-note"));

    // ---- 2. what share of the work the policy can even reach
    const timeShare = t.admittedBaseline / t.baseline;
    const ceiling = t.skippedBaseline > 0 ? t.baseline / t.skippedBaseline : null;
    target.appendChild(text("h4", "Where the baseline's time goes", "pf-subhead"));
    target.appendChild(table(
      ["", "queries", "share of baseline time", "baseline cost", "cost under this gate"],
      [
        ["prefiltered", t.admitted.toLocaleString("en-GB"), percentText(timeShare),
         msText(t.admittedBaseline), msText(t.admittedSubject)],
        ["declined", (t.n - t.admitted).toLocaleString("en-GB"), percentText(1 - timeShare),
         msText(t.skippedBaseline), msText(t.skippedBaseline)],
      ], "pf-table-tight"));
    target.appendChild(text("p",
      `This gate reaches ${percentText(timeShare)} of the baseline's time and leaves the `
      + `rest at baseline price, so no gate declining the same queries can beat `
      + `${ceiling ? `${ceiling.toFixed(2)}x` : "any bound"} however fast the kernel gets. `
      + `It achieves ${speedupText(outcome.neverNs, outcome.policyNs)}, which is `
      + `${percentText((outcome.neverNs - outcome.policyNs)
                       / Math.max(1e-9, outcome.neverNs - outcome.oracleNs))} `
      + `of what a per-query oracle would win.`
      + (t.lost
          ? ` ${t.lost.toLocaleString("en-GB")} admitted queries came out slower, costing `
            + `${msText(t.lostExtra)} (${percentText(t.lostExtra / t.baseline)} of the suite).`
          : " No admitted query came out slower."), "pf-note"));

    // ---- every rule someone might reasonably reach for, scored the same way
    target.appendChild(text("h4", "What a different policy would cost", "pf-subhead"));
    const best = bestGates(pairs, signature);
    const gateLabel = (width, budget) =>
      `SIMD cost ${Number.isFinite(width) ? formatSignificant(width) : "any"}, `
      + `rows ${Number.isFinite(budget) ? percentText(budget) : "any"}`;

    // `admits` rules read only the query row, so a rule that needs a number the
    // scan produces is marked rather than hidden: seeing what it would have
    // been worth is the point, and so is knowing it cannot be had.
    const below = (key, limit) => row => finite(row[key]) ? row[key] < limit : null;
    const policies = [
      {label: "never prefilter (baseline only)", gate: {admits: () => false}},
      {label: "always prefilter", gate: {admits: () => true}},
      {label: "selectivity < 0.1%", gate: {admits: below("selectivity", 0.001)}, late: true},
      {label: "selectivity < 1%", gate: {admits: below("selectivity", 0.01)}, late: true},
      {label: "selectivity < 5%", gate: {admits: below("selectivity", 0.05)}, late: true},
      {label: `this gate (${gateLabel(gate.maxCost, gate.maxAdmitted)})`,
       gate, current: true},
    ];
    if (best && best.fastest) {
      policies.push({
        label: `fastest gate here (${gateLabel(best.fastest.width, best.fastest.budget)})`,
        gate: {maxCost: best.fastest.width, maxAdmitted: best.fastest.budget}});
    }
    if (best && best.safest) {
      policies.push({
        label: `safest gate here (${gateLabel(best.safest.width, best.safest.budget)})`,
        gate: {maxCost: best.safest.width, maxAdmitted: best.safest.budget}});
    }
    policies.push({label: "oracle — knows every runtime in advance", oracle: true});

    // A rule the run cannot score -- because the field it reads is not recorded
    // -- would otherwise appear as "0 selected, 1.00x", which reads as a useless
    // policy rather than an unmeasured one. Drop it and say so.
    const skipped = [];
    const kept = policies.filter(entry => {
      if (entry.oracle || !entry.gate.admits) return true;
      const decided = pairs.some(pair => gateAdmits(pair.row, entry.gate) !== null);
      if (!decided) skipped.push(entry.label);
      return decided;
    });

    target.appendChild(table(POLICY_HEADERS, kept.map(entry => {
      if (entry.oracle) {
        return [entry.label, `${outcome.n.toLocaleString("en-GB")} decided per query`,
                msText(outcome.oracleNs),
                speedupText(outcome.neverNs, outcome.oracleNs), "1.000x", "0", "none"];
      }
      return policyRow(entry.label, pairs, entry.gate, entry.current, entry.late);
    })));
    if (skipped.length) {
      target.appendChild(text("p",
        `Not scored, because this run does not record what they read: `
        + `${skipped.join("; ")}.`, "pf-note"));
    }

    target.appendChild(text("p",
      "Regret is total time under the policy over total time under the oracle: 1.000x cannot "
      + "be beaten. Wrongly admitted counts queries the policy took that the baseline would "
      + "have won. The selectivity rules are scored for comparison only: selectivity is a "
      + "result of the scan, so no planner can consult it to decide whether to run one."
      + (best ? ` The two searched rows are the best of ${best.grid.toLocaleString("en-GB")} `
          + `cuts this selection can express, fitted to it, so they are a ceiling rather than `
          + `constants to ship.` : ""), "pf-note"));
    if (best) {
      ui.best = best;
      [["pf-gate-best", best.fastest], ["pf-gate-safe", best.safest]].forEach(([id, found]) => {
        const button = el(id);
        if (button) button.disabled = !found;
      });
    }

    renderVerdictBars(svg, t, outcome, baseline);
  }

  // Where the baseline's time goes, and what each policy pays for the same work.
  function renderVerdictBars(svg, t, outcome, baseline) {
    const width = 1080;
    const pad = {top: 26, right: 190, bottom: 34, left: 200};
    const bars = [
      {label: baseline.meta.label, parts: [
        {value: t.wonBaseline, fill: "#7fb2d6", name: "prefiltered, faster"},
        {value: t.lostBaseline, fill: "#e0a08c", name: "prefiltered, slower"},
        {value: t.skippedBaseline, fill: "#ccd6dd", name: "declined"},
      ]},
      {label: "this gate", parts: [
        {value: t.admittedSubject, fill: "#2f7fb5", name: "prefilter scan + verify"},
        {value: t.skippedBaseline, fill: "#ccd6dd", name: "declined, at baseline price"},
      ]},
      {label: "prefilter everything", parts: [
        {value: outcome.alwaysNs, fill: "#2f7fb5", name: "prefilter scan + verify"},
      ]},
      {label: "oracle", parts: [
        {value: outcome.oracleNs, fill: "#3f9e6a", name: "best of the two, per query"},
      ]},
    ];
    const barHeight = 26;
    const gap = 14;
    const height = pad.top + bars.length * (barHeight + gap) + pad.bottom;
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
    const longest = Math.max(...bars.map(bar =>
      bar.parts.reduce((sum, part) => sum + part.value, 0)));
    const scale = (width - pad.left - pad.right) / (longest || 1);

    bars.forEach((bar, index) => {
      const y = pad.top + index * (barHeight + gap);
      svg.appendChild(svgElement("text", {
        x: pad.left - 12, y: y + barHeight / 2 + 4, "text-anchor": "end",
        class: "pf-axis-text",
      }, bar.label));
      let x = pad.left;
      bar.parts.forEach(part => {
        const w = Math.max(0, part.value * scale);
        if (w <= 0) return;
        const rect = svgElement("rect", {
          x, y, width: w, height: barHeight, fill: part.fill, rx: 2,
        });
        attachTooltip(rect, [`${bar.label} — ${part.name}`, msText(part.value),
                             `${percentText(part.value / t.baseline)} of baseline time`]);
        svg.appendChild(rect);
        x += w;
      });
      const total = bar.parts.reduce((sum, part) => sum + part.value, 0);
      svg.appendChild(svgElement("text", {
        x: pad.left + total * scale + 10, y: y + barHeight / 2 + 4, class: "pf-axis-text",
      }, `${msText(total)}   ${speedupText(t.baseline, total)}`));
    });

    [["prefiltered, faster", "#7fb2d6"], ["prefiltered, slower", "#e0a08c"],
     ["declined", "#ccd6dd"], ["prefilter cost", "#2f7fb5"], ["oracle", "#3f9e6a"]]
      .forEach(([label, fill], index) => {
        const x = pad.left + index * 172;
        const y = height - 12;
        svg.appendChild(svgElement("rect", {x, y: y - 9, width: 11, height: 11, fill, rx: 2}));
        svg.appendChild(svgElement("text", {x: x + 17, y, class: "pf-axis-text"}, label));
      });
  }

  // ------------------------------------------- panel: what predicts throughput

  // Known before the scan means it can inform the decision; known only after
  // means it can explain a result but never drive one. The distinction is the
  // whole point of the panel, so it is a property of the predictor.
  // `pairWith` keeps a quantity next to the one it should be read against: the
  // policy's pre-scan estimate is only interesting beside what actually
  // happened, and ranking them apart hides exactly that comparison.
  // `pairWith` keeps a quantity next to the one it should be read against: the
  // policy's pre-scan estimate is only interesting beside what actually
  // happened. `of` is for quantities the rows do not carry.
  const PREDICTORS = [
    // The two clauses on one scale, so that margin < 1 is the rule itself. The
    // cost clause admits at the cap while the row clause excludes at the
    // budget, and SIMD cost is a whole number of comparisons, so dividing by
    // one past the cap makes both strict and the equivalence exact.
    // Scored but not plotted: one axis cannot say which of the two clauses put a
    // query where it is, and the frontier already shows that with the clauses
    // on separate axes. See `plottable` below.
    {key: "policy_margin", label: "the policy itself", plot: false,
     of: row => {
       if (!finite(row.comparison_cost)) return null;
       const scan = row.comparison_cost / (Math.floor(ui.maxCost) + 1);
       const verify = finite(row.admitted_rows) ? row.admitted_rows / ui.maxAdmitted : 0;
       return Math.max(scan, verify);
     }},
    {key: "comparison_cost", label: "SIMD cost (points + 2·ranges)"},
    {key: "admitted_rows", label: "covered codes ÷ rows", pairWith: "selectivity"},
    {key: "selectivity", label: "rows actually matching"},
    {key: "covered_fraction", label: "covered codes ÷ all codes"},
    {key: "needle_len", label: "needle length"},
    {key: "candidate_rows", label: "candidate rows"},
  ];

  function valueOf(predictor, row) {
    const value = predictor.of ? predictor.of(row) : row[predictor.key];
    return finite(value) ? value : null;
  }

  function logsOf(rows, key) {
    return rows.map(row => Math.log(Math.max(row[key], 1e-12)));
  }

  function renderCorrelation(rows) {
    const target = el("pf-correlation-body");
    const svg = el("pf-correlation-plot");
    if (!target || !svg) return;
    target.innerHTML = "";
    svg.innerHTML = "";

    // Throughput spans decades, and so do most of the predictors, so the fit is
    // on logs: R^2 here is the share of the spread in log throughput explained.
    const usable = rows.filter(row => finite(row.gbps) && row.gbps > 0);
    if (usable.length < 8) {
      target.appendChild(text("p",
        "Not enough measurements in this selection to say what explains anything.",
        "pf-note"));
      return;
    }
    const available = PREDICTORS.filter(predictor =>
      usable.filter(row => valueOf(predictor, row) !== null).length >= 8);
    if (!available.length) {
      target.appendChild(text("p",
        "No predictor is reported on these measurements.", "pf-note"));
      return;
    }

    // How well each quantity tracks throughput, two ways: one that assumes a
    // shape and one that does not.
    const scored = available.map(predictor => {
      const own = usable.filter(row => valueOf(predictor, row) !== null);
      const values = own.map(row => valueOf(predictor, row));
      const logs = values.map(value => Math.log(Math.max(value, 1e-12)));
      return {
        predictor,
        n: own.length,
        rank: spearman(values, own.map(row => row.gbps)),
        fit: rsquared([logs], logsOf(own, "gbps")),
      };
    });

    // Strongest association first, so the ranking is the answer. A paired
    // quantity follows its partner whatever it scored, because the estimate is
    // only meaningful read against what actually happened.
    const byKey = new Map(scored.map(entry => [entry.predictor.key, entry]));
    const partnered = new Set(scored
      .map(entry => entry.predictor.pairWith)
      .filter(key => byKey.has(key)));
    const ordered = [];
    scored
      .filter(entry => !partnered.has(entry.predictor.key))
      .sort((a, b) => Math.abs(b.rank || 0) - Math.abs(a.rank || 0))
      .forEach(entry => {
        ordered.push(entry);
        const partner = byKey.get(entry.predictor.pairWith);
        if (partner) ordered.push(partner);
      });

    target.appendChild(table(
      ["quantity", "correlation with throughput", "R²"],
      ordered.map(entry => [
        entry.predictor.label,
        finite(entry.rank) ? entry.rank.toFixed(3) : null,
        finite(entry.fit) ? `${(entry.fit * 100).toFixed(0)}%` : null,
      ])));

    target.appendChild(text("p",
      "Correlation is Spearman's, on ranks, so it assumes no shape; R² is a log-log fit and "
      + "assumes a power law. They disagree where the relationship is monotone but bent.",
      "pf-note"));
    target.appendChild(text("p",
      "The policy row puts its two clauses on one scale, normalised so that it admits a "
      + "query exactly when the value is below 1; it is scored but not plotted, since one "
      + "axis cannot show which clause bound a given query -- the frontier does that. "
      + "Covered codes ÷ rows is the share of rows the scan will hand to exact "
      + "verification, known before it runs; the library calls it "
      + "expected_candidate_row_fraction.", "pf-note"));

    // The plot: the chosen predictor against throughput, so the number in the
    // table can be looked at rather than trusted.
    const plottable = available.filter(predictor => predictor.plot !== false);
    if (!plottable.length) return;
    const select = el("pf-predictor");
    let chosen = plottable[0].key;
    if (select) {
      const signature = plottable.map(p => p.key).join("\u001f");
      if (select.dataset.signature !== signature) {
        select.dataset.signature = signature;
        select.innerHTML = "";
        plottable.forEach(predictor => {
          const option = document.createElement("option");
          option.value = predictor.key;
          option.textContent = predictor.label;
          select.appendChild(option);
        });
      }
      if (!plottable.some(p => p.key === ui.predictor)) {
        const best = ordered.find(entry => entry.predictor.plot !== false);
        ui.predictor = best ? best.predictor.key : plottable[0].key;
      }
      chosen = ui.predictor;
      if (select.value !== chosen) select.value = chosen;
    }

    const meta = PREDICTORS.find(p => p.key === chosen) || available[0];
    const points = usable.filter(row => valueOf(meta, row) !== null);
    const plotWidth = 1080;
    const plotHeight = 380;
    const pad = {top: 20, right: 30, bottom: 52, left: 78};
    svg.setAttribute("viewBox", `0 0 ${plotWidth} ${plotHeight}`);
    if (!points.length) return;

    const xScale = makeScale(points.map(row => valueOf(meta, row)), ui.corrXScale,
                             pad.left, plotWidth - pad.right);
    const yScale = makeScale(points.map(row => row.gbps), ui.corrYScale,
                             plotHeight - pad.bottom, pad.top);
    const grid = svgElement("g", {class: "pf-grid-lines"});
    xScale.ticks.forEach(tick => {
      const x = xScale.map(tick);
      grid.appendChild(svgElement("line", {
        x1: x, x2: x, y1: pad.top, y2: plotHeight - pad.bottom}));
      grid.appendChild(svgElement("text", {
        x, y: plotHeight - pad.bottom + 19, "text-anchor": "middle", class: "pf-axis-text",
      }, ["covered_fraction", "admitted_rows", "selectivity"].includes(meta.key)
           ? percentText(tick) : formatSignificant(tick)));
    });
    yScale.ticks.forEach(tick => {
      const y = yScale.map(tick);
      grid.appendChild(svgElement("line", {
        x1: pad.left, x2: plotWidth - pad.right, y1: y, y2: y}));
      grid.appendChild(svgElement("text", {
        x: pad.left - 10, y: y + 4, "text-anchor": "end", class: "pf-axis-text",
      }, formatSignificant(tick)));
    });
    svg.appendChild(grid);

    const marks = svgElement("g", {});
    const stride = Math.max(1, Math.ceil(points.length / 3000));
    points.filter((_row, index) => index % stride === 0).forEach(row => {
      const node = svgElement("circle", {
        cx: xScale.map(valueOf(meta, row)), cy: yScale.map(row.gbps), r: 2.2,
        fill: "#2f7fb5", opacity: 0.22,
      });
      attachTooltip(node, [
        row.query_id,
        `${meta.label}: ${formatSignificant(valueOf(meta, row))}`,
        `${formatSignificant(row.gbps)} GB/s`,
      ]);
      marks.appendChild(node);
    });
    svg.appendChild(marks);

    svg.appendChild(svgElement("text", {
      x: (pad.left + plotWidth - pad.right) / 2, y: plotHeight - 12,
      "text-anchor": "middle", class: "pf-axis-title",
    }, meta.label));
    svg.appendChild(svgElement("text", {
      x: 18, y: (pad.top + plotHeight - pad.bottom) / 2, "text-anchor": "middle",
      class: "pf-axis-title",
      transform: `rotate(-90 18 ${(pad.top + plotHeight - pad.bottom) / 2})`,
    }, "throughput (GB/s, raw payload)"));

    if (stride > 1) {
      target.appendChild(text("p",
        `Plot shows every ${stride}th of ${points.length.toLocaleString("en-GB")} `
        + `measurements; the table uses all of them.`, "pf-note"));
    }
  }

  function renderFrontier(subject, baseline, baselineTimes) {
    const svg = el("pf-frontier-plot");
    if (!svg) return;
    svg.innerHTML = "";
    if (!baseline || !subject) return;
    const points = pairedRatios(subject.rows, baselineTimes).filter(pair =>
      finite(pair.row.comparison_cost) && finite(pair.row.admitted_rows));
    const status = el("pf-frontier-status");
    if (!points.length) {
      if (status) status.textContent = "No queries with both cover facts in this selection.";
      return;
    }
    const width = 1080;
    const height = 460;
    const pad = {top: 18, right: 132, bottom: 52, left: 74};
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

    // Zero admitted rows is meaningful (an empty cover) and cannot sit on a log
    // axis, so it is floored into the bottom decade the way the main plot
    // handles zero selectivity.
    const costs = points.map(p => p.row.comparison_cost);
    const admits = points.map(p => Math.max(p.row.admitted_rows, 0));
    const xScale = makeScale(costs, ui.xScale, pad.left, width - pad.right);
    const yScale = makeScale(admits, ui.yScale, height - pad.bottom, pad.top);

    const grid = svgElement("g", {class: "pf-grid-lines"});
    xScale.ticks.forEach(tick => {
      const x = xScale.map(tick);
      grid.appendChild(svgElement("line", {x1: x, x2: x, y1: pad.top, y2: height - pad.bottom}));
      grid.appendChild(svgElement("text", {
        x, y: height - pad.bottom + 20, "text-anchor": "middle", class: "pf-axis-text",
      }, formatSignificant(tick)));
    });
    yScale.ticks.forEach(tick => {
      const y = yScale.map(tick);
      grid.appendChild(svgElement("line", {x1: pad.left, x2: width - pad.right, y1: y, y2: y}));
      grid.appendChild(svgElement("text", {
        x: pad.left - 10, y: y + 4, "text-anchor": "end", class: "pf-axis-text",
      }, tick === 0 ? "0" : percentText(tick)));
    });
    svg.appendChild(grid);

    // Diverging by outcome, not by magnitude: the only threshold that matters
    // on this plot is 1.0, so the colour has to change there and nowhere else.
    const color = ratio => {
      if (ratio >= 1) {
        const strength = Math.min(1, Math.log10(ratio) / Math.log10(25));
        return `rgb(${Math.round(214 - 190 * strength)},${Math.round(232 - 90 * strength)},${Math.round(244 - 80 * strength)})`;
      }
      const strength = Math.min(1, Math.log10(1 / ratio) / Math.log10(25));
      return `rgb(${Math.round(250 - 30 * strength)},${Math.round(224 - 150 * strength)},${Math.round(210 - 150 * strength)})`;
    };

    const marks = svgElement("g", {});
    points.forEach(point => {
      const node = svgElement("circle", {
        cx: xScale.map(point.row.comparison_cost),
        cy: yScale.map(Math.max(point.row.admitted_rows, 0)),
        r: point.ratio >= 1 ? 3.2 : 4.2,
        fill: color(point.ratio),
        stroke: point.ratio >= 1 ? "rgba(20,40,60,0.25)" : "rgba(120,20,10,0.55)",
        "stroke-width": point.ratio >= 1 ? 0.6 : 1.1,
      });
      attachTooltip(node, [
        point.row.query_id,
        `needle ${point.row.needle_len ?? "?"} B · cover ${point.row.cover_points ?? "?"}P `
        + `${point.row.cover_ranges ?? "?"}R · cost ${point.row.comparison_cost}`,
        `covered codes ÷ rows ${percentText(point.row.admitted_rows)} · selectivity `
        + `${percentText(point.row.selectivity ?? 0)}`,
        `${ratioText(point.ratio)} ${point.ratio >= 1 ? "faster" : "slower"} than baseline`,
      ]);
      marks.appendChild(node);
    });
    svg.appendChild(marks);

    // The gate, drawn where it actually cuts -- and draggable there, because
    // the useful question is "what if the cut were here" and answering it by
    // typing numbers into a box is a poor substitute for moving the line.
    const gateX = xScale.map(ui.maxCost);
    const gateY = yScale.map(ui.maxAdmitted);
    const boundary = svgElement("g", {class: "pf-boundary"});
    boundary.appendChild(svgElement("line", {
      x1: gateX, x2: gateX, y1: pad.top, y2: height - pad.bottom,
    }));
    boundary.appendChild(svgElement("line", {
      x1: pad.left, x2: gateX, y1: gateY, y2: gateY,
    }));
    boundary.appendChild(svgElement("text", {
      x: gateX + 6, y: pad.top + 14, class: "pf-boundary-text",
    }, `SIMD cost ${formatSignificant(ui.maxCost)}`));
    boundary.appendChild(svgElement("text", {
      x: pad.left + 6, y: gateY - 6, class: "pf-boundary-text",
    }, `${percentText(ui.maxAdmitted)} of rows`));
    svg.appendChild(boundary);

    svg.appendChild(svgElement("text", {
      x: (pad.left + width - pad.right) / 2, y: height - 10,
      "text-anchor": "middle", class: "pf-axis-title",
    }, "SIMD cost (points + 2·ranges)"));
    svg.appendChild(svgElement("text", {
      x: 18, y: (pad.top + height - pad.bottom) / 2,
      "text-anchor": "middle", class: "pf-axis-title",
      transform: `rotate(-90 18 ${(pad.top + height - pad.bottom) / 2})`,
    }, "covered codes ÷ rows"));

    const legend = svgElement("g", {});
    [["faster than baseline", color(8)], ["about even", color(1)],
     ["slower than baseline", color(0.2)]].forEach(([label, fill], index) => {
      const y = pad.top + 12 + index * 22;
      legend.appendChild(svgElement("circle", {
        cx: width - pad.right + 18, cy: y - 4, r: 5, fill,
        stroke: "rgba(20,40,60,0.35)", "stroke-width": 0.8,
      }));
      legend.appendChild(svgElement("text", {
        x: width - pad.right + 30, y, class: "pf-axis-text",
      }, label));
    });
    svg.appendChild(legend);

    const inside = points.filter(p =>
      p.row.comparison_cost <= ui.maxCost && p.row.admitted_rows < ui.maxAdmitted);
    const losers = inside.filter(p => p.ratio <= 1);
    if (status) {
      status.textContent =
        `${points.length.toLocaleString("en-GB")} queries; `
        + `${inside.length.toLocaleString("en-GB")} inside the gate, `
        + `${losers.length} of those slower than baseline. `
        + `Bottom-left is cheap to scan and cheap to verify; the top-right corner is `
        + `where prefiltering cannot pay however fast the kernel is.`;
    }
  }

  // ------------------------------------------------------------------ wiring

  function selectorOptions(select, values, selected, label) {
    const signature = values.join("\u001f");
    if (select.dataset.signature !== signature) {
      select.dataset.signature = signature;
      select.innerHTML = "";
      values.forEach(value => {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = label ? label(value) : value;
        select.appendChild(option);
      });
    }
    if (select.value !== selected) select.value = selected;
  }

  // A cover is only compiled for substring search, so an operation that never
  // reaches the prefilter has nothing for this section to say. Rather than
  // hardcode which those are, ask the run: an operation qualifies when some row
  // under it reports cover facts.
  function prefilterOps() {
    const ops = new Set();
    DATA.forEach(row => {
      if (row.source === state.source && row.dataset === state.dataset
          && row.chunk_rows === state.chunk && finite(row.comparison_cost)) {
        ops.add(row.op);
      }
    });
    return [...ops].sort();
  }

  function activeOp(ops) {
    if (!ops.length) return null;
    if (ui.op && ops.includes(ui.op)) return ui.op;
    return ops.includes("contains") ? "contains" : ops[0];
  }

  // Deliberately not the throughput plot's `filteredRows`: that also applies
  // the plot's Focus X range, which is a viewing aid there and would be a
  // silent sample cut here.
  function scopedRows(op) {
    if (!op) return [];
    return DATA.filter(row => row.source === state.source
      && row.dataset === state.dataset
      && row.chunk_rows === state.chunk
      && row.op === op);
  }

  function syncSelection() {
    const sources = helpers.sourceValues();
    const sourceField = el("pf-source-field");
    if (sourceField) sourceField.hidden = sources.length < 2;
    const sourceSelect = el("pf-source");
    if (sourceSelect) selectorOptions(sourceSelect, sources, state.source || "");
    const datasetSelect = el("pf-dataset");
    if (datasetSelect) {
      selectorOptions(datasetSelect, helpers.datasetValues(), state.dataset || "",
        helpers.datasetLabel);
    }
  }

  function render() {
    const panel = el("panel-prefilter");
    if (!panel || panel.hidden) return;
    const ops = prefilterOps();
    ui.op = activeOp(ops);
    syncSelection();
    const rows = scopedRows(ui.op);
    const groups = allSeries(rows);
    const {baseline, subject} = syncSeriesOptions(groups);
    const baselineTimes = baselineMap(baseline);
    const subjectRows = subject ? subject.rows : [];
    const pairs = pairedRatios(subjectRows, baselineTimes);
    // A row only carries cover facts if it went through a prefilter scan, so
    // this is what tells the column panel whether it has a subject at all.
    const prefilterRows = subjectRows.filter(row => finite(row.comparison_cost));
    syncGateInputs();
    renderColumns(prefilterRows);
    const signature = [state.source, state.dataset, ui.op, state.chunk,
                       ui.subject, ui.baseline, pairs.length].join("\u001f");
    renderVerdict(pairs, baseline, subject,
                  {maxCost: ui.maxCost, maxAdmitted: ui.maxAdmitted}, signature);
    renderCorrelation(prefilterRows.length ? prefilterRows : subjectRows);
    renderFrontier(subject, baseline, baselineTimes);
    helpers.bindExportButtons(document);
  }

  function bindControls() {
    const sourceSelect = el("pf-source");
    if (sourceSelect) {
      sourceSelect.addEventListener("change", () => {
        // Changing the run can change which datasets exist, so the shared
        // selection is moved through the host rather than patched here.
        helpers.setSelection({source: sourceSelect.value, dataset: null});
      });
    }
    const datasetSelect = el("pf-dataset");
    if (datasetSelect) {
      datasetSelect.addEventListener("change", () => {
        helpers.setSelection({dataset: datasetSelect.value});
      });
    }
    const baselineSelect = el("pf-baseline");
    if (baselineSelect) {
      baselineSelect.addEventListener("change", () => {
        ui.baseline = baselineSelect.value;
        render();
      });
    }
    const subjectSelect = el("pf-subject");
    if (subjectSelect) {
      subjectSelect.addEventListener("change", () => {
        ui.subject = subjectSelect.value;
        render();
      });
    }
    const cost = el("pf-max-cost");
    if (cost) {
      cost.value = String(ui.maxCost);
      cost.addEventListener("input", () => {
        const value = Number(cost.value);
        if (Number.isFinite(value) && value > 0) {
          ui.maxCost = value;
          render();
        }
      });
    }
    const admitted = el("pf-max-rows");
    if (admitted) {
      admitted.value = String(ui.maxAdmitted * 100);
      admitted.addEventListener("input", () => {
        const value = Number(admitted.value);
        if (Number.isFinite(value) && value > 0) {
          ui.maxAdmitted = value / 100;
          render();
        }
      });
    }
    const reset = el("pf-gate-reset");
    if (reset) {
      reset.addEventListener("click", () => {
        ui.maxCost = ANALYSIS.max_simd_comparisons || 16;
        ui.maxAdmitted = ANALYSIS.max_candidate_row_fraction || 0.1;
        syncGateInputs();
        render();
      });
    }
    const adopt = found => {
      if (!found) return;
      // A searched cut can be Infinity, which no number box can hold; the
      // widest value present is the same policy and is representable.
      ui.maxCost = Number.isFinite(found.width) ? found.width : 1e9;
      ui.maxAdmitted = Number.isFinite(found.budget) ? found.budget : 1;
      syncGateInputs();
      render();
    };
    const bestButton = el("pf-gate-best");
    if (bestButton) {
      bestButton.addEventListener("click", () => adopt(ui.best && ui.best.fastest));
    }
    const safeButton = el("pf-gate-safe");
    if (safeButton) {
      safeButton.addEventListener("click", () => adopt(ui.best && ui.best.safest));
    }
    const predictor = el("pf-predictor");
    if (predictor) {
      predictor.addEventListener("change", () => {
        ui.predictor = predictor.value;
        render();
      });
    }
    document.querySelectorAll("[data-pf-axis][data-scale]").forEach(button => {
      button.addEventListener("click", () => {
        ui[`${button.dataset.pfAxis}Scale`] = button.dataset.scale;
        updateScaleButtons();
        render();
      });
    });
    updateScaleButtons();
  }

  function updateScaleButtons() {
    document.querySelectorAll("[data-pf-axis][data-scale]").forEach(button => {
      button.setAttribute("aria-pressed",
        String(ui[`${button.dataset.pfAxis}Scale`] === button.dataset.scale));
    });
  }

  bindControls();
  host.onRender.push(render);
  render();
})();
