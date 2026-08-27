(() => {
  "use strict";

  const DATA = JSON.parse(document.getElementById("bench-viz-data").textContent);
  const DEFAULTS = JSON.parse(document.getElementById("bench-viz-defaults").textContent);
  // Column shape, the fitted cost model, and the run's own noise floor, all
  // computed by the builder so the statistics are unit tested in Python rather
  // than only here. Absent on viewers built before the prefilter section.
  const ANALYSIS = (() => {
    const node = document.getElementById("bench-viz-analysis");
    if (!node) return {};
    try {
      return JSON.parse(node.textContent) || {};
    } catch (error) {
      return {};
    }
  })();
  const MINCUT_CATALOG = (() => {
    const node = document.getElementById("bench-viz-mincut-graphs");
    if (!node) return {};
    try {
      return JSON.parse(node.textContent) || {};
    } catch (_error) {
      return {};
    }
  })();
  const MINCUT_ARCHIVES = MINCUT_CATALOG.version === 2
    && MINCUT_CATALOG.archives && typeof MINCUT_CATALOG.archives === "object"
    ? MINCUT_CATALOG.archives : {};
  const NS = "http://www.w3.org/2000/svg";
  // Ordered for separation, not variety: consecutive entries differ in hue and
  // in lightness, so the first few selected series stay distinguishable in
  // print, on a projector, and to a red-green colour-blind reader. The old
  // palette held six near-identical blues (#1f70a8, #4267ac, #4d6398,
  // #168aad, #5c7c8a, #3b817a) and assigned them by hashing the series id, so
  // any two selected series could collide with no way for the reader to tell
  // them apart.
  const PALETTE = [
    "#0064a4", "#e08214", "#009e73", "#a05195", "#b23a48", "#0f8b8d",
    "#8c6d1f", "#d45087", "#4b6cc1", "#5c8a1f", "#a94b00", "#7b61b8",
    "#167a8c", "#c45a32", "#2d7d46", "#8f4a9d", "#b24b72", "#52616d"
  ];
  const UNPLOTTED = "#aab3bb";
  const DASHES = ["", "8 4", "2 3", "11 4 2 4", "5 3 1.5 3"];
  const OP_ORDER = ["contains", "prefix", "suffix", "multi_contains", "contains_any"];

  const refs = Object.fromEntries([
    "source-select", "dataset-select", "op-select", "op-field", "chunk-select", "x-metric",
    "y-metric", "bin-count", "focus-label", "focus-min", "focus-max", "focus-reset",
    "focus-error", "ylimit-label", "ylimit-min", "ylimit-max", "ylimit-reset",
    "source-field", "chunk-field", "show-points", "show-band", "edit-labels",
    "label-editor", "title-input", "subtitle-input", "x-label-input",
    "y-label-input", "series-count", "series-search", "series-all", "series-none",
    "series-chips", "decode-section", "decode-all", "decode-none", "decode-chips",
    "plot", "plot-status", "tooltip", "export-scale", "query-details",
    "query-details-summary", "query-details-clear", "query-details-body"
  ].map(id => [id, document.getElementById(id)]));

  const state = {
    source: null,
    dataset: null,
    op: null,
    chunk: null,
    xMetric: "selectivity",
    yMetric: "gbps",
    xScale: "log",
    yScale: "log",
    bins: 12,
    showPoints: true,
    showBand: true,
    title: DEFAULTS.title || "Benchmark Explorer 3000™",
    subtitle: DEFAULTS.subtitle || "",
    // The viewer shows one dataset, one op and one chunking at a time, so a
    // fixed subtitle is wrong in most views. An empty subtitle means "describe
    // the current selection"; typing one pins it until the field is cleared.
    subtitlePinned: Boolean(DEFAULTS.subtitle),
    xLabel: "selectivity (rows matched)",
    yLabel: "throughput (GB/s, raw payload)",
    xLabelCustom: false,
    yLabelCustom: false,
    ranges: {
      selectivity: {min: null, max: null},
      needle_len: {min: null, max: null},
    },
    // Kept per Y measure: a GB/s window means nothing once the axis is ns/row.
    yLimits: {
      gbps: {min: null, max: null},
      ns_per_row: {min: null, max: null},
    },
    visibleSeries: new Set(),
    visibleDecode: new Set(),
    selectedQueryIds: new Set(),
    catalogSignature: null,
    decodeSignature: null,
    initialPatterns: (DEFAULTS.show || []).map(value => String(value).toLowerCase()),
  };

  function unique(values) {
    return [...new Set(values)];
  }

  function finite(value) {
    return typeof value === "number" && Number.isFinite(value);
  }

  function median(values) {
    if (!values.length) return null;
    const ordered = [...values].sort((a, b) => a - b);
    const middle = Math.floor(ordered.length / 2);
    return ordered.length % 2
      ? ordered[middle]
      : (ordered[middle - 1] + ordered[middle]) / 2;
  }

  function quantile(values, fraction) {
    if (!values.length) return null;
    const ordered = [...values].sort((a, b) => a - b);
    const position = (ordered.length - 1) * fraction;
    const lower = Math.floor(position);
    const upper = Math.ceil(position);
    if (lower === upper) return ordered[lower];
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower);
  }


  let colorMap = new Map();
  let colorSignature = null;

  // Colours follow position among the *selected* series rather than a hash of
  // the id. A hash is stable across selections but says nothing about what is
  // on screen together, which is the only thing that decides whether two lines
  // can be told apart. Series not being plotted get a neutral swatch.
  const SERIES_ORDER = Array.isArray(DEFAULTS.series_order) ? DEFAULTS.series_order : [];

  // Where a series sits in the author's --label declaration order. Unnamed
  // series sort after every named one, keeping their catalog order.
  function paletteRank(item) {
    const index = SERIES_ORDER.findIndex(name =>
      item.label === name || item.label.startsWith(`${name} · `));
    return index < 0 ? SERIES_ORDER.length : index;
  }

  function syncColors(catalog) {
    const rank = (a, b) => paletteRank(a) - paletteRank(b);
    const ids = [
      ...catalog.series.filter(item => state.visibleSeries.has(item.id)).sort(rank)
        .map(item => item.id),
      ...catalog.decode.filter(item => state.visibleDecode.has(item.id)).sort(rank)
        .map(item => item.id),
    ];
    const signature = ids.join("\u001f");
    if (signature === colorSignature) return;
    colorSignature = signature;
    colorMap = new Map(ids.map((id, index) => [id, PALETTE[index % PALETTE.length]]));
  }

  function colorFor(key) {
    return colorMap.get(key) || UNPLOTTED;
  }

  function dashFor(key) {
    const index = [...colorMap.keys()].indexOf(key);
    if (index < 0) return "";
    // Colour carries the first six; beyond that the palette wraps, so a dash
    // pattern is what keeps the seventh distinct from the first.
    return DASHES[Math.floor(index / PALETTE.length) % DASHES.length];
  }

  function compactConfig(config) {
    try {
      const parsed = JSON.parse(config || "{}");
      const entries = Object.entries(parsed);
      if (!entries.length) return "";
      return entries.map(([key, value]) => `${key}=${String(value)}`).join(", ");
    } catch (_error) {
      return config && config !== "{}" ? config : "";
    }
  }

  // Everything the axis needs to know about an x measure in one place: how to
  // label it, whether it reads as a percentage, and what unit the focus box is
  // in. Adding a measure means adding a row here.
  const X_METRICS = {
    selectivity: {
      label: "selectivity (rows matched)", focusLabel: "Focus X (%)",
      factor: 100, percent: true,
    },
    needle_len: {
      label: "needle length (bytes)", focusLabel: "Focus X (bytes)",
      factor: 1, percent: false,
    },
  };

  function xMetricInfo() {
    return X_METRICS[state.xMetric] || X_METRICS.selectivity;
  }

  function candidateKey(row) {
    return `${row.candidate}\u001f${row.config}`;
  }

  function candidateLabel(row) {
    // A --label display name is already disambiguated by the builder, config
    // included where it was needed, so it is used verbatim.
    if (row.display) return row.display;
    const config = compactConfig(row.config);
    return config ? `${row.candidate} [${config}]` : row.candidate;
  }

  function seriesKey(row) {
    return [row.candidate, row.config, row.strategy, row.scanner || ""].join("\u001f");
  }

  function seriesMeta(row) {
    const scanner = row.scanner ? ` / ${row.scanner}` : "";
    // A renamed series drops the strategy/scanner suffix: the point of renaming
    // is that the new name already says what the approach is.
    return {
      id: seriesKey(row),
      candidateId: candidateKey(row),
      candidate: row.candidate,
      label: row.display
        ? row.display
        : `${candidateLabel(row)} · ${row.strategy}${scanner}`,
    };
  }

  function decodeMeta(row) {
    return {
      id: candidateKey(row),
      candidate: row.candidate,
      label: `${candidateLabel(row)} · decompression`,
    };
  }

  function sortedOps(values) {
    return [...values].sort((a, b) => {
      const ai = OP_ORDER.indexOf(a);
      const bi = OP_ORDER.indexOf(b);
      if (ai === -1 && bi === -1) return a.localeCompare(b);
      if (ai === -1) return 1;
      if (bi === -1) return -1;
      return ai - bi;
    });
  }

  function setOptions(select, values, selected, formatter = value => value) {
    select.replaceChildren();
    values.forEach(value => {
      const option = document.createElement("option");
      option.value = String(value);
      option.textContent = formatter(value);
      select.appendChild(option);
    });
    const target = values.some(value => String(value) === String(selected))
      ? String(selected)
      : (values.length ? String(values[0]) : "");
    select.value = target;
    return target;
  }

  function populateFilters(initial = false) {
    const sources = unique(DATA.map(row => row.source)).sort();
    state.source = setOptions(refs["source-select"], sources, state.source);

    const sourceRows = DATA.filter(row => row.source === state.source);
    const datasets = unique(sourceRows.map(row => row.dataset)).sort();
    state.dataset = setOptions(refs["dataset-select"], datasets, state.dataset);

    const datasetRows = sourceRows.filter(row => row.dataset === state.dataset);
    const ops = sortedOps(unique(datasetRows.map(row => row.op)));
    const desiredOp = initial && ops.includes("contains") ? "contains" : state.op;
    state.op = setOptions(refs["op-select"], ops, desiredOp);

    const opRows = datasetRows.filter(row => row.op === state.op);
    const chunks = unique(opRows.map(row => row.chunk_rows)).sort((a, b) => a - b);
    state.chunk = Number(setOptions(
      refs["chunk-select"], chunks, state.chunk,
      value => Number(value) === 0 ? "whole column" : Number(value).toLocaleString()
    ));

    // A control offering one option is not a control. Most runs measure one
    // operation over the whole column, and most builds load a single run.
    refs["op-field"].hidden = ops.length < 2;
    refs["chunk-field"].hidden = chunks.length < 2;
    refs["source-field"].hidden = sources.length < 2;
  }

  function contextRows() {
    return DATA.filter(row =>
      row.source === state.source &&
      row.dataset === state.dataset &&
      row.op === state.op &&
      row.chunk_rows === state.chunk
    );
  }

  function activeRange() {
    return state.ranges[state.xMetric];
  }

  function activeYLimits() {
    return state.yLimits[state.yMetric] || {min: null, max: null};
  }

  function filteredRows() {
    const range = activeRange();
    return contextRows().filter(row => {
      const value = xValue(row);
      if (range.min === null && range.max === null) return true;
      if (!finite(value)) return false;
      return (range.min === null || value >= range.min) &&
        (range.max === null || value <= range.max);
    });
  }

  function xValue(row) {
    return row[state.xMetric];
  }

  function yValue(row) {
    return row[state.yMetric];
  }

  function decodeValue(row) {
    return state.yMetric === "gbps" ? row.decode_gbps : row.decode_ns_per_row;
  }

  function catalogs(rows) {
    const bySeries = new Map();
    const byDecode = new Map();
    rows.forEach(row => {
      if (finite(xValue(row)) && finite(yValue(row))) {
        const meta = seriesMeta(row);
        if (!bySeries.has(meta.id)) bySeries.set(meta.id, meta);
      }
      if (finite(decodeValue(row))) {
        const meta = decodeMeta(row);
        if (!byDecode.has(meta.id)) byDecode.set(meta.id, meta);
      }
    });
    const labelSort = (a, b) => a.label.localeCompare(b.label, undefined, {numeric: true});
    return {
      series: [...bySeries.values()].sort(labelSort),
      decode: [...byDecode.values()].sort(labelSort),
    };
  }

  function initiallyVisible(items, limit) {
    if (state.initialPatterns.length) {
      const matches = items.filter(item => {
        const searchable = `${item.candidate} ${item.label}`.toLowerCase();
        return state.initialPatterns.some(pattern => searchable.includes(pattern));
      });
      if (matches.length) return matches.map(item => item.id);
    }
    return items.slice(0, limit).map(item => item.id);
  }

  function reconcileVisibility(catalog) {
    const seriesSignature = catalog.series.map(item => item.id).join("|");
    if (seriesSignature !== state.catalogSignature) {
      const available = new Set(catalog.series.map(item => item.id));
      const retained = [...state.visibleSeries].filter(id => available.has(id));
      state.visibleSeries = new Set(
        retained.length ? retained : initiallyVisible(catalog.series, 3)
      );
      state.catalogSignature = seriesSignature;
    }

    const decodeSignature = catalog.decode.map(item => item.id).join("|");
    if (decodeSignature !== state.decodeSignature) {
      const available = new Set(catalog.decode.map(item => item.id));
      const retained = [...state.visibleDecode].filter(id => available.has(id));
      state.visibleDecode = new Set(
        retained.length ? retained : initiallyVisible(catalog.decode, 0)
      );
      state.decodeSignature = decodeSignature;
    }
  }

  function makeChip(meta, visibleSet, decode = false) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `series-chip${decode ? " decode" : ""}`;
    button.dataset.id = meta.id;
    button.dataset.search = meta.label.toLowerCase();
    button.setAttribute("aria-pressed", String(visibleSet.has(meta.id)));
    button.title = decode ? "Toggle decode-only baseline" : "Toggle candidate series";

    const swatch = document.createElement("span");
    swatch.className = "chip-swatch";
    swatch.style.background = colorFor(meta.id);
    swatch.style.color = colorFor(meta.id);
    button.appendChild(swatch);

    const label = document.createElement("span");
    label.textContent = meta.label;
    button.appendChild(label);

    button.addEventListener("click", () => {
      if (visibleSet.has(meta.id)) visibleSet.delete(meta.id);
      else visibleSet.add(meta.id);
      button.setAttribute("aria-pressed", String(visibleSet.has(meta.id)));
      renderPlot();
      updateCounts();
    });
    return button;
  }

  function renderChips(catalog) {
    syncColors(catalog);
    refs["series-chips"].replaceChildren(
      ...catalog.series.map(item => makeChip(item, state.visibleSeries, false))
    );
    refs["decode-chips"].replaceChildren(
      ...catalog.decode.map(item => makeChip(item, state.visibleDecode, true))
    );
    refs["decode-section"].hidden = catalog.decode.length === 0;
    updateCounts(catalog);
    applySearch();
  }

  function updateCounts(catalog = catalogs(filteredRows())) {
    const selected = catalog.series.filter(item => state.visibleSeries.has(item.id)).length;
    refs["series-count"].textContent = `${selected} of ${catalog.series.length} selected`;
  }

  function applySearch() {
    const query = refs["series-search"].value.trim().toLowerCase();
    refs["series-chips"].querySelectorAll(".series-chip").forEach(chip => {
      chip.hidden = Boolean(query) && !chip.dataset.search.includes(query);
    });
  }

  function chooseAll(kind, visible) {
    const rows = filteredRows();
    const catalog = catalogs(rows)[kind];
    const target = kind === "series" ? state.visibleSeries : state.visibleDecode;
    target.clear();
    if (visible) catalog.forEach(item => target.add(item.id));
    renderChips(catalogs(rows));
    renderPlot();
  }

  function pointsForSeries(rows, id) {
    return rows
      .filter(row => seriesKey(row) === id && finite(xValue(row)) && finite(yValue(row)))
      .map(row => ({x: xValue(row), y: yValue(row), row}));
  }

  function binPoints(points) {
    if (!points.length) return [];
    const groups = new Map();
    if (state.bins === "exact") {
      points.forEach(point => {
        const key = String(point.x);
        if (!groups.has(key)) groups.set(key, []);
        groups.get(key).push(point);
      });
    } else {
      const count = Number(state.bins);
      const positives = points.map(point => point.x).filter(value => value > 0);
      const minPositive = positives.length ? Math.min(...positives) : 1;
      const transformed = points.map(point => {
        if (state.xScale !== "log") return point.x;
        return point.x <= 0 ? Math.log10(minPositive) - 1 : Math.log10(point.x);
      });
      const low = Math.min(...transformed);
      const high = Math.max(...transformed);
      const width = (high - low) / count;
      points.forEach((point, index) => {
        const bucket = width > 0
          ? Math.min(count - 1, Math.floor((transformed[index] - low) / width))
          : 0;
        if (!groups.has(bucket)) groups.set(bucket, []);
        groups.get(bucket).push(point);
      });
    }

    return [...groups.values()].map(group => {
      const xs = group.map(point => point.x);
      const ys = group.map(point => point.y);
      return {
        x: median(xs),
        y: median(ys),
        q1: quantile(ys, 0.25),
        q3: quantile(ys, 0.75),
        n: group.length,
        queryIds: unique(group.map(point => point.row.query_id).filter(Boolean)),
      };
    }).sort((a, b) => a.x - b.x);
  }

  function niceStep(span, target = 6) {
    if (!finite(span) || span <= 0) return 1;
    const rough = span / target;
    const power = 10 ** Math.floor(Math.log10(rough));
    const fraction = rough / power;
    const nice = fraction <= 1 ? 1 : fraction <= 2 ? 2 : fraction <= 2.5 ? 2.5 : fraction <= 5 ? 5 : 10;
    return nice * power;
  }

  function makeScale(values, kind, start, end, bounds = {min: null, max: null}) {
    let clean = values.filter(finite);
    if (!clean.length) clean = [0, 1];

    if (kind === "log") {
      const positive = clean.filter(value => value > 0);
      if (bounds.min !== null && bounds.min > 0) positive.push(bounds.min);
      if (bounds.max !== null && bounds.max > 0) positive.push(bounds.max);
      if (positive.length) {
        const minPositive = bounds.min !== null && bounds.min > 0
          ? bounds.min : Math.min(...positive);
        const maxPositive = bounds.max !== null && bounds.max > 0
          ? bounds.max : Math.max(...positive);
        const hasZero = bounds.min !== null
          ? bounds.min <= 0 : clean.some(value => value <= 0);
        const lowExponent = Math.floor(Math.log10(minPositive));
        const highExponent = Math.ceil(Math.log10(maxPositive));
        let low = hasZero ? lowExponent - 1 : Math.log10(minPositive);
        let high = Math.log10(maxPositive);
        if (high <= low) high = low + 1;
        const padding = (high - low) * 0.035;
        if (bounds.min === null) low -= padding;
        if (bounds.max === null) high += padding;
        const map = value => {
          const transformed = value <= 0 ? lowExponent - 1 : Math.log10(value);
          return start + ((transformed - low) / (high - low)) * (end - start);
        };
        const ticks = [];
        if (hasZero) ticks.push(0);
        const first = lowExponent;
        const last = Math.ceil(Math.log10(maxPositive));
        const stride = Math.max(1, Math.ceil((last - first + 1) / 7));
        for (let exponent = first; exponent <= last; exponent += stride) {
          ticks.push(10 ** exponent);
        }
        if (!ticks.some(value => value === 10 ** last)) ticks.push(10 ** last);
        if (bounds.min !== null) ticks.push(bounds.min);
        if (bounds.max !== null) ticks.push(bounds.max);
        return {map, ticks: unique(ticks).sort((a, b) => a - b), kind: "log", hasZero};
      }
    }

    let low = bounds.min !== null ? bounds.min : Math.min(...clean);
    let high = bounds.max !== null ? bounds.max : Math.max(...clean);
    if (high <= low) {
      const delta = Math.abs(low) * 0.1 || 1;
      low -= delta;
      high += delta;
    }
    const step = niceStep(high - low);
    if (bounds.min === null) low = Math.floor(low / step) * step;
    if (bounds.max === null) high = Math.ceil(high / step) * step;
    if (high <= low) high = low + step;
    const ticks = [];
    const firstTick = Math.ceil(low / step - 1e-10) * step;
    for (let value = firstTick, guard = 0; value <= high + step * 1e-8 && guard < 20; value += step, guard += 1) {
      ticks.push(Math.abs(value) < step * 1e-10 ? 0 : value);
    }
    if (bounds.min !== null) ticks.push(bounds.min);
    if (bounds.max !== null) ticks.push(bounds.max);
    const map = value => start + ((value - low) / (high - low)) * (end - start);
    return {map, ticks: unique(ticks).sort((a, b) => a - b), kind: "linear", hasZero: false};
  }

  function formatSignificant(value, digits = 3) {
    if (value === 0) return "0";
    const magnitude = Math.abs(value);
    if (magnitude >= 10000 || magnitude < 0.001) return value.toExponential(1).replace("e+", "e");
    return Number(value.toPrecision(digits)).toLocaleString("en-GB", {maximumFractionDigits: 6});
  }

  function percentText(fraction) {
    const percent = fraction * 100;
    if (percent === 0) return "0%";
    if (percent < 0.001) return `${percent.toExponential(0).replace("e+", "e")}%`;
    if (percent >= 10) return `${Math.round(percent)}%`;
    return `${formatSignificant(percent, 2)}%`;
  }

  function generatedSubtitle() {
    const rows = filteredRows();
    if (!rows.length) return "";
    const queries = new Set(rows.map(row => row.query_id)).size;
    const dataset = rows[0].dataset_display || state.dataset || "unknown dataset";
    const noun = queries === 1 ? "needle" : "needles";
    let text = `${queries.toLocaleString()} ${noun} on ${dataset}`;

    const selectivities = rows.map(row => row.selectivity).filter(finite);
    if (selectivities.length) {
      const low = selectivities.reduce((a, b) => Math.min(a, b), Infinity);
      const high = selectivities.reduce((a, b) => Math.max(a, b), -Infinity);
      if (low === high) {
        // One query, or a focus range narrow enough to leave one value: "from
        // X to X" reads as a mistake.
        text += `, selectivity ${percentText(high)} of rows`;
      } else {
        const from = low <= 0 ? "zero matches" : percentText(low);
        text += `, spanning selectivity from ${from} to ${percentText(high)} of rows`;
      }
    }

    const extras = [];
    if (state.op && state.op !== "contains") extras.push(state.op);
    if (state.chunk) extras.push(`chunks of ${state.chunk.toLocaleString()} rows`);
    const range = activeRange();
    if (range && (range.min !== null || range.max !== null)) {
      extras.push(`${xMetricInfo().label} restricted`);
    }
    return extras.length ? `${text} · ${extras.join(" · ")}` : text;
  }

  function effectiveSubtitle() {
    return state.subtitlePinned ? state.subtitle : generatedSubtitle();
  }

  function formatX(value, precise = false) {
    if (xMetricInfo().percent) {
      if (value === 0) return "0";
      const percent = value * 100;
      if (precise) return `${formatSignificant(percent, 4)}%`;
      if (percent < 0.001) return `${percent.toExponential(0).replace("e+", "e")}%`;
      return `${formatSignificant(percent, 3)}%`;
    }
    return precise ? formatSignificant(value, 4) : formatSignificant(value, 3);
  }

  function formatY(value, precise = false) {
    const suffix = state.yMetric === "gbps" ? " GB/s" : " ns/row";
    return `${formatSignificant(value, precise ? 4 : 3)}${precise ? suffix : ""}`;
  }

  function rangeDisplayFactor() {
    return xMetricInfo().factor;
  }

  function updateFocusControls() {
    const range = activeRange();
    const factor = rangeDisplayFactor();
    refs["focus-label"].textContent = xMetricInfo().focusLabel;
    refs["focus-min"].value = range.min === null ? "" : String(range.min * factor);
    refs["focus-max"].value = range.max === null ? "" : String(range.max * factor);
    refs["focus-min"].min = "0";
    refs["focus-max"].min = "0";
    refs["focus-error"].textContent = "";
    refs["focus-min"].removeAttribute("aria-invalid");
    refs["focus-max"].removeAttribute("aria-invalid");
  }

  function updateYLimitControls() {
    const limits = activeYLimits();
    refs["ylimit-label"].textContent =
      state.yMetric === "gbps" ? "Clip Y (GB/s)" : "Clip Y (ns/row)";
    refs["ylimit-min"].value = limits.min === null ? "" : String(limits.min);
    refs["ylimit-max"].value = limits.max === null ? "" : String(limits.max);
  }

  function applyYLimitInputs() {
    const read = key => {
      const raw = refs[key].value.trim();
      if (raw === "") return null;
      const value = Number(raw);
      return finite(value) && value >= 0 ? value : null;
    };
    const minimum = read("ylimit-min");
    const maximum = read("ylimit-max");
    // Silently ignoring a reversed pair is better than drawing an inverted
    // axis; the X focus box reports it because it also filters the data, while
    // this only frames what is already plotted.
    if (minimum !== null && maximum !== null && minimum >= maximum) return;
    state.yLimits[state.yMetric] = {min: minimum, max: maximum};
    renderPlot();
  }

  function applyFocusInputs() {
    const factor = rangeDisplayFactor();
    const minText = refs["focus-min"].value.trim();
    const maxText = refs["focus-max"].value.trim();
    const minimum = minText === "" ? null : Number(minText) / factor;
    const maximum = maxText === "" ? null : Number(maxText) / factor;
    const invalidNumber = (minimum !== null && (!finite(minimum) || minimum < 0)) ||
      (maximum !== null && (!finite(maximum) || maximum < 0));
    const invalidOrder = minimum !== null && maximum !== null && minimum >= maximum;
    const invalid = invalidNumber || invalidOrder;

    refs["focus-min"].setAttribute("aria-invalid", String(invalid));
    refs["focus-max"].setAttribute("aria-invalid", String(invalid));
    refs["focus-error"].textContent = invalidOrder
      ? "maximum must exceed minimum"
      : invalidNumber ? "use non-negative numbers" : "";
    if (invalid) return;

    state.ranges[state.xMetric] = {min: minimum, max: maximum};
    rebuild();
  }

  function svgElement(name, attributes = {}, text = null) {
    const node = document.createElementNS(NS, name);
    Object.entries(attributes).forEach(([key, value]) => {
      if (value !== null && value !== undefined && value !== "") node.setAttribute(key, String(value));
    });
    if (text !== null) node.textContent = text;
    return node;
  }

  function pathLine(points, xScale, yScale) {
    return points.map((point, index) =>
      `${index === 0 ? "M" : "L"} ${xScale.map(point.x).toFixed(2)} ${yScale.map(point.y).toFixed(2)}`
    ).join(" ");
  }

  function attachTooltip(node, lines) {
    const show = event => {
      refs.tooltip.textContent = lines.join("\n");
      refs.tooltip.hidden = false;
      moveTooltip(event);
    };
    node.addEventListener("pointerenter", show);
    node.addEventListener("pointermove", moveTooltip);
    node.addEventListener("pointerleave", () => { refs.tooltip.hidden = true; });
  }

  function moveTooltip(event) {
    const tooltip = refs.tooltip;
    const gap = 14;
    let left = event.clientX + gap;
    let top = event.clientY + gap;
    const rect = tooltip.getBoundingClientRect();
    if (left + rect.width > window.innerWidth - 8) left = event.clientX - rect.width - gap;
    if (top + rect.height > window.innerHeight - 8) top = event.clientY - rect.height - gap;
    tooltip.style.left = `${Math.max(8, left)}px`;
    tooltip.style.top = `${Math.max(8, top)}px`;
  }

  function legendLayout(items, width) {
    const rows = [];
    let row = [];
    let used = 0;
    items.forEach(item => {
      const itemWidth = Math.min(width, 38 + item.label.length * 6.5);
      if (row.length && used + itemWidth > width) {
        rows.push(row);
        row = [];
        used = 0;
      }
      row.push({...item, itemWidth});
      used += itemWidth;
    });
    if (row.length) rows.push(row);
    return rows;
  }

  function htmlElement(name, className = "", text = null) {
    const node = document.createElement(name);
    if (className) node.className = className;
    if (text !== null) node.textContent = text;
    return node;
  }

  function queryValue(rows, key) {
    const row = rows.find(item => item[key] !== null && item[key] !== undefined);
    return row ? row[key] : null;
  }

  function countText(value) {
    return finite(value) ? Math.round(value).toLocaleString("en-GB") : "—";
  }

  function durationText(value) {
    if (!finite(value)) return "—";
    if (value >= 1e9) return `${formatSignificant(value / 1e9, 4)} s`;
    if (value >= 1e6) return `${formatSignificant(value / 1e6, 4)} ms`;
    if (value >= 1e3) return `${formatSignificant(value / 1e3, 4)} µs`;
    return `${formatSignificant(value, 4)} ns`;
  }

  function optionalPercent(value) {
    return finite(value) ? percentText(value) : "—";
  }

  function needleText(needles) {
    if (!Array.isArray(needles) || !needles.length) return "needle bytes unavailable";
    return needles.map(needle => JSON.stringify(needle.display)).join("  |  ");
  }

  function appendMetric(list, label, value, title = "") {
    const term = htmlElement("dt", "", label);
    if (title) term.title = title;
    const detail = htmlElement("dd", "", value);
    list.append(term, detail);
  }

  function snapshotForQuery(queryId, rows) {
    const queryRows = rows.filter(row => row.query_id === queryId);
    const points = queryValue(queryRows, "cover_points");
    const ranges = queryValue(queryRows, "cover_ranges");
    const selectivity = queryValue(queryRows, "selectivity");
    const admitted = queryValue(queryRows, "candidate_row_fraction");
    return {
      id: queryId,
      rows: queryRows,
      needles: queryValue(queryRows, "needles"),
      op: queryValue(queryRows, "op"),
      needleLen: queryValue(queryRows, "needle_len"),
      needleLens: queryValue(queryRows, "needle_lens"),
      selectivity,
      matches: queryValue(queryRows, "match_count"),
      rarestByte: queryValue(queryRows, "rarest_byte_freq"),
      points,
      ranges,
      comparisons: queryValue(queryRows, "comparison_cost"),
      coveredCodes: queryValue(queryRows, "covered_codes"),
      indexedCodes: queryValue(queryRows, "indexed_codes"),
      coveredFraction: queryValue(queryRows, "covered_fraction"),
      admitted,
      amplification: finite(admitted) && finite(selectivity) && selectivity > 0
        ? admitted / selectivity : null,
      profitable: queryValue(queryRows, "profitable"),
      prefilterCandidates: queryValue(queryRows, "candidate_rows"),
      pruneRate: queryValue(queryRows, "prune_rate"),
      falsePositiveRate: queryValue(queryRows, "false_positive_rate"),
      verifyPerSurvivor: queryValue(queryRows, "verify_per_survivor"),
      gateExpected: queryValue(queryRows, "gate_expected_count"),
      gateActual: queryValue(queryRows, "gate_actual_count"),
      gateHashOk: queryValue(queryRows, "gate_hash_ok"),
      meta: queryValue(queryRows, "query_meta"),
    };
  }

  function coverShapeText(snapshot) {
    if (!finite(snapshot.points) && !finite(snapshot.ranges)) return "—";
    return `${countText(snapshot.points || 0)} point${snapshot.points === 1 ? "" : "s"}`
      + ` · ${countText(snapshot.ranges || 0)} range${snapshot.ranges === 1 ? "" : "s"}`;
  }

  function renderMeasurementTable(snapshot, selectedSeries) {
    const visible = new Set(selectedSeries.map(meta => meta.id));
    const measurements = snapshot.rows.filter(row => visible.has(seriesKey(row)));
    const wrap = htmlElement("div", "query-measurements");
    const heading = htmlElement("h4", "", "Visible-series measurements");
    wrap.appendChild(heading);
    if (!measurements.length) {
      wrap.appendChild(htmlElement("p", "query-details-empty", "No selected series measured this query."));
      return wrap;
    }
    const tableWrap = htmlElement("div", "query-table-wrap");
    const table = htmlElement("table", "query-measurement-table");
    const head = htmlElement("thead");
    const headRow = htmlElement("tr");
    ["Series", "Throughput", "Latency / row", "Median", "IQR", "Samples", "Measured phases"]
      .forEach(label => headRow.appendChild(htmlElement("th", "", label)));
    head.appendChild(headRow);
    table.appendChild(head);
    const body = htmlElement("tbody");
    measurements.forEach(row => {
      const tr = htmlElement("tr");
      const label = seriesMeta(row).label;
      const phases = [
        finite(row.prefilter_ns) ? `prefilter ${durationText(row.prefilter_ns)}` : null,
        finite(row.verify_ns) ? `verify ${durationText(row.verify_ns)}` : null,
        finite(row.decode_gbps) ? `decode ${formatSignificant(row.decode_gbps, 4)} GB/s` : null,
        finite(row.scan_ns) ? `scan ${durationText(row.scan_ns)}` : null,
      ].filter(Boolean).join(" · ") || "—";
      [
        label,
        finite(row.gbps) ? `${formatSignificant(row.gbps, 4)} GB/s` : "—",
        finite(row.ns_per_row) ? `${formatSignificant(row.ns_per_row, 4)} ns` : "—",
        durationText(row.latency_ns),
        finite(row.latency_p25_ns) && finite(row.latency_p75_ns)
          ? `${durationText(row.latency_p25_ns)} – ${durationText(row.latency_p75_ns)}` : "—",
        countText(row.latency_samples),
        phases,
      ].forEach((value, index) => {
        const td = htmlElement("td", index === 0 ? "query-series-cell" : "", value);
        if (index === 0) {
          const config = compactConfig(row.config);
          td.title = [row.candidate_version, config].filter(Boolean).join(" · ");
        }
        tr.appendChild(td);
      });
      body.appendChild(tr);
    });
    table.appendChild(body);
    tableWrap.appendChild(table);
    wrap.appendChild(tableWrap);
    return wrap;
  }

  function renderQueryInspector(snapshot, selectedSeries, open) {
    const inspector = htmlElement("details", "query-inspector");
    inspector.open = open;
    const summary = htmlElement("summary");
    const title = htmlElement("span", "query-inspector-id", snapshot.id);
    const needle = htmlElement("code", "query-inspector-needle", needleText(snapshot.needles));
    summary.append(title, needle);
    inspector.appendChild(summary);

    const content = htmlElement("div", "query-inspector-content");
    const grids = htmlElement("div", "query-metric-groups");

    const resultGroup = htmlElement("section", "query-metric-group");
    resultGroup.appendChild(htmlElement("h4", "", "Query & result"));
    const resultList = htmlElement("dl");
    appendMetric(resultList, "Needle", needleText(snapshot.needles));
    const lengths = Array.isArray(snapshot.needleLens) ? snapshot.needleLens : [];
    const lengthText = lengths.length > 1
      ? `${countText(snapshot.needleLen)} total (${lengths.map(countText).join(" + ")})`
      : finite(snapshot.needleLen) ? countText(snapshot.needleLen) : "—";
    appendMetric(resultList, "Byte length", lengthText);
    appendMetric(resultList, "Operation", snapshot.op || "—");
    appendMetric(resultList, "Selectivity", optionalPercent(snapshot.selectivity));
    appendMetric(resultList, "Matching rows", countText(snapshot.matches));
    appendMetric(resultList, "Rarest byte frequency", optionalPercent(snapshot.rarestByte));
    const gateValid = snapshot.gateHashOk === null ? "—"
      : snapshot.gateHashOk && snapshot.gateExpected === snapshot.gateActual ? "valid"
        : "mismatch";
    appendMetric(resultList, "Correctness gate", gateValid);
    resultGroup.appendChild(resultList);

    const coverGroup = htmlElement("section", "query-metric-group");
    coverGroup.appendChild(htmlElement("h4", "", "Prefilter / mincut"));
    const coverList = htmlElement("dl");
    appendMetric(coverList, "Live cover shape", coverShapeText(snapshot));
    appendMetric(coverList, "SIMD comparison cost", countText(snapshot.comparisons),
      "points + 2 × ranges per code-stream vector");
    appendMetric(coverList, "Mincut token occurrences", countText(snapshot.coveredCodes),
      "Total occurrences of all token ids retained in the live mincut cover");
    appendMetric(coverList, "All encoded token occurrences", countText(snapshot.indexedCodes),
      "The denominator: every token occurrence in the encoded column");
    appendMetric(coverList, "Cover share of encoded stream", optionalPercent(snapshot.coveredFraction),
      "mincut token occurrences ÷ all encoded token occurrences; this is not row selectivity");
    appendMetric(coverList, "Rows sent to verification", optionalPercent(snapshot.admitted));
    appendMetric(coverList, "Verification amplification", finite(snapshot.amplification)
      ? `${formatSignificant(snapshot.amplification, 4)}× matching rows` : "—");
    appendMetric(coverList, "Policy hint", snapshot.profitable === null ? "—"
      : snapshot.profitable ? "prefilter" : "fallback");
    if (finite(snapshot.prefilterCandidates)) {
      appendMetric(coverList, "Measured candidate rows", countText(snapshot.prefilterCandidates));
    }
    if (finite(snapshot.pruneRate)) {
      appendMetric(coverList, "Measured prune rate", optionalPercent(snapshot.pruneRate));
    }
    if (finite(snapshot.falsePositiveRate)) {
      appendMetric(coverList, "False-positive rate", optionalPercent(snapshot.falsePositiveRate));
    }
    if (finite(snapshot.verifyPerSurvivor)) {
      appendMetric(coverList, "Verify / survivor",
        `${formatSignificant(snapshot.verifyPerSurvivor, 4)} ns`);
    }
    coverGroup.appendChild(coverList);
    grids.append(resultGroup, coverGroup);
    content.appendChild(grids);
    content.appendChild(renderMeasurementTable(snapshot, selectedSeries));
    const mincutInspectors = renderMincutInspectors(snapshot);
    if (mincutInspectors) content.appendChild(mincutInspectors);

    if (snapshot.meta) {
      const meta = htmlElement("details", "query-meta");
      meta.appendChild(htmlElement("summary", "", "Suite metadata / provenance"));
      meta.appendChild(htmlElement("pre", "", JSON.stringify(snapshot.meta, null, 2)));
      content.appendChild(meta);
    }
    inspector.appendChild(content);
    return inspector;
  }

  const mincutBundleCache = new Map();

  function loadMincutBundle(archiveId) {
    if (mincutBundleCache.has(archiveId)) return mincutBundleCache.get(archiveId);
    const archive = MINCUT_ARCHIVES[archiveId];
    if (!archive) return Promise.reject(new Error("no embedded graph bundle for this artifact"));
    const pending = (async () => {
      if (archive.encoding !== "gzip+base64") {
        throw new Error(`unsupported graph bundle encoding ${archive.encoding || "unknown"}`);
      }
      if (typeof DecompressionStream !== "function") {
        throw new Error("this browser cannot decompress the embedded graph bundle");
      }
      const binary = atob(archive.data);
      const bytes = new Uint8Array(binary.length);
      for (let index = 0; index < binary.length; index += 1) {
        bytes[index] = binary.charCodeAt(index);
      }
      const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
      const bundle = await new Response(stream).json();
      if (!bundle || bundle.version !== 1 || !bundle.graphs) {
        throw new Error("embedded graph bundle has an unknown format");
      }
      if (bundle.dictionary_bits !== archive.dictionary_bits
          || bundle.dictionary_fingerprint !== archive.dictionary_fingerprint) {
        throw new Error("embedded graph provenance does not match its archive metadata");
      }
      return bundle;
    })();
    mincutBundleCache.set(archiveId, pending);
    return pending;
  }

  function mincutNeedleLabel(snapshot, index) {
    const needle = Array.isArray(snapshot.needles) ? snapshot.needles[index] : null;
    return needle ? JSON.stringify(needle.display) : `needle ${index + 1}`;
  }

  function mincutProfileLabel(archive) {
    const labels = unique((archive.profiles || []).map(profile => {
      const config = compactConfig(profile.config);
      return [profile.candidate, config ? `[${config}]` : null,
        profile.candidate_version || null].filter(Boolean).join(" · ");
    }));
    return labels.join(" / ") || `${archive.dictionary_bits}-bit OnPair dictionary`;
  }

  function renderMincutInspector(snapshot, archiveId) {
    const archive = MINCUT_ARCHIVES[archiveId];
    if (!archive) return null;
    const details = htmlElement("details", "query-mincut");
    const summary = htmlElement("summary");
    summary.append(
      htmlElement("span", "query-mincut-title", "Mincut graph"),
      htmlElement("span", "query-mincut-profile", mincutProfileLabel(archive)),
    );
    details.appendChild(summary);

    const content = htmlElement("div", "query-mincut-content");
    const toolbar = htmlElement("div", "mincut-toolbar");
    const verification = archive.verification === "all_recorded_cover_facts"
      ? "Legacy artifact: reconstruction checked against every recorded single-needle cover."
      : "Dictionary fingerprint and recorded cover facts verified.";
    const description = htmlElement("p", "",
      `Orange probes form the minimum weighted cut; teal paths show shared parsing states. ${verification}`);
    const controls = htmlElement("div", "mincut-controls");
    const needleCount = Math.max(1, Array.isArray(snapshot.needles) ? snapshot.needles.length : 0);
    let needleIndex = 0;
    let size = "fit";
    let generation = 0;
    let activeSvg = null;
    let activeName = "mincut-graph";

    const needleField = htmlElement("label");
    needleField.appendChild(htmlElement("span", "", "Needle"));
    const needleSelect = htmlElement("select");
    const needleIndices = Array.from({length: needleCount}, (_unused, index) => index);
    setOptions(needleSelect, needleIndices, needleIndex,
      index => mincutNeedleLabel(snapshot, Number(index)));
    needleField.appendChild(needleSelect);
    needleField.hidden = needleCount < 2;

    const sizeControls = htmlElement("div", "segmented");
    sizeControls.setAttribute("role", "group");
    sizeControls.setAttribute("aria-label", "Mincut graph size");
    const fitButton = htmlElement("button", "", "Fit width");
    const actualButton = htmlElement("button", "", "100%");
    [fitButton, actualButton].forEach(button => { button.type = "button"; });
    const download = htmlElement("button", "png-button", "Download SVG");
    download.type = "button";
    download.disabled = true;
    sizeControls.append(fitButton, actualButton);
    controls.append(needleField, sizeControls, download);
    toolbar.append(description, controls);

    const status = htmlElement("p", "query-selection-note mincut-graph-status");
    status.setAttribute("role", "status");
    const profileRows = snapshot.rows.filter(row => row.mincut_archive_id === archiveId);
    const facts = profileRows.find(row => finite(row.comparison_cost)) || profileRows[0] || {};
    const provenance = htmlElement("dl", "query-mincut-provenance");
    appendMetric(provenance, "Recorded cover",
      finite(facts.cover_points) || finite(facts.cover_ranges)
        ? `${countText(facts.cover_points || 0)} points + ${countText(facts.cover_ranges || 0)} ranges`
        : "—");
    appendMetric(provenance, "Covered token occurrences", countText(facts.covered_codes));
    appendMetric(provenance, "Dictionary budget", `${archive.dictionary_bits}-bit`);
    appendMetric(provenance,
      archive.verification === "all_recorded_cover_facts"
        ? "Reconstructed fingerprint" : "Measured dictionary fingerprint",
      archive.dictionary_fingerprint || "—");
    const frame = htmlElement("div", "mincut-graph-frame");
    frame.appendChild(htmlElement("p", "query-details-empty",
      "Expand this section to load the graph."));
    content.append(toolbar, provenance, status, frame);
    details.appendChild(content);

    function syncSizeButtons() {
      fitButton.setAttribute("aria-pressed", String(size === "fit"));
      actualButton.setAttribute("aria-pressed", String(size === "actual"));
    }

    function clearFrame(message) {
      activeSvg = null;
      download.disabled = true;
      frame.replaceChildren(htmlElement("p", "query-details-empty", message));
    }

    function mountSvg(rawSvg, graph, bundle) {
      const parsed = new DOMParser().parseFromString(rawSvg, "image/svg+xml");
      if (parsed.querySelector("parsererror")) {
        throw new Error("the embedded SVG could not be parsed");
      }
      const svg = parsed.documentElement;
      const originalWidth = Number(svg.getAttribute("width")) || 1200;
      const originalHeight = Number(svg.getAttribute("height")) || 700;
      if (size === "fit") {
        svg.setAttribute("width", "100%");
        svg.removeAttribute("height");
        svg.style.width = "100%";
        svg.style.height = "auto";
      } else {
        svg.setAttribute("width", String(originalWidth));
        svg.setAttribute("height", String(originalHeight));
        svg.style.width = `${originalWidth}px`;
        svg.style.height = `${originalHeight}px`;
        svg.style.maxWidth = "none";
      }
      const host = htmlElement("div", "mincut-svg-host");
      const shadow = host.attachShadow({mode: "open"});
      shadow.appendChild(document.importNode(svg, true));
      frame.replaceChildren(host);
      activeSvg = rawSvg;
      activeName = `${snapshot.id}-needle-${graph.needle_index + 1}-${archiveId}`;
      download.disabled = false;
      status.textContent =
        `${graph.states.toLocaleString("en-GB")} parsing states · `
        + `${countText(graph.cover_points)} points + ${countText(graph.cover_ranges)} ranges · `
        + `${countText(graph.comparison_cost)} SIMD comparisons · `
        + `${countText(graph.covered_codes)} covered token occurrences · `
        + `${bundle.dictionary_bits}-bit dictionary · `
        + `fingerprint ${bundle.dictionary_fingerprint.split(":").pop()}`;
    }

    async function draw() {
      const drawGeneration = ++generation;
      status.textContent = "Loading compressed graph bundle…";
      clearFrame("Loading graph…");
      try {
        const bundle = await loadMincutBundle(archiveId);
        if (drawGeneration !== generation || !details.open) return;
        const graphs = bundle.graphs[snapshot.id] || [];
        const graph = graphs.find(item => item.needle_index === needleIndex);
        if (!graph) {
          status.textContent = "No graph was generated for this needle.";
          clearFrame("Graph unavailable for this query.");
          return;
        }
        if (!graph.svg) {
          const reason = graph.error || "Graph SVG unavailable.";
          status.textContent = reason;
          clearFrame(reason);
          return;
        }
        mountSvg(graph.svg, graph, bundle);
      } catch (error) {
        if (drawGeneration !== generation) return;
        const message = `Unable to load mincut graph: ${error && error.message}`;
        status.textContent = message;
        clearFrame(message);
      }
    }

    details.addEventListener("toggle", () => {
      if (details.open) void draw();
      else generation += 1;
    });
    needleSelect.addEventListener("change", () => {
      needleIndex = Number(needleSelect.value);
      if (details.open) void draw();
    });
    fitButton.addEventListener("click", () => {
      size = "fit";
      syncSizeButtons();
      if (details.open) void draw();
    });
    actualButton.addEventListener("click", () => {
      size = "actual";
      syncSizeButtons();
      if (details.open) void draw();
    });
    download.addEventListener("click", () => {
      if (!activeSvg) return;
      const link = document.createElement("a");
      const slug = activeName.toLowerCase()
        .replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
      link.download = `${slug || "mincut-graph"}.svg`;
      link.href = URL.createObjectURL(
        new Blob([activeSvg], {type: "image/svg+xml;charset=utf-8"}));
      link.click();
      setTimeout(() => URL.revokeObjectURL(link.href), 1000);
    });
    syncSizeButtons();
    return details;
  }

  function renderMincutInspectors(snapshot) {
    const archiveIds = unique(snapshot.rows.map(row => row.mincut_archive_id).filter(Boolean));
    if (!archiveIds.length) return null;
    const wrap = htmlElement("section", "query-mincut-list");
    wrap.appendChild(htmlElement("h4", "", archiveIds.length === 1
      ? "Parsing graph" : "Parsing graphs by dictionary artifact"));
    archiveIds.forEach(archiveId => {
      const inspector = renderMincutInspector(snapshot, archiveId);
      if (inspector) wrap.appendChild(inspector);
    });
    return wrap;
  }

  function renderQueryDetails(rows, selectedSeries) {
    const ids = [...state.selectedQueryIds];
    refs["query-details-clear"].disabled = ids.length === 0;
    refs["query-details-summary"].textContent = ids.length
      ? `${ids.length.toLocaleString("en-GB")} quer${ids.length === 1 ? "y" : "ies"} selected`
      : "Click a raw point or median point to inspect its query data";
    refs["query-details-body"].replaceChildren();
    if (!ids.length) {
      refs["query-details-body"].appendChild(htmlElement(
        "p", "query-details-empty", "Nothing selected yet. Click a point in the plot above."));
      return;
    }
    const order = new Map(rows.map((row, index) => [row.query_id, index]));
    const snapshots = ids
      .map(id => snapshotForQuery(id, rows))
      .filter(snapshot => snapshot.rows.length)
      .sort((a, b) => (order.get(a.id) || 0) - (order.get(b.id) || 0));
    const intro = htmlElement("p", "query-selection-note",
      `${snapshots.length.toLocaleString("en-GB")} quer${snapshots.length === 1 ? "y" : "ies"} in the current view. `
      + "Open a row for complete measurements and provenance.");
    refs["query-details-body"].appendChild(intro);
    snapshots.forEach((snapshot, index) => {
      refs["query-details-body"].appendChild(
        renderQueryInspector(snapshot, selectedSeries, snapshots.length === 1 || index === 0));
    });
  }

  function selectQueries(event, queryIds) {
    const ids = unique(queryIds.filter(Boolean));
    if (!ids.length) return;
    const additive = event.shiftKey || event.ctrlKey || event.metaKey;
    if (!additive) {
      state.selectedQueryIds = new Set(ids);
    } else {
      const remove = ids.every(id => state.selectedQueryIds.has(id));
      ids.forEach(id => remove ? state.selectedQueryIds.delete(id) : state.selectedQueryIds.add(id));
    }
    refs.tooltip.hidden = true;
    refs["query-details"].open = true;
    renderPlot();
  }

  function makeQueryPoint(node, queryIds, label) {
    node.classList.add("query-point");
    node.setAttribute("tabindex", "0");
    node.setAttribute("role", "button");
    node.setAttribute("aria-label", label);
    node.addEventListener("click", event => selectQueries(event, queryIds));
    node.addEventListener("keydown", event => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        selectQueries(event, queryIds);
      }
    });
  }

  function renderPlot() {
    const rows = filteredRows();
    const catalog = catalogs(rows);
    syncColors(catalog);
    const selectedSeries = catalog.series.filter(item => state.visibleSeries.has(item.id));
    const selectedDecode = catalog.decode.filter(item => state.visibleDecode.has(item.id));
    const width = 1200;
    const left = 92;
    const right = 38;
    const bottomMargin = 76;
    const legendItems = [
      ...selectedSeries.map(item => ({...item, kind: "series", color: colorFor(item.id)})),
      ...selectedDecode.map(item => ({...item, kind: "decode", color: colorFor(item.id)})),
    ];
    const legendRows = legendLayout(legendItems, width - left - right);
    const subtitle = effectiveSubtitle();
    const legendTop = subtitle ? 88 : 66;
    const chartTop = legendItems.length ? legendTop + legendRows.length * 23 + 19 : legendTop + 15;
    const height = Math.max(660, chartTop + 430 + bottomMargin);
    const chartBottom = height - bottomMargin;
    const chartRight = width - right;

    refs.plot.replaceChildren();
    refs.plot.setAttribute("viewBox", `0 0 ${width} ${height}`);
    refs.plot.setAttribute("width", width);
    refs.plot.setAttribute("height", height);
    refs.plot.setAttribute("xmlns", NS);

    const style = svgElement("style", {}, `
      text { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; fill: #17212b; }
      .mono { font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace; }
      .tick { fill: #788692; font-size: 11px; }
      .axis-label { fill: #52616d; font-size: 12px; font-weight: 650; }
      .query-point { cursor: pointer; }
      .query-point:focus { outline: none; stroke: #17212b; stroke-width: 2.4px; }
    `);
    refs.plot.appendChild(style);
    refs.plot.appendChild(svgElement("rect", {width: "100%", height: "100%", fill: "#ffffff"}));
    refs.plot.appendChild(svgElement("text", {
      id: "plot-accessible-title", x: left, y: 35, "font-size": 22, "font-weight": 720,
      "letter-spacing": "-0.3"
    }, state.title));
    if (subtitle) {
      refs.plot.appendChild(svgElement("text", {
        x: left, y: 58, "font-size": 12, fill: "#71808c"
      }, subtitle));
    }
    // Keep the editor showing what the plot shows, so a generated subtitle can
    // be edited from where it stands rather than from an empty box.
    if (!state.subtitlePinned && refs["subtitle-input"].value !== subtitle) {
      refs["subtitle-input"].value = subtitle;
    }

    legendRows.forEach((legendRow, rowIndex) => {
      let x = left;
      const y = legendTop + rowIndex * 23;
      legendRow.forEach(item => {
        const line = svgElement("line", {
          x1: x, y1: y, x2: x + 22, y2: y,
          stroke: item.color, "stroke-width": 2.6, "stroke-linecap": "round",
          "stroke-dasharray": item.kind === "decode" ? "7 5" : dashFor(item.id)
        });
        refs.plot.appendChild(line);
        refs.plot.appendChild(svgElement("text", {
          x: x + 29, y: y + 4, "font-size": 10.5, fill: "#52616d", class: "mono"
        }, item.label));
        x += item.itemWidth;
      });
    });

    const domainRows = rows.filter(row => finite(xValue(row)) && finite(yValue(row)));
    const selectedIds = new Set(selectedSeries.map(item => item.id));
    const selectedRows = domainRows.filter(row => selectedIds.has(seriesKey(row)));
    const xDomainRows = selectedRows.length ? selectedRows : domainRows;
    const xValues = xDomainRows.map(row => xValue(row));

    // The Y domain comes from the drawn summaries -- median line and IQR band --
    // not from raw scatter. A single query three orders of magnitude off (an
    // empty cover, a needle that matches nothing) would otherwise flatten every
    // line in the plot into the bottom pixel row. Such points still draw; they
    // are clipped instead of setting the scale for everyone else. `Clip Y`
    // overrides this when the outliers are the subject.
    const aggregates = new Map();
    selectedSeries.forEach(meta => {
      aggregates.set(meta.id, binPoints(pointsForSeries(rows, meta.id)));
    });
    const yValues = [];
    aggregates.forEach(points => points.forEach(point => {
      yValues.push(point.y);
      if (state.showBand) {
        yValues.push(point.q1);
        yValues.push(point.q3);
      }
    }));
    selectedDecode.forEach(meta => {
      const value = median(rows.filter(row => candidateKey(row) === meta.id)
        .map(decodeValue).filter(finite));
      if (finite(value)) yValues.push(value);
    });
    if (!yValues.length) domainRows.forEach(row => yValues.push(yValue(row)));
    const xScale = makeScale(xValues, state.xScale, left, chartRight, activeRange());
    const yScale = makeScale(yValues, state.yScale, chartBottom, chartTop, activeYLimits());

    const defs = svgElement("defs");
    const clip = svgElement("clipPath", {id: "bench-viz-clip"});
    clip.appendChild(svgElement("rect", {
      x: left, y: chartTop, width: chartRight - left, height: chartBottom - chartTop
    }));
    defs.appendChild(clip);
    refs.plot.appendChild(defs);

    yScale.ticks.forEach(tick => {
      const y = yScale.map(tick);
      if (y < chartTop - 1 || y > chartBottom + 1) return;
      refs.plot.appendChild(svgElement("line", {
        x1: left, y1: y, x2: chartRight, y2: y, stroke: "#e5ebef", "stroke-width": 1
      }));
      refs.plot.appendChild(svgElement("text", {
        x: left - 11, y: y + 4, "text-anchor": "end", class: "mono tick"
      }, formatY(tick)));
    });

    xScale.ticks.forEach(tick => {
      const x = xScale.map(tick);
      if (x < left - 1 || x > chartRight + 1) return;
      refs.plot.appendChild(svgElement("line", {
        x1: x, y1: chartTop, x2: x, y2: chartBottom, stroke: "#eef2f4", "stroke-width": 1
      }));
      refs.plot.appendChild(svgElement("text", {
        x, y: chartBottom + 21, "text-anchor": "middle", class: "mono tick"
      }, formatX(tick)));
    });

    refs.plot.appendChild(svgElement("rect", {
      x: left, y: chartTop, width: chartRight - left, height: chartBottom - chartTop,
      fill: "none", stroke: "#cad5dc", "stroke-width": 1
    }));
    refs.plot.appendChild(svgElement("text", {
      x: (left + chartRight) / 2, y: height - 22, "text-anchor": "middle", class: "axis-label"
    }, state.xLabel));
    refs.plot.appendChild(svgElement("text", {
      transform: `translate(24 ${(chartTop + chartBottom) / 2}) rotate(-90)`,
      "text-anchor": "middle", class: "axis-label"
    }, state.yLabel));

    const marks = svgElement("g", {"clip-path": "url(#bench-viz-clip)"});
    refs.plot.appendChild(marks);

    selectedSeries.forEach(meta => {
      const raw = pointsForSeries(rows, meta.id);
      const aggregate = aggregates.get(meta.id) || binPoints(raw);
      const color = colorFor(meta.id);
      if (!aggregate.length) return;

      if (state.showBand && aggregate.some(point => point.q1 !== point.q3)) {
        const upper = aggregate.map(point => `${xScale.map(point.x).toFixed(2)},${yScale.map(point.q3).toFixed(2)}`);
        const lower = [...aggregate].reverse().map(point => `${xScale.map(point.x).toFixed(2)},${yScale.map(point.q1).toFixed(2)}`);
        marks.appendChild(svgElement("polygon", {
          points: [...upper, ...lower].join(" "), fill: color, opacity: 0.12
        }));
      }

      if (state.showPoints) {
        raw.forEach(point => {
          const circle = svgElement("circle", {
            cx: xScale.map(point.x), cy: yScale.map(point.y), r: 2.1,
            fill: color, opacity: 0.18
          });
          attachTooltip(circle, [
            meta.label,
            `${state.xLabel}: ${formatX(point.x, true)}`,
            `${state.yLabel}: ${formatY(point.y, true)}`,
            point.row.query_id,
            "click to inspect · modifier-click to compare",
          ]);
          makeQueryPoint(circle, [point.row.query_id], `Inspect query ${point.row.query_id}`);
          marks.appendChild(circle);
        });
      }

      if (aggregate.length > 1) {
        marks.appendChild(svgElement("path", {
          d: pathLine(aggregate, xScale, yScale), fill: "none", stroke: color,
          "stroke-width": 2.5, "stroke-linejoin": "round", "stroke-linecap": "round",
          "stroke-dasharray": dashFor(meta.id), opacity: 0.96
        }));
      }
      aggregate.forEach(point => {
        const selectedCount = point.queryIds.filter(id => state.selectedQueryIds.has(id)).length;
        const circle = svgElement("circle", {
          cx: xScale.map(point.x), cy: yScale.map(point.y), r: selectedCount ? 4.5 : 3.6,
          fill: color, stroke: selectedCount ? "#17212b" : "#ffffff",
          "stroke-width": selectedCount ? 1.8 : 1.2
        });
        attachTooltip(circle, [
          meta.label,
          `median ${state.xLabel}: ${formatX(point.x, true)}`,
          `median ${state.yLabel}: ${formatY(point.y, true)}`,
          `IQR: ${formatY(point.q1, true)} – ${formatY(point.q3, true)} · n=${point.n}`,
          `click to inspect ${point.queryIds.length} quer${point.queryIds.length === 1 ? "y" : "ies"}`,
        ]);
        makeQueryPoint(circle, point.queryIds,
          `Inspect ${point.queryIds.length} queries in this aggregate point`);
        marks.appendChild(circle);
      });
    });

    // Persistent query highlights sit above the summaries and remain visible
    // even when raw scatter is hidden. The same query is outlined in every
    // visible series, making cross-candidate outliers immediately comparable.
    if (state.selectedQueryIds.size) {
      selectedSeries.forEach(meta => {
        const color = colorFor(meta.id);
        pointsForSeries(rows, meta.id)
          .filter(point => state.selectedQueryIds.has(point.row.query_id))
          .forEach(point => {
            const circle = svgElement("circle", {
              cx: xScale.map(point.x), cy: yScale.map(point.y), r: 5.2,
              fill: color, opacity: 0.96, stroke: "#17212b", "stroke-width": 2
            });
            attachTooltip(circle, [
              meta.label,
              `${state.xLabel}: ${formatX(point.x, true)}`,
              `${state.yLabel}: ${formatY(point.y, true)}`,
              point.row.query_id,
              "selected · click to keep only this query",
            ]);
            makeQueryPoint(circle, [point.row.query_id], `Selected query ${point.row.query_id}`);
            marks.appendChild(circle);
          });
      });
    }

    selectedDecode.forEach(meta => {
      const values = rows
        .filter(row => candidateKey(row) === meta.id)
        .map(decodeValue)
        .filter(finite);
      const value = median(values);
      if (!finite(value)) return;
      const color = colorFor(meta.id);
      const line = svgElement("line", {
        x1: left, y1: yScale.map(value), x2: chartRight, y2: yScale.map(value),
        stroke: color, "stroke-width": 2.2, "stroke-dasharray": "7 5", opacity: 0.9
      });
      attachTooltip(line, [
        meta.label,
        `${formatY(value, true)} (median of ${values.length} instrumented decode passes)`,
        "scan time excluded",
      ]);
      marks.appendChild(line);
    });

    if (!selectedSeries.length && !selectedDecode.length) {
      refs.plot.appendChild(svgElement("text", {
        x: (left + chartRight) / 2, y: (chartTop + chartBottom) / 2,
        "text-anchor": "middle", class: "mono", fill: "#7e8a94", "font-size": 13
      }, "Select a candidate or decompression baseline above"));
    }

    const zeroNote = state.xScale === "log" && xValues.some(value => value <= 0)
      ? " · zero uses a dedicated log-axis bucket"
      : "";
    const range = activeRange();
    const contextCount = contextRows().filter(row => finite(xValue(row)) && finite(yValue(row))).length;
    const rangeNote = range.min !== null || range.max !== null
      ? ` · focus ${range.min === null ? "start" : formatX(range.min, true)}–${range.max === null ? "end" : formatX(range.max, true)} · ${Math.max(0, contextCount - domainRows.length)} excluded`
      : "";
    refs["plot-status"].textContent =
      `${domainRows.length} measurements · ${selectedSeries.length} candidate series · ${selectedDecode.length} decode baselines`
      + `${state.selectedQueryIds.size ? ` · ${state.selectedQueryIds.size} queries highlighted` : ""}${rangeNote}${zeroNote}`;
    renderQueryDetails(contextRows(), selectedSeries);
    document.title = `${state.title} — Benchmark Explorer 3000™`;
  }

  function setDefaultAxisLabels() {
    if (!state.xLabelCustom) {
      state.xLabel = xMetricInfo().label;
      refs["x-label-input"].value = state.xLabel;
    }
    if (!state.yLabelCustom) {
      state.yLabel = state.yMetric === "gbps"
        ? "throughput (GB/s, raw payload)"
        : "latency (ns/row)";
      refs["y-label-input"].value = state.yLabel;
    }
  }

  function updateScaleButtons() {
    document.querySelectorAll("[data-axis][data-scale]").forEach(button => {
      const selected = state[`${button.dataset.axis}Scale`] === button.dataset.scale;
      button.setAttribute("aria-pressed", String(selected));
    });
  }

  function rebuild() {
    const rows = filteredRows();
    const availableQueries = new Set(rows.map(row => row.query_id));
    state.selectedQueryIds = new Set(
      [...state.selectedQueryIds].filter(queryId => availableQueries.has(queryId))
    );
    const catalog = catalogs(rows);
    reconcileVisibility(catalog);
    renderChips(catalog);
    updateScaleButtons();
    renderPlot();
    renderPanels();
  }

  // Panels registered by other sections. They share this file's selection --
  // run, dataset, op, chunking, focus range and visible series -- so switching
  // tabs never changes what is being described, only how.
  function renderPanels() {
    const slot = document.getElementById("pf-error");
    if (slot) slot.hidden = true;
    (host.onRender || []).forEach(hook => {
      try {
        hook();
      } catch (error) {
        // A broken panel must not take the main plot down with it -- but the
        // message has to land somewhere the reader is actually looking, which
        // is the panel that failed, not the plot footer inside the tab they
        // just navigated away from.
        const slot = document.getElementById("pf-error");
        if (slot) {
          slot.textContent = `This panel failed to render: ${error && error.message}. `
            + `The rest of the page is unaffected.`;
          slot.hidden = false;
        } else if (refs["plot-status"]) {
          refs["plot-status"].textContent = `panel error: ${error && error.message}`;
        }
      }
    });
  }

  function selectTab(name) {
    document.querySelectorAll("[data-tab]").forEach(button => {
      const active = button.dataset.tab === name;
      button.classList.toggle("is-active", active);
      button.setAttribute("aria-selected", String(active));
    });
    document.querySelectorAll("[data-panel]").forEach(panel => {
      panel.hidden = panel.dataset.panel !== name;
    });
    if (name === "throughput") renderPlot();
    else renderPanels();
  }

  // Any SVG on the page can be exported; the caller says which and under what
  // name. `label` becomes part of the filename so a folder of exports from one
  // session is still tellable apart.
  function exportSvg(svg, label, button) {
    if (!svg || !svg.viewBox || !svg.viewBox.baseVal.width) return;
    const original = button ? button.textContent : null;
    if (button) {
      button.disabled = true;
      button.textContent = "…";
    }
    const scale = Number(refs["export-scale"].value) || 1;
    const viewBox = svg.viewBox.baseVal;
    const source = svg.cloneNode(true);
    source.setAttribute("width", viewBox.width);
    source.setAttribute("height", viewBox.height);
    source.setAttribute("xmlns", NS);
    const url = URL.createObjectURL(
      new Blob([new XMLSerializer().serializeToString(source)],
               {type: "image/svg+xml;charset=utf-8"}));
    const image = new Image();

    const finish = () => {
      URL.revokeObjectURL(url);
      if (button) {
        button.disabled = false;
        button.textContent = original;
      }
    };

    image.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = Math.round(viewBox.width * scale);
      canvas.height = Math.round(viewBox.height * scale);
      const context = canvas.getContext("2d");
      // The SVG has no background of its own in the panels, and a transparent
      // PNG dropped into a document reads as a rendering fault.
      context.fillStyle = "#ffffff";
      context.fillRect(0, 0, canvas.width, canvas.height);
      context.setTransform(scale, 0, 0, scale, 0, 0);
      context.drawImage(image, 0, 0, viewBox.width, viewBox.height);
      canvas.toBlob(png => {
        if (png) {
          const link = document.createElement("a");
          const slug = value => String(value).toLowerCase()
            .replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
          link.download =
            `${slug(state.title) || "benchmark"}-${slug(label) || "plot"}`
            + `-${Math.round(viewBox.width * scale)}px.png`;
          link.href = URL.createObjectURL(png);
          link.click();
          setTimeout(() => URL.revokeObjectURL(link.href), 1000);
        }
        finish();
      }, "image/png");
    };
    image.onerror = finish;
    image.src = url;
  }

  // Turns a `data-export="<svg id>"` button into a working export control, so a
  // new plot needs no wiring beyond putting a button next to it.
  function bindExportButtons(root = document) {
    root.querySelectorAll("[data-export]").forEach(button => {
      if (button.dataset.exportBound) return;
      button.dataset.exportBound = "yes";
      button.addEventListener("click", () => exportSvg(
        document.getElementById(button.dataset.export),
        button.dataset.exportName || button.dataset.export, button));
    });
  }

  function wireEvents() {
    refs["source-select"].addEventListener("change", () => {
      state.source = refs["source-select"].value;
      state.dataset = null; state.op = null; state.chunk = null;
      populateFilters(true); rebuild();
    });
    refs["dataset-select"].addEventListener("change", () => {
      state.dataset = refs["dataset-select"].value;
      state.op = null; state.chunk = null;
      populateFilters(true); rebuild();
    });
    refs["op-select"].addEventListener("change", () => {
      state.op = refs["op-select"].value;
      state.chunk = null;
      populateFilters(false); rebuild();
    });
    refs["chunk-select"].addEventListener("change", () => {
      state.chunk = Number(refs["chunk-select"].value); rebuild();
    });
    refs["x-metric"].addEventListener("change", () => {
      state.xMetric = refs["x-metric"].value;
      setDefaultAxisLabels();
      updateFocusControls();
      rebuild();
    });
    refs["y-metric"].addEventListener("change", () => {
      state.yMetric = refs["y-metric"].value;
      setDefaultAxisLabels();
      updateYLimitControls();
      rebuild();
    });
    refs["bin-count"].addEventListener("change", () => {
      state.bins = refs["bin-count"].value === "exact" ? "exact" : Number(refs["bin-count"].value);
      renderPlot();
    });
    refs["show-points"].addEventListener("change", () => {
      state.showPoints = refs["show-points"].checked; renderPlot();
    });
    refs["show-band"].addEventListener("change", () => {
      state.showBand = refs["show-band"].checked; renderPlot();
    });
    refs["focus-min"].addEventListener("change", applyFocusInputs);
    refs["focus-max"].addEventListener("change", applyFocusInputs);
    refs["ylimit-min"].addEventListener("change", applyYLimitInputs);
    refs["ylimit-max"].addEventListener("change", applyYLimitInputs);
    refs["ylimit-reset"].addEventListener("click", () => {
      state.yLimits[state.yMetric] = {min: null, max: null};
      updateYLimitControls();
      renderPlot();
    });
    refs["focus-reset"].addEventListener("click", () => {
      state.ranges[state.xMetric] = {min: null, max: null};
      updateFocusControls();
      rebuild();
    });
    document.querySelectorAll("[data-axis][data-scale]").forEach(button => {
      button.addEventListener("click", () => {
        state[`${button.dataset.axis}Scale`] = button.dataset.scale;
        updateScaleButtons();
        renderPlot();
      });
    });

    refs["edit-labels"].addEventListener("click", () => {
      const opening = refs["label-editor"].hidden;
      refs["label-editor"].hidden = !opening;
      refs["edit-labels"].setAttribute("aria-expanded", String(opening));
      refs["edit-labels"].textContent = opening ? "Hide labels" : "Edit labels";
    });
    refs["title-input"].addEventListener("input", () => {
      state.title = refs["title-input"].value; renderPlot();
    });
    refs["subtitle-input"].addEventListener("input", () => {
      state.subtitle = refs["subtitle-input"].value;
      state.subtitlePinned = state.subtitle.trim().length > 0;
      renderPlot();
    });
    refs["x-label-input"].addEventListener("input", () => {
      state.xLabelCustom = true; state.xLabel = refs["x-label-input"].value; renderPlot();
    });
    refs["y-label-input"].addEventListener("input", () => {
      state.yLabelCustom = true; state.yLabel = refs["y-label-input"].value; renderPlot();
    });
    refs["series-search"].addEventListener("input", applySearch);
    refs["series-all"].addEventListener("click", () => chooseAll("series", true));
    refs["series-none"].addEventListener("click", () => chooseAll("series", false));
    refs["decode-all"].addEventListener("click", () => chooseAll("decode", true));
    refs["decode-none"].addEventListener("click", () => chooseAll("decode", false));
    refs["query-details-clear"].addEventListener("click", () => {
      state.selectedQueryIds.clear();
      renderPlot();
    });
    bindExportButtons();
    document.querySelectorAll("[data-tab]").forEach(button => {
      button.addEventListener("click", () => selectTab(button.dataset.tab));
    });
  }

  // Shared with sibling sections (prefilter.js). Exposed rather than inlined so
  // the two panels stay separable: this file owns the selection and the drawing
  // primitives, and knows nothing about what else is rendered from them.
  const host = {
    DATA, DEFAULTS, ANALYSIS, state,
    helpers: {
      finite, unique, median, quantile, svgElement, makeScale, pathLine,
      formatSignificant, percentText, seriesMeta, seriesKey, candidateLabel,
      colorFor, filteredRows, contextRows, attachTooltip, refs,
      exportSvg, bindExportButtons,
      // The prefilter panel carries its own Run/Dataset selectors, so it needs
      // a way to move the shared selection and have every panel follow.
      setSelection(patch) {
        Object.assign(state, patch);
        populateFilters(false);
        rebuild();
      },
      sourceValues: () => unique(DATA.map(row => row.source)).sort(),
      datasetValues: () => unique(
        DATA.filter(row => row.source === state.source).map(row => row.dataset)).sort(),
      datasetLabel: value => {
        const row = DATA.find(item => item.dataset === value && item.dataset_display);
        return (row && row.dataset_display) || value;
      },
    },
    onRender: [],
  };
  globalThis.benchViz = host;

  function init() {
    refs["title-input"].value = state.title;
    if (state.subtitlePinned) refs["subtitle-input"].value = state.subtitle;
    refs["x-metric"].value = state.xMetric;
    refs["y-metric"].value = state.yMetric;
    refs["bin-count"].value = String(state.bins);
    setDefaultAxisLabels();
    updateFocusControls();
    updateYLimitControls();
    populateFilters(true);
    wireEvents();
    rebuild();
  }

  init();
})();
