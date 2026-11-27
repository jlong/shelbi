//! The Wiki (`wiki-desktop-tree`): its tree in the leading column, pushed
//! in place of the sidebar as Settings' and a workspace's panels are
//! (`column_swap`), and the shown document (or the set-up screen) in the
//! main area.
//!
//! ```text
//! ┌────────────────────────┬──────────────────────────────────────────────┐
//! │ ‹  Wiki        ＋ 🗀 ⋮ ◧ │  Plans › OpenCode integration                │
//! │ 🔍 Filter               │ ──────────────────────────────────────────── │
//! │ ▾ 🗀 Plans              │  # OpenCode integration                      │
//! │ │   📄 Awareness v2     │                                              │
//! │ │   📄 OpenCode integ…  │  ## Context                                  │
//! │ │ ▸ 🗀 Archive          │                                              │
//! │ ▸ 🗀 Research           │                                              │
//! │   📄 README             │                                              │
//! │ ● Synced · 2m ago      │                                              │
//! └────────────────────────┴──────────────────────────────────────────────┘
//! ```
//!
//! The tree is VS Code's: chevrons and indent guides; a folder's emoji icon
//! (its `.contextstore/settings.yml`) or the folder drawing, a document's
//! drawing; rows show a document's title, its file name in the tooltip. A
//! click or Enter opens a document (Enter on the one already open, or F2,
//! renames it in place), ↑ ↓ move, ← → close and open folders (VS Code's
//! rules, [`shelbi_app::wiki_tree`]); Delete trashes with an Undo bar; ⌘N
//! and ⇧⌘N make a document and a folder in the selected folder and rename
//! them in place; a drag moves into a folder, and between folders reorders
//! them (the wiki's `folderOrder`); the context menu has New Document, New
//! Folder, Rename, Duplicate, Move to…, Reveal in Finder, Copy Path and
//! Delete; the filter box narrows the rows. The open folders and the open
//! document are kept per project (the window's view state).
//!
//! Every read and write goes to the hub's wiki service through the window's
//! link ([`shelbi_app::wiki::ask`]): this machine's hub, or the far one for a
//! remote project tab. While the Wiki shows, its pushed changes
//! ([`shelbi_app::wiki_sub`]) re-read the tree, the open document and the
//! footer's sync status, so `shelbi wiki new` shows up within moments; the
//! view also tells the hub it is shown ([`SyncOp::Active`]), so the hub
//! pulls on its faster clock.
//!
//! A project without a wiki shows the set-up screen in the main area: the
//! two sources, and one click sets either up (the shared Settings ▸ Wiki
//! form's Set up, [`WikiForm`]). The document itself shows read-only here;
//! the in-place editor is `wiki-desktop-doc`'s.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, ClipboardItem, Context, DragMoveEvent, Entity, EventEmitter,
    FocusHandle, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, SharedString, Subscription, Task, Window,
};
use gpui_kit::base::text::TextViewState;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use shelbi_app::settings::store::Store;
use shelbi_app::settings::wiki::{self as wk, Ask, Tone, WikiForm};
use shelbi_app::source::ProjectHandle;
use shelbi_app::wiki::{Author, NodeKind, SyncOp, WikiReply, WikiRequest, WikiStatus};
use shelbi_app::wiki_sub::WikiWake;
use shelbi_app::wiki_tree::{TreeRow, WikiTree};
use shelbi_app::wiki_tree_edit::{self as edit, DropAt};
use shelbi_core::WikiSource;

use super::{markdown, sidebar, task_panel};
use crate::assets::{heroicon, icons, Icon};
use crate::elements::placed::placed;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography, NAV_RADIUS};
use crate::theme::Theme;

/// A tree row's height.
pub const ROW_H: f32 = 26.0;
/// How far each level steps in.
pub const INDENT: f32 = 12.0;
/// The column's inset either side of the rows.
const PAD_X: f32 = 8.0;
/// How long the Undo bar stays after a delete.
pub const UNDO_FOR: Duration = Duration::from_secs(8);
/// How often a shown Wiki tells the hub so (its fast pull clock lasts a
/// minute and a half).
const ACTIVE_EVERY: Duration = Duration::from_secs(60);
/// The context menu's width.
const MENU_W: f32 = 220.0;

/// What the Wiki asks of the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiEvent {
    /// The ‹ button, or Esc in the tree: back to the view it was opened over.
    Back,
    /// The header's Toggle sidebar.
    Collapse,
    /// The header's Search.
    OpenSearch,
    /// The open folders or the open document changed: worth remembering.
    Remember,
}

/// What the context menu is about, and which list it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// The row it was opened on; `None` for the empty space under the rows
    /// (the wiki's top level).
    pub path: Option<String>,
    /// Showing Move to…'s folders instead of the actions.
    pub moving: bool,
}

/// A rename in place.
struct Rename {
    path: String,
    input: Entity<InputState>,
    error: Option<String>,
    _sub: Subscription,
}

/// The Undo bar after a delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    pub token: String,
    pub label: String,
    pub until: Instant,
}

/// What a dragged row carries.
#[derive(Debug, Clone)]
pub struct WikiDrag {
    pub path: String,
    title: SharedString,
    icon: Icon,
}

/// The row that follows the pointer during a drag.
pub struct WikiDragGhost {
    title: SharedString,
    icon: Icon,
}

impl Render for WikiDragGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().opaque();
        style::popover(div(), &theme)
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            .h(px(ROW_H))
            .px(px(8.0))
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(heroicon(self.icon).text_color(theme.icon))
            .child(self.title.clone())
    }
}

/// The Wiki: the tree's state, the open document, the sync status and the
/// set-up form, for one project.
pub struct WikiView {
    focus: FocusHandle,
    ask: Ask,
    /// The project's config (the set-up writes its `wiki:` block).
    store: Store,
    /// The project's machine is this one (Reveal in Finder needs it).
    local: bool,
    project: String,
    tree: WikiTree,
    /// The hub's answer to the last Status: `None` before one lands.
    status: Option<Result<WikiStatus, String>>,
    /// Why the tree couldn't be read.
    tree_error: Option<String>,
    /// The last edit's failure, shown over the footer.
    notice: Option<String>,
    filter: Entity<InputState>,
    rename: Option<Rename>,
    /// A new entry made by ⌘N / ⇧⌘N, renamed once the tree shows it.
    rename_when_shown: Option<String>,
    menu: Option<Menu>,
    menu_anchor: style::Anchor,
    /// Where the selection goes once the next tree read shows it (after a
    /// move).
    pending_select: Option<String>,
    /// The row and edge a drag hovers.
    drop_target: Option<(String, DropAt)>,
    undo: Option<Undo>,
    /// The document shown in the main area, its text once read.
    open: Option<String>,
    doc: Option<Result<String, String>>,
    doc_state: Option<Entity<TextViewState>>,
    setup: WikiForm,
    /// Increments on every reload, so a stale answer is dropped.
    generation: u64,
    shown: bool,
    live: Option<(shelbi_app::changes::ChangeSubscription, Task<()>)>,
    last_active: Option<Instant>,
    /// The link, for its subscription; `None` in a test.
    handle: Option<ProjectHandle>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WikiEvent> for WikiView {}

impl WikiView {
    /// The Wiki of `handle`'s project, with `expanded` folders open and
    /// `open` shown (where the project was left). Reads nothing until
    /// [`Self::show`].
    pub fn new(
        handle: &ProjectHandle,
        expanded: Vec<String>,
        open: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let link = handle.link.clone();
        let ask: Ask = Arc::new(move |req| shelbi_app::wiki::ask(&link, req));
        let store: Store = Arc::new(handle.clone());
        let mut view = Self::with_ask(ask, store, handle.key().name.clone(), !handle.link.is_remote(), window, cx);
        view.handle = Some(handle.clone());
        view.tree.set_expanded(expanded);
        view.open = open;
        view
    }

    /// A Wiki that asks `ask` (a test's fake hub) and follows nothing.
    pub fn with_ask(ask: Ask, store: Store, project: String, local: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let subs = vec![cx.subscribe_in(&filter, window, |this, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    this.tree.set_filter(text);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => {
                    // Return in the filter opens the first match.
                    this.focus.focus(window, cx);
                    this.activate(window, cx);
                }
                _ => {}
            }
        })];
        WikiView {
            focus: cx.focus_handle(),
            ask,
            store,
            local,
            project,
            tree: WikiTree::new(),
            status: None,
            tree_error: None,
            notice: None,
            filter,
            rename: None,
            rename_when_shown: None,
            menu: None,
            menu_anchor: Default::default(),
            pending_select: None,
            drop_target: None,
            undo: None,
            open: None,
            doc: None,
            doc_state: None,
            setup: WikiForm::default(),
            generation: 0,
            shown: false,
            live: None,
            last_active: None,
            handle: None,
            _subs: subs,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn tree(&self) -> &WikiTree {
        &self.tree
    }

    pub fn status(&self) -> Option<&Result<WikiStatus, String>> {
        self.status.as_ref()
    }

    /// The document shown in the main area.
    pub fn open_path(&self) -> Option<&str> {
        self.open.as_deref()
    }

    /// The open folders, for the window to remember.
    pub fn expanded(&self) -> Vec<String> {
        self.tree.expanded().map(str::to_string).collect()
    }

    pub fn menu(&self) -> Option<&Menu> {
        self.menu.as_ref()
    }

    pub fn undo(&self) -> Option<&Undo> {
        self.undo.as_ref()
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The path a rename in place is open on.
    pub fn renaming(&self) -> Option<&str> {
        self.rename.as_ref().map(|r| r.path.as_str())
    }

    #[cfg(test)]
    pub fn rename_input(&self) -> Option<Entity<InputState>> {
        self.rename.as_ref().map(|r| r.input.clone())
    }

    #[cfg(test)]
    pub fn filter_input(&self) -> Entity<InputState> {
        self.filter.clone()
    }

    #[cfg(test)]
    pub fn drop_target(&self) -> Option<&(String, DropAt)> {
        self.drop_target.as_ref()
    }

    /// The Wiki is shown: read the status and the tree, follow the hub's
    /// changes while it stays shown, and tell the hub so.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = !self.shown;
        self.shown = true;
        if first {
            self.follow(window, cx);
        }
        self.reload(window, cx);
    }

    /// The Wiki is no longer shown: stop following the hub.
    pub fn hide(&mut self) {
        self.shown = false;
        self.live = None;
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Follow the hub's wiki changes (not in a test: its fake has none).
    fn follow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(handle) = self.handle.clone() else {
            return;
        };
        if cfg!(test) {
            return;
        }
        let (subscription, rx) = handle.link.subscribe_wiki();
        let rx = Arc::new(std::sync::Mutex::new(rx));
        let task = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let wakes: Vec<WikiWake> = {
                let rx = rx.lock().unwrap_or_else(|p| p.into_inner());
                std::iter::from_fn(|| rx.try_recv().ok()).collect()
            };
            let alive = this
                .update_in(cx, |this, window, cx| {
                    if !wakes.is_empty() {
                        this.woke(wakes, window, cx);
                    }
                    this.tick(window, cx);
                })
                .is_ok();
            if !alive {
                return;
            }
        });
        self.live = Some((subscription, task));
    }

    /// The hub pushed changes.
    pub fn woke(&mut self, wakes: Vec<WikiWake>, window: &mut Window, cx: &mut Context<Self>) {
        let mut tree = false;
        let mut doc = false;
        let mut status = false;
        for wake in wakes {
            match wake {
                WikiWake::Changed { paths } => {
                    tree = true;
                    doc |= self.open.as_ref().is_some_and(|open| paths.iter().any(|p| p == open));
                }
                WikiWake::Status(_) => status = true,
                WikiWake::Open(path) => {
                    if let Some(path) = path {
                        self.reveal(&path, window, cx);
                    }
                }
            }
        }
        if status {
            self.read_status(window, cx);
        }
        if tree {
            self.read_tree(window, cx);
        }
        if doc {
            self.read_doc(window, cx);
        }
    }

    /// The clock: the Undo bar's expiry, and the hub's Active ping.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.undo.as_ref().is_some_and(|u| now >= u.until) {
            self.undo = None;
            cx.notify();
        }
        let set_up = matches!(&self.status, Some(Ok(s)) if s.set_up);
        if self.shown && set_up && self.last_active.is_none_or(|at| now.duration_since(at) >= ACTIVE_EVERY) {
            self.last_active = Some(now);
            self.request(WikiRequest::Sync(SyncOp::Active), window, cx, |_, _, _, _| {});
        }
    }

    /// Read everything again.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.read_status(window, cx);
    }

    fn read_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.generation;
        self.request(WikiRequest::Status, window, cx, move |this, reply, window, cx| {
            if this.generation != generation {
                return;
            }
            let was_set_up = matches!(&this.status, Some(Ok(s)) if s.set_up);
            this.status = Some(match reply {
                WikiReply::Status(s) => Ok(s),
                WikiReply::Failed(f) => Err(f.message),
                other => Err(format!("the hub answered {other:?}")),
            });
            let set_up = matches!(&this.status, Some(Ok(s)) if s.set_up);
            if set_up && (!was_set_up || !this.tree.is_loaded()) {
                this.read_tree(window, cx);
                this.last_active = None;
            }
            if !set_up && this.setup.config().is_none() {
                this.setup.visit();
            }
        });
    }

    fn read_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.generation;
        self.request(WikiRequest::Tree { path: None }, window, cx, move |this, reply, window, cx| {
            if this.generation != generation {
                return;
            }
            match reply {
                WikiReply::Tree(nodes) => this.take_tree(nodes, window, cx),
                WikiReply::Failed(f) => this.tree_error = Some(f.message),
                other => this.tree_error = Some(format!("the hub answered {other:?}")),
            }
        });
    }

    /// A tree read in: keep the selection on its path, open what was asked
    /// to be renamed, and show the remembered document.
    pub fn take_tree(&mut self, nodes: Vec<shelbi_app::wiki::WikiNode>, window: &mut Window, cx: &mut Context<Self>) {
        let first = !self.tree.is_loaded();
        let selected = self.tree.selected_path().map(str::to_string);
        self.tree.set_nodes(nodes);
        self.tree_error = None;
        if first {
            if self.tree.expanded().next().is_none() {
                self.tree.set_expanded(edit::first_level_folders(self.tree.nodes()));
            }
            match self.open.clone() {
                Some(open) if self.tree.select_path(&open) => self.read_doc(window, cx),
                Some(_) => {
                    self.open = None;
                    self.doc = None;
                }
                None => {}
            }
        } else if let Some(path) = self.pending_select.take().or(selected) {
            self.tree.select_path(&path);
        }
        if let Some(path) = self.rename_when_shown.take() {
            if self.tree.select_path(&path) {
                self.begin_rename(&path, window, cx);
            }
        }
        // The open document went away (removed elsewhere).
        if self.open.as_ref().is_some_and(|p| self.tree.node(p).is_none()) {
            self.open = None;
            self.doc = None;
            self.doc_state = None;
            cx.emit(WikiEvent::Remember);
        }
        cx.notify();
    }

    fn read_doc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.open.clone() else {
            return;
        };
        let req = WikiRequest::Read {
            path: path.clone(),
            section: None,
        };
        self.request(req, window, cx, move |this, reply, _, cx| {
            if this.open.as_deref() != Some(path.as_str()) {
                return;
            }
            let doc = match reply {
                WikiReply::Doc { content, .. } => Ok(content),
                WikiReply::Failed(f) => Err(f.message),
                other => Err(format!("the hub answered {other:?}")),
            };
            if let Ok(text) = &doc {
                match &this.doc_state {
                    Some(state) => state.update(cx, |s, cx| s.set_text(text, cx)),
                    None => this.doc_state = Some(cx.new(|cx| TextViewState::markdown(text, cx))),
                }
            }
            this.doc = Some(doc);
        });
    }

    /// Ask the hub `req` off the UI thread and hand its answer to `then`.
    fn request(
        &mut self,
        req: WikiRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, WikiReply, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let ask = self.ask.clone();
        cx.spawn_in(window, async move |this, cx| {
            let reply = cx.background_executor().spawn(async move { ask(req) }).await;
            this.update_in(cx, |this, window, cx| {
                then(this, reply, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ask for an edit; on success read the tree again (a pushed change
    /// will too, but the edit's author shouldn't wait for it), on failure
    /// say why.
    fn edit(
        &mut self,
        req: WikiRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, WikiReply, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.notice = None;
        self.request(req, window, cx, move |this, reply, window, cx| {
            if let WikiReply::Failed(f) = &reply {
                this.notice = Some(f.message.clone());
            }
            this.read_tree(window, cx);
            then(this, reply, window, cx);
        });
    }

    // --- the tree's actions ------------------------------------------------

    /// Open the selected row: a document shows in the main area; a folder
    /// opens or closes.
    pub fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.tree.selected_row() else {
            return;
        };
        if row.is_folder() {
            self.tree.toggle();
            cx.emit(WikiEvent::Remember);
        } else {
            self.open_doc(&row.path, window, cx);
        }
        cx.notify();
    }

    /// Show the document at `path` in the main area.
    pub fn open_doc(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.as_deref() != Some(path) {
            self.open = Some(path.to_string());
            self.doc = None;
            self.read_doc(window, cx);
            cx.emit(WikiEvent::Remember);
        }
        cx.notify();
    }

    /// Select and show `path` (a `shelbi wiki open`), its folders opened.
    pub fn reveal(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.tree.select_path(path) {
            if self.tree.node(path).is_some_and(|n| n.kind == NodeKind::Document) {
                self.open_doc(path, window, cx);
            }
            cx.emit(WikiEvent::Remember);
        }
        cx.notify();
    }

    /// Make a document (or a folder) in the selected folder and rename it
    /// in place once the tree shows it.
    pub fn create(&mut self, kind: NodeKind, window: &mut Window, cx: &mut Context<Self>) {
        let folder = match &self.menu {
            Some(Menu { path: None, .. }) => String::new(),
            _ => self.tree.folder_for_new(),
        };
        self.menu = None;
        let path = edit::new_path(self.tree.nodes(), &folder, kind);
        if !folder.is_empty() {
            let mut open: Vec<String> = self.expanded();
            open.push(folder.clone());
            self.tree.set_expanded(open);
        }
        let req = WikiRequest::New {
            path: path.clone(),
            content: None,
            title: None,
            by: Author::Person,
        };
        self.rename_when_shown = Some(path.clone());
        self.edit(req, window, cx, move |this, reply, window, cx| {
            if matches!(reply, WikiReply::Failed(_)) {
                this.rename_when_shown = None;
            } else if kind == NodeKind::Document {
                this.open_doc(&path, window, cx);
            }
        });
    }

    /// Rename `path` in place: its name in a field over the row.
    pub fn begin_rename(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self.tree.node(path) else {
            return;
        };
        self.menu = None;
        let draft = edit::rename_draft(node);
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |s, cx| {
            s.set_value(draft, window, cx);
            s.select_all(window, cx);
        });
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
            InputEvent::Blur => {
                if this.rename.is_some() {
                    this.commit_rename(window, cx)
                }
            }
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.rename = Some(Rename {
            path: path.to_string(),
            input,
            error: None,
            _sub: sub,
        });
        cx.notify();
    }

    /// The rename's field was committed (Return, or it lost focus).
    pub fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.as_ref() else {
            return;
        };
        let typed = rename.input.read(cx).value().to_string();
        let path = rename.path.clone();
        let Some(node) = self.tree.node(&path) else {
            self.rename = None;
            cx.notify();
            return;
        };
        match edit::rename_target(node, &typed) {
            Err(why) => {
                if let Some(r) = self.rename.as_mut() {
                    r.error = Some(why);
                }
            }
            Ok(None) => self.end_rename(window, cx),
            Ok(Some(to)) => {
                self.end_rename(window, cx);
                self.move_path(&path, &to, window, cx);
            }
        }
        cx.notify();
    }

    /// Close the rename (Esc keeps the old name).
    pub fn end_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Move (or rename) `from` to `to`, keeping the selection, the open
    /// folders and the open document on what moved.
    fn move_path(&mut self, from: &str, to: &str, window: &mut Window, cx: &mut Context<Self>) {
        let req = WikiRequest::Move {
            from: from.to_string(),
            to: to.to_string(),
            by: Author::Person,
        };
        let (from, to) = (from.to_string(), to.to_string());
        self.edit(req, window, cx, move |this, reply, window, cx| {
            if matches!(reply, WikiReply::Failed(_)) {
                return;
            }
            let expanded: Vec<String> = this.expanded().iter().map(|p| edit::moved(p, &from, &to)).collect();
            this.tree.set_expanded(expanded);
            if let Some(selected) = this.tree.selected_path().map(|p| edit::moved(p, &from, &to)) {
                this.rename_when_shown = None;
                this.select_after_reload(selected);
            }
            if let Some(open) = this.open.clone() {
                let now = edit::moved(&open, &from, &to);
                if now != open {
                    this.open = Some(now);
                    this.read_doc(window, cx);
                }
            }
            cx.emit(WikiEvent::Remember);
        });
    }

    /// Select `path` once the next tree read shows it.
    fn select_after_reload(&mut self, path: String) {
        // The tree read that follows every edit keeps the selection on the
        // path it holds; pointing it at the new path first does the rest.
        if !self.tree.select_path(&path) {
            self.pending_select = Some(path);
        }
    }

    /// Copy `path` beside itself (`plan copy.md`).
    pub fn duplicate(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let to = edit::duplicate_path(self.tree.nodes(), path);
        let req = WikiRequest::Copy {
            from: path.to_string(),
            to,
            by: Author::Person,
        };
        self.edit(req, window, cx, |_, _, _, _| {});
    }

    /// Move `path` into `folder` (Move to…).
    pub fn move_into(&mut self, path: &str, folder: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let to = if folder.is_empty() {
            edit::name_of(path).to_string()
        } else {
            format!("{folder}/{}", edit::name_of(path))
        };
        if to != path {
            self.move_path(path, &to, window, cx);
        }
        cx.notify();
    }

    /// Trash `path`, offering Undo for [`UNDO_FOR`].
    pub fn delete(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(node) = self.tree.node(path) else {
            return;
        };
        let label = format!("Deleted “{}”", node.title);
        let req = WikiRequest::Trash {
            path: path.to_string(),
            by: Author::Person,
        };
        let path = path.to_string();
        self.edit(req, window, cx, move |this, reply, _, cx| {
            if let WikiReply::Trashed { token } = reply {
                this.undo = Some(Undo {
                    token,
                    label,
                    until: Instant::now() + UNDO_FOR,
                });
                if this.open.as_ref().is_some_and(|o| *o == path || o.starts_with(&format!("{path}/"))) {
                    this.open = None;
                    this.doc = None;
                    this.doc_state = None;
                    cx.emit(WikiEvent::Remember);
                }
            }
        });
    }

    /// Undo the last delete.
    pub fn undo_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(undo) = self.undo.take() else {
            return;
        };
        let req = WikiRequest::Untrash {
            token: undo.token,
            by: Author::Person,
        };
        self.edit(req, window, cx, |_, _, _, _| {});
    }

    /// Drop `dragged` on `target` at `at`: the moves and folder order it
    /// makes ([`edit::drop_plan`]).
    pub fn drop_on(&mut self, dragged: &str, target: &str, at: DropAt, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_target = None;
        let plan = match edit::drop_plan(self.tree.nodes(), dragged, target, at) {
            Ok(plan) => plan,
            Err(why) => {
                self.notice = Some(why);
                cx.notify();
                return;
            }
        };
        let order = plan.order.clone();
        let then_order = move |this: &mut Self, reply: WikiReply, window: &mut Window, cx: &mut Context<Self>| {
            if matches!(reply, WikiReply::Failed(_)) {
                return;
            }
            if let Some((folder, order)) = order {
                let req = WikiRequest::Reorder {
                    folder,
                    order,
                    by: Author::Person,
                };
                this.edit(req, window, cx, |_, _, _, _| {});
            }
        };
        match plan.moves {
            Some((from, to)) => {
                if !edit::folder_of(&to).is_empty() {
                    let mut open = self.expanded();
                    open.push(edit::folder_of(&to).to_string());
                    self.tree.set_expanded(open);
                }
                let req = WikiRequest::Move {
                    from: from.clone(),
                    to: to.clone(),
                    by: Author::Person,
                };
                self.edit(req, window, cx, move |this, reply, window, cx| {
                    if !matches!(reply, WikiReply::Failed(_)) {
                        if let Some(open) = this.open.clone() {
                            let now = edit::moved(&open, &from, &to);
                            if now != open {
                                this.open = Some(now);
                                this.read_doc(window, cx);
                            }
                        }
                        this.select_after_reload(to.clone());
                        cx.emit(WikiEvent::Remember);
                    }
                    then_order(this, reply, window, cx);
                });
            }
            None => then_order(self, WikiReply::Done, window, cx),
        }
        cx.notify();
    }

    /// The wiki's folder on disk (Reveal in Finder).
    fn local_path(&self, path: &str) -> Option<PathBuf> {
        let Some(Ok(status)) = &self.status else {
            return None;
        };
        (self.local && !status.root.is_empty()).then(|| PathBuf::from(&status.root).join(path))
    }

    /// Open the context menu on `path` (`None`: the space under the rows).
    pub fn open_menu(&mut self, path: Option<String>, at: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = &path {
            self.tree.select_path(p);
        }
        self.menu_anchor.set(Some(gpui::Bounds::new(at, gpui::Size::default())));
        self.menu = Some(Menu { path, moving: false });
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        cx.notify();
    }

    /// Run one of the context menu's actions.
    pub fn menu_action(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.menu.as_ref().and_then(|m| m.path.clone());
        match (action, path) {
            (MenuAction::NewDocument, _) => self.create(NodeKind::Document, window, cx),
            (MenuAction::NewFolder, _) => self.create(NodeKind::Folder, window, cx),
            (MenuAction::Rename, Some(p)) => self.begin_rename(&p, window, cx),
            (MenuAction::Duplicate, Some(p)) => self.duplicate(&p, window, cx),
            (MenuAction::MoveTo, Some(_)) => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.moving = true;
                }
            }
            (MenuAction::Reveal, p) => {
                self.menu = None;
                if let Some(abs) = self.local_path(p.as_deref().unwrap_or("")) {
                    cx.reveal_path(&abs);
                }
            }
            (MenuAction::CopyPath, Some(p)) => {
                self.menu = None;
                cx.write_to_clipboard(ClipboardItem::new_string(p));
            }
            (MenuAction::Delete, Some(p)) => self.delete(&p, window, cx),
            (_, None) => self.menu = None,
        }
        cx.notify();
    }

    /// The actions the context menu lists, and why each can't run (`None`
    /// when it can).
    pub fn menu_actions(&self) -> Vec<(MenuAction, Option<&'static str>)> {
        let on_row = self.menu.as_ref().is_some_and(|m| m.path.is_some());
        MenuAction::ALL
            .iter()
            .copied()
            .filter(|a| on_row || matches!(a, MenuAction::NewDocument | MenuAction::NewFolder | MenuAction::Reveal))
            .map(|a| {
                let refusal = match a {
                    MenuAction::Reveal if !self.local => Some("The wiki is on another machine"),
                    _ => None,
                };
                (a, refusal)
            })
            .collect()
    }

    /// The folders Move to… offers for `path`: the top level and every
    /// folder, minus where it is and what's inside it.
    pub fn move_targets(&self, path: &str) -> Vec<(String, String, usize)> {
        let mut out = vec![(String::new(), "Top level".to_string(), 0)];
        fn walk(nodes: &[shelbi_app::wiki::WikiNode], depth: usize, out: &mut Vec<(String, String, usize)>) {
            for n in nodes.iter().filter(|n| n.kind == NodeKind::Folder) {
                out.push((n.path.clone(), n.title.clone(), depth));
                walk(&n.children, depth + 1, out);
            }
        }
        walk(self.tree.nodes(), 1, &mut out);
        let here = edit::folder_of(path).to_string();
        out.retain(|(p, _, _)| *p != here && *p != path && !p.starts_with(&format!("{path}/")));
        out
    }

    /// Set the wiki up from the empty state: `source` with its default
    /// location.
    pub fn set_up(&mut self, source: WikiSource, window: &mut Window, cx: &mut Context<Self>) {
        self.setup.choose_source(source);
        self.setup.press();
        self.run_setup(window, cx);
        cx.notify();
    }

    /// Run the set-up form's waiting work off the UI thread.
    fn run_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.setup.work_due() {
            return;
        }
        let generation = self.setup.generation();
        let work = self.setup.take_work();
        let store = self.store();
        let ask = self.ask.clone();
        cx.spawn_in(window, async move |this, cx| {
            for item in work {
                let (store, ask) = (store.clone(), ask.clone());
                let done = cx.background_executor().spawn(async move { item.run(store.as_ref(), &ask) }).await;
                let set_up = matches!(&done, wk::WikiDone::SetUp { reply: Some(WikiReply::Status(s)), .. } if s.set_up);
                let alive = this
                    .update_in(cx, |this, window, cx| {
                        this.setup.done(generation, done);
                        if set_up {
                            this.reload(window, cx);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
            this.update_in(cx, |this, window, cx| this.run_setup(window, cx)).ok();
        })
        .detach();
    }

    fn store(&self) -> Store {
        self.store.clone()
    }
}

/// One of the context menu's actions, in its order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    NewDocument,
    NewFolder,
    Rename,
    Duplicate,
    MoveTo,
    Reveal,
    CopyPath,
    Delete,
}

impl MenuAction {
    pub const ALL: [MenuAction; 8] = [
        MenuAction::NewDocument,
        MenuAction::NewFolder,
        MenuAction::Rename,
        MenuAction::Duplicate,
        MenuAction::MoveTo,
        MenuAction::Reveal,
        MenuAction::CopyPath,
        MenuAction::Delete,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MenuAction::NewDocument => "New Document",
            MenuAction::NewFolder => "New Folder",
            MenuAction::Rename => "Rename",
            MenuAction::Duplicate => "Duplicate",
            MenuAction::MoveTo => "Move to…",
            MenuAction::Reveal => reveal_label(),
            MenuAction::CopyPath => "Copy Path",
            MenuAction::Delete => "Delete",
        }
    }

    /// Its row's selector (`wiki-menu-<slug>`).
    pub fn slug(self) -> &'static str {
        match self {
            MenuAction::NewDocument => "new-document",
            MenuAction::NewFolder => "new-folder",
            MenuAction::Rename => "rename",
            MenuAction::Duplicate => "duplicate",
            MenuAction::MoveTo => "move-to",
            MenuAction::Reveal => "reveal",
            MenuAction::CopyPath => "copy-path",
            MenuAction::Delete => "delete",
        }
    }

    /// The key that does the same in the tree.
    fn hint(self) -> &'static str {
        match self {
            MenuAction::NewDocument if cfg!(target_os = "macos") => "⌘N",
            MenuAction::NewDocument => "Ctrl+N",
            MenuAction::NewFolder if cfg!(target_os = "macos") => "⇧⌘N",
            MenuAction::NewFolder => "Ctrl+Shift+N",
            MenuAction::Rename => "F2",
            MenuAction::Delete if cfg!(target_os = "macos") => "⌘⌫",
            MenuAction::Delete => "Delete",
            _ => "",
        }
    }
}

/// Reveal in Finder on a Mac, Show in Files elsewhere.
fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in Files"
    }
}

/// A key the focused tree takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKey {
    Up,
    Down,
    First,
    Last,
    Collapse,
    Expand,
    Enter,
    Open,
    Rename,
    Delete,
    NewDocument,
    NewFolder,
    Filter,
    Undo,
    Escape,
}

impl TreeKey {
    /// What `event` is to the tree, if anything. `secondary` is the app
    /// modifier (⌘ on a Mac, Ctrl elsewhere).
    pub fn of(key: &str, secondary: bool, shift: bool, alt: bool) -> Option<TreeKey> {
        if alt {
            return None;
        }
        Some(match (key, secondary, shift) {
            ("up", false, false) => TreeKey::Up,
            ("down", false, false) => TreeKey::Down,
            ("home", false, false) | ("up", true, false) => TreeKey::First,
            ("end", false, false) | ("down", true, false) => TreeKey::Last,
            ("left", false, false) => TreeKey::Collapse,
            ("right", false, false) => TreeKey::Expand,
            ("enter", false, false) => TreeKey::Enter,
            ("space", false, false) => TreeKey::Open,
            ("f2", false, false) => TreeKey::Rename,
            ("delete", false, false) | ("backspace", true, false) => TreeKey::Delete,
            ("n", true, false) => TreeKey::NewDocument,
            ("n", true, true) => TreeKey::NewFolder,
            ("f", true, false) => TreeKey::Filter,
            ("z", true, false) => TreeKey::Undo,
            ("escape", false, false) => TreeKey::Escape,
            _ => return None,
        })
    }
}

impl WikiView {
    /// A key while the tree (not a field) has focus. Returns whether it took
    /// it.
    pub fn key(&mut self, key: TreeKey, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.menu.is_some() {
            if key == TreeKey::Escape {
                self.close_menu(cx);
                return true;
            }
            self.menu = None;
        }
        let selected = self.tree.selected_path().map(str::to_string);
        match key {
            TreeKey::Up => self.tree.move_up(),
            TreeKey::Down => self.tree.move_down(),
            TreeKey::First => self.tree.move_to_first(),
            TreeKey::Last => self.tree.move_to_last(),
            TreeKey::Collapse => {
                self.tree.collapse();
                cx.emit(WikiEvent::Remember);
            }
            TreeKey::Expand => {
                self.tree.expand();
                cx.emit(WikiEvent::Remember);
            }
            // VS Code's Enter: open what's selected; on the document already
            // open, rename it.
            TreeKey::Enter => match self.tree.selected_row() {
                Some(row) if !row.is_folder() && self.open.as_deref() == Some(row.path.as_str()) => {
                    self.begin_rename(&row.path, window, cx)
                }
                Some(_) => self.activate(window, cx),
                None => return false,
            },
            TreeKey::Open => self.activate(window, cx),
            TreeKey::Rename => match selected {
                Some(p) => self.begin_rename(&p, window, cx),
                None => return false,
            },
            TreeKey::Delete => match selected {
                Some(p) => self.delete(&p, window, cx),
                None => return false,
            },
            TreeKey::NewDocument => self.create(NodeKind::Document, window, cx),
            TreeKey::NewFolder => self.create(NodeKind::Folder, window, cx),
            TreeKey::Filter => self.filter.update(cx, |s, cx| s.focus(window, cx)),
            TreeKey::Undo => {
                if self.undo.is_none() {
                    return false;
                }
                self.undo_delete(window, cx);
            }
            TreeKey::Escape => {
                if !self.tree.filter().is_empty() {
                    self.clear_filter(window, cx);
                } else {
                    cx.emit(WikiEvent::Back);
                }
            }
        }
        cx.notify();
        true
    }

    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |s, cx| s.set_value("", window, cx));
        self.tree.set_filter("");
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // A field (the filter, a rename) takes its own keys.
        if !self.focus.is_focused(window) {
            return;
        }
        let m = &event.keystroke.modifiers;
        if let Some(key) = TreeKey::of(&event.keystroke.key, m.secondary(), m.shift, m.alt || (m.control && cfg!(target_os = "macos"))) {
            if self.key(key, window, cx) {
                cx.stop_propagation();
            }
        }
    }

    // --- rendering ---------------------------------------------------------

    /// The main area: the set-up screen, the open document, or a prompt to
    /// pick one.
    pub fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let body = match &self.status {
            None => note("wiki-main-loading", "Loading the wiki…", None, &theme).into_any_element(),
            Some(Err(e)) => note("wiki-main-error", "Couldn't reach the wiki", Some(e.clone()), &theme).into_any_element(),
            Some(Ok(s)) if !s.set_up => self.render_setup(&theme, cx),
            Some(Ok(_)) => match (&self.open, &self.doc) {
                (Some(path), doc) => self.render_doc(path.clone(), doc.clone(), &theme),
                (None, _) => {
                    let count = edit::document_count(self.tree.nodes());
                    let detail = match count {
                        0 => "This wiki has no documents yet. ⌘N makes one.".to_string(),
                        1 => "1 document. Pick it in the tree.".to_string(),
                        n => format!("{n} documents. Pick one in the tree."),
                    };
                    note("wiki-main-empty", "No document open", Some(detail), &theme).into_any_element()
                }
            },
        };
        div()
            .id("wiki-main")
            .debug_selector(|| "wiki-main".into())
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .bg(theme.background)
            .child(body)
            .into_any_element()
    }

    fn render_doc(&self, path: String, doc: Option<Result<String, String>>, theme: &Theme) -> AnyElement {
        let crumbs = edit::breadcrumb(self.tree.nodes(), &path);
        let last = crumbs.len().saturating_sub(1);
        let crumb_row = div()
            .debug_selector(|| "wiki-breadcrumb".into())
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .h(px(44.0))
            .px(px(24.0))
            .border_b_1()
            .border_color(theme.divider)
            .type_style(Type::Base)
            .children(crumbs.into_iter().enumerate().flat_map(|(i, c)| {
                let sep = (i > 0).then(|| div().text_color(theme.muted).child("›").into_any_element());
                let label = div()
                    .min_w_0()
                    .truncate()
                    .text_color(if i == last { theme.text_loud } else { theme.muted })
                    .child(c)
                    .into_any_element();
                sep.into_iter().chain([label])
            }));
        let body = match (doc, &self.doc_state) {
            (Some(Ok(_)), Some(state)) => div()
                .id("wiki-doc-body")
                .debug_selector(|| "wiki-doc-body".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(32.0))
                .py(px(24.0))
                .child(div().max_w(px(760.0)).child(markdown::body_with_state(state, theme, None, None)))
                .into_any_element(),
            (Some(Err(e)), _) => note("wiki-doc-error", "Couldn't read this document", Some(e), theme).into_any_element(),
            _ => note("wiki-doc-loading", "Loading…", None, theme).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(crumb_row)
            .child(body)
            .into_any_element()
    }

    /// No wiki yet: the two sources, each set up in one click.
    fn render_setup(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.setup.setting_up() || !self.setup.loaded();
        let card = |source: WikiSource, title: &'static str, detail: &'static str, action: &'static str| {
            let id = match source {
                WikiSource::Branch => "wiki-setup-branch",
                WikiSource::Folder => "wiki-setup-folder",
            };
            let button = if busy {
                style::disabled_button(id, action, ButtonKind::Secondary, ButtonSize::Base, theme)
            } else {
                let kind = if source == WikiSource::Branch { ButtonKind::Primary } else { ButtonKind::Secondary };
                style::button(id, action, kind, ButtonSize::Base, false, theme)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.set_up(source, window, cx)))
            };
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .p(px(16.0))
                .rounded(px(10.0))
                .border_1()
                .border_color(theme.divider)
                .child(div().type_style(Type::BaseSemibold).text_color(theme.text_loud).child(title))
                .child(div().type_style(Type::Small).text_color(theme.muted).child(detail))
                .child(div().pt(px(4.0)).child(button))
        };
        let status = if self.setup.setting_up() {
            Some(("Setting up…".to_string(), theme.icon_yellow))
        } else {
            self.setup
                .sync_error()
                .or(self.setup.error())
                .or(self.setup.unreadable())
                .map(|e| (e.to_string(), theme.error))
        };
        div()
            .id("wiki-setup")
            .debug_selector(|| "wiki-setup".into())
            .flex()
            .flex_col()
            .size_full()
            .items_center()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.0))
                    .w_full()
                    .max_w(px(560.0))
                    .px(px(24.0))
                    .py(px(48.0))
                    .child(heroicon(icons::BOOK_OPEN).text_color(theme.icon))
                    .child(div().type_style(Type::Title).text_color(theme.text_loud).child("Set up the wiki"))
                    .child(
                        div()
                            .type_style(Type::Base)
                            .text_color(theme.muted)
                            .child("The wiki keeps this project's plans, decisions and docs as Markdown in git, where you and every agent read and write them. Shelbi commits, pulls and pushes it for you. Pick where it lives:"),
                    )
                    .child(card(
                        WikiSource::Branch,
                        "An orphan branch",
                        "Its own branch, docs/planning, beside your code but never in it. The same folder ContextStore opens.",
                        "Create docs/planning",
                    ))
                    .child(card(
                        WikiSource::Folder,
                        "A folder in the repo",
                        "The docs/ folder on your main branch. Edits land on a rolling pull request for you to merge.",
                        "Use docs/",
                    ))
                    .children(status.map(|(text, color)| {
                        div()
                            .debug_selector(|| "wiki-setup-status".into())
                            .type_style(Type::Small)
                            .text_color(color)
                            .child(text)
                    }))
                    .child(
                        div()
                            .type_style(Type::XSmall)
                            .text_color(theme.form_hint)
                            .child("Settings ▸ Wiki changes the source, the branch or folder, and auto-sync later."),
                    ),
            )
            .into_any_element()
    }

    fn render_header(&self, palette_chord: SharedString, theme: &Theme, cx: &mut Context<Self>) -> gpui::Div {
        let back = task_panel::back_button("wiki-back", false, theme)
            .tooltip(style::tooltip("Back (Esc)"))
            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(WikiEvent::Back)));
        let title = div()
            .debug_selector(|| "wiki-title".into())
            .flex_1()
            .min_w_0()
            .truncate()
            .type_style(Type::LargeSemibold)
            .text_color(theme.foreground)
            .child("Wiki");
        let set_up = matches!(&self.status, Some(Ok(s)) if s.set_up);
        let new_doc = sidebar::toolbar_button("wiki-new-document", heroicon(icons::DOCUMENT_PLUS), theme)
            .tooltip(style::tooltip(format!("New Document ({})", MenuAction::NewDocument.hint())))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.create(NodeKind::Document, window, cx)));
        let new_folder = sidebar::toolbar_button("wiki-new-folder", heroicon(icons::FOLDER_PLUS), theme)
            .tooltip(style::tooltip(format!("New Folder ({})", MenuAction::NewFolder.hint())))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.create(NodeKind::Folder, window, cx)));
        let anchor = self.menu_anchor.clone();
        let more = sidebar::toolbar_button("wiki-more", heroicon(icons::ELLIPSIS_VERTICAL), theme)
            .tooltip(style::tooltip("More"))
            .child(style::anchor_probe(&Default::default()))
            .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                let at = e.position();
                let _ = &anchor;
                this.open_menu(None, at, window, cx);
            }));
        let lead = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(12.0))
            .child(back)
            .child(title)
            .when(set_up, |d| d.child(div().flex().flex_row().gap(px(2.0)).child(new_doc).child(new_folder).child(more)));
        sidebar::header_row(
            lead,
            sidebar::HEADER_PAD_LEFT,
            Some(sidebar::search_button("wiki-search", &palette_chord, theme).on_click(
                cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(WikiEvent::OpenSearch)),
            )),
            sidebar::toggle_button("wiki-toggle", Some(true), theme)
                .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(WikiEvent::Collapse))),
        )
        .debug_selector(|| "wiki-header".into())
    }

    fn render_filter(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let empty = self.tree.filter().is_empty();
        let focus = self.filter.read(cx).focus_handle(cx);
        let focused = focus.is_focused(window);
        let field = div()
            .debug_selector(|| "wiki-filter-field".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .type_style(Type::Base)
            .text_color(theme.form_text)
            .child(
                Input::new(&self.filter)
                    .appearance(false)
                    .px(px(0.0))
                    .py(px(0.0))
                    .h_full()
                    .type_style(Type::Base),
            );
        div()
            .flex_none()
            .px(px(12.0))
            .pb(px(8.0))
            .child(
                style::search_box_base("wiki-filter", field, focused, theme)
                    .w_full()
                    .on_click(move |_, window, cx| window.focus(&focus, cx))
                    .when(!empty, |d| {
                        d.child(style::search_clear("wiki-filter-clear", theme).on_click(cx.listener(
                            |this, _: &ClickEvent, window, cx| {
                                this.clear_filter(window, cx);
                                cx.stop_propagation();
                            },
                        )))
                    })
                    .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                        if this.tree.filter().is_empty() {
                            this.focus.focus(window, cx);
                        } else {
                            this.clear_filter(window, cx);
                        }
                        cx.stop_propagation();
                    })),
            )
            .into_any_element()
    }

    /// One row of the tree.
    fn render_row(&self, ix: usize, row: &TreeRow, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let path = row.path.clone();
        let selected = self.tree.selected_path() == Some(path.as_str());
        let open = self.open.as_deref() == Some(path.as_str());
        let focused = self.focus.is_focused(window) || self.menu.is_some();
        let id = SharedString::from(format!("wiki-row-{path}"));
        let folder_icon = self.tree.node(&path).and_then(|n| n.icon.clone());
        let icon = match (row.kind, row.expanded) {
            (NodeKind::Folder, true) => icons::FOLDER_OPEN_MENU,
            (NodeKind::Folder, false) => icons::FOLDER_MENU,
            (NodeKind::Document, _) => icons::DOCUMENT_TEXT_MENU,
        };
        let chevron = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.0))
            .when(row.is_folder() && row.has_children, |d| {
                d.child(
                    heroicon(if row.expanded { icons::CHEVRON_DOWN_MENU } else { icons::CHEVRON_RIGHT_MENU })
                        .text_color(theme.icon),
                )
            });
        let mark = match folder_icon {
            Some(emoji) => div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(16.0))
                .type_style(Type::Small)
                .child(emoji)
                .into_any_element(),
            None => heroicon(icon)
                .text_color(if open { theme.accent } else { theme.icon })
                .into_any_element(),
        };
        // The indent guides: one 1pt rule per level, centred on its parent's
        // chevron.
        let guides = (0..row.depth).map(|level| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(PAD_X + 6.0 + 7.5 + level as f32 * INDENT))
                .w(px(1.0))
                .bg(theme.divider)
        });
        let renaming = self.rename.as_ref().filter(|r| r.path == path);
        let label: AnyElement = match renaming {
            Some(r) => div()
                .flex_1()
                .min_w_0()
                .h(px(ROW_H - 4.0))
                .px(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(if r.error.is_some() { theme.error } else { theme.accent })
                .bg(theme.background)
                .flex()
                .items_center()
                .debug_selector(|| "wiki-rename".into())
                .child(
                    Input::new(&r.input)
                        .appearance(false)
                        .px(px(0.0))
                        .py(px(0.0))
                        .h_full()
                        .type_style(Type::Base),
                )
                .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                    this.end_rename(window, cx);
                    cx.stop_propagation();
                }))
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if open { theme.text_loud } else { theme.foreground })
                .child(row.title.clone())
                .into_any_element(),
        };
        let drag = WikiDrag {
            path: path.clone(),
            title: row.title.clone().into(),
            icon,
        };
        let drop_edge = self
            .drop_target
            .as_ref()
            .filter(|(p, _)| *p == path)
            .map(|(_, at)| *at);
        let (click, menu_path, drop_path, move_path) = (path.clone(), path.clone(), path.clone(), path.clone());
        let is_folder = row.is_folder();
        let tip = row.name.clone();
        let fill = if selected && focused {
            Some(theme.nested(theme.background_active))
        } else if selected {
            Some(theme.nested(theme.background_hover))
        } else {
            None
        };
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .relative()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .h(px(ROW_H))
            .pl(px(PAD_X + 6.0 + row.depth as f32 * INDENT))
            .pr(px(8.0))
            .mx(px(PAD_X))
            .rounded(px(NAV_RADIUS))
            .type_style(Type::Base)
            .cursor_pointer()
            .when_some(fill, |d, f| d.bg(f))
            .when(fill.is_none(), |d| d.hover(|s| s.bg(theme.nested(theme.background_hover))))
            .when(drop_edge == Some(DropAt::Into), |d| d.bg(theme.accent.opacity(0.18)))
            .children(guides.map(|g| g.ml(px(-(PAD_X + 6.0 + row.depth as f32 * INDENT)))))
            .when(selected, |d| d.child(div().debug_selector(|| "wiki-selection".into()).absolute().size_0()))
            .when(open, |d| d.child(div().debug_selector(|| "wiki-open-row".into()).absolute().size_0()))
            .child(chevron)
            .child(mark)
            .child(label)
            .children(drop_edge.filter(|a| *a != DropAt::Into).map(|at| {
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .h(px(2.0))
                    .bg(theme.accent)
                    .when(at == DropAt::Before, |d| d.top(px(-1.0)))
                    .when(at == DropAt::After, |d| d.bottom(px(-1.0)))
            }))
            .tooltip(style::tooltip(tip))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.focus.focus(window, cx);
                this.tree.select_index(ix);
                if this.rename.as_ref().is_some_and(|r| r.path == click) {
                    return;
                }
                this.activate(window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_menu(Some(menu_path.clone()), e.position, window, cx);
                }),
            )
            .on_drag(drag, |drag: &WikiDrag, _offset, _window, cx: &mut App| {
                let (title, icon) = (drag.title.clone(), drag.icon);
                cx.new(|_| WikiDragGhost { title, icon })
            })
            .on_drag_move(cx.listener(move |this, e: &DragMoveEvent<WikiDrag>, _, cx| {
                if !e.bounds.contains(&e.event.position) {
                    if this.drop_target.as_ref().is_some_and(|(p, _)| *p == move_path) {
                        this.drop_target = None;
                        cx.notify();
                    }
                    return;
                }
                let at = drop_at(e.event.position.y, e.bounds, is_folder);
                let target = Some((move_path.clone(), at));
                if this.drop_target != target {
                    this.drop_target = target;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(move |this, drag: &WikiDrag, window, cx| {
                let at = this
                    .drop_target
                    .as_ref()
                    .filter(|(p, _)| *p == drop_path)
                    .map_or(DropAt::Into, |(_, at)| *at);
                this.drop_on(&drag.path, &drop_path, at, window, cx);
            }))
            .into_any_element()
    }

    /// The context menu, while open: at the pointer, over everything.
    fn render_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let theme = cx.global::<Theme>().opaque();
        let mut items: Vec<AnyElement> = Vec::new();
        if menu.moving {
            let path = menu.path.clone().unwrap_or_default();
            items.push(
                div()
                    .px(px(10.0))
                    .py(px(4.0))
                    .type_style(Type::XSmall)
                    .text_color(theme.muted)
                    .child("Move to")
                    .into_any_element(),
            );
            for (folder, title, depth) in self.move_targets(&path) {
                let id = SharedString::from(format!("wiki-move-{folder}"));
                let (p, f) = (path.clone(), folder.clone());
                items.push(
                    style::menu_item(id, false, &theme)
                        .child(div().w(px(depth as f32 * 12.0)).flex_none())
                        .child(heroicon(icons::FOLDER_MENU).text_color(theme.icon))
                        .child(div().flex_1().min_w_0().truncate().child(title))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.move_into(&p, &f, window, cx)))
                        .into_any_element(),
                );
            }
        } else {
            for (action, refusal) in self.menu_actions() {
                if matches!(action, MenuAction::Rename | MenuAction::Reveal | MenuAction::Delete) && !items.is_empty() {
                    items.push(style::menu_divider(&theme).into_any_element());
                }
                let id = SharedString::from(format!("wiki-menu-{}", action.slug()));
                let base = match refusal {
                    Some(why) => style::disabled_menu_item(id, &theme).tooltip(style::tooltip(why)),
                    None => style::menu_item(id, false, &theme).on_click(cx.listener(
                        move |this, _: &ClickEvent, window, cx| this.menu_action(action, window, cx),
                    )),
                };
                let base = if action == MenuAction::Delete { base.text_color(theme.error) } else { base };
                items.push(
                    base.child(div().flex_1().min_w_0().truncate().child(action.label()))
                        .child(div().flex_none().type_style(Type::XSmall).text_color(theme.muted).child(action.hint()))
                        .into_any_element(),
                );
            }
        }
        let panel = style::menu_panel("wiki-menu", MENU_W, &theme).child(
            div()
                .id("wiki-menu-scroll")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .children(items),
        );
        let menu = placed(&self.menu_anchor, panel)
            .gap(super::tab_bar::CONTEXT_GAP)
            .on_dismiss(cx.listener(|this, _: &MouseDownEvent, _, cx| this.close_menu(cx)));
        Some(gpui::deferred(menu).with_priority(1).into_any_element())
    }

    fn render_footer(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let line = match &self.status {
            None => wk::StatusLine { tone: Tone::Busy, text: "Checking…".into() },
            Some(Err(e)) => wk::StatusLine { tone: Tone::Bad, text: format!("Couldn't reach the hub: {e}") },
            Some(Ok(s)) if !s.set_up => wk::StatusLine { tone: Tone::Muted, text: "Not set up".into() },
            Some(Ok(s)) => wk::status_line(s, chrono::Utc::now()),
        };
        let color = tone_color(line.tone, theme);
        let undo = self.undo.as_ref().map(|u| {
            div()
                .debug_selector(|| "wiki-undo".into())
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .mx(px(12.0))
                .mb(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(8.0))
                .bg(theme.nested(theme.background_hover))
                .type_style(Type::Small)
                .child(div().flex_1().min_w_0().truncate().text_color(theme.foreground).child(u.label.clone()))
                .child(
                    style::button("wiki-undo-button", "Undo", ButtonKind::Ghost, ButtonSize::XSmall, false, theme)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.undo_delete(window, cx))),
                )
        });
        let notice = self.notice.as_ref().map(|n| {
            div()
                .debug_selector(|| "wiki-notice".into())
                .mx(px(12.0))
                .mb(px(6.0))
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(n.clone())
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .children(notice)
            .children(undo)
            .child(
                div()
                    .debug_selector(|| "wiki-footer".into())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(36.0))
                    .px(px(20.0))
                    .border_t_1()
                    .border_color(theme.divider)
                    .type_style(Type::XSmall)
                    .child(div().size(px(8.0)).rounded_full().flex_none().bg(color))
                    .child(div().debug_selector(|| "wiki-status".into()).min_w_0().truncate().text_color(theme.muted).child(line.text)),
            )
            .into_any_element()
    }
}

/// Where a drag over a row lands: its top and bottom quarters are before
/// and after it (a folder's; a document's halves, since nothing drops into
/// one), the rest into it.
pub fn drop_at(y: gpui::Pixels, bounds: gpui::Bounds<gpui::Pixels>, folder: bool) -> DropAt {
    let h = bounds.size.height.as_f32().max(1.0);
    let t = ((y - bounds.origin.y).as_f32() / h).clamp(0.0, 1.0);
    if !folder {
        return if t < 0.5 { DropAt::Before } else { DropAt::After };
    }
    if t < 0.25 {
        DropAt::Before
    } else if t > 0.75 {
        DropAt::After
    } else {
        DropAt::Into
    }
}

fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Good => theme.icon_green,
        Tone::Busy => theme.icon_yellow,
        Tone::Muted => theme.form_hint,
        Tone::Bad => theme.error,
    }
}

/// A centred note in the main area.
fn note(selector: &'static str, title: &'static str, detail: Option<String>, theme: &Theme) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .p(px(24.0))
        .child(div().type_style(Type::BaseMedium).text_color(theme.text_loud).child(title))
        .children(detail.map(|d| div().type_style(Type::Small).text_color(theme.muted).child(d)))
}

impl Render for WikiView {
    /// The leading column's Wiki sidebar.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let palette = sidebar::PaletteChord::of(cx);
        let header = self.render_header(palette, &theme, cx);
        let set_up = matches!(&self.status, Some(Ok(s)) if s.set_up);
        let rows = self.tree.rows();
        let body: AnyElement = if !set_up {
            let text = match &self.status {
                None => "Loading…",
                Some(Err(_)) => "The wiki's hub is out of reach.",
                Some(Ok(_)) => "This project has no wiki yet. Set one up from the main area.",
            };
            div()
                .debug_selector(|| "wiki-tree-empty".into())
                .flex_1()
                .px(px(20.0))
                .py(px(8.0))
                .type_style(Type::Small)
                .text_color(theme.muted)
                .child(text)
                .into_any_element()
        } else {
            let empty = rows.is_empty().then(|| {
                let text = if self.tree.filter().is_empty() {
                    if self.tree_error.is_some() { "Couldn't read the wiki." } else { "No documents yet." }
                } else {
                    "Nothing matches."
                };
                div()
                    .debug_selector(|| "wiki-tree-empty".into())
                    .px(px(20.0))
                    .py(px(8.0))
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(text)
            });
            let row_elements: Vec<AnyElement> =
                rows.iter().enumerate().map(|(ix, row)| self.render_row(ix, row, window, &theme, cx)).collect();
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(self.render_filter(window, &theme, cx))
                .child(
                    div()
                        .id("wiki-tree")
                        .debug_selector(|| "wiki-tree".into())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .pb(px(8.0))
                        .children(row_elements)
                        .children(empty)
                        // The space under the rows: the top level's menu, and
                        // a drop there moves to the top level.
                        .child(
                            div()
                                .id("wiki-tree-rest")
                                .flex_1()
                                .min_h(px(24.0))
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(|this, e: &MouseDownEvent, window, cx| {
                                        this.open_menu(None, e.position, window, cx)
                                    }),
                                )
                                .on_drop(cx.listener(|this, drag: &WikiDrag, window, cx| {
                                    this.drop_target = None;
                                    let path = drag.path.clone();
                                    if !edit::folder_of(&path).is_empty() {
                                        this.move_into(&path, "", window, cx);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element()
        };
        div()
            .id("wiki-sidebar")
            .debug_selector(|| "wiki-sidebar".into())
            .track_focus(&self.focus)
            .key_context("WikiTree")
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.nested(theme.sidebar))
            .text_color(theme.foreground)
            .type_style(Type::Base)
            .child(header)
            .child(body)
            .child(self.render_footer(&theme, cx))
            .children(self.render_menu(cx))
    }
}

/// The open folders kept as a set, for tests.
#[allow(dead_code)]
fn as_set(paths: Vec<String>) -> BTreeSet<String> {
    paths.into_iter().collect()
}
