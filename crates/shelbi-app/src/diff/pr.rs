//! A task's pull request in the diff view: the description block above the first
//! file, the review threads at their lines, and what goes back to GitHub.
//!
//! The task names its PR (its recorded `pr`, or the status cache's find from its
//! branch: [`shelbi_state::pr_status`]). Everything else comes from one GraphQL
//! read ([`fetch`]), on a worker thread: the view opens without it, and when
//! GitHub can't answer the block says why ([`PrView::why`]).
//!
//! GitHub's threads are shown through the same rows as a task's own comments.
//! Each becomes a [`Comment`] ([`PrThread::as_comment`]) whose id says it's
//! GitHub's ([`GH_ID_PREFIX`]), and is placed the way a comment is: by the text of
//! its line, at or nearest the line GitHub names, so a workspace that has moved
//! since the last push still shows a thread on its code.
//!
//! Writes go through `gh api graphql` ([`write`]). `-f` carries every string,
//! never `-F`: `-F body=@file` would read a file and post it.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{IssuePr, PrState};

use super::comments::{Anchor, Author, Comment, CommentReply, Placement, SideIndex};
use super::git::GitRunner;
use super::model::{Changeset, Side};

/// What marks a [`Comment`] as a GitHub thread's, ahead of the thread's id.
pub const GH_ID_PREFIX: &str = "gh:";

/// How long a body line runs before it wraps. A PR's paragraphs are one long
/// line each, and the rows don't wrap.
const BODY_WIDTH: usize = 100;

/// The failed checks a block names before it says how many more there are.
const FAILED_NAMED: usize = 3;

// ---- The model -------------------------------------------------------------

/// A reviewer's word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    /// The viewer's own, not submitted yet.
    Pending,
}

impl ReviewState {
    fn label(self) -> &'static str {
        match self {
            ReviewState::Approved => "approved",
            ReviewState::ChangesRequested => "requested changes",
            ReviewState::Commented => "commented",
            ReviewState::Dismissed => "dismissed",
            ReviewState::Pending => "pending",
        }
    }

    fn mark(self) -> (&'static str, PrTone) {
        match self {
            ReviewState::Approved => ("✓", PrTone::Good),
            ReviewState::ChangesRequested => ("✗", PrTone::Bad),
            ReviewState::Pending => ("●", PrTone::Pending),
            ReviewState::Commented | ReviewState::Dismissed => ("·", PrTone::Muted),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReview {
    pub author: String,
    pub state: ReviewState,
    pub body: String,
}

/// How a check stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// Passed, or has no say: neutral and skipped checks don't hold a PR up.
    Passed,
    Failed,
    Pending,
}

impl CheckState {
    fn mark(self) -> (&'static str, PrTone) {
        match self {
            CheckState::Passed => ("✓", PrTone::Good),
            CheckState::Failed => ("✗", PrTone::Bad),
            CheckState::Pending => ("●", PrTone::Pending),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrCheck {
    pub name: String,
    pub state: CheckState,
    /// GitHub's word for it: `success`, `timed out`, `in progress`.
    pub detail: String,
}

/// One comment in a GitHub thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrThreadComment {
    pub id: String,
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// In the viewer's review, not submitted yet.
    pub pending: bool,
}

/// A GitHub review thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrThread {
    pub id: String,
    pub path: String,
    pub side: Side,
    /// The line it's on now. `None` once GitHub calls it outdated.
    pub line: Option<u32>,
    /// The line it was written on.
    pub original_line: Option<u32>,
    pub start_line: Option<u32>,
    /// The text of its line, from the comment's diff hunk.
    pub code: Option<String>,
    pub resolved: bool,
    pub outdated: bool,
    /// On the file as a whole, not a line.
    pub file_level: bool,
    /// Never empty: a thread with no comment isn't kept.
    pub comments: Vec<PrThreadComment>,
}

impl PrThread {
    /// The id its [`Comment`] has.
    pub fn comment_id(&self) -> String {
        format!("{GH_ID_PREFIX}{}", self.id)
    }

    /// Unresolved threads open with their replies showing, as the wireframe
    /// has them, and resolved ones closed, as GitHub does.
    pub fn opens_expanded(&self) -> bool {
        !self.resolved
    }

    pub fn is_pending(&self) -> bool {
        self.comments.first().is_some_and(|c| c.pending)
    }

    /// The tag at the end of its head row.
    pub fn tag(&self) -> (&'static str, PrTone) {
        if self.is_pending() {
            ("[pending]", PrTone::Pending)
        } else if self.resolved {
            ("[resolved]", PrTone::Muted)
        } else {
            ("[unresolved]", PrTone::Pending)
        }
    }

    /// This thread as a row-table [`Comment`]. `None` when it has no comments.
    pub fn as_comment(&self) -> Option<Comment> {
        let (head, rest) = self.comments.split_first()?;
        let line = self.line.or(self.original_line).unwrap_or(0);
        Some(Comment {
            id: self.comment_id(),
            anchor: Anchor {
                path: self.path.clone(),
                side: self.side,
                start: self.start_line.unwrap_or(line),
                end: line,
                code: self.code.iter().cloned().collect(),
            },
            author: Author::human(format!("⎇ {} (GitHub)", head.author)),
            body: head.body.clone(),
            created_at: head.created_at,
            replies: rest
                .iter()
                .map(|c| CommentReply {
                    id: c.id.clone(),
                    author: Author::human(c.author.clone()),
                    body: c.body.clone(),
                    created_at: c.created_at,
                })
                .collect(),
            sent: false,
        })
    }
}

/// How a pull request's checks stand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChecksTally {
    pub passed: usize,
    pub failed: usize,
    pub pending: usize,
    pub failed_names: Vec<String>,
}

impl ChecksTally {
    pub fn of(checks: &[PrCheck]) -> ChecksTally {
        let mut tally = ChecksTally::default();
        for check in checks {
            match check.state {
                CheckState::Passed => tally.passed += 1,
                CheckState::Failed => {
                    tally.failed += 1;
                    tally.failed_names.push(check.name.clone());
                }
                CheckState::Pending => tally.pending += 1,
            }
        }
        tally
    }

    /// `(lint, test, +2)`, or nothing when nothing failed.
    fn failed_label(&self) -> Option<String> {
        if self.failed_names.is_empty() {
            return None;
        }
        let mut named: Vec<String> = self.failed_names.iter().take(FAILED_NAMED).cloned().collect();
        if self.failed_names.len() > FAILED_NAMED {
            named.push(format!("+{}", self.failed_names.len() - FAILED_NAMED));
        }
        Some(format!("({})", named.join(", ")))
    }
}

/// Who stands where. A reviewer's latest approval, request for changes or
/// dismissal is the one that counts: a later plain comment doesn't withdraw an
/// approval, and a dismissal leaves them with no verdict.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdicts {
    pub approved: Vec<String>,
    pub changes_requested: Vec<String>,
}

impl Verdicts {
    /// From `reviews`, oldest first.
    pub fn of(reviews: &[PrReview]) -> Verdicts {
        let mut order: Vec<&str> = Vec::new();
        let mut latest: HashMap<&str, ReviewState> = HashMap::new();
        for review in reviews {
            if !matches!(review.state, ReviewState::Approved | ReviewState::ChangesRequested | ReviewState::Dismissed) {
                continue;
            }
            if latest.insert(&review.author, review.state).is_none() {
                order.push(&review.author);
            }
        }
        let who = |state| -> Vec<String> {
            order.iter().filter(|a| latest[**a] == state).map(|a| (*a).to_string()).collect()
        };
        Verdicts { approved: who(ReviewState::Approved), changes_requested: who(ReviewState::ChangesRequested) }
    }
}

/// A pull request, as read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDetails {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub body: String,
    pub author: String,
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub draft: bool,
    /// Oldest first.
    pub reviews: Vec<PrReview>,
    pub checks: Vec<PrCheck>,
    pub threads: Vec<PrThread>,
    /// There are more threads than one read brings back.
    pub threads_cut: bool,
}

impl PrDetails {
    pub fn tally(&self) -> ChecksTally {
        ChecksTally::of(&self.checks)
    }

    pub fn verdicts(&self) -> Verdicts {
        Verdicts::of(&self.reviews)
    }

    pub fn open_threads(&self) -> usize {
        self.threads.iter().filter(|t| !t.resolved).count()
    }

    /// The comments waiting in the viewer's unsubmitted review.
    pub fn pending_comments(&self) -> usize {
        self.threads.iter().flat_map(|t| &t.comments).filter(|c| c.pending).count()
    }
}

// ---- Placing threads --------------------------------------------------------

/// Where each thread sits in `changeset`, by [`PrThread::comment_id`]. One whose
/// file isn't in the changeset has no place: the info panel still counts it.
pub fn place_threads(threads: &[PrThread], changeset: &Changeset) -> HashMap<String, Placement> {
    let mut out = HashMap::with_capacity(threads.len());
    for thread in threads {
        let Some(file) = changeset.file_index(&thread.path) else { continue };
        let found = if thread.file_level {
            None
        } else {
            find_line(thread, &SideIndex::new(&changeset.files[file], thread.side))
        };
        out.insert(
            thread.comment_id(),
            Placement {
                file: file as u32,
                at: found.map(|(h, l, _, _)| (h, l)),
                lines: found.map(|(_, _, s, e)| (s, e)),
            },
        );
    }
    out
}

/// The thread's line by its text, or (with no text to go on) at exactly its
/// number. An unrelated line that happens to be nearby would be worse than the
/// top of the file.
fn find_line(thread: &PrThread, index: &SideIndex<'_>) -> Option<(u32, u32, u32, u32)> {
    let line = thread.line.or(thread.original_line)?;
    match &thread.code {
        Some(code) => index.find(std::slice::from_ref(code), line),
        None => index.find(&[], line).filter(|(_, _, _, at)| *at == line),
    }
}

// ---- What the block and the panel say ---------------------------------------

/// How a piece of text reads, in either client's colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrTone {
    Normal,
    Loud,
    Muted,
    Good,
    Bad,
    Pending,
}

/// A run of text in one tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSpan {
    pub text: String,
    pub tone: PrTone,
}

fn span(text: impl Into<String>, tone: PrTone) -> PrSpan {
    PrSpan { text: text.into(), tone }
}

/// A row of the description block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrPart {
    /// Number, title, author, branches.
    Title,
    /// Verdicts and checks, or why they aren't here.
    Status,
    /// The gap above the description.
    Gap,
    /// A line of the description.
    Body(u16),
}

/// What the diff view knows of the task's pull request.
#[derive(Debug, Clone, Default)]
pub struct PrView {
    /// The task's PR, before any read of it.
    pub record: Option<IssuePr>,
    pub details: Option<Arc<PrDetails>>,
    /// Reading it for the first time.
    pub loading: bool,
    /// Why the last read didn't answer. What was read before stays on show.
    pub why: Option<String>,
    /// The block shows only its first two rows.
    pub folded: bool,
    body: Vec<String>,
}

impl PrView {
    /// Is there anything to say? No PR means no block.
    pub fn shows(&self) -> bool {
        self.record.is_some() || self.details.is_some() || self.why.is_some()
    }

    /// The task's PR as looked up, or why it couldn't be.
    pub fn set_lookup(&mut self, lookup: Result<Option<IssuePr>, String>) {
        match lookup {
            Ok(Some(record)) => {
                self.loading = self.details.is_none();
                self.record = Some(record);
            }
            Ok(None) => {
                *self = PrView { folded: self.folded, ..PrView::default() };
            }
            Err(why) => {
                self.loading = false;
                self.why = Some(why);
            }
        }
    }

    /// A read's answer.
    pub fn set_details(&mut self, details: Result<Arc<PrDetails>, String>) {
        self.loading = false;
        match details {
            Ok(details) => {
                self.body = body_lines(&details.body);
                self.details = Some(details);
                self.why = None;
            }
            Err(why) => self.why = Some(why),
        }
    }

    /// The block's rows.
    pub fn parts(&self) -> Vec<PrPart> {
        if !self.shows() {
            return Vec::new();
        }
        let mut parts = vec![PrPart::Title, PrPart::Status];
        if !self.folded && !self.body.is_empty() {
            parts.push(PrPart::Gap);
            parts.extend((0..self.body.len().min(usize::from(u16::MAX))).map(|i| PrPart::Body(i as u16)));
        }
        parts
    }

    /// What a block row says.
    pub fn spans(&self, part: PrPart) -> Vec<PrSpan> {
        match part {
            PrPart::Title => self.title_spans(),
            PrPart::Status => self.status_spans(),
            PrPart::Gap => Vec::new(),
            PrPart::Body(line) => match self.body.get(usize::from(line)) {
                Some(text) => vec![span(text.clone(), PrTone::Normal)],
                None => Vec::new(),
            },
        }
    }

    fn title_spans(&self) -> Vec<PrSpan> {
        let chevron = if self.folded { "▸ " } else { "▾ " };
        let mut out = vec![span(chevron, PrTone::Muted)];
        match (&self.details, &self.record) {
            (Some(d), _) => {
                out.push(span(format!("#{} {}", d.number, d.title), PrTone::Loud));
                out.push(span(format!("  {} · {} → {}", d.author, d.head, d.base), PrTone::Muted));
                if d.draft {
                    out.push(span(" · draft", PrTone::Muted));
                }
                if d.state != PrState::Open {
                    out.push(span(format!(" · {}", d.state.label()), PrTone::Muted));
                }
            }
            (None, Some(record)) => out.push(span(record.number_label(), PrTone::Loud)),
            (None, None) => out.push(span("Pull request", PrTone::Loud)),
        }
        out
    }

    fn status_spans(&self) -> Vec<PrSpan> {
        let Some(details) = &self.details else {
            return vec![match &self.why {
                Some(why) => span(format!("PR details aren't here: {why}"), PrTone::Pending),
                None => span("Reading the pull request…", PrTone::Muted),
            }];
        };
        let mut groups = verdict_groups(&details.verdicts());
        groups.push(check_group(&details.tally()));
        let open = details.open_threads();
        if open > 0 {
            let noun = if open == 1 { "thread" } else { "threads" };
            groups.push(vec![span(format!("{open} {noun} open"), PrTone::Normal)]);
        }
        let pending = details.pending_comments();
        if pending > 0 {
            let noun = if pending == 1 { "comment" } else { "comments" };
            groups.push(vec![span(format!("{pending} {noun} pending (R submits)"), PrTone::Pending)]);
        }
        if let Some(why) = &self.why {
            groups.push(vec![span(format!("not current: {why}"), PrTone::Pending)]);
        }
        join_groups(groups)
    }

    /// The `i` panel, a row at a time: every check and every review with its
    /// first line, the threads, and the description.
    pub fn info_rows(&self, changeset: &Changeset) -> Vec<Vec<PrSpan>> {
        let mut rows: Vec<Vec<PrSpan>> = Vec::new();
        if let Some(why) = &self.why {
            rows.push(vec![span(format!("Not current: {why}"), PrTone::Pending)]);
            rows.push(Vec::new());
        }
        let Some(d) = &self.details else {
            if self.why.is_none() {
                rows.push(vec![span("Reading the pull request…", PrTone::Muted)]);
            }
            return rows;
        };
        rows.push(vec![span(format!("#{} {}", d.number, d.title), PrTone::Loud)]);
        rows.push(vec![span(format!("{} · {} → {} · {}", d.author, d.head, d.base, d.state.label()), PrTone::Muted)]);
        rows.push(vec![span(d.url.clone(), PrTone::Muted)]);
        rows.push(Vec::new());
        rows.push(heading(format!("Checks ({})", d.checks.len())));
        if d.checks.is_empty() {
            rows.push(vec![span("None reported", PrTone::Muted)]);
        }
        // What needs looking at first.
        let mut checks: Vec<&PrCheck> = d.checks.iter().collect();
        checks.sort_by_key(|c| match c.state {
            CheckState::Failed => 0,
            CheckState::Pending => 1,
            CheckState::Passed => 2,
        });
        for check in checks {
            let (mark, tone) = check.state.mark();
            rows.push(vec![
                span(format!("{mark} "), tone),
                span(check.name.clone(), PrTone::Normal),
                span(format!("  {}", check.detail), PrTone::Muted),
            ]);
        }
        rows.push(Vec::new());
        rows.push(heading(format!("Reviews ({})", d.reviews.len())));
        if d.reviews.is_empty() {
            rows.push(vec![span("None yet", PrTone::Muted)]);
        }
        for review in &d.reviews {
            let (mark, tone) = review.state.mark();
            let mut row = vec![
                span(format!("{mark} "), tone),
                span(review.author.clone(), PrTone::Normal),
                span(format!(" {}", review.state.label()), PrTone::Muted),
            ];
            if let Some(first) = review.body.lines().map(str::trim).find(|l| !l.is_empty()) {
                row.push(span(format!(": {first}"), PrTone::Normal));
            }
            rows.push(row);
        }
        rows.push(Vec::new());
        rows.push(heading(format!("Threads ({})", d.threads.len())));
        rows.push(vec![span(self.threads_summary(d, changeset), PrTone::Muted)]);
        if !self.body.is_empty() {
            rows.push(Vec::new());
            rows.push(heading("Description".to_string()));
            rows.extend(self.body.iter().map(|line| vec![span(line.clone(), PrTone::Normal)]));
        }
        rows
    }

    fn threads_summary(&self, d: &PrDetails, changeset: &Changeset) -> String {
        let open = d.open_threads();
        let mut out = format!("{open} open, {} resolved", d.threads.len() - open);
        let away = d.threads.iter().filter(|t| changeset.file_index(&t.path).is_none()).count();
        if away > 0 {
            out.push_str(&format!(", {away} on files not in this view"));
        }
        if d.threads_cut {
            out.push_str(" (the first 100: GitHub has more)");
        }
        out
    }
}

fn heading(text: String) -> Vec<PrSpan> {
    vec![span(text, PrTone::Loud)]
}

fn verdict_groups(verdicts: &Verdicts) -> Vec<Vec<PrSpan>> {
    let mut groups = Vec::new();
    if !verdicts.approved.is_empty() {
        groups.push(vec![span(format!("✓ approved by {}", verdicts.approved.join(", ")), PrTone::Good)]);
    }
    if !verdicts.changes_requested.is_empty() {
        groups.push(vec![span(format!("✗ changes requested by {}", verdicts.changes_requested.join(", ")), PrTone::Bad)]);
    }
    groups
}

/// `checks ✓12 ✗1 ●2 (lint)`, each count in its color, leaving out the zeros.
fn check_group(tally: &ChecksTally) -> Vec<PrSpan> {
    if tally.passed + tally.failed + tally.pending == 0 {
        return vec![span("no checks", PrTone::Muted)];
    }
    let mut group = vec![span("checks", PrTone::Muted)];
    for (count, mark, tone) in [
        (tally.passed, "✓", PrTone::Good),
        (tally.failed, "✗", PrTone::Bad),
        (tally.pending, "●", PrTone::Pending),
    ] {
        if count > 0 {
            group.push(span(format!(" {mark}{count}"), tone));
        }
    }
    if let Some(failed) = tally.failed_label() {
        group.push(span(format!(" {failed}"), PrTone::Bad));
    }
    group
}

fn join_groups(groups: Vec<Vec<PrSpan>>) -> Vec<PrSpan> {
    let mut out = Vec::new();
    for (i, group) in groups.into_iter().enumerate() {
        if i > 0 {
            out.push(span(" · ", PrTone::Muted));
        }
        out.extend(group);
    }
    out
}

/// A description as rows: its line endings evened out, HTML comments (a PR
/// template's guidance) dropped, long lines wrapped, blank ends trimmed.
fn body_lines(body: &str) -> Vec<String> {
    let body = body.replace("\r\n", "\n");
    let mut rest = body.as_str();
    let mut text = String::with_capacity(body.len());
    while let Some(start) = rest.find("<!--") {
        text.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
            }
        }
    }
    text.push_str(rest);
    let mut lines: Vec<String> = text.lines().flat_map(wrap_line).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let blank = lines.iter().take_while(|l| l.trim().is_empty()).count();
    lines.drain(..blank);
    lines
}

/// `line` at spaces to [`BODY_WIDTH`]. A word longer than that stays whole.
fn wrap_line(line: &str) -> Vec<String> {
    if line.chars().count() <= BODY_WIDTH {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut width = 0;
    for word in line.split(' ') {
        let len = word.chars().count();
        if width > 0 && width + 1 + len > BODY_WIDTH {
            out.push(std::mem::take(&mut current));
            width = 0;
        }
        if width > 0 {
            current.push(' ');
            width += 1;
        }
        current.push_str(word);
        width += len;
    }
    out.push(current);
    out
}

// ---- Reading and writing ----------------------------------------------------

/// `gh` as the diff view reaches it: blocking, with GitHub's limits respected,
/// and failures already in a person's words. A seam, so tests stub `gh`.
pub trait GhApi: Send + Sync {
    fn read(&self, args: &[String]) -> Result<String, String>;
    fn write(&self, args: &[String]) -> Result<String, String>;
}

/// [`GhApi`] on a project's token, through [`shelbi_state::graphql_read`] and
/// its governor.
pub struct ProjectGh {
    pub project: String,
}

impl GhApi for ProjectGh {
    fn read(&self, args: &[String]) -> Result<String, String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        shelbi_state::graphql_read(&self.project, &args).map_err(|e| shelbi_state::gh_failure_reason(&e))
    }

    fn write(&self, args: &[String]) -> Result<String, String> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        shelbi_state::graphql_write(&self.project, &args).map_err(|e| shelbi_state::gh_failure_reason(&e))
    }
}

/// A repository, as GitHub's API names one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// From a PR's URL, or a remote's (`https://`, `ssh://` or `git@host:`).
    pub fn parse(url: &str) -> Option<Repo> {
        let url = url.trim();
        let path = match url.split_once("://") {
            Some((_, rest)) => rest.split_once('/')?.1,
            None => url.split_once(':')?.1,
        };
        let mut parts = path.trim_matches('/').split('/');
        let owner = parts.next().filter(|s| !s.is_empty())?;
        let name = parts.next().filter(|s| !s.is_empty())?;
        Some(Repo { owner: owner.to_string(), name: name.trim_end_matches(".git").to_string() })
    }
}

/// `gh api graphql`, with `query` and these variables. Strings go through `-f`
/// (`-F` would read `@file`), numbers through `-F`.
fn graphql_args(query: &str, strings: &[(&str, &str)], numbers: &[(&str, u64)]) -> Vec<String> {
    let mut args: Vec<String> = vec!["api".into(), "graphql".into(), "-f".into(), format!("query={query}")];
    for (key, value) in strings {
        args.push("-f".into());
        args.push(format!("{key}={value}"));
    }
    for (key, value) in numbers {
        args.push("-F".into());
        args.push(format!("{key}={value}"));
    }
    args
}

const PR_QUERY: &str = "query ShelbiDiffPr($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number title url body state isDraft headRefName baseRefName
      author { login }
      reviews(last: 100) { nodes { author { login } state body } }
      commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
        __typename
        ... on CheckRun { name status conclusion }
        ... on StatusContext { context state }
      } } } } } }
      reviewThreads(first: 100) {
        pageInfo { hasNextPage }
        nodes {
          id isResolved isOutdated path line originalLine startLine diffSide subjectType
          comments(first: 50) { nodes { id author { login } body createdAt state diffHunk } }
        }
      }
    }
  }
}";

const PENDING_QUERY: &str = "query ShelbiDiffPrPending($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      id
      reviews(states: [PENDING], first: 1) { nodes { id viewerDidAuthor } }
    }
  }
}";

/// Read PR `number` of `repo`.
pub fn fetch(gh: &dyn GhApi, repo: &Repo, number: u64) -> Result<PrDetails, String> {
    let args = graphql_args(PR_QUERY, &[("owner", &repo.owner), ("name", &repo.name)], &[("number", number)]);
    parse_details(&gh.read(&args)?)
}

/// What to do on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrWrite {
    /// Reply to a thread.
    Reply { thread: String, body: String },
    /// Resolve or reopen a thread.
    Resolve { thread: String, resolved: bool },
    /// A new thread on these lines, in the viewer's pending review.
    AddThread { anchor: Anchor, body: String },
    /// Submit the viewer's review, with what's pending in it.
    SubmitReview { event: ReviewEvent, body: String },
}

/// How a review is submitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvent {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub fn label(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "Comment",
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
        }
    }

    fn api_name(self) -> &'static str {
        match self {
            ReviewEvent::Comment => "COMMENT",
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        }
    }

    /// The next verdict in the order `Tab` cycles.
    pub fn next(self) -> ReviewEvent {
        match self {
            ReviewEvent::Comment => ReviewEvent::Approve,
            ReviewEvent::Approve => ReviewEvent::RequestChanges,
            ReviewEvent::RequestChanges => ReviewEvent::Comment,
        }
    }

    pub fn prev(self) -> ReviewEvent {
        self.next().next()
    }

    /// GitHub refuses a comment or a request for changes with nothing to say.
    pub fn needs_body(self) -> bool {
        !matches!(self, ReviewEvent::Approve)
    }
}

/// The title of a review's editor: the verdict in brackets among the others.
pub fn review_title(event: ReviewEvent) -> String {
    let choice = |e: ReviewEvent| if e == event { format!("[{}]", e.label()) } else { e.label().to_string() };
    format!(
        "Submit review: {} {} {}",
        choice(ReviewEvent::Comment),
        choice(ReviewEvent::Approve),
        choice(ReviewEvent::RequestChanges)
    )
}

/// Carry out `change` on PR `number` of `repo`, answering what the status bar
/// says. Each is one mutation, except that a comment or a review first asks
/// GitHub for the viewer's pending review: a read made earlier may not know of
/// the one the last comment started.
pub fn write(gh: &dyn GhApi, repo: &Repo, number: u64, change: &PrWrite) -> Result<String, String> {
    match change {
        PrWrite::Reply { thread, body } => {
            let query = "mutation ShelbiDiffPrReply($thread: ID!, $body: String!) {
  addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $thread, body: $body}) { clientMutationId }
}";
            gh.write(&graphql_args(query, &[("thread", thread), ("body", body)], &[]))?;
            Ok("Replied on GitHub".to_string())
        }
        PrWrite::Resolve { thread, resolved } => {
            let (query, done) = if *resolved {
                (
                    "mutation ShelbiDiffPrResolve($thread: ID!) {
  resolveReviewThread(input: {threadId: $thread}) { clientMutationId }
}",
                    "Resolved the thread on GitHub",
                )
            } else {
                (
                    "mutation ShelbiDiffPrUnresolve($thread: ID!) {
  unresolveReviewThread(input: {threadId: $thread}) { clientMutationId }
}",
                    "Reopened the thread on GitHub",
                )
            };
            gh.write(&graphql_args(query, &[("thread", thread)], &[]))?;
            Ok(done.to_string())
        }
        PrWrite::AddThread { anchor, body } => {
            let (pull, pending) = pending_review(gh, repo, number)?;
            let review = match pending {
                Some(review) => review,
                None => start_review(gh, &pull)?,
            };
            add_thread(gh, &review, anchor, body)?;
            Ok("Added to your pending review on GitHub (R submits it)".to_string())
        }
        PrWrite::SubmitReview { event, body } => {
            let (pull, pending) = pending_review(gh, repo, number)?;
            match pending {
                Some(review) => {
                    let query = "mutation ShelbiDiffPrSubmit($review: ID!, $event: PullRequestReviewEvent!, $body: String!) {
  submitPullRequestReview(input: {pullRequestReviewId: $review, event: $event, body: $body}) { clientMutationId }
}";
                    let vars = [("review", review.as_str()), ("event", event.api_name()), ("body", body.as_str())];
                    gh.write(&graphql_args(query, &vars, &[]))?;
                }
                None => {
                    let query = "mutation ShelbiDiffPrReview($pull: ID!, $event: PullRequestReviewEvent!, $body: String!) {
  addPullRequestReview(input: {pullRequestId: $pull, event: $event, body: $body}) { clientMutationId }
}";
                    let vars = [("pull", pull.as_str()), ("event", event.api_name()), ("body", body.as_str())];
                    gh.write(&graphql_args(query, &vars, &[]))?;
                }
            }
            Ok(format!("Review submitted: {}", event.label()))
        }
    }
}

/// The pull request's id, and the viewer's pending review if there is one.
fn pending_review(gh: &dyn GhApi, repo: &Repo, number: u64) -> Result<(String, Option<String>), String> {
    let args = graphql_args(PENDING_QUERY, &[("owner", &repo.owner), ("name", &repo.name)], &[("number", number)]);
    let raw: RawEnvelope<RawPendingPull> = parse_json(&gh.read(&args)?)?;
    let pull = raw.data.repository.pull_request;
    let review = pull.reviews.nodes.into_iter().find(|r| r.viewer_did_author).map(|r| r.id);
    Ok((pull.id, review))
}

/// A new pending review: no event is what makes it pending.
fn start_review(gh: &dyn GhApi, pull: &str) -> Result<String, String> {
    let query = "mutation ShelbiDiffPrStart($pull: ID!) {
  addPullRequestReview(input: {pullRequestId: $pull}) { pullRequestReview { id } }
}";
    let out = gh.write(&graphql_args(query, &[("pull", pull)], &[]))?;
    let raw: RawStarted = parse_json(&out)?;
    Ok(raw.data.add_pull_request_review.pull_request_review.id)
}

fn add_thread(gh: &dyn GhApi, review: &str, anchor: &Anchor, body: &str) -> Result<(), String> {
    let side = match anchor.side {
        Side::Old => "LEFT",
        Side::New => "RIGHT",
    };
    let mut strings = vec![("review", review), ("path", anchor.path.as_str()), ("side", side), ("body", body)];
    let mut numbers = vec![("line", u64::from(anchor.end))];
    if anchor.start != anchor.end {
        strings.push(("startSide", side));
        numbers.push(("startLine", u64::from(anchor.start)));
    }
    let query = "mutation ShelbiDiffPrThread($review: ID!, $path: String!, $side: DiffSide!, $line: Int!, $startLine: Int, $startSide: DiffSide, $body: String!) {
  addPullRequestReviewThread(input: {pullRequestReviewId: $review, path: $path, side: $side, line: $line, startLine: $startLine, startSide: $startSide, body: $body}) { clientMutationId }
}";
    gh.write(&graphql_args(query, &strings, &numbers))?;
    Ok(())
}

// ---- GitHub's JSON ----------------------------------------------------------

fn parse_json<T: for<'de> Deserialize<'de>>(raw: &str) -> Result<T, String> {
    serde_json::from_str(raw).map_err(|e| format!("GitHub's answer didn't read: {e}"))
}

#[derive(Deserialize)]
struct RawEnvelope<P> {
    data: RawData<P>,
}

#[derive(Deserialize)]
struct RawData<P> {
    repository: RawRepository<P>,
}

#[derive(Deserialize)]
struct RawRepository<P> {
    #[serde(rename = "pullRequest")]
    pull_request: P,
}

#[derive(Deserialize)]
struct RawNodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct RawLogin {
    login: String,
}

/// A deleted account has no author: GitHub shows these as `ghost`.
fn login(author: Option<RawLogin>) -> String {
    author.map_or_else(|| "ghost".to_string(), |a| a.login)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPull {
    number: u64,
    title: String,
    url: String,
    #[serde(default)]
    body: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    head_ref_name: String,
    base_ref_name: String,
    author: Option<RawLogin>,
    reviews: RawNodes<RawReview>,
    commits: RawNodes<RawCommitNode>,
    review_threads: RawThreads,
}

#[derive(Deserialize)]
struct RawReview {
    author: Option<RawLogin>,
    state: String,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
struct RawCommitNode {
    commit: RawCommit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommit {
    status_check_rollup: Option<RawRollup>,
}

#[derive(Deserialize)]
struct RawRollup {
    contexts: RawNodes<RawContext>,
}

#[derive(Deserialize)]
struct RawContext {
    #[serde(rename = "__typename")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreads {
    page_info: RawPageInfo,
    nodes: Vec<RawThread>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPageInfo {
    has_next_page: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
    path: String,
    line: Option<u32>,
    original_line: Option<u32>,
    start_line: Option<u32>,
    diff_side: String,
    subject_type: Option<String>,
    comments: RawNodes<RawThreadComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreadComment {
    id: String,
    author: Option<RawLogin>,
    #[serde(default)]
    body: String,
    created_at: DateTime<Utc>,
    state: Option<String>,
    diff_hunk: Option<String>,
}

#[derive(Deserialize)]
struct RawPendingPull {
    id: String,
    reviews: RawNodes<RawPendingReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPendingReview {
    id: String,
    viewer_did_author: bool,
}

#[derive(Deserialize)]
struct RawStarted {
    data: RawStartedData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStartedData {
    add_pull_request_review: RawStartedReview,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStartedReview {
    pull_request_review: RawId,
}

#[derive(Deserialize)]
struct RawId {
    id: String,
}

/// A `gh api graphql` answer to [`PR_QUERY`].
pub fn parse_details(raw: &str) -> Result<PrDetails, String> {
    let raw: RawEnvelope<RawPull> = parse_json(raw)?;
    let pull = raw.data.repository.pull_request;
    let state = match pull.state.as_str() {
        "MERGED" => PrState::Merged,
        "CLOSED" => PrState::Closed,
        _ => PrState::Open,
    };
    let contexts = pull
        .commits
        .nodes
        .into_iter()
        .next()
        .and_then(|node| node.commit.status_check_rollup)
        .map_or_else(Vec::new, |rollup| rollup.contexts.nodes);
    Ok(PrDetails {
        number: pull.number,
        title: pull.title,
        url: pull.url,
        body: pull.body,
        author: login(pull.author),
        head: pull.head_ref_name,
        base: pull.base_ref_name,
        state,
        draft: pull.is_draft,
        reviews: pull.reviews.nodes.into_iter().filter_map(read_review).collect(),
        checks: contexts.into_iter().map(read_check).collect(),
        threads_cut: pull.review_threads.page_info.has_next_page,
        threads: pull.review_threads.nodes.into_iter().filter_map(read_thread).collect(),
    })
}

fn read_review(raw: RawReview) -> Option<PrReview> {
    let state = match raw.state.as_str() {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        "DISMISSED" => ReviewState::Dismissed,
        "PENDING" => ReviewState::Pending,
        _ => return None,
    };
    Some(PrReview { author: login(raw.author), state, body: raw.body })
}

/// A check run and a commit status report differently: read both as one.
fn read_check(raw: RawContext) -> PrCheck {
    let word = |s: &str| s.to_ascii_lowercase().replace('_', " ");
    if raw.kind == "StatusContext" {
        let state = raw.state.unwrap_or_default();
        return PrCheck {
            name: raw.context.unwrap_or_default(),
            state: match state.as_str() {
                "SUCCESS" => CheckState::Passed,
                "PENDING" | "EXPECTED" => CheckState::Pending,
                _ => CheckState::Failed,
            },
            detail: word(&state),
        };
    }
    let name = raw.name.unwrap_or_default();
    match raw.conclusion {
        Some(conclusion) => PrCheck {
            name,
            state: match conclusion.as_str() {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => CheckState::Passed,
                _ => CheckState::Failed,
            },
            detail: word(&conclusion),
        },
        None => PrCheck {
            name,
            state: CheckState::Pending,
            detail: word(raw.status.as_deref().unwrap_or("pending")),
        },
    }
}

fn read_thread(raw: RawThread) -> Option<PrThread> {
    let code = raw.comments.nodes.first().and_then(|c| c.diff_hunk.as_deref()).and_then(last_hunk_line);
    let comments: Vec<PrThreadComment> = raw
        .comments
        .nodes
        .into_iter()
        .map(|c| PrThreadComment {
            id: c.id,
            author: login(c.author),
            body: c.body,
            created_at: c.created_at,
            pending: c.state.as_deref() == Some("PENDING"),
        })
        .collect();
    if comments.is_empty() {
        return None;
    }
    Some(PrThread {
        id: raw.id,
        path: raw.path,
        side: if raw.diff_side == "LEFT" { Side::Old } else { Side::New },
        line: raw.line,
        original_line: raw.original_line,
        start_line: raw.start_line,
        code,
        resolved: raw.is_resolved,
        outdated: raw.is_outdated,
        file_level: raw.subject_type.as_deref() == Some("FILE"),
        comments,
    })
}

/// A comment's diff hunk ends on the line it's about: its text, without the
/// `+`, `-` or space that opens it.
fn last_hunk_line(hunk: &str) -> Option<String> {
    let last = hunk.lines().next_back()?;
    if last.starts_with("@@") {
        return None;
    }
    let text = last.strip_prefix(['+', '-', ' ']).unwrap_or(last);
    Some(text.trim_end_matches('\r').to_string())
}

// ---- Where a target's pull request comes from -------------------------------

/// A target's pull request, for a backend to answer with. Blocking.
pub trait PrSource: Send + Sync {
    /// The task's PR, by its record or its branch. `None`: it hasn't one.
    fn lookup(&self) -> Result<Option<IssuePr>, String>;
    fn details(&self, pr: &IssuePr) -> Result<PrDetails, String>;
    fn write(&self, pr: &IssuePr, change: &PrWrite) -> Result<String, String>;
}

/// A target with no pull request: a fixture's, or one with no task.
pub struct NoPr;

impl PrSource for NoPr {
    fn lookup(&self) -> Result<Option<IssuePr>, String> {
        Ok(None)
    }

    fn details(&self, _pr: &IssuePr) -> Result<PrDetails, String> {
        Err("There's no pull request".to_string())
    }

    fn write(&self, _pr: &IssuePr, _change: &PrWrite) -> Result<String, String> {
        Err("There's no pull request".to_string())
    }
}

/// A task's pull request on GitHub, reached with `gh`.
pub struct TaskPr {
    pub project: String,
    pub task: Option<String>,
    pub gh: Arc<dyn GhApi>,
    /// Finds the repository when the PR has no URL yet (`pr: 2137` by hand).
    pub git: Arc<dyn GitRunner>,
}

impl TaskPr {
    fn repo(&self, pr: &IssuePr) -> Result<Repo, String> {
        if let Some(repo) = pr.url.as_deref().and_then(Repo::parse) {
            return Ok(repo);
        }
        let origin = self.git.git(&["remote".to_string(), "get-url".to_string(), "origin".to_string()], &[0])?;
        Repo::parse(&origin).ok_or_else(|| format!("Can't tell the GitHub repository from {}", origin.trim()))
    }
}

impl PrSource for TaskPr {
    fn lookup(&self) -> Result<Option<IssuePr>, String> {
        let Some(task) = &self.task else { return Ok(None) };
        let project = shelbi_state::load_project(&self.project).map_err(|e| e.to_string())?;
        let store = shelbi_state::resolve_issue_store(&self.project, &project.issue_tracker)
            .map_err(|e| shelbi_state::gh_failure_reason(&e))?;
        let found = store.get(task).map_err(|e| shelbi_state::gh_failure_reason(&e))?;
        Ok(found.and_then(|file| shelbi_state::pr_status::load(&self.project).resolve(&file.task)))
    }

    fn details(&self, pr: &IssuePr) -> Result<PrDetails, String> {
        fetch(&*self.gh, &self.repo(pr)?, pr.number)
    }

    fn write(&self, pr: &IssuePr, change: &PrWrite) -> Result<String, String> {
        write(&*self.gh, &self.repo(pr)?, pr.number, change)
    }
}

#[cfg(test)]
#[path = "pr_tests.rs"]
mod tests;
