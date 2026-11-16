//! `shelbi changelog <subcommand>`: add to and read the project's changelog,
//! the blog-style record of what has shipped (`shelbi_state::changelog`).
//!
//! `add` fills the project's `changelog-template.md` and writes a new entry
//! file; `list`, `search` and `show` read them back as a compact text list,
//! blog-style markdown, or JSON for agents and scripts. The JSON shape is
//! versioned by `schema_version` and documented on the CLI docs page
//! (`site/content/docs/cli/changelog.mdx`). `strip-links` cleans up older
//! entries whose description ends with a list that only repeats their
//! frontmatter links. `add --image/--attach` and `attach` copy files into the
//! entry's `assets/<entry-id>/` directory and print where each one landed.

use std::io::{IsTerminal, Read};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::{Path, PathBuf};

use shelbi_state::changelog::{
    self, Attachment, AttachmentKind, BareDate, ChangelogEntry, Link, NewAttachment, NewEntry,
    PrRef, StoredAttachment,
};

use super::require_project;

/// The link syntax, shared by the command's and `add`'s long help.
pub const LINK_HELP: &str = "\
Links:
  An entry can link Shelbi issues, pull requests, and any other URL. Give them
  as flags, or write them in the description:

    --issue <id>       issue:<id>      a Shelbi issue by id (issue:tui-about)
    --pr <n|url>       #<n>            a pull request in the project's GitHub
                                       repository (#1942), or a PR URL
    --link <url>       http(s)://...   any other URL

  Code spans and fenced code blocks in the description are not scanned. A PR
  number resolves to a URL against the project's GitHub issue tracker, or its
  repository's origin remote when that is on github.com.";

/// The attachment flags, shared by `add`'s and `attach`'s long help.
pub const ATTACH_HELP: &str = "\
Attachments:
  --image <file>[:<alt>]  copy an image (png, jpg, gif, webp, svg) into the
                          entry and show it inline: `![alt](assets/...)` is
                          added at the end of the description. The alt text
                          (its caption) defaults to the file name.
  --attach <file>         copy any other file (a log, a PDF, a zip, a video);
                          it is listed under the entry.

  Files are copied, never moved, into the changelog's assets/<entry-id>/
  directory, at most 50 MB each. A name already taken there gets a number
  (shot.png, then shot-2.png, shot-3.png); each file's final path is printed.";

#[derive(Debug, Subcommand)]
pub enum ChangelogCmd {
    /// Add an entry, its description laid out by the project's
    /// changelog-template.md.
    #[command(after_long_help = format!("{LINK_HELP}\n\n{ATTACH_HELP}"))]
    Add(AddArgs),
    /// Add images and files to an existing entry. ENTRY_ID is the id `list`
    /// shows, or a unique prefix of one.
    #[command(after_long_help = ATTACH_HELP)]
    Attach {
        entry_id: String,
        #[command(flatten)]
        files: FileArgs,
        /// How to print what was attached.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// List entries, newest first.
    List {
        /// Show at most N entries.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Only entries on or after this date (YYYY-MM-DD, or a date and time).
        #[arg(long, value_name = "DATE")]
        since: Option<String>,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Find entries whose title, description, or link targets contain every
    /// word of QUERY (case-insensitive), newest first.
    Search {
        query: String,
        /// Show at most N entries.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Print one entry. ENTRY_ID is the id `list` shows, or a unique prefix of
    /// one.
    Show {
        entry_id: String,
        #[arg(long, value_enum, default_value_t = Format::Markdown)]
        format: Format,
    },
    /// Remove the list of links older entries end with, where it only repeats
    /// the entry's issue, PR and link fields (the links the Changelog page
    /// shows under every entry). Any other list is left alone.
    StripLinks {
        /// List the entries that would change without writing them.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Args)]
pub struct AddArgs {
    /// The entry's title (one line).
    #[arg(long)]
    title: String,
    /// When it shipped: YYYY-MM-DD (keeps the current time of day), a local
    /// date and time (2026-10-28 14:30), or RFC 3339. Defaults to now.
    #[arg(long, value_name = "DATE")]
    date: Option<String>,
    /// Who wrote it: `orchestrator`, `user`, or an agent's name. Defaults to
    /// $SHELBI_AGENT inside an agent's session, else `user`.
    #[arg(long)]
    author: Option<String>,
    /// Link a Shelbi issue by id. Repeatable.
    #[arg(long = "issue", value_name = "ID")]
    issues: Vec<String>,
    /// Link a pull request by number (1942 or #1942) or URL. Repeatable.
    #[arg(long = "pr", value_name = "N|URL")]
    prs: Vec<String>,
    /// Link any other URL. Repeatable.
    #[arg(long = "link", value_name = "URL")]
    links: Vec<String>,
    /// The description, in markdown. Read from stdin when omitted and stdin is
    /// not a terminal.
    #[arg(short = 'd', long = "description", value_name = "DESC")]
    description: Option<String>,
    #[command(flatten)]
    files: FileArgs,
    /// How to print the new entry.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

/// The files `add` and `attach` copy into an entry.
#[derive(Debug, Args, Default)]
pub struct FileArgs {
    /// An image to show inline, with optional alt text: `shot.png` or
    /// `shot.png:Jobs sheet after the change`. Repeatable.
    #[arg(long = "image", value_name = "FILE[:ALT]")]
    images: Vec<String>,
    /// Any other file to list under the entry. Repeatable.
    #[arg(long = "attach", value_name = "FILE")]
    attach: Vec<String>,
}

impl FileArgs {
    fn is_empty(&self) -> bool {
        self.images.is_empty() && self.attach.is_empty()
    }

    /// The files asked for, images first, each in the order given.
    fn new_attachments(&self) -> Result<Vec<NewAttachment>> {
        let mut out = Vec::new();
        for value in &self.images {
            let (source, alt) = split_image_arg(value, |p| p.is_file())?;
            out.push(NewAttachment {
                source,
                kind: AttachmentKind::Image,
                alt,
            });
        }
        for value in &self.attach {
            out.push(NewAttachment {
                source: PathBuf::from(value),
                kind: AttachmentKind::File,
                alt: None,
            });
        }
        Ok(out)
    }
}

/// Split `--image <file>[:<alt>]`: the whole value when it names a file (a
/// path may hold a colon), else the shortest prefix before a `:` that does,
/// the rest being the alt text.
fn split_image_arg(value: &str, exists: impl Fn(&Path) -> bool) -> Result<(PathBuf, Option<String>)> {
    if exists(Path::new(value)) {
        return Ok((PathBuf::from(value), None));
    }
    for (i, _) in value.match_indices(':') {
        let (path, alt) = (&value[..i], &value[i + 1..]);
        if !path.is_empty() && exists(Path::new(path)) {
            let alt = alt.trim();
            return Ok((PathBuf::from(path), (!alt.is_empty()).then(|| alt.to_string())));
        }
    }
    let path = value.split(':').next().unwrap_or(value);
    bail!("--image {value}: no such file `{path}`")
}

/// How entries print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// A compact list: date and title, then id, author and links.
    Text,
    /// Full entries, the way the blog reads.
    Markdown,
    /// Structured, for agents and scripts.
    Json,
}

/// The version of the `--format json` shape. Bump on any breaking change.
pub const JSON_SCHEMA_VERSION: u32 = 1;

pub fn run(project_opt: Option<String>, cmd: ChangelogCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    let repo = changelog::project_github_repo(&project);
    let repo = repo.as_deref();
    match cmd {
        ChangelogCmd::Add(args) => {
            let format = args.format;
            // Every file is checked before the entry is written, so a bad one
            // fails the command without leaving an entry behind.
            let files = changelog::prepare_attachments(&args.files.new_attachments()?)
                .map_err(|e| anyhow!(e))?;
            let new = new_entry(args, read_stdin_description)?;
            let entry = changelog::add_entry(&project, new).map_err(|e| anyhow!(e))?;
            let (entry, stored) = if files.is_empty() {
                (entry, Vec::new())
            } else {
                changelog::attach_files(&entry, &files).map_err(|e| {
                    anyhow!(
                        "added {} but attaching failed: {e}\n  retry with: shelbi changelog attach {} …",
                        entry.id,
                        entry.id
                    )
                })?
            };
            let out = match format {
                Format::Text => {
                    let mut out = format!("✓ added {}\n  {}\n", entry.id, entry.path.display());
                    out.push_str(&render_stored(&stored));
                    out
                }
                Format::Markdown => render_markdown(&entry, repo),
                Format::Json => render_json_one(&project, &entry, repo, Some(&stored))?,
            };
            print!("{out}");
        }
        ChangelogCmd::Attach {
            entry_id,
            files,
            format,
        } => {
            if files.is_empty() {
                bail!("nothing to attach: give --image <file> or --attach <file>");
            }
            let prepared =
                changelog::prepare_attachments(&files.new_attachments()?).map_err(|e| anyhow!(e))?;
            let (entry, stored) = changelog::attach_to_entry(&project, &entry_id, &prepared)
                .map_err(|e| anyhow!(e))?;
            let out = match format {
                Format::Text => {
                    let n = stored.len();
                    let mut out = format!(
                        "✓ attached {n} file{} to {}\n",
                        if n == 1 { "" } else { "s" },
                        entry.id
                    );
                    out.push_str(&render_stored(&stored));
                    out
                }
                Format::Markdown => render_markdown(&entry, repo),
                Format::Json => render_json_one(&project, &entry, repo, Some(&stored))?,
            };
            print!("{out}");
        }
        ChangelogCmd::List {
            limit,
            since,
            format,
        } => {
            let since = since
                .map(|s| {
                    changelog::parse_date(&s, BareDate::StartOfDay)
                        .ok_or_else(|| anyhow!("--since: unreadable date `{s}`"))
                })
                .transpose()?;
            let mut entries = read(&project)?;
            if let Some(since) = since {
                entries.retain(|e| e.date >= since);
            }
            entries.truncate(limit.unwrap_or(usize::MAX));
            print!("{}", render_many(&project, &entries, repo, format, None)?);
        }
        ChangelogCmd::Search {
            query,
            limit,
            format,
        } => {
            let mut entries = read(&project)?;
            entries.retain(|e| e.matches(&query, repo));
            entries.truncate(limit.unwrap_or(usize::MAX));
            print!(
                "{}",
                render_many(&project, &entries, repo, format, Some(&query))?
            );
        }
        ChangelogCmd::Show { entry_id, format } => {
            let entry = changelog::find_entry(&project, &entry_id).map_err(|e| anyhow!(e))?;
            let out = match format {
                Format::Text => render_text(&entry, repo),
                Format::Markdown => render_markdown(&entry, repo),
                Format::Json => render_json_one(&project, &entry, repo, None)?,
            };
            print!("{out}");
        }
        ChangelogCmd::StripLinks { dry_run } => {
            let entries = read(&project)?;
            let mut changed = 0;
            for entry in &entries {
                if changelog::strip_link_list(entry, repo, dry_run).map_err(|e| anyhow!(e))? {
                    changed += 1;
                    println!("{} {}", if dry_run { "would strip" } else { "✓ stripped" }, entry.id);
                }
            }
            let verb = if dry_run { "would change" } else { "changed" };
            println!("{changed} of {} entries {verb}", entries.len());
        }
    }
    Ok(())
}

/// The project's entries, newest first, warning on stderr about any file that
/// couldn't be read.
fn read(project: &str) -> Result<Vec<ChangelogEntry>> {
    let read = changelog::read_entries(project).map_err(|e| anyhow!(e))?;
    for (path, why) in &read.skipped {
        eprintln!("shelbi changelog: skipping {}: {why}", path.display());
    }
    Ok(read.entries)
}

/// The description from stdin, when stdin is piped.
fn read_stdin_description() -> Result<Option<String>> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut text = String::new();
    stdin.read_to_string(&mut text)?;
    Ok(Some(text))
}

/// Validate `add`'s arguments into a [`NewEntry`]. `stdin` supplies the
/// description when `-d` is absent.
fn new_entry(args: AddArgs, stdin: impl FnOnce() -> Result<Option<String>>) -> Result<NewEntry> {
    let date = match &args.date {
        Some(s) => changelog::parse_date(s, BareDate::NowTimeOfDay)
            .ok_or_else(|| anyhow!("--date: unreadable date `{s}` (try YYYY-MM-DD or RFC 3339)"))?,
        None => changelog::now(),
    };
    for id in &args.issues {
        shelbi_core::validate_task_id(id).map_err(|e| anyhow!("--issue {id}: {e}"))?;
    }
    let prs = args
        .prs
        .iter()
        .map(|p| PrRef::parse(p).ok_or_else(|| anyhow!("--pr {p}: expected a number or a URL")))
        .collect::<Result<Vec<_>>>()?;
    for url in &args.links {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            bail!("--link {url}: expected an http(s) URL");
        }
    }
    let description = match args.description {
        Some(d) => d,
        None => stdin()?.unwrap_or_default(),
    };
    let author = args
        .author
        .or_else(|| {
            std::env::var("SHELBI_AGENT")
                .ok()
                .filter(|a| !a.trim().is_empty())
        })
        .unwrap_or_else(|| changelog::DEFAULT_AUTHOR.to_string());
    Ok(NewEntry {
        title: args.title,
        date,
        author,
        issues: args.issues,
        prs,
        urls: args.links,
        description,
    })
}

// ---------------------------------------------------------------------------
// Rendering

/// One line per stored file: its absolute path, then the path the entry
/// uses, so a caller can reference either.
fn render_stored(stored: &[StoredAttachment]) -> String {
    stored
        .iter()
        .map(|s| {
            format!(
                "  {} {}  ({})\n",
                match s.attachment.kind {
                    AttachmentKind::Image => "image",
                    AttachmentKind::File => "file ",
                },
                s.absolute.display(),
                s.attachment.path
            )
        })
        .collect()
}

fn render_many(
    project: &str,
    entries: &[ChangelogEntry],
    repo: Option<&str>,
    format: Format,
    query: Option<&str>,
) -> Result<String> {
    Ok(match format {
        Format::Text if entries.is_empty() => match query {
            Some(q) => format!("No changelog entries match `{q}`.\n"),
            None => "No changelog entries yet.\n".to_string(),
        },
        Format::Text => entries.iter().map(|e| render_text(e, repo)).collect(),
        Format::Markdown => entries
            .iter()
            .map(|e| render_markdown(e, repo))
            .collect::<Vec<_>>()
            .join("\n"),
        Format::Json => {
            let doc = JsonList {
                schema_version: JSON_SCHEMA_VERSION,
                project,
                query,
                entries: entries.iter().map(|e| JsonEntry::new(e, repo)).collect(),
            };
            format!("{}\n", serde_json::to_string_pretty(&doc)?)
        }
    })
}

/// `2026-10-28 14:30  Title`, then the id, author and links indented below.
fn render_text(entry: &ChangelogEntry, repo: Option<&str>) -> String {
    let mut second = vec![entry.id.clone(), entry.author.clone()];
    second.extend(entry.links(repo).iter().map(Link::label));
    format!(
        "{}  {}\n                  {}\n",
        entry.date.format("%Y-%m-%d %H:%M"),
        entry.title,
        second.join("  ")
    )
}

/// One entry as a blog post: a heading, a byline, the description, and the
/// links the description doesn't already show.
fn render_markdown(entry: &ChangelogEntry, repo: Option<&str>) -> String {
    let mut out = format!(
        "## {}\n\n*{} · {} · `{}`*\n",
        entry.title,
        entry.date.format("%Y-%m-%d %H:%M"),
        entry.author,
        entry.id
    );
    if !entry.body.trim().is_empty() {
        out.push('\n');
        out.push_str(entry.body.trim_end());
        out.push('\n');
    }
    let extra = entry.extra_links(repo);
    if !extra.is_empty() {
        let links: Vec<String> = extra.iter().map(markdown_link).collect();
        out.push_str(&format!("\nLinks: {}\n", links.join(" · ")));
    }
    let listed = entry.listed_attachments();
    if !listed.is_empty() {
        out.push_str("\nAttachments:\n\n");
        for a in listed {
            out.push_str(&format!(
                "- [{}](<{}>) ({})\n",
                a.name,
                a.absolute(entry.dir()).display(),
                changelog::format_size(a.size)
            ));
        }
    }
    out
}

fn markdown_link(link: &Link) -> String {
    match link {
        Link::Issue { id } => format!("issue:{id}"),
        Link::Pr { url: Some(url), .. } => format!("[{}]({url})", link.label()),
        Link::Pr { url: None, .. } => link.label(),
        Link::Url { url } => format!("<{url}>"),
    }
}

fn render_json_one(
    project: &str,
    entry: &ChangelogEntry,
    repo: Option<&str>,
    added: Option<&[StoredAttachment]>,
) -> Result<String> {
    let doc = JsonOne {
        schema_version: JSON_SCHEMA_VERSION,
        project,
        entry: JsonEntry::new(entry, repo),
        added: added.map(|stored| {
            stored
                .iter()
                .map(|s| JsonAttachment::new(&s.attachment, entry.dir()))
                .collect()
        }),
    };
    Ok(format!("{}\n", serde_json::to_string_pretty(&doc)?))
}

/// `list` / `search`: `{schema_version, project, query?, entries: [...]}`.
#[derive(Serialize)]
struct JsonList<'a> {
    schema_version: u32,
    project: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    entries: Vec<JsonEntry<'a>>,
}

/// `show` / `add` / `attach`: `{schema_version, project, entry: {...}}`;
/// `add` and `attach` add `added: [...]`, the files they just stored.
#[derive(Serialize)]
struct JsonOne<'a> {
    schema_version: u32,
    project: &'a str,
    entry: JsonEntry<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    added: Option<Vec<JsonAttachment<'a>>>,
}

/// One attachment: `path` absolute on this machine, `relative_path` as the
/// entry records it (relative to the changelog directory, the path an
/// inline image's Markdown uses).
#[derive(Serialize)]
struct JsonAttachment<'a> {
    name: &'a str,
    kind: AttachmentKind,
    size: u64,
    path: String,
    relative_path: &'a str,
}

impl<'a> JsonAttachment<'a> {
    fn new(a: &'a Attachment, dir: &Path) -> Self {
        JsonAttachment {
            name: &a.name,
            kind: a.kind,
            size: a.size,
            path: a.absolute(dir).display().to_string(),
            relative_path: &a.path,
        }
    }
}

#[derive(Serialize)]
struct JsonEntry<'a> {
    id: &'a str,
    title: &'a str,
    /// RFC 3339, with the offset it was written in.
    date: String,
    author: &'a str,
    /// Markdown.
    description: &'a str,
    links: Vec<Link>,
    attachments: Vec<JsonAttachment<'a>>,
    path: String,
}

impl<'a> JsonEntry<'a> {
    fn new(entry: &'a ChangelogEntry, repo: Option<&str>) -> Self {
        JsonEntry {
            id: &entry.id,
            title: &entry.title,
            date: entry.date.to_rfc3339(),
            author: &entry.author,
            description: &entry.body,
            links: entry.links(repo),
            attachments: entry
                .attachments
                .iter()
                .map(|a| JsonAttachment::new(a, entry.dir()))
                .collect(),
            path: entry.path.display().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::PathBuf;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: ChangelogCmd,
    }

    fn parse(args: &[&str]) -> ChangelogCmd {
        Cli::try_parse_from(std::iter::once("changelog").chain(args.iter().copied()))
            .unwrap()
            .cmd
    }

    fn add_args(args: &[&str]) -> AddArgs {
        match parse(&[&["add"], args].concat()) {
            ChangelogCmd::Add(a) => a,
            other => panic!("expected add, got {other:?}"),
        }
    }

    fn entry() -> ChangelogEntry {
        ChangelogEntry {
            id: "2026-10-28-213005-changelog".into(),
            title: "Changelog lands".into(),
            date: chrono::DateTime::parse_from_rfc3339("2026-10-28T14:30:05-07:00").unwrap(),
            author: "orchestrator".into(),
            issues: vec!["changelog".into()],
            prs: vec![PrRef::Number(1953)],
            urls: vec![],
            body: "Activity becomes Changelog, see #1953.".into(),
            attachments: vec![],
            path: PathBuf::from("/h/projects/p/changelog/2026-10-28-213005-changelog.md"),
        }
    }

    const REPO: Option<&str> = Some("jlong/shelbi");

    #[test]
    fn add_flags_become_a_new_entry() {
        let args = add_args(&[
            "--title",
            "Ship it",
            "--date",
            "2026-10-28T10:00:00Z",
            "--author",
            "user",
            "--issue",
            "tui-about",
            "--issue",
            "changelog",
            "--pr",
            "#12",
            "--pr",
            "https://github.com/a/b/pull/3",
            "--link",
            "https://x.dev",
            "-d",
            "Body",
        ]);
        let new = new_entry(args, || panic!("stdin is not read when -d is given")).unwrap();
        assert_eq!(new.title, "Ship it");
        assert_eq!(new.date.to_rfc3339(), "2026-10-28T10:00:00+00:00");
        assert_eq!(new.author, "user");
        assert_eq!(new.issues, ["tui-about", "changelog"]);
        assert_eq!(
            new.prs,
            [
                PrRef::Number(12),
                PrRef::Url("https://github.com/a/b/pull/3".into())
            ]
        );
        assert_eq!(new.urls, ["https://x.dev"]);
        assert_eq!(new.description, "Body");
    }

    #[test]
    fn add_reads_the_description_from_stdin_when_d_is_absent() {
        let new = new_entry(add_args(&["--title", "T", "--author", "user"]), || {
            Ok(Some("from stdin\n".into()))
        })
        .unwrap();
        assert_eq!(new.description, "from stdin\n");
    }

    #[test]
    fn add_rejects_bad_links_and_dates() {
        for args in [
            &["--title", "T", "--issue", "Bad Id"][..],
            &["--title", "T", "--pr", "twelve"],
            &["--title", "T", "--link", "x.dev"],
            &["--title", "T", "--date", "soon"],
        ] {
            assert!(new_entry(add_args(args), || Ok(None)).is_err(), "{args:?}");
        }
    }

    #[test]
    fn subcommands_parse_with_their_defaults() {
        match parse(&[
            "list",
            "--limit",
            "5",
            "--since",
            "2026-10-01",
            "--format",
            "json",
        ]) {
            ChangelogCmd::List {
                limit,
                since,
                format,
            } => {
                assert_eq!(limit, Some(5));
                assert_eq!(since.as_deref(), Some("2026-10-01"));
                assert_eq!(format, Format::Json);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(&["search", "board"]),
            ChangelogCmd::Search {
                format: Format::Text,
                limit: None,
                ..
            }
        ));
        assert!(matches!(
            parse(&["show", "2026-10"]),
            ChangelogCmd::Show {
                format: Format::Markdown,
                ..
            }
        ));
        assert!(matches!(
            parse(&["strip-links"]),
            ChangelogCmd::StripLinks { dry_run: false }
        ));
        assert!(matches!(
            parse(&["strip-links", "--dry-run"]),
            ChangelogCmd::StripLinks { dry_run: true }
        ));
    }

    #[test]
    fn text_is_a_compact_two_line_list() {
        assert_eq!(
            render_text(&entry(), REPO),
            "2026-10-28 14:30  Changelog lands\n                  \
             2026-10-28-213005-changelog  orchestrator  issue:changelog  #1953\n"
        );
        assert_eq!(
            render_many("p", &[], REPO, Format::Text, Some("x")).unwrap(),
            "No changelog entries match `x`.\n"
        );
    }

    #[test]
    fn markdown_reads_like_the_blog_without_repeating_body_links() {
        assert_eq!(
            render_markdown(&entry(), REPO),
            "## Changelog lands\n\n*2026-10-28 14:30 · orchestrator · `2026-10-28-213005-changelog`*\n\n\
             Activity becomes Changelog, see #1953.\n\nLinks: issue:changelog\n"
        );
    }

    #[test]
    fn json_is_versioned_with_structured_links() {
        let out = render_many("p", &[entry()], REPO, Format::Json, Some("land")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "schema_version": 1,
                "project": "p",
                "query": "land",
                "entries": [{
                    "id": "2026-10-28-213005-changelog",
                    "title": "Changelog lands",
                    "date": "2026-10-28T14:30:05-07:00",
                    "author": "orchestrator",
                    "description": "Activity becomes Changelog, see #1953.",
                    "links": [
                        {"kind": "issue", "id": "changelog"},
                        {"kind": "pr", "number": 1953, "url": "https://github.com/jlong/shelbi/pull/1953"},
                    ],
                    "attachments": [],
                    "path": "/h/projects/p/changelog/2026-10-28-213005-changelog.md",
                }],
            })
        );
        let one: serde_json::Value =
            serde_json::from_str(&render_json_one("p", &entry(), None, None).unwrap()).unwrap();
        assert_eq!(one["schema_version"], 1);
        assert!(one.get("added").is_none(), "only add and attach report added files");
        assert_eq!(
            one["entry"]["links"][1],
            serde_json::json!({"kind": "pr", "number": 1953, "url": null})
        );
    }

    fn with_files() -> ChangelogEntry {
        let mut e = entry();
        e.body.push_str("\n\n![Board](assets/2026-10-28-213005-changelog/board.png)");
        e.attachments = vec![
            Attachment {
                path: "assets/2026-10-28-213005-changelog/board.png".into(),
                name: "board.png".into(),
                size: 2048,
                kind: AttachmentKind::Image,
            },
            Attachment {
                path: "assets/2026-10-28-213005-changelog/bench.csv".into(),
                name: "bench.csv".into(),
                size: 12 * 1024,
                kind: AttachmentKind::File,
            },
        ];
        e
    }

    #[test]
    fn image_args_split_off_alt_text_only_after_a_real_file() {
        let exists = |p: &Path| ["shot.png", "a:b.png", "/tmp/x.png"].contains(&p.to_str().unwrap());
        let split = |v: &str| split_image_arg(v, exists).map_err(|e| e.to_string());
        assert_eq!(split("shot.png").unwrap(), (PathBuf::from("shot.png"), None));
        assert_eq!(
            split("shot.png:The board: after").unwrap(),
            (PathBuf::from("shot.png"), Some("The board: after".into()))
        );
        // A name holding a colon is the file when it exists.
        assert_eq!(split("a:b.png").unwrap(), (PathBuf::from("a:b.png"), None));
        assert_eq!(
            split("a:b.png:caption").unwrap(),
            (PathBuf::from("a:b.png"), Some("caption".into()))
        );
        assert_eq!(split("/tmp/x.png:  ").unwrap(), (PathBuf::from("/tmp/x.png"), None));
        assert!(split("missing.png:alt").unwrap_err().contains("no such file `missing.png`"));
    }

    #[test]
    fn add_and_attach_take_images_and_files() {
        let args = add_args(&["--title", "T", "--image", "a.png", "--image", "b.png:B", "--attach", "log.txt"]);
        assert_eq!(args.files.images, ["a.png", "b.png:B"]);
        assert_eq!(args.files.attach, ["log.txt"]);
        match parse(&["attach", "2026-10", "--attach", "x.zip", "--format", "json"]) {
            ChangelogCmd::Attach { entry_id, files, format } => {
                assert_eq!(entry_id, "2026-10");
                assert_eq!(files.attach, ["x.zip"]);
                assert!(files.images.is_empty());
                assert_eq!(format, Format::Json);
            }
            other => panic!("{other:?}"),
        }
        assert!(FileArgs::default().is_empty());
    }

    #[test]
    fn json_and_markdown_carry_attachments_with_absolute_paths() {
        let e = with_files();
        let stored = vec![StoredAttachment {
            absolute: PathBuf::from("/h/projects/p/changelog/assets/2026-10-28-213005-changelog/bench.csv"),
            attachment: e.attachments[1].clone(),
        }];
        let v: serde_json::Value =
            serde_json::from_str(&render_json_one("p", &e, REPO, Some(&stored)).unwrap()).unwrap();
        assert_eq!(
            v["entry"]["attachments"][1],
            serde_json::json!({
                "name": "bench.csv",
                "kind": "file",
                "size": 12288,
                "path": "/h/projects/p/changelog/assets/2026-10-28-213005-changelog/bench.csv",
                "relative_path": "assets/2026-10-28-213005-changelog/bench.csv",
            })
        );
        assert_eq!(v["added"], serde_json::json!([v["entry"]["attachments"][1]]));

        let md = render_markdown(&e, REPO);
        assert!(md.contains("![Board](assets/2026-10-28-213005-changelog/board.png)\n"), "{md}");
        assert!(
            md.ends_with(
                "\nAttachments:\n\n- [bench.csv](</h/projects/p/changelog/assets/2026-10-28-213005-changelog/bench.csv>) (12 KB)\n"
            ),
            "{md}"
        );
        assert_eq!(
            render_stored(&stored),
            "  file  /h/projects/p/changelog/assets/2026-10-28-213005-changelog/bench.csv  \
             (assets/2026-10-28-213005-changelog/bench.csv)\n"
        );
    }
}
