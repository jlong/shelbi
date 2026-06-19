//! Render the sidebar UI — fills the entire pane (it's the only thing
//! this process renders; the orchestrator and workers live in other tmux
//! panes / windows).

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::app::{App, WorkerBadge};

pub fn render_full(f: &mut Frame, app: &mut App, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(area);

    render_list(f, app, outer[0]);
    render_footer(f, app, outer[1]);
}

fn render_list(f: &mut Frame, app: &mut App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);

    let title = Paragraph::new(vec![
        Line::from(Span::styled(
            app.project_name.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ]);
    f.render_widget(title, layout[0]);

    let inner = layout[1];
    app.list_area = inner;
    let rows = app.rows();
    let mut items: Vec<ListItem> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut spans: Vec<Span> = Vec::new();
        if let Some(badge) = row.worker_badge {
            spans.push(Span::styled(
                format!("{} ", badge.glyph()),
                Style::default().fg(worker_badge_color(badge)),
            ));
        } else if let Some(status) = row.status {
            spans.push(Span::styled(
                format!("{} ", status.glyph()),
                Style::default().fg(status_color(status)),
            ));
        } else {
            spans.push(Span::styled("▶ ", Style::default().fg(Color::Cyan)));
        }
        spans.push(Span::raw(row.label.clone()));
        if let Some(b) = &row.badge {
            spans.push(Span::styled(
                format!("  {b}"),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let mut style = Style::default();
        if i == app.sidebar_index {
            style = style.bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD);
        }
        items.push(ListItem::new(Line::from(spans)).style(style));
    }
    if rows.len() <= 1 {
        items.push(ListItem::new(Span::styled(
            "(no agents yet — ask the orchestrator →)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let mut state = ListState::default();
    state.select(Some(app.sidebar_index));
    let list = List::new(items);
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let lines = if app.status_line.is_empty() {
        vec![
            Line::from(Span::styled(
                "  ^P palette  Enter focus",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "  q  quit shelbi",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                format!("  {}", app.status_line),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "  ^P palette  q quit",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    };
    f.render_widget(Paragraph::new(lines), area);
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

fn worker_badge_color(b: WorkerBadge) -> Color {
    match b {
        WorkerBadge::Working => Color::Green,
        WorkerBadge::AwaitingInput => Color::Yellow,
        WorkerBadge::AwaitingPermission => Color::Red,
        WorkerBadge::ReviewReady => Color::Cyan,
        WorkerBadge::Idle => Color::DarkGray,
    }
}
