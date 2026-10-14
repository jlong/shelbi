//! `--print-version`: print the app's version and the CLI check, then exit,
//! with no window and no daemon.
//!
//! The headless end of bootstrap's version check, for scripts: the release
//! workflow's install test launches the installed bundle with
//! `open -W --stdout <file> Shelbi.app --args --print-version`, which proves
//! the bundle launches through LaunchServices and Gatekeeper and finds the
//! `shelbi` formula it depends on; `scripts/bundle.sh` runs it to check the
//! bundled binary. It stops short of the daemon: starting one on a build or
//! test machine has side effects, and the daemon's version follows the CLI's.
//!
//! Output (stdout):
//!
//! ```text
//! Shelbi 0.10.0
//! shelbi CLI: /opt/homebrew/bin/shelbi 0.10.0 (matches)
//! ```
//!
//! Exit status 0 when the CLI was found and its version equals the app's,
//! 1 otherwise (not found, didn't answer, or a different version).

use super::bootstrap::{find_cli, Platform, SystemPlatform};
use super::discovery::{CliLocation, CliNotFound};
use super::env;
use super::version::APP_VERSION;

/// Run the check against the real machine and return the exit status.
pub fn run() -> i32 {
    let mut platform = SystemPlatform::default();
    let env = env::resolve();
    let found = find_cli(&mut platform, &env).map(|cli| {
        let version = platform.cli_version(&cli.path, &env);
        (cli, version)
    });
    let (lines, status) = report(APP_VERSION, &found);
    for line in lines {
        println!("{line}");
    }
    status
}

/// The lines to print and the exit status.
pub fn report(
    app: &str,
    found: &Result<(CliLocation, Option<String>), CliNotFound>,
) -> (Vec<String>, i32) {
    let mut lines = vec![format!("Shelbi {app}")];
    let status = match found {
        Ok((cli, Some(version))) if version == app => {
            lines.push(format!(
                "shelbi CLI: {} {version} (matches)",
                cli.path.display()
            ));
            0
        }
        Ok((cli, Some(version))) => {
            lines.push(format!(
                "shelbi CLI: {} {version} (MISMATCH: the app is {app})",
                cli.path.display()
            ));
            1
        }
        Ok((cli, None)) => {
            lines.push(format!(
                "shelbi CLI: {} (did not answer --version)",
                cli.path.display()
            ));
            1
        }
        Err(not_found) => {
            let searched = not_found
                .searched
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("shelbi CLI: not found (searched {searched})"));
            1
        }
    };
    (lines, status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::discovery::CliSource;
    use std::path::PathBuf;

    fn cli() -> CliLocation {
        CliLocation {
            path: PathBuf::from("/opt/homebrew/bin/shelbi"),
            source: CliSource::LoginPath,
        }
    }

    #[test]
    fn a_matching_cli_exits_zero() {
        let (lines, status) = report("0.10.0", &Ok((cli(), Some("0.10.0".into()))));
        assert_eq!(status, 0);
        assert_eq!(
            lines,
            [
                "Shelbi 0.10.0",
                "shelbi CLI: /opt/homebrew/bin/shelbi 0.10.0 (matches)"
            ]
        );
    }

    #[test]
    fn a_different_or_silent_cli_exits_one_but_still_prints_the_app_version() {
        let (lines, status) = report("0.11.0", &Ok((cli(), Some("0.10.0".into()))));
        assert_eq!(status, 1);
        assert_eq!(lines[0], "Shelbi 0.11.0");
        assert!(lines[1].contains("MISMATCH"), "{lines:?}");

        let (lines, status) = report("0.11.0", &Ok((cli(), None)));
        assert_eq!(status, 1);
        assert!(lines[1].contains("did not answer"), "{lines:?}");
    }

    #[test]
    fn no_cli_lists_where_it_looked() {
        let not_found = CliNotFound {
            searched: vec![PathBuf::from("/opt/homebrew/bin/shelbi")],
            setting: None,
            config_error: None,
        };
        let (lines, status) = report("0.10.0", &Err(not_found));
        assert_eq!(status, 1);
        assert_eq!(
            lines[1],
            "shelbi CLI: not found (searched /opt/homebrew/bin/shelbi)"
        );
    }
}
