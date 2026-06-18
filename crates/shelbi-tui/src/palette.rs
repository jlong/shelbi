//! ⌘K command palette overlay.
//!
//! Built on top of `shelbi-palette` for matching. This module owns the input
//! state, entry building, and rendering. Activation logic lives in `App`.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph},
    Frame,
};
use shelbi_palette::{Entry, EntryKind};

use crate::app::{App, View};

pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

impl PaletteState {
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            selected: 0,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.query.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
    }

    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
    }
}

/// Build the palette's entry catalog from the app's current state.
pub fn entries(app: &App) -> Vec<Entry> {
    let mut out = Vec::new();
    let nav = app.nav();
    // Main nav items.
    for row in nav.iter().take(4) {
        out.push(Entry {
            id: format!("view:{}", row.label.to_lowercase()),
            label: row.label.clone(),
            kind: EntryKind::View,
            subtitle: Some("nav".into()),
        });
    }
    // Agents (every active worker is fuzzy-findable).
    for a in &app.agents {
        out.push(Entry {
            id: format!("agent:{}", a.id),
            label: a.id.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!(
                "{} · {:?}",
                a.machine,
                a.status
            )),
        });
    }
    // Global actions.
    for (id, label, sub) in [
        ("action:new-task", "New task", "compose"),
        ("action:refresh", "Refresh state", "reload state files"),
        ("action:quit", "Quit shelbi", "Ctrl+C also works"),
    ] {
        out.push(Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::Action,
            subtitle: Some(sub.into()),
        });
    }
    out
}

pub fn render(f: &mut Frame, state: &PaletteState, results: &[(Entry, u16)]) {
    if !state.open {
        return;
    }
    let area = centered_rect(f.area(), 60, 22);
    f.render_widget(Clear, area);

    let outer = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::new(1, 1, 0, 0))
        .title(Span::styled(
            " ⌘K ",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        ));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    // Input row.
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(state.query.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[0]);

    // Result list.
    let items: Vec<ListItem> = results
        .iter()
        .map(|(e, _score)| {
            let mut spans = vec![
                Span::styled(format!(" {} ", e.kind.icon()), Style::default().fg(Color::DarkGray)),
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
}

/// Activate the selected palette entry. Returns true if the palette should
/// stay open (e.g. we need user follow-up) — false to close it.
pub fn activate(app: &mut App, entry: &Entry) -> bool {
    match entry.kind {
        EntryKind::View => {
            let label = entry.label.to_lowercase();
            let view = match label.as_str() {
                "chat" => View::Chat,
                "tasks" => View::Tasks,
                "review" => View::Review,
                "machines" => View::Machines,
                _ => return false,
            };
            // Sync sidebar_index to match the new view.
            for (i, row) in app.nav().iter().take(4).enumerate() {
                if row.view == view {
                    app.sidebar_index = i;
                    break;
                }
            }
            app.view = view;
            false
        }
        EntryKind::Agent => {
            let id = entry.label.clone();
            for (i, row) in app.nav().iter().enumerate() {
                if let View::Agent(a) = &row.view {
                    if a == &id {
                        app.sidebar_index = i;
                        break;
                    }
                }
            }
            app.view = View::Agent(id);
            false
        }
        EntryKind::Action => {
            match entry.id.as_str() {
                "action:quit" => {
                    app.should_quit = true;
                }
                "action:refresh" => {
                    app.refresh().ok();
                    app.status_line = "refreshed".into();
                }
                "action:new-task" => {
                    app.status_line =
                        "(new-task form lands in Phase 7)".into();
                }
                _ => {}
            }
            false
        }
    }
}

fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    let w = (area.width * percent_x / 100).max(40);
    let h = height.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 3;
    Rect {
        x: area.x + x,
        y: area.y + y,
        width: w,
        height: h,
    }
}
