//! The TUI's startup requirements screen: the shared
//! [`shelbi_app::prereqs`] model in the terminal, shown before `shelbi`
//! opens anything when a tool it depends on is missing ([`gate`]).
//!
//! ```text
//!                     A few tools are missing
//!  Run the commands below in another terminal, then check again.
//!
//!   ✓ Git                                  2.39.5 · /usr/bin/git
//!
//! › ✕ GitHub CLI
//!     Not found on your PATH. Install it with Homebrew, then check again.
//!     brew install gh                                     c Copy
//!     Already installed somewhere else?  b Browse…
//!                                                                  ┃
//!   ✕ Coding agent                                                 │
//!     Shelbi runs your agents with Claude Code, Codex or OpenCode. …
//!
//!                                   ▄▄▄▄▄▄▄▄▄▄▄▄▄▄ ▄▄▄▄▄▄▄▄▄▄▄▄▄
//!                                     Skip for now   Check again
//!                                   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀ ▀▀▀▀▀▀▀▀▀▀▀▀▀
//!   ↑↓ Move   c Copy   b Browse   r Check again   s Skip   Esc Quit
//! ```
//!
//! The keyboard moves between the rows (and, in a missing coding-agent row,
//! between its Claude Code, Codex and OpenCode commands); the list scrolls to keep the
//! selection in view, with a scrollbar when it doesn't fit. `c` copies the
//! selected command (OSC 52, and `pbcopy` on macOS, as the shell copies) and
//! the line reads "Copied" for two seconds. `b` (the row's Browse…) opens its
//! path field; Return checks the path (`<bin> --version`) and remembers a
//! good one in `config.yaml`. `r` checks again, `s` asks to skip (`y`/Return skips, Esc
//! goes back), Return continues once everything is found. Esc, `q` or
//! Ctrl+C leaves `shelbi`.

#[cfg(test)]
mod tests;

use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
    Frame, Terminal,
};
use shelbi_app::overlay::error_log::wrap;
use shelbi_app::prereqs::{
    self as model, CheckEvent, Client, Found, InstallCommand, Phase, Platform, Prereqs, Probe, RowState, SystemProbe, Tool,
    ToolPaths,
};

use crate::button::{self, Align, Button};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// The widest the screen's content runs.
const MAX_W: u16 = 84;
/// Spinner frames, as the startup progress draws them.
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// How the screen ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrereqExit {
    /// Everything's there: carry on.
    Continue,
    /// Skip anyway: carry on without the missing tools.
    Skip,
    /// The user left `shelbi`.
    Quit,
}

/// Check the tools before `shelbi` opens anything: quietly when they're all
/// on this terminal's `PATH` (nothing drawn, nothing run), else on the
/// requirements screen, which checks the login shell's `PATH` too and goes
/// on by itself when that finds everything.
pub fn gate() -> Result<PrereqExit> {
    let platform = Platform::current();
    let located = shelbi_state::tool_paths::located();
    if model::quick_check(&located, platform, &SystemProbe::process()) {
        return Ok(PrereqExit::Continue);
    }
    crate::theme::install_from_config();
    let mut app = PrereqApp::new(platform, Arc::new(SystemProbe::login()), located, shelbi_core::home_dir());
    let _guard = crate::shell::RawGuard::enter().context("entering raw mode")?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout())).context("initializing the terminal")?;
    term.draw(|f| app.draw(f))?;
    loop {
        let mut dirty = false;
        if event::poll(Duration::from_millis(80)).unwrap_or(false) {
            while event::poll(Duration::ZERO).unwrap_or(false) {
                let Ok(ev) = event::read() else { break };
                if matches!(ev, Event::Resize(..)) {
                    let _ = term.autoresize();
                }
                if let Some(exit) = app.handle_event(ev) {
                    return Ok(exit);
                }
                dirty = true;
            }
        }
        let (changed, exit) = app.tick();
        if let Some(exit) = exit {
            return Ok(exit);
        }
        if dirty || changed {
            term.draw(|f| app.draw(f))?;
        }
    }
}

/// One place the keyboard can be: a row, or one of its commands.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stop {
    tool: Tool,
    command: Option<InstallCommand>,
}

/// Copies to the clipboard (a test records instead).
pub type Copier = Box<dyn FnMut(&str)>;

/// The requirements screen and its skip confirmation, without a terminal of
/// its own, so tests drive it over a `TestBackend`.
pub struct PrereqApp {
    model: Prereqs,
    probe: Arc<dyn Probe>,
    located: ToolPaths,
    home: Option<PathBuf>,
    checks: Option<Receiver<CheckEvent>>,
    validating: Option<Receiver<(Tool, Result<Found, String>)>>,
    /// The selected stop.
    focus: usize,
    /// The list's first shown line.
    scroll: usize,
    /// The path being typed in the open Locate panel.
    path: String,
    /// The command last copied, and when.
    copied: Option<(String, Instant)>,
    skip_dialog: bool,
    /// The first check finds everything: go on without a stop.
    auto_continue: bool,
    /// Saving a located path failed.
    save_error: Option<String>,
    started: Instant,
    copier: Copier,
}

impl PrereqApp {
    /// The screen, its first check already running.
    pub fn new(platform: Platform, probe: Arc<dyn Probe>, located: ToolPaths, home: Option<PathBuf>) -> Self {
        let mut app = PrereqApp {
            model: Prereqs::new(platform, Client::Tui),
            probe,
            located,
            home,
            checks: None,
            validating: None,
            focus: 0,
            scroll: 0,
            path: String::new(),
            copied: None,
            skip_dialog: false,
            auto_continue: true,
            save_error: None,
            started: Instant::now(),
            copier: Box::new(crate::shell::copy_to_clipboard),
        };
        app.check_again();
        app
    }

    /// Copy through `copier` instead of the terminal's clipboard.
    pub fn with_copier(mut self, copier: Copier) -> Self {
        self.copier = copier;
        self
    }

    pub fn model(&self) -> &Prereqs {
        &self.model
    }

    /// Whether the skip confirmation is open.
    pub fn skip_dialog_open(&self) -> bool {
        self.skip_dialog
    }

    /// The typed Locate path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Run every check again, off the UI thread.
    fn check_again(&mut self) {
        let run = self.model.begin_check();
        self.validating = None;
        self.path.clear();
        let (tx, rx) = mpsc::channel();
        let probe = self.probe.clone();
        let located = self.located.clone();
        let platform = self.model.platform();
        std::thread::spawn(move || {
            model::run_check(run, &located, platform, probe.as_ref(), |e| {
                let _ = tx.send(e);
            });
        });
        self.checks = Some(rx);
        self.focus = 0;
        self.scroll = 0;
    }

    /// Fold in what finished off the UI thread: `(redraw, exit)`.
    pub fn tick(&mut self) -> (bool, Option<PrereqExit>) {
        let mut changed = false;
        if let Some(rx) = &self.checks {
            loop {
                match rx.try_recv() {
                    Ok(event) => changed |= self.model.apply(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.checks = None;
                        break;
                    }
                }
            }
        }
        match self.model.phase() {
            Phase::Ready if self.auto_continue && self.checks.is_none() => return (true, Some(PrereqExit::Continue)),
            Phase::Missing => self.auto_continue = false,
            _ => {}
        }
        if let Some(rx) = &self.validating {
            match rx.try_recv() {
                Ok((tool, result)) => {
                    if let Ok(found) = &result {
                        if let Err(e) = model::save_located(found) {
                            self.save_error = Some(format!("Couldn't remember this location: {e}"));
                        }
                        self.located.set(found.binary.command(), Some(found.path.clone()));
                    }
                    self.model.located(tool, result);
                    self.validating = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.validating = None,
            }
        }
        if let Some((_, at)) = &self.copied {
            if at.elapsed() >= model::COPIED_FOR {
                self.copied = None;
                changed = true;
            }
        }
        // The spinner turns while a check runs.
        changed |= self.model.phase() == Phase::Checking || self.validating.is_some();
        self.clamp_focus();
        (changed, None)
    }

    /// The stops, in order.
    fn stops(&self) -> Vec<Stop> {
        let mut out = Vec::new();
        for tool in self.model.tools() {
            let commands = self.model.install_commands(tool);
            let missing = matches!(self.model.state(tool), RowState::Missing);
            if missing && self.model.locating() != Some(tool) && !commands.is_empty() {
                out.extend(commands.into_iter().map(|c| Stop { tool, command: Some(c) }));
            } else {
                out.push(Stop { tool, command: None });
            }
        }
        out
    }

    fn clamp_focus(&mut self) {
        let n = self.stops().len();
        if n > 0 && self.focus >= n {
            self.focus = n - 1;
        }
    }

    fn focused(&self) -> Option<Stop> {
        self.stops().into_iter().nth(self.focus)
    }

    /// One terminal event: `Some` when the screen is done.
    pub fn handle_event(&mut self, ev: Event) -> Option<PrereqExit> {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key),
            Event::Mouse(m) => {
                match m.kind {
                    MouseEventKind::ScrollDown => self.scroll += 1,
                    MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(1),
                    _ => {}
                }
                None
            }
            Event::Paste(text) if self.model.locating().is_some() => {
                self.path.push_str(text.trim());
                None
            }
            _ => None,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<PrereqExit> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(PrereqExit::Quit);
        }
        if self.skip_dialog {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => return Some(PrereqExit::Skip),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.skip_dialog = false,
                _ => {}
            }
            return None;
        }
        if let Some(tool) = self.model.locating() {
            // The path field has the keyboard until it's closed.
            let typing = !matches!(self.model.locate_result(), Some(Ok(_)));
            match key.code {
                KeyCode::Esc => {
                    self.model.cancel_locate();
                    self.path.clear();
                    self.validating = None;
                }
                KeyCode::Enter if typing && self.validating.is_none() => self.validate(tool),
                KeyCode::Enter => {
                    if self.model.phase() == Phase::Ready {
                        return Some(PrereqExit::Continue);
                    }
                    self.model.cancel_locate();
                }
                KeyCode::Backspace if typing => {
                    self.path.pop();
                }
                KeyCode::Char('u') if ctrl && typing => self.path.clear(),
                KeyCode::Char(c) if typing && !ctrl => self.path.push(c),
                KeyCode::Up | KeyCode::Down if !typing => {
                    self.model.cancel_locate();
                    return self.handle_key(key);
                }
                _ => {}
            }
            return None;
        }
        let phase = self.model.phase();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Some(PrereqExit::Quit),
            KeyCode::Up | KeyCode::Char('k') => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus + 1 < self.stops().len() {
                    self.focus += 1;
                }
            }
            KeyCode::PageDown => self.scroll += 5,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            KeyCode::Char('c') => self.copy(),
            KeyCode::Char('b') => {
                if let Some(stop) = self.focused() {
                    if matches!(self.model.state(stop.tool), RowState::Missing) {
                        self.model.start_locate(stop.tool);
                        self.path.clear();
                        self.save_error = None;
                    }
                }
            }
            KeyCode::Char('r') if phase != Phase::Checking => self.check_again(),
            KeyCode::Char('s') if phase == Phase::Missing => self.skip_dialog = true,
            KeyCode::Enter if phase == Phase::Ready => return Some(PrereqExit::Continue),
            _ => {}
        }
        None
    }

    fn copy(&mut self) {
        let Some(command) = self.focused().and_then(|s| s.command) else {
            return;
        };
        (self.copier)(&command.text);
        self.copied = Some((command.text, Instant::now()));
    }

    /// Check the typed path off the UI thread.
    fn validate(&mut self, tool: Tool) {
        let (tx, rx) = mpsc::channel();
        let probe = self.probe.clone();
        let input = self.path.clone();
        let home = self.home.clone();
        let platform = self.model.platform();
        std::thread::spawn(move || {
            let result = model::validate_located(tool, &input, home.as_deref(), platform, probe.as_ref());
            let _ = tx.send((tool, result));
        });
        self.validating = Some(rx);
    }

    // ---- drawing -------------------------------------------------------

    fn spinner(&self) -> char {
        SPINNER[(self.started.elapsed().as_millis() / 80) as usize % SPINNER.len()]
    }

    /// The rows' lines `width` wide, and the line each stop starts on.
    fn list_lines(&self, width: usize) -> (Vec<Line<'static>>, Vec<usize>) {
        let p = palette();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut starts = Vec::new();
        let stops = self.stops();
        let focused = stops.get(self.focus).cloned();
        let inner = width.saturating_sub(4);
        let home = self.home.as_deref();
        for (i, tool) in self.model.tools().into_iter().enumerate() {
            if i > 0 {
                lines.push(Line::raw(""));
            }
            let row_focused = focused.as_ref().is_some_and(|s| s.tool == tool && s.command.is_none());
            let state = self.model.state(tool);
            let locating = self.model.locating() == Some(tool);
            let (icon, icon_style) = match state {
                RowState::Found(_) => ('✓', Style::default().fg(p.green)),
                RowState::Missing => ('✕', Style::default().fg(p.red)),
                RowState::Checking => (self.spinner(), Style::default().fg(p.cyan)),
                RowState::Pending => ('○', Style::default().fg(p.muted)),
            };
            let name_style = match state {
                RowState::Pending => Style::default().fg(p.muted).add_modifier(Modifier::BOLD),
                _ => Style::default().fg(p.white).add_modifier(Modifier::BOLD),
            };
            let detail = match state {
                RowState::Found(f) if !locating => f.detail(home),
                RowState::Checking => model::CHECKING_ROW.to_string(),
                RowState::Pending => model::WAITING_ROW.to_string(),
                _ => String::new(),
            };
            if row_focused {
                starts.push(lines.len());
            }
            let marker = if row_focused { Span::styled("› ", Style::default().fg(p.cyan)) } else { Span::raw("  ") };
            let name = tool.name();
            let room = width.saturating_sub(4 + panel::display_width(name) + 2);
            let detail = panel::truncate_left(&detail, room);
            let gap = width.saturating_sub(4 + panel::display_width(name) + panel::display_width(&detail));
            lines.push(Line::from(vec![
                marker,
                Span::styled(format!("{icon} "), icon_style),
                Span::styled(name.to_string(), name_style),
                Span::raw(" ".repeat(gap)),
                Span::styled(detail, Style::default().fg(p.muted)),
            ]));
            let indent = || Span::raw("    ");
            if locating {
                for l in wrap(&tool.locate_body(), inner.max(1)) {
                    lines.push(Line::from(vec![indent(), Span::styled(l, Style::default().fg(p.foreground))]));
                }
                let typing = !matches!(self.model.locate_result(), Some(Ok(_)));
                let placeholder = tool.locate_placeholder();
                let field = Textbox::new(&self.path)
                    .placeholder(&placeholder)
                    .clip_left(true)
                    .caret(typing.then(|| self.path.chars().count()));
                for l in field.lines(inner) {
                    let mut spans = vec![indent()];
                    spans.extend(l.spans);
                    lines.push(Line::from(spans));
                }
                let (text, style) = match (self.validating.is_some(), self.model.locate_result()) {
                    (true, _) => (format!("{} Checking…", self.spinner()), Style::default().fg(p.muted)),
                    (false, Some(Ok(found))) => (format!("✓ {}", found.located_message()), Style::default().fg(p.green)),
                    (false, Some(Err(e))) => (format!("✕ {e}"), Style::default().fg(p.red)),
                    (false, None) => ("⏎ Check this path   Esc Cancel".to_string(), Style::default().fg(p.muted)),
                };
                for l in wrap(&text, inner.max(1)) {
                    lines.push(Line::from(vec![indent(), Span::styled(l, style)]));
                }
                if let Some(e) = &self.save_error {
                    lines.push(Line::from(vec![indent(), Span::styled(e.clone(), Style::default().fg(p.red))]));
                }
                continue;
            }
            if !matches!(state, RowState::Missing) {
                continue;
            }
            for l in wrap(self.model.missing_detail(tool), inner.max(1)) {
                lines.push(Line::from(vec![indent(), Span::styled(l, Style::default().fg(p.muted))]));
            }
            for command in self.model.install_commands(tool) {
                let selected = focused.as_ref().is_some_and(|s| s.command.as_ref() == Some(&command));
                if selected {
                    starts.push(lines.len());
                }
                lines.push(self.command_line(&command, selected, width));
            }
            lines.push(Line::from(vec![
                indent(),
                Span::styled(format!("{}  ", model::LOCATE_PROMPT), Style::default().fg(p.muted)),
                Span::styled("b ", Style::default().fg(p.cyan)),
                Span::styled(tool.locate_label(), Style::default().fg(p.white)),
            ]));
        }
        (lines, starts)
    }

    /// A command on its code fill, the selected one marked, its Copy (or
    /// Copied) at the right.
    fn command_line(&self, command: &InstallCommand, selected: bool, width: usize) -> Line<'static> {
        let p = palette();
        let copied = self.copied.as_ref().is_some_and(|(t, _)| *t == command.text);
        let marker = if selected { Span::styled("› ", Style::default().fg(p.cyan)) } else { Span::raw("  ") };
        let label = command.label.map(|l| format!("{l:<12}")).unwrap_or_default();
        let action: Vec<Span<'static>> = if copied {
            vec![Span::styled(format!("✓ {} ", model::COPIED), Style::default().fg(p.green).bg(p.code_bg))]
        } else if selected {
            vec![
                Span::styled("c ", Style::default().fg(p.cyan).bg(p.code_bg)),
                Span::styled(format!("{} ", model::COPY), Style::default().fg(p.white).bg(p.code_bg)),
            ]
        } else {
            Vec::new()
        };
        let action_w: usize = action.iter().map(|s| panel::display_width(&s.content)).sum();
        let box_w = width.saturating_sub(4 + panel::display_width(&label));
        let room = box_w.saturating_sub(2 + action_w + 1);
        let text = panel::truncate_right(&command.text, room);
        let pad = box_w.saturating_sub(1 + panel::display_width(&text) + action_w);
        let code = Style::default().fg(p.code_fg).bg(p.code_bg);
        let mut spans = vec![
            marker,
            Span::raw("  "),
            Span::styled(label, Style::default().fg(p.foreground)),
            Span::styled(" ", code),
            Span::styled(text, code),
            Span::styled(" ".repeat(pad), code),
        ];
        spans.extend(action);
        Line::from(spans)
    }

    fn footer_hint(&self) -> &'static str {
        if self.model.locating().is_some() {
            return if matches!(self.model.locate_result(), Some(Ok(_))) {
                "⏎ Done   Esc Close"
            } else {
                "⏎ Check this path   Esc Cancel"
            };
        }
        match self.model.phase() {
            Phase::Checking => "Esc Quit",
            Phase::Missing => "↑↓ Move   c Copy   b Browse   r Check again   s Skip   Esc Quit",
            Phase::Ready => "⏎ Continue   r Check again   Esc Quit",
        }
    }

    /// Draw the screen, and the skip confirmation over it.
    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.render(f.buffer_mut(), area);
        if self.skip_dialog {
            crate::shell::dim_area(f.buffer_mut(), area);
            let width = 60.min(area.width);
            let lines = self.skip_lines(width.saturating_sub(2 + 2 * crate::overlay::DIALOG_PAD_X) as usize);
            let rect = crate::overlay::centered_rect(area, width, lines.len() as u16 + 2);
            f.render_widget(Clear, rect);
            let (title, _, _) = self.model.skip_copy();
            let inner = crate::overlay::dialog_frame(f.buffer_mut(), rect, Some(palette().dialog), Some(title));
            Paragraph::new(lines).render(inner, f.buffer_mut());
        }
    }

    /// The skip confirmation's lines: what breaks, and its two buttons.
    fn skip_lines(&self, width: usize) -> Vec<Line<'static>> {
        let p = palette();
        let (_, body, footer) = self.model.skip_copy();
        let mut lines = vec![Line::raw("")];
        for l in wrap(body, width.max(1)) {
            lines.push(Line::from(Span::styled(l, Style::default().fg(p.foreground))));
        }
        for (tool, consequence) in self.model.consequences() {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("✕ ", Style::default().fg(p.red)),
                Span::styled(tool.name().to_string(), Style::default().fg(p.white).add_modifier(Modifier::BOLD)),
            ]));
            for l in wrap(consequence, width.saturating_sub(2).max(1)) {
                lines.push(Line::from(vec![Span::raw("  "), Span::styled(l, Style::default().fg(p.foreground))]));
            }
        }
        lines.push(Line::raw(""));
        for l in wrap(footer, width.max(1)) {
            lines.push(Line::from(Span::styled(l, Style::default().fg(p.muted))));
        }
        lines.push(Line::raw(""));
        let back = format!("{}  Esc", model::GO_BACK);
        let skip = format!("{}  y", model::SKIP_ANYWAY);
        let buttons = [Button::secondary(&back), Button::danger(&skip)];
        let (row, _) = button::row(&buttons, width, Align::Right);
        lines.extend(row);
        lines
    }

    /// The screen over `area`.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) {
        if area.width < 20 || area.height < 8 {
            return;
        }
        let p = palette();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].reset();
            }
        }
        let width = area.width.saturating_sub(4).min(MAX_W);
        let left = area.x + (area.width - width) / 2;
        let col = |y: u16, height: u16| Rect { x: left, y, width, height };
        let center = |text: &str, style: Style| {
            let text = panel::truncate_right(text, width as usize);
            let pad = (width as usize).saturating_sub(panel::display_width(&text)) / 2;
            Line::from(vec![Span::raw(" ".repeat(pad)), Span::styled(text, style)])
        };
        let top = area.y + u16::from(area.height > 16);
        Paragraph::new(vec![
            center(self.model.title(), Style::default().fg(p.white).add_modifier(Modifier::BOLD)),
            center(self.model.body(), Style::default().fg(p.gray)),
        ])
        .render(col(top, 2), buf);

        // The actions (three rows) and the key hints at the foot.
        let foot = area.bottom() - 1;
        let actions_y = foot.saturating_sub(button::BUTTON_H as u16);
        let labels: Vec<Button> = match self.model.phase() {
            Phase::Checking => vec![Button::primary(model::CONTINUE).enabled(false)],
            Phase::Missing => vec![Button::secondary(model::SKIP_FOR_NOW), Button::primary(model::CHECK_AGAIN)],
            Phase::Ready => vec![Button::primary(model::CONTINUE)],
        };
        let (row, _) = button::row(&labels, width as usize, Align::Right);
        Paragraph::new(row.to_vec()).render(col(actions_y, button::BUTTON_H as u16), buf);
        let hint = panel::truncate_right(self.footer_hint(), width as usize);
        buf.set_line(left, foot, &Line::from(Span::styled(hint, Style::default().fg(p.muted))), width);

        // The list between, scrolled to the selection.
        let list_y = top + 3;
        let list_h = actions_y.saturating_sub(list_y + 1);
        if list_h == 0 {
            return;
        }
        let list_w = width.saturating_sub(2);
        let (lines, starts) = self.list_lines(list_w as usize);
        let total = lines.len();
        let h = list_h as usize;
        if let Some(&at) = starts.first() {
            if at < self.scroll {
                self.scroll = at;
            } else if at + 3 > self.scroll + h {
                self.scroll = (at + 3).saturating_sub(h);
            }
        }
        self.scroll = self.scroll.min(total.saturating_sub(h));
        let shown: Vec<Line<'static>> = lines.into_iter().skip(self.scroll).take(h).collect();
        Paragraph::new(shown).render(Rect { width: list_w, ..col(list_y, list_h) }, buf);
        if total > h {
            // The scrollbar: a thumb on a track down the right edge.
            let x = left + width - 1;
            let thumb_h = (h * h / total).max(1);
            let thumb_y = (self.scroll * h).div_ceil(total).min(h - thumb_h);
            for i in 0..h {
                let (glyph, color) = if i >= thumb_y && i < thumb_y + thumb_h { ("┃", p.gray) } else { ("│", p.divider) };
                buf.set_string(x, list_y + i as u16, glyph, Style::default().fg(color));
            }
        }
    }
}
