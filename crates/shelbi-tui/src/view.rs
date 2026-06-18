//! Render the right-hand content pane for each view kind.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Padding, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, View};

/// Render the status footer (one line at the bottom).
pub fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let proj = app.project_name.as_deref().unwrap_or("(no project)");
    let line = if app.status_line.is_empty() {
        Line::from(vec![
            Span::styled(format!(" session: {} ", app.session_name), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("· project: {} ", proj), Style::default().fg(Color::DarkGray)),
            Span::styled(
                "· q quit  ^P palette  ↑↓ nav  Enter activate",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(app.status_line.clone(), Style::default().fg(Color::Yellow)),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let title = app.view.title();
    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::uniform(1))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    match &app.view {
        View::Chat => render_chat(f, app, inner),
        View::Tasks => render_tasks(f, app, inner),
        View::Review => render_review(f, app, inner),
        View::Machines => render_machines(f, app, inner),
        View::Agent(id) => render_agent(f, app, inner, id),
    }
}

fn render_chat(f: &mut Frame, app: &App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(area);

    // Transcript: live capture of the orchestrator pane, or a setup hint.
    let snap = strip_ansi(&app.pane_snapshot);
    let transcript = if app.project_name.is_none() {
        Paragraph::new(vec![
            Line::from(Span::styled(
                "no project loaded in this session",
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from("set one up with:"),
            Line::from(""),
            Line::from(Span::styled(
                "  cd /path/to/your/repo",
                Style::default().fg(Color::Cyan),
            )),
            Line::from(Span::styled(
                "  shelbi init --project myapp",
                Style::default().fg(Color::Cyan),
            )),
            Line::from(""),
            Line::from("then add it to your session at ~/.shelbi/sessions/default.yaml:"),
            Line::from(Span::styled(
                "  projects:",
                Style::default().fg(Color::Cyan),
            )),
            Line::from(Span::styled(
                "    - { name: myapp, machines: [hub] }",
                Style::default().fg(Color::Cyan),
            )),
            Line::from(""),
            Line::from("press q to exit, fix the config, and relaunch."),
        ])
    } else if snap.trim().is_empty() {
        let project = app.project_name.as_deref().unwrap_or("");
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("orchestrator not running for `{project}`"),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(""),
            Line::from("start it from another terminal:"),
            Line::from(""),
            Line::from(Span::styled(
                format!("  shelbi --project {project} orchestrate"),
                Style::default().fg(Color::Cyan),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "then come back here — the chat below will reach it.",
                Style::default().fg(Color::DarkGray),
            )),
        ])
    } else {
        let lines: Vec<Line> = snap.lines().map(|l| Line::from(l.to_string())).collect();
        let take = lines.len().saturating_sub(layout[0].height as usize);
        Paragraph::new(lines[take..].to_vec()).wrap(Wrap { trim: false })
    };
    f.render_widget(transcript, layout[0]);

    // Input.
    let input = Paragraph::new(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(app.chat_input.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]))
    .block(
        Block::default()
            .borders(Borders::TOP)
            .padding(Padding::new(0, 0, 1, 0)),
    );
    f.render_widget(input, layout[1]);
}

fn render_tasks(f: &mut Frame, app: &App, area: Rect) {
    let mut items: Vec<ListItem> = Vec::new();
    if app.agents.is_empty() {
        items.push(ListItem::new(Span::styled(
            "(no tasks yet — run `shelbi spawn` or use Chat to ask the orchestrator)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for a in &app.agents {
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", a.status.glyph()),
                    Style::default().fg(status_color(a.status)),
                ),
                Span::raw(format!("{:<24}", a.id)),
                Span::styled(
                    format!(" {:<10}", a.machine),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!(" {}", a.branch),
                    Style::default().fg(Color::DarkGray),
                ),
            ])));
        }
    }
    f.render_widget(List::new(items), area);
}

fn render_review(f: &mut Frame, app: &App, area: Rect) {
    let pending: Vec<_> = app
        .agents
        .iter()
        .filter(|a| {
            matches!(
                a.status,
                shelbi_core::Status::Done | shelbi_core::Status::Waiting
            )
        })
        .collect();
    let mut items: Vec<ListItem> = Vec::new();
    if pending.is_empty() {
        items.push(ListItem::new(Span::styled(
            "(no agents waiting for review)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for a in pending {
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", a.status.glyph()),
                    Style::default().fg(status_color(a.status)),
                ),
                Span::raw(format!("{:<24}", a.id)),
                Span::styled(
                    format!("  {}", a.branch),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("   "),
                Span::styled(
                    "[d]iff  [m]erge  [P]R  [x] archive",
                    Style::default().fg(Color::DarkGray),
                ),
            ])));
        }
    }
    f.render_widget(List::new(items), area);
}

fn render_machines(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    let project_name = app.project_name.as_deref().unwrap_or("(no project)");
    lines.push(Line::from(vec![
        Span::styled("project: ", Style::default().fg(Color::DarkGray)),
        Span::raw(project_name.to_string()),
    ]));
    lines.push(Line::from(""));
    if let Some(p) = &app.project_name {
        match shelbi_state::load_project(p) {
            Ok(project) => {
                for m in &project.machines {
                    let kind = match m.kind {
                        shelbi_core::MachineKind::Local => "local".to_string(),
                        shelbi_core::MachineKind::Ssh => {
                            format!("ssh {}", m.host.as_deref().unwrap_or(&m.name))
                        }
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  ● ", Style::default().fg(Color::Green)),
                        Span::raw(format!("{:<10}", m.name)),
                        Span::styled(format!(" {:<14}", kind), Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!(" {}", m.work_dir.display()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
            Err(e) => {
                lines.push(Line::from(Span::styled(
                    format!("(couldn't load project: {e})"),
                    Style::default().fg(Color::Red),
                )));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn render_agent(f: &mut Frame, app: &App, area: Rect, id: &str) {
    let Some(a) = app.agents.iter().find(|a| a.id == id) else {
        let p = Paragraph::new(Span::styled(
            format!("agent `{id}` no longer exists"),
            Style::default().fg(Color::Red),
        ))
        .alignment(Alignment::Left);
        f.render_widget(p, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(1)])
        .split(area);

    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("status:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} {:?}", a.status.glyph(), a.status),
                Style::default().fg(status_color(a.status)),
            ),
        ]),
        Line::from(vec![
            Span::styled("machine:  ", Style::default().fg(Color::DarkGray)),
            Span::raw(a.machine.clone()),
            Span::styled("   runner: ", Style::default().fg(Color::DarkGray)),
            Span::raw(a.runner.clone()),
            Span::styled("   branch: ", Style::default().fg(Color::DarkGray)),
            Span::raw(a.branch.clone()),
        ]),
        Line::from(vec![
            Span::styled("worktree: ", Style::default().fg(Color::DarkGray)),
            Span::raw(a.worktree.display().to_string()),
        ]),
        Line::from(vec![
            Span::styled("tmux:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}:{}", a.tmux.session, a.tmux.window)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "─── live tail ───",
            Style::default().fg(Color::DarkGray),
        )),
    ]);
    f.render_widget(header, layout[0]);

    let snap = strip_ansi(&app.pane_snapshot);
    let lines: Vec<Line> = if snap.trim().is_empty() {
        vec![Line::from(Span::styled(
            "(pane idle / not yet captured)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let v: Vec<Line> = snap.lines().map(|l| Line::from(l.to_string())).collect();
        let take = v.len().saturating_sub(layout[1].height as usize);
        v[take..].to_vec()
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), layout[1]);
}

fn strip_ansi(s: &str) -> String {
    String::from_utf8(strip_ansi_escapes::strip(s.as_bytes()))
        .unwrap_or_else(|_| s.to_string())
}

fn status_color(s: shelbi_core::Status) -> Color {
    use shelbi_core::Status::*;
    match s {
        Running => Color::Green,
        Waiting => Color::Yellow,
        Queued => Color::Blue,
        Done => Color::Cyan,
        Error => Color::Red,
        Archived => Color::DarkGray,
    }
}
