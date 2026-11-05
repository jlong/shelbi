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
//!   remote read never delays a save, and looks at the task's comments every
//!   [`REVIEW_STATE_WATCH`], so an agent's reply from the CLI shows by itself.
//! - A **job** runs what can take a while and has to be answered: a message to
//!   the agent (which waits to see it submitted), a revert's check and write.
//! - The **PR reader** reads the task's pull request on open, on `r`, after a
//!   change to it, and every [`PR_WATCH`] while anyone's looking, and carries out
//!   those changes in order (a comment may have to start the review the next one
//!   joins). The view never waits on it: what it can't read, the block says.
//!
//! Dropping the session ends them all; none is joined, so dropping never
//! blocks on a git command still running (each has a deadline).

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use shelbi_core::IssuePr;
use shelbi_state::keymap::{DiffAction, KeyChord, ModeKeymap};

use super::backend::{backend_for, AgentDelivery, AgentSend, DiffBackend, DiffTarget, TargetKind};
use super::comments::{Author, CommentDoc, CommentOp};
use super::git::BlameLine;
use super::highlight::{highlight_file, FileHighlights};
use super::model::{Changeset, CommitScope};
use super::pr::{PrDetails, PrWrite};
use super::revert::{RevertPlan, RevertSpec};
use super::state::{DiffEffect, DiffState};
use super::store::{ViewedDoc, ViewedOp};
use crate::source::ProjectLink;

/// How a live workspace diff re-reads: often enough to follow an agent, slowly
/// enough not to matter.
pub const WORKSPACE_WATCH: Duration = Duration::from_secs(2);

/// How often an open view looks at the task's comments and viewed marks, for
/// what an agent or another client wrote.
pub const REVIEW_STATE_WATCH: Duration = Duration::from_secs(2);

/// How often an open view reads the pull request again, for CI and reviewers.
/// One cheap GraphQL read, spent only while someone's looking.
pub const PR_WATCH: Duration = Duration::from_secs(60);

/// A watch waits at least this many times as long as its last read took.
const WATCH_BACKOFF: u32 = 4;

/// Tells a client's event loop there's something to [`DiffSession::poll`].
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a session behaves.
#[derive(Clone)]
pub struct DiffOptions {
    /// Signs new comments.
    pub author: Author,
    /// What it's a view of, which decides where `gY` goes, whether anything can
    /// revert, and the reply command a message gives an agent.
    pub target: DiffTarget,
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
    AgentSent { ids: Vec<String>, result: Result<AgentDelivery, String> },
    RevertPlanned(Result<RevertPlan, String>),
    Reverted(Result<String, String>),
    PrLookup(Result<Option<IssuePr>, String>),
    PrDetails(Result<Arc<PrDetails>, String>),
    PrWritten { change: PrWrite, result: Result<String, String> },
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
    Job(Job),
}

enum PrCommand {
    /// Read it now.
    Refresh,
    /// Someone's looking again: read it if it's been a while.
    Resume,
    Write(PrWrite),
}

/// Work that waits on an agent or on git, with an answer to report. Each gets a
/// thread: a slow send mustn't hold up a comment being saved.
enum Job {
    Send { message: String, ids: Vec<String> },
    PlanRevert(RevertSpec),
    Revert(RevertPlan),
}

/// An open diff view.
pub struct DiffSession {
    state: DiffState,
    events: Receiver<Event>,
    load: Sender<LoadCommand>,
    store: Sender<StoreCommand>,
    pr: Sender<PrCommand>,
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
    /// Effects a worker's answer produced, for [`Self::take_effects`].
    from_workers: Vec<DiffEffect>,
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
        let (pr, pr_rx) = mpsc::channel();
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
            state: DiffState::new(options.author).for_target(&options.target),
            events,
            load: load.clone(),
            store,
            pr,
            generation: 0,
            published: 0,
            priority,
            scope: CommitScope::All,
            store_error: None,
            unsaved: VecDeque::new(),
            blame_asked: HashSet::new(),
            from_workers: Vec::new(),
            paused,
            stop,
        };
        let mut session = session;
        let spawned = thread::Builder::new().name("diff-loader".into()).spawn(move || match open_backend() {
            Ok(backend) => run_workers(backend, load_rx, store_rx, pr_rx, shared, watch),
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
    pub fn for_target(
        target: DiffTarget,
        link: Option<ProjectLink>,
        author: Author,
        agent: AgentSend,
        wake: Wake,
    ) -> DiffSession {
        let options = DiffOptions {
            author,
            target: target.clone(),
            watch: (target.kind == TargetKind::Workspace).then_some(WORKSPACE_WATCH),
            wake,
        };
        DiffSession::open(move || backend_for(&target, link.as_ref(), agent), options)
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
                DiffEffect::Refresh => {
                    self.request_load(self.scope.clone());
                    let _ = self.pr.send(PrCommand::Refresh);
                }
                DiffEffect::PrWrite(change) => {
                    let _ = self.pr.send(PrCommand::Write(change));
                }
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
                DiffEffect::SendToAgent { message, ids } => {
                    let _ = self.store.send(StoreCommand::Job(Job::Send { message, ids }));
                }
                DiffEffect::PlanRevert(spec) => {
                    let _ = self.store.send(StoreCommand::Job(Job::PlanRevert(spec)));
                }
                DiffEffect::Revert(plan) => {
                    let _ = self.store.send(StoreCommand::Job(Job::Revert(plan)));
                }
                other @ (DiffEffect::Yank(_)
                | DiffEffect::OpenInEditor { .. }
                | DiffEffect::RejectWithComments { .. }) => for_client.push(other),
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
            let _ = self.pr.send(PrCommand::Resume);
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
        let mut for_client = Vec::new();
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
                Event::AgentSent { ids, result } => {
                    let effects = self.state.agent_sent(ids, result);
                    self.carry_out(effects);
                }
                Event::RevertPlanned(result) => self.state.revert_planned(result),
                Event::Reverted(result) => {
                    // A write that failed changed nothing, but the read it would
                    // have been is as good a look as any.
                    self.state.reverted(result);
                    self.refresh();
                }
                Event::PrLookup(lookup) => self.state.set_pr_lookup(lookup),
                Event::PrDetails(details) => self.state.set_pr_details(details),
                Event::PrWritten { change, result } => {
                    let effects = self.state.pr_written(change, result);
                    for effect in self.carry_out(effects) {
                        for_client.push(effect);
                    }
                }
            }
        }
        self.from_workers.extend(for_client);
        changed
    }

    /// What a worker's answer left for the client to carry out, like the words
    /// of a post GitHub refused, on their way to the clipboard.
    pub fn take_effects(&mut self) -> Vec<DiffEffect> {
        std::mem::take(&mut self.from_workers)
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
    pr: Receiver<PrCommand>,
    shared: Shared,
    watch: Option<Duration>,
) {
    let (store_backend, store_shared) = (backend.clone(), shared.clone());
    thread::Builder::new()
        .name("diff-store".into())
        .spawn(move || run_store(store_backend, store, store_shared))
        .expect("spawning the diff store");
    let (pr_backend, pr_shared) = (backend.clone(), shared.clone());
    let spawned = thread::Builder::new()
        .name("diff-pr".into())
        .spawn(move || run_pr(pr_backend, pr, pr_shared));
    // The diff doesn't need it, so losing it costs only the block.
    if let Err(e) = spawned {
        tracing::warn!("couldn't start the diff's PR reader: {e}");
    }
    run_loader(backend, load, shared, watch);
}

fn run_store(backend: Arc<dyn DiffBackend>, commands: Receiver<StoreCommand>, shared: Shared) {
    let mut seen = SeenState::default();
    seen.send_changed(&*backend, &shared);
    loop {
        if shared.stopped() {
            return;
        }
        match commands.recv_timeout(REVIEW_STATE_WATCH) {
            Ok(StoreCommand::Comment(op)) => {
                let saved = backend.apply_comment(&op);
                seen.comments = saved.as_ref().ok().cloned();
                shared.send(Event::CommentSaved(saved));
            }
            Ok(StoreCommand::Viewed(op)) => {
                let event = match backend.apply_viewed(&op) {
                    Ok(doc) => {
                        seen.viewed = Some(doc.clone());
                        Event::Viewed(doc)
                    }
                    Err(e) => Event::StoreFailed(e),
                };
                shared.send(event);
            }
            Ok(StoreCommand::Job(job)) => start_job(job, backend.clone(), shared.clone()),
            Err(RecvTimeoutError::Timeout) => {
                if !shared.paused.load(Ordering::Relaxed) {
                    seen.send_changed(&*backend, &shared);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// What the store last told the session. A look that finds the same says
/// nothing, so an idle view isn't redrawn every couple of seconds.
#[derive(Default)]
struct SeenState {
    comments: Option<CommentDoc>,
    viewed: Option<ViewedDoc>,
}

impl SeenState {
    /// Read the task's comments and viewed marks. Reads are on the same thread
    /// as saves, so one can't answer with a document older than a save it missed.
    fn send_changed(&mut self, backend: &dyn DiffBackend, shared: &Shared) {
        match backend.comments() {
            Ok(doc) if self.comments.as_ref() == Some(&doc) => {}
            Ok(doc) => {
                self.comments = Some(doc.clone());
                shared.send(Event::Comments(doc));
            }
            Err(e) => shared.send(Event::StoreFailed(e)),
        }
        match backend.viewed() {
            Ok(doc) if self.viewed.as_ref() == Some(&doc) => {}
            Ok(doc) => {
                self.viewed = Some(doc.clone());
                shared.send(Event::Viewed(doc));
            }
            Err(e) => shared.send(Event::StoreFailed(e)),
        }
    }
}

fn run_pr(backend: Arc<dyn DiffBackend>, commands: Receiver<PrCommand>, shared: Shared) {
    let mut seen = SeenPr::default();
    seen.read(&*backend, &shared);
    loop {
        if shared.stopped() {
            return;
        }
        match commands.recv_timeout(PR_WATCH) {
            Ok(PrCommand::Refresh) => seen.read(&*backend, &shared),
            Ok(PrCommand::Resume) => {
                if seen.read_at.is_none_or(|at| at.elapsed() >= PR_WATCH) {
                    seen.read(&*backend, &shared);
                }
            }
            Ok(PrCommand::Write(change)) => {
                let result = match &seen.record {
                    Some(pr) => backend.pr_write(pr, &change),
                    None => Err("This task has no pull request".to_string()),
                };
                let wrote = result.is_ok();
                shared.send(Event::PrWritten { change, result });
                if wrote {
                    seen.read(&*backend, &shared);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !shared.paused.load(Ordering::Relaxed) {
                    seen.read(&*backend, &shared);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// What the PR reader last told the session, so an idle view isn't redrawn by
/// a read that found the same.
#[derive(Default)]
struct SeenPr {
    record: Option<IssuePr>,
    lookup_failed: Option<String>,
    details: Option<Result<PrDetails, String>>,
    read_at: Option<Instant>,
}

impl SeenPr {
    fn read(&mut self, backend: &dyn DiffBackend, shared: &Shared) {
        self.read_at = Some(Instant::now());
        match backend.pr_lookup() {
            Ok(None) => {
                if self.record.take().is_some() || self.lookup_failed.take().is_some() {
                    self.details = None;
                    shared.send(Event::PrLookup(Ok(None)));
                }
            }
            Err(why) => {
                if self.lookup_failed.as_ref() != Some(&why) {
                    self.lookup_failed = Some(why.clone());
                    shared.send(Event::PrLookup(Err(why)));
                }
            }
            Ok(Some(pr)) => {
                // A lookup that failed left its reason on show: this clears it.
                let recovered = self.lookup_failed.take().is_some();
                if self.record.as_ref() != Some(&pr) {
                    self.record = Some(pr.clone());
                    shared.send(Event::PrLookup(Ok(Some(pr.clone()))));
                }
                let details = backend.pr_details(&pr);
                if recovered || self.details.as_ref() != Some(&details) {
                    shared.send(Event::PrDetails(details.clone().map(Arc::new)));
                    self.details = Some(details);
                }
            }
        }
    }
}

fn start_job(job: Job, backend: Arc<dyn DiffBackend>, shared: Shared) {
    let reporter = shared.clone();
    let spawned = thread::Builder::new().name("diff-job".into()).spawn(move || {
        let event = match job {
            Job::Send { message, ids } => Event::AgentSent { ids, result: backend.send_to_agent(&message) },
            Job::PlanRevert(spec) => Event::RevertPlanned(backend.plan_revert(&spec)),
            Job::Revert(plan) => Event::Reverted(backend.revert(&plan)),
        };
        shared.send(event);
    });
    if let Err(e) = spawned {
        reporter.send(Event::StoreFailed(format!("Couldn't start the work: {e}")));
    }
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
