//! The TUI's launch screen and Connect to Remote Session (`launch-tui`).
//!
//! `shelbi` started outside a registered project, with no daemon running,
//! shows the launch screen ([`run`]) instead of the onboarding prompt: the
//! projects on this machine and the recent remote sessions
//! ([`shelbi_app::launch::LaunchModel`]), Add project and Connect to remote.
//! Opening a project starts the daemon and hands back the project for
//! [`crate::run_main`] to open, as `shelbi` in that project's folder would;
//! connecting to a remote session hands that back for [`open_remote`].
//! Inside a project, or with a daemon running, the CLI never comes here.
//!
//! Connecting is offered only when this build has a connector
//! ([`remote_connector`]); that is what turns the client's
//! `CommandModel::remote_connect` on, and with it Connect to remote, the
//! recent sessions' Connect and the palette's "Connect to Remote
//! Session…".
//!
//! Three pieces, each usable on its own, so the shell's palette can open
//! them too ([`crate::shell`]: "Connect to Remote Session…", "Show Launch
//! Screen"):
//!
//! - [`screen::LaunchScreen`]: the screen, its keys and mouse, and the
//!   off-thread open and reconnect;
//! - [`connect::ConnectDialog`]: the two-step dialog over it;
//! - [`rows`]: the project and session rows both draw.
//!
//! [`LaunchApp`] is the standalone screen: the launch screen with the
//! add-project form ([`crate::overlay::add_project`], the shell's Add
//! project) or the Connect dialog over it, without a terminal of its own, so
//! tests drive it over a `TestBackend`.

pub mod connect;
pub(crate) mod rows;
pub mod screen;

#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{backend::CrosstermBackend, widgets::Clear, Frame, Terminal};
use shelbi_app::launch::{DaemonStarter, LaunchModel, RecentRemotes, RemoteConnector, RemoteSession, SharedDaemon};
use shelbi_core::IssueTrackerConfig;
use shelbi_orchestrator::project_create;

pub use connect::{ConnectDialog, ConnectStep};
pub use screen::{LaunchScreen, LaunchStep, Notice, Target};

use crate::overlay::{self, add_project};

/// How the launch screen ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchExit {
    /// Open this project (the daemon is running).
    Open(String),
    /// Connected to this remote session: open it ([`open_remote`]).
    OpenRemote(RemoteSession),
    /// The user left.
    Quit,
}

/// The connector this build reaches other machines through, `None` while
/// there's none (connecting isn't offered). The SSH connector
/// (`launch-remote-connect`) goes here.
pub fn remote_connector() -> Option<Arc<dyn RemoteConnector>> {
    None
}

/// Open `session`, connected to from the launch screen or Connect to Remote
/// Session, the way a local project opens. The one place the remote
/// session's opening goes (`launch-remote-connect` fills it in): the
/// foundation's `RemoteConnector::connect` doesn't hand back anything to
/// open yet. Unreachable while [`remote_connector`] is `None`.
pub fn open_remote(session: &RemoteSession) -> Result<()> {
    anyhow::bail!(
        "opening {} on {} isn't wired up in this build",
        session.display_label(),
        session.host
    )
}

/// What's drawn over the launch screen.
enum Layer {
    AddProject(add_project::Form),
    Connect(Box<ConnectDialog>),
}

/// The standalone launch screen and the dialogs it opens.
pub struct LaunchApp {
    screen: LaunchScreen,
    layer: Option<Layer>,
    /// The add-project scaffold running off the UI thread: the created
    /// project's id, or why it failed.
    create_job: Option<Receiver<Result<String, String>>>,
    /// `None`: connecting isn't offered.
    remote: Option<Arc<dyn RemoteConnector>>,
    recents: RecentRemotes,
    cwd: PathBuf,
    emoji: bool,
}

impl LaunchApp {
    pub fn new(
        model: LaunchModel,
        daemon: Arc<dyn DaemonStarter>,
        remote: Option<Arc<dyn RemoteConnector>>,
        recents: RecentRemotes,
        cwd: &Path,
        emoji: bool,
    ) -> Self {
        LaunchApp {
            screen: LaunchScreen::new(model, daemon, remote.clone(), recents.clone(), emoji),
            layer: None,
            create_job: None,
            remote,
            recents,
            cwd: cwd.to_path_buf(),
            emoji,
        }
    }

    pub fn screen(&self) -> &LaunchScreen {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut LaunchScreen {
        &mut self.screen
    }

    /// The Connect dialog, when it's open.
    pub fn connect_dialog(&self) -> Option<&ConnectDialog> {
        match &self.layer {
            Some(Layer::Connect(d)) => Some(d),
            _ => None,
        }
    }

    /// Whether the add-project form is open.
    pub fn adding_project(&self) -> bool {
        matches!(self.layer, Some(Layer::AddProject(_)))
    }

    /// One terminal event: `Some` when the screen is done.
    pub fn handle_event(&mut self, ev: Event) -> Option<LaunchExit> {
        match ev {
            Event::Key(key) if key.kind != KeyEventKind::Release => match self.layer.as_mut() {
                Some(Layer::AddProject(form)) => {
                    match form.handle_key(key) {
                        add_project::Step::Continue => {}
                        add_project::Step::Cancel => self.layer = None,
                        add_project::Step::Submit => self.submit_project(),
                    }
                    None
                }
                Some(Layer::Connect(dialog)) => {
                    let step = dialog.handle_key(key);
                    self.connect_step(step)
                }
                None => {
                    let step = self.screen.handle_key(key);
                    self.launch_step(step)
                }
            },
            Event::Mouse(m) => match self.layer.as_mut() {
                Some(Layer::Connect(dialog)) => {
                    let step = dialog.handle_mouse(m);
                    self.connect_step(step)
                }
                // The form takes keys only; a click elsewhere keeps it.
                Some(Layer::AddProject(_)) => None,
                None => {
                    if self.create_job.is_some() && m.kind == MouseEventKind::Down(MouseButton::Left) {
                        return None;
                    }
                    let step = self.screen.handle_mouse(m);
                    self.launch_step(step)
                }
            },
            Event::Paste(text) => {
                if let Some(Layer::Connect(dialog)) = self.layer.as_mut() {
                    dialog.paste(&text);
                }
                None
            }
            _ => None,
        }
    }

    /// Fold in what finished off the UI thread: `(redraw, exit)`.
    pub fn tick(&mut self) -> (bool, Option<LaunchExit>) {
        let mut redraw = false;
        if let Some(Layer::Connect(dialog)) = self.layer.as_mut() {
            if let Some(step) = dialog.poll() {
                redraw = true;
                if let Some(exit) = self.connect_step(step) {
                    return (true, Some(exit));
                }
            }
        }
        if let Some(step) = self.screen.poll() {
            redraw = true;
            if let Some(exit) = self.launch_step(step) {
                return (true, Some(exit));
            }
        }
        if let Some(exit) = self.poll_create() {
            return (true, Some(exit));
        }
        (redraw || self.create_job.is_some(), None)
    }

    fn launch_step(&mut self, step: LaunchStep) -> Option<LaunchExit> {
        match step {
            LaunchStep::Stay => None,
            LaunchStep::Open(slug) => Some(LaunchExit::Open(slug)),
            LaunchStep::Connected(session) => Some(LaunchExit::OpenRemote(session)),
            LaunchStep::Quit => Some(LaunchExit::Quit),
            LaunchStep::AddProject => {
                self.layer = Some(Layer::AddProject(add_project::Form::new(&self.cwd)));
                None
            }
            LaunchStep::ConnectDialog => {
                if let Some(remote) = self.remote.clone() {
                    let dialog = ConnectDialog::new(remote, self.recents.clone(), self.emoji);
                    self.layer = Some(Layer::Connect(Box::new(dialog)));
                }
                None
            }
        }
    }

    fn connect_step(&mut self, step: ConnectStep) -> Option<LaunchExit> {
        match step {
            ConnectStep::Stay => None,
            ConnectStep::Close => {
                self.layer = None;
                None
            }
            ConnectStep::Connected(session) => {
                self.layer = None;
                Some(LaunchExit::OpenRemote(session))
            }
        }
    }

    /// Read the lists again from disk.
    fn reload(&mut self) {
        match LaunchModel::load(&self.recents) {
            Ok(model) => self.screen.set_model(model),
            Err(e) => self.screen.set_notice(Notice::Error(e.to_string())),
        }
    }

    /// Validate the form; scaffold the project off the UI thread, or say
    /// what's wrong on the form.
    fn submit_project(&mut self) {
        let Some(Layer::AddProject(form)) = self.layer.as_mut() else {
            return;
        };
        let mode = form.mode();
        match project_create::validate_add_project(form.name(), form.root(), &self.cwd) {
            Ok(resolved) => {
                self.layer = None;
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new().name("shelbi-add-project".into()).spawn(move || {
                    let slug = resolved.name.clone();
                    let created = project_create::scaffold_project(
                        &resolved,
                        mode,
                        &IssueTrackerConfig::default(),
                        &mut project_create::NullReporter,
                    );
                    let _ = tx.send(created.map(|()| slug).map_err(|e| format!("Add project failed: {e}")));
                });
                match spawned {
                    Ok(_) => {
                        self.create_job = Some(rx);
                        self.screen.set_notice(Notice::Info("Creating project…".to_string()));
                    }
                    Err(e) => self.screen.set_notice(Notice::Error(format!("Add project failed: {e}"))),
                }
            }
            Err(msg) => form.set_error(msg),
        }
    }

    /// The scaffold came back: open the new project (`shelbi` starts the
    /// daemon as it opens it), or say why it failed.
    fn poll_create(&mut self) -> Option<LaunchExit> {
        let result = match self.create_job.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("add project stopped unexpectedly".to_string()),
        };
        self.create_job = None;
        match result {
            Ok(slug) => Some(LaunchExit::Open(slug)),
            Err(msg) => {
                self.screen.set_notice(Notice::Error(msg));
                self.reload();
                None
            }
        }
    }

    /// Draw the screen and whatever is over it.
    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.screen.render(f.buffer_mut(), area);
        // Dimmed under a dialog, as the shell dims its view under one.
        if self.layer.is_some() {
            crate::shell::dim_area(f.buffer_mut(), area);
        }
        match self.layer.as_mut() {
            Some(Layer::AddProject(form)) => {
                let rect = overlay::centered_pct(area, 70, 60, 48, 14);
                f.render_widget(Clear, rect);
                form.render(f, rect);
            }
            Some(Layer::Connect(dialog)) => {
                let rect = dialog.rect(area);
                dialog.render(f, rect);
            }
            None => {}
        }
    }
}

/// Run the launch screen in this terminal until a project is chosen or the
/// user leaves. `notice` is said under the lists from the start (why
/// `shelbi` came here rather than to a project).
pub fn run(notice: Option<String>) -> Result<LaunchExit> {
    let caps = crate::shell::caps::Caps::detect_fast();
    crate::theme::install_from_config();
    let recents = RecentRemotes::live();
    let model = LaunchModel::load(&recents).map_err(|e| anyhow::anyhow!(e))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut app = LaunchApp::new(model, Arc::new(SharedDaemon), remote_connector(), recents, &cwd, caps.emoji);
    if let Some(text) = notice {
        app.screen_mut().set_notice(Notice::Info(text));
    }

    let _guard = crate::shell::RawGuard::enter().context("entering raw mode")?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout())).context("initializing the terminal")?;
    term.draw(|f| app.draw(f))?;
    loop {
        let mut dirty = false;
        if event::poll(Duration::from_millis(50)).unwrap_or(false) {
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
