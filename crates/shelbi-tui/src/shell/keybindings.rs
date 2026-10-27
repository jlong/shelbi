//! Settings ▸ Keybindings in the TUI (`tui-settings-keybindings`), the
//! desktop app's bindings editor over the shared
//! `shelbi_app::settings::keybindings` state: the same `keys.yaml`, the same
//! listing, filters, capture and conflict rules, and the same edits.
//!
//! The page, top to bottom: the title and helper, the search, the context
//! filter, Reset all and Open keys.yaml, then one group per keymap context
//! with a row per action (its label and its chords, `or` between them). A
//! customized row has a `●` in its gutter; an unbound one reads `None`.
//!
//! Everything is reachable from the keyboard (the desktop app's hover
//! buttons are row keys here). Up / down (or the sidebar's nav keys) move
//! between the controls and the rows; on a row, left / right pick one of its
//! chords or the `+` after them, and:
//!
//! - Enter changes the picked chord (or adds one on `+`), `a` adds one;
//! - Backspace / Delete removes the picked chord; `u` resets the row;
//! - `/` searches, `c` steps the context filter.
//!
//! A capture takes every key ahead of the shell's own (the palette chord,
//! the focus moves, a session): Esc cancels, Backspace / Delete removes the
//! picked chord, any other chord replaces it. A chord another action in the
//! context has is named on the row and needs pressing again to move it.
//! What a capture records is the chord the terminal reported: on a terminal
//! without the kitty keyboard protocol Ctrl+H arrives as Backspace (so it
//! removes), Ctrl+I as Tab, Ctrl+M as Enter. A listed chord this terminal
//! can't send (⌘ chords bound in the desktop app, say) is marked `⚠`, and the
//! picked one says why under its row.
//!
//! Settings' own keys stay fixed whatever is rebound: the arrows, Enter, Tab
//! and Esc always move, open and leave, and Esc always ends a capture (it can
//! never be bound by one), so a changed navigation binding can't strand you.
//!
//! Saves run off the UI thread, one at a time; the rows (and the shell's
//! bindings) take a change only once it is written.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use shelbi_app::settings::keybindings::{Change, KeyOutcome, KeybindingsState, Row, Slot};
use shelbi_app::settings::{Job, Ticket};
use shelbi_state::keymap::{
    format_chord, Action, DisplayStyle, Key, KeyChord, Keymaps, Mods, SidebarAction,
};

use crate::panel;
use crate::theme::palette;

/// A place the page's keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Search,
    Context,
    ResetAll,
    OpenKeys,
    Row(Action),
}

/// What a click at a recorded rect hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Stop(Stop),
    /// A row's chord, by its place in the row's shown order.
    Chord(Action, usize),
    /// A row's `+`.
    Add(Action),
}

/// What the shell does after a page key or click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum KeysAction {
    Stay,
    /// Write this change to `keys.yaml` (off the UI thread), then hand the
    /// result to [`KeysPage::saved`].
    Save(Change),
    /// Open `keys.yaml` in the user's editor.
    OpenFile,
}

/// How the TUI spells a chord (its display style, as every hint does).
fn spell(chord: &KeyChord) -> String {
    format_chord(chord, DisplayStyle::detect())
}

/// Why this terminal can't send `chord` as bound, if it can't. `kitty` is
/// whether the kitty keyboard protocol round-trips (without it, several
/// chords arrive as other keys or lose a modifier).
pub(crate) fn chord_note(chord: &KeyChord, kitty: bool) -> Option<&'static str> {
    let m = chord.mods;
    if m.contains(Mods::CONTROL) && matches!(chord.code, Key::Char(' ') | Key::Char('@')) {
        return Some("the TUI keeps this for the command palette");
    }
    if kitty {
        return None;
    }
    if m.contains(Mods::SUPER) {
        return Some("this terminal doesn't send ⌘ / Super keys");
    }
    if m == Mods::CONTROL {
        match chord.code {
            Key::Char('h') => return Some("arrives as Backspace in this terminal"),
            Key::Char('i') => return Some("arrives as Tab in this terminal"),
            Key::Char('m') | Key::Char('j') => return Some("arrives as Enter in this terminal"),
            Key::Char('[') => return Some("arrives as Esc in this terminal"),
            _ => {}
        }
    }
    if m.contains(Mods::CONTROL | Mods::SHIFT) && matches!(chord.code, Key::Char(c) if c.is_ascii_alphabetic()) {
        return Some("this terminal drops Shift from Ctrl chords");
    }
    let named = matches!(chord.code, Key::Enter | Key::Tab | Key::Backspace | Key::Esc | Key::Char(' '));
    if named && m.intersects(Mods::CONTROL | Mods::SHIFT) {
        return Some("this terminal sends it without the modifier");
    }
    None
}

/// The Keybindings page.
pub(crate) struct KeysPage {
    state: KeybindingsState,
    stop: Stop,
    /// The picked place in the focused row: an index into its shown chords,
    /// or one past them for the `+`.
    slot: usize,
    /// The search field has the keyboard.
    editing: bool,
    scroll: u16,
    /// The last read, save or open that failed.
    error: Option<String>,
    /// The last read of `keys.yaml`.
    load: Job<String>,
    /// A read asked for while a save was out: run it once the save lands.
    reload_after_save: bool,
    /// The kitty keyboard protocol round-trips (see [`chord_note`]).
    kitty: bool,
    hits: Vec<(Rect, Hit)>,
}

impl KeysPage {
    pub(crate) fn new(project: Option<String>) -> Self {
        KeysPage {
            state: KeybindingsState::new(project, spell),
            stop: Stop::Search,
            slot: 0,
            editing: false,
            scroll: 0,
            error: None,
            load: Job::default(),
            reload_after_save: false,
            kitty: false,
            hits: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &KeybindingsState {
        &self.state
    }

    /// The bindings in effect: what the shell runs with.
    pub(crate) fn keymaps(&self) -> &Keymaps {
        self.state.keymaps()
    }

    pub(crate) fn set_kitty(&mut self, kitty: bool) {
        self.kitty = kitty;
    }

    #[cfg(test)]
    pub(crate) fn stop(&self) -> Stop {
        self.stop
    }

    #[cfg(test)]
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// A read or a save is out.
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        self.state.saving() || self.load.running()
    }

    #[cfg(test)]
    pub(crate) fn editing(&self) -> bool {
        self.editing
    }

    /// Take `text` (a `keys.yaml`) as the bindings in effect: the shell's
    /// first read at launch. Returns whether it was taken (one that doesn't
    /// parse is refused, and the built-in bindings stay).
    pub(crate) fn seed(&mut self, text: String) -> bool {
        match self.state.load(text) {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("{e}. Fix it in keys.yaml."));
                false
            }
        }
    }

    /// The shell switched project: show (and run with) its bindings.
    pub(crate) fn set_project(&mut self, project: Option<String>) {
        self.state.set_project(project);
        self.editing = false;
    }

    /// The page is leaving the screen: an open capture or search edit ends.
    pub(crate) fn blur(&mut self) {
        self.state.cancel();
        self.editing = false;
    }

    /// Whether a key should reach the page before the shell's own chords (the
    /// palette, the focus moves): every key while capturing, and plain typing
    /// while the search has the keyboard.
    pub(crate) fn takes_key_first(&self, k: &KeyEvent) -> bool {
        if self.state.capture().is_some() {
            return true;
        }
        self.editing && !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    }

    /// Whether every key is the page's (Settings' own Esc, Tab and digits
    /// included).
    pub(crate) fn holds_keys(&self) -> bool {
        self.state.capture().is_some() || self.editing
    }

    // --- loading and saving ---------------------------------------------

    /// Start a read of `keys.yaml`: a ticket the shell's reader carries, or
    /// `None` while one is running (or a save is out; it runs after).
    pub(crate) fn begin_load(&mut self) -> Option<Ticket> {
        if self.state.saving() {
            self.reload_after_save = true;
            return None;
        }
        if self.load.running() {
            return None;
        }
        Some(self.load.begin(None))
    }

    /// A read landed. Returns whether the bindings in effect changed.
    pub(crate) fn loaded(&mut self, ticket: &Ticket, result: Result<String, String>) -> bool {
        if !self.load.complete(ticket, result.clone()) {
            return false;
        }
        let text = match result {
            Ok(text) => text,
            Err(e) => {
                self.error = Some(e);
                return false;
            }
        };
        if text == self.state.text() {
            // What runs is what's on disk: an old complaint no longer holds.
            self.error = None;
            return false;
        }
        match self.state.load(text) {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(format!("{e}. Your bindings stay as they were until it's fixed."));
                false
            }
        }
    }

    /// A save landed. Returns whether the bindings in effect changed, and
    /// whether to read `keys.yaml` again (one was asked for meanwhile).
    pub(crate) fn saved(&mut self, result: Result<String, String>) -> (bool, bool) {
        let reload = std::mem::take(&mut self.reload_after_save);
        match self.state.saved(result) {
            Ok(()) => {
                self.error = None;
                (true, reload)
            }
            Err(e) => {
                self.error = Some(format!("Couldn't save keys.yaml: {e}"));
                (false, reload)
            }
        }
    }

    /// Opening `keys.yaml` failed.
    pub(crate) fn open_failed(&mut self, e: String) {
        self.error = Some(e);
    }

    fn settle(&mut self, outcome: KeyOutcome) -> KeysAction {
        match outcome {
            KeyOutcome::Ignored | KeyOutcome::Cancelled | KeyOutcome::Conflict => KeysAction::Stay,
            KeyOutcome::Save(change) => {
                self.error = None;
                KeysAction::Save(change)
            }
            KeyOutcome::Failed(e) => {
                self.error = Some(format!("{e}. Fix it in keys.yaml."));
                KeysAction::Stay
            }
        }
    }

    // --- the stops --------------------------------------------------------

    fn rows(&self) -> Vec<Row> {
        self.state.groups().into_iter().flat_map(|g| g.rows).collect()
    }

    fn stops(&self) -> Vec<Stop> {
        let mut out = vec![Stop::Search, Stop::Context, Stop::ResetAll, Stop::OpenKeys];
        out.extend(self.rows().iter().map(|r| Stop::Row(r.action)));
        out
    }

    /// Keep the focus on a stop that is still listed (a filter can hide
    /// its row) and the picked chord inside its row.
    fn clamp(&mut self) {
        let stops = self.stops();
        if !stops.contains(&self.stop) {
            self.stop = stops.get(4).copied().unwrap_or(Stop::Search);
            self.slot = 0;
        }
        if let Stop::Row(action) = self.stop {
            let n = self.state.chords(action).len();
            self.slot = self.slot.min(n);
        }
    }

    fn step(&mut self, by: isize) {
        let stops = self.stops();
        let at = stops.iter().position(|s| *s == self.stop).unwrap_or(0) as isize;
        let next = (at + by).clamp(0, stops.len() as isize - 1) as usize;
        if stops[next] != self.stop {
            self.stop = stops[next];
            self.slot = 0;
        }
    }

    /// The picked row's capture slot.
    fn picked(&self, action: Action) -> Slot {
        let row = self.state.row(action);
        match row.shown().get(self.slot) {
            Some((at, _)) => Slot::Chord(*at),
            None => Slot::New,
        }
    }

    fn capture(&mut self, action: Action, slot: Slot) {
        self.editing = false;
        self.state.start_capture(action, slot);
    }

    // --- input -------------------------------------------------------------

    /// A key while the page has focus (after Settings took its own, unless
    /// [`Self::holds_keys`]).
    pub(crate) fn handle_key(&mut self, k: KeyEvent, keymaps: &Keymaps) -> KeysAction {
        self.clamp();
        let chord = crate::keymap::chord_from_event(k);
        if self.state.capture().is_some() {
            // A key outside the chord vocabulary can't be bound: ignore it.
            let Some(chord) = chord else {
                return KeysAction::Stay;
            };
            let outcome = self.state.key(chord);
            return self.settle(outcome);
        }
        if self.editing {
            self.search_key(k);
            return KeysAction::Stay;
        }
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let sidebar = chord.and_then(|c| keymaps.sidebar.dispatch(c));
        let up = k.code == KeyCode::Up || sidebar == Some(SidebarAction::NavUp);
        let down = k.code == KeyCode::Down || sidebar == Some(SidebarAction::NavDown);
        if up || down {
            self.step(if down { 1 } else { -1 });
            return KeysAction::Stay;
        }
        // Typing on the search field starts a search.
        if let (Stop::Search, KeyCode::Char(c)) = (self.stop, k.code) {
            if plain {
                self.editing = true;
                if c != '/' {
                    self.state.query.push(c);
                }
                self.clamp();
                return KeysAction::Stay;
            }
        }
        match k.code {
            KeyCode::PageUp => self.step(-8),
            KeyCode::PageDown => self.step(8),
            KeyCode::Home => self.step(isize::MIN / 2),
            KeyCode::End => self.step(isize::MAX / 2),
            KeyCode::Char('/') if plain => {
                self.stop = Stop::Search;
                self.editing = true;
            }
            KeyCode::Char('c') if plain => self.state.cycle_context(true),
            _ => return self.stop_key(k, sidebar),
        }
        self.clamp();
        KeysAction::Stay
    }

    /// A key on the focused stop.
    fn stop_key(&mut self, k: KeyEvent, sidebar: Option<SidebarAction>) -> KeysAction {
        let activate = k.code == KeyCode::Enter || sidebar == Some(SidebarAction::Activate);
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        match self.stop {
            Stop::Search => {
                if activate {
                    self.editing = true;
                }
                KeysAction::Stay
            }
            Stop::Context => {
                match k.code {
                    KeyCode::Left => self.state.cycle_context(false),
                    KeyCode::Right => self.state.cycle_context(true),
                    _ if activate => self.state.cycle_context(true),
                    _ => {}
                }
                self.clamp();
                KeysAction::Stay
            }
            Stop::ResetAll => match k.code {
                KeyCode::Right => {
                    self.stop = Stop::OpenKeys;
                    KeysAction::Stay
                }
                _ if activate && self.state.any_customized() => {
                    let outcome = self.state.reset_all();
                    self.settle(outcome)
                }
                _ => KeysAction::Stay,
            },
            Stop::OpenKeys => match k.code {
                KeyCode::Left => {
                    self.stop = Stop::ResetAll;
                    KeysAction::Stay
                }
                _ if activate => KeysAction::OpenFile,
                _ => KeysAction::Stay,
            },
            Stop::Row(action) => self.row_key(k, action, activate, plain),
        }
    }

    fn row_key(&mut self, k: KeyEvent, action: Action, activate: bool, plain: bool) -> KeysAction {
        let n = self.state.chords(action).len();
        match k.code {
            KeyCode::Left => self.slot = self.slot.saturating_sub(1),
            KeyCode::Right => self.slot = (self.slot + 1).min(n),
            KeyCode::Backspace | KeyCode::Delete => {
                let row = self.state.row(action);
                if let Some((at, _)) = row.shown().get(self.slot) {
                    let outcome = self.state.remove(action, *at);
                    return self.settle(outcome);
                }
            }
            KeyCode::Char('a') | KeyCode::Char('+') if plain => {
                self.slot = n;
                self.capture(action, Slot::New);
            }
            KeyCode::Char('u') if plain => {
                if self.state.row(action).customized {
                    let outcome = self.state.reset(action);
                    return self.settle(outcome);
                }
            }
            _ if activate => {
                let slot = self.picked(action);
                self.capture(action, slot);
            }
            _ => {}
        }
        KeysAction::Stay
    }

    /// A key while the search has the keyboard: typing filters live; Esc
    /// clears it (then gives the keyboard back); Enter, Tab or ↓ go to the
    /// first match, ↑ back to the controls.
    fn search_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => {
                if self.state.query.is_empty() {
                    self.editing = false;
                } else {
                    self.state.query.clear();
                }
            }
            KeyCode::Backspace => {
                self.state.query.pop();
            }
            KeyCode::Char('u') if ctrl => self.state.query.clear(),
            KeyCode::Char(c) if !ctrl && !k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER) => {
                self.state.query.push(c)
            }
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                self.editing = false;
                self.stop = self.stops().get(4).copied().unwrap_or(Stop::Search);
                self.slot = 0;
            }
            KeyCode::Up | KeyCode::BackTab => self.editing = false,
            _ => {}
        }
        self.clamp();
    }

    /// A paste while the search has the keyboard goes into it. Returns
    /// whether it was taken.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        if !self.editing {
            return false;
        }
        self.state.query.extend(text.chars().filter(|c| !c.is_control()));
        self.clamp();
        true
    }

    /// A mouse event over the page.
    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) -> KeysAction {
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                KeysAction::Stay
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(1);
                KeysAction::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(r, _)| panel::in_rect(*r, m.column, m.row))
                    .map(|(_, h)| *h);
                // A click anywhere ends a capture (on a chord, it starts anew).
                self.state.cancel();
                self.editing = false;
                match hit {
                    Some(Hit::Chord(action, shown)) => {
                        self.stop = Stop::Row(action);
                        self.slot = shown;
                        let slot = self.picked(action);
                        self.capture(action, slot);
                        KeysAction::Stay
                    }
                    Some(Hit::Add(action)) => {
                        self.stop = Stop::Row(action);
                        self.slot = self.state.chords(action).len();
                        self.capture(action, Slot::New);
                        KeysAction::Stay
                    }
                    Some(Hit::Stop(stop)) => {
                        self.stop = stop;
                        self.slot = 0;
                        match stop {
                            Stop::Search => {
                                self.editing = true;
                                KeysAction::Stay
                            }
                            Stop::Context => {
                                self.state.cycle_context(true);
                                self.clamp();
                                KeysAction::Stay
                            }
                            Stop::ResetAll if self.state.any_customized() => {
                                let outcome = self.state.reset_all();
                                self.settle(outcome)
                            }
                            Stop::OpenKeys => KeysAction::OpenFile,
                            Stop::ResetAll | Stop::Row(_) => KeysAction::Stay,
                        }
                    }
                    None => KeysAction::Stay,
                }
            }
            _ => KeysAction::Stay,
        }
    }

    // --- drawing -------------------------------------------------------------

    /// The key hints for the page, by what has the keyboard.
    pub(crate) fn hints(&self) -> String {
        if self.state.capture().is_some() {
            return "Press a shortcut · Esc cancel · ⌫ remove".into();
        }
        if self.editing {
            return "Type to filter · ⏎ results · Esc clear".into();
        }
        // Short enough for the page at 80 columns.
        match self.stop {
            Stop::Row(_) => "←→ pick · ⏎ change · a add · ⌫ remove · u reset".into(),
            Stop::Context => "←→ context · ⏎ next · / search · Esc back".into(),
            _ => "↑↓ move · ⏎ choose · / search · Esc back".into(),
        }
    }

    /// Draw the page into `area` (the main area's content rect). `focused`
    /// is whether the page has the keyboard.
    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        self.hits.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        self.clamp();
        let width = area.width as usize;
        let lines = self.lines(width, focused);
        // Keep the focused stop (and the lines under its row) in view.
        let height = area.height as usize;
        if let Some(at) = lines.iter().position(|l| l.stop == Some(self.stop)) {
            let end = lines[at..]
                .iter()
                .skip(1)
                .take_while(|l| l.stop.is_none() && l.under_row)
                .count()
                + at;
            let scroll = self.scroll as usize;
            if at < scroll {
                self.scroll = at as u16;
            } else if end >= scroll + height {
                self.scroll = (end + 1).saturating_sub(height).min(at) as u16;
            }
        }
        let max = lines.len().saturating_sub(height) as u16;
        self.scroll = self.scroll.min(max);
        let clipped_below = self.scroll < max;
        for (i, line) in lines.iter().skip(self.scroll as usize).take(height).enumerate() {
            let y = area.y + i as u16;
            let rect = Rect { y, height: 1, ..area };
            if i + 1 == height && clipped_below {
                f.render_widget(
                    Paragraph::new(Span::styled("↓ more", Style::default().fg(palette().muted))),
                    rect,
                );
                continue;
            }
            f.render_widget(Paragraph::new(line.line.clone()), rect);
            for (x0, x1, hit) in &line.hits {
                let x = area.x + *x0;
                let w = x1.saturating_sub(*x0).min(area.right().saturating_sub(x));
                if w > 0 {
                    self.hits.push((Rect { x, y, width: w, height: 1 }, *hit));
                }
            }
        }
    }

    /// Every line of the page at `width`.
    fn lines(&self, width: usize, focused: bool) -> Vec<PageLine> {
        let mut out = Vec::new();
        out.push(PageLine::text(Line::from(Span::styled(
            "Keybindings",
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        ))));
        wrapped(
            &mut out,
            "Pick a shortcut and press ⏎ to change it. Shared with the desktop app.",
            width,
            Style::default().fg(palette().muted),
        );
        if let Some(e) = &self.error {
            wrapped(&mut out, e, width, Style::default().fg(palette().red));
        }
        if self.state.saving() {
            out.push(PageLine::text(Line::from(Span::styled(
                "Saving…",
                Style::default().fg(palette().yellow),
            ))));
        }
        out.push(PageLine::blank());

        let is = |stop: Stop| focused && self.stop == stop;
        // The search.
        let label = "Search  ";
        let field_w = width.saturating_sub(label.len()).max(1);
        let mut spans = vec![Span::styled(label, Style::default().fg(palette().gray))];
        let field_style = if is(Stop::Search) || self.editing {
            Style::default().fg(palette().white).bg(palette().selection)
        } else {
            Style::default().fg(palette().foreground).bg(palette().code_bg)
        };
        let text = if self.state.query.is_empty() && !self.editing {
            Span::styled(
                pad(&panel::truncate_right("actions or shortcuts", field_w), field_w),
                field_style.fg(palette().muted),
            )
        } else {
            let cursor = if self.editing { "▏" } else { "" };
            // The end of a long query stays in view.
            let shown = tail(&format!("{}{cursor}", self.state.query), field_w);
            Span::styled(pad(&shown, field_w), field_style)
        };
        spans.push(text);
        out.push(PageLine::stop(Line::from(spans), Stop::Search, width));

        // The context filter.
        let context = format!("Context  ‹ {} ›", self.state.context_label());
        out.push(PageLine::stop(
            Line::from(Span::styled(
                pad(&panel::truncate_right(&context, width), width),
                button_style(is(Stop::Context), true),
            )),
            Stop::Context,
            width,
        ));

        // Reset all and Open keys.yaml, side by side when they fit.
        let reset = " Reset all ";
        let open = " Open keys.yaml ";
        let reset_style = button_style(is(Stop::ResetAll), self.state.any_customized());
        let open_style = button_style(is(Stop::OpenKeys), true);
        let reset_w = reset.chars().count() as u16;
        let open_w = open.chars().count() as u16;
        if (reset_w + 2 + open_w) as usize <= width {
            let mut line = PageLine::text(Line::from(vec![
                Span::styled(reset, reset_style),
                Span::raw("  "),
                Span::styled(open, open_style),
            ]));
            line.hits = vec![
                (0, reset_w, Hit::Stop(Stop::ResetAll)),
                (reset_w + 2, reset_w + 2 + open_w, Hit::Stop(Stop::OpenKeys)),
            ];
            // Either button's stop scrolls this line into view.
            line.stop = Some(if self.stop == Stop::OpenKeys { Stop::OpenKeys } else { Stop::ResetAll });
            out.push(line);
        } else {
            out.push(PageLine::stop(Line::from(Span::styled(reset, reset_style)), Stop::ResetAll, width));
            out.push(PageLine::stop(Line::from(Span::styled(open, open_style)), Stop::OpenKeys, width));
        }

        // The groups.
        let groups = self.state.groups();
        if groups.is_empty() {
            out.push(PageLine::blank());
            wrapped(&mut out, "No actions or shortcuts match.", width, Style::default().fg(palette().muted));
            let hint = if self.state.context.is_some() {
                "Clear the search or press c for another context."
            } else {
                "Clear the search to see every binding."
            };
            wrapped(&mut out, hint, width, Style::default().fg(palette().muted));
        }
        for group in &groups {
            out.push(PageLine::blank());
            let title = group.context.title;
            let hint = group.context.hint;
            let room = width.saturating_sub(panel::display_width(title) + 2);
            let mut spans = vec![Span::styled(
                panel::truncate_right(title, width),
                Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
            )];
            if room >= hint.chars().count() {
                spans.push(Span::raw(" ".repeat(room - hint.chars().count() + 2)));
                spans.push(Span::styled(hint, Style::default().fg(palette().muted)));
            }
            out.push(PageLine::text(Line::from(spans)));
            for row in &group.rows {
                self.row_lines(&mut out, row, width, focused);
            }
        }
        out
    }

    /// A row's line (or two, when its chords don't fit beside the label),
    /// then its capture, conflict or note line.
    fn row_lines(&self, out: &mut Vec<PageLine>, row: &Row, width: usize, focused: bool) {
        let action = row.action;
        let selected = focused && self.stop == Stop::Row(action);
        let capture = self.state.capture().filter(|c| c.action == action);
        let capturing = |slot: Slot| capture.is_some_and(|c| c.slot == slot);
        let shown = row.shown();

        // The chords: `⌃P or ⌃Space`, the picked one filled, then `+` on the
        // selected row.
        let mut chips: Vec<(String, Style, Option<Hit>)> = Vec::new();
        let or = || (" or ".to_string(), Style::default().fg(palette().muted), None);
        for (i, (at, chord)) in shown.iter().enumerate() {
            if i > 0 {
                chips.push(or());
            }
            let picked = selected && self.slot == i;
            if capturing(Slot::Chord(*at)) {
                chips.push((" Press keys… ".into(), capture_style(), Some(Hit::Chord(action, i))));
                continue;
            }
            let mut text = spell(chord);
            if chord_note(chord, self.kitty).is_some() {
                text.push('⚠');
            }
            let style = if picked {
                Style::default().fg(palette().on_color).bg(palette().cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette().cyan)
            };
            chips.push((format!(" {text} "), style, Some(Hit::Chord(action, i))));
        }
        if capturing(Slot::New) {
            if !shown.is_empty() {
                chips.push(or());
            }
            chips.push((" Press keys… ".into(), capture_style(), Some(Hit::Add(action))));
        } else if shown.is_empty() {
            let picked = selected && self.slot == 0;
            let style = if picked {
                Style::default().fg(palette().on_color).bg(palette().cyan)
            } else {
                Style::default().fg(palette().muted)
            };
            chips.push((" None ".into(), style, Some(Hit::Add(action))));
        } else if selected {
            let picked = self.slot >= shown.len();
            let style = if picked {
                Style::default().fg(palette().on_color).bg(palette().cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette().muted)
            };
            chips.push((" ".into(), Style::default(), None));
            chips.push((" + ".into(), style, Some(Hit::Add(action))));
        }
        let chips_w: usize = chips.iter().map(|(t, _, _)| panel::display_width(t)).sum();

        let gutter = if row.customized {
            Span::styled("●", Style::default().fg(palette().yellow))
        } else {
            Span::raw(" ")
        };
        let label_style = if selected {
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().foreground)
        };
        let marker = if selected { "›" } else { " " };
        let lead = 3; // gutter, marker, space
        let fits = lead + 12 + 1 + chips_w <= width;
        let label_w = if fits {
            width.saturating_sub(lead + 1 + chips_w)
        } else {
            width.saturating_sub(lead)
        };
        let label = panel::truncate_right(row.label, label_w);
        let mut spans = vec![
            gutter,
            Span::styled(marker, Style::default().fg(palette().cyan)),
            Span::raw(" "),
            Span::styled(label.clone(), label_style),
        ];
        let mut hits = Vec::new();
        let mut line_x;
        if fits {
            let gap = width.saturating_sub(lead + panel::display_width(&label) + chips_w);
            spans.push(Span::raw(" ".repeat(gap)));
            line_x = (width - chips_w) as u16;
            push_chips(&mut spans, &mut hits, &chips, &mut line_x);
            let mut line = PageLine::stop(Line::from(spans), Stop::Row(action), width);
            line.hits.extend(hits);
            out.push(line);
        } else {
            out.push(PageLine::stop(Line::from(spans), Stop::Row(action), width));
            // The chords on their own line, indented under the label.
            let indent = lead.min(width);
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            line_x = indent as u16;
            push_chips(&mut spans, &mut hits, &chips, &mut line_x);
            let mut line = PageLine::under(Line::from(spans));
            line.hits = hits;
            out.push(line);
        }

        // Under the row: the conflict, the capture's keys, or why the picked
        // chord can't be sent here.
        let under = |out: &mut Vec<PageLine>, text: &str, style: Style| {
            for l in shelbi_app::overlay::error_log::wrap(text, width.saturating_sub(lead).max(1)) {
                out.push(PageLine::under(Line::from(vec![
                    Span::raw(" ".repeat(lead.min(width))),
                    Span::styled(l, style),
                ])));
            }
        };
        if capture.is_some() {
            match self.state.conflict() {
                Some(c) => under(out, &self.state.conflict_message(&c), Style::default().fg(palette().red)),
                None => under(
                    out,
                    "Press the new shortcut. Esc cancels, ⌫ removes this one.",
                    Style::default().fg(palette().yellow),
                ),
            }
        } else if selected {
            let picked = shown.get(self.slot).map(|(_, c)| *c);
            if let Some(note) = picked.and_then(|c| chord_note(&c, self.kitty)) {
                under(out, &format!("⚠ {note}"), Style::default().fg(palette().yellow));
            } else if row.customized {
                under(out, "Customized · u resets it", Style::default().fg(palette().muted));
            }
        }
    }
}

/// A drawn line of the page: its text, the stop it is (if any), the click
/// targets on it, and whether it belongs to the row above (kept in view
/// with it).
struct PageLine {
    line: Line<'static>,
    stop: Option<Stop>,
    hits: Vec<(u16, u16, Hit)>,
    under_row: bool,
}

impl PageLine {
    fn text(line: Line<'static>) -> Self {
        PageLine { line, stop: None, hits: Vec::new(), under_row: false }
    }

    fn blank() -> Self {
        PageLine::text(Line::raw(""))
    }

    /// A stop's line, the whole width its click target.
    fn stop(line: Line<'static>, stop: Stop, width: usize) -> Self {
        PageLine {
            line,
            stop: Some(stop),
            hits: vec![(0, width as u16, Hit::Stop(stop))],
            under_row: false,
        }
    }

    fn under(line: Line<'static>) -> Self {
        PageLine { line, stop: None, hits: Vec::new(), under_row: true }
    }
}

fn push_chips(
    spans: &mut Vec<Span<'static>>,
    hits: &mut Vec<(u16, u16, Hit)>,
    chips: &[(String, Style, Option<Hit>)],
    x: &mut u16,
) {
    for (text, style, hit) in chips {
        let w = panel::display_width(text) as u16;
        if let Some(hit) = hit {
            hits.push((*x, *x + w, *hit));
        }
        spans.push(Span::styled(text.clone(), *style));
        *x += w;
    }
}

fn capture_style() -> Style {
    Style::default().fg(palette().on_color).bg(palette().yellow).add_modifier(Modifier::BOLD)
}

/// A button's look: filled while focused, muted while it does nothing.
fn button_style(focused: bool, enabled: bool) -> Style {
    let base = Style::default().bg(palette().selection);
    match (focused, enabled) {
        (true, true) => base.fg(palette().white).add_modifier(Modifier::BOLD),
        (true, false) => base.fg(palette().muted),
        (false, true) => Style::default().fg(palette().cyan),
        (false, false) => Style::default().fg(palette().muted),
    }
}

fn wrapped(out: &mut Vec<PageLine>, text: &str, width: usize, style: Style) {
    for line in shelbi_app::overlay::error_log::wrap(text, width.max(1)) {
        out.push(PageLine::text(Line::from(Span::styled(line, style))));
    }
}

fn pad(text: &str, width: usize) -> String {
    let w = panel::display_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(w)))
}

/// The last `width` columns of `text`.
fn tail(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut w = 0;
    for c in chars.iter().rev() {
        let cw = panel::display_width(&c.to_string());
        if w + cw > width {
            break;
        }
        w += cw;
        out.insert(0, *c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(spec: &str) -> KeyChord {
        KeyChord::parse(spec).unwrap()
    }

    #[test]
    fn chords_a_legacy_terminal_cant_send_say_why() {
        for (spec, want) in [
            ("super-k", "doesn't send ⌘"),
            ("ctrl-h", "arrives as Backspace"),
            ("ctrl-i", "arrives as Tab"),
            ("ctrl-m", "arrives as Enter"),
            ("ctrl-shift-p", "drops Shift"),
            ("shift-enter", "without the modifier"),
            ("ctrl-space", "command palette"),
        ] {
            let note = chord_note(&chord(spec), false).unwrap_or_else(|| panic!("{spec}"));
            assert!(note.contains(want), "{spec}: {note}");
        }
        for spec in ["ctrl-p", "alt-z", "k", "shift-k", "up", "f5", "back-tab", "alt-enter"] {
            assert_eq!(chord_note(&chord(spec), false), None, "{spec}");
        }
        // The kitty protocol sends them all, but Ctrl+Space stays the
        // palette's.
        assert_eq!(chord_note(&chord("super-k"), true), None);
        assert_eq!(chord_note(&chord("ctrl-h"), true), None);
        assert!(chord_note(&chord("ctrl-space"), true).is_some());
    }

    #[test]
    fn tail_keeps_the_end_of_a_long_query() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 5), "ab");
    }
}
