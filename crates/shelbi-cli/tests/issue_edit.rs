//! End-to-end tests for `shelbi issue edit`: `$EDITOR` mode on a GitHub board,
//! and the workflow-param flags on both backends.
//!
//! Each test runs the real binary and a real daemon in an isolated home, with a
//! stub `gh` on `PATH` standing in for one GitHub issue (#7, shelbi id `t`). The
//! stub keeps the issue's title and raw body in files, so a test can read what
//! a save really wrote, metadata block included, and a later read sees it.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");
const T0: &str = "2026-01-01T00:00:00+00:00";

const META_BODY: &str = "Prose.\n\n<!-- shelbi:begin -->\n```yaml\nbranch: feat/old\n\
                         priority: 2\nkeep: me\n```\n<!-- shelbi:end -->\n";

/// The stub `gh`. It answers the reads a single-issue get makes, and a PATCH
/// stores the `-f` fields it was given and counts itself.
const GH_STUB: &str = r#"#!/bin/sh
D="$FAKE_GH_DIR"
escaped() { sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' "$1" | awk '{printf "%s\\n", $0}'; }
title() { tr -d '\n' < "$D/title"; }
labels='{"name":"shelbi:id/t"},{"name":"shelbi:status/todo"}'
node() {
  printf '{"number":7,"title":"%s","state":"OPEN","stateReason":null,"createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z","body":"%s","assignees":{"nodes":[]},"labels":{"nodes":[%s]}}' \
    "$(title)" "$(escaped "$D/body")" "$labels"
}
rest() {
  printf '{"number":7,"title":"%s","state":"open","created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z","body":"%s","assignees":[],"labels":[%s]}' \
    "$(title)" "$(escaped "$D/body")" "$labels"
}
limit='"rateLimit":{"remaining":4999,"resetAt":"2030-01-01T00:00:00Z"}'
case "$*" in
  *graphql*IdSearch*)
    printf '{"data":{%s,"search":{"nodes":[{"number":7}]}}}\n' "$limit"; exit 0 ;;
  *graphql*IssuesByNumber*)
    printf '{"data":{%s,"repository":{"i0":%s}}}\n' "$limit" "$(node)"; exit 0 ;;
  *graphql*)
    printf '{"data":{%s,"repository":{"issue":%s}}}\n' "$limit" "$(node)"; exit 0 ;;
esac
patch=false; include=false
for a in "$@"; do
  case "$a" in PATCH) patch=true ;; --include|-i) include=true ;; esac
done
$include && printf 'HTTP/2.0 200 OK\r\n\r\n'
if $patch; then
  echo patch >> "$D/patches"
  while [ $# -gt 0 ]; do
    if [ "$1" = "-f" ]; then
      case "$2" in
        title=*) printf '%s' "${2#title=}" > "$D/title" ;;
        body=*) printf '%s' "${2#body=}" > "$D/body" ;;
      esac
      shift
    fi
    shift
  done
  rest; echo; exit 0
fi
case "$*" in
  */labels*|*/comments*|*rate_limit*) exit 0 ;;
esac
rest; echo
"#;

/// An isolated home with a real daemon. `github` picks the project's backend:
/// the stub's issue, or one `t.md` on disk.
struct Home {
    path: PathBuf,
    hub: PathBuf,
    control: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new(tag: &str, github: bool) -> Self {
        let pid = std::process::id();
        let path = PathBuf::from(format!("/tmp/shb-edit-{tag}-{pid}"));
        let _ = std::fs::remove_dir_all(&path);
        for dir in ["projects/p/tasks", "bin", "gh", "tmp"] {
            std::fs::create_dir_all(path.join(dir)).unwrap();
        }
        let tracker = if github {
            "issue_tracker:\n  backend: github\n  github:\n    repo: owner/repo\n"
        } else {
            ""
        };
        std::fs::write(
            path.join("projects/p.yaml"),
            format!(
                "name: p\nrepo: /tmp/p\ndefault_branch: main\n{tracker}\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/p\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n"
            ),
        )
        .unwrap();
        // Keep the daemon alive (the idle monitor exits when no project is open).
        std::fs::write(path.join("projects/p/state.json"), br#"{"open":true}"#).unwrap();
        write_script(&path.join("bin/gh"), GH_STUB);
        // The daemon overlays the login shell's environment onto its own, which
        // would put the real `gh` and token back. This shell reports this home's.
        write_script(&path.join("bin/login-shell"), "#!/bin/sh\nexec env\n");
        std::fs::write(path.join("gh/title"), "Title\n").unwrap();
        std::fs::write(path.join("gh/body"), META_BODY).unwrap();

        let hub = PathBuf::from(format!("/tmp/shb-edit-{tag}-{pid}-h.sock"));
        let control = PathBuf::from(format!("/tmp/shb-edit-{tag}-{pid}-c.sock"));
        let _ = std::fs::remove_file(&hub);
        let _ = std::fs::remove_file(&control);
        let mut home = Self {
            path,
            hub,
            control,
            daemon: None,
        };
        home.start_daemon();
        home
    }

    /// `cmd` with this home's environment, and no pane's leaking in.
    fn env(&self, cmd: &mut Command) {
        let path = format!(
            "{}:{}",
            self.path.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("PATH", path)
            .env("SHELBI_HOME", &self.path)
            .env("SHELBI_HUB_SOCK", &self.hub)
            .env("SHELBI_CONTROL_SOCK", &self.control)
            .env("FAKE_GH_DIR", self.path.join("gh"))
            .env("SHELL", self.path.join("bin/login-shell"))
            .env("GH_TOKEN", "test-token")
            .env("TMPDIR", self.path.join("tmp"))
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env("SHELBI_YES", "0")
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("VISUAL")
            .stdin(Stdio::null());
    }

    fn start_daemon(&mut self) {
        let mut cmd = Command::new(BIN);
        self.env(&mut cmd);
        let child = cmd
            .arg("daemon")
            .env("SHELBI_DAEMON_IDLE_GRACE_MS", "3000")
            .env("SHELBI_DAEMON_IDLE_POLL_MS", "500")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn shelbi daemon");
        self.daemon = Some(child);
        let start = Instant::now();
        while !self.control.exists() {
            assert!(start.elapsed() < Duration::from_secs(10), "control socket never came up");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Run `shelbi -p p issue edit <args>` with `$EDITOR` set to `editor`.
    fn edit(&self, editor: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        self.env(&mut cmd);
        cmd.args(["--project", "p", "issue", "edit"])
            .args(args)
            .env("EDITOR", editor)
            .output()
            .expect("run shelbi issue edit")
    }

    /// Install an editor script. `$1` is the file it's given, `$D` this home.
    fn editor(&self, body: &str) -> String {
        let path = self.path.join("bin/editor");
        write_script(
            &path,
            &format!("#!/bin/sh\nD=\"{}\"\nf=\"$1\"\n{body}\n", self.path.display()),
        );
        path.display().to_string()
    }

    fn gh_file(&self, name: &str) -> String {
        std::fs::read_to_string(self.path.join("gh").join(name)).unwrap_or_default()
    }

    fn patches(&self) -> usize {
        self.gh_file("patches").lines().count()
    }

    fn write_task(&self) {
        std::fs::write(
            self.task_path(),
            format!(
                "---\nid: t\ntitle: Title\ncolumn: backlog\npriority: 0\nprefers_machine: m\n\
                 keep: me\ncreated_at: {T0}\nupdated_at: {T0}\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    fn task_path(&self) -> PathBuf {
        self.path.join("projects/p/tasks/t.md")
    }

    fn task(&self) -> String {
        std::fs::read_to_string(self.task_path()).unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(&self.hub);
        let _ = std::fs::remove_file(&self.control);
    }
}

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[track_caller]
fn assert_ok(out: &Output) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// The reported bug: editor mode looked for a local file a GitHub board never
/// has. It now opens the issue's frontmatter and body, and saving writes both
/// back: the title, the prose, and the metadata block, keeping what wasn't
/// touched.
#[test]
fn editor_mode_round_trips_a_github_issue() {
    let home = Home::new("rt", true);
    let editor = home.editor(
        r#"cp "$f" "$D/seen.md"
awk '/^column:/ { print "task: parent" }
     /^keep:/ { next }
     { print }' "$f" |
  sed -e 's/^title: .*/title: New title/' \
      -e 's|^branch: .*|branch: feat/new|' \
      -e 's/^Prose\.$/New prose./' > "$f.new"
mv "$f.new" "$f""#,
    );
    let stdout = assert_ok(&home.edit(&editor, &["t"]));
    assert!(stdout.contains("t edited"), "{stdout}");

    let seen = std::fs::read_to_string(home.path.join("seen.md")).unwrap();
    assert!(seen.starts_with("---\nid: t\ntitle: Title\n"), "{seen}");
    assert!(seen.contains("branch: feat/old"), "{seen}");
    assert!(seen.contains("keep: me"), "{seen}");
    assert!(seen.ends_with("---\nProse.\n"), "{seen}");
    assert!(!seen.contains("shelbi:begin"), "the editor shows fields, not the block: {seen}");

    assert_eq!(home.gh_file("title"), "New title");
    let body = home.gh_file("body");
    assert!(body.starts_with("New prose.\n\n<!-- shelbi:begin -->"), "{body}");
    assert!(body.contains("branch: feat/new"), "{body}");
    assert!(body.contains("task: parent"), "{body}");
    assert!(body.contains("priority: 2"), "an untouched key survives: {body}");
    assert!(!body.contains("keep:"), "a removed param is gone: {body}");
    assert_eq!(home.patches(), 1, "one save is one write");
}

/// `EDITOR=true` is how the bug was reported: saving nothing writes nothing.
#[test]
fn an_unchanged_editor_session_writes_nothing() {
    let home = Home::new("noop", true);
    let stdout = assert_ok(&home.edit("true", &["t"]));
    assert!(stdout.contains("(no change)"), "{stdout}");
    assert_eq!(home.patches(), 0);
}

/// A field with its own command is refused, not silently dropped, and the
/// user's other edits aren't lost with it.
#[test]
fn an_editor_save_that_cant_land_keeps_the_edits() {
    let home = Home::new("keep", true);
    let editor = home.editor(
        r#"sed -e 's/^column: .*/column: done/' -e 's/^Prose\.$/My long edit./' "$f" > "$f.new"
mv "$f.new" "$f""#,
    );
    let out = home.edit(&editor, &["t"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(stderr.contains("`column` (use `shelbi issue move`)"), "{stderr}");
    assert_eq!(home.patches(), 0, "a refused save writes nothing");

    let kept = stderr
        .split("kept in ")
        .nth(1)
        .unwrap_or_else(|| panic!("no kept path: {stderr}"))
        .trim();
    let kept = std::fs::read_to_string(kept).unwrap();
    assert!(kept.contains("My long edit."), "{kept}");
}

/// A broken document is the same: nothing written, the text kept.
#[test]
fn an_unparseable_edit_keeps_the_edits() {
    let home = Home::new("bad", true);
    let editor = home.editor(r#"echo "no frontmatter any more" > "$f""#);
    let out = home.edit(&editor, &["t"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("can't read the edited issue"), "{stderr}");
    assert!(stderr.contains("kept in "), "{stderr}");
    assert_eq!(home.patches(), 0);
}

#[test]
fn param_flags_set_and_clear_on_github() {
    let home = Home::new("pgh", true);
    let stdout = assert_ok(&home.edit("false", &["t", "--param", "task=parent"]));
    assert!(stdout.contains("t edited (params)"), "{stdout}");
    let body = home.gh_file("body");
    assert!(body.contains("task: parent"), "{body}");
    assert!(body.contains("keep: me"), "{body}");
    assert!(body.starts_with("Prose."), "{body}");

    let stdout = assert_ok(&home.edit("false", &["t", "--no-param", "task"]));
    assert!(stdout.contains("t edited (params)"), "{stdout}");
    let body = home.gh_file("body");
    assert!(!body.contains("task:"), "{body}");
    assert!(body.contains("keep: me"), "{body}");
    assert_eq!(home.patches(), 2);

    let stdout = assert_ok(&home.edit("false", &["t", "--no-param", "task"]));
    assert!(stdout.contains("(no change)"), "clearing an unset param: {stdout}");
    assert_eq!(home.patches(), 2);
}

#[test]
fn param_flags_set_and_clear_on_the_file_backend() {
    let home = Home::new("pfs", false);
    home.write_task();

    let stdout = assert_ok(&home.edit("false", &["t", "--param", "task=parent"]));
    assert!(stdout.contains("t edited (params)"), "{stdout}");
    let task = home.task();
    assert!(task.contains("task: parent"), "{task}");
    assert!(task.contains("keep: me"), "{task}");

    let stdout = assert_ok(&home.edit("false", &["t", "--no-param", "task"]));
    assert!(stdout.contains("t edited (params)"), "{stdout}");
    let task = home.task();
    assert!(!task.contains("task:"), "{task}");
    assert!(task.contains("keep: me"), "{task}");
}

/// The daemon path, where a clear crosses the wire as JSON `null`.
#[test]
fn no_prefers_machine_clears_through_the_daemon() {
    let home = Home::new("npm", false);
    home.write_task();
    let stdout = assert_ok(&home.edit("false", &["t", "--no-prefers-machine"]));
    assert!(stdout.contains("t edited (prefers_machine)"), "{stdout}");
    assert!(!home.task().contains("prefers_machine"), "{}", home.task());
}

/// The file backend's editor mode still opens the task file itself.
#[test]
fn editor_mode_on_the_file_backend_opens_the_task_file() {
    let home = Home::new("fs", false);
    home.write_task();
    let editor = home.editor(r#"echo "$f" > "$D/opened""#);
    assert_ok(&home.edit(&editor, &["t"]));
    let opened = std::fs::read_to_string(home.path.join("opened")).unwrap();
    assert_eq!(Path::new(opened.trim()), home.task_path());
}

#[test]
fn editor_mode_still_reports_a_missing_issue() {
    let home = Home::new("miss", false);
    let out = home.edit("true", &["nope"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("issue `nope` not found"));
}
