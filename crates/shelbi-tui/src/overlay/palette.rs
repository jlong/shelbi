//! The command palette — shared overlay rendering + the in-process state.
//!
//! The palette lists and runs commands fuzzy-matched from
//! [`shelbi_palette::Entry`] values. The **rendering** here is the single
//! implementation shared by the legacy tmux `shelbi __palette` popup and the
//! single-process TUI overlay (removing-tmux Phase 4d): [`render`] paints the
//! title, the search input, the command results list, an optional projects
//! column, and a footer into a caller-supplied [`Rect`], from a plain
//! [`PaletteView`] of data.
//!
//! The tmux popup builds its [`PaletteView`] (including the empty-query projects
//! column) from its own `State` and keeps its own event loop and
//! projects-column navigation. The in-process overlay uses [`Palette`] — a
//! small state machine over the registry's entries — whose focus model is the
//! plan's: Escape returns to the agent, Tab moves focus to the sidebar, and
//! typing filters and runs any command. The projects column is a tmux-only
//! affordance (the single-process TUI has the real sidebar beside the overlay),
//! so the in-process overlay passes `projects: None`.

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_palette::Entry;
use shelbi_state::keymap::PaletteAction;

use crate::decoration_to_color;

// ---------------------------------------------------------------------------
// Project indicators (empty-query projects column; tmux palette only today)
// ---------------------------------------------------------------------------

/// A project row's liveness indicator in the empty-query projects column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIndicator {
    /// Loaded and has active-category work in progress — pulsing green fill.
    Active,
    /// Loaded but no active work — green ring, no fill.
    LoadedIdle,
    /// Not loaded — neutral/gray ring, no fill.
    Unloaded,
}

/// Resolve a project's indicator from its two live inputs. Not-loaded
/// dominates: a project with no live session shows the neutral ring even if a
/// stale active-category task lingers on disk, because nothing is progressing
/// it.
pub fn project_indicator(loaded: bool, active: bool) -> ProjectIndicator {
    if !loaded {
        ProjectIndicator::Unloaded
    } else if active {
        ProjectIndicator::Active
    } else {
        ProjectIndicator::LoadedIdle
    }
}

/// Leading status glyph + color for a project row, given its indicator and the
/// current pulse `phase`.
pub fn project_status_style(indicator: ProjectIndicator, phase: f32) -> (&'static str, Color) {
    match indicator {
        ProjectIndicator::Active => ("●", crate::theme::project_pulse_color(phase)),
        ProjectIndicator::LoadedIdle => ("○", crate::theme::PROJECT_STATUS_GREEN),
        ProjectIndicator::Unloaded => ("•", crate::theme::PROJECT_STATUS_NEUTRAL),
    }
}

/// Current pulse phase in `[0.0, 1.0)`, derived from how long the picker has
/// been open. Feeds [`project_status_style`] so the active indicator breathes as
/// the render loop repaints.
pub fn pulse_phase(start: Instant) -> f32 {
    let period = crate::theme::PROJECT_PULSE_PERIOD.as_secs_f32();
    (start.elapsed().as_secs_f32() / period).fract()
}

/// Highlight style for a list's selected row. The focused column gets the bright
/// selection bar; an unfocused column keeps a visible but dim marker.
pub fn selection_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .bg(crate::theme::SELECTION_BG)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::DIM)
    }
}

// ---------------------------------------------------------------------------
// The view the shared renderer draws
// ---------------------------------------------------------------------------

/// One row of the empty-query projects column (tmux palette): its display label
/// and resolved indicator.
#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub label: String,
    pub indicator: ProjectIndicator,
}

/// The empty-query projects column: the other-project rows (a trailing "Add
/// project" row is appended by the renderer), the selected index when the column
/// is focused, and the pulse phase.
#[derive(Debug, Clone)]
pub struct ProjectsColumn {
    pub rows: Vec<ProjectRow>,
    /// `Some(i)` when the column is focused (paints the selection bar); `None`
    /// leaves it unselected. `i` may be `rows.len()` (the Add row).
    pub selected: Option<usize>,
    pub phase: f32,
}

/// Everything the shared [`render`] draws, as plain data. Both callers build
/// this and hand it over; neither owns rendering of its own.
pub struct PaletteView<'a> {
    pub project_label: &'a str,
    pub query: &'a str,
    /// Tacks a dim "loading board…" hint onto the title (cold tmux palette).
    pub board_loading: bool,
    pub results: &'a [(Entry, u16)],
    pub selected: usize,
    /// Whether the commands column holds focus (dims its selection otherwise).
    pub commands_focused: bool,
    /// The empty-query projects column, or `None` to render a single-column
    /// completion list spanning the whole width (the in-process overlay).
    pub projects: Option<ProjectsColumn>,
    /// The one-line footer hint.
    pub footer: &'a str,
}

/// Paint the palette into `area` (the whole popup pane in the tmux runtime, a
/// centered overlay rect in the single-process TUI).
pub fn render(f: &mut Frame, area: Rect, view: &PaletteView) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Title, with the cold-board hint.
    let mut title_spans = vec![Span::styled(
        format!("shelbi · {}", view.project_label),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if view.board_loading {
        title_spans.push(Span::styled(
            "  · loading board…",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(title_spans)), layout[0]);

    // Search input.
    let prompt = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::DarkGray)),
        Span::raw(view.query.to_string()),
        Span::styled("▏", Style::default().fg(Color::Cyan)),
    ]);
    f.render_widget(Paragraph::new(vec![prompt, Line::raw("")]), layout[1]);

    // Results area. With a projects column, the area splits into a Commands
    // column (left, the full list) and a Projects column (right); otherwise the
    // commands list reclaims the whole width.
    let (commands_area, projects_area) = if view.projects.is_some() {
        let proj_w = (layout[2].width / 3).clamp(18, 28);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(proj_w)])
            .split(layout[2]);
        (cols[0], Some(cols[1]))
    } else {
        (layout[2], None)
    };

    let items = build_result_items(view.results, commands_area.width as usize);
    let commands_focused = projects_area.is_none() || view.commands_focused;
    let list = List::new(items).highlight_style(selection_style(commands_focused));
    let mut s = ListState::default();
    if !view.results.is_empty() {
        s.select(Some(view.selected.min(view.results.len().saturating_sub(1))));
    }
    f.render_stateful_widget(list, commands_area, &mut s);

    if let (Some(area), Some(projects)) = (projects_area, view.projects.as_ref()) {
        render_projects_column(f, area, projects);
    }

    let footer = Paragraph::new(Line::from(vec![Span::styled(
        view.footer.to_string(),
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(footer, layout[3]);
}

/// Build the command-list rows: glyph + padded label + dim subtitle + a
/// right-aligned shortcut hint. `row_width` is the list pane width (for
/// right-aligning the shortcut); falls back to no padding when too narrow.
fn build_result_items(results: &[(Entry, u16)], row_width: usize) -> Vec<ListItem<'static>> {
    results
        .iter()
        .map(|(e, _)| {
            let (glyph, glyph_color) = match &e.decoration {
                Some(d) => (d.glyph.as_str(), decoration_to_color(d.color)),
                None => (e.kind.icon(), Color::DarkGray),
            };
            let prefix = format!(" {glyph} ");
            let label = format!("{:<22}", e.label);
            let mut content_width = prefix.chars().count() + label.chars().count();
            let mut spans = vec![
                Span::styled(prefix, Style::default().fg(glyph_color)),
                Span::raw(label),
            ];
            if let Some(sub) = &e.subtitle {
                let s = format!("  {sub}");
                content_width += s.chars().count();
                spans.push(Span::styled(s, Style::default().fg(Color::DarkGray)));
            }
            if let Some(short) = &e.shortcut {
                let sw = short.chars().count();
                let pad = row_width
                    .saturating_sub(content_width)
                    .saturating_sub(sw)
                    .saturating_sub(1);
                if pad > 0 {
                    spans.push(Span::raw(" ".repeat(pad)));
                } else {
                    spans.push(Span::raw("  "));
                }
                spans.push(Span::styled(short.clone(), Style::default().fg(Color::DarkGray)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect()
}

/// Render the empty-query second column of other projects into `area`. A left
/// border acts as the divider, a dim "Projects" header labels it, and each row
/// carries the loaded/unloaded indicator glyph. A trailing "Add project" row
/// closes the list.
fn render_projects_column(f: &mut Frame, area: Rect, col: &ProjectsColumn) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let header = Paragraph::new(Line::from(Span::styled(
        " Projects",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(header, rows[0]);

    let mut items: Vec<ListItem> = col
        .rows
        .iter()
        .map(|p| {
            let (glyph, glyph_color) = project_status_style(p.indicator, col.phase);
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {glyph} "), Style::default().fg(glyph_color)),
                Span::raw(p.label.clone()),
            ]))
        })
        .collect();
    items.push(ListItem::new(Line::from(vec![
        Span::styled(" + ", Style::default().fg(Color::DarkGray)),
        Span::raw("Add project"),
    ])));

    // Selection patches background + weight only (never a foreground color) so a
    // loaded project's green `●` keeps its status color under the bar.
    let highlight = Style::default()
        .bg(crate::theme::SELECTION_BG)
        .add_modifier(Modifier::BOLD);
    let list = List::new(items).highlight_style(highlight);
    let mut s = ListState::default();
    if let Some(sel) = col.selected {
        let max = col.rows.len(); // the Add row is index rows.len()
        s.select(Some(sel.min(max)));
    }
    f.render_stateful_widget(list, rows[1], &mut s);
}

// ---------------------------------------------------------------------------
// The in-process overlay state
// ---------------------------------------------------------------------------

/// What feeding one key into the in-process palette resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteStep {
    /// Keep the overlay open (filtered, moved selection, edited the query).
    Continue,
    /// Close the overlay and return focus to the agent (Escape / the palette
    /// chord again).
    Close,
    /// Close the overlay and move focus to the sidebar (Tab).
    FocusSidebar,
    /// Run this entry's command, then close the overlay.
    Activate(Entry),
}

/// The in-process command palette overlay. Holds the current query, the
/// selection, and the registry entries to filter; the host (the shell) supplies
/// the entries from [`shelbi_app::CommandRegistry::entries`] and turns an
/// activated [`Entry`] into a command effect.
pub struct Palette {
    project_label: String,
    query: String,
    selected: usize,
    entries: Vec<Entry>,
}

impl Palette {
    /// Open the palette over `entries` for a project labeled `project_label`.
    pub fn new(project_label: impl Into<String>, entries: Vec<Entry>) -> Self {
        Self {
            project_label: project_label.into(),
            query: String::new(),
            selected: 0,
            entries,
        }
    }

    /// Replace the entry list (a background refresh landed) without disturbing
    /// the query; the renderer re-clamps the selection.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The fuzzy-filtered results for the current query.
    pub fn results(&self) -> Vec<(Entry, u16)> {
        shelbi_palette::search(&self.entries, &self.query)
    }

    /// Feed one key press. `action` is the palette-mode binding the host
    /// resolved from its keymap (so Close/Activate/Nav/Backspace honor the
    /// user's `keys.yaml`); Tab, Space, and unbound printable chars are handled
    /// by raw key code so they behave regardless of bindings.
    pub fn handle_key(&mut self, ev: KeyEvent, action: Option<PaletteAction>) -> PaletteStep {
        // Tab leaves the palette for the sidebar (plan focus model). Handled
        // ahead of the keymap so a user binding can't shadow it.
        if ev.code == KeyCode::Tab && ev.modifiers.is_empty() {
            return PaletteStep::FocusSidebar;
        }
        // Space always types into the query (even if a keymap binds it).
        if ev.code == KeyCode::Char(' ')
            && (ev.modifiers.is_empty() || ev.modifiers == KeyModifiers::SHIFT)
        {
            self.query.push(' ');
            self.selected = 0;
            return PaletteStep::Continue;
        }

        match action {
            Some(PaletteAction::Close) => PaletteStep::Close,
            Some(PaletteAction::Activate) => match self.results().get(self.selected) {
                Some((entry, _)) => PaletteStep::Activate(entry.clone()),
                None => PaletteStep::Continue,
            },
            Some(PaletteAction::NavUp) => {
                self.selected = self.selected.saturating_sub(1);
                PaletteStep::Continue
            }
            Some(PaletteAction::NavDown) => {
                let count = self.results().len();
                if self.selected + 1 < count {
                    self.selected += 1;
                }
                PaletteStep::Continue
            }
            Some(PaletteAction::Backspace) => {
                self.query.pop();
                self.selected = 0;
                PaletteStep::Continue
            }
            None => {
                if let KeyCode::Char(c) = ev.code {
                    if ev.modifiers.is_empty() || ev.modifiers == KeyModifiers::SHIFT {
                        self.query.push(c);
                        self.selected = 0;
                    }
                }
                PaletteStep::Continue
            }
        }
    }

    /// Paint the overlay into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let results = self.results();
        let view = PaletteView {
            project_label: &self.project_label,
            query: &self.query,
            board_loading: false,
            results: &results,
            selected: self.selected,
            commands_focused: true,
            projects: None,
            footer: "↑↓ navigate · Enter run · Esc agent · Tab sidebar",
        };
        render(f, area, &view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_palette::EntryKind;

    fn entry(id: &str, label: &str) -> Entry {
        Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn project_indicator_resolves_the_three_states_in_precedence_order() {
        assert_eq!(project_indicator(false, false), ProjectIndicator::Unloaded);
        // Not-loaded dominates a stale active flag.
        assert_eq!(project_indicator(false, true), ProjectIndicator::Unloaded);
        assert_eq!(project_indicator(true, true), ProjectIndicator::Active);
        assert_eq!(project_indicator(true, false), ProjectIndicator::LoadedIdle);
    }

    #[test]
    fn project_status_style_paints_each_state() {
        let (active_glyph, _) = project_status_style(ProjectIndicator::Active, 0.5);
        assert_eq!(active_glyph, "●");
        let (idle_glyph, idle_color) = project_status_style(ProjectIndicator::LoadedIdle, 0.0);
        assert_eq!(idle_glyph, "○");
        assert_eq!(idle_color, crate::theme::PROJECT_STATUS_GREEN);
        let (unloaded_glyph, unloaded_color) =
            project_status_style(ProjectIndicator::Unloaded, 0.0);
        assert_eq!(unloaded_glyph, "•");
        assert_eq!(unloaded_color, crate::theme::PROJECT_STATUS_NEUTRAL);
    }

    #[test]
    fn active_pulse_fill_breathes_across_the_cycle() {
        // The filled disc's fill must actually change between phases so the
        // pulse is visible; the trough and peak differ in the green channel.
        let (_, trough) = project_status_style(ProjectIndicator::Active, 0.0);
        let (_, peak) = project_status_style(ProjectIndicator::Active, 0.5);
        assert_ne!(trough, peak, "the fill must cycle between phases");
    }

    #[test]
    fn typing_filters_and_resets_selection() {
        let mut p = Palette::new(
            "alpha",
            vec![entry("view:tasks", "Issues"), entry("action:toggle-zen", "Turn Zen Mode on")],
        );
        assert_eq!(p.results().len(), 2);
        p.selected = 1;
        // Type "zen": filters to the zen toggle and resets the selection.
        for c in "zen".chars() {
            assert_eq!(p.handle_key(key(KeyCode::Char(c)), None), PaletteStep::Continue);
        }
        assert_eq!(p.query(), "zen");
        assert_eq!(p.selected(), 0);
        let hits = p.results();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, "action:toggle-zen");
    }

    #[test]
    fn tab_moves_to_the_sidebar_and_esc_closes_to_the_agent() {
        let mut p = Palette::new("alpha", vec![entry("view:tasks", "Issues")]);
        assert_eq!(p.handle_key(key(KeyCode::Tab), None), PaletteStep::FocusSidebar);
        assert_eq!(
            p.handle_key(key(KeyCode::Esc), Some(PaletteAction::Close)),
            PaletteStep::Close
        );
    }

    #[test]
    fn activate_returns_the_selected_entry() {
        let mut p = Palette::new(
            "alpha",
            vec![entry("view:tasks", "Issues"), entry("view:activity", "Activity")],
        );
        assert_eq!(
            p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown)),
            PaletteStep::Continue
        );
        assert_eq!(p.selected(), 1);
        match p.handle_key(key(KeyCode::Enter), Some(PaletteAction::Activate)) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "view:activity"),
            other => panic!("expected Activate, got {other:?}"),
        }
    }

    #[test]
    fn nav_down_clamps_to_the_result_count() {
        let mut p = Palette::new("alpha", vec![entry("view:tasks", "Issues")]);
        // Only one result: NavDown can't move past it.
        p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown));
        assert_eq!(p.selected(), 0);
    }

    #[test]
    fn space_types_even_when_unbound() {
        let mut p = Palette::new("alpha", vec![entry("view:tasks", "Issues")]);
        assert_eq!(p.handle_key(key(KeyCode::Char(' ')), None), PaletteStep::Continue);
        assert_eq!(p.query(), " ");
    }

    #[test]
    fn backspace_pops_the_query() {
        let mut p = Palette::new("alpha", vec![entry("view:tasks", "Issues")]);
        for c in "iss".chars() {
            p.handle_key(key(KeyCode::Char(c)), None);
        }
        assert_eq!(p.query(), "iss");
        assert_eq!(
            p.handle_key(key(KeyCode::Backspace), Some(PaletteAction::Backspace)),
            PaletteStep::Continue
        );
        assert_eq!(p.query(), "is");
    }

    fn draw_projects(col: ProjectsColumn) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let results: Vec<(Entry, u16)> = Vec::new();
        let view = PaletteView {
            project_label: "portal",
            query: "",
            board_loading: false,
            results: &results,
            selected: 0,
            commands_focused: col.selected.is_none(),
            projects: Some(col),
            footer: "footer",
        };
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        term.draw(|f| render(f, f.area(), &view)).unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn projects_column_keeps_the_loaded_glyph_green_when_its_row_is_selected() {
        // Focused + selected on the loaded-but-idle project: the selection bar
        // patches bg + weight only, so the green ring `○` keeps its status color.
        let buf = draw_projects(ProjectsColumn {
            rows: vec![ProjectRow {
                label: "Website".into(),
                indicator: ProjectIndicator::LoadedIdle,
            }],
            selected: Some(0),
            phase: 0.0,
        });
        let mut found = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "○" {
                    assert_eq!(
                        cell.fg,
                        crate::theme::PROJECT_STATUS_GREEN,
                        "the selected loaded-idle ring must stay green"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "the loaded project's green ring should render");
    }

    #[test]
    fn projects_column_pulses_a_filled_green_disc_for_active_work() {
        let buf = draw_projects(ProjectsColumn {
            rows: vec![ProjectRow {
                label: "Website".into(),
                indicator: ProjectIndicator::Active,
            }],
            selected: None,
            phase: 0.0,
        });
        let mut found = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "●" {
                    assert!(
                        matches!(cell.fg, Color::Rgb(0, _, 0)),
                        "the active disc must render a green pulse fill"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "an active project should render the filled disc");
    }

    #[test]
    fn projects_column_renders_the_add_row_and_never_dims_the_first_row_unfocused() {
        // Column unfocused (selected None): the first project renders in normal
        // text (no dim/gray unfocused-selection), and the trailing "+ Add
        // project" row is present.
        let buf = draw_projects(ProjectsColumn {
            rows: vec![ProjectRow {
                label: "Website".into(),
                indicator: ProjectIndicator::LoadedIdle,
            }],
            selected: None,
            phase: 0.0,
        });
        let dumped: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(dumped.contains("Website"), "project row should render");
        assert!(dumped.contains("Add project"), "Add project row should render");
        assert!(dumped.contains('+'), "Add row should carry a `+` glyph");
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if "Website".contains(cell.symbol()) && cell.symbol() != " " {
                    assert!(
                        !cell.modifier.contains(Modifier::DIM),
                        "first project label must not be dimmed when unfocused"
                    );
                    assert_ne!(
                        cell.fg,
                        Color::Gray,
                        "first project label must not render gray when unfocused"
                    );
                }
            }
        }
    }

    #[test]
    fn render_paints_title_input_and_results() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let p = Palette::new("alpha", vec![entry("view:tasks", "Issues")]);
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let dumped: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(dumped.contains("shelbi · alpha"), "title: {dumped}");
        assert!(dumped.contains("Issues"), "result row: {dumped}");
        assert!(dumped.contains("Tab sidebar"), "footer hint: {dumped}");
    }
}
