//! The launch screen itself ([`LaunchScreen`]): the shared
//! [`LaunchModel`] drawn in the TUI's language, its keyboard and mouse, and
//! the off-thread opening and reconnecting.
//!
//! Connecting is offered only with a [`RemoteConnector`] to connect through
//! (the client's `CommandModel::remote_connect`): without one, Connect to
//! remote and the recent sessions' Connect aren't shown, and those rows
//! don't take the keyboard.
//!
//! ```text
//!  ████████████████████████████████
//!  ██                            ██    Projects on this Mac
//!  ██  Welcome to Shelbi         ██   ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
//!  ██                            ██    📁 Shelbi
//!  ██  Open a project on this    ██       ~/Workspaces/shelbi
//!  ██  Mac or connect to Shelbi  ██   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
//!  ██  on another machine.       ██    📁 ContextStore
//!  ██                            ██       ~/Workspaces/ContextStore
//!  ██  ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄  ██
//!  ██      + Add project         ██    Recent remote sessions
//!  ██  ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀  ██
//!  ██  ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄  ██    📁 Shelbi                    Connect
//!  ██    ≡ Connect to remote     ██       jlong@devbox:~/Workspaces/shelbi
//!  ██  ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀  ██
//!  ████████████████████████████████    ↑↓ Move   Tab Next   ⏎ Open   Esc Quit
//! ```
//!
//! The left panel is filled ([`Theme::dialog`], the desktop's lighter
//! column); the right panel is the terminal's own background. Below
//! [`STACK_BELOW`] columns the left panel becomes a band across the top,
//! its buttons side by side, and the lists run under it. With nothing to
//! list, the right panel is the "No projects yet" note, and Add project is
//! the Primary button with the keyboard.
//!
//! Keys: ↑/↓ (and j/k) move within a column, ←/→ between them, Tab / ⇧Tab
//! through everything; Return (or Space) presses: a project opens (the
//! daemon starts first, off the UI thread), Add project and Connect to
//! remote hand over to their dialogs, a recent session's row connects.
//! Esc, q or Ctrl+C leaves. A click presses what it lands on (a remote
//! row's body only takes the keyboard; its Connect connects), the pointer
//! lifts what it's over, and the wheel scrolls a list taller than the
//! screen.
//!
//! [`Theme::dialog`]: shelbi_app::theme::Theme::dialog

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use shelbi_app::launch::{
    self, DaemonStarter, LaunchError, LaunchModel, LaunchOutcome, LaunchState, RecentRemotes, RemoteConnector,
    RemoteSession,
};
use shelbi_app::overlay::error_log::wrap;
use shelbi_state::keymap::{Key, Mods};

use super::rows::{self, Item, Lit};
use crate::button::{self, Align, Button};
use crate::panel;
use crate::theme::palette;

/// Narrower than this, the panels stack.
pub const STACK_BELOW: u16 = 90;
/// The left panel's width beside the lists: about a third of the screen
/// (the desktop's 320 of 900), within these bounds.
const LEFT_W: (u16, u16) = (32, 44);
/// The columns between a panel's edge and its content.
const PAD_X: u16 = 3;

/// The left panel's buttons, with their icons.
fn add_label() -> String {
    format!("+ {}", launch::ADD_PROJECT_LABEL)
}

fn connect_label() -> String {
    format!("≡ {}", launch::CONNECT_REMOTE_LABEL)
}

/// The keys under the lists.
const HINT: &str = "↑↓ Move   Tab Next   ⏎ Open   Esc Quit";

/// A welcome button in its lines: the line, column, width, and target.
type ButtonAt = (usize, u16, u16, Target);
/// A row in the lists: its first line, its target, and its chip's column
/// and width.
type RowAt = (usize, Target, Option<(u16, u16)>);

/// What has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    AddProject,
    ConnectRemote,
    Project(usize),
    Remote(usize),
}

/// What a click lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Target(Target),
    /// A recent session's Connect chip.
    Reconnect(usize),
}

/// What the screen's host does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchStep {
    Stay,
    /// The daemon is up: open this project.
    Open(String),
    /// Open the add-project form.
    AddProject,
    /// Open the Connect to Remote Session dialog.
    ConnectDialog,
    /// Connected to this recent session (now first in the list): open it
    /// ([`super::open_remote`]).
    Connected(RemoteSession),
    /// Esc, q or Ctrl+C.
    Quit,
}

/// A line under the lists: what's happening, or what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
}

/// The open launch screen.
pub struct LaunchScreen {
    model: LaunchModel,
    focus: Target,
    hover: Option<Hit>,
    /// Where each control was drawn last.
    hits: Vec<(Rect, Hit)>,
    /// The lists' first line in view.
    scroll: usize,
    /// The last frame's list height, for the wheel.
    list_view: (usize, usize),
    notice: Option<Notice>,
    job: Option<Receiver<Result<LaunchOutcome, LaunchError>>>,
    daemon: Arc<dyn DaemonStarter>,
    /// `None`: connecting isn't offered.
    remote: Option<Arc<dyn RemoteConnector>>,
    recents: RecentRemotes,
    emoji: bool,
}

impl LaunchScreen {
    /// The screen over `model`: the keyboard on the first project, else
    /// Add project.
    pub fn new(
        model: LaunchModel,
        daemon: Arc<dyn DaemonStarter>,
        remote: Option<Arc<dyn RemoteConnector>>,
        recents: RecentRemotes,
        emoji: bool,
    ) -> Self {
        let focus = if model.projects.is_empty() { Target::AddProject } else { Target::Project(0) };
        LaunchScreen {
            model,
            focus,
            hover: None,
            hits: Vec::new(),
            scroll: 0,
            list_view: (0, 0),
            notice: None,
            job: None,
            daemon,
            remote,
            recents,
            emoji,
        }
    }

    pub fn model(&self) -> &LaunchModel {
        &self.model
    }

    pub fn focus(&self) -> Target {
        self.focus
    }

    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Say `notice` under the lists.
    pub fn set_notice(&mut self, notice: Notice) {
        self.notice = Some(notice);
    }

    /// Whether a project is opening or a session connecting.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// Read the lists again (after a project was added or a session
    /// connected), keeping the keyboard where it was when it still can be.
    pub fn set_model(&mut self, model: LaunchModel) {
        self.model = model;
        self.focus = match self.focus {
            Target::Project(i) if i < self.model.projects.len() => Target::Project(i),
            Target::Remote(i) if i < self.model.remotes.len() => Target::Remote(i),
            Target::Project(_) | Target::Remote(_) => self.rows().first().copied().unwrap_or(Target::AddProject),
            other => other,
        };
        self.hover = None;
    }

    /// Whether connecting is offered.
    pub fn can_connect(&self) -> bool {
        self.remote.is_some()
    }

    /// Everything that takes the keyboard, in Tab order.
    fn order(&self) -> Vec<Target> {
        let mut out = vec![Target::AddProject];
        if self.can_connect() {
            out.push(Target::ConnectRemote);
        }
        out.extend(self.rows());
        out
    }

    /// The right column's targets, top to bottom.
    fn rows(&self) -> Vec<Target> {
        let mut out: Vec<Target> = (0..self.model.projects.len()).map(Target::Project).collect();
        if self.can_connect() {
            out.extend((0..self.model.remotes.len()).map(Target::Remote));
        }
        out
    }

    fn step_focus(&mut self, forward: bool) {
        let order = self.order();
        let at = order.iter().position(|t| *t == self.focus).unwrap_or(0);
        let n = order.len();
        self.focus = order[if forward { (at + 1) % n } else { (at + n - 1) % n }];
    }

    /// ↑/↓: within the focused column, stopping at its ends.
    fn move_vertical(&mut self, down: bool) {
        match self.focus {
            Target::AddProject if down && self.can_connect() => self.focus = Target::ConnectRemote,
            Target::ConnectRemote if !down => self.focus = Target::AddProject,
            Target::AddProject | Target::ConnectRemote => {}
            row => {
                let rows = self.rows();
                let Some(at) = rows.iter().position(|t| *t == row) else {
                    return;
                };
                let to = if down { (at + 1).min(rows.len() - 1) } else { at.saturating_sub(1) };
                self.focus = rows[to];
            }
        }
    }

    /// A key press.
    pub fn handle_key(&mut self, ev: KeyEvent) -> LaunchStep {
        let Some(chord) = crate::keymap::chord_from_event(ev) else {
            return LaunchStep::Stay;
        };
        let bare = chord.mods.is_empty() || chord.mods == Mods::SHIFT;
        match chord.code {
            Key::Char('c') if chord.mods == Mods::CONTROL => LaunchStep::Quit,
            Key::Esc | Key::Char('q') if bare => LaunchStep::Quit,
            // Nothing else while a project opens or a session connects.
            _ if self.busy() => LaunchStep::Stay,
            Key::Tab if chord.mods.is_empty() => {
                self.step_focus(true);
                LaunchStep::Stay
            }
            Key::BackTab => {
                self.step_focus(false);
                LaunchStep::Stay
            }
            Key::Tab if chord.mods == Mods::SHIFT => {
                self.step_focus(false);
                LaunchStep::Stay
            }
            Key::Up | Key::Char('k') if bare => {
                self.move_vertical(false);
                LaunchStep::Stay
            }
            Key::Down | Key::Char('j') if bare => {
                self.move_vertical(true);
                LaunchStep::Stay
            }
            Key::Left | Key::Char('h') if bare => {
                self.focus = Target::AddProject;
                LaunchStep::Stay
            }
            Key::Right | Key::Char('l') if bare => {
                if matches!(self.focus, Target::AddProject | Target::ConnectRemote) {
                    if let Some(first) = self.rows().first() {
                        self.focus = *first;
                    }
                }
                LaunchStep::Stay
            }
            Key::Enter | Key::Char(' ') if bare => self.press(self.focus),
            _ => LaunchStep::Stay,
        }
    }

    /// Press `target`: open, hand over, or connect.
    pub fn press(&mut self, target: Target) -> LaunchStep {
        if self.busy() {
            return LaunchStep::Stay;
        }
        if !self.order().contains(&target) {
            return LaunchStep::Stay;
        }
        self.focus = target;
        match target {
            Target::AddProject => LaunchStep::AddProject,
            Target::ConnectRemote => LaunchStep::ConnectDialog,
            Target::Project(i) => {
                let Some(project) = self.model.projects.get(i) else {
                    return LaunchStep::Stay;
                };
                let notice = format!("Opening {}…", project.label);
                let (slug, daemon) = (project.slug.clone(), self.daemon.clone());
                self.run(notice, move || launch::open_project(slug, daemon.as_ref()));
                LaunchStep::Stay
            }
            Target::Remote(i) => {
                let (Some(recent), Some(remote)) = (self.model.remotes.get(i), self.remote.clone()) else {
                    return LaunchStep::Stay;
                };
                let session = recent.session.clone();
                let notice = format!("Connecting to {} on {}…", session.display_label(), session.host);
                let recents = self.recents.clone();
                self.run(notice, move || {
                    launch::connect_remote(session, remote.as_ref(), &recents).map(LaunchOutcome::Connected)
                });
                LaunchStep::Stay
            }
        }
    }

    /// Run `call` off the UI thread, saying `notice` meanwhile.
    fn run(&mut self, notice: String, call: impl FnOnce() -> Result<LaunchOutcome, LaunchError> + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new().name("shelbi-launch".into()).spawn(move || {
            let _ = tx.send(call());
        });
        match spawned {
            Ok(_) => {
                self.job = Some(rx);
                self.notice = Some(Notice::Info(notice));
            }
            Err(e) => self.notice = Some(Notice::Error(e.to_string())),
        }
    }

    /// The job's answer, when it came back: the project to open, the session
    /// connected, or the reason under the lists (`Some(Stay)`, to redraw).
    pub fn poll(&mut self) -> Option<LaunchStep> {
        let result = match self.job.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(LaunchError::Daemon("stopped unexpectedly".into())),
        };
        self.job = None;
        Some(match result {
            Ok(LaunchOutcome::OpenProject(slug)) => {
                self.notice = None;
                LaunchStep::Open(slug)
            }
            Ok(LaunchOutcome::Connected(session)) => {
                self.notice = None;
                LaunchStep::Connected(session)
            }
            Ok(LaunchOutcome::AddProject) => LaunchStep::AddProject,
            Ok(LaunchOutcome::ConnectDialog) => LaunchStep::ConnectDialog,
            Err(e) => {
                self.notice = Some(Notice::Error(e.to_string()));
                LaunchStep::Stay
            }
        })
    }

    /// A mouse event (screen cells).
    pub fn handle_mouse(&mut self, m: MouseEvent) -> LaunchStep {
        let at = self.hit(m.column, m.row);
        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = at;
                LaunchStep::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => match at {
                // A remote row's body takes the keyboard; its Connect connects.
                Some(Hit::Target(Target::Remote(i))) => {
                    if !self.busy() && self.can_connect() {
                        self.focus = Target::Remote(i);
                    }
                    LaunchStep::Stay
                }
                Some(Hit::Target(target)) => self.press(target),
                Some(Hit::Reconnect(i)) => self.press(Target::Remote(i)),
                None => LaunchStep::Stay,
            },
            MouseEventKind::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                LaunchStep::Stay
            }
            MouseEventKind::ScrollDown => {
                let (total, view) = self.list_view;
                self.scroll = (self.scroll + 1).min(total.saturating_sub(view));
                LaunchStep::Stay
            }
            _ => LaunchStep::Stay,
        }
    }

    fn hit(&self, col: u16, row: u16) -> Option<Hit> {
        // The last drawn wins: a chip over its row.
        self.hits.iter().rev().find(|(r, _)| panel::in_rect(*r, col, row)).map(|(_, h)| *h)
    }

    /// Draw the screen over `area`.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) {
        self.hits.clear();
        if area.width < 4 || area.height < 3 {
            return;
        }
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].reset();
            }
        }
        // The bottom row: what's happening, else the keys.
        let foot = Rect { y: area.bottom() - 1, height: 1, ..area };
        let body = Rect { height: area.height - 1, ..area };
        let (welcome, lists) = if area.width >= STACK_BELOW {
            let w = (area.width * 36 / 100).clamp(LEFT_W.0, LEFT_W.1);
            let left = Rect { width: w, ..body };
            let right = Rect { x: body.x + w, width: body.width - w, ..body };
            self.welcome(buf, left, false);
            (left, right)
        } else {
            let band = self.welcome_band_height(body.width).min(body.height);
            let top = Rect { height: band, ..body };
            self.welcome(buf, top, true);
            (top, Rect { y: body.y + band, height: body.height - band, ..body })
        };
        let lists = Rect {
            x: lists.x + PAD_X,
            width: lists.width.saturating_sub(2 * PAD_X),
            y: lists.y + u16::from(lists.height > 8),
            height: lists.height.saturating_sub(u16::from(lists.height > 8) * 2),
        };
        match self.model.state() {
            LaunchState::NoProjects => self.empty(buf, lists),
            LaunchState::Projects => self.lists(buf, lists),
        }
        let foot = if area.width >= STACK_BELOW {
            Rect { x: welcome.right() + PAD_X, width: area.right().saturating_sub(welcome.right() + 2 * PAD_X), ..foot }
        } else {
            Rect { x: foot.x + PAD_X, width: foot.width.saturating_sub(2 * PAD_X), ..foot }
        };
        self.footer(buf, foot);
    }

    /// The welcome's lines (title, intro, buttons) `width` wide: stacked
    /// buttons beside the lists, a row of them in the band.
    fn welcome_lines(&self, width: usize, band: bool) -> (Vec<Line<'static>>, Vec<ButtonAt>) {
        let p = palette();
        let mut lines = vec![Line::from(Span::styled(
            panel::truncate_right(launch::WELCOME_TITLE, width),
            Style::default().fg(p.white).add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::raw(""));
        for l in wrap(launch::WELCOME_INTRO, width.max(1)) {
            lines.push(Line::from(Span::styled(l, Style::default().fg(p.gray))));
        }
        lines.push(Line::raw(""));
        let (add_text, connect_text) = (add_label(), connect_label());
        let empty = self.model.state() == LaunchState::NoProjects;
        let hovered = |t: Target| self.hover == Some(Hit::Target(t));
        let add = if empty { Button::primary(&add_text) } else { Button::secondary(&add_text) }
            .focused(self.focus == Target::AddProject)
            .hovered(hovered(Target::AddProject));
        let connect = Button::secondary(&connect_text)
            .focused(self.focus == Target::ConnectRemote)
            .hovered(hovered(Target::ConnectRemote));
        let mut controls = Vec::new();
        let mut buttons = vec![(add, Target::AddProject)];
        if self.can_connect() {
            buttons.push((connect, Target::ConnectRemote));
        }
        let shown: Vec<Button> = buttons.iter().map(|(b, _)| *b).collect();
        if band && button::row_width(&shown) <= width {
            let (row, at) = button::row(&shown, width, Align::Left);
            let first = lines.len();
            lines.extend(row);
            for ((x, w), (_, t)) in at.into_iter().zip(&buttons) {
                controls.push((first, x, w, *t));
            }
        } else {
            for (b, t) in buttons {
                controls.push((lines.len(), 0, width as u16, t));
                lines.extend(b.lines_in(width));
            }
        }
        (lines, controls)
    }

    /// The band's rows when the panels stack: its content and a row above
    /// and below.
    fn welcome_band_height(&self, width: u16) -> u16 {
        let w = width.saturating_sub(2 * PAD_X) as usize;
        self.welcome_lines(w, true).0.len() as u16 + 2
    }

    /// The left panel (or the band), its content centered down it.
    fn welcome(&mut self, buf: &mut Buffer, rect: Rect, band: bool) {
        let fill = palette().dialog;
        for y in rect.top()..rect.bottom() {
            for x in rect.left()..rect.right() {
                buf[(x, y)].set_bg(fill);
            }
        }
        let content = Rect {
            x: rect.x + PAD_X,
            width: rect.width.saturating_sub(2 * PAD_X),
            ..rect
        };
        let (lines, controls) = self.welcome_lines(content.width as usize, band);
        let h = lines.len() as u16;
        let y = content.y + content.height.saturating_sub(h) / 2;
        let at = Rect { y, height: h.min(content.bottom().saturating_sub(y)), ..content };
        Paragraph::new(lines).render(at, buf);
        for (line, x, w, t) in controls {
            let top = y + line as u16;
            if top + button::BUTTON_H as u16 <= rect.bottom() {
                self.hits.push((Rect { x: content.x + x, y: top, width: w, height: button::BUTTON_H as u16 }, Hit::Target(t)));
            }
        }
    }

    /// The two lists, or what fits of them, scrolled to the keyboard's row.
    fn lists(&mut self, buf: &mut Buffer, rect: Rect) {
        let p = palette();
        let width = rect.width as usize;
        let heading = |text: &str| {
            Line::from(Span::styled(panel::truncate_right(text, width), Style::default().fg(p.white).add_modifier(Modifier::BOLD)))
        };
        let lit = |focus: Option<usize>, hover: Option<usize>, chip: Option<usize>| Lit { focused: focus, hovered: hover, action_hovered: chip };
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut places: Vec<RowAt> = Vec::new();

        if !self.model.projects.is_empty() {
            lines.push(heading(launch::PROJECTS_HEADING));
            let items: Vec<Item> = self
                .model
                .projects
                .iter()
                .map(|p| Item { title: &p.label, detail: &p.display_path, action: None })
                .collect();
            let focus = match self.focus {
                Target::Project(i) => Some(i),
                _ => None,
            };
            let hover = match self.hover {
                Some(Hit::Target(Target::Project(i))) => Some(i),
                _ => None,
            };
            let base = lines.len();
            let (rows, at) = rows::lines(&items, lit(focus, hover, None), width, self.emoji);
            lines.extend(rows);
            places.extend(at.iter().enumerate().map(|(i, a)| (base + a.line, Target::Project(i), None)));
        }
        if !self.model.remotes.is_empty() {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(heading(launch::REMOTES_HEADING));
            let addresses: Vec<String> = self.model.remotes.iter().map(|r| r.session.address()).collect();
            let items: Vec<Item> = self
                .model
                .remotes
                .iter()
                .zip(&addresses)
                .map(|(r, a)| Item {
                    title: r.session.display_label(),
                    detail: a,
                    action: self.can_connect().then_some(launch::RECONNECT_LABEL),
                })
                .collect();
            let focus = match self.focus {
                Target::Remote(i) => Some(i),
                _ => None,
            };
            let (hover, chip) = match self.hover {
                Some(Hit::Target(Target::Remote(i))) => (Some(i), None),
                Some(Hit::Reconnect(i)) => (Some(i), Some(i)),
                _ => (None, None),
            };
            let base = lines.len();
            let (rows, at) = rows::lines(&items, lit(focus, hover, chip), width, self.emoji);
            lines.extend(rows);
            places.extend(at.iter().enumerate().map(|(i, a)| (base + a.line, Target::Remote(i), a.action)));
        }

        let total = lines.len();
        let view = rect.height as usize;
        // Keep the keyboard's row (and its pill) in view.
        if let Some(&(line, _, _)) = places.iter().find(|(_, t, _)| *t == self.focus) {
            let (top, bottom) = (line.saturating_sub(1), line + 3);
            if top < self.scroll {
                self.scroll = top;
            } else if bottom > self.scroll + view {
                self.scroll = bottom.saturating_sub(view);
            }
            // The first row brings its heading into view.
            if line <= 2 {
                self.scroll = 0;
            }
        }
        self.scroll = self.scroll.min(total.saturating_sub(view));
        self.list_view = (total, view);
        // Centered down the panel when it all fits, as the desktop's.
        let y = if total <= view { rect.y + ((view - total) / 2) as u16 } else { rect.y };
        let shown: Vec<Line> = lines.into_iter().skip(self.scroll).take(view).collect();
        Paragraph::new(shown).render(Rect { y, height: rect.height.min(rect.bottom() - y), ..rect }, buf);
        for (line, target, chip) in places {
            for n in 0..2 {
                let Some(row) = (line + n).checked_sub(self.scroll).filter(|r| *r < view) else {
                    continue;
                };
                if matches!(target, Target::Remote(_)) && !self.can_connect() {
                    continue;
                }
                let row_y = y + row as u16;
                if row_y >= rect.bottom() {
                    continue;
                }
                self.hits.push((Rect { x: rect.x, y: row_y, width: rect.width, height: 1 }, Hit::Target(target)));
                if let (0, Some((x, w)), Target::Remote(i)) = (n, chip, target) {
                    self.hits.push((Rect { x: rect.x + x, y: row_y, width: w, height: 1 }, Hit::Reconnect(i)));
                }
            }
        }
    }

    /// "No projects yet": the folder, the title, the line under it,
    /// centered.
    fn empty(&self, buf: &mut Buffer, rect: Rect) {
        let p = palette();
        let width = rect.width as usize;
        let center = |text: String, style: Style| {
            let text = panel::truncate_right(&text, width);
            let left = width.saturating_sub(panel::display_width(&text)) / 2;
            Line::from(vec![Span::raw(" ".repeat(left)), Span::styled(text, style)])
        };
        let mut lines = vec![
            center(rows::folder(self.emoji).trim_end().to_string(), Style::default().fg(p.muted)),
            Line::raw(""),
            center(launch::EMPTY_TITLE.to_string(), Style::default().fg(p.white).add_modifier(Modifier::BOLD)),
            Line::raw(""),
        ];
        for l in wrap(launch::EMPTY_BODY, width.clamp(1, 48)) {
            lines.push(center(l, Style::default().fg(p.muted)));
        }
        let h = lines.len() as u16;
        let y = rect.y + rect.height.saturating_sub(h) / 2;
        Paragraph::new(lines).render(Rect { y, height: h.min(rect.bottom().saturating_sub(y)), ..rect }, buf);
    }

    /// The bottom row: the notice, else the keys.
    fn footer(&self, buf: &mut Buffer, rect: Rect) {
        let p = palette();
        let width = rect.width as usize;
        let (text, style) = match &self.notice {
            Some(Notice::Info(text)) => (text.as_str(), Style::default().fg(p.gray)),
            Some(Notice::Error(text)) => (text.as_str(), Style::default().fg(p.red)),
            None => (HINT, Style::default().fg(p.muted)),
        };
        let line = Line::from(Span::styled(panel::truncate_right(text, width), style));
        Paragraph::new(line).render(rect, buf);
    }
}
