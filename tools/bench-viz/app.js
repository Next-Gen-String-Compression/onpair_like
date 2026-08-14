(() => {
  "use strict";

  const DATA = JSON.parse(document.getElementById("bench-viz-data").textContent);
  const DEFAULTS = JSON.parse(document.getElementById("bench-viz-defaults").textContent);
  const NS = "http://www.w3.org/2000/svg";
  const PALETTE = [
    "#a94b00", "#007b75", "#7b61b8", "#1f70a8", "#a63d64", "#558b2f",
    "#b07d00", "#4267ac", "#8c564b", "#168aad", "#8f4a9d", "#2d7d46",
    "#c45a32", "#4d6398", "#9a6b16", "#5c7c8a", "#b24b72", "#3b817a"
  ];
  const DASHES = ["", "8 4", "2 3", "11 4 2 4", "5 3 1.5 3"];
  const OP_ORDER = ["contains", "prefix", "suffix", "multi_contains", "contains_any"];

  const refs = Object.fromEntries([
    "source-select", "dataset-select", "op-select", "chunk-select", "x-metric",
    "y-metric", "bin-count", "focus-label", "focus-min", "focus-max", "focus-reset",
    "focus-error", "show-points", "show-band", "edit-labels",
    "label-editor", "title-input", "subtitle-input", "x-label-input",
    "y-label-input", "series-count", "series-search", "series-all", "series-none",
    "series-chips", "decode-section", "decode-all", "decode-none", "decode-chips",
    "plot", "plot-status", "tooltip", "export-scale", "export-png"
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
    title: DEFAULTS.title || "Benchmark explorer",
    subtitle: DEFAULTS.subtitle || "",
    xLabel: "selectivity (rows matched)",
    yLabel: "throughput (GB/s, raw payload)",
    xLabelCustom: false,
    yLabelCustom: false,
    ranges: {
      selectivity: {min: null, max: null},
      needle_len: {min: null, max: null},
    },
    visibleSeries: new Set(),
    visibleDecode: new Set(),
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

  function hash(text) {
    let value = 2166136261;
    for (let index = 0; index < text.length; index += 1) {
      value ^= text.charCodeAt(index);
      value = Math.imul(value, 16777619);
    }
    return value >>> 0;
  }

  function colorFor(key) {
    return PALETTE[hash(key) % PALETTE.length];
  }

  function dashFor(key) {
    return DASHES[hash(`${key}:dash`) % DASHES.length];
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

  function candidateKey(row) {
    return `${row.candidate}\u001f${row.config}`;
  }

  function candidateLabel(row) {
    const config = compactConfig(row.config);
    return config ? `${row.candidate} [${config}]` : row.candidate;
  }

  function seriesKey(row) {
    return [row.candidate, row.config, row.strategy, row.scanner || ""].join("\u001f");
  }

  function seriesMeta(row) {
    const scanner = row.scanner ? ` / ${row.scanner}` : "";
    return {
      id: seriesKey(row),
      candidateId: candidateKey(row),
      candidate: row.candidate,
      label: `${candidateLabel(row)} · ${row.strategy}${scanner}`,
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
    button.title = decode ? "Toggle decode-only baseline" : "Toggle query line";

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

  function formatX(value, precise = false) {
    if (state.xMetric === "selectivity") {
      if (value === 0) return "0";
      const percent = value * 100;
      if (precise) return `${formatSignificant(percent, 4)}%`;
      if (percent < 0.01) return `${percent.toExponential(0).replace("e+", "e")}%`;
      return `${formatSignificant(percent, 3)}%`;
    }
    return precise ? formatSignificant(value, 4) : formatSignificant(value, 3);
  }

  function formatY(value, precise = false) {
    const suffix = state.yMetric === "gbps" ? " GB/s" : " ns/row";
    return `${formatSignificant(value, precise ? 4 : 3)}${precise ? suffix : ""}`;
  }

  function rangeDisplayFactor() {
    return state.xMetric === "selectivity" ? 100 : 1;
  }

  function updateFocusControls() {
    const range = activeRange();
    const factor = rangeDisplayFactor();
    refs["focus-label"].textContent = state.xMetric === "selectivity"
      ? "Focus X (%)"
      : "Focus X (bytes)";
    refs["focus-min"].value = range.min === null ? "" : String(range.min * factor);
    refs["focus-max"].value = range.max === null ? "" : String(range.max * factor);
    refs["focus-min"].min = "0";
    refs["focus-max"].min = "0";
    refs["focus-error"].textContent = "";
    refs["focus-min"].removeAttribute("aria-invalid");
    refs["focus-max"].removeAttribute("aria-invalid");
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

  function renderPlot() {
    const rows = filteredRows();
    const catalog = catalogs(rows);
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
    const legendTop = state.subtitle ? 88 : 66;
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
    `);
    refs.plot.appendChild(style);
    refs.plot.appendChild(svgElement("rect", {width: "100%", height: "100%", fill: "#ffffff"}));
    refs.plot.appendChild(svgElement("text", {
      id: "plot-accessible-title", x: left, y: 35, "font-size": 22, "font-weight": 720,
      "letter-spacing": "-0.3"
    }, state.title));
    if (state.subtitle) {
      refs.plot.appendChild(svgElement("text", {
        x: left, y: 58, "font-size": 12, fill: "#71808c"
      }, state.subtitle));
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
    const yValues = selectedRows.map(row => yValue(row));
    selectedDecode.forEach(meta => {
      rows.filter(row => candidateKey(row) === meta.id).forEach(row => {
        const value = decodeValue(row);
        if (finite(value)) yValues.push(value);
      });
    });
    if (!yValues.length) domainRows.forEach(row => yValues.push(yValue(row)));
    const xScale = makeScale(xValues, state.xScale, left, chartRight, activeRange());
    const yScale = makeScale(yValues, state.yScale, chartBottom, chartTop);

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
      const aggregate = binPoints(raw);
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
        const stride = Math.max(1, Math.ceil(raw.length / 500));
        raw.filter((_point, index) => index % stride === 0).forEach(point => {
          const circle = svgElement("circle", {
            cx: xScale.map(point.x), cy: yScale.map(point.y), r: 2.1,
            fill: color, opacity: 0.18
          });
          attachTooltip(circle, [
            meta.label,
            `${state.xLabel}: ${formatX(point.x, true)}`,
            `${state.yLabel}: ${formatY(point.y, true)}`,
            point.row.query_id,
          ]);
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
        const circle = svgElement("circle", {
          cx: xScale.map(point.x), cy: yScale.map(point.y), r: 3.6,
          fill: color, stroke: "#ffffff", "stroke-width": 1.2
        });
        attachTooltip(circle, [
          meta.label,
          `median ${state.xLabel}: ${formatX(point.x, true)}`,
          `median ${state.yLabel}: ${formatY(point.y, true)}`,
          `IQR: ${formatY(point.q1, true)} – ${formatY(point.q3, true)} · n=${point.n}`,
        ]);
        marks.appendChild(circle);
      });
    });

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
      }, "Select a query line or decompression baseline above"));
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
      `${domainRows.length} measurements · ${selectedSeries.length} query lines · ${selectedDecode.length} decode baselines${rangeNote}${zeroNote}`;
    document.title = `${state.title} — benchmark explorer`;
  }

  function setDefaultAxisLabels() {
    if (!state.xLabelCustom) {
      state.xLabel = state.xMetric === "selectivity"
        ? "selectivity (rows matched)"
        : "needle length (bytes)";
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
    const catalog = catalogs(rows);
    reconcileVisibility(catalog);
    renderChips(catalog);
    updateScaleButtons();
    renderPlot();
  }

  function exportPng() {
    const button = refs["export-png"];
    const original = button.firstElementChild.textContent;
    button.disabled = true;
    button.firstElementChild.textContent = "Rendering…";

    const scale = Number(refs["export-scale"].value);
    const source = refs.plot.cloneNode(true);
    const viewBox = refs.plot.viewBox.baseVal;
    source.setAttribute("width", viewBox.width);
    source.setAttribute("height", viewBox.height);
    source.setAttribute("xmlns", NS);
    const xml = new XMLSerializer().serializeToString(source);
    const blob = new Blob([xml], {type: "image/svg+xml;charset=utf-8"});
    const url = URL.createObjectURL(blob);
    const image = new Image();

    const finish = () => {
      URL.revokeObjectURL(url);
      button.disabled = false;
      button.firstElementChild.textContent = original;
    };

    image.onload = () => {
      const canvas = document.createElement("canvas");
      canvas.width = Math.round(viewBox.width * scale);
      canvas.height = Math.round(viewBox.height * scale);
      const context = canvas.getContext("2d");
      context.setTransform(scale, 0, 0, scale, 0, 0);
      context.drawImage(image, 0, 0, viewBox.width, viewBox.height);
      canvas.toBlob(png => {
        if (png) {
          const link = document.createElement("a");
          const slug = state.title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "benchmark-plot";
          link.download = `${slug}-${viewBox.width * scale}px.png`;
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
      state.yMetric = refs["y-metric"].value; setDefaultAxisLabels(); rebuild();
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
      state.subtitle = refs["subtitle-input"].value; renderPlot();
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
    refs["export-png"].addEventListener("click", exportPng);
  }

  function init() {
    refs["title-input"].value = state.title;
    refs["subtitle-input"].value = state.subtitle;
    refs["x-metric"].value = state.xMetric;
    refs["y-metric"].value = state.yMetric;
    refs["bin-count"].value = String(state.bins);
    setDefaultAxisLabels();
    updateFocusControls();
    populateFilters(true);
    wireEvents();
    rebuild();
  }

  init();
})();
