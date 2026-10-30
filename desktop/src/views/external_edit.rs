//! "Open in editor" for an issue body.
//!
//! The body is written to a scratch file and handed to the configured editor
//! (`shelbi_state::resolve_editor`: the hub config's `editor`, then
//! `$EDITOR`, then `vim`). A watcher thread polls the file; every save sends
//! the new body back so the board submits it through `EditIssue`, the same
//! daemon path as an in-place edit.
//!
//! The terminal element's Cmd+click on `path:line` uses the same editor
//! resolution through [`launch_at`] and [`spawn`], opening the file at that
//! line in whichever editor it is.
//!
//! A window app has no terminal, so a terminal editor (vim, nvim, hx, ...)
//! runs in a new Terminal.app window via `osascript`; anything else (`code
//! --wait`, `zed`, `subl`) is spawned directly, and an app `editor:`
//! (`{ app: /Applications/Zed.app }`) opens the file with `open -a`. The watcher stops when a
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
    /// Spawn `program args...` directly (a GUI editor). The args include the
    /// file.
    Direct { program: String, args: Vec<String> },
    /// Run this shell command line in a new Terminal.app window.
    Terminal { command: String },
    /// Open the file in this app (`open -a <app> <file>`).
    App { app: PathBuf, file: PathBuf },
}

/// Decide how to open `file` in the editor `choice` (an `editor:` setting):
/// an app with `open -a` (no line: an app has no common syntax for one), a
/// command as [`launch_at`] does.
pub fn launch_choice(choice: &shelbi_state::ToolChoice, file: &Path, line: Option<u32>, column: Option<u32>) -> Launch {
    match choice {
        shelbi_state::ToolChoice::App { app } => Launch::App {
            app: app.clone(),
            file: file.to_path_buf(),
        },
        shelbi_state::ToolChoice::Command(editor) => launch_at(editor, file, line, column),
    }
}

/// Decide how to run `editor` on `file`, at `line` (and `column`) when given,
/// in each editor's own syntax: `+42` for vi-family and most terminal
/// editors, `file:42:5` for Helix, Zed, and Sublime, `--goto file:42:5` for
/// VS Code and its forks, `--line 42` for JetBrains IDEs, `-l 42` for
/// TextMate. An editor Shelbi doesn't know opens the file without a line.
/// Opening at a line drops VS Code's `--wait`/`-w` (nothing waits on it).
pub fn launch_at(editor: &str, file: &Path, line: Option<u32>, column: Option<u32>) -> Launch {
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vim").to_string();
    let mut args: Vec<String> = parts.map(str::to_string).collect();
    let base = program.rsplit('/').next().unwrap_or(&program).to_string();
    let file = file.to_string_lossy().into_owned();
    let at = |sep: &str| match (line, column) {
        (Some(l), Some(c)) => format!("{file}:{l}{sep}{c}"),
        (Some(l), None) => format!("{file}:{l}"),
        _ => file.clone(),
    };
    match (base.as_str(), line) {
        (_, None) => args.push(file.clone()),
        ("hx" | "helix", Some(_)) => args.push(at(":")),
        ("emacs" | "kak" | "micro", Some(l)) => {
            args.push(match column {
                Some(c) => format!("+{l}:{c}"),
                None => format!("+{l}"),
            });
            args.push(file.clone());
        }
        ("nano" | "pico", Some(l)) => {
            args.push(match column {
                Some(c) => format!("+{l},{c}"),
                None => format!("+{l}"),
            });
            args.push(file.clone());
        }
        (b, Some(l)) if TERMINAL_EDITORS.contains(&b) => {
            args.push(format!("+{l}"));
            args.push(file.clone());
        }
        ("code" | "code-insiders" | "cursor" | "codium" | "vscodium" | "windsurf", Some(_)) => {
            args.retain(|a| a != "--wait" && a != "-w");
            args.push("--goto".into());
            args.push(at(":"));
        }
        ("zed" | "subl" | "sublime_text", Some(_)) => args.push(at(":")),
        ("mate", Some(l)) => {
            args.extend(["-l".to_string(), l.to_string(), file.clone()]);
        }
        ("idea" | "rustrover" | "clion" | "webstorm" | "pycharm" | "goland" | "rubymine", Some(l)) => {
            args.extend(["--line".to_string(), l.to_string(), file.clone()]);
        }
        (_, Some(_)) => args.push(file.clone()),
    }
    if TERMINAL_EDITORS.contains(&base.as_str()) {
        let mut words = vec![shelbi_core::shell_escape(&program)];
        words.extend(args.iter().map(|a| shelbi_core::shell_escape(a)));
        Launch::Terminal {
            command: words.join(" "),
        }
    } else {
        Launch::Direct { program, args }
    }
}

/// Start `launch`: a GUI editor as a child process (returned, so a caller
/// can watch for its exit), a terminal editor in a new Terminal.app window
/// or a file in an app (`None`: its exit can't be observed).
pub fn spawn(launch: &Launch) -> Result<Option<std::process::Child>, String> {
    match launch {
        Launch::App { app, file } => {
            let status = std::process::Command::new("open")
                .arg("-a")
                .arg(app)
                .arg(file)
                .stdin(std::process::Stdio::null())
                .status()
                .map_err(|e| format!("opening {}: {e}", app.display()))?;
            if status.success() {
                Ok(None)
            } else {
                Err(format!("{} couldn't open {} ({status})", shelbi_state::tools::app_display_name(app), file.display()))
            }
        }
        Launch::Direct { program, args } => std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map(Some)
            .map_err(|e| format!("launching editor `{program}`: {e}")),
        Launch::Terminal { command } => {
            let script = format!(
                "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
                applescript_string(command)
            );
            let status = std::process::Command::new("osascript")
                .arg("-e")
                .arg(script)
                .status()
                .map_err(|e| format!("launching Terminal for `{command}`: {e}"))?;
            if status.success() {
                Ok(None)
            } else {
                Err(format!("Terminal couldn't start `{command}` ({status})"))
            }
        }
    }
}

/// Open `path` in the user's editor ([`shelbi_state::resolve_editor_choice`]),
/// the launch every Edit YAML and `edit:*` share. Hands back the editor's
/// name. A test opens nothing: it records the path ([`tests::opened`]).
pub fn open_file(path: &Path) -> Result<String, String> {
    #[cfg(test)]
    {
        tests::OPENED.lock().unwrap().push(path.to_path_buf());
        Ok("the test editor".into())
    }
    #[cfg(not(test))]
    {
        let (editor, _) = shelbi_state::resolve_editor_choice();
        spawn(&launch_choice(&editor, path, None, None))?;
        Ok(editor.display_name())
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
    let (editor, _) = shelbi_state::resolve_editor_choice();
    let mut child = spawn(&launch_choice(&editor, path, None, None))?;

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
pub(crate) mod tests {
    use super::*;

    /// The files [`open_file`] was asked to open, every test's (each
    /// checks for its own home's paths).
    pub(crate) static OPENED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

    /// How many times [`open_file`] has opened `path`.
    pub(crate) fn opened(path: &Path) -> usize {
        OPENED.lock().unwrap().iter().filter(|p| *p == path).count()
    }

    #[test]
    fn gui_editors_spawn_directly_and_terminal_editors_get_a_terminal() {
        let file = Path::new("/tmp/x y.md");
        assert_eq!(
            launch_at("code --wait", file, None, None),
            Launch::Direct {
                program: "code".into(),
                args: vec!["--wait".into(), "/tmp/x y.md".into()]
            }
        );
        match launch_at("/opt/homebrew/bin/nvim -u NONE", file, None, None) {
            Launch::Terminal { command } => {
                assert!(command.starts_with("/opt/homebrew/bin/nvim -u NONE "), "{command}");
                assert!(command.contains("'/tmp/x y.md'"), "the path is quoted: {command}");
            }
            other => panic!("nvim needs a terminal: {other:?}"),
        }
        assert!(matches!(launch_at("vim", file, None, None), Launch::Terminal { .. }));
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

    #[test]
    fn an_app_editor_opens_the_file_with_open_a() {
        let file = Path::new("/w/notes.md");
        let zed = shelbi_state::ToolChoice::app("/Applications/Zed.app");
        assert_eq!(
            launch_choice(&zed, file, Some(3), None),
            Launch::App {
                app: "/Applications/Zed.app".into(),
                file: file.into()
            }
        );
        assert_eq!(launch_choice(&"vim".into(), file, None, None), launch_at("vim", file, None, None));
    }

    #[test]
    fn launch_at_puts_the_line_in_each_editors_syntax() {
        let file = Path::new("/w/src/main.rs");
        let direct = |program: &str, args: &[&str]| Launch::Direct {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        // A terminal editor's words, each shell-escaped as the command line
        // carries them.
        let terminal = |words: &[&str]| Launch::Terminal {
            command: words
                .iter()
                .map(|w| shelbi_core::shell_escape(w))
                .collect::<Vec<_>>()
                .join(" "),
        };
        assert_eq!(launch_at("vim", file, Some(42), Some(5)), terminal(&["vim", "+42", "/w/src/main.rs"]));
        assert_eq!(
            launch_at("nvim -u NONE", file, Some(42), None),
            terminal(&["nvim", "-u", "NONE", "+42", "/w/src/main.rs"])
        );
        assert_eq!(launch_at("hx", file, Some(42), Some(5)), terminal(&["hx", "/w/src/main.rs:42:5"]));
        assert_eq!(launch_at("emacs", file, Some(42), Some(5)), terminal(&["emacs", "+42:5", "/w/src/main.rs"]));
        assert_eq!(launch_at("nano", file, Some(42), Some(5)), terminal(&["nano", "+42,5", "/w/src/main.rs"]));
        assert_eq!(
            launch_at("code --wait", file, Some(42), Some(5)),
            direct("code", &["--goto", "/w/src/main.rs:42:5"])
        );
        assert_eq!(launch_at("zed", file, Some(42), None), direct("zed", &["/w/src/main.rs:42"]));
        assert_eq!(launch_at("subl", file, Some(7), Some(1)), direct("subl", &["/w/src/main.rs:7:1"]));
        assert_eq!(launch_at("mate", file, Some(7), None), direct("mate", &["-l", "7", "/w/src/main.rs"]));
        assert_eq!(
            launch_at("rustrover", file, Some(7), None),
            direct("rustrover", &["--line", "7", "/w/src/main.rs"])
        );
        // No line, or an editor Shelbi doesn't know: just the file.
        assert_eq!(launch_at("code", file, None, None), direct("code", &["/w/src/main.rs"]));
        assert_eq!(launch_at("myeditor", file, Some(3), None), direct("myeditor", &["/w/src/main.rs"]));
    }
}
