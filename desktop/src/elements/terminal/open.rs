//! Opening a Cmd+clicked link: a URL in the browser, a file path in the
//! user's editor at the line it names.
//!
//! The editor is the one the review panel's "Edit in" uses
//! (`shelbi_state::resolve_editor`: the hub config's `editor`, then
//! `$EDITOR`, then `vim`), launched through `views::external_edit` with the
//! line in that editor's syntax. A relative path resolves against the
//! session's working directory (`shelbi_term::links::path_candidates`); the
//! first candidate that exists opens. All file checks and the launch run off
//! the UI thread.

use std::path::PathBuf;

use gpui::App;
use shelbi_term::links::{path_candidates, LinkTarget};

use crate::views::external_edit;

/// Open `target`. `cwd` is the session's working directory.
pub fn open(target: &LinkTarget, cwd: Option<PathBuf>, cx: &mut App) {
    match target {
        LinkTarget::Url(url) => cx.open_url(url),
        LinkTarget::Path { path, line, column } => {
            let (path, line, column) = (path.clone(), *line, *column);
            cx.background_executor()
                .spawn(async move {
                    if let Err(e) = open_path(&path, line, column, cwd) {
                        tracing::warn!(path, error = %e, "could not open the clicked path");
                    }
                })
                .detach();
        }
    }
}

fn open_path(path: &str, line: Option<u32>, column: Option<u32>, cwd: Option<PathBuf>) -> Result<(), String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let editor = shelbi_state::resolve_editor();
    if let Some(mut child) = open_path_with(&editor, path, line, column, cwd, home)? {
        // Reap the editor when it exits, so it doesn't linger as a zombie.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    Ok(())
}

/// Open `path` in `editor`: the first candidate that exists, at `line` and
/// `column`. A directory opens in Finder. Returns a GUI editor's child.
fn open_path_with(
    editor: &str,
    path: &str,
    line: Option<u32>,
    column: Option<u32>,
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<Option<std::process::Child>, String> {
    let file = path_candidates(path, cwd.as_deref(), home.as_deref())
        .into_iter()
        .find(|p| p.exists())
        .ok_or_else(|| format!("no such file (from {})", cwd.map_or("?".into(), |c| c.display().to_string())))?;
    if file.is_dir() {
        return std::process::Command::new("open")
            .arg(&file)
            .status()
            .map(|_| None)
            .map_err(|e| e.to_string());
    }
    external_edit::spawn(&external_edit::launch_at(editor, &file, line, column))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clicked_path_opens_in_the_editor_at_its_line() {
        // A stand-in `code` (the basename picks VS Code's `--goto`) that
        // records its arguments.
        let dir = std::env::temp_dir().join(format!("shelbi-open-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("w/src")).unwrap();
        std::fs::write(dir.join("w/src/main.rs"), "fn main() {}\n").unwrap();
        let log = dir.join("argv.log");
        let editor = dir.join("bin/code");
        std::fs::write(&editor, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\n", log.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cwd = Some(dir.join("w"));
        let mut child = open_path_with(&editor.to_string_lossy(), "src/main.rs", Some(42), Some(5), cwd.clone(), None)
            .unwrap()
            .expect("a GUI editor runs as a child");
        assert!(child.wait().unwrap().success());
        let argv = std::fs::read_to_string(&log).unwrap();
        let file = dir.join("w/src/main.rs");
        assert_eq!(argv.trim(), format!("--goto {}:42:5", file.display()));

        // A path that doesn't exist opens nothing.
        let err = open_path_with(&editor.to_string_lossy(), "src/nope.rs", Some(1), None, cwd, None).unwrap_err();
        assert!(err.contains("no such file"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
