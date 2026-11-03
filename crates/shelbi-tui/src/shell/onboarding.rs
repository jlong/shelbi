//! The Orchestrator view's onboarding in the TUI (`orch-onboarding`): the
//! three steps of `shelbi_app::onboarding` in the main area, in place of the
//! orchestrator's terminal, for a project whose orchestrator was never set
//! up. The desktop app draws the same steps from the same state
//! (`desktop/src/views/onboarding.rs`); this page only renders it and turns
//! keys and clicks into its calls.
//!
//! ```text
//!   1  (⋯) Your project’s orchestrator · the intro · [ Get started ]
//!   2  Choose your Orchestrator · Harness ▾ · Default model ▾ · [ Continue ]
//!   3  Make it yours · Orchestrator instructions (Markdown) · [ Back ] [ Launch Orchestrator ]
//! ```
//!
//! Keys: Tab / Shift+Tab (and ↑ ↓ off the editor) move between the
//! controls, Enter or Space presses a button or opens a picker, ← → step a
//! picker's choice in place, Esc goes Back (step 3 to 2, 2 to 1) or closes
//! an open picker. On step 3 the instructions editor takes typing while it
//! has focus (Enter is a line break; Tab leaves it). Default model's
//! "Custom model…" types a name in the box (Enter keeps it, Esc cancels).
//! The mouse clicks every control and a picker's choices, and the wheel
//! scrolls the instructions.
//!
//! The reads and saves run off the input loop; [`OnboardingPage::poll`]
//! folds them in and says when Launch Orchestrator's save has landed, so
//! the shell starts the orchestrator.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use shelbi_app::onboarding::{
    self as model, ContinueOutcome, LaunchOutcome, OnboardingState, Setup, BACK, CONTINUE, FIELD_HELP, GET_STARTED,
    HARNESS_BODY, HARNESS_LABEL, HARNESS_TITLE, INSTRUCTIONS_BODY, INSTRUCTIONS_FORMAT, INSTRUCTIONS_LABEL,
    INSTRUCTIONS_TITLE, INTRO_BODY, INTRO_TITLE, LAUNCH, LOADING, MODEL_LABEL,
};
use shelbi_app::text_area::TextArea;
use shelbi_app::view::{OnboardingModel, OnboardingStep};

use crate::button::{self, Align, Button};
use crate::panel;
use crate::picker::{self, select_lines};
use crate::textbox::{self, Textbox};
use crate::theme::palette;

/// The widest the steps' column grows (the frames' 620pt column).
const COLUMN_W: usize = 72;
/// The intro's narrower column (520pt).
const INTRO_W: usize = 60;
/// The instructions editor's rows at most (the frame's 400pt box).
const AREA_MAX: usize = 22;
const AREA_MIN: usize = 4;

/// A control the keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    GetStarted,
    Harness,
    Model,
    Continue,
    Instructions,
    Back,
    Launch,
}

/// An open picker and its highlighted choice.
type Menu = picker::Menu<Item>;

/// What a click hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Item(Item),
    Choice(usize),
}

/// A job's result.
enum Done {
    Loaded(Result<Setup, String>),
    Continued(ContinueOutcome),
    Launched(LaunchOutcome),
}

/// Runs the blocking reads and saves (a thread each in the app; a test may
/// run them in place).
pub(crate) trait Worker: Send + Sync {
    fn run(&self, job: Box<dyn FnOnce() + Send>);
}

/// The production [`Worker`]: a thread per job.
pub(crate) struct Threads;

impl Worker for Threads {
    fn run(&self, job: Box<dyn FnOnce() + Send>) {
        let _ = std::thread::Builder::new().name("shelbi-onboarding".into()).spawn(job);
    }
}

/// What the page asks of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageAction {
    Stay,
    /// Launch Orchestrator saved: start the orchestrator and show it.
    Launch,
}

/// The onboarding page for one project.
pub(crate) struct OnboardingPage {
    state: OnboardingState,
    focus: Item,
    menu: Option<Menu>,
    /// Default model's name being typed (Custom model…), caret at its end.
    custom: Option<String>,
    area: TextArea,
    /// The editor's first shown row and its height last drawn.
    area_top: usize,
    area_rows: usize,
    worker: Box<dyn Worker>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    hits: Vec<(Rect, Hit)>,
    /// The editor's rect last drawn (the wheel scrolls it).
    area_rect: Rect,
}

impl OnboardingPage {
    /// `project`'s onboarding, its settings read off the input loop.
    pub(crate) fn new(project: &str) -> OnboardingPage {
        Self::with_worker(project, Box::new(Threads))
    }

    pub(crate) fn with_worker(project: &str, worker: Box<dyn Worker>) -> OnboardingPage {
        let (tx, rx) = channel();
        let page = OnboardingPage {
            state: OnboardingState::new(project),
            focus: Item::GetStarted,
            menu: None,
            custom: None,
            area: TextArea::from_text(""),
            area_top: 0,
            area_rows: AREA_MIN,
            worker,
            tx,
            rx,
            hits: Vec::new(),
            area_rect: Rect::default(),
        };
        let project = project.to_string();
        page.spawn(move || Done::Loaded(model::load(&project)));
        page
    }

    pub(crate) fn project(&self) -> &str {
        self.state.project()
    }

    #[cfg(test)]
    pub(crate) fn step(&self) -> OnboardingStep {
        self.state.step()
    }

    #[cfg(test)]
    pub(crate) fn focus(&self) -> Item {
        self.focus
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> &OnboardingState {
        &self.state
    }

    fn spawn(&self, job: impl FnOnce() -> Done + Send + 'static) {
        let tx = self.tx.clone();
        self.worker.run(Box::new(move || {
            let _ = tx.send(job());
        }));
    }

    /// Fold in finished jobs. `Launch` once Launch Orchestrator's save
    /// landed.
    pub(crate) fn poll(&mut self) -> (bool, PageAction) {
        let mut changed = false;
        loop {
            let done = match self.rx.try_recv() {
                Ok(done) => done,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return (changed, PageAction::Stay),
            };
            changed = true;
            match done {
                Done::Loaded(setup) => {
                    self.state.loaded(setup);
                    self.area = TextArea::from_text(self.state.instructions());
                }
                Done::Continued(outcome) => {
                    self.state.continued(outcome);
                    if self.state.step() == OnboardingStep::Instructions {
                        self.focus = Item::Instructions;
                    }
                }
                Done::Launched(outcome) => {
                    if self.state.launched(outcome) {
                        return (true, PageAction::Launch);
                    }
                }
            }
        }
    }

    /// The controls of the step shown, in Tab order.
    fn items(&self) -> Vec<Item> {
        match self.state.step() {
            OnboardingStep::Intro => vec![Item::GetStarted],
            OnboardingStep::Harness if self.state.takes_model() => vec![Item::Harness, Item::Model, Item::Continue],
            OnboardingStep::Harness => vec![Item::Harness, Item::Continue],
            OnboardingStep::Instructions => vec![Item::Instructions, Item::Back, Item::Launch],
        }
    }

    /// Keep focus on a control the step has.
    fn settle(&mut self) {
        let items = self.items();
        if !items.contains(&self.focus) {
            self.focus = items[0];
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let items = self.items();
        let at = items.iter().position(|i| *i == self.focus).unwrap_or(0);
        let n = items.len();
        self.focus = items[if forward { (at + 1) % n } else { (at + n - 1) % n }];
    }

    // -----------------------------------------------------------------------
    // Actions

    fn press(&mut self, item: Item) -> PageAction {
        self.focus = item;
        match item {
            Item::GetStarted => {
                self.state.get_started();
                self.settle();
            }
            Item::Harness | Item::Model => self.open_menu(item),
            Item::Continue => {
                if let Some(job) = self.state.begin_continue() {
                    self.spawn(move || Done::Continued(model::run_continue(job)));
                }
            }
            Item::Instructions => {}
            Item::Back => self.back(),
            Item::Launch => {
                self.state.type_instructions(&self.area.text());
                if let Some(job) = self.state.begin_launch() {
                    self.spawn(move || Done::Launched(model::run_launch(job)));
                }
            }
        }
        PageAction::Stay
    }

    fn back(&mut self) {
        self.menu = None;
        self.custom = None;
        if self.state.step() == OnboardingStep::Instructions {
            self.state.type_instructions(&self.area.text());
        }
        self.state.back();
        self.settle();
        // Back on step 2 lands on its first control, step 1 on Get started.
        self.focus = self.items()[0];
    }

    /// The labels and their changes of `item`'s picker, and which is chosen.
    fn choices(&self, item: Item) -> (Vec<String>, Option<usize>) {
        let m = self.state.model();
        match item {
            Item::Harness => picker::harness_labels(&m.harnesses),
            Item::Model => picker::model_labels(&m.models),
            _ => (Vec::new(), None),
        }
    }

    fn open_menu(&mut self, item: Item) {
        if self.state.busy() || (item == Item::Model && !self.state.model().model_enabled) {
            return;
        }
        if self.menu.is_some_and(|m| m.item == item) {
            self.menu = None;
            return;
        }
        let (labels, chosen) = self.choices(item);
        if labels.is_empty() {
            return;
        }
        self.menu = Some(Menu { item, selected: chosen.unwrap_or(0) });
    }

    /// Choose `item`'s choice `index`.
    fn choose(&mut self, item: Item, index: usize) {
        self.menu = None;
        let m = self.state.model();
        match item {
            Item::Harness => {
                if let Some(h) = m.harnesses.get(index) {
                    self.state.select_runner(h.kind);
                    self.settle();
                }
            }
            Item::Model => match m.models.get(index) {
                Some(o) => self.state.select_model(o.model.as_deref()),
                None => self.custom = Some(self.state.model_choice().unwrap_or_default().to_string()),
            },
            _ => {}
        }
    }

    /// ← → on a picker: its previous or next choice, in place.
    fn cycle(&mut self, item: Item, forward: bool) {
        let (labels, chosen) = self.choices(item);
        // Custom model… isn't stepped onto.
        let n = if item == Item::Model { labels.len().saturating_sub(1) } else { labels.len() };
        if n == 0 || self.state.busy() || (item == Item::Model && !self.state.model().model_enabled) {
            return;
        }
        let at = chosen.unwrap_or(0);
        let next = if forward { (at + 1).min(n - 1) } else { at.saturating_sub(1) };
        self.choose(item, next);
    }

    // -----------------------------------------------------------------------
    // Input

    /// A paste goes into the instructions while they have focus, or the
    /// custom model's name.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        if let Some(custom) = self.custom.as_mut() {
            custom.push_str(&text.replace(['\n', '\r'], " "));
            return true;
        }
        if self.state.step() == OnboardingStep::Instructions && self.focus == Item::Instructions && !self.state.busy() {
            self.area.insert_str(text);
            self.state.type_instructions(&self.area.text());
            return true;
        }
        false
    }

    pub(crate) fn handle_key(&mut self, k: KeyEvent) -> PageAction {
        if let Some(custom) = self.custom.as_mut() {
            match k.code {
                KeyCode::Enter => {
                    let text = std::mem::take(custom);
                    self.custom = None;
                    self.state.select_model(Some(&text));
                }
                KeyCode::Esc => self.custom = None,
                KeyCode::Backspace => {
                    custom.pop();
                }
                KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => custom.push(c),
                _ => {}
            }
            return PageAction::Stay;
        }
        if let Some(menu) = self.menu {
            let (labels, _) = self.choices(menu.item);
            let n = labels.len().max(1);
            match k.code {
                KeyCode::Esc => self.menu = None,
                KeyCode::Up | KeyCode::BackTab => {
                    self.menu = Some(Menu { selected: (menu.selected + n - 1) % n, ..menu });
                }
                KeyCode::Down | KeyCode::Tab => self.menu = Some(Menu { selected: (menu.selected + 1) % n, ..menu }),
                KeyCode::Enter | KeyCode::Char(' ') => self.choose(menu.item, menu.selected),
                _ => {}
            }
            return PageAction::Stay;
        }
        self.settle();
        // The instructions editor takes every key but Tab and Esc.
        if self.focus == Item::Instructions && !matches!(k.code, KeyCode::Tab | KeyCode::BackTab | KeyCode::Esc) {
            if !self.state.busy() {
                if let Some(chord) = crate::keymap::chord_from_event(k) {
                    self.area.edit(chord, false, self.area_rows.max(1));
                    self.state.type_instructions(&self.area.text());
                }
            }
            return PageAction::Stay;
        }
        match k.code {
            KeyCode::Tab => self.step_focus(true),
            KeyCode::BackTab => self.step_focus(false),
            KeyCode::Down => self.step_focus(true),
            KeyCode::Up => self.step_focus(false),
            KeyCode::Left | KeyCode::Right if matches!(self.focus, Item::Harness | Item::Model) => {
                self.cycle(self.focus, k.code == KeyCode::Right)
            }
            KeyCode::Left => self.step_focus(false),
            KeyCode::Right => self.step_focus(true),
            KeyCode::Esc => {
                if self.state.step() != OnboardingStep::Intro {
                    self.back();
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => return self.press(self.focus),
            _ => {}
        }
        PageAction::Stay
    }

    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) -> PageAction {
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if panel::in_rect(self.area_rect, m.column, m.row) => {
                let down = matches!(m.kind, MouseEventKind::ScrollDown);
                self.area.move_lines(down, 3);
                PageAction::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.hits.iter().rev().find(|(r, _)| panel::in_rect(*r, m.column, m.row)).map(|h| h.1);
                match hit {
                    Some(Hit::Choice(i)) => {
                        if let Some(menu) = self.menu {
                            self.choose(menu.item, i);
                        }
                        PageAction::Stay
                    }
                    Some(Hit::Item(item)) => {
                        if self.menu.is_some_and(|m| m.item != item) {
                            self.menu = None;
                        }
                        self.custom = None;
                        if item == Item::Instructions {
                            self.focus = item;
                            PageAction::Stay
                        } else {
                            self.press(item)
                        }
                    }
                    None => {
                        self.menu = None;
                        PageAction::Stay
                    }
                }
            }
            _ => PageAction::Stay,
        }
    }

    // -----------------------------------------------------------------------
    // Drawing

    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect) {
        self.hits.clear();
        self.area_rect = Rect::default();
        if area.width < 8 || area.height < 4 {
            return;
        }
        self.settle();
        let m = self.state.model();
        match m.step {
            OnboardingStep::Intro => self.render_intro(f, area),
            OnboardingStep::Harness => self.render_harness(f, area, &m),
            OnboardingStep::Instructions => self.render_instructions(f, area, &m),
        }
    }

    /// The column the step's rows sit in, `want` wide at most, two columns
    /// off each side.
    fn column(area: Rect, want: usize) -> Rect {
        let w = (area.width as usize).saturating_sub(4).min(want) as u16;
        Rect { x: area.x + (area.width - w) / 2, width: w, ..area }
    }

    /// Draw `rows` in `col`, centered top to bottom (from the top when they
    /// don't fit), recording each row's hit.
    fn draw_rows(&mut self, f: &mut Frame, col: Rect, rows: Vec<(Line<'static>, Option<Hit>)>) -> u16 {
        let top = col.y + (col.height.saturating_sub(rows.len() as u16)) / 2;
        for (i, (line, hit)) in rows.into_iter().enumerate() {
            let y = top + i as u16;
            if y >= col.bottom() {
                break;
            }
            let rect = Rect { y, height: 1, ..col };
            f.render_widget(Paragraph::new(line), rect);
            if let Some(hit) = hit {
                self.hits.push((rect, hit));
            }
        }
        top
    }

    fn render_intro(&mut self, f: &mut Frame, area: Rect) {
        let col = Self::column(area, INTRO_W);
        let w = col.width as usize;
        let mut rows: Vec<(Line<'static>, Option<Hit>)> = Vec::new();
        // The frame's chat-bubble disc.
        let disc = button::chip(" ⋯ ".into(), Style::default().fg(palette().cyan), palette().search, palette().canvas);
        rows.push((centered(disc.to_vec(), w), None));
        rows.push((Line::default(), None));
        rows.push((centered(vec![Span::styled(INTRO_TITLE, title_style())], w), None));
        rows.push((Line::default(), None));
        for line in wrap_text(INTRO_BODY, w) {
            rows.push((centered(vec![Span::styled(line, Style::default().fg(palette().muted))], w), None));
        }
        rows.push((Line::default(), None));
        let get = Button::primary(GET_STARTED).focused(self.focus == Item::GetStarted);
        let bw = get.width();
        for spans in get.spans() {
            rows.push((centered(vec![spans], w), None));
        }
        let top = self.draw_rows(f, col, rows);
        // The button's three rows, at its own columns.
        let n = 5 + wrap_text(INTRO_BODY, w).len() as u16;
        let x = col.x + (w.saturating_sub(bw) / 2) as u16;
        self.hits.push((
            Rect { x, y: top + n, width: bw as u16, height: 3 }.intersection(area),
            Hit::Item(Item::GetStarted),
        ));
    }

    fn render_harness(&mut self, f: &mut Frame, area: Rect, m: &OnboardingModel) {
        let col = Self::column(area, COLUMN_W);
        let w = col.width as usize;
        let mut rows: Vec<(Line<'static>, Option<Hit>)> = Vec::new();
        heading_rows(&mut rows, HARNESS_TITLE, HARNESS_BODY, w);
        rows.push((Line::default(), None));

        let harness = m.harnesses.iter().find(|h| h.selected);
        let (label, provider) = harness.map_or((LOADING.to_string(), None), |h| (h.label.clone(), h.provider.clone()));
        rows.push((textbox::label(HARNESS_LABEL, self.focus == Item::Harness, false, w), None));
        let harness_at = rows.len();
        for line in select_lines(&label, provider.as_deref(), self.focus == Item::Harness, m.loading || m.busy, w) {
            rows.push((line, Some(Hit::Item(Item::Harness))));
        }
        rows.push((help_line(FIELD_HELP, w), None));
        rows.push((Line::default(), None));

        let model_focused = self.focus == Item::Model;
        rows.push((textbox::label(MODEL_LABEL, model_focused, !m.model_enabled, w), None));
        let model_at = rows.len();
        match &self.custom {
            Some(text) => {
                let boxed = Textbox::new(text).caret_at_end().placeholder("Model name").focused(true);
                for line in boxed.lines(w) {
                    rows.push((line, Some(Hit::Item(Item::Model))));
                }
            }
            None => {
                let shown = if m.loading { LOADING.to_string() } else { m.model_label.clone() };
                for line in select_lines(&shown, None, model_focused, !m.model_enabled || m.busy, w) {
                    rows.push((line, m.model_enabled.then_some(Hit::Item(Item::Model))));
                }
            }
        }
        match &m.model_note {
            Some(note) => {
                for line in wrap_text(note, w) {
                    rows.push((help_line(&line, w), None));
                }
            }
            None => rows.push((help_line(FIELD_HELP, w), None)),
        }
        error_rows(&mut rows, m.error.as_deref(), w);
        rows.push((Line::default(), None));
        rows.push((divider(w), None));
        rows.push((Line::default(), None));
        let label = if m.busy { "Saving…" } else { CONTINUE };
        let cont = Button::primary(label).focused(self.focus == Item::Continue).enabled(!m.loading && !m.busy);
        let (lines, at) = button::row(&[cont], w, Align::Right);
        let buttons_at = rows.len();
        for line in lines {
            rows.push((line, None));
        }
        let top = self.draw_rows(f, col, rows);
        if let Some(&(x, bw)) = at.first() {
            self.hits.push((
                Rect { x: col.x + x, y: top + buttons_at as u16, width: bw, height: 3 }.intersection(area),
                Hit::Item(Item::Continue),
            ));
        }
        // An open picker drops below its box, over what follows.
        if let Some(menu) = self.menu {
            let below = if menu.item == Item::Harness { harness_at } else { model_at } + 3;
            let (labels, chosen) = self.choices(menu.item);
            self.render_menu(f, area, Rect { y: top + below as u16, height: 1, ..col }, &labels, chosen, menu.selected);
        }
    }

    fn render_menu(&mut self, f: &mut Frame, area: Rect, at: Rect, labels: &[String], chosen: Option<usize>, selected: usize) {
        for (rect, i) in picker::render_menu(f, area, at, labels, chosen, selected) {
            self.hits.push((rect, Hit::Choice(i)));
        }
    }

    fn render_instructions(&mut self, f: &mut Frame, area: Rect, m: &OnboardingModel) {
        let col = Self::column(area, COLUMN_W);
        let w = col.width as usize;
        let mut head: Vec<(Line<'static>, Option<Hit>)> = Vec::new();
        heading_rows(&mut head, INSTRUCTIONS_TITLE, INSTRUCTIONS_BODY, w);
        head.push((Line::default(), None));
        let focused = self.focus == Item::Instructions;
        let label_style = if focused {
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().white)
        };
        let fmt_w = panel::display_width(INSTRUCTIONS_FORMAT);
        let gap = w.saturating_sub(panel::display_width(INSTRUCTIONS_LABEL) + fmt_w);
        head.push((
            Line::from(vec![
                Span::styled(INSTRUCTIONS_LABEL, label_style),
                Span::raw(" ".repeat(gap)),
                Span::styled(INSTRUCTIONS_FORMAT, Style::default().fg(palette().muted)),
            ]),
            None,
        ));
        let mut tail: Vec<(Line<'static>, Option<Hit>)> = Vec::new();
        error_rows(&mut tail, m.error.as_deref(), w);
        tail.push((Line::default(), None));
        tail.push((divider(w), None));
        tail.push((Line::default(), None));
        let tail_fixed = tail.len() + 3;
        // The editor takes what's left, within its bounds.
        let room = (col.height as usize).saturating_sub(head.len() + tail_fixed + 2);
        let rows_n = room.clamp(AREA_MIN, AREA_MAX);
        self.area_rows = rows_n;
        let mut rows = head;
        let area_at = rows.len();
        let loading = m.loading;
        for line in self.area_lines(rows_n, focused && !m.busy, loading, w) {
            rows.push((line, Some(Hit::Item(Item::Instructions))));
        }
        rows.extend(tail);
        let launch_label = if m.busy { "Launching…" } else { LAUNCH };
        let left = vec![button::Piece::button(Button::secondary(BACK).focused(self.focus == Item::Back).enabled(!m.busy), 0, 0)];
        let right = vec![button::Piece::button(
            Button::primary(launch_label).focused(self.focus == Item::Launch).enabled(!m.busy && !loading),
            1,
            0,
        )];
        let (lines, at) = button::bar(left, right, w);
        let buttons_at = rows.len();
        for line in lines {
            rows.push((line, None));
        }
        let top = self.draw_rows(f, col, rows);
        self.area_rect = Rect { y: top + area_at as u16, height: rows_n as u16 + 2, ..col }.intersection(area);
        for (x, bw, tag) in at {
            let item = if tag == 0 { Item::Back } else { Item::Launch };
            self.hits.push((
                Rect { x: col.x + x, y: top + buttons_at as u16, width: bw, height: 3 }.intersection(area),
                Hit::Item(item),
            ));
        }
    }

    /// The instructions editor: `rows` text rows between the box's caps,
    /// scrolled to keep the caret in view while it has focus.
    fn area_lines(&mut self, rows: usize, focused: bool, loading: bool, w: usize) -> Vec<Line<'static>> {
        let text_w = textbox::room(w).max(1);
        let wrap = textbox::wrap(text_w);
        let (wrapped, (caret_row, caret_col)) = self.area.wrapped(&wrap);
        self.area.set_wrap(Some(wrap));
        if caret_row < self.area_top {
            self.area_top = caret_row;
        } else if caret_row >= self.area_top + rows {
            self.area_top = caret_row + 1 - rows;
        }
        self.area_top = self.area_top.min(wrapped.len().saturating_sub(1));
        let style = textbox::area_text(focused, false).bg(textbox::fill(focused, false, false));
        let lines = self.area.lines();
        let mut body = Vec::new();
        if loading {
            body.push(vec![Span::styled(LOADING, Style::default().fg(palette().muted))]);
        }
        for v in self.area_top..self.area_top + rows {
            if body.len() >= rows {
                break;
            }
            let mut spans = Vec::new();
            if let Some(&row) = wrapped.get(v).filter(|_| !loading) {
                let caret = (focused && v == caret_row).then_some(caret_col);
                let (chars, caret) = textbox::row_chars(lines, row, caret, text_w);
                textbox::caret_spans(&mut spans, &chars, caret, text_w, style);
            }
            body.push(spans);
        }
        while body.len() < rows {
            body.push(Vec::new());
        }
        textbox::area(body, focused, false, false, w)
    }
}

// ---------------------------------------------------------------------------
// Row helpers

fn title_style() -> Style {
    Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
}

/// A step's title and the sentence under it.
fn heading_rows(rows: &mut Vec<(Line<'static>, Option<Hit>)>, title: &'static str, body: &'static str, w: usize) {
    rows.push((Line::from(Span::styled(title, title_style())), None));
    rows.push((Line::default(), None));
    for line in wrap_text(body, w) {
        rows.push((Line::from(Span::styled(line, Style::default().fg(palette().foreground))), None));
    }
}

fn help_line(text: &str, w: usize) -> Line<'static> {
    Line::from(Span::styled(panel::truncate_right(text, w), Style::default().fg(palette().muted)))
}

fn error_rows(rows: &mut Vec<(Line<'static>, Option<Hit>)>, error: Option<&str>, w: usize) {
    if let Some(e) = error {
        rows.push((Line::default(), None));
        for line in wrap_text(e, w) {
            rows.push((Line::from(Span::styled(line, Style::default().fg(palette().red))), None));
        }
    }
}

/// The 1-row rule between a step's fields and its buttons.
fn divider(w: usize) -> Line<'static> {
    Line::from(Span::styled("─".repeat(w), Style::default().fg(palette().table_rule)))
}

/// `spans` centered in `w` columns.
fn centered(spans: Vec<Span<'static>>, w: usize) -> Line<'static> {
    let used: usize = spans.iter().map(|s| panel::display_width(&s.content)).sum();
    let mut out = vec![Span::raw(" ".repeat(w.saturating_sub(used) / 2))];
    out.extend(spans);
    Line::from(out)
}

/// `text` word-wrapped to `w` columns.
fn wrap_text(text: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let need = if line.is_empty() { 0 } else { 1 } + panel::display_width(word);
        if !line.is_empty() && panel::display_width(&line) + need > w {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests;
