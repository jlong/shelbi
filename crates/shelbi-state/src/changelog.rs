//! The per-project changelog: a blog-style record of what has shipped.
//!
//! Each entry is one markdown file with YAML frontmatter under the project's
//! *state* half, `~/.shelbi/projects/<name>/changelog/` (never `.claude/`, and
//! never the repo, whatever the project's config mode). The frontmatter carries
//! the title, the date (with time and offset), the author (`orchestrator`,
//! `user`, or an agent's name) and the structured links; the body is the
//! description, in markdown:
//!
//! ```text
//! ---
//! title: Changelog lands in the sidebar
//! date: 2026-10-28T14:30:05-07:00
//! author: orchestrator
//! issues: [changelog]
//! prs: [1953]
//! links: [https://shelbi.dev/docs/cli/changelog]
//! ---
//! The sidebar's Activity row becomes Changelog. …
//! ```
//!
//! Files are named `<YYYY-MM-DD>-<HHMMSS>-<slug>.md` from the entry's UTC date,
//! so a directory listing sorts by date; the file stem is the entry's id.
//! Writers never share a file: [`add_entry`] claims a fresh name with an
//! exclusive create (adding `-2`, `-3`, … on a collision), so the orchestrator
//! and a CLI user adding at the same moment both land.
//!
//! ## Links
//!
//! A link is a Shelbi issue, a pull request, or any other URL ([`Link`]). They
//! come from the frontmatter (the CLI's `--issue`, `--pr` and `--link` flags)
//! and from the description, where the syntax is:
//!
//! - `issue:<id>` — a Shelbi issue by id (`issue:tui-about`), so a client can
//!   open it inside Shelbi;
//! - `#<number>` — a pull request in the project's GitHub repository (`#1942`);
//! - any `http://` or `https://` URL; a GitHub pull-request URL counts as a PR.
//!
//! Code spans and fenced code blocks are skipped. [`ChangelogEntry::links`]
//! merges both sources, de-duplicated, with a PR number resolved to its URL
//! against the project's repository ([`project_github_repo`]).
//!
//! ## Template
//!
//! A new entry's description is the project's `changelog-template.md` (in the
//! config half, beside `pr-template.md`) filled with [`fill_template`]. The file
//! is user-owned: it ships with every new project and is materialized for older
//! ones on boot, never overwritten.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{
    DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc,
};
use serde::{Deserialize, Serialize};
use shelbi_core::{Error, Result};

use crate::{atomic_write, config_project_dir, ensure_dir, expand_tilde_str, project_dir};

/// The directory under a project's state root holding one file per entry.
pub const CHANGELOG_DIR: &str = "changelog";

/// File name of the per-project changelog template.
pub const CHANGELOG_TEMPLATE_FILE: &str = "changelog-template.md";

/// The shipped `changelog-template.md`.
pub const DEFAULT_CHANGELOG_TEMPLATE: &str = include_str!("default_changelog_template.md.template");

/// The author an entry gets when none is named.
pub const DEFAULT_AUTHOR: &str = "user";

/// `~/.shelbi/projects/<name>/changelog/`: always the state half, so an in-repo
/// project's changelog stays out of its repository too.
pub fn changelog_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join(CHANGELOG_DIR))
}

/// `<config_root>/changelog-template.md`, config-mode-aware like
/// [`crate::pr_template_path`].
pub fn changelog_template_path(project: &str) -> Result<PathBuf> {
    Ok(config_project_dir(project)?.join(CHANGELOG_TEMPLATE_FILE))
}

/// Outcome of seeding `changelog-template.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangelogTemplateOutcome {
    /// The file was missing and has just been written from the default.
    Created,
    /// The file was already there and was left alone.
    Unchanged,
}

/// Write the default `changelog-template.md` when absent, leaving an existing
/// one (customized or not) byte-for-byte. Run by `shelbi init`, project
/// creation, and `shelbi reload`; the config-upgrade pass does the same for
/// projects that predate the file.
pub fn scaffold_changelog_template(project: &str) -> Result<ChangelogTemplateOutcome> {
    let path = changelog_template_path(project)?;
    if path.exists() {
        return Ok(ChangelogTemplateOutcome::Unchanged);
    }
    atomic_write(&path, DEFAULT_CHANGELOG_TEMPLATE.as_bytes())?;
    Ok(ChangelogTemplateOutcome::Created)
}

/// The project's template, or the shipped default when the file is missing or
/// unreadable (so `add` never fails on a template problem).
pub fn read_changelog_template(project: &str) -> String {
    changelog_template_path(project)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_else(|| DEFAULT_CHANGELOG_TEMPLATE.to_string())
}

// ---------------------------------------------------------------------------
// Links

/// A pull request as written in the frontmatter: a number in the project's
/// repository, or a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrRef {
    Number(u64),
    Url(String),
}

impl PrRef {
    /// Parse `1942`, `#1942`, or a URL. `None` for anything else.
    pub fn parse(s: &str) -> Option<PrRef> {
        let s = s.trim();
        let digits = s.strip_prefix('#').unwrap_or(s);
        if let Ok(n) = digits.parse::<u64>() {
            return Some(PrRef::Number(n));
        }
        is_url(s).then(|| PrRef::Url(s.to_string()))
    }

    /// Normalize a value read from hand-edited frontmatter (`"#12"` as a
    /// string) the way [`PrRef::parse`] would.
    fn normalized(&self) -> Option<PrRef> {
        match self {
            PrRef::Number(_) => Some(self.clone()),
            PrRef::Url(s) => PrRef::parse(s),
        }
    }

    fn to_link(&self, repo: Option<&str>) -> Link {
        match self {
            PrRef::Number(n) => Link::Pr {
                number: Some(*n),
                url: repo.map(|r| shelbi_core::github::pull_request_url(r, *n)),
            },
            PrRef::Url(url) => Link::Pr {
                number: shelbi_core::github::pull_request_number(url),
                url: Some(url.clone()),
            },
        }
    }
}

/// One link on an entry, resolved. Serialized (for `--format json`) with a
/// `kind` tag: `{"kind":"issue","id":…}`, `{"kind":"pr","number":…,"url":…}`
/// (either may be null: a bare `#12` with no known repository has no URL, a
/// non-GitHub PR URL no number), `{"kind":"url","url":…}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Link {
    /// A Shelbi issue, by id.
    Issue { id: String },
    /// A pull request.
    Pr {
        number: Option<u64>,
        url: Option<String>,
    },
    /// Any other URL.
    Url { url: String },
}

impl Link {
    /// How the link reads in a list: `issue:<id>`, `#<n>` (or the URL of a PR
    /// with no number), or the URL.
    pub fn label(&self) -> String {
        match self {
            Link::Issue { id } => format!("issue:{id}"),
            Link::Pr {
                number: Some(n), ..
            } => format!("#{n}"),
            Link::Pr { number: None, url } => url.clone().unwrap_or_default(),
            Link::Url { url } => url.clone(),
        }
    }

    /// Where the link goes outside Shelbi: the PR or page URL. `None` for an
    /// issue (opened inside Shelbi) and an unresolved PR number.
    pub fn url(&self) -> Option<&str> {
        match self {
            Link::Issue { .. } => None,
            Link::Pr { url, .. } => url.as_deref(),
            Link::Url { url } => Some(url),
        }
    }

    /// The identity two links are the same link by.
    fn key(&self) -> String {
        match self {
            Link::Issue { id } => format!("issue:{id}"),
            Link::Pr { url: Some(url), .. } => format!("url:{url}"),
            Link::Pr { number, url: None } => format!("pr:{}", number.unwrap_or_default()),
            Link::Url { url } => format!("url:{url}"),
        }
    }

    /// The text search runs over: the label plus any URL.
    fn search_text(&self) -> String {
        match self.url() {
            Some(url) => format!("{} {url}", self.label()),
            None => self.label(),
        }
    }
}

fn is_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// Classify a URL: a GitHub PR URL is a PR, anything else a plain URL.
fn url_link(url: &str) -> Link {
    match shelbi_core::github::pull_request_number(url) {
        Some(n) => Link::Pr {
            number: Some(n),
            url: Some(url.to_string()),
        },
        None => Link::Url {
            url: url.to_string(),
        },
    }
}

/// The links written in a markdown `body`, in order of appearance, resolved
/// against `repo` and de-duplicated. See the module docs for the syntax.
pub fn parse_body_links(body: &str, repo: Option<&str>) -> Vec<Link> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        // Outside code spans: split on backticks and take the even pieces.
        for (i, piece) in line.split('`').enumerate() {
            if i % 2 == 0 {
                scan_text(piece, repo, &mut out);
            }
        }
    }
    dedup(out)
}

fn scan_text(text: &str, repo: Option<&str>, out: &mut Vec<Link>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let at_boundary = i == 0 || is_boundary(bytes[i - 1]);
        let rest = &text[i..];
        if at_boundary && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let end = rest
                .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
                .unwrap_or(rest.len());
            let url = trim_url(&rest[..end]);
            if url.len() > "https://".len() {
                out.push(url_link(url));
            }
            i += end.max(1);
            continue;
        }
        if at_boundary && rest.starts_with("issue:") {
            let id = take_while(&rest[6..], |c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
            });
            let id = id.trim_end_matches(['-', '_']);
            if id.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
                out.push(Link::Issue { id: id.to_string() });
                i += 6 + id.len();
                continue;
            }
        }
        if at_boundary && bytes[i] == b'#' {
            let digits = take_while(&rest[1..], |c| c.is_ascii_digit());
            let next = rest[1 + digits.len()..].chars().next();
            let ends_word = next.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if !digits.is_empty() && ends_word {
                if let Ok(n) = digits.parse() {
                    out.push(PrRef::Number(n).to_link(repo));
                }
                i += 1 + digits.len();
                continue;
            }
        }
        // Advance one whole character.
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
}

/// A link can start at the beginning of text or after whitespace or opening
/// punctuation, never mid-word (`foo#12`, `path/issue:x`).
fn is_boundary(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'(' | b'[' | b'{' | b',' | b';' | b'*' | b'_' | b'>')
}

fn take_while(s: &str, f: impl Fn(char) -> bool) -> &str {
    let end = s.find(|c: char| !f(c)).unwrap_or(s.len());
    &s[..end]
}

/// Drop sentence punctuation and an unbalanced closing bracket from the end of
/// a URL found in prose (`see https://x.dev/a.` or `[x](https://x.dev/a)`).
fn trim_url(mut url: &str) -> &str {
    loop {
        let before = url;
        url = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
        for (open, close) in [('(', ')'), ('[', ']')] {
            if url.ends_with(close) && url.matches(close).count() > url.matches(open).count() {
                url = &url[..url.len() - 1];
            }
        }
        if url == before {
            return url;
        }
    }
}

fn dedup(links: Vec<Link>) -> Vec<Link> {
    let mut seen = HashSet::new();
    links.into_iter().filter(|l| seen.insert(l.key())).collect()
}

// ---------------------------------------------------------------------------
// Entries

/// The frontmatter as stored. `date` is read as text so a hand-edited bare date
/// still loads.
#[derive(Debug, Serialize, Deserialize)]
struct Front {
    title: String,
    date: String,
    #[serde(default = "default_author")]
    author: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    issues: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    prs: Vec<PrRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    links: Vec<String>,
}

fn default_author() -> String {
    DEFAULT_AUTHOR.to_string()
}

/// One changelog entry as read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    /// The file stem: `2026-10-28-213005-changelog-lands`.
    pub id: String,
    pub title: String,
    pub date: DateTime<FixedOffset>,
    pub author: String,
    /// Issue ids from the frontmatter.
    pub issues: Vec<String>,
    /// Pull requests from the frontmatter.
    pub prs: Vec<PrRef>,
    /// Other URLs from the frontmatter.
    pub urls: Vec<String>,
    /// The description, in markdown.
    pub body: String,
    pub path: PathBuf,
}

impl ChangelogEntry {
    /// Every link on the entry: the frontmatter's first, then the ones written
    /// in the description, de-duplicated, PR numbers resolved against `repo`.
    pub fn links(&self, repo: Option<&str>) -> Vec<Link> {
        let mut links: Vec<Link> = self
            .issues
            .iter()
            .map(|id| Link::Issue { id: id.clone() })
            .collect();
        links.extend(
            self.prs
                .iter()
                .filter_map(PrRef::normalized)
                .map(|p| p.to_link(repo)),
        );
        links.extend(self.urls.iter().map(|u| url_link(u)));
        links.extend(parse_body_links(&self.body, repo));
        dedup(links)
    }

    /// The links the description doesn't already show: what a reader of the
    /// rendered body would otherwise miss.
    pub fn extra_links(&self, repo: Option<&str>) -> Vec<Link> {
        let in_body: HashSet<String> = parse_body_links(&self.body, repo)
            .iter()
            .map(Link::key)
            .collect();
        self.links(repo)
            .into_iter()
            .filter(|l| !in_body.contains(&l.key()))
            .collect()
    }

    /// Whether the entry matches `query`: every whitespace-separated term must
    /// appear, case-insensitively, in the title, the description, or a link
    /// target. An empty query matches everything.
    pub fn matches(&self, query: &str, repo: Option<&str>) -> bool {
        matches_query(&self.title, &self.body, &self.links(repo), query)
    }
}

/// The search behind [`ChangelogEntry::matches`], over already-resolved
/// `links`, so a client holding resolved entries searches the same way.
pub fn matches_query(title: &str, body: &str, links: &[Link], query: &str) -> bool {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return true;
    }
    let mut hay = format!("{title}\n{body}");
    for link in links {
        hay.push('\n');
        hay.push_str(&link.search_text());
    }
    let hay = hay.to_lowercase();
    terms.iter().all(|t| hay.contains(t.as_str()))
}

/// Parse one entry file's text. `id` and `path` are where it was read from.
pub fn parse_entry(id: &str, path: &Path, text: &str) -> Result<ChangelogEntry> {
    let (front, body) =
        crate::split_frontmatter(text).ok_or_else(|| Error::Other("missing frontmatter".into()))?;
    let front: Front = serde_yaml::from_str(front)?;
    let date = parse_date(&front.date, BareDate::StartOfDay)
        .ok_or_else(|| Error::Other(format!("unreadable date `{}`", front.date)))?;
    Ok(ChangelogEntry {
        id: id.to_string(),
        title: front.title,
        date,
        author: front.author,
        issues: front.issues,
        prs: front.prs,
        urls: front.links,
        body: body.trim_end().to_string(),
        path: path.to_path_buf(),
    })
}

/// What a read of the changelog found: the entries, newest first, and the files
/// that couldn't be parsed (path and why), which are skipped.
#[derive(Debug, Clone, Default)]
pub struct ChangelogRead {
    pub entries: Vec<ChangelogEntry>,
    pub skipped: Vec<(PathBuf, String)>,
}

/// Read every entry under `project`'s changelog, newest first (by date, then
/// id). A missing directory is an empty changelog.
pub fn read_entries(project: &str) -> Result<ChangelogRead> {
    read_entries_in(&changelog_dir(project)?)
}

/// [`read_entries`] over an explicit directory.
pub fn read_entries_in(dir: &Path) -> Result<ChangelogRead> {
    let mut read = ChangelogRead::default();
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(read),
        Err(e) => return Err(crate::annotate_io_error(dir, e).into()),
    };
    for path in rd.flatten().map(|e| e.path()) {
        let Some(id) = entry_id(&path) else { continue };
        match fs::read_to_string(&path)
            .map_err(Error::from)
            .and_then(|text| parse_entry(&id, &path, &text))
        {
            Ok(entry) => read.entries.push(entry),
            Err(e) => read.skipped.push((path, e.to_string())),
        }
    }
    read.entries
        .sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
    Ok(read)
}

/// The entry id of a changelog file (its stem), or `None` for anything that
/// isn't one: another extension, a dotfile, or an in-flight temp file.
fn entry_id(path: &Path) -> Option<String> {
    if path.extension()? != "md" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    (!stem.starts_with('.')).then(|| stem.to_string())
}

/// Find one entry by id: an exact id, else a unique prefix of one. A trailing
/// `.md` is ignored.
pub fn find_entry(project: &str, id: &str) -> Result<ChangelogEntry> {
    let read = read_entries(project)?;
    find_in(read.entries, id)
}

fn find_in(entries: Vec<ChangelogEntry>, id: &str) -> Result<ChangelogEntry> {
    let id = id.trim().trim_end_matches(".md");
    if id.is_empty() {
        return Err(Error::Other("no changelog entry id given".into()));
    }
    let mut prefixed = Vec::new();
    for entry in entries {
        if entry.id == id {
            return Ok(entry);
        }
        if entry.id.starts_with(id) {
            prefixed.push(entry);
        }
    }
    match prefixed.len() {
        1 => Ok(prefixed.remove(0)),
        0 => Err(Error::Other(format!("no changelog entry `{id}`"))),
        _ => Err(Error::Other(format!(
            "`{id}` matches {} changelog entries: {}",
            prefixed.len(),
            prefixed
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// A new entry, before it has a file.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub title: String,
    pub date: DateTime<FixedOffset>,
    pub author: String,
    pub issues: Vec<String>,
    pub prs: Vec<PrRef>,
    pub urls: Vec<String>,
    /// The description as given; the template wraps it.
    pub description: String,
}

/// Add an entry to `project`'s changelog: fill the project's template with it
/// and write a new file. Never overwrites another entry.
pub fn add_entry(project: &str, new: NewEntry) -> Result<ChangelogEntry> {
    let template = read_changelog_template(project);
    let repo = project_github_repo(project);
    add_entry_in(&changelog_dir(project)?, &template, repo.as_deref(), new)
}

/// [`add_entry`] over an explicit directory and template.
pub fn add_entry_in(
    dir: &Path,
    template: &str,
    repo: Option<&str>,
    new: NewEntry,
) -> Result<ChangelogEntry> {
    let title = new.title.trim().to_string();
    if title.is_empty() || title.contains('\n') {
        return Err(Error::Other(
            "a changelog entry needs a one-line title".into(),
        ));
    }
    let author = new.author.trim().to_string();
    if author.is_empty() || author.contains('\n') {
        return Err(Error::Other(
            "a changelog entry's author must be one line".into(),
        ));
    }
    let date = new.date.with_nanosecond(0).unwrap_or(new.date);
    let flag_links: Vec<Link> = new
        .issues
        .iter()
        .map(|id| Link::Issue { id: id.clone() })
        .chain(new.prs.iter().map(|p| p.to_link(repo)))
        .chain(new.urls.iter().map(|u| url_link(u)))
        .collect();
    let body = fill_template(
        template,
        &TemplateValues {
            title: &title,
            date,
            author: &author,
            description: &new.description,
            links: &flag_links,
        },
    );
    let front = Front {
        title,
        date: date.to_rfc3339(),
        author,
        issues: new.issues,
        prs: new.prs,
        links: new.urls,
    };
    let text = crate::render_frontmatter(&front, &body)?;
    let path = write_new_file(dir, &file_stem(date, &front.title), &text)?;
    let id = entry_id(&path).unwrap_or_default();
    parse_entry(&id, &path, &text)
}

/// `<YYYY-MM-DD>-<HHMMSS>-<slug>` from the UTC date, so names sort by date.
fn file_stem(date: DateTime<FixedOffset>, title: &str) -> String {
    format!(
        "{}-{}",
        date.with_timezone(&Utc).format("%Y-%m-%d-%H%M%S"),
        slug(title)
    )
}

/// A filename-safe slug of `title`: lowercase ASCII words joined by `-`, at
/// most 48 characters, `entry` when nothing survives.
fn slug(title: &str) -> String {
    let mut out = String::new();
    for word in title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if out.len() + word.len() + 1 > 48 {
            break;
        }
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&word.to_ascii_lowercase());
    }
    if out.is_empty() {
        "entry".to_string()
    } else {
        out
    }
}

/// Write `text` to a new file `<stem>.md` in `dir` (or `<stem>-2.md`, … when
/// taken) without ever replacing an existing file. The bytes go to a temp file
/// first and are hard-linked into place, so a reader never sees half an entry
/// and two writers can't claim the same name.
fn write_new_file(dir: &Path, stem: &str, text: &str) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    ensure_dir(dir)?;
    let tmp = dir.join(format!(
        ".{stem}.tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| crate::annotate_io_error(&tmp, e))?;
        f.write_all(text.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| crate::annotate_io_error(&tmp, e))?;
    }
    let result = (|| {
        for n in 1..1000 {
            let name = if n == 1 {
                format!("{stem}.md")
            } else {
                format!("{stem}-{n}.md")
            };
            let path = dir.join(name);
            match fs::hard_link(&tmp, &path) {
                Ok(()) => return Ok(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                // A filesystem without hard links: claim the name with an
                // exclusive create instead.
                Err(_) => match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut f) => {
                        f.write_all(text.as_bytes())
                            .and_then(|_| f.sync_all())
                            .map_err(|e| crate::annotate_io_error(&path, e))?;
                        return Ok(path);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(crate::annotate_io_error(&path, e).into()),
                },
            }
        }
        Err(Error::Other(format!(
            "no free changelog file name for {stem}"
        )))
    })();
    let _ = fs::remove_file(&tmp);
    result
}

// ---------------------------------------------------------------------------
// Template

/// The values a template is filled with.
#[derive(Debug, Clone)]
pub struct TemplateValues<'a> {
    pub title: &'a str,
    pub date: DateTime<FixedOffset>,
    pub author: &'a str,
    pub description: &'a str,
    pub links: &'a [Link],
}

/// Fill `template`: drop its leading `<!-- … -->` comment, replace the
/// `{{title}}`, `{{date}}`, `{{time}}`, `{{author}}`, `{{description}}` and
/// `{{links}}` placeholders, and tidy the blank lines an empty value leaves.
/// Unknown placeholders are left as written.
pub fn fill_template(template: &str, values: &TemplateValues<'_>) -> String {
    let template = strip_leading_comment(template);
    // One list item per link, written so it parses back to the same link: a
    // PR as its URL when known (a bare `#12` would resolve against this
    // project's repository even for another repository's PR).
    let links = values
        .links
        .iter()
        .map(|l| match l {
            Link::Pr { url: Some(url), .. } => format!("- {url}"),
            _ => format!("- {}", l.label()),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let params: BTreeMap<String, String> = [
        ("title", values.title.to_string()),
        ("date", values.date.format("%Y-%m-%d").to_string()),
        ("time", values.date.format("%H:%M").to_string()),
        ("author", values.author.to_string()),
        ("description", values.description.trim().to_string()),
        ("links", links),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let mut missing = Vec::new();
    let filled = shelbi_core::substitute_placeholders(template, &params, &mut missing);
    tidy_blank_lines(&filled)
}

fn strip_leading_comment(template: &str) -> &str {
    let trimmed = template.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<!--") {
        if let Some(end) = rest.find("-->") {
            return &rest[end + 3..];
        }
    }
    template
}

/// Trim trailing spaces, collapse runs of blank lines to one, and trim the
/// ends.
fn tidy_blank_lines(text: &str) -> String {
    let mut out = String::new();
    let mut blank_run = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank_run > 0 { "\n\n" } else { "\n" });
        }
        blank_run = 0;
        out.push_str(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Dates

/// What a bare date (no time) means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareDate {
    /// Midnight local time, the start of that day (`--since`, and a stored
    /// date with no time).
    StartOfDay,
    /// That day at the current local time of day (`add --date`), so an entry
    /// backdated to today still sorts after the ones already added.
    NowTimeOfDay,
}

/// Parse a date as the CLI takes it: RFC 3339 (`2026-10-28T14:30:00-07:00`,
/// `…Z`), a local date and time (`2026-10-28 14:30`, `2026-10-28T14:30:05`), or
/// a bare local date (`2026-10-28`, read per `bare`). `None` when unreadable.
pub fn parse_date(s: &str, bare: BareDate) -> Option<DateTime<FixedOffset>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d);
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
            return local(naive);
        }
    }
    let day = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let time = match bare {
        BareDate::StartOfDay => NaiveTime::MIN,
        BareDate::NowTimeOfDay => Local::now().time().with_nanosecond(0)?,
    };
    local(day.and_time(time))
}

fn local(naive: NaiveDateTime) -> Option<DateTime<FixedOffset>> {
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|d| d.fixed_offset())
}

/// Now, in local time, to the second.
pub fn now() -> DateTime<FixedOffset> {
    let now = Local::now().fixed_offset();
    now.with_nanosecond(0).unwrap_or(now)
}

// ---------------------------------------------------------------------------
// The project's repository and change detection

/// The `owner/repo` a `#123` resolves against: the project's GitHub issue
/// tracker when it has one, else its repository's `origin` remote when that
/// points at github.com. `None` when neither does.
pub fn project_github_repo(project: &str) -> Option<String> {
    let p = crate::load_project(project).ok()?;
    if let Some(gh) = p
        .issue_tracker
        .github
        .as_ref()
        .filter(|g| !g.repo.trim().is_empty())
    {
        return Some(gh.repo.trim().to_string());
    }
    let url = origin_url(&expand_tilde_str(&p.repo))?;
    shelbi_core::github::detect_github_repo(Some(&url))
}

/// The `origin` remote URL of the git repository at `repo`, read from its
/// config (following a worktree's `.git` file to the common dir). No `git`
/// process is run.
fn origin_url(repo: &Path) -> Option<String> {
    let dot_git = repo.join(".git");
    let git_dir = if dot_git.is_file() {
        let text = fs::read_to_string(&dot_git).ok()?;
        let dir = PathBuf::from(text.trim().strip_prefix("gitdir:")?.trim());
        let dir = if dir.is_relative() {
            repo.join(dir)
        } else {
            dir
        };
        match fs::read_to_string(dir.join("commondir")) {
            Ok(common) => dir.join(common.trim()),
            Err(_) => dir,
        }
    } else {
        dot_git
    };
    origin_from_config(&fs::read_to_string(git_dir.join("config")).ok()?)
}

fn origin_from_config(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == r#"[remote "origin"]"#;
            continue;
        }
        if in_origin {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "url" {
                    return Some(value.trim().to_string());
                }
            }
        }
    }
    None
}

/// A cheap signature of a changelog directory: how many entry files it holds
/// and the newest modification time among them and the directory. A client
/// polls it and re-reads the changelog when it changes (an entry added,
/// edited, or removed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangelogStamp {
    pub entries: usize,
    pub modified: Option<SystemTime>,
}

/// The [`ChangelogStamp`] of `project`'s changelog. A missing directory is the
/// default (empty) stamp.
pub fn changelog_stamp(project: &str) -> ChangelogStamp {
    changelog_dir(project)
        .map(|d| changelog_stamp_in(&d))
        .unwrap_or_default()
}

/// [`changelog_stamp`] over an explicit directory.
pub fn changelog_stamp_in(dir: &Path) -> ChangelogStamp {
    let mut stamp = ChangelogStamp {
        entries: 0,
        modified: fs::metadata(dir).and_then(|m| m.modified()).ok(),
    };
    let Ok(rd) = fs::read_dir(dir) else {
        return ChangelogStamp::default();
    };
    for entry in rd.flatten() {
        if entry_id(&entry.path()).is_none() {
            continue;
        }
        stamp.entries += 1;
        if let Ok(m) = entry.metadata().and_then(|m| m.modified()) {
            stamp.modified = stamp.modified.max(Some(m));
        }
    }
    stamp
}

#[cfg(test)]
mod tests;
