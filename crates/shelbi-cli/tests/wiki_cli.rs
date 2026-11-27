//! `shelbi wiki`: every command through a real hub.
//!
//! The test stands up an isolated `SHELBI_HOME` under `/tmp` (short socket
//! paths: macOS caps Unix-socket paths at ~104 bytes) holding one project
//! whose hub checkout is a clone of a local bare origin, with a `wiki:`
//! block in manual sync (so nothing commits behind the test's back), spawns
//! a real `shelbi daemon` against it, and drives the `shelbi wiki` binary.
//! The daemon's git runs under the test's own git config.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

struct Fixture {
    home: PathBuf,
    root: PathBuf,
    hub_sock: PathBuf,
    control_sock: PathBuf,
    gitconfig: PathBuf,
    daemon: Option<Child>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let base = PathBuf::from(format!("/tmp/shb-wiki-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("h");
        let root = base.join("r");
        std::fs::create_dir_all(home.join("projects/demo")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let gitconfig = root.join("gitconfig");
        std::fs::write(
            &gitconfig,
            "[user]\n\tname = Test\n\temail = test@example.invalid\n[init]\n\tdefaultBranch = main\n\
             [commit]\n\tgpgsign = false\n",
        )
        .unwrap();
        let fx = Self {
            hub_sock: PathBuf::from(format!("/tmp/shb-wiki-{tag}-{}-h.sock", std::process::id())),
            control_sock: PathBuf::from(format!("/tmp/shb-wiki-{tag}-{}-c.sock", std::process::id())),
            home,
            root,
            gitconfig,
            daemon: None,
        };
        let _ = std::fs::remove_file(&fx.hub_sock);
        let _ = std::fs::remove_file(&fx.control_sock);
        fx.git(&fx.root, &["init", "-q", "--bare", "origin.git"]);
        fx.git(&fx.root, &["clone", "-q", "origin.git", "hub"]);
        let hub = fx.root.join("hub");
        std::fs::write(hub.join("main.txt"), "code\n").unwrap();
        fx.git(&hub, &["add", "-A"]);
        fx.git(&hub, &["commit", "-q", "-m", "init"]);
        fx.git(&hub, &["push", "-q", "origin", "main"]);
        std::fs::write(
            fx.home.join("projects/demo.yaml"),
            format!(
                "name: Demo\nrepo: {hub}\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: {hub}\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n\
                 wiki:\n  source: branch\n  sync: manual\n",
                hub = hub.display()
            ),
        )
        .unwrap();
        // Keep the daemon alive (it exits when no project is open).
        std::fs::write(fx.home.join("projects/demo/state.json"), br#"{"open":true}"#).unwrap();
        fx
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn env(&self, cmd: &mut Command) {
        cmd.env("SHELBI_HOME", &self.home)
            .env("SHELBI_HUB_SOCK", &self.hub_sock)
            .env("SHELBI_CONTROL_SOCK", &self.control_sock)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_MANAGED_CONTEXT")
            .env_remove("SHELBI_WORKSPACE");
    }

    fn start_daemon(&mut self) {
        let mut cmd = Command::new(BIN);
        self.env(&mut cmd);
        let child = cmd
            .arg("daemon")
            .env("SHELBI_DAEMON_IDLE_GRACE_MS", "3000")
            .env("SHELBI_DAEMON_IDLE_POLL_MS", "500")
            .env("SHELBI_YES", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn shelbi daemon");
        self.daemon = Some(child);
        let start = Instant::now();
        while !(self.control_sock.exists() && std::os::unix::net::UnixStream::connect(&self.control_sock).is_ok()) {
            assert!(start.elapsed() < Duration::from_secs(15), "control socket never came up");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `shelbi -p demo wiki <args>`, with `stdin` piped in (else none).
    fn wiki(&self, args: &[&str], stdin: Option<&str>) -> Output {
        self.wiki_env(args, stdin, &[])
    }

    fn wiki_env(&self, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(BIN);
        self.env(&mut cmd);
        cmd.args(["-p", "demo", "wiki"])
            .args(args)
            .envs(env.iter().copied())
            .current_dir(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        let mut child = cmd.spawn().unwrap();
        if let Some(text) = stdin {
            child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
        }
        child.wait_with_output().unwrap()
    }

    /// [`Self::wiki`], requiring success, answering stdout.
    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(args, None)
    }

    fn ok_in(&self, args: &[&str], stdin: Option<&str>) -> String {
        let out = self.wiki(args, stdin);
        assert!(
            out.status.success(),
            "wiki {args:?}: exit {:?}\nstdout: {}\nstderr: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut args = args.to_vec();
        args.extend(["--format", "json"]);
        let out = self.ok(&args);
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("wiki {args:?} isn't JSON ({e}): {out}"))
    }

    fn code(&self, args: &[&str]) -> i32 {
        self.wiki(args, None).status.code().unwrap_or(-1)
    }

    fn wiki_file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.home.join("projects/demo/wiki").join(rel)).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            // Our own child, never a pid read from anywhere else.
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = std::fs::remove_dir_all(self.home.parent().unwrap());
        let _ = std::fs::remove_file(&self.hub_sock);
        let _ = std::fs::remove_file(&self.control_sock);
    }
}

#[test]
fn every_wiki_command_works_through_the_hub() {
    let mut fx = Fixture::new("all");
    fx.start_daemon();

    // sync status: set up, manual, on the default orphan branch.
    let status = fx.json(&["sync", "status"]);
    assert_eq!((status["set_up"].as_bool(), status["branch"].as_str()), (Some(true), Some("docs/planning")));
    assert!(fx.ok(&["sync"]).contains("(manual)"));

    // new: a document (title from --title, content after it), from stdin, and
    // a folder with a description.
    assert_eq!(
        fx.ok(&["new", "Plans/plan.md", "--title", "The Plan", "--content", "## Goals\n\n- [ ] Ship it\n- [ ] Tell people\n\n## Notes\n\nNone yet."]),
        "Created document: Plans/plan.md\n"
    );
    assert_eq!(fx.wiki_file("Plans/plan.md"), "# The Plan\n\n## Goals\n\n- [ ] Ship it\n- [ ] Tell people\n\n## Notes\n\nNone yet.\n");
    fx.ok_in(&["new", "Plans/piped.md"], Some("Piped body."));
    assert_eq!(fx.wiki_file("Plans/piped.md"), "# Piped\n\nPiped body.\n");
    assert_eq!(fx.ok(&["new", "Research", "--content", "Background reading"]), "Created folder: Research\n");
    assert!(fx.wiki_file("Research/.contextstore/settings.yml").contains("Background reading"));
    let created = fx.json(&["new", "Research/r.md"]);
    assert_eq!(created["ok"], true);
    assert_eq!(fx.code(&["new", "Research/r.md"]), 3, "an existing document is invalid");

    // read: whole, one section, head/tail, JSON.
    assert!(fx.ok(&["read", "Plans/plan.md"]).starts_with("# The Plan\n"));
    assert_eq!(fx.ok(&["read", "Plans/plan.md", "--section", "## Notes"]), "## Notes\n\nNone yet.\n");
    assert_eq!(fx.ok(&["read", "Plans/plan.md", "--head", "1"]), "# The Plan\n");
    assert_eq!(fx.ok(&["read", "Plans/plan.md", "--tail", "1"]), "None yet.\n");
    let read = fx.json(&["read", "Plans/plan.md"]);
    assert_eq!((read["title"].as_str(), read["path"].as_str()), (Some("The Plan"), Some("Plans/plan.md")));

    // Frontmatter for the section commands to keep.
    fx.ok(&["write", "Plans/plan.md", "--content", "---\nstatus: draft\n---\n# The Plan\n\n## Goals\n\n- [ ] Ship it\n- [ ] Tell people\n\n## Notes\n\nNone yet.\n"]);

    // write a section, append/prepend to a section and to the document: the
    // frontmatter and the H1 stay put.
    fx.ok(&["write", "Plans/plan.md", "--section", "## Notes", "--content", "Rewritten."]);
    fx.ok(&["append", "Plans/plan.md", "--section", "## Goals", "--content", "- [x] Plan it"]);
    fx.ok(&["prepend", "Plans/plan.md", "--section", "## Notes", "--content", "First note."]);
    fx.ok_in(&["append", "Plans/plan.md"], Some("## Later\n\nAt the end."));
    fx.ok(&["prepend", "Plans/plan.md", "--content", "> Summary."]);
    let text = fx.wiki_file("Plans/plan.md");
    assert!(text.starts_with("---\nstatus: draft\n---\n\n# The Plan\n\n> Summary.\n\n## Goals"), "{text}");
    assert!(text.contains("- [ ] Tell people\n- [x] Plan it\n\n## Notes"), "{text}");
    assert!(text.contains("## Notes\n\nFirst note.\n\nRewritten.\n"), "{text}");
    assert!(text.ends_with("At the end.\n"), "{text}");

    // rm a section keeps the rest (frontmatter and H1 included).
    fx.ok(&["rm", "Plans/plan.md", "--section", "## Later"]);
    let text = fx.wiki_file("Plans/plan.md");
    assert!(!text.contains("At the end.") && text.starts_with("---\nstatus: draft\n---\n") && text.contains("# The Plan"), "{text}");

    // write the whole document without an H1 or frontmatter: both kept.
    fx.ok(&["write", "Plans/piped.md", "--content", "Replaced."]);
    assert_eq!(fx.wiki_file("Plans/piped.md"), "# Piped\n\nReplaced.\n");
    fx.ok(&["write", "Plans/piped.md", "--content", "Again.", "--title", "Renamed"]);
    assert_eq!(fx.wiki_file("Plans/piped.md"), "# Renamed\n\nAgain.\n");

    // tasks, check, uncheck.
    assert_eq!(fx.ok(&["tasks", "Plans/plan.md", "--pending"]), "[ ] Ship it\n[ ] Tell people\n");
    fx.ok(&["check", "Plans/plan.md", "ship"]);
    fx.ok(&["uncheck", "Plans/plan.md", "Plan it", "--section", "## Goals"]);
    assert_eq!(fx.ok(&["tasks", "Plans/plan.md"]), "[x] Ship it\n[ ] Tell people\n[ ] Plan it\n");
    assert_eq!(fx.ok(&["tasks", "Plans/", "--pending"]), "Plans/plan.md: [ ] Tell people\nPlans/plan.md: [ ] Plan it\n");
    let tasks = fx.json(&["tasks", "Plans/plan.md"]);
    assert_eq!(tasks[0]["checked"], true);
    assert_eq!(fx.code(&["check", "Plans/plan.md", "nothing like it"]), 2);
    assert_eq!(fx.code(&["check", "Plans/plan.md", "t"]), 3, "more than one task matches");

    // ls: folders then documents, stats, a document's headings, JSON.
    let root = fx.ok(&["ls"]);
    assert!(root.contains("Plans/\n") && root.contains("Research/\n") && root.contains("README.md\n"), "{root}");
    assert_eq!(fx.ok(&["ls", "Plans"]), "piped.md\nplan.md\n");
    let stats = fx.ok(&["ls", "Plans", "--stats"]);
    assert!(stats.contains("lines") && stats.contains("2 documents"), "{stats}");
    assert_eq!(fx.ok(&["ls", "Plans/plan.md"]), "# The Plan\n## Goals\n## Notes\n");
    assert!(fx.ok(&["ls", "Plans/plan.md", "--stats"]).contains("## Goals"));
    let ls = fx.json(&["ls", "Plans", "--stats"]);
    assert_eq!(ls[0]["type"], "document");
    assert!(ls[0]["words"].as_u64().unwrap() > 0);

    // tree.
    let tree = fx.ok(&["tree", "--documents"]);
    assert!(tree.contains("├── Plans/") && tree.contains("│   ├── piped.md") && tree.contains("README.md"), "{tree}");
    assert!(!fx.ok(&["tree"]).contains("README.md"));
    assert!(!fx.ok(&["tree", "--documents", "-L", "1"]).contains("piped.md"));
    let tree = fx.json(&["tree", "Plans", "--documents"]);
    assert_eq!(tree[0]["kind"], "Document");

    // cat.
    let cat = fx.ok(&["cat", "Plans/", "--headers"]);
    assert!(cat.starts_with("--- Plans/piped.md ---\n\n# Renamed") && cat.contains("--- Plans/plan.md ---"), "{cat}");
    let cat = fx.json(&["cat", "Plans/piped.md", "Research/r.md"]);
    assert_eq!(cat.as_array().unwrap().len(), 2);

    // search.
    let line = fx.wiki_file("Plans/plan.md").lines().position(|l| l == "Rewritten.").unwrap() + 1;
    let hit = format!("Plans/plan.md:{line}: Rewritten.\n");
    assert_eq!(fx.ok(&["search", "rewritten"]), hit);
    assert_eq!(fx.ok(&["search", "R[a-z]+en\\.", "--regex", "--folder", "Plans"]), hit);
    assert_eq!(fx.ok(&["search", "Again", "--document", "Plans/plan.md"]), "");
    let found = fx.json(&["search", "again"]);
    assert_eq!(found[0]["path"], "Plans/piped.md");
    assert_eq!(fx.code(&["search", "(", "--regex"]), 3);

    // mv, cp, rm.
    assert_eq!(fx.ok(&["mv", "Plans/piped.md", "Research/"]), "Moved Plans/piped.md → Research\n");
    fx.ok(&["cp", "Research/piped.md", "Plans/copy.md"]);
    assert_eq!(fx.wiki_file("Plans/copy.md"), "# Renamed\n\nAgain.\n");
    fx.ok(&["rm", "Plans/copy.md"]);
    assert!(!fx.home.join("projects/demo/wiki/Plans/copy.md").exists());
    assert_eq!(fx.code(&["rm", "Plans/copy.md"]), 2);

    // edit, through an "editor" that rewrites the file.
    let editor = fx.root.join("editor.sh");
    std::fs::write(&editor, "#!/bin/sh\nprintf '# Edited\\n\\nBy the editor.\\n' > \"$1\"\n").unwrap();
    std::fs::set_permissions(&editor, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = fx.wiki_env(&["edit", "Research/r.md"], None, &[("EDITOR", editor.to_str().unwrap())]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fx.wiki_file("Research/r.md"), "# Edited\n\nBy the editor.\n");

    // open: announced (no app launched here); a missing path is not found.
    assert!(fx.ok(&["open", "Plans/plan.md", "--no-launch"]).contains("Plans/plan.md"));
    assert_eq!(fx.code(&["open", "Nope.md", "--no-launch"]), 2);

    // diff: the uncommitted change (manual sync), new files included.
    let diff = fx.ok(&["diff"]);
    assert!(diff.contains("Plans/plan.md") && diff.contains("+Rewritten."), "{diff}");
    let one = fx.ok(&["diff", "Research/r.md"]);
    assert!(one.contains("+By the editor.") && !one.contains("plan.md"), "{one}");
    assert!(fx.json(&["diff"])["diff"].as_str().unwrap().contains("Rewritten."));

    // sync now commits and pushes; log shows it; diff is empty after.
    let synced = fx.json(&["sync", "now"]);
    assert_eq!(synced["state"], "synced", "{synced}");
    assert_eq!(fx.ok(&["diff"]), "");
    let log = fx.ok(&["log"]);
    assert!(log.lines().count() >= 2, "{log}");
    let commits = fx.json(&["log", "Plans/plan.md", "-n", "1"]);
    assert_eq!(commits.as_array().unwrap().len(), 1);
    assert!(commits[0]["paths"].as_array().unwrap().iter().any(|p| p == "Plans/plan.md"), "{commits}");
    let pushed = fx.git(&fx.root.join("origin.git"), &["show", "docs/planning:Plans/plan.md"]);
    assert!(pushed.contains("Rewritten."), "{pushed}");

    // pause / resume.
    assert_eq!(fx.json(&["sync", "pause"])["paused"], true);
    assert_eq!(fx.json(&["sync", "resume"])["paused"], false);

    // Exit codes: a missing document is 2 (text and JSON), bad arguments 3.
    let missing = fx.wiki(&["read", "Plans/nope.md", "--agent"], None);
    assert_eq!(missing.status.code(), Some(2));
    let err: serde_json::Value = serde_json::from_slice(&missing.stderr).unwrap();
    assert_eq!(err["error"]["kind"], "not_found");
    assert_eq!(fx.code(&["read", "Plans/plan.md", "--section", "## Missing"]), 2);
    assert_eq!(fx.code(&["read"]), 3);
    assert_eq!(fx.code(&["frobnicate"]), 3);
    assert_eq!(fx.code(&["write", "Plans/plan.md"]), 3, "no content");
    assert_eq!(fx.code(&["write", "Plans/nope.md", "--content", "x"]), 2);
    assert_eq!(fx.code(&["read", "../outside.md"]), 3);
}
