//! The Changelog page: the project's record of what has shipped, newest first.
//!
//! Both clients render the same [`ChangelogModel`]: the entries (read off the
//! UI thread by [`read_changelog`], each with its links resolved to a
//! [`LinkKind`] and a [`LinkTarget`]), a search query narrowing them, and
//! paging ([`PAGE_SIZE`] entries at a time, more on [`ChangelogModel::load_more`]).
//! A [`ChangelogWatch`] tells a client when to read again: it compares the
//! changelog directory's [`ChangelogStamp`], so an entry added by the
//! orchestrator or `shelbi changelog add` shows up on the client's next tick.
//!
//! Storage, the link syntax and search live in [`shelbi_state::changelog`];
//! this module is the view over them.

use chrono::{DateTime, FixedOffset};
use shelbi_state::changelog::{self, ChangelogStamp, Link};

/// How many entries a page shows, and how many more each
/// [`ChangelogModel::load_more`] adds.
pub const PAGE_SIZE: usize = 50;

/// What a link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// A Shelbi issue.
    Issue,
    /// A pull request.
    Pr,
    /// Any other URL.
    Url,
}

/// Where following a link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Open this Shelbi issue inside Shelbi.
    Issue(String),
    /// Open this URL in the browser.
    Url(String),
    /// Nowhere: a PR number with no known GitHub repository.
    Unresolved,
}

/// One resolved link, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogLink {
    pub kind: LinkKind,
    /// How it reads: the issue id, `#1942`, or the URL without its scheme.
    pub label: String,
    pub target: LinkTarget,
    /// The link as the state layer has it, for search.
    link: Link,
}

impl ChangelogLink {
    fn new(link: Link) -> Self {
        let (kind, label, target) = match &link {
            Link::Issue { id } => (LinkKind::Issue, id.clone(), LinkTarget::Issue(id.clone())),
            Link::Pr { number, url } => (
                LinkKind::Pr,
                match (number, url) {
                    (None, Some(url)) => display_url(url),
                    _ => link.label(),
                },
                url.clone().map_or(LinkTarget::Unresolved, LinkTarget::Url),
            ),
            Link::Url { url } => (
                LinkKind::Url,
                display_url(url),
                LinkTarget::Url(url.clone()),
            ),
        };
        ChangelogLink {
            kind,
            label,
            target,
            link,
        }
    }
}

/// A URL as a label: no scheme, no `www.`, no trailing slash.
fn display_url(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.trim_end_matches('/').to_string()
}

/// One entry as a client draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntryView {
    /// The entry id (`shelbi changelog show <id>`).
    pub id: String,
    pub title: String,
    pub date: DateTime<FixedOffset>,
    /// `orchestrator`, `user`, or an agent's name.
    pub author: String,
    /// The description, in markdown.
    pub description: String,
    /// Every link: the frontmatter's, then the description's.
    pub links: Vec<ChangelogLink>,
    /// The links the description doesn't already show, for a client that
    /// renders the description and lists the rest beneath it.
    pub extra_links: Vec<ChangelogLink>,
}

impl ChangelogEntryView {
    /// The view of one stored entry, its PR numbers resolved against `repo`
    /// (`owner/name`). [`read_changelog`] builds every entry with this; a
    /// client's tests build fixtures with it.
    pub fn new(entry: changelog::ChangelogEntry, repo: Option<&str>) -> Self {
        ChangelogEntryView {
            links: entry
                .links(repo)
                .into_iter()
                .map(ChangelogLink::new)
                .collect(),
            extra_links: entry
                .extra_links(repo)
                .into_iter()
                .map(ChangelogLink::new)
                .collect(),
            id: entry.id,
            title: entry.title,
            date: entry.date,
            author: entry.author,
            description: entry.body,
        }
    }

    /// The day it shipped, spelled out like the docs changelog: `October 28, 2026`.
    pub fn date_label(&self) -> String {
        self.date.format("%B %-d, %Y").to_string()
    }

    /// The time it shipped, in the offset it was written in: `14:30`.
    pub fn time_label(&self) -> String {
        self.date.format("%H:%M").to_string()
    }

    /// The description for a client that draws every link as a chip under
    /// it: without the list of bare links the template's `{{links}}` ends it
    /// with (each item one `issue:<id>`, `#<n>` or URL), which the chips
    /// show. Any other description is the whole of it.
    pub fn prose(&self) -> &str {
        let text = self.description.trim_end();
        let mut cut = text.len();
        let mut items = 0;
        for line in text.lines().rev() {
            let Some(item) = line.trim().strip_prefix("- ") else {
                break;
            };
            if !is_bare_link(item.trim()) {
                return text;
            }
            items += 1;
            cut -= line.len();
            // The newline before it, if any.
            cut = text[..cut].strip_suffix('\n').map_or(cut, str::len);
            cut = text[..cut].strip_suffix('\r').map_or(cut, str::len);
        }
        if items == 0 {
            return text;
        }
        // The template sets the list apart with a blank line; a list right
        // under a line of prose is the prose's own.
        // (`rest` then ends with that blank line's newline).
        let rest = &text[..cut];
        if rest.is_empty() || rest.ends_with('\n') {
            rest.trim_end()
        } else {
            text
        }
    }

    /// Whether the entry's title, description, or link targets hold every
    /// word of `query` (case-insensitive): the Changelog page's search, and
    /// the palette's over the same entries.
    pub fn matches(&self, query: &str) -> bool {
        let links: Vec<Link> = self.links.iter().map(|l| l.link.clone()).collect();
        changelog::matches_query(&self.title, &self.description, &links, query)
    }
}

/// One read of a project's changelog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangelogData {
    pub project: String,
    /// Newest first.
    pub entries: Vec<ChangelogEntryView>,
    /// Entry files that couldn't be read and were left out.
    pub skipped: usize,
    /// Why the changelog couldn't be read at all, if it couldn't.
    pub error: Option<String>,
    /// The directory's stamp when it was read.
    pub stamp: ChangelogStamp,
}

/// Read `project`'s changelog with its links resolved. Touches disk (the
/// entries, the project config, the repository's git config), so a client
/// runs it off its UI thread, e.g. on a [`crate::refresh::spawn_worker`].
pub fn read_changelog(project: &str) -> ChangelogData {
    // Stamp first: an entry landing mid-read then shows as a change next tick.
    let stamp = changelog::changelog_stamp(project);
    let repo = changelog::project_github_repo(project);
    match changelog::read_entries(project) {
        Ok(read) => ChangelogData {
            project: project.to_string(),
            skipped: read.skipped.len(),
            entries: read
                .entries
                .into_iter()
                .map(|e| ChangelogEntryView::new(e, repo.as_deref()))
                .collect(),
            error: None,
            stamp,
        },
        Err(e) => ChangelogData {
            project: project.to_string(),
            error: Some(e.to_string()),
            stamp,
            ..ChangelogData::default()
        },
    }
}

/// The Changelog page's state: the entries last read, the search query, and
/// how many matches are shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogModel {
    /// `None` until the first read lands (a client shows "Loading…").
    data: Option<ChangelogData>,
    query: String,
    /// Indices into the entries that match the query, newest first.
    matches: Vec<usize>,
    /// How many of the matches are shown.
    shown: usize,
}

impl Default for ChangelogModel {
    fn default() -> Self {
        ChangelogModel {
            data: None,
            query: String::new(),
            matches: Vec::new(),
            shown: PAGE_SIZE,
        }
    }
}

impl ChangelogModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a fresh read. The query and how far the user has paged survive,
    /// so a refresh while reading doesn't jump back to the first page.
    pub fn set_data(&mut self, data: ChangelogData) {
        self.data = Some(data);
        self.rematch();
    }

    /// The last read, or `None` before the first.
    pub fn data(&self) -> Option<&ChangelogData> {
        self.data.as_ref()
    }

    /// Whether the first read hasn't landed yet.
    pub fn is_loading(&self) -> bool {
        self.data.is_none()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Search: show only entries whose title, description, or link targets
    /// hold every word of `query` (case-insensitive). Back to the first page.
    pub fn set_query(&mut self, query: impl Into<String>) {
        let query = query.into();
        if query == self.query {
            return;
        }
        self.query = query;
        self.shown = PAGE_SIZE;
        self.rematch();
    }

    fn rematch(&mut self) {
        let entries = self.data.as_ref().map_or(&[][..], |d| &d.entries[..]);
        self.matches = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matches(&self.query))
            .map(|(i, _)| i)
            .collect();
    }

    /// The entries on the shown pages, newest first.
    pub fn visible(&self) -> impl Iterator<Item = &ChangelogEntryView> {
        let entries = self.data.as_ref().map_or(&[][..], |d| &d.entries[..]);
        self.matches
            .iter()
            .take(self.shown)
            .filter_map(move |&i| entries.get(i))
    }

    /// How many entries match the query (all of them, with no query).
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// How many entries there are in all.
    pub fn total(&self) -> usize {
        self.data.as_ref().map_or(0, |d| d.entries.len())
    }

    /// Whether more matches are past the shown pages.
    pub fn has_more(&self) -> bool {
        self.matches.len() > self.shown
    }

    /// Show the next page. A client calls this when the user scrolls near the
    /// end (or picks "Show more").
    pub fn load_more(&mut self) {
        if self.has_more() {
            self.shown += PAGE_SIZE;
        }
    }

    /// The entry with `id`, if it's in the last read.
    pub fn entry(&self, id: &str) -> Option<&ChangelogEntryView> {
        self.data.as_ref()?.entries.iter().find(|e| e.id == id)
    }
}

/// Whether `item` is one link and nothing else: `issue:<id>`, `#<n>`, or an
/// http(s) URL.
fn is_bare_link(item: &str) -> bool {
    if item.is_empty() || item.contains(char::is_whitespace) {
        return false;
    }
    let issue = item.strip_prefix("issue:").is_some_and(|id| !id.is_empty());
    let pr = item
        .strip_prefix('#')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    issue || pr || item.starts_with("https://") || item.starts_with("http://")
}

/// Where `query`'s words appear in `text`, for a client to highlight: the
/// byte ranges of every case-insensitive occurrence of each whitespace-
/// separated word (as [`ChangelogModel::set_query`] matches them), in order,
/// overlapping ones merged. Empty for an empty query.
pub fn match_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return Vec::new();
    }
    // Lowercase `text`, remembering where each lowered byte came from:
    // lowercasing can change a character's length.
    let mut lower = String::with_capacity(text.len());
    let mut origin: Vec<usize> = Vec::with_capacity(text.len() + 1);
    for (at, c) in text.char_indices() {
        for l in c.to_lowercase() {
            let start = lower.len();
            lower.push(l);
            origin.extend(std::iter::repeat_n(at, lower.len() - start));
        }
    }
    origin.push(text.len());
    // A lowered byte's original character's end, for a match ending there.
    let end_of = |ix: usize| -> usize {
        let at = origin[ix - 1];
        at + text[at..].chars().next().map_or(0, char::len_utf8)
    };
    let mut ranges: Vec<std::ops::Range<usize>> = terms
        .iter()
        .filter(|t| !t.is_empty())
        .flat_map(|t| {
            lower
                .match_indices(t.as_str())
                .map(|(ix, m)| origin[ix]..end_of(ix + m.len()))
                .collect::<Vec<_>>()
        })
        .collect();
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// Tells a client when the changelog has changed on disk. Poll
/// [`ChangelogWatch::changed`] on the client's tick (it only lists one
/// directory) and re-read when it says so.
#[derive(Debug, Clone)]
pub struct ChangelogWatch {
    project: String,
    stamp: Option<ChangelogStamp>,
}

impl ChangelogWatch {
    /// A watch on `project`. Its first [`changed`](Self::changed) is `true`,
    /// so the first poll triggers the first read.
    pub fn new(project: impl Into<String>) -> Self {
        ChangelogWatch {
            project: project.into(),
            stamp: None,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    /// Whether the changelog differs from the last time this returned `true`.
    pub fn changed(&mut self) -> bool {
        self.observe(changelog::changelog_stamp(&self.project))
    }

    /// Record `stamp`, returning whether it differs from the last one.
    pub fn observe(&mut self, stamp: ChangelogStamp) -> bool {
        let changed = self.stamp != Some(stamp);
        self.stamp = Some(stamp);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::ENV_LOCK;
    use shelbi_state::changelog::{NewEntry, PrRef};
    use std::path::PathBuf;

    fn view(id: &str, title: &str, body: &str, links: Vec<Link>) -> ChangelogEntryView {
        ChangelogEntryView {
            id: id.into(),
            title: title.into(),
            date: DateTime::parse_from_rfc3339("2026-10-28T14:30:00-07:00").unwrap(),
            author: "orchestrator".into(),
            description: body.into(),
            links: links.into_iter().map(ChangelogLink::new).collect(),
            extra_links: vec![],
        }
    }

    fn data(entries: Vec<ChangelogEntryView>) -> ChangelogData {
        ChangelogData {
            project: "p".into(),
            entries,
            ..ChangelogData::default()
        }
    }

    #[test]
    fn links_resolve_to_a_kind_label_and_target() {
        let links: Vec<ChangelogLink> = [
            Link::Issue {
                id: "tui-about".into(),
            },
            Link::Pr {
                number: Some(12),
                url: Some("https://github.com/a/b/pull/12".into()),
            },
            Link::Pr {
                number: Some(13),
                url: None,
            },
            Link::Pr {
                number: None,
                url: Some("https://gitlab.com/a/b/-/merge_requests/4".into()),
            },
            Link::Url {
                url: "https://www.shelbi.dev/docs/".into(),
            },
        ]
        .into_iter()
        .map(ChangelogLink::new)
        .collect();
        let got: Vec<(LinkKind, &str, &LinkTarget)> = links
            .iter()
            .map(|l| (l.kind, l.label.as_str(), &l.target))
            .collect();
        assert_eq!(
            got,
            [
                (
                    LinkKind::Issue,
                    "tui-about",
                    &LinkTarget::Issue("tui-about".into())
                ),
                (
                    LinkKind::Pr,
                    "#12",
                    &LinkTarget::Url("https://github.com/a/b/pull/12".into())
                ),
                (LinkKind::Pr, "#13", &LinkTarget::Unresolved),
                (
                    LinkKind::Pr,
                    "gitlab.com/a/b/-/merge_requests/4",
                    &LinkTarget::Url("https://gitlab.com/a/b/-/merge_requests/4".into())
                ),
                (
                    LinkKind::Url,
                    "shelbi.dev/docs",
                    &LinkTarget::Url("https://www.shelbi.dev/docs/".into())
                ),
            ]
        );
    }

    #[test]
    fn search_narrows_and_paging_extends() {
        let mut entries: Vec<ChangelogEntryView> = (0..(PAGE_SIZE + 5))
            .map(|i| view(&format!("e{i}"), &format!("Entry {i}"), "", vec![]))
            .collect();
        entries.push(view(
            "board",
            "Faster Board",
            "gpui lists",
            vec![Link::Issue {
                id: "dt-board-perf".into(),
            }],
        ));
        let mut m = ChangelogModel::new();
        assert!(m.is_loading());
        m.set_data(data(entries));
        assert!(!m.is_loading());
        assert_eq!(m.total(), PAGE_SIZE + 6);
        assert_eq!(m.visible().count(), PAGE_SIZE);
        assert!(m.has_more());
        m.load_more();
        assert_eq!(m.visible().count(), PAGE_SIZE + 6);
        assert!(!m.has_more());

        // A query matches title, description, and link targets; resets paging.
        for q in ["faster", "GPUI", "board-perf"] {
            m.set_query(q);
            let ids: Vec<&str> = m.visible().map(|e| e.id.as_str()).collect();
            assert_eq!(ids, ["board"], "query {q}");
        }
        m.set_query("");
        assert_eq!(m.match_count(), PAGE_SIZE + 6);
        assert_eq!(m.visible().count(), PAGE_SIZE);
    }

    #[test]
    fn prose_drops_the_templates_trailing_link_list() {
        let prose = |body: &str| view("e", "t", body, vec![]).prose().to_string();
        assert_eq!(
            prose("Every page matches.\n\n- issue:tui-polish\n- https://github.com/a/b/pull/12\n- #13\n"),
            "Every page matches."
        );
        assert_eq!(prose("Fixed.\r\n\r\n- issue:x\r\n- #4\r\n"), "Fixed.");
        assert_eq!(prose("- issue:only"), "");
        // A list with prose in it, or right under a line of prose, stays.
        let keep = "Changes:\n- https://a.dev\n- #4";
        assert_eq!(prose(keep), keep);
        let keep = "Done.\n\n- faster boards\n- https://a.dev";
        assert_eq!(prose(keep), keep);
        assert_eq!(prose("No links here.\n"), "No links here.");
    }

    #[test]
    fn match_ranges_find_every_word_case_insensitively() {
        let text = "Faster Board: the board's gpui lists";
        assert_eq!(match_ranges(text, "board"), [7..12, 18..23]);
        // Each word on its own, overlaps merged, in order.
        assert_eq!(match_ranges(text, "LISTS gpui"), [26..30, 31..36]);
        assert_eq!(match_ranges(text, "boar oard"), [7..12, 18..23]);
        assert!(match_ranges(text, "  ").is_empty());
        assert!(match_ranges(text, "nothing").is_empty());
        // Ranges index the original text where lowercasing changes lengths
        // (`İ` lowers to two characters).
        let text = "İstanbul ships";
        let r = match_ranges(text, "ships");
        assert_eq!(&text[r[0].clone()], "ships");
        let r = match_ranges(text, "i̇stan");
        assert_eq!(&text[r[0].clone()], "İstan");
    }

    #[test]
    fn a_refresh_keeps_the_query_and_the_page() {
        let mut m = ChangelogModel::new();
        m.set_data(data(
            (0..(PAGE_SIZE * 3))
                .map(|i| view(&i.to_string(), "x", "", vec![]))
                .collect(),
        ));
        m.load_more();
        m.set_data(data(
            (0..(PAGE_SIZE * 3))
                .map(|i| view(&i.to_string(), "x", "", vec![]))
                .collect(),
        ));
        assert_eq!(m.visible().count(), PAGE_SIZE * 2);
        let first = m.visible().next().unwrap();
        assert_eq!(first.date_label(), "October 28, 2026");
        assert_eq!(first.time_label(), "14:30");
        m.set_query("nothing matches");
        m.set_data(data(vec![view("a", "x", "", vec![])]));
        assert_eq!(m.query(), "nothing matches");
        assert_eq!(m.visible().count(), 0);
        assert!(m.entry("a").is_some());
    }

    #[test]
    fn the_read_and_the_watch_follow_an_added_entry() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home: PathBuf = std::env::temp_dir().join(format!(
            "shelbi-app-changelog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let mut watch = ChangelogWatch::new("p");
        assert!(watch.changed(), "the first poll reads");
        assert!(!watch.changed());
        let empty = read_changelog("p");
        assert!(empty.entries.is_empty() && empty.error.is_none());

        changelog::add_entry(
            "p",
            NewEntry {
                title: "Changelog lands".into(),
                date: changelog::now(),
                author: "orchestrator".into(),
                issues: vec!["changelog".into()],
                prs: vec![PrRef::Number(7)],
                urls: vec![],
                description: "Sidebar row.".into(),
            },
        )
        .unwrap();
        assert!(watch.changed(), "an added entry is a change");
        let read = read_changelog("p");
        assert_eq!(read.entries.len(), 1);
        let e = &read.entries[0];
        assert_eq!(e.title, "Changelog lands");
        // No GitHub repository for this project: the PR stays unresolved.
        let kinds: Vec<(LinkKind, &LinkTarget)> =
            e.links.iter().map(|l| (l.kind, &l.target)).collect();
        assert_eq!(
            kinds,
            [
                (LinkKind::Issue, &LinkTarget::Issue("changelog".into())),
                (LinkKind::Pr, &LinkTarget::Unresolved),
            ]
        );

        std::env::remove_var("SHELBI_HOME");
    }
}
