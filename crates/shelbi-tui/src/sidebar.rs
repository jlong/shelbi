//! Render the left-hand sidebar: title, nav rows, separator, agents, footer.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Padding},
    Frame,
};

use crate::app::App;

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let nav = app.nav();
    let mut items: Vec<ListItem> = Vec::new();

    // Title row.
    items.push(ListItem::new(Line::from(vec![Span::styled(
        " shelbi ",
        Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
    )])));
    items.push(ListItem::new(Span::raw("")));

    // Pre-compute where the separator falls (after the 4 main nav rows).
    const MAIN_NAV: usize = 4;

    for (i, row) in nav.iter().enumerate() {
        if i == MAIN_NAV && i < nav.len() {
            items.push(ListItem::new(Span::raw("")));
            items.push(ListItem::new(Span::styled(
                " — agents —",
                Style::default().fg(Color::DarkGray),
            )));
        }
        let mut spans: Vec<Span> = vec![Span::raw(format!("{} ", row.icon)), Span::raw(row.label.clone())];
        if let Some(b) = &row.badge {
            spans.push(Span::styled(
                format!("  {}", b),
                Style::default().fg(Color::DarkGray),
            ));
        }
        let line = Line::from(spans);
        items.push(ListItem::new(line));
    }

    items.push(ListItem::new(Span::raw("")));
    items.push(ListItem::new(Span::styled(
        " ^P to switch",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .padding(Padding::new(0, 0, 0, 0));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Compute the listitem index of the highlighted row (account for the
    // title + blank prefix and the agents-separator).
    let highlight_idx = item_index_for_nav(app.sidebar_index);

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸");

    let mut state = ListState::default();
    state.select(Some(highlight_idx));
    f.render_stateful_widget(list, inner, &mut state);
}

/// Maps `App::sidebar_index` (the nth nav row) to its position in the
/// rendered list above. There's a 2-row header and a 2-row separator that
/// appears before the agents rows.
fn item_index_for_nav(nav_idx: usize) -> usize {
    const HEADER_ROWS: usize = 2;
    const MAIN_NAV: usize = 4;
    if nav_idx < MAIN_NAV {
        HEADER_ROWS + nav_idx
    } else {
        HEADER_ROWS + MAIN_NAV + 2 + (nav_idx - MAIN_NAV)
    }
}
