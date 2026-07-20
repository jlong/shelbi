//! OS-supervisor plumbing for `shelbi daemon install|uninstall|status|restart`.
//!
//! The socket listener itself lives in the sibling [`super::serve`]
//! module; this half is purely about making that listener survive
//! crashes and reboots.
//!
//! We don't write our own supervisor — auto-restart, run-at-login, and
//! reboot persistence ride on `launchd` (macOS) or `systemd --user`
//! (Linux). `install` writes the platform-appropriate unit file with
//! absolute paths baked in (so the supervisor doesn't need `$PATH`) and
//! loads it; `uninstall` unloads + removes; `status` summarizes; and
//! `restart` asks the supervisor to bounce the daemon so a freshly
//! installed binary takes effect without losing the auto-restart
//! guarantee.
//!
//! Each verb has one entry point that dispatches by `cfg(target_os = …)`.
//! A catch-all stub on unknown OSes prints a clear "not supported" line
//! and returns Ok so scripts (notably `scripts/install.sh`) don't fail
//! the install just because the host isn't macOS or Linux.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

/// launchd `Label` (and the basename of the plist we install into
/// `~/Library/LaunchAgents`). Reverse-DNS of shelbi.dev, the project's own
/// domain. macOS-only: the Linux build compiles the systemd branch instead
/// and uses [`SYSTEMD_SERVICE_NAME`] for its identifier.
#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "dev.shelbi.daemon";
/// The pre-rename launchd label. `install` retires any agent still
/// registered under it (see [`migrate_legacy_launchd`]) so upgrading from a
/// build that used the old label doesn't leave two supervisors running.
/// This is the sole remaining reference to the retired identifier and
/// exists only to clean it up.
#[cfg(target_os = "macos")]
const LEGACY_SERVICE_LABEL: &str = "co.32pixels.shelbi";
/// systemd needs the `.service` suffix on most commands. Centralized so a
/// future rename touches one place. `dead_code` allowed because the
/// constant is only referenced from `cfg(target_os = "linux")` arms;
/// macOS builds compile it but never read it.
#[allow(dead_code)]
const SYSTEMD_SERVICE_NAME: &str = "dev.shelbi.daemon.service";
/// The pre-rename systemd unit name, retired on install for the same
/// reason as [`LEGACY_SERVICE_LABEL`]. `dead_code` allowed for the same
/// `cfg` reason as [`SYSTEMD_SERVICE_NAME`].
#[allow(dead_code)]
const LEGACY_SYSTEMD_SERVICE_NAME: &str = "shelbi.service";

#[cfg(target_os = "macos")]
pub(super) fn install() -> Result<()> {
    install_launchd()
}
#[cfg(target_os = "linux")]
pub(super) fn install() -> Result<()> {
    install_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn install() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn uninstall() -> Result<()> {
    uninstall_launchd()
}
#[cfg(target_os = "linux")]
pub(super) fn uninstall() -> Result<()> {
    uninstall_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn uninstall() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn status() -> Result<()> {
    status_launchd()
}
#[cfg(target_os = "linux")]
pub(super) fn status() -> Result<()> {
    status_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn status() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn restart() -> Result<()> {
    restart_launchd()
}
#[cfg(target_os = "linux")]
pub(super) fn restart() -> Result<()> {
    restart_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn restart() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported_warning() {
    eprintln!(
        "shelbi daemon: daemon supervision not supported on {}; \
         run `shelbi daemon` manually under your supervisor of choice",
        std::env::consts::OS
    );
}

/// Resolve the absolute path to the currently-running `shelbi` binary so
/// the supervisor doesn't depend on `$PATH` being set in the launchd /
/// systemd execution environment.
fn current_binary() -> Result<PathBuf> {
    std::env::current_exe().context("resolving current shelbi binary path")
}

/// Resolve the shelbi state root we want the supervised daemon to see.
/// Baking this into the unit file (as `SHELBI_ROOT=…`) means the user can
/// later move or rebuild the binary without the supervised daemon
/// silently flipping to a different home directory.
fn baked_state_root() -> Result<PathBuf> {
    shelbi_state::shelbi_home().map_err(|e| anyhow!(e))
}

/// `~/.shelbi/logs/`, the home for `daemon.out`, `daemon.err`, `daemon.log`.
fn ensure_log_dir() -> Result<PathBuf> {
    let dir = baked_state_root()?.join("logs");
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating daemon log dir {}", dir.display()))?;
    Ok(dir)
}

// --------------------------- macOS / launchd ------------------------------

#[cfg(target_os = "macos")]
fn install_launchd() -> Result<()> {
    let plist_path = launch_agent_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating LaunchAgents dir {}", parent.display()))?;
    }
    ensure_log_dir()?;

    let plist = render_launchd_plist(&LaunchdInputs::resolve()?);
    fs::write(&plist_path, plist)
        .with_context(|| format!("writing launchd plist {}", plist_path.display()))?;

    // Retire any daemon left over under the pre-rename label before we
    // bootstrap the new one, so an upgrade doesn't leave two supervisors.
    let uid = current_uid();
    migrate_legacy_launchd(uid);

    // Idempotent reinstall: bootout the old instance (if any), then
    // bootstrap the freshly-written plist — retrying through launchd's
    // transient EIO. See [`bootstrap_launchd`].
    bootstrap_launchd(uid, &plist_path)?;

    println!("✓ installed launchd agent at {}", plist_path.display());
    println!("  label: {SERVICE_LABEL}");
    println!("  daemon should now be running — verify with `shelbi daemon status`");
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    let uid = current_uid();
    let _ = Command::new("launchctl")
        .args(["bootout", &gui_target(uid)])
        .status();

    let plist_path = launch_agent_plist_path()?;
    if plist_path.exists() {
        fs::remove_file(&plist_path)
            .with_context(|| format!("removing {}", plist_path.display()))?;
        println!("✓ removed {}", plist_path.display());
    } else {
        println!("(no plist at {})", plist_path.display());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn status_launchd() -> Result<()> {
    let uid = current_uid();
    let target = gui_target(uid);
    let out = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .context("invoking launchctl print")?;
    if !out.status.success() {
        println!("shelbi daemon: not installed (or not loaded)");
        println!("  install with: shelbi daemon install");
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = parse_launchctl_field(&text, "pid").unwrap_or_else(|| "(none)".into());
    let state = parse_launchctl_field(&text, "state").unwrap_or_else(|| "unknown".into());
    let last_exit = parse_launchctl_field(&text, "last exit code");
    let runs = parse_launchctl_field(&text, "runs");

    println!("shelbi daemon ({SERVICE_LABEL})");
    println!("  state:           {state}");
    println!("  pid:             {pid}");
    if let Some(runs) = runs {
        println!("  total launches:  {runs}");
    }
    if let Some(le) = last_exit {
        println!("  last exit code:  {le}");
    }
    println!(
        "  plist:           {}",
        launch_agent_plist_path()?.display()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn restart_launchd() -> Result<()> {
    // Refresh the plist before restarting. Package-manager upgrades can
    // leave a long-lived daemon running after the binary has moved; a
    // bare kickstart would relaunch the stale ProgramArguments path.
    let plist_path = launch_agent_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating LaunchAgents dir {}", parent.display()))?;
    }
    ensure_log_dir()?;
    let plist = render_launchd_plist(&LaunchdInputs::resolve()?);
    fs::write(&plist_path, plist)
        .with_context(|| format!("writing launchd plist {}", plist_path.display()))?;

    let uid = current_uid();
    let target = gui_target(uid);
    // bootout + bootstrap forces launchd to ingest the refreshed plist;
    // kickstart alone keeps the already-loaded (possibly stale) unit. The
    // retry in [`bootstrap_launchd`] rides out launchd's transient EIO.
    bootstrap_launchd(uid, &plist_path)?;
    println!("✓ restarted {target} on the current shelbi binary");
    Ok(())
}

/// Load the freshly-written plist into launchd, booting out any prior
/// instance first, and retry through the transient `Input/output error`
/// (EIO, exit 5) that `bootstrap` sometimes returns while an unload is
/// still settling.
///
/// A single bootout+bootstrap pair is not reliable: on a host where the
/// service is absent, `bootout` prints "No such process" and the following
/// `bootstrap` can fail with EIO, leaving the daemon DOWN — yet running
/// `shelbi daemon install` again immediately recovers it. This loops that
/// recovery in-process (a fresh bootout clears any half-registered unit
/// between tries) so one invocation is idempotent regardless of the
/// starting state. `bootout` returns non-zero when nothing is loaded; that
/// is expected on a first install, so its status and its "No such process"
/// noise are both swallowed.
///
/// Only a genuine, repeated failure — a bootstrap that never succeeds and
/// leaves the service unloaded — is surfaced as an error.
#[cfg(target_os = "macos")]
fn bootstrap_launchd(uid: u32, plist_path: &std::path::Path) -> Result<()> {
    const ATTEMPTS: u32 = 4;
    let mut last: Option<(Option<i32>, String)> = None;
    for attempt in 1..=ATTEMPTS {
        let _ = Command::new("launchctl")
            .args(["bootout", &gui_target(uid)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        let out = Command::new("launchctl")
            .args(["bootstrap", &gui_domain(uid)])
            .arg(plist_path)
            .output()
            .context("invoking launchctl bootstrap")?;
        // Treat a service that ends up loaded as success even if bootstrap
        // reported non-zero, so a spurious EIO over a good registration
        // doesn't abort the install.
        if out.status.success() || service_loaded(uid) {
            return Ok(());
        }
        last = Some((
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
        if attempt < ATTEMPTS {
            // Let launchd finish tearing down the prior unit before the
            // next bootout+bootstrap.
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
    let (code, stderr) = last.unwrap_or((None, String::new()));
    let detail = if stderr.is_empty() {
        String::new()
    } else {
        format!("{stderr}; ")
    };
    bail!(
        "launchctl bootstrap failed (exit {code:?}) after {ATTEMPTS} attempts — \
         {detail}see `launchctl print {}` for details",
        gui_target(uid)
    );
}

/// True when launchd has the shelbi agent loaded in the user's GUI domain.
/// Used as the fallback success signal for [`bootstrap_launchd`] when
/// `bootstrap` returns a spurious non-zero over an already-registered unit.
#[cfg(target_os = "macos")]
fn service_loaded(uid: u32) -> bool {
    Command::new("launchctl")
        .args(["print", &gui_target(uid)])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    // SAFETY: `getuid()` is a thread-safe POSIX call with no inputs.
    unsafe { libc::getuid() }
}

#[cfg(target_os = "macos")]
fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
fn gui_target(uid: u32) -> String {
    format!("gui/{uid}/{SERVICE_LABEL}")
}

#[cfg(target_os = "macos")]
fn launch_agent_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolving $HOME for LaunchAgents path")?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

/// Path of the plist installed under the pre-rename label.
#[cfg(target_os = "macos")]
fn legacy_launch_agent_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolving $HOME for LaunchAgents path")?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LEGACY_SERVICE_LABEL}.plist")))
}

/// `launchctl bootout` target for the pre-rename service.
#[cfg(target_os = "macos")]
fn legacy_gui_target(uid: u32) -> String {
    format!("gui/{uid}/{LEGACY_SERVICE_LABEL}")
}

/// Retire a daemon still registered under the pre-rename launchd label so
/// an in-place upgrade doesn't leave the old and new agents both running.
/// Best-effort: `bootout` tolerates "not loaded" and plist removal only
/// warns on a real error — a migration hiccup must never abort a fresh
/// install.
#[cfg(target_os = "macos")]
fn migrate_legacy_launchd(uid: u32) {
    let _ = Command::new("launchctl")
        .args(["bootout", &legacy_gui_target(uid)])
        .status();
    match legacy_launch_agent_plist_path() {
        Ok(path) if path.exists() => match fs::remove_file(&path) {
            Ok(()) => println!("✓ removed legacy launchd plist {}", path.display()),
            Err(e) => eprintln!(
                "warning: could not remove legacy launchd plist {}: {e}",
                path.display()
            ),
        },
        Ok(_) => {}
        Err(e) => eprintln!("warning: could not resolve legacy plist path: {e}"),
    }
}

/// Inputs that go into the rendered plist. Bundled into a struct so the
/// renderer is pure (testable without touching the filesystem). macOS-only:
/// the Linux build supervises via systemd and never renders a plist.
#[cfg(target_os = "macos")]
struct LaunchdInputs {
    binary: PathBuf,
    state_root: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

#[cfg(target_os = "macos")]
impl LaunchdInputs {
    fn resolve() -> Result<Self> {
        let state_root = baked_state_root()?;
        let log_dir = state_root.join("logs");
        Ok(Self {
            binary: current_binary()?,
            stdout_path: log_dir.join("daemon.out"),
            stderr_path: log_dir.join("daemon.err"),
            state_root,
        })
    }
}

/// macOS-only: Linux builds compile the systemd unit renderer instead.
#[cfg(target_os = "macos")]
fn render_launchd_plist(inputs: &LaunchdInputs) -> String {
    let exe = xml_escape(&inputs.binary.to_string_lossy());
    let root = xml_escape(&inputs.state_root.to_string_lossy());
    let out = xml_escape(&inputs.stdout_path.to_string_lossy());
    let err = xml_escape(&inputs.stderr_path.to_string_lossy());
    // ThrottleInterval=1 matches the systemd `RestartSec=1s` knob — both
    // platforms aim for a sub-second relaunch on crash so worker writes
    // resume promptly without hammering the OS in a crash loop.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{SERVICE_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>SHELBI_ROOT</key>
        <string>{root}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>1</integer>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#
    )
}

/// Pull a `name = value` pair out of `launchctl print` output. The
/// command emits indented `key = value;` lines per state — we grab the
/// first occurrence of the requested key.
#[cfg(target_os = "macos")]
fn parse_launchctl_field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key} = ")) {
            return Some(rest.trim_end_matches(';').trim().to_string());
        }
    }
    None
}

/// macOS-only: only the plist renderer needs XML escaping; systemd unit
/// files are line-oriented plain text.
#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

// --------------------------- Linux / systemd ------------------------------

#[cfg(target_os = "linux")]
fn install_systemd() -> Result<()> {
    let unit_path = systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating systemd user unit dir {}", parent.display()))?;
    }
    ensure_log_dir()?;

    let unit = render_systemd_unit(&SystemdInputs::resolve()?);
    fs::write(&unit_path, unit)
        .with_context(|| format!("writing systemd unit {}", unit_path.display()))?;

    // Retire any unit left over under the pre-rename name before enabling
    // the new one, so an upgrade doesn't leave two supervisors.
    migrate_legacy_systemd();

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", SYSTEMD_SERVICE_NAME])?;

    println!("✓ installed systemd user unit at {}", unit_path.display());
    println!("  daemon should now be running — verify with `shelbi daemon status`");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<()> {
    // disable --now stops the running unit and removes the wants/links.
    // Tolerate the error path (unit may not be loaded) so uninstall stays
    // idempotent even after a manual `rm`.
    let _ = run_systemctl(&["disable", "--now", SYSTEMD_SERVICE_NAME]);

    let unit_path = systemd_unit_path()?;
    if unit_path.exists() {
        fs::remove_file(&unit_path).with_context(|| format!("removing {}", unit_path.display()))?;
        println!("✓ removed {}", unit_path.display());
    } else {
        println!("(no unit at {})", unit_path.display());
    }
    let _ = run_systemctl(&["daemon-reload"]);
    Ok(())
}

#[cfg(target_os = "linux")]
fn status_systemd() -> Result<()> {
    let out = Command::new("systemctl")
        .args(["--user", "status", SYSTEMD_SERVICE_NAME, "--no-pager"])
        .output()
        .context("invoking systemctl --user status")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        println!("shelbi daemon: not installed (or systemctl returned nothing)");
        println!("  install with: shelbi daemon install");
        return Ok(());
    }
    // systemctl's first ~6 lines are the header block (loaded/active/pid/
    // tasks/memory) — enough for a quick read without dumping the journal.
    for line in text.lines().take(6) {
        println!("{line}");
    }
    println!("  unit: {}", systemd_unit_path()?.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn restart_systemd() -> Result<()> {
    // Refresh ExecStart before restarting. A package-manager upgrade may
    // move the executable while systemd still holds the old unit in
    // memory; daemon-reload is required for the restart to use this CLI.
    let unit_path = systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating systemd user unit dir {}", parent.display()))?;
    }
    ensure_log_dir()?;
    let unit = render_systemd_unit(&SystemdInputs::resolve()?);
    fs::write(&unit_path, unit)
        .with_context(|| format!("writing systemd user unit {}", unit_path.display()))?;
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["restart", SYSTEMD_SERVICE_NAME])?;
    println!("✓ restarted {SYSTEMD_SERVICE_NAME} on the current shelbi binary");
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolving $HOME for systemd unit path")?;
    Ok(home.join(".config/systemd/user").join(SYSTEMD_SERVICE_NAME))
}

/// Path of the systemd user unit installed under the pre-rename name.
#[cfg(target_os = "linux")]
fn legacy_systemd_unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolving $HOME for systemd unit path")?;
    Ok(home
        .join(".config/systemd/user")
        .join(LEGACY_SYSTEMD_SERVICE_NAME))
}

/// Stop, disable, and remove any user unit left over under the pre-rename
/// name so an in-place upgrade doesn't leave two supervisors running.
/// Best-effort: `disable --now` tolerates "no such unit" and unit removal
/// only warns on a real error — a migration hiccup must never abort a fresh
/// install.
#[cfg(target_os = "linux")]
fn migrate_legacy_systemd() {
    let _ = run_systemctl(&["disable", "--now", LEGACY_SYSTEMD_SERVICE_NAME]);
    match legacy_systemd_unit_path() {
        Ok(path) if path.exists() => match fs::remove_file(&path) {
            Ok(()) => println!("✓ removed legacy systemd unit {}", path.display()),
            Err(e) => eprintln!(
                "warning: could not remove legacy systemd unit {}: {e}",
                path.display()
            ),
        },
        Ok(_) => {}
        Err(e) => eprintln!("warning: could not resolve legacy unit path: {e}"),
    }
    let _ = run_systemctl(&["daemon-reload"]);
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("invoking systemctl --user")?;
    if !status.success() {
        bail!(
            "systemctl --user {} failed (exit {:?})",
            args.join(" "),
            status.code()
        );
    }
    Ok(())
}

/// Inputs that go into the rendered systemd unit. Bundled into a struct
/// so the renderer is pure (testable without touching the filesystem).
/// `dead_code` allowed because production callers live behind
/// `cfg(target_os = "linux")` — tests construct it directly on every
/// platform.
#[allow(dead_code)]
struct SystemdInputs {
    binary: PathBuf,
    state_root: PathBuf,
    log_path: PathBuf,
}

impl SystemdInputs {
    #[cfg(target_os = "linux")]
    fn resolve() -> Result<Self> {
        let state_root = baked_state_root()?;
        Ok(Self {
            binary: current_binary()?,
            log_path: state_root.join("logs/daemon.log"),
            state_root,
        })
    }
}

#[allow(dead_code)]
fn render_systemd_unit(inputs: &SystemdInputs) -> String {
    // Restart=always + RestartSec=1s mirrors launchd's KeepAlive +
    // ThrottleInterval=1: sub-second relaunch on any exit path. The
    // append: prefix on StandardOutput/Error keeps the log file alive
    // across restarts so the user can `tail -f` once and never re-open.
    format!(
        "[Unit]
Description=Shelbi hub daemon
Documentation=https://github.com/jlong/shelbi
After=default.target

[Service]
Type=simple
ExecStart={exe} daemon
Environment=SHELBI_ROOT={root}
Restart=always
RestartSec=1s
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
",
        exe = inputs.binary.display(),
        root = inputs.state_root.display(),
        log = inputs.log_path.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_plist_contains_required_keys_and_paths() {
        let inputs = LaunchdInputs {
            binary: PathBuf::from("/Users/dev/bin/shelbi"),
            state_root: PathBuf::from("/Users/dev/.shelbi"),
            stdout_path: PathBuf::from("/Users/dev/.shelbi/logs/daemon.out"),
            stderr_path: PathBuf::from("/Users/dev/.shelbi/logs/daemon.err"),
        };
        let plist = render_launchd_plist(&inputs);
        assert!(
            plist.contains("<string>dev.shelbi.daemon</string>"),
            "{plist}"
        );
        assert!(
            !plist.contains(LEGACY_SERVICE_LABEL),
            "legacy label leaked into plist: {plist}"
        );
        assert!(
            plist.contains("<string>/Users/dev/bin/shelbi</string>"),
            "absolute binary path: {plist}"
        );
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(
            plist.contains("<string>/Users/dev/.shelbi/logs/daemon.out</string>"),
            "stdout path: {plist}"
        );
        assert!(
            plist.contains("<string>/Users/dev/.shelbi/logs/daemon.err</string>"),
            "stderr path: {plist}"
        );
        assert!(
            plist.contains("<string>/Users/dev/.shelbi</string>"),
            "SHELBI_ROOT env: {plist}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn launchd_plist_xml_escapes_path_with_specials() {
        let inputs = LaunchdInputs {
            binary: PathBuf::from("/o&p<x>/shelbi"),
            state_root: PathBuf::from("/o&p<x>/.shelbi"),
            stdout_path: PathBuf::from("/o&p<x>/.shelbi/logs/daemon.out"),
            stderr_path: PathBuf::from("/o&p<x>/.shelbi/logs/daemon.err"),
        };
        let plist = render_launchd_plist(&inputs);
        assert!(
            plist.contains("/o&amp;p&lt;x&gt;/shelbi"),
            "escaped: {plist}"
        );
        assert!(!plist.contains("/o&p<x>/shelbi"), "raw leaked: {plist}");
    }

    #[test]
    fn systemd_unit_contains_required_fields() {
        let inputs = SystemdInputs {
            binary: PathBuf::from("/home/dev/bin/shelbi"),
            state_root: PathBuf::from("/home/dev/.shelbi"),
            log_path: PathBuf::from("/home/dev/.shelbi/logs/daemon.log"),
        };
        let unit = render_systemd_unit(&inputs);
        assert!(
            unit.contains("ExecStart=/home/dev/bin/shelbi daemon"),
            "ExecStart: {unit}"
        );
        assert!(unit.contains("Restart=always"), "restart policy: {unit}");
        assert!(unit.contains("RestartSec=1s"), "restart delay: {unit}");
        assert!(
            unit.contains("Environment=SHELBI_ROOT=/home/dev/.shelbi"),
            "env: {unit}"
        );
        assert!(
            unit.contains("StandardOutput=append:/home/dev/.shelbi/logs/daemon.log"),
            "stdout: {unit}"
        );
        assert!(
            unit.contains("StandardError=append:/home/dev/.shelbi/logs/daemon.log"),
            "stderr: {unit}"
        );
        assert!(
            unit.contains("WantedBy=default.target"),
            "install target: {unit}"
        );
        assert!(
            unit.contains("Documentation=https://github.com/jlong/shelbi"),
            "documentation url: {unit}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_launchd_cleanup_targets_old_label() {
        // The migration must aim `launchctl bootout` and the plist delete
        // at the retired label, and that target must differ from the new
        // one — otherwise the cleanup would tear down the fresh install.
        assert_eq!(
            legacy_gui_target(501),
            format!("gui/501/{LEGACY_SERVICE_LABEL}")
        );
        assert_ne!(legacy_gui_target(501), gui_target(501));
        let legacy_plist = legacy_launch_agent_plist_path().unwrap();
        assert!(
            legacy_plist.ends_with(format!("{LEGACY_SERVICE_LABEL}.plist")),
            "legacy plist path: {}",
            legacy_plist.display()
        );
        assert_ne!(legacy_plist, launch_agent_plist_path().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_systemd_cleanup_targets_old_unit() {
        // The migration must aim `systemctl disable --now` and the unit
        // delete at the retired unit name, distinct from the new one.
        assert_ne!(LEGACY_SYSTEMD_SERVICE_NAME, SYSTEMD_SERVICE_NAME);
        let legacy = legacy_systemd_unit_path().unwrap();
        assert!(
            legacy.ends_with(LEGACY_SYSTEMD_SERVICE_NAME),
            "legacy unit path: {}",
            legacy.display()
        );
        assert_ne!(legacy, systemd_unit_path().unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_launchctl_field_extracts_value() {
        let sample = "\
            \tstate = running\n\
            \tpid = 12345\n\
            \tlast exit code = 0\n\
            \truns = 7\n";
        assert_eq!(
            parse_launchctl_field(sample, "state"),
            Some("running".into())
        );
        assert_eq!(parse_launchctl_field(sample, "pid"), Some("12345".into()));
        assert_eq!(
            parse_launchctl_field(sample, "last exit code"),
            Some("0".into())
        );
        assert_eq!(parse_launchctl_field(sample, "runs"), Some("7".into()));
        assert_eq!(parse_launchctl_field(sample, "missing"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xml_escape_handles_all_specials() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("plain/path/to/binary"), "plain/path/to/binary");
    }
}
