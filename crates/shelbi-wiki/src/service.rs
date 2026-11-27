//! The hub's wiki service for one project: the single writer of the wiki's
//! files and the clock behind its sync.
//!
//! Shelbi's own code. One [`WikiService`] runs per project in the hub
//! daemon:
//!
//! - **Writes** ([`WikiService::write`], [`WikiService::update`], create,
//!   move, copy, remove) all come through here, from the CLI, agents and both
//!   clients, so one writer sequences them; each is announced at once
//!   ([`EventSink::changed`]).
//! - **Watch**: files changed in the worktree by anything else (an editor
//!   pointed at the folder) are noticed by a filesystem watcher, coalesced,
//!   and announced the same way.
//! - **Commit** [`Timing::commit_delay`] after the last change, then pull
//!   and push right away; pull on its own every [`Timing::pull_active`] while
//!   a client shows the Wiki ([`WikiService::touch_active`]), every
//!   [`Timing::pull_idle`] otherwise. Pulled changes are announced with
//!   [`ChangeSource::Pull`].
//! - **Status** ([`SyncStatus`]) is announced whenever it changes.
//!
//! Std threads only, matching the daemon.

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sync::pr::{PullRequest, PullRequests};
use crate::sync::{Engine, Layout, Pushed, Setup, SyncError};

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeSource {
    /// A person: a client's save, the CLI, or an edit straight to the files.
    Local,
    /// An agent (a write from a Shelbi-managed pane).
    Agent,
    /// A commit pulled from the remote.
    Pull,
}

impl ChangeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeSource::Local => "local",
            ChangeSource::Agent => "agent",
            ChangeSource::Pull => "pull",
        }
    }
}

/// Where the wiki's sync stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncStatus {
    /// Everything is committed and on the remote.
    Synced,
    /// A sync is running.
    Syncing,
    /// Changes wait to be committed or pushed.
    Pending,
    /// The remote couldn't be reached; it retries on the next pull.
    Offline,
    /// Some files couldn't be merged and are held.
    Conflict,
    /// The last sync failed.
    Error(String),
}

impl std::fmt::Display for SyncStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncStatus::Synced => f.write_str("synced"),
            SyncStatus::Syncing => f.write_str("syncing"),
            SyncStatus::Pending => f.write_str("pending"),
            SyncStatus::Offline => f.write_str("offline"),
            SyncStatus::Conflict => f.write_str("conflict"),
            SyncStatus::Error(detail) => write!(f, "error {detail}"),
        }
    }
}

/// Where the service announces changes and status.
pub trait EventSink: Send + Sync {
    /// `paths` (wiki-relative) changed.
    fn changed(&self, paths: &[String], source: ChangeSource);
    /// The sync status is now `status`.
    fn status(&self, status: &SyncStatus);
}

/// The service's clocks.
#[derive(Debug, Clone)]
pub struct Timing {
    /// Commit this long after the last change.
    pub commit_delay: Duration,
    /// Pull this often while a client shows the Wiki.
    pub pull_active: Duration,
    /// Pull this often otherwise.
    pub pull_idle: Duration,
    /// How long one [`WikiService::touch_active`] counts as a client showing
    /// the Wiki.
    pub active_lease: Duration,
    /// How long filesystem events are gathered before they're read.
    pub coalesce: Duration,
    /// A full rescan this often, in case the watcher missed something.
    pub rescan: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            commit_delay: Duration::from_secs(5),
            pull_active: Duration::from_secs(30),
            pull_idle: Duration::from_secs(300),
            active_lease: Duration::from_secs(90),
            coalesce: Duration::from_millis(200),
            rescan: Duration::from_secs(15),
        }
    }
}

/// Everything a service needs to start.
pub struct ServiceSpec {
    pub layout: Layout,
    /// Titles a freshly created wiki's starter files.
    pub name: String,
    /// Commit, pull and push on the service's own clock (`sync: auto`);
    /// otherwise only on [`WikiService::sync_now`].
    pub auto: bool,
    /// Where held files are remembered.
    pub conflicts_file: PathBuf,
    pub timing: Timing,
    /// Added to every git command's environment (tests isolate git config).
    pub git_env: Vec<(String, String)>,
    /// Folder mode's rolling PR.
    pub pull_requests: Option<Arc<dyn PullRequests>>,
}

/// A snapshot of the sync for `shelbi wiki sync status` and the clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub status: SyncStatus,
    /// Held files (wiki-relative).
    pub conflicts: Vec<String>,
    pub paused: bool,
    pub auto: bool,
    pub branch: String,
    pub root: PathBuf,
    /// When the last sync finished cleanly, RFC3339.
    pub last_synced: Option<String>,
    /// Folder mode: the rolling PR, once one is open.
    pub pull_request: Option<PullRequest>,
}

/// Why a wiki operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("already exists: {0}")]
    Exists(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Failed(String),
}

impl From<SyncError> for OpError {
    fn from(e: SyncError) -> Self {
        OpError::Failed(e.to_string())
    }
}

/// One project's wiki service. Dropping it stops its thread (committing
/// what's pending first, without pushing) and its watcher.
pub struct WikiService {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

struct Shared {
    root: PathBuf,
    layout: Layout,
    auto: bool,
    timing: Timing,
    prs: Option<Arc<dyn PullRequests>>,
    sink: Arc<dyn EventSink>,
    engine: Mutex<Engine>,
    sched: Mutex<Sched>,
    wake: Condvar,
}

struct Sched {
    stop: bool,
    /// When the oldest uncommitted change was made.
    dirty_at: Option<Instant>,
    /// When the watcher last fired, while its events wait to be read.
    watched_at: Option<Instant>,
    last_rescan: Instant,
    sync_requested: bool,
    paused: bool,
    last_pull: Option<Instant>,
    active_until: Option<Instant>,
    status: SyncStatus,
    /// Content hashes of every file under the root, as last announced.
    snapshot: HashMap<String, u64>,
    last_synced: Option<String>,
    pull_request: Option<PullRequest>,
    /// Counts finished syncs, so [`WikiService::sync_now`] can wait for one.
    cycles: u64,
    /// A sync is running (it started before any request now waiting).
    running: bool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl WikiService {
    /// Set the wiki up (see [`Engine::setup`]) and start its thread and
    /// watcher.
    pub fn start(spec: ServiceSpec, sink: Arc<dyn EventSink>) -> Result<(Self, Setup), SyncError> {
        let (engine, setup) = Engine::setup(spec.layout.clone(), &spec.name, &spec.conflicts_file, &spec.git_env)?;
        let root = spec.layout.wiki_root();
        let has_conflicts = !engine.conflicts().is_empty();
        let shared = Arc::new(Shared {
            root: root.clone(),
            layout: spec.layout,
            auto: spec.auto,
            timing: spec.timing,
            prs: spec.pull_requests,
            sink,
            engine: Mutex::new(engine),
            sched: Mutex::new(Sched {
                stop: false,
                dirty_at: None,
                watched_at: None,
                last_rescan: Instant::now(),
                sync_requested: setup == Setup::Created,
                paused: false,
                last_pull: None,
                active_until: None,
                status: if has_conflicts { SyncStatus::Conflict } else { SyncStatus::Synced },
                snapshot: scan(&root),
                last_synced: None,
                pull_request: None,
                cycles: 0,
                running: false,
            }),
            wake: Condvar::new(),
        });
        let watcher = watch(&root, Arc::downgrade(&shared));
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("shelbi-wiki".into())
            .spawn(move || run(&thread_shared))
            .map_err(|e| SyncError::Io(format!("starting the wiki's thread: {e}")))?;
        Ok((
            Self {
                shared,
                thread: Some(thread),
                _watcher: watcher,
            },
            setup,
        ))
    }

    /// The wiki's root folder.
    pub fn root(&self) -> &Path {
        &self.shared.root
    }

    pub fn status(&self) -> StatusReport {
        let conflicts = lock(&self.shared.engine).conflicts().iter().map(|c| c.path.clone()).collect();
        let s = lock(&self.shared.sched);
        StatusReport {
            status: s.status.clone(),
            conflicts,
            paused: s.paused,
            auto: self.shared.auto,
            branch: self.shared.layout.branch.clone(),
            root: self.shared.root.clone(),
            last_synced: s.last_synced.clone(),
            pull_request: s.pull_request.clone(),
        }
    }

    /// A held file's three sides.
    pub fn conflict(&self, path: &str) -> Option<crate::sync::conflicts::Conflict> {
        lock(&self.shared.engine).conflicts().iter().find(|c| c.path == path).cloned()
    }

    /// A client shows the Wiki: pull on the faster clock for a while.
    pub fn touch_active(&self) {
        let mut s = lock(&self.shared.sched);
        let now = Instant::now();
        let was_idle = s.active_until.is_none_or(|t| t <= now);
        s.active_until = Some(now + self.shared.timing.active_lease);
        if was_idle {
            // The Wiki just opened: don't make it wait out the idle clock.
            if s.last_pull.is_some_and(|t| now >= t + self.shared.timing.pull_active) {
                s.sync_requested = true;
            }
        }
        self.shared.wake.notify_all();
    }

    /// Sync now (commit, pull, push), even when paused or manual, and wait
    /// up to `timeout` for it to finish.
    pub fn sync_now(&self, timeout: Duration) -> StatusReport {
        {
            let mut s = lock(&self.shared.sched);
            // A sync already running may have read the files before the
            // request: wait for the one after it.
            let target = s.cycles + 1 + u64::from(s.running);
            s.sync_requested = true;
            self.shared.wake.notify_all();
            let deadline = Instant::now() + timeout;
            while s.cycles < target && !s.stop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                s = self.shared.wake.wait_timeout(s, deadline - now).unwrap_or_else(|p| p.into_inner()).0;
            }
        }
        self.status()
    }

    /// Stop committing and pulling on the clock until [`Self::resume`].
    pub fn pause(&self) {
        lock(&self.shared.sched).paused = true;
        self.shared.wake.notify_all();
    }

    pub fn resume(&self) {
        let mut s = lock(&self.shared.sched);
        s.paused = false;
        s.sync_requested = true;
        self.shared.wake.notify_all();
    }

    /// Resolve a held file with `content` (`None` deletes it), commit it,
    /// and sync.
    pub fn resolve(&self, path: &str, content: Option<&str>) -> Result<(), OpError> {
        let changed = {
            let mut engine = lock(&self.shared.engine);
            engine.resolve(path, content).map_err(|e| match e {
                SyncError::Setup(m) => OpError::Invalid(m),
                other => OpError::from(other),
            })?;
            self.shared.rescan_locked()
        };
        self.shared.announce(changed, ChangeSource::Local);
        let mut s = lock(&self.shared.sched);
        s.sync_requested = true;
        self.shared.wake.notify_all();
        Ok(())
    }

    /// The absolute path of wiki-relative `path`, refusing anything outside
    /// the wiki or inside git's own files.
    pub fn resolve_path(&self, path: &str) -> Result<PathBuf, OpError> {
        resolve(&self.shared.root, path)
    }

    /// Read a document.
    pub fn read(&self, path: &str) -> Result<String, OpError> {
        let abs = self.resolve_path(path)?;
        read_existing(&abs, path)
    }

    /// Replace (or create) a document with `content`.
    pub fn write(&self, path: &str, content: &str, source: ChangeSource) -> Result<(), OpError> {
        let abs = self.resolve_path(path)?;
        if abs.is_dir() {
            return Err(OpError::Invalid(format!("`{path}` is a folder")));
        }
        self.mutate(source, || write_file(&abs, content))
    }

    /// Rewrite an existing document: `f` gets its content and answers the
    /// new content. Read and write happen under the writer's lock, so two
    /// section edits to one document never lose each other.
    pub fn update(
        &self,
        path: &str,
        source: ChangeSource,
        f: impl FnOnce(&str) -> Result<String, OpError>,
    ) -> Result<(), OpError> {
        let abs = self.resolve_path(path)?;
        self.mutate(source, || {
            let current = read_existing(&abs, path)?;
            let next = f(&current)?;
            if next != current {
                write_file(&abs, &next)?;
            }
            Ok(())
        })
    }

    /// Create a document that doesn't exist yet.
    pub fn create_document(&self, path: &str, content: &str, source: ChangeSource) -> Result<(), OpError> {
        let abs = self.resolve_path(path)?;
        self.mutate(source, || {
            if abs.exists() {
                return Err(OpError::Exists(path.to_string()));
            }
            write_file(&abs, content)
        })
    }

    /// Create a folder (and its parents).
    pub fn create_folder(&self, path: &str, source: ChangeSource) -> Result<(), OpError> {
        let abs = self.resolve_path(path)?;
        self.mutate(source, || {
            if abs.is_file() {
                return Err(OpError::Exists(path.to_string()));
            }
            std::fs::create_dir_all(&abs).map_err(|e| OpError::Failed(format!("creating `{path}`: {e}")))
        })
    }

    /// Move or rename a document or folder. A destination that is an
    /// existing folder receives the source inside it.
    pub fn rename(&self, from: &str, to: &str, source: ChangeSource) -> Result<(), OpError> {
        let src = self.resolve_path(from)?;
        let dst = self.destination(&src, to)?;
        self.mutate(source, || {
            if !src.exists() {
                return Err(OpError::NotFound(from.to_string()));
            }
            if dst.exists() {
                return Err(OpError::Exists(to.to_string()));
            }
            if dst.starts_with(&src) {
                return Err(OpError::Invalid(format!("can't move `{from}` into itself")));
            }
            if let Some(dir) = dst.parent() {
                std::fs::create_dir_all(dir).map_err(|e| OpError::Failed(e.to_string()))?;
            }
            std::fs::rename(&src, &dst).map_err(|e| OpError::Failed(format!("moving `{from}`: {e}")))
        })
    }

    /// Copy a document or folder.
    pub fn copy(&self, from: &str, to: &str, source: ChangeSource) -> Result<(), OpError> {
        let src = self.resolve_path(from)?;
        let dst = self.destination(&src, to)?;
        self.mutate(source, || {
            if !src.exists() {
                return Err(OpError::NotFound(from.to_string()));
            }
            if dst.exists() {
                return Err(OpError::Exists(to.to_string()));
            }
            if dst.starts_with(&src) {
                return Err(OpError::Invalid(format!("can't copy `{from}` into itself")));
            }
            copy_tree(&src, &dst).map_err(|e| OpError::Failed(format!("copying `{from}`: {e}")))
        })
    }

    /// Remove a document or folder. Nothing goes to the platform trash: the
    /// wiki's history keeps every committed version.
    pub fn remove(&self, path: &str, source: ChangeSource) -> Result<(), OpError> {
        let abs = self.resolve_path(path)?;
        if abs == self.shared.root {
            return Err(OpError::Invalid("can't remove the wiki itself".into()));
        }
        self.mutate(source, || {
            let result = if abs.is_dir() {
                std::fs::remove_dir_all(&abs)
            } else {
                std::fs::remove_file(&abs)
            };
            match result {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(OpError::NotFound(path.to_string())),
                Err(e) => Err(OpError::Failed(format!("removing `{path}`: {e}"))),
            }
        })
    }

    fn destination(&self, src: &Path, to: &str) -> Result<PathBuf, OpError> {
        let dst = self.resolve_path(to)?;
        if dst.is_dir() {
            if let Some(name) = src.file_name() {
                return Ok(dst.join(name));
            }
        }
        Ok(dst)
    }

    /// Run one change to the files under the writer's lock, then announce
    /// whatever it changed and arm the commit.
    fn mutate(&self, source: ChangeSource, op: impl FnOnce() -> Result<(), OpError>) -> Result<(), OpError> {
        let changed = {
            let _engine = lock(&self.shared.engine);
            op()?;
            self.shared.rescan_locked()
        };
        self.shared.announce(changed, source);
        Ok(())
    }
}

impl Drop for WikiService {
    fn drop(&mut self) {
        {
            let mut s = lock(&self.shared.sched);
            s.stop = true;
            self.shared.wake.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Shared {
    /// Rescan the root and answer the paths that differ from the snapshot,
    /// updating it. The caller holds the engine lock, so no write or merge
    /// is half done.
    fn rescan_locked(&self) -> Vec<String> {
        let fresh = scan(&self.root);
        let mut s = lock(&self.sched);
        s.last_rescan = Instant::now();
        let mut changed: BTreeSet<String> = BTreeSet::new();
        for (path, hash) in &fresh {
            if s.snapshot.get(path) != Some(hash) {
                changed.insert(path.clone());
            }
        }
        for path in s.snapshot.keys() {
            if !fresh.contains_key(path) {
                changed.insert(path.clone());
            }
        }
        s.snapshot = fresh;
        changed.into_iter().collect()
    }

    /// Announce `changed` and, for a change made here, arm the commit.
    fn announce(&self, changed: Vec<String>, source: ChangeSource) {
        if changed.is_empty() {
            return;
        }
        if source != ChangeSource::Pull {
            let mut s = lock(&self.sched);
            if s.dirty_at.is_none() {
                s.dirty_at = Some(Instant::now());
            }
            if s.status == SyncStatus::Synced {
                s.status = SyncStatus::Pending;
                self.sink.status(&s.status);
            }
            self.wake.notify_all();
        }
        self.sink.changed(&changed, source);
    }

    fn set_status(&self, status: SyncStatus) {
        let mut s = lock(&self.sched);
        if s.status != status {
            s.status = status.clone();
            drop(s);
            self.sink.status(&status);
        }
    }

    /// One sync: commit what's pending, fetch, merge, push (and, in folder
    /// mode, keep the rolling PR open).
    fn cycle(&self, dirty_mark: Option<Instant>) {
        self.set_status(SyncStatus::Syncing);
        let result = self.cycle_steps();
        let status = match &result {
            Ok(()) => match self.settled() {
                Ok(s) => s,
                Err(e) => SyncStatus::Error(one_line(&e.to_string())),
            },
            Err(e) if e.is_offline() => SyncStatus::Offline,
            Err(e) => SyncStatus::Error(one_line(&e.to_string())),
        };
        let changed = {
            let mut s = lock(&self.sched);
            s.last_pull = Some(Instant::now());
            if s.dirty_at == dirty_mark {
                s.dirty_at = None;
            }
            if result.is_ok() {
                s.last_synced = Some(crate::fs::paths::iso8601_now());
            }
            s.cycles += 1;
            s.running = false;
            let changed = s.status != status;
            s.status = status.clone();
            self.wake.notify_all();
            changed
        };
        if changed {
            self.sink.status(&status);
        }
    }

    fn cycle_steps(&self) -> Result<(), SyncError> {
        let remote = {
            let mut engine = lock(&self.engine);
            engine.commit_pending()?;
            engine.remote()
        };
        let mut pushed = Pushed::Nothing;
        for attempt in 0..2 {
            let fetched = remote.fetch()?;
            let (integrated, changed) = {
                let mut engine = lock(&self.engine);
                let integrated = engine.integrate(fetched)?;
                (integrated, self.rescan_locked())
            };
            let _ = integrated;
            self.announce(changed, ChangeSource::Pull);
            match remote.push() {
                Ok(p) => {
                    pushed = p;
                    break;
                }
                // Someone pushed between our fetch and push: pull once more.
                Err(SyncError::Git(e)) if attempt == 0 && e.is_rejected_push() => continue,
                Err(e) => return Err(e),
            }
        }
        if let Some(prs) = &self.prs {
            if let Some(folder) = &self.layout.folder {
                let wants_pr = pushed == Pushed::Pushed || lock(&self.sched).pull_request.is_none();
                if wants_pr && remote.differs_from_base()? {
                    let pr = prs
                        .ensure_open(&self.layout.worktree, &self.layout.branch, &folder.base)
                        .map_err(|e| SyncError::Io(format!("couldn't open the wiki's pull request: {e}")))?;
                    lock(&self.sched).pull_request = Some(pr);
                }
            }
        }
        Ok(())
    }

    /// The status after a clean sync.
    fn settled(&self) -> Result<SyncStatus, SyncError> {
        let engine = lock(&self.engine);
        if !engine.conflicts().is_empty() {
            return Ok(SyncStatus::Conflict);
        }
        Ok(if engine.has_pending()? { SyncStatus::Pending } else { SyncStatus::Synced })
    }
}

/// The service's thread: read filesystem events, then commit and pull when
/// their clocks come due.
fn run(shared: &Shared) {
    loop {
        let mut s = lock(&shared.sched);
        if s.stop {
            break;
        }
        let now = Instant::now();
        let t = &shared.timing;
        let watch_due = s.watched_at.map(|at| at + t.coalesce);
        let rescan_due = s.last_rescan + t.rescan;
        if watch_due.is_some_and(|d| now >= d) || now >= rescan_due {
            s.watched_at = None;
            drop(s);
            let changed = {
                let _engine = lock(&shared.engine);
                shared.rescan_locked()
            };
            shared.announce(changed, ChangeSource::Local);
            continue;
        }
        let clocked = shared.auto && !s.paused;
        let commit_due = s.dirty_at.filter(|_| clocked).map(|at| at + t.commit_delay);
        let interval = if s.active_until.is_some_and(|until| until > now) {
            t.pull_active
        } else {
            t.pull_idle
        };
        let pull_due = if clocked {
            Some(s.last_pull.map_or(now, |at| at + interval))
        } else {
            None
        };
        if s.sync_requested || commit_due.is_some_and(|d| now >= d) || pull_due.is_some_and(|d| now >= d) {
            s.sync_requested = false;
            s.running = true;
            let mark = s.dirty_at;
            drop(s);
            shared.cycle(mark);
            continue;
        }
        let next = [watch_due, Some(rescan_due), commit_due, pull_due]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or(now + Duration::from_secs(1));
        let wait = next.saturating_duration_since(now).max(Duration::from_millis(5));
        let _ = shared.wake.wait_timeout(s, wait);
    }
    // Stopping: commit what's pending so nothing waits on a hub that's gone.
    // The push is the next start's.
    if shared.auto {
        let _ = lock(&shared.engine).commit_pending();
    }
}

fn watch(root: &Path, shared: Weak<Shared>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let root_for_filter = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            return;
        };
        if !event.paths.iter().any(|p| is_watched(&root_for_filter, p)) {
            return;
        }
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let mut s = lock(&shared.sched);
        if s.watched_at.is_none() {
            s.watched_at = Some(Instant::now());
        }
        shared.wake.notify_all();
    })
    .ok()?;
    watcher.watch(root, notify::RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

/// Whether a changed path is one of the wiki's own files (not git's, not a
/// write's temporary file).
fn is_watched(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    rel_is_watched(rel)
}

fn rel_is_watched(rel: &Path) -> bool {
    !rel.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        name == ".git" || name == ".DS_Store" || name.starts_with(".tmp")
    }) && rel != Path::new(".contextstore/settings.local.yml")
}

/// Content hashes of every watched file under `root`, by wiki-relative path.
fn scan(root: &Path) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            if !rel_is_watched(rel) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut h);
                out.insert(rel.to_string_lossy().replace('\\', "/"), h.finish());
            }
        }
    }
    out
}

fn resolve(root: &Path, path: &str) -> Result<PathBuf, OpError> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        return Ok(root.to_path_buf());
    }
    let abs = crate::fs::paths::resolve_in_space(root, trimmed).map_err(|e| OpError::Invalid(e.to_string()))?;
    if Path::new(trimmed).components().any(|c| c.as_os_str() == ".git") {
        return Err(OpError::Invalid(format!("`{path}` is inside git's own files")));
    }
    Ok(abs)
}

fn read_existing(abs: &Path, path: &str) -> Result<String, OpError> {
    match std::fs::read_to_string(abs) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(OpError::NotFound(path.to_string())),
        Err(_) if abs.is_dir() => Err(OpError::Invalid(format!("`{path}` is a folder"))),
        Err(e) => Err(OpError::Failed(format!("reading `{path}`: {e}"))),
    }
}

fn write_file(abs: &Path, content: &str) -> Result<(), OpError> {
    if let Some(dir) = abs.parent() {
        std::fs::create_dir_all(dir).map_err(|e| OpError::Failed(e.to_string()))?;
    }
    crate::fs::paths::write_atomic(abs, content.as_bytes()).map_err(|e| OpError::Failed(e.to_string()))
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(dir) = dst.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_files_and_temp_files_are_not_watched() {
        assert!(rel_is_watched(Path::new("Plans/a.md")));
        assert!(rel_is_watched(Path::new(".contextstore/settings.yml")));
        assert!(!rel_is_watched(Path::new(".git")));
        assert!(!rel_is_watched(Path::new("Plans/.tmpAbC12")));
        assert!(!rel_is_watched(Path::new(".contextstore/settings.local.yml")));
    }

    #[test]
    fn paths_stay_inside_the_wiki() {
        let root = Path::new("/w");
        assert_eq!(resolve(root, "Plans/a.md").unwrap(), PathBuf::from("/w/Plans/a.md"));
        assert!(matches!(resolve(root, "../x"), Err(OpError::Invalid(_))));
        assert!(matches!(resolve(root, "/etc/passwd"), Err(OpError::Invalid(_))));
        assert!(matches!(resolve(root, ".git/config"), Err(OpError::Invalid(_))));
    }

    #[test]
    fn status_reads_as_its_wire_word() {
        assert_eq!(SyncStatus::Synced.to_string(), "synced");
        assert_eq!(SyncStatus::Error("push failed".into()).to_string(), "error push failed");
    }
}
