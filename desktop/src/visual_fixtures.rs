//! `shelbi-desktop --visual-figma-fixtures <root>` (feature `visual-check`)
//! and the fixture project `--visual-figma` renders: "My project", its
//! board, workspace statuses, machines, `events.log` and changelog, written
//! as plain Shelbi state.
//!
//! Split out of `visual_figma` because it only writes files: it needs none
//! of gpui's offscreen rendering (`VisualTestAppContext`, macOS-only in
//! gpui-pre 0.3.9), so the seeded sandbox the real-window walks use builds
//! and runs on Linux too. See `docs/gpui.md` for which visual-check modes
//! are macOS-only.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, TimeZone, Utc};
use shelbi_core::{Column, Issue};

pub(crate) const PROJECT: &str = "my-project";

/// `--visual-figma-fixtures <root>`: the mockups' content (the projects,
/// the board, the Activity events, the workspaces' states) written into
/// `root` as a Shelbi root, timed from the real clock so the Activity ages
/// read as drawn while the real app shows it. `scripts/figma-window-walk.sh`
/// opens it in the bundled app. Exits.
pub fn export_fixtures(root: PathBuf) -> ! {
    std::fs::create_dir_all(&root).expect("create the Shelbi root");
    // SAFETY: single-threaded here, before any reader thread starts.
    unsafe {
        std::env::set_var("SHELBI_HOME", &root);
        std::env::set_var("SHELBI_ROOT", &root);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_CONTROL_SOCK");
        std::env::remove_var("SHELBI_HUB_ADDR");
        std::env::remove_var("SHELBI_PROJECT");
    }
    write_fixtures(&root, Utc::now());
    println!("fixtures for project {PROJECT} written under {}", root.display());
    std::process::exit(0)
}

/// 15:00 local on 2026-10-17, the mockups' "today".
pub(crate) fn pinned_now() -> DateTime<Utc> {
    Local
        .with_ymd_and_hms(2026, 10, 17, 15, 0, 0)
        .single()
        .expect("a local time")
        .with_timezone(&Utc)
}



/// A card: id, title, body (its first paragraph is the card's excerpt).
pub(crate) struct Card(pub(crate) &'static str, pub(crate) &'static str, pub(crate) &'static str);

pub(crate) const BACKLOG: [Card; 10] = [
    Card("theme-docs", "Write the theme docs for the desktop palette", "Document the light and dark palettes, token roles, and contrast requirements for the desktop UI."),
    Card("board-widths", "Sync the board’s column widths with the TUI", "Keep columns readable as the window narrows. Match the TUI’s minimum widths and horizontal scrolling."),
    Card("agent-usage", "Show agent usage and cost by workspace", "Track tokens and estimated cost for each agent run, with a per-workspace breakdown in the activity view."),
    Card("terminal-search", "Search terminal scrollback", "Find previous commands and agent output with incremental search, match counts, and next/previous navigation."),
    Card("notifications", "Add notifications for completed work", "Notify when work is ready for review or an agent needs input. Respect the system notification settings."),
    Card("window-state", "Restore window layout on launch", "Remember the window size, sidebar width, and open panels between launches. Keep restored windows on-screen."),
    Card("ssh-errors", "Make SSH connection errors actionable", "Say which host failed and why, and offer the command that fixes it."),
    Card("session-export", "Export a session transcript", "Save a workspace’s terminal output as text or HTML for sharing."),
    Card("machine-setup", "Set up a remote machine from the app", "Run the machine setup flow from the Machines view."),
    Card("task-templates", "Start issues from templates", "Offer the project’s issue templates when creating a new issue."),
];

pub(crate) const TODO: [Card; 6] = [
    Card("desktop-palette", "Restyle the command palette", "Group commands by context, show keyboard shortcuts, and make the selected result easier to scan."),
    Card("sidebar-keyboard", "Improve keyboard navigation in the sidebar", "Move between sections with the arrow keys and restore focus to the last selected workspace."),
    Card("terminal-scroll", "Preserve terminal scroll position", "Keep the viewport anchored while new output arrives. Follow new output only when scrolled to the bottom."),
    Card("close-workspace", "Confirm before closing a busy workspace", "Warn when an agent is still running and offer to leave it in the background instead of stopping the session."),
    Card("retry-agent", "Retry failed agent starts", "Keep the startup error visible and let users retry with the same agent, model, and working directory."),
    Card("saved-filters", "Remember issue filters per project", "Save board filters per project and restore them when returning, including the selected workflow and workspace."),
];

pub(crate) const IN_PROGRESS: [Card; 2] = [
    Card("session-reconnect", "Handle reconnects after a daemon restart", "Reconnect to running sessions after the hub restarts, preserving terminal output and the active workspace."),
    Card("project-switcher", "Add project switching to the desktop app", "Switch between recent projects without restarting the app. Restore each project’s last open workspace."),
];

/// The umbrella's body, as its detail shows it. (The mockups' card excerpt
/// for it, "Build a native desktop client with an issue board, live agent
/// terminals, and a queue for reviewing completed work.", differs from the
/// detail's; the fixture follows the detail.)
pub(crate) const DESKTOP_APP_BODY: &str = "# Desktop application\n\nBuild a native desktop client for Shelbi using the shared application model. \
The desktop should expose the same orchestrator, workspaces, issues, review, activity, and machines as the TUI.\n\n\
## Acceptance criteria\n\n\
- [x] Native sidebar and project navigation\n\
- [x] Terminal rendering, selection, search, and keyboard input\n\
- [x] Issues board and task details\n\
- [x] Workspace and review views with live sessions\n\
- [ ] Final visual review and packaging\n\n\
## Implementation notes\n\n\
The desktop is its own Rust workspace. Shared state and orchestration stay in the existing crates; the GPUI \
client owns presentation and native interaction.\n";

/// The Task pane frames' task: its title and the body the pane shows (the
/// board's card keeps its own excerpt, as the frames draw it).
pub(crate) const TASK_PANE: (&str, &str) = (
    "Handle reconnects after a daemon restart",
    "## Description\n\nReconnect to running agent sessions after the hub restarts. Keep terminal output and the \
active workspace intact so work can continue without interruption.\n\n\
## Acceptance criteria\n\n\
- [x] Restore the last selected workspace\n\
- [x] Reconnect without losing terminal output\n\
- [ ] Offer a retry when the hub is unavailable\n\n\
## Implementation notes\n\n\
Reuse the shared reconnect policy. A retry should attach to the existing session instead of starting another agent.\n",
);

/// Save the Task pane frames' task in `column` with its frame body: second
/// in its queue either way (the frames' Priority, "2 in queue"), assigned
/// with its branch once `started`, neither before.
pub(crate) fn save_pane_task(column: Column, started: bool) {
    let (id, (title, body)) = (IN_PROGRESS[0].0, TASK_PANE);
    // Ready to start, none of its criteria are met yet.
    let body = if started { body.to_string() } else { body.replace("[x]", "[ ]") };
    save_task(id, title, &body, column, 2, started.then_some("alpha"));
    if !started {
        let mut file = shelbi_state::load_task(PROJECT, id).expect("the pane's task");
        file.task.branch = None;
        shelbi_state::save_task(PROJECT, &file.task, &file.body).expect("save the pane's task");
    }
}

/// Give the pane's task a new body.
pub(crate) fn save_task_body(body: &str) {
    let mut file = shelbi_state::load_task(PROJECT, IN_PROGRESS[0].0).expect("the pane's task");
    file.body = body.to_string();
    shelbi_state::save_task(PROJECT, &file.task, &file.body).expect("save the pane's task");
}

pub(crate) fn day(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, d, 12, 0, 0).single().expect("a fixture date")
}

/// The in-progress tasks' titles as the Machines frame draws them.
pub(crate) const MACHINES_TITLES: [(&str, &str); 2] = [
    ("project-switcher", "Add project switching"),
    ("session-reconnect", "Session reconnect"),
];

/// Give fixture task `id` a new title; the old one comes back.
pub(crate) fn retitle(id: &str, title: &str) -> String {
    let mut file = shelbi_state::load_task(PROJECT, id).expect("a fixture task");
    let old = std::mem::replace(&mut file.task.title, title.into());
    shelbi_state::save_task(PROJECT, &file.task, &file.body).expect("retitle a fixture task");
    old
}

pub(crate) fn save_task(id: &str, title: &str, body: &str, column: Column, priority: i64, assigned: Option<&str>) {
    let task = Issue {
        id: id.into(),
        title: title.into(),
        column,
        priority: priority as _,
        assigned_to: assigned.map(str::to_string),
        workflow: Some("task".into()),
        branch: Some(format!("jlong/{id}")),
        pr: None,
        depends_on: vec![],
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: day(12),
        updated_at: day(16),
        params: Default::default(),
    };
    shelbi_state::save_task(PROJECT, &task, body).expect("save a fixture task");
}

/// The Changelog fixture's entries, newest first: title, days before the
/// pinned now, the description, its issues, PRs and other links.
#[allow(clippy::type_complexity)]
pub(crate) const CHANGELOG_FIXTURES: &[(&str, i64, &str, &[&str], &[u64], &[&str])] = &[
    (
        "Settings polish pass",
        0,
        "Every settings page, dialog and editor now matches the desktop: one half-block button, segmented controls, dropdowns, and the same focus ring everywhere.\n\nThe pages that were still on the old form layout moved over:\n\n- **General** and **Terminal**, with live previews\n- **Keybindings**, whose capture now shows the chord as you press it\n- **Workflows**, including the builder's side panel",
        &["tui-settings-polish"],
        &[1942],
        &["https://shelbi.dev/docs/settings"],
    ),
    (
        "Contact Us feedback dialog",
        0,
        "Send feedback without leaving Shelbi: **Help \u{25b8} Contact Us** opens a small dialog that sends your note (and, if you like, your email) straight to the team.",
        &["dt-feedback-dialog"],
        &[1931],
        &[],
    ),
    (
        "A Contrast slider in Settings",
        1,
        "`Settings \u{25b8} General` has a **Contrast** slider. Moving it right darkens backgrounds and brightens text, for bright rooms and tired eyes; terminals keep their own colors.",
        &[],
        &[1953],
        &[],
    ),
    (
        "A faster board",
        6,
        "The Issues board draws only the cards in view, so a board with a thousand done issues opens as fast as a small one, and scrolling it stays smooth.",
        &["dt-board-perf"],
        &[1817],
        &[],
    ),
    (
        "Drag and drop on the board",
        17,
        "Drag a card to another column to move it, or within its column to reorder. A column that can't take the card dims and says why.",
        &["dt-board-dnd"],
        &[],
        &["https://shelbi.dev/docs/board"],
    ),
];

/// Write [`CHANGELOG_FIXTURES`] into the fixture project's changelog.
/// A repository tagged `v0.13.0`, `v0.13.3` and `v0.14.0` between the
/// changelog fixture's entries (10 days, 3 days and 12 hours before `now`).
pub(crate) fn write_release_repo(repo: &Path, now: DateTime<Utc>) -> PathBuf {
    let _ = std::fs::remove_dir_all(repo);
    std::fs::create_dir_all(repo).expect("create the release repository");
    let git = |args: &[&str], at: DateTime<Utc>| {
        let date = at.to_rfc3339();
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_NAME", "Shelbi")
            .env("GIT_AUTHOR_EMAIL", "visual@example.com")
            .env("GIT_COMMITTER_NAME", "Shelbi")
            .env("GIT_COMMITTER_EMAIL", "visual@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success());
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"], now);
    for (tag, ago) in [
        ("v0.13.0", chrono::Duration::days(10)),
        ("v0.13.3", chrono::Duration::days(3)),
        ("v0.14.0", chrono::Duration::hours(12)),
    ] {
        git(&["commit", "-q", "--allow-empty", "-m", tag], now - ago);
        git(&["tag", "-a", "-m", tag, tag], now - ago);
    }
    repo.to_path_buf()
}

pub(crate) fn write_changelog_fixtures(now: DateTime<Utc>) {
    use shelbi_state::changelog::{add_entry, NewEntry, PrRef};
    for (i, (title, days, body, issues, prs, urls)) in CHANGELOG_FIXTURES.iter().enumerate() {
        // Same-day entries a minute apart, so the order is the fixture's.
        let at = now - chrono::Duration::days(*days) - chrono::Duration::minutes(i as i64);
        add_entry(
            PROJECT,
            NewEntry {
                title: title.to_string(),
                date: at.with_timezone(&Local).fixed_offset(),
                author: "orchestrator".into(),
                issues: issues.iter().map(|s| s.to_string()).collect(),
                prs: prs
                    .iter()
                    .map(|n| PrRef::Url(format!("https://github.com/jlong/shelbi/pull/{n}")))
                    .collect(),
                urls: urls.iter().map(|s| s.to_string()).collect(),
                description: body.to_string(),
            },
        )
        .expect("write a changelog fixture entry");
    }
}

pub(crate) fn write_fixtures(home: &Path, now: DateTime<Utc>) {
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: Shelbi
repo: ''
default_branch: main
orchestrator:
  runner: codex
agent_runners:
  codex:
    command: codex
    flags: []
  claude:
    command: claude
    flags: []
agents:
  developer:
    runner: codex
  review:
    runner: claude
machines:
  - name: hub
    kind: local
    work_dir: {work}
  - name: gpu
    kind: ssh
    host: gpu.lan
    work_dir: /home/me
    tags: [gpu, linux]
workspaces:
  - name: alpha
    machine: hub
  - name: bravo
    machine: hub
  - name: charlie
    machine: hub
  - name: review
    machine: hub
    tags: [review]
    slot: 1
  - name: gpu-1
    machine: gpu
",
            work = home.join("work").display()
        ),
    )
    .unwrap();

    for (column, cards) in [
        (Column::backlog(), &BACKLOG[..]),
        (Column::todo(), &TODO[..]),
        (Column::in_progress(), &IN_PROGRESS[..]),
    ] {
        for (i, Card(id, title, body)) in cards.iter().enumerate() {
            let assigned = match *id {
                "session-reconnect" => Some("bravo"),
                "project-switcher" => Some("alpha"),
                _ => None,
            };
            save_task(id, title, body, column.clone(), i as i64 + 1, assigned);
        }
    }
    save_task("desktop-app", "Desktop application", DESKTOP_APP_BODY, Column::review(), 1, Some("review"));
    save_task(
        "theme-tokens",
        "Share theme tokens across clients",
        "Move the color tokens into the shared application model so both clients paint from one palette.",
        Column::done(),
        1,
        Some("alpha"),
    );

    let status = |workspace: &str, task: Option<&str>, state| {
        shelbi_state::save_workspace_status("demo", &shelbi_state::WorkspaceStatus {
            workspace: workspace.into(),
            current_task: task.map(str::to_string),
            state,
            last_transition: now,
            last_seen: now,
            opencode_session: None,
        })
        .unwrap();
    };
    status("alpha", Some("project-switcher"), shelbi_state::WorkspaceState::Working);
    status("bravo", Some("session-reconnect"), shelbi_state::WorkspaceState::Working);
    status("review", Some("desktop-app"), shelbi_state::WorkspaceState::Serving);
    // The review slot has confirmed its server (Machines: `review`, not
    // `starting`).
    let review_marker = home.join("work/.shelbi/wt/review/.shelbi");
    std::fs::create_dir_all(&review_marker).unwrap();
    std::fs::write(review_marker.join("shelbi-review-loaded"), "desktop-app\n").unwrap();

    shelbi_state::machine_state::save_machine_record(
        "gpu",
        Some(shelbi_state::machine_state::MachineRecord {
            path: "/usr/local/bin/shelbi".into(),
            version: "0.10.0".into(),
            compatible: true,
            source: shelbi_state::machine_state::SOURCE_PATH.into(),
            checked_at: now,
            host: None,
        }),
    )
    .unwrap();

    // The Activity mockup's eight events, newest first there. charlie's
    // "available" is its worktree released after a handoff, and the
    // daemon's "started" its own line (the desktop labels both).
    let at = |mins: i64| (now - chrono::Duration::minutes(mins)).to_rfc3339();
    let p = format!("project={PROJECT}");
    let events = [
        format!("{} daemon started", at(24 * 60 + 50)),
        format!("{} {p} task=theme-tokens workflow=task in-progress -> review reason=workspace:ready-marker", at(24 * 60 + 20)),
        format!("{} worktree-detach task=sidebar-collapse workspace=charlie detached-from=jlong/fix-sidebar-collapse status=ok", at(40)),
        format!("{} {p} task=project-switcher workflow=task todo -> in-progress reason=orchestrator:auto-dispatch", at(25)),
        format!("{} {p} task=session-reconnect workflow=task todo -> in-progress reason=orchestrator:auto-dispatch", at(18)),
        format!("{} {p} task=desktop-app workflow=task in-progress -> review reason=workspace:ready-marker", at(12)),
        format!("{} {p} task=desktop-palette workflow=task backlog -> todo reason=user:board", at(6)),
        format!("{} {p} task=theme-tokens workflow=task review -> done reason=orchestrator:zen-merge_pr=594", at(2)),
    ];
    std::fs::write(home.join("events.log"), events.join("\n") + "\n").unwrap();

    // Settings: the frame's Shelbi CLI path, as an override (the window has
    // no discovery to show, so it draws with Reset). Its font and size predate the
    // Source Code Pro default, so those keep theirs.
    // The run's contrast and opacity are the saved ones too, so Settings
    // shows them (and reading the file doesn't put the theme back to 0
    // contrast, opaque).
    let theme = crate::visual_theme::theme();
    let contrast = theme.contrast();
    let contrast = if contrast == 0 { String::new() } else { format!("  contrast: {contrast}\n") };
    let opacity = (theme.opacity() * 100.0).round() as u8;
    let opacity = if opacity >= 100 { String::new() } else { format!("  window_opacity: {opacity}\n") };
    std::fs::write(
        home.join("config.yaml"),
        format!("desktop:\n  cli_path: /opt/homebrew/bin/shelbi\n{contrast}{opacity}"),
    )
    .unwrap();
}
