//! Kanban Tasks view — 5 columns (backlog / todo / in_progress / review /
//! done), rendered into the dashboard's right pane via the same hidden-pane
//! swap mechanism the other built-in views use. State is read from / written
//! to the task markdown files via `shelbi_state`; no separate cache.
//!
//! The pane is meant to outlive any one TUI process (the parent shell wraps
//! it in a `while true` loop) — so we deliberately don't bind a quit key.
//! Switching away is the palette's job.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use shelbi_core::Column;
use shelbi_state::TaskFile;

/// State for the Kanban TUI. Selection is `(column, row)` — `row` is the
/// index inside the currently focused column.
pub struct KanbanApp {
    pub project_name: String,
    pub tasks: Vec<TaskFile>,
    pub selected_column: usize,
    pub selected_row: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    /// When `Some`, a modal task detail popover is open for the given task id.
    /// Selection underneath stays put so closing the popover returns the
    /// cursor to the same card.
    pub popover: Option<TaskPopover>,
    /// Screen-space rects for each rendered card cell, written each frame
    /// by the renderer and read by the mouse-click handler to map a click
    /// back to a (column, row) pair.
    pub card_hits: Vec<CardHit>,
}

/// One rendered card's screen-space rectangle and its column/row index in
/// the kanban model. Recorded each frame so click handling and keyboard
/// selection share the same source of truth for which card sits where.
#[derive(Clone, Copy, Debug)]
pub struct CardHit {
    pub area: Rect,
    pub col_idx: usize,
    pub row_idx: usize,
}

/// State for the open task detail popover. We key by task id (not column/row
/// indices) so a background refresh that reorders cards doesn't swap the
/// popover's contents under the user.
pub struct TaskPopover {
    pub task_id: String,
    pub scroll: u16,
}

impl KanbanApp {
    pub fn new(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            tasks: Vec::new(),
            selected_column: 0,
            selected_row: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            popover: None,
            card_hits: Vec::new(),
        }
    }

    /// Hit-test a screen coordinate against the most recently rendered cards.
    /// Returns `(col_idx, row_idx)` for the card under the point, or `None`
    /// if the click missed every card (including clicks on column headers,
    /// the footer, or empty space below the last card).
    pub fn card_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        self.card_hits.iter().find_map(|hit| {
            let r = hit.area;
            let in_x = x >= r.x && x < r.x.saturating_add(r.width);
            let in_y = y >= r.y && y < r.y.saturating_add(r.height);
            (in_x && in_y).then_some((hit.col_idx, hit.row_idx))
        })
    }

    /// Move selection to the given card and open its popover. Used by the
    /// click handler so it routes through the same path as ENTER/SPACE.
    pub fn open_popover_at(&mut self, col_idx: usize, row_idx: usize) {
        self.selected_column = col_idx;
        self.selected_row = row_idx;
        self.open_popover();
    }

    pub fn popover_is_open(&self) -> bool {
        self.popover.is_some()
    }

    /// Look up the task currently shown in the popover, if any. Returns
    /// `None` if the popover is closed OR if the task has since vanished
    /// from disk (e.g. deleted by another process between refreshes).
    pub fn popover_task(&self) -> Option<&TaskFile> {
        let id = &self.popover.as_ref()?.task_id;
        self.tasks.iter().find(|tf| &tf.task.id == id)
    }

    /// Open the popover for the currently selected card. No-op if the
    /// focused column is empty.
    pub fn open_popover(&mut self) {
        let Some(tf) = self.selected_task() else {
            return;
        };
        self.popover = Some(TaskPopover {
            task_id: tf.task.id.clone(),
            scroll: 0,
        });
    }

    pub fn close_popover(&mut self) {
        self.popover = None;
    }

    pub fn popover_scroll_up(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_sub(1);
        }
    }

    pub fn popover_scroll_down(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_add(1);
        }
    }

    pub fn popover_scroll_page_up(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_sub(10);
        }
    }

    pub fn popover_scroll_page_down(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_add(10);
        }
    }

    pub fn popover_scroll_home(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = 0;
        }
    }

    pub fn column(&self, idx: usize) -> Column {
        Column::ALL[idx.min(Column::ALL.len() - 1)]
    }

    pub fn column_tasks(&self, col_idx: usize) -> Vec<&TaskFile> {
        let col = self.column(col_idx);
        let mut tasks: Vec<&TaskFile> =
            self.tasks.iter().filter(|tf| tf.task.column == col).collect();
        // Done shows most-recently-completed first: `updated_at` is rewritten
        // on the move-into-done write, so it's a stable proxy for completion
        // time. Other columns keep their priority order (the natural order of
        // `self.tasks`). Stable sort → equal timestamps preserve that order,
        // so identical-state polls don't reshuffle / flicker.
        if col == Column::Done {
            tasks.sort_by(|a, b| b.task.updated_at.cmp(&a.task.updated_at));
        }
        tasks
    }

    /// Snapshot of every task's column, for the blocked derivation. Built
    /// each render call from the in-memory tasks slice — cheap and avoids
    /// a stale cache.
    fn task_columns(&self) -> HashMap<String, Column> {
        self.tasks
            .iter()
            .map(|tf| (tf.task.id.clone(), tf.task.column))
            .collect()
    }

    pub fn selected_task(&self) -> Option<&TaskFile> {
        let col_tasks = self.column_tasks(self.selected_column);
        col_tasks.get(self.selected_row).copied()
    }

    pub fn refresh(&mut self) {
        match shelbi_state::list_tasks(&self.project_name) {
            Ok(tasks) => {
                self.tasks = tasks;
                self.last_refresh = Instant::now();
                self.clamp_selection();
            }
            Err(e) => {
                self.status_line = format!("refresh failed: {e}");
            }
        }
    }

    pub fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_millis(750) {
            self.refresh();
        }
    }

    /// Make sure `selected_row` is in range for the current column. Pulls
    /// the cursor up to the last row if the column shrunk; leaves it at 0
    /// if the column is empty.
    pub fn clamp_selection(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if n == 0 {
            self.selected_row = 0;
        } else if self.selected_row >= n {
            self.selected_row = n - 1;
        }
    }

    pub fn nav_left(&mut self) {
        if self.selected_column == 0 {
            self.selected_column = Column::ALL.len() - 1;
        } else {
            self.selected_column -= 1;
        }
        self.clamp_selection();
    }

    pub fn nav_right(&mut self) {
        self.selected_column = (self.selected_column + 1) % Column::ALL.len();
        self.clamp_selection();
    }

    pub fn nav_up(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if n == 0 {
            return;
        }
        self.selected_row = if self.selected_row == 0 {
            n - 1
        } else {
            self.selected_row - 1
        };
    }

    pub fn nav_down(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if n == 0 {
            return;
        }
        self.selected_row = (self.selected_row + 1) % n;
    }

    /// Shove the selected card one column to the left, wrapping at backlog.
    /// Keeps focus on the moved card in its new column.
    pub fn move_card_left(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let new_col_idx = if self.selected_column == 0 {
            Column::ALL.len() - 1
        } else {
            self.selected_column - 1
        };
        self.move_card(&id, new_col_idx);
    }

    pub fn move_card_right(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let new_col_idx = (self.selected_column + 1) % Column::ALL.len();
        self.move_card(&id, new_col_idx);
    }

    fn move_card(&mut self, id: &str, new_col_idx: usize) {
        let new_col = self.column(new_col_idx);
        match shelbi_state::move_task(&self.project_name, id, new_col) {
            Ok(Some((from, to))) => {
                if let Err(e) = shelbi_state::append_task_event(id, from, to, "user:tui") {
                    tracing::warn!(task = %id, error = %e, "append_task_event failed");
                }
            }
            Ok(None) => {}
            Err(e) => {
                self.status_line = format!("move failed: {e}");
                return;
            }
        }
        self.status_line = format!("{id} → {new_col}");
        self.refresh();
        // Follow the card.
        self.selected_column = new_col_idx;
        if let Some(row) = self
            .column_tasks(new_col_idx)
            .iter()
            .position(|tf| tf.task.id == id)
        {
            self.selected_row = row;
        }
    }

    /// Bump selection up one slot inside its column (lower priority number).
    pub fn reorder_up(&mut self) {
        if self.selected_row == 0 {
            return;
        }
        self.reorder(self.selected_row - 1);
    }

    pub fn reorder_down(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if self.selected_row + 1 >= n {
            return;
        }
        self.reorder(self.selected_row + 1);
    }

    fn reorder(&mut self, new_pos: usize) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        if let Err(e) = shelbi_state::set_task_priority(&self.project_name, &id, new_pos as u32) {
            self.status_line = format!("reorder failed: {e}");
            return;
        }
        self.refresh();
        self.selected_row = new_pos;
    }
}

// ---------------------------------------------------------------------------
// Rendering

pub fn render_full(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // columns
            Constraint::Length(2), // footer
        ])
        .split(area);

    let mut hits: Vec<CardHit> = Vec::new();
    render_title(f, app, outer[0]);
    render_columns(f, app, &mut hits, outer[1]);
    render_footer(f, app, outer[2]);
    app.card_hits = hits;

    if app.popover_is_open() {
        render_popover(f, app, area);
    }
}

fn render_title(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let total = app.tasks.len();
    let line = Line::from(vec![
        Span::styled(
            "Tasks · ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            app.project_name.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {total} total"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_columns(f: &mut Frame, app: &KanbanApp, hits: &mut Vec<CardHit>, area: Rect) {
    // Equal-width columns. With 5 columns at 20% each you get good
    // proportions down to ~50 cols wide; below that things squeeze, but
    // ratatui will still render — just less readable.
    let slots = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 5); 5])
        .split(area);

    let columns = app.task_columns();
    for (i, slot) in slots.iter().enumerate() {
        render_column(f, app, i, *slot, &columns, hits);
    }
}

fn render_column(
    f: &mut Frame,
    app: &KanbanApp,
    col_idx: usize,
    area: Rect,
    columns: &HashMap<String, Column>,
    hits: &mut Vec<CardHit>,
) {
    let column = app.column(col_idx);
    let tasks = app.column_tasks(col_idx);
    let focused = col_idx == app.selected_column;

    let header_style = if focused {
        Style::default()
            .fg(column_color(column))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(column_color(column))
    };
    let title_line = Line::from(vec![
        Span::styled(column_label(column), header_style),
        Span::styled(
            format!(" ({})", tasks.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    // Manual title/list split — keeps hit-test geometry unambiguous (no
    // dependence on Block.inner behavior with title + Borders::NONE).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let header_area = chunks[0];
    let list_area = chunks[1];

    f.render_widget(Paragraph::new(title_line), header_area);

    // Reserve a 2-char right gutter so adjacent cells don't visually
    // collide; List doesn't clip Line spans on its own.
    let max_text = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = Vec::with_capacity(tasks.len());
    for (row, tf) in tasks.iter().enumerate() {
        let blocked = tf.task.is_blocked(columns);
        // 2-char prefix when blocked so the badge can't visually melt into
        // the title text on narrow columns. Lock glyph stays in the same
        // slot whether or not other rows show one.
        let badge_prefix = if blocked { "🔒 " } else { "" };
        let title_text = truncate(&format!("{badge_prefix}{}", tf.task.title), max_text);
        let title_style = if blocked {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let mut lines = vec![Line::from(Span::styled(title_text, title_style))];
        let id_w = tf.task.id.chars().count();
        let worker_w = tf
            .task
            .assigned_to
            .as_deref()
            .map(|w| 3 + w.chars().count())
            .unwrap_or(0);
        let meta_line = if id_w + worker_w <= max_text {
            let mut spans = vec![Span::styled(
                tf.task.id.clone(),
                Style::default().fg(Color::DarkGray),
            )];
            if let Some(w) = &tf.task.assigned_to {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("@{w}"),
                    Style::default().fg(Color::Magenta),
                ));
            }
            Line::from(spans)
        } else {
            Line::from(Span::styled(
                truncate(&tf.task.id, max_text),
                Style::default().fg(Color::DarkGray),
            ))
        };
        lines.push(meta_line);
        lines.push(Line::raw(""));

        let mut item = ListItem::new(lines);
        if focused && row == app.selected_row {
            item = item.style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
        }
        items.push(item);
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        ))));
    }

    let mut state = ListState::default();
    if focused && !tasks.is_empty() {
        state.select(Some(app.selected_row));
    }
    let list = List::new(items);
    f.render_stateful_widget(list, list_area, &mut state);

    // Each card item renders 3 lines (title, meta, blank). Clip the last
    // card if it overflows the visible list area so clicks far below it
    // don't count. Scroll offsets aren't tracked here — a long column that
    // scrolls past its visible area will mis-attribute clicks for any row
    // off-screen; tolerable until columns regularly exceed visible height.
    const ROWS_PER_CARD: u16 = 3;
    let list_bottom = list_area.y.saturating_add(list_area.height);
    for (row, _) in tasks.iter().enumerate() {
        let card_top = list_area.y.saturating_add(row as u16 * ROWS_PER_CARD);
        if card_top >= list_bottom {
            break;
        }
        let card_height = (list_bottom - card_top).min(ROWS_PER_CARD);
        hits.push(CardHit {
            area: Rect {
                x: list_area.x,
                y: card_top,
                width: list_area.width,
                height: card_height,
            },
            col_idx,
            row_idx: row,
        });
    }
}

fn render_footer(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let keys = Line::from(Span::styled(
        "  h/l col   j/k row   enter/␣ open   H/L move col   K/J reorder   r refresh",
        Style::default().fg(Color::DarkGray),
    ));
    let status = if app.status_line.is_empty() {
        Line::raw("")
    } else {
        Line::from(Span::styled(
            format!("  {}", app.status_line),
            Style::default().fg(Color::Yellow),
        ))
    };
    f.render_widget(Paragraph::new(vec![keys, status]), area);
}

fn column_label(c: Column) -> &'static str {
    match c {
        Column::Backlog => "BACKLOG",
        Column::Todo => "TO DO",
        Column::InProgress => "IN PROGRESS",
        Column::Review => "REVIEW",
        Column::Done => "DONE",
    }
}

fn column_color(c: Column) -> Color {
    match c {
        Column::Backlog => Color::Gray,
        Column::Todo => Color::Blue,
        Column::InProgress => Color::Yellow,
        Column::Review => Color::Magenta,
        Column::Done => Color::Green,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Task detail popover

fn render_popover(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let popover_area = centered_rect(80, 80, area);

    // Clear underneath so the kanban columns don't bleed through.
    f.render_widget(Clear, popover_area);

    let columns = app.task_columns();
    let (header_lines, body_text, title) = match app.popover_task() {
        Some(tf) => (
            popover_header(tf, &columns),
            tf.body.clone(),
            tf.task.title.clone(),
        ),
        None => (
            Vec::new(),
            "(task no longer exists — press esc to close)".to_string(),
            "Missing task".to_string(),
        ),
    };

    let title_text = truncate(&title, popover_area.width.saturating_sub(4) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                title_text,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(popover_area);
    f.render_widget(block, popover_area);

    // Layout inside the border: header (fixed height) → separator → body
    // (scrollable) → footer (hint line).
    let header_height = header_lines.len() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Length(1), // separator
            Constraint::Min(1),    // body
            Constraint::Length(1), // hint
        ])
        .split(inner);

    if !header_lines.is_empty() {
        f.render_widget(Paragraph::new(header_lines), chunks[0]);
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(chunks[1].width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[1],
    );

    let scroll = app.popover.as_ref().map(|p| p.scroll).unwrap_or(0);
    let body = Paragraph::new(body_text)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(body, chunks[2]);

    let hint = Line::from(Span::styled(
        "  esc/enter/␣  close      j/k or ↑/↓  scroll      g  top",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(hint), chunks[3]);
}

fn popover_header(tf: &TaskFile, columns: &HashMap<String, Column>) -> Vec<Line<'static>> {
    let task = &tf.task;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(meta_row("id", &task.id));
    let col_label = column_label(task.column);
    let col_span = Span::styled(col_label.to_string(), Style::default().fg(column_color(task.column)));
    let mut col_line = vec![meta_label("column"), col_span];
    col_line.push(Span::raw("   "));
    col_line.push(meta_label("priority"));
    col_line.push(Span::raw(format!("{}", task.priority)));
    if let Some(w) = &task.assigned_to {
        col_line.push(Span::raw("   "));
        col_line.push(meta_label("worker"));
        col_line.push(Span::styled(
            format!("@{w}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    lines.push(Line::from(col_line));

    if let Some(branch) = &task.branch {
        lines.push(meta_row("branch", branch));
    }

    if !task.depends_on.is_empty() {
        let blocked = task.is_blocked(columns);
        let label = if blocked { "blocked by" } else { "depends on" };
        let mut spans: Vec<Span<'static>> = vec![meta_label(label)];
        for (i, dep) in task.depends_on.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(", "));
            }
            let dep_col = columns.get(dep).copied();
            let label_text = match dep_col {
                Some(c) => format!("{dep} [{}]", c.as_str()),
                None => format!("{dep} [missing]"),
            };
            let color = match dep_col {
                Some(c) => column_color(c),
                None => Color::Red,
            };
            spans.push(Span::styled(label_text, Style::default().fg(color)));
        }
        lines.push(Line::from(spans));
    } else {
        // Only mention readiness when a user might wonder — i.e. never for
        // tasks with no deps. Skipping this keeps the header compact.
    }

    let created = task.created_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let updated = task.updated_at.format("%Y-%m-%d %H:%M UTC").to_string();
    lines.push(Line::from(vec![
        meta_label("created"),
        Span::raw(created),
        Span::raw("   "),
        meta_label("updated"),
        Span::raw(updated),
    ]));

    lines
}

fn meta_label(label: &str) -> Span<'static> {
    Span::styled(
        format!("{label}: "),
        Style::default().fg(Color::DarkGray),
    )
}

fn meta_row(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![meta_label(label), Span::raw(value.to_string())])
}

/// Return a `Rect` centered in `area` whose width and height are the given
/// percentages of the outer rect. Used as the bounds for the modal popover.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(area: Rect, col_idx: usize, row_idx: usize) -> CardHit {
        CardHit { area, col_idx, row_idx }
    }

    fn task_file(id: &str, column: Column, priority: u32, updated: &str) -> TaskFile {
        let updated_at = chrono::DateTime::parse_from_rfc3339(updated)
            .unwrap()
            .with_timezone(&chrono::Utc);
        TaskFile {
            task: shelbi_core::Task {
                id: id.to_string(),
                title: id.to_string(),
                column,
                priority,
                assigned_to: None,
                branch: None,
                depends_on: Vec::new(),
                created_at: updated_at,
                updated_at,
            },
            body: String::new(),
        }
    }

    const DONE_IDX: usize = 4;
    const BACKLOG_IDX: usize = 0;

    #[test]
    fn done_column_orders_newest_completed_first() {
        let mut app = KanbanApp::new("demo");
        // Insert in priority order; updated_at is intentionally out of order.
        app.tasks = vec![
            task_file("old", Column::Done, 0, "2026-06-20T10:00:00Z"),
            task_file("newest", Column::Done, 1, "2026-06-22T10:00:00Z"),
            task_file("middle", Column::Done, 2, "2026-06-21T10:00:00Z"),
        ];
        let ids: Vec<&str> = app
            .column_tasks(DONE_IDX)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["newest", "middle", "old"]);
    }

    #[test]
    fn non_done_columns_keep_priority_order() {
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![
            task_file("a", Column::Backlog, 0, "2026-06-20T10:00:00Z"),
            task_file("b", Column::Backlog, 1, "2026-06-22T10:00:00Z"),
            task_file("c", Column::Backlog, 2, "2026-06-21T10:00:00Z"),
        ];
        let ids: Vec<&str> = app
            .column_tasks(BACKLOG_IDX)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn done_sort_is_stable_for_equal_timestamps() {
        let mut app = KanbanApp::new("demo");
        // Equal updated_at → stable sort preserves the priority order, so a
        // re-poll of the same state never reshuffles (no flicker).
        app.tasks = vec![
            task_file("first", Column::Done, 0, "2026-06-20T10:00:00Z"),
            task_file("second", Column::Done, 1, "2026-06-20T10:00:00Z"),
            task_file("third", Column::Done, 2, "2026-06-20T10:00:00Z"),
        ];
        let ids: Vec<&str> = app
            .column_tasks(DONE_IDX)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["first", "second", "third"]);
    }

    #[test]
    fn card_at_returns_first_matching_hit() {
        let mut app = KanbanApp::new("demo");
        app.card_hits = vec![
            hit(Rect { x: 0, y: 2, width: 20, height: 3 }, 0, 0),
            hit(Rect { x: 0, y: 5, width: 20, height: 3 }, 0, 1),
            hit(Rect { x: 20, y: 2, width: 20, height: 3 }, 1, 0),
        ];
        assert_eq!(app.card_at(5, 2), Some((0, 0)));
        assert_eq!(app.card_at(5, 4), Some((0, 0)));   // last row of card
        assert_eq!(app.card_at(5, 5), Some((0, 1)));   // first row of next card
        assert_eq!(app.card_at(25, 3), Some((1, 0)));  // adjacent column
    }

    #[test]
    fn card_at_misses_outside_any_rect() {
        let mut app = KanbanApp::new("demo");
        app.card_hits = vec![hit(Rect { x: 0, y: 2, width: 20, height: 3 }, 0, 0)];
        // Above the card.
        assert_eq!(app.card_at(5, 1), None);
        // Below the card.
        assert_eq!(app.card_at(5, 5), None);
        // Right of the card.
        assert_eq!(app.card_at(20, 3), None);
        // Empty hit list.
        app.card_hits.clear();
        assert_eq!(app.card_at(5, 3), None);
    }

    #[test]
    fn open_popover_at_moves_selection_and_opens() {
        let mut app = KanbanApp::new("demo");
        // No tasks → open_popover is a no-op even when called via the click
        // path, so the popover stays closed. Selection still moves.
        app.open_popover_at(2, 4);
        assert_eq!(app.selected_column, 2);
        assert_eq!(app.selected_row, 4);
        assert!(!app.popover_is_open());
    }

    /// Driving `move_card` should both persist the column change and append
    /// a `task=...` line to `~/.shelbi/events.log` so the orchestrator's
    /// live event tail picks up board nudges from the TUI.
    #[test]
    fn move_card_appends_user_tui_event() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-move-card-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        use chrono::Utc;
        let now = Utc::now();
        let task = shelbi_core::Task {
            id: "fix-login".into(),
            title: "fix login".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            created_at: now,
            updated_at: now,
        };
        shelbi_state::save_task("demo", &task, "").unwrap();

        let mut app = KanbanApp::new("demo");
        app.refresh();
        // Place the cursor on `fix-login` in the todo column (index 1).
        app.selected_column = 1;
        app.selected_row = 0;
        app.move_card_right();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1, "log: {log:?}");
        assert!(lines[0].contains(" task=fix-login "), "line: {}", lines[0]);
        assert!(
            lines[0].contains(" todo -> in_progress "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].ends_with("reason=user:tui"), "line: {}", lines[0]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
