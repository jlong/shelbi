//! The command palette, drawn.
//!
//! The TUI's look (`crates/shelbi-tui/src/overlay/palette.rs`): a panel
//! top-aligned a little below the window's top, 70% of the window wide, as
//! tall as its current results (so it grows and shrinks as the query filters,
//! scrolling inside once it reaches the window's height), in the `search`
//! fill with a muted border. A `❯` search line with a green block cursor (or
//! the dim placeholder), then the results: an icon slot, a fixed-width label,
//! the dim description and a right-aligned shortcut, the selected row on the
//! `selection` pill; a Projects column on the right; a dim footer hint.
//!
//! The rows are the registry's entries (`CommandRegistry::entries`) filtered
//! by `shelbi_palette::search`, through the shared state machine
//! (`shelbi_app::overlay::palette`); this file only paints them.

use std::time::Duration;

use gpui::{
    div, prelude::*, px, Animation, AnimationExt, AnyElement, Context, FontWeight, Hsla, Pixels,
    ScrollHandle, SharedString, Size,
};
use shelbi_app::overlay::palette::{Palette, PaletteFocus, ProjectIndicator};
use shelbi_palette::{DecorationColor, Entry, EntryKind};
use shelbi_state::keymap::{format_chord, DisplayStyle, Keymaps};

use crate::theme::Theme;
use crate::views::overlay::{palette_event, panel, with_overlay, ActiveOverlay, OverlayHost, PALETTE_TOP};

/// The panel's share of the window width, and its floor.
const WIDTH_PCT: f32 = 0.70;
const MIN_WIDTH: f32 = 360.0;
/// The label column's fixed width (the Figma's 211px), so every description
/// starts at the same x.
const LABEL_W: f32 = 211.0;
/// The icon slot.
const ICON_W: f32 = 28.0;
/// A result row's height: the TUI's two-row pill.
pub const ROW_H: f32 = 32.0;
/// The Projects column's width range, and the narrowest commands column it
/// may squeeze before it is dropped.
const PROJECTS_MIN_W: f32 = 160.0;
const PROJECTS_MAX_W: f32 = 270.0;
const COMMANDS_MIN_W: f32 = 270.0;
/// The placeholder on an empty query.
pub const PLACEHOLDER: &str = "Type a command or search";
/// The active project's breathing period (the TUI's).
const PULSE: Duration = Duration::from_millis(1143);

/// The panel's width in a window `window_w` wide.
pub fn panel_width(window_w: f32) -> f32 {
    (window_w * WIDTH_PCT).max(MIN_WIDTH).min((window_w - 32.0).max(0.0))
}

/// The Projects column's width for a results band `band_w` wide, or `None`
/// when the commands would be too narrow beside it (the TUI collapses to one
/// column the same way).
pub fn projects_width(band_w: f32, has_projects: bool) -> Option<f32> {
    if !has_projects {
        return None;
    }
    let w = (band_w / 3.0).clamp(PROJECTS_MIN_W, PROJECTS_MAX_W);
    (band_w - w - 16.0 >= COMMANDS_MIN_W).then_some(w)
}

/// A row's icon and its color: the decoration's glyph (the nav emoji) in its
/// tint, else the entry kind's glyph, the action lightning in yellow and the
/// quieter kinds in gray.
pub fn icon_for(entry: &Entry, theme: &Theme) -> (String, Hsla) {
    match &entry.decoration {
        Some(d) => {
            let color = match d.color {
                DecorationColor::Default => theme.foreground,
                other => theme.decoration(other),
            };
            (d.glyph.clone(), color)
        }
        None => {
            let glyph = entry.kind.icon();
            let color = if glyph == EntryKind::Action.icon() {
                theme.ansi_yellow
            } else {
                theme.ansi_dark_gray
            };
            (glyph.to_string(), color)
        }
    }
}

/// The footer's close chord: the first palette-open chord, as macOS spells
/// it (`⌃P`).
pub fn close_chord(keymaps: &Keymaps) -> String {
    shelbi_app::overlay::palette_open_chords(keymaps)
        .first()
        .map(|c| format_chord(c, DisplayStyle::Mac))
        .unwrap_or_default()
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
    let max_h = (size.height.as_f32() - 2.0 * PALETTE_TOP).max(120.0);
    let results = palette.results();
    let selected = palette.selected_in(results.len());
    let commands_focused = palette.focus() == PaletteFocus::Commands;
    // The band sits inside 16px of padding on each side.
    let projects_w = projects_width(width - 32.0, !palette.projects().is_empty());

    let commands = if results.is_empty() {
        div()
            .id("palette-results")
            .px_2()
            .py_1()
            .text_color(theme.muted)
            .child("No matching commands")
    } else {
        let rows: Vec<AnyElement> = results
            .iter()
            .enumerate()
            .map(|(i, (entry, _))| {
                render_row(i, entry, commands_focused && i == selected, theme, cx)
            })
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

    panel("palette", theme.muted, theme)
        .w(px(width))
        .max_h(px(max_h))
        .child(search_line(palette.query(), theme))
        .child(band)
        .child(
            div()
                .debug_selector(|| "palette-footer".into())
                .px_4()
                .pt_2()
                .pb_3()
                .text_xs()
                .text_color(theme.muted)
                .child(palette.footer(&close_chord(keymaps))),
        )
        .into_any_element()
}

/// The `❯` line: the dim placeholder behind a green block cursor, or the
/// query followed by the cursor.
fn search_line(query: &str, theme: &Theme) -> gpui::Div {
    let cursor = |text: SharedString| {
        div()
            .debug_selector(|| "palette-cursor".into())
            .bg(theme.green)
            .text_color(theme.on_color)
            .min_w(px(8.0))
            .h(px(18.0))
            .child(text)
    };
    let line = div()
        .debug_selector(|| "palette-query".into())
        .flex()
        .flex_row()
        .items_center()
        .px_4()
        .pt_3()
        .pb_3()
        .text_color(theme.foreground)
        .child(div().pr_2().child("❯"));
    if query.is_empty() {
        let mut chars = PLACEHOLDER.chars();
        let first: String = chars.by_ref().take(1).collect();
        let rest: String = chars.collect();
        line.child(cursor(first.into()))
            .child(div().text_color(theme.muted).child(rest))
    } else {
        line.child(div().whitespace_nowrap().child(query.replace(' ', "\u{00a0}")))
            .child(cursor(SharedString::default()))
    }
}

fn render_row<H: OverlayHost>(
    index: usize,
    entry: &Entry,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let (glyph, glyph_color) = icon_for(entry, theme);
    let id = entry.id.clone();
    div()
        .id(("palette-row", index))
        .debug_selector(move || format!("palette-row-{id}"))
        .flex()
        .flex_row()
        .items_center()
        .flex_none()
        .h(px(ROW_H))
        .px_1()
        .rounded_md()
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.selection))
        .when(!selected, |row| row.hover(|s| s.bg(theme.background)))
        .child(
            div()
                .w(px(ICON_W))
                .flex_none()
                .flex()
                .justify_center()
                .text_color(glyph_color)
                .child(glyph),
        )
        .child(
            div()
                .w(px(LABEL_W))
                .flex_none()
                .truncate()
                .when(selected, |l| {
                    l.debug_selector(|| "palette-selected".into())
                        .text_color(theme.ansi_white)
                        .font_weight(FontWeight::BOLD)
                })
                .when(!selected, |l| l.text_color(theme.foreground))
                .child(entry.label.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pl_2()
                .truncate()
                .text_color(theme.muted)
                .child(entry.subtitle.clone().unwrap_or_default()),
        )
        .when_some(entry.shortcut.clone(), |row, shortcut| {
            row.child(div().flex_none().pl_2().pr_1().text_color(theme.muted).child(shortcut))
        })
        .on_click(cx.listener(move |this: &mut H, _, window, cx| {
            with_overlay(this, window, cx, |o| match o {
                ActiveOverlay::Palette { palette, .. } => palette_event(palette.click_item(index)),
                _ => crate::views::overlay::OverlayEvent::Stay,
            })
        }))
        .into_any_element()
}

/// The Projects column: a dim heading, a row per project (status glyph and
/// name), and the trailing "+ Add project".
fn render_projects<H: OverlayHost>(
    palette: &Palette,
    width: f32,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let selection = palette.project_selection();
    let mut rows: Vec<AnyElement> = palette
        .projects()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let color = match p.indicator {
                ProjectIndicator::Active | ProjectIndicator::LoadedIdle => theme.green,
                ProjectIndicator::Unloaded => theme.ansi_dark_gray,
            };
            let glyph = div()
                .w(px(16.0))
                .flex_none()
                .text_color(color)
                .child(p.indicator.glyph());
            let glyph = if p.indicator == ProjectIndicator::Active {
                glyph
                    .with_animation(
                        ("palette-pulse", i),
                        Animation::new(PULSE)
                            .repeat()
                            .with_easing(gpui::pulsating_between(0.35, 1.0)),
                        |g, delta| g.opacity(delta),
                    )
                    .into_any_element()
            } else {
                glyph.into_any_element()
            };
            project_row(i, glyph, p.label.clone(), selection == Some(i), theme, cx)
        })
        .collect();
    let add = palette.projects().len();
    rows.push(project_row(
        add,
        div()
            .w(px(16.0))
            .flex_none()
            .text_color(theme.ansi_dark_gray)
            .child("+")
            .into_any_element(),
        "Add project".into(),
        selection == Some(add),
        theme,
        cx,
    ));
    div()
        .debug_selector(|| "palette-projects".into())
        .w(px(width))
        .flex_none()
        .flex()
        .flex_col()
        .child(div().px_2().pb_1().text_color(theme.muted).child("Projects"))
        .children(rows)
        .into_any_element()
}

fn project_row<H: OverlayHost>(
    index: usize,
    glyph: AnyElement,
    label: String,
    selected: bool,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let name = label.clone();
    div()
        .id(("palette-project", index))
        .debug_selector(move || format!("palette-project-{name}"))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .h(px(26.0))
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .text_color(theme.foreground)
        .when(selected, |r| r.bg(theme.selection).font_weight(FontWeight::BOLD))
        .when(!selected, |r| r.hover(|s| s.bg(theme.background)))
        .child(glyph)
        .child(div().truncate().child(label))
        .on_click(cx.listener(move |this: &mut H, _, window, cx| {
            with_overlay(this, window, cx, |o| match o {
                ActiveOverlay::Palette { palette, .. } => palette_event(palette.click_project(index)),
                _ => crate::views::overlay::OverlayEvent::Stay,
            })
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_palette::Decoration;

    fn entry(kind: EntryKind, decoration: Option<Decoration>) -> Entry {
        Entry {
            id: "x".into(),
            label: "X".into(),
            kind,
            subtitle: None,
            shortcut: None,
            decoration,
            hidden_until_query: false,
        }
    }

    #[test]
    fn the_panel_is_seventy_percent_wide_with_a_floor() {
        assert_eq!(panel_width(1000.0), 700.0);
        assert_eq!(panel_width(400.0), 360.0, "the floor");
        assert_eq!(panel_width(300.0), 268.0, "capped to the window");
    }

    #[test]
    fn the_projects_column_collapses_on_a_narrow_panel() {
        assert_eq!(projects_width(840.0, true), Some(270.0));
        assert_eq!(projects_width(600.0, true), Some(200.0));
        assert_eq!(projects_width(400.0, true), None);
        assert_eq!(projects_width(840.0, false), None);
    }

    #[test]
    fn icons_follow_the_tuis_rules() {
        let theme = Theme::resolve(
            shelbi_state::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        );
        let action = icon_for(&entry(EntryKind::Action, None), &theme);
        assert_eq!(action, ("⚡".to_string(), theme.ansi_yellow));
        let view = icon_for(&entry(EntryKind::View, None), &theme);
        assert_eq!(view, ("◉".to_string(), theme.ansi_dark_gray));
        let nav = icon_for(
            &entry(
                EntryKind::View,
                Some(Decoration {
                    glyph: "📋".into(),
                    color: DecorationColor::Default,
                }),
            ),
            &theme,
        );
        assert_eq!(nav.0, "📋");
    }

    #[test]
    fn the_footer_names_the_users_palette_chord() {
        let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        assert_eq!(close_chord(&keymaps), "⌃P");
    }
}
