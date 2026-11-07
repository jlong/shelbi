//! The pull request in a whole diff session: a real changeset from real git, and
//! `gh` stubbed behind [`PrSource`]. The diff doesn't wait on GitHub, and what
//! GitHub won't answer, the block says.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shelbi_app::diff::backend::{DiffBackend, DiffTarget, HostBackend, TargetKind};
use shelbi_app::diff::comments::Author;
use shelbi_app::diff::model::Side;
use shelbi_app::diff::pr::{PrDetails, PrPart, PrSource, PrThread, PrThreadComment, PrWrite};
use shelbi_app::diff::store::DiffStore;
use shelbi_app::diff::{AgentDelivery, AgentSend, DiffOptions, DiffSession, Row};
use shelbi_core::{Host, IssuePr, PrState};
use shelbi_state::keymap::{validate_keymaps_yaml, KeyChord, ModeKeymap};

fn git(path: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `a.txt` changed on `feature`.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.name", "Fixture"]);
    git(path, &["config", "user.email", "fixture@example.com"]);
    fs::write(path.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "base"]);
    git(path, &["checkout", "-q", "-b", "feature"]);
    fs::write(path.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    git(path, &["add", "-A"]);
    git(path, &["commit", "-qm", "change"]);
    dir
}

fn pull() -> PrDetails {
    PrDetails {
        number: 7,
        title: "Change two".into(),
        url: "https://github.com/o/r/pull/7".into(),
        body: "Because.".into(),
        author: "jlong".into(),
        head: "feature".into(),
        base: "main".into(),
        state: PrState::Open,
        draft: false,
        reviews: Vec::new(),
        checks: Vec::new(),
        threads: vec![PrThread {
            id: "T1".into(),
            path: "a.txt".into(),
            side: Side::New,
            line: Some(2),
            original_line: Some(2),
            start_line: None,
            code: Some("TWO".into()),
            resolved: false,
            outdated: false,
            file_level: false,
            comments: vec![PrThreadComment {
                id: "C1".into(),
                author: "bob".into(),
                body: "shouting?".into(),
                created_at: chrono::Utc::now(),
                pending: false,
            }],
        }],
        threads_cut: false,
    }
}

/// A pull request whose reads wait for the test, and whose writes are recorded.
struct FakePr {
    record: Option<IssuePr>,
    /// Each read takes its answer from here, so a test can hold GitHub back.
    answers: Mutex<Receiver<Result<PrDetails, String>>>,
    writes: Mutex<Vec<PrWrite>>,
    write_answer: Result<String, String>,
}

impl PrSource for FakePr {
    fn lookup(&self) -> Result<Option<IssuePr>, String> {
        Ok(self.record.clone())
    }

    fn details(&self, _pr: &IssuePr) -> Result<PrDetails, String> {
        self.answers.lock().unwrap().recv().map_err(|e| e.to_string())?
    }

    fn write(&self, _pr: &IssuePr, change: &PrWrite) -> Result<String, String> {
        self.writes.lock().unwrap().push(change.clone());
        self.write_answer.clone()
    }
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

fn keymap() -> ModeKeymap<shelbi_app::diff::DiffAction> {
    validate_keymaps_yaml("", None).0.diff
}

fn key(spec: &str) -> KeyChord {
    KeyChord::parse(spec).unwrap()
}

struct Open {
    session: DiffSession,
    github: Sender<Result<PrDetails, String>>,
    pr: Arc<FakePr>,
    _repo: tempfile::TempDir,
    _store: tempfile::TempDir,
}

fn open(record: Option<IssuePr>, write_answer: Result<String, String>) -> Open {
    let (github, answers) = mpsc::channel();
    let pr = Arc::new(FakePr { record, answers: Mutex::new(answers), writes: Mutex::default(), write_answer });
    let repo = repo();
    let store = tempfile::tempdir().unwrap();
    let target = DiffTarget { project: "fixture".into(), workspace: "alpha".into(), kind: TargetKind::Review, task: Some("t".into()) };
    let agent: AgentSend = Arc::new(|_: &DiffTarget, _: &str| Ok(AgentDelivery::Submitted));
    let backend: Arc<dyn DiffBackend> = Arc::new(
        HostBackend::over(
            Host::Local,
            repo.path().to_string_lossy().into_owned(),
            target.clone(),
            "main",
            DiffStore::at(store.path()),
            agent,
        )
        .with_pr(pr.clone()),
    );
    let options = DiffOptions { author: Author::human("jlong"), target, watch: None, wake: Arc::new(|| {}) };
    let session = DiffSession::open(move || Ok(backend), options);
    Open { session, github, pr, _repo: repo, _store: store }
}

fn status_row(session: &DiffSession) -> String {
    session.state().pr_row(PrPart::Status).iter().map(|s| s.text.as_str()).collect()
}

#[test]
fn the_diff_opens_while_github_is_still_answering() {
    let mut o = open(Some(IssuePr::new(7, Some("https://github.com/o/r/pull/7".into()))), Ok("done".into()));
    // Nothing has come back from GitHub, and won't until the test says.
    wait(&mut o.session, "the diff", |s| !s.state().table().is_empty());
    wait(&mut o.session, "the recorded PR", |s| s.state().pr().shows());
    assert!(o.session.state().table().rows.iter().any(|r| matches!(r, Row::Line { .. })));
    assert_eq!(status_row(&o.session), "Reading the pull request…");

    o.github.send(Ok(pull())).unwrap();
    wait(&mut o.session, "the details", |s| s.state().pr().details.is_some());
    assert!(status_row(&o.session).contains("1 thread open"));
    let thread = o.session.state().table().rows.iter().find_map(|r| match r {
        Row::Thread { thread, .. } => Some(*thread),
        _ => None,
    });
    let text = o.session.state().thread_row_text(thread.unwrap(), shelbi_app::diff::ThreadPart::Head).unwrap();
    assert_eq!(text, "⎇ bob (GitHub): shouting?");
}

#[test]
fn a_task_with_no_pr_has_no_block_and_asks_github_nothing() {
    let mut o = open(None, Ok("done".into()));
    wait(&mut o.session, "the diff", |s| !s.state().table().is_empty());
    assert!(!o.session.state().pr().shows());
    assert!(!o.session.state().table().rows.iter().any(|r| matches!(r, Row::Pr { .. })));
}

#[test]
fn rate_limited_the_diff_still_works_and_the_block_says_why() {
    let mut o = open(Some(IssuePr::new(7, None)), Ok("done".into()));
    o.github.send(Err("GitHub's rate limit is spent until 14:05".to_string())).unwrap();
    wait(&mut o.session, "the reason", |s| s.state().pr().why.is_some());
    wait(&mut o.session, "the diff", |s| !s.state().table().is_empty());
    assert_eq!(status_row(&o.session), "PR details aren't here: GitHub's rate limit is spent until 14:05");
    assert!(o.session.state().table().rows.iter().any(|r| matches!(r, Row::Line { .. })));
}

#[test]
fn r_reads_the_pull_request_again() {
    let mut o = open(Some(IssuePr::new(7, None)), Ok("done".into()));
    o.github.send(Err("Can't reach GitHub".to_string())).unwrap();
    wait(&mut o.session, "the reason", |s| s.state().pr().why.is_some());
    wait(&mut o.session, "the diff", |s| !s.state().table().is_empty());
    let km = keymap();
    o.session.key(key("r"), &km);
    o.github.send(Ok(pull())).unwrap();
    wait(&mut o.session, "the details", |s| s.state().pr().details.is_some());
    assert_eq!(o.session.state().pr().why, None);
}

fn loaded(write_answer: Result<String, String>) -> Open {
    let mut o = open(Some(IssuePr::new(7, None)), write_answer);
    o.github.send(Ok(pull())).unwrap();
    wait(&mut o.session, "the details", |s| s.state().pr().details.is_some());
    wait(&mut o.session, "the diff", |s| !s.state().table().is_empty());
    o
}

fn on_thread(o: &mut Open) {
    let row = o.session.state().table().rows.iter().position(|r| matches!(r, Row::Thread { .. })).unwrap();
    o.session.state_mut().click_row(row);
}

#[test]
fn a_resolve_goes_to_github_says_so_and_reads_it_back() {
    let mut o = loaded(Ok("Resolved the thread on GitHub".into()));
    on_thread(&mut o);
    let km = keymap();
    let client = o.session.key(key("x"), &km);
    assert_eq!(client, [], "the session's own");
    let mut resolved = pull();
    resolved.threads[0].resolved = true;
    o.github.send(Ok(resolved)).unwrap();
    wait(&mut o.session, "the write", |s| s.state().flash() == Some("Resolved the thread on GitHub"));
    wait(&mut o.session, "the read back", |s| s.state().pr().details.as_ref().unwrap().threads[0].resolved);
    assert_eq!(
        *o.pr.writes.lock().unwrap(),
        [PrWrite::Resolve { thread: "T1".into(), resolved: true }]
    );
}

#[test]
fn a_reply_refused_while_typing_elsewhere_reaches_the_client_as_a_yank() {
    let mut o = loaded(Err("nope".into()));
    on_thread(&mut o);
    let km = keymap();
    o.session.key(key("r"), &km);
    o.session.key(key("h"), &km);
    o.session.key(key("i"), &km);
    o.session.key(key("enter"), &km);
    // The search prompt has the keyboard when the refusal comes back.
    o.session.key(key("/"), &km);
    wait(&mut o.session, "the refusal", |s| s.state().flash().is_some_and(|f| f.contains("clipboard")));
    let effects = o.session.take_effects();
    assert_eq!(effects, [shelbi_app::diff::DiffEffect::Yank("hi".into())]);
    assert_eq!(o.session.take_effects(), []);
}

#[test]
fn dropping_the_session_doesnt_wait_for_github() {
    let o = open(Some(IssuePr::new(7, None)), Ok("done".into()));
    let started = Instant::now();
    drop(o.session);
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(o.github);
}
