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
use shelbi_state::ZenModeState;

use crate::app::{App, Row, WorkerBadge};

pub fn render_full(f: &mut Frame, app: &mut App, area: Rect) {
    // Footer grows from 2 lines (keymap + status_line) to 4 lines when the
    // green Zen pill is showing (keymap + 3-line pill). Keeps the list area
    // tight in the common no-pill case.
    let footer_height = if matches!(app.zen_mode, ZenModeState::On) {
        4
    } else {
        2
    };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(1)
        .constraints([Constraint::Min(1), Constraint::Length(footer_height)])
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
    if matches!(app.zen_mode, ZenModeState::On) {
        // Keymap line stays untouched; status_line is sacrificed for the
        // 3-line green pill underneath it.
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(3)])
            .split(area);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "^P palette  q quit",
                Style::default().fg(Color::DarkGray),
            ))),
            layout[0],
        );
        render_zen_pill(f, layout[1]);
        return;
    }
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

/// 3-line, ~11-column green pill anchored to the lower-left status block.
/// Box-drawing chars are drawn as text (not a `Block` border) so the entire
/// rectangle — corners, edges, and label — shares the same green fill.
fn render_zen_pill(f: &mut Frame, area: Rect) {
    const PILL_WIDTH: u16 = 11;
    const PILL_HEIGHT: u16 = 3;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width.min(PILL_WIDTH);
    let h = area.height.min(PILL_HEIGHT);
    let pill_area = Rect {
        x: area.x,
        y: area.y,
        width: w,
        height: h,
    };
    let style = Style::default()
        .bg(Color::Rgb(0, 200, 80))
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let inner = w.saturating_sub(2) as usize;
    let top = format!("┌{}┐", "─".repeat(inner));
    let bottom = format!("└{}┘", "─".repeat(inner));
    // Center " ZEN ON " inside the available inner width; if the cell is
    // narrower than 8 chars the label gets clipped — preferable to wrapping
    // onto a fresh row that would break the pill's box.
    let label = " ZEN ON ";
    let label_w = label.chars().count();
    let pad = inner.saturating_sub(label_w);
    let left_pad = pad / 2;
    let right_pad = pad - left_pad;
    let middle = format!(
        "│{}{}{}│",
        " ".repeat(left_pad),
        label,
        " ".repeat(right_pad)
    );
    let mut lines = Vec::with_capacity(h as usize);
    lines.push(Line::from(Span::styled(top, style)));
    if h >= 2 {
        lines.push(Line::from(Span::styled(middle, style)));
    }
    if h >= 3 {
        lines.push(Line::from(Span::styled(bottom, style)));
    }
    f.render_widget(Paragraph::new(lines), pill_area);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// When Zen is on, the bottom of the sidebar shows the keymap line
    /// followed by a 3-line pill carrying the " ZEN ON " label. The box
    /// chars (┌─┐│└┘) come from the renderer's own text — not a ratatui
    /// `Block` border — so the green fill covers the corners too.
    #[test]
    fn pill_renders_three_line_box_with_label_when_zen_on() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::On;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();

        let dump: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        let joined = dump.join("\n");
        assert!(
            joined.contains("ZEN ON"),
            "expected ZEN ON label in:\n{joined}"
        );
        assert!(joined.contains("┌─"), "expected pill top corner in:\n{joined}");
        assert!(joined.contains("└─"), "expected pill bottom corner in:\n{joined}");
        assert!(
            joined.contains("^P palette  q quit"),
            "footer keymap line must stay intact in:\n{joined}"
        );
    }

    /// When Zen is off, the pill disappears entirely — no green chrome,
    /// no leftover box characters. The footer collapses back to the
    /// 2-line keymap.
    #[test]
    fn pill_absent_when_zen_off() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Off;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();

        let dump: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        let joined = dump.join("\n");
        assert!(!joined.contains("ZEN"), "no ZEN label expected, got:\n{joined}");
        assert!(!joined.contains("┌─"), "no pill corner expected, got:\n{joined}");
    }

    /// Paused is *not* treated as on — the orchestrator might be draining
    /// in-flight work, but the pill is a binary visual signal. Showing it
    /// while paused would mislead the user.
    #[test]
    fn pill_absent_when_zen_paused() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Paused;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();

        let dump: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        let joined = dump.join("\n");
        assert!(!joined.contains("ZEN"), "paused must not show pill, got:\n{joined}");
    }
}
