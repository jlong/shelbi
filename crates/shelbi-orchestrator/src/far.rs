//! The far side of a remote window's app requests ([`shelbi_app::far`]):
//! what this machine's daemon runs for a window on one of its projects
//! open on another machine, from a control `AppRequest`.
//!
//! - Workspace and machine changes run [`crate::pool_admin`], as Settings
//!   does on this machine: the same locks, checks and provisioning, over
//!   this hub's routes and keys.
//! - A machine check runs [`crate::machine_readiness`] from this hub,
//!   without recording; the window records a ready answer it still wants
//!   ([`Request::RecordMachine`]), so a check it stopped never records.
//! - An editor session runs the user's `$EDITOR` (or the hub config's
//!   `editor:` command) on the file in a terminal session here, named
//!   `<project>/edit/<key>`, which the window attaches to over its relay.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use shelbi_app::far::{editor_session_name, valid_editor_key, FarFile, Reply, Request};
use shelbi_app::settings::machine_check::ReadinessService;
use shelbi_app::settings::machine_form::MachineService;
use shelbi_app::settings::workspace_form::PoolService;

/// Starts editor session `name` in `cwd` running `argv`.
pub type SpawnEditor = dyn Fn(&str, &Path, Vec<String>) -> Result<(), String> + Send + Sync;

/// What the far side runs requests with: the live services, or a test's.
#[derive(Clone)]
pub struct FarServices {
    pub pool: PoolService,
    pub machines: MachineService,
    pub readiness: ReadinessService,
    pub spawn_editor: Arc<SpawnEditor>,
    /// How a diff view's `gY` reaches an agent's session here.
    pub agent_send: shelbi_app::diff::AgentSend,
}

impl FarServices {
    /// This machine's live services.
    pub fn live() -> FarServices {
        FarServices {
            pool: crate::pool_admin::workspace_service(),
            machines: crate::pool_admin::machine_service(),
            readiness: crate::machine_readiness::service(),
            spawn_editor: Arc::new(spawn_editor),
            agent_send: crate::agent_send::service(),
        }
    }
}

/// Run `request` for `project` with this machine's live services.
pub fn answer(project: &str, request: Request) -> Reply {
    answer_with(&FarServices::live(), project, request)
}

/// Run `request` for `project` with `services`. Blocking.
pub fn answer_with(services: &FarServices, project: &str, request: Request) -> Reply {
    match request {
        Request::AddWorkspace(req) => Reply::Workspace((services.pool.add)(project, &req)),
        Request::UpdateWorkspace(req) => Reply::Workspace((services.pool.update)(project, &req)),
        Request::RemoveWorkspace(req) => Reply::Removed((services.pool.remove)(project, &req)),
        Request::AddMachine(fields) => Reply::Machine((services.machines.add)(project, &fields)),
        Request::UpdateMachine(req) => Reply::Machine((services.machines.update)(project, &req)),
        Request::CheckMachine(req) => {
            // The window's stop flag doesn't reach here: the check runs out
            // on its own deadline, and the window drops a stopped answer.
            Reply::Checked((services.readiness.check)(&req, Arc::new(AtomicBool::new(false))))
        }
        Request::RecordMachine(record) => {
            (services.readiness.record)(&record);
            Reply::Recorded
        }
        Request::OpenEditor { key, file } => Reply::Editor(open_editor(services, project, &key, &file)),
        // The request's project is the connection's: a window asks about the
        // project it's on.
        Request::Diff(mut req) => {
            req.target.project = project.to_string();
            Reply::Diff(shelbi_app::diff::answer(&req, services.agent_send.clone()))
        }
    }
}

/// Start the editor session for `file`, answering its name.
fn open_editor(services: &FarServices, project: &str, key: &str, file: &FarFile) -> Result<String, String> {
    if !valid_editor_key(key) {
        return Err(format!("`{key}` can't name an editor session"));
    }
    let (path, line) = resolve(project, file)?;
    let name = editor_session_name(project, key);
    let cwd = if path.is_dir() {
        path.clone()
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"))
    };
    let configured = shelbi_state::load_user_config()
        .ok()
        .and_then(|c| c.editor)
        .and_then(|e| e.command().map(str::to_string))
        .unwrap_or_default();
    (services.spawn_editor)(&name, &cwd, editor_argv(&path, line, &configured))?;
    Ok(name)
}

/// The far path `file` names and the line to open it at.
fn resolve(project: &str, file: &FarFile) -> Result<(PathBuf, Option<u32>), String> {
    match file {
        FarFile::Target(target) => shelbi_app::command_model::edit_target_path(project, target)
            .filter(|p| p.exists())
            .map(|p| (p, None))
            .ok_or_else(|| format!("project `{project}` has no {target:?} file to edit")),
        FarFile::Path { candidates, line, .. } => candidates
            .iter()
            .map(|c| shelbi_state::expand_tilde_str(c))
            .find(|p| p.is_absolute() && p.exists())
            .map(|p| (p, *line))
            .ok_or_else(|| format!("no such file: {}", candidates.first().map(String::as_str).unwrap_or("?"))),
        FarFile::Statuses => {
            // `statuses.yaml`, or a legacy `statuses.yml`; the shipped
            // catalog written first when there's neither.
            let yaml = shelbi_state::statuses_path(project).map_err(|e| e.to_string())?;
            let yml = yaml.with_extension("yml");
            if !yaml.exists() && !yml.exists() {
                shelbi_state::scaffold_project_statuses(project)
                    .map_err(|e| format!("Couldn't write statuses.yaml: {e}"))?;
            }
            Ok((if yml.exists() && !yaml.exists() { yml } else { yaml }, None))
        }
    }
}

/// The editor session's command: the user's login shell running the
/// configured editor command (`configured`, the hub config's `editor:`),
/// else `$EDITOR` as the login shell sets it, else vim; at `line` in the
/// syntax the common terminal editors take.
pub fn editor_argv(path: &Path, line: Option<u32>, configured: &str) -> Vec<String> {
    const SCRIPT: &str = r#"e=${3:-${EDITOR:-vim}}; b=${e%% *}; b=${b##*/}; case "$b" in vi|vim|nvim|nano|emacs|emacsclient|micro|kak|joe|ne|mg) exec $e ${2:+"+$2"} "$1";; hx|helix) exec $e "$1${2:+:$2}";; *) exec $e "$1";; esac"#;
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    vec![
        shell,
        "-lc".into(),
        SCRIPT.into(),
        "shelbi-edit".into(),
        path.to_string_lossy().into_owned(),
        line.map(|l| l.to_string()).unwrap_or_default(),
        configured.to_string(),
    ]
}

/// Spawn editor session `name` on this machine.
fn spawn_editor(name: &str, cwd: &Path, argv: Vec<String>) -> Result<(), String> {
    let (cols, rows) = crate::session_process_backend::SessionProcessBackend::default_size();
    crate::session_process_backend::SessionProcessBackend
        .spawn_session(shelbi_session::SpawnSpec {
            name: name.to_string(),
            cwd: cwd.to_path_buf(),
            cols,
            rows,
            task: None,
            raw_output_log: false,
            child_argv: argv,
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_runs_in_a_login_shell_with_its_line() {
        let argv = editor_argv(Path::new("/srv/p/a b.rs"), Some(12), "");
        assert_eq!(argv[1], "-lc");
        assert_eq!(&argv[3..], ["shelbi-edit", "/srv/p/a b.rs", "12", ""]);

        // The script picks each editor's line syntax.
        let run = |editor: &str, line: &str| {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(argv[2].replace("exec $e", "echo $e"))
                .args(["shelbi-edit", "/f", line, editor])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        assert_eq!(run("nvim", "3"), "nvim +3 /f");
        assert_eq!(run("/usr/bin/hx", "3"), "/usr/bin/hx /f:3");
        assert_eq!(run("code --wait", "3"), "code --wait /f");
        assert_eq!(run("vim", ""), "vim /f");
    }

    #[test]
    fn an_editor_key_must_be_plain() {
        let services = FarServices {
            pool: crate::pool_admin::workspace_service(),
            machines: crate::pool_admin::machine_service(),
            readiness: crate::machine_readiness::service(),
            spawn_editor: Arc::new(|_, _, _| panic!("never spawned")),
            agent_send: crate::agent_send::service(),
        };
        let reply = answer_with(
            &services,
            "p",
            Request::OpenEditor {
                key: "../x".into(),
                file: FarFile::path("/etc/hosts"),
            },
        );
        assert!(matches!(reply, Reply::Editor(Err(e)) if e.contains("can't name")));
    }
}
