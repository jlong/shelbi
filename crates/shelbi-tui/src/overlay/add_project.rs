//! The new-project flow in the TUI: the shared three-page form
//! ([`shelbi_app::overlay::add_project`]: the folder, the Agent defaults, where
//! settings live) drawn in one dialog, in the desktop wizard's order and with
//! its copy (`desktop/src/views/add_project.rs` draws the same steps over
//! [`shelbi_app::add_project_wizard`]).
//!
//! ```text
//!   1  Add a project · Name · Repo path · Detected: · Folder / id:
//!   2  Agent defaults · Default Harness ▾ · Default model ▾
//!   3  Where should settings live? · (•) In the project · ( ) Outside the project
//!      Step N of 3                                 [ Back ] [ Continue ]
//! ```
//!
//! The pickers are the orchestrator onboarding's ([`crate::picker`] over
//! `shelbi_app::harness_picker`): a box that opens a menu, ← → stepping the
//! choice in place, and Default model's "Custom model…" typing a name in the
//! box (Enter keeps it, Esc cancels).
//!
//! One implementation, two callers: the in-process TUI overlay
//! (`crate::shell`) and the launch screen (`crate::launch`), each feeding it
//! keys and clicks. Continue on page 1 validates the name and path here
//! ([`shelbi_orchestrator::project_create::validate_add_project`]: a slug
//! check, a collision lookup, a path stat), so a taken name shows before the
//! later pages; Create Project hands back [`Step::Submit`] and the caller
//! validates again and scaffolds off the UI thread with [`Form::defaults`].
//!
//! Keys: Tab / Shift+Tab move between the page's controls, Enter presses the
//! primary button from anywhere, Space presses the focused button, opens the
//! focused picker or flips the location, Esc cancels (or closes an open
//! menu). The mouse clicks every control, a menu's rows and the location's
//! rows.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use shelbi_app::add_project_wizard as wiz;
use shelbi_core::ConfigMode;
use crate::button::{self, Align, Button};
use crate::panel;
use crate::picker::{self, select_lines};
use crate::textbox::{self, Textbox};
use crate::theme::palette;

// The form's state machine is shared with the desktop app
// (`shelbi_app::overlay::add_project`).
pub use shelbi_app::overlay::add_project::{Detected, Field, Page, Step};
use shelbi_app::overlay::add_project as shared;

/// An open picker menu.
type Menu = picker::Menu<Field>;

/// What a click lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Field(Field),
    /// A row of the open menu.
    Choice(usize),
    /// Page 3's location rows.
    Mode(ConfigMode),
}

/// The add-project form: the shared state machine, with the "Detected:" check
/// run inline after each key (it is cached per root text, so it only runs git
/// when the repo path changes), page 1's validation, and the pickers' menus.
#[derive(Debug, Clone)]
pub struct Form {
    inner: shared::Form,
    menu: Option<Menu>,
    /// Default model's name being typed (Custom model…), caret at its end.
    custom: Option<String>,
    /// What each drawn control's rect is, from the last render.
    hits: Vec<(Rect, Hit)>,
}

impl Form {
    /// Seed the form. Root prefills to `cwd` (the same default `shelbi init`'s
    /// "Project root?" prompt offers); the agents default to Claude Code on
    /// its default model; config location defaults to `global`, the
    /// low-ceremony solo choice.
    pub fn new(cwd: &Path) -> Self {
        let mut form = Self {
            inner: shared::Form::new(cwd),
            menu: None,
            custom: None,
            hits: Vec::new(),
        };
        form.detect();
        form
    }

    /// The entered (raw, human-readable) project name.
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// The entered repo-path text (not yet absolutized).
    pub fn root(&self) -> &str {
        self.inner.root()
    }

    /// The chosen config location.
    pub fn mode(&self) -> ConfigMode {
        self.inner.mode()
    }

    /// The agents' default runner and model (page 2).
    pub fn defaults(&self) -> shelbi_core::AgentDefaults {
        self.inner.defaults()
    }

    /// The page shown.
    pub fn page(&self) -> Page {
        self.inner.page()
    }

    /// The focused control.
    pub fn focus(&self) -> Field {
        self.inner.focus()
    }

    /// Stash an inline validation error (shown in red on page 1) after a
    /// failed submit, keeping the form open so the user can fix the input.
    /// Cleared the moment they edit anything.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.close_menus();
        self.inner.set_error(msg);
    }

    fn close_menus(&mut self) {
        self.menu = None;
        self.custom = None;
    }

    /// Feed one key event: an open menu or a custom model name takes it
    /// first, else the shared form ([`shared::Form::handle_key`]). Editing
    /// the root re-runs the cached git-repo detection.
    pub fn handle_key(&mut self, key: KeyEvent) -> Step {
        let step = self.key(key);
        self.detect();
        step
    }

    fn key(&mut self, key: KeyEvent) -> Step {
        if let Some(custom) = self.custom.as_mut() {
            match key.code {
                KeyCode::Enter => {
                    let text = std::mem::take(custom);
                    self.custom = None;
                    self.inner.select_model(Some(&text));
                }
                KeyCode::Esc => self.custom = None,
                KeyCode::Backspace => {
                    custom.pop();
                }
                KeyCode::Char(c) if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => custom.push(c),
                _ => {}
            }
            return Step::Continue;
        }
        if let Some(menu) = self.menu {
            let n = self.choices(menu.item).0.len().max(1);
            match key.code {
                KeyCode::Esc => self.menu = None,
                KeyCode::Up | KeyCode::BackTab => {
                    self.menu = Some(Menu { selected: (menu.selected + n - 1) % n, ..menu });
                }
                KeyCode::Down | KeyCode::Tab => self.menu = Some(Menu { selected: (menu.selected + 1) % n, ..menu }),
                KeyCode::Enter | KeyCode::Char(' ') => self.choose(menu.item, menu.selected),
                _ => {}
            }
            return Step::Continue;
        }
        let step = match crate::keymap::chord_from_event(key) {
            Some(chord) => self.inner.handle_key(chord),
            None => Step::Continue,
        };
        self.follow(step)
    }

    /// Act on what the shared form asked for: validate page 1, open a menu.
    /// What's left (Continue, Submit, Cancel) is the caller's.
    fn follow(&mut self, step: Step) -> Step {
        match step {
            Step::Validate => {
                self.close_menus();
                match shelbi_orchestrator::project_create::validate_add_project(
                    self.inner.name(),
                    self.inner.root(),
                    self.inner.cwd(),
                ) {
                    Ok(_) => self.inner.folder_valid(),
                    Err(msg) => self.inner.set_error(msg),
                }
                Step::Continue
            }
            Step::Menu(field) => {
                self.open_menu(field);
                Step::Continue
            }
            other => {
                if other != Step::Continue {
                    self.close_menus();
                }
                other
            }
        }
    }

    /// A mouse event: a left click on a control, a menu row or a location
    /// row. Clicking anywhere else closes an open menu.
    pub fn handle_mouse(&mut self, m: MouseEvent) -> Step {
        if m.kind != MouseEventKind::Down(MouseButton::Left) {
            return Step::Continue;
        }
        let hit = self.hits.iter().rev().find(|(r, _)| panel::in_rect(*r, m.column, m.row)).map(|h| h.1);
        let step = match hit {
            Some(Hit::Choice(i)) => {
                if let Some(menu) = self.menu {
                    self.choose(menu.item, i);
                }
                Step::Continue
            }
            Some(Hit::Field(field)) => {
                let reopen = self.menu.is_some_and(|m| m.item == field);
                self.close_menus();
                if reopen {
                    // A second click on an open picker closes it.
                    self.inner.set_focus(field);
                    Step::Continue
                } else {
                    let step = self.inner.click(field);
                    self.follow(step)
                }
            }
            Some(Hit::Mode(mode)) => {
                self.close_menus();
                self.inner.select_mode(mode);
                Step::Continue
            }
            None => {
                self.close_menus();
                Step::Continue
            }
        };
        self.detect();
        step
    }

    /// The labels of `field`'s menu, and which is chosen.
    fn choices(&self, field: Field) -> (Vec<String>, Option<usize>) {
        let p = self.inner.picker();
        match field {
            Field::Harness => picker::harness_labels(&p.harnesses()),
            Field::Model => picker::model_labels(&p.models()),
            _ => (Vec::new(), None),
        }
    }

    fn open_menu(&mut self, field: Field) {
        if self.menu.is_some_and(|m| m.item == field) {
            self.menu = None;
            return;
        }
        let (labels, chosen) = self.choices(field);
        self.custom = None;
        self.menu = (!labels.is_empty()).then(|| Menu { item: field, selected: chosen.unwrap_or(0) });
    }

    /// Choose `field`'s menu row `index` (Default model's last row types a
    /// name).
    fn choose(&mut self, field: Field, index: usize) {
        self.menu = None;
        let p = self.inner.picker().clone();
        match field {
            Field::Harness => {
                if let Some(h) = p.harnesses().get(index) {
                    self.inner.select_runner(h.kind);
                }
            }
            Field::Model => match p.models().get(index) {
                Some(o) => self.inner.select_model(o.model.as_deref()),
                None => self.custom = Some(p.model().unwrap_or_default().to_string()),
            },
            _ => {}
        }
        self.inner.set_focus(field);
    }

    /// Paint the dialog into `area`.
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        self.hits.clear();
        render(f, area, self);
    }

    /// Run the git-repo check when the root text changed since the last one.
    fn detect(&mut self) {
        if let Some(root) = self.inner.detection_due() {
            let detected =
                shelbi_orchestrator::project_create::detect_root(self.inner.cwd(), &root);
            self.inner.set_detected(&root, detected);
        }
    }
}

/// The "Detected:" line for the latest detection.
fn detected_line(detected: Option<Detected>) -> Span<'static> {
    match detected {
        Some(d) => {
            let color = if d.is_ok() {
                palette().ansi_green
            } else {
                palette().ansi_yellow
            };
            Span::styled(d.message(), Style::default().fg(color))
        }
        None => Span::raw(""),
    }
}

type Rows = Vec<(Line<'static>, Option<Hit>)>;

/// Paint the dialog. The card **fills** `area` (rather than a small centered
/// box), so the fields, the derived-slug preview, and long error/notice text
/// always have room and never truncate. The page's rows run from the top;
/// its footer ("Step N of 3", Back and the primary button) sits at the
/// bottom.
fn render(f: &mut Frame, area: Rect, form: &mut Form) {
    f.render_widget(Clear, area);
    let page = form.inner.page();
    let frame = crate::overlay::dialog_frame(f.buffer_mut(), area, None, Some(page.title()));
    // A column in from the border on each side.
    let inner = Rect {
        x: frame.x + 1,
        width: frame.width.saturating_sub(2),
        ..frame
    };
    if inner.width < 8 || inner.height < 4 {
        return;
    }
    let w = inner.width as usize;
    let mut rows: Rows = vec![(Line::raw(""), None)];
    if let Some(intro) = page.intro() {
        for line in wrap_text(intro, w) {
            rows.push((Line::from(Span::styled(line, Style::default().fg(palette().foreground))), None));
        }
        rows.push((Line::raw(""), None));
    }
    let mut menu_at = None;
    match page {
        Page::Folder => folder_rows(&mut rows, form),
        Page::Defaults => menu_at = defaults_rows(&mut rows, form, w),
        Page::Settings => settings_rows(&mut rows, form, w),
    }
    // The error row is reserved so the footer doesn't jump when one shows.
    rows.push((Line::raw(""), None));
    match form.inner.error() {
        Some(msg) => {
            for line in wrap_text(&format!("✗ {msg}"), w) {
                rows.push((
                    Line::from(Span::styled(
                        line,
                        Style::default().fg(palette().ansi_red).add_modifier(Modifier::BOLD),
                    )),
                    None,
                ));
            }
        }
        None => rows.push((Line::raw(""), None)),
    }

    // The footer: three rows at the bottom.
    let footer_y = inner.bottom().saturating_sub(3).max(inner.y);
    for (i, (line, hit)) in rows.into_iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= footer_y {
            break;
        }
        let rect = Rect { y, height: 1, ..inner };
        f.render_widget(Paragraph::new(line), rect);
        if let Some(hit) = hit {
            form.hits.push((rect, hit));
        }
    }
    footer(f, form, Rect { y: footer_y, height: 3.min(inner.height), ..inner });

    // An open picker drops below its box, over what follows.
    if let (Some(menu), Some((harness_y, model_y))) = (form.menu, menu_at) {
        let below = if menu.item == Field::Harness { harness_y } else { model_y } + 3;
        let (labels, chosen) = form.choices(menu.item);
        let at = Rect { y: inner.y + below as u16, height: 1, ..inner };
        for (rect, i) in picker::render_menu(f, area, at, &labels, chosen, menu.selected) {
            form.hits.push((rect, Hit::Choice(i)));
        }
    }
}

/// Page 1: the name and repo path, the "Detected:" line and the derived id.
fn folder_rows(rows: &mut Rows, form: &Form) {
    let focus = form.inner.focus();
    rows.push((field_line("Name", form.name(), focus == Field::Name), Some(Hit::Field(Field::Name))));
    rows.push((field_line("Repo path", form.root(), focus == Field::Root), Some(Hit::Field(Field::Root))));
    rows.push((Line::raw(""), None));
    rows.push((Line::from(detected_line(form.inner.detected())), None));
    rows.push((preview_line(&form.inner), None));
}

/// Page 2: Default Harness and Default model, each a picker box with the
/// helper under it. Returns where each box starts (row index), for the menu.
fn defaults_rows(rows: &mut Rows, form: &Form, w: usize) -> Option<(usize, usize)> {
    let focus = form.inner.focus();
    let p = form.inner.picker();
    let harness = p.harnesses().into_iter().find(|h| h.selected);
    let (label, provider) = harness.map_or((String::new(), None), |h| (h.label, h.provider));
    rows.push((textbox::label(wiz::HARNESS_LABEL, focus == Field::Harness, false, w), None));
    let harness_at = rows.len();
    for line in select_lines(&label, provider.as_deref(), focus == Field::Harness, false, w) {
        rows.push((line, Some(Hit::Field(Field::Harness))));
    }
    rows.push((help_line(wiz::FIELD_HELP, w), None));
    rows.push((Line::raw(""), None));

    let takes_model = p.takes_model();
    let model_focused = focus == Field::Model;
    rows.push((textbox::label(wiz::MODEL_LABEL, model_focused, !takes_model, w), None));
    let model_at = rows.len();
    match &form.custom {
        Some(text) => {
            let boxed = Textbox::new(text).caret_at_end().placeholder("Model name").focused(true);
            for line in boxed.lines(w) {
                rows.push((line, Some(Hit::Field(Field::Model))));
            }
        }
        None => {
            for line in select_lines(&p.model_label(), None, model_focused, !takes_model, w) {
                rows.push((line, takes_model.then_some(Hit::Field(Field::Model))));
            }
        }
    }
    let note = form.inner.model_note();
    rows.push((help_line(note.as_deref().unwrap_or(wiz::FIELD_HELP), w), None));
    Some((harness_at, model_at))
}

/// Page 3: the two locations, each a radio with its description, and the
/// derived id with where its config lands.
fn settings_rows(rows: &mut Rows, form: &Form, w: usize) {
    let focused = form.inner.focus() == Field::Config;
    for (mode, title, body) in [
        (ConfigMode::InRepo, wiz::IN_REPO_TITLE, wiz::IN_REPO_BODY),
        (ConfigMode::Global, wiz::GLOBAL_TITLE, wiz::GLOBAL_BODY),
    ] {
        let on = form.mode() == mode;
        let mark = if on { "(•) " } else { "( ) " };
        let mut style = Style::default().fg(if on { palette().white } else { palette().gray });
        if on && focused {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        rows.push((
            Line::from(vec![
                Span::styled(mark, Style::default().fg(if on { palette().cyan } else { palette().gray })),
                Span::styled(title, style),
            ]),
            Some(Hit::Mode(mode)),
        ));
        for line in wrap_text(body, w.saturating_sub(4)) {
            rows.push((
                Line::from(vec![Span::raw("    "), Span::styled(line, Style::default().fg(palette().muted))]),
                Some(Hit::Mode(mode)),
            ));
        }
        rows.push((Line::raw(""), None));
    }
    rows.push((preview_line(&form.inner), None));
}

/// "Step N of 3" at the left, Back and the page's primary button at the
/// right.
fn footer(f: &mut Frame, form: &mut Form, rect: Rect) {
    let w = rect.width as usize;
    let page = form.inner.page();
    let focus = form.inner.focus();
    let buttons = [
        Button::secondary(shared::BACK).focused(focus == Field::Back),
        Button::primary(page.primary()).focused(focus == Field::Primary),
    ];
    let right_w = button::row_width(&buttons).min(w);
    let (right, at) = button::row(&buttons, right_w, Align::Left);
    let step = Span::styled(page.step_label(), Style::default().fg(palette().muted));
    let left = [Line::raw(""), Line::from(step), Line::raw("")];
    let lines = button::beside(left, right, w, right_w);
    let x0 = rect.x + w.saturating_sub(right_w) as u16;
    for (i, line) in lines.into_iter().enumerate() {
        let y = rect.y + i as u16;
        if y >= rect.bottom() {
            break;
        }
        f.render_widget(Paragraph::new(line), Rect { y, height: 1, ..rect });
    }
    for ((x, bw), field) in at.into_iter().zip([Field::Back, Field::Primary]) {
        form.hits.push((Rect { x: x0 + x, width: bw, ..rect }, Hit::Field(field)));
    }
}

fn help_line(text: &str, w: usize) -> Line<'static> {
    Line::from(Span::styled(panel::truncate_right(text, w), Style::default().fg(palette().muted)))
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

/// The live "where this lands" preview: the derived slug and the path its
/// config will be written to.
fn preview_line(form: &shared::Form) -> Line<'static> {
    let label_style = Style::default().fg(palette().ansi_gray);
    match form.preview() {
        Some((slug, path)) => Line::from(vec![
            Span::styled("Folder / id: ", label_style),
            Span::styled(
                slug,
                Style::default().fg(palette().ansi_cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("   ({path})"), Style::default().fg(palette().ansi_dark_gray)),
        ]),
        None => Line::from(Span::styled(
            "Folder / id: (type a name to see the derived id)",
            Style::default().fg(palette().ansi_dark_gray),
        )),
    }
}

/// One labelled text field. The focused field shows a cyan cursor bar after its
/// value and an underlined label.
fn field_line(label: &str, value: &str, focused: bool) -> Line<'static> {
    let label_style = if focused {
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(palette().ansi_gray)
    };
    let mut spans = label_spans(label, label_style).to_vec();
    spans.push(Span::raw(value.to_string()));
    if focused {
        spans.push(Span::styled("▏", Style::default().fg(palette().ansi_cyan)));
    }
    Line::from(spans)
}

/// A field's label in its 11-column gutter, the focused one's underline
/// under the label's own letters only.
fn label_spans(label: &str, style: Style) -> [Span<'static>; 3] {
    crate::panel::padded_label(label, 0, 11usize.saturating_sub(label.chars().count()), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
    use shelbi_core::{AgentDefaults, RunnerKind};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A Shelbi home of its own (page 1's Continue checks it for a taken
    /// name) with a git repo `acme` in it; `f` gets the repo.
    fn with_home(f: impl FnOnce(&Path)) {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("projects")).unwrap();
        let repo = home.path().join("acme");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let prev = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", home.path());
        f(&repo);
        match prev {
            Some(v) => std::env::set_var("SHELBI_HOME", v),
            None => std::env::remove_var("SHELBI_HOME"),
        }
    }

    fn typed(f: &mut Form, text: &str) {
        for c in text.chars() {
            f.handle_key(key(KeyCode::Char(c)));
        }
    }

    const W: u16 = 90;
    const H: u16 = 30;

    fn draw(f: &mut Form) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
        term.draw(|frame| f.render(frame, frame.area())).unwrap();
        term.backend().buffer().clone()
    }

    fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Where `needle` is drawn (its first cell).
    fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
            if let Some(i) = row.find(needle) {
                let col = row[..i].chars().count() as u16;
                return (col, y);
            }
        }
        panic!("{needle:?} not drawn in:\n{}", text(buf));
    }

    fn click(f: &mut Form, buf: &Buffer, needle: &str) -> Step {
        let (x, y) = find(buf, needle);
        f.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn new_prefills_root_and_defaults() {
        let f = Form::new(Path::new("/tmp/here"));
        assert_eq!(f.name(), "");
        assert_eq!(f.root(), "/tmp/here");
        assert_eq!(f.mode(), ConfigMode::Global);
        assert_eq!(f.defaults(), AgentDefaults::default());
        assert_eq!(f.page(), Page::Folder);
        assert_eq!(f.focus(), Field::Name);
    }

    #[test]
    fn the_keyboard_walks_folder_then_agent_defaults_then_settings() {
        with_home(|repo| {
            let mut f = Form::new(repo);
            typed(&mut f, "acme");
            assert_eq!(f.handle_key(key(KeyCode::Enter)), Step::Continue);
            assert_eq!(f.page(), Page::Defaults, "the name and path validated");
            assert_eq!(f.focus(), Field::Harness);
            // ← → step the harness in place; Space opens the model's menu.
            f.handle_key(key(KeyCode::Right));
            assert_eq!(f.defaults().runner, RunnerKind::Codex);
            f.handle_key(key(KeyCode::Tab));
            f.handle_key(key(KeyCode::Char(' ')));
            assert!(f.menu.is_some());
            f.handle_key(key(KeyCode::Down));
            f.handle_key(key(KeyCode::Enter));
            assert!(f.menu.is_none());
            assert_eq!(f.defaults(), AgentDefaults { runner: RunnerKind::Codex, model: Some("gpt-5".into()) });
            assert_eq!(f.handle_key(key(KeyCode::Enter)), Step::Continue);
            assert_eq!(f.page(), Page::Settings);
            // Back keeps the choice, Continue returns.
            f.inner.set_focus(Field::Back);
            f.handle_key(key(KeyCode::Char(' ')));
            assert_eq!(f.page(), Page::Defaults);
            assert_eq!(f.defaults().model.as_deref(), Some("gpt-5"));
            f.handle_key(key(KeyCode::Enter));
            assert_eq!(f.handle_key(key(KeyCode::Enter)), Step::Submit);
            assert_eq!(f.handle_key(key(KeyCode::Esc)), Step::Cancel);
        });
    }

    #[test]
    fn a_taken_name_stops_on_page_one() {
        with_home(|repo| {
            let home = std::path::PathBuf::from(std::env::var_os("SHELBI_HOME").unwrap());
            std::fs::write(home.join("projects/acme.yaml"), "name: acme\n").unwrap();
            let mut f = Form::new(repo);
            typed(&mut f, "acme");
            f.handle_key(key(KeyCode::Enter));
            assert_eq!(f.page(), Page::Folder);
            assert!(f.inner.error().is_some_and(|e| e.contains("already")), "{:?}", f.inner.error());
            assert!(text(&draw(&mut f)).contains("already lives at"));
        });
    }

    #[test]
    fn a_custom_model_is_typed_in_the_box() {
        with_home(|repo| {
            let mut f = Form::new(repo);
            typed(&mut f, "acme");
            f.handle_key(key(KeyCode::Enter));
            f.handle_key(key(KeyCode::Tab));
            f.handle_key(key(KeyCode::Char(' ')));
            // Up from the first row wraps to Custom model….
            f.handle_key(key(KeyCode::Up));
            f.handle_key(key(KeyCode::Enter));
            assert_eq!(f.custom.as_deref(), Some(""));
            typed(&mut f, "claude-opus-5-5");
            assert!(text(&draw(&mut f)).contains("claude-opus-5-5"));
            f.handle_key(key(KeyCode::Enter));
            assert_eq!(f.defaults().model.as_deref(), Some("claude-opus-5-5"));
        });
    }

    #[test]
    fn the_mouse_clicks_every_control() {
        with_home(|repo| {
            let mut f = Form::new(repo);
            typed(&mut f, "acme");
            let buf = draw(&mut f);
            assert_eq!(click(&mut f, &buf, "Continue"), Step::Continue);
            assert_eq!(f.page(), Page::Defaults);

            // The harness box opens its menu; a row chooses.
            let buf = draw(&mut f);
            click(&mut f, &buf, "Claude Code");
            let buf = draw(&mut f);
            assert!(text(&buf).contains("Codex  OpenAI"), "the menu is open:\n{}", text(&buf));
            click(&mut f, &buf, "Codex  OpenAI");
            assert_eq!(f.defaults().runner, RunnerKind::Codex);

            // The model box too.
            let buf = draw(&mut f);
            click(&mut f, &buf, "Use runner default");
            let buf = draw(&mut f);
            click(&mut f, &buf, "GPT-5 Codex");
            assert_eq!(f.defaults().model.as_deref(), Some("gpt-5-codex"));

            // Back, then Continue twice, then a location row, Create Project.
            let buf = draw(&mut f);
            click(&mut f, &buf, "Back");
            assert_eq!(f.page(), Page::Folder);
            let buf = draw(&mut f);
            click(&mut f, &buf, "Continue");
            let buf = draw(&mut f);
            click(&mut f, &buf, "Continue");
            assert_eq!(f.page(), Page::Settings);
            let buf = draw(&mut f);
            click(&mut f, &buf, wiz::IN_REPO_TITLE);
            assert_eq!(f.mode(), ConfigMode::InRepo);
            let buf = draw(&mut f);
            assert_eq!(click(&mut f, &buf, "Create Project"), Step::Submit);
            assert_eq!(f.defaults(), AgentDefaults { runner: RunnerKind::Codex, model: Some("gpt-5-codex".into()) });
        });
    }

    #[test]
    fn page_two_shows_the_agent_defaults_copy() {
        with_home(|repo| {
            let mut f = Form::new(repo);
            typed(&mut f, "acme");
            f.handle_key(key(KeyCode::Enter));
            let out = text(&draw(&mut f));
            for needle in [
                "Agent defaults",
                "Choose the provider and model that agents should use by default.",
                "Default Harness",
                "Claude Code  Anthropic",
                "Default model",
                "Use runner default",
                "Used for new Orchestrator sessions in Shelbi.",
                "Step 2 of 3",
                "Back",
                "Continue",
            ] {
                assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
            }
            assert_eq!(out.matches("Used for new Orchestrator sessions in Shelbi.").count(), 2);
        });
    }

    #[test]
    fn page_one_and_three_paint_their_fields() {
        with_home(|repo| {
            let mut f = Form::new(repo);
            typed(&mut f, "acme");
            let out = text(&draw(&mut f));
            for needle in ["Add a project", "Name", "Repo path", "Detected:", "Folder / id:", "Step 1 of 3"] {
                assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
            }
            f.handle_key(key(KeyCode::Enter));
            f.handle_key(key(KeyCode::Enter));
            let out = text(&draw(&mut f));
            for needle in ["Where should settings live?", "In the project", "Outside the project", "Step 3 of 3", "Create Project"] {
                assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
            }
        });
    }

    #[test]
    fn a_focused_label_underlines_its_letters_not_its_gutter() {
        let line = field_line("Name", "alpha", true);
        let underlined: String = line
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::UNDERLINED))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(underlined, "Name");
        let gutter = &line.spans[2];
        assert!(gutter.content.trim().is_empty() && !gutter.content.is_empty());
        assert!(gutter.style.sub_modifier.contains(Modifier::UNDERLINED), "the gutter clears it");
    }

    #[test]
    fn renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        with_home(|repo| {
            crate::theme::with_appearance(Appearance::Light, || {
                let mut f = Form::new(repo);
                typed(&mut f, "acme");
                for name in ["add-project-folder", "add-project-defaults", "add-project-settings"] {
                    let buf = draw(&mut f);
                    crate::theme::assert_light_readable(name, &buf);
                    f.handle_key(key(KeyCode::Enter));
                }
            });
        });
    }
}
