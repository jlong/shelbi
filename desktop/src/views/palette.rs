//! The command palette, drawn.
//!
//! The Figma Command palette frame (dark 2171:628, light 2236:24244; the
//! overlay is 2171:752): over the scrim, 200pt down, a raised panel with 8pt
//! corners and a soft drop, that slides up and fades in. It is as tall as its
//! current results (so it grows and shrinks as the query filters, scrolling
//! inside once it reaches the window's foot). The query line (a `❯` prompt,
//! the caret in `text/cyan`, the query or the muted placeholder; no box),
//! then the results: a 20pt icon, the name in a fixed column, the
//! description in a second one, the selected row on the overlay fill; a
//! Projects column on the right, the switcher's list ([`project_menu`]: a
//! dot, the name and a check on the current project, then "+ Add
//! project"); a muted footer hint.
//!
//! The rows are the registry's entries (`CommandRegistry::entries`) in the
//! desktop's copy and order ([`present`]), filtered by
//! `shelbi_palette::search`, through the shared state machine
//! (`shelbi_app::overlay::palette`); this file only words and paints them.
//!
//! Typing searches the project's issues and Changelog entries as well
//! (`shelbi_app::overlay::palette_search`): while the query isn't blank the
//! Projects column steps aside, a filter row (the app's segmented control:
//! All, Issues, Changelog Entries, Commands, each with its count) sits under
//! the query line, and the results are grouped by kind under All, a muted
//! heading over each group and a "Show all" row closing a capped one.

use gpui::{div, prelude::*, px, AnyElement, BoxShadow, Context, Hsla, Pixels, ScrollHandle, Size};
use shelbi_app::overlay::palette::{Palette, PaletteFocus};
use shelbi_app::overlay::palette_search::{FilterChip, ResultItem, ResultKind, ResultRow, SearchFilter};
use shelbi_palette::{DecorationColor, Entry};
use shelbi_state::keymap::{DisplayStyle, Keymaps};

use crate::assets::{heroicon, icons, Icon};
use crate::elements::controls;
use crate::theme::motion::fill_layer;
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;
use crate::views::overlay::{palette_event, palette_top, with_overlay, ActiveOverlay};
use crate::views::overlay::{OverlayEvent, OverlayHost};
use crate::views::project_menu;

/// The panel's width: the frame's 788pt (a 580pt commands column and the
/// 160pt Projects column, 16pt between them and either side).
const MAX_WIDTH: f32 = 788.0;
/// The name column's width, so every description starts at the same x.
const NAME_W: f32 = 220.0;
/// A result row's height.
pub const ROW_H: f32 = 32.0;
/// A group heading's height while searching.
const HEADING_H: f32 = 28.0;
/// The query line's height.
const QUERY_H: f32 = 44.0;
/// The Projects column's width, and the narrowest commands column it may
/// squeeze before it is dropped.
const PROJECTS_W: f32 = 160.0;
const COMMANDS_MIN_W: f32 = 320.0;
/// The rows' corners and their inset, gap: Figma's `6`.
const ROW_RADIUS: f32 = 6.0;
/// The placeholder on an empty query.
pub const PLACEHOLDER: &str = "Type a command or search";
/// The query line's prompt glyph.
const PROMPT: &str = "❯";

/// The commands the frame lists on an empty query, in its order, with the
/// desktop's name (`None` keeps the registry's, as the Zen toggle's flips
/// with its state) and description. The palette calls Quit Project "Close
/// project". Activity, off the sidebar now, is listed after the Changelog so
/// it stays one keystroke away.
const DEFAULT_LIST: &[(&str, Option<&str>, &str)] = &[
    ("view:orch", Some("Orchestrator"), "Chat with your Orchestrator agent"),
    ("view:tasks", Some("Issues"), "Manage what Shelbi is working on here"),
    ("view:changelog", Some("Changelog"), "Review what your agents have finished"),
    ("view:activity", Some("Activity"), "See what\u{2019}s happened recently"),
    ("view:machines", Some("Machines"), "See the machines running your agents"),
    ("action:toggle-zen", None, "Let Shelbi handle your entire workflow"),
    ("action:error-log", Some("Error log"), "Review recent errors and warnings"),
    ("action:detach", Some("Detach"), "Leave Shelbi running in the background"),
    ("action:quit-project", Some("Close project"), "Stop this project\u{2019}s agents and close it"),
    ("action:quit-shelbi", Some("Quit Shelbi"), "Shut down Shelbi"),
];

/// The registry's entries as the desktop's palette lists them: the frame's
/// commands first, in its order and copy, then every other entry (the
/// review rows, workspaces, Detach Other Clients, the editors, ...) waiting
/// for a query that matches it. The search sorts stably, so a blank query
/// keeps this order.
pub fn present(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = DEFAULT_LIST
        .iter()
        .filter_map(|(id, name, description)| {
            let mut entry = entries.iter().find(|e| e.id == *id)?.clone();
            if let Some(name) = name {
                entry.label = (*name).to_string();
            }
            entry.subtitle = Some((*description).to_string());
            entry.hidden_until_query = false;
            Some(entry)
        })
        .collect();
    out.extend(
        entries
            .into_iter()
            .filter(|e| !DEFAULT_LIST.iter().any(|(id, _, _)| *id == e.id))
            .map(|e| Entry {
                hidden_until_query: true,
                ..e
            }),
    );
    out
}

/// The panel's width in a window `window_w` wide: 788pt, a narrower window
/// keeping 16pt either side.
pub fn panel_width(window_w: f32) -> f32 {
    (window_w - 32.0).clamp(0.0, MAX_WIDTH)
}

/// The Projects column's width for a results band `band_w` wide, or `None`
/// when the commands would be too narrow beside it (the TUI collapses to one
/// column the same way).
pub fn projects_width(band_w: f32, has_projects: bool) -> Option<f32> {
    (has_projects && band_w - PROJECTS_W - 16.0 >= COMMANDS_MIN_W).then_some(PROJECTS_W)
}

/// A row's icon and its color: the frame's drawing for its commands (2171:628;
/// its "Change log" rocket belongs to the Changelog command, so Activity
/// keeps the sidebar's bolt), a Heroicon from the same outline set for the
/// rest, in the icon color (a workspace's or a review's in its sidebar
/// status tint).
pub fn icon_for(entry: &Entry, theme: &Theme) -> (Icon, Hsla) {
    let id = entry.id.as_str();
    let icon = match id {
        "view:orch" => icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS,
        "view:tasks" => icons::RECTANGLE_STACK,
        "view:changelog" => icons::ROCKET_LAUNCH,
        "view:activity" => icons::BOLT,
        "view:machines" => icons::SERVER_STACK,
        "action:detach" => icons::BOLT_SLASH,
        "action:toggle-zen" if entry.label.ends_with(" off") => icons::TOGGLE_ON,
        "action:toggle-zen" => icons::TOGGLE_OFF,
        "action:error-log" => icons::PAGE_CURL,
        // The sidebar footer's megaphone and cog.
        "action:send-feedback" => icons::MEGAPHONE,
        "action:settings" => icons::COG_6_TOOTH,
        "action:quit-project" => icons::X_MARK,
        "action:quit-shelbi" => icons::FLAG_FINISH,
        "action:detach-others" => icons::SIGNAL_SLASH,
        "action:add-project" | "action:new-window" => icons::PLUS,
        _ if id.starts_with("action:switch-project") => icons::ARROWS_RIGHT_LEFT,
        _ if id.starts_with("action:approve-review:") => icons::CHECK,
        _ if id.starts_with("action:reject-review:") => icons::X_MARK,
        _ if id.starts_with("review:") => icons::CHECK,
        _ if id.starts_with("workspace:") => icons::COMMAND_LINE,
        _ if id.starts_with("edit:") => icons::PENCIL_SQUARE,
        _ if id.starts_with("view:") => icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS,
        _ => icons::BOLT,
    };
    let tinted = id.starts_with("workspace:") || id.starts_with("review:");
    let color = match entry.decoration.as_ref().map(|d| d.color) {
        Some(c) if tinted && c != DecorationColor::Default => theme.decoration(c),
        _ => theme.icon,
    };
    (icon, color)
}

/// The footer's close chord: the first palette-open chord, as macOS spells
/// it (`⌃P`).
pub fn close_chord(keymaps: &Keymaps) -> String {
    shelbi_app::overlay::palette_open_chords(keymaps)
        .first()
        .map(|c| crate::keys::chord_text(c, DisplayStyle::Mac))
        .unwrap_or_default()
}

/// The palette's drop: the frame's `0 10 15 -3` effect, its blur through
/// [`style::figma_blur`] (at 15 the drop spread twice as far as the
/// frame's).
fn shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: theme.palette_shadow,
        offset: gpui::point(px(0.0), px(10.0)),
        blur_radius: style::figma_blur(15.0),
        spread_radius: px(-3.0),
        inset: false,
    }]
}

pub fn render<H: OverlayHost>(
    palette: &Palette,
    scroll: &ScrollHandle,
    size: Size<Pixels>,
    keymaps: &Keymaps,
    theme: &Theme,
    cx: &mut gpui::Context<H>,
) -> AnyElement {
    let width = panel_width(size.width.as_f32());
    let top = palette_top(size.height.as_f32());
    let max_h = (size.height.as_f32() - top - 40.0).max(120.0);
    let rows = palette.rows();
    let selected = palette.selected_row();
    let commands_focused = palette.focus() == PaletteFocus::Commands && !palette.filter_focused();
    // The band sits 16pt in from each side.
    let projects_w = projects_width(width - 32.0, palette.shows_projects());

    let commands = if rows.is_empty() {
        let empty = if palette.search_pending() {
            "Searching\u{2026}"
        } else if palette.searching() {
            palette.filter().empty_label()
        } else {
            SearchFilter::Commands.empty_label()
        };
        // In the first row's slot: the column grows to the Projects
        // column's height, and the message stays at its top.
        div().id("palette-results").child(
            div()
                .debug_selector(|| "palette-no-match".into())
                .h(px(ROW_H))
                .px(px(ROW_RADIUS))
                .flex()
                .items_center()
                .type_style(Type::Base)
                .text_color(theme.muted)
                .child(empty),
        )
    } else {
        let rows: Vec<AnyElement> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| render_result(i, row, commands_focused && selected == Some(i), theme, cx))
            .collect();
        div()
            .id("palette-results")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(scroll)
            .children(rows)
    };

    let band = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .gap_4()
        .px_4()
        .child(div().flex_1().min_w_0().flex().flex_col().min_h_0().child(commands.flex_1().min_h_0()))
        .when_some(projects_w, |band, w| {
            band.child(render_projects(palette, w, theme, cx))
        });

    div()
        .id("palette")
        .debug_selector(|| "palette".into())
        .flex()
        .flex_col()
        .overflow_hidden()
        .w(px(width))
        .max_h(px(max_h))
        .bg(theme.raised)
        .rounded_lg()
        .shadow(shadow(theme))
        .text_color(theme.foreground)
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(query_line(palette.query(), theme))
        .when(palette.searching(), |panel| {
            panel.child(filter_row(&palette.chips(), palette.filter_focused(), theme, cx))
        })
        .child(band)
        .child(
            div()
                .debug_selector(|| "palette-footer".into())
                .px_4()
                .pt_2()
                .pb_3()
                .type_style(Type::XSmall)
                .text_color(theme.muted)
                .child(palette.footer(&close_chord(keymaps))),
        )
        .into_any_element()
}

/// The query line: the `❯` prompt, then the query (or the muted
/// placeholder) with the caret in `text/cyan`. No box, no rule.
fn query_line(query: &str, theme: &Theme) -> gpui::Div {
    let cursor = || {
        div()
            .debug_selector(|| "palette-cursor".into())
            .flex_none()
            .w(px(2.0))
            .h(px(18.0))
            .bg(theme.link)
    };
    let text = div().flex().flex_row().items_center().min_w_0();
    let text = if query.is_empty() {
        text.child(cursor())
            .child(div().text_color(theme.muted).child(PLACEHOLDER))
    } else {
        text.child(
            div()
                .whitespace_nowrap()
                .text_color(theme.text_loud)
                .child(query.replace(' ', "\u{00a0}")),
        )
        .child(cursor())
    };
    div()
        .debug_selector(|| "palette-query".into())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_2()
        .h(px(QUERY_H))
        .px_4()
        .type_style(Type::Base)
        .child(div().flex_none().text_color(theme.foreground).child(PROMPT))
        .child(text)
}

/// The filter row: the app's segmented control, one segment per filter
/// with its count, the chosen one on the selected chip. While it has the
/// keys (↑ from the first result) the track wears the focus ring.
fn filter_row<H: OverlayHost>(chips: &[FilterChip], focused: bool, theme: &Theme, cx: &mut Context<H>) -> AnyElement {
    let segments = chips.iter().map(|chip| {
        let filter = chip.filter;
        controls::segment(format!("palette-filter-{}", chip.label), chip.label, chip.selected, true, theme)
            .gap(px(6.0))
            .child(
                div()
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(chip.count.to_string()),
            )
            .on_click(cx.listener(move |this: &mut H, _, window, cx| {
                with_overlay(this, window, cx, |o| match o {
                    ActiveOverlay::Palette { palette, .. } => {
                        palette.click_filter(filter);
                        OverlayEvent::Stay
                    }
                    _ => OverlayEvent::Stay,
                })
            }))
    });
    let track = controls::segmented_track(theme)
        .gap(px(2.0))
        .when(focused, |t| t.border_1().border_color(theme.focus_ring))
        .children(segments);
    div()
        .debug_selector(|| "palette-filters".into())
        .flex()
        .flex_row()
        .flex_none()
        .px_4()
        .pb_2()
        .child(track)
        .into_any_element()
}

/// One row of the results band: a group's heading, or a result.
fn render_result<H: OverlayHost>(
    index: usize,
    row: &ResultRow,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    match row {
        ResultRow::Header(kind) => div()
            .debug_selector(move || format!("palette-heading-{}", kind.heading()))
            .flex()
            .flex_none()
            .items_end()
            .h(px(HEADING_H))
            .px(px(ROW_RADIUS))
            .pb_1()
            .type_style(Type::SmallMedium)
            .text_color(theme.muted)
            .child(kind.heading())
            .into_any_element(),
        ResultRow::Item(ResultItem::Command(entry)) => render_row(index, entry, selected, theme, cx),
        ResultRow::Item(ResultItem::Issue(doc)) => detail_row(
            index,
            format!("palette-issue-{}", doc.id),
            style::status_icon(doc.category, theme).into_any_element(),
            &doc.title,
            doc.detail(),
            selected,
            theme,
            cx,
        ),
        ResultRow::Item(ResultItem::Changelog(hit)) => detail_row(
            index,
            format!("palette-entry-{}", hit.id),
            heroicon(icons::ROCKET_LAUNCH).text_color(theme.icon).into_any_element(),
            &hit.title,
            hit.detail.clone(),
            selected,
            theme,
            cx,
        ),
        ResultRow::Item(ResultItem::ShowAll { kind, count }) => show_all_row(index, *kind, *count, selected, theme, cx),
    }
}

/// A result row's frame: 32pt, 6pt corners, the selected one on the
/// overlay fill, a fill fading in under the pointer; a click activates row
/// `index` of the palette's rows.
fn row_frame<H: OverlayHost>(
    index: usize,
    key: String,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> gpui::Stateful<gpui::Div> {
    let selector = key.clone();
    let row = div()
        .id(("palette-row", index))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(px(6.0))
        .h(px(ROW_H))
        .px(px(6.0))
        .rounded(px(ROW_RADIUS))
        .cursor_pointer()
        .type_style(Type::Base);
    let row = if selected {
        row.bg(theme.background)
    } else {
        let clear = theme.background.opacity(0.0);
        fill_layer(row, key, px(ROW_RADIUS), [clear, theme.background, theme.background])
    };
    row.on_click(cx.listener(move |this: &mut H, _, window, cx| {
        with_overlay(this, window, cx, |o| match o {
            ActiveOverlay::Palette { palette, .. } => palette_event(palette.click_row(index)),
            _ => OverlayEvent::Stay,
        })
    }))
}

/// A result's name: Base medium, loud and semibold when selected.
fn name(label: &str, selected: bool, theme: &Theme) -> gpui::Div {
    div()
        .truncate()
        .when(selected, |l| {
            l.debug_selector(|| "palette-selected".into())
                .type_style(Type::BaseSemibold)
                .text_color(theme.text_loud)
        })
        .when(!selected, |l| l.type_style(Type::BaseMedium).text_color(theme.foreground))
        .child(label.to_string())
}

/// An issue's or a Changelog entry's row: its glyph (the issue's status, the
/// Changelog's rocket), the title, then the detail (`in progress · #1931`,
/// `Oct 28 · #1902`) at the right.
#[allow(clippy::too_many_arguments)]
fn detail_row<H: OverlayHost>(
    index: usize,
    key: String,
    icon: AnyElement,
    title: &str,
    detail: String,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    row_frame(index, key, selected, theme, cx)
        .child(div().flex_none().size(px(20.0)).flex().items_center().justify_center().child(icon))
        .child(name(title, selected, theme).flex_1().min_w_0())
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(if selected { theme.foreground } else { theme.muted })
                .child(detail),
        )
        .into_any_element()
}

/// The row closing a capped group: `Show all 12 issues` in the link color,
/// where the names start.
fn show_all_row<H: OverlayHost>(
    index: usize,
    kind: ResultKind,
    count: usize,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    row_frame(index, format!("palette-show-all-{}", kind.heading()), selected, theme, cx)
        .child(heroicon(icons::ARROW_RIGHT).text_color(theme.link))
        .child(
            div()
                .truncate()
                .type_style(if selected { Type::BaseSemibold } else { Type::BaseMedium })
                .text_color(theme.link)
                .child(kind.show_all_label(count)),
        )
        .into_any_element()
}

fn render_row<H: OverlayHost>(
    index: usize,
    entry: &Entry,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let (icon, icon_color) = icon_for(entry, theme);
    let id = entry.id.clone();
    let key = format!("palette-row-{id}");
    let row = div()
        .id(("palette-row", index))
        .debug_selector(move || format!("palette-row-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .gap(px(6.0))
        .h(px(ROW_H))
        .px(px(6.0))
        .rounded(px(ROW_RADIUS))
        .cursor_pointer()
        .type_style(Type::Base);
    let row = if selected {
        row.bg(theme.background)
    } else {
        let clear = theme.background.opacity(0.0);
        fill_layer(row, key, px(ROW_RADIUS), [clear, theme.background, theme.background])
    };
    row.child(heroicon(icon).text_color(icon_color))
        .child(
            div()
                .w(px(NAME_W))
                .flex_none()
                .truncate()
                .when(selected, |l| {
                    l.debug_selector(|| "palette-selected".into())
                        .type_style(Type::BaseSemibold)
                        .text_color(theme.text_loud)
                })
                .when(!selected, |l| {
                    l.type_style(Type::BaseMedium).text_color(theme.foreground)
                })
                .child(entry.label.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if selected { theme.foreground } else { theme.muted })
                .child(entry.subtitle.clone().unwrap_or_default()),
        )
        .on_click(cx.listener(move |this: &mut H, _, window, cx| {
            with_overlay(this, window, cx, |o| match o {
                ActiveOverlay::Palette { palette, .. } => palette_event(palette.click_row(index)),
                _ => OverlayEvent::Stay,
            })
        }))
        .into_any_element()
}

/// The Projects column: the switcher's list ([`project_menu`]), its rows
/// clicked through the palette's state machine.
fn render_projects<H: OverlayHost>(
    palette: &Palette,
    width: f32,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let selection = palette.project_selection();
    let rows = palette.projects().iter().enumerate().map(|(i, p)| {
        let row = project_menu::project_row(
            ("palette-project", i),
            format!("palette-project-{}", p.label),
            p,
            selection == Some(i),
            theme,
        );
        clicked(row, i, selection == Some(i), cx)
    });
    let add = palette.projects().len();
    let add_row = project_menu::add_row(
        ("palette-project", add),
        "palette-project-Add project",
        selection == Some(add),
        theme,
    );
    div()
        .debug_selector(|| "palette-projects".into())
        .w(px(width))
        .flex_none()
        .flex()
        .flex_col()
        .child(project_menu::heading(theme))
        .children(rows)
        .child(project_menu::divider(theme))
        .child(clicked(add_row, add, selection == Some(add), cx))
        .into_any_element()
}

/// A Projects-column row that activates through the palette when clicked.
fn clicked<H: OverlayHost>(
    row: gpui::Stateful<gpui::Div>,
    index: usize,
    selected: bool,
    cx: &mut Context<H>,
) -> AnyElement {
    row.when(selected, |r| r.debug_selector(|| "palette-project-selected".into()))
        .on_click(cx.listener(move |this: &mut H, _, window, cx| {
            with_overlay(this, window, cx, |o| match o {
                ActiveOverlay::Palette { palette, .. } => palette_event(palette.click_project(index)),
                _ => OverlayEvent::Stay,
            })
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::command::{CommandModel, CommandRegistry};
    use shelbi_palette::{Decoration, EntryKind};

    fn entry(id: &str, decoration: Option<Decoration>) -> Entry {
        Entry {
            id: id.into(),
            label: "X".into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration,
            hidden_until_query: false,
        }
    }

    fn theme() -> Theme {
        Theme::resolve(
            shelbi_state::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        )
    }

    #[test]
    fn the_panel_is_the_frames_width_with_a_floor() {
        assert_eq!(panel_width(1440.0), 788.0, "the frame's 788pt");
        assert_eq!(panel_width(600.0), 568.0, "16px either side");
        assert_eq!(panel_width(300.0), 268.0, "capped to the window");
    }

    #[test]
    fn the_projects_column_collapses_on_a_narrow_panel() {
        assert_eq!(projects_width(756.0, true), Some(160.0), "the frame's 580 + 16 + 160");
        assert_eq!(projects_width(496.0, true), Some(160.0));
        assert_eq!(projects_width(495.0, true), None);
        assert_eq!(projects_width(756.0, false), None);
    }

    #[test]
    fn icons_are_the_frames_drawings_in_the_icon_color() {
        let theme = theme();
        let (icon, color) = icon_for(&entry("view:orch", None), &theme);
        assert_eq!((icon, color), (icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS, theme.icon));
        assert_eq!(icon_for(&entry("action:error-log", None), &theme).0, icons::PAGE_CURL);
        assert_eq!(icon_for(&entry("action:quit-shelbi", None), &theme).0, icons::FLAG_FINISH);
        assert_eq!(icon_for(&entry("view:tasks", None), &theme).0, icons::RECTANGLE_STACK);
        assert_eq!(icon_for(&entry("view:machines", None), &theme).0, icons::SERVER_STACK);
        assert_eq!(icon_for(&entry("action:detach", None), &theme).0, icons::BOLT_SLASH);
        assert_eq!(icon_for(&entry("action:quit-project", None), &theme).0, icons::X_MARK);
        // Activity keeps the sidebar's drawing; the rocket is Changelog's.
        assert_eq!(
            icon_for(&entry("view:activity", None), &theme).0,
            crate::views::sidebar::nav_icon(&shelbi_app::nav::View::Activity)
        );
        let mut zen = entry("action:toggle-zen", None);
        zen.label = "Turn Zen Mode on".into();
        assert_eq!(icon_for(&zen, &theme).0, icons::TOGGLE_OFF);
        zen.label = "Turn Zen Mode off".into();
        assert_eq!(icon_for(&zen, &theme).0, icons::TOGGLE_ON);
        // A workspace keeps its sidebar status tint.
        let working = entry(
            "workspace:alpha",
            Some(Decoration {
                glyph: "▶".into(),
                color: DecorationColor::Green,
            }),
        );
        assert_eq!(icon_for(&working, &theme), (icons::COMMAND_LINE, theme.icon_green));
    }

    /// A model with every kind of entry: the nav views, a review, a
    /// workspace, another project, another client, Zen on.
    fn model() -> CommandModel {
        use shelbi_app::command::{ProjectItem, ReviewItem, ViewItem, WorkspaceItem};
        use shelbi_app::nav::View;
        let view = |id: &str, title: &str, view: View| ViewItem {
            id: id.into(),
            title: title.into(),
            view,
            decoration: None,
        };
        CommandModel {
            project: Some("shelbi".into()),
            views: vec![
                view("view:orch", "Chat", View::Session("orch".into())),
                view("view:tasks", "Tasks", View::Issues),
                view("view:changelog", "Changelog", View::Changelog),
                view("view:activity", "Activity", View::Activity),
                view("view:machines", "Machines", View::Machines),
            ],
            zen_on: true,
            workspaces: vec![WorkspaceItem {
                name: "alpha".into(),
                subtitle: None,
                decoration: None,
            }],
            reviews: vec![ReviewItem {
                task_id: "T-9".into(),
                title: "Ship it".into(),
                subtitle: None,
                decoration: None,
            }],
            other_projects: vec![ProjectItem {
                slug: "web".into(),
                label: "Web".into(),
            }],
            other_clients_attached: true,
            ..CommandModel::default()
        }
    }

    #[test]
    fn a_blank_query_lists_the_frames_commands_in_its_order_and_copy() {
        let entries = present(CommandRegistry::new().entries(&model()));
        let listed: Vec<(String, String)> = shelbi_palette::search(&entries, "")
            .into_iter()
            .map(|(e, _)| (e.label, e.subtitle.unwrap_or_default()))
            .collect();
        let expected = [
            ("Orchestrator", "Chat with your Orchestrator agent"),
            ("Issues", "Manage what Shelbi is working on here"),
            ("Changelog", "Review what your agents have finished"),
            ("Activity", "See what\u{2019}s happened recently"),
            ("Machines", "See the machines running your agents"),
            ("Turn Zen Mode off", "Let Shelbi handle your entire workflow"),
            ("Error log", "Review recent errors and warnings"),
            ("Detach", "Leave Shelbi running in the background"),
            ("Close project", "Stop this project\u{2019}s agents and close it"),
            ("Quit Shelbi", "Shut down Shelbi"),
        ];
        let expected: Vec<(String, String)> =
            expected.iter().map(|(l, d)| (l.to_string(), d.to_string())).collect();
        assert_eq!(listed, expected);
    }

    #[test]
    fn commands_off_the_frame_wait_for_a_query() {
        let entries = present(CommandRegistry::new().entries(&model()));
        let ids = |q: &str| -> Vec<String> {
            shelbi_palette::search(&entries, q).into_iter().map(|(e, _)| e.id).collect()
        };
        assert!(ids("ship").contains(&"review:T-9".to_string()), "the review row");
        assert!(ids("detach other").contains(&"action:detach-others".to_string()));
        assert!(ids("alp").contains(&"workspace:alpha".to_string()));
        assert_eq!(ids("change").first().map(String::as_str), Some("view:changelog"));
        assert_eq!(ids("close").first().map(String::as_str), Some("action:quit-project"));
        // Every registry entry is still reachable.
        assert_eq!(entries.len(), CommandRegistry::new().entries(&model()).len());
    }

    #[test]
    fn the_footer_names_the_users_palette_chord() {
        let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        assert_eq!(close_chord(&keymaps), "⌃P");
    }
}
