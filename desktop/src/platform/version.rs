//! The version policy: the app, the CLI, and the daemon must be the same
//! version, exactly (the daemon's mutation guard already requires it of the
//! CLI and TUI). They ship together, so a mismatch means a partial upgrade,
//! and which way it is decides the fix the app shows.

use std::cmp::Ordering;

/// The app's own version.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How the app's version relates to the CLI and the running daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCheck {
    /// The daemon runs the app's version.
    Match,
    /// The app is newer than the installed CLI: upgrade the CLI
    /// (`brew upgrade shelbi`).
    AppNewer { app: String, cli: String },
    /// The CLI on disk matches the app but the running daemon is another
    /// version (it was started by an older or newer binary): restart it.
    DaemonStale { app: String, daemon: String },
    /// The daemon is newer than this running app: the CLI was upgraded and
    /// this process is the old app. Relaunch it.
    AppOutdated { app: String, daemon: String },
}

/// Classify the versions. `cli` is the installed CLI's `--version` (when it
/// answered), `daemon` the version from the control socket's hello.
pub fn check(app: &str, cli: Option<&str>, daemon: &str) -> VersionCheck {
    if app == daemon {
        return VersionCheck::Match;
    }
    if cli == Some(app) {
        return VersionCheck::DaemonStale {
            app: app.to_string(),
            daemon: daemon.to_string(),
        };
    }
    match compare(app, daemon) {
        Some(Ordering::Less) => VersionCheck::AppOutdated {
            app: app.to_string(),
            daemon: daemon.to_string(),
        },
        // Newer, or unparseable: upgrading the CLI is the general fix.
        _ => VersionCheck::AppNewer {
            app: app.to_string(),
            cli: cli.unwrap_or(daemon).to_string(),
        },
    }
}

/// How the daemon's control protocol relates to the app's, when its hello
/// reports a different one. Version strings can't tell these apart: a dev build
/// shares its version string with the release before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolCheck {
    /// The daemon speaks an older protocol, and the CLI on disk can't do
    /// better (it speaks an older one too, or is too old to say): restarting
    /// the daemon would start the same old binary. Upgrade the CLI, then
    /// restart the daemon.
    CliOlder {
        app: u32,
        /// `None` when the CLI predates `shelbi __control-protocol`.
        cli: Option<u32>,
        daemon: u32,
    },
    /// The CLI on disk speaks the app's protocol (or newer) but the running
    /// daemon is an older binary, left over from before an upgrade: restart it.
    DaemonStale { app: u32, cli: u32, daemon: u32 },
    /// The daemon speaks a newer protocol than this running app: relaunch it.
    AppOutdated { app: u32, daemon: u32 },
}

/// Classify a control-protocol mismatch. `cli` is what the CLI on disk
/// reports (`shelbi __control-protocol`), when it answered.
pub fn check_protocol(app: u32, cli: Option<u32>, daemon: u32) -> ProtocolCheck {
    if daemon > app {
        return ProtocolCheck::AppOutdated { app, daemon };
    }
    match cli {
        // Restarting the daemon onto this CLI brings it up to the app.
        Some(cli) if cli >= app => ProtocolCheck::DaemonStale { app, cli, daemon },
        _ => ProtocolCheck::CliOlder { app, cli, daemon },
    }
}

/// Semver precedence of two versions (`MAJOR.MINOR.PATCH[-pre][+build]`):
/// numbers first, then a pre-release sorts below its release, then pre-release
/// identifiers compare left to right (numeric ones numerically). Build
/// metadata is ignored. `None` when either doesn't parse.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (a_core, a_pre) = parse(a)?;
    let (b_core, b_pre) = parse(b)?;
    Some(a_core.cmp(&b_core).then_with(|| match (a_pre, b_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(a), Some(b)) => compare_pre(a, b),
    }))
}

fn parse(v: &str) -> Option<([u64; 3], Option<&str>)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split_once('+').map_or(v, |(head, _build)| head);
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let mut nums = core.split('.').map(|n| n.parse::<u64>().ok());
    let out = [nums.next()??, nums.next()??, nums.next()??];
    if nums.next().is_some() {
        return None;
    }
    Some((out, pre))
}

fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut a_ids = a.split('.');
    let mut b_ids = b.split('.');
    loop {
        match (a_ids.next(), b_ids.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_only() {
        assert_eq!(
            check("0.10.0", Some("0.10.0"), "0.10.0"),
            VersionCheck::Match
        );
        // A build-metadata-only difference is still not the same build.
        assert_ne!(check("0.10.0", None, "0.10.0+abc"), VersionCheck::Match);
    }

    #[test]
    fn app_newer_than_the_cli_names_both_versions() {
        assert_eq!(
            check("0.11.0", Some("0.10.0"), "0.10.0"),
            VersionCheck::AppNewer {
                app: "0.11.0".into(),
                cli: "0.10.0".into()
            }
        );
        // No --version answer: the daemon stands in for the CLI.
        assert_eq!(
            check("0.11.0", None, "0.10.0"),
            VersionCheck::AppNewer {
                app: "0.11.0".into(),
                cli: "0.10.0".into()
            }
        );
    }

    #[test]
    fn a_daemon_newer_than_the_app_means_relaunch() {
        assert_eq!(
            check("0.10.0", Some("0.11.0"), "0.11.0"),
            VersionCheck::AppOutdated {
                app: "0.10.0".into(),
                daemon: "0.11.0".into()
            }
        );
    }

    #[test]
    fn a_daemon_left_on_an_old_binary_means_restart_the_daemon() {
        assert_eq!(
            check("0.11.0", Some("0.11.0"), "0.10.0"),
            VersionCheck::DaemonStale {
                app: "0.11.0".into(),
                daemon: "0.10.0".into()
            }
        );
    }

    #[test]
    fn an_older_daemon_with_an_older_cli_means_upgrade_the_cli() {
        assert_eq!(
            check_protocol(6, Some(4), 4),
            ProtocolCheck::CliOlder {
                app: 6,
                cli: Some(4),
                daemon: 4
            }
        );
        // A CLI too old to report its protocol is older than this app.
        assert_eq!(
            check_protocol(6, None, 4),
            ProtocolCheck::CliOlder {
                app: 6,
                cli: None,
                daemon: 4
            }
        );
        // Newer than the daemon but still short of the app: a restart alone
        // wouldn't reach the app's protocol.
        assert_eq!(
            check_protocol(6, Some(5), 4),
            ProtocolCheck::CliOlder {
                app: 6,
                cli: Some(5),
                daemon: 4
            }
        );
    }

    #[test]
    fn an_older_daemon_under_a_current_cli_means_restart_the_daemon() {
        assert_eq!(
            check_protocol(6, Some(6), 4),
            ProtocolCheck::DaemonStale {
                app: 6,
                cli: 6,
                daemon: 4
            }
        );
    }

    #[test]
    fn a_newer_daemon_protocol_means_relaunch() {
        assert_eq!(
            check_protocol(6, Some(6), 7),
            ProtocolCheck::AppOutdated { app: 6, daemon: 7 }
        );
    }

    #[test]
    fn semver_precedence() {
        use Ordering::*;
        assert_eq!(compare("0.10.0", "0.9.9"), Some(Greater));
        assert_eq!(compare("v1.0.0", "1.0.0"), Some(Equal));
        assert_eq!(compare("1.0.0-rc.1", "1.0.0"), Some(Less));
        assert_eq!(compare("1.0.0-rc.2", "1.0.0-rc.10"), Some(Less));
        assert_eq!(compare("1.0.0-alpha", "1.0.0-alpha.1"), Some(Less));
        assert_eq!(compare("1.0.0-1", "1.0.0-alpha"), Some(Less));
        assert_eq!(compare("1.0.0+a", "1.0.0+b"), Some(Equal));
        assert_eq!(compare("1.0", "1.0.0"), None);
        assert_eq!(compare("x", "1.0.0"), None);
    }

    #[test]
    fn unparseable_versions_fall_back_to_upgrading_the_cli() {
        assert!(matches!(
            check("0.10.0", None, "dev"),
            VersionCheck::AppNewer { .. }
        ));
    }
}
