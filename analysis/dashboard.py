import marimo

__generated_with = "0.23.16"
app = marimo.App(width="full")


@app.cell
def _():
    from contextlib import contextmanager
    from pathlib import Path

    import altair as alt
    import duckdb
    import marimo as mo
    import pandas as pd

    alt.data_transformers.disable_max_rows()

    DB = Path(__file__).resolve().parents[1] / "results" / "bench.duckdb"


    # A held read-only handle keeps a lock on the file, which stops the harness
    # from opening the db read-write to load a new run while this notebook is up.
    # So no cell keeps a connection: open one, run the cell's queries, close.
    # Opening costs ~0.3 ms, and every query below materialises (.df(), .fetchall(),
    # .fetchone()) before the handle goes away — never let a lazy relation escape
    # the `with` block.
    @contextmanager
    def open_db():
        con = duckdb.connect(str(DB), read_only=True)
        try:
            yield con
        finally:
            con.close()

    return alt, mo, open_db, pd


@app.cell
def _(mo):
    # A click bumps `reload_btn.value`, which is enough to re-run the run-list
    # query below. Nothing is cached across the click — the connection is opened per
    # query — so the refreshed list includes runs loaded into the db since this
    # notebook started.
    reload_btn = mo.ui.button(
        label="↻ refresh data", value=0, on_click=lambda v: v + 1
    )
    return (reload_btn,)


@app.cell
def _(mo, open_db, reload_btn):
    reload_btn.value  # a click on refresh re-runs this query

    with open_db() as _con:
        _runs = _con.sql(
            """
            select r.run_id, r.run_name, r.started_at
            from run r join result res on res.run_id = r.run_id
            group by 1, 2, 3
            order by r.started_at desc
            """
        ).fetchall()

    run_options = {
        f"{name} — {ts:%Y-%m-%d %H:%M}": rid for rid, name, ts in _runs
    }
    run_dd = mo.ui.dropdown(
        options=run_options,
        value=next(iter(run_options)),
        label="Run",
    )
    return (run_dd,)


@app.cell
def _(chunk_dd, len_ms, mo, needle_ms, reload_btn, run_dd):
    mo.vstack(
        [
            mo.hstack([run_dd, reload_btn], justify="start", align="center", gap=0.75),
            chunk_dd,
            len_ms,
            needle_ms,
        ],
        gap=0.4,
    )
    return


@app.cell
def _(mo, open_db, run_dd):
    run_id = run_dd.value

    with open_db() as _con:
        _m = _con.execute(
            """
            select p.hostname, p.os, p.arch, p.cpu_model, p.cores, p.cpu_features,
                   r.started_at, r.duration_s, r.git_commit, r.git_dirty,
                   r.rustc_version, r.cpu_governor, r.pinned_core,
                   r.pinning_effective, r.warmup, r.min_iters, r.min_millis
            from run r join platform p on p.platform_id = r.platform_id
            where r.run_id = ?
            """,
            [run_id],
        ).fetchone()
    (
        _host, _os, _arch, _cpu, _cores, _feats,
        _started, _dur, _commit, _dirty,
        _rustc, _gov, _pin, _pin_ok, _warmup, _iters, _millis,
    ) = _m

    machine_info = mo.md(
        f"""
    | machine | | run |  |
    |---|---|---|---|
    | host | `{_host}` | started | {_started:%Y-%m-%d %H:%M:%S} |
    | CPU | {_cpu} ({_cores} cores) | duration | {_dur:.0f} s |
    | arch / OS | {_arch} · {_os} | commit | `{(_commit or "?")[:12]}`{" (dirty)" if _dirty else ""} |
    | features | {", ".join(_feats)} | rustc | {_rustc or "?"} |
    | governor | {_gov or "n/a"} | warmup / min_iters | {_warmup} / {_iters} |
    | pinned core | {_pin} ({"effective" if _pin_ok else "not effective"}) | min_millis | {_millis} |
    """
    )
    return machine_info, run_id


@app.cell
def _(machine_info):
    machine_info
    return


@app.cell
def _(mo, open_db, run_id):
    # Row group sizes this run actually measured; 0 is the whole-column scan. The
    # options come from the data, so a run with any other row group size lists it
    # here without a code change.
    with open_db() as _con:
        _chunks = [
            r[0]
            for r in _con.execute(
                "select distinct chunk_rows from result where run_id = ? order by chunk_rows",
                [run_id],
            ).fetchall()
        ]

    _names = {c: ("whole column" if c == 0 else f"{c:,} rows") for c in _chunks}
    chunk_dd = mo.ui.dropdown(
        options={_names[c]: c for c in _chunks},
        value=_names[_chunks[0]],
        label="row group",
    )
    return (chunk_dd,)


@app.cell
def _(mo, open_db, run_id):
    # Needle lengths measured by this run, labelled with how many needles at that
    # length actually plot: a needle nothing matches has selectivity 0, and `_base`
    # drops those since they cannot sit on a log axis.
    with open_db() as _con:
        _lens = _con.execute(
            """
            select needle_len, count(distinct needle) as needles
            from v
            where run_id = ? and status = 'ok' and selectivity > 0
            group by 1 order by 1
            """,
            [run_id],
        ).fetchall()

    _len_names = {f"{ln} bytes — {n} needles": ln for ln, n in _lens}
    len_ms = mo.ui.multiselect(
        options=_len_names,
        value=list(_len_names),
        label="needle length",
    )
    return (len_ms,)


@app.cell
def _(len_ms, mo, open_db, run_id):
    # One place builds the needle-length SQL, because both this cell's option list
    # and the frame queries below need it. An empty selection still has to be valid
    # SQL, hence `and false` rather than an empty `in ()`.
    _lens = sorted(len_ms.value)
    len_filter = (
        f"and needle_len in ({', '.join(str(int(n)) for n in _lens)})"
        if _lens
        else "and false"
    )

    # The needles inside those lengths, labelled with length and the share of the
    # column each one matches. Selection keys on `query_id`, which is a plain
    # identifier 1:1 with the needle — the needle itself is raw bytes carrying
    # backslash escapes, and nothing stops one carrying a quote.
    with open_db() as _con:
        _needles = _con.execute(
            f"""
            select query_id, needle, needle_len, selectivity
            from v
            where run_id = ? and status = 'ok' and selectivity > 0
              {len_filter}
            group by 1, 2, 3, 4
            order by needle_len, selectivity
            """,
            [run_id],
        ).fetchall()

    needle_ms = mo.ui.multiselect(
        options={
            f"{_nd}  ·  {_ln} B  ·  {_sel * 100:.4g}%": _qid
            for _qid, _nd, _ln, _sel in _needles
        },
        value=[],
        label="needles (none = all)",
    )
    return len_filter, needle_ms


@app.cell
def _(chunk_dd, len_filter, needle_ms, open_db, run_id):
    chunk_rows = chunk_dd.value
    chunk_label = "whole column" if chunk_rows == 0 else f"{chunk_rows:,} rows"

    # No needles picked means every needle in the selected lengths, so the clause
    # just disappears.
    _needle_filter = (
        "and query_id in ({})".format(
            ", ".join(f"'{q}'" for q in sorted(needle_ms.value))
        )
        if needle_ms.value
        else ""
    )

    # Three bins per log decade: bin k spans [10^(k/3), 10^((k+1)/3)).
    _base = f"""
        select label, selectivity, gbps, needle, needle_len,
               cast(floor(log10(selectivity) * 3) as integer) as bin_idx
        from v
        where run_id = {run_id} and chunk_rows = {chunk_rows}
          and status = 'ok' and selectivity > 0 and gbps is not null
          {len_filter} {_needle_filter}
    """

    with open_db() as _con:
        raw = _con.sql(f"with base as ({_base}) select * from base").df()
        agg = _con.sql(
            f"""
            with base as ({_base})
            select label, bin_idx,
                   pow(10, (bin_idx + 0.5) / 3.0) as center,
                   median(gbps) as med,
                   quantile_cont(gbps, 0.25) as q25,
                   quantile_cont(gbps, 0.75) as q75,
                   count(*) as n
            from base group by 1, 2 order by 1, 2
            """
        ).df()
        meta = _con.sql(
            f"""
            select label, candidate, strategy, coalesce(scanner, '') as scanner,
                   candidate_version, config,
                   median(compression_ratio) as cr, median(build_ns) / 1e6 as build_ms,
                   median(prune_rate) as prune, median(false_positive_rate) as fp
            from v
            where run_id = {run_id} and chunk_rows = {chunk_rows} and status = 'ok'
              and selectivity > 0
              {len_filter} {_needle_filter}
            group by 1, 2, 3, 4, 5, 6
            """
        ).df()
        edges = _con.sql(
            f"""
            with base as ({_base}),
                 b as (select min(bin_idx) lo, max(bin_idx) hi from base)
            select pow(10, k / 3.0) as edge
            from b, range((select lo from b), (select hi from b) + 2) t(k)
            """
        ).df()

    labels = sorted(raw["label"].unique().tolist())
    return agg, chunk_label, edges, labels, meta, raw


@app.cell
def _(labels, meta, raw):
    # Vega's tableau20, spelled out so the chart's colours and the swatches in
    # the kernel table below it come from one source.
    PALETTE = [
        "#4c78a8", "#9ecae9", "#f58518", "#ffbf79", "#54a24b",
        "#88d27a", "#b79a20", "#f2cf5b", "#439894", "#83bcb6",
        "#e45756", "#ff9d98", "#79706e", "#bab0ac", "#d67195",
        "#fcbfd2", "#b279a2", "#d6a5c9", "#9e765f", "#d8b5a5",
    ]
    colors = {name: PALETTE[i % len(PALETTE)] for i, name in enumerate(labels)}

    # Chips carry the candidate name alone; a candidate that exposes more than
    # one kernel keeps the distinguishing half so no two chips read the same.
    _by_label = meta.set_index("label")
    _times = _by_label.loc[labels, "candidate"].value_counts()
    short = {
        name: (
            _by_label.at[name, "candidate"]
            if _times[_by_label.at[name, "candidate"]] == 1
            else name.replace("/", " · ", 1)
        )
        for name in labels
    }

    # Mean throughput over every query of the run. `raw` is already filtered to the
    # selected row group, so the chip order re-sorts whenever `chunk_dd` changes.
    speed = raw.groupby("label")["gbps"].mean().to_dict()
    return colors, short, speed


@app.cell
def _(mo):
    # The selection must outlive a run / row group / needle length change, so this
    # state cannot depend on `labels` — a cell that reads `labels` re-runs whenever
    # the queries do, rebuilding the state and wiping the selection. `None` is the
    # initial "all of them", resolved against the live `labels` in the chip cell.
    # allow_self_loops: the chip cell both reads this state and sets it from the
    # chips' on_change. Without it marimo skips re-running the setting cell, so
    # the buttons keep the `kind` they were built with and never change colour.
    get_sel, set_sel = mo.state(None, allow_self_loops=True)
    return get_sel, set_sel


@app.cell
def _(chunk_label, get_sel, labels, mo, set_sel, short, speed):
    # `None` means every algorithm. Resolving against the current `labels` is what
    # survives a query change: labels that went away drop out of the selection,
    # the rest stay picked.
    _sel = get_sel()
    picked = sorted(labels) if _sel is None else sorted(_sel & set(labels))
    _picked = set(picked)


    def _toggle(name):
        def _on_change(_):
            current = set(labels) if get_sel() is None else set(get_sel())
            current.symmetric_difference_update({name})
            set_sel(current)

        return _on_change


    _all_none = mo.hstack(
        [
            mo.ui.button(label="select all", on_change=lambda _: set_sel(None)),
            mo.ui.button(label="unselect all", on_change=lambda _: set_sel(set())),
        ],
        justify="start",
        gap=0.35,
    )

    # Fastest first, filling each column top to bottom before starting the next.
    _ranked = sorted(labels, key=lambda name: -speed[name])
    _per_col = max(1, -(-len(_ranked) // 3))
    _cols = [_ranked[i : i + _per_col] for i in range(0, len(_ranked), _per_col)]
    _grid = mo.hstack(
        [
            mo.vstack(
                [
                    mo.ui.button(
                        label=f"{short[name]} · {speed[name]:.1f}",
                        kind="success" if name in _picked else "neutral",
                        on_change=_toggle(name),
                        full_width=True,
                    )
                    for name in _col
                ],
                gap=0.35,
                align="stretch",
            )
            for _col in _cols
        ],
        justify="start",
        align="start",
        gap=0.35,
        widths="equal",
    )
    chips = mo.vstack(
        [
            _all_none,
            mo.md(f"mean GB/s at row group **{chunk_label}** — fastest first"),
            _grid,
        ],
        gap=0.4,
    )
    return chips, picked


@app.cell
def _(mo):
    x_scale = mo.ui.radio(
        options=["log", "linear"], value="log", inline=True, label="x axis"
    )
    return (x_scale,)


@app.cell
def _(chips, mo, x_scale):
    mo.vstack([chips, x_scale], gap=0.6)
    return


@app.cell
def _(agg, alt, chunk_label, colors, edges, mo, picked, raw, short, x_scale):
    # An empty needle-length selection empties `raw`, which empties `labels` and so
    # the chip selection too — check it first or that reads as "no candidate".
    if raw.empty:
        chart = mo.md("*No needle length selected — pick one above.*")
    elif not picked:
        chart = mo.md("*No candidate selected — click a chip above.*")
    else:
        _agg = agg[agg["label"].isin(picked)].assign(
            series=lambda d: d["label"].map(short)
        )
        _raw = raw[raw["label"].isin(picked)].assign(
            series=lambda d: d["label"].map(short)
        )

        # Domain is the selection, not every label, so the legend lists only what
        # is plotted; `colors` is keyed on label, so each series keeps its colour
        # regardless of what else is toggled on.
        _color = alt.Color(
            "series:N",
            scale=alt.Scale(
                domain=[short[n] for n in picked],
                range=[colors[n] for n in picked],
            ),
            legend=alt.Legend(title="algorithm", columns=1),
        )
        _x = alt.X(
            "center:Q",
            scale=alt.Scale(type=x_scale.value),
            title="pattern selectivity",
            axis=alt.Axis(grid=False, format=".5~%"),
        )
        _y = alt.Y("q25:Q", title="GB/s per evaluation")

        _rules = (
            alt.Chart(edges)
            .mark_rule(color="#8a8a8a", strokeDash=[3, 3], opacity=0.7)
            .encode(x=alt.X("edge:Q", scale=alt.Scale(type=x_scale.value)))
        )
        _band = (
            alt.Chart(_agg)
            .mark_area(opacity=0.18)
            .encode(x=_x, y=_y, y2="q75:Q", color=_color)
        )
        # The line is binned, so its tooltip can only report the bin centre. The
        # points are one query each: hovering one gives that query's own
        # selectivity rather than the bin it landed in.
        _points = (
            alt.Chart(_raw)
            .mark_point(size=9, opacity=0.7, filled=True)
            .encode(
                x=alt.X("selectivity:Q", scale=alt.Scale(type=x_scale.value)),
                y=alt.Y("gbps:Q"),
                color=_color,
                tooltip=[
                    alt.Tooltip("series:N", title="algorithm"),
                    alt.Tooltip("needle:N", title="needle"),
                    alt.Tooltip("needle_len:Q", title="needle bytes"),
                    alt.Tooltip("selectivity:Q", title="selectivity", format=".5~%"),
                    alt.Tooltip("gbps:Q", title="GB/s", format=".2f"),
                ],
            )
        )
        _line = (
            alt.Chart(_agg)
            .mark_line(point=True, strokeWidth=2)
            .encode(
                x=_x,
                y=alt.Y("med:Q"),
                color=_color,
                tooltip=[
                    alt.Tooltip("series:N", title="algorithm"),
                    alt.Tooltip("center:Q", title="bin centre", format=".5~%"),
                    alt.Tooltip("med:Q", title="median GB/s", format=".2f"),
                    alt.Tooltip("q25:Q", title="q25", format=".2f"),
                    alt.Tooltip("q75:Q", title="q75", format=".2f"),
                    alt.Tooltip("n:Q", title="queries"),
                ],
            )
        )

        chart = mo.ui.altair_chart(
            (_rules + _band + _points + _line)
            .properties(
                width=820,
                height=460,
                title=f"row group = {chunk_label} · 3 bins per log decade",
            )
            .resolve_scale(color="shared"),
            chart_selection=False,
            legend_selection=False,
        )
    return (chart,)


@app.cell
def _(chart):
    chart
    return


@app.cell
def _(agg, colors, meta, mo, pd, picked, short):
    # `prune` / `fp` are NULL for every kernel that reports no prefilter counters,
    # and `fp` alone is NULL on a query where nothing survived the prefilter (no
    # denominator), so both print as an em dash rather than a number.
    #
    # Prune rates run to 0.999857 (143 survivors out of a million), and any plain
    # rounding prints that as a flat "100%" — which reads as "pruned everything"
    # when 143 values did survive. 100% is reserved for a rate that really is 1.0.
    def _pct(x):
        if pd.isna(x):
            return "—"
        if x < 1 and x * 100 >= 99.995:
            return ">99.99%"
        return f"{x * 100:.4g}%"


    _n = agg.groupby("label")["n"].sum().to_dict()
    _rows = []
    for _name in picked:
        _m = meta[meta["label"] == _name].iloc[0]
        _rows.append(
            "<tr>"
            f'<td><span style="display:inline-block;width:.75rem;height:.75rem;'
            f'border-radius:2px;background:{colors[_name]}"></span></td>'
            f"<td><b>{short[_name]}</b></td>"
            f"<td><code>{_m.strategy}</code></td>"
            f"<td><code>{_m.scanner or '—'}</code></td>"
            f"<td><code>{_m.candidate_version}</code></td>"
            f"<td><code>{_m.config}</code></td>"
            f"<td style='text-align:right'>{_m.cr:.2f}×</td>"
            f"<td style='text-align:right'>{_m.build_ms:.0f} ms</td>"
            f"<td style='text-align:right'>{_pct(_m.prune)}</td>"
            f"<td style='text-align:right'>{_pct(_m.fp)}</td>"
            f"<td style='text-align:right'>{_n.get(_name, 0)}</td>"
            "</tr>"
        )
    # Columns collided at the default table width: cells had no padding and the
    # table only claimed its content width. A class-scoped <style> keeps the rules
    # in one place, `nowrap` stops a long config or version wrapping into its
    # neighbour, and the wrapper scrolls rather than squeezing on a narrow window.
    _css = (
        "<style>"
        ".kerneltbl{font-size:.85rem;border-collapse:collapse;width:100%;"
        "min-width:52rem}"
        ".kerneltbl th,.kerneltbl td{padding:.25rem .8rem;white-space:nowrap}"
        ".kerneltbl thead th{font-weight:500;opacity:.7;"
        "border-bottom:1px solid currentColor}"
        ".kerneltbl tbody tr:nth-child(even){background:rgba(127,127,127,.08)}"
        "</style>"
    )
    kernels = mo.Html(
        _css
        + "<div style='overflow-x:auto'>"
        "<table class='kerneltbl'>"
        "<thead><tr>"
        "<th></th><th style='text-align:left'>kernel</th>"
        "<th style='text-align:left'>strategy</th>"
        "<th style='text-align:left'>scanner</th>"
        "<th style='text-align:left'>version</th>"
        "<th style='text-align:left'>config</th>"
        "<th>ratio</th><th>build</th><th>pruned</th><th>false pos</th>"
        "<th>queries</th>"
        "</tr></thead><tbody>"
        + "".join(_rows)
        + "</tbody></table></div>"
    )
    return (kernels,)


@app.cell
def _(kernels):
    kernels
    return


if __name__ == "__main__":
    app.run()
