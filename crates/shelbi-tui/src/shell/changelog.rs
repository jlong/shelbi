//! The Changelog page: the project's record of what has shipped, as an
//! old-style blog. It takes the sidebar's Changelog row (Activity is
//! palette-only now) and draws the shared [`ChangelogModel`].
//!
//! Entries run one after another, newest first, in a centered reading
//! column: the title in bold with the date at the right, a rule, the
//! description through the TUI's markdown renderer
//! ([`crate::markdown::render_note`]), and the entry's links as a row of
//! chips (`⧉` an issue, opened in Shelbi; `↗` a PR or URL, opened in the
//! browser). Over them, the title row: "Changelog", the match count, and
//! the sidebar's half-block search box.
//!
//! Keys: `j` / `k` and the arrows scroll, Page Up / Page Down (Space,
//! Ctrl+D / Ctrl+U) by the page, `n` / `p` (or `]` / `[`) jump to the
//! next or previous entry, `g` / `G` (Home / End) to the newest or oldest.
//! Tab / Shift+Tab step through the links, Enter follows the focused one
//! or, with none, folds or unfolds the entry. `/` searches as you type;
//! Enter keeps the query, Esc clears it. The wheel scrolls, a click on a
//! title folds its entry, a click on a chip follows it. Older entries load
//! as the reader nears the end ([`ChangelogModel::load_more`]).
//!
//! The focused entry's title and the focused link wear the half-block
//! selection pill; what's under the pointer, the quieter hover one.
//!
//! [`Doc`] lays the entries out for a width (cached until they, the query
//! or the folds change); drawing and the mouse both read it.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use shelbi_app::changelog::{
    ChangelogData, ChangelogEntryView, ChangelogLink, ChangelogModel, LinkKind, LinkTarget,
};
use unicode_width::UnicodeWidthStr;

use crate::panel::{padded_label, truncate_right};
use crate::sidebar::{BLEED_ABOVE, BLEED_BELOW};
use crate::theme::palette;

/// The page's title.
const TITLE: &str = "Changelog";
/// The search box's leading glyph and label (the sidebar's).
const SEARCH_GLYPH: &str = " 🔍 ";
const PLACEHOLDER: &str = "Search";
/// The search box's key hint, at its right while it's empty.
const SEARCH_KEY: &str = "/";
/// The clear button at the search box's right, once it has text.
const CLEAR: &str = "×";
/// The empty state, and how to add an entry.
const EMPTY: &str = "No changelog entries yet. Shipped work shows up here.";
const EMPTY_HINT: &str = "Add one with: shelbi changelog add --title \"…\"";
/// The widest the reading column gets: a blog's measure, not the terminal's.
const MAX_MEASURE: u16 = 100;
/// The search box's widest.
const SEARCH_MAX: u16 = 34;
/// Rows the header takes: the search box's two caps around the title row.
const HEADER_ROWS: u16 = 3;
/// The key hints along the bottom show from this height up.
const HINTS_MIN_HEIGHT: u16 = 14;
const HINTS: &str =
    "j/k scroll · n/p entry · g/G ends · / search · Tab links · ↵ open";
/// Lines one wheel notch scrolls.
const WHEEL_LINES: usize = 3;
/// How near the end (in screens) the next page of entries loads.
const LOAD_AHEAD_SCREENS: usize = 1;

/// A chip's glyph for its kind.
fn glyph(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Issue => "⧉",
        LinkKind::Pr | LinkKind::Url => "↗",
    }
}

/// Whether a link goes anywhere (a PR number with no known repository
/// doesn't, and isn't focusable).
fn followable(link: &ChangelogLink) -> bool {
    link.target != LinkTarget::Unresolved
}

/// What a key or click asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Open this Shelbi issue's detail.
    OpenIssue(String),
    /// Open this URL in the browser.
    OpenUrl(String),
}

/// Something the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    /// An entry's title row: its position among the shown entries.
    Title(usize),
    /// A chip: the entry's position and the link's index in its links.
    Link(usize, usize),
    Search,
    Clear,
}

/// One chip, placed on its row: `x` is the column of its pill's left
/// edge, relative to the reading column's left minus one (so a chip's
/// glyph lines up with the text above it).
#[derive(Debug, Clone)]
struct Chip {
    link: usize,
    x: u16,
    width: u16,
    /// The label as it fits.
    label: String,
}

/// One entry laid out: where its rows land in the document.
#[derive(Debug, Clone)]
struct EntryRows {
    /// The gap row over the title (the pill's top bleed).
    top: usize,
    /// The description, wrapped to the measure; empty when folded.
    body: Vec<Line<'static>>,
    /// Each chip row's document line and its chips.
    chips: Vec<(usize, Vec<Chip>)>,
    /// One past its last row.
    end: usize,
}

impl EntryRows {
    fn title(&self) -> usize {
        self.top + 1
    }
    fn rule(&self) -> usize {
        self.top + 2
    }
    fn body_top(&self) -> usize {
        self.top + 3
    }
}

/// The shown entries laid out for one measure.
#[derive(Debug, Clone, Default)]
struct Doc {
    width: u16,
    generation: u64,
    entries: Vec<EntryRows>,
    /// The "Loading older entries…" row, when more are past the shown pages.
    more: Option<usize>,
    height: usize,
}

impl Doc {
    fn build(
        entries: &[&ChangelogEntryView],
        collapsed: &HashSet<String>,
        has_more: bool,
        width: u16,
        generation: u64,
    ) -> Doc {
        let mut y = 0;
        let mut rows = Vec::with_capacity(entries.len());
        for e in entries {
            let top = y;
            let folded = collapsed.contains(&e.id);
            let mut body = Vec::new();
            let mut chips = Vec::new();
            y = top + 3;
            if !folded {
                body = description_lines(&e.description, width as usize);
                y += body.len();
                let links: Vec<(usize, &ChangelogLink)> = e.links.iter().enumerate().collect();
                if !links.is_empty() {
                    // A blank over each chip row for its pill's top bleed,
                    // one under the last for the bottom.
                    for row in chip_rows(&links, width) {
                        y += 1;
                        chips.push((y, row));
                        y += 1;
                    }
                } else if !body.is_empty() {
                    y += 1;
                }
            }
            rows.push(EntryRows {
                top,
                body,
                chips,
                end: y,
            });
        }
        let more = has_more.then(|| {
            y += 1;
            y - 1
        });
        Doc {
            width,
            generation,
            entries: rows,
            more,
            height: y,
        }
    }

    /// The entry whose rows hold document line `line` (or the last one
    /// starting at or before it).
    fn entry_at(&self, line: usize) -> Option<usize> {
        let ix = self.entries.partition_point(|e| e.top <= line);
        ix.checked_sub(1)
    }
}

/// An entry's description as lines for the measure: the markdown
/// renderer's, blank lines at either end dropped, unstyled text in the
/// body color.
fn description_lines(description: &str, width: usize) -> Vec<Line<'static>> {
    let trimmed = description.trim_matches('\n').trim_end();
    if trimmed.trim().is_empty() {
        return Vec::new();
    }
    crate::markdown::render_note(trimmed, width)
        .into_iter()
        .map(|mut line| {
            for span in &mut line.spans {
                if span.style.fg.is_none() {
                    span.style = span.style.fg(palette().foreground);
                }
            }
            line
        })
        .collect()
}

/// Flow `links` into rows of chips across `width`: each chip's pill is
/// one cell of padding, its glyph, a space, its label, one of padding;
/// one blank between pills.
fn chip_rows(links: &[(usize, &ChangelogLink)], width: u16) -> Vec<Vec<Chip>> {
    let room = width as usize + 2;
    let mut rows: Vec<Vec<Chip>> = Vec::new();
    let mut row: Vec<Chip> = Vec::new();
    let mut x = 0usize;
    for (ix, link) in links {
        // The most a label can take on a row of its own.
        let label = truncate_right(&link.label, room.saturating_sub(4).max(1));
        let w = label.width() + 4;
        let at = if row.is_empty() { 0 } else { x + 1 };
        if !row.is_empty() && at + w > room {
            rows.push(std::mem::take(&mut row));
            x = 0;
        }
        let at = if row.is_empty() { 0 } else { x + 1 };
        row.push(Chip {
            link: *ix,
            x: at as u16,
            width: w as u16,
            label,
        });
        x = at + w;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Where everything sits for one area: the header, the document's
/// viewport, the hints.
#[derive(Debug, Clone, Copy, Default)]
struct Plan {
    /// The reading column (left edge and measure).
    column: Rect,
    title_y: u16,
    search: Option<Rect>,
    viewport: Rect,
    hints: Option<Rect>,
}

fn plan(area: Rect) -> Plan {
    let measure = area.width.saturating_sub(4).min(MAX_MEASURE);
    let x = area.x + (area.width - measure) / 2;
    let column = Rect {
        x,
        y: area.y,
        width: measure,
        height: area.height,
    };
    let hints = (area.height >= HINTS_MIN_HEIGHT).then(|| Rect {
        x,
        y: area.bottom() - 1,
        width: measure,
        height: 1,
    });
    let top = area.y + HEADER_ROWS.min(area.height);
    let bottom = hints.map_or(area.bottom(), |h| h.y.saturating_sub(1));
    let viewport = Rect {
        x,
        y: top,
        width: measure,
        height: bottom.saturating_sub(top),
    };
    let search_w = SEARCH_MAX.min(measure / 2);
    let search = (area.height >= HEADER_ROWS && search_w >= 12).then(|| Rect {
        x: column.right() + 1 - search_w,
        y: area.y,
        width: search_w,
        height: 3,
    });
    Plan {
        column,
        title_y: area.y + 1.min(area.height.saturating_sub(1)),
        search,
        viewport,
        hints,
    }
}

/// The Changelog page's state: the shared model, which entries are
/// folded, the focused entry and link, the scroll, and the search.
pub struct ChangelogPage {
    model: ChangelogModel,
    /// Folded entries, by id (kept across refreshes).
    collapsed: HashSet<String>,
    /// The focused entry, by id (kept across refreshes).
    selected: Option<String>,
    /// The focused link: an index into the focused entry's links.
    link: Option<usize>,
    /// Document lines scrolled past.
    scroll: usize,
    /// Whether the search has the keys.
    searching: bool,
    hover: Option<Hit>,
    /// Bumped whenever the laid-out document would change.
    generation: u64,
    doc: Doc,
    last_area: Rect,
}

impl Default for ChangelogPage {
    fn default() -> Self {
        Self::new()
    }
}

impl ChangelogPage {
    pub fn new() -> Self {
        ChangelogPage {
            model: ChangelogModel::new(),
            collapsed: HashSet::new(),
            selected: None,
            link: None,
            scroll: 0,
            searching: false,
            hover: None,
            generation: 1,
            doc: Doc::default(),
            last_area: Rect::default(),
        }
    }

    #[cfg(test)]
    pub fn model(&self) -> &ChangelogModel {
        &self.model
    }

    /// Whether the search has the keys.
    #[cfg(test)]
    pub fn searching(&self) -> bool {
        self.searching
    }

    /// Take a fresh read. Reading in place stays in place: when the reader
    /// has scrolled, the entry at the top of the screen stays there as new
    /// ones land above it.
    pub fn set_data(&mut self, data: ChangelogData) {
        let anchor = (self.scroll > 0)
            .then(|| self.anchor())
            .flatten();
        self.model.set_data(data);
        self.relayout_now();
        if let Some((id, offset)) = anchor {
            if let Some(ix) = self.position_of(&id) {
                self.scroll = self.doc.entries[ix].top + offset;
            }
        }
        self.keep_selection();
    }

    /// Search: narrow to entries holding every word of `query`. Back to the
    /// newest match.
    pub fn set_query(&mut self, query: &str) {
        if query == self.model.query() {
            return;
        }
        self.model.set_query(query);
        self.scroll = 0;
        self.selected = None;
        self.link = None;
        self.relayout_now();
        self.keep_selection();
    }

    /// The entry at the top of the screen and how far into it the screen
    /// starts.
    fn anchor(&self) -> Option<(String, usize)> {
        let ix = self.doc.entry_at(self.scroll)?;
        let id = self.shown().get(ix)?.id.clone();
        Some((id, self.scroll - self.doc.entries[ix].top))
    }

    fn shown(&self) -> Vec<&ChangelogEntryView> {
        self.model.visible().collect()
    }

    fn position_of(&self, id: &str) -> Option<usize> {
        self.model.visible().position(|e| e.id == id)
    }

    fn selected_ix(&self) -> Option<usize> {
        self.selected.as_deref().and_then(|id| self.position_of(id))
    }

    fn select(&mut self, ix: usize) {
        let id = self.model.visible().nth(ix).map(|e| e.id.clone());
        if id != self.selected {
            self.link = None;
        }
        self.selected = id;
    }

    /// The focused entry's still shown and its focused link still there; or
    /// focus the entry at the top of the screen.
    fn keep_selection(&mut self) {
        match self.selected_ix() {
            Some(ix) => {
                let links = self.shown()[ix].links.len();
                if self.link.is_some_and(|l| l >= links) || self.collapsed.contains(self.selected.as_deref().unwrap_or("")) {
                    self.link = None;
                }
            }
            None => {
                self.selected = None;
                self.link = None;
                if let Some(ix) = self.doc.entry_at(self.scroll) {
                    self.select(ix);
                }
            }
        }
    }

    fn relayout_now(&mut self) {
        self.generation += 1;
        self.relayout(plan(self.last_area).column.width);
    }

    /// Lay the shown entries out for `width`, unless they already are.
    fn relayout(&mut self, width: u16) {
        if self.doc.width == width && self.doc.generation == self.generation {
            return;
        }
        let collapsed = &self.collapsed;
        let entries: Vec<&ChangelogEntryView> = self.model.visible().collect();
        self.doc = Doc::build(&entries, collapsed, self.model.has_more(), width, self.generation);
    }

    fn viewport_height(&self) -> usize {
        (plan(self.last_area).viewport.height as usize).max(1)
    }

    fn max_scroll(&self) -> usize {
        self.doc.height.saturating_sub(self.viewport_height())
    }

    /// Scroll by `delta` lines; the focus follows when its entry leaves the
    /// screen.
    fn scroll_by(&mut self, delta: isize) {
        self.scroll = self.scroll.saturating_add_signed(delta).min(self.max_scroll());
        self.load_ahead();
        let (top, bottom) = (self.scroll, self.scroll + self.viewport_height());
        let on_screen = self.selected_ix().is_some_and(|ix| {
            let e = &self.doc.entries[ix];
            e.top < bottom && e.end > top
        });
        if !on_screen {
            if let Some(ix) = self.doc.entry_at(self.scroll) {
                // The entry the screen starts in, or the next when only its
                // tail shows.
                let ix = if self.doc.entries[ix].title() < top && ix + 1 < self.doc.entries.len() {
                    ix + 1
                } else {
                    ix
                };
                self.select(ix);
            }
        }
    }

    /// Load the next page when the reader is within a screen of the end.
    fn load_ahead(&mut self) {
        let screen = self.viewport_height();
        if self.model.has_more()
            && self.scroll + screen * (1 + LOAD_AHEAD_SCREENS) >= self.doc.height
        {
            self.model.load_more();
            self.relayout_now();
        }
    }

    /// Focus entry `ix` and bring its title to the top of the screen.
    fn jump_to(&mut self, ix: usize) {
        let Some(e) = self.doc.entries.get(ix) else {
            return;
        };
        self.scroll = e.top.min(self.max_scroll());
        self.select(ix);
        self.link = None;
        self.load_ahead();
    }

    fn jump_by(&mut self, delta: isize) {
        let n = self.doc.entries.len();
        if n == 0 {
            return;
        }
        let from = self.selected_ix().unwrap_or(0);
        let to = from.saturating_add_signed(delta).min(n - 1);
        if to == from && delta > 0 && self.model.has_more() {
            self.model.load_more();
            self.relayout_now();
            return self.jump_by(delta);
        }
        self.jump_to(to);
    }

    /// `G`: every page, then the oldest entry.
    fn jump_to_oldest(&mut self) {
        let mut grew = false;
        while self.model.has_more() {
            self.model.load_more();
            grew = true;
        }
        if grew {
            self.relayout_now();
        }
        let last = self.doc.entries.len().saturating_sub(1);
        self.jump_to(last);
    }

    /// The followable links in document order, as (entry, link).
    fn focus_order(&self) -> Vec<(usize, usize)> {
        let shown = self.shown();
        self.doc
            .entries
            .iter()
            .enumerate()
            .flat_map(|(ix, rows)| {
                let entry = shown[ix];
                rows.chips
                    .iter()
                    .flat_map(|(_, chips)| chips.iter().map(|c| c.link))
                    .filter(move |l| followable(&entry.links[*l]))
                    .map(move |l| (ix, l))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Tab / Shift+Tab: the next or previous link, from the focused one (or
    /// the focused entry), its row scrolled into view.
    fn step_link(&mut self, forward: bool) {
        let order = self.focus_order();
        if order.is_empty() {
            return;
        }
        let sel = self.selected_ix().unwrap_or(0);
        let next = match self.link {
            Some(l) => {
                let at = order.iter().position(|&p| p == (sel, l));
                match (at, forward) {
                    (Some(i), true) => order.get(i + 1).copied(),
                    (Some(i), false) => i.checked_sub(1).map(|i| order[i]),
                    (None, _) => None,
                }
            }
            None if forward => order.iter().find(|(e, _)| *e >= sel).copied(),
            None => order.iter().rev().find(|(e, _)| *e < sel).copied(),
        };
        let Some((ix, l)) = next else {
            return;
        };
        self.select(ix);
        self.link = Some(l);
        self.reveal_link(ix, l);
    }

    /// Scroll the least that shows link `l` of entry `ix` with its pill.
    fn reveal_link(&mut self, ix: usize, l: usize) {
        let Some(y) = self.doc.entries[ix]
            .chips
            .iter()
            .find(|(_, chips)| chips.iter().any(|c| c.link == l))
            .map(|(y, _)| *y)
        else {
            return;
        };
        let screen = self.viewport_height();
        if y < self.scroll + 1 {
            self.scroll = y.saturating_sub(1);
        } else if y + 2 > self.scroll + screen {
            self.scroll = (y + 2).saturating_sub(screen).min(self.max_scroll());
        }
        self.load_ahead();
    }

    /// Fold or unfold entry `ix`, keeping its title where it is on screen.
    fn toggle(&mut self, ix: usize) {
        let Some(id) = self.model.visible().nth(ix).map(|e| e.id.clone()) else {
            return;
        };
        let screen_y = self.doc.entries[ix].top as isize - self.scroll as isize;
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.select(ix);
        self.link = None;
        self.relayout_now();
        if screen_y >= 0 {
            self.scroll = (self.doc.entries[ix].top - screen_y as usize).min(self.max_scroll());
        }
        self.load_ahead();
    }

    fn follow(&self, ix: usize, l: usize) -> Step {
        let Some(link) = self.model.visible().nth(ix).and_then(|e| e.links.get(l)) else {
            return Step::Stay;
        };
        match &link.target {
            LinkTarget::Issue(id) => Step::OpenIssue(id.clone()),
            LinkTarget::Url(url) => Step::OpenUrl(url.clone()),
            LinkTarget::Unresolved => Step::Stay,
        }
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> Step {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.viewport_height().saturating_sub(2).max(1) as isize;
        // The keys the search shares with the page: the arrows and paging.
        match k.code {
            KeyCode::Up => return self.stay(|p| p.scroll_by(-1)),
            KeyCode::Down => return self.stay(|p| p.scroll_by(1)),
            KeyCode::PageUp => return self.stay(|p| p.scroll_by(-page)),
            KeyCode::PageDown => return self.stay(|p| p.scroll_by(page)),
            _ => {}
        }
        if self.searching {
            self.search_key(k);
            return Step::Stay;
        }
        match k.code {
            KeyCode::Char('k') if !ctrl => self.scroll_by(-1),
            KeyCode::Char('j') if !ctrl => self.scroll_by(1),
            KeyCode::Char('u') if ctrl => self.scroll_by(-page / 2),
            KeyCode::Char('d') if ctrl => self.scroll_by(page / 2),
            KeyCode::Char(' ') => self.scroll_by(page),
            KeyCode::Char('n') | KeyCode::Char(']') => self.jump_by(1),
            KeyCode::Char('p') | KeyCode::Char('[') => self.jump_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.jump_to(0),
            KeyCode::Char('G') | KeyCode::End => self.jump_to_oldest(),
            KeyCode::Char('/') => self.searching = true,
            KeyCode::Char('f') if ctrl => self.searching = true,
            KeyCode::Tab => self.step_link(true),
            KeyCode::BackTab => self.step_link(false),
            KeyCode::Enter => {
                if let (Some(ix), Some(l)) = (self.selected_ix(), self.link) {
                    return self.follow(ix, l);
                }
                if let Some(ix) = self.selected_ix() {
                    self.toggle(ix);
                }
            }
            KeyCode::Esc if self.link.is_some() => self.link = None,
            KeyCode::Esc => self.set_query(""),
            _ => {}
        }
        Step::Stay
    }

    fn stay(&mut self, f: impl FnOnce(&mut Self)) -> Step {
        f(self);
        Step::Stay
    }

    /// A key while the search has them: typing narrows as it goes, Enter
    /// keeps the query, Esc clears it.
    fn search_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter | KeyCode::Tab => self.searching = false,
            KeyCode::Esc => {
                self.searching = false;
                self.set_query("");
            }
            KeyCode::Backspace => {
                let mut q = self.model.query().to_string();
                q.pop();
                self.set_query(&q);
            }
            KeyCode::Char('u') if ctrl => self.set_query(""),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                let q = format!("{}{c}", self.model.query());
                self.set_query(&q);
            }
            _ => {}
        }
    }

    /// Pasted text goes into the search while it has the keys.
    pub fn paste(&mut self, text: &str) -> bool {
        if !self.searching {
            return false;
        }
        let line = text.lines().next().unwrap_or("");
        let q = format!("{}{line}", self.model.query());
        self.set_query(&q);
        true
    }

    /// A mouse event over `area` (the main area).
    pub fn handle_mouse(&mut self, m: MouseEvent, area: Rect) -> Step {
        self.last_area = area;
        let hit = self.hit(m.column, m.row);
        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = hit;
                Step::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(Hit::Title(ix)) => {
                    self.searching = false;
                    self.toggle(ix);
                    Step::Stay
                }
                Some(Hit::Link(ix, l)) => {
                    self.searching = false;
                    self.select(ix);
                    self.link = Some(l);
                    self.follow(ix, l)
                }
                Some(Hit::Search) => {
                    self.searching = true;
                    Step::Stay
                }
                Some(Hit::Clear) => {
                    self.searching = true;
                    self.set_query("");
                    Step::Stay
                }
                None => {
                    self.searching = false;
                    Step::Stay
                }
            },
            MouseEventKind::ScrollUp => self.stay(|p| p.scroll_by(-(WHEEL_LINES as isize))),
            MouseEventKind::ScrollDown => self.stay(|p| p.scroll_by(WHEEL_LINES as isize)),
            _ => Step::Stay,
        }
    }

    fn hit(&self, col: u16, row: u16) -> Option<Hit> {
        let plan = plan(self.last_area);
        let inside = |r: Rect| col >= r.x && col < r.right() && row >= r.y && row < r.bottom();
        if let Some(search) = plan.search.filter(|s| inside(*s)) {
            if !self.model.query().is_empty() && col == clear_x(search) && row == search.y + 1 {
                return Some(Hit::Clear);
            }
            return Some(Hit::Search);
        }
        let vp = plan.viewport;
        if !inside(Rect { x: vp.x.saturating_sub(1), width: vp.width + 2, ..vp }) {
            return None;
        }
        let line = self.scroll + (row - vp.y) as usize;
        let ix = self.doc.entry_at(line)?;
        let rows = &self.doc.entries[ix];
        if line == rows.title() {
            return Some(Hit::Title(ix));
        }
        let entry = self.model.visible().nth(ix)?;
        let (_, chips) = rows.chips.iter().find(|(y, _)| *y == line)?;
        let left = vp.x.saturating_sub(1);
        chips
            .iter()
            .find(|c| col >= left + c.x && col < left + c.x + c.width)
            .filter(|c| followable(&entry.links[c.link]))
            .map(|c| Hit::Link(ix, c.link))
    }

    /// Paint the page over `area`. Returns where the terminal's cursor
    /// goes: the search's caret while it has the keys.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) -> Option<(u16, u16)> {
        self.last_area = area;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].reset();
            }
        }
        if area.width < 8 || area.height < 4 {
            return None;
        }
        let plan = plan(area);
        self.relayout(plan.column.width);
        self.load_ahead();
        self.scroll = self.scroll.min(self.max_scroll());
        if self.selected_ix().is_none() {
            self.keep_selection();
        }

        let cursor = self.render_header(buf, &plan);
        if let Some(h) = plan.hints {
            let style = Style::default().fg(palette().muted);
            buf.set_stringn(h.x, h.y, truncate_right(HINTS, h.width as usize), h.width as usize, style);
        }
        let vp = plan.viewport;
        if vp.height == 0 {
            return cursor;
        }
        if let Some(notice) = self.notice() {
            render_notice(buf, vp, &notice);
            return cursor;
        }
        self.render_doc(buf, vp);
        cursor
    }

    /// The page's message in place of entries, if it has one.
    fn notice(&self) -> Option<Vec<(String, Style)>> {
        let muted = Style::default().fg(palette().muted);
        if self.model.is_loading() {
            return Some(vec![("Loading…".into(), muted)]);
        }
        if let Some(err) = self.model.data().and_then(|d| d.error.as_ref()) {
            return Some(vec![(format!("Couldn't read the changelog: {err}"), muted)]);
        }
        if self.model.total() == 0 {
            return Some(vec![
                (EMPTY.into(), Style::default().fg(palette().foreground)),
                (String::new(), muted),
                (EMPTY_HINT.into(), muted),
            ]);
        }
        if self.model.match_count() == 0 {
            return Some(vec![(
                format!("No entries match “{}”.", self.model.query()),
                muted,
            )]);
        }
        None
    }

    fn render_header(&self, buf: &mut Buffer, plan: &Plan) -> Option<(u16, u16)> {
        let col = plan.column;
        let y = plan.title_y;
        let title = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
        let end = plan.search.map_or(col.right(), |s| s.x.saturating_sub(2));
        let room = end.saturating_sub(col.x) as usize;
        let (x, _) = buf.set_stringn(col.x, y, TITLE, room, title);
        let count = self.count_label();
        if !count.is_empty() && x + 2 + count.width() as u16 <= end {
            buf.set_string(x + 2, y, &count, Style::default().fg(palette().muted));
        }
        let search = plan.search?;
        self.render_search(buf, search)
    }

    /// "41 entries", or with a query "3 of 41".
    fn count_label(&self) -> String {
        if self.model.is_loading() || self.model.total() == 0 {
            return String::new();
        }
        let total = self.model.total();
        if self.model.query().is_empty() {
            format!("{total} {}", if total == 1 { "entry" } else { "entries" })
        } else {
            format!("{} of {total}", self.model.match_count())
        }
    }

    fn render_search(&self, buf: &mut Buffer, rect: Rect) -> Option<(u16, u16)> {
        let fill = if self.searching { palette().input_focus } else { palette().search };
        let (above, below) = crate::textbox::caps(rect.width as usize, fill);
        buf.set_line(rect.x, rect.y, &above, rect.width);
        buf.set_line(rect.x, rect.y + 2, &below, rect.width);
        let y = rect.y + 1;
        let base = Style::default().bg(fill);
        for x in rect.left()..rect.right() {
            buf[(x, y)].set_symbol(" ").set_style(base);
        }
        let gray = base.fg(palette().gray);
        buf.set_stringn(rect.x, y, SEARCH_GLYPH, rect.width as usize, gray);
        let text_x = rect.x + (SEARCH_GLYPH.width() as u16).min(rect.width);
        let query = self.model.query();
        let caret = if query.is_empty() {
            let hint = if self.searching { "" } else { SEARCH_KEY };
            let room = rect.right().saturating_sub(text_x) as usize;
            buf.set_stringn(text_x, y, truncate_right(PLACEHOLDER, room), room, gray);
            if !hint.is_empty() {
                buf.set_string(rect.right() - 1 - hint.width() as u16, y, hint, gray);
            }
            (text_x, y)
        } else {
            let clear = clear_x(rect);
            let room = clear.saturating_sub(text_x + 1) as usize;
            let shown = tail_to_width(query, room.saturating_sub(1));
            buf.set_stringn(text_x, y, &shown, room, base.fg(palette().white));
            let lit = self.hover == Some(Hit::Clear);
            let fg = if lit { palette().white } else { palette().gray };
            buf.set_string(clear, y, CLEAR, base.fg(fg));
            (text_x + shown.width() as u16, y)
        };
        // The double-width 🔍 clears its trailing cell's fill as it's laid
        // down: paint the fill back over the row.
        for x in rect.left()..rect.right() {
            buf[(x, y)].set_bg(fill);
        }
        Some(caret).filter(|(x, _)| self.searching && *x < rect.right())
    }

    fn render_doc(&self, buf: &mut Buffer, vp: Rect) {
        let shown = self.shown();
        let selected = self.selected_ix();
        let top = self.scroll;
        let bottom = top + vp.height as usize;
        let screen = |line: usize| (line >= top && line < bottom).then(|| vp.y + (line - top) as u16);
        // The pills reach one column past the reading column on each side.
        let pill_x = vp.x.saturating_sub(1);
        let pill_w = vp.width + 2;
        for (ix, rows) in self.doc.entries.iter().enumerate() {
            if rows.end <= top || rows.top >= bottom {
                continue;
            }
            let entry = shown[ix];
            let fill = if Some(ix) == selected && self.link.is_none() {
                Some(palette().selection)
            } else if self.hover == Some(Hit::Title(ix)) {
                Some(palette().row_hover)
            } else {
                None
            };
            if let (Some(y), Some(f)) = (screen(rows.top), fill) {
                buf.set_string(pill_x, y, BLEED_ABOVE.repeat(pill_w as usize), Style::default().fg(f));
            }
            if let Some(y) = screen(rows.title()) {
                self.render_title(buf, entry, pill_x, y, pill_w, fill);
            }
            if let Some(y) = screen(rows.rule()) {
                match fill {
                    Some(f) => buf.set_string(pill_x, y, BLEED_BELOW.repeat(pill_w as usize), Style::default().fg(f)),
                    None => buf.set_string(vp.x, y, "─".repeat(vp.width as usize), Style::default().fg(palette().divider)),
                };
            }
            for (i, line) in rows.body.iter().enumerate() {
                if let Some(y) = screen(rows.body_top() + i) {
                    buf.set_line(vp.x, y, line, vp.width);
                }
            }
            for (line, chips) in &rows.chips {
                for chip in chips {
                    self.render_chip(buf, entry, ix, chip, pill_x, *line, &screen);
                }
            }
        }
        if let Some(y) = self.doc.more.and_then(screen) {
            let style = Style::default().fg(palette().muted);
            buf.set_stringn(vp.x, y, "Loading older entries…", vp.width as usize, style);
        }
    }

    /// An entry's title row: the title in bold, `⋯` after it when folded,
    /// the date at the right; on its pill's fill when it has one.
    fn render_title(
        &self,
        buf: &mut Buffer,
        entry: &ChangelogEntryView,
        pill_x: u16,
        y: u16,
        pill_w: u16,
        fill: Option<Color>,
    ) {
        let base = fill.map_or(Style::default(), |f| Style::default().bg(f));
        if fill.is_some() {
            for x in pill_x..pill_x + pill_w {
                buf[(x, y)].set_symbol(" ").set_style(base);
            }
        }
        let x = pill_x + 1;
        let width = pill_w.saturating_sub(2) as usize;
        let title = base.fg(palette().white).add_modifier(Modifier::BOLD);
        let date_fg = if fill.is_some() { palette().gray } else { palette().muted };
        let date_style = base.fg(date_fg);
        let folded = self.collapsed.contains(&entry.id);
        let suffix = if folded { " ⋯" } else { "" };
        let title_w = entry.title.width() + suffix.width();
        // The date at the right: spelled out, or short, or not at all when
        // the title needs the room.
        let long = entry.date_label();
        let short = entry.date.format("%b %-d, %Y").to_string();
        let date = if title_w + 2 + long.width() <= width {
            long
        } else if title_w + 2 + short.width() <= width || width >= 48 {
            short
        } else {
            String::new()
        };
        let date_w = date.width();
        let title_room = if date.is_empty() { width } else { width - date_w - 2 };
        let shown = truncate_right(&entry.title, title_room.saturating_sub(suffix.width()).max(1));
        let (end, _) = buf.set_stringn(x, y, &shown, title_room, title);
        if folded {
            buf.set_stringn(end, y, suffix, title_room.saturating_sub(shown.width()), base.fg(palette().muted));
        }
        if !date.is_empty() {
            buf.set_string(x + (width - date_w) as u16, y, &date, date_style);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_chip(
        &self,
        buf: &mut Buffer,
        entry: &ChangelogEntryView,
        ix: usize,
        chip: &Chip,
        pill_x: u16,
        line: usize,
        screen: &dyn Fn(usize) -> Option<u16>,
    ) {
        let link = &entry.links[chip.link];
        let live = followable(link);
        let focused = self.selected_ix() == Some(ix) && self.link == Some(chip.link);
        let hovered = self.hover == Some(Hit::Link(ix, chip.link));
        let x = pill_x + chip.x;
        let fill = if focused {
            Some(palette().selection)
        } else if hovered {
            Some(palette().row_hover)
        } else {
            None
        };
        if let Some(f) = fill {
            let caps = Style::default().fg(f);
            if let Some(y) = line.checked_sub(1).and_then(screen) {
                buf.set_string(x, y, BLEED_ABOVE.repeat(chip.width as usize), caps);
            }
            if let Some(y) = screen(line + 1) {
                buf.set_string(x, y, BLEED_BELOW.repeat(chip.width as usize), caps);
            }
        }
        let Some(y) = screen(line) else {
            return;
        };
        let base = fill.map_or(Style::default(), |f| Style::default().bg(f));
        let fg = match (live, focused) {
            (false, _) => palette().muted,
            (true, true) => palette().white,
            (true, false) => palette().cyan,
        };
        let mut style = base.fg(fg);
        if focused {
            style = style.add_modifier(Modifier::BOLD);
        }
        if hovered {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        let mut spans = vec![
            Span::styled(" ", base),
            Span::styled(glyph(link.kind), base.fg(fg)),
        ];
        spans.extend(padded_label(chip.label.clone(), 1, 1, style));
        buf.set_line(x, y, &Line::from(spans), chip.width);
    }
}

/// The clear button's column: one in from the search box's right edge.
fn clear_x(search: Rect) -> u16 {
    search.right().saturating_sub(2)
}

/// The end of `s` that fits `width` columns.
fn tail_to_width(s: &str, width: usize) -> String {
    let mut w = 0;
    let mut out: Vec<char> = Vec::new();
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.into_iter().rev().collect()
}

/// A message in place of the entries, centered in the viewport.
fn render_notice(buf: &mut Buffer, vp: Rect, lines: &[(String, Style)]) {
    let width = vp.width as usize;
    let wrapped: Vec<(String, Style)> = lines
        .iter()
        .flat_map(|(text, style)| {
            if text.is_empty() {
                return vec![(String::new(), *style)];
            }
            shelbi_app::overlay::error_log::wrap(text, width)
                .into_iter()
                .map(|l| (l, *style))
                .collect()
        })
        .collect();
    let top = vp.y + (vp.height.saturating_sub(wrapped.len() as u16)) / 3;
    for (i, (text, style)) in wrapped.iter().enumerate() {
        let y = top + i as u16;
        if y >= vp.bottom() {
            break;
        }
        let x = vp.x + vp.width.saturating_sub(text.width() as u16) / 2;
        buf.set_stringn(x, y, text, width, *style);
    }
}

#[cfg(test)]
pub(crate) mod tests;
