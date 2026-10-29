//! The Issues board's text search, shared by every client.
//!
//! On a GitHub board a query is a GitHub issue search (open and closed, scoped
//! to the project's repo), so a match shows whether or not the board has it
//! loaded: the done column is paginated, and a filter over the loaded cards
//! looked arbitrarily cut off. A local (`file_system`) board, a one-character
//! query, and a search GitHub won't serve (rate-limited, unreachable, failed)
//! all filter the board the client already has instead
//! ([`crate::board::card_matches_search`]); the last of those says so.
//!
//! [`BoardSearch`] is the pure half: what was typed, when a request is due
//! (debounced [`DEBOUNCE`] after the last change), which response is current
//! (each request carries a sequence number and only the newest is shown, so a
//! slow older response never replaces a newer one), the per-query cache
//! ([`CACHE_TTL`]), and "load more" paging. It takes the clock as an argument
//! and performs no IO. [`SearchWorker`] runs the requests off the UI thread
//! through a [`SearchClient`] ([`GithubSearch`] in the apps, a fake in tests).
//! A client holds one of each and calls [`BoardSearch::pump`] on its tick.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shelbi_state::IssueFile;

use crate::board::card_matches_search;

/// How long typing must pause before a search goes out.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// The shortest query (in characters, trimmed) worth a GitHub search; shorter
/// ones filter the board locally.
pub const MIN_QUERY_CHARS: usize = 2;

/// How long a query's results are reused before it is searched again.
pub const CACHE_TTL: Duration = Duration::from_secs(60);

/// How many queries the cache holds at most.
const CACHE_CAP: usize = 32;

/// One page of search results.
#[derive(Debug, Clone)]
pub struct SearchPage {
    /// The page's matching issues, in the tracker's relevance order.
    pub issues: Vec<IssueFile>,
    /// How many issues match in all, as the tracker reports it.
    pub total: u64,
}

/// Why a search came back without results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchFailure {
    /// The board's tracker has no remote search: filter locally, silently.
    Unsupported,
    /// The search quota is spent (or parked until it resets).
    RateLimited,
    /// The tracker can't be reached.
    Unreachable,
    /// Anything else.
    Failed(String),
}

/// Runs one search request. Called on [`SearchWorker`]'s thread.
pub trait SearchClient: Send + Sync {
    fn search(&self, project: &str, query: &str, page: u32) -> Result<SearchPage, SearchFailure>;
}

/// The real client: GitHub's issue search through `gh`
/// ([`shelbi_state::search_issues`]), on the token's search budget.
pub struct GithubSearch;

impl SearchClient for GithubSearch {
    fn search(&self, project: &str, query: &str, page: u32) -> Result<SearchPage, SearchFailure> {
        use shelbi_state::IssueSearchError as E;
        shelbi_state::search_issues(project, query, page)
            .map(|p| SearchPage {
                issues: p.issues,
                total: p.total_count.min(shelbi_state::ISSUE_SEARCH_MAX_RESULTS),
            })
            .map_err(|e| match e {
                E::NotGithub => SearchFailure::Unsupported,
                E::RateLimited => SearchFailure::RateLimited,
                E::Unreachable => SearchFailure::Unreachable,
                E::Failed(msg) => SearchFailure::Failed(msg),
            })
    }
}

/// A search to run: page `page` (1-based) of `query`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRequest {
    /// Increases with every request; only the newest one's response shows.
    pub seq: u64,
    pub query: String,
    pub page: u32,
}

/// What a [`SearchRequest`] came back with.
#[derive(Debug, Clone)]
pub struct SearchResponse {
    pub request: SearchRequest,
    pub result: Result<SearchPage, SearchFailure>,
}

/// One query's results, as far as they're paged in.
#[derive(Debug, Clone)]
struct Results {
    key: String,
    issues: Vec<IssueFile>,
    ids: HashSet<String>,
    pages: u32,
    total: u64,
}

impl Results {
    fn first(key: String, page: SearchPage) -> Results {
        let mut r = Results {
            key,
            issues: Vec::new(),
            ids: HashSet::new(),
            pages: 0,
            total: page.total,
        };
        r.append(page);
        r
    }

    fn append(&mut self, page: SearchPage) {
        self.pages += 1;
        self.total = page.total;
        for tf in page.issues {
            if self.ids.insert(tf.task.id.clone()) {
                self.issues.push(tf);
            }
        }
    }

    /// More pages are left: fewer pages loaded than the total fills, and the
    /// tracker's result ceiling isn't reached.
    fn has_more(&self) -> bool {
        let page_size = u64::from(shelbi_state::ISSUE_SEARCH_PAGE_SIZE);
        let reachable = self.total.min(shelbi_state::ISSUE_SEARCH_MAX_RESULTS);
        u64::from(self.pages) * page_size < reachable
    }
}

/// What the search box says about the search, for both clients.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchStatus {
    /// A request is due or in flight for the current query: show the
    /// searching indicator.
    pub searching: bool,
    /// The tracker's total match count, while its results show.
    pub total: Option<u64>,
    /// How many of those are loaded.
    pub loaded: usize,
    /// "Load more" has another page of results to fetch.
    pub has_more: bool,
    /// The tracker's search is unavailable and the board shows a local filter
    /// of the cards it has loaded instead.
    pub local_fallback: bool,
}

impl SearchStatus {
    /// The search box's note: the match count, or the local-results notice.
    pub fn label(&self) -> Option<String> {
        if self.local_fallback {
            return Some("showing local results".to_string());
        }
        let total = self.total?;
        let noun = if total == 1 { "match" } else { "matches" };
        Some(if self.has_more {
            format!("{} of {total} {noun}", self.loaded)
        } else {
            format!("{total} {noun}")
        })
    }
}

/// The board search's state. See the module docs.
#[derive(Debug, Default)]
pub struct BoardSearch {
    /// The query as typed.
    query: String,
    /// Whether the board's tracker can search remotely (a GitHub board).
    remote: bool,
    /// When the query last changed while a request is due: the debounce
    /// clock. `None` when nothing is waiting to go out.
    due_since: Option<Instant>,
    seq: u64,
    /// The newest request sent; its response is the only one shown.
    in_flight: Option<SearchRequest>,
    /// The current query's results.
    results: Option<Results>,
    /// The previous query's results, kept on screen (narrowed locally to the
    /// new query) while the new one loads, so typing doesn't flash the board.
    last: Option<Results>,
    /// The current query's search failed: filter locally and say so.
    failed: Option<SearchFailure>,
    cache: HashMap<String, (Instant, Results)>,
}

/// The cache key of a query: its words, lowercased (the search ignores case
/// and spacing). Also what is sent.
fn normalize(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl BoardSearch {
    pub fn new() -> Self {
        Self::default()
    }

    /// A search that only ever filters locally, for `query`.
    pub fn local(query: &str) -> Self {
        BoardSearch {
            query: query.to_string(),
            ..Self::default()
        }
    }

    /// The query as typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether a query is active (a non-blank one).
    pub fn is_active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn key(&self) -> String {
        normalize(&self.query)
    }

    /// Whether the current query goes to the tracker.
    fn wants_remote(&self) -> bool {
        self.remote && self.key().chars().count() >= MIN_QUERY_CHARS
    }

    /// Say whether the board can search remotely (it is GitHub-backed), from
    /// each board read. Turning it off drops any remote results.
    pub fn set_remote(&mut self, remote: bool, now: Instant) {
        if self.remote == remote {
            return;
        }
        self.remote = remote;
        let query = std::mem::take(&mut self.query);
        self.results = None;
        self.last = None;
        self.in_flight = None;
        self.due_since = None;
        self.set_query(&query, now);
    }

    /// Change the query. Cached results show at once; otherwise a search is
    /// due [`DEBOUNCE`] after the last change, and until it lands the board
    /// filters locally (keeping the previous results that still match).
    /// Returns whether anything changed.
    pub fn set_query(&mut self, query: &str, now: Instant) -> bool {
        if self.query == query {
            return false;
        }
        let old_key = self.key();
        self.query = query.to_string();
        let key = self.key();
        if key == old_key {
            // Only spacing or case changed: same search.
            return true;
        }
        if let Some(prev) = self.results.take() {
            self.last = Some(prev);
        }
        self.failed = None;
        self.in_flight = None;
        self.due_since = None;
        if !self.wants_remote() {
            self.last = None;
            return true;
        }
        if let Some(hit) = self.cached(&key, now) {
            self.results = Some(hit);
        } else {
            self.due_since = Some(now);
        }
        true
    }

    fn cached(&self, key: &str, now: Instant) -> Option<Results> {
        self.cache
            .get(key)
            .filter(|(at, _)| now.duration_since(*at) < CACHE_TTL)
            .map(|(_, r)| r.clone())
    }

    fn remember(&mut self, results: &Results, now: Instant) {
        self.cache
            .retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
        if self.cache.len() >= CACHE_CAP && !self.cache.contains_key(&results.key) {
            if let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
            {
                self.cache.remove(&oldest);
            }
        }
        self.cache.insert(results.key.clone(), (now, results.clone()));
    }

    fn request(&mut self, page: u32) -> SearchRequest {
        self.seq += 1;
        let req = SearchRequest {
            seq: self.seq,
            query: self.key(),
            page,
        };
        self.in_flight = Some(req.clone());
        req
    }

    /// The request to send now, once the debounce has passed since the last
    /// change. At most one per change.
    pub fn poll(&mut self, now: Instant) -> Option<SearchRequest> {
        let since = self.due_since?;
        if now.duration_since(since) < DEBOUNCE {
            return None;
        }
        self.due_since = None;
        Some(self.request(1))
    }

    /// The next page of the current results ("load more"), or `None` when
    /// there's none or a request is already out.
    pub fn load_more(&mut self) -> Option<SearchRequest> {
        if self.in_flight.is_some() || self.due_since.is_some() {
            return None;
        }
        let page = self.results.as_ref().filter(|r| r.has_more())?.pages + 1;
        Some(self.request(page))
    }

    /// Fold a response in. Only the newest request's response shows; an older
    /// one's first page is still cached for its query. Returns whether the
    /// board changed.
    pub fn apply(&mut self, response: SearchResponse, now: Instant) -> bool {
        let SearchResponse { request, result } = response;
        let current = self.in_flight.as_ref().is_some_and(|r| r.seq == request.seq);
        if !current {
            if let (Ok(page), 1) = (result, request.page) {
                let results = Results::first(request.query, page);
                self.remember(&results, now);
            }
            return false;
        }
        self.in_flight = None;
        match result {
            Ok(page) => {
                match self.results.as_mut() {
                    Some(r) if request.page > 1 && r.key == request.query => r.append(page),
                    _ => self.results = Some(Results::first(request.query, page)),
                }
                self.last = None;
                self.failed = None;
                if let Some(r) = self.results.clone() {
                    self.remember(&r, now);
                }
            }
            Err(SearchFailure::Unsupported) => {
                // Not a remote board after all: filter locally, silently.
                self.remote = false;
                self.results = None;
                self.last = None;
            }
            Err(e) => {
                if request.page == 1 {
                    self.results = None;
                    self.last = None;
                    self.failed = Some(e);
                }
                // A failed "load more" keeps the pages already shown.
            }
        }
        true
    }

    /// Send what's due through `worker` and fold in what it answered: the
    /// client's per-tick call. Returns whether the board changed.
    pub fn pump(&mut self, worker: &SearchWorker, now: Instant) -> bool {
        let mut changed = false;
        while let Some(resp) = worker.try_recv() {
            changed |= self.apply(resp, now);
        }
        if let Some(req) = self.poll(now) {
            worker.send(req);
            changed = true;
        }
        changed
    }

    /// Whether a request is waiting on the debounce (the client should keep
    /// ticking until it goes out).
    pub fn is_due(&self) -> bool {
        self.due_since.is_some()
    }

    /// Whether the tracker's results for the current query are what the board
    /// shows (so "load more" pages the results, not the board's history).
    pub fn showing_results(&self) -> bool {
        self.is_active() && self.results.is_some()
    }

    /// Issues the search adds to the board: the current results, or while
    /// they load the previous query's. A card the board already has wins over
    /// its copy here ([`crate::board::BoardInput::tasks`]).
    pub fn hits(&self) -> &[IssueFile] {
        if !self.is_active() {
            return &[];
        }
        self.results
            .as_ref()
            .or(self.last.as_ref())
            .map_or(&[], |r| r.issues.as_slice())
    }

    /// Whether a card shows under the search: every card with no query; the
    /// tracker's matches once they've landed; otherwise a local filter of the
    /// query over the card's title, id and branch.
    pub fn shows(&self, title: &str, id: &str, branch: Option<&str>) -> bool {
        if !self.is_active() {
            return true;
        }
        match &self.results {
            Some(r) => r.ids.contains(id),
            None => card_matches_search(title, id, branch, &self.query),
        }
    }

    /// The search box's state, or `None` with no query.
    pub fn status(&self) -> Option<SearchStatus> {
        if !self.is_active() {
            return None;
        }
        let searching = self.due_since.is_some() || self.in_flight.is_some();
        Some(match &self.results {
            Some(r) => SearchStatus {
                searching,
                total: Some(r.total),
                loaded: r.issues.len(),
                has_more: r.has_more(),
                local_fallback: false,
            },
            None => SearchStatus {
                searching,
                local_fallback: self.failed.is_some(),
                ..SearchStatus::default()
            },
        })
    }
}

/// Runs search requests on its own thread. A request queued behind a newer
/// one is dropped unsent (only the newest matters), so a burst costs at most
/// the one in flight plus the latest. Dropping the worker lets its thread
/// finish the request in hand and exit; nothing waits on it.
pub struct SearchWorker {
    tx: Sender<SearchRequest>,
    rx: Receiver<SearchResponse>,
}

impl SearchWorker {
    pub fn spawn(project: impl Into<String>, client: Arc<dyn SearchClient>) -> SearchWorker {
        let project = project.into();
        let (req_tx, req_rx) = mpsc::channel::<SearchRequest>();
        let (resp_tx, resp_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("board-search".into())
            .spawn(move || {
                while let Ok(mut req) = req_rx.recv() {
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let result = client.search(&project, &req.query, req.page);
                    if resp_tx.send(SearchResponse { request: req, result }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn board-search thread");
        SearchWorker {
            tx: req_tx,
            rx: resp_rx,
        }
    }

    /// The worker over GitHub's search for `project`.
    pub fn github(project: impl Into<String>) -> SearchWorker {
        SearchWorker::spawn(project, Arc::new(GithubSearch))
    }

    pub fn send(&self, request: SearchRequest) {
        let _ = self.tx.send(request);
    }

    pub fn try_recv(&self) -> Option<SearchResponse> {
        self.rx.try_recv().ok()
    }

    /// Wait up to `timeout` for a response (tests).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<SearchResponse> {
        self.rx.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use shelbi_core::{Column, Issue};

    use super::*;
    use crate::board::{BoardInput, ClosedPage};
    use crate::view::{BoardFilters, IssuesModel};

    fn issue(id: &str, title: &str, status: &str) -> IssueFile {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        IssueFile {
            task: Issue {
                id: id.to_string(),
                title: title.to_string(),
                column: Column::from_status_id(status),
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
                params: BTreeMap::new(),
            },
            body: String::new(),
            tracker_assignees: Vec::new(),
        }
    }

    fn page(total: u64, issues: Vec<IssueFile>) -> Result<SearchPage, SearchFailure> {
        Ok(SearchPage { issues, total })
    }

    fn respond(req: &SearchRequest, result: Result<SearchPage, SearchFailure>) -> SearchResponse {
        SearchResponse {
            request: req.clone(),
            result,
        }
    }

    fn remote(t0: Instant) -> BoardSearch {
        let mut s = BoardSearch::new();
        s.set_remote(true, t0);
        s
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    #[test]
    fn typing_sends_one_request_after_the_pause() {
        let t0 = Instant::now();
        let mut s = remote(t0);
        s.set_query("c", t0);
        assert!(s.poll(ms(t0, 400)).is_none(), "one character never searches");
        s.set_query("ch", ms(t0, 100));
        s.set_query("cha", ms(t0, 200));
        assert!(s.poll(ms(t0, 450)).is_none(), "the pause restarts on each keystroke");
        assert!(s.status().unwrap().searching, "a pending search shows as searching");
        let req = s.poll(ms(t0, 500)).expect("due 300ms after the last keystroke");
        assert_eq!((req.query.as_str(), req.page), ("cha", 1));
        assert!(s.poll(ms(t0, 900)).is_none(), "one request per change");
    }

    #[test]
    fn a_stale_response_never_replaces_a_newer_one() {
        let t0 = Instant::now();
        let mut s = remote(t0);
        s.set_query("cha", t0);
        let old = s.poll(ms(t0, 300)).unwrap();
        s.set_query("change", ms(t0, 400));
        let new = s.poll(ms(t0, 700)).unwrap();
        assert!(new.seq > old.seq);

        assert!(s.apply(respond(&new, page(1, vec![issue("b", "Change B", "todo")])), ms(t0, 800)));
        assert!(
            !s.apply(respond(&old, page(2, vec![issue("a", "Cha A", "todo")])), ms(t0, 900)),
            "the older query's late response is ignored"
        );
        assert_eq!(s.hits().len(), 1);
        assert_eq!(s.hits()[0].task.id, "b");
        assert!(!s.status().unwrap().searching);

        // ...but cached for its own query, which then shows with no request.
        s.set_query("cha", ms(t0, 1_000));
        assert!(s.showing_results());
        assert_eq!(s.hits()[0].task.id, "a");
        assert!(s.poll(ms(t0, 2_000)).is_none(), "a cached query sends nothing");
    }

    #[test]
    fn the_cache_expires() {
        let t0 = Instant::now();
        let mut s = remote(t0);
        s.set_query("change", t0);
        let req = s.poll(ms(t0, 300)).unwrap();
        s.apply(respond(&req, page(0, vec![])), ms(t0, 400));
        s.set_query("", ms(t0, 500));
        s.set_query("change", ms(t0, 600));
        assert!(s.showing_results(), "fresh: served from the cache");
        s.set_query("", ms(t0, 700));
        let later = ms(t0, 400) + CACHE_TTL;
        s.set_query("change", later);
        assert!(!s.showing_results());
        assert!(s.poll(later + DEBOUNCE).is_some(), "expired: searched again");
    }

    #[test]
    fn results_map_to_their_columns_across_the_board() {
        let t0 = Instant::now();
        // The board has loaded one open card and the first done page.
        let mut input = BoardInput {
            open: vec![issue("t-1", "Change the palette", "todo"), issue("t-2", "Other", "todo")],
            loading: false,
            remote_search: true,
            ..BoardInput::default()
        };
        input.apply_closed(Some(ClosedPage {
            issues: vec![issue("d-1", "Recent change", "done")],
            next_cursor: Some("c".into()),
        }));
        let mut s = remote(t0);
        s.set_query("change", t0);
        let req = s.poll(ms(t0, 300)).unwrap();
        // GitHub finds the loaded todo card (its board copy wins), an old done
        // issue beyond the loaded page, and one in review.
        let mut stale = issue("t-1", "Change the palette (stale copy)", "todo");
        stale.task.priority = 9;
        s.apply(
            respond(
                &req,
                page(
                    45,
                    vec![stale, issue("d-9", "Old change", "done"), issue("r-1", "A change", "review")],
                ),
            ),
            ms(t0, 400),
        );
        input.set_search_hits(&s);
        let model = IssuesModel::build_searched(&input, &BoardFilters::default(), &s);
        let ids = |status: &str| -> Vec<String> {
            model
                .columns
                .iter()
                .find(|c| c.status_id == status)
                .unwrap()
                .cards
                .iter()
                .map(|c| c.id.clone())
                .collect()
        };
        assert_eq!(ids("todo"), ["t-1"], "non-matching loaded cards drop out");
        assert_eq!(model.card("t-1").unwrap().title, "Change the palette", "the board copy wins");
        assert_eq!(ids("review"), ["r-1"]);
        assert_eq!(ids("done"), ["d-9"], "a match beyond the loaded done page shows");
        let status = model.search.clone().unwrap();
        assert_eq!(status.total, Some(45));
        assert!(model.has_more_done, "load more pages the results");
        assert_eq!(status.label().as_deref(), Some("3 of 45 matches"));

        // Clearing puts the board back as it was, history cursor and all.
        s.set_query("", ms(t0, 500));
        input.set_search_hits(&s);
        let model = IssuesModel::build_searched(&input, &BoardFilters::default(), &s);
        assert!(model.search.is_none());
        assert_eq!(model.columns.iter().map(|c| c.cards.len()).sum::<usize>(), 3);
        assert!(model.has_more_done, "the history cursor is untouched");
        assert_eq!(input.closed_next_cursor.as_deref(), Some("c"));
    }

    #[test]
    fn load_more_pages_the_results() {
        let t0 = Instant::now();
        let mut s = remote(t0);
        assert!(s.load_more().is_none(), "nothing to page without results");
        s.set_query("change", t0);
        let req = s.poll(ms(t0, 300)).unwrap();
        let first: Vec<IssueFile> = (0..30).map(|i| issue(&format!("a{i}"), "x", "done")).collect();
        s.apply(respond(&req, page(31, first)), ms(t0, 400));
        assert!(s.status().unwrap().has_more);
        let more = s.load_more().expect("a second page");
        assert_eq!((more.query.as_str(), more.page), ("change", 2));
        assert!(s.load_more().is_none(), "one page request at a time");
        s.apply(respond(&more, page(31, vec![issue("b", "x", "done")])), ms(t0, 500));
        let status = s.status().unwrap();
        assert_eq!((status.loaded, status.has_more), (31, false));
        assert_eq!(status.label().as_deref(), Some("31 matches"));
    }

    #[test]
    fn a_rate_limit_falls_back_to_local_results_and_says_so() {
        let t0 = Instant::now();
        let mut s = remote(t0);
        s.set_query("palette", t0);
        let req = s.poll(ms(t0, 300)).unwrap();
        s.apply(respond(&req, Err(SearchFailure::RateLimited)), ms(t0, 400));
        let status = s.status().unwrap();
        assert!(status.local_fallback && !status.searching);
        assert_eq!(status.label().as_deref(), Some("showing local results"));
        assert!(s.hits().is_empty());
        assert!(s.shows("Restyle the palette", "t-1", None), "filters locally");
        assert!(!s.shows("Other", "t-2", None));
        assert!(!s.showing_results(), "load more stays the board's own");
    }

    #[test]
    fn a_local_board_never_searches() {
        let t0 = Instant::now();
        let mut s = BoardSearch::new();
        s.set_query("palette", t0);
        assert!(s.poll(ms(t0, 1_000)).is_none());
        let status = s.status().unwrap();
        assert!(!status.searching && !status.local_fallback && status.label().is_none());
        assert!(s.shows("The palette", "t-1", None));
        assert!(!s.shows("Other", "t-2", None));
        // An unsupported tracker answer turns remote search off quietly.
        let mut s = remote(t0);
        s.set_query("palette", t0);
        let req = s.poll(ms(t0, 300)).unwrap();
        s.apply(respond(&req, Err(SearchFailure::Unsupported)), ms(t0, 400));
        assert!(s.status().unwrap().label().is_none());
        s.set_query("palettes", ms(t0, 500));
        assert!(s.poll(ms(t0, 1_000)).is_none());
    }

    #[test]
    fn previous_results_that_still_match_stay_while_the_next_query_loads() {
        let t0 = Instant::now();
        let mut s = remote(t0);
        s.set_query("chan", t0);
        let req = s.poll(ms(t0, 300)).unwrap();
        s.apply(
            respond(&req, page(2, vec![issue("a", "Change A", "done"), issue("b", "Chant B", "done")])),
            ms(t0, 400),
        );
        s.set_query("chang", ms(t0, 500));
        assert_eq!(s.hits().len(), 2, "the previous hits stay on the board");
        assert!(s.shows("Change A", "a", None));
        assert!(!s.shows("Chant B", "b", None), "narrowed locally to the new query");
    }

    struct FakeClient {
        calls: Mutex<Vec<(String, u32)>>,
    }

    impl SearchClient for FakeClient {
        fn search(&self, _project: &str, query: &str, page: u32) -> Result<SearchPage, SearchFailure> {
            self.calls.lock().unwrap().push((query.to_string(), page));
            Ok(SearchPage {
                issues: vec![issue(&format!("{query}-{page}"), query, "todo")],
                total: 1,
            })
        }
    }

    #[test]
    fn the_worker_runs_debounced_requests_through_the_client() {
        let client = Arc::new(FakeClient {
            calls: Mutex::new(Vec::new()),
        });
        let worker = SearchWorker::spawn("p", client.clone());
        let t0 = Instant::now();
        let mut s = remote(t0);
        for (i, q) in ["ch", "cha", "chan", "change"].iter().enumerate() {
            s.set_query(q, ms(t0, 50 * i as u64));
            s.pump(&worker, ms(t0, 50 * i as u64 + 10));
        }
        assert!(s.pump(&worker, ms(t0, 500)), "sending the search changes the board");
        let resp = worker.recv_timeout(Duration::from_secs(5)).expect("the worker answers");
        assert!(s.apply(resp, ms(t0, 600)));
        assert_eq!(*client.calls.lock().unwrap(), [("change".to_string(), 1)]);
        assert_eq!(s.hits()[0].task.id, "change-1");
    }
}
