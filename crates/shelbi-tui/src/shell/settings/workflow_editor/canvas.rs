//! The Builder's graph in terminal cells (`tui-workflow-editor-builder`).
//!
//! **Where nodes go** is the shared graph's call
//! (`shelbi_app::settings::workflow_graph`): its columns (statuses in
//! flight on the left, the rest on the right) and rows (by category, then
//! the catalog's order) are read off its node positions, so both clients
//! put every status in the same place. Only the cell sizes are the TUI's: a
//! node is a 3-row box (its symbol and name in the top border, `Owner ·
//! agent` inside, `Initial` in the bottom border of the initial status),
//! the columns 14 cells apart, the rows 2.
//!
//! **Edges** are orthogonal lines routed cell by cell (an A* search over
//! the canvas): each leaves its source from a free port beside one of its
//! sides (the border there shows a tee) and ends in an arrowhead on a free
//! port of its destination. Ports are never shared, nodes are never
//! crossed, and a line costs more to turn, to cross another, and much more
//! to run along one, so two transitions between the same statuses (one
//! each way, or the same way twice), a status to itself and a cycle each
//! get their own line. Transitions are routed in the workflow's order, then
//! the status-order dashes; the same draft always draws the same way.
//! Labels (a transition's actions) sit over the line's longest run where
//! the cells are free, shortened to fit, and are left off where they can't.
//!
//! Positions are worked out from the draft on every change (the Builder
//! caches the result while nothing changes): nothing here is stored in the
//! workflow's YAML.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
};
use shelbi_app::settings::workflow_graph::{EdgeKind, Graph, Side};

use crate::panel;

pub(crate) const NODE_H: i32 = 3;
const MIN_NODE_W: i32 = 22;
const MAX_NODE_W: i32 = 30;
const COL_GAP: i32 = 14;
const ROW_GAP: i32 = 2;
const MARGIN_LEFT: i32 = 3;
const MARGIN_TOP: i32 = 2;
const MARGIN_RIGHT: i32 = 14;
const MARGIN_BOTTOM: i32 = 2;
/// A route's costs: a step, a turn, crossing another line, and running
/// along one (allowed only when nothing else gets through).
const TURN: u32 = 2;
const CROSS: u32 = 3;
const OVERLAP: u32 = 40;
/// A turn in a cell another line uses would make a junction glyph.
const CROWDED_TURN: u32 = 30;
/// A status to itself loops out at least this far.
const LOOP_MIN: u32 = 5;

/// A status as the canvas draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeIn {
    pub id: String,
    pub name: String,
    /// `Owner · agent`.
    pub summary: String,
    pub initial: bool,
    /// Its column and row in the shared layout.
    pub col: i32,
    pub row: i32,
}

/// A line to draw: a transition (with its label) or a status-order dash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EdgeIn {
    pub kind: EdgeKind,
    pub from: String,
    pub to: String,
    pub label: String,
}

/// Everything the canvas is worked out from: the Builder recomputes it only
/// when this changes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Input {
    pub nodes: Vec<NodeIn>,
    pub edges: Vec<EdgeIn>,
}

impl Input {
    /// The canvas's input from the shared `graph` (positions, which edges),
    /// `node` giving each status's name, summary and whether it's initial.
    pub(crate) fn of(graph: &Graph, node: impl Fn(&str) -> (String, String, bool), transition_label: impl Fn(usize) -> String) -> Input {
        let mut xs: Vec<i64> = graph.nodes.iter().map(|n| n.rect.x.round() as i64).collect();
        let mut ys: Vec<i64> = graph.nodes.iter().map(|n| n.rect.y.round() as i64).collect();
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        let nodes = graph
            .nodes
            .iter()
            .map(|n| {
                let (name, summary, initial) = node(&n.id);
                NodeIn {
                    id: n.id.clone(),
                    name,
                    summary,
                    initial,
                    col: xs.binary_search(&(n.rect.x.round() as i64)).unwrap_or(0) as i32,
                    row: ys.binary_search(&(n.rect.y.round() as i64)).unwrap_or(0) as i32,
                }
            })
            .collect();
        // Transitions first (in the workflow's order), so they get the
        // straightest lines; the dashes after.
        let mut edges: Vec<EdgeIn> = Vec::new();
        let mut transitions: Vec<_> = graph.edges.iter().filter_map(|e| match e.kind {
            EdgeKind::Transition(i) => Some((i, e)),
            EdgeKind::Order => None,
        }).collect();
        transitions.sort_by_key(|(i, _)| *i);
        for (i, e) in transitions {
            edges.push(EdgeIn { kind: e.kind, from: e.from.clone(), to: e.to.clone(), label: transition_label(i) });
        }
        for e in graph.edges.iter().filter(|e| e.kind == EdgeKind::Order) {
            edges.push(EdgeIn { kind: e.kind, from: e.from.clone(), to: e.to.clone(), label: String::new() });
        }
        Input { nodes, edges }
    }
}

/// A node's box on the canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeBox {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
}

impl NodeBox {
    pub(crate) fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + NODE_H
    }

    /// Where `side`'s connection handle shows (just outside the box).
    pub(crate) fn handle(&self, side: Side) -> (i32, i32) {
        let mid = self.x + self.w / 2;
        match side {
            Side::Top => (mid, self.y - 1),
            Side::Bottom => (mid, self.y + NODE_H),
            Side::Left => (self.x - 1, self.y + 1),
            Side::Right => (self.x + self.w, self.y + 1),
        }
    }
}

/// Directions on the grid, as bits in a cell's mask.
const UP: u8 = 1;
const RIGHT: u8 = 2;
const DOWN: u8 = 4;
const LEFT: u8 = 8;
const DIRS: [u8; 4] = [UP, RIGHT, DOWN, LEFT];

fn step(dir: u8) -> (i32, i32) {
    match dir {
        UP => (0, -1),
        RIGHT => (1, 0),
        DOWN => (0, 1),
        _ => (-1, 0),
    }
}

fn reverse(dir: u8) -> u8 {
    match dir {
        UP => DOWN,
        RIGHT => LEFT,
        DOWN => UP,
        _ => RIGHT,
    }
}

fn dir_index(dir: u8) -> usize {
    match dir {
        UP => 0,
        RIGHT => 1,
        DOWN => 2,
        _ => 3,
    }
}

/// A port: the cell beside a node side a line leaves from or arrives at,
/// and the way into the node from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Port {
    x: i32,
    y: i32,
    into: u8,
}

/// A routed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    /// Index into the input's edges.
    pub edge: usize,
    /// From the source's port to the destination's (where the arrowhead
    /// is).
    pub cells: Vec<(i32, i32)>,
    /// The way into the source from its port (its border shows a tee), and
    /// into the destination (the arrowhead's way).
    pub out_of: u8,
    pub into: u8,
}

/// A transition's label on the canvas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Label {
    pub edge: usize,
    pub x: i32,
    pub y: i32,
    pub text: String,
}

/// The laid-out canvas.
#[derive(Debug, Clone, Default)]
pub(crate) struct Canvas {
    pub input: Input,
    pub nodes: Vec<NodeBox>,
    pub routes: Vec<Route>,
    pub labels: Vec<Label>,
    pub w: i32,
    pub h: i32,
    /// What lines use each cell (a [`Grid`] index): the directions, and
    /// whether a line ends there.
    occupied: Vec<u8>,
}

/// A cell's use bits beyond the four directions.
const END: u8 = 16;
const TURNS: u8 = 32;

pub(crate) fn layout(input: Input) -> Canvas {
    let widest = input
        .nodes
        .iter()
        .map(|n| (panel::display_width(&n.name) as i32 + 6).max(panel::display_width(&n.summary) as i32 + 4))
        .max()
        .unwrap_or(0);
    let w = widest.clamp(MIN_NODE_W, MAX_NODE_W);
    let cols = input.nodes.iter().map(|n| n.col + 1).max().unwrap_or(0);
    let rows = input.nodes.iter().map(|n| n.row + 1).max().unwrap_or(0);
    let nodes: Vec<NodeBox> = input
        .nodes
        .iter()
        .map(|n| NodeBox {
            id: n.id.clone(),
            x: MARGIN_LEFT + n.col * (w + COL_GAP),
            y: MARGIN_TOP + n.row * (NODE_H + ROW_GAP),
            w,
        })
        .collect();
    let cw = MARGIN_LEFT + cols * w + (cols - 1).max(0) * COL_GAP + MARGIN_RIGHT;
    let ch = MARGIN_TOP + rows * NODE_H + (rows - 1).max(0) * ROW_GAP + MARGIN_BOTTOM;
    let mut canvas = Canvas {
        input,
        nodes,
        w: cw.max(1),
        h: ch.max(1),
        occupied: vec![0; (cw.max(1) * ch.max(1)) as usize],
        ..Canvas::default()
    };
    for i in 0..canvas.input.edges.len() {
        let (from, to) = (canvas.input.edges[i].from.clone(), canvas.input.edges[i].to.clone());
        if let Some(route) = canvas.route(&from, &to) {
            canvas.occupy(&route.cells, route.out_of);
            canvas.routes.push(Route { edge: i, ..route });
        }
    }
    canvas.place_labels();
    canvas
}

impl Canvas {
    pub(crate) fn node(&self, id: &str) -> Option<&NodeBox> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub(crate) fn node_at(&self, x: i32, y: i32) -> Option<&NodeBox> {
        self.nodes.iter().find(|n| n.contains(x, y))
    }

    /// The transition (its index in the workflow) whose line or label is at
    /// `(x, y)`; the dashes aren't selectable.
    pub(crate) fn transition_at(&self, x: i32, y: i32) -> Option<usize> {
        let transition = |edge: usize| match self.input.edges.get(edge)?.kind {
            EdgeKind::Transition(i) => Some(i),
            EdgeKind::Order => None,
        };
        let on_label = self
            .labels
            .iter()
            .find(|l| y == l.y && x >= l.x && x < l.x + panel::display_width(&l.text) as i32)
            .and_then(|l| transition(l.edge));
        on_label.or_else(|| self.routes.iter().filter(|r| r.cells.contains(&(x, y))).find_map(|r| transition(r.edge)))
    }

    /// The route drawn for transition `index`.
    pub(crate) fn transition_route(&self, index: usize) -> Option<&Route> {
        self.routes.iter().find(|r| self.input.edges.get(r.edge).is_some_and(|e| e.kind == EdgeKind::Transition(index)))
    }

    /// How many transitions the canvas couldn't find a line for.
    pub(crate) fn unrouted(&self) -> usize {
        let transitions = self.input.edges.iter().filter(|e| matches!(e.kind, EdgeKind::Transition(_))).count();
        transitions - self.routes.iter().filter(|r| matches!(self.input.edges[r.edge].kind, EdgeKind::Transition(_))).count()
    }

    /// A line from `from` to `to` among the ones drawn (a drawing aimed at
    /// a node, a new transition): it takes no port another line has.
    pub(crate) fn preview(&self, from: &str, to: &str) -> Option<Route> {
        self.route(from, to).map(|r| Route { edge: usize::MAX, ..r })
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        (x >= 0 && y >= 0 && x < self.w && y < self.h).then(|| (y * self.w + x) as usize)
    }

    fn blocked(&self, x: i32, y: i32) -> bool {
        self.nodes.iter().any(|n| n.contains(x, y))
    }

    /// `node`'s free ports, each side's in order along it.
    fn ports(&self, node: &NodeBox) -> Vec<Port> {
        let mut out = Vec::new();
        let (x, y, w) = (node.x, node.y, node.w);
        let (top, bottom) = self.border_text(node);
        for px in (x + 2).max(top)..=x + w - 3 {
            out.push(Port { x: px, y: y - 1, into: DOWN });
        }
        out.push(Port { x: x + w, y: y + 1, into: LEFT });
        for px in (x + 2).max(bottom)..=x + w - 3 {
            out.push(Port { x: px, y: y + NODE_H, into: UP });
        }
        out.push(Port { x: x - 1, y: y + 1, into: RIGHT });
        out.into_iter()
            .filter(|p| self.index(p.x, p.y).is_some_and(|i| self.occupied[i] == 0) && !self.blocked(p.x, p.y))
            .collect()
    }

    /// Where `node`'s top and bottom borders are free of text from: past
    /// its symbol and name, and past Initial.
    fn border_text(&self, node: &NodeBox) -> (i32, i32) {
        let spec = self.input.nodes.iter().find(|n| n.id == node.id);
        let name = spec.map_or(0, |n| panel::display_width(&name_in(&n.name, node.w)) as i32);
        let initial = spec.is_some_and(|n| n.initial);
        (node.x + 6 + name, if initial { node.x + 11 } else { node.x + 2 })
    }

    /// A route from `from` to `to` over the lines drawn so far.
    fn route(&self, from: &str, to: &str) -> Option<Route> {
        let (s, t) = (self.node(from)?, self.node(to)?);
        let starts = self.ports(s);
        let goals = self.ports(t);
        if starts.is_empty() || goals.is_empty() {
            return None;
        }
        let looped = from == to;
        let (tcx, tcy) = (t.x + t.w / 2, t.y + 1);
        let (scx, scy) = (s.x + s.w / 2, s.y + 1);
        let n = (self.w * self.h) as usize;
        let mut best = vec![u32::MAX; n * 4];
        let mut prev: Vec<u32> = vec![u32::MAX; n * 4];
        let mut heap = BinaryHeap::new();
        let mut seq = 0u32;
        let h = |x: i32, y: i32| goals.iter().map(|g| (g.x - x).unsigned_abs() + (g.y - y).unsigned_abs()).min().unwrap_or(0);
        for p in &starts {
            let out = reverse(p.into);
            // Prefer leaving from the side facing the destination.
            let (dx, dy) = step(out);
            let facing = (dx * (tcx - scx)).max(0) + (dy * (tcy - scy)).max(0);
            let bias = if looped || facing > 0 { 0 } else { 3 };
            let state = self.index(p.x, p.y).unwrap() * 4 + dir_index(out);
            if bias < best[state] {
                best[state] = bias;
                heap.push((Reverse(bias + h(p.x, p.y)), Reverse(seq), state, bias));
                seq += 1;
            }
        }
        let goal_at = |x: i32, y: i32| goals.iter().find(|g| g.x == x && g.y == y).copied();
        let mut found: Option<(usize, Port)> = None;
        while let Some((_, _, state, g)) = heap.pop() {
            if g > best[state] {
                continue;
            }
            let (cell, d) = (state / 4, DIRS[state % 4]);
            let (x, y) = ((cell as i32) % self.w, (cell as i32) / self.w);
            if prev[state] != u32::MAX || starts.iter().all(|p| (p.x, p.y) != (x, y)) {
                if let Some(port) = goal_at(x, y) {
                    if !looped || g >= LOOP_MIN {
                        found = Some((state, port));
                        break;
                    }
                }
            }
            for nd in DIRS {
                if nd == reverse(d) {
                    continue;
                }
                let (dx, dy) = step(nd);
                let (nx, ny) = (x + dx, y + dy);
                let Some(ni) = self.index(nx, ny) else { continue };
                if self.blocked(nx, ny) {
                    continue;
                }
                let used = self.occupied[ni];
                if used & END != 0 {
                    continue;
                }
                let mut cost = 1;
                if nd != d {
                    cost += TURN;
                    if self.occupied[cell] != 0 {
                        cost += CROWDED_TURN;
                    }
                }
                let along = if nd == UP || nd == DOWN { UP | DOWN } else { LEFT | RIGHT };
                if used & along != 0 {
                    cost += OVERLAP;
                } else if used != 0 {
                    cost += CROSS;
                }
                if used & TURNS != 0 {
                    cost += CROWDED_TURN;
                }
                let next = ni * 4 + dir_index(nd);
                let ng = g + cost;
                if ng < best[next] {
                    best[next] = ng;
                    prev[next] = state as u32;
                    heap.push((Reverse(ng + h(nx, ny)), Reverse(seq), next, ng));
                    seq += 1;
                }
            }
        }
        let (mut state, port) = found?;
        let mut cells = vec![];
        loop {
            let cell = state / 4;
            cells.push(((cell as i32) % self.w, (cell as i32) / self.w));
            match prev[state] {
                u32::MAX => break,
                p => state = p as usize,
            }
        }
        cells.reverse();
        let first = cells[0];
        let out_of = starts.iter().find(|p| (p.x, p.y) == first).map_or(DOWN, |p| p.into);
        Some(Route { edge: 0, cells, out_of, into: port.into })
    }

    /// Mark a routed line's cells used.
    fn occupy(&mut self, cells: &[(i32, i32)], out_of: u8) {
        for (k, &(x, y)) in cells.iter().enumerate() {
            let Some(i) = self.index(x, y) else { continue };
            let mask = link_mask(cells, k, out_of);
            let turns = mask != (UP | DOWN) && mask != (LEFT | RIGHT);
            self.occupied[i] |= mask | if turns { TURNS } else { 0 };
            if k == 0 || k + 1 == cells.len() {
                self.occupied[i] |= END;
            }
        }
    }

    fn free(&self, x: i32, y: i32, taken: &[Label]) -> bool {
        self.index(x, y).is_some_and(|i| self.occupied[i] == 0)
            && !self.blocked(x, y)
            && !taken.iter().any(|l| l.y == y && x >= l.x - 1 && x <= l.x + panel::display_width(&l.text) as i32)
    }

    /// Put each transition's label over its line's longest run that has
    /// room, shortened to fit (four characters at least).
    fn place_labels(&mut self) {
        let mut labels: Vec<Label> = Vec::new();
        for r in &self.routes {
            let text = &self.input.edges[r.edge].label;
            if text.is_empty() {
                continue;
            }
            let mut runs = runs(&r.cells);
            runs.sort_by_key(|run| Reverse(run.len));
            let placed = runs.iter().find_map(|run| {
                let candidates: Vec<(i32, i32, i32)> = if run.horizontal {
                    let room = run.len - 2;
                    let (x0, x1) = (run.x.min(run.x2), run.x.max(run.x2));
                    let mid = (x0 + x1) / 2;
                    vec![(mid, run.y - 1, room), (mid, run.y + 1, room)]
                } else {
                    let (y0, y1) = (run.y.min(run.y2), run.y.max(run.y2));
                    let mid = (y0 + y1) / 2;
                    vec![(run.x + 2, mid, 22), (run.x - 2, mid, -22)]
                };
                candidates.into_iter().find_map(|(x, y, room)| {
                    let max = room.unsigned_abs() as usize;
                    let fit = |len: usize| -> Option<Label> {
                        let t = panel::truncate_right(text, len);
                        let tw = panel::display_width(&t) as i32;
                        let lx = if run.horizontal {
                            x - tw / 2
                        } else if room < 0 {
                            x - tw + 1
                        } else {
                            x
                        };
                        // A cell clear of nodes either side, so it never reads as
                        // part of a box.
                        let clear = !self.blocked(lx - 1, y) && !self.blocked(lx + tw, y);
                        (clear && (lx..lx + tw).all(|cx| self.free(cx, y, &labels))).then_some(Label { edge: r.edge, x: lx, y, text: t })
                    };
                    let full = panel::display_width(text).min(max);
                    (4..=full).rev().find_map(fit)
                })
            });
            labels.extend(placed);
        }
        self.labels = labels;
    }

    /// Draw the canvas into `buf` over `area`, scrolled by `scroll`.
    pub(crate) fn paint(&self, buf: &mut Buffer, area: Rect, scroll: (i32, i32), look: &Look) {
        let put = |buf: &mut Buffer, x: i32, y: i32, text: &str, style: Style| {
            let (sx, sy) = (x - scroll.0, y - scroll.1);
            if sy < 0 || sy >= area.height as i32 {
                return;
            }
            let mut cx = sx;
            for ch in text.chars() {
                let w = panel::display_width(&ch.to_string()) as i32;
                if cx >= 0 && cx + w <= area.width as i32 {
                    if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + sy as u16)) {
                        cell.set_char(ch).set_style(style);
                    }
                }
                cx += w.max(1);
            }
        };
        // Lines: each cell's directions from every line through it, the
        // style of the most emphasized one.
        let n = (self.w * self.h) as usize;
        let mut mask = vec![0u8; n];
        let mut solid = vec![false; n];
        let mut rank = vec![0u8; n];
        let mut style = vec![look.line; n];
        let mut arrows: Vec<(i32, i32, u8, Style, u8)> = Vec::new();
        let mut tees: Vec<(i32, i32, u8, u8)> = Vec::new();
        let preview: Vec<(&Route, Style, u8)> = look.preview.iter().map(|(r, s)| (r, *s, 3)).collect();
        let routes = self.routes.iter().map(|r| {
            let (s, k) = look.edge(&self.input.edges[r.edge]);
            (r, s, k)
        });
        for (r, s, k) in routes.chain(preview) {
            let dashed = r.edge != usize::MAX && self.input.edges[r.edge].kind == EdgeKind::Order || r.edge == usize::MAX;
            for (i, &(x, y)) in r.cells.iter().enumerate() {
                let Some(c) = self.index(x, y) else { continue };
                mask[c] |= link_mask(&r.cells, i, r.out_of);
                solid[c] |= !dashed;
                if k >= rank[c] {
                    rank[c] = k;
                    style[c] = s;
                }
            }
            if let Some(&(x, y)) = r.cells.last() {
                arrows.push((x, y, r.into, s, k));
            }
            if let Some(&(x, y)) = r.cells.first() {
                tees.push((x, y, r.out_of, k));
            }
        }
        for y in scroll.1.max(0)..(scroll.1 + area.height as i32).min(self.h) {
            for x in scroll.0.max(0)..(scroll.0 + area.width as i32).min(self.w) {
                let c = (y * self.w + x) as usize;
                if mask[c] != 0 {
                    put(buf, x, y, glyph(mask[c], !solid[c]), style[c]);
                }
            }
        }
        arrows.sort_by_key(|a| a.4);
        for (x, y, into, s, _) in arrows {
            let a = match into {
                UP => "▲",
                RIGHT => "▶",
                DOWN => "▼",
                _ => "◀",
            };
            put(buf, x, y, a, s);
        }
        for l in &self.labels {
            let (s, _) = look.edge(&self.input.edges[l.edge]);
            let s = if s == look.line { look.label } else { s };
            put(buf, l.x, l.y, &l.text, s);
        }
        // Nodes over the lines.
        for (node, spec) in self.nodes.iter().zip(&self.input.nodes) {
            let deco = look.node(&node.id);
            let (h, v, tl, tr, bl, br) = match deco.border {
                Border::Plain => ("─", "│", "╭", "╮", "╰", "╯"),
                Border::Heavy => ("━", "┃", "┏", "┓", "┗", "┛"),
                Border::Double => ("═", "║", "╔", "╗", "╚", "╝"),
            };
            let bs = deco.border_style;
            let inner = (node.w - 2) as usize;
            // Top: the symbol and the name.
            let name = name_in(&spec.name, node.w);
            let nw = panel::display_width(&name) as i32;
            put(buf, node.x, node.y, tl, bs);
            put(buf, node.x + 1, node.y, &format!("{h} "), bs);
            put(buf, node.x + 3, node.y, deco.glyph, deco.glyph_style);
            put(buf, node.x + 4, node.y, &format!(" {name} "), deco.name_style);
            put(buf, node.x + 6 + nw, node.y, &h.repeat((node.w - 7 - nw).max(0) as usize), bs);
            put(buf, node.x + node.w - 1, node.y, tr, bs);
            // Middle: the assignment.
            put(buf, node.x, node.y + 1, v, bs);
            put(buf, node.x + 1, node.y + 1, &format!(" {}", panel::truncate_right(&spec.summary, inner - 1)), look.summary.patch(deco.fill));
            let summary_w = 1 + panel::display_width(&panel::truncate_right(&spec.summary, inner - 1));
            put(buf, node.x + 1 + summary_w as i32, node.y + 1, &" ".repeat(inner - summary_w), deco.fill);
            put(buf, node.x + node.w - 1, node.y + 1, v, bs);
            // Bottom: Initial on the initial status.
            put(buf, node.x, node.y + 2, bl, bs);
            if spec.initial {
                put(buf, node.x + 1, node.y + 2, &format!("{h} "), bs);
                put(buf, node.x + 3, node.y + 2, "Initial", look.initial);
                put(buf, node.x + 10, node.y + 2, &format!(" {}", h.repeat(inner.saturating_sub(10))), bs);
            } else {
                put(buf, node.x + 1, node.y + 2, &h.repeat(inner), bs);
            }
            put(buf, node.x + node.w - 1, node.y + 2, br, bs);
        }
        // Where lines leave a node: a tee in its border.
        tees.sort_by_key(|t| t.3);
        for (x, y, into, _) in tees {
            let (dx, dy) = step(into);
            let (bx, by) = (x + dx, y + dy);
            let Some(node) = self.node_at(bx, by) else { continue };
            let deco = look.node(&node.id);
            let tee = match (into, deco.border) {
                (DOWN, Border::Plain) => "┴",
                (UP, Border::Plain) => "┬",
                (LEFT, Border::Plain) => "├",
                (RIGHT, Border::Plain) => "┤",
                (DOWN, Border::Heavy) => "┸",
                (UP, Border::Heavy) => "┰",
                (LEFT, Border::Heavy) => "┝",
                (RIGHT, Border::Heavy) => "┥",
                (DOWN, _) => "╧",
                (UP, _) => "╤",
                (LEFT, _) => "╟",
                _ => "╢",
            };
            let (top, bottom) = self.border_text(node);
            let on_text = (by == node.y && bx < top) || (by == node.y + 2 && bx < bottom);
            if !on_text {
                put(buf, bx, by, tee, deco.border_style);
            }
        }
        // The handles on the nodes that show them.
        for (id, hot) in &look.handles {
            let Some(node) = self.node(id) else { continue };
            for side in Side::ALL {
                let (x, y) = node.handle(side);
                let is_hot = *hot == Some(side);
                put(buf, x, y, if is_hot { "●" } else { "◆" }, if is_hot { look.hot } else { look.handle });
            }
        }
    }
}

/// The straight runs of a route.
struct Run {
    x: i32,
    y: i32,
    x2: i32,
    y2: i32,
    len: i32,
    horizontal: bool,
}

fn runs(cells: &[(i32, i32)]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut start = 0;
    for k in 1..cells.len() {
        let horizontal = cells[k].1 == cells[k - 1].1;
        let next = cells.get(k + 1).map(|n| n.1 == cells[k].1);
        if next != Some(horizontal) {
            let (a, b) = (cells[start], cells[k]);
            out.push(Run { x: a.0, y: a.1, x2: b.0, y2: b.1, len: (k - start + 1) as i32, horizontal });
            start = k;
        }
    }
    out
}

/// The directions cell `k` of `cells` joins: the cells before and after it
/// (before the first, the node it leaves).
fn link_mask(cells: &[(i32, i32)], k: usize, out_of: u8) -> u8 {
    let toward = |a: (i32, i32), b: (i32, i32)| match (b.0 - a.0, b.1 - a.1) {
        (0, -1) => UP,
        (1, 0) => RIGHT,
        (0, 1) => DOWN,
        (-1, 0) => LEFT,
        _ => 0,
    };
    let here = cells[k];
    let before = if k == 0 { out_of } else { toward(here, cells[k - 1]) };
    let after = cells.get(k + 1).map_or(0, |&n| toward(here, n));
    before | after
}

/// The box-drawing glyph joining `mask`'s directions.
fn glyph(mask: u8, dashed: bool) -> &'static str {
    match mask & 15 {
        m if m == UP | DOWN || m == UP || m == DOWN => {
            if dashed {
                "┆"
            } else {
                "│"
            }
        }
        m if m == LEFT | RIGHT || m == LEFT || m == RIGHT => {
            if dashed {
                "┄"
            } else {
                "─"
            }
        }
        m if m == DOWN | RIGHT => "╭",
        m if m == DOWN | LEFT => "╮",
        m if m == UP | RIGHT => "╰",
        m if m == UP | LEFT => "╯",
        m if m == UP | DOWN | RIGHT => "├",
        m if m == UP | DOWN | LEFT => "┤",
        m if m == LEFT | RIGHT | DOWN => "┬",
        m if m == LEFT | RIGHT | UP => "┴",
        _ => "┼",
    }
}

/// A node's border weight: plain, the keyboard's (heavy), selected
/// (double).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Border {
    Plain,
    Heavy,
    Double,
}

/// How one node draws.
#[derive(Debug, Clone, Copy)]
pub(crate) struct NodeLook {
    pub border: Border,
    pub border_style: Style,
    pub glyph: &'static str,
    pub glyph_style: Style,
    pub name_style: Style,
    /// The box's inside.
    pub fill: Style,
}

/// How the canvas draws: the Builder's emphasis (selection, the keyboard,
/// a drawing) in the theme's colors.
pub(crate) struct Look<'a> {
    pub line: Style,
    pub label: Style,
    pub summary: Style,
    pub initial: Style,
    pub handle: Style,
    pub hot: Style,
    /// Each node's look.
    pub node: &'a dyn Fn(&str) -> NodeLook,
    /// An edge's style and rank (higher draws over lower).
    pub edge: &'a dyn Fn(&EdgeIn) -> (Style, u8),
    /// A line not in the draft yet (a drawing, a new transition).
    pub preview: Vec<(Route, Style)>,
    /// The nodes showing their handles, and the hot one.
    pub handles: Vec<(String, Option<Side>)>,
}

impl Look<'_> {
    fn node(&self, id: &str) -> NodeLook {
        (self.node)(id)
    }

    fn edge(&self, e: &EdgeIn) -> (Style, u8) {
        (self.edge)(e)
    }
}

/// A status's name as its node's top border shows it.
fn name_in(name: &str, w: i32) -> String {
    panel::truncate_right(name, (w - 8).max(1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::StatusCategory;
    use shelbi_app::settings::workflow_graph::{layout as graph_layout, EdgeSpec, NodeSpec};

    fn input(nodes: &[(&str, StatusCategory)], edges: &[(&str, &str, &str)]) -> Input {
        let specs: Vec<NodeSpec> = nodes.iter().map(|(id, c)| NodeSpec { id: id.to_string(), category: *c }).collect();
        let es: Vec<EdgeSpec> = edges
            .iter()
            .map(|(f, t, a)| EdgeSpec { from: f.to_string(), to: t.to_string(), actions: if a.is_empty() { vec![] } else { vec![a.to_string()] } })
            .collect();
        let g = graph_layout(&specs, &es);
        Input::of(&g, |id| (id.to_string(), "User · None".into(), false), |i| es[i].actions.join(" · "))
    }

    /// The routes as cells' masks: no two lines share a port or a run.
    fn assert_distinct(c: &Canvas) {
        let mut ends = std::collections::HashSet::new();
        for r in &c.routes {
            for end in [r.cells[0], *r.cells.last().unwrap()] {
                assert!(ends.insert(end), "a port shared at {end:?}");
            }
            for &(x, y) in &r.cells {
                assert!(c.node_at(x, y).is_none(), "a line through a node at {x},{y}");
            }
        }
    }

    #[test]
    fn two_way_and_doubled_transitions_and_a_self_loop_each_get_a_line() {
        use StatusCategory::*;
        let c = layout(input(
            &[("a", Active), ("b", Handoff)],
            &[("a", "b", "Push branch"), ("b", "a", "Close PR"), ("a", "b", "Merge"), ("a", "a", "")],
        ));
        assert_eq!(c.unrouted(), 0);
        assert_eq!(c.routes.len(), 4);
        assert_distinct(&c);
        // Each runs from its source's port to its destination's.
        for r in &c.routes {
            let e = &c.input.edges[r.edge];
            let (sx, sy) = r.cells[0];
            let (dx, dy) = step(r.out_of);
            assert_eq!(c.node_at(sx + dx, sy + dy).unwrap().id, e.from);
            let (tx, ty) = *r.cells.last().unwrap();
            let (dx, dy) = step(r.into);
            assert_eq!(c.node_at(tx + dx, ty + dy).unwrap().id, e.to);
        }
        // The self-loop goes out and back.
        let looped = &c.routes[3];
        assert!(looped.cells.len() >= 3, "{:?}", looped.cells);
    }

    #[test]
    fn the_same_draft_always_draws_the_same_and_disconnected_nodes_still_show() {
        use StatusCategory::*;
        let nodes = [("a", Backlog), ("b", Ready), ("c", Active), ("d", Done), ("lone", Archived)];
        let edges = [("a", "c", "Merge"), ("c", "a", ""), ("c", "d", "Push branch")];
        let one = layout(input(&nodes, &edges));
        let two = layout(input(&nodes, &edges));
        assert_eq!(one.routes, two.routes);
        assert_eq!(one.labels, two.labels);
        assert!(one.node("lone").is_some());
        assert_distinct(&one);
    }

    #[test]
    fn twenty_statuses_and_forty_transitions_route_quickly() {
        use shelbi_app::settings::workflow_graph::big_fixture;
        let nodes: Vec<(String, StatusCategory)> = big_fixture::CATEGORIES.iter().enumerate().map(|(i, &c)| (big_fixture::id(i), c)).collect();
        let edges: Vec<(String, String, String)> =
            big_fixture::transitions().into_iter().map(|(f, t, a)| (big_fixture::id(f), big_fixture::id(t), a.join(" · "))).collect();
        let nodes: Vec<(&str, StatusCategory)> = nodes.iter().map(|(i, c)| (i.as_str(), *c)).collect();
        let edges: Vec<(&str, &str, &str)> = edges.iter().map(|(f, t, a)| (f.as_str(), t.as_str(), a.as_str())).collect();
        let start = std::time::Instant::now();
        let c = layout(input(&nodes, &edges));
        assert!(start.elapsed() < std::time::Duration::from_secs(2), "{:?}", start.elapsed());
        assert_eq!(c.nodes.len(), 20);
        assert_distinct(&c);
    }
}
