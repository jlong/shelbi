//! Finding the `shelbi` CLI.
//!
//! Order: the `desktop.cli_path` setting in `~/.shelbi/config.yaml`, then the
//! login shell's `PATH`, then the places Shelbi gets installed when that
//! `PATH` doesn't say (`/opt/homebrew/bin`, `/usr/local/bin`,
//! `~/.cargo/bin`, and `~/bin`, where the app's own install and
//! `scripts/install.sh` put it). A set override is the only candidate: a typo in it is
//! reported, never papered over by a different `shelbi` found elsewhere.
//!
//! The order and the search are pure ([`candidates`], [`discover`]) so they
//! are tested without a filesystem.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::env::Env;

/// The executable's name.
pub const CLI_NAME: &str = "shelbi";

/// How long `shelbi --version` may take before the app stops waiting.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a candidate came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliSource {
    /// `desktop.cli_path` in `config.yaml`.
    Setting,
    /// A directory on the login shell's `PATH`.
    LoginPath,
    /// One of the well-known install directories.
    KnownLocation,
}

impl CliSource {
    /// How the source reads in the UI and the log.
    pub fn label(self) -> &'static str {
        match self {
            CliSource::Setting => "the desktop.cli_path setting",
            CliSource::LoginPath => "your login shell's PATH",
            CliSource::KnownLocation => "a standard install location",
        }
    }
}

/// One place `shelbi` might be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub source: CliSource,
}

/// The `shelbi` the app uses, and where it was found.
pub type CliLocation = Candidate;

/// Discovery found no usable `shelbi`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliNotFound {
    /// Every path tried, in order.
    pub searched: Vec<PathBuf>,
    /// The `desktop.cli_path` override, when one is set (then it is the only
    /// path searched).
    pub setting: Option<PathBuf>,
    /// `config.yaml` could not be read, so its override (if any) was not
    /// applied.
    pub config_error: Option<String>,
}

/// The well-known install directories, in search order.
pub fn known_locations(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = home {
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join("bin"));
    }
    dirs
}

/// Every candidate, in search order, without duplicates (a `PATH` that already
/// lists `/opt/homebrew/bin` doesn't search it twice).
pub fn candidates(
    setting: Option<&Path>,
    path_dirs: &[PathBuf],
    home: Option<&Path>,
) -> Vec<Candidate> {
    if let Some(path) = setting {
        return vec![Candidate {
            path: expand_tilde(path, home),
            source: CliSource::Setting,
        }];
    }
    let mut out: Vec<Candidate> = Vec::new();
    let path_candidates = path_dirs
        .iter()
        // A relative PATH entry means nothing to an app with no shell cwd.
        .filter(|dir| dir.is_absolute())
        .map(|dir| (dir.join(CLI_NAME), CliSource::LoginPath));
    let known = known_locations(home)
        .into_iter()
        .map(|dir| (dir.join(CLI_NAME), CliSource::KnownLocation));
    for (path, source) in path_candidates.chain(known) {
        if !out.iter().any(|c| c.path == path) {
            out.push(Candidate { path, source });
        }
    }
    out
}

/// The first candidate `is_executable` accepts.
pub fn discover(
    setting: Option<&Path>,
    path_dirs: &[PathBuf],
    home: Option<&Path>,
    is_executable: impl Fn(&Path) -> bool,
) -> Result<CliLocation, CliNotFound> {
    let all = candidates(setting, path_dirs, home);
    match all.iter().find(|c| is_executable(&c.path)) {
        Some(found) => Ok(found.clone()),
        None => Err(CliNotFound {
            searched: all.into_iter().map(|c| c.path).collect(),
            setting: setting.map(|p| expand_tilde(p, home)),
            config_error: None,
        }),
    }
}

/// `~/x` → `<home>/x`; anything else unchanged.
pub fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// A regular file with an execute bit (following symlinks, as Homebrew's
/// `bin/shelbi` is one).
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// The `desktop.cli_path` override from `config.yaml`.
pub fn read_setting() -> Result<Option<PathBuf>, String> {
    shelbi_state::load_user_config()
        .map(|cfg| cfg.desktop.cli_path)
        .map_err(|e| format!("{}: {e}", config_path_label()))
}

/// `config.yaml`'s path for messages.
pub fn config_path_label() -> String {
    shelbi_state::user_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~/.shelbi/config.yaml".to_string())
}

/// The CLI's version, from `shelbi --version` (`shelbi 0.10.0`). `None` when
/// it doesn't run or doesn't answer in time; the daemon's hello is then the
/// only version the check has.
pub fn cli_version(cli: &Path, env: &Env) -> Option<String> {
    parse_cli_version(&cli_output(cli, "--version", env)?)
}

/// The control protocol the CLI speaks, from `shelbi __control-protocol`.
/// `None` when it doesn't answer: a CLI from before that command existed
/// (it exits with a usage error), or one that doesn't run.
pub fn cli_control_protocol(cli: &Path, env: &Env) -> Option<u32> {
    cli_output(cli, "__control-protocol", env)?.trim().parse().ok()
}

/// `<cli> <arg>`'s stdout under `env`, when it exits 0 within
/// [`VERSION_TIMEOUT`].
fn cli_output(cli: &Path, arg: &str, env: &Env) -> Option<String> {
    let mut child = Command::new(cli)
        .arg(arg)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

/// The version in `shelbi --version` output: the first whitespace-separated
/// word that starts with a digit (`shelbi 0.10.0` → `0.10.0`).
pub fn parse_cli_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(cands: &[Candidate]) -> Vec<String> {
        cands.iter().map(|c| c.path.display().to_string()).collect()
    }

    #[test]
    fn order_is_login_path_then_homebrew_then_usr_local_then_cargo_then_home_bin() {
        let home = Path::new("/Users/me");
        let cands = candidates(None, &[PathBuf::from("/Users/me/.local/bin")], Some(home));
        assert_eq!(
            paths(&cands),
            [
                "/Users/me/.local/bin/shelbi",
                "/opt/homebrew/bin/shelbi",
                "/usr/local/bin/shelbi",
                "/Users/me/.cargo/bin/shelbi",
                "/Users/me/bin/shelbi",
            ]
        );
        assert_eq!(cands[0].source, CliSource::LoginPath);
        assert!(cands[1..]
            .iter()
            .all(|c| c.source == CliSource::KnownLocation));
    }

    #[test]
    fn a_known_location_already_on_path_keeps_its_path_position() {
        let cands = candidates(
            None,
            &[PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin")],
            None,
        );
        assert_eq!(
            paths(&cands),
            [
                "/usr/local/bin/shelbi",
                "/usr/bin/shelbi",
                "/opt/homebrew/bin/shelbi"
            ]
        );
        assert_eq!(cands[0].source, CliSource::LoginPath);
    }

    #[test]
    fn relative_path_entries_are_skipped() {
        let cands = candidates(None, &[PathBuf::from("bin"), PathBuf::from(".")], None);
        assert_eq!(
            paths(&cands),
            ["/opt/homebrew/bin/shelbi", "/usr/local/bin/shelbi"]
        );
    }

    #[test]
    fn the_setting_overrides_discovery_and_is_the_only_candidate() {
        let home = Path::new("/Users/me");
        let found = discover(
            Some(Path::new("~/src/shelbi/target/debug/shelbi")),
            &[PathBuf::from("/opt/homebrew/bin")],
            Some(home),
            |_| true,
        )
        .unwrap();
        assert_eq!(
            found.path,
            PathBuf::from("/Users/me/src/shelbi/target/debug/shelbi")
        );
        assert_eq!(found.source, CliSource::Setting);

        // A broken setting is reported, not replaced by a PATH hit.
        let missing = discover(
            Some(Path::new("/nope/shelbi")),
            &[PathBuf::from("/opt/homebrew/bin")],
            Some(home),
            |p| p != Path::new("/nope/shelbi"),
        )
        .unwrap_err();
        assert_eq!(missing.searched, [PathBuf::from("/nope/shelbi")]);
        assert_eq!(missing.setting, Some(PathBuf::from("/nope/shelbi")));
    }

    #[test]
    fn discover_takes_the_first_executable_and_lists_every_path_when_none() {
        let home = Path::new("/Users/me");
        let found = discover(None, &[], Some(home), |p| {
            p == Path::new("/usr/local/bin/shelbi") || p == Path::new("/Users/me/.cargo/bin/shelbi")
        })
        .unwrap();
        assert_eq!(found.path, PathBuf::from("/usr/local/bin/shelbi"));

        let none = discover(None, &[PathBuf::from("/x")], Some(home), |_| false).unwrap_err();
        assert_eq!(none.searched.len(), 5);
        assert_eq!(none.setting, None);
    }

    #[test]
    fn version_output_parses() {
        assert_eq!(
            parse_cli_version("shelbi 0.10.0\n").as_deref(),
            Some("0.10.0")
        );
        assert_eq!(
            parse_cli_version("shelbi 1.2.3-rc.1").as_deref(),
            Some("1.2.3-rc.1")
        );
        assert_eq!(parse_cli_version("garbage"), None);
    }

    #[test]
    fn executable_check_needs_a_file_with_an_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shb-dt-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shelbi");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable_file(&file));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable_file(&file));
        assert!(!is_executable_file(&dir), "a directory is not a CLI");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
