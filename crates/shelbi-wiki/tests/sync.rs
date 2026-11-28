//! The wiki's sync against real git: temp repos with a local bare origin,
//! under a git config of their own (no signing, a fixed identity), so the
//! user's config and repos are never touched.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shelbi_wiki::service::{ChangeSource, EventSink, ServiceSpec, SyncStatus, Timing, WikiService};
use shelbi_wiki::sync::pr::{PullRequest, PullRequests};
use shelbi_wiki::sync::{Layout, Setup};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    env: Vec<(String, String)>,
}

impl Fixture {
    /// A bare `origin.git`, and `hub/`, a clone with one commit on `main`.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let gitconfig = root.join("gitconfig");
        std::fs::write(
            &gitconfig,
            "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n\
             [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n[core]\n\thooksPath = /dev/null\n",
        )
        .unwrap();
        let env = vec![
            ("GIT_CONFIG_GLOBAL".to_string(), gitconfig.to_string_lossy().into_owned()),
            ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
        ];
        let fx = Self { _dir: dir, root, env };
        fx.git(&fx.root, &["init", "-q", "--bare", "origin.git"]);
        fx.git(&fx.root, &["clone", "-q", "origin.git", "hub"]);
        std::fs::write(fx.hub().join("main.txt"), "code\n").unwrap();
        std::fs::create_dir_all(fx.hub().join("docs")).unwrap();
        std::fs::write(fx.hub().join("docs/intro.md"), "# Intro\n\nHello.\n").unwrap();
        fx.git(&fx.hub(), &["add", "-A"]);
        fx.git(&fx.hub(), &["commit", "-q", "-m", "init"]);
        fx.git(&fx.hub(), &["push", "-q", "origin", "main"]);
        fx
    }

    fn hub(&self) -> PathBuf {
        self.root.join("hub")
    }

    fn origin(&self) -> PathBuf {
        self.root.join("origin.git")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Another machine: a fresh clone of origin with `branch` checked out.
    fn elsewhere(&self, name: &str, branch: &str) -> PathBuf {
        let dir = self.root.join(name);
        self.git(&self.root, &["clone", "-q", "-b", branch, "origin.git", name]);
        dir
    }

    fn origin_log(&self, branch: &str) -> Vec<String> {
        self.git(&self.origin(), &["log", "--format=%s", branch])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn origin_file(&self, branch: &str, path: &str) -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(self.origin())
            .args(["show", &format!("{branch}:{path}")])
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .unwrap();
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn worktree(&self) -> PathBuf {
        self.root.join("home/projects/demo/wiki")
    }

    fn branch_layout(&self) -> Layout {
        Layout::branch(self.hub(), self.worktree(), "docs/planning")
    }

    fn spec(&self, layout: Layout, auto: bool) -> ServiceSpec {
        ServiceSpec {
            layout,
            name: "Demo".into(),
            auto,
            conflicts_file: self.root.join("home/projects/demo/wiki-conflicts.json"),
            timing: fast(),
            git_env: self.env.clone(),
            pull_requests: None,
        }
    }
}

fn fast() -> Timing {
    Timing {
        commit_delay: Duration::from_millis(400),
        pull_active: Duration::from_secs(3600),
        pull_idle: Duration::from_secs(3600),
        active_lease: Duration::from_secs(60),
        coalesce: Duration::from_millis(50),
        rescan: Duration::from_millis(500),
    }
}

#[derive(Default)]
struct Recorder {
    changes: Mutex<Vec<(Vec<String>, ChangeSource)>>,
    statuses: Mutex<Vec<SyncStatus>>,
}

impl EventSink for Recorder {
    fn changed(&self, paths: &[String], source: ChangeSource) {
        self.changes.lock().unwrap().push((paths.to_vec(), source));
    }
    fn status(&self, status: &SyncStatus) {
        self.statuses.lock().unwrap().push(status.clone());
    }
}

impl Recorder {
    fn pulled(&self) -> Vec<String> {
        self.changes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| *s == ChangeSource::Pull)
            .flat_map(|(p, _)| p.clone())
            .collect()
    }
}

fn wait_for(what: &str, timeout: Duration, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

const SYNC: Duration = Duration::from_secs(30);

#[test]
fn a_fresh_project_gets_an_orphan_branch_with_starter_files_pushed() {
    let fx = Fixture::new();
    let sink = Arc::new(Recorder::default());
    let (svc, setup) = WikiService::start(fx.spec(fx.branch_layout(), true), sink.clone()).unwrap();
    assert_eq!(setup, Setup::Created);
    assert!(fx.worktree().join(".contextstore/settings.yml").is_file());
    assert!(fx.worktree().join("README.md").is_file());
    assert!(!fx.worktree().join("main.txt").exists(), "an orphan holds none of main's files");

    wait_for("the new branch on origin", SYNC, || fx.origin_file("docs/planning", "README.md").is_some());
    assert_eq!(fx.origin_log("docs/planning"), vec!["Create wiki".to_string()], "an orphan: one root commit");
    let settings = fx.origin_file("docs/planning", ".contextstore/settings.yml").unwrap();
    assert!(settings.contains("name: Demo"), "{settings}");
    wait_for("synced", SYNC, || svc.status().status == SyncStatus::Synced);
}

#[test]
fn an_existing_branch_is_checked_out_as_is() {
    let fx = Fixture::new();
    // Someone already made the wiki on another machine.
    let other = fx.root.join("other");
    fx.git(&fx.root, &["clone", "-q", "origin.git", "other"]);
    fx.git(&other, &["checkout", "-q", "--orphan", "docs/planning"]);
    fx.git(&other, &["rm", "-rq", "--cached", "."]);
    std::fs::remove_file(other.join("main.txt")).unwrap();
    std::fs::remove_dir_all(other.join("docs")).unwrap();
    std::fs::write(other.join("Plan.md"), "# Plan\n").unwrap();
    fx.git(&other, &["add", "-A"]);
    fx.git(&other, &["commit", "-q", "-m", "theirs"]);
    fx.git(&other, &["push", "-q", "origin", "docs/planning"]);

    let sink = Arc::new(Recorder::default());
    let (svc, setup) = WikiService::start(fx.spec(fx.branch_layout(), true), sink).unwrap();
    assert_eq!(setup, Setup::CheckedOut);
    assert_eq!(std::fs::read_to_string(fx.worktree().join("Plan.md")).unwrap(), "# Plan\n");
    assert!(!fx.worktree().join("README.md").exists(), "no starter files on an existing wiki");
    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Synced);
    assert_eq!(fx.origin_log("docs/planning"), vec!["theirs".to_string()]);

    // A second start finds the worktree there.
    drop(svc);
    let (_svc, again) = WikiService::start(fx.spec(fx.branch_layout(), true), Arc::new(Recorder::default())).unwrap();
    assert_eq!(again, Setup::Existing);
}

#[test]
fn a_change_is_committed_after_the_last_write_with_a_generated_message_and_pushed() {
    let fx = Fixture::new();
    let sink = Arc::new(Recorder::default());
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), true), sink.clone()).unwrap();
    wait_for("the wiki on origin", SYNC, || fx.origin_file("docs/planning", "README.md").is_some());

    // Two writes in a burst land in one commit, made only after the last.
    svc.write("Plans/a.md", "# A\n", ChangeSource::Agent).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    svc.write("Plans/a.md", "# A\n\nMore.\n", ChangeSource::Agent).unwrap();
    svc.write("README.md", "# Demo\n\nEdited.\n", ChangeSource::Local).unwrap();
    assert!(
        sink.changes.lock().unwrap().iter().any(|(p, s)| p == &vec!["Plans/a.md".to_string()] && *s == ChangeSource::Agent),
        "each write is announced at once with its source"
    );

    wait_for("the commit on origin", SYNC, || fx.origin_log("docs/planning").len() == 2);
    assert_eq!(fx.origin_log("docs/planning")[0], "Add a.md; Update README.md");
    assert_eq!(fx.origin_file("docs/planning", "Plans/a.md").unwrap(), "# A\n\nMore.\n");

    // An edit straight to the files (an editor on the folder) is noticed too.
    std::fs::write(fx.worktree().join("Plans/b.md"), "# B\n").unwrap();
    wait_for("the watched edit on origin", SYNC, || fx.origin_file("docs/planning", "Plans/b.md").is_some());
    assert!(sink
        .changes
        .lock()
        .unwrap()
        .iter()
        .any(|(p, s)| p.contains(&"Plans/b.md".to_string()) && *s == ChangeSource::Local));
    wait_for("synced", SYNC, || svc.status().status == SyncStatus::Synced);
}

#[test]
fn a_commit_pushed_elsewhere_is_pulled_and_announced_with_its_paths() {
    let fx = Fixture::new();
    let sink = Arc::new(Recorder::default());
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), true), sink.clone()).unwrap();
    wait_for("the wiki on origin", SYNC, || fx.origin_file("docs/planning", "README.md").is_some());

    let other = fx.elsewhere("other", "docs/planning");
    std::fs::create_dir_all(other.join("Research")).unwrap();
    std::fs::write(other.join("Research/notes.md"), "# Notes\n").unwrap();
    fx.git(&other, &["add", "-A"]);
    fx.git(&other, &["commit", "-q", "-m", "notes"]);
    fx.git(&other, &["push", "-q", "origin", "docs/planning"]);

    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Synced);
    assert_eq!(std::fs::read_to_string(fx.worktree().join("Research/notes.md")).unwrap(), "# Notes\n");
    assert_eq!(sink.pulled(), vec!["Research/notes.md".to_string()]);
}

#[test]
fn the_pull_clock_runs_faster_while_a_client_shows_the_wiki() {
    let fx = Fixture::new();
    let mut spec = fx.spec(fx.branch_layout(), true);
    spec.timing.pull_active = Duration::from_millis(300);
    let sink = Arc::new(Recorder::default());
    let (svc, _) = WikiService::start(spec, sink.clone()).unwrap();
    wait_for("the wiki on origin", SYNC, || fx.origin_file("docs/planning", "README.md").is_some());
    svc.touch_active();

    let other = fx.elsewhere("other", "docs/planning");
    std::fs::write(other.join("x.md"), "# X\n").unwrap();
    fx.git(&other, &["add", "-A"]);
    fx.git(&other, &["commit", "-q", "-m", "x"]);
    fx.git(&other, &["push", "-q", "origin", "docs/planning"]);

    wait_for("the pull", SYNC, || sink.pulled().contains(&"x.md".to_string()));
}

/// Start a manual-sync service on a wiki that already exists on origin with
/// `doc` at `Plan.md`, and a second clone of it.
fn diverge_setup(fx: &Fixture, doc: &str) -> (WikiService, Arc<Recorder>, PathBuf) {
    let sink = Arc::new(Recorder::default());
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), sink.clone()).unwrap();
    svc.write("Plan.md", doc, ChangeSource::Local).unwrap();
    assert_eq!(svc.sync_now(SYNC).status, SyncStatus::Synced);
    let other = fx.elsewhere("other", "docs/planning");
    (svc, sink, other)
}

fn push_from(fx: &Fixture, dir: &Path, path: &str, content: &str) {
    std::fs::write(dir.join(path), content).unwrap();
    fx.git(dir, &["add", "-A"]);
    fx.git(dir, &["commit", "-q", "-m", "elsewhere"]);
    fx.git(dir, &["push", "-q", "origin", "docs/planning"]);
}

const TWO_SECTIONS: &str = "# Plan\n\n## Alpha\n\nAlpha text.\n\n## Beta\n\nBeta text.\n";

#[test]
fn edits_to_different_sections_of_one_doc_merge_cleanly() {
    let fx = Fixture::new();
    let (svc, sink, other) = diverge_setup(&fx, TWO_SECTIONS);

    // Here: Alpha changes. Elsewhere: Beta changes, pushed first.
    svc.update("Plan.md", ChangeSource::Agent, |c| {
        Ok(shelbi_wiki::markdown::sections::write_section(c, "## Alpha", "\nAlpha, edited here.\n\n").unwrap())
    })
    .unwrap();
    push_from(&fx, &other, "Plan.md", &TWO_SECTIONS.replace("Beta text.", "Beta, edited elsewhere."));

    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Synced, "{status:?}");
    assert!(status.conflicts.is_empty());
    let merged = std::fs::read_to_string(fx.worktree().join("Plan.md")).unwrap();
    assert!(merged.contains("Alpha, edited here."), "{merged}");
    assert!(merged.contains("Beta, edited elsewhere."), "{merged}");
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), merged, "the merge is pushed");
    assert!(sink.pulled().contains(&"Plan.md".to_string()));
}

#[test]
fn an_unresolvable_edit_is_held_reported_and_resolved() {
    let fx = Fixture::new();
    let (svc, _sink, other) = diverge_setup(&fx, TWO_SECTIONS);

    svc.write("Plan.md", &TWO_SECTIONS.replace("Alpha text.", "Ours."), ChangeSource::Local).unwrap();
    let theirs = TWO_SECTIONS.replace("Alpha text.", "Theirs.");
    push_from(&fx, &other, "Plan.md", &theirs);

    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Conflict, "{status:?}");
    assert_eq!(status.conflicts, vec!["Plan.md".to_string()]);
    // The worktree keeps ours; origin keeps theirs (never overwritten).
    let local = std::fs::read_to_string(fx.worktree().join("Plan.md")).unwrap();
    assert!(local.contains("Ours."), "{local}");
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), theirs);
    let held = svc.conflict("Plan.md").unwrap();
    assert!(held.ours.unwrap().contains("Ours."));
    assert!(held.theirs.unwrap().contains("Theirs."));
    assert!(held.base.unwrap().contains("Alpha text."));

    // Other files keep syncing; the held one stays out of commits.
    svc.write("Other.md", "# Other\n", ChangeSource::Local).unwrap();
    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Conflict);
    assert_eq!(fx.origin_file("docs/planning", "Other.md").unwrap(), "# Other\n");
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), theirs, "held: not pushed");

    // The hold survives a restart.
    drop(svc);
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), Arc::new(Recorder::default())).unwrap();
    assert_eq!(svc.status().conflicts, vec!["Plan.md".to_string()]);
    assert_eq!(svc.status().status, SyncStatus::Conflict);

    // Resolving commits it and releases the hold.
    let resolved = TWO_SECTIONS.replace("Alpha text.", "Both.");
    svc.resolve("Plan.md", Some(&resolved)).unwrap();
    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Synced, "{status:?}");
    assert!(status.conflicts.is_empty());
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), resolved);
    assert_eq!(fx.origin_log("docs/planning")[0], "Resolve merge conflicts");
}

/// A service holding `Plan.md`: "Ours." here, "Theirs." on origin.
fn held_setup(fx: &Fixture) -> (WikiService, Arc<Recorder>, String, String) {
    let (svc, sink, other) = diverge_setup(fx, TWO_SECTIONS);
    let ours = TWO_SECTIONS.replace("Alpha text.", "Ours.");
    svc.write("Plan.md", &ours, ChangeSource::Local).unwrap();
    let theirs = TWO_SECTIONS.replace("Alpha text.", "Theirs.");
    push_from(fx, &other, "Plan.md", &theirs);
    assert_eq!(svc.sync_now(SYNC).status, SyncStatus::Conflict);
    (svc, sink, ours, theirs)
}

fn origin_trailers(fx: &Fixture) -> String {
    fx.git(&fx.origin(), &["log", "-1", "--format=%(trailers:key=Shelbi-By,valueonly)", "docs/planning"])
}

#[test]
fn keeping_ours_whole_commits_it_and_syncs() {
    let fx = Fixture::new();
    let (svc, _sink, ours, _) = held_setup(&fx);
    svc.resolve_with("Plan.md", ChangeSource::Local, |held| {
        assert_eq!(held.ours.as_deref(), Some(ours.as_str()));
        Ok(held.ours.clone())
    })
    .unwrap();
    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Synced, "{status:?}");
    assert!(status.conflicts.is_empty());
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), ours);
    assert_eq!(fx.origin_log("docs/planning")[0], "Resolve merge conflicts");
    assert_eq!(origin_trailers(&fx).trim(), "person");
}

#[test]
fn taking_theirs_whole_still_commits_the_resolution() {
    let fx = Fixture::new();
    let (svc, sink, _, theirs) = held_setup(&fx);
    svc.resolve_with("Plan.md", ChangeSource::Agent, |held| Ok(held.theirs.clone())).unwrap();
    // The worktree takes theirs, and the clients hear of it.
    assert_eq!(std::fs::read_to_string(fx.worktree().join("Plan.md")).unwrap(), theirs);
    assert!(sink
        .changes
        .lock()
        .unwrap()
        .iter()
        .any(|(p, s)| p.contains(&"Plan.md".to_string()) && *s == ChangeSource::Agent));
    let status = svc.sync_now(SYNC);
    assert_eq!(status.status, SyncStatus::Synced, "{status:?}");
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), theirs);
    // Nothing changed in the file, but the history says it was settled.
    assert_eq!(fx.origin_log("docs/planning")[0], "Resolve merge conflicts");
    assert_eq!(origin_trailers(&fx).trim(), "agent");
}

#[test]
fn an_edit_while_held_is_ours_and_a_refused_resolution_keeps_the_hold() {
    let fx = Fixture::new();
    let (svc, _sink, ours, theirs) = held_setup(&fx);
    // Ours is the file as it is now, not as it was when the pull found it.
    let edited = ours.replace("Beta text.", "Beta, edited while held.");
    svc.write("Plan.md", &edited, ChangeSource::Local).unwrap();
    assert_eq!(svc.conflict("Plan.md").unwrap().ours.as_deref(), Some(edited.as_str()));

    let refused = svc.resolve_with("Plan.md", ChangeSource::Local, |_| {
        Err(shelbi_wiki::service::OpError::Conflict("stale".into()))
    });
    assert!(refused.is_err());
    assert_eq!(svc.status().conflicts, vec!["Plan.md".to_string()]);
    assert_eq!(std::fs::read_to_string(fx.worktree().join("Plan.md")).unwrap(), edited, "nothing written");
    assert_eq!(fx.origin_file("docs/planning", "Plan.md").unwrap(), theirs);

    // A file that isn't held can't be resolved.
    assert!(svc.resolve("README.md", Some("# x\n")).is_err());
}

#[test]
fn an_unreachable_remote_reads_offline() {
    let fx = Fixture::new();
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), Arc::new(Recorder::default())).unwrap();
    assert_eq!(svc.sync_now(SYNC).status, SyncStatus::Synced);
    std::fs::rename(fx.origin(), fx.root.join("gone.git")).unwrap();
    svc.write("a.md", "# A\n", ChangeSource::Local).unwrap();
    assert_eq!(svc.sync_now(SYNC).status, SyncStatus::Offline);
    std::fs::rename(fx.root.join("gone.git"), fx.origin()).unwrap();
    assert_eq!(svc.sync_now(SYNC).status, SyncStatus::Synced);
    assert_eq!(fx.origin_file("docs/planning", "a.md").unwrap(), "# A\n");
}

#[derive(Default)]
struct FakePrs {
    open: Mutex<Option<PullRequest>>,
    created: Mutex<u32>,
}

impl PullRequests for FakePrs {
    fn ensure_open(&self, _worktree: &Path, head: &str, base: &str) -> Result<PullRequest, String> {
        assert_eq!((head, base), ("shelbi/wiki-docs", "main"));
        let mut open = self.open.lock().unwrap();
        if let Some(pr) = open.clone() {
            return Ok(pr);
        }
        let mut created = self.created.lock().unwrap();
        *created += 1;
        let pr = PullRequest {
            number: *created as u64,
            url: format!("https://example.invalid/pull/{created}"),
        };
        *open = Some(pr.clone());
        Ok(pr)
    }
}

#[test]
fn folder_mode_commits_to_the_rolling_branch_and_keeps_one_pr() {
    let fx = Fixture::new();
    let prs = Arc::new(FakePrs::default());
    let layout = Layout::folder(fx.hub(), fx.worktree(), "shelbi/wiki-docs", "docs", "main");
    let mut spec = fx.spec(layout, true);
    spec.pull_requests = Some(prs.clone());
    let sink = Arc::new(Recorder::default());
    let (svc, setup) = WikiService::start(spec, sink.clone()).unwrap();
    assert_eq!(setup, Setup::CheckedOut);
    assert_eq!(svc.root(), fx.worktree().join("docs"));
    assert_eq!(svc.read("intro.md").unwrap(), "# Intro\n\nHello.\n", "main's docs show");

    // A stray file outside the folder is never committed.
    std::fs::write(fx.worktree().join("stray.txt"), "no\n").unwrap();
    svc.write("guide.md", "# Guide\n", ChangeSource::Local).unwrap();
    wait_for("the edit on the rolling branch", SYNC, || fx.origin_file("shelbi/wiki-docs", "docs/guide.md").is_some());
    assert_eq!(fx.origin_log("shelbi/wiki-docs")[0], "Add guide.md");
    assert!(fx.origin_file("shelbi/wiki-docs", "stray.txt").is_none());
    assert!(fx.origin_file("main", "docs/guide.md").is_none(), "main is untouched until the PR merges");
    wait_for("the PR", SYNC, || svc.status().pull_request.is_some());

    // A second edit rides the same PR.
    svc.write("intro.md", "# Intro\n\nHello again.\n", ChangeSource::Local).unwrap();
    wait_for("the second edit", SYNC, || {
        fx.origin_file("shelbi/wiki-docs", "docs/intro.md").as_deref() == Some("# Intro\n\nHello again.\n")
    });
    let _ = svc.sync_now(SYNC);
    assert_eq!(*prs.created.lock().unwrap(), 1, "one rolling PR");

    // Main moving on is merged in, so the wiki shows main's docs too.
    let other = fx.elsewhere("other", "main");
    push_main(&fx, &other, "docs/new-on-main.md", "# New\n");
    let status = svc.sync_now(SYNC);
    assert_ne!(status.status, SyncStatus::Conflict);
    assert_eq!(svc.read("new-on-main.md").unwrap(), "# New\n");
    assert!(sink.pulled().contains(&"new-on-main.md".to_string()));
}

fn push_main(fx: &Fixture, dir: &Path, path: &str, content: &str) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, content).unwrap();
    fx.git(dir, &["add", "-A"]);
    fx.git(dir, &["commit", "-q", "-m", "main moves"]);
    fx.git(dir, &["push", "-q", "origin", "main"]);
}

#[test]
fn writes_refuse_paths_outside_the_wiki() {
    let fx = Fixture::new();
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), Arc::new(Recorder::default())).unwrap();
    assert!(svc.write("../escape.md", "x", ChangeSource::Local).is_err());
    assert!(svc.write(".git/config", "x", ChangeSource::Local).is_err());
    assert!(!fx.root.join("home/projects/demo/escape.md").exists());
}

/// `diff` and `log` read wiki-relative in folder mode (the repo folder is
/// stripped), include new files, and narrow to one path.
#[test]
fn diff_and_log_answer_wiki_relative_paths_in_folder_mode() {
    let fx = Fixture::new();
    let layout = Layout::folder(fx.hub(), fx.worktree(), "shelbi/wiki-docs", "docs", "main");
    let mut spec = fx.spec(layout, false);
    spec.pull_requests = Some(Arc::new(FakePrs::default()));
    let (svc, _) = WikiService::start(spec, Arc::new(Recorder::default())).unwrap();
    assert_eq!(svc.diff(None).unwrap(), "");

    svc.write("intro.md", "# Intro\n\nChanged.\n", ChangeSource::Local).unwrap();
    svc.write("Guides/new.md", "# New\n", ChangeSource::Local).unwrap();
    std::fs::write(fx.worktree().join("stray.txt"), "outside the wiki\n").unwrap();
    let diff = svc.diff(None).unwrap();
    assert!(diff.contains("a/intro.md") && diff.contains("+Changed."), "{diff}");
    assert!(diff.contains("b/Guides/new.md") && diff.contains("+# New"), "{diff}");
    assert!(!diff.contains("docs/") && !diff.contains("stray"), "{diff}");
    let one = svc.diff(Some("Guides/new.md")).unwrap();
    assert!(one.contains("+# New") && !one.contains("intro"), "{one}");

    let _ = svc.sync_now(SYNC);
    assert_eq!(svc.diff(None).unwrap(), "");
    let log = svc.log(None, 10).unwrap();
    assert!(log.len() >= 2, "{log:?}");
    assert!(log[0].paths.contains(&"Guides/new.md".to_string()), "{log:?}");
    assert!(log.iter().all(|c| c.paths.iter().all(|p| !p.starts_with("docs/"))), "{log:?}");
    let intro = svc.log(Some("intro.md"), 1).unwrap();
    assert_eq!(intro.len(), 1);
    assert_eq!(intro[0].paths, vec!["intro.md".to_string()]);
    assert!(svc.log(Some("../main.txt"), 5).is_err());
}

/// A commit says who made its changes (`Shelbi-By`), and `log` reads it
/// back: an agent's write alone, a person's alone, both when mixed.
#[test]
fn log_says_who_made_each_commits_changes() {
    let fx = Fixture::new();
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), Arc::new(Recorder::default())).unwrap();
    svc.write("plan.md", "# Plan\n", ChangeSource::Agent).unwrap();
    let _ = svc.sync_now(SYNC);
    assert_eq!(svc.log(Some("plan.md"), 1).unwrap()[0].by, vec!["agent".to_string()]);

    svc.write("plan.md", "# Plan\n\nMine.\n", ChangeSource::Local).unwrap();
    let _ = svc.sync_now(SYNC);
    let last = &svc.log(Some("plan.md"), 1).unwrap()[0];
    assert_eq!(last.by, vec!["person".to_string()]);
    assert!(!last.subject.contains("Shelbi-By"), "the trailer stays out of the subject: {last:?}");

    svc.write("plan.md", "# Plan\n\nBoth.\n", ChangeSource::Local).unwrap();
    svc.write("plan.md", "# Plan\n\nBoth, then.\n", ChangeSource::Agent).unwrap();
    let _ = svc.sync_now(SYNC);
    let mut by = svc.log(Some("plan.md"), 1).unwrap()[0].by.clone();
    by.sort();
    assert_eq!(by, vec!["agent".to_string(), "person".to_string()]);
}

/// A trashed document or folder (empty subfolders too) comes back where it
/// was on undo, once; a path taken since refuses it.
#[test]
fn a_trashed_folder_is_put_back_by_its_token() {
    let fx = Fixture::new();
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), Arc::new(Recorder::default())).unwrap();
    svc.write("Plans/a.md", "# A\n", ChangeSource::Local).unwrap();
    svc.write("Plans/Deep/b.md", "# B\n", ChangeSource::Local).unwrap();
    svc.create_folder("Plans/Empty", ChangeSource::Local).unwrap();
    let token = svc.trash("Plans", ChangeSource::Local).unwrap();
    assert!(svc.read("Plans/a.md").is_err());

    assert_eq!(svc.untrash(&token, ChangeSource::Local).unwrap(), "Plans");
    assert_eq!(svc.read("Plans/a.md").unwrap(), "# A\n");
    assert_eq!(svc.read("Plans/Deep/b.md").unwrap(), "# B\n");
    assert!(svc.resolve_path("Plans/Empty").unwrap().is_dir());
    assert!(svc.untrash(&token, ChangeSource::Local).is_err(), "an undo runs once");

    let token = svc.trash("Plans/a.md", ChangeSource::Local).unwrap();
    svc.write("Plans/a.md", "# Someone else's\n", ChangeSource::Local).unwrap();
    assert!(svc.untrash(&token, ChangeSource::Local).is_err());
    assert_eq!(svc.read("Plans/a.md").unwrap(), "# Someone else's\n");
}

/// A folder order lands in `folderOrder`, keeping the settings' other keys.
#[test]
fn a_folder_order_is_written_to_the_space_settings() {
    let fx = Fixture::new();
    let (svc, _) = WikiService::start(fx.spec(fx.branch_layout(), false), Arc::new(Recorder::default())).unwrap();
    svc.create_folder("Plans/B", ChangeSource::Local).unwrap();
    svc.create_folder("Plans/A", ChangeSource::Local).unwrap();
    let before = shelbi_wiki::fs::settings::load_space_settings(svc.root()).unwrap();
    svc.set_folder_order("Plans", &["B".into(), "A".into()], ChangeSource::Local).unwrap();
    let after = shelbi_wiki::fs::settings::load_space_settings(svc.root()).unwrap();
    assert_eq!(after.name, before.name);
    assert_eq!(after.folder_order.unwrap()["Plans"], vec!["B".to_string(), "A".to_string()]);
    assert!(svc.set_folder_order("Plans", &["x/y".into()], ChangeSource::Local).is_err());
    assert!(svc.set_folder_order("Nope", &["A".into()], ChangeSource::Local).is_err());
    svc.set_folder_order("Plans", &[], ChangeSource::Local).unwrap();
    let cleared = shelbi_wiki::fs::settings::load_space_settings(svc.root()).unwrap();
    assert!(cleared.folder_order.is_none_or(|m| !m.contains_key("Plans")));
}
