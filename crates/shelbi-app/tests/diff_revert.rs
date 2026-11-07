//! Reverting and sending comments, against real git: what each kind of change
//! reverts to, what's refused, and that `gY` reaches the agent and marks only
//! what got there.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shelbi_app::diff::backend::{AgentDelivery, DiffBackend, DiffTarget, HostBackend, TargetKind};
use shelbi_app::diff::comments::{Anchor, Author, Comment, CommentOp};
use shelbi_app::diff::git::{collect, HostGit};
use shelbi_app::diff::model::{Changeset, CommitScope, Side};
use shelbi_app::diff::revert::{plan, run, RevertClass, RevertPlan, RevertSpec};
use shelbi_app::diff::store::DiffStore;
use shelbi_app::diff::{AgentSend, DiffEffect, DiffOptions, DiffSession, Mode};
use shelbi_core::Host;
use shelbi_state::keymap::{validate_keymaps_yaml, KeyChord, ModeKeymap};

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

const LINES: &str = "01\n02\n03\n04\n05\n06\n07\n08\n09\n10\n11\n12\n13\n14\n15\n16\n17\n18\n19\n20\n";

impl Repo {
    /// `a.txt` and `bin.dat` on `main`, on a `feature` branch cut from it.
    fn new() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        let repo = Repo { _dir: dir, path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "Fixture"]);
        repo.git(&["config", "user.email", "fixture@example.com"]);
        repo.git(&["config", "core.autocrlf", "false"]);
        repo.write("a.txt", LINES);
        repo.write_bytes("bin.dat", &[0, 1, 2, 3, 0, 255]);
        repo.write("old-name.txt", "keep me\nall of me\n");
        repo.commit("base");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(&self.path, args)
    }

    fn write(&self, path: &str, content: &str) {
        self.write_bytes(path, content.as_bytes());
    }

    fn write_bytes(&self, path: &str, content: &[u8]) {
        let full = self.path.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.path.join(path)).unwrap()
    }

    fn exists(&self, path: &str) -> bool {
        self.path.join(path).exists()
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn runner(&self) -> HostGit {
        HostGit { host: Host::Local, worktree: self.path.to_string_lossy().into_owned() }
    }

    fn changes(&self, scope: &CommitScope) -> Changeset {
        collect(&self.runner(), TargetKind::Workspace, "main", scope).unwrap().parse()
    }

    fn spec(&self, scope: CommitScope, path: &str, hunk: Option<u32>) -> RevertSpec {
        let changes = self.changes(&scope);
        let file = changes.files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no {path}"));
        RevertSpec { scope, path: path.into(), hunk, fingerprint: file.fingerprint }
    }

    fn plan(&self, spec: &RevertSpec) -> Result<RevertPlan, String> {
        plan(&self.runner(), TargetKind::Workspace, "main", spec)
    }

    fn revert(&self, scope: CommitScope, path: &str, hunk: Option<u32>) -> Result<(RevertClass, String), String> {
        let plan = self.plan(&self.spec(scope, path, hunk))?;
        let done = run(&self.runner(), TargetKind::Workspace, "main", &plan)?;
        Ok((plan.class, done))
    }
}

/// `line` of `LINES` replaced.
fn with_line(line: usize, text: &str) -> String {
    LINES
        .lines()
        .enumerate()
        .map(|(i, l)| if i + 1 == line { format!("{text}\n") } else { format!("{l}\n") })
        .collect()
}

#[test]
fn uncommitted_work_goes_back_to_head_and_says_its_gone() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.git(&["add", "a.txt"]);
    let spec = repo.spec(CommitScope::All, "a.txt", None);
    let plan = repo.plan(&spec).unwrap();
    assert_eq!(plan.class, RevertClass::Uncommitted);
    assert_eq!(plan.prompt(), "Revert a.txt to HEAD? This uncommitted work will be gone.");
    let done = run(&repo.runner(), TargetKind::Workspace, "main", &plan).unwrap();
    assert_eq!(done, "Reverted a.txt to HEAD");
    assert_eq!(repo.read("a.txt"), LINES);
    assert_eq!(repo.git(&["status", "--porcelain"]), "", "nothing's left staged");
}

#[test]
fn committed_work_is_undone_as_an_uncommitted_change_and_the_commits_stay() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let head = repo.commit("edit");
    let plan = repo.plan(&repo.spec(CommitScope::All, "a.txt", None)).unwrap();
    assert_eq!(plan.class, RevertClass::Committed);
    assert_eq!(plan.prompt(), "Undo a.txt as an uncommitted change? Your commits stay as they are.");
    let done = run(&repo.runner(), TargetKind::Workspace, "main", &plan).unwrap();
    assert_eq!(done, "Undid a.txt as an uncommitted change");
    assert_eq!(repo.read("a.txt"), LINES);
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), head, "HEAD didn't move");
    assert_eq!(repo.git(&["status", "--porcelain"]), "M a.txt", "an uncommitted change, unstaged");
    // It can be taken back.
    repo.git(&["checkout", "--", "a.txt"]);
    assert_eq!(repo.read("a.txt"), with_line(3, "three"));
}

#[test]
fn a_file_with_both_undoes_both_and_says_so() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let head = repo.commit("edit");
    let both = with_line(3, "three").replace("15\n", "fifteen\n");
    repo.write("a.txt", &both);
    let plan = repo.plan(&repo.spec(CommitScope::All, "a.txt", None)).unwrap();
    assert_eq!(plan.class, RevertClass::Mixed);
    assert!(plan.prompt().contains("Your commits stay as they are"), "{}", plan.prompt());
    assert!(plan.prompt().contains("uncommitted edits will be gone"), "{}", plan.prompt());
    run(&repo.runner(), TargetKind::Workspace, "main", &plan).unwrap();
    assert_eq!(repo.read("a.txt"), LINES);
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn a_hunk_reverts_alone_whoever_wrote_it() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.commit("edit");
    let two = with_line(3, "three").replace("15\n", "fifteen\n");
    repo.write("a.txt", &two);
    let changes = repo.changes(&CommitScope::All);
    assert_eq!(changes.files[0].hunks.len(), 2, "two hunks: one committed, one not");

    // The uncommitted one first: only it goes.
    let (class, done) = repo.revert(CommitScope::All, "a.txt", Some(1)).unwrap();
    assert_eq!(class, RevertClass::Uncommitted);
    assert!(done.contains("the hunk at a.txt:"), "{done}");
    assert_eq!(repo.read("a.txt"), with_line(3, "three"));

    // Then the committed one.
    let (class, _) = repo.revert(CommitScope::All, "a.txt", Some(0)).unwrap();
    assert_eq!(class, RevertClass::Committed);
    assert_eq!(repo.read("a.txt"), LINES);
    assert_eq!(repo.git(&["status", "--porcelain"]), "M a.txt");
}

#[test]
fn a_mixed_hunk_reverses_both_the_same_way() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.commit("edit");
    repo.write("a.txt", &with_line(3, "three").replace("04\n", "four\n"));
    let changes = repo.changes(&CommitScope::All);
    assert_eq!(changes.files[0].hunks.len(), 1, "one hunk, part committed");
    let (class, done) = repo.revert(CommitScope::All, "a.txt", Some(0)).unwrap();
    assert_eq!(class, RevertClass::Mixed);
    assert!(done.starts_with("Undid"), "{done}");
    assert_eq!(repo.read("a.txt"), LINES);
}

#[test]
fn a_change_that_moved_since_it_was_read_is_refused() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let spec = repo.spec(CommitScope::All, "a.txt", None);
    let plan = repo.plan(&spec).unwrap();
    // The agent edits between the question and the answer.
    let edited = with_line(3, "THREE");
    repo.write("a.txt", &edited);
    let err = repo.plan(&spec).unwrap_err();
    assert!(err.contains("changed since this was read"), "{err}");
    let err = run(&repo.runner(), TargetKind::Workspace, "main", &plan).unwrap_err();
    assert!(err.contains("changed since this was read"), "{err}");
    assert_eq!(repo.read("a.txt"), edited, "left alone");
}

#[test]
fn a_conflicting_edit_refuses_with_gits_reason_and_changes_nothing() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let sha = repo.commit("edit");
    repo.write("a.txt", &with_line(3, "something else"));
    let scope = CommitScope::Commit(sha);
    let err = repo.revert(scope.clone(), "a.txt", Some(0)).unwrap_err();
    assert!(err.contains("conflicting edit"), "{err}");
    assert_eq!(repo.read("a.txt"), with_line(3, "something else"));
    let err = repo.revert(scope, "a.txt", None).unwrap_err();
    assert!(err.contains("conflicting edit"), "{err}");
    assert_eq!(repo.read("a.txt"), with_line(3, "something else"));
}

#[test]
fn one_commit_can_be_undone_when_the_worktree_takes_it() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let sha = repo.commit("edit");
    repo.write("a.txt", &with_line(3, "three").replace("15\n", "fifteen\n"));
    repo.commit("later");
    let (class, _) = repo.revert(CommitScope::Commit(sha), "a.txt", Some(0)).unwrap();
    assert_eq!(class, RevertClass::Committed);
    assert_eq!(repo.read("a.txt"), LINES.replace("15\n", "fifteen\n"));
}

#[test]
fn every_shape_of_file_reverts() {
    let repo = Repo::new();
    repo.write("committed-new.txt", "new\n");
    repo.write_bytes("bin.dat", &[9, 9, 0, 9]);
    repo.git(&["mv", "old-name.txt", "new-name.txt"]);
    repo.commit("edit");
    repo.write("untracked.txt", "scratch\n");
    repo.write("staged-new.txt", "staged\n");
    repo.git(&["add", "staged-new.txt"]);
    fs::remove_file(repo.path.join("a.txt")).unwrap();

    // Committed: a new file goes, a binary comes back, a rename undoes.
    repo.revert(CommitScope::All, "committed-new.txt", None).unwrap();
    assert!(!repo.exists("committed-new.txt"));
    repo.revert(CommitScope::All, "bin.dat", None).unwrap();
    assert_eq!(fs::read(repo.path.join("bin.dat")).unwrap(), [0, 1, 2, 3, 0, 255]);
    repo.revert(CommitScope::All, "new-name.txt", None).unwrap();
    assert!(!repo.exists("new-name.txt"));
    assert_eq!(repo.read("old-name.txt"), "keep me\nall of me\n");

    // Uncommitted: new files go whether or not they're staged, a delete returns.
    let (class, _) = repo.revert(CommitScope::All, "untracked.txt", None).unwrap();
    assert_eq!(class, RevertClass::Uncommitted);
    assert!(!repo.exists("untracked.txt"));
    repo.revert(CommitScope::All, "staged-new.txt", None).unwrap();
    assert!(!repo.exists("staged-new.txt"));
    repo.revert(CommitScope::All, "a.txt", None).unwrap();
    assert_eq!(repo.read("a.txt"), LINES);
}

#[test]
fn a_file_named_like_a_pattern_reverts_only_itself() {
    let repo = Repo::new();
    repo.write("a*", "star\n");
    repo.write("a.txt", &with_line(3, "three"));
    repo.write("odd \"name\".txt", "base\n");
    repo.commit("edit");
    let head = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["branch", "-f", "main", &head]);
    repo.write("a*", "STAR\n");
    repo.write("odd \"name\".txt", "BASE\n");
    repo.write("a.txt", &with_line(3, "THREE"));
    repo.revert(CommitScope::All, "a*", None).unwrap();
    assert_eq!(repo.read("a*"), "star\n");
    assert_eq!(repo.read("a.txt"), with_line(3, "THREE"), "not caught by the pattern");
    // A quote in the name survives the hunk's own patch header.
    repo.revert(CommitScope::All, "odd \"name\".txt", Some(0)).unwrap();
    assert_eq!(repo.read("odd \"name\".txt"), "base\n");
}

#[test]
fn a_hunk_of_a_new_file_reverts_the_file() {
    let repo = Repo::new();
    repo.write("fresh.txt", "one\ntwo\n");
    let (class, _) = repo.revert(CommitScope::All, "fresh.txt", Some(0)).unwrap();
    assert_eq!(class, RevertClass::Uncommitted);
    assert!(!repo.exists("fresh.txt"), "not left behind empty");
}

#[test]
fn lines_without_a_newline_and_crlf_reverse_exactly() {
    let repo = Repo::new();
    repo.write("eof.txt", "one\ntwo");
    repo.write("crlf.txt", "one\r\ntwo\r\nthree\r\n");
    repo.commit("more base");
    let head = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["branch", "-f", "main", &head]);
    repo.write("eof.txt", "one\nTWO");
    repo.write("crlf.txt", "one\r\nTWO\r\nthree\r\n");
    repo.revert(CommitScope::All, "eof.txt", Some(0)).unwrap();
    assert_eq!(repo.read("eof.txt"), "one\ntwo");
    repo.revert(CommitScope::All, "crlf.txt", Some(0)).unwrap();
    assert_eq!(repo.read("crlf.txt"), "one\r\ntwo\r\nthree\r\n");
}

#[test]
fn a_review_never_reverts_and_a_merge_in_progress_is_left_alone() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.commit("edit");
    let spec = repo.spec(CommitScope::All, "a.txt", None);
    let err = plan(&repo.runner(), TargetKind::Review, "main", &spec).unwrap_err();
    assert!(err.contains("checkout of the branch"), "{err}");

    // A conflicting merge stops halfway.
    repo.git(&["checkout", "-q", "-b", "other", "main"]);
    repo.write("a.txt", &with_line(3, "drei"));
    repo.commit("other");
    repo.git(&["checkout", "-q", "feature"]);
    let out = Command::new("git").arg("-C").arg(&repo.path).args(["merge", "other"]).output().unwrap();
    assert!(!out.status.success(), "the fixture's merge should conflict");
    let err = repo.plan(&spec).unwrap_err();
    assert!(err.contains("middle of a merge"), "{err}");
}

// ---- The session ------------------------------------------------------------

fn keys() -> ModeKeymap<shelbi_state::keymap::DiffAction> {
    validate_keymaps_yaml("", None).0.diff
}

fn key(spec: &str) -> KeyChord {
    KeyChord::parse(spec).unwrap()
}

fn wait(session: &mut DiffSession, what: &str, done: impl Fn(&DiffSession) -> bool) {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        session.poll();
        if done(session) {
            return;
        }
        assert!(Instant::now() < until, "timed out waiting for {what}: {}", session.state().status());
        std::thread::sleep(Duration::from_millis(5));
    }
}

type Sent = Arc<Mutex<Vec<(String, String)>>>;

fn open(repo: &Repo, store: &Path, kind: TargetKind, answer: Result<AgentDelivery, String>) -> (DiffSession, Sent) {
    let sent: Sent = Arc::default();
    let record = sent.clone();
    let agent: AgentSend = Arc::new(move |target: &DiffTarget, message: &str| {
        record.lock().unwrap().push((target.workspace.clone(), message.to_string()));
        answer.clone()
    });
    let target = DiffTarget { project: "fixture".into(), workspace: "alpha".into(), kind, task: Some("t".into()) };
    let backend: Arc<dyn DiffBackend> = Arc::new(HostBackend::over(
        Host::Local,
        repo.path.to_string_lossy().into_owned(),
        target.clone(),
        "main",
        DiffStore::at(store),
        agent,
    ));
    let options = DiffOptions { author: Author::human("jlong"), target, watch: None, wake: Arc::new(|| {}) };
    let mut session = DiffSession::open(move || Ok(backend), options);
    wait(&mut session, "the first read", |s| !s.state().loading());
    (session, sent)
}

fn thread(id: &str) -> CommentOp {
    CommentOp::Add(Comment {
        id: id.into(),
        anchor: Anchor { path: "a.txt".into(), side: Side::New, start: 3, end: 3, code: vec!["three".into()] },
        author: Author::human("jlong"),
        body: "why?".into(),
        created_at: chrono::Utc::now(),
        replies: Vec::new(),
        sent: false,
    })
}

fn press(session: &mut DiffSession, keymap: &ModeKeymap<shelbi_state::keymap::DiffAction>, specs: &[&str]) -> Vec<DiffEffect> {
    specs.iter().flat_map(|spec| session.key(key(spec), keymap)).collect()
}

#[test]
fn gy_delivers_one_message_to_the_workspaces_agent_and_marks_it_sent() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let store = tempfile::tempdir().unwrap();
    DiffStore::at(store.path()).apply_comment(&thread("c1")).unwrap();
    DiffStore::at(store.path()).apply_comment(&thread("c2")).unwrap();
    let (mut s, sent) = open(&repo, store.path(), TargetKind::Workspace, Ok(AgentDelivery::Submitted));
    wait(&mut s, "the comments", |s| s.state().comments().len() == 2);
    assert!(s.state().status().contains("2 comments (gY send to agent)"), "{}", s.state().status());

    let keymap = keys();
    let left = press(&mut s, &keymap, &["g", "shift-y"]);
    assert!(left.is_empty(), "the session sends it, not the client: {left:?}");
    wait(&mut s, "the delivery", |s| s.state().flash().is_some_and(|f| f.starts_with("Sent")));
    assert_eq!(s.state().flash(), Some("Sent 2 comments to the agent"));
    assert!(s.state().comments().iter().all(|c| c.sent));
    {
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "one message");
        assert_eq!(sent[0].0, "alpha");
        assert!(sent[0].1.contains("[thread c1] a.txt:3") && sent[0].1.contains("[thread c2] a.txt:3"), "{}", sent[0].1);
        assert!(sent[0].1.contains("```\nthree\n```"), "the code's quoted: {}", sent[0].1);
    }
    wait(&mut s, "the save", |_| DiffStore::at(store.path()).comments().unwrap().comments.iter().all(|c| c.sent));

    // Nothing's left to send.
    press(&mut s, &keymap, &["g", "shift-y"]);
    assert_eq!(s.state().flash(), Some("No comments to send"));
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[test]
fn a_send_that_fails_keeps_the_comments_unsent_and_says_why() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let store = tempfile::tempdir().unwrap();
    DiffStore::at(store.path()).apply_comment(&thread("c1")).unwrap();
    let (mut s, _) = open(&repo, store.path(), TargetKind::Workspace, Err("no agent is running on `alpha`".into()));
    wait(&mut s, "the comments", |s| s.state().comments().len() == 1);
    press(&mut s, &keys(), &["g", "shift-y"]);
    wait(&mut s, "the failure", |s| s.state().flash().is_some_and(|f| f.starts_with("Couldn't")));
    assert!(s.state().flash().unwrap().contains("no agent is running"));
    assert!(!s.state().comments()[0].sent);
}

#[test]
fn in_a_review_gy_asks_reject_and_ga_sends_to_the_reviewer() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.commit("edit");
    let store = tempfile::tempdir().unwrap();
    DiffStore::at(store.path()).apply_comment(&thread("c1")).unwrap();
    let (mut s, sent) = open(&repo, store.path(), TargetKind::Review, Ok(AgentDelivery::Queued));
    wait(&mut s, "the comments", |s| s.state().comments().len() == 1);
    assert!(s.state().status().contains("gA send to the Reviewer"), "{}", s.state().status());

    let keymap = keys();
    let effects = press(&mut s, &keymap, &["g", "shift-y"]);
    let [DiffEffect::RejectWithComments { reason }] = effects.as_slice() else { panic!("{effects:?}") };
    assert!(reason.contains("[thread c1]"), "{reason}");
    assert!(sent.lock().unwrap().is_empty(), "nothing goes to a session");
    assert!(!s.state().comments()[0].sent, "not until the reject lands");

    press(&mut s, &keymap, &["g", "shift-a"]);
    wait(&mut s, "the delivery", |s| s.state().flash().is_some_and(|f| f.starts_with("Queued")));
    assert_eq!(s.state().flash(), Some("Queued 1 comment for the Reviewer: it's mid-turn"));
    assert!(s.state().comments()[0].sent);
    assert_eq!(sent.lock().unwrap().len(), 1);

    // A review reverts nothing.
    press(&mut s, &keymap, &["g", "shift-r"]);
    assert!(s.state().flash().unwrap().contains("checkout of the branch"), "{:?}", s.state().flash());
    assert!(!matches!(s.state().mode(), Mode::Confirm(_)));
}

#[test]
fn revert_asks_then_writes_and_any_other_key_keeps_the_work() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.commit("edit");
    let store = tempfile::tempdir().unwrap();
    let (mut s, _) = open(&repo, store.path(), TargetKind::Workspace, Ok(AgentDelivery::Submitted));
    let keymap = keys();

    press(&mut s, &keymap, &["j", "g", "shift-r"]);
    wait(&mut s, "the question", |s| matches!(s.state().mode(), Mode::Confirm(_)));
    assert!(
        s.state().status().contains("Undo a.txt as an uncommitted change? Your commits stay as they are."),
        "{}",
        s.state().status()
    );
    press(&mut s, &keymap, &["n"]);
    assert!(matches!(s.state().mode(), Mode::Normal));
    assert_eq!(s.state().flash(), Some("Not reverted"));
    assert_eq!(repo.read("a.txt"), with_line(3, "three"));

    // `gr` on a line is the hunk.
    press(&mut s, &keymap, &["g", "r"]);
    wait(&mut s, "the question", |s| matches!(s.state().mode(), Mode::Confirm(_)));
    assert!(s.state().status().contains("the hunk at a.txt:"), "{}", s.state().status());
    press(&mut s, &keymap, &["y"]);
    wait(&mut s, "the write", |s| s.state().flash().is_some_and(|f| f.starts_with("Undid")));
    assert_eq!(repo.read("a.txt"), LINES);
}

#[test]
fn a_reply_from_the_cli_shows_in_an_open_view_with_no_refresh() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    repo.commit("edit");
    let store = tempfile::tempdir().unwrap();
    DiffStore::at(store.path()).apply_comment(&thread("c1")).unwrap();
    // A review reads nothing on its own, but it still follows its threads.
    let (mut s, _) = open(&repo, store.path(), TargetKind::Review, Ok(AgentDelivery::Submitted));
    wait(&mut s, "the comments", |s| s.state().comments().len() == 1);
    DiffStore::at(store.path()).reply("c1", Author::agent("developer"), "fixed").unwrap();
    wait(&mut s, "the reply", |s| s.state().comments()[0].replies.len() == 1);
    assert_eq!(s.state().comments()[0].replies[0].author.name, "developer");
}

#[test]
fn a_thread_keeps_its_line_when_an_edit_moves_it() {
    let repo = Repo::new();
    repo.write("a.txt", &with_line(3, "three"));
    let store = tempfile::tempdir().unwrap();
    DiffStore::at(store.path()).apply_comment(&thread("c1")).unwrap();
    let (mut s, _) = open(&repo, store.path(), TargetKind::Workspace, Ok(AgentDelivery::Submitted));
    wait(&mut s, "the thread's row", |s| s.state().thread(0).is_some());
    assert!(!s.state().thread_outdated(0));
    // Lines arrive above it, and the view is told to look again.
    repo.write("a.txt", &format!("new one\nnew two\n{}", with_line(3, "three")));
    s.refresh();
    wait(&mut s, "the new read", |s| s.state().changeset().files[0].additions >= 3);
    assert!(!s.state().thread_outdated(0), "still found by its text");
    // Its line goes: outdated, but kept.
    repo.write("a.txt", LINES);
    s.refresh();
    wait(&mut s, "the last read", |s| s.state().changeset().files.is_empty());
    assert_eq!(s.state().comments().len(), 1);
}
