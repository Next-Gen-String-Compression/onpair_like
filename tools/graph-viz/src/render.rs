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

use onpair::Token;
use serde::Serialize;

use crate::graph::{
    CoverShape, CoverTerm, EdgeKind, GraphEdge, PathGraph, Probe, ProbeSet, ProbeWeight,
};

const ENTRY_X: f64 = 28.0;
const ENTRY_W: f64 = 250.0;
/// Entry card with no token list. Each listed line adds a [`TOKEN_LINE_H`].
const ENTRY_BASE_H: f64 = 74.0;
const TOKEN_LINE_H: f64 = 13.0;
/// A predicate row is two lines — terms, then their estimated shares — plus
/// breathing space.
const PREDICATE_ROW_H: f64 = 32.0;
/// A measured row is one line of shares under the same columns.
const PREDICATE_MEASURED_ROW_H: f64 = 17.0;
/// Characters of a listed token line, at the monospace size the list is set in.
const ENTRY_TOKEN_CHARS: usize = 40;
/// Clear space between one entry card and the next, which is what a taller card
/// has to buy back from the lane pitch.
const ENTRY_LANE_MARGIN: f64 = 34.0;
const GRAPH_LEFT: f64 = 350.0;
/// Header height with one caption line; every other line adds or reclaims one
/// [`CAPTION_LINE_H`], down to a figure that needs no caption at all.
const HEADER_BASE_H: f64 = 175.0;
/// Baseline of the first caption line, just under the chip row.
const CAPTION_TOP: f64 = 152.0;
const CAPTION_LINE_H: f64 = 17.0;
const LANE_GAP: f64 = 108.0;
/// Space between the header and the first lane. Enough for a callout card to sit
/// *above* that lane: without it, a step leaving the top lane can only put its
/// card below, which is where its own edge is going.
const LANE_TOP: f64 = 112.0;
const STATE_W: f64 = 54.0;
const STATE_H: f64 = 34.0;
/// Card width with a title of ordinary length. A card whose token is wider grows
/// past this rather than eliding it — see [`card_w`].
const PROBE_W: f64 = 152.0;
const PROBE_H: f64 = 70.0;
const TERMINAL_W: f64 = 200.0;
const TERMINAL_H: f64 = 72.0;

/// The numbers printed in the figure's header, around the graph itself.
///
/// The distinction the chips are careful about: the **cut** is a set of DAG nodes
/// and the **cover** is what OnPair actually probes for — the cut's ids, minus the
/// ones pruned as never used, plus the mandatory tokens that hold the whole needle
/// and are therefore not in the DAG at all. They are different sets, so a cut of
/// weight zero can still leave a cover with several probes.
///
/// Only [`metric`](Self::metric), the weights and the labels are required. The
/// optional fields are measurements over a column; when absent their chips are
/// left out rather than shown as zero.
#[derive(Clone, Debug, Serialize)]
pub struct RenderSummary {
    /// Headline, e.g. the column or corpus this dictionary was trained on.
    pub title: String,
    /// Second line: whatever identifies this particular query.
    pub subtitle: String,
    /// Which weight the cut minimized.
    pub metric: ProbeWeight,
    /// The minimum cut's objective value under [`metric`](Self::metric). Zero
    /// means no occurrence crosses a token boundary in this column — not that
    /// nothing matches.
    pub cut_value: u64,
    /// Dictionary tokens holding the whole needle. Mandatory members of every
    /// cover, and outside the DAG, so no drawn probe stands for them.
    pub contained_tokens: usize,
    /// Their term frequency.
    pub contained_frequency: u64,
    /// Term frequency of the whole cover, contained ids included.
    pub cover_frequency: u64,
    /// The terms of the scan's disjunction — what the figure spells out as the
    /// predicate a row is admitted by.
    pub cover_terms: Vec<CoverTerm>,
    /// Probes the cover compiles to: one per maximal run of member ids.
    pub cover_probes: usize,
    /// Single-id probes among them.
    pub cover_points: usize,
    /// Id-range probes among them.
    pub cover_ranges: usize,
    /// Fraction of rows that truly match, if measured.
    pub selectivity: Option<f64>,
    /// Rows in the column, so a frequency can be read as a share of them. Known
    /// whenever a column was in hand, measured or not.
    pub column_rows: Option<u64>,
    /// Rows the cover admits, if measured.
    pub cut_candidates: Option<usize>,
    /// Share of the column's rows the cover admits, if measured. Not derivable
    /// from the per-term shares: a row holding two terms is admitted once.
    pub candidate_selectivity: Option<f64>,
    /// Rows that truly match, if measured.
    pub exact_rows: Option<usize>,
}

impl RenderSummary {
    /// A summary with labels and cover weights only.
    ///
    /// [`cut_value`](Self::cut_value), the contained-token fields and
    /// [`dead_probes`](Self::dead_probes) default to zero; the caller fills in
    /// what it knows.
    pub fn new(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        metric: ProbeWeight,
        cover_frequency: u64,
        cover: CoverShape,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            metric,
            cut_value: 0,
            contained_tokens: 0,
            contained_frequency: 0,
            cover_frequency,
            cover_terms: Vec::new(),
            cover_probes: cover.cmp_cost,
            cover_points: cover.points,
            cover_ranges: cover.ranges,
            selectivity: None,
            column_rows: None,
            cut_candidates: None,
            candidate_selectivity: None,
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

    /// Area shared with `other`, zero when they do not meet. Used to pick the
    /// least-bad placement when nothing is free.
    fn overlap_area(self, other: Self) -> f64 {
        let x = (self.right().min(other.right()) - self.x.max(other.x)).max(0.0);
        let y = (self.bottom().min(other.bottom()) - self.y.max(other.y)).max(0.0);
        x * y
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
    /// The alignment's `k`: needle bytes its first token holds at its tail.
    offset: usize,
    /// The entry step's edge id, or `None` for `k = 0`, which needs none.
    entry: Option<usize>,
    /// Lane centre.
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
pub(crate) fn xml(text: &str) -> String {
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

pub(crate) fn compact_u64(value: u64) -> String {
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

/// Edge ids leaving each needle offset.
fn adjacency(graph: &PathGraph) -> Vec<Vec<usize>> {
    let mut out = vec![Vec::new(); graph.end() + 1];
    for edge in &graph.edges {
        out[edge.from].push(edge.id);
    }
    out
}

/// Steps arriving at each needle offset.
fn incoming_counts(graph: &PathGraph) -> Vec<usize> {
    let mut counts = vec![0usize; graph.end() + 1];
    for edge in &graph.edges {
        counts[edge.to] += 1;
    }
    counts
}

/// One lane per alignment a layout can enter through, top to bottom.
///
/// Alignment 0 has no entry step — its layouts begin at the start node — so it is
/// a lane with no probe rather than a lane the graph names.
fn alignments(graph: &PathGraph, header_h: f64, lane_gap: f64) -> Vec<Alignment> {
    let mut entries = vec![Alignment {
        offset: 0,
        entry: None,
        y: 0.0,
    }];
    for edge in &graph.edges {
        if let EdgeKind::Entry { alignment, .. } = edge.kind {
            entries.push(Alignment {
                offset: alignment,
                entry: Some(edge.id),
                y: 0.0,
            });
        }
    }
    entries.sort_by_key(|entry| entry.offset);
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.y = header_h + LANE_TOP + index as f64 * lane_gap;
    }
    entries
}

/// Which alignments' layouts pass through each state, by walking forward from
/// each lane's own starting offset.
fn state_users(
    graph: &PathGraph,
    adjacent: &[Vec<usize>],
    entries: &[Alignment],
) -> HashMap<usize, Vec<usize>> {
    let mut users = HashMap::<usize, Vec<usize>>::new();
    for (lane, entry) in entries.iter().enumerate() {
        let mut stack = vec![entry.offset];
        let mut seen = HashSet::new();
        while let Some(offset) = stack.pop() {
            if !seen.insert(offset) {
                continue;
            }
            if offset != graph.end() {
                users.entry(offset).or_default().push(lane);
            }
            for &id in &adjacent[offset] {
                // An entry step belongs to its own lane, not to this walk.
                if !matches!(graph.edges[id].kind, EdgeKind::Entry { .. }) {
                    stack.push(graph.edges[id].to);
                }
            }
        }
    }
    users
}

/// How much room an edge running along a lane is given, so a card placed beside
/// it does not end up with an arrow through it.
const EDGE_CORRIDOR_H: f64 = 22.0;

/// How far back from a state an entry edge starts bending towards it.
const ENTRY_BEND_W: f64 = 40.0;

/// Minimum clear space between a card and the next state column, so an edge
/// running between two states never passes behind a card.
const CARD_GUTTER: f64 = 26.0;

fn state_positions(graph: &PathGraph, min_gap: f64) -> BTreeMap<usize, f64> {
    // The accept position is the MATCH rail, not a state pill.
    let offsets: Vec<usize> = graph
        .states
        .iter()
        .copied()
        .filter(|&offset| offset != graph.end())
        .collect();
    let scale = (1_650.0 / graph.needle.len().max(1) as f64).clamp(21.0, 72.0);
    let mut positions = BTreeMap::new();
    let mut previous = GRAPH_LEFT - min_gap;
    for offset in offsets {
        let ideal = GRAPH_LEFT + offset as f64 * scale;
        let x = ideal.max(previous + min_gap);
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
    state_x
        .iter()
        .map(|(&offset, &x)| {
            let lanes = users
                .get(&offset)
                .expect("every state is reached from some alignment");
            let y = lanes
                .iter()
                .map(|&lane| entries[lane].y)
                .sum::<f64>()
                / lanes.len() as f64;
            let _ = graph;
            (
                offset,
                Box2d {
                    x: x - STATE_W / 2.0,
                    y: y - STATE_H / 2.0,
                    w: STATE_W,
                    h: STATE_H,
                },
            )
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
    // Least overlap wins when nothing is free, rather than the first thing tried.
    // The old fallback clamped to the top of the band and could drop a card onto
    // the mandatory-token card in the row below it.
    let mut best: Option<(f64, Box2d)> = None;
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
            let overlap: f64 = obstacles
                .iter()
                .map(|other| desired.padded(5.0).overlap_area(*other))
                .sum();
            if overlap == 0.0 {
                return desired;
            }
            if best.is_none_or(|(least, _)| overlap < least) {
                best = Some((overlap, desired));
            }
        }
    }
    if let Some((_, rect)) = best {
        return rect;
    }
    // Nothing fit the band at all. Stay inside it anyway: a card outside the band
    // is a card on top of whatever the next row of the figure holds.
    desired.y = (anchor_y - desired.h - 30.0).clamp(min_y, (max_y - desired.h).max(min_y));
    desired
}

fn point_layouts(
    graph: &PathGraph,
    state_x: &BTreeMap<usize, f64>,
    states: &HashMap<usize, Box2d>,
    entries: &[Alignment],
    header_h: f64,
    graph_bottom: f64,
) -> HashMap<usize, PointLayout> {
    let mut steps: Vec<&GraphEdge> = graph
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::Step { .. }))
        .collect();
    steps.sort_by_key(|edge| edge.from);

    let mut obstacles: Vec<Box2d> = states.values().map(|rect| rect.padded(8.0)).collect();
    // The lane an alignment enters on is a corridor its edge runs along, all the
    // way from the entry card to its first state. A card parked on one is a card
    // with an arrow through it.
    for entry in entries {
        let Some(&target) = state_x.get(&entry.offset) else {
            continue;
        };
        let left = target - STATE_W / 2.0;
        // The run along the lane…
        obstacles.push(Box2d {
            x: ENTRY_X + ENTRY_W,
            y: entry.y - EDGE_CORRIDOR_H / 2.0,
            w: (left - (ENTRY_X + ENTRY_W)).max(0.0),
            h: EDGE_CORRIDOR_H,
        });
        // …and the bend up or down into the state, which is where the run ends
        // and where most of the collisions were.
        let arrive = states.get(&entry.offset).map_or(entry.y, |rect| rect.cy());
        obstacles.push(Box2d {
            x: left - ENTRY_BEND_W,
            y: entry.y.min(arrive) - EDGE_CORRIDOR_H / 2.0,
            w: ENTRY_BEND_W,
            h: (entry.y - arrive).abs() + EDGE_CORRIDOR_H,
        });
    }
    let mut layouts = HashMap::new();
    for (index, edge) in steps.into_iter().enumerate() {
        let (offset, next_offset) = (edge.from, edge.to);
        let source_state = states[&offset];
        let width = card_w(
            PROBE_W,
            &edge.probe.as_ref().map_or_else(String::new, token_preview),
        );

        // States the step flies over. With none, it runs along the row and the
        // anchor sits in the gap after the source state — never on a state, since
        // a state has only one step leaving it. With some, the step has to arc
        // past them, and the anchor is then the top of that arc: a card belongs
        // over the middle of the arc it labels, not over one end of it.
        let flown: Vec<Box2d> = state_x
            .range(offset + 1..next_offset)
            .map(|(between, _)| states[between])
            .collect();
        let (anchor_x, anchor_y, prefer_above) = if flown.is_empty() {
            let next_x = state_x
                .range(offset + 1..)
                .next()
                .map_or_else(|| state_x[&offset], |(_, &x)| x);
            // Away from where the step is heading. States merge across lanes, so a
            // step can leave one lane and arrive in another, and a card parked on
            // the side it descends towards is a card with its own arrow through
            // it. Level steps keep alternating, which spreads them out.
            let arrive = states
                .get(&next_offset)
                .map_or(source_state.cy(), |rect| rect.cy());
            let prefer_above = match arrive.partial_cmp(&source_state.cy()) {
                Some(std::cmp::Ordering::Greater) => true,
                Some(std::cmp::Ordering::Less) => false,
                _ => index % 2 == 0,
            };
            (
                (state_x[&offset] + next_x) / 2.0,
                source_state.cy(),
                prefer_above,
            )
        } else {
            let destination = state_x
                .get(&next_offset)
                .copied()
                .unwrap_or_else(|| state_x[&offset]);
            let arrive = states
                .get(&next_offset)
                .map_or(source_state.cy(), |rect| rect.cy());
            let above = flown
                .iter()
                .map(|rect| rect.y - APEX_LIFT)
                .fold(source_state.cy().min(arrive), f64::min);
            ((state_x[&offset] + destination) / 2.0, above, true)
        };

        let desired = Box2d {
            x: anchor_x - width / 2.0,
            y: 0.0,
            w: width,
            h: PROBE_H,
        };
        let placed = place_callout(
            desired,
            anchor_y,
            prefer_above,
            &obstacles,
            header_h + 12.0,
            graph_bottom - 16.0,
        );
        obstacles.push(placed.padded(5.0));
        // The anchor too: a lifted anchor sits where cards go, and a card covering
        // one — its own or a neighbour's — hides where the probe attaches.
        obstacles.push(Box2d {
            x: anchor_x - 9.0,
            y: anchor_y - 9.0,
            w: 18.0,
            h: 18.0,
        });
        layouts.insert(
            edge.id,
            PointLayout {
                anchor_x,
                anchor_y,
                card: placed,
            },
        );
    }
    layouts
}

/// Places the accepting-terminal cards, packing them into rows so none collides.
///
/// `occupied` is what already stands in the first row — the mandatory-token card,
/// which lives in the same band but is placed by its own rule. A terminal card
/// centred on the leftmost state can be wide enough to reach it.
fn terminal_boxes(
    graph: &PathGraph,
    state_x: &BTreeMap<usize, f64>,
    terminal_top: f64,
    occupied: Option<Box2d>,
) -> HashMap<usize, Box2d> {
    // Every step that lands on the accept position gets a card in this row: a
    // terminal range, the whole-needle step, or an escape.
    let mut terminals: Vec<(usize, usize)> = graph
        .edges
        .iter()
        .filter(|edge| edge.to == graph.end())
        .map(|edge| (edge.from, edge.id))
        .collect();
    terminals.sort_unstable();

    let mut rows = Vec::<Vec<Box2d>>::new();
    if let Some(rect) = occupied {
        rows.push(vec![rect]);
    }
    let mut boxes = HashMap::new();
    for (offset, edge_id) in terminals {
        let width = card_w(TERMINAL_W, &terminal_title(graph, edge_id));
        let row_y = |row: usize| terminal_top + row as f64 * (TERMINAL_H + 24.0);
        let candidate = |row: usize| Box2d {
            x: state_x[&offset] - width / 2.0,
            y: row_y(row),
            w: width,
            h: TERMINAL_H,
        };
        // Test the box where it would actually go. Testing it at row 0's height
        // would clear every later row for free and pack collisions into row 1.
        let row = (0..)
            .find(|&row| {
                rows.get(row).is_none_or(|placed| {
                    placed
                        .iter()
                        .all(|other| !candidate(row).padded(8.0).overlaps(*other))
                })
            })
            .unwrap();
        if row == rows.len() {
            rows.push(Vec::new());
        }
        let rect = candidate(row);
        rows[row].push(rect);
        boxes.insert(edge_id, rect);
    }
    boxes
}

/// The title an accepting card carries, which is what it has to be wide enough
/// for.
fn terminal_title(graph: &PathGraph, edge_id: usize) -> String {
    let edge = &graph.edges[edge_id];
    match (edge.kind, edge.probe.as_ref().map(|probe| &probe.set)) {
        (EdgeKind::Contained { .. }, Some(ProbeSet::Explicit { ids })) => {
            token_list(graph, ids, 34)
        }
        (EdgeKind::Contained { .. }, _) => "contains the whole needle".to_string(),
        (_, Some(&ProbeSet::Range { lo, hi })) => token_span(graph, lo, hi),
        (_, Some(&ProbeSet::Point { id })) => token_span(graph, id, id),
        _ => String::new(),
    }
}

/// Offsets still reachable from the start once the cut's steps are removed.
/// Everything past the cut is drawn faded: the cover has already stopped it.
fn reachable_without_cut(graph: &PathGraph, cut: &HashSet<usize>) -> HashSet<usize> {
    let adjacent = adjacency(graph);
    let mut reachable = HashSet::new();
    let mut queue = VecDeque::from([0usize]);
    while let Some(offset) = queue.pop_front() {
        if !reachable.insert(offset) {
            continue;
        }
        for &id in &adjacent[offset] {
            if !cut.contains(&id) {
                queue.push_back(graph.edges[id].to);
            }
        }
    }
    reachable
}

/// A step's tone: cut, live, or already stopped by the cover upstream.
fn edge_tone(
    edge: &GraphEdge,
    cut: &HashSet<usize>,
    reachable: &HashSet<usize>,
) -> EdgeTone {
    if cut.contains(&edge.id) {
        EdgeTone::Cut
    } else if reachable.contains(&edge.from) && reachable.contains(&edge.to) {
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

/// Clearance an edge keeps from a state box it has to get past.
const EDGE_CLEARANCE: f64 = 20.0;

/// How far above the states it flies over a probe's anchor sits. More than
/// [`EDGE_CLEARANCE`] so the anchor clears a merged state's `merge ×n` label,
/// which is drawn just above the state box.
const APEX_LIFT: f64 = 34.0;

/// A point on the cubic `p0 → c1 → c2 → p3` at `t`.
fn cubic_at(p0: (f64, f64), c1: (f64, f64), c2: (f64, f64), p3: (f64, f64), t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        a * p0.0 + b * c1.0 + c * c2.0 + d * p3.0,
        a * p0.1 + b * c1.1 + c * c2.1 + d * p3.1,
    )
}

/// How much of a cubic lies inside `boxes`, sampled densely enough that a state
/// box — 54 by 34 units — cannot fit between two samples. Zero means it clears.
fn cubic_intrusion(
    p0: (f64, f64),
    c1: (f64, f64),
    c2: (f64, f64),
    p3: (f64, f64),
    boxes: &[&Box2d],
) -> usize {
    const SAMPLES: usize = 96;
    (0..=SAMPLES)
        .filter(|&step| {
            let (x, y) = cubic_at(p0, c1, c2, p3, step as f64 / SAMPLES as f64);
            boxes
                .iter()
                .any(|rect| x > rect.x && x < rect.right() && y > rect.y && y < rect.bottom())
        })
        .count()
}

/// A curve from `p0` to `p3` that keeps out of `over`.
///
/// The route is chosen by trying shapes and checking them, not by a formula.
/// Formulas were tried: aiming the control points at a clearance line works when
/// both ends sit in the same lane and does nothing at all when they do not, since
/// a control point between the two ends bends the curve without ever taking it
/// outside them. The unbowed shape is tried first, so an edge with nothing in its
/// way keeps the figure it always had.
///
/// `card_above` only breaks ties. When an end already sits clear of the boxes on
/// one side, that side is tried first — an edge leaving a lifted anchor should stay
/// up rather than dive under everything and come back.
fn routed_curve(
    p0: (f64, f64),
    p3: (f64, f64),
    over: &[Box2d],
    soft: &[Box2d],
    card_above: bool,
) -> String {
    let (x1, y1) = p0;
    let (x2, y2) = p3;
    let bend = ((x2 - x1).abs() * 0.45).max(20.0);
    let plain = |c1y: f64, c2y: f64| {
        format!(
            "M {x1:.1} {y1:.1} C {:.1} {c1y:.1}, {:.1} {c2y:.1}, {x2:.1} {y2:.1}",
            x1 + bend,
            x2 - bend
        )
    };

    let low = x1.min(x2);
    let high = x1.max(x2);
    let in_span = |rect: &&Box2d| rect.x < high && rect.right() > low;
    let blocking: Vec<&Box2d> = over.iter().filter(in_span).collect();
    let nudging: Vec<&Box2d> = soft.iter().filter(in_span).collect();
    // A state pill is a node; a card is a label beside one. Crossing the first is
    // wrong, crossing the second is untidy, so they are not traded one for one.
    const STATE_COST: usize = 1_000;
    let cost = |c1: (f64, f64), c2: (f64, f64)| {
        cubic_intrusion(p0, c1, c2, p3, &blocking) * STATE_COST
            + cubic_intrusion(p0, c1, c2, p3, &nudging)
    };
    // Only bow if the plain shape really hits something. A box merely sharing the
    // edge's x-span is not in its way — the ends can already be above or below it,
    // and bowing toward a clearance line would then drag the curve *at* the box.
    let straight = cost((x1 + bend, y1), (x2 - bend, y2));
    if straight == 0 {
        return plain(y1, y2);
    }

    let clearance = || blocking.iter().chain(&nudging);
    let below = clearance()
        .map(|rect| rect.bottom() + EDGE_CLEARANCE)
        .fold(f64::MIN, f64::max);
    let above = clearance()
        .map(|rect| rect.y - EDGE_CLEARANCE)
        .fold(f64::MAX, f64::min);
    if !below.is_finite() || !above.is_finite() {
        return plain(y1, y2);
    }

    // Which side an end is already on wins over which side the card is on.
    let ends_above = [y1, y2]
        .iter()
        .any(|&y| clearance().all(|rect| y <= rect.y));
    let ends_below = [y1, y2]
        .iter()
        .any(|&y| clearance().all(|rect| y >= rect.bottom()));
    let above_first = match (ends_above, ends_below) {
        (true, false) => true,
        (false, true) => false,
        _ => !card_above,
    };

    let mut routes: Vec<f64> = Vec::new();
    for push in [0.0, 34.0, 78.0, 140.0] {
        if above_first {
            routes.push(above - push);
            routes.push(below + push);
        } else {
            routes.push(below + push);
            routes.push(above - push);
        }
    }

    // Nothing may clear — a card can sit right where the edge has to go. Then the
    // least-intruding route wins, and the unbowed shape is the tie-breaker: a wild
    // bow that still crosses something is worse than a straight line that does.
    let mut best = (straight, plain(y1, y2));
    for control_y in routes {
        let intrusion = cost((x1 + bend, control_y), (x2 - bend, control_y));
        if intrusion == 0 {
            return plain(control_y, control_y);
        }
        if intrusion < best.0 {
            best = (intrusion, plain(control_y, control_y));
        }
    }
    best.1
}

fn edge_to_point(
    a: Box2d,
    x2: f64,
    y2: f64,
    over: &[Box2d],
    soft: &[Box2d],
    card_above: bool,
) -> String {
    routed_curve((a.right(), a.cy()), (x2, y2), over, soft, card_above)
}

fn edge_from_point(
    x1: f64,
    y1: f64,
    b: Box2d,
    over: &[Box2d],
    soft: &[Box2d],
    card_above: bool,
) -> String {
    routed_curve((x1, y1), (b.x, b.cy()), over, soft, card_above)
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

/// A frequency as a percentage of the column's rows, clamped to 100%.
///
/// This is the figure's one meaning for a percentage. A frequency counts token
/// occurrences, and over the row count it is the share of rows the token is priced
/// as admitting — an upper bound, because a row holding it twice counts twice.
/// Shares of the *code stream* were what this printed before, and they answer a
/// question nobody asks: a scan skips rows, not codes.
fn row_share(graph: &PathGraph, frequency: u64) -> f64 {
    if graph.column_rows == 0 {
        0.0
    } else {
        (frequency as f64 * 100.0 / graph.column_rows as f64).min(100.0)
    }
}

/// `TF … · DF … · …%`, with the `DF` field left out when no rows were counted.
///
/// Row frequency is not in OnPair's index, so it only exists when the weights came
/// from a column ([`Weights::from_column`](crate::Weights::from_column), which the
/// `df` metrics require). Under a `tf` metric the field would otherwise print the
/// term frequency again, which reads as a measurement and is not one.
fn probe_frequency(graph: &PathGraph, probe: &Probe) -> String {
    let rf = row_share(graph, probe.term_frequency);
    if graph.total_rows == 0 {
        format!("TF {} · RF {rf:.4}%", compact_u64(probe.term_frequency))
    } else {
        format!(
            "TF {} · DF {} · RF {rf:.4}%",
            compact_u64(probe.term_frequency),
            compact_u64(probe.row_frequency)
        )
    }
}

fn token_preview(probe: &Probe) -> String {
    probe
        .detail
        .split(" → byte ")
        .next()
        .unwrap_or(&probe.detail)
        .to_string()
}

/// `"s"`, or `"c" … "css"` for a range of more than one token. Ranges are
/// contiguous in a sorted dictionary, so the two ends say what the probe spans
/// more usefully than its id bounds do.
fn token_span(graph: &PathGraph, lo: Token, hi: Token) -> String {
    let text = |id: Token| {
        graph
            .token_text
            .get(id as usize)
            .map_or_else(|| format!("#{id}"), |token| format!("\"{token}\""))
    };
    if lo == hi {
        text(lo)
    } else {
        format!("{} … {}", text(lo), text(hi))
    }
}

/// `ids` as their tokens, quoted and comma-separated, the tail replaced by a
/// count once the text would outgrow `budget` characters.
fn token_list(graph: &PathGraph, ids: &[Token], budget: usize) -> String {
    let mut shown = Vec::new();
    let mut length = 0usize;
    for &id in ids {
        let token = token_span(graph, id, id);
        let next = length + token.chars().count() + usize::from(!shown.is_empty()) * 2;
        if !shown.is_empty() && next > budget {
            break;
        }
        length = next;
        shown.push(token);
    }
    let hidden = ids.len() - shown.len();
    let text = shown.join(", ");
    if hidden == 0 {
        text
    } else {
        format!("{text} +{hidden}")
    }
}

/// The predicate the scan evaluates, as one line of text.
///
/// A disjunction, always: a term per probe, a point spelled as its token and a run
/// as its two ends. There is no conjunction to show — the cut put a term on every
/// path an occurrence could take, so one hit is enough to admit the row. The
/// conjunctive reading (*this* token **and** its continuation) belongs to a single
/// path through the DAG, and relaxing it to a disjunction over paths is exactly
/// what makes the cover one comparison per term instead of positional bookkeeping.
pub fn predicate_text(graph: &PathGraph, terms: &[CoverTerm]) -> String {
    if terms.is_empty() {
        return "never — pruning left no probe, so no row can match".to_string();
    }
    terms
        .iter()
        .map(|term| predicate_term(graph, *term))
        .collect::<Vec<_>>()
        .join(" ∨ ")
}

/// One term: a token to equal, or a bracketed range to fall inside. The brackets
/// are the whole point — `"a" … "b"` on its own reads as an elision rather than as
/// the id-range comparison the scan actually issues.
fn predicate_term(graph: &PathGraph, term: CoverTerm) -> String {
    if term.begin == term.last {
        token_span(graph, term.begin, term.begin)
    } else {
        format!("[{}]", token_span(graph, term.begin, term.last))
    }
}

/// Terms the panel spells out before summarizing the rest as a count. A cover of
/// a common substring runs to dozens of terms, which is a fact worth stating and
/// not worth drawing.
const PREDICATE_TERM_CAP: usize = 24;

/// Width of a predicate character at `.predicate-term`'s monospace size.
const PREDICATE_GLYPH_W: f64 = 6.65;

/// The `= …` clause closing the estimate line: what the terms add up to.
///
/// An addition of upper bounds, so an upper bound itself, and above the measured
/// share whenever a row holds more than one term.
fn predicate_estimate_total(summary: &RenderSummary) -> String {
    format!(
        "= {} of rows, estimated",
        row_share_text(summary.cover_frequency, summary.column_rows)
    )
}

/// The `= …` clause closing the measured line: what the prefilter really admits,
/// what really matches, and the gap between them.
///
/// The gap is the whole point of the line. A prefilter is judged by what it hands
/// the verifier that the verifier then throws away, and that number cannot be
/// derived from any estimate above — only counted.
fn predicate_measured_total(summary: &RenderSummary) -> String {
    let (Some(candidates), Some(rows)) = (summary.cut_candidates, summary.column_rows) else {
        return "not measured".to_string();
    };
    let admitted = row_share_text(candidates as u64, Some(rows));
    let Some(exact) = summary.exact_rows else {
        return format!("= {admitted} of rows admitted");
    };
    let false_positives = candidates.saturating_sub(exact);
    let waste = if candidates == 0 {
        0.0
    } else {
        false_positives as f64 / candidates as f64 * 100.0
    };
    format!(
        "= {admitted} admitted vs {} matching → {} false positive{} ({waste:.2}% of the candidates)",
        row_share_text(exact as u64, Some(rows)),
        compact_u64(false_positives as u64),
        plural(false_positives)
    )
}

/// One column of the predicate panel: a term, the rows it is estimated to admit,
/// and the rows it really does.
struct PredicateColumn {
    term: String,
    estimate: String,
    measured: String,
}

impl PredicateColumn {
    /// Characters the column occupies: whichever of its lines is widest — a share
    /// can outrun a short term like `"og"`.
    fn width(&self) -> usize {
        self.term
            .chars()
            .count()
            .max(self.estimate.chars().count())
            .max(self.measured.chars().count())
    }
}

/// A count as a percentage of `rows`, clamped to 100%.
///
/// The clamp is for the estimate, not the measurement: term frequency over the
/// row count is an upper bound on the rows a term admits, and a token that
/// repeats inside rows can push it past every row there is.
fn row_share_text(count: u64, rows: Option<u64>) -> String {
    match rows {
        Some(rows) if rows > 0 => {
            let share = (count as f64 / rows as f64 * 100.0).min(100.0);
            format!("{share:.4}%")
        }
        _ => "–".to_string(),
    }
}

/// The predicate as columns: each term with the share of the code stream it
/// admits, and the terms past [`PREDICATE_TERM_CAP`] collapsed into one column
/// carrying their combined share — so the addition the panel prints still holds.
fn predicate_columns(
    graph: &PathGraph,
    terms: &[CoverTerm],
    rows: Option<u64>,
) -> Vec<PredicateColumn> {
    let measured = |term: &CoverTerm| {
        term.measured_rows
            .map_or_else(|| "not measured".to_string(), |count| {
                row_share_text(count, rows)
            })
    };
    let shown = terms.len().min(PREDICATE_TERM_CAP);
    let mut columns: Vec<PredicateColumn> = terms[..shown]
        .iter()
        .map(|term| PredicateColumn {
            term: predicate_term(graph, *term),
            estimate: row_share_text(term.term_frequency, rows),
            measured: measured(term),
        })
        .collect();
    if terms.len() > shown {
        let hidden = &terms[shown..];
        let frequency: u64 = hidden.iter().map(|term| term.term_frequency).sum();
        // The tail's measured share is not a sum — two of its terms can hold the
        // same row — so it is left blank rather than guessed at.
        columns.push(PredicateColumn {
            term: format!("… {} more term{}", hidden.len(), plural(hidden.len())),
            estimate: row_share_text(frequency, rows),
            measured: String::new(),
        });
    }
    columns
}

/// Pack columns into rows of at most `budget` characters, counting the ` ∨ `
/// between them. `tail` is the total clause, which has to fit on the last row or
/// take a row of its own.
fn predicate_rows(columns: &[PredicateColumn], budget: usize, tail: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut used = 0usize;
    for (index, column) in columns.iter().enumerate() {
        let fits = !rows.is_empty() && used + 3 + column.width() <= budget;
        if fits {
            rows.last_mut().expect("fits implies a row").push(index);
            used += 3 + column.width();
        } else {
            rows.push(vec![index]);
            used = column.width();
        }
    }
    if rows.is_empty() || used + 3 + tail > budget {
        rows.push(Vec::new());
    }
    rows
}

/// A probe's [`heaviest`](Probe::heaviest) ids as quoted tokens, wrapped to
/// [`ENTRY_TOKEN_CHARS`] per line, with whatever the cap dropped named as a
/// count. Empty for a probe that carries no list.
fn token_lines(graph: &PathGraph, probe: &Probe) -> Vec<String> {
    if probe.heaviest.is_empty() {
        return Vec::new();
    }
    let last = probe.heaviest.len() - 1;
    let mut items: Vec<String> = probe
        .heaviest
        .iter()
        .enumerate()
        .map(|(index, &id)| {
            let token = token_span(graph, id, id);
            if index == last { token } else { format!("{token},") }
        })
        .collect();
    let hidden = probe.set.len() - probe.heaviest.len();
    if hidden > 0 {
        items.push(format!("… and {hidden} more"));
    }

    let mut lines: Vec<String> = Vec::new();
    for item in items {
        let fits = lines
            .last()
            .is_some_and(|line| line.chars().count() + 1 + item.chars().count() <= ENTRY_TOKEN_CHARS);
        if fits {
            let line = lines.last_mut().expect("fits implies a last line");
            line.push(' ');
            line.push_str(&item);
        } else {
            lines.push(item);
        }
    }
    lines
}

/// Width of a card title character at `.probe-title`'s size.
const TITLE_GLYPH_W: f64 = 6.4;

/// A card wide enough for its own title. Tokens are never cut to fit a card, so
/// it is the card that gives: a 16-byte token of escaped bytes is 64 characters
/// and needs every one of them.
fn card_w(base: f64, title: &str) -> f64 {
    base.max(title.chars().count() as f64 * TITLE_GLYPH_W + 26.0)
}

/// The `s` in "2 tokens".
fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Whether a step is in the cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CutMark {
    /// Not in the cut.
    No,
    /// In the cut, so the cover probes for its set.
    Probed,
}

impl CutMark {
    fn of(edge: usize, selected: &HashSet<usize>) -> Self {
        if selected.contains(&edge) {
            Self::Probed
        } else {
            Self::No
        }
    }

    /// The badge to stamp on the card, if any.
    fn badge(self) -> Option<(&'static str, f64, &'static str)> {
        match self {
            Self::No => None,
            Self::Probed => Some(("CUT", 30.0, "cut-badge-bg")),
        }
    }
}

/// A pill badge hung off the top-right corner of `rect`.
fn corner_badge(rect: Box2d, label: &str, width: f64, class: &str) -> String {
    format!(
        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{width:.1}\" height=\"15\" rx=\"7.5\" class=\"{class}\"/><text x=\"{:.1}\" y=\"{:.1}\" class=\"cut-badge\">{label}</text>",
        rect.right() - width - 5.0,
        rect.y - 7.5,
        rect.right() - width / 2.0 - 5.0,
        rect.y + 3.0
    )
}

/// The `CUT` badge, hung off the top-right corner of `rect`.
fn cut_badge(rect: Box2d, cut: CutMark) -> String {
    match cut.badge() {
        Some((label, width, class)) => corner_badge(rect, label, width, class),
        None => String::new(),
    }
}




/// `text` broken into lines of at most `max_chars`, on word boundaries.
///
/// The figure's width comes from the graph, not the prose, so a caption long
/// enough to leave the canvas is a real possibility on a short needle.
fn wrap(text: &str, max_chars: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= max_chars => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

fn render_probe(
    graph: &PathGraph,
    edge_id: usize,
    rect: Box2d,
    cut: CutMark,
    reachable: bool,
) -> String {
    let edge = &graph.edges[edge_id];
    let probe = edge.probe.as_ref().expect("a drawn probe card has a probe");
    let contained = matches!(edge.kind, EdgeKind::Contained { .. });
    let class = match (cut, contained) {
        // The whole-needle step is on a path of its own, so a cut always takes it
        // — drawn in its own colour rather than as one more orange card.
        (_, true) => "probe contained",
        (CutMark::Probed, _) => "probe selected",
        (CutMark::No, _) if reachable => "probe",
        (CutMark::No, _) => "probe downstream",
    };
    let (title, detail) = match edge.kind {
        EdgeKind::Step { .. } => (
            token_preview(probe),
            format!("p{}→p{}", edge.from, edge.to),
        ),
        EdgeKind::Terminal => (
            terminal_title(graph, edge_id),
            format!(
                "{} token{} · p{}→match",
                probe.set.len(),
                plural(probe.set.len()),
                edge.from
            ),
        ),
        // The title names the tokens and the chip counts them, so all this line
        // has to carry is the one thing that makes the step different.
        EdgeKind::Contained { .. } => (
            terminal_title(graph, edge_id),
            "no boundary → match".to_string(),
        ),
        EdgeKind::Escape => (
            terminal_title(graph, edge_id),
            format!("no token starts p{} → match", edge.from),
        ),
        EdgeKind::Entry { .. } => (probe.label.clone(), probe.detail.clone()),
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
) -> String {
    let edge = entry.entry.map(|id| &graph.edges[id]);
    let cut = entry
        .entry
        .map_or(CutMark::No, |id| CutMark::of(id, selected));
    let class = match cut {
        CutMark::Probed => "alignment selected",
        CutMark::No => "alignment",
    };
    let (detail, frequency) = match edge {
        // k = 0 has no entry step: nothing precedes its first token.
        None => ("starts at a token boundary".to_string(), None),
        Some(edge) => match (&edge.probe, edge.kind) {
            (Some(probe), _) => (probe.detail.clone(), Some(probe_frequency(graph, probe))),
            // The set was never materialized, so the step is entered free and the
            // cut pays for this lane further along its chain.
            (None, EdgeKind::Entry { qualifying, .. }) => (
                format!("unprobed entry · {qualifying} tokens > cap {}", crate::graph::SET_CAP),
                None,
            ),
            (None, _) => ("unprobed entry".to_string(), None),
        },
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
        let lines = edge
            .and_then(|edge| edge.probe.as_ref())
            .map(|probe| token_lines(graph, probe))
            .unwrap_or_default();
        for (index, line) in lines.iter().enumerate() {
            out.push_str(&format!(
                "<text x=\"{:.1}\" y=\"{:.1}\" class=\"alignment-tokens\">{}</text>",
                rect.x + 13.0,
                rect.y + 78.0 + index as f64 * TOKEN_LINE_H,
                xml(line)
            ));
        }
    }
    out.push_str(&cut_badge(rect, cut));
    out.push_str("</g>");
    out
}

fn render_state(
    offset: usize,
    rect: Box2d,
    user_count: usize,
    incoming: usize,
    reachable: bool,
) -> String {
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

/// The caption lines under the chips: only a number the drawing cannot walk back.
///
/// Everything the badges and colours already say is left to them, so most figures
/// get no caption at all. What survives is the reading that would otherwise be
/// wrong rather than merely unexplained — an empty cover looks like a broken
/// figure and is an answer.
///
/// Each is wrapped to `max_chars`, so the returned count is what the header has to
/// make room for.
fn captions(summary: &RenderSummary, max_chars: usize) -> Vec<String> {
    let mut paragraphs: Vec<String> = Vec::new();
    if summary.cover_probes == 0 {
        paragraphs.push(
            "The cover is empty: no step of the graph reaches the accept position, so no row can \
             match and no scan is needed."
                .to_string(),
        );
    } else if summary.cut_value == 0 {
        paragraphs.push(
            "The cut weighs nothing: every step it takes is for a token this column never uses, \
             so the scan compares codes it will not find."
                .to_string(),
        );
    }
    paragraphs
        .iter()
        .flat_map(|paragraph| wrap(paragraph, max_chars))
        .collect()
}

/// Render `graph` with `selected_cut` highlighted, as one self-contained SVG.
///
/// # Panics
/// If `graph` is not a DAG the builder could have produced — the layout asserts
/// its structural invariants rather than drawing something misleading.
pub fn render_svg(graph: &PathGraph, selected_cut: &[usize], summary: &RenderSummary) -> String {
    // Width comes from the graph, so it is known before anything is placed — which
    // is what lets the captions wrap to it and the header size itself to the result.
    // A step's card sits between the two states it joins, so the columns have to
    // stand further apart than the widest card is wide — otherwise an edge into
    // the next state passes behind it.
    let widest_card = graph
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::Step { .. }))
        .filter_map(|edge| edge.probe.as_ref())
        .map(|probe| card_w(PROBE_W, &token_preview(probe)))
        .fold(PROBE_W, f64::max);
    let state_x = state_positions(graph, widest_card + STATE_W + CARD_GUTTER);
    let last_state_x = state_x.values().next_back().copied().unwrap_or(GRAPH_LEFT);
    let accept_x = last_state_x + 235.0;
    let width = (accept_x + 95.0).max(1_150.0);
    // A caption glyph is about 5.9 units wide at 10.5px in the font stack below.
    let captions = captions(summary, (((width - 60.0) / 5.9) as usize).max(40));
    // One caption line is baked into the base; a figure that needs none gets it back.
    let header_h = HEADER_BASE_H + (captions.len() as f64 - 1.0) * CAPTION_LINE_H;

    let adjacent = adjacency(graph);
    // Entry cards name their first-token set, so the lane pitch follows the
    // tallest one rather than a constant that a long list would overflow.
    let entry_lines = graph
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::Entry { .. }))
        .filter_map(|edge| edge.probe.as_ref())
        .map(|probe| token_lines(graph, probe).len())
        .max()
        .unwrap_or(0);
    let entry_h = ENTRY_BASE_H + entry_lines as f64 * TOKEN_LINE_H;
    let lane_gap = LANE_GAP.max(entry_h + ENTRY_LANE_MARGIN);
    let entries = alignments(graph, header_h, lane_gap);
    let users = state_users(graph, &adjacent, &entries);
    let incoming = incoming_counts(graph);
    let states = state_boxes(graph, &state_x, &users, &entries);
    let main_bottom = header_h + LANE_TOP + 32.0 + entries.len().max(1) as f64 * lane_gap;
    let points = point_layouts(graph, &state_x, &states, &entries, header_h, main_bottom);
    let selected: HashSet<usize> = selected_cut.iter().copied().collect();
    let reachable = reachable_without_cut(graph, &selected);

    let terminal_y = main_bottom + 34.0;
    let terminal_boxes = terminal_boxes(graph, &state_x, terminal_y, None);
    let terminal_bottom = terminal_boxes
        .values()
        .map(|rect| rect.bottom())
        .fold(terminal_y, f64::max);
    let rail_y = terminal_bottom + 52.0;
    // Predicate panel, between the accept rail and the footer caption: the terms
    // with the rows each is estimated to admit. The measured block sits below it,
    // on the same columns, so estimate and outcome read against each other.
    let columns = predicate_columns(graph, &summary.cover_terms, summary.column_rows);
    let estimate_total = predicate_estimate_total(summary);
    let measured_total = predicate_measured_total(summary);
    let budget = (((width - 92.0) / PREDICATE_GLYPH_W) as usize).max(40);
    let rows = predicate_rows(
        &columns,
        budget,
        estimate_total
            .chars()
            .count()
            .max(measured_total.chars().count()),
    );
    let predicate_top = rail_y + 52.0;
    let predicate_h = 26.0 + rows.len() as f64 * PREDICATE_ROW_H;
    let measured_top = predicate_top + predicate_h + 26.0;
    let measured_h = 26.0 + rows.len() as f64 * PREDICATE_MEASURED_ROW_H;
    let height = measured_top + measured_h + 62.0;
    let accept_box = Box2d {
        x: accept_x,
        y: rail_y - 17.0,
        w: 76.0,
        h: 34.0,
    };
    let entry_boxes: HashMap<usize, Box2d> = entries
        .iter()
        .enumerate()
        .map(|(lane, entry)| {
            (
                lane,
                Box2d {
                    x: ENTRY_X,
                    y: entry.y - entry_h / 2.0,
                    w: ENTRY_W,
                    h: entry_h,
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
  .alignment-tokens {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:9px; fill:#52666d; text-anchor:start; }}
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
  .probe.contained rect:first-of-type {{ fill:#f2f7f6; stroke:#176f73; stroke-width:2.2; }}
  .probe.contained .probe-title {{ fill:#0b5c58; }}
  .cut-badge-bg {{ fill:#d55e00 !important; stroke:none !important; }}
  .cut-badge-bg.pruned {{ fill:#8d989c !important; }}
  .cut-badge-bg.mandatory {{ fill:#176f73 !important; }}
  .cut-badge {{ font-size:8px; font-weight:800; fill:#ffffff; text-anchor:middle; }}
  .terminal-label {{ font-size:9.5px; font-weight:700; fill:#718388; letter-spacing:0.7px; }}
  .accept rect {{ fill:#20363c; stroke:#20363c; }}
  .accept text {{ font-size:10px; font-weight:750; fill:#ffffff; text-anchor:middle; }}
  .accept-rail {{ stroke:#9eafb2; stroke-width:1.4; }}
  .caption {{ font-size:10.5px; fill:#52666d; }}
  .predicate rect {{ fill:#f5f8f8; stroke:#dce5e6; stroke-width:1; }}
  .predicate-lead {{ font-size:9.5px; fill:#718388; }}
  .predicate-term {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:11px; font-weight:650; fill:#20363c; }}
  .predicate-share {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:10px; fill:#718388; font-variant-numeric:tabular-nums; }}
  .predicate-total {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:10px; font-weight:650; fill:#45666d; font-variant-numeric:tabular-nums; }}
  .measured rect {{ fill:#ffffff; stroke:#c6d5d7; stroke-width:1; }}
  .measured-share {{ font-family:"SFMono-Regular",Consolas,monospace; font-size:10px; font-weight:650; fill:#20363c; font-variant-numeric:tabular-nums; }}
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
            154.0,
            "WHOLE-NEEDLE TOKENS",
            if summary.contained_tokens == 0 {
                "none".to_string()
            } else {
                summary.contained_tokens.to_string()
            },
        ),
        (118.0, "CUT WEIGHT", compact_u64(summary.cut_value)),
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
    for (line, text) in captions.iter().enumerate() {
        svg.push_str(&format!(
            "<text x=\"28\" y=\"{:.1}\" class=\"caption\">{}</text>",
            CAPTION_TOP + line as f64 * CAPTION_LINE_H,
            xml(text)
        ));
    }
    let label_y = header_h - 4.0;
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{label_y:.1}\" class=\"section-label\">ALIGNMENT ENTRY</text><text x=\"{:.1}\" y=\"{label_y:.1}\" class=\"section-label\">TOKENIZED NEEDLE DAG  →</text>",
        ENTRY_X,
        GRAPH_LEFT - STATE_W / 2.0
    ));

    // Draw the steps beneath everything they connect.
    for (lane, entry) in entries.iter().enumerate() {
        let a = entry_boxes[&lane];
        let b = states[&entry.offset];
        let cut_here = entry.entry.is_some_and(|id| selected.contains(&id));
        let tone = if cut_here || !reachable.contains(&entry.offset) {
            EdgeTone::Muted
        } else {
            EdgeTone::Active
        };
        svg.push_str(&edge(&alignment_entry_edge(a, b), tone, true));
    }
    for edge_data in &graph.edges {
        match edge_data.kind {
            EdgeKind::Entry { .. } => {}
            EdgeKind::Step { .. } => {
                let (offset, next_offset) = (edge_data.from, edge_data.to);
                let source = states[&offset];
                let probe = points[&edge_data.id];
                let destination = states[&next_offset];
                // What this step has to get past: the states it flies over, and
                // every card but its own. Its own is where it is going.
                // Cards are obstacles too, all but this step's own — that one is
                // what the leader connects to. The two halves exclude different
                // states: each keeps only the one it actually starts or ends on,
                // since the other is something to get past.
                let cards: Vec<Box2d> = points
                    .iter()
                    .filter(|&(&id, _)| id != edge_data.id)
                    .map(|(_, layout)| layout.card)
                    .chain(terminal_boxes.values().copied())
                    .collect();
                let states_but = |keep: usize| -> Vec<Box2d> {
                    states
                        .iter()
                        .filter(|&(&at, _)| at != keep)
                        .map(|(_, rect)| *rect)
                        .collect()
                };
                let (over_in, over_out) = (states_but(offset), states_but(next_offset));
                let tone = edge_tone(edge_data, &selected, &reachable);
                let card_above = probe.card.cy() < probe.anchor_y;
                svg.push_str(&edge(
                    &edge_to_point(source, probe.anchor_x, probe.anchor_y, &over_in, &cards, card_above),
                    tone,
                    false,
                ));
                svg.push_str(&edge(
                    &edge_from_point(
                        probe.anchor_x,
                        probe.anchor_y,
                        destination,
                        &over_out,
                        &cards,
                        card_above,
                    ),
                    tone,
                    true,
                ));
                svg.push_str(&callout_leader(probe, tone.class()));
            }
            EdgeKind::Terminal | EdgeKind::Contained { .. } | EdgeKind::Escape => {
                let source = states[&edge_data.from];
                let terminal = terminal_boxes[&edge_data.id];
                let tone = edge_tone(edge_data, &selected, &reachable);
                svg.push_str(&edge(&vertical_edge(source, terminal), tone, true));
                // Past the accepting card, the drop onto the rail is only live if
                // nothing upstream stopped the lane.
                let out = if selected.contains(&edge_data.id)
                    || !reachable.contains(&edge_data.from)
                {
                    EdgeTone::Muted
                } else {
                    EdgeTone::Active
                };
                let path = format!(
                    "M {:.1} {:.1} V {rail_y:.1}",
                    terminal.cx(),
                    terminal.bottom()
                );
                svg.push_str(&edge(&path, out, false));
            }
        }
    }
    let rail_left = state_x.values().next().copied().unwrap_or(GRAPH_LEFT) - 82.0;
    svg.push_str(&format!(
        "<line x1=\"{:.1}\" y1=\"{rail_y:.1}\" x2=\"{:.1}\" y2=\"{rail_y:.1}\" class=\"accept-rail\"/>",
        rail_left,
        accept_box.x
    ));

    for (lane, entry) in entries.iter().enumerate() {
        svg.push_str(&render_alignment(
            graph,
            entry,
            entry_boxes[&lane],
            &selected,
        ));
    }
    // In offset order, not the state map's: the SVG has to be byte-identical from
    // one run to the next for the golden figure to mean anything.
    for &offset in state_x.keys() {
        svg.push_str(&render_state(
            offset,
            states[&offset],
            users[&offset].len(),
            incoming[offset],
            reachable.contains(&offset),
        ));
    }
    for edge_data in &graph.edges {
        if matches!(edge_data.kind, EdgeKind::Step { .. }) {
            let mark = CutMark::of(edge_data.id, &selected);
            let live = reachable.contains(&edge_data.from);
            svg.push_str(&render_probe_anchor(points[&edge_data.id], mark, live));
            svg.push_str(&render_probe(
                graph,
                edge_data.id,
                points[&edge_data.id].card,
                mark,
                live,
            ));
        }
    }
    if !terminal_boxes.is_empty() {
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" class=\"terminal-label\">ACCEPTING STEPS</text>",
            GRAPH_LEFT - STATE_W / 2.0,
            terminal_y - 15.0
        ));
    }
    for edge_data in &graph.edges {
        // Every step that accepts gets its card: a terminal range, the
        // whole-needle step, or an escape.
        if edge_data.to == graph.end() {
            svg.push_str(&render_probe(
                graph,
                edge_data.id,
                terminal_boxes[&edge_data.id],
                CutMark::of(edge_data.id, &selected),
                reachable.contains(&edge_data.from),
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
        "<text x=\"{ENTRY_X:.1}\" y=\"{:.1}\" class=\"terminal-label\">SCAN PREDICATE · {} comparison{}</text>",
        predicate_top - 10.0,
        summary.cover_probes,
        if summary.cover_probes == 1 { "" } else { "s" }
    ));
    svg.push_str(&format!(
        "<g class=\"predicate\"><rect x=\"{ENTRY_X:.1}\" y=\"{predicate_top:.1}\" width=\"{:.1}\" height=\"{predicate_h:.1}\" rx=\"8\"/>",
        width - ENTRY_X * 2.0
    ));
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"predicate-lead\">a row is a candidate if any of its codes is · under each term, the rows its frequency says it should admit</text>",
        ENTRY_X + 16.0,
        predicate_top + 17.0
    ));
    let left = ENTRY_X + 16.0;
    // Column x positions, kept so the measured block can sit under the same ones.
    let mut column_x: Vec<f64> = vec![0.0; columns.len()];
    for (row_index, row) in rows.iter().enumerate() {
        let term_y = predicate_top + 36.0 + row_index as f64 * PREDICATE_ROW_H;
        let share_y = term_y + 13.0;
        let mut x = left;
        for (position, &column_index) in row.iter().enumerate() {
            let column = &columns[column_index];
            column_x[column_index] = x;
            svg.push_str(&format!(
                "<text x=\"{x:.1}\" y=\"{term_y:.1}\" class=\"predicate-term\">{}</text>",
                xml(&column.term)
            ));
            svg.push_str(&format!(
                "<text x=\"{x:.1}\" y=\"{share_y:.1}\" class=\"predicate-share\">{}</text>",
                xml(&column.estimate)
            ));
            let span = column.width() as f64 * PREDICATE_GLYPH_W;
            if position + 1 < row.len() || row_index + 1 < rows.len() {
                // The terms are a disjunction; their estimates are an addition.
                // One glyph for both would be wrong for whichever it was not.
                svg.push_str(&format!(
                    "<text x=\"{:.1}\" y=\"{term_y:.1}\" class=\"predicate-term\">∨</text><text x=\"{:.1}\" y=\"{share_y:.1}\" class=\"predicate-share\">+</text>",
                    x + span + PREDICATE_GLYPH_W,
                    x + span + PREDICATE_GLYPH_W
                ));
            }
            x += span + 3.0 * PREDICATE_GLYPH_W;
        }
        if row_index + 1 == rows.len() {
            svg.push_str(&format!(
                "<text x=\"{x:.1}\" y=\"{share_y:.1}\" class=\"predicate-total\">{}</text>",
                xml(&estimate_total)
            ));
        }
    }
    svg.push_str("</g>");

    // Measured block: the same columns, counted rather than estimated.
    svg.push_str(&format!(
        "<text x=\"{ENTRY_X:.1}\" y=\"{:.1}\" class=\"terminal-label\">MEASURED OVER THE COLUMN</text>",
        measured_top - 10.0
    ));
    svg.push_str(&format!(
        "<g class=\"measured\"><rect x=\"{ENTRY_X:.1}\" y=\"{measured_top:.1}\" width=\"{:.1}\" height=\"{measured_h:.1}\" rx=\"8\"/>",
        width - ENTRY_X * 2.0
    ));
    svg.push_str(&format!(
        "<text x=\"{:.1}\" y=\"{:.1}\" class=\"predicate-lead\">rows that really hold each term</text>",
        left,
        measured_top + 17.0
    ));
    for (row_index, row) in rows.iter().enumerate() {
        let y = measured_top + 34.0 + row_index as f64 * PREDICATE_MEASURED_ROW_H;
        for &column_index in row {
            svg.push_str(&format!(
                "<text x=\"{:.1}\" y=\"{y:.1}\" class=\"measured-share\">{}</text>",
                column_x[column_index],
                xml(&columns[column_index].measured)
            ));
        }
        if row_index + 1 == rows.len() {
            let last = row.last().copied().unwrap_or(0);
            let x = column_x.get(last).copied().unwrap_or(left)
                + columns
                    .get(last)
                    .map_or(0.0, |column| column.width() as f64 * PREDICATE_GLYPH_W)
                + 3.0 * PREDICATE_GLYPH_W;
            svg.push_str(&format!(
                "<text x=\"{x:.1}\" y=\"{y:.1}\" class=\"predicate-total\">{}</text>",
                xml(&measured_total)
            ));
        }
    }
    svg.push_str("</g>");
    let row_frequency_note = if graph.total_rows == 0 {
        ""
    } else {
        " DF sums per-token row frequencies, counted under a df metric."
    };
    svg.push_str(&format!(
        "<text x=\"28\" y=\"{:.1}\" class=\"caption\">p denotes a needle byte offset. TF counts token occurrences; RF is TF / rows, the share of rows a probe is priced as admitting — an upper bound, since a row can hold a token twice.{row_frequency_note} The measured panel counts those rows instead of estimating them.</text>",
        height - 22.0
    ));
    svg.push_str("</svg>\n");
    svg
}
