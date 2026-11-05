//! Shelbi's own diff view in the TUI: the [`shelbi_app::diff`] session, drawn
//! into the main area of a review or a workspace.
//!
//! Everything about what a key does lives in the shared [`DiffState`]; this is
//! the terminal's half: crossterm events in, ratatui cells out ([`paint`]), and
//! the effects only a client can carry out.
//!
//! **Keys the diff takes from the shell** while it has focus: any chord bound in
//! the `diff` keymap context wins over a global, and a mode that takes text
//! (search, a comment, a palette) takes everything. So `Ctrl-p` is the diff's
//! file palette and Shelbi's own palette stays on `Ctrl-Space`; `Tab` is the hunk
//! filter, not the panel focus toggle, and the panel stays on `Esc` and `Ctrl-h`.

mod paint;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use shelbi_app::diff::backend::backend_for;
use shelbi_app::diff::comments::Author;
use shelbi_app::diff::state::normalize_key;
use shelbi_app::diff::{DiffAction, DiffEffect, DiffOptions, DiffSession, DiffTarget, Mode, TargetKind, WORKSPACE_WATCH};
use shelbi_orchestrator::agent_send;
use shelbi_state::keymap::{DisplayStyle, Key, KeyChord, Keymaps, Mods};

use self::paint::{layout, paint, Colors, DiffLayout, Paint, NARROW_BELOW};

/// How long a review's diff waits for its worktree to be checked out, as View
/// Diff's external tool does ([`shelbi_app::review::diff_load`]).
const READY_LIMIT: Duration = Duration::from_secs(15);

/// How often it looks while it waits.
const READY_POLL: Duration = Duration::from_millis(500);

/// Rows a wheel notch scrolls.
const WHEEL_ROWS: isize = 3;

/// Opens the view's session. Called again to retry an open that failed.
pub type DiffOpener = Arc<dyn Fn() -> DiffSession + Send + Sync>;

/// Copies text to the clipboard. A seam so a test doesn't write to the real one.
type Clipboard = fn(&str);

/// What the view reads from the shell's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffSetup {
    pub keymaps: Keymaps,
    pub style: DisplayStyle,
}

impl Default for DiffSetup {
    fn default() -> Self {
        DiffSetup { keymaps: Keymaps::default(), style: DisplayStyle::Linux }
    }
}

/// What the interface around the view has to do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOut {
    None,
    /// `Esc` with nothing to let go of: give the keyboard back to the panel.
    FocusPanel,
    /// Open this file in the user's editor, at this line.
    Edit { path: PathBuf, line: Option<u32> },
    /// `gY` in a review: ask for Reject's reason, starting from this.
    Reject { reason: String },
}

/// One review's or workspace's diff view.
pub struct DiffView {
    open: DiffOpener,
    session: DiffSession,
    setup: DiffSetup,
    /// The worktree on this machine. `None` when it's on another, where `Ctrl-g`
    /// has no local file to open.
    worktree: Option<String>,
    /// What an empty changeset says.
    empty: &'static str,
    /// The first file the list shows.
    list_top: usize,
    /// Whether the last frame was narrow: crossing the line resets the list.
    narrow: Option<bool>,
    /// Where the last frame put everything, for the mouse.
    last: Option<DiffLayout>,
    copy: Clipboard,
}

/// The TUI's loop polls every frame, so nothing has to wake it.
fn no_wake() -> shelbi_app::diff::Wake {
    Arc::new(|| {})
}

impl DiffView {
    /// A view over whatever `open` opens.
    pub fn new(open: DiffOpener, setup: DiffSetup, worktree: Option<String>, empty: &'static str) -> Self {
        let session = open();
        DiffView {
            open,
            session,
            setup,
            worktree,
            empty,
            list_top: 0,
            narrow: None,
            last: None,
            copy: super::copy_to_clipboard,
        }
    }

    /// Opens a review's diff: once its worktree holds the task's branch. A slot
    /// is assigned before its branch is checked out, and until then the diff
    /// would be the previous task's.
    pub fn review_opener(project: &str, slot: &str, task: &str) -> DiffOpener {
        let target = DiffTarget {
            project: project.to_string(),
            workspace: slot.to_string(),
            kind: TargetKind::Review,
            task: Some(task.to_string()),
        };
        Arc::new(move || {
            let target = target.clone();
            let options = DiffOptions { author: Author::local(), target: target.clone(), watch: None, wake: no_wake() };
            DiffSession::open(
                move || {
                    wait_for_review(&target)?;
                    backend_for(&target, None, agent_send::service())
                },
                options,
            )
        })
    }

    /// Opens a workspace's live diff.
    pub fn workspace_opener(project: &str, workspace: &str, task: Option<String>) -> DiffOpener {
        let target = DiffTarget {
            project: project.to_string(),
            workspace: workspace.to_string(),
            kind: TargetKind::Workspace,
            task,
        };
        Arc::new(move || {
            let target = target.clone();
            let options =
                DiffOptions { author: Author::local(), target: target.clone(), watch: Some(WORKSPACE_WATCH), wake: no_wake() };
            DiffSession::open(move || backend_for(&target, None, agent_send::service()), options)
        })
    }

    /// The keymap changed in Settings.
    pub fn set_setup(&mut self, setup: DiffSetup) {
        self.setup = setup;
    }

    /// The view went on or off screen. Off, a live diff stops reading.
    pub fn set_showing(&mut self, showing: bool) {
        self.session.set_visible(showing);
    }

    /// Take in what the session's workers finished. Returns whether to redraw.
    pub fn poll(&mut self) -> bool {
        self.session.poll()
    }

    /// Whether this view takes `k` ahead of the shell's global keys. Shelbi's
    /// palette stays reachable whatever the diff is doing.
    pub fn claims(&self, k: &KeyEvent) -> bool {
        if super::terminal_view::is_focus_key(k) {
            return false;
        }
        let Some(chord) = crate::keymap::chord_from_event(*k) else { return false };
        self.session.state().wants_key(chord, &self.setup.keymaps.diff)
    }

    /// Whether the keyboard has nothing to let go of: Esc means leave.
    fn idle(&self) -> bool {
        let state = self.session.state();
        matches!(state.mode(), Mode::Normal)
            && state.pending().is_none()
            && state.count().is_none()
            && state.query().is_empty()
    }

    /// The first read failed, so the session behind this has nothing to retry
    /// with: a backend that wouldn't open ended its workers.
    fn open_failed(&self) -> bool {
        let state = self.session.state();
        state.error().is_some() && state.table().is_empty()
    }

    /// A key.
    pub fn key(&mut self, k: KeyEvent) -> DiffOut {
        let Some(chord) = crate::keymap::chord_from_event(k) else { return DiffOut::None };
        if chord == KeyChord::new(Key::Esc, Mods::NONE) && self.idle() {
            return DiffOut::FocusPanel;
        }
        let keymap = &self.setup.keymaps.diff;
        if self.open_failed() && keymap.dispatch(normalize_key(chord)) == Some(DiffAction::ReplyOrRefresh) {
            self.session = (self.open)();
            return DiffOut::None;
        }
        let effects = self.session.key(chord, keymap);
        self.carry_out(effects)
    }

    /// A paste. Only a mode taking text has anywhere to put it; a line break in
    /// a comment is its `Ctrl-J`, since a bare one would save.
    pub fn paste(&mut self, text: &str) {
        let takes_lines = matches!(self.session.state().mode(), Mode::Comment(_));
        if !takes_lines && !matches!(self.session.state().mode(), Mode::Search { .. } | Mode::Palette(_)) {
            return;
        }
        for c in text.chars() {
            let chord = match c {
                '\n' if takes_lines => KeyChord::new(Key::Char('j'), Mods::CONTROL),
                '\n' | '\r' => continue,
                c => KeyChord::new(Key::Char(c), Mods::NONE),
            };
            let effects = self.session.key(chord, &self.setup.keymaps.diff);
            self.carry_out(effects);
        }
    }

    /// Carry out what's left for a client: a copy, an editor, the reject prompt.
    fn carry_out(&mut self, effects: Vec<DiffEffect>) -> DiffOut {
        let mut out = DiffOut::None;
        for effect in effects {
            match effect {
                DiffEffect::Yank(text) => {
                    (self.copy)(&text);
                    self.session.state_mut().set_flash("Copied");
                }
                DiffEffect::OpenInEditor { path, line } => match self.local_file(&path) {
                    Some(path) => out = DiffOut::Edit { path, line },
                    None => self.session.state_mut().set_flash(format!("{path} isn't on this machine")),
                },
                DiffEffect::RejectWithComments { reason } => out = DiffOut::Reject { reason },
                // The session's own: it never hands these back.
                DiffEffect::Refresh
                | DiffEffect::LoadScope(_)
                | DiffEffect::LoadBlame { .. }
                | DiffEffect::SaveComment(_)
                | DiffEffect::SaveViewed(_)
                | DiffEffect::SendToAgent { .. }
                | DiffEffect::PlanRevert(_)
                | DiffEffect::Revert(_) => {}
            }
        }
        out
    }

    fn local_file(&self, path: &str) -> Option<PathBuf> {
        let full = Path::new(self.worktree.as_deref()?).join(path);
        full.exists().then_some(full)
    }

    /// A mouse event inside the view.
    pub fn mouse(&mut self, m: MouseEvent) {
        let Some(at) = self.last else { return };
        if !matches!(self.session.state().mode(), Mode::Normal | Mode::Visual { .. }) {
            return;
        }
        let over_list = at.list.is_some_and(|list| contains(list, m.column, m.row));
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = matches!(m.kind, MouseEventKind::ScrollDown);
                if over_list {
                    let last = self.session.state().changeset().files.len().saturating_sub(1);
                    self.list_top = if down {
                        (self.list_top + WHEEL_ROWS as usize).min(last)
                    } else {
                        self.list_top.saturating_sub(WHEEL_ROWS as usize)
                    };
                } else {
                    self.session.state_mut().scroll_by(if down { WHEEL_ROWS } else { -WHEEL_ROWS });
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if at.commits.is_some_and(|r| contains(r, m.column, m.row)) {
                    let effects = self.session.action(DiffAction::PickCommit);
                    self.carry_out(effects);
                } else if contains(at.files, m.column, m.row) {
                    self.click_file(at.files, m);
                } else if contains(at.rows, m.column, m.row) {
                    let top = self.session.state().top();
                    self.session.state_mut().click_row(top + usize::from(m.row - at.rows.y));
                }
            }
            _ => {}
        }
    }

    /// A click on a file row: its check column toggles viewed, the rest jumps.
    fn click_file(&mut self, files: Rect, m: MouseEvent) {
        let index = self.list_top + usize::from(m.row - files.y);
        let Some(file) = self.session.state().changeset().files.get(index).cloned() else { return };
        if m.column == files.x {
            let viewed = !self.session.state().is_viewed(&file);
            let effects = self.session.state_mut().set_viewed(index, viewed);
            let effects = self.session.carry_out(effects);
            self.carry_out(effects);
        } else {
            self.session.state_mut().goto_file(index);
        }
    }

    /// Draw into `area`. Returns where the terminal's cursor belongs.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect, truecolor: bool, focused: bool) -> Option<(u16, u16)> {
        // A narrow view starts without the list, and the toggle overrides that
        // until the view crosses the line again.
        let narrow = area.width < NARROW_BELOW;
        if self.narrow != Some(narrow) {
            self.narrow = Some(narrow);
            self.session.state_mut().set_file_list(!narrow);
        }
        let at = layout(area, self.session.state().file_list_open());
        self.session.state_mut().set_viewport(usize::from(at.rows.height));
        self.keep_file_in_view(&at);
        self.last = Some(at);
        let colors = Colors::new(truecolor);
        let p = Paint {
            state: self.session.state(),
            keymaps: &self.setup.keymaps,
            style: self.setup.style,
            colors,
            focused,
            store_error: self.session.store_error(),
            empty: self.empty,
        };
        paint(&p, buf, area, &at, self.list_top)
    }

    /// Scroll the list so the current file shows, when the cursor moved it off.
    fn keep_file_in_view(&mut self, at: &DiffLayout) {
        let Some(current) = self.session.state().current_file() else { return };
        let height = usize::from(at.files.height);
        if height == 0 {
            return;
        }
        if current < self.list_top {
            self.list_top = current;
        } else if current >= self.list_top + height {
            self.list_top = current + 1 - height;
        }
    }

    /// The session, for tests.
    #[cfg(test)]
    pub(crate) fn session(&mut self) -> &mut DiffSession {
        &mut self.session
    }

    #[cfg(test)]
    pub(crate) fn set_clipboard(&mut self, copy: Clipboard) {
        self.copy = copy;
    }
}

/// Wait for a review's worktree to be checked out. Runs on the session's loader
/// thread: "Loading diff…" shows meanwhile.
fn wait_for_review(target: &DiffTarget) -> Result<(), String> {
    let Some(task) = target.task.as_deref() else { return Ok(()) };
    let deadline = Instant::now() + READY_LIMIT;
    loop {
        match shelbi_orchestrator::review_session::diff_ready(&target.project, task) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= deadline => return Err(e.to_string()),
            Err(_) => std::thread::sleep(READY_POLL),
        }
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}
