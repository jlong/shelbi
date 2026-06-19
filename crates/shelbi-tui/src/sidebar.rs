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

use crate::app::{App, Row, WorkerBadge};

pub fn render_full(f: &mut Frame, app: &mut App, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(1)
        .constraints([Constraint::Min(1), Constraint::Length(2)])
        .split(area);

    render_list(f, app, outer[0]);
    render_footer(f, app, outer[1]);
}

fn render_list(f: &mut Frame, app: &mut App, area: Rect) {
    // Project-name header — strong color, blank line below for breathing
    // room. The wireframe is the spec: the project is the context, so no
    // 'shelbi' brand chrome.
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
    let width = inner.width as usize;
    let rows = app.rows();
    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let selected = i == app.sidebar_index && row.is_selectable();
        items.push(render_row(row, selected, width));
    }

    let mut state = ListState::default();
    state.select(Some(app.sidebar_index));
    // Full-row dark-gray fill on the selected row. fg/bold for the
    // selected text is set per-span in render_row so the contrast against
    // the new bg is explicit.
    let list = List::new(items).highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_row(row: &Row, selected: bool, width: usize) -> ListItem<'static> {
    match row {
        Row::Nav { icon, label, .. } => {
            let style = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(Line::from(vec![Span::styled(
                format!("{icon} {label}"),
                style,
            )]))
        }
        Row::Section { label } => {
            let style = Style::default().fg(Color::DarkGray);
            ListItem::new(Line::from(vec![Span::styled(
                format!("— {label} —"),
                style,
            )]))
        }
        Row::Blank => ListItem::new(Line::raw("")),
        Row::Worker { name, badge, .. } => {
            let mut spans = vec![
                Span::styled(
                    format!("{} ", badge.glyph()),
                    Style::default().fg(worker_badge_color(*badge)),
                ),
                Span::styled(name.clone(), name_style(selected)),
            ];
            // No machine badge here — the agents list reflects the worker
            // pool, not per-row metadata. Machine assignment surfaces on
            // the Machines view.
            if selected {
                spans = bold(spans);
            }
            ListItem::new(Line::from(spans))
        }
        Row::Review { title, worker, .. } => {
            // Cyan ✓: the task is review-ready, awaiting human action.
            // Mirrors the WorkerBadge::ReviewReady glyph so the worker
            // and its review row share a vocabulary.
            let badge = Span::styled(
                "✓ ".to_string(),
                Style::default().fg(Color::Cyan),
            );
            let title_span = Span::styled(title.clone(), name_style(selected));
            let worker_label = worker.clone().unwrap_or_default();
            let line = right_align(
                vec![badge, title_span],
                worker_label,
                Style::default().fg(Color::DarkGray),
                width,
            );
            ListItem::new(if selected { Line::from(bold(line)) } else { Line::from(line) })
        }
        Row::LegacyAgent {
            id,
            machine,
            status,
            ..
        } => {
            let badge = Span::styled(
                format!("{} ", status.glyph()),
                Style::default().fg(status_color(*status)),
            );
            let id_span = Span::styled(id.clone(), name_style(selected));
            let line = right_align(
                vec![badge, id_span],
                machine.clone(),
                Style::default().fg(Color::DarkGray),
                width,
            );
            ListItem::new(if selected { Line::from(bold(line)) } else { Line::from(line) })
        }
    }
}

fn name_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn bold(mut spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    for s in &mut spans {
        s.style = s.style.add_modifier(Modifier::BOLD);
    }
    spans
}

/// Pad `left` so a right-aligned `right` ends at `width`. Right column
/// dims to DarkGray. If there isn't room for both, the right column is
/// dropped rather than pushing the title off-screen.
fn right_align(
    left: Vec<Span<'static>>,
    right: String,
    right_style: Style,
    width: usize,
) -> Vec<Span<'static>> {
    let left_w: usize = left.iter().map(|s| display_width(&s.content)).sum();
    let right_w = display_width(&right);
    // Need at least one space between the two columns.
    if right.is_empty() || left_w + right_w + 1 > width {
        return left;
    }
    let pad = width.saturating_sub(left_w + right_w);
    let mut out = left;
    out.push(Span::raw(" ".repeat(pad)));
    out.push(Span::styled(right, right_style));
    out
}

/// Cheap display-width estimate — counts chars rather than full Unicode
/// width. Enough for the labels we use; multi-cell emojis are rare in
/// these strings.
fn display_width(s: &str) -> usize {
    s.chars().count()
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let lines = if app.status_line.is_empty() {
        vec![
            Line::from(Span::styled(
                "^P palette  Enter focus",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                "q  quit shelbi",
                Style::default().fg(Color::DarkGray),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                app.status_line.clone(),
                Style::default().fg(Color::Yellow),
            )),
            Line::from(Span::styled(
                "^P palette  q quit",
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
