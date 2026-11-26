//! macOS: Homebrew, the `.app` bundle, `open`, Terminal.app.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The well-known install directories, in search order: Homebrew on Apple
/// silicon, then Intel, `cargo install`, and `~/bin`, where the app's own
/// install and `scripts/install.sh` put it.
pub fn known_cli_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(home) = home {
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join("bin"));
    }
    dirs
}

/// With no CLI anywhere, the app installs the release matching it
/// (`platform::install`): the zip of the app alone has none.
pub const INSTALLS_CLI: bool = true;

/// Where the CLI is looked for besides the login `PATH`, as Settings'
/// help line names them.
pub const KNOWN_CLI_PLACES: &str = "in Homebrew, ~/.cargo/bin, or ~/bin";
/// The words before [`INSTALL_COMMAND`].
pub const INSTALL_LABEL: &str = "Install it with Homebrew:";
/// The same, offered after another fix.
pub const OR_INSTALL_LABEL: &str = "Or install it with Homebrew:";
/// The CLI's install command.
pub const INSTALL_COMMAND: &str = "brew install jlong/shelbi/shelbi";
/// The CLI's upgrade command.
pub const UPGRADE_COMMAND: &str = "brew upgrade shelbi";
/// The app's own upgrade command (its cask, which upgrades the formula it
/// depends on).
pub const APP_UPGRADE_COMMAND: &str = "brew upgrade --cask shelbi-desktop";

/// Where Settings' Browse… starts: the apps.
pub const BROWSE_START: &str = "/Applications";

/// Where the save panel for a download starts: `~/Downloads`.
pub fn downloads_dir() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from).join("Downloads")
}

/// Open a URL, file or folder with its default app (a folder in Finder).
pub fn open(target: &str) -> Result<(), String> {
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg(target);
    run_opener(cmd, target)
}

/// Open `path` in the default text editor.
pub fn open_text(path: &Path) -> Result<(), String> {
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-t").arg(path);
    run_opener(cmd, &format!("-t {}", path.display()))
}

/// Open `file` in the app at `app` (an `.app`).
pub fn open_with_app(app: &Path, file: &Path) -> Result<(), String> {
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-a").arg(app).arg(file);
    run_opener(cmd, &format!("-a {} {}", app.display(), file.display()))
}

/// Run the shell command line `command` in a new Terminal.app window.
pub fn run_in_terminal(command: &str) -> Result<(), String> {
    let script = format!(
        "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
        double_quoted(command)
    );
    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status()
        .map_err(|e| format!("launching Terminal for `{command}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Terminal couldn't start `{command}` ({status})"))
    }
}

/// The command that starts a fresh instance of the app running as `exe`:
/// from the `.app` bundle, `open -n` on the bundle on disk (which
/// `brew upgrade` has replaced); an unbundled `cargo run` binary re-runs
/// itself.
pub fn relaunch_command(exe: &Path) -> Command {
    match crate::platform::relaunch::bundle_of(exe) {
        Some(app) => {
            let mut open = Command::new("/usr/bin/open");
            open.arg("-n").arg(app);
            open
        }
        None => Command::new(exe),
    }
}

/// The bundle's `Info.plist` names the app to Notification Center and the
/// Dock: nothing to set at runtime.
pub fn set_identity(_cx: &mut gpui::App) {}

/// A Finder launch inherits launchd's environment, which names the login
/// shell: nothing to fill in.
pub fn adopt_login_shell() {}

/// Wait for an opener that finishes at once (`open`) and turn
/// its exit into a message the caller can show.
fn run_opener(mut cmd: std::process::Command, what: &str) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let status = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("couldn't run `{program}`: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {what}` exited {status}"))
    }
}

/// The AppleScript string literal for `s`, double-quoted.
pub fn double_quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_quoting_escapes_quotes_and_backslashes() {
        assert_eq!(double_quoted(r#"vim "a\b""#), r#""vim \"a\\b\"""#);
    }
}
