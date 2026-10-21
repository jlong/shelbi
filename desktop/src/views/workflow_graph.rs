//! The Workflow Builder's graph, toolkit-free (`dt-wf-editor-builder`):
//! where each status node sits, how each edge runs between nodes, where its
//! label goes, what is under the pointer, and the canvas's pan and zoom.
//!
//! Everything here is in **graph coordinates**: the Workflow graph frame's
//! own points at 100%, its top-left at the origin (the frames' `Workflow
//! graph`, 776x760 for Task). The view scales them by the zoom and places
//! the frame in the canvas ([`Viewport`]).
//!
//! **Layout** follows the Workflow Builder frame (2305:10965): the statuses
//! still in flight (backlog, ready and active categories) run down the left
//! column; the rest go right. Archived statuses stack from
//! the top of the right column, handoff and done statuses from the row of
//! the last left status (Review beside In Progress, Done under it). A row
//! whose node has a transition straight down to the next row's gets the
//! taller gap (Review to Done, room for its label). Positions are always
//! worked out; nothing stores them.
//!
//! **Order down a column** (`dt-builder-catalog-order`): by category first
//! (Backlog, Ready, Active on the left; Archived, Handoff, Done on the
//! right), then by the project's catalog (`statuses.yaml`) within a
//! category. The workflow's own `statuses:` list doesn't count, so every
//! workflow draws its statuses the same way up (Review above Done even where
//! a workflow lists `done` first) and reordering the catalog in Settings
//! re-lays every workflow. [`layout`] ranks the categories, keeping the
//! order it is given within one; [`Graph::of`] gives it the catalog's.
//!
//! **Edges** are orthogonal polylines between node sides. A straight run
//! when the nodes share a row or column and nothing is in the way, else the
//! first of an L (vertical first, then horizontal first) or a Z (through the
//! gap between the columns, then between the rows) that crosses no other
//! node, else a bracket around the side. Status order edges join the
//! statuses that aren't terminal, each to the next in the columns' order
//! (the left column's, then the right's handoffs), where no transition
//! already does. A transition's label sits over its longest segment: above a
//! horizontal one on a line of its own (`Push branch · Open PR`), beside a
//! vertical one an action per line.

use shelbi_core::{ProjectStatus, StatusCategory, Transition, Workflow};

use super::workflow_editor_state::action_label;

/// A status node (the Workflow Graph Node component).
pub const NODE_W: f32 = 216.0;
pub const NODE_H: f32 = 88.0;
/// The graph frame's room around its nodes: the hint above, the legend
/// below, labels to the right of the right column's edges.
const PAD_LEFT: f32 = 48.0;
const PAD_TOP: f32 = 64.0;
const PAD_RIGHT: f32 = 128.0;
const PAD_BOTTOM: f32 = 48.0;
/// Between the columns, and between rows (the taller one under a labeled
/// vertical transition).
const COLUMN_GAP: f32 = 168.0;
const ROW_GAP: f32 = 88.0;
const LABELED_ROW_GAP: f32 = 120.0;
/// How far a bracket route stands off the nodes it goes around.
const BRACKET: f32 = 32.0;
/// Two transitions between the same statuses, one each way, run this far
/// apart; routes sharing a lane, at most this far.
const PARALLEL: f32 = 6.0;
const LANE: f32 = 10.0;
/// A label's box: 12/16 text in 4pt of padding, 4pt above a horizontal
/// segment or 12pt right of a vertical one.
pub const LABEL_LINE_H: f32 = 16.0;
pub const LABEL_PAD: f32 = 4.0;
const LABEL_ABOVE: f32 = 4.0;
const LABEL_BESIDE: f32 = 12.0;
/// X-Small's average advance, for a label's extent (hit-testing, the
/// frame's size); the view lays the text out itself.
const LABEL_CHAR_W: f32 = 6.2;
/// A connection handle: a 10pt circle centered on a node side's middle
/// (14pt under the pointer), hit within 12pt (on screen, at any zoom) of
/// its center: a 24pt target, Apple's minimum for a pointer.
pub const HANDLE_D: f32 = 10.0;
pub const HANDLE_HOT_D: f32 = 14.0;
pub const HANDLE_HIT: f32 = 12.0;
/// A hovered node stays hovered this far (on screen) past its edges and its
/// handles' hit areas, so moving out onto a handle never drops it.
const HOVER_SLACK: f32 = 6.0;
/// An edge is hit within 5pt (on screen) of its line.
const EDGE_HIT: f32 = 5.0;

/// The zoom picker's levels, and the zoom's range.
pub const ZOOMS: [f32; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
pub const ZOOM_MIN: f32 = 0.25;
pub const ZOOM_MAX: f32 = 2.0;
/// How much of the graph frame panning keeps inside the canvas.
const KEEP_VISIBLE: f32 = 96.0;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub const fn new(x: f32, y: f32) -> Pt {
        Pt { x, y }
    }

    fn dist(self, o: Pt) -> f32 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }

    pub fn center(&self) -> Pt {
        Pt::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }

    /// This rect, `by` bigger on every side.
    fn grown(&self, by: f32) -> Rect {
        Rect::new(self.x - by, self.y - by, self.w + 2.0 * by, self.h + 2.0 * by)
    }

    /// The middle of `side`: where its handle sits and its edges meet it.
    pub fn side(&self, side: Side) -> Pt {
        let c = self.center();
        match side {
            Side::Top => Pt::new(c.x, self.y),
            Side::Right => Pt::new(self.right(), c.y),
            Side::Bottom => Pt::new(c.x, self.bottom()),
            Side::Left => Pt::new(self.x, c.y),
        }
    }

    /// Whether the axis-aligned segment `a`–`b` passes through the inside
    /// (touching an edge doesn't count).
    fn crossed_by(&self, a: Pt, b: Pt) -> bool {
        let inside = |v: f32, lo: f32, hi: f32| v > lo + 0.5 && v < hi - 0.5;
        if (a.y - b.y).abs() < 0.01 {
            inside(a.y, self.y, self.bottom()) && a.x.max(b.x) > self.x + 0.5 && a.x.min(b.x) < self.right() - 0.5
        } else {
            inside(a.x, self.x, self.right()) && a.y.max(b.y) > self.y + 0.5 && a.y.min(b.y) < self.bottom() - 0.5
        }
    }
}

/// A node's side: where a handle sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    pub const ALL: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

    pub fn name(self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Right => "right",
            Side::Bottom => "bottom",
            Side::Left => "left",
        }
    }
}

/// A status as the layout needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSpec {
    pub id: String,
    pub category: StatusCategory,
}

/// A transition as the layout needs it: its ends and its actions' names.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeSpec {
    pub from: String,
    pub to: String,
    pub actions: Vec<String>,
}

impl EdgeSpec {
    pub fn of(t: &Transition) -> EdgeSpec {
        EdgeSpec {
            from: t.from.clone(),
            to: t.to.clone(),
            actions: t.actions.iter().map(|&a| action_label(a).to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    pub rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// Consecutive statuses, dashed.
    Order,
    /// The workflow's transition at this index, solid.
    Transition(usize),
}

/// Where a label sits against its segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LabelAt {
    /// Over a horizontal segment: centered on `x`, its box's bottom at
    /// `bottom`.
    Above { x: f32, bottom: f32 },
    /// Right of a vertical segment: its box's left at `left`, centered on
    /// `y`.
    Beside { left: f32, y: f32 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub lines: Vec<String>,
    pub at: LabelAt,
}

impl Label {
    /// Over the longest segment of `points`: the actions on one line above
    /// a horizontal one, a line each beside a vertical one.
    pub fn along(points: &[Pt], actions: &[String]) -> Option<Label> {
        if actions.is_empty() || points.len() < 2 {
            return None;
        }
        let (a, b) = points
            .windows(2)
            .map(|w| (w[0], w[1]))
            .max_by(|(a, b), (c, d)| a.dist(*b).total_cmp(&c.dist(*d)))?;
        Some(if (a.y - b.y).abs() < 0.01 {
            Label {
                lines: vec![actions.join(" · ")],
                at: LabelAt::Above {
                    x: (a.x + b.x) / 2.0,
                    bottom: a.y - LABEL_ABOVE,
                },
            }
        } else {
            Label {
                lines: actions.to_vec(),
                at: LabelAt::Beside {
                    left: a.x + LABEL_BESIDE,
                    y: (a.y + b.y) / 2.0,
                },
            }
        })
    }

    /// One line of text, `text`, over a horizontal run (the drawing's
    /// `Connect to Done`).
    pub fn line(points: &[Pt], text: &str) -> Option<Label> {
        let mut label = Label::along(points, &[text.to_string()])?;
        if let LabelAt::Beside { .. } = label.at {
            // A vertical run: put it above the line's end instead.
            let end = *points.last()?;
            label.at = LabelAt::Above {
                x: end.x,
                bottom: end.y - LABEL_ABOVE,
            };
        }
        Some(label)
    }

    /// The label's box, its width estimated from its text.
    pub fn rect(&self) -> Rect {
        let w = self.lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32 * LABEL_CHAR_W + 2.0 * LABEL_PAD;
        let h = self.lines.len() as f32 * LABEL_LINE_H + 2.0 * LABEL_PAD;
        match self.at {
            LabelAt::Above { x, bottom } => Rect::new(x - w / 2.0, bottom - h, w, h),
            LabelAt::Beside { left, y } => Rect::new(left, y - h / 2.0, w, h),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
    pub points: Vec<Pt>,
    pub label: Option<Label>,
}

impl Edge {
    /// The distance from `p` to the line.
    fn distance(&self, p: Pt) -> f32 {
        self.points
            .windows(2)
            .map(|w| segment_distance(p, w[0], w[1]))
            .fold(f32::INFINITY, f32::min)
    }
}

/// The laid-out graph.
#[derive(Debug, Clone, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The graph frame's size.
    pub w: f32,
    pub h: f32,
}

impl Graph {
    /// `workflow`'s graph, each category's statuses in `catalog`'s order
    /// (the project's `statuses.yaml`). A status the catalog has lost goes
    /// after the rest of its category, in workflow order.
    pub fn of(workflow: &Workflow, catalog: &[ProjectStatus]) -> Graph {
        let mut nodes: Vec<NodeSpec> = workflow
            .statuses
            .iter()
            .map(|s| NodeSpec {
                id: s.id.clone(),
                category: s.category,
            })
            .collect();
        nodes.sort_by_key(|n| catalog.iter().position(|s| s.id == n.id).unwrap_or(usize::MAX));
        let edges: Vec<EdgeSpec> = workflow.transitions.as_deref().unwrap_or(&[]).iter().map(EdgeSpec::of).collect();
        layout(&nodes, &edges)
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn rect(&self, id: &str) -> Option<Rect> {
        self.node(id).map(|n| n.rect)
    }

    /// Transition `index`'s edge.
    #[cfg(test)]
    pub fn transition(&self, index: usize) -> Option<&Edge> {
        self.edges.iter().find(|e| e.kind == EdgeKind::Transition(index))
    }

    /// The node under `p`.
    pub fn node_at(&self, p: Pt) -> Option<&Node> {
        self.nodes.iter().find(|n| n.rect.contains(p))
    }

    /// The node to hover with the pointer at `p`, `hovered` being hovered
    /// now, at `zoom`: the node under `p`, else `hovered` while `p` is near
    /// it or its handles' hit areas. A handle's dot is half outside its
    /// node, so the node's rect alone would drop the handles as the pointer
    /// moved onto one.
    pub fn hover_at(&self, p: Pt, hovered: Option<&str>, zoom: f32) -> Option<&Node> {
        if let Some(n) = self.node_at(p) {
            return Some(n);
        }
        let n = self.node(hovered?)?;
        let reach = (HANDLE_HIT + HOVER_SLACK) / zoom;
        let near = n.rect.grown(HOVER_SLACK / zoom).contains(p) || Side::ALL.iter().any(|&s| n.rect.side(s).dist(p) <= reach);
        near.then_some(n)
    }

    /// Node `id`'s handle under `p` (handles show only on the hovered
    /// node, so only its are hit), the nearest if the hit areas overlap
    /// (a small node at a low zoom), at `zoom`.
    pub fn handle_at(&self, id: &str, p: Pt, zoom: f32) -> Option<Side> {
        let rect = self.rect(id)?;
        let reach = HANDLE_HIT / zoom;
        Side::ALL
            .into_iter()
            .map(|s| (s, rect.side(s).dist(p)))
            .filter(|(_, d)| *d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(s, _)| s)
    }

    /// The transition whose line or label is under `p`, at `zoom`.
    pub fn transition_at(&self, p: Pt, zoom: f32) -> Option<usize> {
        let reach = EDGE_HIT / zoom;
        let index = |e: &Edge| match e.kind {
            EdgeKind::Transition(i) => Some(i),
            EdgeKind::Order => None,
        };
        // Labels first (they sit over other lines), then the nearest line.
        if let Some(i) = self
            .edges
            .iter()
            .filter(|e| e.label.as_ref().is_some_and(|l| l.rect().contains(p)))
            .find_map(index)
        {
            return Some(i);
        }
        self.edges
            .iter()
            .filter_map(|e| Some((index(e)?, e.distance(p))))
            .filter(|(_, d)| *d <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The route a transition from `from` to `to` would take (the drawing's
    /// line once it's over a node, and a new transition's).
    pub fn route_between(&self, from: &str, to: &str) -> Option<Vec<Pt>> {
        let (s, t) = (self.rect(from)?, self.rect(to)?);
        let obstacles: Vec<Rect> = self.nodes.iter().filter(|n| n.id != from && n.id != to).map(|n| n.rect).collect();
        Some(route(s, t, &obstacles))
    }

    /// The nearest node from `id` toward `side` (arrow keys), by distance
    /// along that way plus twice the distance across it.
    pub fn neighbor(&self, id: &str, side: Side) -> Option<&Node> {
        let from = self.rect(id)?.center();
        self.nodes
            .iter()
            .filter(|n| n.id != id)
            .filter_map(|n| {
                let c = n.rect.center();
                let (along, across) = match side {
                    Side::Top => (from.y - c.y, (c.x - from.x).abs()),
                    Side::Bottom => (c.y - from.y, (c.x - from.x).abs()),
                    Side::Left => (from.x - c.x, (c.y - from.y).abs()),
                    Side::Right => (c.x - from.x, (c.y - from.y).abs()),
                };
                (along > 1.0).then_some((n, along + 2.0 * across))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n)
    }
}

/// A category's place down its column: in flight on the left, then the
/// right's archived, handoff and done statuses.
fn rank(category: StatusCategory) -> usize {
    match category {
        StatusCategory::Backlog => 0,
        StatusCategory::Ready => 1,
        StatusCategory::Active => 2,
        StatusCategory::Archived => 3,
        StatusCategory::Handoff => 4,
        StatusCategory::Done => 5,
    }
}

/// Lay `nodes` and `edges` out (see the module docs): by category, then in
/// `nodes`' order within one.
pub fn layout(nodes: &[NodeSpec], edges: &[EdgeSpec]) -> Graph {
    let mut nodes: Vec<&NodeSpec> = nodes.iter().collect();
    nodes.sort_by_key(|n| rank(n.category));
    let in_flight = |c: StatusCategory| matches!(c, StatusCategory::Backlog | StatusCategory::Ready | StatusCategory::Active);
    let left: Vec<&NodeSpec> = nodes.iter().copied().filter(|n| in_flight(n.category)).collect();
    let archived: Vec<&NodeSpec> = nodes.iter().copied().filter(|n| n.category == StatusCategory::Archived).collect();
    let after: Vec<&NodeSpec> = nodes
        .iter()
        .copied()
        .filter(|n| matches!(n.category, StatusCategory::Handoff | StatusCategory::Done))
        .collect();
    let first_after = left.len().saturating_sub(1).max(archived.len());

    // (column, row) per node, in the order given.
    let mut cells: Vec<(&NodeSpec, usize, usize)> = Vec::new();
    cells.extend(left.iter().enumerate().map(|(r, n)| (*n, 0, r)));
    cells.extend(archived.iter().enumerate().map(|(r, n)| (*n, 1, r)));
    cells.extend(after.iter().enumerate().map(|(r, n)| (*n, 1, first_after + r)));
    // With nothing in flight, the right column is the only one: draw it
    // on the left.
    if left.is_empty() {
        for cell in &mut cells {
            cell.1 = 0;
        }
    }
    let rows = cells.iter().map(|c| c.2 + 1).max().unwrap_or(0);
    let at = |column: usize, row: usize| cells.iter().find(|c| c.1 == column && c.2 == row).map(|c| c.0.id.as_str());
    let linked = |a: &str, b: &str| edges.iter().any(|e| (e.from == a && e.to == b) || (e.from == b && e.to == a));
    let mut row_y = Vec::with_capacity(rows);
    let mut y = PAD_TOP;
    for row in 0..rows {
        row_y.push(y);
        // The taller gap under a row with a transition straight down.
        let labeled = [0, 1].into_iter().any(|column| {
            matches!((at(column, row), at(column, row + 1)), (Some(a), Some(b)) if edges.iter().any(|e| ((e.from == a && e.to == b) || (e.from == b && e.to == a)) && !e.actions.is_empty()))
        });
        y += NODE_H + if labeled { LABELED_ROW_GAP } else { ROW_GAP };
    }
    let positioned: Vec<Node> = nodes
        .iter()
        .map(|spec| {
            let (_, column, row) = cells.iter().find(|c| c.0.id == spec.id).copied().expect("every node has a cell");
            Node {
                id: spec.id.clone(),
                rect: Rect::new(PAD_LEFT + column as f32 * (NODE_W + COLUMN_GAP), row_y[row], NODE_W, NODE_H),
            }
        })
        .collect();

    let rect = |id: &str| positioned.iter().find(|n| n.id == id).map(|n| n.rect);
    let obstacles = |a: &str, b: &str| -> Vec<Rect> {
        positioned.iter().filter(|n| n.id != a && n.id != b).map(|n| n.rect).collect()
    };
    let mut out = Vec::new();
    // Status order: each status that isn't terminal to the next, no
    // transition between them already. (The archived ones sort between the
    // left column and the handoffs; skipping them joins the two.)
    let terminal = |c: StatusCategory| matches!(c, StatusCategory::Done | StatusCategory::Archived);
    let flow: Vec<&NodeSpec> = nodes.iter().copied().filter(|n| !terminal(n.category)).collect();
    for pair in flow.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if linked(&a.id, &b.id) {
            continue;
        }
        let (Some(s), Some(t)) = (rect(&a.id), rect(&b.id)) else {
            continue;
        };
        out.push(Edge {
            kind: EdgeKind::Order,
            from: a.id.clone(),
            to: b.id.clone(),
            points: route(s, t, &obstacles(&a.id, &b.id)),
            label: None,
        });
    }
    for (i, e) in edges.iter().enumerate() {
        let (Some(s), Some(t)) = (rect(&e.from), rect(&e.to)) else {
            continue;
        };
        let mut points = route(s, t, &obstacles(&e.from, &e.to));
        // One each way between the same two statuses: run them apart.
        if e.from != e.to && edges.iter().any(|o| o.from == e.to && o.to == e.from) {
            let order = |id: &str| nodes.iter().position(|n| n.id == id).unwrap_or(0);
            let d = if order(&e.from) < order(&e.to) { -PARALLEL } else { PARALLEL };
            points = offset(&points, d);
        }
        out.push(Edge {
            kind: EdgeKind::Transition(i),
            from: e.from.clone(),
            to: e.to.clone(),
            points,
            label: None,
        });
    }
    let rects: Vec<Rect> = positioned.iter().map(|n| n.rect).collect();
    spread_lanes(&mut out, &rects);
    for e in &mut out {
        if let EdgeKind::Transition(i) = e.kind {
            e.label = Label::along(&e.points, &edges[i].actions);
        }
    }

    // The frame: the nodes in their padding, and room for every label and
    // bracket.
    let mut w: f32 = 0.0;
    let mut h: f32 = 0.0;
    for n in &positioned {
        w = w.max(n.rect.right() + PAD_RIGHT);
        h = h.max(n.rect.bottom() + PAD_BOTTOM);
    }
    for e in &out {
        for p in &e.points {
            w = w.max(p.x + PAD_LEFT);
            h = h.max(p.y + PAD_BOTTOM);
        }
        if let Some(l) = &e.label {
            let r = l.rect();
            w = w.max(r.right() + 24.0);
            h = h.max(r.bottom() + PAD_BOTTOM);
        }
    }
    Graph {
        nodes: positioned,
        edges: out,
        w: w.max(PAD_LEFT + NODE_W + PAD_RIGHT),
        h: h.max(PAD_TOP + PAD_BOTTOM),
    }
}

/// An orthogonal route from `s` to `t` that crosses none of `obstacles`
/// where one can.
pub fn route(s: Rect, t: Rect, obstacles: &[Rect]) -> Vec<Pt> {
    if s == t {
        // A status to itself: a loop off its right side.
        let (r, c) = (s.right(), s.center());
        return vec![
            Pt::new(r, c.y - 16.0),
            Pt::new(r + BRACKET, c.y - 16.0),
            Pt::new(r + BRACKET, c.y + 16.0),
            Pt::new(r, c.y + 16.0),
        ];
    }
    let clear = |points: &[Pt]| points.windows(2).all(|w| obstacles.iter().all(|o| !o.crossed_by(w[0], w[1])));
    let (sc, tc) = (s.center(), t.center());
    let mut candidates: Vec<Vec<Pt>> = Vec::new();
    let same_row = (sc.y - tc.y).abs() < 0.5;
    let same_column = (sc.x - tc.x).abs() < 0.5;
    let (right, down) = (tc.x > sc.x, tc.y > sc.y);
    if same_row {
        let (a, b) = if right { (s.side(Side::Right), t.side(Side::Left)) } else { (s.side(Side::Left), t.side(Side::Right)) };
        candidates.push(vec![a, b]);
        let below = s.bottom().max(t.bottom()) + BRACKET;
        candidates.push(vec![s.side(Side::Bottom), Pt::new(sc.x, below), Pt::new(tc.x, below), t.side(Side::Bottom)]);
        let above = s.y.min(t.y) - BRACKET;
        candidates.push(vec![s.side(Side::Top), Pt::new(sc.x, above), Pt::new(tc.x, above), t.side(Side::Top)]);
    } else if same_column {
        let (a, b) = if down { (s.side(Side::Bottom), t.side(Side::Top)) } else { (s.side(Side::Top), t.side(Side::Bottom)) };
        candidates.push(vec![a, b]);
        let out = s.right().max(t.right()) + BRACKET;
        candidates.push(vec![s.side(Side::Right), Pt::new(out, sc.y), Pt::new(out, tc.y), t.side(Side::Right)]);
        let out = s.x.min(t.x) - BRACKET;
        candidates.push(vec![s.side(Side::Left), Pt::new(out, sc.y), Pt::new(out, tc.y), t.side(Side::Left)]);
    } else {
        let (s_v, t_h) = (
            if down { s.side(Side::Bottom) } else { s.side(Side::Top) },
            if right { t.side(Side::Left) } else { t.side(Side::Right) },
        );
        let (s_h, t_v) = (
            if right { s.side(Side::Right) } else { s.side(Side::Left) },
            if down { t.side(Side::Top) } else { t.side(Side::Bottom) },
        );
        // L, vertical first: unless the corner lands in the target.
        if !(sc.x > t.x && sc.x < t.right()) {
            candidates.push(vec![s_v, Pt::new(sc.x, tc.y), t_h]);
        }
        // L, horizontal first.
        if !(tc.y > s.y && tc.y < s.bottom()) {
            candidates.push(vec![s_h, Pt::new(tc.x, sc.y), t_v]);
        }
        // Z through the gap between the columns.
        let gap_x = if right { (s.right() < t.x).then(|| (s.right() + t.x) / 2.0) } else { (t.right() < s.x).then(|| (t.right() + s.x) / 2.0) };
        if let Some(mx) = gap_x {
            candidates.push(vec![s_h, Pt::new(mx, sc.y), Pt::new(mx, tc.y), t_h]);
        }
        // Z through the gap between the rows.
        let gap_y = if down { (s.bottom() < t.y).then(|| (s.bottom() + t.y) / 2.0) } else { (t.bottom() < s.y).then(|| (t.bottom() + s.y) / 2.0) };
        if let Some(my) = gap_y {
            candidates.push(vec![s_v, Pt::new(sc.x, my), Pt::new(tc.x, my), t_v]);
        }
    }
    let fallback = candidates.first().cloned().unwrap_or_else(|| vec![sc, tc]);
    candidates.into_iter().find(|c| clear(c)).unwrap_or(fallback)
}

/// Routes whose middle run shares a lane (a Z through the same gap, a
/// bracket on the same side) are spread apart so each stays its own line:
/// across the gap around its middle, or outward from a bracket's side.
fn spread_lanes(edges: &mut [Edge], nodes: &[Rect]) {
    for vertical in [true, false] {
        // The middle run's position (x of a vertical run, y of a
        // horizontal one), in half points, and the edges on it.
        let mut lanes: Vec<(i32, Vec<usize>)> = Vec::new();
        for (i, e) in edges.iter().enumerate() {
            let p = &e.points;
            if p.len() != 4 {
                continue;
            }
            let (along, across) = if vertical { (p[1].x - p[2].x, p[1].y - p[2].y) } else { (p[1].y - p[2].y, p[1].x - p[2].x) };
            if along.abs() > 0.01 || across.abs() < 0.01 {
                continue;
            }
            let key = ((if vertical { p[1].x } else { p[1].y }) * 2.0).round() as i32;
            match lanes.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(i),
                None => lanes.push((key, vec![i])),
            }
        }
        for (key, members) in lanes {
            let n = members.len();
            if n < 2 {
                continue;
            }
            let at = key as f32 / 2.0;
            let span = |r: &Rect| if vertical { (r.x, r.right()) } else { (r.y, r.bottom()) };
            // The free run around the lane: between the nearest nodes on
            // either side, if there are both.
            let before = nodes.iter().map(|r| span(r).1).filter(|&e| e <= at).fold(f32::NEG_INFINITY, f32::max);
            let after = nodes.iter().map(|r| span(r).0).filter(|&e| e >= at).fold(f32::INFINITY, f32::min);
            let step = |room: f32| LANE.min(room / n as f32);
            let offsets: Vec<f32> = if before.is_finite() && after.is_finite() {
                let s = step(after - before - 2.0 * LANE);
                (0..n).map(|k| (k as f32 - (n - 1) as f32 / 2.0) * s).collect()
            } else if before.is_finite() {
                (0..n).map(|k| k as f32 * LANE).collect()
            } else {
                (0..n).map(|k| -(k as f32) * LANE).collect()
            };
            for (&i, d) in members.iter().zip(offsets) {
                let p = &mut edges[i].points;
                if vertical {
                    p[1].x += d;
                    p[2].x += d;
                } else {
                    p[1].y += d;
                    p[2].y += d;
                }
            }
        }
    }
}

/// `points` moved `d` off their line (across every segment), the ends kept
/// on their nodes' sides.
fn offset(points: &[Pt], d: f32) -> Vec<Pt> {
    let mut out: Vec<Pt> = points.iter().map(|p| Pt::new(p.x + d, p.y + d)).collect();
    let n = out.len();
    if n >= 2 {
        let horizontal = |a: Pt, b: Pt| (a.y - b.y).abs() < 0.01;
        if horizontal(points[0], points[1]) {
            out[0].x = points[0].x;
        } else {
            out[0].y = points[0].y;
        }
        if horizontal(points[n - 2], points[n - 1]) {
            out[n - 1].x = points[n - 1].x;
        } else {
            out[n - 1].y = points[n - 1].y;
        }
    }
    out
}

/// A drawing line's route from a handle to the pointer: along the handle's
/// axis first, then across.
pub fn route_to_pointer(from: Pt, side: Side, to: Pt) -> Vec<Pt> {
    let corner = match side {
        Side::Top | Side::Bottom => Pt::new(from.x, to.y),
        Side::Left | Side::Right => Pt::new(to.x, from.y),
    };
    vec![from, corner, to]
}

fn segment_distance(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.dist(Pt::new(a.x + t * dx, a.y + t * dy))
}

/// The canvas's pan and zoom over the graph frame. At zero pan the frame is
/// centered across the canvas and flush with its top, as the frames draw it
/// at 100%.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub zoom: f32,
    pub pan: Pt,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            zoom: 1.0,
            pan: Pt::default(),
        }
    }
}

/// A size: the canvas's, the graph frame's.
pub type Size = (f32, f32);

impl Viewport {
    /// The graph frame's top-left in the canvas.
    pub fn origin(&self, canvas: Size, graph: Size) -> Pt {
        Pt::new((canvas.0 - graph.0 * self.zoom) / 2.0 + self.pan.x, self.pan.y)
    }

    /// Canvas point `p` in graph coordinates.
    pub fn to_graph(self, p: Pt, canvas: Size, graph: Size) -> Pt {
        let o = self.origin(canvas, graph);
        Pt::new((p.x - o.x) / self.zoom, (p.y - o.y) / self.zoom)
    }

    /// Graph point `p` in the canvas.
    #[cfg(test)]
    pub fn to_canvas(self, p: Pt, canvas: Size, graph: Size) -> Pt {
        let o = self.origin(canvas, graph);
        Pt::new(o.x + p.x * self.zoom, o.y + p.y * self.zoom)
    }

    /// Zoom to `zoom` (clamped) keeping the graph point under canvas point
    /// `at` where it is.
    pub fn zoom_at(&mut self, zoom: f32, at: Pt, canvas: Size, graph: Size) {
        let under = self.to_graph(at, canvas, graph);
        self.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.pan.x = at.x - under.x * self.zoom - (canvas.0 - graph.0 * self.zoom) / 2.0;
        self.pan.y = at.y - under.y * self.zoom;
        self.clamp(canvas, graph);
    }

    /// Zoom to `zoom` about the canvas's center (the zoom picker).
    pub fn zoom_to(&mut self, zoom: f32, canvas: Size, graph: Size) {
        self.zoom_at(zoom, Pt::new(canvas.0 / 2.0, canvas.1 / 2.0), canvas, graph);
    }

    /// Fit: the whole frame in the canvas, centered.
    pub fn fit(&mut self, canvas: Size, graph: Size) {
        if graph.0 <= 0.0 || graph.1 <= 0.0 || canvas.0 <= 0.0 || canvas.1 <= 0.0 {
            return;
        }
        self.zoom = (canvas.0 / graph.0).min(canvas.1 / graph.1).clamp(ZOOM_MIN, ZOOM_MAX);
        self.pan = Pt::new(0.0, ((canvas.1 - graph.1 * self.zoom) / 2.0).max(0.0));
    }

    /// Pan by `(dx, dy)` canvas points.
    pub fn pan_by(&mut self, dx: f32, dy: f32, canvas: Size, graph: Size) {
        self.pan.x += dx;
        self.pan.y += dy;
        self.clamp(canvas, graph);
    }

    /// Keep some of the frame in the canvas, so it can't be panned away.
    fn clamp(&mut self, canvas: Size, graph: Size) {
        let (gw, gh) = (graph.0 * self.zoom, graph.1 * self.zoom);
        let keep_x = KEEP_VISIBLE.min(gw);
        let keep_y = KEEP_VISIBLE.min(gh);
        let center = (canvas.0 - gw) / 2.0;
        // The frame's left edge stays in [keep - gw, canvas - keep].
        let left = (center + self.pan.x).clamp(keep_x - gw, canvas.0 - keep_x);
        self.pan.x = left - center;
        self.pan.y = self.pan.y.clamp(keep_y - gh, canvas.1 - keep_y);
    }

    /// The zoom picker's label: `100%`.
    pub fn label(&self) -> String {
        format!("{}%", (self.zoom * 100.0).round() as i32)
    }
}

/// A 20-status, 40-transition workflow, `big` (the canvas's frame-rate
/// check: the tests, `--visual-figma`'s `state-workflow-builder-big`, and
/// `--bench-builder`). Its statuses go after the Workflows fixture's.
#[cfg(any(test, feature = "visual-check"))]
pub(crate) mod big_fixture {
    use shelbi_core::StatusCategory::{self, *};

    pub const CATEGORIES: [StatusCategory; 20] = [
        Backlog, Ready, Active, Active, Active, Active, Active, Active, Active, Active, Active, Active, Handoff, Handoff,
        Handoff, Handoff, Done, Done, Archived, Archived,
    ];

    pub fn id(i: usize) -> String {
        format!("stage-{i}")
    }

    /// Each status's two transitions: to the next, and seven on.
    pub fn transitions() -> Vec<(usize, usize, &'static [&'static str])> {
        (0..20)
            .flat_map(|i| [(i, (i + 1) % 20, &["push_branch"][..]), (i, (i + 7) % 20, &["merge", "delete_branch"][..])])
            .collect()
    }

    /// The Workflows fixture's `statuses.yaml` with the twenty added.
    pub fn statuses() -> String {
        let mut out = crate::views::workflows_state::fixture::STATUSES.to_string();
        for (i, c) in CATEGORIES.iter().enumerate() {
            out.push_str(&format!("- id: {}\n  name: Stage {i}\n  category: {}\n", id(i), c.as_str()));
        }
        out
    }

    /// `big.yaml`.
    pub fn workflow() -> String {
        let mut out = String::from("name: big\ndescription: Twenty statuses, forty transitions.\nstatuses:\n");
        for i in 0..20 {
            out.push_str(&format!("- id: {}\n  owner: user\n", id(i)));
        }
        out.push_str("transitions:\n");
        for (from, to, actions) in transitions() {
            out.push_str(&format!("- from: {}\n  to: {}\n  actions: [{}]\n", id(from), id(to), actions.join(", ")));
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn spec(id: &str, category: StatusCategory) -> NodeSpec {
        NodeSpec { id: id.into(), category }
    }

    fn edge(from: &str, to: &str, actions: &[&str]) -> EdgeSpec {
        EdgeSpec {
            from: from.into(),
            to: to.into(),
            actions: actions.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// The fixture's Task, as the frames draw it.
    pub(crate) fn task() -> Graph {
        use StatusCategory::*;
        layout(
            &[
                spec("backlog", Backlog),
                spec("todo", Ready),
                spec("in-progress", Active),
                spec("review", Handoff),
                spec("done", Done),
                spec("canceled", Archived),
            ],
            &[
                edge("in-progress", "review", &["Push branch", "Open PR"]),
                edge("review", "done", &["Push branch", "Merge"]),
                edge("in-progress", "canceled", &["Close PR", "Delete branch"]),
                edge("review", "canceled", &["Close PR", "Delete branch"]),
            ],
        )
    }

    fn at(g: &Graph, id: &str) -> (f32, f32) {
        let r = g.rect(id).unwrap();
        (r.x, r.y)
    }

    #[test]
    fn the_task_workflow_lays_out_as_the_frame_draws_it() {
        let g = task();
        assert_eq!((g.w, g.h), (776.0, 760.0), "the Workflow graph frame");
        assert_eq!(at(&g, "backlog"), (48.0, 64.0));
        assert_eq!(at(&g, "todo"), (48.0, 240.0));
        assert_eq!(at(&g, "in-progress"), (48.0, 416.0));
        assert_eq!(at(&g, "review"), (432.0, 416.0), "beside the last status in flight");
        assert_eq!(at(&g, "done"), (432.0, 624.0), "under Review, the labeled gap");
        assert_eq!(at(&g, "canceled"), (432.0, 64.0), "archived at the top");
    }

    #[test]
    fn status_order_dashes_join_the_statuses_in_flight() {
        let g = task();
        let order: Vec<(&str, &str)> = g
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Order)
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect();
        // In Progress → Review has a transition; Review → Done ends in a
        // terminal status.
        assert_eq!(order, [("backlog", "todo"), ("todo", "in-progress")]);
        let e = &g.edges[0];
        assert_eq!(e.points, [Pt::new(156.0, 152.0), Pt::new(156.0, 240.0)], "the frame's 152..240 run");
    }

    /// The fixture's Task as loaded, and its project's catalog.
    fn loaded(tag: &str) -> (Workflow, Vec<ProjectStatus>) {
        use crate::views::workflows_state::tests::{Home, PROJECT};
        let _home = Home::new(tag);
        let config = shelbi_state::workflow_config::WorkflowConfig::load(PROJECT).unwrap();
        (config.workflow("task").unwrap().clone(), config.statuses().to_vec())
    }

    fn order_dashes(g: &Graph) -> Vec<(&str, &str)> {
        g.edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Order)
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    /// `catalog` rearranged into `ids`' order.
    fn reordered(catalog: &[ProjectStatus], ids: &[&str]) -> Vec<ProjectStatus> {
        ids.iter().map(|id| catalog.iter().find(|s| s.id == *id).unwrap().clone()).collect()
    }

    /// Task with a second handoff status, QA, listed last, and the catalog
    /// with it after Review.
    fn with_qa(tag: &str) -> (Workflow, Vec<ProjectStatus>) {
        let (mut workflow, mut catalog) = loaded(tag);
        let mut qa = workflow.status("review").unwrap().clone();
        qa.id = "qa".into();
        workflow.statuses.push(qa);
        let mut qa = catalog.iter().find(|s| s.id == "review").unwrap().clone();
        qa.id = "qa".into();
        catalog.insert(4, qa);
        (workflow, catalog)
    }

    #[test]
    fn the_workflows_own_list_order_moves_nothing() {
        let (workflow, catalog) = loaded("graph-list-order");
        assert_eq!(Graph::of(&workflow, &catalog), task());

        // Listed backwards, Done before Review: Review still above Done.
        let mut listed = workflow.clone();
        listed.statuses.reverse();
        let g = Graph::of(&listed, &catalog);
        assert_eq!(g, task());
        assert_eq!(at(&g, "review"), (432.0, 416.0));
        assert_eq!(at(&g, "done"), (432.0, 624.0));
        assert_eq!(order_dashes(&g), [("backlog", "todo"), ("todo", "in-progress")]);
    }

    #[test]
    fn categories_rank_before_the_catalogs_order() {
        // The catalog backwards: each column still runs Backlog, Ready,
        // Active, and Archived, Handoff, Done.
        let (workflow, catalog) = loaded("graph-category-rank");
        let backwards = reordered(&catalog, &["canceled", "done", "review", "in-progress", "todo", "backlog"]);
        assert_eq!(Graph::of(&workflow, &backwards), task());
    }

    #[test]
    fn the_catalog_orders_statuses_within_a_category() {
        let (workflow, catalog) = with_qa("graph-catalog-order");
        let g = Graph::of(&workflow, &catalog);
        assert_eq!(at(&g, "review"), (432.0, 416.0), "beside the last status in flight");
        assert_eq!(at(&g, "qa"), (432.0, 592.0), "the catalog's next, the plain gap");
        assert_eq!(at(&g, "done"), (432.0, 768.0), "done after every handoff");
        assert_eq!(at(&g, "canceled"), (432.0, 64.0), "archived at the top");
        // The dashes run the same way, past Canceled to the handoffs (In
        // Progress to Review is a transition).
        assert_eq!(order_dashes(&g), [("backlog", "todo"), ("todo", "in-progress"), ("review", "qa")]);

        // QA moved above Review in the catalog.
        let moved = reordered(&catalog, &["backlog", "todo", "in-progress", "qa", "review", "done", "canceled"]);
        let g = Graph::of(&workflow, &moved);
        assert_eq!(at(&g, "qa"), (432.0, 416.0));
        assert_eq!(at(&g, "review"), (432.0, 592.0));
        assert_eq!(at(&g, "done"), (432.0, 800.0), "under Review, the labeled gap");
        assert_eq!(order_dashes(&g), [("backlog", "todo"), ("todo", "in-progress"), ("in-progress", "qa"), ("qa", "review")]);

        // A status the catalog has lost goes after the rest of its category.
        let lost: Vec<ProjectStatus> = moved.into_iter().filter(|s| s.id != "qa").collect();
        let g = Graph::of(&workflow, &lost);
        assert_eq!(at(&g, "review"), (432.0, 416.0));
        assert_eq!(at(&g, "qa"), (432.0, 592.0));
    }

    #[test]
    fn transitions_route_around_other_nodes() {
        let g = task();
        let route = |i| g.transition(i).unwrap().points.clone();
        assert_eq!(route(0), [Pt::new(264.0, 460.0), Pt::new(432.0, 460.0)], "straight across");
        assert_eq!(route(1), [Pt::new(540.0, 504.0), Pt::new(540.0, 624.0)], "straight down");
        assert_eq!(
            route(2),
            [Pt::new(264.0, 460.0), Pt::new(348.0, 460.0), Pt::new(348.0, 108.0), Pt::new(432.0, 108.0)],
            "a Z through the column gap, as an L would cross To Do or Review"
        );
        assert_eq!(route(3), [Pt::new(540.0, 416.0), Pt::new(540.0, 152.0)]);
        // The frame's new In Progress → Done: an L, down then across.
        assert_eq!(
            g.route_between("in-progress", "done").unwrap(),
            [Pt::new(156.0, 504.0), Pt::new(156.0, 668.0), Pt::new(432.0, 668.0)]
        );
    }

    #[test]
    fn labels_sit_over_the_longest_segment() {
        let g = task();
        let label = |i| g.transition(i).unwrap().label.clone().unwrap();
        let across = label(0);
        assert_eq!(across.lines, ["Push branch · Open PR"]);
        assert_eq!(across.at, LabelAt::Above { x: 348.0, bottom: 456.0 }, "the frame's box ends at 456");
        let down = label(1);
        assert_eq!(down.lines, ["Push branch", "Merge"]);
        assert_eq!(down.at, LabelAt::Beside { left: 552.0, y: 564.0 }, "the frame's box starts at x 552");
        assert!(matches!(label(2).at, LabelAt::Beside { left: 360.0, .. }), "beside the Z's long run");
        // No actions, no label.
        let g = layout(&[spec("a", StatusCategory::Ready), spec("b", StatusCategory::Done)], &[edge("a", "b", &[])]);
        assert_eq!(g.transition(0).unwrap().label, None);
    }

    #[test]
    fn a_blocked_straight_run_brackets_around() {
        use StatusCategory::*;
        let g = layout(
            &[spec("a", Backlog), spec("b", Ready), spec("c", Active), spec("d", Done)],
            &[edge("a", "c", &["Merge"])],
        );
        let p = &g.transition(0).unwrap().points;
        assert_eq!(p.len(), 4, "around b: {p:?}");
        assert_eq!(p[0], g.rect("a").unwrap().side(Side::Right));
        assert_eq!(p[3], g.rect("c").unwrap().side(Side::Right));
        assert!(g.w >= p[1].x + 24.0, "the frame holds the bracket");
    }

    #[test]
    fn routes_through_the_same_gap_take_their_own_lanes() {
        let z = |y0: f32, y1: f32| Edge {
            kind: EdgeKind::Order,
            from: String::new(),
            to: String::new(),
            points: vec![Pt::new(264.0, y0), Pt::new(348.0, y0), Pt::new(348.0, y1), Pt::new(432.0, y1)],
            label: None,
        };
        let bracket = |y0: f32, y1: f32| Edge {
            points: vec![Pt::new(264.0, y0), Pt::new(296.0, y0), Pt::new(296.0, y1), Pt::new(264.0, y1)],
            ..z(0.0, 0.0)
        };
        let nodes = [Rect::new(48.0, 64.0, NODE_W, NODE_H), Rect::new(432.0, 64.0, NODE_W, NODE_H)];
        let mut edges = vec![z(460.0, 108.0), z(284.0, 108.0), z(636.0, 108.0)];
        spread_lanes(&mut edges, &nodes);
        let lanes: Vec<f32> = edges.iter().map(|e| e.points[1].x).collect();
        assert_eq!(lanes, [338.0, 348.0, 358.0], "spread around the gap's middle");
        assert!(edges.iter().all(|e| e.points[1].x == e.points[2].x), "still vertical");

        // Brackets off the right of a column spread outward.
        let only_left = [Rect::new(48.0, 64.0, NODE_W, NODE_H)];
        let mut edges = vec![bracket(108.0, 460.0), bracket(284.0, 636.0)];
        spread_lanes(&mut edges, &only_left);
        assert_eq!(edges.iter().map(|e| e.points[1].x).collect::<Vec<_>>(), [296.0, 306.0]);
    }

    #[test]
    fn transitions_each_way_run_apart() {
        use StatusCategory::*;
        let g = layout(&[spec("a", Active), spec("b", Handoff)], &[edge("a", "b", &[]), edge("b", "a", &[])]);
        let (there, back) = (&g.transition(0).unwrap().points, &g.transition(1).unwrap().points);
        assert_ne!(there[0].y, back[0].y);
        assert_eq!(there[0].x, g.rect("a").unwrap().right(), "still on the node's side");
        assert_eq!(back[0].x, g.rect("b").unwrap().x);
    }

    #[test]
    fn hit_testing_finds_nodes_handles_and_edges() {
        let g = task();
        assert_eq!(g.node_at(Pt::new(100.0, 100.0)).map(|n| n.id.as_str()), Some("backlog"));
        assert_eq!(g.node_at(Pt::new(300.0, 300.0)), None);
        // In Progress's bottom handle, within reach at 100% and not at 200%.
        assert_eq!(g.handle_at("in-progress", Pt::new(160.0, 508.0), 1.0), Some(Side::Bottom));
        assert_eq!(g.handle_at("in-progress", Pt::new(160.0, 514.0), 2.0), None, "reach is on screen");
        assert_eq!(g.handle_at("in-progress", Pt::new(160.0, 460.0), 1.0), None);
        // A line, and a label.
        assert_eq!(g.transition_at(Pt::new(348.0, 300.0), 1.0), Some(2));
        assert_eq!(g.transition_at(Pt::new(560.0, 560.0), 1.0), Some(1), "on its label");
        assert_eq!(g.transition_at(Pt::new(300.0, 300.0), 1.0), None);
        // Arrow keys.
        assert_eq!(g.neighbor("in-progress", Side::Right).map(|n| n.id.as_str()), Some("review"));
        assert_eq!(g.neighbor("in-progress", Side::Top).map(|n| n.id.as_str()), Some("todo"));
        assert_eq!(g.neighbor("backlog", Side::Top), None);
    }

    #[test]
    fn a_handle_is_a_24pt_target_at_every_zoom() {
        let g = task();
        // In Progress's bottom handle sits at (156, 504).
        for zoom in [0.25, 0.5, 1.0, 2.0] {
            // 11pt on screen out: inside; 13pt: outside. Graph units scale
            // by the zoom, the target on screen doesn't.
            let at = |pts: f32| Pt::new(156.0, 504.0 + pts / zoom);
            assert_eq!(g.handle_at("in-progress", at(11.0), zoom), Some(Side::Bottom), "{zoom}");
            assert_eq!(g.handle_at("in-progress", at(-8.0), zoom), Some(Side::Bottom), "{zoom}: its inner half");
            assert_eq!(g.handle_at("in-progress", at(13.0), zoom), None, "{zoom}");
        }
        // A small node at a low zoom: the nearest side's handle, not the
        // first one listed.
        let small = Graph {
            nodes: vec![Node {
                id: "a".into(),
                rect: Rect::new(0.0, 0.0, 40.0, 40.0),
            }],
            edges: vec![],
            w: 40.0,
            h: 40.0,
        };
        assert_eq!(small.handle_at("a", Pt::new(34.0, 14.0), 0.5), Some(Side::Right), "not Top");
        assert_eq!(small.handle_at("a", Pt::new(2.0, 26.0), 0.5), Some(Side::Left), "not Bottom");
    }

    #[test]
    fn a_hovered_node_stays_hovered_over_its_handles() {
        let g = task();
        let hover = |p: Pt, hovered: Option<&str>, zoom: f32| g.hover_at(p, hovered, zoom).map(|n| n.id.clone());
        // In Progress's right side's middle is (264, 460). The handle's
        // outer half lies past the node's edge: the node's rect alone loses
        // the hover there.
        let outer = Pt::new(268.0, 460.0);
        assert_eq!(g.node_at(outer), None, "outside the rect");
        for zoom in [0.5, 1.0, 2.0] {
            let past = |pts: f32| Pt::new(264.0 + pts / zoom, 460.0);
            let ip = Some("in-progress");
            for pts in [1.0, 5.0, 12.0, 17.0] {
                assert_eq!(hover(past(pts), ip, zoom).as_deref(), ip, "{pts}pt out at {zoom}");
            }
            assert_eq!(hover(past(30.0), ip, zoom), None, "well away at {zoom}");
            // Only a hovered node keeps it: arriving from outside doesn't.
            assert_eq!(hover(past(5.0), None, zoom), None);
        }
        // A little slack all round the node too, and the node under the
        // pointer always wins.
        assert_eq!(hover(Pt::new(100.0, 507.0), Some("in-progress"), 1.0).as_deref(), Some("in-progress"));
        assert_eq!(hover(Pt::new(100.0, 100.0), Some("in-progress"), 1.0).as_deref(), Some("backlog"));
        // There, the handle wins over the transition whose line starts
        // under it (In Progress → Review); the view presses handles first.
        assert_eq!(g.transition_at(outer, 1.0), Some(0));
        assert_eq!(g.handle_at("in-progress", outer, 1.0), Some(Side::Right));
    }

    #[test]
    fn the_viewport_centers_zooms_about_the_pointer_and_fits() {
        let (canvas, graph) = ((1390.0, 874.0), (776.0, 760.0));
        let mut v = Viewport::default();
        assert_eq!(v.origin(canvas, graph), Pt::new(307.0, 0.0), "centered, flush with the top");
        let p = Pt::new(500.0, 300.0);
        let under = v.to_graph(p, canvas, graph);
        v.zoom_at(1.5, p, canvas, graph);
        assert_eq!(v.label(), "150%");
        let after = v.to_canvas(under, canvas, graph);
        assert!((after.x - p.x).abs() < 0.01 && (after.y - p.y).abs() < 0.01, "the point stays put");
        v.zoom_at(9.0, p, canvas, graph);
        assert_eq!(v.zoom, ZOOM_MAX);
        v.fit(canvas, graph);
        assert!((v.zoom - 874.0 / 760.0).abs() < 0.001);
        let o = v.origin(canvas, graph);
        assert!((o.x - (1390.0 - 776.0 * v.zoom) / 2.0).abs() < 0.01 && o.y.abs() < 0.01);
        // Panning keeps some of the frame in view.
        v.pan_by(-10_000.0, 0.0, canvas, graph);
        let o = v.origin(canvas, graph);
        assert!(o.x + graph.0 * v.zoom >= KEEP_VISIBLE - 0.01);
    }

    #[test]
    fn twenty_statuses_and_forty_transitions_lay_out_fast() {
        let (nodes, edges) = big();
        let start = std::time::Instant::now();
        for _ in 0..10 {
            let g = layout(&nodes, &edges);
            assert_eq!(g.nodes.len(), 20);
        }
        let each = start.elapsed() / 10;
        // A frame is 16ms; the layout runs once per drawn frame.
        assert!(each < std::time::Duration::from_millis(4), "{each:?} a layout");
    }

    /// 20 statuses, 40 transitions ([`big_fixture`]).
    pub(crate) fn big() -> (Vec<NodeSpec>, Vec<EdgeSpec>) {
        let nodes: Vec<NodeSpec> = big_fixture::CATEGORIES.iter().enumerate().map(|(i, &c)| spec(&big_fixture::id(i), c)).collect();
        let edges = big_fixture::transitions()
            .into_iter()
            .map(|(from, to, actions)| edge(&big_fixture::id(from), &big_fixture::id(to), actions))
            .collect();
        (nodes, edges)
    }
}
