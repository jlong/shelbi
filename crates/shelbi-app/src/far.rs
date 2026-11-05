//! The app's requests that run where a project lives (remote-window P8).
//!
//! A window on a project on another machine reads that project through its
//! source and saves its config with `SaveConfig`. The rest of Settings'
//! actions, and Edit, can't run on this Mac at all:
//!
//! - **Workspace and machine changes** (`shelbi_orchestrator::pool_admin`)
//!   provision worktrees and check sessions on the project's machines, over
//!   the far hub's routes and keys.
//! - **Machine checks** (`shelbi_orchestrator::machine_readiness`) run SSH
//!   from the far hub to the machine, and record what they verified in the
//!   far hub's state.
//! - **Edit** opens a far file: in a terminal session on the far machine
//!   running the far `$EDITOR` ([`FarFile`]), which the window shows in a
//!   window of its own. Far paths are never opened as local files.
//!
//! Each is one [`Request`] in a control `AppRequest` to the far daemon,
//! answered with one [`Reply`] (`shelbi_orchestrator::far::answer` is the
//! far side). [`crate::source::ProjectLink`] sends them, and builds the
//! Settings forms' services over them ([`pool_service`], [`machine_service`],
//! [`readiness_service`]).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use shelbi_core::pool::{
    AddWorkspace, MachineChange, MachineFields, PoolError, RemoveWorkspace, UpdateMachine, UpdateWorkspace,
    WorkspaceChange, WorkspaceRemoved,
};

use crate::exec::EditTarget;
use crate::settings::machine_check::{CheckOutcome, ReadinessRequest, ReadinessService, Record, Report};
use crate::settings::machine_form::MachineService;
use crate::settings::workspace_form::PoolService;
use crate::source::ProjectLink;

/// The file an editor session opens, on the far machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FarFile {
    /// An `edit:*` command's target, resolved there
    /// (`command_model::edit_target_path`).
    Target(EditTarget),
    /// A path on the far machine: a Settings file (a workflow's YAML, an
    /// agent's), or one Cmd+clicked in a far terminal. The first of
    /// `candidates` that exists there opens, at `line` and `column` when the
    /// editor takes them.
    Path {
        candidates: Vec<String>,
        #[serde(default)]
        line: Option<u32>,
        #[serde(default)]
        column: Option<u32>,
    },
    /// The project's `statuses.yaml`, written from the shipped catalog first
    /// when the project has none yet.
    Statuses,
}

impl FarFile {
    /// One far path.
    pub fn path(path: impl Into<String>) -> FarFile {
        FarFile::Path {
            candidates: vec![path.into()],
            line: None,
            column: None,
        }
    }
}

/// One request the far daemon runs for a window on its project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Request {
    AddWorkspace(AddWorkspace),
    UpdateWorkspace(UpdateWorkspace),
    RemoveWorkspace(RemoveWorkspace),
    AddMachine(MachineFields),
    UpdateMachine(UpdateMachine),
    /// Run a readiness check (or Install CLI) from the far hub. Nothing is
    /// recorded: the window records a ready answer with
    /// [`Request::RecordMachine`] unless the check was stopped meanwhile.
    CheckMachine(ReadinessRequest),
    /// Record a verified binary in the far hub's state.
    RecordMachine(Record),
    /// Start an editor session on `file`, named [`editor_session_name`] with
    /// `key`.
    OpenEditor { key: String, file: FarFile },
    /// A read or write for the diff view (`crate::diff`): its changeset,
    /// blame, comments and viewed marks live on the project's machine.
    Diff(crate::diff::DiffRequest),
}

/// The far daemon's answer to a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Workspace(Result<WorkspaceChange, PoolError>),
    Removed(Result<WorkspaceRemoved, PoolError>),
    Machine(Result<MachineChange, PoolError>),
    Checked(CheckOutcome),
    Recorded,
    /// The editor session's name, or why it couldn't start.
    Editor(Result<String, String>),
    Diff(crate::diff::DiffReply),
}

/// The far paths a path Cmd+clicked in a far terminal may name: as
/// written when absolute or `~/…` (the far machine expands it), else under
/// the session's working directory `cwd` (a far path), with a diff's
/// `a/` / `b/` prefix dropped too. The far side opens the first that
/// exists.
pub fn link_candidates(path: &str, cwd: Option<&std::path::Path>) -> Vec<String> {
    if path.starts_with("~/") {
        return vec![path.to_string()];
    }
    if path.starts_with('/') {
        return vec![path.to_string()];
    }
    let Some(cwd) = cwd else {
        return Vec::new();
    };
    let mut out = vec![cwd.join(path).to_string_lossy().into_owned()];
    if let Some(rest) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        out.push(cwd.join(rest).to_string_lossy().into_owned());
    }
    out
}

/// The largest file a drop uploads (the relay `put`'s cap).
pub const MAX_DROP: u64 = shelbi_client::relay::services::MAX_PUT_LEN as u64;

/// Upload files dropped on a far terminal to its machine over `link` (the
/// relay's `put`: each into a fresh directory under the far user's temp
/// directory) and answer their far paths, in order, for the pane to paste.
/// A folder, a file past [`MAX_DROP`], or one that can't be read or sent
/// refuses the drop with a sentence saying so. Blocking.
pub fn upload(link: &ProjectLink, paths: &[std::path::PathBuf]) -> Result<Vec<std::path::PathBuf>, String> {
    let host = crate::remote_window::bare_host(link.key().host().unwrap_or("the far machine")).to_string();
    let mut far = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "drop".into());
        let meta = std::fs::metadata(path).map_err(|e| format!("Couldn't read {name}: {e}."))?;
        if meta.is_dir() {
            return Err(format!("{name} is a folder: only files can be dropped on {host}'s terminals."));
        }
        if meta.len() > MAX_DROP {
            return Err(format!(
                "{name} is too big to send to {host} (the limit is {} MB).",
                MAX_DROP / (1024 * 1024)
            ));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read {name}: {e}."))?;
        let sent = link
            .put_file(&name, &bytes)
            .map_err(|e| format!("Couldn't send {name} to {host}: {e}."))?;
        far.push(std::path::PathBuf::from(sent));
    }
    Ok(far)
}

/// The session name an editor session started with `key` has.
pub fn editor_session_name(project: &str, key: &str) -> String {
    format!("{project}/edit/{key}")
}

/// Whether `key` may name an editor session: short, lowercase letters,
/// digits and `-`.
pub fn valid_editor_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 32 && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A fresh key for an editor session.
pub fn fresh_editor_key() -> String {
    shelbi_client::fresh_client_id().to_ascii_lowercase()
}

/// Why a reply isn't the one the request asks for.
fn unexpected(reply: &Reply) -> String {
    format!("the far daemon answered something else ({reply:?})")
}

/// The workspace form's operations, run by the far daemon over `link`.
pub fn pool_service(link: &ProjectLink) -> PoolService {
    let (a, u, r) = (link.clone(), link.clone(), link.clone());
    PoolService {
        add: Arc::new(move |_, req| match a.far_request(&Request::AddWorkspace(req.clone())) {
            Ok(Reply::Workspace(result)) => result,
            Ok(other) => Err(PoolError::Provisioning(unexpected(&other))),
            Err(e) => Err(PoolError::Provisioning(e)),
        }),
        update: Arc::new(move |_, req| match u.far_request(&Request::UpdateWorkspace(req.clone())) {
            Ok(Reply::Workspace(result)) => result,
            Ok(other) => Err(PoolError::Provisioning(unexpected(&other))),
            Err(e) => Err(PoolError::Provisioning(e)),
        }),
        remove: Arc::new(move |_, req| match r.far_request(&Request::RemoveWorkspace(req.clone())) {
            Ok(Reply::Removed(result)) => result,
            Ok(other) => Err(PoolError::Provisioning(unexpected(&other))),
            Err(e) => Err(PoolError::Provisioning(e)),
        }),
    }
}

/// The machine form's operations, run by the far daemon over `link`.
pub fn machine_service(link: &ProjectLink) -> MachineService {
    let (a, u) = (link.clone(), link.clone());
    MachineService {
        add: Arc::new(move |_, fields| match a.far_request(&Request::AddMachine(fields.clone())) {
            Ok(Reply::Machine(result)) => result,
            Ok(other) => Err(PoolError::Persistence(unexpected(&other))),
            Err(e) => Err(PoolError::Persistence(e)),
        }),
        update: Arc::new(move |_, req| match u.far_request(&Request::UpdateMachine(req.clone())) {
            Ok(Reply::Machine(result)) => result,
            Ok(other) => Err(PoolError::Persistence(unexpected(&other))),
            Err(e) => Err(PoolError::Persistence(e)),
        }),
    }
}

/// How often a far check's waiter looks at its stop flag.
const STOP_POLL: Duration = Duration::from_millis(100);

/// The machine forms' readiness check and Install CLI, run from the far hub
/// over `link`. A check stopped meanwhile answers [`Report::cancelled`] at
/// once (the far one runs out on its own deadline, and its answer is
/// dropped); a ready one is recorded far side, unless it was stopped
/// ([`ReadinessService::run`]).
pub fn readiness_service(link: &ProjectLink) -> ReadinessService {
    let (c, r) = (link.clone(), link.clone());
    ReadinessService {
        check: Arc::new(move |request, cancel: Arc<AtomicBool>| {
            let (tx, rx) = std::sync::mpsc::channel();
            let link = c.clone();
            let request = request.clone();
            std::thread::spawn(move || {
                let _ = tx.send(link.far_request(&Request::CheckMachine(request)));
            });
            loop {
                if cancel.load(Ordering::Acquire) {
                    return CheckOutcome::Checked(Report::cancelled());
                }
                match rx.recv_timeout(STOP_POLL) {
                    Ok(Ok(Reply::Checked(outcome))) => return outcome,
                    Ok(Ok(other)) => return far_check_failed(unexpected(&other)),
                    Ok(Err(e)) => return far_check_failed(e),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return far_check_failed("the check stopped without an answer".into())
                    }
                }
            }
        }),
        record: Arc::new(move |record| {
            if let Err(e) = r.far_request(&Request::RecordMachine(record.clone())) {
                tracing::warn!(machine = %record.machine, error = %e, "couldn't record the far machine's check");
            }
        }),
    }
}

/// A far check that never ran: the far hub couldn't be asked.
fn far_check_failed(why: String) -> CheckOutcome {
    use crate::settings::machine_check::Reach;
    CheckOutcome::Checked(Report::unreached(
        Reach::Unreachable,
        "Not checked",
        format!("Couldn't ask the project's hub to check the machine: {why}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_replies_cross_the_wire() {
        let requests = [
            Request::AddMachine(MachineFields {
                name: "Build".into(),
                kind: shelbi_core::MachineKind::Ssh,
                host: Some("build".into()),
                work_dir: "/srv/p".into(),
                tags: vec!["gpu".into()],
            }),
            Request::OpenEditor {
                key: "ab12".into(),
                file: FarFile::Target(EditTarget::Agent("developer".into())),
            },
            Request::OpenEditor {
                key: "ab13".into(),
                file: FarFile::Path {
                    candidates: vec!["/srv/p/src/main.rs".into()],
                    line: Some(4),
                    column: None,
                },
            },
        ];
        for r in requests {
            let json = serde_json::to_value(&r).unwrap();
            assert_eq!(serde_json::from_value::<Request>(json).unwrap(), r);
        }
        let replies = [
            Reply::Machine(Err(PoolError::Conflict("changed".into()))),
            Reply::Checked(CheckOutcome::Checked(Report::cancelled())),
            Reply::Editor(Ok("p/edit/ab12".into())),
            Reply::Recorded,
        ];
        for r in replies {
            let json = serde_json::to_value(&r).unwrap();
            assert_eq!(serde_json::from_value::<Reply>(json).unwrap(), r);
        }
    }

    #[test]
    fn a_clicked_far_path_resolves_on_the_far_side() {
        let cwd = std::path::Path::new("/srv/p");
        assert_eq!(link_candidates("~/notes.md", Some(cwd)), ["~/notes.md"]);
        assert_eq!(link_candidates("/etc/hosts", Some(cwd)), ["/etc/hosts"]);
        assert_eq!(link_candidates("b/src/main.rs", Some(cwd)), ["/srv/p/b/src/main.rs", "/srv/p/src/main.rs"]);
        assert!(link_candidates("src/main.rs", None).is_empty());
    }

    #[test]
    fn editor_keys_are_plain_and_name_their_session() {
        let key = fresh_editor_key();
        assert!(valid_editor_key(&key), "{key}");
        assert!(!valid_editor_key("../x"));
        assert!(!valid_editor_key(""));
        assert_eq!(editor_session_name("shelbi", "ab12"), "shelbi/edit/ab12");
    }
}
