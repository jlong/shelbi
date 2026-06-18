//! `shelbi __palette PROJECT` — full-screen ratatui picker meant to run
//! inside a `tmux display-popup`. Lists the orchestrator + every active
//! agent + global actions; on Enter, performs the action and exits.

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
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph},
    Frame, Terminal,
};
use shelbi_core::{Agent, Status};
use shelbi_palette::{Entry, EntryKind};

pub fn run(project: String) -> Result<()> {
    let mut term = setup_terminal()?;
    let mut state = State::new(&project)?;

    let chosen = picker_loop(&mut term, &mut state);

    restore_terminal(&mut term)?;

    if let Ok(Some(entry)) = chosen {
        dispatch(&project, &entry, &state.agents)?;
    }
    Ok(())
}

struct State {
    query: String,
    selected: usize,
    all_entries: Vec<Entry>,
    agents: Vec<Agent>,
    project: String,
}

impl State {
    fn new(project: &str) -> Result<Self> {
        let agents = load_agents(project).unwrap_or_default();
        let all_entries = build_entries(&agents);
        Ok(Self {
            query: String::new(),
            selected: 0,
            all_entries,
            agents,
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
    let outer = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::new(1, 1, 0, 0))
        .title(Span::styled(
            format!(" shelbi · {} ", state.project),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Search input.
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(state.query.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[0]);

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
    f.render_stateful_widget(list, layout[1], &mut s);

    // Footer.
    let footer = Paragraph::new(Line::from(vec![Span::styled(
        " ↑↓ navigate · Enter activate · Esc / Ctrl+P close",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, layout[2]);
}

// ---------------------------------------------------------------------------
// Entry building + dispatch

fn build_entries(agents: &[Agent]) -> Vec<Entry> {
    let mut out = vec![Entry {
        id: "view:orchestrator".into(),
        label: "orchestrator".into(),
        kind: EntryKind::View,
        subtitle: Some("focus the orchestrator pane".into()),
    }];
    for a in agents {
        out.push(Entry {
            id: format!("agent:{}", a.id),
            label: a.id.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!("{} · {:?}", a.machine, a.status)),
        });
    }
    out.push(Entry {
        id: "action:quit".into(),
        label: "quit shelbi".into(),
        kind: EntryKind::Action,
        subtitle: Some("kill the shelbi-<project> tmux session".into()),
    });
    out
}

fn dispatch(project: &str, entry: &Entry, _agents: &[Agent]) -> Result<()> {
    match entry.kind {
        EntryKind::View => {
            // The only view today is "orchestrator" — focus the right pane.
            let win = format!("shelbi-{project}:dashboard");
            run_tmux(["select-window", "-t", &win]);
            run_tmux(["select-pane", "-t", &format!("{win}.{{right}}")]);
        }
        EntryKind::Agent => {
            // Strip the "agent:" prefix from id, or use label.
            let id = entry
                .id
                .strip_prefix("agent:")
                .unwrap_or(&entry.label);
            run_tmux(["select-window", "-t", &format!("shelbi-{project}:{id}")]);
        }
        EntryKind::Action => match entry.id.as_str() {
            "action:quit" => {
                run_tmux(["kill-session", "-t", &format!("shelbi-{project}")]);
            }
            _ => {}
        },
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
