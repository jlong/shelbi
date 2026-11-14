//! The project list the switcher's menu and the palette's Projects column
//! share (the Figma `Project switcher`, 2186:1692, and the Command palette's
//! Projects, 2171:825): a muted "Projects" heading, a 26pt row per project
//! (its dot, its name, a check on the current one), a divider, then
//! "+ Add project". No avatars.
//!
//! A row's dot comes from its [`ProjectIndicator`], which
//! `shelbi_app::command_model::project_rows` decides for both lists (and the
//! tabs use too): a green disc breathing while the project has work in
//! progress (one of its workspaces' agents is working), a solid green disc
//! while it is loaded with nothing in progress, a hollow gray ring when it
//! isn't loaded. The dot never marks the current project: that one wears
//! the check ([`ProjectEntry::current`]).

use gpui::{div, prelude::*, px, Animation, AnimationExt, AnyElement, Div, ElementId, Hsla, SharedString, Stateful};
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

/// A project's dot (16pt, `flex_none`): the `circle-solid` disc in
/// `icon_green` while it is loaded, a `gray` `circle` ring when it isn't.
/// While the project has work in progress the disc's color breathes
/// ([`pulse_color`]); `id` keys that animation. Under Reduce Motion (and in
/// the visual checks, which render with it) gpui holds a repeating
/// animation at its start, so the disc holds at the pulse's peak.
pub fn dot(id: impl Into<ElementId>, indicator: ProjectIndicator, theme: &Theme) -> AnyElement {
    let (icon, color) = dot_drawing(indicator, theme);
    let disc = heroicon(icon).flex_none().text_color(color);
    if !indicator.pulses() {
        return disc.into_any_element();
    }
    let theme = *theme;
    disc.with_animation(
        id,
        Animation::new(shelbi_app::theme::PROJECT_PULSE_PERIOD).repeat(),
        move |d, t| d.text_color(pulse_color(&theme, t)),
    )
    .into_any_element()
}

/// The working dot's color `t` (0..1) of the way through its animation.
/// The breath starts at its peak, so the frame gpui holds under Reduce
/// Motion (`t` 0) is the brightest, never the trough.
fn pulse_color(theme: &Theme, t: f32) -> Hsla {
    theme.project_pulse(t + 0.5)
}

/// [`dot`]'s drawing and its resting tint.
fn dot_drawing(indicator: ProjectIndicator, theme: &Theme) -> (Icon, Hsla) {
    match indicator {
        ProjectIndicator::Working | ProjectIndicator::LoadedIdle => (icons::CIRCLE_SOLID_MENU, theme.icon_green),
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
    let key: SharedString = key.into();
    let current = project.current;
    row(id.into(), key.clone(), selected, theme)
        .child(dot(SharedString::from(format!("{key}-dot")), project.indicator, theme))
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
    fn a_loaded_project_is_a_green_disc_and_one_not_loaded_a_gray_ring() {
        let theme = theme();
        let filled = (icons::CIRCLE_SOLID_MENU, theme.icon_green);
        assert_eq!(dot_drawing(ProjectIndicator::Working, &theme), filled, "at work: the disc, pulsing");
        assert_eq!(dot_drawing(ProjectIndicator::LoadedIdle, &theme), filled, "idle: the disc, solid");
        assert_eq!(dot_drawing(ProjectIndicator::Unloaded, &theme), (icons::CIRCLE_MENU, theme.gray));
        let mut d = heroicon(dot_drawing(ProjectIndicator::Unloaded, &theme).0);
        assert_eq!(d.style().size.width, Some(ICON_SIZE_MENU.into()), "the Figma's 16pt");
    }

    /// The working dot breathes between the shared pulse tokens, and the
    /// frame Reduce Motion holds (the animation's start) is its peak.
    #[test]
    fn the_working_dot_breathes_from_its_peak() {
        for theme in [
            theme(),
            Theme::resolve(shelbi_state::ThemePreference::Light, gpui::WindowAppearance::Light),
        ] {
            let tokens = if theme.is_dark() {
                shelbi_app::theme::Theme::dark()
            } else {
                shelbi_app::theme::Theme::light()
            };
            let hsla = |c: shelbi_app::theme::Rgb| -> Hsla { gpui::rgb(u32::from_be_bytes([0, c.r, c.g, c.b])).into() };
            assert_eq!(pulse_color(&theme, 0.0), hsla(tokens.pulse_peak.rgb), "held at the peak");
            assert_eq!(pulse_color(&theme, 0.5), hsla(tokens.pulse_trough.rgb), "the trough mid-breath");
            assert_eq!(pulse_color(&theme, 1.0), pulse_color(&theme, 0.0), "one breath per period");
        }
    }
}
