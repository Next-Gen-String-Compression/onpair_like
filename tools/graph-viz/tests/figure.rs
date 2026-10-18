//! End-to-end checks over the plan logs in `tests/fixtures`, which
//! `tests/fixtures/make.sh` regenerates from two small corpora with both
//! loggers.

use std::path::Path;

use onpair_graph_viz::{Figure, Log, Options, read_log, visualize};

fn log(name: &str) -> Log {
    read_log(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
        .expect("fixture reads")
}

fn figure(log: &Log, algorithm: &str, needle: &[u8], options: &Options) -> Figure {
    let query = log
        .queries
        .iter()
        .find(|q| q.algorithm == algorithm && q.needle == needle)
        .unwrap_or_else(|| panic!("no {algorithm} query for {needle:?}"));
    visualize(log, query, options).expect("figure renders")
}

/// Every logged query draws, and no control character reaches an SVG.
#[test]
fn every_logged_query_draws() {
    for name in ["plain", "repetitive"] {
        let log = log(name);
        for query in &log.queries {
            let figure = visualize(&log, query, &Options::default()).expect("figure renders");
            for svg in [figure.svg.expect("small graph renders"), figure.table_svg] {
                let stray = svg.chars().find(|c| c.is_control() && *c != '\n');
                assert_eq!(stray, None, "{name}/{}: control character in the SVG", query.uuid);
            }
        }
    }
}

/// The edges marked as cut compile to the cover the planner priced: their
/// frequency over the rows is OnPair's logged estimate. FSST is left out, since
/// its estimate also counts pairs the frequency sum does not.
#[test]
fn the_cut_compiles_to_the_planners_estimate() {
    for name in ["plain", "repetitive"] {
        let log = log(name);
        for query in log.queries.iter().filter(|q| q.algorithm.starts_with("onpair")) {
            let figure = visualize(&log, query, &Options::default()).expect("figure renders");
            let drawn = (figure.summary.cover_frequency as f64 / query.rows as f64).min(1.0);
            assert!(
                (drawn - query.cut_selectivity_estimated).abs() < 1e-12,
                "{name}/{}: drawn {drawn} vs logged {}",
                query.uuid,
                query.cut_selectivity_estimated
            );
        }
    }
}

/// A ClickBench URL needle with a `0x0e` in it is what found this.
#[test]
fn control_bytes_in_a_needle_do_not_break_the_svg() {
    let svg = figure(&log("plain"), "onpair16", b"sho\x0ep?q=", &Options::default())
        .svg
        .expect("small graph renders");
    assert!(svg.contains("\\x0e"), "the byte should be shown, not dropped");
}

/// A step the FSST cut takes only as the second code of a pair is badged.
#[test]
fn paired_steps_are_badged() {
    let log = log("plain");
    let mut seen = 0;
    for query in log.queries.iter().filter(|q| q.algorithm == "fsst") {
        let figure = visualize(&log, query, &Options::default()).expect("figure renders");
        let svg = figure.svg.expect("small graph renders");
        assert_eq!(
            svg.matches(">PAIR<").count(),
            figure.logged.paired.len(),
            "{}",
            query.uuid
        );
        seen += figure.logged.paired.len();
    }
    assert!(seen > 0, "the fixture should hold a paired step");
}

/// Past the guard the graph is still returned; only the drawing is skipped.
#[test]
fn wide_graphs_skip_the_svg() {
    let log = log("plain");
    let needle = b"/articles/2026/08?utm_source=";
    let options = Options {
        max_states: Some(1),
        ..Options::default()
    };
    let figure = figure(&log, "onpair16", needle, &options);
    assert!(figure.states() > 1, "this needle needs several states");
    assert!(figure.svg.is_none(), "the guard should have skipped the SVG");
    let options = Options {
        max_states: None,
        ..options
    };
    assert!(figure_svg(&log, needle, &options).is_some(), "no guard means always render");
}

fn figure_svg(log: &Log, needle: &[u8], options: &Options) -> Option<String> {
    figure(log, "onpair16", needle, options).svg
}

/// Edges route around the states they do not touch.
#[test]
fn no_edge_runs_through_a_state_it_does_not_touch() {
    /// `x`, `y`, `w`, `h` of every state box, with the offset it stands for.
    fn state_boxes(svg: &str) -> Vec<(String, f64, f64, f64, f64)> {
        let mut out = Vec::new();
        for group in svg.split("<g class=\"state").skip(1) {
            let Some(offset) = group
                .split("needle byte offset ")
                .nth(1)
                .and_then(|rest| rest.split(';').next())
            else {
                continue;
            };
            let Some(rect) = group.split("<rect ").nth(1) else {
                continue;
            };
            let number = |name: &str| -> Option<f64> {
                rect.split(&format!("{name}=\""))
                    .nth(1)?
                    .split('"')
                    .next()?
                    .parse()
                    .ok()
            };
            if let (Some(x), Some(y), Some(w), Some(h)) =
                (number("x"), number("y"), number("width"), number("height"))
            {
                out.push((offset.to_string(), x, y, w, h));
            }
        }
        out
    }

    /// Points along one path. Handles the `M`/`H`/`V`/`C` the renderer emits.
    fn sample(path: &str) -> Vec<(f64, f64)> {
        let tokens: Vec<&str> = path.split([' ', ',']).filter(|t| !t.is_empty()).collect();
        let mut points: Vec<(f64, f64)> = Vec::new();
        let mut cursor = (0.0, 0.0);
        let mut index = 0;
        let number = |token: &str| token.parse::<f64>().unwrap_or(f64::NAN);
        while index < tokens.len() {
            match tokens[index] {
                "M" => {
                    cursor = (number(tokens[index + 1]), number(tokens[index + 2]));
                    points.push(cursor);
                    index += 3;
                }
                "H" | "V" => {
                    let target = number(tokens[index + 1]);
                    let end = if tokens[index] == "H" {
                        (target, cursor.1)
                    } else {
                        (cursor.0, target)
                    };
                    for step in 1..=32 {
                        let t = f64::from(step) / 32.0;
                        points.push((
                            cursor.0 + (end.0 - cursor.0) * t,
                            cursor.1 + (end.1 - cursor.1) * t,
                        ));
                    }
                    cursor = end;
                    index += 2;
                }
                "C" => {
                    let c1 = (number(tokens[index + 1]), number(tokens[index + 2]));
                    let c2 = (number(tokens[index + 3]), number(tokens[index + 4]));
                    let end = (number(tokens[index + 5]), number(tokens[index + 6]));
                    for step in 1..=96 {
                        let t = f64::from(step) / 96.0;
                        let u = 1.0 - t;
                        let (a, b, c, d) =
                            (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                        points.push((
                            a * cursor.0 + b * c1.0 + c * c2.0 + d * end.0,
                            a * cursor.1 + b * c1.1 + c * c2.1 + d * end.1,
                        ));
                    }
                    cursor = end;
                    index += 7;
                }
                _ => index += 1,
            }
        }
        points
    }

    for (name, needle) in [
        ("plain", b"example.com".as_slice()),
        ("repetitive", b"newsletter".as_slice()),
        // Jumps over materialized states, which is what needs routing.
        ("repetitive", b"art/checkou".as_slice()),
        ("repetitive", b"ut?ref=newsl".as_slice()),
        ("repetitive", b"rg/cart/chec".as_slice()),
    ] {
        let svg = figure_svg(&log(name), needle, &Options::default())
            .expect("figure is small enough to draw");
        let label = String::from_utf8_lossy(needle);
        let boxes = state_boxes(&svg);
        assert!(!boxes.is_empty(), "{label}: no state boxes parsed");

        for chunk in svg.split("<path d=\"").skip(1) {
            let Some((path, rest)) = chunk.split_once('"') else {
                continue;
            };
            if !rest.starts_with(" class=\"edge") {
                continue;
            }
            let points = sample(path);
            let (first, last) = (points[0], points[points.len() - 1]);
            for (offset, x, y, w, h) in &boxes {
                // An edge that starts or ends on this box is attached to it.
                let attached = [first, last].iter().any(|&(px, py)| {
                    px >= x - 4.0 && px <= x + w + 4.0 && py >= y - 4.0 && py <= y + h + 4.0
                });
                if attached {
                    continue;
                }
                let inside = points.iter().any(|&(px, py)| {
                    px > x + 3.0 && px < x + w - 3.0 && py > y + 3.0 && py < y + h - 3.0
                });
                assert!(
                    !inside,
                    "{label}: edge {path} runs through the p={offset} state box"
                );
            }
        }
    }
}

/// Nothing the figure draws may sit on top of anything else.
#[test]
fn nothing_drawn_overlaps_anything_else() {
    /// Every `<g class="…">`'s first rect: one box per drawn element.
    fn boxes(svg: &str) -> Vec<(String, f64, f64, f64, f64)> {
        let mut out = Vec::new();
        for group in svg.split("<g class=\"").skip(1) {
            let Some((class, rest)) = group.split_once('"') else {
                continue;
            };
            let Some(rect) = rest.split("<rect ").nth(1) else {
                continue;
            };
            let number = |name: &str| -> Option<f64> {
                rect.split(&format!("{name}=\""))
                    .nth(1)?
                    .split('"')
                    .next()?
                    .parse()
                    .ok()
            };
            if let (Some(x), Some(y), Some(w), Some(h)) = (
                number("x"),
                number("y"),
                number("width"),
                number("height"),
            ) {
                out.push((class.to_string(), x, y, w, h));
            }
        }
        out
    }

    for (label, needle) in [
        ("plain", b"example.com".as_slice()),
        ("repetitive", b"newsletter".as_slice()),
        // Both shapes of the collision the anchor placement used to cause: a
        // jump whose midpoint is a state's centre, and one whose midpoint is
        // under another probe's card.
        ("repetitive", b"art/checkou".as_slice()),
        ("repetitive", b"ref=new".as_slice()),
    ] {
        let svg = figure_svg(&log(label), needle, &Options::default())
            .expect("figure is small enough to draw");
        let drawn = boxes(&svg);
        assert!(drawn.len() > 5, "{label}: only {} boxes parsed", drawn.len());

        for (i, a) in drawn.iter().enumerate() {
            for b in &drawn[i + 1..] {
                let x = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                let y = (a.2 + a.4).min(b.2 + b.4) - a.2.max(b.2);
                assert!(
                    x <= 0.0 || y <= 0.0,
                    "{label} / {}: {} at ({}, {}) overlaps {} at ({}, {}) by {x}×{y}",
                    String::from_utf8_lossy(needle),
                    a.0,
                    a.1,
                    a.2,
                    b.0,
                    b.1,
                    b.2
                );
            }
        }
    }
}

/// The companion table is the figure that has to be exhaustive: the DAG can
/// omit a token, the symbol table cannot.
#[test]
fn the_symbol_table_names_every_token_and_marks_the_cover() {
    let log = log("repetitive");
    let figure = figure(&log, "onpair16", b"newsletter", &Options::default());
    let query = &log.queries[0];
    let symbols = log.symbols_of(query).expect("symbol table");
    for id in 0..symbols.content.len() {
        assert!(
            figure.table_svg.contains(&format!(">{id}</text>")),
            "token {id} is missing from the table"
        );
    }
    let probes = figure
        .summary
        .cover_terms
        .iter()
        .flat_map(|term| term.begin..=term.last)
        .filter(|id| !figure.logged.graph.contained.contains(id))
        .count();
    assert_eq!(figure.table_svg.matches(">PROBE<").count(), probes);
    assert_eq!(
        figure.table_svg.matches(">WHOLE<").count(),
        figure.logged.graph.contained.len()
    );
}

/// Byte-compare one figure against a committed SVG.
///
/// Regenerate with `UPDATE_GOLDEN=1 cargo test`, and look at the diff before
/// accepting it.
#[test]
fn golden_figure() {
    let options = Options {
        title: Some("golden".to_string()),
        subtitle: Some("contains \"utm_source=newsletter\"".to_string()),
        ..Options::default()
    };
    let svg = figure(&log("plain"), "onpair16", b"utm_source=newsletter", &options)
        .svg
        .expect("small graph renders");

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/newsletter.svg");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(path, &svg).expect("write golden");
        return;
    }
    let expected = std::fs::read_to_string(path)
        .expect("golden figure missing, regenerate with UPDATE_GOLDEN=1");
    assert_eq!(svg, expected, "the rendered figure changed");
}
