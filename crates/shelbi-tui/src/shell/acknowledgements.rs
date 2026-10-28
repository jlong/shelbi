//! Acknowledgements: the TUI's twin of the desktop's window
//! (`desktop/src/views/bundled_software.rs`; Figma "Bundled Software ·
//! Dark", 2453:8397). The third-party software the `shelbi` binary ships
//! ([`crate::licenses`]), each with its full license.
//!
//! A full-screen view over everything (the sidebar's column and the main
//! area, Settings included), as the desktop's is its own window: what's
//! under it keeps running and shows again on Done or Esc. The same
//! structure, words and order as the desktop's:
//!
//! - the title row: "Acknowledgements", and Done (the primary button) at
//!   the right;
//! - on the left the search (the sidebar's half-block search box,
//!   filtering names and licenses as you type), then the list: each entry's
//!   name over its license, the selected row in the half-block selection
//!   pill, the row under the pointer in a quieter one;
//! - on the right the selected entry: its name, who made it and what it is,
//!   View source (its homepage or repository in the browser), what it does
//!   in Shelbi, a rule, then the license's name over its full text,
//!   scrolled by the keyboard and the mouse wheel.
//!
//! Keys: ↑ / ↓ move the selection (Home / End to the ends), Page Up / Page
//! Down and Shift+↑ / Shift+↓ scroll the license, `/` (or the desktop's
//! ⌘F, as Ctrl+F in a terminal that doesn't pass ⌘) types into the search,
//! Tab steps through the search, the list, View source and Done, Enter or
//! Space presses the focused button. Esc in the search clears it, or,
//! empty, hands the keys back to the list; anywhere else it leaves, as Done
//! does.
//!
//! [`layout`] is one pure function of the area and the state, shared by
//! drawing and the mouse.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use shelbi_app::licenses::{license_blocks, Block, Browser, Entry};
use shelbi_app::overlay::error_log::wrap;
use unicode_width::UnicodeWidthStr;

use crate::button::Button;
use crate::panel::truncate_right;
use crate::sidebar::{bleed_separator, BLEED_ABOVE, BLEED_BELOW};
use crate::theme::palette;

/// The view's title, the desktop window's.
const TITLE: &str = "Acknowledgements";
/// The primary button, which leaves.
const DONE: &str = "Done";
/// The detail's button; the arrow stands for the desktop's
/// arrow-top-right-on-square icon.
const VIEW_SOURCE: &str = "View source ↗";
/// The search's placeholder, the desktop's.
const PLACEHOLDER: &str = "Search acknowledgements";
/// The detail with nothing selected (the search hides every row).
const NO_MATCH: &str = "Nothing matches the search.";
/// The search box's leading glyph and the space around it (the sidebar's).
const SEARCH_GLYPH: &str = " 🔍 ";
/// The clear button at the search box's right, shown once it has text (the
/// desktop's x-mark).
const CLEAR: &str = "×";
/// Rows an item takes in the list (name, license) and the separator under
/// it.
const ITEM_ROWS: u16 = 3;
/// Lines one wheel notch scrolls the license.
const WHEEL_LINES: usize = 3;
/// The detail below this width stacks View source under the description
/// instead of beside the name.
const STACK_BELOW: u16 = 48;

/// What has the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Search,
    ViewSource,
    Done,
}

/// The Tab order, as the window reads left to right.
const ORDER: [Focus; 4] = [Focus::Search, Focus::List, Focus::ViewSource, Focus::Done];

/// Something the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A list row: its entry's index.
    Row(usize),
    Search,
    Clear,
    ViewSource,
    Done,
}

/// What a key or click asks of the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Done or Esc: back to what it was opened over.
    Leave,
    /// View source: open this page in the browser; the view stays.
    Open(&'static str),
}

/// The view's state: the browser (search and selection), what has the keys,
/// what the pointer is over, and the list's and the license's scroll.
pub struct Acknowledgements {
    browser: Browser,
    focus: Focus,
    hover: Option<Hit>,
    /// The first list item drawn, as a position in the visible rows.
    list_top: usize,
    /// License lines scrolled past.
    scroll: usize,
    /// The selected entry's license laid out for a width: `(entry, width,
    /// lines)`, kept until either changes (the long tail's runs to
    /// thousands of lines).
    wrapped: Option<(usize, u16, Vec<Line<'static>>)>,
    /// The area last drawn into, for the keys' page size.
    last_area: Rect,
}

impl Acknowledgements {
    /// Opened on the first entry, the list focused, as the desktop's
    /// window opens.
    pub fn new(browser: Browser) -> Self {
        Acknowledgements {
            browser,
            focus: Focus::List,
            hover: None,
            list_top: 0,
            scroll: 0,
            wrapped: None,
            last_area: Rect::default(),
        }
    }

    #[cfg(test)]
    pub fn browser(&self) -> &Browser {
        &self.browser
    }

    #[cfg(test)]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The license scroll, in lines.
    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Search for `query`, as typing it would.
    pub fn set_query(&mut self, query: &str) {
        let before = self.browser.selected();
        self.browser.set_query(query);
        if self.browser.selected() != before {
            self.scroll = 0;
        }
        self.list_top = 0;
        self.reveal();
    }

    /// Type `text` into the search (a paste).
    pub fn paste(&mut self, text: &str) {
        self.focus = Focus::Search;
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let query = format!("{}{text}", self.browser.query());
        self.set_query(&query);
    }

    fn select(&mut self, ix: usize) {
        if self.browser.selected() != Some(ix) {
            self.scroll = 0;
        }
        self.browser.select(ix);
        self.reveal();
    }

    fn step(&mut self, delta: isize) {
        let before = self.browser.selected();
        self.browser.step(delta);
        if self.browser.selected() != before {
            self.scroll = 0;
        }
        self.reveal();
    }

    /// The selection's row to either end of the visible ones.
    fn step_to_end(&mut self, last: bool) {
        let n = self.browser.visible().len() as isize;
        self.step(if last { n } else { -n });
    }

    /// Scroll the list so the selected row shows, in the area last drawn.
    fn reveal(&mut self) {
        let Some(sel) = self.browser.selected() else {
            return;
        };
        let Some(at) = self.browser.visible().iter().position(|i| *i == sel) else {
            return;
        };
        let fits = layout(self.last_area, self.stack()).items_fit().max(1);
        if at < self.list_top {
            self.list_top = at;
        } else if at >= self.list_top + fits {
            self.list_top = at + 1 - fits;
        }
    }

    fn stack(&self) -> bool {
        layout(self.last_area, false).detail.width < STACK_BELOW
    }

    /// Scroll the license `delta` lines, kept within its text.
    fn scroll_by(&mut self, delta: isize) {
        let max = self.max_scroll();
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    /// The furthest the license scrolls: its last line at the bottom.
    fn max_scroll(&mut self) -> usize {
        let plan = layout(self.last_area, self.stack());
        let Some(entry) = self.browser.selected_entry() else {
            return 0;
        };
        let header = detail_header(entry, plan.detail, plan.stack).0.len() as u16;
        let room = plan.detail.height.saturating_sub(header) as usize;
        let total = self.license_lines(plan.detail.width).len();
        total.saturating_sub(room)
    }

    /// A page of license: the rows it shows, less one for context.
    fn page(&self) -> isize {
        let plan = layout(self.last_area, self.stack());
        let header = self
            .browser
            .selected_entry()
            .map_or(0, |e| detail_header(e, plan.detail, plan.stack).0.len() as u16);
        (plan.detail.height.saturating_sub(header) as isize - 1).max(1)
    }

    /// The selected entry's license lines at `width`, laid out once.
    fn license_lines(&mut self, width: u16) -> &[Line<'static>] {
        let Some(ix) = self.browser.selected() else {
            return &[];
        };
        let fresh = matches!(&self.wrapped, Some((i, w, _)) if *i == ix && *w == width);
        if !fresh {
            let entry = &self.browser.entries()[ix];
            self.wrapped = Some((ix, width, license_lines(entry, width as usize)));
        }
        self.wrapped.as_ref().map_or(&[], |(_, _, lines)| lines.as_slice())
    }

    fn step_focus(&mut self, by: isize) {
        let at = ORDER.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        let mut next = ORDER[(at + by).rem_euclid(ORDER.len() as isize) as usize];
        // View source is only there with an entry selected.
        if next == Focus::ViewSource && self.browser.selected().is_none() {
            next = ORDER[(at + 2 * by).rem_euclid(ORDER.len() as isize) as usize];
        }
        self.focus = next;
    }

    fn view_source(&self) -> Step {
        self.browser.selected_entry().map_or(Step::Stay, |e| Step::Open(e.source))
    }

    pub fn handle_key(&mut self, k: KeyEvent) -> Step {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let search_chord = matches!(k.code, KeyCode::Char('f'))
            && (ctrl || k.modifiers.contains(KeyModifiers::SUPER))
            && !k.modifiers.contains(KeyModifiers::ALT);
        if search_chord {
            self.focus = Focus::Search;
            return Step::Stay;
        }
        // The keys every focus shares: the list's arrows, the license's
        // scroll, Tab.
        match k.code {
            KeyCode::Up | KeyCode::Down if shift => {
                self.scroll_by(if k.code == KeyCode::Up { -1 } else { 1 });
                return Step::Stay;
            }
            KeyCode::Up | KeyCode::Down => {
                if self.focus != Focus::Search {
                    self.focus = Focus::List;
                }
                self.step(if k.code == KeyCode::Up { -1 } else { 1 });
                return Step::Stay;
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = self.page();
                self.scroll_by(if k.code == KeyCode::PageUp { -page } else { page });
                return Step::Stay;
            }
            KeyCode::Tab => {
                self.step_focus(1);
                return Step::Stay;
            }
            KeyCode::BackTab => {
                self.step_focus(-1);
                return Step::Stay;
            }
            _ => {}
        }
        match self.focus {
            Focus::Search => self.search_key(k),
            Focus::List => match k.code {
                KeyCode::Esc => Step::Leave,
                KeyCode::Char('/') => {
                    self.focus = Focus::Search;
                    Step::Stay
                }
                KeyCode::Char('k') if !ctrl => {
                    self.step(-1);
                    Step::Stay
                }
                KeyCode::Char('j') if !ctrl => {
                    self.step(1);
                    Step::Stay
                }
                KeyCode::Home => {
                    self.step_to_end(false);
                    Step::Stay
                }
                KeyCode::End => {
                    self.step_to_end(true);
                    Step::Stay
                }
                _ => Step::Stay,
            },
            Focus::ViewSource | Focus::Done => match k.code {
                KeyCode::Esc => Step::Leave,
                KeyCode::Char('/') => {
                    self.focus = Focus::Search;
                    Step::Stay
                }
                KeyCode::Enter | KeyCode::Char(' ') if self.focus == Focus::Done => Step::Leave,
                KeyCode::Enter | KeyCode::Char(' ') => self.view_source(),
                _ => Step::Stay,
            },
        }
    }

    /// A key while the search has the keys: typing edits it, Enter hands
    /// the keys to the list, Esc clears it or, empty, does the same.
    fn search_key(&mut self, k: KeyEvent) -> Step {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc if !self.browser.query().is_empty() => self.set_query(""),
            KeyCode::Esc | KeyCode::Enter => self.focus = Focus::List,
            KeyCode::Backspace => {
                let mut query = self.browser.query().to_string();
                query.pop();
                self.set_query(&query);
            }
            KeyCode::Char('u') if ctrl => self.set_query(""),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => {
                let query = format!("{}{c}", self.browser.query());
                self.set_query(&query);
            }
            _ => {}
        }
        Step::Stay
    }

    /// A mouse event over `area` (the full window): a move tracks the
    /// hover, a click selects a row, focuses the search, clears it or
    /// presses a button, and the wheel scrolls the list or the license,
    /// whichever it's over.
    pub fn handle_mouse(&mut self, m: MouseEvent, area: Rect) -> Step {
        self.last_area = area;
        let plan = self.plan(area);
        let hit = plan.hit(m.column, m.row);
        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = hit;
                Step::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(Hit::Row(ix)) => {
                    self.focus = Focus::List;
                    self.select(ix);
                    Step::Stay
                }
                Some(Hit::Search) => {
                    self.focus = Focus::Search;
                    Step::Stay
                }
                Some(Hit::Clear) => {
                    self.focus = Focus::Search;
                    self.set_query("");
                    Step::Stay
                }
                Some(Hit::ViewSource) => {
                    self.focus = Focus::ViewSource;
                    self.view_source()
                }
                Some(Hit::Done) => Step::Leave,
                None => Step::Stay,
            },
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = m.kind == MouseEventKind::ScrollDown;
                if contains(plan.list, m.column, m.row) {
                    let max = self.browser.visible().len().saturating_sub(plan.items_fit());
                    self.list_top = if down { (self.list_top + 1).min(max) } else { self.list_top.saturating_sub(1) };
                } else if contains(plan.detail, m.column, m.row) {
                    self.scroll_by(if down { WHEEL_LINES as isize } else { -(WHEEL_LINES as isize) });
                }
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    /// The layout for `area`, with the list's rows placed.
    fn plan(&self, area: Rect) -> Layout {
        let mut plan = layout(area, layout(area, false).detail.width < STACK_BELOW);
        let visible = self.browser.visible();
        let fits = plan.items_fit();
        plan.rows = visible
            .iter()
            .skip(self.list_top)
            .take(fits)
            .enumerate()
            .map(|(p, ix)| (plan.list.y + 1 + p as u16 * ITEM_ROWS, *ix))
            .collect();
        if let Some(entry) = self.browser.selected_entry() {
            plan.view_source = Some(detail_header(entry, plan.detail, plan.stack).1);
        }
        plan
    }

    /// Paint the view over `area` (the full window). Returns where the
    /// terminal's cursor goes: the search's caret while it has the keys.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) -> Option<(u16, u16)> {
        self.last_area = area;
        // Keep the scroll in range for this size (a resize, a new entry).
        let max = self.max_scroll();
        self.scroll = self.scroll.min(max);
        let fits = layout(area, self.stack()).items_fit();
        let visible_n = self.browser.visible().len();
        self.list_top = self.list_top.min(visible_n.saturating_sub(fits.max(1)));
        let plan = self.plan(area);

        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].reset();
            }
        }
        if area.width == 0 || area.height == 0 {
            return None;
        }

        // The title row: the title, Done at the right.
        if let Some(title) = plan.title {
            let style = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
            let room = plan.done.map_or(area.right(), |d| d.x.saturating_sub(1)).saturating_sub(title.x);
            buf.set_stringn(title.x, title.y, TITLE, room as usize, style);
        }
        if let Some(done) = plan.done {
            let look = self.button_look(Hit::Done);
            draw_button(buf, done, DONE, look);
        }

        // The rule between the list and the detail.
        if let Some(x) = plan.divider_x {
            for y in plan.list_top_y..area.bottom() {
                buf[(x, y)].set_symbol("│").set_style(Style::default().fg(palette().divider));
            }
        }

        let cursor = self.render_search(buf, &plan);
        self.render_list(buf, &plan);
        self.render_detail(buf, &plan);
        cursor
    }

    fn render_search(&self, buf: &mut Buffer, plan: &Layout) -> Option<(u16, u16)> {
        let rect = plan.search?;
        let focused = self.focus == Focus::Search;
        let fill = if focused { palette().input_focus } else { palette().search };
        let caps = Style::default().fg(fill);
        buf.set_string(rect.x, rect.y, BLEED_ABOVE.repeat(rect.width as usize), caps);
        buf.set_string(rect.x, rect.y + 2, BLEED_BELOW.repeat(rect.width as usize), caps);
        let y = rect.y + 1;
        let base = Style::default().bg(fill);
        for x in rect.left()..rect.right() {
            buf[(x, y)].set_symbol(" ").set_style(base);
        }
        let query = self.browser.query();
        let glyph_w = SEARCH_GLYPH.width() as u16;
        buf.set_stringn(rect.x, y, SEARCH_GLYPH, rect.width as usize, base.fg(palette().gray));
        let text_x = rect.x + glyph_w.min(rect.width);
        let clear = (!query.is_empty()).then(|| clear_rect(rect));
        let text_end = clear.map_or(rect.right(), |c| c.x.saturating_sub(1));
        let room = text_end.saturating_sub(text_x) as usize;
        let caret = if query.is_empty() {
            buf.set_stringn(text_x, y, truncate_right(PLACEHOLDER, room), room, base.fg(palette().gray));
            (text_x, y)
        } else {
            // A query longer than the box shows its end, where the caret is.
            let shown = tail_to_width(query, room.saturating_sub(1));
            buf.set_stringn(text_x, y, &shown, room, base.fg(palette().white));
            (text_x + shown.width() as u16, y)
        };
        if let Some(c) = clear {
            let hovered = self.hover == Some(Hit::Clear);
            let fg = if hovered { palette().white } else { palette().gray };
            buf.set_string(c.x, c.y, CLEAR, base.fg(fg));
        }
        // A double-width glyph (the 🔍) clears its trailing cell's fill as
        // it is laid down: paint the row's fill over every cell again.
        for x in rect.left()..rect.right() {
            buf[(x, y)].set_bg(fill);
        }
        Some(caret).filter(|(x, _)| focused && *x < rect.right())
    }

    fn render_list(&self, buf: &mut Buffer, plan: &Layout) {
        let list = plan.list;
        if list.width < 2 || plan.rows.is_empty() {
            return;
        }
        let selected = self.browser.selected();
        let fill_of = |ix: usize| {
            if Some(ix) == selected {
                Some(palette().selection)
            } else if self.hover == Some(Hit::Row(ix)) {
                Some(palette().row_hover)
            } else {
                None
            }
        };
        let width = list.width as usize;
        let entries = self.browser.entries();
        let mut above = None;
        for (y, ix) in &plan.rows {
            let fill = fill_of(*ix);
            draw_line(buf, list.x, y - 1, list.width, &bleed_separator(above, fill, width));
            let entry = &entries[*ix];
            let base = fill.map_or(Style::default(), |f| Style::default().bg(f));
            // On a fill the license reads in gray: muted is too faint there.
            let (name_fg, license_fg) = match fill {
                Some(_) => (palette().white, palette().gray),
                None => (palette().foreground, palette().muted),
            };
            let name_style = if Some(*ix) == selected {
                base.fg(name_fg).add_modifier(Modifier::BOLD)
            } else {
                base.fg(name_fg)
            };
            let text_w = width.saturating_sub(2);
            for (dy, text, style) in [
                (0, entry.name, name_style),
                (1, entry.license, base.fg(license_fg)),
            ] {
                let row = y + dy;
                if row >= list.bottom() {
                    break;
                }
                for x in list.left()..list.right() {
                    buf[(x, row)].set_symbol(" ").set_style(base);
                }
                buf.set_stringn(list.x + 1, row, truncate_right(text, text_w), text_w, style);
            }
            above = fill;
        }
        if let Some((y, _)) = plan.rows.last() {
            let below = y + 2;
            if below < list.bottom() {
                draw_line(buf, list.x, below, list.width, &bleed_separator(above, None, width));
            }
        }
    }

    fn render_detail(&mut self, buf: &mut Buffer, plan: &Layout) {
        let rect = plan.detail;
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let Some(entry) = self.browser.selected_entry() else {
            let y = rect.y + rect.height / 2;
            let x = rect.x + rect.width.saturating_sub(NO_MATCH.width() as u16) / 2;
            buf.set_stringn(x, y, NO_MATCH, rect.width as usize, Style::default().fg(palette().muted));
            return;
        };
        let (header, _) = detail_header(entry, rect, plan.stack);
        for (i, line) in header.iter().enumerate() {
            let y = rect.y + i as u16;
            if y >= rect.bottom() {
                return;
            }
            draw_line(buf, rect.x, y, rect.width, line);
        }
        if let Some(button) = plan.view_source.filter(|b| b.bottom() <= rect.bottom()) {
            let look = self.button_look(Hit::ViewSource);
            draw_button(buf, button, VIEW_SOURCE, look);
        }
        let top = rect.y + header.len() as u16;
        let room = rect.bottom().saturating_sub(top) as usize;
        let scroll = self.scroll;
        let lines = self.license_lines(rect.width);
        for (i, line) in lines.iter().skip(scroll).take(room).enumerate() {
            draw_line(buf, rect.x, top + i as u16, rect.width, line);
        }
    }

    /// How a button draws: its kind, focus and hover.
    fn button_look(&self, which: Hit) -> Look {
        let focus = match which {
            Hit::Done => Focus::Done,
            _ => Focus::ViewSource,
        };
        Look {
            primary: which == Hit::Done,
            focused: self.focus == focus,
            hovered: self.hover == Some(which),
        }
    }
}

/// A button's state as it draws.
#[derive(Debug, Clone, Copy)]
struct Look {
    primary: bool,
    focused: bool,
    hovered: bool,
}

/// A button as Settings' draw: Done the Primary (the desktop's), View
/// source Secondary; its focus, and its lift under the pointer this view
/// tracks.
fn draw_button(buf: &mut Buffer, rect: Rect, label: &str, look: Look) {
    let button = if look.primary { Button::primary(label) } else { Button::secondary(label) };
    button.focused(look.focused).hovered(look.hovered).draw(buf, rect);
}

/// A button's columns: its label with two of fill each side.
fn button_w(label: &str) -> u16 {
    label.width() as u16 + 4
}

/// The detail's fixed top, over the license, and where View source goes
/// in it: the name and attribution (View source beside them, its label on
/// the name's row; or, when the pane is narrow, on rows of its own under
/// the description), the description, a rule, a blank line. The button's
/// rows are left blank for it.
fn detail_header(entry: &Entry, rect: Rect, stack: bool) -> (Vec<Line<'static>>, Rect) {
    let width = rect.width as usize;
    let button_w = button_w(VIEW_SOURCE).min(rect.width);
    let beside = if stack { 0 } else { button_w as usize + 2 };
    let ident_w = width.saturating_sub(beside).max(1);
    let mut out = vec![Line::raw("")];
    out.push(Line::from(Span::styled(
        truncate_right(entry.name, ident_w),
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
    )));
    for line in wrap(&entry.attribution, ident_w) {
        out.push(Line::from(Span::styled(line, Style::default().fg(palette().foreground))));
    }
    // Beside, the button's bottom half-row needs the row under the name's.
    if out.len() < 3 {
        out.push(Line::raw(""));
    }
    let mut button = Rect {
        x: rect.right().saturating_sub(button_w),
        y: rect.y,
        width: button_w,
        height: 3,
    };
    out.push(Line::raw(""));
    for line in wrap(entry.description, width.max(1)) {
        out.push(Line::from(Span::styled(line, Style::default().fg(palette().foreground))));
    }
    if stack {
        out.push(Line::raw(""));
        button = Rect {
            x: rect.x,
            y: rect.y + out.len() as u16,
            ..button
        };
        out.extend([Line::raw(""), Line::raw(""), Line::raw("")]);
    }
    out.push(Line::raw(""));
    out.push(Line::from(Span::styled("─".repeat(width), Style::default().fg(palette().divider))));
    out.push(Line::raw(""));
    (out, button)
}

/// The selected entry's license as lines `width` wide: its name (loud),
/// a blank, then its blocks a blank line apart, a section heading bold, a
/// paragraph's lines each wrapped.
fn license_lines(entry: &Entry, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out = vec![
        Line::from(Span::styled(
            entry.license_title.to_string(),
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    let body = Style::default().fg(palette().foreground);
    for (i, block) in license_blocks(entry.text).into_iter().enumerate() {
        if i > 0 {
            out.push(Line::raw(""));
        }
        match block {
            Block::Heading(text) => {
                for line in wrap(&text, width) {
                    out.push(Line::from(Span::styled(line, body.add_modifier(Modifier::BOLD))));
                }
            }
            Block::Paragraph(lines) => {
                for line in lines {
                    for part in wrap(&line, width) {
                        out.push(Line::from(Span::styled(part, body)));
                    }
                }
            }
        }
    }
    out
}

/// The end of `s` that fits `width` columns.
fn tail_to_width(s: &str, width: usize) -> String {
    let mut out: Vec<char> = Vec::new();
    let mut w = 0;
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

/// The clear button's cell: one column in from the box's right edge.
fn clear_rect(search: Rect) -> Rect {
    Rect {
        x: search.right().saturating_sub(2),
        y: search.y + 1,
        width: 1,
        height: 1,
    }
}

fn draw_line(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &Line<'_>) {
    buf.set_line(x, y, line, width);
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

// ---------------------------------------------------------------------------
// Layout: one pure function of the window, shared by drawing and clicking.

/// Where everything lands for one window size.
#[derive(Debug, Clone, Default)]
struct Layout {
    /// The title's cell, when the window has a row for it.
    title: Option<Rect>,
    /// Done's three rows.
    done: Option<Rect>,
    /// The search box's three rows.
    search: Option<Rect>,
    /// The list's rows, under the search.
    list: Rect,
    /// Each drawn list item's name row, and its entry.
    rows: Vec<(u16, usize)>,
    /// The column of the rule between the list and the detail.
    divider_x: Option<u16>,
    /// The rule's (and the panes') first row.
    list_top_y: u16,
    /// The detail pane.
    detail: Rect,
    /// Whether View source stacks under the description.
    stack: bool,
    /// View source's three rows, with an entry selected.
    view_source: Option<Rect>,
}

impl Layout {
    /// How many items the list shows: three rows each, a separator over
    /// the first.
    fn items_fit(&self) -> usize {
        (self.list.height.saturating_sub(1) / ITEM_ROWS) as usize
    }

    fn hit(&self, col: u16, row: u16) -> Option<Hit> {
        let within = |r: Option<Rect>| r.is_some_and(|r| contains(r, col, row));
        if within(self.done) {
            return Some(Hit::Done);
        }
        if within(self.view_source) {
            return Some(Hit::ViewSource);
        }
        if let Some(search) = self.search {
            if contains(search, col, row) {
                let clear = clear_rect(search);
                return Some(if row == clear.y && (clear.x..=clear.x + 1).contains(&col) {
                    Hit::Clear
                } else {
                    Hit::Search
                });
            }
        }
        if contains(self.list, col, row) {
            // An item's name and license rows, and the separator under it.
            return self
                .rows
                .iter()
                .find(|(y, _)| (*y..*y + 2).contains(&row))
                .map(|(_, ix)| Hit::Row(*ix));
        }
        None
    }
}

/// The list's column for a window `width` wide: about the desktop's 280 of
/// 1000, at least wide enough for a name, leaving the detail most.
fn list_width(width: u16) -> u16 {
    if width < 44 {
        return width / 2;
    }
    (width * 3 / 10).clamp(22, 40)
}

fn layout(area: Rect, stack: bool) -> Layout {
    let mut plan = Layout {
        stack,
        ..Layout::default()
    };
    if area.width == 0 || area.height == 0 {
        return plan;
    }
    // The title row: three rows (Done's half rows above and below), the
    // title on the middle one.
    let header_h = 3.min(area.height);
    let done_w = button_w(DONE);
    if area.height >= 3 {
        plan.title = Some(Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: 1,
        });
        if area.width >= done_w + TITLE.len() as u16 + 6 {
            plan.done = Some(Rect {
                x: area.right() - 2 - done_w,
                y: area.y,
                width: done_w,
                height: 3,
            });
        }
    }
    let body_y = area.y + header_h;
    let body_h = area.bottom().saturating_sub(body_y);
    plan.list_top_y = body_y;
    let list_w = list_width(area.width);
    plan.divider_x = (area.width > list_w + 2).then_some(area.x + list_w);
    // The left column, one column in from either side.
    let col = Rect {
        x: area.x + 1,
        y: body_y,
        width: list_w.saturating_sub(2),
        height: body_h,
    };
    if body_h >= 3 {
        plan.search = Some(Rect { height: 3, ..col });
    }
    plan.list = Rect {
        y: body_y + 3.min(body_h),
        height: body_h.saturating_sub(3),
        ..col
    };
    // The detail: two columns in from the rule, two from the window's edge.
    if let Some(x) = plan.divider_x {
        let left = x + 3;
        plan.detail = Rect {
            x: left,
            y: body_y,
            width: area.right().saturating_sub(2).saturating_sub(left),
            height: body_h,
        };
    }
    plan
}

#[cfg(test)]
mod tests;
