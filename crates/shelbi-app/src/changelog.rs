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
//! An entry's files ([`ChangelogAttachment`]) resolve against the changelog
//! directory on this machine ([`ChangelogEntryView::dir`]), which never
//! crosses the wire: a remote window's entries have none, so its images show
//! their alt text and its files can't be opened ([`ImageSource::Unavailable`]),
//! rather than failing.
//!
//! The page draws a timeline down its left edge: a tick per entry, and a
//! marker per release (the project's `releases:` tag glob, read with the
//! entries by [`read_changelog`]) placed between the entries that came
//! before and after it ([`ChangelogModel::timeline`]).
//!
//! Storage, the link syntax and search live in [`shelbi_state::changelog`];
//! this module is the view over them.

use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
pub use shelbi_state::changelog::Release;
use shelbi_state::changelog::{self, AttachmentKind, ChangelogStamp, Link};

/// How many entries a page shows, and how many more each
/// [`ChangelogModel::load_more`] adds.
pub const PAGE_SIZE: usize = 50;

/// What a link points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// A Shelbi issue.
    Issue,
    /// A pull request.
    Pr,
    /// Any other URL.
    Url,
}

/// Where following a link goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkTarget {
    /// Open this Shelbi issue inside Shelbi.
    Issue(String),
    /// Open this URL in the browser.
    Url(String),
    /// Nowhere: a PR number with no known GitHub repository.
    Unresolved,
}

/// One resolved link, ready to draw.
///
/// On the wire it is the state layer's [`Link`] alone (`{"kind":"pr",…}`):
/// the kind, label and target are derived from it, so decoding rebuilds them
/// with [`ChangelogLink::new`] rather than trusting a second copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Link", into = "Link")]
pub struct ChangelogLink {
    pub kind: LinkKind,
    /// How it reads: the issue id, `#1942`, or the URL without its scheme.
    pub label: String,
    pub target: LinkTarget,
    /// The link as the state layer has it, for search.
    link: Link,
}

impl From<Link> for ChangelogLink {
    fn from(link: Link) -> Self {
        ChangelogLink::new(link)
    }
}

impl From<ChangelogLink> for Link {
    fn from(link: ChangelogLink) -> Self {
        link.link
    }
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

/// One file on an entry, as a client lists or draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangelogAttachment {
    /// The stored file name.
    pub name: String,
    pub kind: AttachmentKind,
    /// Bytes.
    pub size: u64,
    /// Relative to the changelog directory: `assets/<entry-id>/<name>`.
    pub path: String,
    /// Whether it belongs in the entry's Attachments list: every file, and an
    /// image the description doesn't show.
    pub listed: bool,
}

impl ChangelogAttachment {
    /// `12 KB`, `4.1 MB`.
    pub fn size_label(&self) -> String {
        changelog::format_size(self.size)
    }
}

/// Where an image in a description comes from, for a client to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageSource {
    /// A file on this machine.
    File(PathBuf),
    /// A web image.
    Web(String),
    /// A local path whose file isn't there: draw the alt text, "image
    /// missing".
    Missing(PathBuf),
    /// The changelog can't be read from here (a remote window): draw the alt
    /// text.
    Unavailable,
}

/// One entry as a client draws it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
    /// The entry's files, from its frontmatter.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChangelogAttachment>,
    /// The changelog directory on this machine, that attachment paths and the
    /// description's relative image paths resolve against. Never sent over
    /// the wire, so `None` in a remote window, where the far directory can't
    /// be read.
    #[serde(skip)]
    pub dir: Option<PathBuf>,
}

impl ChangelogEntryView {
    /// The view of one stored entry, its PR numbers resolved against `repo`
    /// (`owner/name`). [`read_changelog`] builds every entry with this; a
    /// client's tests build fixtures with it.
    pub fn new(entry: changelog::ChangelogEntry, repo: Option<&str>) -> Self {
        let listed: Vec<String> = entry
            .listed_attachments()
            .iter()
            .map(|a| a.path.clone())
            .collect();
        let attachments = entry
            .attachments
            .iter()
            .map(|a| ChangelogAttachment {
                name: a.name.clone(),
                kind: a.kind,
                size: a.size,
                path: a.path.clone(),
                listed: listed.contains(&a.path),
            })
            .collect();
        let dir = Some(entry.dir().to_path_buf());
        ChangelogEntryView {
            attachments,
            dir,
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
    /// it: without a trailing list of bare links (each item one `issue:<id>`,
    /// `#<n>` or URL), which the chips show. The shipped template no longer
    /// writes one, but entries from the old default and templates that keep
    /// `{{links}}` still end with it. Any other description is the whole of
    /// it.
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

    /// The attachments for the entry's Attachments list
    /// ([`ChangelogAttachment::listed`]), in the order they were attached.
    pub fn listed_attachments(&self) -> impl Iterator<Item = &ChangelogAttachment> {
        self.attachments.iter().filter(|a| a.listed)
    }

    /// The file behind `attachment` on this machine, or `None` where the
    /// changelog can't be read from here (a remote window).
    pub fn attachment_file(&self, attachment: &ChangelogAttachment) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(&attachment.path))
    }

    /// Where the description's image `url` (a Markdown image's destination)
    /// comes from: a web URL as itself; a path relative to the changelog
    /// directory (`assets/<entry-id>/shot.png`, what `--image` writes), an
    /// absolute path or a `file://` URL as a file here when it exists.
    pub fn image_source(&self, url: &str) -> ImageSource {
        resolve_image(self.dir.as_deref(), url)
    }

    /// Whether the entry's title, description, or link targets hold every
    /// word of `query` (case-insensitive): the Changelog page's search, and
    /// the palette's over the same entries.
    pub fn matches(&self, query: &str) -> bool {
        let links: Vec<Link> = self.links.iter().map(|l| l.link.clone()).collect();
        changelog::matches_query(&self.title, &self.description, &links, query)
    }
}

/// One read of a project's changelog. Serializable for the remote window's
/// query service (`crate::wire` pins the JSON shape).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
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
    /// The project's releases (the git tags its `releases:` glob matches),
    /// newest first. Empty for a project with no `releases:` block.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub releases: Vec<Release>,
    /// Why the releases couldn't be read: a malformed `releases:` block, or
    /// a repository git couldn't list. The entries still show.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub releases_error: Option<String>,
}

/// Read `project`'s changelog with its links resolved. Touches disk (the
/// entries, the project config, the repository's git config), so a client
/// runs it off its UI thread, e.g. on a [`crate::refresh::spawn_worker`].
pub fn read_changelog(project: &str) -> ChangelogData {
    // Stamp first: an entry landing mid-read then shows as a change next tick.
    let stamp = changelog::changelog_stamp(project);
    let config = shelbi_state::load_project(project).ok();
    let repo = config.as_ref().and_then(changelog::project_github_repo_of);
    // The tags are cached per repository, so a re-read with no new tag runs
    // no git.
    let releases = config
        .as_ref()
        .map(changelog::read_releases_of)
        .unwrap_or_default();
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
            releases: releases.releases,
            releases_error: releases.error,
        },
        Err(e) => ChangelogData {
            project: project.to_string(),
            error: Some(e.to_string()),
            stamp,
            ..ChangelogData::default()
        },
    }
}

/// How a release reads on its marker: its label and the day it was tagged,
/// `v0.14.0 · Nov 19`.
pub fn release_caption(release: &Release) -> String {
    format!("{} \u{b7} {}", release.label, release.date.format("%b %-d"))
}

/// One shown entry on the timeline, with the releases drawn just above it,
/// newest first: those made no earlier than this entry and before the
/// entry above it (for the newest entry, every release since it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineEntry<'a> {
    pub entry: &'a ChangelogEntryView,
    pub releases: &'a [Release],
}

/// Where each release goes among entries dated `entries` (newest first):
/// for each entry, the range of `releases` (newest first) drawn above it.
/// A release sits above the newest entry no newer than it, so every entry
/// made after a release sits above the release's marker (an entry made at
/// the release's very moment sits under it). A release older than every
/// entry has no place and is left out: the timeline doesn't run on past its
/// last entry.
pub fn place_releases(
    entries: &[DateTime<FixedOffset>],
    releases: &[DateTime<FixedOffset>],
) -> Vec<std::ops::Range<usize>> {
    let mut next = 0;
    entries
        .iter()
        .map(|date| {
            let start = next;
            while next < releases.len() && releases[next] >= *date {
                next += 1;
            }
            start..next
        })
        .collect()
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

    /// The shown entries, newest first, each with the releases drawn above
    /// it ([`place_releases`]). Releases older than the last shown entry
    /// wait for the page that reaches them.
    pub fn timeline(&self) -> Vec<TimelineEntry<'_>> {
        let releases = self.data.as_ref().map_or(&[][..], |d| &d.releases[..]);
        let entries: Vec<&ChangelogEntryView> = self.visible().collect();
        let dates: Vec<DateTime<FixedOffset>> = entries.iter().map(|e| e.date).collect();
        let release_dates: Vec<DateTime<FixedOffset>> = releases.iter().map(|r| r.date).collect();
        place_releases(&dates, &release_dates)
            .into_iter()
            .zip(entries)
            .map(|(range, entry)| TimelineEntry {
                entry,
                releases: &releases[range],
            })
            .collect()
    }

    /// Why the releases couldn't be read, if they couldn't (the page says
    /// so over its entries).
    pub fn releases_error(&self) -> Option<&str> {
        self.data.as_ref()?.releases_error.as_deref()
    }
}

/// [`ChangelogEntryView::image_source`] over a changelog directory (`None`
/// where it can't be read from here), for a renderer that keeps only the
/// directory.
pub fn resolve_image(dir: Option<&Path>, url: &str) -> ImageSource {
    let url = url.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        return ImageSource::Web(url.to_string());
    }
    let Some(dir) = dir else {
        return ImageSource::Unavailable;
    };
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let decoded = percent_decode(raw);
    let path = Path::new(&decoded);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    };
    if path.is_file() {
        ImageSource::File(path)
    } else {
        ImageSource::Missing(path)
    }
}

/// `%20`-style escapes decoded (a Markdown image path with a space in it).
/// Anything that doesn't decode to UTF-8 is kept as written.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// `markdown` with every image (`![alt](url)`, outside code) as a text
/// placeholder, `[image: alt]` (the file name when the alt text is empty):
/// how a client that can't draw images (the TUI) shows where one sits.
pub fn image_placeholders(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut in_fence = false;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence || !line.contains("![") {
            out.push_str(line);
            continue;
        }
        // Outside code spans: the even pieces between backticks.
        for (i, piece) in line.split('`').enumerate() {
            if i > 0 {
                out.push('`');
            }
            if i % 2 == 1 {
                out.push_str(piece);
            } else {
                replace_images(piece, &mut out);
            }
        }
    }
    out
}

fn replace_images(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        let Some((alt, url, len)) = parse_image(&rest[start..]) else {
            out.push_str(&rest[..start + 2]);
            rest = &rest[start + 2..];
            continue;
        };
        out.push_str(&rest[..start]);
        let label = if alt.trim().is_empty() {
            url.rsplit('/').next().unwrap_or(url).to_string()
        } else {
            alt.replace("\\[", "[").replace("\\]", "]")
        };
        out.push_str(&format!("[image: {}]", label.trim()));
        rest = &rest[start + len..];
    }
    out.push_str(rest);
}

/// `![alt](url "title")` at the start of `s`: the alt, the url, and the
/// bytes it takes.
fn parse_image(s: &str) -> Option<(&str, &str, usize)> {
    let body = s.strip_prefix("![")?;
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut depth = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' if depth == 0 => break,
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    let alt = body.get(..i)?;
    let after = body.get(i + 1..)?.strip_prefix('(')?;
    let close = after.find(')')?;
    let dest = after[..close].trim();
    let url = match dest.strip_prefix('<') {
        Some(d) => d.split('>').next().unwrap_or(d),
        None => dest.split_whitespace().next().unwrap_or(""),
    };
    Some((alt, url, 2 + i + 1 + 1 + close + 1))
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
            attachments: vec![],
            dir: None,
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

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn dated(id: &str, date: &str) -> ChangelogEntryView {
        let mut e = view(id, id, "", vec![]);
        e.date = at(date);
        e
    }

    fn release(tag: &str, date: &str) -> Release {
        Release {
            tag: tag.into(),
            label: tag.into(),
            date: at(date),
        }
    }

    #[test]
    fn releases_sit_between_the_entries_before_and_after_them() {
        let entries = [
            at("2026-11-20T10:00:00-08:00"),
            at("2026-11-19T12:00:00-08:00"),
            at("2026-11-18T09:00:00-08:00"),
            at("2026-11-16T09:00:00-08:00"),
        ];
        let releases = [
            // Newer than every entry: over the newest.
            at("2026-11-20T11:00:00-08:00"),
            // Between the 20th and 19th entries: over the 19th.
            at("2026-11-19T15:30:00-08:00"),
            // At the 18th entry's very moment: over it.
            at("2026-11-18T09:00:00-08:00"),
            // Two between the 18th and the 16th, newest first.
            at("2026-11-17T09:00:00Z"),
            at("2026-11-16T20:00:00Z"),
            // Older than every entry: nowhere.
            at("2026-11-01T00:00:00Z"),
        ];
        assert_eq!(place_releases(&entries, &releases), [0..1, 1..2, 2..3, 3..5]);
        // Offsets compare as instants: 17:00Z is 09:00-08:00.
        assert_eq!(
            place_releases(&[at("2026-11-16T09:00:00-08:00")], &[at("2026-11-16T17:00:00Z")]),
            vec![std::ops::Range { start: 0, end: 1 }]
        );
        assert_eq!(place_releases(&entries, &[]), [0..0, 0..0, 0..0, 0..0]);
        assert!(place_releases(&[], &releases).is_empty());
    }

    #[test]
    fn the_timeline_follows_search_and_paging() {
        let mut m = ChangelogModel::new();
        let mut entries = vec![
            dated("tracker", "2026-11-20T10:00:00-08:00"),
            dated("labels", "2026-11-19T09:00:00-08:00"),
        ];
        // A full page of older entries, so the oldest release waits for the
        // second page.
        entries.extend((0..PAGE_SIZE).map(|i| dated(&format!("old{i}"), "2026-11-10T00:00:00Z")));
        entries.push(dated("first", "2026-10-01T00:00:00Z"));
        let mut d = data(entries);
        d.releases = vec![
            release("v0.14.0", "2026-11-19T15:30:00-08:00"),
            release("v0.13.3", "2026-11-16T12:00:00-08:00"),
            release("v0.1.0", "2026-10-02T00:00:00Z"),
        ];
        m.set_data(d);
        let rows = |m: &ChangelogModel| -> Vec<(String, Vec<String>)> {
            m.timeline()
                .into_iter()
                .filter(|t| !t.releases.is_empty())
                .map(|t| {
                    (
                        t.entry.id.clone(),
                        t.releases.iter().map(|r| r.tag.clone()).collect(),
                    )
                })
                .collect()
        };
        assert_eq!(m.timeline().len(), PAGE_SIZE);
        assert_eq!(
            rows(&m),
            [
                ("labels".to_string(), vec!["v0.14.0".to_string()]),
                ("old0".to_string(), vec!["v0.13.3".to_string()]),
            ]
        );
        m.load_more();
        assert_eq!(rows(&m).last().unwrap(), &("first".to_string(), vec!["v0.1.0".to_string()]));
        // A search keeps each release between the matches around it.
        m.set_query("tracker labels");
        assert!(m.timeline().is_empty());
        m.set_query("labels");
        assert_eq!(rows(&m), [("labels".to_string(), vec!["v0.14.0".to_string()])]);
        assert_eq!(m.releases_error(), None);
        assert_eq!(release_caption(&release("v0.14.0", "2026-11-19T15:30:00-08:00")), "v0.14.0 \u{b7} Nov 19");
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
    fn images_become_placeholders_outside_code() {
        assert_eq!(
            image_placeholders("Before.\n\n![Jobs sheet after](assets/e/jobs.png)\n"),
            "Before.\n\n[image: Jobs sheet after]\n"
        );
        // No alt: the file name. Escaped brackets read plainly; a title is
        // dropped with the destination.
        assert_eq!(
            image_placeholders("![](assets/e/a.png) and ![x \\[1\\]](<b c.png> \"t\")"),
            "[image: a.png] and [image: x [1]]"
        );
        // Code is left as written, and so is a `![` that isn't an image.
        let code = "`![a](b.png)`\n```\n![a](b.png)\n```\n![not an image\n";
        assert_eq!(image_placeholders(code), code);
    }

    #[test]
    fn image_sources_resolve_against_the_changelog_dir() {
        let dir = std::env::temp_dir().join(format!("shelbi-app-cl-img-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets/e")).unwrap();
        std::fs::write(dir.join("assets/e/a b.png"), b"x").unwrap();
        let mut entry = view("e", "t", "", vec![]);
        entry.dir = Some(dir.clone());
        assert_eq!(
            entry.image_source("assets/e/a%20b.png"),
            ImageSource::File(dir.join("assets/e/a b.png"))
        );
        let abs = dir.join("assets/e/a b.png");
        assert_eq!(
            entry.image_source(&format!("file://{}", abs.display())),
            ImageSource::File(abs.clone())
        );
        assert_eq!(
            entry.image_source("assets/e/gone.png"),
            ImageSource::Missing(dir.join("assets/e/gone.png"))
        );
        assert_eq!(
            entry.image_source("https://x.dev/a.png"),
            ImageSource::Web("https://x.dev/a.png".into())
        );
        // No directory here (a remote window): the alt text, no error.
        entry.dir = None;
        assert_eq!(entry.image_source("assets/e/a%20b.png"), ImageSource::Unavailable);
        let _ = std::fs::remove_dir_all(&dir);
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

        let added = changelog::add_entry(
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
        // Attach an image and a file: the image shows inline, the file is
        // listed, and both resolve on this machine.
        let src = home.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(src.join("bench.csv"), "a,b\n").unwrap();
        let files = changelog::prepare_attachments(&[
            changelog::NewAttachment {
                source: src.join("shot.png"),
                kind: changelog::AttachmentKind::Image,
                alt: Some("Sidebar".into()),
            },
            changelog::NewAttachment {
                source: src.join("bench.csv"),
                kind: changelog::AttachmentKind::File,
                alt: None,
            },
        ])
        .unwrap();
        changelog::attach_files(&added, &files).unwrap();
        let read = read_changelog("p");
        assert_eq!(read.entries.len(), 1);
        let e = &read.entries[0];
        assert_eq!(e.title, "Changelog lands");
        let listed: Vec<&str> = e.listed_attachments().map(|a| a.name.as_str()).collect();
        assert_eq!(listed, ["bench.csv"]);
        assert_eq!(e.attachments[1].size_label(), "4 B");
        let dir = changelog::changelog_dir("p").unwrap();
        assert_eq!(
            e.attachment_file(&e.attachments[1]),
            Some(dir.join(format!("assets/{}/bench.csv", e.id)))
        );
        assert_eq!(
            e.image_source(&e.attachments[0].path),
            ImageSource::File(dir.join(format!("assets/{}/shot.png", e.id)))
        );
        // Over the wire the directory stays behind: a remote window's entry
        // keeps its attachments but resolves none of them.
        let far: ChangelogEntryView =
            serde_json::from_str(&serde_json::to_string(e).unwrap()).unwrap();
        assert_eq!(far.attachments, e.attachments);
        assert_eq!(far.dir, None);
        assert_eq!(far.attachment_file(&far.attachments[1]), None);
        assert_eq!(far.image_source(&far.attachments[0].path), ImageSource::Unavailable);
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
