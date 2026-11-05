//! A diff view's life: the [`DiffState`], and the worker threads that read and
//! write for it, so a client's UI thread never waits on git or a disk.
//!
//! A client opens a session on a target, passes it keys, carries out what comes
//! back, and calls [`DiffSession::poll`] when woken (and each frame, if it has
//! frames). Threads and `std::sync::mpsc` only, so a gpui client takes it
//! without tokio, like [`crate::refresh`].
//!
//! - The **loader** reads changesets: on open, on `r`, on a scope change, and
//!   every [`DiffOptions::watch`] interval for a live workspace. A re-read whose
//!   digest matches what's on show publishes nothing.
//! - The **highlighter** works through a changeset's files, the one in view
//!   first. A newer changeset abandons the old one's.
//! - The **store** applies comment and viewed changes in order, so a slow
//!   remote read never delays a save.
//!
//! Dropping the session ends all three; none is joined, so dropping never
//! blocks on a git command still running (each has a deadline).

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use shelbi_state::keymap::{DiffAction, KeyChord, ModeKeymap};

use super::backend::{backend_for, DiffBackend, DiffTarget, TargetKind};
use super::comments::{Author, CommentDoc, CommentOp};
use super::git::BlameLine;
use super::highlight::{highlight_file, FileHighlights};
use super::model::{Changeset, CommitScope};
use super::state::{DiffEffect, DiffState};
use super::store::{ViewedDoc, ViewedOp};
use crate::source::ProjectLink;

/// How a live workspace diff re-reads: often enough to follow an agent, slowly
/// enough not to matter.
pub const WORKSPACE_WATCH: Duration = Duration::from_secs(2);

/// A watch waits at least this many times as long as its last read took.
const WATCH_BACKOFF: u32 = 4;

/// Tells a client's event loop there's something to [`DiffSession::poll`].
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a session behaves.
#[derive(Clone)]
pub struct DiffOptions {
    /// Signs new comments.
    pub author: Author,
    /// Re-read this often. `None` reads only when asked (a review's checkout
    /// doesn't move under it).
    pub watch: Option<Duration>,
    pub wake: Wake,
}

/// How a loaded changeset came to be.
enum Loaded {
    Changeset(Arc<Changeset>),
    Failed(String),
}

enum Event {
    /// `generation` is stale once a newer load has been asked for. `published`
    /// says which changeset this is (a watch's re-read is a new one in the same
    /// generation).
    Loaded { generation: u64, published: u64, loaded: Loaded },
    /// Highlights for changeset `published`.
    Highlights { published: u64, file: u32, highlights: Arc<FileHighlights> },
    Blame { path: String, blame: Vec<BlameLine> },
    /// The task's comments as read.
    Comments(CommentDoc),
    /// The oldest comment save still out landed, with the file it left.
    CommentSaved(Result<CommentDoc, String>),
    Viewed(ViewedDoc),
    /// A read of the task's review state or a viewed save failed, with why.
    StoreFailed(String),
}

enum LoadCommand {
    Load { scope: CommitScope, generation: u64 },
    Blame { path: String, rev: Option<String> },
    /// The watch is back on: read now, not at the next interval.
    Resume,
}

enum StoreCommand {
    Comment(CommentOp),
    Viewed(ViewedOp),
}

/// An open diff view.
pub struct DiffSession {
    state: DiffState,
    events: Receiver<Event>,
    load: Sender<LoadCommand>,
    store: Sender<StoreCommand>,
    /// The newest load asked for.
    generation: u64,
    /// The changeset on show.
    published: u64,
    /// The file the highlighter should do next.
    priority: Arc<AtomicUsize>,
    scope: CommitScope,
    /// Why the task's review state couldn't be read or saved, until the next
    /// one works.
    store_error: Option<String>,
    /// Comment changes applied here and not yet saved. A document read while
    /// one is out doesn't have it, so they're applied over what's read: a
    /// fresh comment doesn't blink out and take the cursor with it.
    unsaved: VecDeque<CommentOp>,
    /// The blame reads asked for since the last changeset.
    blame_asked: HashSet<String>,
    /// Nobody's looking: a live watch stops reading until they are.
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Drop for DiffSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl DiffSession {
    /// Open a view over `backend`, and start reading. Returns at once.
    /// `open_backend` runs on the loader thread, so resolving it may block.
    pub fn open(
        open_backend: impl FnOnce() -> Result<Arc<dyn DiffBackend>, String> + Send + 'static,
        options: DiffOptions,
    ) -> DiffSession {
        let (events_tx, events) = mpsc::channel();
        let (load, load_rx) = mpsc::channel();
        let (store, store_rx) = mpsc::channel();
        let priority = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let shared = Shared {
            events: events_tx,
            wake: options.wake.clone(),
            published: Arc::new(AtomicU64::new(0)),
            priority: priority.clone(),
            paused: paused.clone(),
            stop: stop.clone(),
        };
        let watch = options.watch;
        let session = DiffSession {
            state: DiffState::new(options.author),
            events,
            load: load.clone(),
            store,
            generation: 0,
            published: 0,
            priority,
            scope: CommitScope::All,
            store_error: None,
            unsaved: VecDeque::new(),
            blame_asked: HashSet::new(),
            paused,
            stop,
        };
        let mut session = session;
        let spawned = thread::Builder::new().name("diff-loader".into()).spawn(move || match open_backend() {
            Ok(backend) => run_workers(backend, load_rx, store_rx, shared, watch),
            Err(e) => shared.send(Event::Loaded {
                generation: 0,
                published: 0,
                loaded: Loaded::Failed(e),
            }),
        });
        match spawned {
            Ok(_) => {
                let _ = load.send(LoadCommand::Load { scope: CommitScope::All, generation: 0 });
            }
            Err(e) => session.state.set_error(format!("Couldn't start reading the diff: {e}")),
        }
        session
    }

    /// Open a view of `target`, as seen through `link`: a live watch for a
    /// workspace, reads on request for a review.
    pub fn for_target(target: DiffTarget, link: Option<ProjectLink>, author: Author, wake: Wake) -> DiffSession {
        let options = DiffOptions {
            author,
            watch: (target.kind == TargetKind::Workspace).then_some(WORKSPACE_WATCH),
            wake,
        };
        DiffSession::open(move || backend_for(&target, link.as_ref()), options)
    }

    pub fn state(&self) -> &DiffState {
        &self.state
    }

    /// The state itself, for what a client sets directly: its viewport, a
    /// click, a scroll.
    pub fn state_mut(&mut self) -> &mut DiffState {
        &mut self.state
    }

    /// Why the task's comments or viewed marks can't be read or saved.
    pub fn store_error(&self) -> Option<&str> {
        self.store_error.as_deref()
    }

    /// A key. Returns what only the client can do: copy, open an editor, send
    /// to the agent. The rest goes to the workers.
    pub fn key(&mut self, chord: KeyChord, keymap: &ModeKeymap<DiffAction>) -> Vec<DiffEffect> {
        let effects = self.state.key(chord, keymap);
        self.carry_out(effects)
    }

    /// An action from a menu or a button, as if its key were pressed.
    pub fn action(&mut self, action: DiffAction) -> Vec<DiffEffect> {
        let effects = self.state.apply(action, None);
        self.carry_out(effects)
    }

    /// Carry out what a [`DiffState`] call answered (one made through
    /// [`Self::state_mut`], like `set_viewed` or `mark_sent`).
    pub fn carry_out(&mut self, effects: Vec<DiffEffect>) -> Vec<DiffEffect> {
        let mut for_client = Vec::new();
        for effect in effects {
            match effect {
                DiffEffect::Refresh => self.request_load(self.scope.clone()),
                DiffEffect::LoadScope(scope) => {
                    self.scope = scope.clone();
                    self.request_load(scope);
                }
                DiffEffect::LoadBlame { path, rev } => self.ask_blame(path, rev),
                DiffEffect::SaveComment(op) => {
                    self.unsaved.push_back(op.clone());
                    let _ = self.store.send(StoreCommand::Comment(op));
                }
                DiffEffect::SaveViewed(op) => {
                    let _ = self.store.send(StoreCommand::Viewed(op));
                }
                other @ (DiffEffect::Yank(_)
                | DiffEffect::OpenInEditor { .. }
                | DiffEffect::SendToAgent { .. }) => for_client.push(other),
            }
        }
        self.after_input();
        for_client
    }

    /// Whether anyone's looking. A live workspace's watch rests while the view
    /// is hidden, and catches up the moment it's back, publishing only if
    /// something changed, so the place and the highlights on show stay.
    pub fn set_visible(&mut self, visible: bool) {
        let was_paused = self.paused.swap(!visible, Ordering::Relaxed);
        if was_paused && visible {
            let _ = self.load.send(LoadCommand::Resume);
        }
    }

    /// Read the changeset again now.
    pub fn refresh(&mut self) {
        self.request_load(self.scope.clone());
    }

    fn request_load(&mut self, scope: CommitScope) {
        self.generation += 1;
        let _ = self.load.send(LoadCommand::Load { scope, generation: self.generation });
    }

    /// Tell the workers where the cursor is: its file highlights next, and
    /// blame follows it when the gutter is on.
    fn after_input(&mut self) {
        if let Some(file) = self.state.current_file() {
            self.priority.store(file, Ordering::Relaxed);
        }
        if let Some(DiffEffect::LoadBlame { path, rev }) = self.state.blame_request() {
            self.ask_blame(path, rev);
        }
    }

    /// Ask once: a file blame can't read (a new file) isn't asked again.
    fn ask_blame(&mut self, path: String, rev: Option<String>) {
        if self.blame_asked.insert(path.clone()) {
            let _ = self.load.send(LoadCommand::Blame { path, rev });
        }
    }

    /// A document as stored, with the changes not saved yet on top.
    fn show_comments(&mut self, mut doc: CommentDoc) {
        for op in &self.unsaved {
            doc.apply(op);
        }
        self.state.set_comments(doc);
    }

    /// Take in what the workers have finished. Returns whether anything
    /// changed, so a client knows to redraw.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        // What a click or a scroll moved to, through `state_mut`.
        self.after_input();
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                Event::Loaded { generation, published, loaded } => {
                    if generation != self.generation {
                        continue;
                    }
                    match loaded {
                        Loaded::Changeset(changeset) => {
                            self.published = published;
                            self.blame_asked.clear();
                            self.state.set_changeset(changeset);
                        }
                        Loaded::Failed(e) => self.state.set_error(e),
                    }
                    self.after_input();
                }
                Event::Highlights { published, file, highlights } => {
                    if published == self.published {
                        self.state.set_highlights(file, highlights);
                    }
                }
                Event::Blame { path, blame } => self.state.set_blame(path, blame),
                Event::Comments(doc) => {
                    self.store_error = None;
                    self.show_comments(doc);
                }
                Event::CommentSaved(result) => {
                    self.unsaved.pop_front();
                    match result {
                        Ok(doc) => {
                            self.store_error = None;
                            self.show_comments(doc);
                        }
                        // What's on show stays, so the words aren't lost; the
                        // client says it wasn't saved.
                        Err(e) => self.store_error = Some(e),
                    }
                }
                Event::Viewed(doc) => {
                    self.store_error = None;
                    self.state.set_viewed_doc(doc);
                }
                Event::StoreFailed(e) => self.store_error = Some(e),
            }
        }
        changed
    }
}

/// What every worker holds.
#[derive(Clone)]
struct Shared {
    events: Sender<Event>,
    wake: Wake,
    /// The changeset published last. A highlighter for an older one stops.
    published: Arc<AtomicU64>,
    priority: Arc<AtomicUsize>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Shared {
    fn send(&self, event: Event) {
        if self.events.send(event).is_ok() {
            (self.wake)();
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// The loader and store threads, once the backend is known.
fn run_workers(
    backend: Arc<dyn DiffBackend>,
    load: Receiver<LoadCommand>,
    store: Receiver<StoreCommand>,
    shared: Shared,
    watch: Option<Duration>,
) {
    let (store_backend, store_shared) = (backend.clone(), shared.clone());
    thread::Builder::new()
        .name("diff-store".into())
        .spawn(move || run_store(store_backend, store, store_shared))
        .expect("spawning the diff store");
    run_loader(backend, load, shared, watch);
}

fn run_store(backend: Arc<dyn DiffBackend>, commands: Receiver<StoreCommand>, shared: Shared) {
    send_review_state(&*backend, &shared);
    for command in commands {
        let event = match command {
            StoreCommand::Comment(op) => Event::CommentSaved(backend.apply_comment(&op)),
            StoreCommand::Viewed(op) => backend.apply_viewed(&op).map(Event::Viewed).unwrap_or_else(Event::StoreFailed),
        };
        shared.send(event);
    }
}

/// Read the task's comments and viewed marks.
fn send_review_state(backend: &dyn DiffBackend, shared: &Shared) {
    shared.send(backend.comments().map(Event::Comments).unwrap_or_else(Event::StoreFailed));
    shared.send(backend.viewed().map(Event::Viewed).unwrap_or_else(Event::StoreFailed));
}

fn run_loader(
    backend: Arc<dyn DiffBackend>,
    commands: Receiver<LoadCommand>,
    shared: Shared,
    watch: Option<Duration>,
) {
    let mut scope = CommitScope::All;
    let mut generation = 0;
    let mut digest: Option<u64> = None;
    let mut next_watch = watch.map(|w| Instant::now() + w);
    loop {
        if shared.stopped() {
            return;
        }
        let wait = next_watch.map_or(Duration::from_secs(60), |at| at.saturating_duration_since(Instant::now()));
        match commands.recv_timeout(wait) {
            Ok(LoadCommand::Load { scope: asked, generation: asked_generation }) => {
                scope = asked;
                generation = asked_generation;
                // Asked for: it publishes even if nothing changed, which is
                // what ends the client's wait.
                digest = None;
                read_changeset(&*backend, &scope, generation, &mut digest, &shared);
            }
            Ok(LoadCommand::Blame { path, rev }) => {
                if let Ok(blame) = backend.blame(&path, rev.as_deref()) {
                    shared.send(Event::Blame { path, blame });
                }
            }
            Ok(LoadCommand::Resume) => {
                if watch.is_some() {
                    next_watch = Some(Instant::now());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(interval) = watch {
                    if shared.paused.load(Ordering::Relaxed) {
                        // Woken by Resume, or another interval from now.
                        next_watch = Some(Instant::now() + interval);
                    } else if Instant::now() >= next_watch.unwrap_or_else(Instant::now) {
                        let started = Instant::now();
                        read_changeset(&*backend, &scope, generation, &mut digest, &shared);
                        // An agent's reply from the CLI shows without a refresh.
                        send_review_state(&*backend, &shared);
                        // A slow read (a remote worktree, a loaded machine)
                        // backs the watch off instead of filling the interval.
                        next_watch = Some(Instant::now() + interval.max(started.elapsed() * WATCH_BACKOFF));
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Read `scope`, and publish it if it differs from the last one published.
fn read_changeset(
    backend: &dyn DiffBackend,
    scope: &CommitScope,
    generation: u64,
    digest: &mut Option<u64>,
    shared: &Shared,
) {
    let raw = match backend.changeset(scope) {
        Ok(raw) => raw,
        Err(e) => {
            shared.send(Event::Loaded { generation, published: 0, loaded: Loaded::Failed(e) });
            return;
        }
    };
    let new_digest = raw.digest();
    if *digest == Some(new_digest) {
        return;
    }
    *digest = Some(new_digest);
    let changeset = Arc::new(raw.parse());
    let published = shared.published.fetch_add(1, Ordering::SeqCst) + 1;
    shared.send(Event::Loaded { generation, published, loaded: Loaded::Changeset(changeset.clone()) });
    start_highlighter(changeset, published, shared.clone());
}

/// Highlight `changeset`'s files, the one in view next each time.
fn start_highlighter(changeset: Arc<Changeset>, published: u64, shared: Shared) {
    let spawned = thread::Builder::new().name("diff-highlight".into()).spawn(move || {
        let total = changeset.files.len();
        let mut done = vec![false; total];
        for _ in 0..total {
            if shared.stopped() || shared.published.load(Ordering::SeqCst) != published {
                return;
            }
            let wanted = shared.priority.load(Ordering::Relaxed);
            let next = (wanted..total)
                .chain(0..wanted.min(total))
                .find(|f| !done[*f]);
            let Some(file) = next else { return };
            done[file] = true;
            if let Some(highlights) = highlight_file(&changeset.files[file]) {
                shared.send(Event::Highlights {
                    published,
                    file: file as u32,
                    highlights: Arc::new(highlights),
                });
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("couldn't start the diff highlighter: {e}");
    }
}
