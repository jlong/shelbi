//! Render the sidebar UI — fills the entire pane (it's the only thing
//! this process renders; the orchestrator and workspaces live in other tmux
//! panes / windows).

use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_palette::DecorationColor;
use shelbi_state::keymap::{GlobalAction, SidebarAction};
use shelbi_state::ZenModeState;

use crate::app::{App, Row};
use crate::keymap::format_chord_or_unbound;

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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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
    let list = List::new(items).highlight_style(Style::default().bg(crate::theme::SELECTION_BG));
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
        Row::MachineGroup {
            name,
            collapsed,
            total,
            active,
        } => {
            // `▾` when expanded (workspace rows follow), `▸` when
            // collapsed (rows hidden). `▶` stays reserved for an active
            // workspace row's Working badge, so the two never collide on
            // screen. The header is dim by default and brightens to
            // white-bold when focused so the user can see what Space /
            // Enter will toggle.
            let glyph = if *collapsed { "▸" } else { "▾" };
            let header_style = if selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let left = vec![Span::styled(format!("{glyph} {name}"), header_style)];
            if *collapsed {
                // `(<total>, <active> active)` right-aligned in dim
                // gray — same column treatment workspace / review rows
                // use for their right label so the sidebar reads in one
                // visual vocabulary. The count is suppressed when
                // expanded; per-workspace rows surface the same info.
                let right_label = format!("({total}, {active} active)");
                let spans = right_align(
                    left,
                    right_label,
                    Style::default().fg(Color::DarkGray),
                    width,
                );
                ListItem::new(Line::from(spans))
            } else {
                ListItem::new(Line::from(left))
            }
        }
        Row::Workspace {
            name,
            agent,
            indent,
            ..
        } => {
            let dec = row
                .decoration()
                .expect("workspace rows always have a decoration");
            // Multi-machine projects indent workspace rows by two columns
            // so they sit visually inside their `▾ <machine>` group; flat
            // single-machine layouts skip the indent.
            let leading = if *indent { "  " } else { "" };
            let left = vec![
                Span::raw(leading),
                Span::styled(
                    format!("{} ", dec.glyph),
                    Style::default().fg(decoration_to_color(dec.color)),
                ),
                Span::styled(name.clone(), name_style(selected)),
            ];
            // Right column: title-cased agent name when an agent is loaded,
            // dim `idle` placeholder otherwise. Same dim style as the
            // review row's machine column so the two surfaces read in one
            // visual vocabulary.
            let right_label = match agent {
                Some(a) => title_case(a),
                None => "idle".to_string(),
            };
            let spans = right_align(
                left,
                right_label,
                Style::default().fg(Color::DarkGray),
                width,
            );
            ListItem::new(if selected {
                Line::from(bold(spans))
            } else {
                Line::from(spans)
            })
        }
        Row::Review {
            title,
            branch,
            location,
            ..
        } => {
            // Two-line review entry (spec §16). Line 1: decoration badge +
            // title, with the `machine:port` URL right-aligned when the task
            // is loaded on a review worktree (Ready — cyan ✓); a Queued task
            // (dim ·) has no location yet. Line 2: branch, dim. The badge
            // glyph/color come from `row.decoration()`, the single source
            // shared with the palette.
            let dec = row
                .decoration()
                .expect("review rows always have a decoration");
            let badge = Span::styled(
                format!("{} ", dec.glyph),
                Style::default().fg(decoration_to_color(dec.color)),
            );
            let title_span = Span::styled(title.clone(), name_style(selected));
            let line1 = match location {
                Some(loc) => right_align(
                    vec![badge, title_span],
                    loc.clone(),
                    Style::default().fg(Color::DarkGray),
                    width,
                ),
                None => vec![badge, title_span],
            };
            let line1 = if selected {
                Line::from(bold(line1))
            } else {
                Line::from(line1)
            };
            // Branch sits under the title (indented past the badge column),
            // dim in both states — brightening only its weight when selected
            // so the highlighted row still reads as one unit.
            let branch_style = if selected {
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let line2 = Line::from(Span::styled(format!("  {branch}"), branch_style));
            ListItem::new(vec![line1, line2])
        }
        Row::LegacyAgent { id, machine, .. } => {
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
            ListItem::new(if selected {
                Line::from(bold(line))
            } else {
                Line::from(line)
            })
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

/// Uppercase the first character of a lowercase identifier (e.g.
/// `developer` → `Developer`). Agent directory names live on disk in
/// lowercase; the sidebar surfaces them in display form. Empty input
/// is preserved as-is so callers don't need a guard.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
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
    let km = app.keymaps();
    let style = app.display_style();
    let keybinds = format!(
        "{} palette  {} quit",
        format_chord_or_unbound(km.global.first_chord_for(GlobalAction::OpenPalette), style),
        format_chord_or_unbound(km.sidebar.first_chord_for(SidebarAction::Quit), style),
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            keybinds,
            Style::default().fg(Color::DarkGray),
        ))),
        rows[idx].inner(indent),
    );
    idx += 1;

    // Version row (the former blank spacer): daemon/CLI version probed
    // once at startup, painted red when the running daemon doesn't match
    // this binary. Blank until probed, so the rhythm is unchanged.
    render_version_row(f, rows[idx].inner(indent), app);
    idx += 1;

    render_zen_row(f, rows[idx], app);
}

/// Single-line daemon/CLI version segment in the footer's spacer row.
/// Dim on match (`daemon 0.4.0 · cli 0.4.0`), red on mismatch (`daemon
/// 0.1.0 ≠ cli 0.4.0 — shelbi daemon restart`). Nothing until the startup
/// probe has run.
fn render_version_row(f: &mut Frame, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(line) = app.daemon_version_line.clone() else {
        return;
    };
    let style = if app.daemon_version_mismatch {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    f.render_widget(Paragraph::new(Line::from(Span::styled(line, style))), area);
}

/// Single-line zen indicator anchored to the sidebar footer.
///
/// On: full-width green band with `ZEN MODE ON` in black — paints
/// edge-to-edge of the sidebar column, no border, no padding box.
/// Off: dim `<hotkey> Zen mode` hint in the same DarkGray style as the
/// palette/quit keybind line above it, so the hotkey is discoverable
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
        f.render_widget(Paragraph::new(Line::from(Span::styled(line, style))), area);
        return;
    }

    // Off / Paused: same indent + dim style as the palette.
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
    use shelbi_state::keymap::{DisplayStyle, KeyChord};
    use shelbi_state::ZenToggleChord;

    /// Render the footer at a width wide enough for the longest expected
    /// keybind string and return the flattened buffer.
    fn footer_text(app: &mut App) -> String {
        let backend = TestBackend::new(40, 16);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, app, f.area())).unwrap();
        dump(&term).join("\n")
    }

    fn bind(app: &mut App, palette: Option<&str>, quit: Option<&str>) {
        if let Some(c) = palette {
            app.keymaps
                .global
                .by_action
                .insert(GlobalAction::OpenPalette, vec![KeyChord::parse(c).unwrap()]);
        }
        if let Some(c) = quit {
            app.keymaps
                .sidebar
                .by_action
                .insert(SidebarAction::Quit, vec![KeyChord::parse(c).unwrap()]);
        }
    }

    /// Default chords (`ctrl-p` palette, `q` quit) render in the host
    /// platform's convention — the whole point of the platform-aware row.
    #[test]
    fn footer_renders_default_chords_per_platform() {
        let mut app = App::new_sidebar("demo");
        bind(&mut app, Some("ctrl-p"), Some("q"));
        let joined = footer_text(&mut app);
        let want = match app.display_style() {
            DisplayStyle::Mac => "⌃P palette  q quit",
            DisplayStyle::Linux => "Ctrl+P palette  q quit",
        };
        assert!(joined.contains(want), "expected {want:?} in:\n{joined}");
    }

    /// Overriding `global.open_palette` to a new chord updates the footer
    /// — it's sourced from the keymap, not hardcoded.
    #[test]
    fn footer_reflects_open_palette_override() {
        let mut app = App::new_sidebar("demo");
        bind(&mut app, Some("ctrl-shift-space"), Some("q"));
        let joined = footer_text(&mut app);
        let want = match app.display_style() {
            DisplayStyle::Mac => "⌃⇧Space palette",
            DisplayStyle::Linux => "Ctrl+Shift+Space palette",
        };
        assert!(joined.contains(want), "expected {want:?} in:\n{joined}");
    }

    /// Unbinding a help-referenced action renders `<unbound>` rather than
    /// panicking or dropping the hint.
    #[test]
    fn footer_shows_unbound_for_missing_binding() {
        let mut app = App::new_sidebar("demo");
        // Leave keymaps at default (empty) — no binding for either action.
        let joined = footer_text(&mut app);
        assert!(
            joined.contains("<unbound> palette  <unbound> quit"),
            "expected <unbound> markers in:\n{joined}"
        );
    }

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
            .unwrap_or_else(|| {
                panic!(
                    "expected row containing {needle:?} in:\n{}",
                    rows.join("\n")
                )
            })
    }

    /// On state: full-width green band carrying `ZEN MODE ON`, one blank
    /// row above it, the palette/quit keybind line above that.
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
        let keybind_y = row_y(&rows, "palette");
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
        assert!(
            !joined.contains("ZEN MODE ON"),
            "no green band when off, got:\n{joined}"
        );
        assert!(
            joined.contains("⌥Z Zen mode"),
            "expected hotkey hint sourced from configured chord in:\n{joined}"
        );
        let keybind_y = row_y(&rows, "palette");
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
        let off_y = row_y(&off_rows, "palette");

        let backend_on = TestBackend::new(24, 16);
        let mut term_on = Terminal::new(backend_on).unwrap();
        app.zen_mode = ZenModeState::On;
        term_on
            .draw(|f| render_full(f, &mut app, f.area()))
            .unwrap();
        let on_rows = dump(&term_on);
        let on_y = row_y(&on_rows, "palette");

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
        assert!(
            !joined.contains("ZEN MODE ON"),
            "paused must not show green band, got:\n{joined}"
        );
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
        assert!(
            !joined.contains("Zen mode"),
            "no hint expected when no chord bound, got:\n{joined}"
        );
    }

    /// Multi-machine layout reads like the wireframe: the renamed
    /// `Workspaces` section, a `▾ <machine>` header per declared host,
    /// each workspace name on its own row with its agent (or `idle`)
    /// right-aligned. Asserting on the textual buffer keeps the rename +
    /// grouping change anchored in the visible output, not just the row
    /// shape.
    #[test]
    fn workspaces_section_renders_grouped_layout_with_agent_column() {
        let backend = TestBackend::new(28, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.workspaces = vec![
            crate::app::WorkspaceOverview {
                name: "alpha".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: Some("t-1".into()),
                badge: crate::app::WorkspaceBadge::Working,
                agent: Some("developer".into()),
            },
            crate::app::WorkspaceOverview {
                name: "charlie".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                badge: crate::app::WorkspaceBadge::Idle,
                agent: None,
            },
            crate::app::WorkspaceOverview {
                name: "delta".into(),
                machine: "devbox".into(),
                is_remote: true,
                current_task: Some("t-2".into()),
                badge: crate::app::WorkspaceBadge::Working,
                agent: Some("developer".into()),
            },
        ];
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let rows = dump(&term);
        let joined = rows.join("\n");

        assert!(
            joined.contains("Workspaces"),
            "section header must read 'Workspaces' post-rebrand, got:\n{joined}"
        );
        assert!(
            !joined.contains("— Agents —"),
            "old 'Agents' label must be gone, got:\n{joined}"
        );
        assert!(
            joined.contains("▾ hub"),
            "expected '▾ hub' machine header in multi-machine layout, got:\n{joined}"
        );
        assert!(
            joined.contains("▾ devbox"),
            "expected '▾ devbox' machine header in multi-machine layout, got:\n{joined}"
        );
        // Title-cased agent on the active row, dim `idle` on the idle one
        // — same wireframe vocabulary the task spec calls out.
        let alpha_y = row_y(&rows, "alpha");
        assert!(
            rows[alpha_y].contains("Developer"),
            "active workspace row must surface title-cased agent, got: {:?}",
            rows[alpha_y]
        );
        let charlie_y = row_y(&rows, "charlie");
        assert!(
            rows[charlie_y].contains("idle"),
            "idle workspace row must surface the `idle` placeholder, got: {:?}",
            rows[charlie_y]
        );
        // Grouped rows indent under their machine header (two-column
        // shift) so they read as nested rather than peers of the divider.
        assert!(
            rows[alpha_y].starts_with("   "),
            "indented workspace rows expect ≥3 leading spaces (sidebar pad + indent), got: {:?}",
            rows[alpha_y]
        );
    }

    /// A collapsed machine renders `▸ <name>` plus a right-aligned
    /// `(<total>, <active> active)` suffix, and the workspace rows
    /// beneath it vanish from the rendered buffer. The expanded
    /// machine's rows continue to use their badge glyph (`⏵` for an
    /// active workspace's Working badge) — distinct from the collapsed
    /// `▸` so the two never collide on screen.
    #[test]
    fn collapsed_machine_renders_count_suffix_and_hides_workspaces() {
        let backend = TestBackend::new(40, 20);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.workspaces = vec![
            crate::app::WorkspaceOverview {
                name: "alpha".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: Some("t-1".into()),
                badge: crate::app::WorkspaceBadge::Working,
                agent: Some("developer".into()),
            },
            crate::app::WorkspaceOverview {
                name: "bravo".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                badge: crate::app::WorkspaceBadge::Idle,
                agent: None,
            },
            crate::app::WorkspaceOverview {
                name: "charlie".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                badge: crate::app::WorkspaceBadge::Idle,
                agent: None,
            },
            crate::app::WorkspaceOverview {
                name: "delta".into(),
                machine: "devbox".into(),
                is_remote: true,
                current_task: None,
                badge: crate::app::WorkspaceBadge::Idle,
                agent: None,
            },
        ];
        app.collapsed_machines.insert("hub".into());
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let rows = dump(&term);
        let joined = rows.join("\n");

        // The collapsed glyph + name appear, but with no per-workspace
        // rows under hub — alpha/bravo/charlie are gone.
        let hub_y = row_y(&rows, "▸ hub");
        assert!(
            rows[hub_y].contains("(3, 1 active)"),
            "collapsed hub must surface count suffix, got: {:?}",
            rows[hub_y]
        );
        assert!(
            !joined.contains("alpha"),
            "collapsed hub must hide its workspaces, got:\n{joined}"
        );
        assert!(
            !joined.contains("bravo") && !joined.contains("charlie"),
            "collapsed hub must hide all workspace rows, got:\n{joined}"
        );
        // devbox is still expanded — its single workspace renders.
        assert!(
            joined.contains("▾ devbox"),
            "devbox stays expanded, got:\n{joined}"
        );
        assert!(
            joined.contains("delta"),
            "expanded devbox surfaces its workspace, got:\n{joined}"
        );
    }

    /// Active workspaces use the `Working` badge glyph (`⏵`). Collapsed
    /// machines use `▸`. The two never share a row, and they're distinct
    /// glyphs — acceptance criterion (e) from the task spec.
    #[test]
    fn collapsed_machine_glyph_and_active_workspace_glyph_do_not_collide() {
        let backend = TestBackend::new(40, 20);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.workspaces = vec![
            crate::app::WorkspaceOverview {
                name: "alpha".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: Some("t-1".into()),
                badge: crate::app::WorkspaceBadge::Working,
                agent: Some("developer".into()),
            },
            crate::app::WorkspaceOverview {
                name: "delta".into(),
                machine: "devbox".into(),
                is_remote: true,
                current_task: Some("t-2".into()),
                badge: crate::app::WorkspaceBadge::Working,
                agent: Some("developer".into()),
            },
        ];
        // Collapse devbox; leave hub expanded so an active workspace
        // glyph and a collapsed machine glyph share the same render.
        app.collapsed_machines.insert("devbox".into());
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let rows = dump(&term);
        let joined = rows.join("\n");

        let working_glyph = crate::app::WorkspaceBadge::Working.glyph();
        assert_ne!(
            working_glyph, "▸",
            "Working badge must not reuse the collapsed-machine glyph"
        );
        assert!(joined.contains("▾ hub"));
        let alpha_y = row_y(&rows, "alpha");
        assert!(
            rows[alpha_y].contains(working_glyph),
            "expanded hub's active workspace must use the Working badge ({working_glyph}), got: {:?}",
            rows[alpha_y]
        );
        let devbox_y = row_y(&rows, "▸ devbox");
        // The collapsed header row carries `▸` but NOT the active
        // workspace glyph (the per-row badge stays with the row, even
        // though the row is hidden).
        assert!(
            !rows[devbox_y].contains(working_glyph),
            "collapsed machine header must not borrow the workspace badge glyph, got: {:?}",
            rows[devbox_y]
        );
    }

    /// Single-machine layout collapses the group header — the section
    /// goes straight to a flat list of workspace rows with no indent and
    /// no `▾ <machine>` divider. Adding a second machine to the same
    /// project flips back to grouped form (verified by the test above).
    #[test]
    fn single_machine_skips_group_header_and_indent() {
        let backend = TestBackend::new(28, 16);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.workspaces = vec![crate::app::WorkspaceOverview {
            name: "alpha".into(),
            machine: "hub".into(),
            is_remote: false,
            current_task: Some("t-1".into()),
            badge: crate::app::WorkspaceBadge::Working,
            agent: Some("qa".into()),
        }];
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let rows = dump(&term);
        let joined = rows.join("\n");

        assert!(joined.contains("Workspaces"));
        assert!(
            !joined.contains("▾ hub"),
            "single-machine projects must not emit the group header, got:\n{joined}"
        );
        let alpha_y = row_y(&rows, "alpha");
        // Sidebar pad is 1 col; flat rows skip the extra indent, so the
        // first non-space column lands at index 1 (the badge glyph).
        assert!(
            !rows[alpha_y].starts_with("   "),
            "flat-list workspace rows must not carry the indent, got: {:?}",
            rows[alpha_y]
        );
        assert!(
            rows[alpha_y].contains("Qa"),
            "agent name must title-case verbatim (no special-casing for known acronyms), got: {:?}",
            rows[alpha_y]
        );
    }

    /// Both review sections render as two-line entries (spec §16): line 1 is
    /// the decoration badge + title (+ a right-aligned `machine:port` URL for
    /// a Ready item), line 2 is the branch, dim. Ready uses ✓; Queued uses ·.
    #[test]
    fn review_sections_render_two_line_entries_with_badge_url_and_branch() {
        let backend = TestBackend::new(44, 24);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = App::new_sidebar("demo");
        app.ready_review = vec![crate::app::ReviewEntry {
            task_id: "palette".into(),
            title: "Palette fuzzy-match fix".into(),
            branch: "shelbi/palette-fuzzy-match-fix".into(),
            location: Some("hub:3000".into()),
        }];
        app.queued_review = vec![crate::app::ReviewEntry {
            task_id: "onboarding".into(),
            title: "Rework onboarding copy".into(),
            branch: "shelbi/rework-onboarding-copy".into(),
            location: None,
        }];
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let rows = dump(&term);
        let joined = rows.join("\n");

        assert!(
            joined.contains("Ready for Review"),
            "Ready header, got:\n{joined}"
        );
        assert!(
            joined.contains("Queued for Review"),
            "Queued header, got:\n{joined}"
        );

        // Ready item: ✓ badge, title, and the machine:port URL on line 1;
        // branch dim on the very next line.
        let ready_y = row_y(&rows, "Palette fuzzy-match fix");
        assert!(
            rows[ready_y].contains('✓'),
            "Ready row carries ✓, got: {:?}",
            rows[ready_y]
        );
        assert!(
            rows[ready_y].contains("hub:3000"),
            "Ready row's line 1 carries the machine:port URL, got: {:?}",
            rows[ready_y]
        );
        assert!(
            rows[ready_y + 1].contains("shelbi/palette-fuzzy-match-fix"),
            "branch renders on the line directly below the title, got: {:?}",
            rows[ready_y + 1]
        );

        // Queued item: · badge, no URL; branch on the next line.
        let queued_y = row_y(&rows, "Rework onboarding copy");
        assert!(
            rows[queued_y].contains('·'),
            "Queued row carries ·, got: {:?}",
            rows[queued_y]
        );
        assert!(
            !rows[queued_y].contains(':'),
            "a queued item has no location badge, got: {:?}",
            rows[queued_y]
        );
        assert!(
            rows[queued_y + 1].contains("shelbi/rework-onboarding-copy"),
            "queued branch renders directly below its title, got: {:?}",
            rows[queued_y + 1]
        );

        // Ready section sits above Queued.
        assert!(
            row_y(&rows, "Ready for Review") < row_y(&rows, "Queued for Review"),
            "Ready section must render above Queued, got:\n{joined}"
        );
    }
}
