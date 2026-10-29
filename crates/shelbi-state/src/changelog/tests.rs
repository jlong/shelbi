use super::*;
use crate::test_lock::LOCK as TEST_LOCK;

fn scratch(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "shelbi-changelog-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn new_entry(title: &str, date: &str) -> NewEntry {
    NewEntry {
        title: title.into(),
        date: at(date),
        author: "orchestrator".into(),
        issues: vec![],
        prs: vec![],
        urls: vec![],
        description: String::new(),
    }
}

const REPO: Option<&str> = Some("jlong/shelbi");

// ---- links ----------------------------------------------------------------

#[test]
fn body_links_parse_issues_prs_and_urls_in_order() {
    let body = "Lands issue:tui-about with #1942.\n\
                See https://shelbi.dev/docs/cli/changelog, and \
                [the PR](https://github.com/other/repo/pull/7).";
    assert_eq!(
        parse_body_links(body, REPO),
        vec![
            Link::Issue {
                id: "tui-about".into()
            },
            Link::Pr {
                number: Some(1942),
                url: Some("https://github.com/jlong/shelbi/pull/1942".into()),
            },
            Link::Url {
                url: "https://shelbi.dev/docs/cli/changelog".into()
            },
            Link::Pr {
                number: Some(7),
                url: Some("https://github.com/other/repo/pull/7".into()),
            },
        ]
    );
}

#[test]
fn body_links_skip_code_mid_word_and_headings() {
    let body = "# Heading\n\
                `issue:not-me` and `#12` stay code; foo#13 and path/issue:x are words.\n\
                ```\n#14 issue:fenced https://fenced.dev\n```\n\
                ## 2026 notes\n\
                (#15) works";
    assert_eq!(
        parse_body_links(body, None),
        vec![Link::Pr {
            number: Some(15),
            url: None
        }]
    );
}

#[test]
fn body_links_dedupe_and_trim_trailing_punctuation() {
    let body = "issue:a, issue:a. #3 #3 (https://x.dev/a_(b)). https://x.dev/a_(b)";
    assert_eq!(
        parse_body_links(body, None),
        vec![
            Link::Issue { id: "a".into() },
            Link::Pr {
                number: Some(3),
                url: None
            },
            Link::Url {
                url: "https://x.dev/a_(b)".into()
            },
        ]
    );
}

#[test]
fn pr_refs_parse_numbers_hashes_and_urls() {
    assert_eq!(PrRef::parse("1942"), Some(PrRef::Number(1942)));
    assert_eq!(PrRef::parse("#1942"), Some(PrRef::Number(1942)));
    assert_eq!(
        PrRef::parse("https://github.com/a/b/pull/9"),
        Some(PrRef::Url("https://github.com/a/b/pull/9".into()))
    );
    assert_eq!(PrRef::parse("pull nine"), None);
}

#[test]
fn entry_links_merge_frontmatter_and_body() {
    let entry = ChangelogEntry {
        id: "x".into(),
        title: "T".into(),
        date: at("2026-10-28T10:00:00Z"),
        author: "user".into(),
        issues: vec!["changelog".into()],
        prs: vec![PrRef::Number(12), PrRef::Url("#13".into())],
        urls: vec!["https://example.com".into()],
        body: "Also issue:changelog and #12 and #99.".into(),
        path: PathBuf::new(),
    };
    let labels: Vec<String> = entry.links(REPO).iter().map(Link::label).collect();
    assert_eq!(
        labels,
        [
            "issue:changelog",
            "#12",
            "#13",
            "https://example.com",
            "#99"
        ]
    );
    // Only the frontmatter links the body doesn't already show.
    let extra: Vec<String> = entry.extra_links(REPO).iter().map(Link::label).collect();
    assert_eq!(extra, ["#13", "https://example.com"]);
}

#[test]
fn links_serialize_with_a_kind_tag() {
    let json = serde_json::to_value(vec![
        Link::Issue { id: "a".into() },
        Link::Pr {
            number: Some(1),
            url: None,
        },
        Link::Url {
            url: "https://x.dev".into(),
        },
    ])
    .unwrap();
    assert_eq!(
        json,
        serde_json::json!([
            {"kind": "issue", "id": "a"},
            {"kind": "pr", "number": 1, "url": null},
            {"kind": "url", "url": "https://x.dev"},
        ])
    );
}

// ---- template ---------------------------------------------------------------

#[test]
fn the_default_template_names_every_placeholder() {
    for p in ["{{title}}", "{{date}}", "{{description}}", "{{links}}"] {
        assert!(
            DEFAULT_CHANGELOG_TEMPLATE.contains(p),
            "default template lacks {p}"
        );
    }
}

#[test]
fn filling_drops_the_comment_and_lays_out_links() {
    let links = [
        Link::Issue {
            id: "changelog".into(),
        },
        Link::Pr {
            number: Some(5),
            url: Some("https://github.com/jlong/shelbi/pull/5".into()),
        },
        Link::Pr {
            number: Some(6),
            url: None,
        },
        Link::Url {
            url: "https://x.dev".into(),
        },
    ];
    let body = fill_template(
        DEFAULT_CHANGELOG_TEMPLATE,
        &TemplateValues {
            title: "T",
            date: at("2026-10-28T14:30:00-07:00"),
            author: "user",
            description: "  Did the thing.\n",
            links: &links,
        },
    );
    assert_eq!(
        body,
        "Did the thing.\n\n- issue:changelog\n- https://github.com/jlong/shelbi/pull/5\n- #6\n- https://x.dev"
    );
    // The rendered list parses back to the same links.
    assert_eq!(parse_body_links(&body, REPO).len(), 4);
}

#[test]
fn filling_a_custom_template_and_empty_values() {
    let template = "## {{title}}\n_{{date}} {{time}} by {{author}}_\n\n{{description}}\n\n\n{{links}}\n\n{{unknown}}\n";
    let body = fill_template(
        template,
        &TemplateValues {
            title: "Ship",
            date: at("2026-10-28T09:05:00+02:00"),
            author: "developer",
            description: "",
            links: &[],
        },
    );
    assert_eq!(
        body,
        "## Ship\n_2026-10-28 09:05 by developer_\n\n{{unknown}}"
    );
}

// ---- dates ----------------------------------------------------------------

#[test]
fn dates_parse_every_accepted_form() {
    assert_eq!(
        parse_date("2026-10-28T14:30:00-07:00", BareDate::StartOfDay),
        Some(at("2026-10-28T14:30:00-07:00"))
    );
    assert_eq!(
        parse_date("2026-10-28T21:30:00Z", BareDate::StartOfDay),
        Some(at("2026-10-28T21:30:00Z"))
    );
    let local = parse_date("2026-10-28 14:30", BareDate::StartOfDay).unwrap();
    assert_eq!(
        local.format("%Y-%m-%d %H:%M:%S").to_string(),
        "2026-10-28 14:30:00"
    );
    let midnight = parse_date("2026-10-28", BareDate::StartOfDay).unwrap();
    assert_eq!(midnight.format("%H:%M:%S").to_string(), "00:00:00");
    let today = parse_date("2026-10-28", BareDate::NowTimeOfDay).unwrap();
    assert_eq!(today.format("%Y-%m-%d").to_string(), "2026-10-28");
    assert_eq!(parse_date("yesterday", BareDate::StartOfDay), None);
}

// ---- storage ----------------------------------------------------------------

#[test]
fn add_writes_a_dated_markdown_file_that_reads_back() {
    let dir = scratch("add");
    let mut new = new_entry("Changelog lands: the sidebar!", "2026-10-28T14:30:05-07:00");
    new.issues = vec!["changelog".into()];
    new.prs = vec![PrRef::Number(1953)];
    new.urls = vec!["https://shelbi.dev".into()];
    new.description = "The sidebar's Activity row becomes Changelog.".into();
    let entry = add_entry_in(&dir, DEFAULT_CHANGELOG_TEMPLATE, REPO, new).unwrap();

    assert_eq!(entry.id, "2026-10-28-213005-changelog-lands-the-sidebar");
    assert_eq!(entry.path, dir.join(format!("{}.md", entry.id)));
    let text = fs::read_to_string(&entry.path).unwrap();
    assert!(
        text.starts_with("---\ntitle: 'Changelog lands: the sidebar!'\n"),
        "{text}"
    );
    assert!(text.contains("date: 2026-10-28T14:30:05-07:00\n"), "{text}");
    assert!(text.contains("author: orchestrator\n"), "{text}");
    assert!(text.contains("prs:\n- 1953\n"), "{text}");
    assert!(
        !text.contains("<!--"),
        "the template comment stays out: {text}"
    );

    let read = read_entries_in(&dir).unwrap();
    assert!(read.skipped.is_empty());
    assert_eq!(read.entries, vec![entry.clone()]);
    assert_eq!(entry.date, at("2026-10-28T14:30:05-07:00"));
    assert_eq!(
        entry.body,
        "The sidebar's Activity row becomes Changelog.\n\n- issue:changelog\n- https://github.com/jlong/shelbi/pull/1953\n- https://shelbi.dev"
    );
    assert_eq!(entry.links(REPO).len(), 3);
}

#[test]
fn add_rejects_a_blank_or_multiline_title() {
    let dir = scratch("title");
    for title in ["", "   ", "two\nlines"] {
        assert!(add_entry_in(
            &dir,
            "{{description}}",
            None,
            new_entry(title, "2026-10-28T00:00:00Z")
        )
        .is_err());
    }
}

#[test]
fn entries_list_newest_first_and_skip_broken_files() {
    let dir = scratch("order");
    for (title, date) in [
        ("middle", "2026-10-02T12:00:00Z"),
        ("oldest", "2026-10-01T12:00:00Z"),
        ("newest", "2026-10-03T12:00:00-07:00"),
    ] {
        add_entry_in(&dir, "{{description}}", None, new_entry(title, date)).unwrap();
    }
    fs::write(dir.join("broken.md"), "no frontmatter").unwrap();
    fs::write(dir.join("notes.txt"), "ignored").unwrap();
    let read = read_entries_in(&dir).unwrap();
    let titles: Vec<&str> = read.entries.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, ["newest", "middle", "oldest"]);
    assert_eq!(read.skipped.len(), 1);
    assert_eq!(read.skipped[0].0, dir.join("broken.md"));
}

#[test]
fn a_missing_directory_is_an_empty_changelog() {
    let dir = scratch("missing").join("changelog");
    assert!(read_entries_in(&dir).unwrap().entries.is_empty());
    assert_eq!(changelog_stamp_in(&dir), ChangelogStamp::default());
}

#[test]
fn hand_edited_entries_load_leniently() {
    let dir = scratch("hand");
    fs::write(
        dir.join("2026-10-01-000000-by-hand.md"),
        "---\ntitle: By hand\ndate: 2026-10-01\nprs: ['#4']\n---\nBody #5\n",
    )
    .unwrap();
    let read = read_entries_in(&dir).unwrap();
    let entry = &read.entries[0];
    assert_eq!(entry.author, DEFAULT_AUTHOR);
    assert_eq!(entry.date.format("%Y-%m-%d").to_string(), "2026-10-01");
    let labels: Vec<String> = entry.links(None).iter().map(Link::label).collect();
    assert_eq!(labels, ["#4", "#5"]);
}

#[test]
fn concurrent_adds_with_the_same_name_all_land() {
    let dir = scratch("race");
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mut new = new_entry("Same", "2026-10-28T10:00:00Z");
                new.description = format!("writer {n}");
                let entry = add_entry_in(&dir, "{{description}}", None, new).unwrap();
                // Each file holds its own writer's words, never another's.
                assert_eq!(entry.body, format!("writer {n}"));
                entry.id
            })
        })
        .collect();
    let mut ids: Vec<String> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8, "every writer got its own file: {ids:?}");
    assert_eq!(read_entries_in(&dir).unwrap().entries.len(), 8);
    // No temp files are left behind.
    let leftovers = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn find_takes_an_exact_id_or_a_unique_prefix() {
    let dir = scratch("find");
    let a = add_entry_in(&dir, "", None, new_entry("Alpha", "2026-10-01T00:00:00Z")).unwrap();
    let b = add_entry_in(&dir, "", None, new_entry("Beta", "2026-10-02T00:00:00Z")).unwrap();
    let entries = || read_entries_in(&dir).unwrap().entries;
    assert_eq!(find_in(entries(), &a.id).unwrap().title, "Alpha");
    assert_eq!(
        find_in(entries(), &format!("{}.md", b.id)).unwrap().title,
        "Beta"
    );
    assert_eq!(find_in(entries(), "2026-10-02").unwrap().title, "Beta");
    let ambiguous = find_in(entries(), "2026-10").unwrap_err().to_string();
    assert!(ambiguous.contains("matches 2"), "{ambiguous}");
    assert!(find_in(entries(), "2025").is_err());
}

#[test]
fn search_matches_title_body_and_link_targets_case_insensitively() {
    let dir = scratch("search");
    let mut a = new_entry("Faster Board", "2026-10-01T00:00:00Z");
    a.description = "Columns are a gpui list now.".into();
    a.issues = vec!["dt-board-perf".into()];
    add_entry_in(&dir, "{{description}}", REPO, a).unwrap();
    let mut b = new_entry("Changelog", "2026-10-02T00:00:00Z");
    b.prs = vec![PrRef::Number(1953)];
    add_entry_in(&dir, "{{description}}", REPO, b).unwrap();

    let read = read_entries_in(&dir).unwrap();
    let hits = |q: &str| -> Vec<String> {
        read.entries
            .iter()
            .filter(|e| e.matches(q, REPO))
            .map(|e| e.title.clone())
            .collect()
    };
    assert_eq!(hits("faster"), ["Faster Board"]);
    assert_eq!(hits("GPUI LIST"), ["Faster Board"]);
    assert_eq!(hits("board-perf"), ["Faster Board"]);
    assert_eq!(hits("#1953"), ["Changelog"]);
    assert_eq!(hits("pull/1953"), ["Changelog"]);
    assert_eq!(hits("board changelog"), Vec::<String>::new());
    assert_eq!(hits("").len(), 2);
}

#[test]
fn the_stamp_changes_when_an_entry_is_added() {
    let dir = scratch("stamp");
    let before = changelog_stamp_in(&dir);
    add_entry_in(&dir, "", None, new_entry("One", "2026-10-01T00:00:00Z")).unwrap();
    let after = changelog_stamp_in(&dir);
    assert_ne!(before, after);
    assert_eq!(after.entries, 1);
}

#[test]
fn origin_urls_come_from_the_git_config() {
    let config = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@github.com:a/b.git\n\
                  [remote \"origin\"]\n\turl = https://github.com/jlong/shelbi.git\n\tfetch = +refs/heads/*\n";
    assert_eq!(
        origin_from_config(config).as_deref(),
        Some("https://github.com/jlong/shelbi.git")
    );
    assert_eq!(origin_from_config("[core]\n"), None);

    // A worktree's `.git` file is followed to the common dir's config.
    let root = scratch("git");
    let main_git = root.join("main/.git");
    fs::create_dir_all(main_git.join("worktrees/wt")).unwrap();
    fs::write(main_git.join("config"), config).unwrap();
    fs::write(main_git.join("worktrees/wt/commondir"), "../..\n").unwrap();
    let wt = root.join("wt");
    fs::create_dir_all(&wt).unwrap();
    fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", main_git.join("worktrees/wt").display()),
    )
    .unwrap();
    assert_eq!(
        origin_url(&wt).as_deref(),
        Some("https://github.com/jlong/shelbi.git")
    );
}

// ---- project paths (temp SHELBI_HOME) -----------------------------------------

#[test]
fn project_paths_scaffold_and_add_under_shelbi_home() {
    let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = scratch("home");
    std::env::set_var("SHELBI_HOME", &home);

    assert_eq!(
        changelog_dir("p").unwrap(),
        home.join("projects/p/changelog")
    );
    assert_eq!(
        changelog_template_path("p").unwrap(),
        home.join("projects/p/changelog-template.md")
    );

    // Missing → the default template is used, then seeded.
    assert_eq!(read_changelog_template("p"), DEFAULT_CHANGELOG_TEMPLATE);
    assert_eq!(
        scaffold_changelog_template("p").unwrap(),
        ChangelogTemplateOutcome::Created
    );
    let path = changelog_template_path("p").unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        DEFAULT_CHANGELOG_TEMPLATE
    );

    // A customized template survives a re-seed and shapes new entries.
    fs::write(&path, "**{{title}}**\n\n{{description}}\n").unwrap();
    assert_eq!(
        scaffold_changelog_template("p").unwrap(),
        ChangelogTemplateOutcome::Unchanged
    );
    let mut new = new_entry("Custom", "2026-10-28T00:00:00Z");
    new.description = "Body.".into();
    let entry = add_entry("p", new).unwrap();
    assert_eq!(entry.body, "**Custom**\n\nBody.");
    assert!(entry.path.starts_with(home.join("projects/p/changelog")));

    let read = read_entries("p").unwrap();
    assert_eq!(read.entries.len(), 1);
    assert_eq!(find_entry("p", &entry.id).unwrap(), entry);
    assert_eq!(changelog_stamp("p").entries, 1);

    std::env::remove_var("SHELBI_HOME");
}
