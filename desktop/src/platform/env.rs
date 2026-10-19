//! The environment the app runs `shelbi`, `git`, `gh`, and `ssh` under.
//!
//! An app launched from Finder inherits launchd's minimal environment
//! (`PATH=/usr/bin:/bin:/usr/sbin:/sbin`), not the user's shell setup. The
//! capture is the one the session process and the daemon already use,
//! [`shelbi_core::login_env::login_shell_env`] (`$SHELL -l -i -c env`, bounded,
//! cached for the process lifetime, falling back to the launcher's own
//! environment), so the app and the sessions it starts see the same `PATH`.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// A complete child-process environment.
pub type Env = BTreeMap<String, String>;

/// The variables that say *which* Shelbi the app talks to. The app's own
/// process decides these (its sockets are resolved from them), so a child it
/// starts must agree with it even when the login shell sets them differently:
/// otherwise the app would start a daemon on one socket and wait on another.
const SHELBI_LOCATION_VARS: [&str; 3] = ["SHELBI_HOME", "SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK"];

/// Capture the login-shell environment for the app's children. Blocks for as
/// long as the user's shell startup takes (bounded by the shared capture's
/// deadline), so call it off the UI thread.
pub fn resolve() -> Env {
    child_env(shelbi_core::login_env::login_shell_env(), |var| {
        std::env::var(var).ok()
    })
}

/// The login environment with the [`SHELBI_LOCATION_VARS`] replaced by the
/// app's own values (`own`), and removed where the app has none.
pub fn child_env(mut login: Env, own: impl Fn(&str) -> Option<String>) -> Env {
    for var in SHELBI_LOCATION_VARS {
        match own(var) {
            Some(value) => {
                login.insert(var.to_string(), value);
            }
            None => {
                login.remove(var);
            }
        }
    }
    login
}

/// The `PATH` entries of `env`, in order, skipping empty ones.
pub fn path_dirs(env: &Env) -> Vec<PathBuf> {
    env.get("PATH")
        .map(|path| {
            path.split(':')
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_apps_shelbi_location_wins_over_the_login_shells() {
        let login: Env = [
            ("PATH", "/opt/homebrew/bin:/usr/bin"),
            ("SHELBI_HOME", "/from/zshrc"),
            ("SHELBI_HUB_SOCK", "/from/zshrc/hub.sock"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let env = child_env(login, |var| {
            (var == "SHELBI_HOME").then(|| "/app/home".to_string())
        });
        assert_eq!(env["SHELBI_HOME"], "/app/home");
        assert!(
            !env.contains_key("SHELBI_HUB_SOCK"),
            "the app has none, so neither does the child"
        );
        assert_eq!(env["PATH"], "/opt/homebrew/bin:/usr/bin");
    }

    #[test]
    fn path_dirs_split_in_order_and_skip_empties() {
        let env: Env = [("PATH".to_string(), "/a::/b:".to_string())].into();
        assert_eq!(
            path_dirs(&env),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert!(path_dirs(&Env::new()).is_empty());
    }
}
