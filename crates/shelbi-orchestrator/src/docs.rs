//! The hub's docs services, one per project, and the requests clients send
//! them ([`shelbi_app::docs`], riding a control `AppRequest`).
//!
//! The daemon owns these: [`reconcile`] (called on the daemon's docs tick)
//! starts a service for each open project that has docs and stops the
//! ones nobody has used for a while after their project closed; [`answer`]
//! (called from [`crate::far::answer`]) runs one request, starting the
//! project's service on demand. A project has docs when its YAML has a
//! `docs:` block, or when its docs worktree
//! (`~/.shelbi/projects/<name>/docs/`) already exists; anything else gets
//! one only from [`DocsRequest::Init`].
//!
//! Every change and status the services report goes two places: a line on
//! `events.log` (`docs_changed project=… paths=… source=…`,
//! `docs_status project=… status=…`) and a push to subscribed clients on
//! the change bus ([`shelbi_state::ChangeNotification::Docs`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use shelbi_app::docs::{
    Author, EntryKind, FailureKind, NodeKind, SyncOp, SyncState, DocsCommit, DocsConflict, DocsEntry, DocsMatch,
    DocsNode, DocsReply, DocsRequest, DocsStatus, DocsTask,
};
use shelbi_core::{Project, DocsConfig, DocsSource, DocsSyncMode};
use shelbi_docs::service::{ChangeSource, EventSink, OpError, ServiceSpec, StatusReport, SyncStatus, Timing, DocsService};
use shelbi_docs::sync::Layout;

/// How long docs whose project closed keeps running after its last
/// request (an agent or the CLI may still be writing to it).
const IDLE_STOP: Duration = Duration::from_secs(10 * 60);

/// How long a failed start (a remote out of reach while creating the
/// worktree) waits before the next tick retries it.
const START_BACKOFF: Duration = Duration::from_secs(60);

/// How long `shelbi docs sync now` waits for its sync.
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
    service: Arc<DocsService>,
    config: DocsConfig,
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

/// The worktree a project's docs live in: `<project dir>/docs/`. One set up
/// before Documentation was called that is at `wiki/`, and is moved here the
/// first time it's asked for ([`moved_from_legacy`]).
pub fn docs_dir(project: &str) -> Result<PathBuf, String> {
    let dir = shelbi_state::project_dir(project).map_err(|e| e.to_string())?;
    Ok(moved_from_legacy(&dir, LEGACY_WORKTREE, "docs"))
}

fn conflicts_file(project: &str) -> Result<PathBuf, String> {
    let dir = shelbi_state::project_dir(project).map_err(|e| e.to_string())?;
    Ok(moved_from_legacy(&dir, LEGACY_CONFLICTS_FILE, "docs-conflicts.json"))
}

/// Where the docs' worktree and its held-conflicts file were kept before
/// Documentation was called that, beside the project's other state.
const LEGACY_WORKTREE: &str = "wiki";
const LEGACY_CONFLICTS_FILE: &str = "wiki-conflicts.json";

/// `dir/current`, after moving `dir/legacy` there when only the legacy one
/// exists. A git worktree moves with `git worktree move` (so the hub
/// checkout's record of it follows, and nothing uncommitted is lost);
/// anything else with a rename. When the move fails the legacy path is
/// returned and used as is, so the documents are never out of reach; the
/// next start tries again.
fn moved_from_legacy(dir: &Path, legacy: &str, current: &str) -> PathBuf {
    static MOVING: Mutex<()> = Mutex::new(());
    let _guard = lock(&MOVING);
    let (old, new) = (dir.join(legacy), dir.join(current));
    if new.exists() || !old.exists() {
        return new;
    }
    let moved = if old.join(".git").is_file() {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&old)
            .args(["worktree", "move"])
            .arg(&old)
            .arg(&new)
            .envs(git_env())
            .output()
            .map_err(|e| e.to_string())
            .and_then(|out| {
                if out.status.success() {
                    Ok(())
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
                }
            })
    } else {
        std::fs::rename(&old, &new).map_err(|e| e.to_string())
    };
    match moved {
        Ok(()) => {
            tracing::info!(from = %old.display(), to = %new.display(), "docs: moved from its pre-rename place");
            new
        }
        Err(e) => {
            tracing::warn!(from = %old.display(), to = %new.display(), error = %e, "docs: couldn't move from its pre-rename place; using it there");
            old
        }
    }
}

/// The docs `project` has: its `docs:` block, else the default when its
/// worktree is already there (or `create` asks for one), else none.
fn effective_config(project: &Project, create: bool) -> Result<Option<DocsConfig>, String> {
    if let Some(cfg) = &project.docs {
        return Ok(Some(cfg.clone()));
    }
    if create || docs_dir(&project.name)?.join(".git").exists() {
        return Ok(Some(DocsConfig::default()));
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
        .ok_or_else(|| "the project has no local machine to keep its docs on".to_string())
}

/// The service spec for `project`'s docs as `config` describes it.
pub fn spec_for(project: &Project, config: &DocsConfig) -> Result<ServiceSpec, String> {
    config.validate().map_err(|e| e.to_string())?;
    let repo = hub_checkout(project)?;
    let worktree = docs_dir(&project.name)?;
    let layout = match (config.source, config.folder()) {
        (DocsSource::Folder, Some(folder)) => {
            Layout::folder(repo, worktree, config.worktree_branch(), folder, project.base_branch())
        }
        _ => Layout::branch(repo, worktree, config.worktree_branch()),
    };
    let pull_requests: Option<Arc<dyn shelbi_docs::sync::pr::PullRequests>> = match config.source {
        DocsSource::Folder => Some(Arc::new(shelbi_docs::sync::pr::GhPullRequests)),
        DocsSource::Branch => None,
    };
    Ok(ServiceSpec {
        layout,
        name: project.display_label().to_string(),
        auto: config.sync == DocsSyncMode::Auto,
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

/// `project`'s running service, started if it has docs (or `create`
/// asks for one). `Ok(None)` when it has none.
fn service(project: &str, create: bool) -> Result<Option<Arc<DocsService>>, String> {
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
    match DocsService::start(spec, sink) {
        Ok((service, setup)) => {
            let service = Arc::new(service);
            tracing::info!(project, ?setup, root = %service.root().display(), "docs: service started");
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
            Err(format!("couldn't set up the docs: {e}"))
        }
    }
}

/// One docs tick: start a service for each open project with docs, and
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
            tracing::warn!(project = %project, error = %e, "docs: service didn't start");
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
            tracing::info!(project = %project, "docs: service stopped (project closed)");
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

/// Run one docs request for `project`. Blocking.
pub fn answer(project: &str, req: DocsRequest) -> DocsReply {
    let create = matches!(req, DocsRequest::Init);
    let svc = match service(project, create) {
        Ok(Some(svc)) => svc,
        Ok(None) if matches!(req, DocsRequest::Status) => return DocsReply::Status(DocsStatus::not_set_up()),
        Ok(None) => {
            return DocsReply::failed(
                FailureKind::NotSetUp,
                format!("project `{project}` has no docs yet; `shelbi docs init` sets them up"),
            )
        }
        Err(e) => return DocsReply::failed(FailureKind::Failed, e),
    };
    let source_kind = shelbi_state::load_project(project)
        .ok()
        .and_then(|p| p.docs)
        .map(|w| w.source)
        .unwrap_or_default();
    answer_for(&svc, source_kind, Some(project), req)
}

/// Run `req` against a running service.
pub fn answer_with(svc: &DocsService, source_kind: DocsSource, req: DocsRequest) -> DocsReply {
    answer_for(svc, source_kind, None, req)
}

/// [`answer_with`] for `project`, which names the docs an
/// [`DocsRequest::Open`] is announced for.
fn answer_for(svc: &DocsService, source_kind: DocsSource, project: Option<&str>, req: DocsRequest) -> DocsReply {
    use shelbi_docs::markdown::{compose, sections};
    let status = |svc: &DocsService| DocsReply::Status(status_reply(&svc.status(), source_kind));
    let result: Result<DocsReply, OpError> = match req {
        DocsRequest::Init | DocsRequest::Status => Ok(status(svc)),
        DocsRequest::Tree { path } => tree(svc.root(), path.as_deref().unwrap_or("")).map(DocsReply::Tree),
        DocsRequest::Read { path, section } => svc.read(&path).and_then(|content| {
            let content = match section {
                Some(h) => sections::read_section(&content, &h).map_err(section_error)?,
                None => content,
            };
            Ok(DocsReply::Doc { path, content })
        }),
        DocsRequest::Write {
            path,
            content,
            section,
            by,
        } => match section {
            None => svc.write(&path, &content, source(by)),
            Some(h) => svc.update(&path, source(by), |c| compose::write_section(c, &h, &content).map_err(section_error)),
        }
        .map(|()| DocsReply::Done),
        DocsRequest::Overwrite {
            path,
            content,
            title,
            by,
        } => svc
            .update(&path, source(by), |c| Ok(compose::render_write_output(c, &content, title.as_deref())))
            .map(|()| DocsReply::Done),
        DocsRequest::Append {
            path,
            content,
            section,
            by,
        } => svc
            .update(&path, source(by), |c| match &section {
                None => Ok(compose::append_to_document(c, &content)),
                Some(h) => compose::append_to_section(c, h, &content).map_err(section_error),
            })
            .map(|()| DocsReply::Done),
        DocsRequest::Prepend {
            path,
            content,
            section,
            by,
        } => svc
            .update(&path, source(by), |c| match &section {
                None => Ok(compose::prepend_to_document(c, &content)),
                Some(h) => compose::prepend_to_section(c, h, &content).map_err(section_error),
            })
            .map(|()| DocsReply::Done),
        DocsRequest::New {
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
        .map(|()| DocsReply::Done),
        DocsRequest::Move { from, to, by } => svc.rename(&from, &to, source(by)).map(|()| DocsReply::Done),
        DocsRequest::Copy { from, to, by } => svc.copy(&from, &to, source(by)).map(|()| DocsReply::Done),
        DocsRequest::Remove { path, section, by } => match section {
            None => svc.remove(&path, source(by)),
            Some(h) => svc.update(&path, source(by), |c| compose::remove_section(c, &h).map_err(section_error)),
        }
        .map(|()| DocsReply::Done),
        DocsRequest::Trash { path, by } => svc.trash(&path, source(by)).map(|token| DocsReply::Trashed { token }),
        DocsRequest::Untrash { token, by } => svc.untrash(&token, source(by)).map(|_| DocsReply::Done),
        DocsRequest::Reorder { folder, order, by } => {
            svc.set_folder_order(&folder, &order, source(by)).map(|()| DocsReply::Done)
        }
        DocsRequest::Search { query, folder, regex } => search(svc.root(), &query, folder, regex).map(DocsReply::Matches),
        DocsRequest::List { path } => list(svc.root(), path.as_deref().unwrap_or("")).map(DocsReply::Entries),
        DocsRequest::Tasks { path, section, pending } => {
            tasks(svc, &path, section.as_deref(), pending).map(DocsReply::Tasks)
        }
        DocsRequest::SetTask {
            path,
            text,
            section,
            checked,
            by,
        } => svc
            .update(&path, source(by), |c| set_task(c, &text, section.as_deref(), checked))
            .map(|()| DocsReply::Done),
        DocsRequest::Diff { path } => svc.diff(path.as_deref()).map(DocsReply::Text),
        DocsRequest::Log { path, limit } => svc.log(path.as_deref(), limit).map(|commits| {
            DocsReply::Log(
                commits
                    .into_iter()
                    .map(|c| DocsCommit {
                        sha: c.sha,
                        author: c.author,
                        date: c.date,
                        subject: c.subject,
                        paths: c.paths,
                        by: match c.by.as_slice() {
                            [one] if one == "agent" => Some(Author::Agent),
                            [one] if one == "person" => Some(Author::Person),
                            _ => None,
                        },
                    })
                    .collect(),
            )
        }),
        DocsRequest::Open { path } => open(svc, project, path),
        DocsRequest::Sync(op) => {
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
        DocsRequest::Conflict { path } => svc
            .conflict(&path)
            .map(|c| DocsReply::Conflict(wire_conflict(c)))
            .ok_or_else(|| OpError::Conflict(format!("`{path}` isn't in conflict"))),
        DocsRequest::Resolve { path, resolution, by } => svc
            .resolve_with(&path, source(by), |held| {
                shelbi_app::docs_conflict::resolve(&wire_conflict(held.clone()), &resolution).map_err(OpError::Conflict)
            })
            .map(|()| DocsReply::Done),
    };
    result.unwrap_or_else(op_failure)
}

fn wire_conflict(c: shelbi_docs::sync::conflicts::Conflict) -> DocsConflict {
    DocsConflict {
        path: c.path,
        base: c.base,
        ours: c.ours,
        theirs: c.theirs,
        detected_at: c.detected_at,
    }
}

fn source(by: Author) -> ChangeSource {
    match by {
        Author::Person => ChangeSource::Local,
        Author::Agent => ChangeSource::Agent,
    }
}

fn op_failure(e: OpError) -> DocsReply {
    let kind = match &e {
        OpError::NotFound(_) => FailureKind::NotFound,
        OpError::Invalid(_) => FailureKind::Invalid,
        OpError::Exists(_) => FailureKind::Exists,
        OpError::Conflict(_) => FailureKind::Conflict,
        OpError::Failed(_) => FailureKind::Failed,
    };
    DocsReply::failed(kind, e.to_string())
}

fn section_error(e: shelbi_docs::markdown::MarkdownError) -> OpError {
    use shelbi_docs::markdown::MarkdownError;
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
        shelbi_docs::fs::paths::humanize_folder_name(&stem)
    });
    shelbi_docs::markdown::compose::render_new_document(&title, content.unwrap_or(""))
}

/// Give a folder a description: its `.contextstore/settings.yml`, as
/// ContextStore writes it.
fn describe_folder(svc: &DocsService, path: &str, description: &str, by: ChangeSource) -> Result<(), OpError> {
    use shelbi_docs::fs::settings::{ProjectSettings, ProjectSettingsFile};
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
    use shelbi_docs::markdown::{sections, tasks, MarkdownError};
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
fn tasks(svc: &DocsService, path: &str, section: Option<&str>, pending: bool) -> Result<Vec<DocsTask>, OpError> {
    use shelbi_docs::markdown::sections;
    let abs = svc.resolve_path(path)?;
    let root = svc.root();
    let docs: Vec<String> = if abs.is_dir() {
        let folder_name = abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        shelbi_docs::model::list_documents(&abs, root, &folder_name)
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
        for task in shelbi_docs::markdown::tasks::list(&text) {
            if pending && task.checked {
                continue;
            }
            out.push(DocsTask {
                path: doc.clone(),
                line: task.line + offset,
                checked: task.checked,
                text: task.text,
            });
        }
    }
    Ok(out)
}

/// Announce that someone wants to see the docs at `path`.
fn open(svc: &DocsService, project: Option<&str>, path: Option<String>) -> Result<DocsReply, OpError> {
    let path = path.map(|p| p.trim_matches('/').to_string()).filter(|p| !p.is_empty());
    if let Some(p) = &path {
        if !svc.resolve_path(p)?.exists() {
            return Err(OpError::NotFound(p.clone()));
        }
    }
    if let Some(project) = project {
        shelbi_state::publish_change(shelbi_state::ChangeNotification::DocsOpen {
            project: project.to_string(),
            path,
        });
    }
    Ok(DocsReply::Done)
}

fn relative(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root).unwrap_or(abs).to_string_lossy().replace('\\', "/")
}

/// One folder's entries (its subfolders, then its documents, each with its
/// counts), or a document's headings when `under` is a document.
pub fn list(root: &Path, under: &str) -> Result<Vec<DocsEntry>, OpError> {
    use shelbi_docs::markdown::compose;
    let under = under.trim_matches('/');
    let fail = |e: shelbi_docs::fs::FsError| OpError::Failed(e.to_string());
    let abs = if under.is_empty() {
        root.to_path_buf()
    } else {
        shelbi_docs::fs::paths::resolve_in_space(root, under).map_err(|e| OpError::Invalid(e.to_string()))?
    };
    if abs.is_file() {
        let content = std::fs::read_to_string(&abs).map_err(|e| OpError::Failed(e.to_string()))?;
        return Ok(compose::section_counts(&content)
            .into_iter()
            .map(|s| DocsEntry {
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
        shelbi_docs::fs::settings::load_space_settings(root)
            .map(|s| s.name)
            .unwrap_or_default()
    } else {
        abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    let mut out = Vec::new();
    for folder in shelbi_docs::fs::tree::build_children(root, Some(root), under).map_err(fail)? {
        let documents = shelbi_docs::model::list_documents(&folder.path, root, &folder.name)
            .map(|d| d.len())
            .unwrap_or(0);
        out.push(DocsEntry {
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
    for doc in shelbi_docs::model::list_documents(&abs, root, &folder_name).map_err(fail)? {
        let content = std::fs::read_to_string(&doc.path).unwrap_or_default();
        let counts = compose::counts(&content);
        out.push(DocsEntry {
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
pub fn status_reply(report: &StatusReport, source_kind: DocsSource) -> DocsStatus {
    DocsStatus {
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
            DocsSource::Branch => "branch".into(),
            DocsSource::Folder => "folder".into(),
        },
        branch: report.branch.clone(),
        root: report.root.to_string_lossy().into_owned(),
        last_synced: report.last_synced.clone(),
        pull_request: report.pull_request.as_ref().map(|pr| pr.url.clone()),
    }
}

/// The folders and documents under `under` (docs-relative; `""` is the
/// root): subfolders in `folderOrder`, then documents pinned and by title.
pub fn tree(root: &Path, under: &str) -> Result<Vec<DocsNode>, OpError> {
    let under = under.trim_matches('/');
    let dir = if under.is_empty() {
        root.to_path_buf()
    } else {
        shelbi_docs::fs::paths::resolve_in_space(root, under).map_err(|e| OpError::Invalid(e.to_string()))?
    };
    if !dir.is_dir() {
        return Err(OpError::NotFound(under.to_string()));
    }
    let folder_name = if under.is_empty() {
        shelbi_docs::fs::settings::load_space_settings(root)
            .map(|s| s.name)
            .unwrap_or_default()
    } else {
        dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    build_level(root, under, &folder_name)
}

fn build_level(root: &Path, under: &str, folder_name: &str) -> Result<Vec<DocsNode>, OpError> {
    let fail = |e: shelbi_docs::fs::FsError| OpError::Failed(e.to_string());
    let mut out = Vec::new();
    for folder in shelbi_docs::fs::tree::build_children(root, Some(root), under).map_err(fail)? {
        let children = build_level(root, &folder.relative_path, &folder.name)?;
        let icon = shelbi_docs::fs::folder::folder_icon(&root.join(&folder.relative_path));
        out.push(DocsNode {
            icon,
            path: folder.relative_path,
            title: folder.name.clone(),
            name: folder.name,
            kind: NodeKind::Folder,
            children,
        });
    }
    let dir = if under.is_empty() { root.to_path_buf() } else { root.join(under) };
    for doc in shelbi_docs::model::list_documents(&dir, root, folder_name).map_err(fail)? {
        let rel = doc.path.strip_prefix(root).unwrap_or(&doc.path).to_string_lossy().replace('\\', "/");
        out.push(DocsNode {
            name: doc.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            path: rel,
            title: doc.title,
            kind: NodeKind::Document,
            icon: None,
            children: Vec::new(),
        });
    }
    Ok(out)
}

fn search(root: &Path, pattern: &str, folder: Option<String>, regex: bool) -> Result<Vec<DocsMatch>, OpError> {
    use shelbi_docs::search::{Query, QueryMode};
    let query = Query {
        pattern: pattern.to_string(),
        mode: if regex { QueryMode::Regex } else { QueryMode::Substring },
        folder: folder.map(PathBuf::from),
        ..Query::default()
    };
    let found = shelbi_docs::search::search(root, &query).map_err(|e| OpError::Invalid(e.to_string()))?;
    Ok(found
        .into_iter()
        .map(|m| DocsMatch {
            path: m.relative_path.to_string_lossy().replace('\\', "/"),
            line: m.line_index + 1,
            text: m.line_text,
            start: m.byte_range.0,
            end: m.byte_range.1,
        })
        .collect())
}

/// Announces one project's docs change on `events.log` and the change bus.
struct HubSink {
    project: String,
}

impl EventSink for HubSink {
    fn changed(&self, paths: &[String], source: ChangeSource) {
        let line = changed_line(&self.project, paths, source.as_str());
        if let Err(e) = shelbi_state::append_external_event(&line) {
            tracing::warn!(project = %self.project, error = %e, "docs: couldn't log a change");
        }
        shelbi_state::publish_change(shelbi_state::ChangeNotification::Docs {
            project: self.project.clone(),
            paths: paths.to_vec(),
            source: source.as_str().to_string(),
        });
    }

    fn status(&self, status: &SyncStatus) {
        let status = status.to_string();
        let line = format!("docs_status project={} status={}", self.project, quoted(&status));
        if let Err(e) = shelbi_state::append_external_event(&line) {
            tracing::warn!(project = %self.project, error = %e, "docs: couldn't log a status");
        }
        shelbi_state::publish_change(shelbi_state::ChangeNotification::DocsStatus {
            project: self.project.clone(),
            status,
        });
    }
}

/// The `docs_changed` event line: the paths comma-separated, summarized
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
    format!("docs_changed project={project} paths={} source={source}", quoted(&shown))
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

    /// A docs worktree and conflicts file set up before the rename (at
    /// `wiki/`, `wiki-conflicts.json`) move to `docs/` and
    /// `docs-conflicts.json` with their uncommitted work, once.
    #[test]
    fn a_worktree_from_before_the_rename_moves_to_docs_with_its_work() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let gitconfig = root.join("gitconfig");
        std::fs::write(&gitconfig, "[user]\n\tname = T\n\temail = t@example.invalid\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n").unwrap();
        let env = vec![
            ("GIT_CONFIG_GLOBAL".to_string(), gitconfig.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ];
        GIT_ENV.with(|e| *e.borrow_mut() = env.clone());
        let hub = root.join("hub");
        std::fs::create_dir_all(&hub).unwrap();
        git(&env, &hub, &["init", "-q"]);
        git(&env, &hub, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let state = root.join("projects/demo");
        std::fs::create_dir_all(&state).unwrap();
        let legacy = state.join("wiki");
        git(&env, &hub, &["worktree", "add", "-q", "-b", "docs/planning", legacy.to_str().unwrap()]);
        std::fs::write(legacy.join("Plan.md"), "# Plan\n\nNot committed yet.\n").unwrap();
        std::fs::write(state.join("wiki-conflicts.json"), "[]").unwrap();

        let dir = moved_from_legacy(&state, LEGACY_WORKTREE, "docs");
        assert_eq!(dir, state.join("docs"));
        assert!(!legacy.exists());
        assert_eq!(std::fs::read_to_string(dir.join("Plan.md")).unwrap(), "# Plan\n\nNot committed yet.\n");
        assert!(git(&env, &hub, &["worktree", "list"]).contains(dir.to_str().unwrap()), "git knows where it went");
        assert_eq!(git(&env, &dir, &["branch", "--show-current"]).trim(), "docs/planning");
        let file = moved_from_legacy(&state, LEGACY_CONFLICTS_FILE, "docs-conflicts.json");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "[]");
        assert!(!state.join("wiki-conflicts.json").exists());

        // Once moved (or for a project that never had one), it's `docs/`.
        assert_eq!(moved_from_legacy(&state, LEGACY_WORKTREE, "docs"), dir);
        let fresh = root.join("projects/fresh");
        assert_eq!(moved_from_legacy(&fresh, LEGACY_WORKTREE, "docs"), fresh.join("docs"));
    }

    /// The hub path end to end: a project with no docs, `Init`, writes and
    /// reads through [`answer`], the `docs_changed` / `docs_status` lines on
    /// events.log, and a sync that lands on the (local bare) origin.
    #[test]
    fn a_project_docs_is_set_up_written_and_synced_through_the_hub() {
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

        // No `docs:` block and no worktree: no docs until asked.
        match answer("demo", DocsRequest::Status) {
            DocsReply::Status(s) => assert!(!s.set_up),
            other => panic!("{other:?}"),
        }
        match answer("demo", DocsRequest::Read { path: "README.md".into(), section: None }) {
            DocsReply::Failed(f) => assert_eq!(f.kind, FailureKind::NotSetUp),
            other => panic!("{other:?}"),
        }
        assert!(!home.join("projects/demo/docs").exists());

        match answer("demo", DocsRequest::Init) {
            DocsReply::Status(s) => {
                assert!(s.set_up);
                assert_eq!((s.source.as_str(), s.branch.as_str()), ("branch", "docs/planning"));
            }
            other => panic!("{other:?}"),
        }
        assert!(home.join("projects/demo/docs/.contextstore/settings.yml").is_file());

        let doc = "# Plan\n\n## Goals\n\nShip it.\n";
        assert_eq!(
            answer("demo", DocsRequest::Write { path: "Plans/plan.md".into(), content: doc.into(), section: None, by: Author::Agent }),
            DocsReply::Done
        );
        assert!(
            events().contains("docs_changed project=demo paths=Plans/plan.md source=agent"),
            "{}",
            events()
        );
        assert_eq!(
            answer("demo", DocsRequest::Append { path: "Plans/plan.md".into(), content: "Soon.".into(), section: Some("## Goals".into()), by: Author::Person }),
            DocsReply::Done
        );
        match answer("demo", DocsRequest::Read { path: "Plans/plan.md".into(), section: Some("## Goals".into()) }) {
            DocsReply::Doc { content, .. } => assert!(content.contains("Ship it.") && content.contains("Soon."), "{content}"),
            other => panic!("{other:?}"),
        }
        match answer("demo", DocsRequest::Tree { path: None }) {
            DocsReply::Tree(nodes) => {
                let plans = nodes.iter().find(|n| n.path == "Plans").expect("the Plans folder");
                assert_eq!(plans.kind, NodeKind::Folder);
                assert_eq!(plans.children[0].title, "Plan");
                assert_eq!(plans.children[0].path, "Plans/plan.md");
                assert!(nodes.iter().any(|n| n.path == "README.md" && n.title == "Demo"));
            }
            other => panic!("{other:?}"),
        }
        match answer("demo", DocsRequest::Search { query: "ship".into(), folder: None, regex: false }) {
            DocsReply::Matches(m) => assert_eq!((m[0].path.as_str(), m[0].line), ("Plans/plan.md", 5)),
            other => panic!("{other:?}"),
        }
        match answer("demo", DocsRequest::Read { path: "nope.md".into(), section: None }) {
            DocsReply::Failed(f) => assert_eq!(f.kind.exit_code(), 2),
            other => panic!("{other:?}"),
        }

        match answer("demo", DocsRequest::Sync(SyncOp::Now)) {
            DocsReply::Status(s) => assert_eq!(s.state, SyncState::Synced, "{s:?}"),
            other => panic!("{other:?}"),
        }
        let pushed = git(&env, &root.join("origin.git"), &["show", "docs/planning:Plans/plan.md"]);
        assert!(pushed.contains("Soon."), "{pushed}");
        assert!(events().contains("docs_status project=demo status=synced"), "{}", events());

        stop_all();
        GIT_ENV.with(|e| e.borrow_mut().clear());
    }

    struct NoSink;

    impl EventSink for NoSink {
        fn changed(&self, _: &[String], _: ChangeSource) {}
        fn status(&self, _: &SyncStatus) {}
    }

    /// A conflict settled per hunk through [`answer_with`]: the client's
    /// choices, made on the [`DocsRequest::Conflict`] it read, applied to the
    /// hub's own diff of the sides; stale choices are refused.
    #[test]
    fn a_held_document_resolves_per_hunk_through_the_hub() {
        use shelbi_app::docs::Resolution;
        use shelbi_app::docs_conflict::{diff_of, revision, Side};

        let _home = crate::test_lock::isolated_home();
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let gitconfig = root.join("gitconfig");
        std::fs::write(
            &gitconfig,
            "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n\
             [commit]\n\tgpgsign = false\n[core]\n\thooksPath = /dev/null\n",
        )
        .unwrap();
        let env = vec![
            ("GIT_CONFIG_GLOBAL".to_string(), gitconfig.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ];
        git(&env, &root, &["init", "-q", "--bare", "origin.git"]);
        git(&env, &root, &["clone", "-q", "origin.git", "hub"]);
        let hub = root.join("hub");
        std::fs::write(hub.join("main.txt"), "code\n").unwrap();
        git(&env, &hub, &["add", "-A"]);
        git(&env, &hub, &["commit", "-q", "-m", "init"]);
        git(&env, &hub, &["push", "-q", "origin", "main"]);
        let spec = ServiceSpec {
            layout: Layout::branch(&hub, root.join("docs"), "docs/planning"),
            name: "Demo".into(),
            auto: false,
            conflicts_file: root.join("conflicts.json"),
            timing: Timing::default(),
            git_env: env.clone(),
            pull_requests: None,
        };
        let (svc, _) = DocsService::start(spec, Arc::new(NoSink)).unwrap();
        let ask = |req| answer_with(&svc, DocsSource::Branch, req);
        let write = |content: &str| DocsRequest::Write {
            path: "Plan.md".into(),
            content: content.into(),
            section: None,
            by: Author::Person,
        };
        let sync = || match ask(DocsRequest::Sync(SyncOp::Now)) {
            DocsReply::Status(s) => s,
            other => panic!("{other:?}"),
        };

        let base = "# Plan\n\n## Alpha\n\nAlpha.\n\n## Beta\n\nBeta.\n";
        assert_eq!(ask(write(base)), DocsReply::Done);
        assert_eq!(sync().state, SyncState::Synced);
        // Both sections change on both sides: the merge can't combine them.
        git(&env, &root, &["clone", "-q", "-b", "docs/planning", "origin.git", "other"]);
        let other = root.join("other");
        let theirs = base.replace("Alpha.", "Alpha theirs.").replace("Beta.", "Beta theirs.");
        std::fs::write(other.join("Plan.md"), &theirs).unwrap();
        git(&env, &other, &["commit", "-qam", "elsewhere"]);
        git(&env, &other, &["push", "-q", "origin", "docs/planning"]);
        let ours = base.replace("Alpha.", "Alpha ours.").replace("Beta.", "Beta ours.");
        assert_eq!(ask(write(&ours)), DocsReply::Done);
        let status = sync();
        assert_eq!((status.state.clone(), status.conflicts.clone()), (SyncState::Conflict, vec!["Plan.md".to_string()]));

        let conflict = match ask(DocsRequest::Conflict { path: "Plan.md".into() }) {
            DocsReply::Conflict(c) => c,
            other => panic!("{other:?}"),
        };
        assert_eq!(diff_of(&conflict).hunks.len(), 2);
        let pick = Resolution::Hunks {
            choices: vec![Side::Theirs, Side::Ours],
            revision: revision(&conflict),
        };

        // An edit since the read makes the choices stale: refused, still held.
        let edited = ours.replace("## Beta", "## Beta (edited)");
        assert_eq!(ask(write(&edited)), DocsReply::Done);
        match ask(DocsRequest::Resolve { path: "Plan.md".into(), resolution: pick, by: Author::Person }) {
            DocsReply::Failed(f) => assert_eq!(f.kind, FailureKind::Conflict, "{}", f.message),
            other => panic!("{other:?}"),
        }
        assert_eq!(ask(write(&ours)), DocsReply::Done);

        let conflict = match ask(DocsRequest::Conflict { path: "Plan.md".into() }) {
            DocsReply::Conflict(c) => c,
            other => panic!("{other:?}"),
        };
        let pick = Resolution::Hunks {
            choices: vec![Side::Theirs, Side::Ours],
            revision: revision(&conflict),
        };
        assert_eq!(
            ask(DocsRequest::Resolve { path: "Plan.md".into(), resolution: pick, by: Author::Person }),
            DocsReply::Done
        );
        let status = sync();
        assert_eq!(status.state, SyncState::Synced, "{status:?}");
        assert!(status.conflicts.is_empty());
        let merged = base.replace("Alpha.", "Alpha theirs.").replace("Beta.", "Beta ours.");
        assert_eq!(git(&env, &root.join("origin.git"), &["show", "docs/planning:Plan.md"]), merged);
        assert_eq!(
            git(&env, &root.join("origin.git"), &["log", "-1", "--format=%s", "docs/planning"]).trim(),
            "Resolve merge conflicts"
        );
        match ask(DocsRequest::Resolve { path: "Plan.md".into(), resolution: Resolution::Ours, by: Author::Person }) {
            DocsReply::Failed(f) => assert_eq!(f.kind, FailureKind::Conflict),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_changed_line_lists_its_paths_and_source() {
        let line = changed_line("demo", &["Plans/a.md".into(), "README.md".into()], "agent");
        assert_eq!(line, "docs_changed project=demo paths=Plans/a.md,README.md source=agent");
        let line = changed_line("demo", &["My Plan.md".into()], "local");
        let words = shelbi_state::split_event_words(&line);
        assert_eq!(words, vec!["docs_changed", "project=demo", "paths=\"My Plan.md\"", "source=local"]);
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
    fn the_tree_carries_a_folders_emoji_icon() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("Research/.contextstore")).unwrap();
        std::fs::create_dir_all(root.join("Plans")).unwrap();
        std::fs::write(
            root.join("Research/.contextstore/settings.yml"),
            "version: 1\nproject:\n  name: Research\n  icon: \"🔬\"\n",
        )
        .unwrap();
        std::fs::write(root.join("Research/a.md"), "# A\n").unwrap();
        let nodes = tree(root, "").unwrap();
        let icon = |path: &str| nodes.iter().find(|n| n.path == path).and_then(|n| n.icon.clone());
        assert_eq!(icon("Research").as_deref(), Some("🔬"));
        assert_eq!(icon("Plans"), None);
        // An answer without icons (an older hub's) still reads.
        let old = r#"{"path":"Plans","name":"Plans","title":"Plans","kind":"Folder","children":[]}"#;
        let node: DocsNode = serde_json::from_str(old).unwrap();
        assert_eq!(node.icon, None);
    }

    #[test]
    fn a_new_document_is_titled_from_its_file_name() {
        assert_eq!(new_document("Plans/docs-sync.md", None, None), "# Docs Sync\n");
        assert_eq!(new_document("Plans/a.md", Some("Body."), Some("API")), "# API\n\nBody.\n");
    }
}
