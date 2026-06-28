//! Render the sidebar UI — fills the entire pane (it's the only thing
//! this process renders; the orchestrator and workers live in other tmux
//! panes / windows).

use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_palette::DecorationColor;
use shelbi_state::ZenModeState;

use crate::app::{App, Row};

pub fn render_full(f: &mut Frame, app: &mut App, area: Rect) {
    // Footer is always: [status?] keybinds, blank, zen-row. The zen row is
    // a single line in both states so toggling never shifts the rows above
    // it. Width grows by 1 when there's a `status_line` to surface.
    let footer_height: u16 = if app.status_line.is_empty() { 3 } else { 4 };
    // Outer split is full-width — the zen ON row paints its green band
    // edge-to-edge of the sidebar column, so it can't sit inside the 1-col
    // horizontal padding the list uses. We re-apply that padding only to
    // the list and the keybind/status rows below.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(footer_height)])
        .split(area);

    let list_area = outer[0].inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    render_list(f, app, list_area);
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
    let list = List::new(items).highlight_style(Style::default().bg(Color::Rgb(63,63,63)));
    f.render_stateful_widget(list, inner, &mut state);
}

fn render_row(row: &Row, selected: bool, width: usize) -> ListItem<'static> {
    match row {
        Row::Nav { label, .. } => {
            let dec = row.decoration().expect("nav rows always have a decoration");
            let style = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(Line::from(vec![Span::styled(
                format!("{} {label}", dec.glyph),
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
        Row::Worker { name, .. } => {
            let dec = row.decoration().expect("worker rows always have a decoration");
            let mut spans = vec![
                Span::styled(
                    format!("{} ", dec.glyph),
                    Style::default().fg(decoration_to_color(dec.color)),
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
            // Same decoration the palette uses, so worker / review / palette
            // row share one visual vocabulary.
            let dec = row.decoration().expect("review rows always have a decoration");
            let badge = Span::styled(
                format!("{} ", dec.glyph),
                Style::default().fg(decoration_to_color(dec.color)),
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
            ..
        } => {
            let dec = row
                .decoration()
                .expect("legacy-agent rows always have a decoration");
            let badge = Span::styled(
                format!("{} ", dec.glyph),
                Style::default().fg(decoration_to_color(dec.color)),
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
    // Vertical rhythm is fixed: [status?] keybinds, blank, zen-row. The
    // zen row sits at the same y-coordinate whether Zen is On or Off so
    // toggling never nudges the keybind line above.
    let has_status = !app.status_line.is_empty();
    let constraints: Vec<Constraint> = if has_status {
        vec![Constraint::Length(1); 4]
    } else {
        vec![Constraint::Length(1); 3]
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    // Indent for status + keybinds matches the list's 1-col horizontal
    // padding. The zen row deliberately bypasses this so its background
    // can reach the sidebar edge.
    let indent = Margin {
        horizontal: 1,
        vertical: 0,
    };
    let mut idx = 0;
    if has_status {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                app.status_line.clone(),
                Style::default().fg(Color::Yellow),
            ))),
            rows[idx].inner(indent),
        );
        idx += 1;
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "^P palette  q quit",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[idx].inner(indent),
    );
    idx += 2; // skip blank row

    render_zen_row(f, rows[idx], app);
}

/// Single-line zen indicator anchored to the sidebar footer.
///
/// On: full-width green band with `ZEN MODE ON` in black — paints
/// edge-to-edge of the sidebar column, no border, no padding box.
/// Off: dim `<hotkey> Zen mode` hint in the same DarkGray style as the
/// `^P palette  q quit` line above it, so the hotkey is discoverable
/// without changing the footer's visual weight. The hotkey glyph is
/// sourced from the configured chord — rebinding updates it
/// automatically. When no chord is bound the hint is suppressed so the
/// row stays blank rather than implying a non-existent shortcut.
fn render_zen_row(f: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if matches!(app.zen_mode, ZenModeState::On) {
        let style = Style::default()
            .bg(Color::Rgb(0, 127, 0))
            .fg(Color::Rgb(255, 255, 255))
            .add_modifier(Modifier::BOLD);
        let width = area.width as usize;
        let label = "ZEN MODE ON";
        let label_w = label.chars().count();
        let line = if width <= label_w {
            // Narrower than the label — clip rather than wrap so the
            // band stays exactly one row tall.
            label.chars().take(width).collect::<String>()
        } else {
            // Align the label left inside a full-width green band.
            let pad = width - label_w;
            let left = 1;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), label, " ".repeat(right))
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(line, style))),
            area,
        );
        return;
    }

    // Off / Paused: same indent + dim style as `^P palette  q quit`.
    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let Some(glyph) = app.zen_toggle_chord.glyph() else {
        return;
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{glyph} Zen mode"),
            Style::default().fg(Color::DarkGray),
        ))),
        inner,
    );
}

/// Map the palette's ratatui-free [`DecorationColor`] to a ratatui
/// [`Color`]. Single conversion point so the sidebar and the palette
/// agree on what each decoration tint looks like on screen.
pub fn decoration_to_color(c: DecorationColor) -> Color {
    match c {
        DecorationColor::Default => Color::Reset,
        DecorationColor::Gray => Color::Gray,
        DecorationColor::DarkGray => Color::DarkGray,
        DecorationColor::Green => Color::Green,
        DecorationColor::Yellow => Color::Yellow,
        DecorationColor::Red => Color::Red,
        DecorationColor::Cyan => Color::Cyan,
        DecorationColor::Blue => Color::Blue,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_state::ZenToggleChord;

    fn dump(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    /// Locate the y of the row containing `needle`. Fails the test if it
    /// isn't there — callers use it both to assert presence and to check
    /// relative positioning between rows.
    fn row_y(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("expected row containing {needle:?} in:\n{}", rows.join("\n")))
    }

    /// On state: full-width green band carrying `ZEN MODE ON`, one blank
    /// row above it, the `^P palette  q quit` keybind line above that.
    /// No box-drawing chrome — the old bordered pill is gone.
    #[test]
    fn zen_row_renders_full_width_band_when_on() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::On;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let rows = dump(&term);
        let joined = rows.join("\n");
        assert!(
            joined.contains("ZEN MODE ON"),
            "expected ZEN MODE ON label in:\n{joined}"
        );
        assert!(
            !joined.contains("┌─") && !joined.contains("└─"),
            "no border chrome expected, got:\n{joined}"
        );
        let keybind_y = row_y(&rows, "^P palette  q quit");
        let zen_y = row_y(&rows, "ZEN MODE ON");
        assert!(
            zen_y == keybind_y + 2,
            "expected one blank row between keybinds (y={keybind_y}) and zen (y={zen_y}) in:\n{joined}"
        );
        // The zen row must paint edge-to-edge of the sidebar column —
        // every column on that row is non-empty.
        let zen_row = &rows[zen_y];
        assert_eq!(
            zen_row.chars().count(),
            24,
            "zen row should span full sidebar width, got: {zen_row:?}"
        );
        assert!(
            !zen_row.trim().is_empty(),
            "zen row should not be empty: {zen_row:?}"
        );
    }

    /// Off state: dim `<hotkey> Zen mode` hint sourced from the configured
    /// chord. No green band, same row position as the on-state band so
    /// toggling doesn't shift the layout.
    #[test]
    fn zen_row_renders_hotkey_hint_when_off() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Off;
        app.zen_toggle_chord = ZenToggleChord::AltZ;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let rows = dump(&term);
        let joined = rows.join("\n");
        assert!(!joined.contains("ZEN MODE ON"), "no green band when off, got:\n{joined}");
        assert!(
            joined.contains("⌥Z Zen mode"),
            "expected hotkey hint sourced from configured chord in:\n{joined}"
        );
        let keybind_y = row_y(&rows, "^P palette  q quit");
        let hint_y = row_y(&rows, "Zen mode");
        assert_eq!(
            hint_y,
            keybind_y + 2,
            "hint row must sit two rows below keybinds (blank between), got:\n{joined}"
        );
    }

    /// Rebinding the chord updates the off-state hint without touching
    /// any sidebar code — the glyph is read from `app.zen_toggle_chord`
    /// each render.
    #[test]
    fn off_state_hint_tracks_configured_chord() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Off;
        app.zen_toggle_chord = ZenToggleChord::CtrlG;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let joined = dump(&term).join("\n");
        assert!(
            joined.contains("^G Zen mode"),
            "rebinding to Ctrl+G must change the hint, got:\n{joined}"
        );
    }

    /// Toggling Zen Mode must not shift rows above the zen line — the
    /// keybind y-coordinate is identical in both states.
    #[test]
    fn keybind_line_stays_put_across_zen_toggle() {
        let backend_off = TestBackend::new(24, 16);
        let mut term_off = Terminal::new(backend_off).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Off;
        term_off
            .draw(|f| render_full(f, &mut app, f.area()))
            .unwrap();
        let off_rows = dump(&term_off);
        let off_y = row_y(&off_rows, "^P palette  q quit");

        let backend_on = TestBackend::new(24, 16);
        let mut term_on = Terminal::new(backend_on).unwrap();
        app.zen_mode = ZenModeState::On;
        term_on
            .draw(|f| render_full(f, &mut app, f.area()))
            .unwrap();
        let on_rows = dump(&term_on);
        let on_y = row_y(&on_rows, "^P palette  q quit");

        assert_eq!(off_y, on_y, "keybind line must not move when toggling Zen");
    }

    /// Paused is treated like Off for the hint — the orchestrator may
    /// still be draining work, but visually we surface the hotkey so the
    /// user can resume.
    #[test]
    fn paused_shows_hotkey_hint_not_green_band() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Paused;
        app.zen_toggle_chord = ZenToggleChord::AltZ;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let joined = dump(&term).join("\n");
        assert!(!joined.contains("ZEN MODE ON"), "paused must not show green band, got:\n{joined}");
        assert!(
            joined.contains("⌥Z Zen mode"),
            "paused should still show the hotkey hint, got:\n{joined}"
        );
    }

    /// When the user explicitly chose no chord (`ZenToggleChord::None`),
    /// the hint is suppressed — showing an empty hotkey would imply a
    /// shortcut that doesn't exist. The row stays blank to preserve
    /// the layout above it.
    #[test]
    fn no_hint_when_chord_is_none() {
        let backend = TestBackend::new(24, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.zen_mode = ZenModeState::Off;
        app.zen_toggle_chord = ZenToggleChord::None;
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let joined = dump(&term).join("\n");
        assert!(!joined.contains("Zen mode"), "no hint expected when no chord bound, got:\n{joined}");
    }
}
