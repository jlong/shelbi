//! The **Wiki** in the single-process TUI: the sidebar's Wiki row (📖, after
//! Issues) swaps the nav for the wiki's tree, as the review and workspace
//! panels do, and the main area shows the open document.
//!
//! ```text
//! ┌──────────────────────────┬─────────────────────────────────────────┐
//! │ ▄▄▄▄▄                    │ Plans › OpenCode integration            │
//! │  ←   WIKI                │ ─────────────────────────────────────── │
//! │ ▀▀▀▀▀                    │ OpenCode integration                    │
//! │ / Filter                 │ ────────────────────                    │
//! │ ▾ Plans                  │                                         │
//! │     Awareness v2         │ Context                                 │
//! │     OpenCode integration │ OpenCode is a terminal coding agent …   │
//! │   ▸ Archive              │                                         │
//! │ ▸ Research               │                                         │
//! │   Shelbi docs            │                                         │
//! │ ● Synced · 2m ago        │                                         │
//! └──────────────────────────┴─────────────────────────────────────────┘
//! ```
//!
//! The tree's state is the shared [`WikiTree`] (the desktop walks the same
//! one). Tree keys: ↑↓ / `j` `k` move, → / `l` open a folder (or step into
//! it), ← / `h` close it (or step out), Enter opens a document (or toggles a
//! folder), `n` a new document, `N` a new folder, `r` rename, `m` move, `d`
//! delete (after a `y`), `/` filter, `e` edit, Esc back to the nav. In the
//! document: the arrows, `j` `k`, Page Up/Down, Space, `g` `G` scroll, `e`
//! edits, Esc goes back to the tree.
//!
//! Every read and write goes through the project's hub (its wiki service is
//! the single writer), off the UI thread: [`WikiPage::poll`] folds the
//! answers in. A `wiki` change from the hub ([`WikiPage::changed`]) reads the
//! tree, and the open document when it's among the paths, again. `e` copies
//! the document to a temporary file and hands the terminal to `$EDITOR`
//! (the shell's run loop, [`WikiPage::take_edit`]); a changed copy is saved
//! through the hub when the editor exits, unless the document changed
//! elsewhere meanwhile (the copy is kept then, as `shelbi wiki edit` does).
//! The hub commits the save.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    Frame,
};
use shelbi_app::settings::wiki::Ask;
use shelbi_app::wiki::{Author, NodeKind, SyncOp, SyncState, WikiReply, WikiRequest, WikiStatus};
use shelbi_app::wiki_tree::{join, parent_of, WikiTree};

use crate::panel::{display_width, truncate_right, BACK_BLOCK_H, PAD};
use crate::theme::palette;

/// How often a shown Wiki tells the hub it's being looked at
/// ([`SyncOp::Active`]: pull every 30 s instead of every 5 min).
const ACTIVE_EVERY: Duration = Duration::from_secs(60);

/// What the shell does after a Wiki key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WikiStep {
    /// Nothing more (the page changed itself, or ignored the key).
    Stay,
    /// Esc on the tree: back to the nav sidebar.
    Back,
    /// The document takes the keys.
    FocusDoc,
    /// The tree takes the keys.
    FocusTree,
}

/// A document out for editing: the hub's text when it went out, and the
/// copy the editor works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WikiEdit {
    pub path: String,
    pub file: PathBuf,
    pub original: String,
}

/// A line of input the tree's footer collects.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptKind {
    /// A new document in `folder`.
    NewDoc { folder: String },
    /// A new folder in `folder`.
    NewFolder { folder: String },
    /// A new name for `path`.
    Rename { path: String },
    /// The folder to move `path` to.
    Move { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Prompt {
    kind: PromptKind,
    text: String,
}

impl Prompt {
    fn label(&self) -> String {
        let place = |folder: &str| if folder.is_empty() { "the top level".to_string() } else { folder.to_string() };
        match &self.kind {
            PromptKind::NewDoc { folder } => format!("New document in {}", place(folder)),
            PromptKind::NewFolder { folder } => format!("New folder in {}", place(folder)),
            PromptKind::Rename { .. } => "Rename to".to_string(),
            PromptKind::Move { .. } => "Move to folder (empty: top level)".to_string(),
        }
    }
}

/// The document the main area shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenDoc {
    path: String,
    /// `None` until the first read lands.
    content: Option<String>,
    error: Option<String>,
    /// The first line shown.
    scroll: usize,
}

/// A finished piece of hub work.
enum Msg {
    Status(WikiReply),
    Tree(u64, WikiReply),
    Doc(u64, String, WikiReply),
    /// A write (new, rename, move, delete, init): what it was, and the path
    /// to select (and open, when it's a document) once the tree reloads.
    Op { what: String, reply: WikiReply, select: Option<String> },
    /// An edit came back from the hub: saved, unchanged elsewhere or not.
    Saved { edit: WikiEdit, outcome: Result<(), String> },
}

/// The Wiki of one project. See the module docs.
pub(crate) struct WikiPage {
    project: String,
    ask: Ask,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub(crate) tree: WikiTree,
    /// The hub's last status answer; `None` before the first.
    status: Option<WikiStatus>,
    /// Why the last status or tree read failed.
    error: Option<String>,
    doc: Option<OpenDoc>,
    prompt: Option<Prompt>,
    /// The path a `d` asks to delete, until `y` (or anything else).
    confirm: Option<String>,
    /// Typing a filter (`/`): keys edit [`WikiTree::filter`].
    filtering: bool,
    /// The last operation's outcome, shown above the status line.
    note: Option<String>,
    /// Whether the tree stands in the sidebar's column (else the nav does,
    /// with the document still in the main area).
    pub(crate) tree_open: bool,
    /// Reads in flight, so a stale answer is dropped.
    tree_gen: u64,
    doc_gen: u64,
    /// Select (and open) this path when the next tree lands.
    pending_select: Option<String>,
    /// Edit this document once its read lands.
    edit_when_loaded: Option<String>,
    /// An edit ready for the shell to hand to `$EDITOR`.
    edit_ready: Option<WikiEdit>,
    /// The first tree row shown (scrolled to keep the selection in view).
    tree_scroll: usize,
    /// Rows the document area showed last frame, for paging.
    doc_height: usize,
    last_active: Option<Instant>,
    /// Hub requests sent and not answered yet.
    in_flight: usize,
    /// Where each visible tree row was drawn, for clicks: `(y, row index)`.
    row_hits: Vec<(u16, usize)>,
    back_hit: Option<Rect>,
}

impl WikiPage {
    pub(crate) fn new(project: impl Into<String>, ask: Ask) -> Self {
        let (tx, rx) = channel();
        Self {
            project: project.into(),
            ask,
            tx,
            rx,
            tree: WikiTree::new(),
            status: None,
            error: None,
            doc: None,
            prompt: None,
            confirm: None,
            filtering: false,
            note: None,
            tree_open: false,
            tree_gen: 0,
            doc_gen: 0,
            pending_select: None,
            edit_when_loaded: None,
            edit_ready: None,
            tree_scroll: 0,
            doc_height: 0,
            last_active: None,
            in_flight: 0,
            row_hits: Vec::new(),
            back_hit: None,
        }
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    /// The Wiki shows (its row was opened): the tree takes the sidebar's
    /// column, and the status and tree are read again.
    pub(crate) fn open(&mut self) {
        self.tree_open = true;
        self.reload();
        self.mark_active(Instant::now());
    }

    /// Read the status and tree again (and the open document).
    pub(crate) fn reload(&mut self) {
        self.read_status();
        self.read_tree();
        if let Some(path) = self.doc.as_ref().map(|d| d.path.clone()) {
            self.read_doc(&path);
        }
    }

    /// The hub says `paths` changed (empty: anything may have): read the
    /// tree again, and the open document when it's one of them.
    pub(crate) fn changed(&mut self, paths: &[String]) {
        self.read_tree();
        if let Some(path) = self.doc.as_ref().map(|d| d.path.clone()) {
            if paths.is_empty() || paths.iter().any(|p| p == &path) {
                self.read_doc(&path);
            }
        }
    }

    /// The hub's sync status moved: read it again.
    pub(crate) fn status_changed(&mut self) {
        self.read_status();
    }

    /// While the Wiki shows, tell the hub every minute so it pulls often.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self.last_active.is_none_or(|t| now.duration_since(t) >= ACTIVE_EVERY) {
            self.mark_active(now);
        }
    }

    fn mark_active(&mut self, now: Instant) {
        self.last_active = Some(now);
        if self.status.as_ref().is_some_and(|s| !s.set_up) {
            return;
        }
        let ask = self.ask.clone();
        let _ = std::thread::Builder::new()
            .name("shelbi-wiki-active".into())
            .spawn(move || drop(ask(WikiRequest::Sync(SyncOp::Active))));
    }

    /// The open document's path.
    pub(crate) fn doc_path(&self) -> Option<&str> {
        self.doc.as_ref().map(|d| d.path.as_str())
    }

    /// The first document line shown.
    #[cfg(test)]
    pub(crate) fn doc_scroll(&self) -> usize {
        self.doc.as_ref().map_or(0, |d| d.scroll)
    }

    /// The open document's text, once read.
    #[cfg(test)]
    pub(crate) fn doc_content(&self) -> Option<&str> {
        self.doc.as_ref().and_then(|d| d.content.as_deref())
    }

    #[cfg(test)]
    pub(crate) fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Whether the page is collecting typed text (a prompt or the filter),
    /// so every key is its own.
    #[cfg(test)]
    pub(crate) fn typing(&self) -> bool {
        self.prompt.is_some() || self.filtering
    }

    /// Whether hub work is still out.
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        self.in_flight > 0
    }

    /// An edit ready for `$EDITOR` (the run loop takes it).
    pub(crate) fn take_edit(&mut self) -> Option<WikiEdit> {
        self.edit_ready.take()
    }

    // ---- hub work ----------------------------------------------------------

    fn spawn(&mut self, req: WikiRequest, wrap: impl FnOnce(WikiReply) -> Msg + Send + 'static) {
        self.in_flight += 1;
        let ask = self.ask.clone();
        let tx = self.tx.clone();
        let spawned = std::thread::Builder::new()
            .name("shelbi-wiki".into())
            .spawn(move || {
                let _ = tx.send(wrap(ask(req)));
            });
        if let Err(e) = spawned {
            self.in_flight -= 1;
            self.note = Some(format!("Couldn't start the wiki's work: {e}"));
        }
    }

    fn read_status(&mut self) {
        self.spawn(WikiRequest::Status, Msg::Status);
    }

    fn read_tree(&mut self) {
        self.tree_gen += 1;
        let generation = self.tree_gen;
        self.spawn(WikiRequest::Tree { path: None }, move |r| Msg::Tree(generation, r));
    }

    fn read_doc(&mut self, path: &str) {
        self.doc_gen += 1;
        let generation = self.doc_gen;
        let p = path.to_string();
        self.spawn(
            WikiRequest::Read {
                path: path.to_string(),
                section: None,
            },
            move |r| Msg::Doc(generation, p, r),
        );
    }

    fn run_op(&mut self, what: impl Into<String>, req: WikiRequest, select: Option<String>) {
        let what = what.into();
        self.note = Some(format!("{what}…"));
        self.spawn(req, move |reply| Msg::Op { what, reply, select });
    }

    /// Fold in the hub's answers. `true` when something landed (redraw).
    pub(crate) fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok(msg) = self.rx.try_recv() {
            any = true;
            self.in_flight = self.in_flight.saturating_sub(1);
            self.apply(msg);
        }
        any
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Status(WikiReply::Status(s)) => {
                let was_not_set_up = self.status.as_ref().is_some_and(|s| !s.set_up);
                let now_set_up = s.set_up;
                self.status = Some(s);
                self.error = None;
                // Just set up (elsewhere: `shelbi wiki init`, Settings): there's
                // a tree to read.
                if now_set_up && was_not_set_up {
                    self.read_tree();
                }
            }
            Msg::Status(WikiReply::Failed(f)) => self.error = Some(f.message),
            Msg::Status(_) => {}
            Msg::Tree(generation, reply) if generation == self.tree_gen => match reply {
                WikiReply::Tree(nodes) => {
                    self.tree.set_nodes(nodes);
                    self.error = None;
                    if let Some(path) = self.pending_select.take() {
                        if self.tree.select_path(&path) && path.ends_with(".md") {
                            self.open_doc(&path);
                        }
                    }
                }
                WikiReply::Failed(f) if f.kind == shelbi_app::wiki::FailureKind::NotSetUp => {
                    self.tree.set_nodes(Vec::new());
                }
                WikiReply::Failed(f) => self.error = Some(f.message),
                _ => {}
            },
            Msg::Tree(..) => {}
            Msg::Doc(generation, path, reply) if generation == self.doc_gen => {
                let Some(doc) = self.doc.as_mut().filter(|d| d.path == path) else {
                    return;
                };
                match reply {
                    WikiReply::Doc { content, .. } => {
                        doc.content = Some(content);
                        doc.error = None;
                    }
                    WikiReply::Failed(f) => doc.error = Some(f.message),
                    _ => {}
                }
                if self.edit_when_loaded.as_deref() == Some(path.as_str()) && doc.content.is_some() {
                    self.edit_when_loaded = None;
                    self.prepare_edit();
                }
            }
            Msg::Doc(..) => {}
            Msg::Op { what, reply, select } => match reply {
                WikiReply::Failed(f) => self.note = Some(format!("{what} failed: {}", f.message)),
                WikiReply::Status(s) => {
                    self.status = Some(s);
                    self.note = None;
                    self.read_tree();
                }
                _ => {
                    self.note = None;
                    self.pending_select = select;
                    self.read_tree();
                    self.read_status();
                }
            },
            Msg::Saved { edit, outcome } => {
                match outcome {
                    Ok(()) => {
                        self.note = Some(format!("Saved {}", edit.path));
                        remove_edit_copy(&edit);
                        self.read_status();
                    }
                    Err(message) => self.note = Some(message),
                }
                if self.doc_path() == Some(edit.path.as_str()) {
                    self.read_doc(&edit.path);
                }
            }
        }
    }

    // ---- actions -----------------------------------------------------------

    /// Show `path` in the main area (read through the hub).
    pub(crate) fn open_doc(&mut self, path: &str) {
        if self.doc_path() != Some(path) {
            self.doc = Some(OpenDoc {
                path: path.to_string(),
                content: None,
                error: None,
                scroll: 0,
            });
        }
        self.read_doc(path);
    }

    /// `e`: edit `path` (the selected document, or the open one) in
    /// `$EDITOR`, once its text is in.
    fn edit(&mut self, path: String) {
        let loaded = self.doc.as_ref().is_some_and(|d| d.path == path && d.content.is_some());
        if loaded {
            self.prepare_edit();
        } else {
            self.edit_when_loaded = Some(path.clone());
            self.open_doc(&path);
        }
    }

    /// Copy the open document to a temporary file for the editor.
    fn prepare_edit(&mut self) {
        let Some(doc) = self.doc.as_ref() else {
            return;
        };
        let Some(original) = doc.content.clone() else {
            return;
        };
        let name = doc.path.rsplit('/').next().unwrap_or(&doc.path);
        // One folder per edit: two edits (or two shells) never share a copy.
        static EDITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = EDITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("shelbi-wiki-edit-{}-{n}", std::process::id()));
        let file = dir.join(name);
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, &original));
        match written {
            Ok(()) => {
                self.edit_ready = Some(WikiEdit {
                    path: doc.path.clone(),
                    file,
                    original,
                })
            }
            Err(e) => self.note = Some(format!("Couldn't copy {} for the editor: {e}", doc.path)),
        }
    }

    /// The editor exited (`Err`: it couldn't start, or failed). A changed
    /// copy goes to the hub, unless the document changed there meanwhile.
    pub(crate) fn finish_edit(&mut self, edit: WikiEdit, editor: Result<(), String>) {
        if let Err(e) = editor {
            self.note = Some(format!("{e}; nothing was saved (your copy: {})", edit.file.display()));
            return;
        }
        let edited = match std::fs::read_to_string(&edit.file) {
            Ok(text) => text,
            Err(e) => {
                self.note = Some(format!("Couldn't read {}: {e}", edit.file.display()));
                return;
            }
        };
        if edited == edit.original {
            self.note = Some(format!("No changes to {}", edit.path));
            remove_edit_copy(&edit);
            return;
        }
        self.note = Some(format!("Saving {}…", edit.path));
        self.in_flight += 1;
        let ask = self.ask.clone();
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new().name("shelbi-wiki-save".into()).spawn(move || {
            let outcome = save_edit(&ask, &edit, edited);
            let _ = tx.send(Msg::Saved { edit, outcome });
        });
    }

    fn submit_prompt(&mut self, prompt: Prompt) {
        let text = prompt.text.trim().to_string();
        match prompt.kind {
            PromptKind::NewDoc { folder } => {
                if text.is_empty() {
                    return;
                }
                let path = join(&folder, &with_md(&text));
                let req = WikiRequest::New {
                    path: path.clone(),
                    content: None,
                    title: None,
                    by: Author::Person,
                };
                self.run_op(format!("Creating {path}"), req, Some(path));
            }
            PromptKind::NewFolder { folder } => {
                if text.is_empty() {
                    return;
                }
                let path = join(&folder, text.trim_end_matches('/'));
                let req = WikiRequest::New {
                    path: path.clone(),
                    content: None,
                    title: None,
                    by: Author::Person,
                };
                self.run_op(format!("Creating {path}"), req, Some(path));
            }
            PromptKind::Rename { path } => {
                if text.is_empty() || text.contains('/') {
                    self.note = Some("A new name, not a path (m moves it)".into());
                    return;
                }
                let is_doc = self.tree.node(&path).is_some_and(|n| n.kind == NodeKind::Document);
                let name = if is_doc { with_md(&text) } else { text };
                let to = join(parent_of(&path).unwrap_or_default(), &name);
                self.move_entry(path, to, "Renaming");
            }
            PromptKind::Move { path } => {
                let folder = text.trim_matches('/');
                let name = path.rsplit('/').next().unwrap_or(&path).to_string();
                let to = join(folder, &name);
                self.move_entry(path, to, "Moving");
            }
        }
    }

    fn move_entry(&mut self, from: String, to: String, verb: &str) {
        if from == to {
            return;
        }
        // The open document goes with it.
        let open = self.doc_path().is_some_and(|p| p == from || p.starts_with(&format!("{from}/")));
        if open {
            if let Some(doc) = self.doc.as_mut() {
                doc.path = format!("{to}{}", &doc.path[from.len()..]);
            }
        }
        let req = WikiRequest::Move {
            from: from.clone(),
            to: to.clone(),
            by: Author::Person,
        };
        self.run_op(format!("{verb} {from}"), req, Some(to));
    }

    fn delete(&mut self, path: String) {
        let open = self.doc_path().is_some_and(|p| p == path || p.starts_with(&format!("{path}/")));
        if open {
            self.doc = None;
        }
        let req = WikiRequest::Remove {
            path: path.clone(),
            section: None,
            by: Author::Person,
        };
        self.run_op(format!("Deleting {path}"), req, None);
    }

    // ---- keys --------------------------------------------------------------

    /// A key while the tree has the keyboard.
    pub(crate) fn handle_tree_key(&mut self, k: KeyEvent) -> WikiStep {
        if let Some(mut prompt) = self.prompt.take() {
            match k.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.submit_prompt(prompt),
                _ => {
                    edit_line(&mut prompt.text, &k);
                    self.prompt = Some(prompt);
                }
            }
            return WikiStep::Stay;
        }
        if let Some(path) = self.confirm.take() {
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.delete(path);
            }
            return WikiStep::Stay;
        }
        if self.filtering {
            match k.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.tree.set_filter("");
                }
                KeyCode::Enter => self.filtering = false,
                // The selection still moves while typing a filter.
                KeyCode::Up => self.tree.move_up(),
                KeyCode::Down => self.tree.move_down(),
                _ => {
                    let mut text = self.tree.filter().to_string();
                    edit_line(&mut text, &k);
                    self.tree.set_filter(text);
                }
            }
            return WikiStep::Stay;
        }
        if self.status.as_ref().is_some_and(|s| !s.set_up) {
            return match k.code {
                KeyCode::Char('i') => {
                    self.run_op("Setting up the wiki", WikiRequest::Init, None);
                    WikiStep::Stay
                }
                KeyCode::Esc => WikiStep::Back,
                _ => WikiStep::Stay,
            };
        }
        let selected = self.tree.selected_row();
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.tree.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.tree.move_down(),
            KeyCode::Home | KeyCode::Char('g') => self.tree.move_to_first(),
            KeyCode::End | KeyCode::Char('G') => self.tree.move_to_last(),
            KeyCode::Right | KeyCode::Char('l') => self.tree.expand(),
            KeyCode::Left | KeyCode::Char('h') => self.tree.collapse(),
            KeyCode::Enter | KeyCode::Char(' ') => match selected {
                Some(row) if row.is_folder() => self.tree.toggle(),
                Some(row) => {
                    self.open_doc(&row.path);
                    return WikiStep::FocusDoc;
                }
                None => {}
            },
            KeyCode::Char('n') => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::NewDoc {
                        folder: self.tree.folder_for_new(),
                    },
                    text: String::new(),
                })
            }
            KeyCode::Char('N') => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::NewFolder {
                        folder: self.tree.folder_for_new(),
                    },
                    text: String::new(),
                })
            }
            KeyCode::Char('r') => {
                if let Some(row) = selected {
                    let text = row.name.strip_suffix(".md").unwrap_or(&row.name).to_string();
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Rename { path: row.path },
                        text,
                    });
                }
            }
            KeyCode::Char('m') => {
                if let Some(row) = selected {
                    let text = parent_of(&row.path).unwrap_or_default().to_string();
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Move { path: row.path },
                        text,
                    });
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(row) = selected {
                    self.confirm = Some(row.path);
                }
            }
            KeyCode::Char('/') => self.filtering = true,
            KeyCode::Char('e') => match selected {
                Some(row) if !row.is_folder() => self.edit(row.path),
                _ => {}
            },
            KeyCode::Esc => {
                if !self.tree.filter().is_empty() {
                    self.tree.set_filter("");
                } else {
                    return WikiStep::Back;
                }
            }
            _ => {}
        }
        WikiStep::Stay
    }

    /// A key while the document has the keyboard.
    pub(crate) fn handle_doc_key(&mut self, k: KeyEvent) -> WikiStep {
        let page = self.doc_height.max(2) - 1;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return WikiStep::FocusTree,
            KeyCode::Char('e') => {
                if let Some(path) = self.doc_path().map(str::to_string) {
                    self.edit(path);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::Char('d') if ctrl => self.scroll_by(page as isize / 2),
            KeyCode::Char('u') if ctrl => self.scroll_by(-(page as isize) / 2),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_by(page as isize),
            KeyCode::PageUp => self.scroll_by(-(page as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.scroll_by(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.scroll_by(isize::MAX / 2),
            _ => {}
        }
        WikiStep::Stay
    }

    /// Scroll the document by `delta` lines (clamped when it draws).
    pub(crate) fn scroll_by(&mut self, delta: isize) {
        if let Some(doc) = self.doc.as_mut() {
            doc.scroll = doc.scroll.saturating_add_signed(delta);
        }
    }

    /// A click in the sidebar's column at `(x, y)`.
    pub(crate) fn click_tree(&mut self, x: u16, y: u16) -> WikiStep {
        if self.back_hit.is_some_and(|r| crate::panel::in_rect(r, x, y)) {
            return WikiStep::Back;
        }
        let Some(&(_, index)) = self.row_hits.iter().find(|(row_y, _)| *row_y == y) else {
            return WikiStep::Stay;
        };
        self.tree.select_index(index);
        match self.tree.selected_row() {
            Some(row) if row.is_folder() => self.tree.toggle(),
            Some(row) => self.open_doc(&row.path),
            None => {}
        }
        WikiStep::Stay
    }

    // ---- drawing -----------------------------------------------------------

    /// The sync status line: a dot in the state's color, the state, and how
    /// long ago the last clean sync was.
    pub(crate) fn status_line(&self, now: chrono::DateTime<chrono::Utc>) -> Line<'static> {
        let p = palette();
        let Some(status) = self.status.as_ref() else {
            return Line::from(Span::styled("○ Reading the wiki…", Style::default().fg(p.ansi_gray)));
        };
        if !status.set_up {
            return Line::from(Span::styled("○ Not set up", Style::default().fg(p.ansi_gray)));
        }
        let (color, word) = match &status.state {
            SyncState::Synced => (p.ansi_green, "Synced".to_string()),
            SyncState::Syncing => (p.ansi_yellow, "Syncing".to_string()),
            SyncState::Pending => (p.ansi_yellow, "Pending".to_string()),
            SyncState::Offline => (p.ansi_gray, "Offline".to_string()),
            SyncState::Conflict => (p.ansi_red, format!("Conflict ({})", status.conflicts.len())),
            SyncState::Error(detail) => (p.ansi_red, format!("Error: {detail}")),
        };
        let mut spans = vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(word, Style::default().fg(p.ansi_white)),
        ];
        if status.paused {
            spans.push(Span::styled(" · paused", Style::default().fg(p.ansi_gray)));
        }
        if let Some(ago) = status.last_synced.as_deref().and_then(|t| ago(t, now)) {
            spans.push(Span::styled(format!(" · {ago}"), Style::default().fg(p.ansi_gray)));
        }
        Line::from(spans)
    }

    /// Draw the tree into the sidebar's column.
    pub(crate) fn render_tree(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        self.row_hits.clear();
        self.back_hit = None;
        if area.width < 4 || area.height < 4 {
            return;
        }
        let p = palette();
        crate::panel::render_back_button(f, area, false, "WIKI", Style::default().fg(p.ansi_white).add_modifier(Modifier::BOLD));
        self.back_hit = Some(Rect {
            x: area.x + PAD,
            y: area.y,
            width: crate::panel::BACK_BTN_WIDTH as u16,
            height: BACK_BLOCK_H.min(area.height),
        });
        let buf = f.buffer_mut();
        let x = area.x + 1;
        let w = area.width.saturating_sub(2) as usize;
        let bottom = area.bottom();
        let mut y = area.y + BACK_BLOCK_H + 1;

        // The footer, bottom up: the status line, the note, the prompt.
        let mut footer: Vec<Line<'static>> = Vec::new();
        if let Some(prompt) = &self.prompt {
            footer.push(Line::from(Span::styled(prompt.label(), Style::default().fg(p.ansi_gray))));
            footer.push(Line::from(vec![
                Span::styled(prompt.text.clone(), Style::default().fg(p.ansi_white).bg(p.input_focus)),
                Span::styled("▏", Style::default().fg(p.ansi_white).bg(p.input_focus)),
            ]));
        } else if let Some(path) = &self.confirm {
            let title = self.tree.node(path).map_or(path.clone(), |n| n.title.clone());
            footer.push(Line::from(Span::styled(format!("Delete {title}? y / n"), Style::default().fg(p.ansi_red))));
        }
        if let Some(note) = self.note.as_ref().or(self.error.as_ref()) {
            footer.push(Line::from(Span::styled(note.clone(), Style::default().fg(p.ansi_gray))));
        }
        footer.push(self.status_line(chrono::Utc::now()));
        let footer_h = (footer.len() as u16 + 1).min(area.height.saturating_sub(y - area.y));
        let footer_top = bottom.saturating_sub(footer_h) + 1;
        for (i, line) in footer.iter().enumerate() {
            let fy = footer_top + i as u16;
            if fy >= bottom {
                break;
            }
            buf.set_line(x, fy, line, w as u16);
        }
        let list_bottom = footer_top.saturating_sub(1);

        // The filter.
        if y < list_bottom {
            let (text, style) = if self.filtering || !self.tree.filter().is_empty() {
                let cursor = if self.filtering { "▏" } else { "" };
                (format!("/ {}{cursor}", self.tree.filter()), Style::default().fg(p.ansi_white))
            } else {
                ("/ Filter".to_string(), Style::default().fg(p.ansi_dark_gray))
            };
            buf.set_stringn(x, y, truncate_right(&text, w), w, style);
            y += 2;
        }

        if self.status.as_ref().is_some_and(|s| !s.set_up) {
            let lines = [
                "This project has no wiki yet.",
                "",
                "Press i to set one up on the",
                "docs/planning branch",
                "(Settings ▸ Wiki has the options).",
            ];
            for line in lines {
                if y >= list_bottom {
                    break;
                }
                buf.set_stringn(x, y, truncate_right(line, w), w, Style::default().fg(p.ansi_gray));
                y += 1;
            }
            return;
        }

        let rows = self.tree.rows();
        if rows.is_empty() {
            let text = if !self.tree.is_loaded() {
                "Loading…"
            } else if self.tree.filter().is_empty() {
                "No documents yet. n adds one."
            } else {
                "Nothing matches."
            };
            if y < list_bottom {
                buf.set_stringn(x, y, truncate_right(text, w), w, Style::default().fg(p.ansi_dark_gray));
            }
            return;
        }
        let room = list_bottom.saturating_sub(y) as usize;
        let selected = self.tree.selected_index();
        if let Some(sel) = selected {
            if sel < self.tree_scroll {
                self.tree_scroll = sel;
            } else if room > 0 && sel >= self.tree_scroll + room {
                self.tree_scroll = sel + 1 - room;
            }
        }
        self.tree_scroll = self.tree_scroll.min(rows.len().saturating_sub(room.max(1)));
        for (i, row) in rows.iter().enumerate().skip(self.tree_scroll).take(room) {
            let is_selected = selected == Some(i);
            let glyph = match (row.is_folder(), row.expanded) {
                (true, true) => "▾ ",
                (true, false) => "▸ ",
                (false, _) => "  ",
            };
            let text = format!("{}{glyph}{}", "  ".repeat(row.depth), row.title);
            let mut style = if row.is_folder() {
                Style::default().fg(p.ansi_white).add_modifier(Modifier::BOLD)
            } else if self.doc_path() == Some(row.path.as_str()) {
                Style::default().fg(p.ansi_cyan)
            } else {
                Style::default().fg(p.ansi_white)
            };
            if is_selected {
                style = style.bg(if focused { p.selection } else { p.row_hover });
                buf.set_stringn(area.x, y, " ".repeat(area.width as usize), area.width as usize, style);
            }
            buf.set_stringn(x, y, truncate_right(&text, w), w, style);
            self.row_hits.push((y, i));
            y += 1;
        }
    }

    /// Draw the open document into the main area.
    pub(crate) fn render_doc(&mut self, buf: &mut Buffer, area: Rect) {
        if area.width < 4 || area.height < 3 {
            return;
        }
        let p = palette();
        let x = area.x + 2;
        let w = area.width.saturating_sub(4) as usize;
        let Some(doc) = self.doc.as_mut() else {
            let text = if self.status.as_ref().is_some_and(|s| !s.set_up) {
                "No wiki yet: press i in the tree to set one up."
            } else {
                "Pick a document in the tree (Enter opens it)."
            };
            buf.set_stringn(x, area.y + area.height / 2, truncate_right(text, w), w, Style::default().fg(p.ansi_dark_gray));
            return;
        };
        // The breadcrumb: the folders, then the document's title.
        let title = self.tree.node(&doc.path).map_or_else(
            || doc.path.rsplit('/').next().unwrap_or(&doc.path).to_string(),
            |n| n.title.clone(),
        );
        let mut crumbs: Vec<String> = doc.path.split('/').map(str::to_string).collect();
        crumbs.pop();
        crumbs.push(title);
        let crumb = crumbs.join(" › ");
        buf.set_stringn(x, area.y, truncate_right(&crumb, w), w, Style::default().fg(p.ansi_gray));
        buf.set_stringn(x, area.y + 1, "─".repeat(w), w, Style::default().fg(p.ansi_dark_gray));
        let top = area.y + 2;
        let height = area.bottom().saturating_sub(top) as usize;
        self.doc_height = height;
        let lines = match (&doc.content, &doc.error) {
            (_, Some(e)) => vec![Line::from(Span::styled(e.clone(), Style::default().fg(p.ansi_red)))],
            (None, None) => vec![Line::from(Span::styled("Loading…", Style::default().fg(p.ansi_dark_gray)))],
            (Some(text), None) => crate::markdown::render_document(text, w),
        };
        doc.scroll = doc.scroll.min(lines.len().saturating_sub(height));
        for (i, line) in lines.iter().skip(doc.scroll).take(height).enumerate() {
            buf.set_line(x, top + i as u16, line, w as u16);
        }
        // A scroll position at the right when there's more than fits.
        if lines.len() > height && height > 0 {
            let pct = (doc.scroll * 100) / lines.len().saturating_sub(height).max(1);
            let label = format!("{pct}%");
            let lx = area.right().saturating_sub(display_width(&label) as u16 + 1);
            buf.set_stringn(lx, area.y, &label, label.len(), Style::default().fg(p.ansi_dark_gray));
        }
    }
}

/// Save an edited copy: unless the hub's text moved on since it went out.
fn save_edit(ask: &Ask, edit: &WikiEdit, edited: String) -> Result<(), String> {
    let now = match ask(WikiRequest::Read {
        path: edit.path.clone(),
        section: None,
    }) {
        WikiReply::Doc { content, .. } => content,
        WikiReply::Failed(f) => return Err(format!("Couldn't save {}: {}", edit.path, f.message)),
        other => return Err(format!("Couldn't save {}: the hub answered {other:?}", edit.path)),
    };
    if now != edit.original {
        return Err(format!(
            "{} changed elsewhere while you edited it; nothing was saved. Your copy is at {}",
            edit.path,
            edit.file.display()
        ));
    }
    match ask(WikiRequest::Write {
        path: edit.path.clone(),
        content: edited,
        section: None,
        by: Author::Person,
    }) {
        WikiReply::Done => Ok(()),
        WikiReply::Failed(f) => Err(format!("Couldn't save {}: {}", edit.path, f.message)),
        other => Err(format!("Couldn't save {}: the hub answered {other:?}", edit.path)),
    }
}

fn remove_edit_copy(edit: &WikiEdit) {
    if let Some(dir) = edit.file.parent() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// `name` as a document's file name.
fn with_md(name: &str) -> String {
    if name.ends_with(".md") {
        name.to_string()
    } else {
        format!("{name}.md")
    }
}

/// A one-line text field's key: type, Backspace, Ctrl+U clears.
fn edit_line(text: &mut String, k: &KeyEvent) {
    match k.code {
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => text.clear(),
        KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
        _ => {}
    }
}

/// How long ago RFC3339 `when` was, roughly (`just now`, `2m ago`, `3h
/// ago`, `4d ago`).
fn ago(when: &str, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let then = chrono::DateTime::parse_from_rfc3339(when).ok()?;
    let secs = (now - then.with_timezone(&chrono::Utc)).num_seconds().max(0);
    Some(match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    })
}

#[cfg(test)]
#[path = "wiki_tests.rs"]
pub(crate) mod tests;
