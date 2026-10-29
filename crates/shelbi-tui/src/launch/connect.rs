//! Connect to Remote Session in the TUI: the desktop's two-step dialog in
//! the dialogs' bordered panel ([`crate::overlay::dialog_frame`]), opened
//! from the launch screen's Connect to remote and from the palette.
//!
//! [`ConnectFlow`] (shared with the desktop) holds the steps and decides
//! what a press does; this draws it, keeps the keyboard, types into the
//! host field, and runs the two blocking calls (find the host's sessions,
//! connect to one) off the UI thread ([`ConnectDialog::poll`]).
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────────┐
//!  │                                                              │
//!  │ Connect to Remote Session                                    │
//!  │ Connect to Shelbi running on another machine.                │
//!  │                                                              │
//!  │ SSH host                                                     │
//!  │ ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄ │
//!  │  devbox█                                                     │
//!  │ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀ │
//!  │ Use an SSH alias or user@hostname. Your existing SSH keys    │
//!  │ and configuration are used.                                  │
//!  │                                                              │
//!  │                             ▄▄▄▄▄▄▄▄▄▄ ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄ │
//!  │                               Cancel     Find Sessions       │
//!  │                             ▀▀▀▀▀▀▀▀▀▀ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀ │
//!  └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! The Choose session step swaps the field for "✓ Connected as <user>" with
//! Change Host beside it, then the host's sessions (the launch screen's
//! rows, [`super::rows`]), and Connect for Find Sessions. A refusal or a
//! failed call is said in red under the field or the list.
//!
//! Keys: Tab / ⇧Tab walk the controls and never leave the dialog; ↑/↓ move
//! between sessions; Return presses (in the field, Find Sessions; on a
//! session, its Connect); Esc cancels. A click works on every control, a
//! session's body picks it and its Connect connects.
//!
//! A host whose Shelbi isn't running says so under the list, and the
//! primary button becomes Start Shelbi: it starts the host's daemon (only
//! then), and connects once it's up.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Clear,
    Frame,
};
use shelbi_app::launch::connect::{ConnectFlow, Stage};
use shelbi_app::launch::remote::{self, RemoteError};
use shelbi_app::launch::{connect_remote, LaunchError, RecentRemotes, RemoteConnector, RemoteSession};
use shelbi_app::text_area::TextArea;
use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::rows::{self, Item, Lit};
use crate::button::{self, Align, Button};
use crate::modal_form::{Control, FormLines};
use crate::overlay::{centered_rect, dialog_frame, DIALOG_PAD_X};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// The panel's width (the desktop's 580pt), less on a narrower terminal.
pub const PANEL_W: u16 = 66;
/// The columns between the panel's edge and its content.
const PAD_X: u16 = 1 + DIALOG_PAD_X;

/// What has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Host,
    ChangeHost,
    Session(usize),
    Cancel,
    Primary,
}

/// What a click lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Control(Focus),
    /// A session's Connect.
    Connect(usize),
}

/// What the dialog's host does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectStep {
    Stay,
    /// Cancelled.
    Close,
    /// Connected to this session (and remembered it): close.
    Connected(RemoteSession),
}

enum Job {
    Find(String, Receiver<Result<(String, Vec<RemoteSession>), RemoteError>>),
    Connect(Receiver<Result<RemoteSession, LaunchError>>),
    Start(Receiver<Result<(), RemoteError>>),
}

/// The open dialog.
pub struct ConnectDialog {
    flow: ConnectFlow,
    host: TextArea,
    focus: Focus,
    hover: Option<Hit>,
    hits: Vec<(Rect, Hit)>,
    job: Option<Job>,
    remote: Arc<dyn RemoteConnector>,
    recents: RecentRemotes,
    emoji: bool,
}

impl ConnectDialog {
    /// The Host step, the field empty and typed in.
    pub fn new(remote: Arc<dyn RemoteConnector>, recents: RecentRemotes, emoji: bool) -> Self {
        ConnectDialog {
            flow: ConnectFlow::new(),
            host: TextArea::default(),
            focus: Focus::Host,
            hover: None,
            hits: Vec::new(),
            job: None,
            remote,
            recents,
            emoji,
        }
    }

    pub fn flow(&self) -> &ConnectFlow {
        &self.flow
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The step's controls in Tab order.
    fn order(&self) -> Vec<Focus> {
        match self.flow.stage() {
            Stage::Host => vec![Focus::Host, Focus::Cancel, Focus::Primary],
            Stage::Choose => {
                let mut out = vec![Focus::ChangeHost];
                out.extend((0..self.flow.sessions().len()).map(Focus::Session));
                out.extend([Focus::Cancel, Focus::Primary]);
                out
            }
        }
    }

    fn set_focus(&mut self, focus: Focus) {
        if let Focus::Session(i) = focus {
            self.flow.select(i);
        }
        self.focus = focus;
    }

    fn step_focus(&mut self, forward: bool) {
        let order = self.order();
        let n = order.len();
        let at = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.set_focus(order[if forward { (at + 1) % n } else { (at + n - 1) % n }]);
    }

    /// A key press.
    pub fn handle_key(&mut self, ev: KeyEvent) -> ConnectStep {
        match crate::keymap::chord_from_event(ev) {
            Some(chord) => self.key(chord),
            None => ConnectStep::Stay,
        }
    }

    pub fn key(&mut self, chord: KeyChord) -> ConnectStep {
        let bare = chord.mods.is_empty();
        let in_field = self.focus == Focus::Host && self.flow.busy().is_none();
        match chord.code {
            Key::Esc if bare => ConnectStep::Close,
            Key::Tab if bare => {
                self.step_focus(true);
                ConnectStep::Stay
            }
            Key::BackTab => {
                self.step_focus(false);
                ConnectStep::Stay
            }
            Key::Tab if chord.mods == Mods::SHIFT => {
                self.step_focus(false);
                ConnectStep::Stay
            }
            Key::Enter if bare => self.press(self.focus),
            Key::Char(' ') if bare && !in_field => self.press(self.focus),
            Key::Up | Key::Down if bare => {
                if let Focus::Session(i) = self.focus {
                    let n = self.flow.sessions().len();
                    let to = if chord.code == Key::Down { (i + 1).min(n.saturating_sub(1)) } else { i.saturating_sub(1) };
                    self.set_focus(Focus::Session(to));
                }
                ConnectStep::Stay
            }
            _ => {
                if in_field && self.host.edit(chord, true, 1) {
                    self.flow.set_host(&self.host.text());
                }
                ConnectStep::Stay
            }
        }
    }

    /// Bracketed paste into the host field. `false` when it isn't typed in.
    pub fn paste(&mut self, text: &str) -> bool {
        if self.focus != Focus::Host || self.flow.busy().is_some() {
            return false;
        }
        self.host.insert_str(&text.replace(['\r', '\n'], ""));
        self.flow.set_host(&self.host.text());
        true
    }

    /// Press `focus`'s control.
    fn press(&mut self, focus: Focus) -> ConnectStep {
        match focus {
            Focus::Cancel => return ConnectStep::Close,
            Focus::ChangeHost => {
                self.flow.change_host();
                if self.flow.stage() == Stage::Host {
                    self.focus = Focus::Host;
                }
            }
            Focus::Host | Focus::Primary if self.flow.stage() == Stage::Host => self.find(),
            Focus::Primary if self.flow.offers_start() => self.start(),
            Focus::Primary => self.connect(None),
            Focus::Session(i) => self.connect(Some(i)),
            Focus::Host => {}
        }
        ConnectStep::Stay
    }

    fn find(&mut self) {
        let Some(host) = self.flow.find() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let remote = self.remote.clone();
        let searched = host.clone();
        let spawned = std::thread::Builder::new().name("shelbi-find-sessions".into()).spawn(move || {
            let _ = tx.send(remote.find_sessions(&searched));
        });
        match spawned {
            Ok(_) => self.job = Some(Job::Find(host, rx)),
            Err(e) => self.flow.found(&host, Err(RemoteError::Failed(e.to_string()))),
        }
    }

    fn connect(&mut self, index: Option<usize>) {
        let Some(session) = self.flow.connect(index) else {
            return;
        };
        self.spawn_connect(session);
    }

    /// Start Shelbi on the host a connect found it stopped on (the user
    /// pressed Start Shelbi), then connect.
    fn start(&mut self) {
        let Some(host) = self.flow.start() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let remote = self.remote.clone();
        let spawned = std::thread::Builder::new().name("shelbi-start-remote".into()).spawn(move || {
            let _ = tx.send(remote.start_daemon(&host));
        });
        match spawned {
            Ok(_) => self.job = Some(Job::Start(rx)),
            Err(e) => {
                self.flow.started(Err(RemoteError::Failed(e.to_string())));
            }
        }
    }

    fn spawn_connect(&mut self, session: RemoteSession) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (remote, recents) = (self.remote.clone(), self.recents.clone());
        let spawned = std::thread::Builder::new().name("shelbi-connect".into()).spawn(move || {
            let _ = tx.send(connect_remote(session, remote.as_ref(), &recents));
        });
        match spawned {
            Ok(_) => self.job = Some(Job::Connect(rx)),
            Err(e) => {
                self.flow.connected(Err(LaunchError::Remote(RemoteError::Failed(e.to_string()))));
            }
        }
    }

    /// A finished call's answer: `Some` when one came back (redraw), with
    /// [`ConnectStep::Connected`] when the connection was made.
    pub fn poll(&mut self) -> Option<ConnectStep> {
        let gone = || "stopped unexpectedly".to_string();
        match self.job.as_ref()? {
            Job::Find(host, rx) => {
                let result = match rx.try_recv() {
                    Ok(r) => r,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Err(RemoteError::Failed(gone())),
                };
                let host = host.clone();
                self.job = None;
                self.flow.found(&host, result);
                if self.flow.stage() == Stage::Choose {
                    self.set_focus(Focus::Session(0));
                }
                Some(ConnectStep::Stay)
            }
            Job::Start(rx) => {
                let result = match rx.try_recv() {
                    Ok(r) => r,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Err(RemoteError::Failed(gone())),
                };
                self.job = None;
                if let Some(session) = self.flow.started(result) {
                    self.spawn_connect(session);
                }
                Some(ConnectStep::Stay)
            }
            Job::Connect(rx) => {
                let result = match rx.try_recv() {
                    Ok(r) => r,
                    Err(TryRecvError::Empty) => return None,
                    Err(TryRecvError::Disconnected) => Err(LaunchError::Remote(RemoteError::Failed(gone()))),
                };
                self.job = None;
                let session = result.as_ref().ok().cloned();
                Some(match (self.flow.connected(result), session) {
                    (true, Some(session)) => ConnectStep::Connected(session),
                    _ => ConnectStep::Stay,
                })
            }
        }
    }

    /// A mouse event (screen cells).
    pub fn handle_mouse(&mut self, m: MouseEvent) -> ConnectStep {
        let at = self.hits.iter().rev().find(|(r, _)| panel::in_rect(*r, m.column, m.row)).map(|(_, h)| *h);
        match m.kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.hover = at;
                ConnectStep::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => match at {
                Some(Hit::Control(f @ (Focus::Host | Focus::Session(_)))) => {
                    self.set_focus(f);
                    ConnectStep::Stay
                }
                Some(Hit::Control(f)) => {
                    self.focus = f;
                    self.press(f)
                }
                Some(Hit::Connect(i)) => {
                    self.set_focus(Focus::Session(i));
                    self.press(Focus::Session(i))
                }
                None => ConnectStep::Stay,
            },
            _ => ConnectStep::Stay,
        }
    }

    /// The panel's rect in `area`: centered, as tall as its content, never
    /// taller than `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let width = PANEL_W.min(area.width.saturating_sub(2)).max(1);
        let lines = self.lines(content_width(width) as usize).lines.len() as u16;
        centered_rect(area, width, lines + 4)
    }

    /// Draw the panel into `rect` ([`Self::rect`]'s).
    pub fn render(&mut self, f: &mut Frame, rect: Rect) {
        self.hits.clear();
        f.render_widget(Clear, rect);
        if rect.width < 8 || rect.height < 4 {
            return;
        }
        dialog_frame(f.buffer_mut(), rect, Some(palette().dialog), None);
        let pad = u16::from(rect.height >= 8);
        let content = Rect {
            x: rect.x + PAD_X,
            y: rect.y + 1 + pad,
            width: content_width(rect.width),
            height: rect.height - 2 - 2 * pad,
        };
        let out = self.lines(content.width as usize);
        out.render(f, content, Hit::Control(self.focus), &mut self.hits);
    }

    fn lines(&self, width: usize) -> FormLines<Hit> {
        let mut out = FormLines::new(width).with_rest(palette().dialog_control);
        out.heading(remote::DIALOG_TITLE, &self.flow.intro());
        match self.flow.stage() {
            Stage::Host => self.host_step(&mut out),
            Stage::Choose => self.choose_step(&mut out),
        }
        out.footer();
        out.blank();
        let busy = self.flow.busy().is_some();
        let hovered = |f: Focus| self.hover == Some(Hit::Control(f));
        let cancel = Button::secondary(remote::CANCEL_LABEL)
            .focused(self.focus == Focus::Cancel)
            .hovered(hovered(Focus::Cancel));
        let primary = Button::primary(self.flow.primary_label())
            .focused(self.focus == Focus::Primary && !busy)
            .hovered(hovered(Focus::Primary))
            .enabled(!busy);
        out.buttons(&[(cancel, Hit::Control(Focus::Cancel)), (primary, Hit::Control(Focus::Primary))], Align::Right);
        out
    }

    fn host_step(&self, out: &mut FormLines<Hit>) {
        let p = palette();
        let focused = self.focus == Focus::Host;
        let busy = self.flow.busy().is_some();
        let text = self.flow.host();
        let field = Textbox::new(text)
            .focused(focused)
            .disabled(busy)
            .invalid(self.flow.error().is_some());
        let field = if focused && !busy { field.caret(Some(self.host.cursor().1)) } else { field };
        let under = match self.flow.error() {
            Some(error) => vec![(error, Style::default().fg(p.red))],
            None => vec![(remote::HOST_HINT, Style::default().fg(p.muted))],
        };
        out.field(remote::HOST_LABEL, field, focused, busy, &under, Hit::Control(Focus::Host));
    }

    fn choose_step(&self, out: &mut FormLines<Hit>) {
        let p = palette();
        let width = out.width;
        // "✓ Connected as jlong", Change Host at the right.
        let change = Button::secondary(remote::CHANGE_HOST_LABEL)
            .focused(self.focus == Focus::ChangeHost)
            .hovered(self.hover == Some(Hit::Control(Focus::ChangeHost)))
            .enabled(self.flow.busy().is_none());
        let as_user = self.flow.connected_as().unwrap_or_default();
        let left = [
            Line::raw(""),
            Line::from(vec![
                Span::styled("✓ ", Style::default().fg(p.green).add_modifier(Modifier::BOLD)),
                Span::styled(
                    panel::truncate_right(&as_user, width.saturating_sub(change.width() + 4)),
                    Style::default().fg(p.foreground),
                ),
            ]),
            Line::raw(""),
        ];
        let (right, _) = button::row(&[change], change.width(), Align::Left);
        let first = out.lines.len();
        out.lines.extend(button::beside(left, right, width, change.width()));
        let x = width.saturating_sub(change.width());
        out.controls.push(Control {
            first,
            end: first + button::BUTTON_H,
            line: first,
            x: x as u16,
            w: change.width() as u16,
            h: button::BUTTON_H as u16,
            hit: Hit::Control(Focus::ChangeHost),
        });
        out.blank();

        let sessions = self.flow.sessions();
        let addresses: Vec<String> = sessions.iter().map(RemoteSession::address).collect();
        let items: Vec<Item> = sessions
            .iter()
            .zip(&addresses)
            .map(|(s, a)| Item { title: s.display_label(), detail: a, action: Some(remote::CONNECT_LABEL) })
            .collect();
        let lit = Lit {
            focused: match self.focus {
                Focus::Session(i) => Some(i),
                _ => None,
            },
            hovered: match self.hover {
                Some(Hit::Control(Focus::Session(i)) | Hit::Connect(i)) => Some(i),
                _ => None,
            },
            action_hovered: match self.hover {
                Some(Hit::Connect(i)) => Some(i),
                _ => None,
            },
        };
        let (lines, places) = rows::lines(&items, lit, width, self.emoji);
        let base = out.lines.len();
        out.lines.extend(lines);
        for (i, place) in places.iter().enumerate() {
            let line = base + place.line;
            out.controls.push(Control {
                first: line - 1,
                end: line + 3,
                line,
                x: 0,
                w: width as u16,
                h: 2,
                hit: Hit::Control(Focus::Session(i)),
            });
            if let Some((x, w)) = place.action {
                out.controls.push(Control { first: line - 1, end: line + 3, line, x, w, h: 1, hit: Hit::Connect(i) });
            }
        }
        if let Some(error) = self.flow.error() {
            out.text(error, Style::default().fg(p.red));
        }
    }
}

fn content_width(panel_w: u16) -> u16 {
    panel_w.saturating_sub(2 * PAD_X).max(1)
}
