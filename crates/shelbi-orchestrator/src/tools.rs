//! Launching the user's editor and diff tool (`editor:` / `diff_tool:`, see
//! [`shelbi_state::tools`]) on a worktree, the same way from both clients.
//!
//! A CLI tool opens as a pane: the daemon's editor / diff content session
//! runs [`diff_invocation`] (or the editor command) in the worktree. An app
//! opens in the app: a client's Edit or View Diff entry calls
//! [`crate::workspace_session::open_in_app`] /
//! [`crate::review_session::open_in_app`], which land in [`open_in_app`]
//! here and leave the client's current view as it was.
//!
//! Remote worktrees: neither client offers Edit or View Diff on a workspace
//! on another machine (the daemon spawns content sessions for local ones
//! only), and an app can't open a worktree it can't see, so an app launch on
//! a remote worktree is refused with that reason.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use shelbi_core::{shell_escape, Error, Project, Result};
use shelbi_state::tools::{DiffLaunch, EditorLaunch};
use shelbi_state::{AppTools, ToolChoice, UserConfig};

use crate::review_session::ReviewContentRole;

/// How long an app launch is watched for an early failure (an app that
/// isn't there, a helper that won't start) before it's left to run.
const EARLY_EXIT: Duration = Duration::from_millis(1500);

/// Which of a project's Edit / View Diff entries open in an app, from
/// `cfg`: the editor when `editor:` is an app, the diff when `diff_tool:`
/// is one and the project's `review.diff_command` doesn't take precedence.
pub fn app_tools_for(cfg: &UserConfig, project_diff_command: bool) -> AppTools {
    let is_app = |c: &Option<ToolChoice>| c.clone().and_then(ToolChoice::normalized).is_some_and(|c| c.is_app());
    AppTools {
        editor: is_app(&cfg.editor),
        diff: !project_diff_command && is_app(&cfg.diff_tool),
    }
}

/// [`app_tools_for`] the user config as it is on disk.
pub fn app_tools(project: &Project) -> AppTools {
    let cfg = shelbi_state::load_user_config().unwrap_or_default();
    app_tools_for(&cfg, project.review_diff_command().is_some())
}

/// The diff a pane runs when the project has no `review.diff_command`:
/// `diff_tool:` when it is a command, git's configured difftool when it is
/// unset or an app (a pane can't show an app).
pub fn pane_diff_launch(diff_tool: Option<&ToolChoice>, path_dirs: &[std::path::PathBuf]) -> Result<DiffLaunch> {
    match diff_tool {
        Some(choice) if !choice.is_app() => shelbi_state::tools::diff_launch(Some(choice), path_dirs).map_err(Error::Other),
        _ => Ok(DiffLaunch::GitConfigured),
    }
}

/// The shell command that runs `launch` over `merge-base..HEAD` (`base` is
/// the merge-base) in `worktree`, the caller already `cd`'d there:
///
/// * git's own difftool: `git difftool -d -y [-g] <base> HEAD` (`-g` when
///   `use_gui`, the `diff.guitool` pick),
/// * a difftool command: `git -c difftool.shelbi.cmd=<cmd> difftool [-d] -y
///   --tool=shelbi <base> HEAD`,
/// * a pager tool: `git -c diff.external=<cmd> diff --ext-diff <base> HEAD`
///   through `less -R` (no `F`, so a short diff doesn't close the pane at
///   once),
/// * a worktree tool: `<cmd> <worktree>`.
pub fn diff_invocation(launch: &DiffLaunch, worktree: &Path, base: &str, use_gui: bool) -> String {
    let base = shell_escape(base);
    match launch {
        DiffLaunch::GitConfigured => {
            let gui = if use_gui { "-g " } else { "" };
            format!("git difftool -d -y {gui}{base} HEAD")
        }
        DiffLaunch::Difftool { cmd, dir_diff } => {
            let dir = if *dir_diff { "-d " } else { "" };
            format!(
                "git -c difftool.shelbi.cmd={} difftool {dir}-y --tool=shelbi {base} HEAD",
                shell_escape(cmd)
            )
        }
        DiffLaunch::Pager { cmd } => format!(
            "LESS=R GIT_PAGER='less -R' git -c diff.external={} diff --ext-diff {base} HEAD",
            shell_escape(cmd)
        ),
        DiffLaunch::Worktree { cmd } => format!("{cmd} {}", shell_escape(&worktree.to_string_lossy())),
    }
}

/// The argv that opens `role`'s app on `worktree`: the editor app with
/// `open -a <app> <worktree>`, the diff app through `sh -c` (its
/// [`diff_invocation`] over `base`). `Err` when the setting isn't an app, or
/// the diff app's helper isn't installed.
pub fn app_launch_argv(
    role: ReviewContentRole,
    cfg: &UserConfig,
    worktree: &Path,
    base: impl FnOnce() -> Result<String>,
    path_dirs: &[std::path::PathBuf],
) -> Result<Vec<String>> {
    let wt = worktree.to_string_lossy().to_string();
    match role {
        ReviewContentRole::Editor => match shelbi_state::tools::editor_launch(cfg.editor.as_ref(), "") {
            EditorLaunch::App(app) => Ok(vec!["open".into(), "-a".into(), app.to_string_lossy().into(), wt]),
            EditorLaunch::Pane(_) => Err(Error::Other("the editor setting isn't an app".into())),
        },
        ReviewContentRole::Diff => {
            let choice = cfg
                .diff_tool
                .clone()
                .and_then(ToolChoice::normalized)
                .filter(ToolChoice::is_app)
                .ok_or_else(|| Error::Other("the diff tool setting isn't an app".into()))?;
            let launch = shelbi_state::tools::diff_launch(Some(&choice), path_dirs).map_err(Error::Other)?;
            let invocation = diff_invocation(&launch, worktree, &base()?, false);
            Ok(vec![
                "sh".into(),
                "-c".into(),
                format!("cd {} && {invocation}", shell_escape(&wt)),
            ])
        }
    }
}

/// The name of `role`'s tool in `cfg`, for a status line or a refusal.
pub fn tool_name(role: ReviewContentRole, cfg: &UserConfig) -> String {
    let choice = match role {
        ReviewContentRole::Editor => cfg.editor.as_ref(),
        ReviewContentRole::Diff => cfg.diff_tool.as_ref(),
    };
    choice.map(ToolChoice::display_name).unwrap_or_else(|| "The app".into())
}

/// Why an app can't open a worktree on `machine`.
pub fn remote_refusal(role: ReviewContentRole, cfg: &UserConfig, machine: &str) -> String {
    format!(
        "{} can't open a worktree on `{machine}`: apps open local worktrees only",
        tool_name(role, cfg)
    )
}

/// Open `role`'s app on the local `worktree` (`base_branch` is what the diff
/// compares against) and return the status line to show. Blocking for up to
/// [`EARLY_EXIT`] (it watches the launch for an early failure), so call it
/// off the UI thread. The launch runs with the login shell's environment,
/// detached from the caller: a diff app's `git difftool` keeps running until
/// the comparison is closed.
pub fn open_in_app(role: ReviewContentRole, worktree: &Path, base_branch: &str) -> Result<String> {
    let cfg = shelbi_state::load_user_config()?;
    let path_dirs = shelbi_state::tools::login_path_dirs();
    let argv = app_launch_argv(
        role,
        &cfg,
        worktree,
        || crate::review_ui::merge_base(worktree, base_branch),
        &path_dirs,
    )?;
    spawn_watched(&argv, worktree)?;
    let name = tool_name(role, &cfg);
    Ok(match role {
        ReviewContentRole::Editor => format!("Opened the worktree in {name}"),
        ReviewContentRole::Diff => format!("Opened the diff in {name}"),
    })
}

/// Spawn `argv` in `cwd` with the login environment, and wait up to
/// [`EARLY_EXIT`] for it to fail: a non-zero exit in that window is an
/// `Err` carrying its stderr. A launch still running then (a diff waiting
/// on its app) is left running, reaped by a thread when it exits.
fn spawn_watched(argv: &[String], cwd: &Path) -> Result<()> {
    let (program, args) = argv.split_first().ok_or_else(|| Error::Other("nothing to launch".into()))?;
    let mut child = std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(shelbi_core::login_env::login_shell_env())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(format!("couldn't start `{program}`: {e}")))?;
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut err = String::new();
        if let Some(mut s) = stderr {
            let _ = s.read_to_string(&mut err);
        }
        let status = child.wait();
        let _ = tx.send((status, err));
    });
    match rx.recv_timeout(EARLY_EXIT) {
        Ok((Ok(status), err)) if !status.success() => {
            let err = err.trim();
            Err(Error::Other(if err.is_empty() {
                format!("`{program}` exited with {status}")
            } else {
                err.lines().last().unwrap_or(err).to_string()
            }))
        }
        Ok((Err(e), _)) => Err(Error::Other(format!("`{program}` failed: {e}"))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg(editor: Option<ToolChoice>, diff_tool: Option<ToolChoice>) -> UserConfig {
        UserConfig {
            editor,
            diff_tool,
            ..UserConfig::default()
        }
    }

    #[test]
    fn app_tools_follow_the_settings_and_the_project_override_wins() {
        let zed = Some(ToolChoice::app("/Applications/Zed.app"));
        let ks = Some(ToolChoice::app("/Applications/Kaleidoscope.app"));
        assert_eq!(app_tools_for(&cfg(None, None), false), AppTools::default());
        assert_eq!(app_tools_for(&cfg(Some("hx".into()), Some("difft".into())), false), AppTools::default());
        assert_eq!(app_tools_for(&cfg(zed.clone(), ks.clone()), false), AppTools { editor: true, diff: true });
        assert_eq!(
            app_tools_for(&cfg(zed, ks), true),
            AppTools { editor: true, diff: false },
            "review.diff_command takes precedence over diff_tool"
        );
    }

    #[test]
    fn a_pane_runs_a_diff_command_and_falls_back_to_git_for_an_app() {
        assert_eq!(pane_diff_launch(None, &[]).unwrap(), DiffLaunch::GitConfigured);
        assert_eq!(
            pane_diff_launch(Some(&ToolChoice::app("/Applications/Kaleidoscope.app")), &[]).unwrap(),
            DiffLaunch::GitConfigured
        );
        assert_eq!(
            pane_diff_launch(Some(&"vimdiff".into()), &[]).unwrap(),
            DiffLaunch::Difftool { cmd: "vimdiff \"$LOCAL\" \"$REMOTE\"".into(), dir_diff: false }
        );
    }

    #[test]
    fn diff_invocations_drive_git() {
        let wt = Path::new("/w/t");
        assert_eq!(diff_invocation(&DiffLaunch::GitConfigured, wt, "abc", false), "git difftool -d -y abc HEAD");
        assert_eq!(diff_invocation(&DiffLaunch::GitConfigured, wt, "abc", true), "git difftool -d -y -g abc HEAD");
        assert_eq!(
            diff_invocation(&DiffLaunch::Difftool { cmd: "nvim -d \"$LOCAL\" \"$REMOTE\"".into(), dir_diff: false }, wt, "abc", true),
            "git -c difftool.shelbi.cmd='nvim -d \"$LOCAL\" \"$REMOTE\"' difftool -y --tool=shelbi abc HEAD"
        );
        assert_eq!(
            diff_invocation(&DiffLaunch::Difftool { cmd: "ksdiff --wait \"$LOCAL\" \"$REMOTE\"".into(), dir_diff: true }, wt, "abc", false),
            "git -c difftool.shelbi.cmd='ksdiff --wait \"$LOCAL\" \"$REMOTE\"' difftool -d -y --tool=shelbi abc HEAD"
        );
        assert_eq!(
            diff_invocation(&DiffLaunch::Pager { cmd: "difft --color=always".into() }, wt, "abc", false),
            "LESS=R GIT_PAGER='less -R' git -c diff.external='difft --color=always' diff --ext-diff abc HEAD"
        );
        assert_eq!(
            diff_invocation(&DiffLaunch::Worktree { cmd: "smerge".into() }, Path::new("/my wt"), "abc", false),
            "smerge '/my wt'"
        );
    }

    #[test]
    fn an_app_launch_opens_the_worktree_or_the_diff() {
        let wt = Path::new("/w/my wt");
        let zed = cfg(Some(ToolChoice::app("/Applications/Zed.app")), None);
        let no_base = || -> Result<String> { panic!("the editor needs no base") };
        assert_eq!(
            app_launch_argv(ReviewContentRole::Editor, &zed, wt, no_base, &[]).unwrap(),
            ["open", "-a", "/Applications/Zed.app", "/w/my wt"]
        );
        assert!(app_launch_argv(ReviewContentRole::Editor, &cfg(Some("hx".into()), None), wt, no_base, &[]).is_err());
        assert!(app_launch_argv(ReviewContentRole::Diff, &zed, wt, || Ok("abc".into()), &[]).is_err());

        // A diff app the catalog doesn't know needs no helper.
        let other = cfg(None, Some(ToolChoice::app("/Applications/Diffy.app")));
        let argv = app_launch_argv(ReviewContentRole::Diff, &other, wt, || Ok("abc".into()), &[]).unwrap();
        assert_eq!(&argv[..2], ["sh", "-c"]);
        assert_eq!(
            argv[2],
            "cd '/w/my wt' && git -c difftool.shelbi.cmd='open -W -a /Applications/Diffy.app \"$LOCAL\" \"$REMOTE\"' difftool -d -y --tool=shelbi abc HEAD"
        );
        // A known one whose helper is missing says so.
        let ks = cfg(None, Some(ToolChoice::app("/nowhere/Kaleidoscope.app")));
        let err = app_launch_argv(ReviewContentRole::Diff, &ks, wt, || Ok("abc".into()), &[PathBuf::from("/nowhere")]).unwrap_err();
        assert!(err.to_string().contains("ksdiff"), "{err}");
    }

    #[test]
    fn a_remote_refusal_names_the_app_and_the_machine() {
        let c = cfg(Some(ToolChoice::app("/Applications/Zed.app")), None);
        assert_eq!(
            remote_refusal(ReviewContentRole::Editor, &c, "gpu"),
            "Zed can't open a worktree on `gpu`: apps open local worktrees only"
        );
    }

    #[test]
    fn a_launch_that_fails_at_once_reports_its_stderr() {
        let cwd = std::env::temp_dir();
        let err = spawn_watched(&["sh".into(), "-c".into(), "echo 'no such app' >&2; exit 1".into()], &cwd).unwrap_err();
        assert_eq!(err.to_string(), "no such app");
        assert!(spawn_watched(&["sh".into(), "-c".into(), "exit 0".into()], &cwd).is_ok());
        assert!(spawn_watched(&["sh".into(), "-c".into(), "sleep 3".into()], &cwd).is_ok(), "a running launch is left running");
    }
}
