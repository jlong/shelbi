//! Ctrl+P fuzzy command palette overlay.

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

/// Palette entries: orchestrator first, then each agent, then actions.
pub fn entries(app: &App) -> Vec<Entry> {
    let mut out = vec![Entry {
        id: "view:orchestrator".into(),
        label: "orchestrator".into(),
        kind: EntryKind::View,
        subtitle: Some("focus the orchestrator pane".into()),
    }];
    for a in &app.agents {
        out.push(Entry {
            id: format!("agent:{}", a.id),
            label: a.id.clone(),
            kind: EntryKind::Agent,
            subtitle: Some(format!("{} · {:?}", a.machine, a.status)),
        });
    }
    out.push(Entry {
        id: "action:refresh".into(),
        label: "refresh state".into(),
        kind: EntryKind::Action,
        subtitle: Some("reload agent files".into()),
    });
    out.push(Entry {
        id: "action:quit".into(),
        label: "quit shelbi".into(),
        kind: EntryKind::Action,
        subtitle: Some("kills the sidebar; orchestrator keeps running".into()),
    });
    out
}

pub fn render(f: &mut Frame, state: &PaletteState, results: &[(Entry, u16)]) {
    if !state.open {
        return;
    }
    let area = centered_rect(f.area(), 90, 18);
    f.render_widget(Clear, area);

    let outer = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::new(1, 1, 0, 0))
        .title(Span::styled(
            " C-␣ palette ",
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

    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(state.query.clone()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[0]);

    let items: Vec<ListItem> = results
        .iter()
        .map(|(e, _)| {
            let mut spans = vec![
                Span::styled(
                    format!(" {} ", e.kind.icon()),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(format!("{:<18}", e.label)),
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

/// Activate the selected entry. Returns true if the palette should stay
/// open (it never does today).
pub fn activate(app: &mut App, entry: &Entry) -> bool {
    match entry.kind {
        EntryKind::View => {
            app.activate_view(&View::Orchestrator);
        }
        EntryKind::Agent => {
            app.activate_view(&View::Agent(entry.label.clone()));
        }
        EntryKind::Action => match entry.id.as_str() {
            "action:quit" => {
                app.should_quit = true;
            }
            "action:refresh" => {
                app.refresh().ok();
                app.status_line = "refreshed".into();
            }
            _ => {}
        },
    }
    false
}

fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    // Always clamp to the frame: writing outside the buffer panics ratatui.
    let target_w = (area.width.saturating_mul(percent_x) / 100).max(20);
    let w = target_w.min(area.width);
    let h = height.min(area.height);
    let x = area.width.saturating_sub(w) / 2;
    let y = area.height.saturating_sub(h) / 3;
    Rect {
        x: area.x + x,
        y: area.y + y,
        width: w,
        height: h,
    }
}
