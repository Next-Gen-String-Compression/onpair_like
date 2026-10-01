import marimo

__generated_with = "0.23.16"
app = marimo.App(width="full", auto_download=["html"])


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


    # Shared by the kernel table and the scatter tooltip, so a rate reads the same
    # in both. NULL means the kernel reports no prefilter counters, or that nothing
    # survived and the false-positive rate has no denominator. Prune rates run to
    # 0.999857 — 143 survivors out of a million — and plain rounding prints that as
    # a flat "100%", which reads as "pruned everything"; 100% is reserved for a rate
    # that really is 1.0.
    def pct(x):
        if pd.isna(x):
            return "—"
        if x < 1 and x * 100 >= 99.995:
            return ">99.99%"
        return f"{x * 100:.4g}%"


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

    return alt, mo, open_db, pct, pd


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
def _(mo):
    # Fixed stops, not a continuous range: selectivity spans five decades, so the
    # cuts worth making are order-of-magnitude ones. The value caps how much of the
    # column a needle may match, and reaches every query below via `sel_filter`.
    sel_max = mo.ui.slider(
        steps=[0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0],
        value=1.0,
        label="max selectivity",
        show_value=True,
        debounce=True,
    )
    return (sel_max,)


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
def _(mo, open_db, run_id):
    # A run may span several columns (specs/literals-extended.tmp.toml pairs five).
    # Everything below scopes to the picked ones, so a needle ladder from one
    # dataset never sits in the same bin as another's.
    with open_db() as _con:
        _dss = _con.execute(
            """
            select dataset, count(distinct needle) as needles
            from v
            where run_id = ? and status = 'ok' and selectivity > 0
            group by 1 order by 1
            """,
            [run_id],
        ).fetchall()

    _ds_names = {f"{_d} — {_n} needles": _d for _d, _n in _dss}
    ds_ms = mo.ui.multiselect(
        options=_ds_names,
        value=list(_ds_names),
        label="dataset",
    )
    return (ds_ms,)


@app.cell
def _(chunk_dd, ds_ms, len_ms, mo, needle_ms, reload_btn, run_dd, sel_max):
    # Everything that scopes the data, narrowing left to right down the stack:
    # which run, which row group, which datasets, how selective a needle may be,
    # which needle lengths, and finally individual needles.
    mo.vstack(
        [
            mo.hstack([run_dd, reload_btn], justify="start", align="center", gap=0.75),
            chunk_dd,
            ds_ms,
            sel_max,
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
def _(ds_ms, mo, open_db, run_id, sel_max):
    # One place builds the selectivity cap, because every query from here down has
    # to agree on which needles are in scope.
    sel_filter = f"and selectivity <= {sel_max.value}"

    # Same rule for the dataset scope. An empty selection is `and false`, not an
    # empty `in ()`, which is not valid SQL.
    ds_filter = (
        "and dataset in ({})".format(", ".join(f"'{d}'" for d in sorted(ds_ms.value)))
        if ds_ms.value
        else "and false"
    )

    # Needle lengths measured by this run, labelled with how many needles at that
    # length actually plot: a needle nothing matches has selectivity 0, and `_base`
    # drops those since they cannot sit on a log axis.
    with open_db() as _con:
        _lens = _con.execute(
            f"""
            select needle_len, count(distinct needle) as needles
            from v
            where run_id = ? and status = 'ok' and selectivity > 0
              {ds_filter} {sel_filter}
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
    return ds_filter, len_ms, sel_filter


@app.cell
def _(ds_filter, len_ms, mo, open_db, run_id, sel_filter):
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
            select query_id, dataset, needle, needle_len, selectivity
            from v
            where run_id = ? and status = 'ok' and selectivity > 0
              {ds_filter} {len_filter} {sel_filter}
            group by 1, 2, 3, 4, 5
            order by dataset, needle_len, selectivity
            """,
            [run_id],
        ).fetchall()

    needle_ms = mo.ui.multiselect(
        options={
            f"{_nd}  ·  {_ln} B  ·  {_sel * 100:.4g}%  ·  {_ds}": _qid
            for _qid, _ds, _nd, _ln, _sel in _needles
        },
        value=[],
        label="needles (none = all)",
    )
    return len_filter, needle_ms


@app.cell
def _(
    chunk_dd,
    ds_filter,
    len_filter,
    needle_ms,
    open_db,
    run_id,
    sel_filter,
    splits_sl,
):
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

    # The plotted selectivity range cut into `splits` equal slices in log space,
    # rather than a fixed grid: the slices follow whatever the filters left, so
    # one of them is one slice over everything.
    _splits = splits_sl.value
    _base = f"""
        with pts as (
            select label, query_id, selectivity, gbps, needle, needle_len,
                   dataset, prune_rate
            from v
            where run_id = {run_id} and chunk_rows = {chunk_rows}
              and status = 'ok' and selectivity > 0 and gbps is not null
              {ds_filter} {len_filter} {sel_filter} {_needle_filter}
        ),
        span as (
            select min(log10(selectivity)) as lo,
                   (max(log10(selectivity)) - min(log10(selectivity)))
                       / {_splits} as width
            from pts
        ),
        binned as (
            -- `least` puts the top of the range in the last slice rather than in
            -- one past it; `nullif` is the single-selectivity case, where every
            -- row is slice 0 and the centre lands on the one value there is.
            select pts.*, span.lo, span.width,
                   least(
                       {_splits} - 1,
                       coalesce(
                           cast(
                               floor((log10(selectivity) - span.lo)
                                     / nullif(span.width, 0)) as integer
                           ),
                           0
                       )
                   ) as bin_idx
            from pts, span
        )
        select *, pow(10, lo + (bin_idx + 0.5) * width) as center from binned
    """

    with open_db() as _con:
        raw = _con.sql(f"with base as ({_base}) select * from base").df()
        agg = _con.sql(
            f"""
            with base as ({_base})
            select label, bin_idx, min(center) as center,
                   median(gbps) as med,
                   avg(gbps) as amean,
                   -- Total bytes over total time: every query in a bucket walks
                   -- the same payload, so GB/s is a rescaled 1/time and only the
                   -- harmonic mean composes back into a throughput.
                   count(*) / sum(1.0 / gbps) as hmean,
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
                   median(gbps) as gbps,
                   -- Over the bytes the kernel actually walked. `gbps` is over
                   -- payload bytes, `compression_ratio` over dataset.raw_bytes,
                   -- and the two denominators differ by ~9% on clickbench --
                   -- so this divides by the footprint rather than by the ratio.
                   median(gbps * compressed_bytes / nullif(payload_mb * 1e6, 0))
                       as comp_gbps,
                   median(prune_rate) as prune, median(false_positive_rate) as fp,
                   -- Where a cell's time went, for the modules that report it.
                   -- Single-shot instrumented timings, so the share is the honest
                   -- summary: the absolute nanoseconds are not comparable to the
                   -- sampled median beside them.
                   median(prefilter_ns) as pf_ns, median(verify_ns) as ver_ns
            from v
            where run_id = {run_id} and chunk_rows = {chunk_rows} and status = 'ok'
              and selectivity > 0
              {ds_filter} {len_filter} {sel_filter} {_needle_filter}
            group by 1, 2, 3, 4, 5, 6
            """
        ).df()
        edges = _con.sql(
            f"""
            with base as ({_base}),
                 s as (select distinct lo, width from base)
            select pow(10, lo + k * width) as edge
            from s, range(0, {_splits} + 1) t(k)
            """
        ).df()

    labels = sorted(raw["label"].unique().tolist())
    return agg, chunk_label, edges, labels, meta, raw


@app.cell
def _(labels, meta, raw):
    # `dict_onpair` is OnPair run over a dictionary rather than a second
    # algorithm, so it belongs under the same heading as `onpair` instead of a
    # column away from it under "d".
    _by_label = meta.set_index("label")
    _family = {
        name: _by_label.at[name, "candidate"].removeprefix("dict_")
        for name in labels
    }

    # Mean throughput over every query of the run. `raw` is already filtered to the
    # selected row group, so the chip order re-sorts whenever `chunk_dd` changes.
    speed = raw.groupby("label")["gbps"].mean().to_dict()

    # Fastest kernel first, which orders the families too: a family takes the
    # place of its fastest member.
    groups = {}
    for _name in sorted(labels, key=lambda n: -speed[n]):
        groups.setdefault(_family[_name], []).append(_name)

    # The legend and the kernel table name a kernel by its candidate alone; a
    # candidate that exposes more than one kernel keeps the distinguishing half
    # so no two rows read the same.
    _times = _by_label.loc[labels, "candidate"].value_counts()

    # `bits=16,prune=1.0` reads as a config of its own; `bits=16 +prune` reads as
    # the variant it is, which is what the shared hue is saying too.
    def _pretty(text):
        head, sep, _ = text.partition(",prune=")
        return f"{head} +prune" if sep else text

    short = {
        name: _pretty(
            _by_label.at[name, "candidate"]
            if _times[_by_label.at[name, "candidate"]] == 1
            else name.replace("/", " · ", 1)
        )
        for name in labels
    }

    # A chip sits under its family heading, so it only has to carry what tells it
    # apart from its siblings: the strategy, and whatever config the run pinned.
    chip = {name: _pretty(name.split("/", 1)[1]) for name in labels}
    return chip, groups, short, speed


@app.cell
def _(groups, labels, picked):
    # Vega's tableau20, spelled out so the chart's colours and the swatches in the
    # kernel table below it come from one source.
    PALETTE = [
        "#4c78a8", "#9ecae9", "#f58518", "#ffbf79", "#54a24b",
        "#88d27a", "#b79a20", "#f2cf5b", "#439894", "#83bcb6",
        "#e45756", "#ff9d98", "#79706e", "#bab0ac", "#d67195",
        "#fcbfd2", "#b279a2", "#d6a5c9", "#9e765f", "#d8b5a5",
    ]
    # Adjacent entries are two shades of one hue, so the darks come first and the
    # lights after: ten distinct hues before any hue is used twice. Handing them
    # out family by family put a family's kernels on neighbouring shades, which is
    # tidy in the legend and unreadable on the plot.
    HUES = PALETTE[0::2] + PALETTE[1::2]

    # A pruned build is the same kernel with one knob turned, so it takes its
    # base's hue rather than an unrelated one: same colour, mixed toward white.
    # It only counts as a variant when the base is present too — on its own it is
    # just another kernel and gets a hue like any other.
    def _base_of(name):
        head, sep, cfg = name.partition("@")
        if not sep or "prune=" not in cfg:
            return None
        kept = [part for part in cfg.split(",") if not part.startswith("prune=")]
        return f"{head}@{','.join(kept)}" if kept else head

    def _lighter(hex_color, amount=0.45):
        channels = (int(hex_color[at : at + 2], 16) for at in (1, 3, 5))
        return "#" + "".join(
            f"{round(value + (255 - value) * amount):02x}" for value in channels
        )

    # Selected kernels take their hues first, in speed order, so what is on the
    # plot is as far apart in colour as the palette allows. The rest still get one
    # — a colour has to exist for every label — but they take what is left, and a
    # kernel's colour therefore moves when the selection does.
    _by_speed = [name for _members in groups.values() for name in _members]
    _live = set(picked)
    _ordered = [n for n in _by_speed if n in _live] + [
        n for n in _by_speed if n not in _live
    ]
    _variants = {
        name: base
        for name, base in ((n, _base_of(n)) for n in _ordered)
        if base in set(labels)
    }
    colors = {}
    _next = 0
    for _name in _ordered:
        if _name in _variants:
            continue
        colors[_name] = HUES[_next % len(HUES)]
        _next += 1
    for _name, _base in _variants.items():
        colors[_name] = _lighter(colors[_base])
    return (colors,)


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
def _(chip, chunk_label, get_sel, groups, labels, mo, set_sel, short, speed):
    # `None` means every algorithm. Resolving against the current `labels` is what
    # survives a query change: labels that went away drop out of the selection,
    # the rest stay picked.
    _sel = get_sel()
    picked = sorted(labels) if _sel is None else sorted(_sel & set(labels))
    _picked = set(picked)


    # One handler for a chip and for a family heading alike: a heading whose family
    # is already whole clears it, anything else fills in what is missing. On a
    # single chip that is the old flip.
    def _toggle(names):
        def _on_change(_):
            current = set(labels) if get_sel() is None else set(get_sel())
            set_sel(current - names if names <= current else current | names)

        return _on_change


    _all_none = mo.hstack(
        [
            mo.ui.button(label="select all", on_change=lambda _: set_sel(None)),
            mo.ui.button(label="unselect all", on_change=lambda _: set_sel(set())),
        ],
        justify="start",
        gap=0.35,
    )


    def _block(fam, members):
        # A family that contributes one kernel needs no heading over a single
        # chip, so there the heading is the chip — and it is named for the kernel,
        # since a lone `dict_` kernel must not read as its row-domain twin.
        _lone = len(members) == 1
        _head = mo.ui.button(
            label=f"{short[members[0]]} · {speed[members[0]]:.1f}" if _lone else fam,
            kind="success" if set(members) <= _picked else "neutral",
            on_change=_toggle(set(members)),
            full_width=True,
        )
        _kernels = [] if _lone else [
            mo.ui.button(
                label=f"{chip[name]} · {speed[name]:.1f}",
                kind="success" if name in _picked else "neutral",
                on_change=_toggle({name}),
                full_width=True,
            )
            for name in members
        ]
        return mo.vstack([_head, *_kernels], gap=0.15, align="stretch")


    # Fastest family first, each one landing in whichever column is shortest so
    # far — a family is a block of rows, so equal-sized columns would leave the
    # last one with three families and the first with one.
    _cols = [[], [], []]
    _height = [0, 0, 0]
    for _fam, _members in groups.items():
        _i = _height.index(min(_height))
        _cols[_i].append(_block(_fam, _members))
        _height[_i] += 1 if len(_members) == 1 else len(_members) + 1
    _grid = mo.hstack(
        [
            mo.vstack(_col, gap=0.5, align="stretch")
            for _col in _cols
            if _col
        ],
        justify="start",
        align="start",
        gap=0.35,
        widths="equal",
    )
    chips = mo.vstack(
        [
            _all_none,
            mo.md(
                f"mean GB/s at row group **{chunk_label}** — fastest first; a "
                "heading toggles the whole family"
            ),
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
    # Equal slices of the plotted selectivity range, in log space. One slice
    # covers everything, so the line is the overall median drawn flat under the
    # scatter; more slices follow the dots more closely with fewer queries behind
    # each point, until a slice holds one query and the line is the scatter.
    splits_sl = mo.ui.slider(1, 16, value=8, label="splits", show_value=True)
    # What the line through a bucket is. The harmonic mean is the only one that
    # composes back into a throughput -- it is total bytes over total time --
    # while the median says what a typical needle in the bucket did and the
    # arithmetic mean of a rate is dominated by the cheapest query in it.
    metric_dd = mo.ui.dropdown(
        options={"harmonic mean": "hmean", "median": "med", "mean": "amean"},
        value="harmonic mean",
        label="line",
    )
    # Throughput spans two orders of magnitude across a selectivity ladder, so a
    # linear y flattens everything below the fastest needle. Linear stays the
    # default: it is the one that reads as a ratio.
    y_scale = mo.ui.radio(
        options=["linear", "log"], value="linear", inline=True, label="y axis"
    )
    # The band is the spread the line hid, which is worth seeing until several
    # kernels overlap -- then four translucent areas over each other read as one
    # muddy shape and hide the dots underneath.
    band_cb = mo.ui.checkbox(value=True, label="q25-q75 band")
    return band_cb, metric_dd, splits_sl, x_scale, y_scale


@app.cell
def _(band_cb, chips, metric_dd, mo, splits_sl, x_scale, y_scale):
    mo.vstack(
        [
            chips,
            mo.hstack(
                [x_scale, y_scale, splits_sl, metric_dd, band_cb],
                justify="start",
                gap=2,
            ),
        ],
        gap=0.6,
    )
    return


@app.cell
def _(
    agg,
    alt,
    band_cb,
    chunk_label,
    colors,
    edges,
    metric_dd,
    mo,
    pct,
    pd,
    picked,
    raw,
    short,
    splits_sl,
    x_scale,
    y_scale,
):
    # An empty needle-length selection empties `raw`, which empties `labels` and so
    # the chip selection too — check it first or that reads as "no candidate".
    if raw.empty:
        chart = mo.md("*No needle length selected — pick one above.*")
        points = raw
    elif not picked:
        chart = mo.md("*No candidate selected — click a chip above.*")
        points = raw.iloc[:0]
    else:
        _agg = agg[agg["label"].isin(picked)].assign(
            series=lambda d: d["label"].map(short)
        )
        # `prune_rate` is per (kernel, query) — the same grain as one dot — and NULL
        # for every kernel that reports no prefilter counters.
        _raw = raw[raw["label"].isin(picked)].assign(
            series=lambda d: d["label"].map(short),
            pruned=lambda d: d["prune_rate"].map(pct),
        )

        # The line and band run between bin centres, so at either end they stop
        # short of the outermost dot, or run past it when the bin's centre falls
        # outside its own points. Both ends get carried to the series' extreme
        # query, keeping that bin's median and quartiles: the same summary, read
        # at the edge of the data rather than at the centre of a bin.
        _edges = _raw.groupby("label")["selectivity"].agg(["min", "max"])
        _ends = []
        for _name, (_lo, _hi) in _edges.iterrows():
            _bins = _agg[_agg["label"] == _name].sort_values("bin_idx")
            if _bins.empty:
                continue
            if _lo < _bins.iloc[0]["center"]:
                _ends.append({**_bins.iloc[0], "center": _lo})
            if _hi > _bins.iloc[-1]["center"]:
                _ends.append({**_bins.iloc[-1], "center": _hi})
        if _ends:
            _agg = pd.concat([_agg, pd.DataFrame(_ends)], ignore_index=True)
        # An end bin whose centre sits past its own outermost point would draw
        # the line into empty space, which is the same defect the other way up.
        _bounds = _agg.join(_edges, on="label")
        _agg = _agg.assign(
            center=_agg["center"].clip(
                lower=_bounds["min"].to_numpy(), upper=_bounds["max"].to_numpy()
            )
        ).sort_values(["label", "center"])

        # Domain is what the filters actually left on the plot, not everything
        # picked: a kernel with no row under this dataset, row group or needle
        # length would otherwise sit in the legend with nothing to point at.
        # `colors` is keyed on label, so each series keeps its colour regardless
        # of what else is toggled on.
        _drawn = set(_raw["label"])
        _shown = [n for n in picked if n in _drawn]
        _color = alt.Color(
            "series:N",
            scale=alt.Scale(
                domain=[short[n] for n in _shown],
                range=[colors[n] for n in _shown],
            ),
            # Bottom and horizontal: a right-hand column of up to twenty
            # kernel names ran off the edge of the container and took the
            # last entries with it. Four columns wrap the names into rows
            # under the plot instead, where the width is the page's.
            legend=alt.Legend(
                title="algorithm",
                orient="bottom",
                direction="horizontal",
                columns=4,
                labelLimit=260,
                symbolLimit=0,
            ),
        )
        # `nice` rounds a domain out to the next round number, which on a log
        # axis means the next power of ten: a ladder starting at 3e-6 got an
        # axis starting at 1e-6, half a decade of empty plot before the first
        # dot. The band and line already run to the data's edge, so the axis
        # should too.
        _x_scale = alt.Scale(type=x_scale.value, nice=False)
        _y_scale = alt.Scale(type=y_scale.value)
        _x = alt.X(
            "center:Q",
            scale=_x_scale,
            title="pattern selectivity",
            axis=alt.Axis(grid=False, format=".5~%"),
        )
        _y = alt.Y("q25:Q", title="GB/s per evaluation", scale=_y_scale)

        _rules = (
            alt.Chart(edges)
            .mark_rule(color="#8a8a8a", strokeDash=[3, 3], opacity=0.7)
            .encode(x=alt.X("edge:Q", scale=_x_scale))
        )
        _band = (
            alt.Chart(_agg)
            .mark_area(opacity=0.18)
            .encode(x=_x, y=_y, y2="q75:Q", color=_color)
        )
        # The line is binned, so its tooltip can only report the bin centre. The
        # points are one query each: hovering one gives that query's own
        # selectivity rather than the bin it landed in.
        # Selects by value, not by row index: a layered chart hoists its param to
        # the top level and marimo hands the selection back as field values, so
        # the click has to name a field. `query_id` alone, because the panel
        # below asks what every selected algorithm did on that one needle: the
        # click picks the needle, and every series' dot on it grows with it.
        _pick = alt.selection_point(
            name="cell",
            fields=["query_id"],
            on="click",
            clear="dblclick",
            empty=False,
        )
        _points = (
            alt.Chart(_raw)
            .mark_point(opacity=0.7, filled=True)
            .encode(
                x=alt.X("selectivity:Q", scale=_x_scale),
                y=alt.Y("gbps:Q", scale=_y_scale),
                color=_color,
                # Big enough to hit with a mouse: these are the click target
                # for the phase panel and the figure below it.
                size=alt.condition(_pick, alt.value(220), alt.value(45)),
                tooltip=[
                    alt.Tooltip("series:N", title="algorithm"),
                    alt.Tooltip("needle:N", title="needle"),
                    alt.Tooltip("dataset:N", title="dataset"),
                    alt.Tooltip("needle_len:Q", title="needle bytes"),
                    alt.Tooltip("selectivity:Q", title="selectivity", format=".5~%"),
                    alt.Tooltip("pruned:N", title="pruned"),
                    alt.Tooltip("gbps:Q", title="GB/s", format=".2f"),
                ],
            )
            .add_params(_pick)
        )
        _line = (
            alt.Chart(_agg)
            # No point marks: the dots are the click target for the phase
            # panel, and a symbol on the line is one more thing to hit by
            # mistake. The line still carries its own tooltip.
            .mark_line(strokeWidth=2)
            .encode(
                x=_x,
                y=alt.Y(f"{metric_dd.value}:Q", scale=_y_scale),
                color=_color,
                tooltip=[
                    alt.Tooltip("series:N", title="algorithm"),
                    alt.Tooltip("center:Q", title="bucket centre", format=".5~%"),
                    alt.Tooltip("hmean:Q", title="harmonic mean", format=".2f"),
                    alt.Tooltip("med:Q", title="median", format=".2f"),
                    alt.Tooltip("amean:Q", title="mean", format=".2f"),
                    alt.Tooltip("q25:Q", title="q25", format=".2f"),
                    alt.Tooltip("q75:Q", title="q75", format=".2f"),
                    alt.Tooltip("n:Q", title="queries"),
                ],
            )
        )

        _layers = [_rules]
        if band_cb.value:
            _layers.append(_band)
        _layers += [_points, _line]
        chart = mo.ui.altair_chart(
            alt.layer(*_layers)
            .properties(
                width="container",
                height=460,
                title=f"row group = {chunk_label} · "
                f"{splits_sl.value} split"
                f"{'' if splits_sl.value == 1 else 's'} · "
                f"line = {metric_dd.selected_key}"
                f"{', band = q25-q75' if band_cb.value else ''}",
            )
            .resolve_scale(color="shared"),
            chart_selection=False,
            legend_selection=False,
        )
        points = _raw
    return chart, points


@app.cell
def _(chart):
    chart
    return


@app.cell
def _(alt, chart, chunk_dd, mo, open_db, pd, picked, points, run_id, short):
    # Phase label, its column, and the colour it keeps in the bar and the table.
    # A module reports a phase or it does not: a NULL column drops out of that
    # bar rather than reading as a zero.
    PHASES = [
        ("plan", "setup_ns", "#b79a20"),
        ("decode", "decode_ns", "#79706e"),
        ("scan", "scan_ns", "#4c78a8"),
        ("prefilter", "prefilter_ns", "#54a24b"),
        ("verify", "verify_ns", "#e45756"),
    ]

    def _dur(ns):
        if ns >= 1e6:
            return f"{ns / 1e6:.2f} ms"
        if ns >= 1e3:
            return f"{ns / 1e3:.1f} µs"
        return f"{ns:.0f} ns"

    # `chart` is an mo.md placeholder whenever the scatter has nothing to draw.
    # `apply_selection` hands back the whole frame when nothing is selected --
    # only `.value` guards that -- so an empty selection has to be caught here or
    # the panel opens showing every point at once.
    _sel = getattr(chart, "selections", None) or {}
    _hit = (
        chart.apply_selection(points)
        if any(_sel.values())
        else points.iloc[:0]
    )

    if _hit.empty:
        phases = mo.md("")
    else:
        # The click names a needle, not the series it landed on. Every selected
        # algorithm is then shown on that needle, which is the comparison the
        # scatter cannot make: one dot is one algorithm.
        _qs = ", ".join(
            "'{}'".format(_q.replace("'", "''"))
            for _q in sorted(set(_hit["query_id"]))
        )
        _ls = ", ".join("'{}'".format(_l.replace("'", "''")) for _l in picked)
        with open_db() as _con:
            _cells = _con.sql(
                f"""
                select label, needle, dataset, selectivity, gbps,
                       setup_ns, decode_ns, scan_ns, prefilter_ns, verify_ns
                from v
                where run_id = {run_id} and chunk_rows = {chunk_dd.value}
                  and status = 'ok'
                  and query_id in ({_qs}) and label in ({_ls})
                """
            ).df()

        _multi = _cells["needle"].nunique() > 1
        _long = []
        for _c in _cells.itertuples():
            _kernel = short.get(_c.label, _c.label)
            _row = f"{_c.needle} · {_kernel}" if _multi else _kernel
            for _i, (_lab, _col, _) in enumerate(PHASES):
                _v = getattr(_c, _col)
                if pd.isna(_v) or _v <= 0:
                    continue
                _long.append(
                    {
                        "row": _row,
                        "phase": _lab,
                        "order": _i,
                        "ns": float(_v),
                        "ms": float(_v) / 1e6,
                        "dur": _dur(_v),
                        "gbps": float(_c.gbps),
                    }
                )

        if not _long:
            phases = mo.md("*No kernel reports phase timings for this needle.*")
        else:
            _df = pd.DataFrame(_long)
            _tot = _df.groupby("row", as_index=False)["ms"].sum()
            _sorted = _tot.sort_values("ms", ascending=False)["row"].tolist()
            _q0 = _cells.iloc[0]
            _title = (
                f"{_cells['needle'].nunique()} needles · {_q0.dataset}"
                if _multi
                else f"{_q0.needle} · {_q0.dataset} · "
                f"{_q0.selectivity * 100:.4g}% selective"
            )

            _y = alt.Y("row:N", title=None, sort=_sorted)
            _bars = (
                alt.Chart(_df)
                .mark_bar()
                .encode(
                    x=alt.X(
                        "ms:Q",
                        title="milliseconds",
                        axis=alt.Axis(grid=False),
                    ),
                    y=_y,
                    color=alt.Color(
                        "phase:N",
                        scale=alt.Scale(
                            domain=[_lab for _lab, _, _ in PHASES],
                            range=[_hex for _, _, _hex in PHASES],
                        ),
                        legend=alt.Legend(title="phase", orient="top"),
                    ),
                    order=alt.Order("order:Q"),
                    tooltip=[
                        alt.Tooltip("row:N", title="kernel"),
                        alt.Tooltip("phase:N", title="phase"),
                        alt.Tooltip("dur:N", title="time"),
                        alt.Tooltip("gbps:Q", title="GB/s", format=".2f"),
                    ],
                )
            )
            _ends = (
                alt.Chart(_tot)
                .mark_text(align="left", dx=4, fontSize=11, opacity=0.7)
                .encode(x=alt.X("ms:Q"), y=_y, text=alt.Text("ms:Q", format=".3~f"))
            )

            _css = (
                "<style>"
                ".phasetbl{font-size:.85rem;border-collapse:collapse;width:100%;"
                "min-width:48rem}"
                ".phasetbl th,.phasetbl td{padding:.25rem .8rem;white-space:nowrap}"
                ".phasetbl thead th{font-weight:500;opacity:.7;"
                "border-bottom:1px solid currentColor}"
                ".phasetbl tbody tr:nth-child(even){background:rgba(127,127,127,.08)}"
                "</style>"
            )
            _by_row = {
                _r: _g.set_index("phase")["ns"].to_dict()
                for _r, _g in _df.groupby("row")
            }
            _gbps = _df.drop_duplicates("row").set_index("row")["gbps"].to_dict()
            _trs = []
            for _r in _sorted:
                _p = _by_row[_r]
                _trs.append(
                    "<tr>"
                    f"<td><b>{_r}</b></td>"
                    f"<td style='text-align:right'>{_gbps[_r]:.2f}</td>"
                    + "".join(
                        "<td style='text-align:right'>"
                        + (_dur(_p[_lab]) if _lab in _p else "—")
                        + "</td>"
                        for _lab, _, _ in PHASES
                    )
                    + f"<td style='text-align:right'>{_dur(sum(_p.values()))}</td>"
                    "</tr>"
                )
            _head = "".join(
                f"<th style='text-align:right;color:{_hex}'>{_lab}</th>"
                for _lab, _, _hex in PHASES
            )

            phases = mo.vstack(
                [
                    mo.ui.altair_chart(
                        (_bars + _ends).properties(
                            width="container",
                            height=26 * len(_sorted) + 40,
                            title=_title,
                        ),
                        chart_selection=False,
                        legend_selection=False,
                    ),
                    mo.Html(
                        _css
                        + "<div style='overflow-x:auto'><table class='phasetbl'>"
                        "<thead><tr>"
                        "<th style='text-align:left'>kernel</th><th>GB/s</th>"
                        + _head
                        + "<th>sum</th>"
                        "</tr></thead><tbody>" + "".join(_trs) + "</tbody></table></div>"
                    ),
                    mo.md(
                        "<span style='font-size:.8rem;opacity:.65'>"
                        "Phase times come from a single instrumented pass, run "
                        "once outside the timing loop, so they are one sample "
                        "each and do not divide into the sampled GB/s median "
                        "beside them."
                        "</span>"
                    ),
                ]
            )
    return (phases,)


@app.cell
def _(phases):
    phases
    return


@app.cell
def _(mo):
    # Off by default: a figure costs a dictionary training run of its own, 0.3 s
    # on tpch-pname up to 3 s on dbpedia-abstract, and most clicks only want the
    # phase panel above.
    gv_on = mo.ui.checkbox(label="alignment graph")
    return (gv_on,)


@app.cell
def _(meta, mo, picked):
    # A cut and a cover are properties of one dictionary, so a figure is drawn
    # for one config at a time: the OnPair configs among the selected chips.
    _opts = {}
    for _n in sorted(picked):
        _m = meta[meta["label"] == _n]
        if _m.empty or _m.iloc[0]["candidate"] != "onpair_spiral":
            continue
        _cfg = _m.iloc[0]["config"] or "{}"
        _opts.setdefault(_cfg.strip("{}").replace('"', "").replace(":", " ") or "default", _cfg)
    gv_dd = mo.ui.dropdown(
        options=_opts, value=next(iter(_opts), None), label="dictionary"
    )
    return (gv_dd,)


@app.cell
def _(gv_dd, gv_on, mo):
    mo.hstack([gv_on, gv_dd], justify="start", gap=1)
    return


@app.cell
def _(chart, chunk_dd, gv_dd, gv_on, mo, points):
    # Underscore-aliased: marimo takes every top-level name in a cell as a
    # definition, and `Path` is already one in the imports cell.
    import hashlib as _hashlib
    import json as _json
    import re as _re
    import subprocess as _subprocess
    import tempfile as _tempfile
    from pathlib import Path as _Path

    import pyarrow as _pa

    _root = _Path(__file__).resolve().parents[1]
    _bin = _root / "tools" / "graph-viz" / "target" / "release" / "onpair-graph-viz"
    _cache = _Path(_tempfile.gettempdir()) / "onpair-graph-viz"

    def _rows_file(dataset, lo, hi):
        """Rows [lo, hi) of a dataset as the newline-delimited file the tool reads.

        Kept on disk between clicks: the export is fast (0.05 s for a chunk,
        0.3 s for a whole clickbench column) but the file is the same every time.
        """
        _cache.mkdir(exist_ok=True)
        path = _cache / f"{dataset}-{lo}-{hi}.txt"
        if not path.exists():
            source = _root / "datasets" / dataset / "data.arrow"
            with _pa.memory_map(str(source), "rb") as src:
                column = _pa.ipc.open_file(src).read_all().column(0).combine_chunks()
            path.write_bytes(b"\n".join(column.slice(lo, hi - lo).to_pylist()) + b"\n")
        return path

    _sel = getattr(chart, "selections", None) or {}
    _hit = chart.apply_selection(points) if any(_sel.values()) else points.iloc[:0]

    if not gv_on.value or _hit.empty or gv_dd.value is None:
        graph = mo.md("")
    elif not _bin.exists():
        graph = mo.md(
            "*No `onpair-graph-viz` binary. Build it with "
            "`cargo build --release` in `tools/graph-viz`.*"
        )
    else:
        _ds = _hit.iloc[0]["dataset"]
        _needles = sorted({_r.needle for _r in _hit.itertuples() if _r.dataset == _ds})
        _cfg = _json.loads(gv_dd.value)
        _bits = int(_cfg.get("bits", 16))
        _prune = _cfg.get("prune")

        # A chunked cell trains one dictionary per chunk, so its figure has to be
        # drawn over one chunk's rows -- the first -- and says so. Whole column is
        # the whole column, and the two are different pictures of one needle.
        _rows = _json.loads(
            (_root / "datasets" / _ds / "manifest.json").read_text()
        )["num_rows"]
        _lo = 0
        _hi = min(chunk_dd.value, _rows) if chunk_dd.value else _rows
        _scope = (
            f"chunk 1 of {-(-_rows // chunk_dd.value)}, rows 0-{_hi:,}"
            if chunk_dd.value
            else f"whole column, {_rows:,} rows"
        )

        _key = _hashlib.sha1(
            repr((_ds, _lo, _hi, _bits, _prune, tuple(_needles))).encode()
        ).hexdigest()[:16]
        _out = _cache / _key
        _log = _out / "run.log"

        if not _log.exists():
            _out.mkdir(parents=True, exist_ok=True)
            _cmd = [
                str(_bin),
                "--rows", str(_rows_file(_ds, _lo, _hi)),
                "--bits", str(_bits),
                "--out", str(_out),
                "--title", f"{_ds} · {_scope}",
            ]
            if _prune is not None:
                _cmd += ["--prune", str(_prune)]
            for _needle in _needles:
                _cmd += ["--pattern", _needle]
            _run = _subprocess.run(_cmd, capture_output=True, text=True)
            (_out / "run.err").write_text(_run.stderr)
            _log.write_text(_run.stdout)

        _stdout = _log.read_text()
        _stderr = (_out / "run.err").read_text()

        # The tool prints two lines per pattern: the counts, then the predicate
        # the scan compiles to. Both are keyed by the figure's stem.
        _said = {}
        _lines = [_l.strip() for _l in _stdout.splitlines()]
        for _i, _line in enumerate(_lines):
            _stem, _, _rest = _line.partition(": ")
            if _rest and _stem[:2].isdigit():
                _next = _lines[_i + 1] if _i + 1 < len(_lines) else ""
                _said[_stem] = (_rest, _next)

        _blocks = []
        if _stderr.strip():
            _blocks.append(
                mo.md(f"<span style='color:#e45756'>{_stderr.strip()}</span>")
            )
        for _svg in sorted(_out.glob("[0-9][0-9]-*.svg")):
            if _svg.name.endswith("-table.svg"):
                continue
            _counts, _predicate = _said.get(_svg.stem, ("", ""))
            _blocks.append(
                mo.md(
                    f"**`{_svg.stem.split('-', 1)[1]}`** · {_scope} · bits {_bits}"
                    + (f" · prune {_prune}" if _prune is not None else "")
                    + f"<br><span style='font-size:.8rem;opacity:.7'>{_counts}<br>"
                    f"{_predicate}</span>"
                )
            )
            # The tool sizes a figure for a page: a fixed width and height in
            # the <svg> tag, half the viewBox. Dropping both and keeping the
            # viewBox lets it scale to whatever the notebook column is.
            _blocks.append(
                mo.Html(
                    _re.sub(
                        r'<svg\b([^>]*?)\s+width="[^"]*"\s+height="[^"]*"',
                        r'<svg\1 style="width:100%;height:auto"',
                        _svg.read_text(),
                        count=1,
                    )
                )
            )
        if not _blocks:
            _blocks.append(mo.md("*The tool wrote no figure. Its output:*"))
            _blocks.append(mo.md(f"```\n{_stdout}\n```"))
        graph = mo.vstack(_blocks)
    return (graph,)


@app.cell
def _(graph):
    graph
    return


@app.cell
def _(agg, colors, meta, mo, pct, pd, picked, short):
    def _split(pf_ns, ver_ns):
        """Share of an instrumented cell spent in the prefilter, and in the verify.

        A module reports these or it does not; "—" is the second case, not a zero.
        Shares rather than milliseconds because these are single-shot instrumented
        timings and the column beside them is a median over samples — the two do
        not divide into each other (schema.sql says so at the column).
        """
        # NaN is its own inequality, which is the NULL a median over a column no
        # module filled comes back as.
        if pf_ns is None or ver_ns is None or pf_ns != pf_ns or ver_ns != ver_ns:
            return "—"
        total = pf_ns + ver_ns
        if total <= 0:
            return "—"
        return f"{pf_ns / total * 100:.0f}% / {ver_ns / total * 100:.0f}%"

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
            f"<td style='text-align:right'>{_m.gbps:.2f}</td>"
            f"<td style='text-align:right'>"
            f"{'—' if pd.isna(_m.comp_gbps) else f'{_m.comp_gbps:.2f}'}</td>"
            f"<td style='text-align:right'>{_m.build_ms:.0f} ms</td>"
            f"<td style='text-align:right'>{pct(_m.prune)}</td>"
            f"<td style='text-align:right'>{pct(_m.fp)}</td>"
            f"<td style='text-align:right'>{_split(_m.pf_ns, _m.ver_ns)}</td>"
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
        "<th>ratio</th><th>GB/s</th><th>GB/s&nbsp;compressed</th>"
        "<th>build</th><th>pruned</th><th>false pos</th>"
        "<th>prefilter / verify</th>"
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
