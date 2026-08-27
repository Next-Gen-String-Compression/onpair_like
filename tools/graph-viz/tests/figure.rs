//! End-to-end checks over a fixed in-test corpus.
//!
//! The corpus lives here rather than in a data file so the crate stays free of
//! datasets, and the dictionary it trains is reproducible: OnPair's default
//! config seeds deterministically, and the revision is pinned in `Cargo.toml`.
//! Both of those are what make the golden figure stable.

use onpair::{Column, DEFAULT_CONFIG, Dictionary, DictionaryView};
use onpair_graph_viz::{
    Error, Options, ProbeWeight, build_path_graph, candidate_rows, index_for, live_cover,
    minimum_vertex_cut, visualize, visualize_index,
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

#[test]
fn exact_sidecar_produces_the_same_graph_without_retraining() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let frequencies = index_for(column.view()).expect("index builds");
    let encoded = onpair::encode_sidecar(&column.dict, &frequencies);
    let artifact = onpair::decode_sidecar(&encoded).expect("sidecar validates");
    let options = Options {
        measure: false,
        ..Options::default()
    };

    for pattern in PATTERNS {
        let from_column = visualize(column.view(), &frequencies, pattern.as_bytes(), &options)
            .expect("column graph builds");
        let from_sidecar = visualize_index(
            artifact.dictionary.as_view(),
            &artifact.frequencies,
            pattern.as_bytes(),
            &options,
        )
        .expect("sidecar graph builds");
        assert_eq!(
            (
                from_sidecar.cover.points,
                from_sidecar.cover.ranges,
                from_sidecar.cover.cmp_cost,
                from_sidecar.cover.member_ids,
            ),
            (
                from_column.cover.points,
                from_column.cover.ranges,
                from_column.cover.cmp_cost,
                from_column.cover.member_ids,
            ),
            "{pattern:?}"
        );
        assert_eq!(
            from_sidecar.summary.cover_frequency, from_column.summary.cover_frequency,
            "{pattern:?}"
        );
        assert_eq!(from_sidecar.cut.value, from_column.cut.value, "{pattern:?}");
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

/// The explorer displays these static cover facts beside the benchmark's copy,
/// so matching admitted rows is not enough: shape, SIMD price, and frequency
/// must agree with the exact pinned planner too.
#[test]
fn cover_facts_match_onpair() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    for pattern in PATTERNS {
        let figure = visualize(view, &frequencies, pattern.as_bytes(), &Options::default())
            .expect("figure builds");
        let analysis =
            onpair::search::analyze_prefilter(pattern.as_bytes(), view.dict, &frequencies);
        assert_eq!(
            figure.cover.points,
            analysis.probe_cover().points().len(),
            "{pattern:?}"
        );
        assert_eq!(
            figure.cover.ranges,
            analysis.probe_cover().ranges().len(),
            "{pattern:?}"
        );
        assert_eq!(
            figure.cover.cmp_cost,
            analysis.comparison_cost(),
            "{pattern:?}"
        );
        assert_eq!(
            figure.summary.cover_frequency,
            u64::from(analysis.covered_frequency()),
            "{pattern:?}"
        );
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
    let graph = build_path_graph(view.dict, pattern, &weights).expect("graph builds");
    let cut = minimum_vertex_cut(&graph, ProbeWeight::TermFrequency);
    let candidates = candidate_rows(view, &graph.membership_for_cut(&cut.selected_nodes));

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

/// A safety-validated stored frequency index may undercount, so the pinned planner
/// never removes zero-weight members from a cut. They still issue comparisons,
/// even though an exact freshly-built index proves they occur nowhere.
#[test]
fn probes_for_absent_tokens_are_retained() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    for pattern in ["zz", "q=z", "zzz-absent"] {
        let figure = visualize(view, &frequencies, pattern.as_bytes(), &Options::default())
            .expect("figure builds");
        let measurement = figure.measurement.as_ref().expect("measured by default");

        assert!(
            !figure.cut.selected_nodes.is_empty(),
            "{pattern:?}: the cut should still name the probes it chose"
        );
        assert!(
            figure.dead_probes.is_empty(),
            "{pattern:?}: no probe is pruned"
        );
        assert!(
            figure.cover.cmp_cost > 0 && figure.cover.member_ids > 0,
            "{pattern:?}: the zero-frequency probes still cost comparisons"
        );
        assert_eq!(figure.cover.dead_ids, 0, "{pattern:?}: nothing was dropped");
        assert_eq!(
            (measurement.candidates, measurement.onpair_candidates),
            (0, Some(0)),
            "{pattern:?}: an empty cover admits no rows, and OnPair should agree"
        );
        assert!(
            !figure.svg.expect("small graph renders").contains("PRUNED"),
            "{pattern:?}: the figure must retain the cut"
        );
    }
}

/// A token holding the whole needle is a mandatory member of every cover and has no
/// node in the DAG, so nothing the graph draws accounts for it. That gap is what
/// made a figure look wrong: a cut of three probes producing a cover of ten, or a
/// cut of weight zero producing a cover at all. The figure has to name them.
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
    assert!(svg.contains("MANDATORY"), "the card should be badged");
    assert!(
        svg.contains("contains the whole needle"),
        "and should say what it is"
    );
    assert!(
        svg.contains("WHOLE-NEEDLE TOKENS"),
        "and the chip should count them"
    );

    // A needle past MAX_TOKEN_SIZE cannot be contained by construction, and then
    // the figure has no reason to mention the case at all.
    let options = Options {
        max_states: None,
        ..Options::default()
    };
    let figure = visualize(
        view,
        &frequencies,
        b"/cart/checkout?ref=newsletter",
        &options,
    )
    .expect("figure builds");
    assert_eq!(figure.summary.contained_tokens, 0);
    assert!(!figure.svg.expect("renders").contains("MANDATORY"));
}

/// A cut of weight zero says no occurrence crosses a token boundary — not that
/// nothing matches. When every occurrence sits inside one token the cover is the
/// mandatory ids alone, and it is exact.
#[test]
fn a_weightless_cut_still_covers_the_matches() {
    let corpus = Corpus::repetitive();
    let column = corpus.column();
    let view = column.view();
    let frequencies = index_for(view).expect("index builds");

    // Every row holds "/cart/", so the dictionary holds it too and no occurrence of
    // "cart" ever straddles two tokens.
    let figure =
        visualize(view, &frequencies, b"cart", &Options::default()).expect("figure builds");
    let measurement = figure.measurement.as_ref().expect("measured by default");
    assert_eq!(
        figure.cut.value, 0,
        "no boundary-crossing occurrence to pay for"
    );
    assert!(
        figure.cover.cmp_cost > 0,
        "yet the cover still has probes to issue"
    );
    assert_eq!(
        (measurement.candidates, measurement.exact_rows),
        (view.num_rows(), view.num_rows()),
        "and every row matches, all of them admitted exactly"
    );
    assert!(measurement.sound);

    let svg = figure.svg.expect("small graph renders");
    assert!(
        svg.contains("The cut weighs nothing"),
        "the figure has to say why a zero weight is not an empty answer"
    );
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

    let figure =
        visualize(view, &frequencies, b"utm_source=", &Options::default()).expect("figure builds");
    let svg = figure.svg.expect("small graph renders");
    assert!(
        svg.contains("TF "),
        "term frequency always comes from the index"
    );
    assert!(
        !svg.contains("DF "),
        "no rows were counted, so no row frequency should be claimed"
    );

    let options = Options {
        metric: ProbeWeight::RowFrequency,
        ..Options::default()
    };
    let figure = visualize(view, &frequencies, b"utm_source=", &options).expect("figure builds");
    let svg = figure.svg.expect("small graph renders");
    assert!(
        svg.contains("DF "),
        "the df metric counts rows, so the figure may report them"
    );
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

/// Where the normalization copied from OnPair is pinned: membership is preserved,
/// maximal runs merge, points cost one comparison, and ranges cost two.
#[test]
fn live_cover_preserves_membership_and_prices_ranges() {
    // Ids 2, 3 and 5 are used; every other id in an eight-token dictionary is not.
    let frequencies = onpair::search::index::build_token_frequency_index(&[2u16, 3, 5, 5], 8)
        .expect("index builds");

    // Runs 1..=5 and 7..=7 become one range and one point, regardless of their
    // exact frequencies.
    let live = live_cover(
        &[false, true, true, true, true, true, false, true],
        &frequencies,
    );
    assert_eq!(
        live.members,
        [false, true, true, true, true, true, false, true]
    );
    assert_eq!(
        (live.shape.points, live.shape.ranges, live.shape.cmp_cost),
        (1, 1, 3)
    );
    assert_eq!((live.shape.member_ids, live.shape.dead_ids), (6, 0));
    assert!(!live.is_empty());

    // A two-id run remains a range, priced as its lower and upper comparisons.
    let live = live_cover(
        &[false, true, true, false, false, false, false, false],
        &frequencies,
    );
    assert_eq!(
        (
            live.shape.points,
            live.shape.ranges,
            live.shape.member_ids,
            live.shape.dead_ids
        ),
        (0, 1, 2, 0)
    );

    let live = live_cover(
        &[true, true, false, false, false, false, true, true],
        &frequencies,
    );
    assert!(!live.is_empty(), "frequency does not erase membership");
    assert_eq!(
        (live.shape.points, live.shape.ranges, live.shape.cmp_cost),
        (0, 2, 4)
    );
    assert_eq!((live.shape.member_ids, live.shape.dead_ids), (4, 0));
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

#[test]
fn mismatched_index_is_rejected() {
    let corpus = Corpus::new();
    let column = corpus.column();
    let view = column.view();
    // An index over a truncated code stream still has the right token count, so
    // build one for a different dictionary size instead.
    let frequencies =
        onpair::search::index::build_token_frequency_index(view.codes, view.dict.num_tokens())
            .expect("index builds");
    let smaller = Column::compress(b"abc", &[0u32, 3], DEFAULT_CONFIG).expect("compresses");
    let smaller_view = smaller.view();

    let error = visualize(smaller_view, &frequencies, b"ab", &Options::default()).unwrap_err();
    assert!(
        matches!(error, Error::IndexMismatch { .. }),
        "expected a mismatch, got {error}"
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
