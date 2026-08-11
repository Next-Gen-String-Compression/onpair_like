//! End-to-end checks over a fixed in-test corpus.
//!
//! The corpus lives here rather than in a data file so the crate stays free of
//! datasets, and the dictionary it trains is reproducible: OnPair's default
//! config seeds deterministically, and the revision is pinned in `Cargo.toml`.
//! Both of those are what make the golden figure stable.

use onpair::{Column, DEFAULT_CONFIG, DictionaryView};
use onpair_graph_viz::{
    Error, Options, ProbeWeight, build_path_graph, candidate_rows, index_for, minimum_vertex_cut,
    visualize,
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
        onpair::search::build_token_frequency_index(view.codes, view.dict.num_tokens())
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
