//! The dictionary itself, as a companion figure to the alignment DAG.
//!
//! The DAG names the handful of tokens a cut touches; this names all of them,
//! which is what a reader needs to ask the next question — *why that token and
//! not its neighbour?* Rows are id order, and a sorted dictionary's id order is
//! bytewise order, so neighbours in the table really are the ones a prefix range
//! would have swept over.
//!
//! Roles come from the same cover the DAG figure drew, so the two are two views
//! of one decision rather than two computations.

use onpair::{DictionaryView, MAX_TOKEN_SIZE, Token};

use crate::graph::{PathGraph, Weights, preview};
use crate::render::{compact_u64, xml};

/// Every token is spelled out in full — [`MAX_TOKEN_SIZE`] is the whole of one —
/// and the column widens to whatever that takes.
const TOKEN_CHARS: usize = MAX_TOKEN_SIZE;

/// Column width with tokens of ordinary length. A column holding a wider one
/// grows past this; nothing in the table is ever cut to fit.
const COL_W_BASE: f64 = 268.0;
/// Width of a token character at the monospace size the rows are set in.
const TOKEN_GLYPH_W: f64 = 5.75;
/// Everything in a row that is not the token: id, TF, share, badge, padding.
const ROW_FURNITURE_W: f64 = 130.0;
const ROW_H: f64 = 15.0;
const PAD: f64 = 28.0;
const HEADER_H: f64 = 104.0;
/// Column length to aim for before opening another column. A small dictionary
/// then stays a single readable list instead of being split for no reason.
const PREFERRED_ROWS: usize = 44;
/// Columns to spread a large dictionary over before growing downward instead.
const MAX_COLS: usize = 8;

/// What a token is to this query.
enum Role {
    /// Holds the whole needle: mandatory in every cover, in no cut.
    Whole,
    /// In the cover the scan issues.
    Probe,
    /// Occurs nowhere in the code stream.
    Unused,
    /// Everything else.
    Plain,
}

impl Role {
    fn of(id: Token, graph: &PathGraph, cover: &[bool], term_frequency: u64) -> Self {
        if graph.contained.contains(&id) {
            Self::Whole
        } else if cover.get(id as usize).copied().unwrap_or(false) {
            Self::Probe
        } else if term_frequency == 0 {
            Self::Unused
        } else {
            Self::Plain
        }
    }

    /// Row class, and the badge to stamp at the end of the row.
    fn marks(&self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Whole => ("row whole", Some("WHOLE")),
            Self::Probe => ("row probe", Some("PROBE")),
            Self::Unused => ("row unused", None),
            Self::Plain => ("row", None),
        }
    }
}

/// Draw every dictionary token, marking the ones this query's cover probes for.
///
/// `cover` is the membership table the DAG figure drew — one bool per token id.
pub fn render_table_svg<V: DictionaryView>(
    dict: V,
    weights: &Weights,
    graph: &PathGraph,
    cover: &[bool],
    title: &str,
) -> String {
    let num_tokens = dict.num_tokens();
    let cols = num_tokens.div_ceil(PREFERRED_ROWS).clamp(1, MAX_COLS);
    let rows_per_col = num_tokens.div_ceil(cols).max(1);

    let tokens: Vec<String> = (0..num_tokens)
        .map(|id| preview(dict.token(id as Token), TOKEN_CHARS))
        .collect();
    let widest = tokens
        .iter()
        .map(|token| token.chars().count() + 2)
        .max()
        .unwrap_or(0);
    let col_w = COL_W_BASE.max(widest as f64 * TOKEN_GLYPH_W + ROW_FURNITURE_W);

    let probed = (0..num_tokens)
        .filter(|&id| cover.get(id).copied().unwrap_or(false))
        .count();
    let unused = (0..num_tokens)
        .filter(|&id| weights.token_term_frequency(id as Token) == 0)
        .count();
    let needle = preview(&graph.needle, 32);

    let heading = format!("Symbol table · {title}");
    let meta = format!(
        "{num_tokens} tokens · {} codes over {} rows · {probed} in the cover · {} whole-needle · {unused} unused",
        compact_u64(graph.total_codes),
        compact_u64(graph.column_rows),
        graph.contained.len()
    );
    let needle_line = format!("needle \"{needle}\"");

    // A one-column table is narrower than its own header, so the header sets the
    // width instead. Per-glyph widths for the three sizes below, as elsewhere in
    // this crate: measuring text properly would mean shipping a font.
    let header_w = [
        heading.chars().count() as f64 * 10.6,
        meta.chars().count() as f64 * 6.05,
        needle_line.chars().count() as f64 * 6.9,
    ]
    .into_iter()
    .fold(0.0, f64::max);
    let width = PAD * 2.0 + (cols as f64 * col_w).max(header_w);
    let height = HEADER_H + rows_per_col as f64 * ROW_H + PAD * 2.0;

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{:.0}" height="{:.0}" viewBox="0 0 {width:.0} {height:.0}" role="img" aria-labelledby="table-title">
<title id="table-title">Dictionary symbol table, {num_tokens} tokens, cover for contains "{}" marked</title>
<style>
  .background {{ fill:#ffffff; }}
  text {{ font-family:Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; fill:#17252a; }}
  .heading {{ font-size:20px; font-weight:700; letter-spacing:-0.2px; }}
  .meta {{ font-size:11.5px; fill:#52666d; }}
  .needle {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:11.5px; fill:#243b42; }}
  .col-label {{ font-size:8.5px; font-weight:700; letter-spacing:0.9px; fill:#8b9b9f; }}
  .id {{ font-size:9px; fill:#8b9b9f; text-anchor:end; font-variant-numeric:tabular-nums; }}
  .token {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:9.5px; fill:#20363c; }}
  .tf {{ font-size:9px; fill:#52666d; text-anchor:end; font-variant-numeric:tabular-nums; }}
  .pct {{ font-size:8.5px; fill:#8b9b9f; text-anchor:end; font-variant-numeric:tabular-nums; }}
  .row rect {{ fill:#ffffff; }}
  .row:nth-of-type(even) rect {{ fill:#fafcfc; }}
  .row.probe rect {{ fill:#fff1e8; stroke:#d55e00; stroke-width:1.4; }}
  .row.probe .token {{ font-weight:700; fill:#8f4000; }}
  .row.whole rect {{ fill:#e6f4f1; stroke:#0f7b76; stroke-width:1.4; }}
  .row.whole .token {{ font-weight:700; fill:#0b5c58; }}
  .row.unused .token, .row.unused .id, .row.unused .tf {{ fill:#b3c0c3; }}
  .badge rect {{ stroke:none; }}
  .badge text {{ font-size:7px; font-weight:700; letter-spacing:0.5px; fill:#ffffff; text-anchor:middle; }}
  .badge.probe rect {{ fill:#d55e00; }}
  .badge.whole rect {{ fill:#0f7b76; }}
</style>
<rect class="background" x="0" y="0" width="{width:.0}" height="{height:.0}"/>
"##,
        width / 2.0,
        height / 2.0,
        xml(&needle),
    );

    svg.push_str(&format!(
        "<text x=\"{PAD:.1}\" y=\"38\" class=\"heading\">{}</text>",
        xml(&heading)
    ));
    svg.push_str(&format!(
        "<text x=\"{PAD:.1}\" y=\"60\" class=\"meta\">{}</text>",
        xml(&meta)
    ));
    svg.push_str(&format!(
        "<text x=\"{PAD:.1}\" y=\"80\" class=\"needle\">{}</text>",
        xml(&needle_line)
    ));

    for col in 0..cols {
        let x = PAD + col as f64 * col_w;
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"col-label\">ID · TOKEN · TF · RF</text>",
            x,
            HEADER_H - 6.0
        ));
    }

    for (id, token) in tokens.iter().enumerate() {
        let col = id / rows_per_col;
        let row = id % rows_per_col;
        let x = PAD + col as f64 * col_w;
        let y = HEADER_H + row as f64 * ROW_H;

        let term_frequency = weights.token_term_frequency(id as Token);
        let role = Role::of(id as Token, graph, cover, term_frequency);
        let (class, badge) = role.marks();
        // RF, as everywhere else in these figures: occurrences over rows, the
        // share of rows the token is priced as admitting.
        let share = if graph.column_rows == 0 {
            0.0
        } else {
            (term_frequency as f64 / graph.column_rows as f64 * 100.0).min(100.0)
        };

        svg.push_str(&format!(
            "<g class=\"{class}\"><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"3\"/>",
            x,
            y,
            col_w - 18.0,
            ROW_H - 1.5
        ));
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"id\">{id}</text>",
            x + 26.0,
            y + 10.5
        ));
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"token\">\"{}\"</text>",
            x + 34.0,
            y + 10.5,
            xml(token)
        ));
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"tf\">{}</text>",
            x + col_w - 78.0,
            y + 10.5,
            compact_u64(term_frequency)
        ));
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"pct\">{share:.2}%</text>",
            x + col_w - 42.0,
            y + 10.5
        ));
        if let Some(badge) = badge {
            let class = if matches!(role, Role::Whole) {
                "badge whole"
            } else {
                "badge probe"
            };
            svg.push_str(&format!(
                "<g class=\"{class}\"><rect x=\"{:.1}\" y=\"{:.1}\" width=\"36\" height=\"11\" rx=\"5.5\"/><text x=\"{:.1}\" y=\"{:.1}\">{badge}</text></g>",
                x + col_w - 36.0,
                y + 1.0,
                x + col_w - 18.0,
                y + 9.0
            ));
        }
        svg.push_str("</g>");
    }

    svg.push_str("</svg>\n");
    svg
}
