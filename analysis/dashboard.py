import marimo

__generated_with = "0.23.16"
app = marimo.App(width="medium")


@app.cell
def _():
    from pathlib import Path

    import altair as alt
    import duckdb
    import marimo as mo

    alt.data_transformers.disable_max_rows()

    DB = Path(__file__).resolve().parents[1] / "results" / "bench.duckdb"
    con = duckdb.connect(str(DB), read_only=True)
    return alt, con, mo


@app.cell
def _(con, mo):
    _runs = con.sql(
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
def _(run_dd):
    run_dd
    return


@app.cell
def _(con, mo, run_dd):
    run_id = run_dd.value

    _m = con.execute(
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
def _(con, run_id):
    # Whole-column when the run has it, else its smallest chunk size.
    _chunks = [
        r[0]
        for r in con.execute(
            "select distinct chunk_rows from result where run_id = ? order by chunk_rows",
            [run_id],
        ).fetchall()
    ]
    chunk_rows = 0 if 0 in _chunks else _chunks[0]

    # Three bins per log decade: bin k spans [10^(k/3), 10^((k+1)/3)).
    _base = f"""
        select label, selectivity, gbps,
               cast(floor(log10(selectivity) * 3) as integer) as bin_idx
        from v
        where run_id = {run_id} and chunk_rows = {chunk_rows}
          and status = 'ok' and selectivity > 0 and gbps is not null
    """

    raw = con.sql(f"with base as ({_base}) select * from base").df()
    agg = con.sql(
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

    labels = sorted(raw["label"].unique().tolist())
    meta = con.sql(
        f"""
        select label, candidate, strategy, coalesce(scanner, '') as scanner,
               candidate_version, config,
               median(compression_ratio) as cr, median(build_ns) / 1e6 as build_ms
        from v
        where run_id = {run_id} and chunk_rows = {chunk_rows} and status = 'ok'
        group by 1, 2, 3, 4, 5, 6
        """
    ).df()
    edges = con.sql(
        f"""
        with base as ({_base}),
             b as (select min(bin_idx) lo, max(bin_idx) hi from base)
        select pow(10, k / 3.0) as edge
        from b, range((select lo from b), (select hi from b) + 2) t(k)
        """
    ).df()
    return agg, chunk_rows, edges, labels, meta, raw


@app.cell
def _(labels, meta):
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
    return colors, short


@app.cell
def _(labels, mo):
    # allow_self_loops: the chip cell both reads this state and sets it from the
    # chips' on_change. Without it marimo skips re-running the setting cell, so
    # the buttons keep the `kind` they were built with and never change colour.
    get_sel, set_sel = mo.state(set(labels), allow_self_loops=True)
    return get_sel, set_sel


@app.cell
def _(get_sel, labels, mo, set_sel, short):
    def _toggle(name):
        def _on_change(_):
            picked = set(get_sel())
            picked.symmetric_difference_update({name})
            set_sel(picked)

        return _on_change

    _picked = get_sel()
    _all_none = mo.hstack(
        [
            mo.ui.button(
                label="select all", on_change=lambda _: set_sel(set(labels))
            ),
            mo.ui.button(label="unselect all", on_change=lambda _: set_sel(set())),
        ],
        justify="start",
        gap=0.35,
    )
    _pills = mo.hstack(
        [
            mo.ui.button(
                label=short[name],
                kind="success" if name in _picked else "neutral",
                on_change=_toggle(name),
            )
            for name in labels
        ],
        justify="start",
        wrap=True,
        gap=0.35,
    )
    chips = mo.vstack([_all_none, _pills], gap=0.4)
    return (chips,)


@app.cell
def _(mo):
    x_scale = mo.ui.radio(
        options=["log", "linear"], value="log", inline=True, label="x axis"
    )
    return (x_scale,)


@app.cell
def _(chips, mo, x_scale):
    mo.vstack([chips, x_scale])
    return


@app.cell
def _(agg, alt, chunk_rows, colors, edges, get_sel, labels, mo, raw, short, x_scale):
    picked = sorted(get_sel())

    if not picked:
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
            axis=alt.Axis(grid=False, format=".1e"),
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
        _points = (
            alt.Chart(_raw)
            .mark_point(size=9, opacity=0.28, filled=True)
            .encode(
                x=alt.X("selectivity:Q", scale=alt.Scale(type=x_scale.value)),
                y=alt.Y("gbps:Q"),
                color=_color,
            )
        )
        _line = (
            alt.Chart(_agg)
            .mark_line(point=True, strokeWidth=2)
            .encode(
                x=_x,
                y=alt.Y("med:Q"),
                color=_color,
                tooltip=["series:N", "center:Q", "med:Q", "q25:Q", "q75:Q", "n:Q"],
            )
        )

        chart = mo.ui.altair_chart(
            (_rules + _band + _points + _line)
            .properties(
                width=820,
                height=460,
                title=f"chunk_rows = {chunk_rows} · 3 bins per log decade",
            )
            .resolve_scale(color="shared"),
            chart_selection=False,
            legend_selection=False,
        )
    return chart, picked


@app.cell
def _(chart):
    chart
    return


@app.cell
def _(agg, colors, meta, mo, picked, short):
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
            f"<td style='text-align:right'>{_n.get(_name, 0)}</td>"
            "</tr>"
        )
    kernels = mo.Html(
        "<table style='font-size:.85rem;border-collapse:collapse'>"
        "<thead><tr>"
        "<th></th><th style='text-align:left'>kernel</th>"
        "<th style='text-align:left'>strategy</th>"
        "<th style='text-align:left'>scanner</th>"
        "<th style='text-align:left'>version</th>"
        "<th style='text-align:left'>config</th>"
        "<th>ratio</th><th>build</th><th>queries</th>"
        "</tr></thead><tbody>" + "".join(_rows) + "</tbody></table>"
    )
    return (kernels,)


@app.cell
def _(kernels):
    kernels
    return


if __name__ == "__main__":
    app.run()
