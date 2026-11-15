//! The tools the user located outside the login shell's `PATH`
//! (`config.yaml`'s `tools:` block, [`ToolPaths`]).
//!
//! The startup requirements check (`shelbi_app::prereqs`) counts a located
//! binary as the tool and saves the location here ([`save`]). For Shelbi's
//! own commands to find it too, [`install_path_extension`] hands the located
//! binaries' directories to [`shelbi_core::login_env::set_path_extension`]:
//! every login environment Shelbi builds (the daemon's own, a session's, the
//! tool detection's) then ends its `PATH` with them, so `git`, `gh`,
//! `claude` and `codex` resolve to the located binary wherever Shelbi runs
//! them by name.

use std::path::{Path, PathBuf};

use shelbi_core::Result;

use crate::user_config::{load_user_config, update_user_config, ToolPaths};

/// The located tools, or none when `config.yaml` doesn't read.
pub fn located() -> ToolPaths {
    load_user_config().map(|c| c.tools).unwrap_or_default()
}

/// Save `path` as the binary named `name` (`git`, `gh`, `claude`, `codex`,
/// `brew`), or with `None` forget it.
pub fn save(name: &str, path: Option<&Path>) -> Result<()> {
    update_user_config(|c| c.tools.set(name, path.map(Path::to_path_buf))).map(|_| ())
}

/// The directories of the located binaries, in the block's order, each once.
pub fn dirs(paths: &ToolPaths) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in paths.paths().filter_map(|p| p.parent()) {
        if dir.is_absolute() && !out.iter().any(|d| d == dir) {
            out.push(dir.to_path_buf());
        }
    }
    out
}

/// Make every login environment this process builds end its `PATH` with the
/// located tools' directories (read fresh from `config.yaml` each time, so a
/// tool located while the process runs counts from its next spawn). Call once,
/// early, from a binary's `main`; tests never install it.
pub fn install_path_extension() {
    shelbi_core::login_env::set_path_extension(|| dirs(&located()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_are_the_parents_in_order_once_each() {
        let mut paths = ToolPaths::default();
        paths.set("gh", Some("/opt/x/bin/gh".into()));
        paths.set("codex", Some("/opt/x/bin/codex".into()));
        paths.set("git", Some("/usr/local/git/bin/git".into()));
        // Relative paths say nothing about a directory to search.
        paths.set("brew", Some("brew".into()));
        assert_eq!(dirs(&paths), vec![PathBuf::from("/usr/local/git/bin"), PathBuf::from("/opt/x/bin")]);
    }

    #[test]
    fn set_and_get_by_name() {
        let mut paths = ToolPaths::default();
        assert!(paths.is_empty());
        paths.set("claude", Some("/a/claude".into()));
        paths.set("nope", Some("/a/nope".into()));
        assert_eq!(paths.get("claude"), Some(&PathBuf::from("/a/claude")));
        assert_eq!(paths.get("nope"), None);
        assert!(!paths.is_empty());
        paths.set("claude", None);
        assert!(paths.is_empty());
    }

    #[test]
    fn the_block_round_trips_and_is_omitted_when_empty() {
        let cfg: crate::UserConfig = yaml_serde::from_str("tools:\n  gh: /x/gh\n").unwrap();
        assert_eq!(cfg.tools.gh, Some(PathBuf::from("/x/gh")));
        let text = yaml_serde::to_string(&crate::UserConfig::default()).unwrap();
        assert!(!text.contains("tools"), "{text}");
    }

    #[test]
    fn save_patches_config_yaml() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("shelbi-tool-paths-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        save("gh", Some(Path::new("/opt/gh/bin/gh"))).unwrap();
        assert_eq!(located().gh, Some(PathBuf::from("/opt/gh/bin/gh")));
        save("gh", None).unwrap();
        assert_eq!(located().gh, None);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
