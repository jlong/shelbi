//! The palette's search beyond the commands: issues and Changelog entries.
//!
//! Typing in the palette searches three kinds of thing at once: the commands
//! (the registry's entries, fuzzy-matched by [`shelbi_palette::search`] as
//! they always were), the current project's issues (title, id and body), and
//! its Changelog entries (the Changelog page's own search,
//! [`ChangelogEntryView::matches`]). [`result_rows`] lays the matches out for
//! a [`SearchFilter`]: under [`SearchFilter::All`], one group per kind with a
//! header (commands, then issues, then entries), best matches first, each capped at [`GROUP_CAP`] with a "Show all"
//! row that switches to that kind's filter; under a kind's filter, all of
//! that kind.
//!
//! The issues and entries come from what a client already holds (its board,
//! its Changelog read) or, for a client that hasn't read them yet, from
//! [`read_corpus`], which reads the daemon's board index and the changelog
//! directory: never the GitHub API. The search itself runs on a
//! [`SearchWorker`] thread, debounced, so a keystroke never waits on it; the
//! commands stay synchronous, as they always were.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelbi_core::{ProjectStatuses, StatusCategory};
use shelbi_palette::{Entry, EntryKind, Scorer};
use shelbi_state::IssueFile;

use crate::board::{BoardInput, BoardRules};
use crate::changelog::{ChangelogEntryView, LinkKind};

/// How many of each kind [`SearchFilter::All`] shows before its "Show all"
/// row.
pub const GROUP_CAP: usize = 3;

/// How long the search worker waits for the typing to pause before it
/// searches.
pub const DEBOUNCE: Duration = Duration::from_millis(90);

/// What the palette's search shows: everything, or one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchFilter {
    #[default]
    All,
    Issues,
    Changelog,
    Commands,
}

impl SearchFilter {
    /// The filter row, left to right.
    pub const ROW: [SearchFilter; 4] = [
        SearchFilter::All,
        SearchFilter::Issues,
        SearchFilter::Changelog,
        SearchFilter::Commands,
    ];

    /// The chip's label.
    pub fn label(self) -> &'static str {
        match self {
            SearchFilter::All => "All",
            SearchFilter::Issues => "Issues",
            SearchFilter::Changelog => "Changelog Entries",
            SearchFilter::Commands => "Commands",
        }
    }

    /// The next chip to the right, wrapping (Tab, →).
    pub fn next(self) -> Self {
        let ix = Self::ROW.iter().position(|f| *f == self).unwrap_or(0);
        Self::ROW[(ix + 1) % Self::ROW.len()]
    }

    /// The next chip to the left, wrapping (Shift-Tab, ←).
    pub fn prev(self) -> Self {
        let ix = Self::ROW.iter().position(|f| *f == self).unwrap_or(0);
        Self::ROW[(ix + Self::ROW.len() - 1) % Self::ROW.len()]
    }

    /// The one kind this filter shows, or `None` for All.
    pub fn kind(self) -> Option<ResultKind> {
        match self {
            SearchFilter::All => None,
            SearchFilter::Issues => Some(ResultKind::Issue),
            SearchFilter::Changelog => Some(ResultKind::Changelog),
            SearchFilter::Commands => Some(ResultKind::Command),
        }
    }

    /// What the results band says when nothing matches under this filter.
    pub fn empty_label(self) -> &'static str {
        match self {
            SearchFilter::All => "No matches",
            SearchFilter::Issues => "No matching issues",
            SearchFilter::Changelog => "No matching Changelog entries",
            SearchFilter::Commands => "No matching commands",
        }
    }
}

/// A kind of search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Issue,
    Changelog,
    Command,
}

impl ResultKind {
    /// The groups under All, top to bottom. Commands lead, so typing a
    /// command's name and Enter still runs it; a group with no matches is
    /// left out, so when no command matches the first issue is on top.
    pub const ORDER: [ResultKind; 3] = [ResultKind::Command, ResultKind::Issue, ResultKind::Changelog];

    /// The group's header under All.
    pub fn heading(self) -> &'static str {
        match self {
            ResultKind::Issue => "Issues",
            ResultKind::Changelog => "Changelog Entries",
            ResultKind::Command => "Commands",
        }
    }

    /// The filter that shows only this kind.
    pub fn filter(self) -> SearchFilter {
        match self {
            ResultKind::Issue => SearchFilter::Issues,
            ResultKind::Changelog => SearchFilter::Changelog,
            ResultKind::Command => SearchFilter::Commands,
        }
    }

    /// The label of the row that ends a capped group: `Show all 12 issues`.
    pub fn show_all_label(self, count: usize) -> String {
        let noun = match self {
            ResultKind::Issue => "issues",
            ResultKind::Changelog => "Changelog entries",
            ResultKind::Command => "commands",
        };
        format!("Show all {count} {noun}")
    }
}

// ---------------------------------------------------------------------------
// What is searched
// ---------------------------------------------------------------------------

/// One issue as the palette searches and draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDoc {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Its status's name, as the board's column has it (`In Progress`).
    pub status: String,
    pub category: StatusCategory,
    /// `body` lowercased once, for the word search.
    body_lower: String,
}

impl IssueDoc {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        body: impl Into<String>,
        status: impl Into<String>,
        category: StatusCategory,
    ) -> Self {
        let body = body.into();
        IssueDoc {
            id: id.into(),
            title: title.into(),
            body_lower: body.to_lowercase(),
            body,
            status: status.into(),
            category,
        }
    }

    /// How the id reads beside the title: `#1931` for a tracker's number,
    /// the id itself otherwise.
    pub fn reference(&self) -> String {
        if !self.id.is_empty() && self.id.bytes().all(|b| b.is_ascii_digit()) {
            format!("#{}", self.id)
        } else {
            self.id.clone()
        }
    }

    /// The row's description: `in progress · #1931`.
    pub fn detail(&self) -> String {
        format!("{} · {}", self.status.to_lowercase(), self.reference())
    }
}

/// The board's issues as the palette searches them, in board order (open
/// first, then history), each with its resolved status's name and category.
pub fn issue_docs(tasks: &[IssueFile], statuses: &ProjectStatuses, rules: BoardRules<'_>) -> Vec<IssueDoc> {
    tasks
        .iter()
        .map(|tf| {
            let status_id = rules.resolved_status_id(&tf.task);
            let (name, category) = match statuses.statuses.iter().find(|s| s.id == status_id) {
                Some(s) => (s.name.clone(), s.category),
                None => (status_id.replace('-', " "), tf.task.column.category()),
            };
            IssueDoc::new(&tf.task.id, &tf.task.title, &tf.body, name, category)
        })
        .collect()
}

/// Everything the palette's search looks through besides the commands.
/// Cheap to clone (shared), so each query hands the worker the same data.
#[derive(Debug, Clone, Default)]
pub struct SearchCorpus {
    pub issues: Arc<Vec<IssueDoc>>,
    pub changelog: Arc<Vec<ChangelogEntryView>>,
}

impl SearchCorpus {
    pub fn new(issues: Vec<IssueDoc>, changelog: Vec<ChangelogEntryView>) -> Self {
        SearchCorpus {
            issues: Arc::new(issues),
            changelog: Arc::new(changelog),
        }
    }

    /// From a board a client holds and its Changelog entries.
    pub fn from_board(board: &BoardInput, changelog: Vec<ChangelogEntryView>) -> Self {
        let issues = issue_docs(&board.tasks(), &board.project_statuses, board.rules());
        SearchCorpus::new(issues, changelog)
    }

    pub fn is_empty(&self) -> bool {
        self.issues.is_empty() && self.changelog.is_empty()
    }
}

/// Read `project`'s corpus for a client that doesn't hold a board or a
/// Changelog read yet: the open board from the daemon's board index (the
/// same read the board makes, so no GitHub call) and the changelog
/// directory. Touches disk; run it off the UI thread.
pub fn read_corpus(project: &str) -> SearchCorpus {
    let mut board = BoardInput::default();
    board.apply_open(crate::board::read_open_board(project));
    let changelog = crate::changelog::read_changelog(project).entries;
    SearchCorpus::from_board(&board, changelog)
}

// ---------------------------------------------------------------------------
// Ranking
// ---------------------------------------------------------------------------

/// A Changelog entry as a result row draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogHit {
    pub id: String,
    pub title: String,
    /// The row's description: `Oct 28 · #1902` (the date, then the entry's
    /// first PR, or its first issue when it has no PR).
    pub detail: String,
}

impl ChangelogHit {
    fn new(e: &ChangelogEntryView) -> Self {
        let link = e
            .links
            .iter()
            .find(|l| l.kind == LinkKind::Pr)
            .or_else(|| e.links.iter().find(|l| l.kind == LinkKind::Issue));
        let date = e.date.format("%b %-d").to_string();
        ChangelogHit {
            id: e.id.clone(),
            title: e.title.clone(),
            detail: match link {
                Some(l) => format!("{date} · {}", l.label),
                None => date,
            },
        }
    }
}

/// One query's issue and Changelog matches, best first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchHits {
    /// The query they match.
    pub query: String,
    pub issues: Vec<IssueDoc>,
    pub changelog: Vec<ChangelogHit>,
}

/// Search `corpus` for `query`. A blank query matches nothing (the palette
/// shows only its commands then).
pub fn search_corpus(corpus: &SearchCorpus, query: &str) -> SearchHits {
    if query.trim().is_empty() {
        return SearchHits {
            query: query.to_string(),
            ..SearchHits::default()
        };
    }
    SearchHits {
        query: query.to_string(),
        issues: rank_issues(&corpus.issues, query),
        changelog: rank_changelog(&corpus.changelog, query),
    }
}

/// The issues matching `query`, best first: a fuzzy title match (the
/// palette's matcher) ranks above a fuzzy id match, which ranks above a
/// body that holds every word; ties keep board order.
pub fn rank_issues(docs: &[IssueDoc], query: &str) -> Vec<IssueDoc> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut scorer = Scorer::new(query);
    let mut ranked: Vec<((u8, u16), usize)> = docs
        .iter()
        .enumerate()
        .filter_map(|(ix, d)| {
            let rank = if let Some(s) = scorer.score(&d.title) {
                (2, s)
            } else if let Some(s) = scorer.score(&d.id) {
                (1, s)
            } else if words.iter().all(|w| d.body_lower.contains(w.as_str())) {
                (0, 0)
            } else {
                return None;
            };
            Some((rank, ix))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, ix)| docs[ix].clone()).collect()
}

/// The Changelog entries matching `query` (the Changelog page's search), best
/// first: those whose title fuzzy-matches lead, by score; ties stay newest
/// first.
pub fn rank_changelog(entries: &[ChangelogEntryView], query: &str) -> Vec<ChangelogHit> {
    let mut scorer = Scorer::new(query);
    let mut ranked: Vec<(u16, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.matches(query))
        .map(|(ix, e)| (scorer.score(&e.title).map_or(0, |s| s.saturating_add(1)), ix))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ranked
        .into_iter()
        .map(|(_, ix)| ChangelogHit::new(&entries[ix]))
        .collect()
}

// ---------------------------------------------------------------------------
// The rows a client draws
// ---------------------------------------------------------------------------

/// One selectable result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultItem {
    Command(Entry),
    Issue(IssueDoc),
    Changelog(ChangelogHit),
    /// The end of a capped group under All: switches to `kind`'s filter.
    ShowAll { kind: ResultKind, count: usize },
}

impl ResultItem {
    /// The palette entry activating this item runs, or `None` for a "Show
    /// all" row (which only switches the filter).
    pub fn entry(&self) -> Option<Entry> {
        let (id, label) = match self {
            ResultItem::Command(e) => return Some(e.clone()),
            ResultItem::Issue(d) => (format!("action:open-issue:{}", d.id), d.title.clone()),
            ResultItem::Changelog(h) => (format!("action:open-changelog-entry:{}", h.id), h.title.clone()),
            ResultItem::ShowAll { .. } => return None,
        };
        Some(Entry {
            id,
            label,
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        })
    }
}

/// One row of the results band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultRow {
    /// A group's header under All (not selectable).
    Header(ResultKind),
    Item(ResultItem),
}

impl ResultRow {
    pub fn item(&self) -> Option<&ResultItem> {
        match self {
            ResultRow::Item(i) => Some(i),
            ResultRow::Header(_) => None,
        }
    }
}

/// How many results `filter`'s chip counts.
pub fn match_count(filter: SearchFilter, commands: usize, hits: &SearchHits) -> usize {
    let of = |kind| match kind {
        ResultKind::Issue => hits.issues.len(),
        ResultKind::Changelog => hits.changelog.len(),
        ResultKind::Command => commands,
    };
    match filter.kind() {
        Some(kind) => of(kind),
        None => ResultKind::ORDER.iter().map(|k| of(*k)).sum(),
    }
}

/// The results band's rows. Not searching: the commands, as the palette has
/// always listed them. Searching under All: a header per kind that matched,
/// its best [`GROUP_CAP`], and a "Show all" row when it has more (a kind
/// that matched alone shows all of it: there is nothing to make room for);
/// under a kind's filter: every match of that kind.
pub fn result_rows(
    commands: &[(Entry, u16)],
    hits: &SearchHits,
    filter: SearchFilter,
    searching: bool,
) -> Vec<ResultRow> {
    let items = |kind: ResultKind| -> Vec<ResultItem> {
        match kind {
            ResultKind::Issue => hits.issues.iter().cloned().map(ResultItem::Issue).collect(),
            ResultKind::Changelog => hits.changelog.iter().cloned().map(ResultItem::Changelog).collect(),
            ResultKind::Command => commands.iter().map(|(e, _)| ResultItem::Command(e.clone())).collect(),
        }
    };
    if !searching {
        return items(ResultKind::Command).into_iter().map(ResultRow::Item).collect();
    }
    if let Some(kind) = filter.kind() {
        return items(kind).into_iter().map(ResultRow::Item).collect();
    }
    let groups: Vec<(ResultKind, Vec<ResultItem>)> = ResultKind::ORDER
        .iter()
        .map(|&kind| (kind, items(kind)))
        .filter(|(_, all)| !all.is_empty())
        .collect();
    let cap = if groups.len() > 1 { GROUP_CAP } else { usize::MAX };
    let mut rows = Vec::new();
    for (kind, all) in groups {
        let count = all.len();
        rows.push(ResultRow::Header(kind));
        rows.extend(all.into_iter().take(cap).map(ResultRow::Item));
        if count > cap {
            rows.push(ResultRow::Item(ResultItem::ShowAll { kind, count }));
        }
    }
    rows
}

/// One chip of the filter row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterChip {
    pub filter: SearchFilter,
    pub label: &'static str,
    pub count: usize,
    pub selected: bool,
}

// ---------------------------------------------------------------------------
// Running the search
// ---------------------------------------------------------------------------

/// Called on the worker thread after a search lands, so a client that sleeps
/// between frames can wake and draw it.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

struct Job {
    query: String,
    corpus: SearchCorpus,
}

/// A thread that runs the palette's searches: it waits [`DEBOUNCE`] for the
/// typing to pause (a newer query replaces a waiting one), searches, and
/// publishes the hits. Dropping it lets the thread finish what it's on and
/// exit; nothing waits for it.
pub struct SearchWorker {
    jobs: Sender<Job>,
    /// Behind a lock so a palette holding the worker can cross threads.
    hits: Mutex<Receiver<SearchHits>>,
}

impl std::fmt::Debug for SearchWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SearchWorker")
    }
}

impl SearchWorker {
    pub fn spawn(debounce: Duration, wake: Option<Wake>) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (hits_tx, hits) = mpsc::channel::<SearchHits>();
        let spawned = std::thread::Builder::new()
            .name("shelbi-palette-search".into())
            .spawn(move || {
                while let Ok(mut job) = job_rx.recv() {
                    loop {
                        match job_rx.recv_timeout(debounce) {
                            Ok(newer) => job = newer,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    if hits_tx.send(search_corpus(&job.corpus, &job.query)).is_err() {
                        return;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("palette search worker didn't start: {e}");
        }
        SearchWorker {
            jobs,
            hits: Mutex::new(hits),
        }
    }

    /// Search `corpus` for `query` once the typing pauses.
    pub fn submit(&self, query: &str, corpus: &SearchCorpus) {
        let _ = self.jobs.send(Job {
            query: query.to_string(),
            corpus: corpus.clone(),
        });
    }

    /// The newest hits that landed since the last call, if any.
    pub fn latest(&self) -> Option<SearchHits> {
        let hits = self.hits.lock().ok()?;
        let mut newest = None;
        while let Ok(h) = hits.try_recv() {
            newest = Some(h);
        }
        newest
    }

    /// Wait up to `timeout` for the next hits (tests).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<SearchHits> {
        self.hits.lock().ok()?.recv_timeout(timeout).ok()
    }
}

/// The palette's side of the search: the corpus, where searches run, and the
/// last hits that landed.
#[derive(Debug, Clone, Default)]
pub struct PaletteSearch {
    corpus: SearchCorpus,
    /// `None` searches inline, on the caller's thread (tests, and a palette
    /// with nothing but commands to search).
    worker: Option<Arc<SearchWorker>>,
    hits: SearchHits,
    /// The query last asked about.
    asked: String,
}

impl PaletteSearch {
    /// Search `corpus` on the caller's thread, as each query is typed.
    pub fn inline(corpus: SearchCorpus) -> Self {
        PaletteSearch {
            corpus,
            ..PaletteSearch::default()
        }
    }

    /// Search `corpus` on a [`SearchWorker`], calling `wake` when hits land.
    pub fn background(corpus: SearchCorpus, wake: Option<Wake>) -> Self {
        PaletteSearch {
            corpus,
            worker: Some(Arc::new(SearchWorker::spawn(DEBOUNCE, wake))),
            ..PaletteSearch::default()
        }
    }

    pub fn corpus(&self) -> &SearchCorpus {
        &self.corpus
    }

    /// Search fresh data (a board or Changelog read landed): the current
    /// query again.
    pub fn set_corpus(&mut self, corpus: SearchCorpus) {
        self.corpus = corpus;
        let query = std::mem::take(&mut self.asked);
        self.ask(&query);
    }

    /// Search for `query`. Blank clears the hits at once.
    pub fn ask(&mut self, query: &str) {
        self.asked = query.to_string();
        if query.trim().is_empty() {
            self.hits = SearchHits {
                query: query.to_string(),
                ..SearchHits::default()
            };
            return;
        }
        match &self.worker {
            Some(worker) => worker.submit(query, &self.corpus),
            None => self.hits = search_corpus(&self.corpus, query),
        }
    }

    /// Take hits that landed. Returns whether the results changed.
    pub fn poll(&mut self) -> bool {
        let Some(hits) = self.worker.as_ref().and_then(|w| w.latest()) else {
            return false;
        };
        // A blank query cleared the hits already; a search still in flight
        // for an older query doesn't bring them back.
        if self.asked.trim().is_empty() {
            return false;
        }
        let changed = hits != self.hits;
        self.hits = hits;
        changed
    }

    /// The last hits that landed (possibly for an earlier query, while the
    /// current one is searched).
    pub fn hits(&self) -> &SearchHits {
        &self.hits
    }

    /// Whether the current query's search hasn't landed yet.
    pub fn pending(&self) -> bool {
        self.hits.query != self.asked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use shelbi_state::changelog::{ChangelogEntry, PrRef};

    pub(crate) fn doc(id: &str, title: &str, body: &str) -> IssueDoc {
        IssueDoc::new(id, title, body, "In Progress", StatusCategory::Active)
    }

    fn entry(id: &str, title: &str, body: &str, prs: Vec<PrRef>) -> ChangelogEntryView {
        ChangelogEntryView::new(
            ChangelogEntry {
                id: id.into(),
                title: title.into(),
                date: DateTime::parse_from_rfc3339("2026-10-28T14:30:00-07:00").unwrap(),
                author: "orchestrator".into(),
                issues: vec![],
                prs,
                urls: vec![],
                body: body.into(),
                path: Default::default(),
            },
            Some("a/b"),
        )
    }

    fn command(id: &str, label: &str) -> (Entry, u16) {
        (
            Entry {
                id: id.into(),
                label: label.into(),
                kind: EntryKind::Action,
                subtitle: None,
                shortcut: None,
                decoration: None,
                hidden_until_query: false,
            },
            0,
        )
    }

    fn ids(issues: &[IssueDoc]) -> Vec<&str> {
        issues.iter().map(|d| d.id.as_str()).collect()
    }

    #[test]
    fn issues_rank_title_then_id_then_body_and_keep_board_order_on_ties() {
        let docs = vec![
            doc("1902", "Unrelated", "the login form is broken"),
            doc("login-copy", "Copy tweaks", ""),
            doc("1931", "Fix login redirect loop", ""),
            doc("1940", "Login form validation", ""),
            doc("1999", "Nothing here", "nope"),
        ];
        let got = rank_issues(&docs, "login");
        assert_eq!(&ids(&got)[2..], ["login-copy", "1902"], "id, then body");
        let mut titles = ids(&got)[..2].to_vec();
        titles.sort();
        assert_eq!(titles, ["1931", "1940"], "title matches lead");
        // Every word must be in the body for a body-only match.
        assert!(rank_issues(&docs, "login zebra").is_empty());
        assert_eq!(ids(&rank_issues(&docs, "form broken")), ["1902"]);
    }

    #[test]
    fn an_issue_row_reads_its_status_and_reference() {
        assert_eq!(doc("1931", "t", "").detail(), "in progress · #1931");
        let d = IssueDoc::new("tui-about", "t", "", "Backlog", StatusCategory::Backlog);
        assert_eq!(d.detail(), "backlog · tui-about");
    }

    #[test]
    fn changelog_uses_the_pages_search_and_ranks_titles_first() {
        let entries = vec![
            entry("a", "Faster board", "login is quicker now", vec![]),
            entry("b", "Login form shipped", "", vec![PrRef::Number(1902)]),
            entry("c", "Unrelated", "", vec![]),
        ];
        let got = rank_changelog(&entries, "login");
        let got: Vec<(&str, &str)> = got.iter().map(|h| (h.id.as_str(), h.detail.as_str())).collect();
        assert_eq!(got, [("b", "Oct 28 · #1902"), ("a", "Oct 28")]);
        // Link targets count, as on the page.
        assert_eq!(rank_changelog(&entries, "1902").len(), 1);
    }

    #[test]
    fn all_groups_by_kind_caps_each_and_offers_show_all() {
        let commands = vec![command("action:log-out", "Log out")];
        let hits = SearchHits {
            query: "lo".into(),
            issues: (0..5).map(|i| doc(&i.to_string(), "Login", "")).collect(),
            changelog: vec![ChangelogHit {
                id: "c".into(),
                title: "Login shipped".into(),
                detail: "Oct 28".into(),
            }],
        };
        let rows = result_rows(&commands, &hits, SearchFilter::All, true);
        let shape: Vec<String> = rows
            .iter()
            .map(|r| match r {
                ResultRow::Header(k) => format!("# {}", k.heading()),
                ResultRow::Item(ResultItem::Issue(d)) => format!("issue {}", d.id),
                ResultRow::Item(ResultItem::Changelog(h)) => format!("entry {}", h.id),
                ResultRow::Item(ResultItem::Command(e)) => format!("command {}", e.id),
                ResultRow::Item(ResultItem::ShowAll { kind, count }) => kind.show_all_label(*count),
            })
            .collect();
        assert_eq!(
            shape,
            [
                "# Commands",
                "command action:log-out",
                "# Issues",
                "issue 0",
                "issue 1",
                "issue 2",
                "Show all 5 issues",
                "# Changelog Entries",
                "entry c",
            ]
        );
        // A kind that matched alone isn't capped.
        let alone = SearchHits {
            query: "lo".into(),
            issues: hits.issues.clone(),
            changelog: vec![],
        };
        let rows = result_rows(&[], &alone, SearchFilter::All, true);
        assert_eq!(rows.len(), 6, "a header and all five issues");
        assert!(rows.iter().all(|r| !matches!(r, ResultRow::Item(ResultItem::ShowAll { .. }))));
        // One kind's filter: all of it, no header.
        assert_eq!(result_rows(&commands, &hits, SearchFilter::Issues, true).len(), 5);
        assert_eq!(
            result_rows(&commands, &hits, SearchFilter::Commands, true),
            [ResultRow::Item(ResultItem::Command(commands[0].0.clone()))]
        );
        // Not searching: the commands alone, as ever.
        assert_eq!(result_rows(&commands, &hits, SearchFilter::All, false).len(), 1);
        // The chips count each kind; All sums them.
        let counts: Vec<usize> = SearchFilter::ROW
            .iter()
            .map(|f| match_count(*f, commands.len(), &hits))
            .collect();
        assert_eq!(counts, [7, 5, 1, 1]);
    }

    #[test]
    fn items_activate_open_commands() {
        let issue = ResultItem::Issue(doc("1931", "Fix login", ""));
        assert_eq!(issue.entry().unwrap().id, "action:open-issue:1931");
        let entry = ResultItem::Changelog(ChangelogHit {
            id: "2026-10-28-login".into(),
            title: "t".into(),
            detail: String::new(),
        });
        assert_eq!(entry.entry().unwrap().id, "action:open-changelog-entry:2026-10-28-login");
        assert!(ResultItem::ShowAll {
            kind: ResultKind::Issue,
            count: 4
        }
        .entry()
        .is_none());
        let (e, _) = command("view:tasks", "Issues");
        assert_eq!(ResultItem::Command(e.clone()).entry(), Some(e));
    }

    #[test]
    fn filters_cycle_both_ways() {
        assert_eq!(SearchFilter::All.next(), SearchFilter::Issues);
        assert_eq!(SearchFilter::Commands.next(), SearchFilter::All);
        assert_eq!(SearchFilter::All.prev(), SearchFilter::Commands);
        assert_eq!(SearchFilter::Changelog.label(), "Changelog Entries");
    }

    #[test]
    fn the_worker_debounces_to_the_last_query() {
        let corpus = SearchCorpus::new(
            vec![doc("1", "Login", ""), doc("2", "Logout", "")],
            vec![],
        );
        let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = woke.clone();
        let worker = SearchWorker::spawn(
            Duration::from_millis(40),
            Some(Arc::new(move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
        );
        for q in ["l", "lo", "logo"] {
            worker.submit(q, &corpus);
        }
        let hits = worker.recv_timeout(Duration::from_secs(5)).expect("a search lands");
        assert_eq!(hits.query, "logo", "the burst searched once, for the last query");
        assert_eq!(ids(&hits.issues), ["2"]);
        assert!(worker.recv_timeout(Duration::from_millis(150)).is_none());
        assert_eq!(woke.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn the_palette_search_clears_on_blank_and_reruns_on_fresh_data() {
        let mut s = PaletteSearch::inline(SearchCorpus::default());
        s.ask("login");
        assert!(s.hits().issues.is_empty() && !s.pending());
        s.set_corpus(SearchCorpus::new(vec![doc("1", "Login", "")], vec![]));
        assert_eq!(ids(&s.hits().issues), ["1"], "fresh data searched again");
        s.ask(" ");
        assert!(s.hits().issues.is_empty());

        let mut bg = PaletteSearch::background(SearchCorpus::new(vec![doc("1", "Login", "")], vec![]), None);
        bg.ask("login");
        assert!(bg.pending());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !bg.poll() {
            assert!(std::time::Instant::now() < deadline, "the search lands");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!bg.pending());
        assert_eq!(ids(&bg.hits().issues), ["1"]);
    }

    #[test]
    fn issue_docs_name_the_resolved_status() {
        let statuses = shelbi_core::default_project_statuses();
        let workflows = vec![shelbi_core::default_workflow()];
        let rules = BoardRules {
            workflows: &workflows,
            default_workflow_name: shelbi_core::DEFAULT_WORKFLOW_NAME,
        };
        let now = chrono::Utc::now();
        let task = shelbi_core::Issue {
            id: "1931".into(),
            title: "Fix login".into(),
            column: shelbi_core::Column::from_status_id("in-progress"),
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: Default::default(),
        };
        let docs = issue_docs(
            &[IssueFile {
                task,
                body: "Body".into(),
                tracker_assignees: Vec::new(),
            }],
            &statuses,
            rules,
        );
        assert_eq!(docs[0].status, "In Progress");
        assert_eq!(docs[0].category, StatusCategory::Active);
        assert_eq!(docs[0].body, "Body");
    }
}
