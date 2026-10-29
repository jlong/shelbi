//! `shelbi changelog <subcommand>`: add to and read the project's changelog,
//! the blog-style record of what has shipped (`shelbi_state::changelog`).
//!
//! `add` fills the project's `changelog-template.md` and writes a new entry
//! file; `list`, `search` and `show` read them back as a compact text list,
//! blog-style markdown, or JSON for agents and scripts. The JSON shape is
//! versioned by `schema_version` and documented on the CLI docs page
//! (`site/content/docs/cli/changelog.mdx`).

use std::io::{IsTerminal, Read};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use shelbi_state::changelog::{self, BareDate, ChangelogEntry, Link, NewEntry, PrRef};

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

#[derive(Debug, Subcommand)]
pub enum ChangelogCmd {
    /// Add an entry, its description laid out by the project's
    /// changelog-template.md.
    #[command(after_long_help = LINK_HELP)]
    Add(AddArgs),
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
    /// How to print the new entry.
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
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
            let new = new_entry(args, read_stdin_description)?;
            let entry = changelog::add_entry(&project, new).map_err(|e| anyhow!(e))?;
            let out = match format {
                Format::Text => format!("✓ added {}\n  {}\n", entry.id, entry.path.display()),
                Format::Markdown => render_markdown(&entry, repo),
                Format::Json => render_json_one(&project, &entry, repo)?,
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
                Format::Json => render_json_one(&project, &entry, repo)?,
            };
            print!("{out}");
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

fn render_json_one(project: &str, entry: &ChangelogEntry, repo: Option<&str>) -> Result<String> {
    let doc = JsonOne {
        schema_version: JSON_SCHEMA_VERSION,
        project,
        entry: JsonEntry::new(entry, repo),
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

/// `show` / `add`: `{schema_version, project, entry: {...}}`.
#[derive(Serialize)]
struct JsonOne<'a> {
    schema_version: u32,
    project: &'a str,
    entry: JsonEntry<'a>,
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
                    "path": "/h/projects/p/changelog/2026-10-28-213005-changelog.md",
                }],
            })
        );
        let one: serde_json::Value =
            serde_json::from_str(&render_json_one("p", &entry(), None).unwrap()).unwrap();
        assert_eq!(one["schema_version"], 1);
        assert_eq!(
            one["entry"]["links"][1],
            serde_json::json!({"kind": "pr", "number": 1953, "url": null})
        );
    }
}
