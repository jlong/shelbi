//! Shelbi's own diff view on the desktop (`ndv-desktop`): the Review view's
//! Diff tab and the Workspace view's View Diff when `diff_tool` is `builtin`,
//! over the shared [`shelbi_app::diff`] (its keys are Skim's, from the `diff`
//! keymap context; the TUI draws the same state).
//!
//! The view holds a [`DiffSession`] and draws what it says:
//!
//! ```text
//! ┌ file list ──────────┬ the rows ([`body::DiffBody`]) ────────────────────┐
//! │ Files 7 · +412 −37  │ ▾ M crates/shelbi-app/src/review.rs     +42 −3 ✓  │
//! │ Commits: all ▾      │ ▾ @@ -120,6 +120,9 @@ impl ReviewDecisions        │
//! │ ✓ M review.rs +42 −3│  120  120 │     fn begin(&mut self, id: &str) {   │
//! ├─────────────────────┴───────────────────────────────────────────────────┤
//! │ NORMAL · unified · all lines · 1/7 · ? help                             │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! - **Keys** go to the session, which reads them through the keymap and answers
//!   with what only a client can do (copy, open the editor, ask for Reject's
//!   reason); sending to the agent and reverting are its own. The window asks
//!   [`DiffView::wants_key`] before taking a chord for itself, so the diff's
//!   `Ctrl-p` beats the window's palette chord while it has focus; Ctrl+Space
//!   and ⌘K still open the window's palette.
//! - **The worker threads** wake the view through a channel; nothing on the UI
//!   thread waits on git.
//! - **The file list** can hold the keyboard ([`DiffView::list_focus`], after a
//!   click on it or the file list key opening it): `j` / `k` step through the
//!   files, `Space` marks one viewed, `Enter` or `Esc` go back to the rows.
//!   The rest of the keys work as usual from there.
//! - **The pull request** is rows like any other: the description block above the
//!   first file and GitHub's threads at their lines (`DiffState::pr_row`,
//!   `thread_tag`), with the `i` panel over the view. `gh` is the session's.
//! - **Esc** lets go of what's held (a count, a selection, a search's marks);
//!   with nothing held it hands the keyboard back to the host's panel.

mod body;
#[cfg(any(test, feature = "visual-check"))]
pub mod canned;
mod overlays;
pub mod text;

use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable,
    Global, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, ScrollStrategy, ScrollWheelEvent, Task,
    UniformListScrollHandle, Window,
};
use shelbi_app::diff::comments::Author;
use shelbi_app::diff::{
    DiffAction, DiffEffect, DiffSession, DiffState, DiffTarget, FileStatus, Mode, Row, Wake,
};
use shelbi_app::source::ProjectHandle;
use shelbi_state::keymap::{Key, KeyChord, Mods, ModeKeymap};

use crate::assets::{heroicon, icons};
use crate::keys::{chord_from_keystroke, overlay_chord};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;
use crate::views::external_edit;

pub use body::{BodyGeometry, DiffBody};

/// The file list's width.
const LIST_W: f32 = 280.0;
/// A file row's height.
const FILE_ROW_H: f32 = 28.0;
/// The status bar's height.
const STATUS_H: f32 = 26.0;

/// The `diff` keymap as loaded: the window keeps it current from `keys.yaml`.
#[derive(Clone)]
pub struct DiffKeys(pub Rc<ModeKeymap<DiffAction>>);

impl Global for DiffKeys {}

/// The keys in effect.
pub fn diff_keys(cx: &gpui::App) -> Rc<ModeKeymap<DiffAction>> {
    match cx.try_global::<DiffKeys>() {
        Some(keys) => keys.0.clone(),
        None => Rc::new(shelbi_state::keymap::validate_keymaps_yaml("", None).0.diff),
    }
}

/// Opens a session on a target: the real workers, or a test's.
pub type OpenDiff = Rc<dyn Fn(DiffTarget, Wake) -> DiffSession>;

/// Opens a worktree file in the user's editor, at a line.
pub type OpenFile = Arc<dyn Fn(&str, Option<u32>) -> Result<(), String> + Send + Sync>;

/// What the view needs from the outside world.
#[derive(Clone)]
pub struct DiffDeps {
    pub open: OpenDiff,
    pub open_file: OpenFile,
    /// Why this window can't open a diff's files, when it's on a project on
    /// another machine.
    pub far: Option<String>,
}

impl DiffDeps {
    /// The real thing, for a window on `project`.
    pub fn live(project: &ProjectHandle) -> Self {
        let link = project.link.clone();
        DiffDeps {
            open: Rc::new(move |target, wake| {
                DiffSession::for_target(
                    target,
                    Some(link.clone()),
                    Author::local(),
                    shelbi_orchestrator::agent_send::service(),
                    wake,
                )
            }),
            open_file: Arc::new(open_in_editor),
            far: project.key().host().map(shelbi_app::remote_window::local_open_unavailable),
        }
    }

    /// Over a patch instead of a worktree: what the tests and the visual
    /// checks draw.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn canned(patch: &str, opened: canned::Opened) -> Self {
        Self::canned_with(patch, None, opened)
    }

    /// [`Self::canned`], with a pull request (or why GitHub won't answer).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn canned_with(
        patch: &str,
        pr: Option<Result<shelbi_app::diff::pr::PrDetails, String>>,
        opened: canned::Opened,
    ) -> Self {
        let patch = patch.to_string();
        DiffDeps {
            // No wake: a gpui test scheduler refuses one from another thread.
            // A test or a visual check looks with `poll_now`.
            open: Rc::new(move |target, _wake| {
                let mut canned = canned::Canned::new(&patch);
                if let Some(pr) = pr.clone() {
                    canned = canned.with_pr(pr);
                }
                let backend = Arc::new(canned);
                let options = shelbi_app::diff::DiffOptions {
                    author: Author::human("jlong"),
                    target,
                    watch: None,
                    wake: Arc::new(|| {}),
                };
                DiffSession::open(move || Ok(backend), options)
            }),
            open_file: Arc::new(move |path, line| {
                opened.lock().unwrap().push((path.to_string(), line));
                Ok(())
            }),
            far: None,
        }
    }
}

/// Open `path` in the user's editor at `line`, the launch the terminals' Cmd+click
/// uses. A GUI editor's child is reaped when it exits.
fn open_in_editor(path: &str, line: Option<u32>) -> Result<(), String> {
    let (editor, _) = shelbi_state::resolve_editor_choice();
    let launch = external_edit::launch_choice(&editor, std::path::Path::new(path), line, None);
    if let Some(mut child) = external_edit::spawn(&launch)? {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}

/// Where the diff's files are, for the editor jump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePlace {
    /// A worktree on this machine.
    Local(String),
    /// Somewhere an editor here can't reach, with what to say about it.
    Elsewhere(String),
}

/// What the view tells its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffViewEvent {
    /// The view took keyboard focus.
    Focused,
    /// Esc with nothing to let go of: the host's panel takes the keyboard.
    ReleaseFocus,
    /// `gY` in a review: ask for Reject's reason, starting from this.
    Reject(String),
}

/// The view.
pub struct DiffView {
    target: DiffTarget,
    place: FilePlace,
    pub(super) session: DiffSession,
    open_file: OpenFile,
    pub(super) focus_handle: FocusHandle,
    /// The file list holds the keyboard.
    list_focus: bool,
    list_scroll: UniformListScrollHandle,
    /// The file the list last scrolled to.
    list_file: Option<usize>,
    /// How far long lines are scrolled sideways.
    pub(super) scroll_x: Pixels,
    /// The wheel's rows, not yet a whole one.
    wheel_rows: f32,
    pub(super) geometry: BodyGeometry,
    /// Take the keyboard at the next frame (the host swapped this in while
    /// its terminal had it).
    wants_focus: bool,
    _pump: Task<()>,
}

impl EventEmitter<DiffViewEvent> for DiffView {}

impl DiffView {
    pub fn new(target: DiffTarget, place: FilePlace, deps: &DiffDeps, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<()>();
        let wake: Wake = Arc::new(move || {
            let _ = tx.unbounded_send(());
        });
        let session = (deps.open)(target.clone(), wake);
        let pump = cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                // A burst of wakes (a file's highlights each) is one look.
                while rx.try_recv().is_ok() {}
                let alive = this.update(cx, |this, cx| {
                    if this.session.poll() {
                        let effects = this.session.take_effects();
                        this.client_effects(effects, cx);
                        this.after_change();
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        DiffView {
            target,
            place,
            session,
            open_file: deps.open_file.clone(),
            focus_handle: cx.focus_handle(),
            list_focus: false,
            list_scroll: UniformListScrollHandle::new(),
            list_file: None,
            scroll_x: px(0.0),
            wheel_rows: 0.0,
            geometry: BodyGeometry::default(),
            wants_focus: false,
            _pump: pump,
        }
    }

    pub fn target(&self) -> &DiffTarget {
        &self.target
    }

    pub fn state(&self) -> &DiffState {
        self.session.state()
    }

    /// Look at what the workers finished, without a wake.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn poll_now(&mut self, cx: &mut Context<Self>) {
        if self.session.poll() {
            let effects = self.session.take_effects();
            self.client_effects(effects, cx);
            self.after_change();
            cx.notify();
        }
    }

    pub fn list_focus(&self) -> bool {
        self.list_focus
    }

    /// Take the keyboard at the next frame.
    pub fn request_focus(&mut self, cx: &mut Context<Self>) {
        self.wants_focus = true;
        cx.notify();
    }

    /// Whether anyone's looking (a host showing or hiding the view): a live
    /// workspace's reads rest while they aren't.
    pub fn set_visible(&mut self, visible: bool) {
        self.session.set_visible(visible);
    }

    /// Whether keyboard focus is here.
    pub fn contains_focus(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    /// Whether this key is the view's, ahead of the window's own chords.
    pub fn wants_key(&self, chord: KeyChord, cx: &gpui::App) -> bool {
        self.list_key_action(chord, &diff_keys(cx)).is_some() || self.state().wants_key(chord, &diff_keys(cx))
    }

    /// What the state changed to, in the parts the view keeps beside it.
    fn after_change(&mut self) {
        let file = self.state().current_file();
        if file != self.list_file {
            self.list_file = file;
            if let Some(file) = file {
                self.list_scroll.scroll_to_item(file, ScrollStrategy::Nearest);
            }
        }
    }

    /// A click off the palette or the help: Esc's way out of either.
    fn dismiss_overlay(&mut self, cx: &mut Context<Self>) {
        self.key(KeyChord::new(Key::Esc, Mods::NONE), cx);
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // What the keystroke types in a text mode, the key pressed otherwise.
        let chord = if self.state().takes_text() {
            overlay_chord(&event.keystroke)
        } else {
            chord_from_keystroke(&event.keystroke)
        };
        let Some(chord) = chord else { return };
        if !self.wants_key(chord, cx) {
            // Nothing to let go of: Esc gives the keyboard back to the panel.
            if chord.code == Key::Esc && chord.mods == Mods::NONE {
                cx.stop_propagation();
                cx.emit(DiffViewEvent::ReleaseFocus);
            }
            return;
        }
        cx.stop_propagation();
        self.key(chord, cx);
        cx.notify();
    }

    /// A key the view took.
    pub fn key(&mut self, chord: KeyChord, cx: &mut Context<Self>) {
        let keys = diff_keys(cx);
        if let Some(action) = self.list_key_action(chord, &keys) {
            self.list_key(action, cx);
        } else {
            let list_was = self.state().file_list_open();
            let effects = self.session.key(chord, &keys);
            self.client_effects(effects, cx);
            // Opening the list by its key hands it the keyboard, so the keys
            // reach it without the mouse; closing it takes the keyboard back.
            let list_is = self.state().file_list_open();
            if list_was != list_is {
                self.list_focus = list_is;
            }
        }
        self.after_change();
    }

    /// What `chord` means to the file list, while it has the keyboard.
    fn list_key_action(&self, chord: KeyChord, keys: &ModeKeymap<DiffAction>) -> Option<ListKey> {
        if !self.list_focus || !matches!(self.state().mode(), Mode::Normal) || self.state().pending().is_some() {
            return None;
        }
        let chord = shelbi_app::diff::state::normalize_key(chord);
        match (chord.code, keys.dispatch(chord)) {
            (Key::Down, _) | (_, Some(DiffAction::CursorDown | DiffAction::NextFile)) => Some(ListKey::Step(1)),
            (Key::Up, _) | (_, Some(DiffAction::CursorUp | DiffAction::PrevFile)) => Some(ListKey::Step(-1)),
            (Key::Char(' '), _) => Some(ListKey::ToggleViewed),
            (Key::Enter | Key::Esc, _) => Some(ListKey::Leave),
            _ => None,
        }
    }

    fn list_key(&mut self, key: ListKey, cx: &mut Context<Self>) {
        match key {
            ListKey::Step(by) => {
                let action = if by > 0 { DiffAction::NextFile } else { DiffAction::PrevFile };
                let effects = self.session.action(action);
                self.client_effects(effects, cx);
            }
            ListKey::ToggleViewed => {
                let effects = self.session.action(DiffAction::ToggleViewed);
                self.client_effects(effects, cx);
            }
            ListKey::Leave => self.list_focus = false,
        }
    }

    /// Carry out what the session left to the client.
    fn client_effects(&mut self, effects: Vec<DiffEffect>, cx: &mut Context<Self>) {
        for effect in effects {
            match effect {
                DiffEffect::Yank(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.session.state_mut().set_flash("Copied");
                }
                DiffEffect::OpenInEditor { path, line } => self.open_file(path, line, cx),
                DiffEffect::RejectWithComments { reason } => cx.emit(DiffViewEvent::Reject(reason)),
                // The session's own: it never hands these back.
                DiffEffect::Refresh
                | DiffEffect::LoadScope(_)
                | DiffEffect::LoadBlame { .. }
                | DiffEffect::SaveComment(_)
                | DiffEffect::SaveViewed(_)
                | DiffEffect::SendToAgent { .. }
                | DiffEffect::PlanRevert(_)
                | DiffEffect::Revert(_)
                | DiffEffect::PrWrite(_) => {}
            }
        }
    }

    /// `Ctrl-g`: the file at the cursor in the user's editor, off the UI thread.
    fn open_file(&mut self, path: String, line: Option<u32>, cx: &mut Context<Self>) {
        let worktree = match &self.place {
            FilePlace::Local(worktree) => worktree.clone(),
            FilePlace::Elsewhere(why) => {
                let why = why.clone();
                self.session.state_mut().set_flash(why);
                return;
            }
        };
        let open = self.open_file.clone();
        let full = std::path::Path::new(&worktree).join(&path).to_string_lossy().into_owned();
        let launch = cx.background_executor().spawn(async move { open(&full, line) });
        cx.spawn(async move |this, cx| {
            let result = launch.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.session.state_mut().set_flash(format!("Opened {path}"));
                    cx.notify();
                }
                Err(e) => {
                    this.session.state_mut().set_flash(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Take the keyboard (any click in the view).
    fn take_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        cx.emit(DiffViewEvent::Focused);
    }

    fn body_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.take_focus(window, cx);
        self.list_focus = false;
        let top = self.state().top();
        if let Some(row) = self.geometry.row_at(event.position, top) {
            self.session.state_mut().click_row(row);
            let on_chevron = self.geometry.on_chevron(event.position);
            let clicked = self.state().table().rows.get(row).copied();
            let header = matches!(clicked, Some(Row::File { .. } | Row::Hunk { .. }));
            if header && on_chevron {
                let effects = self.session.action(DiffAction::ToggleHunkFold);
                self.client_effects(effects, cx);
            } else if on_chevron && clicked == Some(Row::Pr { part: shelbi_app::diff::PrPart::Title }) {
                let effects = self.session.action(DiffAction::ToggleThread);
                self.client_effects(effects, cx);
            } else {
                let _ = self.session.carry_out(Vec::new());
            }
            self.after_change();
        }
        cx.notify();
    }

    fn body_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(self.geometry.row_h.max(px(1.0)));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        // A trackpad's stroke reports both axes: the larger is the one meant.
        if dx.abs() > dy.abs() {
            let max = self.geometry.max_scroll_x;
            self.scroll_x = (self.scroll_x - px(dx)).clamp(px(0.0), max.max(px(0.0)));
        } else {
            self.wheel_rows -= dy / f32::from(self.geometry.row_h.max(px(1.0)));
            let rows = self.wheel_rows.trunc();
            if rows != 0.0 {
                self.wheel_rows -= rows;
                self.session.state_mut().scroll_by(rows as isize);
                let _ = self.session.carry_out(Vec::new());
                self.after_change();
            }
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn file_clicked(&mut self, file: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.take_focus(window, cx);
        self.list_focus = true;
        self.session.state_mut().goto_file(file);
        let _ = self.session.carry_out(Vec::new());
        self.after_change();
        cx.notify();
    }

    fn check_clicked(&mut self, file: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.take_focus(window, cx);
        let Some(viewed) = self.state().changeset().files.get(file).map(|f| self.state().is_viewed(f)) else {
            return;
        };
        let effects = self.session.state_mut().set_viewed(file, !viewed);
        let effects = self.session.carry_out(effects);
        self.client_effects(effects, cx);
        cx.notify();
    }

    /// Choose a row of the open palette (a click).
    fn palette_clicked(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.take_focus(window, cx);
        let effects = self.session.state_mut().palette_choose(index);
        let effects = self.session.carry_out(effects);
        self.client_effects(effects, cx);
        self.after_change();
        cx.notify();
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.session.refresh();
        cx.notify();
    }

    // --- rendering ---------------------------------------------------------

    fn render_file_row(&self, file: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state();
        let Some(f) = state.changeset().files.get(file) else {
            return div().into_any_element();
        };
        let current = state.current_file() == Some(file);
        let viewed = state.is_viewed(f);
        let colors = crate::theme::diff::DiffColors::of(theme);
        let status_color = match f.status {
            FileStatus::Added => colors.add,
            FileStatus::Deleted => colors.del,
            FileStatus::Renamed => theme.icon_cyan,
            FileStatus::Modified => theme.gray,
        };
        let check_id = ("diff-file-check", file);
        let check = div()
            .id(check_id)
            .debug_selector(move || format!("diff-file-check-{file}"))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(18.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(|d| d.bg(theme.background_hover))
            .when(viewed, |d| d.child(heroicon(icons::CHECK).size(px(14.0)).text_color(colors.viewed)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _, window, cx| this.check_clicked(file, window, cx)));
        let (name, dir) = (f.name().to_string(), parent_dir(&f.path));
        let (additions, deletions) = (f.additions, f.deletions);
        let list_focus = self.list_focus;
        div()
            .id(("diff-file", file))
            .debug_selector(move || format!("diff-file-{file}"))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .h(px(FILE_ROW_H))
            .px(px(8.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .type_style(Type::XSmall)
            .text_color(theme.foreground)
            .when(current && list_focus, |d| d.bg(theme.select_fill))
            .when(current && !list_focus, |d| d.bg(theme.background))
            .hover(|d| d.bg(theme.background_hover))
            .child(check)
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_color(status_color)
                    .child(f.status.letter().to_string()),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().flex_none().when(current, |d| d.text_color(theme.text_loud)).child(name))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().text_color(theme.muted).child(dir)),
            )
            .when(f.binary, |d| d.child(div().flex_none().text_color(theme.muted).child("bin")))
            .when(additions > 0, |d| {
                d.child(div().flex_none().text_color(colors.add).child(format!("+{additions}")))
            })
            .when(deletions > 0, |d| {
                d.child(div().flex_none().text_color(colors.del).child(format!("−{deletions}")))
            })
            .on_click(cx.listener(move |this, _, window, cx| this.file_clicked(file, window, cx)))
            .into_any_element()
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let changeset = self.state().changeset().clone();
        let count = changeset.files.len();
        let scope = format!("Commits: {}", self.state().scope_label());
        let commits = style::content_button(
            "diff-commits",
            style::ButtonContent::label(scope).dropdown(),
            ButtonKind::Secondary,
            ButtonSize::Small,
            false,
            theme,
        )
        .debug_selector(|| "diff-commits".into())
        .on_click(cx.listener(|this, _, window, cx| {
            this.take_focus(window, cx);
            let effects = this.session.action(DiffAction::PickCommit);
            this.client_effects(effects, cx);
            cx.notify();
        }));
        div()
            .debug_selector(|| "diff-file-list".into())
            .flex()
            .flex_col()
            .flex_none()
            .w(px(LIST_W))
            .h_full()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(8.0))
                    .px(px(12.0))
                    .pt(px(12.0))
                    .pb(px(8.0))
                    .child(
                        div()
                            .debug_selector(|| "diff-files-summary".into())
                            .type_style(Type::XSmallMedium)
                            .text_color(theme.foreground)
                            .child(files_summary(count, changeset.additions, changeset.deletions)),
                    )
                    .child(div().flex().flex_row().child(commits)),
            )
            .child(
                uniform_list(
                    "diff-files",
                    count,
                    cx.processor(|this, range: Range<usize>, _window, cx| {
                        let theme = *cx.global::<Theme>();
                        range.map(|file| this.render_file_row(file, &theme, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.list_scroll)
                .flex_1()
                .min_h_0()
                .px(px(4.0)),
            )
            .into_any_element()
    }

    /// In place of the rows: reading, an error, or nothing to show.
    fn render_placeholder(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.state();
        if !state.table().is_empty() {
            return None;
        }
        let base = div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .text_center()
            .type_style(Type::Base)
            .text_color(theme.muted);
        if let Some(error) = state.error() {
            return Some(
                base.debug_selector(|| "diff-failed".into())
                    .child(
                        div()
                            .type_style(Type::BaseMedium)
                            .text_color(theme.text_loud)
                            .child("Couldn't read the diff"),
                    )
                    .child(div().max_w(px(520.0)).child(error.to_string()))
                    .child(
                        style::button("diff-retry", "Try Again", ButtonKind::Secondary, ButtonSize::Base, false, theme)
                            .debug_selector(|| "diff-retry".into())
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    )
                    .into_any_element(),
            );
        }
        if state.loading() {
            return Some(
                base.debug_selector(|| "diff-loading".into())
                    .child(shelbi_app::review::diff_load::LOADING_DIFF)
                    .into_any_element(),
            );
        }
        Some(base.debug_selector(|| "diff-empty".into()).child("No changes").into_any_element())
    }
}

/// What the file list's keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListKey {
    Step(i8),
    ToggleViewed,
    Leave,
}

/// A path's directory, what a file row shows beside the name.
fn parent_dir(path: &str) -> String {
    path.rsplit_once('/').map_or_else(String::new, |(dir, _)| dir.to_string())
}

/// The file list's heading.
fn files_summary(files: usize, additions: u32, deletions: u32) -> String {
    format!("Files {files} · +{additions} −{deletions}")
}

impl Focusable for DiffView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.wants_focus) {
            window.focus(&self.focus_handle, cx);
        }
        let theme = *cx.global::<Theme>();
        let panel_rows = overlays::pr_info_rows_fit(window);
        self.session.state_mut().set_pr_info_height(panel_rows);
        let placeholder = self.render_placeholder(&theme, cx);
        let empty = self.state().changeset().files.is_empty();
        let list = (self.state().file_list_open() && !empty).then(|| self.render_list(&theme, cx));
        let body = div()
            .id("diff-body")
            .debug_selector(|| "diff-body".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_hidden()
            .cursor_text()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::body_down))
            .on_scroll_wheel(cx.listener(Self::body_scroll));
        let body = match placeholder {
            Some(placeholder) => body.child(placeholder),
            None => body.child(DiffBody::new(cx.entity())),
        };
        let draft = self.render_draft(&theme);
        let status = self.render_status(&theme);
        let overlay = self.render_overlay(&theme, window, cx);
        div()
            .id("diff-view")
            .debug_selector(|| "diff-view".into())
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(theme.nested(theme.canvas))
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.take_focus(window, cx)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .children(list.map(|list| {
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .h_full()
                            .child(list)
                            .child(div().flex_none().w(px(1.0)).h_full().bg(theme.divider))
                    }))
                    .child(body),
            )
            .children(draft)
            .child(div().flex_none().h(px(1.0)).bg(theme.divider))
            .child(status)
            .children(overlay)
    }
}

#[cfg(test)]
mod tests;
