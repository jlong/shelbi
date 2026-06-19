//! Kanban Tasks view — 5 columns (backlog / todo / in_progress / review /
//! done), rendered into the dashboard's right pane via the same hidden-pane
//! swap mechanism the other built-in views use. State is read from / written
//! to the task markdown files via `shelbi_state`; no separate cache.
//!
//! The pane is meant to outlive any one TUI process (the parent shell wraps
//! it in a `while true` loop) — so we deliberately don't bind a quit key.
//! Switching away is the palette's job.

use std::time::{Duration, Instant};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
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
        }
    }

    pub fn column(&self, idx: usize) -> Column {
        Column::ALL[idx.min(Column::ALL.len() - 1)]
    }

    pub fn column_tasks(&self, col_idx: usize) -> Vec<&TaskFile> {
        let col = self.column(col_idx);
        self.tasks.iter().filter(|tf| tf.task.column == col).collect()
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
        if let Err(e) = shelbi_state::move_task(&self.project_name, id, new_col) {
            self.status_line = format!("move failed: {e}");
            return;
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

pub fn render_full(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // columns
            Constraint::Length(2), // footer
        ])
        .split(area);

    render_title(f, app, outer[0]);
    render_columns(f, app, outer[1]);
    render_footer(f, app, outer[2]);
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

fn render_columns(f: &mut Frame, app: &KanbanApp, area: Rect) {
    // Equal-width columns. With 5 columns at 20% each you get good
    // proportions down to ~50 cols wide; below that things squeeze, but
    // ratatui will still render — just less readable.
    let slots = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 5); 5])
        .split(area);

    for (i, slot) in slots.iter().enumerate() {
        render_column(f, app, i, *slot);
    }
}

fn render_column(f: &mut Frame, app: &KanbanApp, col_idx: usize, area: Rect) {
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

    // Reserve a 2-char right gutter so adjacent cells don't visually
    // collide; List doesn't clip Line spans on its own.
    let max_text = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = Vec::with_capacity(tasks.len());
    for (row, tf) in tasks.iter().enumerate() {
        let title = truncate(&tf.task.title, max_text);
        let mut lines = vec![Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))];
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

    let block = Block::default().borders(Borders::NONE).title(title_line);
    let mut state = ListState::default();
    if focused && !tasks.is_empty() {
        state.select(Some(app.selected_row));
    }
    let list = List::new(items).block(block);
    f.render_stateful_widget(list, area, &mut state);
}

fn render_footer(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let keys = Line::from(Span::styled(
        "  h/l col   j/k row   H/L move col   K/J reorder   r refresh",
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
