//! Linux (and any other non-macOS Unix): the AUR packages, XDG, `xdg-open`,
//! `xdg-terminal-exec`, freedesktop notifications.
//!
//! The app ships as the `shelbi-desktop-bin` package, which depends on
//! `shelbi-bin` (the CLI), so the package manager owns both installs and
//! upgrades: the app never downloads a CLI here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The well-known install directories, in search order: pipx-style
/// `~/.local/bin`, the package's `/usr/bin`, a hand install in
/// `/usr/local/bin`, `cargo install`, and `~/bin` (`scripts/install.sh`).
/// A launcher (Walker, a `.desktop` file) may hand the app a thin `PATH`
/// whose login-shell capture failed, so these are searched too.
pub fn known_cli_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = home {
        dirs.push(home.join(".local/bin"));
    }
    dirs.push(PathBuf::from("/usr/bin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    if let Some(home) = home {
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join("bin"));
    }
    dirs
}

/// The package provides the CLI; a missing one is reported with the install
/// command, never downloaded.
pub const INSTALLS_CLI: bool = false;

/// Where the CLI is looked for besides the login `PATH`, as Settings'
/// help line names them.
pub const KNOWN_CLI_PLACES: &str = "in ~/.local/bin, /usr/bin, ~/.cargo/bin, or ~/bin";
/// The words before [`INSTALL_COMMAND`].
pub const INSTALL_LABEL: &str = "Install it from the AUR:";
/// The same, offered after another fix.
pub const OR_INSTALL_LABEL: &str = "Or install it from the AUR:";
/// The CLI's install command.
pub const INSTALL_COMMAND: &str = "yay -S shelbi-bin";
/// The CLI's upgrade command.
pub const UPGRADE_COMMAND: &str = "yay -Syu shelbi-bin";
/// The app's own upgrade command. Both packages, so the app and the CLI stay
/// the same version.
pub const APP_UPGRADE_COMMAND: &str = "yay -Syu shelbi-desktop-bin shelbi-bin";

/// Where Settings' Browse… starts: the executables.
pub const BROWSE_START: &str = "/usr/bin";

/// Where the save panel for a download starts: the XDG download folder
/// (`XDG_DOWNLOAD_DIR`, from the environment or `user-dirs.dirs`), else
/// `~/Downloads`.
pub fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR").filter(|d| !d.is_empty()) {
        return expand_home(&dir.to_string_lossy(), &home);
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map_or_else(|| home.join(".config"), PathBuf::from);
    std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| user_dir(&text, "XDG_DOWNLOAD_DIR"))
        .map(|dir| expand_home(&dir, &home))
        .unwrap_or_else(|| home.join("Downloads"))
}

/// `key`'s value in a `user-dirs.dirs` file (`KEY="$HOME/Downloads"`).
pub fn user_dir(text: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| line.strip_prefix(key)?.trim_start().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_string())
        .filter(|value| !value.is_empty())
}

/// `$HOME/x` (or `~/x`) → `<home>/x`.
fn expand_home(dir: &str, home: &Path) -> PathBuf {
    match dir.strip_prefix("$HOME").or_else(|| dir.strip_prefix('~')) {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(dir),
    }
}

/// Open a URL, file or folder with its default app (`xdg-open`). Under a
/// bare window manager `xdg-open` can run the handler in the foreground
/// until it exits, so it isn't waited on here: a thread reaps it, and only
/// a failure to start is reported.
pub fn open(target: &str) -> Result<(), String> {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(target);
    spawn_detached(cmd)
}

/// Open `path` in the default app for its type: the text editor for YAML.
pub fn open_text(path: &Path) -> Result<(), String> {
    open(&path.to_string_lossy())
}

/// Open `file` in `app`: an executable is run with the file; anything else
/// (an `.app` from a Mac's `config.yaml`) has no meaning here.
pub fn open_with_app(app: &Path, file: &Path) -> Result<(), String> {
    if !crate::platform::discovery::is_executable_file(app) {
        return Err(format!(
            "{} isn't a program this computer can run: choose an editor command in Settings",
            app.display()
        ));
    }
    let mut cmd = Command::new(app);
    cmd.arg(file);
    spawn_detached(cmd)
}

/// Run the shell command line `command` in a new terminal window: the
/// user's default terminal through `xdg-terminal-exec` (Omarchy's), else
/// `$TERMINAL -e`, else `x-terminal-emulator -e`.
pub fn run_in_terminal(command: &str) -> Result<(), String> {
    let terminal = std::env::var("TERMINAL").ok().filter(|t| !t.trim().is_empty());
    let (program, mut args) = terminal_command(on_path("xdg-terminal-exec"), terminal.as_deref())
        .ok_or_else(|| "No terminal found: install xdg-terminal-exec, or set $TERMINAL".to_string())?;
    args.extend(["sh".to_string(), "-c".to_string(), command.to_string()]);
    let mut cmd = Command::new(&program);
    cmd.args(args);
    spawn_detached(cmd).map_err(|e| format!("launching a terminal for `{command}`: {e}"))
}

/// The terminal launcher and the arguments before the command it runs.
pub fn terminal_command(has_xdg_terminal_exec: bool, terminal: Option<&str>) -> Option<(String, Vec<String>)> {
    if has_xdg_terminal_exec {
        return Some(("xdg-terminal-exec".into(), Vec::new()));
    }
    if let Some(terminal) = terminal {
        let mut words = terminal.split_whitespace().map(str::to_string);
        let program = words.next()?;
        let mut args: Vec<String> = words.collect();
        args.push("-e".into());
        return Some((program, args));
    }
    on_path("x-terminal-emulator").then(|| ("x-terminal-emulator".into(), vec!["-e".into()]))
}

/// The command that starts a fresh instance of the app: the executable
/// `/proc/self/exe` names. After a package upgrade the link reads
/// `/usr/bin/shelbi-desktop (deleted)` (it still points at the old inode),
/// so the path is re-resolved without the suffix to run the new binary.
pub fn relaunch_command(exe: &Path) -> Command {
    let path = std::fs::read_link("/proc/self/exe")
        .map(|link| strip_deleted(&link))
        .unwrap_or_else(|_| exe.to_path_buf());
    Command::new(path)
}

/// `/x/y (deleted)` → `/x/y`.
pub fn strip_deleted(link: &Path) -> PathBuf {
    let text = link.to_string_lossy();
    match text.strip_suffix(" (deleted)") {
        Some(path) => PathBuf::from(path),
        None => link.to_path_buf(),
    }
}

/// Name the app to the notification daemon (mako shows "Shelbi", and its
/// `app-name=Shelbi` criteria match).
pub fn set_identity(cx: &mut gpui::App) {
    cx.set_app_identity(super::APP_ID, super::APP_NAME);
}

/// A launcher's environment may lack `SHELL` (a systemd user unit, some
/// app launchers), and the login-shell capture then runs `/bin/sh`, which
/// reads none of the user's startup files. Name the user's own shell from
/// the password database first. Call before any thread starts.
pub fn adopt_login_shell() {
    if std::env::var_os("SHELL").is_some_and(|s| !s.is_empty()) {
        return;
    }
    let Some(uid) = current_uid() else {
        return;
    };
    let Ok(passwd) = std::fs::read_to_string("/etc/passwd") else {
        return;
    };
    if let Some(shell) = passwd_shell(&passwd, uid) {
        std::env::set_var("SHELL", shell);
    }
}

/// The uid this process runs as (the owner of its `/proc` entry).
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

/// The login shell `/etc/passwd` gives `uid`, when it names one.
pub fn passwd_shell(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        (fields.len() >= 7 && fields[2].parse() == Ok(uid))
            .then(|| fields[6].trim().to_string())
            .filter(|shell| shell.starts_with('/'))
    })
}

/// Whether `name` is an executable on the app's own `PATH`.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| crate::platform::discovery::is_executable_file(&dir.join(name)))
    })
}

/// Start `cmd` with no stdio and reap it on a thread of its own, so it never
/// becomes a zombie and never blocks the caller.
fn spawn_detached(mut cmd: Command) -> Result<(), String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't run `{program}`: {e}"))?;
    let _ = std::thread::Builder::new()
        .name("shelbi-desktop-opener".into())
        .spawn(move || match child.wait() {
            Ok(status) if !status.success() => tracing::warn!(%program, %status, "the opener failed"),
            Err(e) => tracing::warn!(%program, %e, "couldn't wait for the opener"),
            Ok(_) => {}
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_is_searched_in_local_bin_then_usr_bin() {
        let dirs = known_cli_dirs(Some(Path::new("/home/me")));
        let dirs: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert_eq!(
            dirs,
            ["/home/me/.local/bin", "/usr/bin", "/usr/local/bin", "/home/me/.cargo/bin", "/home/me/bin"]
        );
        assert_eq!(known_cli_dirs(None), [PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")]);
    }

    #[test]
    fn user_dirs_give_the_download_folder() {
        let text = "# comment\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\nXDG_DOWNLOAD_DIR=\"$HOME/Dl\"\n";
        assert_eq!(user_dir(text, "XDG_DOWNLOAD_DIR").as_deref(), Some("$HOME/Dl"));
        assert_eq!(user_dir(text, "XDG_MUSIC_DIR"), None);
        assert_eq!(expand_home("$HOME/Dl", Path::new("/home/me")), PathBuf::from("/home/me/Dl"));
        assert_eq!(expand_home("/data/dl", Path::new("/home/me")), PathBuf::from("/data/dl"));
    }

    #[test]
    fn a_deleted_exe_relaunches_the_path_it_was_installed_at() {
        assert_eq!(
            strip_deleted(Path::new("/usr/bin/shelbi-desktop (deleted)")),
            PathBuf::from("/usr/bin/shelbi-desktop")
        );
        assert_eq!(strip_deleted(Path::new("/usr/bin/shelbi-desktop")), PathBuf::from("/usr/bin/shelbi-desktop"));
    }

    #[test]
    fn the_passwd_entry_names_the_login_shell() {
        let passwd = "root:x:0:0::/root:/usr/bin/bash\nme:x:1000:1000::/home/me:/usr/bin/zsh\nsvc:x:5:5::/:\n";
        assert_eq!(passwd_shell(passwd, 1000).as_deref(), Some("/usr/bin/zsh"));
        assert_eq!(passwd_shell(passwd, 5), None, "no shell named");
        assert_eq!(passwd_shell(passwd, 42), None);
    }

    #[test]
    fn the_terminal_is_xdg_terminal_exec_then_dollar_terminal() {
        assert_eq!(terminal_command(true, Some("kitty")), Some(("xdg-terminal-exec".into(), vec![])));
        assert_eq!(
            terminal_command(false, Some("alacritty --class x")),
            Some(("alacritty".into(), vec!["--class".into(), "x".into(), "-e".into()]))
        );
    }
}
