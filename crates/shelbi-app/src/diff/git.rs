//! Reading a changeset out of a worktree with `git`, on whichever machine the
//! worktree is on. Everything here blocks: it runs on the session's worker
//! threads ([`super::session`]), never a client's UI thread.
//!
//! The result is a [`RawChangeset`]: the patch text and what surrounds it,
//! plain data a far daemon can send back over the control socket. Parsing it
//! into a [`Changeset`] is the same on every path.
//!
//! What each target shows:
//!
//! - **Review, all**: `merge-base..HEAD`, the branch as it would merge. A
//!   review slot is a checkout of the branch, and a reviewer's own edits or a
//!   dev server's generated files aren't what's being reviewed.
//! - **Workspace, all**: `merge-base` to the worktree, including work the
//!   agent hasn't committed and new files git doesn't track yet.
//! - **Uncommitted**: `HEAD` to the worktree, with the new files.
//! - **One commit**: its first parent to it.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use shelbi_core::Host;

use super::model::{Changeset, CommitInfo, CommitScope, FileDiff, FileStatus, Fnv};
use super::parse::parse_patch;
use super::TargetKind;

/// How long one git command may take.
pub const GIT_DEADLINE: Duration = Duration::from_secs(30);
/// New files beyond this many aren't diffed: an unignored `node_modules`
/// shouldn't take the view down. Each is one `git` run, and over SSH one round
/// trip.
pub const MAX_UNTRACKED: usize = 200;
/// A new file whose patch is larger than this is listed without its body.
pub const MAX_UNTRACKED_PATCH: usize = 4 * 1024 * 1024;
/// The commits listed for "review one commit".
pub const MAX_COMMITS: usize = 500;

/// Runs git in one worktree.
pub trait GitRunner: Send + Sync {
    /// Run `git <args>` and return stdout (bytes that aren't UTF-8 read as
    /// U+FFFD) when the exit status is one of `ok`.
    fn git(&self, args: &[String], ok: &[i32]) -> Result<String, String>;
}

/// [`GitRunner`] for a worktree on `host`: this machine, or another over the
/// SSH transport every other worktree operation uses.
#[derive(Debug, Clone)]
pub struct HostGit {
    pub host: Host,
    pub worktree: String,
}

impl GitRunner for HostGit {
    fn git(&self, args: &[String], ok: &[i32]) -> Result<String, String> {
        let mut argv: Vec<String> = vec!["git".into(), "-C".into(), self.worktree.clone()];
        argv.extend(pinned_config());
        argv.extend_from_slice(args);
        let out = shelbi_ssh::run_with_deadline(&self.host, &argv, GIT_DEADLINE)
            .map_err(|e| format!("git {}: {e}", args.first().map_or("", String::as_str)))?;
        let code = out.status.code();
        if code.is_some_and(|c| ok.contains(&c)) {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        Err(format!(
            "git {} failed: {}",
            args.first().map_or("", String::as_str),
            shelbi_ssh::describe_failure(&self.host, &out).trim()
        ))
    }
}

/// `-c` settings that would otherwise change the shape of what's read, from
/// the user's or the repo's config.
fn pinned_config() -> Vec<String> {
    [
        "core.quotePath=true",
        "color.ui=never",
        "diff.noprefix=false",
        "diff.mnemonicPrefix=false",
        "diff.relative=false",
        "diff.suppressBlankEmpty=false",
        "log.showSignature=false",
    ]
    .iter()
    .flat_map(|c| ["-c".to_string(), (*c).to_string()])
    .collect()
}

/// `git diff` with the options that make its output the one grammar
/// [`super::parse`] reads, ending in `from` (and `to`, when not the worktree).
fn diff_args(from: &str, to: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--unified=3",
        "--src-prefix=a/",
        "--dst-prefix=b/",
    ]
    .iter()
    .map(|a| (*a).to_string())
    .collect();
    args.push(from.to_string());
    if let Some(to) = to {
        args.push(to.to_string());
    }
    args.push("--".into());
    args
}

/// A changeset as read from git, before parsing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RawChangeset {
    pub scope: CommitScope,
    pub base: String,
    pub head: String,
    /// `git diff`'s output, and each new file's after it.
    pub patch: String,
    /// New files, by path.
    pub untracked: Vec<String>,
    /// New files whose patch was too large to load.
    pub oversized: Vec<String>,
    pub omitted_untracked: usize,
    pub commits: Vec<CommitInfo>,
}

impl RawChangeset {
    /// A hash of everything a reader sees: a re-read with the same digest
    /// changed nothing.
    pub fn digest(&self) -> u64 {
        let mut hash = Fnv::new();
        for part in [&self.base, &self.head, &self.patch] {
            hash.write(part.as_bytes());
            hash.write(&[0]);
        }
        for path in self.untracked.iter().chain(&self.oversized) {
            hash.write(path.as_bytes());
            hash.write(&[0]);
        }
        hash.write(&(self.omitted_untracked as u64).to_le_bytes());
        hash.finish()
    }

    /// The patch's size, in bytes.
    pub fn len(&self) -> usize {
        self.patch.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patch.is_empty() && self.oversized.is_empty()
    }

    /// Parse it.
    pub fn parse(&self) -> Changeset {
        let mut files = parse_patch(&self.patch);
        for file in &mut files {
            file.untracked = self.untracked.contains(&file.path);
        }
        for path in &self.oversized {
            files.push(FileDiff {
                path: path.clone(),
                old_path: None,
                status: FileStatus::Added,
                binary: false,
                additions: 0,
                deletions: 0,
                similarity: None,
                mode_change: None,
                untracked: true,
                omitted: Some(format!(
                    "Too large to show (over {} MB).",
                    MAX_UNTRACKED_PATCH / (1024 * 1024)
                )),
                hunks: Vec::new(),
                fingerprint: 0,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Changeset {
            scope: self.scope.clone(),
            base: self.base.clone(),
            head: self.head.clone(),
            additions: files.iter().map(|f| f.additions).sum(),
            deletions: files.iter().map(|f| f.deletions).sum(),
            files,
            commits: self.commits.clone(),
            omitted_untracked: self.omitted_untracked,
        }
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_string()).collect()
}

/// `git <list>`, trimmed, requiring success.
fn capture(git: &dyn GitRunner, list: &[&str]) -> Result<String, String> {
    Ok(git.git(&args(list), &[0])?.trim().to_string())
}

/// The commit a branch's changes are measured from. The fork point with
/// `origin/<base>` as well as `<base>`, taking the later of the two: a local
/// base can lag origin (a branch rebased onto a newer `origin/main` would
/// otherwise show every commit between them), and can lead it in a repo whose
/// base only moves locally.
pub fn merge_base(git: &dyn GitRunner, base_branch: &str) -> Result<String, String> {
    let local = capture(git, &["merge-base", base_branch, "HEAD"]);
    let remote = capture(git, &["merge-base", &format!("origin/{base_branch}"), "HEAD"]);
    match (local, remote) {
        (Ok(a), Ok(b)) if a == b => Ok(a),
        (Ok(a), Ok(b)) => {
            // Exits 0 when `a` is an ancestor of `b`, 1 when it isn't.
            let a_then_b = capture(git, &["merge-base", "--is-ancestor", &a, &b]).is_ok();
            Ok(if a_then_b { b } else { a })
        }
        (Ok(a), Err(_)) => Ok(a),
        (Err(_), Ok(b)) => Ok(b),
        (Err(e), Err(_)) => Err(format!("Couldn't find where this branch left `{base_branch}`: {e}")),
    }
}

/// Whether `rev` is a commit id: anything else from a far client is refused
/// rather than handed to git as a revision or an option.
fn is_commit_id(rev: &str) -> bool {
    (4..=64).contains(&rev.len()) && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commits in `base..HEAD`, newest first.
fn commits(git: &dyn GitRunner, base: &str) -> Result<Vec<CommitInfo>, String> {
    let max = format!("--max-count={MAX_COMMITS}");
    let range = format!("{base}..HEAD");
    let out = git.git(&args(&["log", "--no-color", &max, "--format=%H%x1f%h%x1f%an%x1f%at%x1f%s%x1e", &range]), &[0])?;
    Ok(out
        .split('\u{1e}')
        .filter_map(|record| {
            let mut parts = record.trim_start_matches('\n').splitn(5, '\u{1f}');
            Some(CommitInfo {
                sha: parts.next().filter(|s| !s.is_empty())?.to_string(),
                short: parts.next()?.to_string(),
                author: parts.next()?.to_string(),
                time: parts.next()?.parse().unwrap_or(0),
                subject: parts.next()?.to_string(),
            })
        })
        .collect())
}

/// The first parent of `sha`, or the empty tree for a root commit.
fn parent_of(git: &dyn GitRunner, sha: &str) -> Result<String, String> {
    match capture(git, &["rev-parse", "--verify", "--quiet", &format!("{sha}^1")]) {
        Ok(parent) => Ok(parent),
        Err(_) => capture(git, &["hash-object", "-t", "tree", "/dev/null"]),
    }
}

/// New files, as a patch appended to `raw`.
fn add_untracked(git: &dyn GitRunner, raw: &mut RawChangeset) -> Result<(), String> {
    let listing = git.git(&args(&["ls-files", "--others", "--exclude-standard", "-z"]), &[0])?;
    // A nested repository lists as `dir/`: nothing to diff.
    let paths: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty() && !p.ends_with('/')).collect();
    raw.omitted_untracked = paths.len().saturating_sub(MAX_UNTRACKED);
    for path in paths.into_iter().take(MAX_UNTRACKED) {
        let list = [
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-index",
            "--unified=3",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--",
            "/dev/null",
            path,
        ];
        // `--no-index` exits 1 when the two sides differ, which they do.
        let patch = git.git(&args(&list), &[0, 1])?;
        if patch.len() > MAX_UNTRACKED_PATCH {
            raw.oversized.push(path.to_string());
            continue;
        }
        raw.untracked.push(path.to_string());
        raw.patch.push_str(&patch);
    }
    Ok(())
}

/// Read `scope`'s changes for a `kind` target.
pub fn collect(
    git: &dyn GitRunner,
    kind: TargetKind,
    base_branch: &str,
    scope: &CommitScope,
) -> Result<RawChangeset, String> {
    let head = capture(git, &["rev-parse", "--verify", "HEAD"])
        .map_err(|e| format!("This worktree has no commit checked out: {e}"))?;
    let fork = merge_base(git, base_branch)?;
    let mut raw = RawChangeset {
        scope: scope.clone(),
        head: head.clone(),
        commits: commits(git, &fork)?,
        ..RawChangeset::default()
    };
    match (scope, kind) {
        (CommitScope::All, TargetKind::Review) => {
            raw.patch = git.git(&diff_args(&fork, Some(&head)), &[0])?;
            raw.base = fork;
        }
        (CommitScope::All, TargetKind::Workspace) => {
            raw.patch = git.git(&diff_args(&fork, None), &[0])?;
            raw.base = fork;
            add_untracked(git, &mut raw)?;
        }
        (CommitScope::Uncommitted, _) => {
            raw.patch = git.git(&diff_args(&head, None), &[0])?;
            raw.base = head;
            add_untracked(git, &mut raw)?;
        }
        (CommitScope::Commit(sha), _) => {
            if !is_commit_id(sha) {
                return Err(format!("`{sha}` isn't a commit id"));
            }
            let parent = parent_of(git, sha)?;
            raw.patch = git.git(&diff_args(&parent, Some(sha)), &[0])?;
            raw.base = parent;
        }
    }
    Ok(raw)
}

/// Who last changed one line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlameLine {
    /// The line's number in the file blamed.
    pub line: u32,
    pub short: String,
    pub author: String,
    /// Commit time, seconds since the epoch.
    pub time: i64,
    pub summary: String,
    /// The line isn't committed yet.
    pub uncommitted: bool,
}

/// Blame `path` at `rev` (the worktree when `None`).
pub fn blame(git: &dyn GitRunner, path: &str, rev: Option<&str>) -> Result<Vec<BlameLine>, String> {
    let mut list = args(&["blame", "--porcelain"]);
    if let Some(rev) = rev {
        if !is_commit_id(rev) {
            return Err(format!("`{rev}` isn't a commit id"));
        }
        list.push(rev.to_string());
    }
    list.push("--".into());
    list.push(path.to_string());
    Ok(parse_blame(&git.git(&list, &[0])?))
}

/// `git blame --porcelain` output.
pub fn parse_blame(text: &str) -> Vec<BlameLine> {
    #[derive(Default, Clone)]
    struct Commit {
        author: String,
        time: i64,
        summary: String,
    }
    let mut commits: HashMap<String, Commit> = HashMap::new();
    let mut out = Vec::new();
    let mut current: Option<(String, u32)> = None;
    for line in text.lines() {
        if line.starts_with('\t') {
            // The source line closes the entry.
            if let Some((sha, number)) = current.take() {
                let commit = commits.get(&sha).cloned().unwrap_or_default();
                out.push(BlameLine {
                    line: number,
                    short: sha.chars().take(8).collect(),
                    author: commit.author,
                    time: commit.time,
                    summary: commit.summary,
                    uncommitted: sha.bytes().all(|b| b == b'0'),
                });
            }
            continue;
        }
        let mut words = line.split(' ');
        let first = words.next().unwrap_or("");
        if first.len() >= 40 && first.bytes().all(|b| b.is_ascii_hexdigit()) {
            let number = words.nth(1).and_then(|n| n.parse().ok()).unwrap_or(0);
            current = Some((first.to_string(), number));
            continue;
        }
        let Some((sha, _)) = &current else { continue };
        let entry = commits.entry(sha.clone()).or_default();
        if let Some(rest) = line.strip_prefix("author ") {
            entry.author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            entry.time = rest.parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("summary ") {
            entry.summary = rest.to_string();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blame_output_reads_per_line() {
        let sha = "a".repeat(40);
        let text = format!(
            "{sha} 1 1 2\nauthor Ada\nauthor-mail <a@x>\nauthor-time 100\nsummary First\nfilename x\n\tone\n{sha} 2 2\n\ttwo\n{zero} 3 3 1\nauthor Not Committed Yet\nauthor-time 200\nsummary Version of x\nfilename x\n\tthree\n",
            zero = "0".repeat(40)
        );
        let lines = parse_blame(&text);
        assert_eq!(lines.len(), 3);
        assert_eq!((lines[1].line, lines[1].author.as_str(), lines[1].time, lines[1].summary.as_str()), (2, "Ada", 100, "First"));
        assert_eq!(lines[1].short, "aaaaaaaa");
        assert!(lines[2].uncommitted && !lines[0].uncommitted);
    }

    #[test]
    fn only_commit_ids_reach_git_as_revisions() {
        assert!(is_commit_id("abc1234"));
        assert!(!is_commit_id("--output=/tmp/x"));
        assert!(!is_commit_id("HEAD"));
    }

    #[test]
    fn the_digest_sees_the_patch() {
        let a = RawChangeset { patch: "x".into(), ..RawChangeset::default() };
        let b = RawChangeset { patch: "y".into(), ..RawChangeset::default() };
        assert_ne!(a.digest(), b.digest());
        assert_eq!(a.digest(), a.clone().digest());
    }
}
