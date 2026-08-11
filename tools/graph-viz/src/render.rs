//! The figure: a standalone SVG of the alignment DAG with its cut highlighted.
//!
//! Layout is deterministic, not force-directed. Alignments become horizontal
//! lanes ordered by `k`; byte-offset states get an x from their offset, floored
//! to a minimum spacing so labels never collide; a merged state sits at the mean
//! y of the lanes that reach it, which is what makes convergence visible. Interior
//! token probes are drawn as callout cards placed by a small collision search,
//! anchored to the edge they sit on. Terminal ranges drop to a row beneath the
//! lanes and rail into a single `MATCH` node.
//!
//! Everything is inline — one `<style>` block, three marker defs, no external
//! references — so the file drops straight into a paper or a browser. Fonts are a
//! system stack rather than an embedded face; see the crate README.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::Serialize;

use crate::graph::{NodeKind, PathGraph, Probe, ProbeSet, ProbeWeight};

const ENTRY_X: f64 = 28.0;
const ENTRY_W: f64 = 250.0;
const GRAPH_LEFT: f64 = 350.0;
const HEADER_H: f64 = 202.0;
const LANE_GAP: f64 = 108.0;
const STATE_W: f64 = 54.0;
const STATE_H: f64 = 34.0;
const PROBE_W: f64 = 152.0;
const PROBE_H: f64 = 70.0;
const TERMINAL_W: f64 = 200.0;
const TERMINAL_H: f64 = 72.0;

/// The numbers printed in the figure's header, around the graph itself.
///
/// Only [`metric`](Self::metric), the two weights and the labels are required.
/// The optional fields are measurements over a column; when absent their chips
/// are left out rather than shown as zero.
#[derive(Clone, Debug, Serialize)]
pub struct RenderSummary {
    /// Headline, e.g. the column or corpus this dictionary was trained on.
    pub title: String,
    /// Second line: whatever identifies this particular query.
    pub subtitle: String,
    /// Which weight the cut minimized.
    pub metric: ProbeWeight,
    /// Term frequency of the cut's full token membership, contained ids included.
    pub cut_member_frequency: u64,
    /// SIMD comparisons the cover costs.
    pub cut_cmp_cost: usize,
    /// Cut probes the planner pruned as unusable, of the ones drawn.
    pub dead_probes: usize,
    /// Fraction of rows that truly match, if measured.
    pub selectivity: Option<f64>,
    /// Rows the cover admits, if measured.
    pub cut_candidates: Option<usize>,
    /// Rows that truly match, if measured.
    pub exact_rows: Option<usize>,
}

impl RenderSummary {
    /// A summary with labels and weights only, no column measurements.
    pub fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        metric: ProbeWeight,
        cut_member_frequency: u64,
        cut_cmp_cost: usize,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            metric,
            cut_member_frequency,
            cut_cmp_cost,
            dead_probes: 0,
            selectivity: None,
            cut_candidates: None,
            exact_rows: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Box2d {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Box2d {
    fn cx(self) -> f64 {
        self.x + self.w / 2.0
    }

    fn cy(self) -> f64 {
        self.y + self.h / 2.0
    }

    fn right(self) -> f64 {
        self.x + self.w
    }

    fn bottom(self) -> f64 {
        self.y + self.h
    }

    fn padded(self, amount: f64) -> Self {
        Self {
            x: self.x - amount,
            y: self.y - amount,
            w: self.w + 2.0 * amount,
            h: self.h + 2.0 * amount,
        }
    }

    fn overlaps(self, other: Self) -> bool {
        self.x < other.right()
            && self.right() > other.x
            && self.y < other.bottom()
            && self.bottom() > other.y
    }
}

#[derive(Clone, Debug)]
struct Alignment {
    offset: usize,
    junction: usize,
    first_set: Option<usize>,
    start_state: usize,
    y: f64,
}

#[derive(Clone, Copy, Debug)]
struct PointLayout {
    anchor_x: f64,
    anchor_y: f64,
    card: Box2d,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeTone {
    Active,
    Cut,
    Muted,
}

impl EdgeTone {
    const fn class(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Cut => "cut",
            Self::Muted => "muted",
        }
    }
}

/// `text` as XML character data.
///
/// Control characters are written as `\xNN` rather than escaped, because there is
/// no escape for them: XML 1.0 forbids the C0 range outright, entity reference or
/// not, and a real needle taken from a column of URLs or titles will contain them.
/// A viewer that rejects the whole figure over one stray byte is worse than a
/// figure that shows the byte.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            control if control.is_control() => {
                out.push_str(&format!("\\x{:02x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out
}

fn needle_preview(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &byte in bytes {
        match byte {
            b' '..=b'~' if !matches!(byte, b'\\' | b'"') => out.push(byte as char),
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out
}

fn compact_u64(value: u64) -> String {
    if value >= 1_000_000_000 {
        format!("{:.2}B", value as f64 / 1_000_000_000.0)
    } else if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn metric_label(metric: ProbeWeight) -> &'static str {
    match metric {
        ProbeWeight::TermFrequency => "term-frequency weighted",
        ProbeWeight::ResidualTermFrequency => "residual-TF weighted",
        ProbeWeight::RowFrequency => "row-frequency weighted",
        ProbeWeight::ResidualRowFrequency => "residual-DF weighted",
    }
}

fn adjacency(graph: &PathGraph) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); graph.nodes.len()];
    for edge in &graph.edges {
        out[edge.from].push(edge.to);
    }
    out
}

fn incoming_counts(graph: &PathGraph) -> Vec<usize> {
    let mut counts = vec![0usize; graph.nodes.len()];
    for edge in &graph.edges {
        counts[edge.to] += 1;
    }
    counts
}

fn alignments(graph: &PathGraph, adjacent: &[Vec<usize>]) -> Vec<Alignment> {
    let mut entries = Vec::new();
    for node in &graph.nodes {
        let NodeKind::Junction {
            offset: Some(offset),
            ..
        } = node.kind
        else {
            continue;
        };
        let next = *adjacent[node.id]
            .first()
            .expect("an alignment junction must have an outgoing edge");
        let (first_set, start_state) =
            if matches!(graph.nodes[next].kind, NodeKind::FirstSet { .. }) {
                (
                    Some(next),
                    *adjacent[next]
                        .first()
                        .expect("a first-token set must lead to a state"),
                )
            } else {
                (None, next)
            };
        assert!(matches!(
            graph.nodes[start_state].kind,
            NodeKind::State { .. }
        ));
        entries.push(Alignment {
            offset,
            junction: node.id,
            first_set,
            start_state,
            y: 0.0,
        });
    }
    entries.sort_by_key(|entry| entry.offset);
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.y = HEADER_H + 60.0 + index as f64 * LANE_GAP;
    }
    entries
}

fn state_users(
    graph: &PathGraph,
    adjacent: &[Vec<usize>],
    entries: &[Alignment],
) -> HashMap<usize, Vec<usize>> {
    let mut users = HashMap::<usize, Vec<usize>>::new();
    for (alignment_index, entry) in entries.iter().enumerate() {
        let mut stack = vec![entry.start_state];
        let mut seen = HashSet::new();
        while let Some(node_id) = stack.pop() {
            if !seen.insert(node_id) {
                continue;
            }
            if matches!(graph.nodes[node_id].kind, NodeKind::State { .. }) {
                users.entry(node_id).or_default().push(alignment_index);
            }
            for &next in &adjacent[node_id] {
                if !matches!(graph.nodes[next].kind, NodeKind::Sink) {
                    stack.push(next);
                }
            }
        }
    }
    users
}

fn state_positions(graph: &PathGraph) -> BTreeMap<usize, f64> {
    let mut offsets: Vec<usize> = graph
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::State { offset } => Some(offset),
            _ => None,
        })
        .collect();
    offsets.sort_unstable();
    offsets.dedup();
    let scale = (1_650.0 / graph.needle.len().max(1) as f64).clamp(21.0, 72.0);
    let mut positions = BTreeMap::new();
    let mut previous = GRAPH_LEFT - 148.0;
    for offset in offsets {
        let ideal = GRAPH_LEFT + offset as f64 * scale;
        let x = ideal.max(previous + 148.0);
        positions.insert(offset, x);
        previous = x;
    }
    positions
}

fn state_boxes(
    graph: &PathGraph,
    state_x: &BTreeMap<usize, f64>,
    users: &HashMap<usize, Vec<usize>>,
    entries: &[Alignment],
) -> HashMap<usize, Box2d> {
    graph
        .nodes
        .iter()
        .filter_map(|node| {
            let NodeKind::State { offset } = node.kind else {
                return None;
            };
            let alignment_users = users
                .get(&node.id)
                .expect("every materialized state must be reachable from an alignment");
            let y = alignment_users
                .iter()
                .map(|&index| entries[index].y)
                .sum::<f64>()
                / alignment_users.len() as f64;
            Some((
                node.id,
                Box2d {
                    x: state_x[&offset] - STATE_W / 2.0,
                    y: y - STATE_H / 2.0,
                    w: STATE_W,
                    h: STATE_H,
                },
            ))
        })
        .collect()
}

fn place_callout(
    mut desired: Box2d,
    anchor_y: f64,
    prefer_above: bool,
    obstacles: &[Box2d],
    min_y: f64,
    max_y: f64,
) -> Box2d {
    for layer in 0..7 {
        for above in [prefer_above, !prefer_above] {
            let gap = 30.0 + layer as f64 * 77.0;
            desired.y = if above {
                anchor_y - gap - desired.h
            } else {
                anchor_y + gap
            };
            if desired.y < min_y || desired.bottom() > max_y {
                continue;
            }
            if obstacles
                .iter()
                .all(|other| !desired.padded(5.0).overlaps(*other))
            {
                return desired;
            }
        }
    }
    desired.y = (anchor_y - desired.h - 30.0).max(min_y);
    desired
}

fn point_layouts(
    graph: &PathGraph,
    state_x: &BTreeMap<usize, f64>,
    states: &HashMap<usize, Box2d>,
    state_by_offset: &HashMap<usize, usize>,
    graph_bottom: f64,
) -> HashMap<usize, PointLayout> {
    let mut point_nodes: Vec<_> = graph
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Point { .. }))
        .collect();
    point_nodes.sort_by_key(|node| match node.kind {
        NodeKind::Point { offset, .. } => offset,
        _ => unreachable!(),
    });

    let mut obstacles: Vec<Box2d> = states.values().map(|rect| rect.padded(8.0)).collect();
    let mut layouts = HashMap::new();
    for (index, node) in point_nodes.into_iter().enumerate() {
        let NodeKind::Point {
            offset,
            next_offset,
        } = node.kind
        else {
            unreachable!();
        };
        let source_state = states[&state_by_offset[&offset]];
        let anchor_x = (state_x[&offset] + state_x[&next_offset]) / 2.0;
        let anchor_y = source_state.cy();
        let desired = Box2d {
            x: anchor_x - PROBE_W / 2.0,
            y: 0.0,
            w: PROBE_W,
            h: PROBE_H,
        };
        let placed = place_callout(
            desired,
            anchor_y,
            index % 2 == 0,
            &obstacles,
            HEADER_H + 12.0,
            graph_bottom - 16.0,
        );
        obstacles.push(placed.padded(5.0));
        layouts.insert(
            node.id,
            PointLayout {
                anchor_x,
                anchor_y,
                card: placed,
            },
        );
    }
    layouts
}

fn terminal_boxes(
    graph: &PathGraph,
    state_x: &BTreeMap<usize, f64>,
    terminal_top: f64,
) -> HashMap<usize, Box2d> {
    let mut terminals: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::TerminalRange { offset } => Some((offset, node.id)),
            _ => None,
        })
        .collect();
    terminals.sort_unstable();

    let mut rows = Vec::<Vec<Box2d>>::new();
    let mut boxes = HashMap::new();
    for (offset, node_id) in terminals {
        let mut rect = Box2d {
            x: state_x[&offset] - TERMINAL_W / 2.0,
            y: terminal_top,
            w: TERMINAL_W,
            h: TERMINAL_H,
        };
        let row = (0..)
            .find(|&row| {
                rows.get(row).is_none_or(|placed| {
                    placed
                        .iter()
                        .all(|other| !rect.padded(8.0).overlaps(*other))
                })
            })
            .unwrap();
        if row == rows.len() {
            rows.push(Vec::new());
        }
        rect.y += row as f64 * (TERMINAL_H + 24.0);
        rows[row].push(rect);
        boxes.insert(node_id, rect);
    }
    boxes
}

fn reachable_without_cut(graph: &PathGraph, cut: &HashSet<usize>) -> HashSet<usize> {
    let adjacent = adjacency(graph);
    let mut reachable = HashSet::new();
    let mut queue = VecDeque::from([graph.source]);
    while let Some(node) = queue.pop_front() {
        if cut.contains(&node) || !reachable.insert(node) {
            continue;
        }
        for &next in &adjacent[node] {
            if !cut.contains(&next) {
                queue.push_back(next);
            }
        }
    }
    reachable
}

fn edge_tone(from: usize, to: usize, cut: &HashSet<usize>, reachable: &HashSet<usize>) -> EdgeTone {
    if cut.contains(&to) {
        EdgeTone::Cut
    } else if reachable.contains(&from) && reachable.contains(&to) {
        EdgeTone::Active
    } else {
        EdgeTone::Muted
    }
}

fn alignment_entry_edge(a: Box2d, b: Box2d) -> String {
    let x1 = a.right();
    let y1 = a.cy();
    let x2 = b.x;
    let y2 = b.cy();
    if (y1 - y2).abs() < 1.0 {
        return format!("M {x1:.1} {y1:.1} H {x2:.1}");
    }
    let turn_x = (x2 - 34.0).max(x1 + 24.0);
    format!(
        "M {x1:.1} {y1:.1} H {turn_x:.1} C {:.1} {y1:.1}, {:.1} {y2:.1}, {x2:.1} {y2:.1}",
        turn_x + 17.0,
        x2 - 17.0
    )
}

fn edge_to_point(a: Box2d, x2: f64, y2: f64) -> String {
    let x1 = a.right();
    let y1 = a.cy();
    let bend = ((x2 - x1).abs() * 0.45).max(20.0);
    format!(
        "M {x1:.1} {y1:.1} C {:.1} {y1:.1}, {:.1} {y2:.1}, {x2:.1} {y2:.1}",
        x1 + bend,
        x2 - bend
    )
}

fn edge_from_point(x1: f64, y1: f64, b: Box2d) -> String {
    let x2 = b.x;
    let y2 = b.cy();
    let bend = ((x2 - x1).abs() * 0.45).max(20.0);
    format!(
        "M {x1:.1} {y1:.1} C {:.1} {y1:.1}, {:.1} {y2:.1}, {x2:.1} {y2:.1}",
        x1 + bend,
        x2 - bend
    )
}

fn callout_leader(layout: PointLayout, class: &str) -> String {
    let card_edge_y = if layout.card.cy() < layout.anchor_y {
        layout.card.bottom()
    } else {
        layout.card.y
    };
    format!(
        "<path d=\"M {:.1} {:.1} V {:.1}\" class=\"callout-leader {class}\"/>",
        layout.anchor_x, layout.anchor_y, card_edge_y
    )
}

fn render_probe_anchor(layout: PointLayout, cut: CutMark, reachable: bool) -> String {
    let class = match cut {
        CutMark::Probed => "probe-anchor selected",
        CutMark::Pruned => "probe-anchor pruned",
        CutMark::No if reachable => "probe-anchor",
        CutMark::No => "probe-anchor downstream",
    };
    format!(
        "<g class=\"{class}\"><rect x=\"{:.1}\" y=\"{:.1}\" width=\"10\" height=\"10\" rx=\"1.5\" transform=\"rotate(45 {:.1} {:.1})\"/></g>",
        layout.anchor_x - 5.0,
        layout.anchor_y - 5.0,
        layout.anchor_x,
        layout.anchor_y
    )
}

fn vertical_edge(a: Box2d, b: Box2d) -> String {
    format!(
        "M {:.1} {:.1} C {:.1} {:.1}, {:.1} {:.1}, {:.1} {:.1}",
        a.cx(),
        a.bottom(),
        a.cx(),
        a.bottom() + 34.0,
        b.cx(),
        b.y - 34.0,
        b.cx(),
        b.y
    )
}

fn edge(path: &str, tone: EdgeTone, arrow: bool) -> String {
    format!(
        "<path d=\"{path}\" class=\"edge {}{}\"/>",
        tone.class(),
        if arrow { " arrow" } else { "" }
    )
}

fn probe_frequency(graph: &PathGraph, probe: &Probe) -> String {
    let pct = if graph.total_codes == 0 {
        0.0
    } else {
        probe.term_frequency as f64 * 100.0 / graph.total_codes as f64
    };
    format!(
        "TF {} · DF {} · {pct:.4}%",
        compact_u64(probe.term_frequency),
        compact_u64(probe.row_frequency)
    )
}

fn token_preview(probe: &Probe) -> String {
    probe
        .detail
        .split(" → byte ")
        .next()
        .unwrap_or(&probe.detail)
        .to_string()
}

/// Whether a node is in the cut, and whether the planner kept it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutMark {
    /// Not in the cut.
    No,
    /// In the cut and probed for.
    Probed,
    /// In the cut, but every id it names occurs nowhere, so no probe is issued.
    Pruned,
}

impl CutMark {
    fn of(node: usize, selected: &HashSet<usize>, pruned: &HashSet<usize>) -> Self {
        match (selected.contains(&node), pruned.contains(&node)) {
            (_, true) => Self::Pruned,
            (true, false) => Self::Probed,
            (false, false) => Self::No,
        }
    }

    /// The badge to stamp on the node, if any.
    fn badge(self) -> Option<(&'static str, f64, &'static str)> {
        match self {
            Self::No => None,
            Self::Probed => Some(("CUT", 30.0, "cut-badge-bg")),
            Self::Pruned => Some(("PRUNED", 50.0, "cut-badge-bg pruned")),
        }
    }
}

/// The `CUT` / `PRUNED` badge, hung off the top-right corner of `rect`.
fn cut_badge(rect: Box2d, cut: CutMark) -> String {
    let Some((label, width, class)) = cut.badge() else {
        return String::new();
    };
    format!(
        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{width:.1}\" height=\"15\" rx=\"7.5\" class=\"{class}\"/><text x=\"{:.1}\" y=\"{:.1}\" class=\"cut-badge\">{label}</text>",
        rect.right() - width - 5.0,
        rect.y - 7.5,
        rect.right() - width / 2.0 - 5.0,
        rect.y + 3.0
    )
}

fn render_probe(
    graph: &PathGraph,
    node_id: usize,
    rect: Box2d,
    cut: CutMark,
    reachable: bool,
) -> String {
    let node = &graph.nodes[node_id];
    let probe = node.probe.as_ref().expect("probe node has probe metadata");
    let class = match cut {
        CutMark::Probed => "probe selected",
        CutMark::Pruned => "probe pruned",
        CutMark::No if reachable => "probe",
        CutMark::No => "probe downstream",
    };
    let (title, detail) = match (&node.kind, &probe.set) {
        (
            NodeKind::Point {
                offset,
                next_offset,
            },
            ProbeSet::Point { id },
        ) => (
            token_preview(probe),
            format!("token {id} · p{offset}→p{next_offset}"),
        ),
        (NodeKind::TerminalRange { offset }, ProbeSet::Range { lo, hi }) => (
            "terminal token range".to_string(),
            format!("{} IDs ({lo}…{hi}) · p{offset}→match", probe.set.len()),
        ),
        _ => (probe.label.clone(), probe.detail.clone()),
    };

    let mut out = format!(
        "<g class=\"{class}\"><title>{}</title><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"8\"/>",
        xml(&format!("{}; {}", probe.label, probe.detail)),
        rect.x,
        rect.y,
        rect.w,
        rect.h
    );
    out.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"probe-title\">{}</text>",
        rect.cx(),
        rect.y + 20.0,
        xml(&title)
    ));
    out.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"probe-detail\">{}</text>",
        rect.cx(),
        rect.y + 40.0,
        xml(&detail)
    ));
    out.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"probe-freq\">{}</text>",
        rect.cx(),
        rect.y + 58.0,
        xml(&probe_frequency(graph, probe))
    ));
    out.push_str(&cut_badge(rect, cut));
    out.push_str("</g>");
    out
}

fn render_alignment(
    graph: &PathGraph,
    entry: &Alignment,
    rect: Box2d,
    selected: &HashSet<usize>,
    pruned: &HashSet<usize>,
) -> String {
    let cut = entry
        .first_set
        .map_or(CutMark::No, |node| CutMark::of(node, selected, pruned));
    let class = match cut {
        CutMark::Probed => "alignment selected",
        CutMark::Pruned => "alignment pruned",
        CutMark::No => "alignment",
    };
    let (detail, frequency) = if let Some(first_set) = entry.first_set {
        let probe = graph.nodes[first_set].probe.as_ref().unwrap();
        (probe.detail.clone(), Some(probe_frequency(graph, probe)))
    } else if entry.offset == 0 {
        ("starts at a token boundary".to_string(), None)
    } else {
        let NodeKind::Junction { label, .. } = &graph.nodes[entry.junction].kind else {
            unreachable!();
        };
        let detail = label
            .split_once("; ")
            .map_or("first-token set unavailable", |(_, reason)| reason);
        (format!("unprobed entry · {detail}"), None)
    };

    let mut out = format!(
        "<g class=\"{class}\"><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"9\"/>",
        rect.x, rect.y, rect.w, rect.h
    );
    out.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"alignment-title\">alignment k={}</text>",
        rect.x + 13.0,
        rect.y + 22.0,
        entry.offset
    ));
    out.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"alignment-detail\">{}</text>",
        rect.x + 13.0,
        rect.y + 43.0,
        xml(&detail)
    ));
    if let Some(frequency) = frequency {
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"alignment-freq\">{}</text>",
            rect.x + 13.0,
            rect.y + 62.0,
            xml(&frequency)
        ));
    }
    out.push_str(&cut_badge(rect, cut));
    out.push_str("</g>");
    out
}

fn render_state(
    graph: &PathGraph,
    node_id: usize,
    rect: Box2d,
    user_count: usize,
    incoming: usize,
    reachable: bool,
) -> String {
    let NodeKind::State { offset } = graph.nodes[node_id].kind else {
        unreachable!();
    };
    let merge = incoming > 1;
    let class = if !reachable {
        "state downstream"
    } else if merge {
        "state merge"
    } else {
        "state"
    };
    let mut out = format!(
        "<g class=\"{class}\"><title>needle byte offset {offset}; reached by {user_count} alignment path(s)</title><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"17\"/><text x=\"{:.1}\" y=\"{:.1}\" class=\"state-label\">p={offset}</text>",
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        rect.cx(),
        rect.cy() + 4.5
    );
    if merge {
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"merge-label\">merge ×{user_count}</text>",
            rect.cx(),
            rect.y - 8.0
        ));
    }
    out.push_str("</g>");
    out
}

fn summary_chip(x: f64, y: f64, width: f64, label: &str, value: &str) -> String {
    format!(
        "<g class=\"summary-chip\"><rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{width:.1}\" height=\"39\" rx=\"7\"/><text x=\"{:.1}\" y=\"{:.1}\" class=\"chip-label\">{}</text><text x=\"{:.1}\" y=\"{:.1}\" class=\"chip-value\">{}</text></g>",
        x + 10.0,
        y + 14.0,
        xml(label),
        x + 10.0,
        y + 31.0,
        xml(value)
    )
}

/// Render `graph` with `selected_cut` highlighted, as one self-contained SVG.
///
/// # Panics
/// If `graph` is not a DAG the builder could have produced — the layout asserts
/// its structural invariants rather than drawing something misleading.
pub fn render_svg(
    graph: &PathGraph,
    selected_cut: &[usize],
    pruned_cut: &[usize],
    summary: &RenderSummary,
) -> String {
    let adjacent = adjacency(graph);
    let entries = alignments(graph, &adjacent);
    let users = state_users(graph, &adjacent, &entries);
    let incoming = incoming_counts(graph);
    let state_x = state_positions(graph);
    let states = state_boxes(graph, &state_x, &users, &entries);
    let state_by_offset: HashMap<usize, usize> = graph
        .nodes
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::State { offset } => Some((offset, node.id)),
            _ => None,
        })
        .collect();
    let main_bottom = HEADER_H + 92.0 + entries.len().max(1) as f64 * LANE_GAP;
    let points = point_layouts(graph, &state_x, &states, &state_by_offset, main_bottom);
    let selected: HashSet<usize> = selected_cut.iter().copied().collect();
    let pruned: HashSet<usize> = pruned_cut.iter().copied().collect();
    let reachable = reachable_without_cut(graph, &selected);

    let last_state_x = state_x.values().next_back().copied().unwrap_or(GRAPH_LEFT);
    let accept_x = last_state_x + 235.0;
    let width = (accept_x + 95.0).max(1_150.0);
    let terminal_y = main_bottom + 34.0;
    let terminal_boxes = terminal_boxes(graph, &state_x, terminal_y);
    let terminal_bottom = terminal_boxes
        .values()
        .map(|rect| rect.bottom())
        .fold(terminal_y, f64::max);
    let rail_y = terminal_bottom + 52.0;
    let height = rail_y + 92.0;
    let accept_box = Box2d {
        x: accept_x,
        y: rail_y - 17.0,
        w: 76.0,
        h: 34.0,
    };
    let entry_boxes: HashMap<usize, Box2d> = entries
        .iter()
        .map(|entry| {
            (
                entry.junction,
                Box2d {
                    x: ENTRY_X,
                    y: entry.y - 37.0,
                    w: ENTRY_W,
                    h: 74.0,
                },
            )
        })
        .collect();
    let description = format!(
        "{} feasible token alignments converge into {} shared byte-offset states. The highlighted probes form a weighted vertex cut.",
        graph.stats.feasible_alignments, graph.stats.unique_states
    );
    // Keep the viewBox in layout units while giving viewers a practical
    // intrinsic size. The vector remains resolution independent in papers.
    let display_width = width / 2.0;
    let display_height = height / 2.0;
    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{display_width:.0}" height="{display_height:.0}" viewBox="0 0 {width:.0} {height:.0}" role="img" aria-labelledby="figure-title figure-desc">
<title id="figure-title">OnPair global probe cut for {}</title>
<desc id="figure-desc">{}</desc>
<defs>
  <marker id="arrow-active" markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto"><path d="M0,0 L0,7 L7,3.5 z" fill="#45666d"/></marker>
  <marker id="arrow-cut" markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto"><path d="M0,0 L0,7 L7,3.5 z" fill="#d55e00"/></marker>
  <marker id="arrow-muted" markerWidth="7" markerHeight="7" refX="6" refY="3.5" orient="auto"><path d="M0,0 L0,7 L7,3.5 z" fill="#cbd5d7"/></marker>
</defs>
<style>
  .background {{ fill:#ffffff; }}
  text {{ font-family:Inter,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; fill:#17252a; }}
  .heading {{ font-size:23px; font-weight:700; letter-spacing:-0.2px; }}
  .meta {{ font-size:12px; fill:#52666d; }}
  .needle {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:12px; fill:#243b42; }}
  .section-label {{ font-size:10px; font-weight:700; letter-spacing:1.2px; fill:#75888e; }}
  .summary-chip rect {{ fill:#f5f8f8; stroke:#dce5e6; stroke-width:1; }}
  .chip-label {{ font-size:8.5px; font-weight:700; letter-spacing:0.55px; fill:#718388; }}
  .chip-value {{ font-size:12px; font-weight:650; fill:#20363c; }}
  .alignment rect {{ fill:#f8faf9; stroke:#aabbbc; stroke-width:1.2; }}
  .alignment.selected rect:first-child {{ fill:#fff1e8; stroke:#d55e00; stroke-width:2.5; }}
  .alignment.pruned rect:first-child {{ fill:#f4f6f6; stroke:#8d989c; stroke-width:2.2; stroke-dasharray:5 3.5; }}
  .alignment-title {{ font-size:12px; font-weight:700; text-anchor:start; }}
  .alignment-detail {{ font-size:10.5px; fill:#52666d; text-anchor:start; }}
  .alignment-freq {{ font-size:9.5px; fill:#718388; text-anchor:start; font-variant-numeric:tabular-nums; }}
  .edge {{ fill:none; stroke-linecap:round; stroke-linejoin:round; }}
  .edge.active {{ stroke:#45666d; stroke-width:1.65; }}
  .edge.cut {{ stroke:#d55e00; stroke-width:2.35; }}
  .edge.muted {{ stroke:#cbd5d7; stroke-width:1.25; }}
  .edge.active.arrow {{ marker-end:url(#arrow-active); }}
  .edge.cut.arrow {{ marker-end:url(#arrow-cut); }}
  .edge.muted.arrow {{ marker-end:url(#arrow-muted); }}
  .callout-leader {{ fill:none; stroke-width:1; stroke-dasharray:2.5 2.5; }}
  .callout-leader.active {{ stroke:#84999e; }}
  .callout-leader.cut {{ stroke:#d55e00; stroke-width:1.5; }}
  .callout-leader.muted {{ stroke:#cbd5d7; }}
  .probe-anchor rect {{ fill:#ffffff; stroke:#45666d; stroke-width:1.6; }}
  .probe-anchor.selected rect {{ fill:#d55e00; stroke:#a94700; stroke-width:2; }}
  .probe-anchor.pruned rect {{ fill:#ffffff; stroke:#8d989c; stroke-width:2; stroke-dasharray:2 1.8; }}
  .probe-anchor.downstream {{ opacity:0.42; }}
  .state rect {{ fill:#f2f7f6; stroke:#36747a; stroke-width:1.4; }}
  .state.merge rect {{ fill:#176f73; stroke:#0d595d; stroke-width:1.6; }}
  .state.downstream {{ opacity:0.42; }}
  .state-label {{ font-size:10.5px; font-weight:700; text-anchor:middle; }}
  .state.merge .state-label {{ fill:#ffffff; }}
  .merge-label {{ font-size:9px; font-weight:700; fill:#176f73; text-anchor:middle; }}
  .probe rect:first-of-type {{ fill:#ffffff; stroke:#84999e; stroke-width:1.2; }}
  .probe.selected rect:first-of-type {{ fill:#fff1e8; stroke:#d55e00; stroke-width:2.5; }}
  .probe.pruned rect:first-of-type {{ fill:#f4f6f6; stroke:#8d989c; stroke-width:2.4; stroke-dasharray:5 3.5; }}
  .probe.downstream {{ opacity:0.42; }}
  .probe-title {{ font-size:11px; font-weight:700; text-anchor:middle; }}
  .probe-detail {{ font-size:9.5px; fill:#52666d; text-anchor:middle; }}
  .probe-freq {{ font-size:8.8px; fill:#718388; text-anchor:middle; font-variant-numeric:tabular-nums; }}
  .cut-badge-bg {{ fill:#d55e00 !important; stroke:none !important; }}
  .cut-badge-bg.pruned {{ fill:#8d989c !important; }}
  .cut-badge {{ font-size:8px; font-weight:800; fill:#ffffff; text-anchor:middle; }}
  .terminal-label {{ font-size:9.5px; font-weight:700; fill:#718388; letter-spacing:0.7px; }}
  .accept rect {{ fill:#20363c; stroke:#20363c; }}
  .accept text {{ font-size:10px; font-weight:750; fill:#ffffff; text-anchor:middle; }}
  .accept-rail {{ stroke:#9eafb2; stroke-width:1.4; }}
  .caption {{ font-size:10.5px; fill:#52666d; }}
</style>
<rect class="background" width="100%" height="100%"/>
"##,
        xml(&summary.subtitle),
        xml(&description)
    );

    svg.push_str(&format!(
        "<text id=\"figure-title-visible\" x=\"28\" y=\"32\" class=\"heading\">Global probe cut · {}</text>",
        xml(&summary.title)
    ));
    let selectivity = summary
        .selectivity
        .map(|value| format!(" · selectivity {:.6}%", value * 100.0))
        .unwrap_or_default();
    svg.push_str(&format!(
        "<text x=\"28\" y=\"55\" class=\"meta\">{}{selectivity} · {} B needle · {}</text>",
        xml(&summary.subtitle),
        graph.needle.len(),
        xml(metric_label(summary.metric))
    ));
    svg.push_str(&format!(
        "<text x=\"28\" y=\"79\" class=\"needle\">needle  “{}”</text>",
        xml(&needle_preview(&graph.needle))
    ));

    let mut chip_x = 28.0;
    let chip_y = 96.0;
    let mut chips = vec![
        (
            132.0,
            "ALIGNMENTS",
            graph.stats.feasible_alignments.to_string(),
        ),
        (
            164.0,
            "STATE VISITS → MERGED",
            format!(
                "{} → {}",
                graph.stats.state_visits_before_merge, graph.stats.unique_states
            ),
        ),
        (
            if summary.dead_probes > 0 {
                152.0
            } else {
                118.0
            },
            "CUT PROBES",
            if summary.dead_probes > 0 {
                format!("{} · {} pruned", selected_cut.len(), summary.dead_probes)
            } else {
                selected_cut.len().to_string()
            },
        ),
        (
            154.0,
            "CUT TOKEN FREQUENCY",
            compact_u64(summary.cut_member_frequency),
        ),
        (118.0, "SIMD COST", summary.cut_cmp_cost.to_string()),
    ];
    if let (Some(candidates), Some(exact)) = (summary.cut_candidates, summary.exact_rows) {
        chips.push((
            160.0,
            "CANDIDATES / EXACT",
            format!("{candidates} / {exact}"),
        ));
    }
    for (chip_width, label, value) in chips {
        svg.push_str(&summary_chip(chip_x, chip_y, chip_width, label, &value));
        chip_x += chip_width + 8.0;
    }
    let pruned_note = if summary.dead_probes > 0 {
        " Grey dashed PRUNED probes name only tokens absent from the column, so the scan never compares against them."
    } else {
        ""
    };
    svg.push_str(&format!(
        "<text x=\"28\" y=\"162\" class=\"caption\">Each row is a feasible starting alignment. Paths funnel together when greedy tokenization reaches the same needle byte offset.</text><text x=\"28\" y=\"179\" class=\"caption\">The orange probe set is a vertex cut: every route from an alignment to an accepting terminal range crosses it. Faded nodes lie downstream of the cut.{pruned_note}</text>"
    ));
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"198\" class=\"section-label\">ALIGNMENT ENTRY</text><text x=\"{:.1}\" y=\"198\" class=\"section-label\">TOKENIZED NEEDLE DAG  →</text>",
        ENTRY_X,
        GRAPH_LEFT - STATE_W / 2.0
    ));

    // Draw graph edges beneath all nodes.
    for entry in &entries {
        let a = entry_boxes[&entry.junction];
        let b = states[&entry.start_state];
        let selected_entry = entry.first_set.is_some_and(|node| selected.contains(&node));
        let tone = if selected_entry {
            EdgeTone::Muted
        } else if reachable.contains(&entry.start_state) {
            EdgeTone::Active
        } else {
            EdgeTone::Muted
        };
        svg.push_str(&edge(&alignment_entry_edge(a, b), tone, true));
    }
    for node in &graph.nodes {
        match node.kind {
            NodeKind::Point {
                offset,
                next_offset,
            } => {
                let source = states[&state_by_offset[&offset]];
                let probe = points[&node.id];
                let destination = states[&state_by_offset[&next_offset]];
                let into = edge_tone(state_by_offset[&offset], node.id, &selected, &reachable);
                let out = edge_tone(
                    node.id,
                    state_by_offset[&next_offset],
                    &selected,
                    &reachable,
                );
                svg.push_str(&edge(
                    &edge_to_point(source, probe.anchor_x, probe.anchor_y),
                    into,
                    false,
                ));
                svg.push_str(&edge(
                    &edge_from_point(probe.anchor_x, probe.anchor_y, destination),
                    out,
                    true,
                ));
                svg.push_str(&callout_leader(probe, into.class()));
            }
            NodeKind::TerminalRange { offset } => {
                let source_id = state_by_offset[&offset];
                let source = states[&source_id];
                let terminal = terminal_boxes[&node.id];
                let into = edge_tone(source_id, node.id, &selected, &reachable);
                svg.push_str(&edge(&vertical_edge(source, terminal), into, true));
                let out = if reachable.contains(&node.id) {
                    EdgeTone::Active
                } else {
                    EdgeTone::Muted
                };
                let path = format!(
                    "M {:.1} {:.1} V {rail_y:.1}",
                    terminal.cx(),
                    terminal.bottom()
                );
                svg.push_str(&edge(&path, out, false));
            }
            _ => {}
        }
    }
    svg.push_str(&format!(
        "<line x1=\"{:.1}\" y1=\"{rail_y:.1}\" x2=\"{:.1}\" y2=\"{rail_y:.1}\" class=\"accept-rail\"/>",
        state_x.values().next().copied().unwrap_or(GRAPH_LEFT) - 82.0,
        accept_box.x
    ));

    for entry in &entries {
        svg.push_str(&render_alignment(
            graph,
            entry,
            entry_boxes[&entry.junction],
            &selected,
            &pruned,
        ));
    }
    for node in &graph.nodes {
        if matches!(node.kind, NodeKind::State { .. }) {
            svg.push_str(&render_state(
                graph,
                node.id,
                states[&node.id],
                users[&node.id].len(),
                incoming[node.id],
                reachable.contains(&node.id),
            ));
        }
    }
    for node in &graph.nodes {
        if matches!(node.kind, NodeKind::Point { .. }) {
            svg.push_str(&render_probe_anchor(
                points[&node.id],
                CutMark::of(node.id, &selected, &pruned),
                reachable.contains(&node.id),
            ));
            svg.push_str(&render_probe(
                graph,
                node.id,
                points[&node.id].card,
                CutMark::of(node.id, &selected, &pruned),
                reachable.contains(&node.id),
            ));
        }
    }
    if !terminal_boxes.is_empty() {
        svg.push_str(&format!(
            "<text x=\"28\" y=\"{:.1}\" class=\"terminal-label\">ACCEPTING TERMINAL PROBES</text>",
            terminal_y - 15.0
        ));
    }
    for node in &graph.nodes {
        if matches!(node.kind, NodeKind::TerminalRange { .. }) {
            svg.push_str(&render_probe(
                graph,
                node.id,
                terminal_boxes[&node.id],
                CutMark::of(node.id, &selected, &pruned),
                reachable.contains(&node.id),
            ));
        }
    }
    svg.push_str(&format!(
        "<g class=\"accept\"><rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"17\"/><text x=\"{:.1}\" y=\"{:.1}\">MATCH</text></g>",
        accept_box.x,
        accept_box.y,
        accept_box.w,
        accept_box.h,
        accept_box.cx(),
        accept_box.cy() + 4.0
    ));
    svg.push_str(&format!(
        "<text x=\"28\" y=\"{:.1}\" class=\"caption\">p denotes a needle byte offset. TF counts token occurrences; DF sums per-token row frequencies. The percentage is TF / encoded token count, not row selectivity.</text>",
        height - 22.0
    ));
    svg.push_str("</svg>\n");
    svg
}
