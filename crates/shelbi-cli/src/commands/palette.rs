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
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use shelbi_core::{Agent, Column, Status};
use shelbi_palette::{Entry, EntryKind};
use shelbi_state::{ProjectSummary, TaskFile};

pub fn run(project: String) -> Result<()> {
    let mut term = setup_terminal()?;
    let mut state = State::new(&project)?;

    let chosen = picker_loop(&mut term, &mut state);

    // "Switch project" stays inside the same alt-screen so the second
    // picker is just a re-render — restoring the terminal first would
    // flash the popup back to its host pane mid-flow.
    let switch_target = match &chosen {
        Ok(Some(entry)) if entry.id == "action:switch-project" => {
            run_project_picker(&mut term, &project)?
        }
        _ => None,
    };

    restore_terminal(&mut term)?;

    if let Some(target) = switch_target {
        switch_to_project(&target)?;
    } else if let Ok(Some(entry)) = chosen {
        if entry.id != "action:switch-project" {
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
        let workers = load_workers(project).unwrap_or_default();
        let review_queue = load_review_queue(project);
        let agents = load_agents(project).unwrap_or_default();
        let all_entries = build_entries(&workers, &review_queue, &agents);
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

    // Result list.
    let items: Vec<ListItem> = results
        .iter()
        .map(|(e, _)| {
            let mut spans = vec![
                Span::styled(
                    format!(" {} ", e.kind.icon()),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!("{:<22}", e.label)),
            ];
            if let Some(sub) = &e.subtitle {
                spans.push(Span::styled(
                    format!("  {sub}"),
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
// Order mirrors `App::rows()` in shelbi-tui: Chat, Tasks, then workers,
// then review-ready tasks, then legacy spawned agents, then the two
// global actions. An empty-query palette should read top-to-bottom like
// the sidebar.

fn build_entries(
    workers: &[WorkerEntry],
    review_queue: &[TaskFile],
    agents: &[Agent],
) -> Vec<Entry> {
    let mut out = vec![
        Entry {
            id: "view:orch".into(),
            label: "Chat".into(),
            kind: EntryKind::View,
            subtitle: Some("the claude pane you talk to".into()),
        },
        Entry {
            id: "view:tasks".into(),
            label: "Tasks".into(),
            kind: EntryKind::View,
            subtitle: Some("live `shelbi list`".into()),
        },
    ];
    for w in workers {
        out.push(Entry {
            id: format!("worker:{}", w.name),
            label: w.name.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!("worker · {}", w.machine)),
        });
    }
    for tf in review_queue {
        out.push(Entry {
            id: format!("review:{}", tf.task.id),
            label: tf.task.title.clone(),
            kind: EntryKind::Action,
            subtitle: Some(match tf.task.assigned_to.as_deref() {
                Some(w) => format!("review · {w}"),
                None => "review".into(),
            }),
        });
    }
    for a in agents {
        out.push(Entry {
            id: format!("agent:{}", a.id),
            label: a.id.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!("{} · {:?}", a.machine, a.status)),
        });
    }
    out.push(Entry {
        id: "action:switch-project".into(),
        label: "Switch Project".into(),
        kind: EntryKind::Action,
        subtitle: Some("fuzzy-pick another project and swap the dashboard".into()),
    });
    out.push(Entry {
        id: "action:quit".into(),
        label: "Quit Shelbi".into(),
        kind: EntryKind::Action,
        subtitle: Some("kill the shelbi-<project> tmux session".into()),
    });
    out
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
    if entry.id == "action:quit" {
        run_tmux(["kill-session", "-t", &format!("shelbi-{project}")]);
    }
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

/// Minimal palette-side view of a declared worker — just what the entry
/// list needs (name for the label, machine for the subtitle). Mirrors the
/// sidebar's worker rows in shelbi-tui without dragging in `WorkerOverview`
/// (which carries badge state the palette doesn't render).
struct WorkerEntry {
    name: String,
    machine: String,
}

fn load_workers(project: &str) -> Result<Vec<WorkerEntry>> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow::anyhow!(e))?;
    let mut out = Vec::with_capacity(p.workers.len());
    for worker in &p.workers {
        // Silently skip mis-configured workers — same forgiveness the
        // sidebar uses; surfacing them in the palette would just be noise.
        if p.machine(&worker.machine).is_none() {
            continue;
        }
        out.push(WorkerEntry {
            name: worker.name.clone(),
            machine: worker.machine.clone(),
        });
    }
    Ok(out)
}

fn load_review_queue(project: &str) -> Vec<TaskFile> {
    shelbi_state::list_column(project, Column::Review).unwrap_or_default()
}

fn load_agents(project: &str) -> Result<Vec<Agent>> {
    let dir = match shelbi_state::agents_dir(project) {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".log.md") || !name.ends_with(".md") {
            continue;
        }
        let id = name.trim_end_matches(".md");
        if let Ok(file) = shelbi_state::load_agent(project, id) {
            out.push(file.agent);
        }
    }
    out.sort_by_key(|a| status_order(a.status));
    Ok(out)
}

fn status_order(s: Status) -> u8 {
    match s {
        Status::Running => 0,
        Status::Waiting => 1,
        Status::Queued => 2,
        Status::Done => 3,
        Status::Error => 4,
        Status::Archived => 5,
    }
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
