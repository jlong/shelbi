//! Reverting a file or a hunk in a workspace's worktree. Nothing here moves
//! `HEAD` or touches a commit, so what it does to committed work can be undone
//! with `git checkout`:
//!
//! - **Uncommitted** work restores `HEAD`, after a confirm that says it's gone.
//! - **Committed** work is undone as an uncommitted change: the reverse written
//!   to the worktree, the commits left as they are.
//! - **Mixed** (a hunk that's part of each) reverses both the same way.
//!
//! It's two steps, because the confirm has to say which one this is. [`plan`]
//! reads the worktree as it is now and refuses a change that moved since it was
//! drawn; [`run`] re-checks that and writes. The write is `git apply -R`, which
//! either lands whole or leaves the file alone, so an edit on those lines
//! refuses it, with git's reason.
//!
//! A review's slot is a checkout of the branch: it has nothing of its own to
//! revert, and is refused here as well as in the view.

use serde::{Deserialize, Serialize};

use super::git::{self, GitRunner};
use super::model::{CommitScope, Eol, FileDiff, FileStatus, Hunk, LineKind};
use super::TargetKind;

/// What a client asks to revert, as it's drawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertSpec {
    pub scope: CommitScope,
    pub path: String,
    /// The hunk, by index in the file. `None` is the whole file.
    pub hunk: Option<u32>,
    /// [`FileDiff::fingerprint`] as drawn: a different one now means it changed
    /// under the reader, and what they confirmed isn't what's there.
    pub fingerprint: u64,
}

/// Whose work a revert would change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevertClass {
    Uncommitted,
    Committed,
    Mixed,
}

/// A checked revert, ready to confirm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevertPlan {
    pub spec: RevertSpec,
    pub class: RevertClass,
    /// What a person reads: `src/a.rs`, or `the hunk at src/a.rs:12`.
    pub label: String,
}

impl RevertPlan {
    /// The confirm's question.
    pub fn prompt(&self) -> String {
        let label = &self.label;
        match self.class {
            RevertClass::Uncommitted => format!("Revert {label} to HEAD? This uncommitted work will be gone."),
            RevertClass::Committed => format!("Undo {label} as an uncommitted change? Your commits stay as they are."),
            RevertClass::Mixed => format!(
                "Undo {label} as an uncommitted change? Your commits stay as they are; its uncommitted edits will be gone."
            ),
        }
    }

    /// What the status bar says when it's done.
    pub fn done(&self) -> String {
        let label = &self.label;
        match self.class {
            RevertClass::Uncommitted => format!("Reverted {label} to HEAD"),
            RevertClass::Committed | RevertClass::Mixed => format!("Undid {label} as an uncommitted change"),
        }
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_string()).collect()
}

/// `path` as a pathspec that means only that path: a file named `a*` mustn't
/// revert every file starting with `a`.
fn literal(path: &str) -> String {
    format!(":(literal){path}")
}

/// Refuse what a revert can't take: a review, or a worktree in the middle of a
/// merge, a rebase or a cherry-pick, whose half-applied state isn't anyone's to
/// write over.
fn refuse_unless_clean(git: &dyn GitRunner, kind: TargetKind) -> Result<(), String> {
    if kind == TargetKind::Review {
        return Err("A review is a checkout of the branch: there's nothing of its own to revert.".to_string());
    }
    for (head, doing) in [("MERGE_HEAD", "merge"), ("REBASE_HEAD", "rebase"), ("CHERRY_PICK_HEAD", "cherry-pick")] {
        let found = git.git(&args(&["rev-parse", "--quiet", "--verify", head]), &[0, 1])?;
        if !found.trim().is_empty() {
            return Err(format!("This worktree is in the middle of a {doing}: finish it first."));
        }
    }
    Ok(())
}

/// The file as it is now, refusing one that moved since `spec` was drawn.
fn current_file(git: &dyn GitRunner, kind: TargetKind, base_branch: &str, spec: &RevertSpec) -> Result<FileDiff, String> {
    let changeset = git::collect(git, kind, base_branch, &spec.scope)?.parse();
    let changed = || format!("{} changed since this was read: look again and retry.", spec.path);
    let file = changeset.files.into_iter().find(|f| f.path == spec.path).ok_or_else(changed)?;
    if file.fingerprint != spec.fingerprint {
        return Err(changed());
    }
    Ok(file)
}

/// The changed lines of a hunk, by where they land in the new file: what's the
/// same change whichever diff it's read from.
fn changes(hunk: &Hunk) -> Vec<(LineKind, u32, &str)> {
    let mut next_new = hunk.new_start;
    let mut out = Vec::new();
    for line in &hunk.lines {
        if let Some(new) = line.new {
            next_new = new + 1;
        }
        match line.kind {
            LineKind::Context => {}
            LineKind::Added => out.push((LineKind::Added, line.new.unwrap_or(next_new), line.text.as_str())),
            LineKind::Deleted => out.push((LineKind::Deleted, next_new, line.text.as_str())),
        }
    }
    out
}

/// Whose work `hunk` of `path` is: how much of it the worktree's own diff from
/// `HEAD` accounts for.
fn classify_hunk(git: &dyn GitRunner, kind: TargetKind, base_branch: &str, path: &str, hunk: &Hunk) -> Result<RevertClass, String> {
    let uncommitted = git::collect(git, kind, base_branch, &CommitScope::Uncommitted)?.parse();
    let ours: Vec<(LineKind, u32, String)> = uncommitted
        .files
        .iter()
        .find(|f| f.path == path)
        .map(|f| {
            f.hunks
                .iter()
                .flat_map(changes)
                .map(|(k, at, text)| (k, at, text.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let theirs = changes(hunk);
    let held = theirs.iter().filter(|(k, at, text)| ours.iter().any(|o| o.0 == *k && o.1 == *at && o.2 == *text)).count();
    Ok(match held {
        0 => RevertClass::Committed,
        n if n == theirs.len() => RevertClass::Uncommitted,
        _ => RevertClass::Mixed,
    })
}

/// Whether `paths` differ between two points (`to: None` is the worktree).
fn differs(git: &dyn GitRunner, from: &str, to: Option<&str>, paths: &[&str]) -> Result<bool, String> {
    let paths: Vec<String> = paths.iter().map(|p| literal(p)).collect();
    let mut list = vec!["diff", "--name-only", "--no-ext-diff", from];
    list.extend(to);
    list.push("--");
    list.extend(paths.iter().map(String::as_str));
    Ok(!git.git(&args(&list), &[0])?.trim().is_empty())
}

fn file_paths(file: &FileDiff) -> Vec<&str> {
    let mut paths = vec![file.path.as_str()];
    paths.extend(file.old_path.as_deref());
    paths
}

fn classify_file(git: &dyn GitRunner, base_branch: &str, spec: &RevertSpec, file: &FileDiff) -> Result<RevertClass, String> {
    if file.untracked {
        return Ok(RevertClass::Uncommitted);
    }
    let base = match &spec.scope {
        CommitScope::All => git::merge_base(git, base_branch)?,
        CommitScope::Uncommitted => return Ok(RevertClass::Uncommitted),
        CommitScope::Commit(_) => return Ok(RevertClass::Committed),
    };
    let paths = file_paths(file);
    let committed = differs(git, &base, Some("HEAD"), &paths)?;
    let uncommitted = differs(git, "HEAD", None, &paths)?;
    Ok(match (committed, uncommitted) {
        (false, _) => RevertClass::Uncommitted,
        (true, false) => RevertClass::Committed,
        (true, true) => RevertClass::Mixed,
    })
}

/// An added or deleted file is one hunk from nothing: reversing it as a hunk
/// would leave an empty file behind, so it's the file's revert.
fn the_hunk<'a>(spec: &RevertSpec, file: &'a FileDiff) -> Result<Option<&'a Hunk>, String> {
    let Some(index) = spec.hunk else { return Ok(None) };
    if matches!(file.status, FileStatus::Added | FileStatus::Deleted) {
        return Ok(None);
    }
    let hunk = file
        .hunks
        .get(index as usize)
        .ok_or_else(|| format!("{} changed since this was read: look again and retry.", spec.path))?;
    Ok(Some(hunk))
}

/// Check a revert and say what it would do.
pub fn plan(git: &dyn GitRunner, kind: TargetKind, base_branch: &str, spec: &RevertSpec) -> Result<RevertPlan, String> {
    refuse_unless_clean(git, kind)?;
    let file = current_file(git, kind, base_branch, spec)?;
    let (class, label) = match the_hunk(spec, &file)? {
        Some(hunk) => {
            let class = match &spec.scope {
                CommitScope::Commit(_) => RevertClass::Committed,
                CommitScope::Uncommitted => RevertClass::Uncommitted,
                CommitScope::All => classify_hunk(git, kind, base_branch, &spec.path, hunk)?,
            };
            (class, format!("the hunk at {}:{}", spec.path, hunk.new_start))
        }
        None => (classify_file(git, base_branch, spec, &file)?, spec.path.clone()),
    };
    Ok(RevertPlan { spec: spec.clone(), class, label })
}

/// A path as `git diff` writes it in a patch header.
fn quoted(path: &str) -> String {
    if !path.chars().any(|c| matches!(c, '"' | '\\' | '\t' | '\n')) {
        return path.to_string();
    }
    let body: String = path
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            '\t' => vec!['\\', 't'],
            '\n' => vec!['\\', 'n'],
            c => vec![c],
        })
        .collect();
    format!("\"{body}\"")
}

/// One hunk as a patch of its own, on its file's new path.
pub(super) fn hunk_patch(path: &str, hunk: &Hunk) -> String {
    let (a, b) = (quoted(&format!("a/{path}")), quoted(&format!("b/{path}")));
    let mut out = format!("diff --git {a} {b}\n--- {a}\n+++ {b}\n");
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
    ));
    for line in &hunk.lines {
        out.push(match line.kind {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Deleted => '-',
        });
        out.push_str(&line.text);
        match line.eol {
            Eol::Lf => out.push('\n'),
            Eol::Crlf => out.push_str("\r\n"),
            Eol::None => out.push_str("\n\\ No newline at end of file\n"),
        }
    }
    out
}

/// Why `git apply` refused, as the status bar says it. It's a conflict when git
/// says so; anything else (a lost connection) is reported as itself.
fn cannot_apply(what: &str, error: String) -> String {
    let conflict = error
        .lines()
        .map(str::trim)
        .find(|l| l.contains("patch failed") || l.contains("does not match") || l.contains("already exists"));
    match conflict {
        Some(why) => format!(
            "Couldn't revert {what}: the worktree has a conflicting edit there ({}).",
            why.trim_start_matches("error: ")
        ),
        None => format!("Couldn't revert {what}: {}", error.trim()),
    }
}

fn apply_reverse(git: &dyn GitRunner, patch: &str, index_too: bool, what: &str) -> Result<(), String> {
    let mut list = vec!["apply", "-R", "--whitespace=nowarn"];
    if index_too {
        list.push("--index");
    }
    list.push("-");
    git.git_with_input(&args(&list), patch).map(|_| ()).map_err(|e| cannot_apply(what, e))
}

/// Put `path` back as `HEAD` has it, in the worktree and the index.
fn restore_to_head(git: &dyn GitRunner, path: &str) -> Result<(), String> {
    let spec = literal(path);
    let at_head = git.git(&args(&["ls-tree", "HEAD", "--", path]), &[0])?;
    if !at_head.trim().is_empty() {
        git.git(&args(&["restore", "--source=HEAD", "--staged", "--worktree", "--", &spec]), &[0])?;
        return Ok(());
    }
    let staged = git.git(&args(&["ls-files", "--", &spec]), &[0])?;
    if staged.trim().is_empty() {
        git.git(&args(&["clean", "-f", "-q", "--", &spec]), &[0])?;
    } else {
        git.git(&args(&["rm", "-f", "-q", "--", &spec]), &[0])?;
    }
    Ok(())
}

/// The file's whole change, reversed into the worktree. The binary form, so a
/// binary file reverses like any other.
fn reverse_file(git: &dyn GitRunner, base_branch: &str, spec: &RevertSpec, file: &FileDiff) -> Result<(), String> {
    let (from, to) = match &spec.scope {
        CommitScope::All => (git::merge_base(git, base_branch)?, None),
        CommitScope::Uncommitted => ("HEAD".to_string(), None),
        CommitScope::Commit(sha) => (git::parent_of(git, sha)?, Some(sha.clone())),
    };
    let paths: Vec<String> = file_paths(file).into_iter().map(literal).collect();
    let mut list = vec![
        "diff",
        "--binary",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        from.as_str(),
    ];
    list.extend(to.as_deref());
    list.push("--");
    list.extend(paths.iter().map(String::as_str));
    let patch = git.git(&args(&list), &[0])?;
    apply_reverse(git, &patch, false, &spec.path)
}

/// Carry out `plan`, checking again that it's still what's there. Answers what
/// the status bar says.
pub fn run(git: &dyn GitRunner, kind: TargetKind, base_branch: &str, plan: &RevertPlan) -> Result<String, String> {
    refuse_unless_clean(git, kind)?;
    let spec = &plan.spec;
    let file = current_file(git, kind, base_branch, spec)?;
    match (the_hunk(spec, &file)?, plan.class) {
        (Some(hunk), RevertClass::Uncommitted) => {
            // The index too when it holds the same change (the agent staged
            // it), so nothing's left staged against the restored file.
            let patch = hunk_patch(&spec.path, hunk);
            if apply_reverse(git, &patch, true, &spec.path).is_err() {
                apply_reverse(git, &patch, false, &spec.path)?;
            }
        }
        (Some(hunk), _) => apply_reverse(git, &hunk_patch(&spec.path, hunk), false, &spec.path)?,
        (None, RevertClass::Uncommitted) => {
            for path in file_paths(&file) {
                restore_to_head(git, path)?;
            }
        }
        (None, _) => reverse_file(git, base_branch, spec, &file)?,
    }
    Ok(plan.done())
}
