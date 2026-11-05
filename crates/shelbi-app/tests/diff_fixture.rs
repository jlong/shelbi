//! The diff foundation against real git: fixture repositories for every shape
//! a changeset can take, through local git, SSH (a stub `ssh` that does what
//! ssh does to an argv), a far daemon's answer over JSON, and a whole session.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use shelbi_app::diff::backend::{answer, DiffBackend, DiffOp, DiffReply, DiffRequest, DiffTarget, HostBackend, TargetKind};
use shelbi_app::diff::comments::{Anchor, Author, Comment, CommentOp};
use shelbi_app::diff::git::{collect, HostGit, RawChangeset};
use shelbi_app::diff::model::{Changeset, CommitScope, Eol, FileDiff, FileStatus, LineKind, Side};
use shelbi_app::diff::store::{DiffStore, ViewedOp};
use shelbi_app::diff::{DiffOptions, DiffSession, Row};
use shelbi_core::Host;
use shelbi_state::keymap::{validate_keymaps_yaml, KeyChord};

/// A git repository in a temp directory, on a feature branch cut from `main`.
struct Repo {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn git_in(path: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Repo {
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let repo = Repo { _dir: dir, path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "Fixture"]);
        repo.git(&["config", "user.email", "fixture@example.com"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.path, args)
    }

    fn write(&self, path: &str, content: impl AsRef<[u8]>) {
        let full = self.path.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    /// Base files on `main`, then the feature branch.
    fn with_base(files: &[(&str, &[u8])]) -> Repo {
        let repo = Repo::new();
        for (path, content) in files {
            repo.write(path, content);
        }
        repo.commit("base");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo
    }

    fn runner(&self) -> HostGit {
        HostGit { host: Host::Local, worktree: self.path.to_string_lossy().into_owned() }
    }

    fn read(&self, kind: TargetKind, scope: &CommitScope) -> RawChangeset {
        collect(&self.runner(), kind, "main", scope).unwrap()
    }

    fn review(&self) -> Changeset {
        self.read(TargetKind::Review, &CommitScope::All).parse()
    }

    fn workspace(&self) -> Changeset {
        self.read(TargetKind::Workspace, &CommitScope::All).parse()
    }
}

fn file<'a>(changeset: &'a Changeset, path: &str) -> &'a FileDiff {
    changeset
        .files
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no {path} in {:?}", changeset.files.iter().map(|f| &f.path).collect::<Vec<_>>()))
}

const TEN: &[u8] = b"1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";

#[test]
fn adds_deletes_edits_and_counts() {
    let repo = Repo::with_base(&[("keep.txt", TEN), ("gone.txt", b"bye\n")]);
    repo.write("keep.txt", "1\n2\n3\n4\nfive\n6\n7\n8\n9\n10\n11\n");
    repo.write("new/added.txt", "hello\nworld\n");
    fs::remove_file(repo.path.join("gone.txt")).unwrap();
    repo.commit("work");

    let cs = repo.review();
    assert_eq!(cs.files.len(), 3);
    assert_eq!((cs.additions, cs.deletions), (4, 2));

    let keep = file(&cs, "keep.txt");
    assert_eq!((keep.status, keep.additions, keep.deletions), (FileStatus::Modified, 2, 1));
    let added = file(&cs, "new/added.txt");
    assert_eq!((added.status, added.additions, added.deletions), (FileStatus::Added, 2, 0));
    let gone = file(&cs, "gone.txt");
    assert_eq!((gone.status, gone.additions, gone.deletions), (FileStatus::Deleted, 0, 1));

    // Numbers on both sides, and the changed word found.
    let lines: Vec<_> = keep.hunks.iter().flat_map(|h| &h.lines).collect();
    let five = lines.iter().find(|l| l.text == "five").unwrap();
    assert_eq!((five.kind, five.old, five.new), (LineKind::Added, None, Some(5)));
    let old = lines.iter().find(|l| l.text == "5").unwrap();
    assert_eq!((old.kind, old.old, old.new), (LineKind::Deleted, Some(5), None));
    let ctx = lines.iter().find(|l| l.text == "4").unwrap();
    assert_eq!((ctx.old, ctx.new), (Some(4), Some(4)));
}

#[test]
fn renames_keep_both_paths_and_their_edits() {
    let repo = Repo::with_base(&[("src/old name.txt", TEN), ("plain.txt", b"same\nsame\nsame\nsame\n")]);
    repo.git(&["mv", "src/old name.txt", "src/new name.txt"]);
    repo.git(&["mv", "plain.txt", "moved.txt"]);
    repo.write("src/new name.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    repo.commit("rename");
    let cs = repo.review();
    let edited = file(&cs, "src/new name.txt");
    assert_eq!(edited.status, FileStatus::Renamed);
    assert_eq!(edited.old_path.as_deref(), Some("src/old name.txt"));
    assert!(edited.similarity.unwrap() > 50);
    assert_eq!((edited.additions, edited.deletions), (1, 1));
    let moved = file(&cs, "moved.txt");
    assert_eq!((moved.status, moved.old_path.as_deref(), moved.hunks.len()), (FileStatus::Renamed, Some("plain.txt"), 0));
}

#[test]
fn binary_files_have_no_lines() {
    let repo = Repo::with_base(&[("image.png", b"\x89PNG\0\x01\x02")]);
    repo.write("image.png", b"\x89PNG\0\x03\x04\x05");
    repo.write("added.bin", b"\0\0\0");
    repo.commit("bytes");
    let cs = repo.review();
    let image = file(&cs, "image.png");
    assert!(image.binary && image.hunks.is_empty());
    assert_eq!(image.status, FileStatus::Modified);
    assert_eq!(file(&cs, "added.bin").status, FileStatus::Added);
    assert!(file(&cs, "added.bin").binary);

    // The fingerprint still sees the bytes change.
    let before = image.fingerprint;
    repo.write("image.png", b"\x89PNG\0\x06");
    repo.commit("more bytes");
    assert_ne!(file(&repo.review(), "image.png").fingerprint, before);
}

#[test]
fn crlf_and_missing_final_newlines() {
    let repo = Repo::with_base(&[("crlf.txt", b"one\r\ntwo\r\n"), ("eof.txt", b"a\nb\n"), ("mixed.txt", b"same\r\nother\n")]);
    repo.write("crlf.txt", "one\r\nTWO\r\n");
    repo.write("eof.txt", "a\nb");
    repo.write("mixed.txt", "same\nother\n");
    repo.commit("line endings");
    let cs = repo.review();

    let crlf: Vec<_> = file(&cs, "crlf.txt").hunks.iter().flat_map(|h| &h.lines).collect();
    assert!(crlf.iter().all(|l| l.eol == Eol::Crlf));
    assert!(crlf.iter().all(|l| !l.text.ends_with('\r')));
    assert_eq!(crlf.iter().find(|l| l.kind == LineKind::Added).unwrap().text, "TWO");

    let eof: Vec<_> = file(&cs, "eof.txt").hunks.iter().flat_map(|h| &h.lines).collect();
    let (old, new) = (
        eof.iter().find(|l| l.kind == LineKind::Deleted).unwrap(),
        eof.iter().find(|l| l.kind == LineKind::Added).unwrap(),
    );
    assert_eq!((old.text.as_str(), old.eol), ("b", Eol::Lf));
    assert_eq!((new.text.as_str(), new.eol), ("b", Eol::None));

    // A change that only touches a line ending still has both lines to show.
    let mixed: Vec<_> = file(&cs, "mixed.txt").hunks.iter().flat_map(|h| &h.lines).collect();
    let changed: Vec<_> = mixed.iter().filter(|l| l.kind != LineKind::Context).collect();
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[0].text, changed[1].text);
    assert_ne!(changed[0].eol, changed[1].eol);
}

#[test]
fn an_empty_diff_is_no_files_not_an_error() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    let cs = repo.review();
    assert!(cs.files.is_empty());
    assert_eq!(cs.base, cs.head);
    assert!(cs.commits.is_empty());
    assert!(repo.workspace().files.is_empty());
}

#[test]
fn empty_files_mode_changes_and_awkward_paths() {
    let repo = Repo::with_base(&[("script.sh", b"#!/bin/sh\n")]);
    repo.write("empty file.txt", "");
    repo.write("sp\u{e9}cial/fil\u{e9}.txt", "x\n");
    repo.write("a b/c d.txt", "x\n");
    let mode = fs::metadata(repo.path.join("script.sh")).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::mode(&mode);
    fs::set_permissions(repo.path.join("script.sh"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    repo.commit("shapes");
    let cs = repo.review();
    assert_eq!(file(&cs, "empty file.txt").status, FileStatus::Added);
    assert!(file(&cs, "empty file.txt").hunks.is_empty());
    assert_eq!(file(&cs, "sp\u{e9}cial/fil\u{e9}.txt").additions, 1);
    assert_eq!(file(&cs, "a b/c d.txt").additions, 1);
    assert_eq!(file(&cs, "script.sh").mode_change, Some(("100644".into(), "100755".into())));
}

#[test]
fn non_utf8_content_reads_as_replacement_characters() {
    let repo = Repo::with_base(&[("latin.txt", b"caf\xe9 one\ntwo\n")]);
    repo.write("latin.txt", b"caf\xe9 ONE\ntwo\n");
    repo.commit("bytes");
    let cs = repo.review();
    let lines: Vec<_> = file(&cs, "latin.txt").hunks.iter().flat_map(|h| &h.lines).collect();
    assert!(lines.iter().any(|l| l.text == "caf\u{fffd} ONE"));
}

#[test]
fn the_users_git_config_doesnt_change_what_we_parse() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    repo.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    repo.commit("edit");
    for (key, value) in [
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("diff.external", "false"),
        ("diff.renames", "false"),
        ("core.quotePath", "false"),
    ] {
        repo.git(&["config", key, value]);
    }
    let cs = repo.review();
    let a = file(&cs, "a.txt");
    assert_eq!((a.additions, a.deletions), (1, 1));
    assert_eq!(a.hunks[0].lines.iter().find(|l| l.kind == LineKind::Added).unwrap().text, "ten");
}

#[test]
fn a_workspace_includes_uncommitted_work_and_new_files_and_a_review_doesnt() {
    let repo = Repo::with_base(&[("a.txt", TEN), ("b.txt", b"b\n"), ("c.txt", b"c\n")]);
    repo.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    repo.commit("committed");
    repo.write("b.txt", "staged\n");
    repo.git(&["add", "b.txt"]);
    repo.write("c.txt", "unstaged\n");
    repo.write("new.txt", "brand new\n");
    repo.write("dir/nested new.txt", "also new\n");
    repo.write(".gitignore", "ignored.txt\n");
    repo.write("ignored.txt", "nope\n");

    let review = repo.review();
    assert_eq!(review.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt"]);

    let ws = repo.workspace();
    let paths: Vec<&str> = ws.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, [".gitignore", "a.txt", "b.txt", "c.txt", "dir/nested new.txt", "new.txt"]);
    assert!(file(&ws, "new.txt").untracked);
    assert_eq!(file(&ws, "new.txt").status, FileStatus::Added);
    assert_eq!(file(&ws, "dir/nested new.txt").additions, 1);
    assert!(!file(&ws, "a.txt").untracked);

    let uncommitted = repo.read(TargetKind::Workspace, &CommitScope::Uncommitted).parse();
    let paths: Vec<&str> = uncommitted.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, [".gitignore", "b.txt", "c.txt", "dir/nested new.txt", "new.txt"]);
    assert_eq!(uncommitted.base, uncommitted.head);
}

#[test]
fn too_many_new_files_are_counted_not_read() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    for i in 0..(shelbi_app::diff::git::MAX_UNTRACKED + 7) {
        repo.write(&format!("gen/{i:04}.txt"), "x\n");
    }
    let raw = repo.read(TargetKind::Workspace, &CommitScope::All);
    assert_eq!(raw.omitted_untracked, 7);
    assert_eq!(raw.parse().files.len(), shelbi_app::diff::git::MAX_UNTRACKED);
}

#[test]
fn an_oversized_new_file_is_listed_without_its_body() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    repo.write("huge.log", "line\n".repeat(shelbi_app::diff::git::MAX_UNTRACKED_PATCH / 4));
    let cs = repo.workspace();
    let huge = file(&cs, "huge.log");
    assert!(huge.omitted.is_some() && huge.hunks.is_empty() && huge.untracked);
}

#[test]
fn one_commit_at_a_time() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    repo.write("a.txt", "one\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    let first = repo.commit("first change");
    repo.write("b.txt", "b\n");
    let second = repo.commit("second change");

    let all = repo.review();
    assert_eq!(all.files.len(), 2);
    assert_eq!(all.commits.len(), 2);
    assert_eq!(all.commits[0].sha, second, "newest first");
    assert_eq!(all.commits[1].subject, "first change");
    assert_eq!(all.commits[1].author, "Fixture");
    assert!(all.commits[1].time > 0);

    let one = repo.read(TargetKind::Review, &CommitScope::Commit(first.clone())).parse();
    assert_eq!(one.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt"]);
    assert_eq!(one.scope, CommitScope::Commit(first.clone()));
    let two = repo.read(TargetKind::Review, &CommitScope::Commit(second)).parse();
    assert_eq!(two.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["b.txt"]);
    // The list stays the branch's whichever commit is on show.
    assert_eq!(two.commits.len(), 2);

    let err = collect(&repo.runner(), TargetKind::Review, "main", &CommitScope::Commit("--output=/tmp/x".into()));
    assert!(err.unwrap_err().contains("isn't a commit id"));
}

#[test]
fn a_root_commit_diffs_against_nothing() {
    let repo = Repo::new();
    repo.write("a.txt", "x\n");
    let root = repo.commit("root");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    let cs = repo.read(TargetKind::Review, &CommitScope::Commit(root)).parse();
    assert_eq!(cs.files[0].status, FileStatus::Added);
}

#[test]
fn the_fork_point_follows_a_newer_origin_and_a_newer_local_base() {
    // A branch rebased onto an `origin/main` the local `main` hasn't caught up
    // to shows only its own commit.
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    repo.git(&["checkout", "-q", "main"]);
    repo.write("landed.txt", "someone else's\n");
    let landed = repo.commit("landed on origin");
    repo.git(&["update-ref", "refs/remotes/origin/main", &landed]);
    repo.git(&["reset", "-q", "--hard", "HEAD~1"]);
    repo.git(&["checkout", "-q", "feature"]);
    repo.git(&["reset", "-q", "--hard", &landed]);
    repo.write("mine.txt", "mine\n");
    repo.commit("mine");
    let cs = repo.review();
    assert_eq!(cs.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["mine.txt"]);
    assert_eq!(cs.commits.len(), 1);

    // A base with a local commit origin doesn't have.
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    let start = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["update-ref", "refs/remotes/origin/main", &start]);
    repo.git(&["checkout", "-q", "main"]);
    repo.write("local.txt", "local\n");
    repo.commit("local only");
    repo.git(&["checkout", "-q", "-B", "feature"]);
    repo.write("mine.txt", "mine\n");
    repo.commit("mine");
    let cs = repo.review();
    assert_eq!(cs.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["mine.txt"]);

    // No base at all says so.
    let repo = Repo::new();
    repo.write("a.txt", "x\n");
    repo.commit("only");
    let err = collect(&repo.runner(), TargetKind::Review, "develop", &CommitScope::All).unwrap_err();
    assert!(err.contains("develop"), "{err}");
}

#[test]
fn blame_reads_lines_and_says_whats_uncommitted() {
    let repo = Repo::with_base(&[("a.txt", b"one\ntwo\n")]);
    repo.write("a.txt", "one\nTWO\nthree\n");
    let commit = repo.commit("edit");
    repo.write("a.txt", "one\nTWO\nthree\nfour\n");
    let lines = shelbi_app::diff::git::blame(&repo.runner(), "a.txt", None).unwrap();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[1].author, "Fixture");
    assert_eq!(lines[1].summary, "edit");
    assert!(lines[3].uncommitted);
    assert!(!lines[0].uncommitted);
    let at = shelbi_app::diff::git::blame(&repo.runner(), "a.txt", Some(&commit)).unwrap();
    assert_eq!(at.len(), 3);
    assert!(shelbi_app::diff::git::blame(&repo.runner(), "a.txt", Some("HEAD; rm -rf")).is_err());
}

/// A `ssh` on `PATH` that does what ssh does: drops its options and the host,
/// and hands the rest to a shell as one line. A command that survives this has
/// survived the wire.
fn fake_ssh() {
    static ONCE: OnceLock<PathBuf> = OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("shelbi-diff-fake-ssh-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("ssh");
        fs::write(&script, "#!/bin/sh\nwhile [ \"$1\" != \"--\" ]; do shift; done\nshift\nexec /bin/sh -c \"$*\"\n").unwrap();
        fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", dir.display()));
        dir
    });
}

/// Set once, before any reader: this binary's Shelbi home.
fn scratch_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("shelbi-diff-home-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // Set once, before any reader. `SHELBI_ROOT` wins over `SHELBI_HOME`,
        // so a pane that exports it can't leak in.
        std::env::set_var("SHELBI_ROOT", &dir);
        fs::create_dir_all(shelbi_state::projects_dir().unwrap()).unwrap();
        dir
    })
}

#[test]
fn over_ssh_the_same_read_gives_the_same_changeset() {
    fake_ssh();
    scratch_home();
    let repo = Repo::with_base(&[("a b.txt", TEN), ("gone.txt", b"x\n")]);
    repo.write("a b.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    fs::remove_file(repo.path.join("gone.txt")).unwrap();
    repo.commit("edit");
    repo.write("uncommitted new.txt", "new\n");

    let local = repo.read(TargetKind::Workspace, &CommitScope::All);
    let ssh = HostGit { host: Host::Ssh { host: "fixture-host".into() }, worktree: repo.path.to_string_lossy().into_owned() };
    let remote = collect(&ssh, TargetKind::Workspace, "main", &CommitScope::All).unwrap();
    assert_eq!(remote, local);
    assert_eq!(remote.parse().files.len(), 3);
    assert_eq!(shelbi_app::diff::git::blame(&ssh, "a b.txt", None).unwrap().len(), 10);
}

/// A registered project whose `name` workspace is `repo`'s worktree. The
/// machine's `work_dir` is laid out so `<work_dir>/.shelbi/wt/<workspace>` is
/// the repo.
fn project_on(name: &str, workspace: &str, repo: &Repo, machine: &str) {
    scratch_home();
    let work_dir = tempfile::tempdir().unwrap().keep();
    let wt = work_dir.join(".shelbi").join("wt");
    fs::create_dir_all(&wt).unwrap();
    std::os::unix::fs::symlink(&repo.path, wt.join(workspace)).unwrap();
    fs::write(
        shelbi_state::projects_dir().unwrap().join(format!("{name}.yaml")),
        format!(
            "repo: {repo}\nbase_branch: main\nagent_runners:\n  claude: {{ command: claude, flags: [] }}\norchestrator: {{ runner: claude }}\nmachines:\n  - {machine}\nworkspaces:\n  - {{ name: {workspace}, machine: hub, runner: claude }}\n",
            repo = repo.path.display(),
            machine = format_args!("{{ name: hub, {machine}, work_dir: {} }}", work_dir.display()),
        ),
    )
    .unwrap();
}

#[test]
fn a_far_daemons_answer_survives_the_wire() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    repo.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    let sha = repo.commit("edit");
    project_on("far-project", "dev", &repo, "kind: local");
    let target = DiffTarget {
        project: "far-project".into(),
        workspace: "dev".into(),
        kind: TargetKind::Review,
        task: Some("t-1".into()),
    };
    let ask = |op: DiffOp| -> DiffReply {
        let agent: shelbi_app::diff::AgentSend = Arc::new(|target, message| {
            assert_eq!(target.workspace, "dev");
            assert!(message.contains("Review comments"), "{message}");
            Ok(shelbi_app::diff::AgentDelivery::Queued)
        });
        let reply = shelbi_app::far::Reply::Diff(answer(&DiffRequest { target: target.clone(), op }, agent));
        let wire = serde_json::to_string(&reply).unwrap();
        match serde_json::from_str(&wire).unwrap() {
            shelbi_app::far::Reply::Diff(reply) => reply,
            other => panic!("{other:?}"),
        }
    };

    let reply = ask(DiffOp::Changeset(CommitScope::All));
    let DiffReply::Changeset(Ok(raw)) = reply else { panic!("{reply:?}") };
    assert_eq!(raw, repo.read(TargetKind::Review, &CommitScope::All));
    let DiffReply::Changeset(Ok(raw)) = ask(DiffOp::Changeset(CommitScope::Commit(sha))) else { panic!() };
    assert_eq!(raw.parse().files[0].additions, 1);
    let DiffReply::Blame(Ok(blame)) = ask(DiffOp::Blame { path: "a.txt".into(), rev: None }) else { panic!() };
    assert_eq!(blame.len(), 10);

    // Comments live on the far machine, keyed by task.
    let DiffReply::Comments(Ok(doc)) = ask(DiffOp::ApplyComment(CommentOp::Add(comment("c1", "a.txt", 10, "ten")))) else {
        panic!()
    };
    assert_eq!(doc.comments.len(), 1);
    let DiffReply::Comments(Ok(doc)) = ask(DiffOp::Comments) else { panic!() };
    assert_eq!(doc.comments[0].id, "c1");
    let on_disk = shelbi_state::project_dir("far-project").unwrap().join("diff/t-1/comments.json");
    assert!(on_disk.exists(), "{}", on_disk.display());
    assert!(!on_disk.to_string_lossy().contains(".claude"));
    let DiffReply::Viewed(Ok(doc)) = ask(DiffOp::ApplyViewed(ViewedOp::Mark { path: "a.txt".into(), fingerprint: 3 })) else {
        panic!()
    };
    assert!(doc.is_viewed("a.txt", 3));

    // Reject reaches them by task alone: its reason starts from what's unsent,
    // and a reject that lands marks only what its reason still carries.
    let access = shelbi_app::diff::comment_access("far-project", "t-1", None);
    let reason = access.reject_reason().unwrap().expect("c1 is unsent");
    assert!(reason.contains("[thread c1] a.txt:10"), "{reason}");
    access.mark_sent_in("Nothing of theirs in here").unwrap();
    assert!(!access.comments().unwrap().comments[0].sent);
    access.mark_sent_in(&format!("Please redo this.\n\n{reason}")).unwrap();
    assert!(access.comments().unwrap().comments[0].sent);
    assert_eq!(access.reject_reason().unwrap(), None);

    // The agent's session and a revert's refusal answer across it too.
    let reply = ask(DiffOp::SendToAgent("Review comments on this branch:".into()));
    assert_eq!(reply, DiffReply::Sent(Ok(shelbi_app::diff::AgentDelivery::Queued)));
    let spec = shelbi_app::diff::revert::RevertSpec {
        scope: CommitScope::All,
        path: "a.txt".into(),
        hunk: None,
        fingerprint: 0,
    };
    let DiffReply::RevertPlanned(Err(e)) = ask(DiffOp::PlanRevert(spec)) else { panic!() };
    assert!(e.contains("checkout of the branch"), "{e}");

    // What isn't there says so. A task's comments don't need the workspace.
    let nowhere = DiffTarget { workspace: "nope".into(), ..target.clone() };
    let no_agent = no_agent();
    let bad = DiffRequest { target: nowhere.clone(), op: DiffOp::Viewed };
    let DiffReply::Viewed(Err(e)) = answer(&bad, no_agent.clone()) else { panic!() };
    assert!(e.contains("nope"), "{e}");
    let still = DiffRequest { target: nowhere, op: DiffOp::Comments };
    let DiffReply::Comments(Ok(doc)) = answer(&still, no_agent) else { panic!() };
    assert_eq!(doc.comments[0].id, "c1");
}

#[test]
fn a_workspace_on_another_machine_reads_through_its_host() {
    fake_ssh();
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    repo.write("b.txt", "uncommitted\n");
    project_on("ssh-project", "dev", &repo, "kind: ssh, host: fixture-host");
    let target = DiffTarget {
        project: "ssh-project".into(),
        workspace: "dev".into(),
        kind: TargetKind::Workspace,
        task: None,
    };
    assert_eq!(target.state_key(), "ws-dev");
    let backend = HostBackend::for_target(&target, no_agent()).unwrap();
    let raw = backend.changeset(&CommitScope::All).unwrap();
    assert_eq!(raw.untracked, ["b.txt"]);
}

fn comment(id: &str, path: &str, line: u32, code: &str) -> Comment {
    Comment {
        id: id.into(),
        anchor: Anchor { path: path.into(), side: Side::New, start: line, end: line, code: vec![code.into()] },
        author: Author::human("jlong"),
        body: "why?".into(),
        created_at: chrono::Utc::now(),
        replies: Vec::new(),
        sent: false,
    }
}

fn key(spec: &str) -> KeyChord {
    KeyChord::parse(spec).unwrap()
}

/// Poll until `done`, or fail after a while.
fn wait(session: &mut DiffSession, what: &str, done: impl Fn(&DiffSession) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        session.poll();
        if done(session) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn target_of(kind: TargetKind) -> DiffTarget {
    DiffTarget { project: "fixture".into(), workspace: "alpha".into(), kind, task: Some("t".into()) }
}

fn no_agent() -> shelbi_app::diff::AgentSend {
    Arc::new(|_, _| Err("no agent in this test".to_string()))
}

fn options(watch: Option<Duration>) -> (DiffOptions, Arc<AtomicUsize>) {
    let woken = Arc::new(AtomicUsize::new(0));
    let count = woken.clone();
    (
        DiffOptions {
            author: Author::human("jlong"),
            target: target_of(TargetKind::Workspace),
            watch,
            wake: Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
        },
        woken,
    )
}

fn session_on(repo: &Repo, store: &Path, kind: TargetKind, watch: Option<Duration>) -> (DiffSession, Arc<AtomicUsize>) {
    let backend: Arc<dyn DiffBackend> = Arc::new(HostBackend::over(
        Host::Local,
        repo.path.to_string_lossy().into_owned(),
        target_of(kind),
        "main",
        DiffStore::at(store),
        no_agent(),
    ));
    let (options, woken) = options(watch);
    (DiffSession::open(move || Ok(backend), options), woken)
}

#[test]
fn a_session_loads_highlights_comments_and_restarts_with_them() {
    let keymap = validate_keymaps_yaml("", None).0.diff;
    let repo = Repo::with_base(&[("src/x.rs", b"fn one() {}\nfn two() {}\n")]);
    repo.write("src/x.rs", "fn one() {}\nfn three() {}\n");
    repo.write("notes.txt", "a\n");
    let first = repo.commit("edit");
    repo.write("later.txt", "b\n");
    repo.commit("later");
    let store = tempfile::tempdir().unwrap();

    let (mut s, woken) = session_on(&repo, store.path(), TargetKind::Review, None);
    assert!(s.state().loading(), "open returns before the read does");
    wait(&mut s, "the changeset", |s| !s.state().loading());
    assert!(woken.load(Ordering::SeqCst) > 0, "the client was woken");
    assert_eq!(s.state().changeset().files.len(), 3);
    wait(&mut s, "highlights", |s| s.state().highlights(2).is_some());

    // Write a comment and a viewed mark, and let them save.
    s.state_mut().goto_file(2);
    s.key(key("] h"), &keymap);
    s.key(key("j"), &keymap);
    let on = s.state().cursor();
    assert!(matches!(s.state().table().rows[on], Row::Line { file: 2, .. }));
    s.key(key("enter"), &keymap);
    for c in "a note".chars() {
        s.key(KeyChord::new(shelbi_state::keymap::Key::Char(c), shelbi_state::keymap::Mods::NONE), &keymap);
    }
    s.key(key("enter"), &keymap);
    s.key(key("m"), &keymap);
    let stored = DiffStore::at(store.path());
    let deadline = Instant::now() + Duration::from_secs(10);
    while (stored.comments().unwrap().comments.is_empty() || stored.viewed().unwrap().files.is_empty())
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(stored.comments().unwrap().comments[0].body, "a note");
    assert_eq!(s.store_error(), None);

    // A refresh still answers when nothing changed.
    s.refresh();
    wait(&mut s, "the refresh", |s| !s.state().loading());

    // One commit at a time.
    let effects = s.key(key("c"), &keymap);
    assert!(effects.is_empty());
    for c in first.chars().take(7) {
        s.key(KeyChord::new(shelbi_state::keymap::Key::Char(c), shelbi_state::keymap::Mods::NONE), &keymap);
    }
    s.key(key("enter"), &keymap);
    assert!(s.state().loading());
    wait(&mut s, "the commit", |s| !s.state().loading());
    assert_eq!(s.state().changeset().scope, CommitScope::Commit(first));
    assert_eq!(s.state().changeset().files.len(), 2);

    // Restart: a new session finds them.
    drop(s);
    let (mut s, _) = session_on(&repo, store.path(), TargetKind::Review, None);
    wait(&mut s, "the comments", |s| !s.state().loading() && !s.state().comments().is_empty());
    wait(&mut s, "the marks", |s| {
        let file = &s.state().changeset().files[2];
        s.state().is_viewed(file)
    });
    assert_eq!(s.state().comments()[0].body, "a note");
    assert!(s.state().table().rows.iter().any(|r| matches!(r, Row::Thread { .. })));
}

#[test]
fn a_workspace_session_follows_the_agents_edits() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    let store = tempfile::tempdir().unwrap();
    let (mut s, _) = session_on(&repo, store.path(), TargetKind::Workspace, Some(Duration::from_millis(50)));
    wait(&mut s, "the empty diff", |s| !s.state().loading());
    assert!(s.state().changeset().files.is_empty());

    repo.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    wait(&mut s, "the edit", |s| s.state().changeset().files.len() == 1);
    let first_viewed_file = s.state().changeset().files[0].fingerprint;

    // A comment written elsewhere (an agent's reply through the CLI) shows up.
    DiffStore::at(store.path()).apply_comment(&CommentOp::Add(comment("c", "a.txt", 10, "ten"))).unwrap();
    wait(&mut s, "the comment", |s| s.state().comments().len() == 1);
    assert!(s.state().table().rows.iter().any(|r| matches!(r, Row::Thread { .. })));

    repo.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\nnine\nten\n");
    wait(&mut s, "the second edit", |s| s.state().changeset().files[0].fingerprint != first_viewed_file);
    assert!(s.state().table().rows.iter().any(|r| matches!(r, Row::Thread { .. })), "the line is still there");
}

#[test]
fn a_hidden_workspace_rests_and_catches_up_when_shown() {
    let repo = Repo::with_base(&[("a.txt", TEN)]);
    let store = tempfile::tempdir().unwrap();
    // An interval a test would never outlast: only showing it again can read.
    let (mut s, _) = session_on(&repo, store.path(), TargetKind::Workspace, Some(Duration::from_secs(600)));
    wait(&mut s, "the empty diff", |s| !s.state().loading());
    s.set_visible(false);
    repo.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\nten\n");
    s.set_visible(false);
    std::thread::sleep(Duration::from_millis(50));
    s.poll();
    assert!(s.state().changeset().files.is_empty(), "nobody's looking");
    s.set_visible(true);
    wait(&mut s, "the catch-up", |s| s.state().changeset().files.len() == 1);
}

#[test]
fn a_session_that_cant_read_says_why_and_keeps_going() {
    let dir = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let backend: Arc<dyn DiffBackend> = Arc::new(HostBackend::over(
        Host::Local,
        dir.path().to_string_lossy().into_owned(),
        target_of(TargetKind::Review),
        "main",
        DiffStore::at(store.path()),
        no_agent(),
    ));
    let (options, _) = options(None);
    let mut s = DiffSession::open(move || Ok(backend), options);
    wait(&mut s, "the failure", |s| s.state().error().is_some());
    assert!(!s.state().loading());
    assert!(s.state().table().is_empty());

    let (options2, _) = self::options(None);
    let mut s = DiffSession::open(|| Err("no such project".to_string()), options2);
    wait(&mut s, "the failure", |s| s.state().error().is_some());
    assert_eq!(s.state().error(), Some("no such project"));
}

/// A repository whose diff is 80 files and 5,000 lines of hunks, every one of
/// them an edit or beside one, which is the most work a line can be.
fn large_repo() -> Repo {
    const LINES: usize = 40;
    let repo = Repo::new();
    for i in 0..80 {
        let mut text = String::new();
        for line in 0..LINES {
            text.push_str(&format!("let value_{line} = compute({line}, \"file {i}\");\n"));
        }
        repo.write(&format!("src/module_{i:02}/file.rs"), text);
    }
    repo.commit("base");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    for i in 0..80 {
        let mut text = String::new();
        for line in 0..LINES {
            if line % 3 == 1 {
                text.push_str(&format!("let value_{line} = compute_changed({line}, \"file {i}\");\n"));
                text.push_str(&format!("let extra_{line} = {line} * 2;\n"));
            } else {
                text.push_str(&format!("let value_{line} = compute({line}, \"file {i}\");\n"));
            }
        }
        repo.write(&format!("src/module_{i:02}/file.rs"), text);
    }
    repo.commit("work");
    repo
}

#[test]
fn the_large_diff_opens_in_under_a_second() {
    let repo = large_repo();
    let keymap = validate_keymaps_yaml("", None).0.diff;
    let store = tempfile::tempdir().unwrap();

    // Best of three: the bar is what the code takes, not what a loaded machine
    // lets it have.
    let mut best = Duration::MAX;
    let mut lines = 0;
    for _ in 0..3 {
        let start = Instant::now();
        let (mut s, _) = session_on(&repo, store.path(), TargetKind::Review, None);
        wait(&mut s, "the large diff", |s| !s.state().loading());
        best = best.min(start.elapsed());
        lines = s.state().changeset().line_count();
        assert_eq!(s.state().changeset().files.len(), 80);
        assert!(s.state().table().len() >= lines);

        // Moving through it stays cheap: the table doesn't change, only a slice
        // of it is read.
        let start = Instant::now();
        for _ in 0..200 {
            s.key(key("space"), &keymap);
            assert!(s.state().visible_rows().len() <= 24);
        }
        assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
    }
    assert!(lines >= 5_000, "the fixture is the size it says: {lines} lines");
    eprintln!("{lines} lines in 80 files opened in {best:?}");
    assert!(best < Duration::from_secs(1), "took {best:?}");
}
