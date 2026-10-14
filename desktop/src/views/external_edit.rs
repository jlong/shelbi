//! "Open in editor" for an issue body.
//!
//! The body is written to a scratch file and handed to the configured editor
//! (`shelbi_state::resolve_editor`: the hub config's `editor`, then
//! `$EDITOR`, then `vim`). A watcher thread polls the file; every save sends
//! the new body back so the board submits it through `EditIssue`, the same
//! daemon path as an in-place edit.
//!
//! A window app has no terminal, so a terminal editor (vim, nvim, hx, ...)
//! runs in a new Terminal.app window via `osascript`; anything else (`code
//! --wait`, `zed`, `subl`) is spawned directly. The watcher stops when a
//! directly spawned editor exits (after one last check), or after
//! [`WATCH_LIMIT`] for a Terminal-hosted one, whose exit it can't observe.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant, SystemTime};

/// How long a Terminal-hosted editor's file is watched for saves.
pub const WATCH_LIMIT: Duration = Duration::from_secs(4 * 60 * 60);

/// How often the watcher polls the file.
const POLL: Duration = Duration::from_millis(500);

/// Editors that need a terminal.
const TERMINAL_EDITORS: &[&str] = &[
    "vi", "vim", "nvim", "nano", "pico", "emacs", "hx", "helix", "micro", "kak", "joe", "ed",
    "mg", "ne",
];

/// How to launch the editor on a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Spawn `program args... file` directly (a GUI editor).
    Direct { program: String, args: Vec<String> },
    /// Run this shell command line in a new Terminal.app window.
    Terminal { command: String },
}

/// Decide how to run `editor` (a command line, possibly with flags) on
/// `file`.
pub fn launch_for(editor: &str, file: &Path) -> Launch {
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vim").to_string();
    let args: Vec<String> = parts.map(str::to_string).collect();
    let base = program.rsplit('/').next().unwrap_or(&program);
    if TERMINAL_EDITORS.contains(&base) {
        let mut words = vec![shelbi_core::shell_escape(&program)];
        words.extend(args.iter().map(|a| shelbi_core::shell_escape(a)));
        words.push(shelbi_core::shell_escape(&file.to_string_lossy()));
        Launch::Terminal {
            command: words.join(" "),
        }
    } else {
        Launch::Direct { program, args }
    }
}

/// The AppleScript string literal for `s`.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The scratch file an issue's body is edited in.
pub fn scratch_path(project: &str, id: &str) -> PathBuf {
    let safe = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect::<String>()
    };
    std::env::temp_dir()
        .join("shelbi-desktop")
        .join(format!("{}--{}.md", safe(project), safe(id)))
}

/// What the watcher reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    /// The file was saved with this body.
    Saved { id: String, body: String },
    /// The editor couldn't be launched.
    Failed { id: String, error: String },
}

/// Write `body` to the scratch file, launch the configured editor on it, and
/// watch it on a background thread, sending each save to `tx`.
pub fn open(project: &str, id: &str, body: &str, tx: Sender<EditorEvent>) {
    let path = scratch_path(project, id);
    let id = id.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        if let Err(e) = run(&path, &id, &body, &tx) {
            let _ = tx.send(EditorEvent::Failed { id, error: e });
        }
    });
}

fn run(path: &Path, id: &str, body: &str, tx: &Sender<EditorEvent>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    std::fs::write(path, body).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    let editor = shelbi_state::resolve_editor();
    let mut child = match launch_for(&editor, path) {
        Launch::Direct { program, args } => Some(
            std::process::Command::new(&program)
                .args(&args)
                .arg(path)
                .stdin(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("launching editor `{program}`: {e}"))?,
        ),
        Launch::Terminal { command } => {
            let script = format!(
                "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
                applescript_string(&command)
            );
            let status = std::process::Command::new("osascript")
                .arg("-e")
                .arg(script)
                .status()
                .map_err(|e| format!("launching Terminal for `{editor}`: {e}"))?;
            if !status.success() {
                return Err(format!("Terminal couldn't start `{editor}` ({status})"));
            }
            None
        }
    };

    let mut last = modified(path);
    let mut last_body = body.to_string();
    let started = Instant::now();
    loop {
        std::thread::sleep(POLL);
        let exited = match child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
            None => started.elapsed() > WATCH_LIMIT,
        };
        let now = modified(path);
        if now != last {
            last = now;
            if let Ok(new_body) = std::fs::read_to_string(path) {
                if new_body != last_body {
                    last_body = new_body.clone();
                    if tx
                        .send(EditorEvent::Saved {
                            id: id.to_string(),
                            body: new_body,
                        })
                        .is_err()
                    {
                        return Ok(());
                    }
                }
            }
        }
        if exited {
            return Ok(());
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_editors_spawn_directly_and_terminal_editors_get_a_terminal() {
        let file = Path::new("/tmp/x y.md");
        assert_eq!(
            launch_for("code --wait", file),
            Launch::Direct {
                program: "code".into(),
                args: vec!["--wait".into()]
            }
        );
        match launch_for("/opt/homebrew/bin/nvim -u NONE", file) {
            Launch::Terminal { command } => {
                assert!(command.starts_with("/opt/homebrew/bin/nvim -u NONE "), "{command}");
                assert!(command.contains("'/tmp/x y.md'"), "the path is quoted: {command}");
            }
            other => panic!("nvim needs a terminal: {other:?}"),
        }
        assert!(matches!(launch_for("vim", file), Launch::Terminal { .. }));
    }

    #[test]
    fn scratch_paths_are_safe_file_names() {
        let p = scratch_path("my/proj", "dt-1");
        assert_eq!(p.file_name().unwrap(), "my_proj--dt-1.md");
    }

    #[test]
    fn applescript_strings_escape_quotes_and_backslashes() {
        assert_eq!(applescript_string(r#"vim "a\b""#), r#""vim \"a\\b\"""#);
    }
}
