//! `shelbi __palette PROJECT` — full-screen ratatui picker meant to run
//! inside a `tmux display-popup`. Lists every destination the sidebar can
//! reach (Chat, Tasks, each declared worker, each review-ready task, each
//! legacy spawned agent) plus the global actions; on Enter, performs the
//! action and exits.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use shelbi_palette::{Entry, EntryKind};
use shelbi_state::{load_user_config, ProjectSummary, ZenModeState, ZenToggleChord};
use shelbi_tui::{decoration_to_color, App, Row, View, WorkerOverview};

pub fn run(project: String) -> Result<()> {
    let mut term = setup_terminal()?;
    let mut state = State::new(&project)?;

    // Sub-screens share the palette's alt-screen so the follow-up
    // surface is a re-render rather than a popup-pane flash:
    //
    // - Switch Project — sub-picker. Esc inside it exits the palette
    //   (preserves the prior single-shot UX so the popup feels like a
    //   one-key chord).
    // - Quit Project — confirmation popover. Cancel bounces back to
    //   the main picker so a fat-fingered Enter on the destructive
    //   action doesn't kick the user out of the palette entirely —
    //   they can pick a different option without re-summoning it.
    // - Quit Shelbi — confirmation popover. Handled below the loop
    //   because its cancel-path exits the palette (matches the prior
    //   single-shot UX for that entry); quit-project's bounce-back
    //   loop here intentionally diverges.
    let (chosen, switch_target, quit_project_confirmed) = loop {
        let chosen = picker_loop(&mut term, &mut state);
        match &chosen {
            Ok(Some(entry)) if entry.id == "action:switch-project" => {
                let target = run_project_picker(&mut term, &project)?;
                break (chosen, target, false);
            }
            Ok(Some(entry)) if entry.id == "action:quit-project" => {
                if run_quit_project_confirm(&mut term, &project)? {
                    break (chosen, None, true);
                }
                // Cancel: re-enter the main picker with state preserved.
                continue;
            }
            _ => break (chosen, None, false),
        }
    };
    let quit_shelbi_confirmed = match &chosen {
        Ok(Some(entry)) if entry.id == "action:quit-shelbi" => {
            run_quit_shelbi_confirm(&mut term)?
        }
        _ => false,
    };

    restore_terminal(&mut term)?;

    if let Some(target) = switch_target {
        switch_to_project(&target)?;
    } else if quit_project_confirmed {
        super::quit_project::run(&project)?;
    } else if quit_shelbi_confirmed {
        super::quit_shelbi::run()?;
    } else if let Ok(Some(entry)) = chosen {
        if entry.id != "action:switch-project"
            && entry.id != "action:quit-project"
            && entry.id != "action:quit-shelbi"
        {
            dispatch(&project, &entry)?;
        }
    }
    Ok(())
}

struct State {
    query: String,
    selected: usize,
    all_entries: Vec<Entry>,
    project: String,
}

impl State {
    fn new(project: &str) -> Result<Self> {
        // Lean on the sidebar's `App` for the row list — same icons,
        // same status decorations, same data source. Anything that
        // changes how the sidebar paints a destination automatically
        // shows up in the palette on the next open.
        let mut app = App::new_sidebar(project);
        app.refresh().ok();
        // The chord lives in user config (loaded by the sidebar via its
        // first-run probe, not by `App::refresh`), so the palette has
        // to read it separately. Defaults to Alt+Z on missing config —
        // same fallback the probe uses on cooperative terminals.
        let chord = load_user_config()
            .map(|c| c.keymap.zen_toggle)
            .unwrap_or(ZenToggleChord::AltZ);
        let all_entries = build_entries(&app, app.zen_mode, chord);
        Ok(Self {
            query: String::new(),
            selected: 0,
            all_entries,
            project: project.to_string(),
        })
    }

    fn results(&self) -> Vec<(Entry, u16)> {
        shelbi_palette::search(&self.all_entries, &self.query)
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

fn picker_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    state: &mut State,
) -> Result<Option<Entry>> {
    loop {
        let results = state.results();
        term.draw(|f| render(f, state, &results))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Allow closing with Ctrl+C / Ctrl+P / Esc.
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('p'))
                {
                    return Ok(None);
                }
                match k.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => {
                        if let Some((entry, _)) = results.get(state.selected) {
                            return Ok(Some(entry.clone()));
                        }
                    }
                    KeyCode::Up => {
                        if state.selected > 0 {
                            state.selected -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if state.selected + 1 < results.len() {
                            state.selected += 1;
                        }
                    }
                    KeyCode::Backspace => {
                        state.query.pop();
                        state.selected = 0;
                    }
                    KeyCode::Char(c) => {
                        state.query.push(c);
                        state.selected = 0;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render(f: &mut Frame, state: &State, results: &[(Entry, u16)]) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Title.
    let title = Paragraph::new(Line::from(Span::styled(
        format!("shelbi · {}", state.project),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    f.render_widget(title, layout[0]);

    // Search input.
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(state.query.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[1]);

    // Result list. List row width matches the list pane so we can pad
    // out to the right edge and tuck the shortcut hint flush against
    // it; falls back to no padding when the area is narrower than the
    // content (the shortcut still appears, just not right-aligned).
    let row_width = layout[2].width as usize;
    let items: Vec<ListItem> = results
        .iter()
        .map(|(e, _)| {
            let (glyph, glyph_color) = match &e.decoration {
                Some(d) => (d.glyph.as_str(), decoration_to_color(d.color)),
                None => (e.kind.icon(), Color::DarkGray),
            };
            let prefix = format!(" {glyph} ");
            let label = format!("{:<22}", e.label);
            let mut content_width = prefix.chars().count() + label.chars().count();
            let mut spans = vec![
                Span::styled(prefix, Style::default().fg(glyph_color)),
                Span::raw(label),
            ];
            if let Some(sub) = &e.subtitle {
                let s = format!("  {sub}");
                content_width += s.chars().count();
                spans.push(Span::styled(s, Style::default().fg(Color::DarkGray)));
            }
            if let Some(short) = &e.shortcut {
                let sw = short.chars().count();
                // 1-col right margin keeps the glyph off the pane edge.
                let pad = row_width
                    .saturating_sub(content_width)
                    .saturating_sub(sw)
                    .saturating_sub(1);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                } else {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(
                    short.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let mut s = ListState::default();
    if !results.is_empty() {
        s.select(Some(state.selected.min(results.len().saturating_sub(1))));
    }
    f.render_stateful_widget(list, layout[2], &mut s);

    // Footer.
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        "↑↓ navigate · Enter activate · Esc / Ctrl+P close",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, layout[3]);
}

// ---------------------------------------------------------------------------
// Entry building + dispatch
//
// Order mirrors `App::rows()` in shelbi-tui: Chat, Tasks, Activity,
// then workers, then review-ready tasks, then legacy spawned agents,
// then the two global actions. An empty-query palette should read
// top-to-bottom like the sidebar. We literally walk `app.rows()` to
// build the entry list so the palette inherits whatever icons and
// status decorations the sidebar paints — no parallel lookup to drift.

fn build_entries(app: &App, zen_mode: ZenModeState, zen_chord: ZenToggleChord) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    // First pass: every sidebar row becomes an entry with the row's
    // own decoration attached. Section headers and blanks have no
    // decoration and are skipped.
    for row in app.rows() {
        if let Some(entry) = entry_from_row(&row, &app.workers) {
            out.push(entry);
        }
        // The Zen-Mode toggle sits inline with the top nav block so users
        // discover it where they already look for the dashboard.
        if matches!(&row, Row::Nav { view: View::Builtin("activity"), .. }) {
            out.push(zen_toggle_entry(zen_mode, zen_chord));
        }
    }
    out.push(Entry {
        id: "action:switch-project".into(),
        label: "Switch Project".into(),
        kind: EntryKind::Action,
        subtitle: Some("fuzzy-pick another project and swap the dashboard".into()),
        shortcut: None,
        decoration: None,
    });
    out.push(Entry {
        id: "action:quit-project".into(),
        label: "Quit Project".into(),
        kind: EntryKind::Action,
        subtitle: Some(
            "close every pane (workers + stash + main) and switch to the next project".into(),
        ),
        shortcut: None,
        decoration: None,
    });
    // Most-destructive action lands last so fuzzy search doesn't
    // surface it ahead of the per-project quit, and so users who
    // scroll the list bottom-up have to step over the project-level
    // option before they hit the global one.
    out.push(Entry {
        id: "action:quit-shelbi".into(),
        label: "Quit Shelbi".into(),
        kind: EntryKind::Action,
        subtitle: Some(
            "close every Shelbi session on this host (all projects + workers + stash)".into(),
        ),
        shortcut: None,
        decoration: None,
    });
    out
}

/// Translate one sidebar [`Row`] into a palette [`Entry`]. The row's
/// `decoration()` is the source of truth for icon + status tint — this
/// just copies it across and stitches a subtitle suited to the
/// palette's wider layout. Returns `None` for section headers / blanks
/// since they have no destination to activate.
fn entry_from_row(row: &Row, workers: &[WorkerOverview]) -> Option<Entry> {
    let decoration = row.decoration();
    match row {
        Row::Nav { label, view, .. } => {
            let id = match view {
                View::Builtin(name) => format!("view:{name}"),
                _ => return None,
            };
            Some(Entry {
                id,
                label: label.to_string(),
                kind: EntryKind::View,
                subtitle: nav_subtitle(view),
                shortcut: None,
                decoration,
            })
        }
        Row::Worker { name, view, .. } => {
            let machine = workers
                .iter()
                .find(|w| w.name == *name)
                .map(|w| w.machine.clone());
            let id = match view {
                View::Worker(_) => format!("worker:{name}"),
                _ => format!("worker:{name}"),
            };
            Some(Entry {
                id,
                label: name.clone(),
                kind: EntryKind::Agent,
                subtitle: Some(match machine {
                    Some(m) => format!("worker · {m}"),
                    None => "worker".into(),
                }),
                shortcut: None,
                decoration,
            })
        }
        Row::Review { title, worker, view } => {
            let id = match view {
                View::ReviewTask(task_id) => format!("review:{task_id}"),
                _ => return None,
            };
            Some(Entry {
                id,
                label: title.clone(),
                kind: EntryKind::Action,
                subtitle: Some(match worker.as_deref() {
                    Some(w) => format!("review · {w}"),
                    None => "review".into(),
                }),
                shortcut: None,
                decoration,
            })
        }
        Row::LegacyAgent {
            id,
            machine,
            status,
            ..
        } => Some(Entry {
            id: format!("agent:{id}"),
            label: id.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!("{machine} · {status:?}")),
            shortcut: None,
            decoration,
        }),
        Row::Section { .. } | Row::Blank => None,
    }
}

/// Per-destination palette subtitle. Sidebar nav rows render label-only,
/// so the subtitle is palette-specific copy that explains what each
/// destination is for first-time users.
fn nav_subtitle(view: &View) -> Option<String> {
    match view {
        View::Builtin("orch") => Some("the claude pane you talk to".into()),
        View::Builtin("tasks") => Some("live `shelbi list`".into()),
        View::Builtin("activity") => Some("human-readable events feed".into()),
        _ => None,
    }
}

/// Build the "Toggle Zen Mode" entry. Label flips with current state
/// so users can see what's on before activating, and the right-aligned
/// shortcut hint mirrors the user's configured chord (defaults to
/// `⌥Z`). When the chord is explicitly disabled we omit the hint
/// rather than render an empty column.
fn zen_toggle_entry(current: ZenModeState, chord: ZenToggleChord) -> Entry {
    let (label, subtitle) = match current {
        ZenModeState::On => ("Turn Zen Mode off", "currently on"),
        ZenModeState::Off => ("Turn Zen Mode on", "currently off"),
        ZenModeState::Paused => ("Turn Zen Mode on", "currently paused"),
    };
    let hint = chord.hint();
    let shortcut = if hint.is_empty() {
        None
    } else {
        Some(hint.to_string())
    };
    Entry {
        id: "action:toggle-zen".into(),
        label: label.into(),
        kind: EntryKind::Action,
        subtitle: Some(subtitle.into()),
        shortcut,
        decoration: None,
    }
}

fn dispatch(project: &str, entry: &Entry) -> Result<()> {
    if let Some(view) = entry.id.strip_prefix("view:") {
        shelbi_orchestrator::show_view(project, view).map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    if let Some(worker) = entry.id.strip_prefix("worker:") {
        shelbi_orchestrator::focus_worker(project, worker).map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    if let Some(task_id) = entry.id.strip_prefix("review:") {
        let target = shelbi_orchestrator::review::start_review_by_id(project, task_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        run_tmux(["select-window", "-t", &target]);
        return Ok(());
    }
    if let Some(id) = entry.id.strip_prefix("agent:") {
        run_tmux(["select-window", "-t", &format!("shelbi-{project}:{id}")]);
        return Ok(());
    }
    if entry.id == "action:toggle-zen" {
        // Shares the read/write/log path with the TUI's Alt+Z handler
        // and the CLI's `shelbi zen on|off` — only the source tag
        // differs (`user:palette`) so the activity feed can attribute
        // the toggle back to the palette.
        shelbi_state::toggle_zen_mode(project, "user:palette")
            .map_err(|e| anyhow::anyhow!(e))?;
        return Ok(());
    }
    // `action:quit-project` is intentionally not handled here — the
    // outer `run` flow gates it behind a confirmation popover and
    // invokes `super::quit_project::run` directly on confirm. Routing
    // it through `dispatch` would bypass the popover.
    Ok(())
}

fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// "Switch project" sub-picker
//
// A second ratatui loop sharing the palette's alt-screen. Lists every
// project except the current one, fuzzy-filtered the same way the main
// palette filters its entries (nucleo via `shelbi_palette::score`).

fn run_project_picker<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    current: &str,
) -> Result<Option<String>> {
    let projects: Vec<ProjectSummary> = shelbi_state::list_projects()
        .map_err(|e| anyhow::anyhow!(e))?
        .into_iter()
        .filter(|p| p.name != current)
        .collect();
    if projects.is_empty() {
        // Nothing to switch to — degrade silently; the user lands back
        // on the regular palette flow which exits cleanly.
        return Ok(None);
    }

    let mut query = String::new();
    let mut selected = 0usize;

    loop {
        let results = filter_projects(&projects, &query);
        term.draw(|f| render_project_picker(f, current, &query, &results, selected))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('p'))
                {
                    return Ok(None);
                }
                match k.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => {
                        if let Some(p) = results.get(selected) {
                            return Ok(Some(p.name.clone()));
                        }
                    }
                    KeyCode::Up => {
                        selected = selected.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        if selected + 1 < results.len() {
                            selected += 1;
                        }
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        selected = 0;
                    }
                    KeyCode::Char(c) => {
                        query.push(c);
                        selected = 0;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn filter_projects(projects: &[ProjectSummary], query: &str) -> Vec<ProjectSummary> {
    if query.is_empty() {
        return projects.to_vec();
    }
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
    let mut hits: Vec<(ProjectSummary, u16)> = projects
        .iter()
        .filter_map(|p| {
            shelbi_palette::score(&mut matcher, query, &p.name).and_then(|s| {
                if s == 0 {
                    None
                } else {
                    Some((p.clone(), s))
                }
            })
        })
        .collect();
    hits.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    hits.into_iter().map(|(p, _)| p).collect()
}

fn render_project_picker(
    f: &mut Frame,
    current: &str,
    query: &str,
    results: &[ProjectSummary],
    selected: usize,
) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            "switch project",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  (current: {current})"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    f.render_widget(title, layout[0]);

    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(query.to_string()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[1]);

    let items: Vec<ListItem> = results
        .iter()
        .map(|p| {
            let m = if p.machine_count == 1 {
                "machine"
            } else {
                "machines"
            };
            let w = if p.worker_count == 1 {
                "worker"
            } else {
                "workers"
            };
            let spans = vec![
                Span::styled(" ◉ ", Style::default().fg(Color::DarkGray)),
                Span::raw(format!("{:<22}", p.name)),
                Span::styled(
                    format!("  {} {m} · {} {w}", p.machine_count, p.worker_count),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let mut s = ListState::default();
    if !results.is_empty() {
        s.select(Some(selected.min(results.len().saturating_sub(1))));
    }
    f.render_stateful_widget(list, layout[2], &mut s);

    let footer = Paragraph::new(Line::from(vec![Span::styled(
        "↑↓ navigate · Enter switch · Esc cancel",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, layout[3]);
}

// ---------------------------------------------------------------------------
// "Quit Shelbi" confirmation popover
//
// A second ratatui screen sharing the palette's alt-screen, same
// pattern as `run_project_picker`. Lists every project shelbi is
// currently managing (live `shelbi-<name>` tmux sessions) with a
// count of each one's active worker panes so users see exactly what
// they're about to tear down. Cancel is default focus — the popover
// is a guard against accidental confirmation, so the safe button is
// the one Enter activates by default.

fn run_quit_shelbi_confirm<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
) -> Result<bool> {
    let projects = super::quit_shelbi::list_managed_projects();
    let mut focus_quit = false;

    loop {
        term.draw(|f| render_quit_shelbi_confirm(f, &projects, focus_quit))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('p'))
                {
                    return Ok(false);
                }
                match k.code {
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Enter => return Ok(focus_quit),
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        focus_quit = !focus_quit;
                    }
                    _ => {}
                }
            }
        }
    }
}

// "Quit Project" confirmation popover
//
// A second ratatui screen sharing the palette's alt-screen, same
// pattern as `run_project_picker`. Lists each declared worker whose
// tmux pane is currently live, with its state + assigned task, so
// users see exactly what's about to be torn down. Cancel is default
// focus — this is a guard against accidental confirmation, so the
// safe button is the one Enter activates by default.

fn run_quit_project_confirm<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    project: &str,
) -> Result<bool> {
    let workers = super::quit_project::list_active_workers(project);
    let mut focus_quit = false;

    loop {
        term.draw(|f| render_quit_project_confirm(f, project, &workers, focus_quit))?;

        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('p'))
                {
                    return Ok(false);
                }
                match k.code {
                    KeyCode::Esc => return Ok(false),
                    KeyCode::Enter => return Ok(focus_quit),
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        focus_quit = !focus_quit;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn render_quit_shelbi_confirm(
    f: &mut Frame,
    projects: &[super::quit_shelbi::ManagedProject],
    focus_quit: bool,
) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // blank
            Constraint::Length(4), // warning body (wraps)
            Constraint::Length(1), // blank
            Constraint::Min(1),    // project list
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
            Constraint::Length(1), // footer
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        "Quit Shelbi?",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    f.render_widget(title, layout[0]);

    let body = Paragraph::new(
        "This will close all open Shelbi sessions across every project, including any \
         active workers. In-flight task changes will remain in worker worktrees but \
         won't be merged.",
    )
    .style(Style::default().fg(Color::Gray))
    .wrap(Wrap { trim: true });
    f.render_widget(body, layout[2]);

    let items: Vec<ListItem> = projects
        .iter()
        .map(|p| {
            let count_text = format_active_workers(p.active_workers);
            ListItem::new(Line::from(vec![
                Span::raw(format!("  {:<22}  ", p.name)),
                Span::styled(count_text, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    f.render_widget(List::new(items), layout[4]);

    render_confirm_buttons(f, layout[6], "Quit Shelbi", focus_quit);
    render_confirm_footer(f, layout[7]);
}

fn render_quit_project_confirm(
    f: &mut Frame,
    project: &str,
    workers: &[super::quit_project::ActiveWorker],
    focus_quit: bool,
) {
    let area = f.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // blank
            Constraint::Length(3), // warning body (wraps)
            Constraint::Length(1), // blank
            Constraint::Min(1),    // worker list / empty state
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
            Constraint::Length(1), // footer
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        format!("Quit project: {project}?"),
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    f.render_widget(title, layout[0]);

    let body = Paragraph::new(
        "This will close the project session and any worker panes. In-flight \
         task changes will remain in worker worktrees but won't be merged.",
    )
    .style(Style::default().fg(Color::Gray))
    .wrap(Wrap { trim: true });
    f.render_widget(body, layout[2]);

    if workers.is_empty() {
        let empty =
            Paragraph::new("No active workers.").style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, layout[4]);
    } else {
        let items: Vec<ListItem> = workers
            .iter()
            .map(|w| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("  {:<14}  ", w.name)),
                    Span::styled(
                        format!("{:<16}", w.state),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(
                        format!("  {}", w.task),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect();
        f.render_widget(List::new(items), layout[4]);
    }

    render_confirm_buttons(f, layout[6], "Quit project", focus_quit);
    render_confirm_footer(f, layout[7]);
}

// Destructive button gets the red tint when focused; the cancel
// button gets the standard blue highlight. Unfocused buttons
// render dim so the focused option is unambiguous at a glance.
fn render_confirm_buttons(f: &mut Frame, area: Rect, quit_label: &str, focus_quit: bool) {
    let cancel_style = if focus_quit {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    };
    let quit_style = if focus_quit {
        Style::default()
            .bg(Color::Red)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let buttons = Paragraph::new(Line::from(vec![
        Span::styled("  [ Cancel ]  ", cancel_style),
        Span::raw("  "),
        Span::styled(format!("  [ {quit_label} ]  "), quit_style),
    ]));
    f.render_widget(buttons, area);
}

fn render_confirm_footer(f: &mut Frame, area: Rect) {
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        "←→/Tab switch · Enter activate · Esc cancel",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, area);
}

/// Per-project worker-count suffix shown in the Quit Shelbi popover.
/// Singular/plural distinction kept so `1 active worker` reads
/// naturally — matches how `picker.rs` formats machine/worker counts.
fn format_active_workers(n: usize) -> String {
    match n {
        0 => "(no active workers)".to_string(),
        1 => "(1 active worker)".to_string(),
        n => format!("({n} active workers)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_toggle_entry_label_and_subtitle_flip_with_current_state() {
        let on = zen_toggle_entry(ZenModeState::On, ZenToggleChord::AltZ);
        assert_eq!(on.label, "Turn Zen Mode off");
        assert_eq!(on.subtitle.as_deref(), Some("currently on"));

        let off = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(off.label, "Turn Zen Mode on");
        assert_eq!(off.subtitle.as_deref(), Some("currently off"));

        let paused = zen_toggle_entry(ZenModeState::Paused, ZenToggleChord::AltZ);
        assert_eq!(paused.label, "Turn Zen Mode on");
        assert_eq!(paused.subtitle.as_deref(), Some("currently paused"));
    }

    #[test]
    fn zen_toggle_entry_uses_configured_chord_hint() {
        let alt = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(alt.shortcut.as_deref(), Some("⌥Z"));

        let bs = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::CtrlBackslash);
        assert_eq!(bs.shortcut.as_deref(), Some("⌃\\"));
    }

    #[test]
    fn zen_toggle_entry_omits_shortcut_when_chord_disabled() {
        let none = zen_toggle_entry(ZenModeState::On, ZenToggleChord::None);
        assert!(none.shortcut.is_none());
    }

    #[test]
    fn zen_toggle_entry_id_is_stable_dispatch_key() {
        // The dispatch() match keys off this exact id; renaming the entry
        // breaks the toggle silently. Pin it.
        let e = zen_toggle_entry(ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(e.id, "action:toggle-zen");
    }

    #[test]
    fn build_entries_places_zen_toggle_directly_after_view_block() {
        // A fresh `App` (no `refresh`) carries empty worker / review / agent
        // lists, so `rows()` is just the three nav entries — perfect for
        // pinning the inline-actions order without touching the disk.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        // View block first (Chat/Tasks/Activity), then the mode toggle,
        // then the global actions trail. Workers/reviews/agents are
        // empty in this fixture so they don't appear.
        assert_eq!(
            ids,
            vec![
                "view:orch",
                "view:tasks",
                "view:activity",
                "action:toggle-zen",
                "action:switch-project",
                "action:quit-project",
                "action:quit-shelbi",
            ]
        );
    }

    #[test]
    fn format_active_workers_handles_zero_one_and_many() {
        assert_eq!(format_active_workers(0), "(no active workers)");
        assert_eq!(format_active_workers(1), "(1 active worker)");
        assert_eq!(format_active_workers(2), "(2 active workers)");
        assert_eq!(format_active_workers(11), "(11 active workers)");
    }

    #[test]
    fn quit_shelbi_is_the_absolute_last_entry_in_the_palette() {
        // Destructive global action — fuzzy search shouldn't surface
        // it ahead of per-project quit, and a user scrolling bottom-up
        // should have to step over the project-level option before
        // hitting the host-wide one. Pinned because the dispatch
        // assumes this exact id and a rename would silently break it.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        assert_eq!(entries.last().map(|e| e.id.as_str()), Some("action:quit-shelbi"));
    }

    #[test]
    fn nav_entries_carry_the_sidebar_icon_as_their_decoration() {
        // The palette must paint each sidebar destination with the same
        // glyph the sidebar uses, sourced from `Row::decoration` so the
        // two surfaces can't drift.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        let by_id = |id: &str| {
            entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing entry {id}"))
        };
        assert_eq!(by_id("view:orch").decoration.as_ref().unwrap().glyph, "💬");
        assert_eq!(by_id("view:tasks").decoration.as_ref().unwrap().glyph, "📋");
        assert_eq!(
            by_id("view:activity").decoration.as_ref().unwrap().glyph,
            "⚡"
        );
    }

    #[test]
    fn global_actions_without_a_sidebar_twin_keep_their_default_icon() {
        // The task scope explicitly excludes the global actions — they
        // have no sidebar row to mirror, so their decoration stays None
        // and the renderer falls back to the dim `EntryKind::icon()`.
        let app = App::new_sidebar("demo");
        let entries = build_entries(&app, ZenModeState::Off, ZenToggleChord::AltZ);
        for id in [
            "action:toggle-zen",
            "action:switch-project",
            "action:quit-project",
            "action:quit-shelbi",
        ] {
            let e = entries
                .iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("missing entry {id}"));
            assert!(
                e.decoration.is_none(),
                "{id} should fall back to the dim default icon, got {:?}",
                e.decoration
            );
        }
    }
}

fn switch_to_project(target: &str) -> Result<()> {
    shelbi_state::touch_project_launched(target).map_err(|e| anyhow::anyhow!(e))?;
    shelbi_orchestrator::ensure_dashboard(target).map_err(|e| anyhow::anyhow!(e))?;

    let session = format!("shelbi-{target}");
    let inside_tmux = std::env::var("TMUX").is_ok();
    let args: &[&str] = if inside_tmux {
        &["switch-client", "-t"]
    } else {
        &["attach", "-t"]
    };
    let _ = std::process::Command::new("tmux")
        .args(args)
        .arg(&session)
        .status();
    Ok(())
}
