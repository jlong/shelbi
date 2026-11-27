//! The hub's wiki services, one per project, and the requests clients send
//! them ([`shelbi_app::wiki`], riding a control `AppRequest`).
//!
//! The daemon owns these: [`reconcile`] (called on the daemon's wiki tick)
//! starts a service for each open project that has a wiki and stops the
//! ones nobody has used for a while after their project closed; [`answer`]
//! (called from [`crate::far::answer`]) runs one request, starting the
//! project's service on demand. A project has a wiki when its YAML has a
//! `wiki:` block, or when its wiki worktree
//! (`~/.shelbi/projects/<name>/wiki/`) already exists; anything else gets
//! one only from [`WikiRequest::Init`].
//!
//! Every change and status the services report goes two places: a line on
//! `events.log` (`wiki_changed project=… paths=… source=…`,
//! `wiki_status project=… status=…`) and a push to subscribed clients on
//! the change bus ([`shelbi_state::ChangeNotification::Wiki`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use shelbi_app::wiki::{
    Author, EntryKind, FailureKind, NodeKind, SyncOp, SyncState, WikiCommit, WikiConflict, WikiEntry, WikiMatch,
    WikiNode, WikiReply, WikiRequest, WikiStatus, WikiTask,
};
use shelbi_core::{Project, WikiConfig, WikiSource, WikiSyncMode};
use shelbi_wiki::service::{ChangeSource, EventSink, OpError, ServiceSpec, StatusReport, SyncStatus, Timing, WikiService};
use shelbi_wiki::sync::Layout;

/// How long a wiki whose project closed keeps running after its last
/// request (an agent or the CLI may still be writing to it).
const IDLE_STOP: Duration = Duration::from_secs(10 * 60);

/// How long a failed start (a remote out of reach while creating the
/// worktree) waits before the next tick retries it.
const START_BACKOFF: Duration = Duration::from_secs(60);

/// How long `shelbi wiki sync now` waits for its sync.
const SYNC_NOW_WAIT: Duration = Duration::from_secs(180);

/// The longest `paths=` value an event line carries before it summarizes.
const MAX_PATHS_BYTES: usize = 2048;

/// One project's service, while it runs, and its start backoff.
#[derive(Default)]
struct Slot {
    running: Option<Running>,
    failed_at: Option<Instant>,
}

struct Running {
    service: Arc<WikiService>,
    config: WikiConfig,
    last_used: Instant,
}

type Registry = Mutex<HashMap<String, Arc<Mutex<Slot>>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn slot(project: &str) -> Arc<Mutex<Slot>> {
    lock(registry()).entry(project.to_string()).or_default().clone()
}

/// The worktree a project's wiki lives in.
pub fn wiki_dir(project: &str) -> Result<PathBuf, String> {
    shelbi_state::project_dir(project).map(|d| d.join("wiki")).map_err(|e| e.to_string())
}

fn conflicts_file(project: &str) -> Result<PathBuf, String> {
    shelbi_state::project_dir(project)
        .map(|d| d.join("wiki-conflicts.json"))
        .map_err(|e| e.to_string())
}

/// The wiki `project` has: its `wiki:` block, else the default when its
/// worktree is already there (or `create` asks for one), else none.
fn effective_config(project: &Project, create: bool) -> Result<Option<WikiConfig>, String> {
    if let Some(cfg) = &project.wiki {
        return Ok(Some(cfg.clone()));
    }
    if create || wiki_dir(&project.name)?.join(".git").exists() {
        return Ok(Some(WikiConfig::default()));
    }
    Ok(None)
}

/// The project's hub checkout: its first local machine's `work_dir`.
fn hub_checkout(project: &Project) -> Result<PathBuf, String> {
    project
        .machines
        .iter()
        .find(|m| matches!(m.kind, shelbi_core::MachineKind::Local))
        .map(|m| m.work_dir.clone())
        .ok_or_else(|| "the project has no local machine to keep its wiki on".to_string())
}

/// The service spec for `project`'s wiki as `config` describes it.
pub fn spec_for(project: &Project, config: &WikiConfig) -> Result<ServiceSpec, String> {
    config.validate().map_err(|e| e.to_string())?;
    let repo = hub_checkout(project)?;
    let worktree = wiki_dir(&project.name)?;
    let layout = match (config.source, config.folder()) {
        (WikiSource::Folder, Some(folder)) => {
            Layout::folder(repo, worktree, config.worktree_branch(), folder, project.base_branch())
        }
        _ => Layout::branch(repo, worktree, config.worktree_branch()),
    };
    let pull_requests: Option<Arc<dyn shelbi_wiki::sync::pr::PullRequests>> = match config.source {
        WikiSource::Folder => Some(Arc::new(shelbi_wiki::sync::pr::GhPullRequests)),
        WikiSource::Branch => None,
    };
    Ok(ServiceSpec {
        layout,
        name: project.display_label().to_string(),
        auto: config.sync == WikiSyncMode::Auto,
        conflicts_file: conflicts_file(&project.name)?,
        timing: Timing::default(),
        git_env: git_env(),
        pull_requests,
    })
}

/// Extra environment for the services' git: none in production (the
/// user's own config applies); a test's isolated git config otherwise.
#[cfg(not(test))]
fn git_env() -> Vec<(String, String)> {
    Vec::new()
}

#[cfg(test)]
fn git_env() -> Vec<(String, String)> {
    tests::GIT_ENV.with(|e| e.borrow().clone())
}

/// `project`'s running service, started if it has a wiki (or `create`
/// asks for one). `Ok(None)` when it has none.
fn service(project: &str, create: bool) -> Result<Option<Arc<WikiService>>, String> {
    let loaded = shelbi_state::load_project(project).map_err(|e| e.to_string())?;
    let config = effective_config(&loaded, create)?;
    let slot = slot(project);
    let mut slot = lock(&slot);
    let Some(config) = config else {
        slot.running = None;
        return Ok(None);
    };
    if let Some(running) = slot.running.as_mut() {
        if running.config == config {
            running.last_used = Instant::now();
            return Ok(Some(running.service.clone()));
        }
    }
    // The block changed (or nothing runs yet): start afresh.
    slot.running = None;
    let spec = spec_for(&loaded, &config)?;
    let sink = Arc::new(HubSink {
        project: project.to_string(),
    });
    match WikiService::start(spec, sink) {
        Ok((service, setup)) => {
            let service = Arc::new(service);
            tracing::info!(project, ?setup, root = %service.root().display(), "wiki: service started");
            slot.failed_at = None;
            slot.running = Some(Running {
                service: service.clone(),
                config,
                last_used: Instant::now(),
            });
            Ok(Some(service))
        }
        Err(e) => {
            slot.failed_at = Some(Instant::now());
            Err(format!("couldn't set up the wiki: {e}"))
        }
    }
}

/// One wiki tick: start a service for each open project with a wiki, and
/// stop those whose project closed and that have sat unused for a while.
pub fn reconcile(open: &[String]) {
    for project in open {
        let backing_off = lock(&slot(project))
            .failed_at
            .is_some_and(|at| at.elapsed() < START_BACKOFF);
        if backing_off {
            continue;
        }
        if let Err(e) = service(project, false) {
            tracing::warn!(project = %project, error = %e, "wiki: service didn't start");
        }
    }
    let slots: Vec<(String, Arc<Mutex<Slot>>)> =
        lock(registry()).iter().map(|(p, s)| (p.clone(), s.clone())).collect();
    for (project, slot) in slots {
        if open.contains(&project) {
            continue;
        }
        let mut slot = lock(&slot);
        if slot.running.as_ref().is_some_and(|r| r.last_used.elapsed() >= IDLE_STOP) {
            tracing::info!(project = %project, "wiki: service stopped (project closed)");
            slot.running = None;
        }
    }
}

/// Stop every service (the daemon is exiting). Each commits what's pending.
pub fn stop_all() {
    let slots: Vec<Arc<Mutex<Slot>>> = lock(registry()).drain().map(|(_, s)| s).collect();
    for slot in slots {
        lock(&slot).running = None;
    }
}

/// Run one wiki request for `project`. Blocking.
pub fn answer(project: &str, req: WikiRequest) -> WikiReply {
    let create = matches!(req, WikiRequest::Init);
    let svc = match service(project, create) {
        Ok(Some(svc)) => svc,
        Ok(None) if matches!(req, WikiRequest::Status) => return WikiReply::Status(WikiStatus::not_set_up()),
        Ok(None) => {
            return WikiReply::failed(
                FailureKind::NotSetUp,
                format!("project `{project}` has no wiki yet; `shelbi wiki init` sets one up"),
            )
        }
        Err(e) => return WikiReply::failed(FailureKind::Failed, e),
    };
    let source_kind = shelbi_state::load_project(project)
        .ok()
        .and_then(|p| p.wiki)
        .map(|w| w.source)
        .unwrap_or_default();
    answer_for(&svc, source_kind, Some(project), req)
}

/// Run `req` against a running service.
pub fn answer_with(svc: &WikiService, source_kind: WikiSource, req: WikiRequest) -> WikiReply {
    answer_for(svc, source_kind, None, req)
}

/// [`answer_with`] for `project`, which names the wiki an
/// [`WikiRequest::Open`] is announced for.
fn answer_for(svc: &WikiService, source_kind: WikiSource, project: Option<&str>, req: WikiRequest) -> WikiReply {
    use shelbi_wiki::markdown::{compose, sections};
    let status = |svc: &WikiService| WikiReply::Status(status_reply(&svc.status(), source_kind));
    let result: Result<WikiReply, OpError> = match req {
        WikiRequest::Init | WikiRequest::Status => Ok(status(svc)),
        WikiRequest::Tree { path } => tree(svc.root(), path.as_deref().unwrap_or("")).map(WikiReply::Tree),
        WikiRequest::Read { path, section } => svc.read(&path).and_then(|content| {
            let content = match section {
                Some(h) => sections::read_section(&content, &h).map_err(section_error)?,
                None => content,
            };
            Ok(WikiReply::Doc { path, content })
        }),
        WikiRequest::Write {
            path,
            content,
            section,
            by,
        } => match section {
            None => svc.write(&path, &content, source(by)),
            Some(h) => svc.update(&path, source(by), |c| compose::write_section(c, &h, &content).map_err(section_error)),
        }
        .map(|()| WikiReply::Done),
        WikiRequest::Overwrite {
            path,
            content,
            title,
            by,
        } => svc
            .update(&path, source(by), |c| Ok(compose::render_write_output(c, &content, title.as_deref())))
            .map(|()| WikiReply::Done),
        WikiRequest::Append {
            path,
            content,
            section,
            by,
        } => svc
            .update(&path, source(by), |c| match &section {
                None => Ok(compose::append_to_document(c, &content)),
                Some(h) => compose::append_to_section(c, h, &content).map_err(section_error),
            })
            .map(|()| WikiReply::Done),
        WikiRequest::Prepend {
            path,
            content,
            section,
            by,
        } => svc
            .update(&path, source(by), |c| match &section {
                None => Ok(compose::prepend_to_document(c, &content)),
                Some(h) => compose::prepend_to_section(c, h, &content).map_err(section_error),
            })
            .map(|()| WikiReply::Done),
        WikiRequest::New {
            path,
            content,
            title,
            by,
        } => if path.ends_with(".md") {
            let content = new_document(&path, content.as_deref(), title.as_deref());
            svc.create_document(&path, &content, source(by))
        } else {
            svc.create_folder(&path, source(by)).and_then(|()| match content.filter(|c| !c.trim().is_empty()) {
                Some(description) => describe_folder(svc, &path, description.trim(), source(by)),
                None => Ok(()),
            })
        }
        .map(|()| WikiReply::Done),
        WikiRequest::Move { from, to, by } => svc.rename(&from, &to, source(by)).map(|()| WikiReply::Done),
        WikiRequest::Copy { from, to, by } => svc.copy(&from, &to, source(by)).map(|()| WikiReply::Done),
        WikiRequest::Remove { path, section, by } => match section {
            None => svc.remove(&path, source(by)),
            Some(h) => svc.update(&path, source(by), |c| compose::remove_section(c, &h).map_err(section_error)),
        }
        .map(|()| WikiReply::Done),
        WikiRequest::Search { query, folder, regex } => search(svc.root(), &query, folder, regex).map(WikiReply::Matches),
        WikiRequest::List { path } => list(svc.root(), path.as_deref().unwrap_or("")).map(WikiReply::Entries),
        WikiRequest::Tasks { path, section, pending } => {
            tasks(svc, &path, section.as_deref(), pending).map(WikiReply::Tasks)
        }
        WikiRequest::SetTask {
            path,
            text,
            section,
            checked,
            by,
        } => svc
            .update(&path, source(by), |c| set_task(c, &text, section.as_deref(), checked))
            .map(|()| WikiReply::Done),
        WikiRequest::Diff { path } => svc.diff(path.as_deref()).map(WikiReply::Text),
        WikiRequest::Log { path, limit } => svc.log(path.as_deref(), limit).map(|commits| {
            WikiReply::Log(
                commits
                    .into_iter()
                    .map(|c| WikiCommit {
                        sha: c.sha,
                        author: c.author,
                        date: c.date,
                        subject: c.subject,
                        paths: c.paths,
                    })
                    .collect(),
            )
        }),
        WikiRequest::Open { path } => open(svc, project, path),
        WikiRequest::Sync(op) => {
            match op {
                SyncOp::Now => {
                    svc.sync_now(SYNC_NOW_WAIT);
                }
                SyncOp::Pause => svc.pause(),
                SyncOp::Resume => svc.resume(),
                SyncOp::Active => svc.touch_active(),
            }
            Ok(status(svc))
        }
        WikiRequest::Conflict { path } => svc
            .conflict(&path)
            .map(|c| {
                WikiReply::Conflict(WikiConflict {
                    path: c.path,
                    base: c.base,
                    ours: c.ours,
                    theirs: c.theirs,
                    detected_at: c.detected_at,
                })
            })
            .ok_or_else(|| OpError::Conflict(format!("`{path}` isn't in conflict"))),
        WikiRequest::Resolve { path, content } => svc.resolve(&path, content.as_deref()).map(|()| WikiReply::Done),
    };
    result.unwrap_or_else(op_failure)
}

fn source(by: Author) -> ChangeSource {
    match by {
        Author::Person => ChangeSource::Local,
        Author::Agent => ChangeSource::Agent,
    }
}

fn op_failure(e: OpError) -> WikiReply {
    let kind = match &e {
        OpError::NotFound(_) => FailureKind::NotFound,
        OpError::Invalid(_) => FailureKind::Invalid,
        OpError::Exists(_) => FailureKind::Exists,
        OpError::Conflict(_) => FailureKind::Conflict,
        OpError::Failed(_) => FailureKind::Failed,
    };
    WikiReply::failed(kind, e.to_string())
}

fn section_error(e: shelbi_wiki::markdown::MarkdownError) -> OpError {
    use shelbi_wiki::markdown::MarkdownError;
    match e {
        MarkdownError::SectionNotFound(h) => OpError::NotFound(format!("section `{h}`")),
        MarkdownError::TaskNotFound(t) => OpError::NotFound(format!("a task matching `{t}`")),
        other => OpError::Invalid(other.to_string()),
    }
}

/// A new document's text: `content` under `# <title>` (the title from the
/// file name when none is given), unless `content` has an H1 of its own.
fn new_document(path: &str, content: Option<&str>, title: Option<&str>) -> String {
    let title = title.map(str::to_string).unwrap_or_else(|| {
        let stem = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        shelbi_wiki::fs::paths::humanize_folder_name(&stem)
    });
    shelbi_wiki::markdown::compose::render_new_document(&title, content.unwrap_or(""))
}

/// Give a folder a description: its `.contextstore/settings.yml`, as
/// ContextStore writes it.
fn describe_folder(svc: &WikiService, path: &str, description: &str, by: ChangeSource) -> Result<(), OpError> {
    use shelbi_wiki::fs::settings::{ProjectSettings, ProjectSettingsFile};
    let name = path.trim_matches('/').rsplit('/').next().unwrap_or(path).to_string();
    let file = ProjectSettingsFile::current(ProjectSettings {
        name,
        icon: None,
        description: Some(description.to_string()),
    });
    let yaml = yaml_serde::to_string(&file).map_err(|e| OpError::Failed(e.to_string()))?;
    svc.write(&format!("{}/.contextstore/settings.yml", path.trim_matches('/')), &yaml, by)
}

/// Check or uncheck the one task matching `needle` (in `section` when
/// given), as `cstore check` does: the match is made within the section,
/// the toggle on the whole document by the matched task's full text.
fn set_task(content: &str, needle: &str, section: Option<&str>, checked: bool) -> Result<String, OpError> {
    use shelbi_wiki::markdown::{sections, tasks, MarkdownError};
    let needle = match section {
        None => needle.to_string(),
        Some(h) => {
            let scoped = sections::read_section(content, h).map_err(section_error)?;
            let lowered = needle.to_lowercase();
            let found: Vec<tasks::Task> =
                tasks::list(&scoped).into_iter().filter(|t| t.text.to_lowercase().contains(&lowered)).collect();
            match found.len() {
                0 => return Err(section_error(MarkdownError::TaskNotFound(needle.to_string()))),
                1 => found[0].text.clone(),
                count => {
                    return Err(section_error(MarkdownError::AmbiguousTask {
                        needle: needle.to_string(),
                        count,
                    }))
                }
            }
        }
    };
    let out = if checked {
        tasks::check(content, &needle)
    } else {
        tasks::uncheck(content, &needle)
    };
    out.map(|mut o| {
        if content.ends_with('\n') && !o.ends_with('\n') {
            o.push('\n');
        }
        o
    })
    .map_err(section_error)
}

/// The tasks of the document at `path`, or of every document directly in
/// the folder at `path`, with document line numbers.
fn tasks(svc: &WikiService, path: &str, section: Option<&str>, pending: bool) -> Result<Vec<WikiTask>, OpError> {
    use shelbi_wiki::markdown::sections;
    let abs = svc.resolve_path(path)?;
    let root = svc.root();
    let docs: Vec<String> = if abs.is_dir() {
        let folder_name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        shelbi_wiki::model::list_documents(&abs, root, &folder_name)
            .map_err(|e| OpError::Failed(e.to_string()))?
            .into_iter()
            .map(|d| relative(root, &d.path))
            .collect()
    } else {
        vec![path.trim_matches('/').to_string()]
    };
    let folder_mode = abs.is_dir();
    let mut out = Vec::new();
    for doc in docs {
        let content = svc.read(&doc)?;
        let (text, offset) = match section {
            None => (content.clone(), 0),
            Some(h) => match sections::find_section(&content, h) {
                Ok(found) => (sections::read_section(&content, h).map_err(section_error)?, found.start_line - 1),
                // Across a folder, a document without the section has none
                // of its tasks.
                Err(_) if folder_mode => continue,
                Err(e) => return Err(section_error(e)),
            },
        };
        for task in shelbi_wiki::markdown::tasks::list(&text) {
            if pending && task.checked {
                continue;
            }
            out.push(WikiTask {
                path: doc.clone(),
                line: task.line + offset,
                checked: task.checked,
                text: task.text,
            });
        }
    }
    Ok(out)
}

/// Announce that someone wants to see the wiki at `path`.
fn open(svc: &WikiService, project: Option<&str>, path: Option<String>) -> Result<WikiReply, OpError> {
    let path = path.map(|p| p.trim_matches('/').to_string()).filter(|p| !p.is_empty());
    if let Some(p) = &path {
        if !svc.resolve_path(p)?.exists() {
            return Err(OpError::NotFound(p.clone()));
        }
    }
    if let Some(project) = project {
        shelbi_state::publish_change(shelbi_state::ChangeNotification::WikiOpen {
            project: project.to_string(),
            path,
        });
    }
    Ok(WikiReply::Done)
}

fn relative(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root).unwrap_or(abs).to_string_lossy().replace('\\', "/")
}

/// One folder's entries (its subfolders, then its documents, each with its
/// counts), or a document's headings when `under` is a document.
pub fn list(root: &Path, under: &str) -> Result<Vec<WikiEntry>, OpError> {
    use shelbi_wiki::markdown::compose;
    let under = under.trim_matches('/');
    let fail = |e: shelbi_wiki::fs::FsError| OpError::Failed(e.to_string());
    let abs = if under.is_empty() {
        root.to_path_buf()
    } else {
        shelbi_wiki::fs::paths::resolve_in_space(root, under).map_err(|e| OpError::Invalid(e.to_string()))?
    };
    if abs.is_file() {
        let content = std::fs::read_to_string(&abs).map_err(|e| OpError::Failed(e.to_string()))?;
        return Ok(compose::section_counts(&content)
            .into_iter()
            .map(|s| WikiEntry {
                kind: EntryKind::Section,
                name: s.heading,
                path: under.to_string(),
                title: s.text,
                level: s.level,
                lines: s.lines,
                words: s.words,
                bytes: 0,
                documents: 0,
            })
            .collect());
    }
    if !abs.is_dir() {
        return Err(OpError::NotFound(under.to_string()));
    }
    let folder_name = if under.is_empty() {
        shelbi_wiki::fs::settings::load_space_settings(root)
            .map(|s| s.name)
            .unwrap_or_default()
    } else {
        abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    let mut out = Vec::new();
    for folder in shelbi_wiki::fs::tree::build_children(root, Some(root), under).map_err(fail)? {
        let documents = shelbi_wiki::model::list_documents(&folder.path, root, &folder.name)
            .map(|d| d.len())
            .unwrap_or(0);
        out.push(WikiEntry {
            kind: EntryKind::Folder,
            title: folder.name.clone(),
            name: folder.name,
            path: folder.relative_path,
            level: 0,
            lines: 0,
            words: 0,
            bytes: 0,
            documents,
        });
    }
    for doc in shelbi_wiki::model::list_documents(&abs, root, &folder_name).map_err(fail)? {
        let content = std::fs::read_to_string(&doc.path).unwrap_or_default();
        let counts = compose::counts(&content);
        out.push(WikiEntry {
            kind: EntryKind::Document,
            name: doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path: relative(root, &doc.path),
            title: doc.title,
            level: 0,
            lines: counts.lines,
            words: counts.words,
            bytes: counts.bytes,
            documents: 0,
        });
    }
    Ok(out)
}

/// The wire form of a service's status.
pub fn status_reply(report: &StatusReport, source_kind: WikiSource) -> WikiStatus {
    WikiStatus {
        set_up: true,
        state: match &report.status {
            SyncStatus::Synced => SyncState::Synced,
            SyncStatus::Syncing => SyncState::Syncing,
            SyncStatus::Pending => SyncState::Pending,
            SyncStatus::Offline => SyncState::Offline,
            SyncStatus::Conflict => SyncState::Conflict,
            SyncStatus::Error(d) => SyncState::Error(d.clone()),
        },
        conflicts: report.conflicts.clone(),
        paused: report.paused,
        auto: report.auto,
        source: match source_kind {
            WikiSource::Branch => "branch".into(),
            WikiSource::Folder => "folder".into(),
        },
        branch: report.branch.clone(),
        root: report.root.to_string_lossy().into_owned(),
        last_synced: report.last_synced.clone(),
        pull_request: report.pull_request.as_ref().map(|pr| pr.url.clone()),
    }
}

/// The folders and documents under `under` (wiki-relative; `""` is the
/// root): subfolders in `folderOrder`, then documents pinned and by title.
pub fn tree(root: &Path, under: &str) -> Result<Vec<WikiNode>, OpError> {
    let under = under.trim_matches('/');
    let dir = if under.is_empty() {
        root.to_path_buf()
    } else {
        shelbi_wiki::fs::paths::resolve_in_space(root, under).map_err(|e| OpError::Invalid(e.to_string()))?
    };
    if !dir.is_dir() {
        return Err(OpError::NotFound(under.to_string()));
    }
    let folder_name = if under.is_empty() {
        shelbi_wiki::fs::settings::load_space_settings(root)
            .map(|s| s.name)
            .unwrap_or_default()
    } else {
        dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    build_level(root, under, &folder_name)
}

fn build_level(root: &Path, under: &str, folder_name: &str) -> Result<Vec<WikiNode>, OpError> {
    let fail = |e: shelbi_wiki::fs::FsError| OpError::Failed(e.to_string());
    let mut out = Vec::new();
    for folder in shelbi_wiki::fs::tree::build_children(root, Some(root), under).map_err(fail)? {
        let children = build_level(root, &folder.relative_path, &folder.name)?;
        out.push(WikiNode {
            path: folder.relative_path,
            title: folder.name.clone(),
            name: folder.name,
            kind: NodeKind::Folder,
            children,
        });
    }
    let dir = if under.is_empty() { root.to_path_buf() } else { root.join(under) };
    for doc in shelbi_wiki::model::list_documents(&dir, root, folder_name).map_err(fail)? {
        let rel = doc.path.strip_prefix(root).unwrap_or(&doc.path).to_string_lossy().replace('\\', "/");
        out.push(WikiNode {
            name: doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path: rel,
            title: doc.title,
            kind: NodeKind::Document,
            children: Vec::new(),
        });
    }
    Ok(out)
}

fn search(root: &Path, pattern: &str, folder: Option<String>, regex: bool) -> Result<Vec<WikiMatch>, OpError> {
    use shelbi_wiki::search::{Query, QueryMode};
    let query = Query {
        pattern: pattern.to_string(),
        mode: if regex { QueryMode::Regex } else { QueryMode::Substring },
        folder: folder.map(PathBuf::from),
        ..Query::default()
    };
    let found = shelbi_wiki::search::search(root, &query).map_err(|e| OpError::Invalid(e.to_string()))?;
    Ok(found
        .into_iter()
        .map(|m| WikiMatch {
            path: m.relative_path.to_string_lossy().replace('\\', "/"),
            line: m.line_index + 1,
            text: m.line_text,
            start: m.byte_range.0,
            end: m.byte_range.1,
        })
        .collect())
}

/// Announces one project's wiki changes on `events.log` and the change bus.
struct HubSink {
    project: String,
}

impl EventSink for HubSink {
    fn changed(&self, paths: &[String], source: ChangeSource) {
        let line = changed_line(&self.project, paths, source.as_str());
        if let Err(e) = shelbi_state::append_external_event(&line) {
            tracing::warn!(project = %self.project, error = %e, "wiki: couldn't log a change");
        }
        shelbi_state::publish_change(shelbi_state::ChangeNotification::Wiki {
            project: self.project.clone(),
            paths: paths.to_vec(),
            source: source.as_str().to_string(),
        });
    }

    fn status(&self, status: &SyncStatus) {
        let status = status.to_string();
        let line = format!("wiki_status project={} status={}", self.project, quoted(&status));
        if let Err(e) = shelbi_state::append_external_event(&line) {
            tracing::warn!(project = %self.project, error = %e, "wiki: couldn't log a status");
        }
        shelbi_state::publish_change(shelbi_state::ChangeNotification::WikiStatus {
            project: self.project.clone(),
            status,
        });
    }
}

/// The `wiki_changed` event line: the paths comma-separated, summarized
/// past [`MAX_PATHS_BYTES`] so the line stays one atomic append.
pub fn changed_line(project: &str, paths: &[String], source: &str) -> String {
    let mut shown = String::new();
    let mut count = 0;
    for path in paths {
        let next = if shown.is_empty() { path.len() } else { shown.len() + 1 + path.len() };
        if next > MAX_PATHS_BYTES {
            break;
        }
        if !shown.is_empty() {
            shown.push(',');
        }
        shown.push_str(path);
        count += 1;
    }
    if count < paths.len() {
        shown.push_str(&format!(",+{}", paths.len() - count));
    }
    format!("wiki_changed project={project} paths={} source={source}", quoted(&shown))
}

/// An event value, quoted when it holds whitespace (`split_event_words`
/// keeps a quoted value whole). Quotes and line breaks inside it are
/// replaced so the record stays one well-formed line.
fn quoted(value: &str) -> String {
    let clean: String = value
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '\n' | '\r' => ' ',
            c => c,
        })
        .collect();
    if clean.chars().any(char::is_whitespace) || clean.is_empty() {
        format!("\"{clean}\"")
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::process::Command;

    thread_local! {
        pub(super) static GIT_ENV: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    }

    fn git(env: &[(String, String)], dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn events() -> String {
        std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap_or_default()
    }

    /// The hub path end to end: a project with no wiki, `Init`, writes and
    /// reads through [`answer`], the `wiki_changed` / `wiki_status` lines on
    /// events.log, and a sync that lands on the (local bare) origin.
    #[test]
    fn a_project_wiki_is_set_up_written_and_synced_through_the_hub() {
        let _home = crate::test_lock::isolated_home();
        let home = PathBuf::from(std::env::var_os("SHELBI_HOME").unwrap());
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let gitconfig = root.join("gitconfig");
        std::fs::write(
            &gitconfig,
            "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        let env = vec![
            ("GIT_CONFIG_GLOBAL".to_string(), gitconfig.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ];
        GIT_ENV.with(|e| *e.borrow_mut() = env.clone());
        git(&env, &root, &["init", "-q", "--bare", "origin.git"]);
        git(&env, &root, &["clone", "-q", "origin.git", "hub"]);
        let hub = root.join("hub");
        std::fs::write(hub.join("main.txt"), "code\n").unwrap();
        git(&env, &hub, &["add", "-A"]);
        git(&env, &hub, &["commit", "-q", "-m", "init"]);
        git(&env, &hub, &["push", "-q", "origin", "main"]);
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(
            home.join("projects/demo.yaml"),
            format!(
                "name: Demo\nrepo: {hub}\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: {hub}\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n",
                hub = hub.display()
            ),
        )
        .unwrap();

        // No `wiki:` block and no worktree: no wiki until asked.
        match answer("demo", WikiRequest::Status) {
            WikiReply::Status(s) => assert!(!s.set_up),
            other => panic!("{other:?}"),
        }
        match answer("demo", WikiRequest::Read { path: "README.md".into(), section: None }) {
            WikiReply::Failed(f) => assert_eq!(f.kind, FailureKind::NotSetUp),
            other => panic!("{other:?}"),
        }
        assert!(!home.join("projects/demo/wiki").exists());

        match answer("demo", WikiRequest::Init) {
            WikiReply::Status(s) => {
                assert!(s.set_up);
                assert_eq!((s.source.as_str(), s.branch.as_str()), ("branch", "docs/planning"));
            }
            other => panic!("{other:?}"),
        }
        assert!(home.join("projects/demo/wiki/.contextstore/settings.yml").is_file());

        let doc = "# Plan\n\n## Goals\n\nShip it.\n";
        assert_eq!(
            answer("demo", WikiRequest::Write { path: "Plans/plan.md".into(), content: doc.into(), section: None, by: Author::Agent }),
            WikiReply::Done
        );
        assert!(
            events().contains("wiki_changed project=demo paths=Plans/plan.md source=agent"),
            "{}",
            events()
        );
        assert_eq!(
            answer("demo", WikiRequest::Append { path: "Plans/plan.md".into(), content: "Soon.".into(), section: Some("## Goals".into()), by: Author::Person }),
            WikiReply::Done
        );
        match answer("demo", WikiRequest::Read { path: "Plans/plan.md".into(), section: Some("## Goals".into()) }) {
            WikiReply::Doc { content, .. } => assert!(content.contains("Ship it.") && content.contains("Soon."), "{content}"),
            other => panic!("{other:?}"),
        }
        match answer("demo", WikiRequest::Tree { path: None }) {
            WikiReply::Tree(nodes) => {
                let plans = nodes.iter().find(|n| n.path == "Plans").expect("the Plans folder");
                assert_eq!(plans.kind, NodeKind::Folder);
                assert_eq!(plans.children[0].title, "Plan");
                assert_eq!(plans.children[0].path, "Plans/plan.md");
                assert!(nodes.iter().any(|n| n.path == "README.md" && n.title == "Demo"));
            }
            other => panic!("{other:?}"),
        }
        match answer("demo", WikiRequest::Search { query: "ship".into(), folder: None, regex: false }) {
            WikiReply::Matches(m) => assert_eq!((m[0].path.as_str(), m[0].line), ("Plans/plan.md", 5)),
            other => panic!("{other:?}"),
        }
        match answer("demo", WikiRequest::Read { path: "nope.md".into(), section: None }) {
            WikiReply::Failed(f) => assert_eq!(f.kind.exit_code(), 2),
            other => panic!("{other:?}"),
        }

        match answer("demo", WikiRequest::Sync(SyncOp::Now)) {
            WikiReply::Status(s) => assert_eq!(s.state, SyncState::Synced, "{s:?}"),
            other => panic!("{other:?}"),
        }
        let pushed = git(&env, &root.join("origin.git"), &["show", "docs/planning:Plans/plan.md"]);
        assert!(pushed.contains("Soon."), "{pushed}");
        assert!(events().contains("wiki_status project=demo status=synced"), "{}", events());

        stop_all();
        GIT_ENV.with(|e| e.borrow_mut().clear());
    }

    #[test]
    fn a_changed_line_lists_its_paths_and_source() {
        let line = changed_line("demo", &["Plans/a.md".into(), "README.md".into()], "agent");
        assert_eq!(line, "wiki_changed project=demo paths=Plans/a.md,README.md source=agent");
        let line = changed_line("demo", &["My Plan.md".into()], "local");
        let words = shelbi_state::split_event_words(&line);
        assert_eq!(words, vec!["wiki_changed", "project=demo", "paths=\"My Plan.md\"", "source=local"]);
    }

    #[test]
    fn a_long_path_list_is_summarized() {
        let paths: Vec<String> = (0..500).map(|i| format!("Folder/document-{i}.md")).collect();
        let line = changed_line("demo", &paths, "pull");
        assert!(line.len() < 2300, "{}", line.len());
        assert!(line.contains(",+"), "{line}");
    }

    #[test]
    fn a_task_is_toggled_by_its_text_within_a_section() {
        let doc = "# T\n\n## A\n\n- [ ] Ship\n\n## B\n\n- [ ] Ship docs\n";
        assert_eq!(
            set_task(doc, "ship", Some("## B"), true).unwrap(),
            "# T\n\n## A\n\n- [ ] Ship\n\n## B\n\n- [x] Ship docs\n"
        );
        assert!(matches!(set_task(doc, "ship", None, true), Err(OpError::Invalid(_))), "two match");
        assert!(matches!(set_task(doc, "nope", None, true), Err(OpError::NotFound(_))));
        assert!(matches!(set_task(doc, "x", Some("## C"), true), Err(OpError::NotFound(_))));
    }

    #[test]
    fn list_answers_folders_documents_and_headings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("Plans/Old")).unwrap();
        std::fs::write(root.join("Plans/Old/x.md"), "# X\n").unwrap();
        std::fs::write(root.join("Plans/b.md"), "# Bee\n\n## One\n\ntwo words\n").unwrap();
        std::fs::write(root.join("Plans/a.md"), "# Ay\n").unwrap();
        let entries = list(root, "Plans/").unwrap();
        let shape: Vec<(EntryKind, &str)> = entries.iter().map(|e| (e.kind, e.path.as_str())).collect();
        assert_eq!(
            shape,
            vec![(EntryKind::Folder, "Plans/Old"), (EntryKind::Document, "Plans/a.md"), (EntryKind::Document, "Plans/b.md")]
        );
        assert_eq!(entries[0].documents, 1);
        assert_eq!((entries[2].title.as_str(), entries[2].words), ("Bee", 6));
        let headings = list(root, "Plans/b.md").unwrap();
        assert_eq!(headings.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["# Bee", "## One"]);
        assert!(matches!(list(root, "Nope"), Err(OpError::NotFound(_))));
    }

    #[test]
    fn a_new_document_is_titled_from_its_file_name() {
        assert_eq!(new_document("Plans/wiki-sync.md", None, None), "# Wiki Sync\n");
        assert_eq!(new_document("Plans/a.md", Some("Body."), Some("API")), "# API\n\nBody.\n");
    }
}
