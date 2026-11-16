//! `shelbi changelog add --image/--attach` and `shelbi changelog attach`:
//! files are copied into the entry's `assets/<entry-id>/` directory, recorded
//! in its frontmatter, images referenced from its description, a taken name
//! numbered (`shot.png`, `shot-2.png`), and every success prints each file's
//! final path (absolute and as the entry records it), in text and in JSON.
//!
//! Each test runs the real binary under its own `SHELBI_ROOT`, with no daemon.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// A one-pixel PNG.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

/// An isolated root holding one registered global project, `demo`, and a
/// directory of files to attach.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("shelbi-cl-attach-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let projects = shelbi_state::projects_dir_in(&path);
        std::fs::create_dir_all(projects.join("demo")).unwrap();
        let work = path.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            projects.join("demo.yaml"),
            format!(
                "name: demo\nrepo: {w}\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: {w}\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n",
                w = work.display()
            ),
        )
        .unwrap();
        let files = path.join("files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("shot.png"), PNG).unwrap();
        std::fs::write(files.join("build log.txt"), "ok\n").unwrap();
        Self { path }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .arg("-p")
            .arg("demo")
            .args(args)
            .current_dir(self.path.join("files"))
            .env("SHELBI_ROOT", &self.path)
            .env_remove("SHELBI_HOME")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_CONTROL_SOCK")
            .env_remove("SHELBI_MANAGED_CONTEXT")
            .env_remove("SHELBI_AGENT")
            .output()
            .unwrap()
    }

    fn changelog(&self) -> PathBuf {
        shelbi_state::projects_dir_in(&self.path).join("demo/changelog")
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// The `  <kind> <absolute>  (<relative>)` lines of a text success.
fn stored_lines(text: &str) -> Vec<(String, PathBuf, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (kind, rest) = l.split_once(' ')?;
            if kind != "image" && kind != "file" {
                return None;
            }
            let (abs, rel) = rest.trim_start().rsplit_once("  (")?;
            Some((kind.to_string(), PathBuf::from(abs), rel.trim_end_matches(')').to_string()))
        })
        .collect()
}

#[test]
fn add_with_an_image_and_a_file_stores_and_reports_both() {
    let root = Root::new("add");
    let out = ok(&root.run(&[
        "changelog",
        "add",
        "--title",
        "Board scrolls",
        "--author",
        "user",
        "-d",
        "Smooth now.",
        "--image",
        "shot.png:The board after",
        "--attach",
        "build log.txt",
    ]));
    let id = out
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("✓ added "))
        .expect(&out)
        .to_string();
    let stored = stored_lines(&out);
    let dir = root.changelog();
    assert_eq!(
        stored,
        [
            (
                "image".to_string(),
                dir.join(format!("assets/{id}/shot.png")),
                format!("assets/{id}/shot.png")
            ),
            (
                "file".to_string(),
                dir.join(format!("assets/{id}/build-log.txt")),
                format!("assets/{id}/build-log.txt")
            ),
        ],
        "{out}"
    );
    for (_, abs, _) in &stored {
        assert!(abs.is_file(), "{}", abs.display());
    }
    // Copied, not moved.
    assert!(root.path.join("files/shot.png").is_file());

    // The entry file stays readable Markdown: frontmatter, then the
    // description with the image referenced at its end.
    let text = std::fs::read_to_string(dir.join(format!("{id}.md"))).unwrap();
    assert!(text.contains("\nattachments:\n"), "{text}");
    assert!(
        text.ends_with(&format!("Smooth now.\n\n![The board after](assets/{id}/shot.png)\n")),
        "{text}"
    );

    // JSON lists every attachment with its absolute path; markdown keeps the
    // image and lists the file.
    let json: serde_json::Value = serde_json::from_str(&ok(&root.run(&[
        "changelog", "show", &id, "--format", "json",
    ])))
    .unwrap();
    let attachments = json["entry"]["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 2);
    assert_eq!(attachments[0]["kind"], "image");
    assert_eq!(attachments[1]["name"], "build-log.txt");
    assert_eq!(attachments[1]["size"], 3);
    assert_eq!(
        Path::new(attachments[1]["path"].as_str().unwrap()),
        dir.join(format!("assets/{id}/build-log.txt"))
    );
    assert_eq!(attachments[1]["relative_path"], format!("assets/{id}/build-log.txt"));
    let md = ok(&root.run(&["changelog", "show", &id]));
    assert!(md.contains(&format!("![The board after](assets/{id}/shot.png)")), "{md}");
    assert!(md.contains("Attachments:\n\n- [build-log.txt]("), "{md}");
    assert!(!md.contains("- [shot.png]"), "the inline image isn't listed again\n{md}");
}

#[test]
fn attach_numbers_a_taken_name_and_prints_the_final_path() {
    let root = Root::new("attach");
    let out = ok(&root.run(&[
        "changelog", "add", "--title", "Dup", "--author", "user", "-d", "x", "--image", "shot.png",
    ]));
    let id = out.lines().next().unwrap().strip_prefix("✓ added ").unwrap().to_string();

    let out = ok(&root.run(&[
        "changelog",
        "attach",
        &id[..16],
        "--image",
        "shot.png",
        "--image",
        "shot.png",
    ]));
    assert!(out.starts_with(&format!("✓ attached 2 files to {id}\n")), "{out}");
    let rel: Vec<String> = stored_lines(&out).into_iter().map(|(_, _, r)| r).collect();
    assert_eq!(rel, [format!("assets/{id}/shot-2.png"), format!("assets/{id}/shot-3.png")]);

    // --format json reports the same paths under `added`.
    let json: serde_json::Value = serde_json::from_str(&ok(&root.run(&[
        "changelog", "attach", &id, "--attach", "build log.txt", "--format", "json",
    ])))
    .unwrap();
    let added = json["added"].as_array().unwrap();
    assert_eq!(added.len(), 1);
    assert_eq!(added[0]["relative_path"], format!("assets/{id}/build-log.txt"));
    assert_eq!(
        Path::new(added[0]["path"].as_str().unwrap()),
        root.changelog().join(format!("assets/{id}/build-log.txt"))
    );
    assert_eq!(json["entry"]["attachments"].as_array().unwrap().len(), 4);
}

#[test]
fn a_bad_file_is_refused_and_nothing_is_written() {
    let root = Root::new("bad");
    for args in [
        &["--image", "build log.txt"][..],
        &["--attach", "missing.bin"],
        &["--image", "nope.png:alt"],
    ] {
        let mut full = vec!["changelog", "add", "--title", "T", "--author", "user", "-d", "x"];
        full.extend_from_slice(args);
        let out = root.run(&full);
        assert!(!out.status.success(), "{args:?} succeeded");
        assert!(!String::from_utf8_lossy(&out.stderr).is_empty(), "{args:?}: says why");
    }
    let entries = std::fs::read_dir(root.changelog()).map(|d| d.count()).unwrap_or(0);
    assert_eq!(entries, 0, "no entry written");

    let out = root.run(&["changelog", "attach", "nothing-here", "--attach", "build log.txt"]);
    assert!(!out.status.success());
    let out = root.run(&["changelog", "attach", "x"]);
    assert!(!out.status.success(), "attach with no files");
}
