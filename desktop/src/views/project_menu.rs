//! The project list the switcher's menu and the palette's Projects column
//! share (the Figma `Project switcher`, 2186:1692, and the Command palette's
//! Projects, 2171:825): a muted "Projects" heading, a 26pt row per project
//! (its dot, its name, a check on the current one), a divider, then
//! "+ Add project". No avatars.
//!
//! A row's dot comes from its [`ProjectIndicator`], which
//! `shelbi_app::command_model::project_rows` decides for both lists: a
//! filled green dot while the project is open (this window's, or open in
//! another), a hollow gray ring when it isn't. The current project (the
//! indicator's `Active`) wears the check.

use gpui::{div, prelude::*, px, Div, ElementId, Hsla, SharedString, Stateful, Svg};
use shelbi_app::overlay::palette::{ProjectEntry, ProjectIndicator};

use crate::assets::{heroicon, icons, Icon, ICON_SIZE_MENU};
use crate::theme::motion::fill_layer;
use crate::theme::style::{Type, Typography};
use crate::theme::Theme;

/// A row's height.
pub const ROW_H: f32 = 26.0;
/// A row's corners (Figma's `6`).
const RADIUS: f32 = 6.0;

/// The list's heading: "Projects", Base medium in `muted`, 8pt in, 4pt above
/// the rows.
pub fn heading(theme: &Theme) -> Div {
    div()
        .flex_none()
        .px_2()
        .pb_1()
        .type_style(Type::BaseMedium)
        .text_color(theme.muted)
        .child("Projects")
}

/// A project's dot: filled `icon_green` while it is open, a `gray` ring when
/// it isn't.
pub fn dot(indicator: ProjectIndicator, theme: &Theme) -> Svg {
    let (icon, color) = dot_drawing(indicator, theme);
    heroicon(icon).text_color(color)
}

/// [`dot`]'s drawing and its tint.
fn dot_drawing(indicator: ProjectIndicator, theme: &Theme) -> (Icon, Hsla) {
    match indicator {
        ProjectIndicator::Active | ProjectIndicator::LoadedIdle => (icons::CIRCLE_SOLID_MENU, theme.icon_green),
        ProjectIndicator::Unloaded => (icons::CIRCLE_MENU, theme.gray),
    }
}

/// A row: 26pt, 8pt in, its parts 4pt apart, Base in `foreground`. The
/// keyboard's row (`selected`) wears the overlay fill and the loud label;
/// the rest fade the fill in on hover. `key` names it for its debug selector
/// and its fade.
fn row(id: ElementId, key: SharedString, selected: bool, theme: &Theme) -> Stateful<Div> {
    let selector = key.clone();
    let d = div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap_1()
        .h(px(ROW_H))
        .px_2()
        .rounded(px(RADIUS))
        .cursor_pointer()
        .type_style(Type::Base)
        .text_color(theme.foreground);
    if selected {
        d.bg(theme.background).text_color(theme.text_loud)
    } else {
        let clear = theme.background.opacity(0.0);
        fill_layer(d, key, px(RADIUS), [clear, theme.background, theme.background])
    }
}

/// A project's row: its dot, its name (truncated), and the check on the
/// current project (the slot kept on the others, so the names line up).
pub fn project_row(
    id: impl Into<ElementId>,
    key: impl Into<SharedString>,
    project: &ProjectEntry,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let current = project.indicator == ProjectIndicator::Active;
    row(id.into(), key.into(), selected, theme)
        .child(dot(project.indicator, theme))
        .child(div().flex_1().min_w_0().truncate().child(project.label.clone()))
        .child(div().flex_none().size(ICON_SIZE_MENU).when(current, |d| {
            d.child(heroicon(icons::CHECK_SMALL_HEAVY_MENU).text_color(theme.gray))
        }))
}

/// The rule between the projects and "Add project": 1pt of `divider`, 8pt
/// clear all round.
pub fn divider(theme: &Theme) -> Div {
    div()
        .flex_none()
        .p_2()
        .child(div().h(px(1.0)).bg(theme.divider))
}

/// The trailing "+ Add project" row.
pub fn add_row(
    id: impl Into<ElementId>,
    key: impl Into<SharedString>,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    row(id.into(), key.into(), selected, theme)
        .child(heroicon(icons::PLUS_MENU).text_color(theme.gray))
        .child(div().flex_1().min_w_0().truncate().child("Add project"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::resolve(
            shelbi_state::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        )
    }

    #[test]
    fn an_open_project_is_a_green_dot_and_a_closed_one_a_gray_ring() {
        let theme = theme();
        let filled = (icons::CIRCLE_SOLID_MENU, theme.icon_green);
        assert_eq!(dot_drawing(ProjectIndicator::Active, &theme), filled, "this window's");
        assert_eq!(dot_drawing(ProjectIndicator::LoadedIdle, &theme), filled, "open elsewhere");
        assert_eq!(dot_drawing(ProjectIndicator::Unloaded, &theme), (icons::CIRCLE_MENU, theme.gray));
        let mut d = dot(ProjectIndicator::Unloaded, &theme);
        assert_eq!(d.style().size.width, Some(ICON_SIZE_MENU.into()), "the Figma's 16pt");
    }
}
