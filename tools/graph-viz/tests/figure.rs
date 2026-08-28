//! End-to-end checks over a fixed in-test corpus.
//!
//! The corpus lives here rather than in a data file so the crate stays free of
//! datasets, and the dictionary it trains is reproducible: OnPair's default
//! config seeds deterministically, and the revision is pinned in `Cargo.toml`.
//! Both of those are what make the golden figure stable.

use onpair::{Column, DEFAULT_CONFIG, DictionaryView};
use onpair_graph_viz::{
    EdgeKind, Error, Options, ProbeWeight, build_path_graph, candidate_rows, compile_cover,
    index_for, minimum_cut, predicate_text, visualize,
};

const ROWS: &[&str] = &[
    "https://www.example.com/index.html",
    "https://www.example.com/search?q=onpair",
    "https://www.example.com/search?q=compression",
    "https://shop.example.org/cart/checkout?ref=newsletter",
    "https://shop.example.org/products/list?page=2&sort=desc",
    "https://news.site.co.uk/articles/2026/08?utm_source=newsletter",
    "https://news.site.co.uk/articles/2026/08?utm_source=google",
    "https://cdn.assets.example.net/static/app.js",
    "https://forum.example.com/user/profile?id=42",
    "https://forum.example.com/search?q=substring-search&page=3",
];

/// Patterns spanning the interesting cases: several boundary alignments, a
/// pattern short enough to sit inside one token, one that matches nothing, and
/// one long enough to force a chain of interior probes.
const PATTERNS: &[&str] = &[
    "utm_source=newsletter",
    "example",
    "?q=",
    "co",
    "zzz-absent",
    "/articles/2026/08?utm_source=",
];

struct Corpus {
    bytes: Vec<u8>,
    offsets: Vec<u32>,
}

impl Corpus {
    fn new() -> Self {
        let mut bytes = Vec::new();
        let mut offsets = vec![0u32];
        for row in ROWS {
            bytes.extend_from_slice(row.as_bytes());
            offsets.push(bytes.len() as u32);
        }
        Self { bytes, offsets }
    }

    fn column(&self) -> Column<u32> {
        Column::compress(&self.bytes, &self.offsets, DEFAULT_CONFIG).expect("corpus compresses")
    }

    /// A deliberately repetitive corpus, for the whole-needle case.
    ///
    /// Ten rows do not give OnPair enough evidence to merge a phrase into one long
    /// token, so no needle over that corpus is ever *contained* — and the case that
    /// most confuses a reader of these figures is exactly the contained one.
    fn repetitive() -> Self {
        let mut bytes = Vec::new();
        let mut offsets = vec![0u32];
        for row in 0..200 {
            let text = if row % 2 == 0 {
                format!("https://shop.example.org/cart/checkout?ref=newsletter&id={row}")
            } else {
                format!("https://shop.example.org/cart/basket?ref=google&id={row}")
            };
            bytes.extend_from_slice(text.as_bytes());
            offsets.push(bytes.len() as u32);
        }
        Self { bytes, offsets }
    }
}

/// The point of the whole exercise: a rendered cover must admit every true match.
/// This graph is a second implementation of OnPair's planning logic, so it is
/// worth proving rather than assuming.
#[test]
fn every_cover_is_sound() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    for metric in ProbeWeight::ALL {
        for pattern in PATTERNS {
            let options = Options {
                metric,
                ..Options::default()
            };
            let figure = visualize(view, &frequencies, pattern.as_bytes(), &options)
                .unwrap_or_else(|error| panic!("{pattern:?} under {}: {error}", metric.name()));
            let measurement = figure.measurement.expect("measured by default");
            assert!(
                measurement.sound,
                "{pattern:?} under {}: cover missed a true match",
                metric.name()
            );
            assert!(
                measurement.candidates >= measurement.exact_rows,
                "{pattern:?} under {}: fewer candidates than matches",
                metric.name()
            );
        }
    }
}

/// Same cover, computed from the graph by hand rather than through `visualize`,
/// to pin the relationship between the cut and the token membership it implies.
#[test]
fn cut_membership_admits_the_matching_rows() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");
    let weights = onpair_graph_viz::Weights::from_index(&frequencies);

    let pattern = b"utm_source=newsletter";
    let graph = build_path_graph(view.dict, pattern, &weights, None).expect("graph builds");
    let cut = minimum_cut(&graph, ProbeWeight::TermFrequency);
    let candidates = candidate_rows(view, &graph.membership_for_cut(&cut.selected_edges));

    let truth = view.rows_containing(pattern);
    assert!(!truth.is_empty(), "the corpus should contain the pattern");
    for row in truth {
        assert!(candidates.contains(&row), "row {row} was filtered out");
    }
}

/// OnPair's own prefilter and this crate's cut should agree on selectivity.
///
/// Equal-weight cuts can tie, so a difference is not automatically a bug — but
/// on this corpus they do agree, and a change in that is worth being told about.
#[test]
fn selectivity_matches_onpair() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    for pattern in PATTERNS {
        let figure = visualize(view, &frequencies, pattern.as_bytes(), &Options::default())
            .expect("figure builds");
        let measurement = figure.measurement.expect("measured by default");
        if let Some(theirs) = measurement.onpair_candidates {
            assert_eq!(
                measurement.candidates, theirs,
                "{pattern:?}: this crate admits {} rows, OnPair admits {theirs}",
                measurement.candidates
            );
        }
    }
}

/// A cut can name a probe for tokens the code stream never uses — they weigh
/// nothing, so its objective is indifferent — and nothing downstream removes it.
/// The pruning both sides used to do was unsound: a safety-valid stored index may
/// hold false zeroes, so a probe dropped for looking unused can lose a real match.
///
/// What is still worth asserting is the outcome: patterns that cannot match admit
/// no rows, and OnPair agrees.
#[test]
fn probes_for_absent_tokens_are_kept_and_admit_nothing() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    for pattern in ["zz", "q=z", "zzz-absent"] {
        let figure = visualize(view, &frequencies, pattern.as_bytes(), &Options::default())
            .expect("figure builds");
        let measurement = figure.measurement.as_ref().expect("measured by default");

        assert_eq!(
            measurement.candidates, 0,
            "{pattern:?}: nothing can match, so nothing may be admitted"
        );
        assert_eq!(measurement.exact_rows, 0, "{pattern:?}: nothing matches");
        assert!(measurement.sound, "{pattern:?}: vacuously sound");
        assert_eq!(
            measurement.onpair_candidates,
            Some(0),
            "{pattern:?}: OnPair admits nothing either"
        );
    }
}

/// Where the run-merging rule copied from OnPair is pinned: every maximal run of
/// member ids is one probe, a singleton run a point, and nothing is dropped for
/// looking unused — stored weights may hold false zeroes, so pruning by frequency
/// would be unsound. Both sides used to prune; neither does now.
#[test]
fn the_cover_merges_runs_and_prunes_nothing() {
    // Ids 2, 3 and 5 are used; every other id in an eight-token dictionary is not.
    let frequencies = onpair::search::index::build_token_frequency_index(&[2u16, 3, 5, 5], 8)
        .expect("index builds");

    // Runs 1..=5 and 7..=7: one range, one point, membership alone deciding.
    let cover = compile_cover(
        &[false, true, true, true, true, true, false, true],
        &frequencies,
    );
    assert_eq!(
        cover.members,
        [false, true, true, true, true, true, false, true]
    );
    assert_eq!(
        (cover.shape.points, cover.shape.ranges, cover.shape.cmp_cost),
        (1, 1, 2)
    );
    assert_eq!(cover.shape.member_ids, 6);
    assert!(!cover.is_empty());

    // A term's frequency is its own ids', off the index.
    let terms: Vec<(u16, u16, u64)> = cover
        .terms
        .iter()
        .map(|term| (term.begin, term.last, term.term_frequency))
        .collect();
    assert_eq!(terms, vec![(1, 5, 4), (7, 7, 0)]);

    // A run of ids the stream never uses is still a probe: the cut chose it.
    let cover = compile_cover(
        &[true, true, false, false, false, false, true, true],
        &frequencies,
    );
    assert!(!cover.is_empty());
    assert_eq!(cover.shape.cmp_cost, 2);
}

/// A token holding the whole needle is now an ordinary step — start to accept, no
/// boundary crossed — so a cut has to take it and pay its frequency. It used to sit
/// outside the graph as a mandatory member, which is why a cut could weigh zero and
/// still produce a cover; there is no such case any more.
#[test]
fn the_whole_needle_step_is_taken_and_paid_for() {
    let corpus = Corpus::repetitive();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    // Every row holds "/cart/", so the dictionary holds it too and no occurrence of
    // "cart" ever straddles two tokens.
    let figure =
        visualize(view, &frequencies, b"cart", &Options::default()).expect("figure builds");
    let measurement = figure.measurement.as_ref().expect("measured by default");

    assert!(
        !figure.graph.contained.is_empty(),
        "some token holds the whole needle"
    );
    let step = figure
        .graph
        .edges
        .iter()
        .find(|edge| matches!(edge.kind, EdgeKind::Contained { .. }))
        .expect("the whole-needle step is in the graph");
    assert_eq!(
        (step.from, step.to),
        (0, figure.graph.needle.len()),
        "it runs start to accept, crossing nothing"
    );
    assert!(
        figure.cut.selected_edges.contains(&step.id),
        "being the only step on its path, every cut takes it"
    );
    assert_eq!(
        figure.cut.value, figure.summary.contained_frequency,
        "and pays exactly what those tokens cost"
    );
    assert!(figure.cover.cmp_cost > 0, "the cover has probes to issue");
    assert_eq!(
        (measurement.candidates, measurement.exact_rows),
        (view.num_rows(), view.num_rows()),
        "every row matches, all of them admitted exactly"
    );
    assert!(measurement.sound);
}

/// Needles taken from a real column carry raw bytes, and XML 1.0 has no escape for
/// a C0 control character — a figure with one in a label is rejected outright by
/// every viewer, not merely ugly. So they are written as `\xNN` instead. A
/// ClickBench URL needle with a `0x0e` in it is what found this.
#[test]
fn control_bytes_in_a_needle_do_not_break_the_svg() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    let figure =
        visualize(view, &frequencies, b"sho\x0ep?q=", &Options::default()).expect("figure builds");
    let svg = figure.svg.expect("small graph renders");

    assert!(
        svg.contains("\\x0e"),
        "the byte should be shown, not dropped"
    );
    let stray = svg
        .chars()
        .find(|character| character.is_control() && *character != '\n');
    assert_eq!(stray, None, "a control character reached the SVG");
}

/// Row frequency is not in OnPair's index, so under a `tf` metric there is nothing
/// to print — and printing the term frequency again under a `DF` label, as this
/// once did, is a measurement the figure never took.
#[test]
fn row_frequency_is_shown_only_when_it_was_counted() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    let figure = visualize(view, &frequencies, b"utm_source=", &Options::default())
        .expect("figure builds");
    let svg = figure.svg.expect("small graph renders");
    assert!(svg.contains("TF "), "term frequency always comes from the index");
    assert!(
        !svg.contains("DF "),
        "no rows were counted, so no row frequency should be claimed"
    );

    let options = Options {
        metric: ProbeWeight::RowFrequency,
        ..Options::default()
    };
    let figure =
        visualize(view, &frequencies, b"utm_source=", &options).expect("figure builds");
    let svg = figure.svg.expect("small graph renders");
    assert!(
        svg.contains("DF "),
        "the df metric counts rows, so the figure may report them"
    );
}

/// A token holding the whole needle is now a step of its own — start to accept —
/// and the figure has to name it. Before, it sat outside the graph entirely, and
/// nothing the drawing showed accounted for the probes it added to the cover.
#[test]
fn whole_needle_tokens_are_counted_and_drawn() {
    let corpus = Corpus::repetitive();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    // Frequent enough in this corpus to have been merged into single tokens, and
    // short enough to fit in one.
    let figure =
        visualize(view, &frequencies, b"google", &Options::default()).expect("figure builds");
    assert!(
        !figure.graph.contained.is_empty(),
        "some dictionary token should hold the whole needle"
    );
    assert_eq!(
        figure.summary.contained_tokens,
        figure.graph.contained.len()
    );
    assert!(
        figure.summary.contained_frequency > 0,
        "those tokens occur in this column"
    );
    assert!(
        figure.summary.cover_frequency >= figure.summary.contained_frequency,
        "the cover holds them, so it cannot weigh less"
    );
    assert_eq!(
        figure.summary.cover_probes,
        figure.summary.cover_points + figure.summary.cover_ranges,
        "every probe is a point or a range"
    );

    let svg = figure.svg.expect("small graph renders");
    assert!(
        svg.contains("no boundary → match"),
        "the card should say what makes its step different"
    );
    assert!(
        svg.contains("WHOLE-NEEDLE TOKENS"),
        "and the chip should count them"
    );
    assert!(
        svg.contains("probe contained"),
        "and it should be drawn in its own colour, not as one more orange card"
    );

    // A needle past MAX_TOKEN_SIZE cannot be contained by construction, and then
    // the figure has no reason to mention the case at all.
    let options = Options {
        max_states: None,
        ..Options::default()
    };
    let figure = visualize(view, &frequencies, b"/cart/checkout?ref=newsletter", &options)
        .expect("figure builds");
    assert_eq!(figure.summary.contained_tokens, 0);
    assert!(!figure.svg.expect("renders").contains("MANDATORY"));
}

/// Past the guard the graph is still returned; only the drawing is skipped.
#[test]
fn wide_graphs_skip_the_svg() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    let options = Options {
        max_states: Some(1),
        ..Options::default()
    };
    let figure =
        visualize(view, &frequencies, b"/articles/2026/08", &options).expect("figure builds");
    assert!(figure.states() > 1, "this needle needs several states");
    assert!(
        figure.svg.is_none(),
        "the guard should have skipped the SVG"
    );

    let options = Options {
        max_states: None,
        ..options
    };
    let figure =
        visualize(view, &frequencies, b"/articles/2026/08", &options).expect("figure builds");
    assert!(figure.svg.is_some(), "no guard means always render");
}

#[test]
fn empty_pattern_is_rejected() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    assert_eq!(
        visualize(view, &frequencies, b"", &Options::default()).unwrap_err(),
        Error::EmptyPattern
    );
}

/// An index *narrower* than the dictionary describes something else, and weighing
/// a token it does not cover would read past its end.
///
/// A wider one is not a mismatch: an FSST code stream carries escape markers and
/// the bytes behind them as codes of their own, so the index has to be wider than
/// the dictionary for those to be weighed at all.
#[test]
fn an_index_narrower_than_the_dictionary_is_rejected() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();

    let smaller = Column::compress(b"abc", &[0u32, 3], DEFAULT_CONFIG).expect("compresses");
    let narrow = onpair::search::index::build_token_frequency_index(
        smaller.view().codes,
        smaller.view().dict.num_tokens(),
    )
    .expect("index builds");

    let error = visualize(view, &narrow, b"ab", &Options::default()).unwrap_err();
    assert!(
        matches!(error, Error::IndexMismatch { .. }),
        "expected a mismatch, got {error}"
    );

    // The other way round is fine, and is what an FSST figure needs.
    let wide = onpair::search::index::build_token_frequency_index(
        view.codes,
        view.dict.num_tokens() + 8,
    )
    .expect("index builds");
    visualize(view, &wide, b"ab", &Options::default()).expect("a wider index is accepted");
}

/// The predicate has to be the cover, term for term: it is the figure's answer to
/// "what will actually be compared", and a term more or less would misstate the
/// scan's cost as well as its meaning.
#[test]
fn the_predicate_has_one_term_per_comparison() {
    for (corpus, needle) in [
        (Corpus::new(), b"example.com".as_slice()),
        (Corpus::repetitive(), b"newsletter".as_slice()),
        (Corpus::repetitive(), b"google".as_slice()),
    ] {
        let column = corpus.column();
        let view = column.view();
        let frequencies = index_for(view).expect("index builds");
        let figure = visualize(view, &frequencies, needle, &Options::default())
            .expect("figure renders");

        let text = predicate_text(&figure.graph, &figure.summary.cover_terms);
        assert_eq!(
            text.split(" ∨ ").count(),
            figure.cover.cmp_cost,
            "{}: predicate {text:?} does not match {} comparisons",
            String::from_utf8_lossy(needle),
            figure.cover.cmp_cost
        );
        // A range term is a comparison against two ends; a point is one token.
        assert_eq!(
            text.matches('[').count(),
            figure.cover.ranges,
            "{}: bracketed terms should be exactly the ranges",
            String::from_utf8_lossy(needle)
        );
    }
}

/// The measured per-term row counts have to bracket the prefilter's own: no term
/// can admit more rows than the cover does, and the cover cannot admit more than
/// every term put together.
///
/// This is the check that the measured line is counted rather than copied from the
/// estimate beside it — the two agree exactly for a token that never repeats
/// inside a row, which is most of them, so equality proves nothing on its own.
#[test]
fn measured_term_rows_bracket_the_candidate_count() {
    for (corpus, needle) in [
        (Corpus::new(), b"example.com".as_slice()),
        (Corpus::repetitive(), b"newsletter".as_slice()),
        (Corpus::repetitive(), b"cart".as_slice()),
    ] {
        let column = corpus.column();
        let view = column.view();
        let frequencies = index_for(view).expect("index builds");
        let figure = visualize(view, &frequencies, needle, &Options::default())
            .expect("figure renders");
        let measurement = figure.measurement.as_ref().expect("measured by default");
        let label = String::from_utf8_lossy(needle);

        assert_eq!(
            measurement.term_rows.len(),
            figure.summary.cover_terms.len(),
            "{label}: one count per term"
        );
        let sum: u64 = measurement.term_rows.iter().sum();
        assert!(
            sum >= measurement.candidates as u64,
            "{label}: terms sum to {sum} but {} rows were admitted",
            measurement.candidates
        );
        for &rows in &measurement.term_rows {
            assert!(
                rows <= measurement.candidates as u64,
                "{label}: a term admits {rows} rows of {} candidates",
                measurement.candidates
            );
        }
        // The estimate is an upper bound on the count, per term.
        for term in &figure.summary.cover_terms {
            let measured = term.measured_rows.expect("measured by default");
            assert!(
                measured <= term.term_frequency,
                "{label}: term measured {measured} rows above its frequency {}",
                term.term_frequency
            );
        }
    }
}

/// No edge may pass through a state it is not attached to.
///
/// A probe consuming several needle bytes advances past the states in between, and
/// the direct route runs straight through them — `p2 → p4` crossing `p=3` was the
/// reported bug. The renderer picks that edge's route by checking candidates, so
/// this samples the drawn curves and checks the result.
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

    for (corpus, needle) in [
        (Corpus::new(), b"example.com".as_slice()),
        (Corpus::repetitive(), b"newsletter".as_slice()),
        // Jumps over materialized states, which is what needs routing.
        (Corpus::repetitive(), b"art/checkou".as_slice()),
        (Corpus::repetitive(), b"ut?ref=newsl".as_slice()),
        (Corpus::repetitive(), b"rg/cart/chec".as_slice()),
    ] {
        let column = corpus.column();
        let view = column.view();
        let frequencies = index_for(view).expect("index builds");
        let figure = visualize(view, &frequencies, needle, &Options::default())
            .expect("figure renders");
        let svg = figure.svg.expect("figure is small enough to draw");
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
///
/// The layout places cards by hand — callouts dodge states, entry cards grow with
/// their token list — so a collision is a plain bug that no other assertion here
/// would notice. It was a real one: a token consuming several needle bytes had
/// its anchor at the midpoint of the jump, which is exactly where an intermediate
/// state sits.
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

    for (label, corpus, needle) in [
        ("plain", Corpus::new(), b"example.com".as_slice()),
        ("repetitive", Corpus::repetitive(), b"newsletter".as_slice()),
        // Both shapes of the collision the anchor placement used to cause, found
        // by rendering every substring of this corpus: a jump whose midpoint is a
        // state's centre, and one whose midpoint is under another probe's card.
        // Neither reproduces from a jump of odd length, which is why these two
        // needles are spelled out rather than picked for looking representative.
        ("repetitive", Corpus::repetitive(), b"art/checkou".as_slice()),
        ("repetitive", Corpus::repetitive(), b"ref=new".as_slice()),
    ] {
        let column = corpus.column();
        let view = column.view();
        let frequencies = index_for(view).expect("index builds");
        let figure = visualize(view, &frequencies, needle, &Options::default())
            .expect("figure renders");
        let svg = figure.svg.expect("figure is small enough to draw");
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
/// legitimately omit a token, the symbol table cannot.
#[test]
fn the_symbol_table_names_every_token_and_marks_the_cover() {
    let corpus = Corpus::repetitive();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");
    let figure = visualize(view, &frequencies, b"newsletter", &Options::default())
        .expect("figure renders");

    let table = &figure.table_svg;
    for id in 0..view.dict.num_tokens() {
        assert!(
            table.contains(&format!(">{id}</text>")),
            "token {id} is missing from the table"
        );
    }

    // One PROBE badge per cover member that is not a whole-needle token, which
    // carries WHOLE instead.
    let members = figure.graph.membership_for_cut(&figure.cut.selected_edges);
    let live = compile_cover(&members, &frequencies);
    let probes = live
        .members
        .iter()
        .enumerate()
        .filter(|&(id, &member)| member && !figure.graph.contained.contains(&(id as u16)))
        .count();
    assert_eq!(table.matches(">PROBE<").count(), probes);
    assert_eq!(
        table.matches(">WHOLE<").count(),
        figure.graph.contained.len()
    );
}

/// Byte-compare one figure against a committed SVG.
///
/// Regenerate with `UPDATE_GOLDEN=1 cargo test`, and look at the diff before
/// accepting it — a layout change that makes figures worse looks exactly like a
/// layout change that makes them better until someone opens the file.
#[test]
fn golden_figure() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    let options = Options {
        title: "golden".to_string(),
        subtitle: Some("contains \"utm_source=newsletter\"".to_string()),
        ..Options::default()
    };
    let figure =
        visualize(view, &frequencies, b"utm_source=newsletter", &options).expect("figure builds");
    let svg = figure.svg.expect("small graph renders");

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/newsletter.svg");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"))
            .expect("golden directory");
        std::fs::write(path, &svg).expect("write golden");
        return;
    }

    let expected = std::fs::read_to_string(path)
        .expect("golden figure missing — regenerate with UPDATE_GOLDEN=1");
    assert_eq!(svg, expected, "the rendered figure changed");
}

