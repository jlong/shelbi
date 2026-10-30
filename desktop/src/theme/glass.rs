//! The roles of a translucent window ([`super::Theme::with_opacity`]): what
//! each becomes on the glass.
//!
//! The surfaces (`canvas`, `sidebar`) take the window opacity as their
//! alpha, and the command well, the canvas inside a divider, is the glass
//! itself. A fill that is opaque in the palette but sits on a surface (a
//! board card, a textbox's well, the workflow badge, the selected nav row,
//! a keycap) becomes a tint: the color and alpha that, laid over the
//! opaque surface it sits on, paint exactly its solid color ([`unblend`]),
//! with that alpha scaled by the window opacity. At 100% nothing changes;
//! below it the blurred desktop shows through the fill as it does through
//! the glass around it.
//!
//! The fills that are already tints (the button, chip, row, selection and
//! search fills, the `button/*/background` families) keep their values: laid
//! over the glass they let it through by themselves. So do text, icons,
//! borders, focus rings and glows, shadows and the scrim, and `raised`, the
//! surface menus, popovers and the palette draw on (an overlay paints from
//! [`super::Theme::opaque`] anyway).
//!
//! The tints are straight-alpha sRGB, blended the way gpui's renderer
//! blends (per channel, gamma-encoded), so "over the surface" is exactly
//! what the screen shows.

use gpui::{Hsla, Rgba};

use super::Palette;

/// What a role becomes on the glass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnGlass {
    /// A window surface: the window opacity is its alpha.
    Surface,
    /// The glass itself (the command well): no fill of its own.
    Clear,
    /// An opaque fill on the canvas: a tint over it.
    TintOverCanvas,
    /// An opaque fill on the sidebar: a tint over it.
    TintOverSidebar,
    /// An opaque fill on whatever fill it sits on (a keycap inside any
    /// button): itself at the window opacity.
    AtOpacity,
    /// Kept as it is: already a tint, or not a fill (text, icons, borders,
    /// shadows), or an overlay's surface.
    Kept,
}

/// What `role` becomes on the glass. Every role the palette declares has an
/// answer (`every_role_has_a_decision` lists them).
pub fn on_glass(role: &str) -> OnGlass {
    match role {
        "canvas" | "sidebar" => OnGlass::Surface,
        "well" => OnGlass::Clear,
        "card" | "textbox" | "workflow_badge" => OnGlass::TintOverCanvas,
        "nav_selected" => OnGlass::TintOverSidebar,
        "keycap" => OnGlass::AtOpacity,
        _ => OnGlass::Kept,
    }
}

/// `solid`'s roles in a window `opacity` (below 1) opaque.
pub fn glass(solid: &Palette<Hsla>, opacity: f32) -> Palette<Hsla> {
    let (canvas, sidebar) = (solid.canvas, solid.sidebar);
    solid.map_named(|role, color| match on_glass(role) {
        OnGlass::Surface => color.opacity(opacity),
        OnGlass::Clear => canvas.opacity(0.0),
        OnGlass::TintOverCanvas => scaled(unblend(color, canvas), opacity),
        OnGlass::TintOverSidebar => scaled(unblend(color, sidebar), opacity),
        OnGlass::AtOpacity => scaled(color, opacity),
        OnGlass::Kept => color,
    })
}

/// The tint, lightest first, that paints the opaque `fill` when laid over
/// the opaque `under`: per channel the alpha that reaches `fill` with the
/// tint's channel at 0 or 1, the largest of the three, then the channels
/// that alpha needs. A `fill` equal to `under` is fully clear.
pub fn unblend(fill: Hsla, under: Hsla) -> Hsla {
    let (f, u) = (fill.to_rgb(), under.to_rgb());
    let need = |f: f32, u: f32| {
        if f > u {
            (f - u) / (1.0 - u)
        } else if f < u {
            (u - f) / u
        } else {
            0.0
        }
    };
    let a = need(f.r, u.r).max(need(f.g, u.g)).max(need(f.b, u.b)).clamp(0.0, 1.0);
    if a <= f32::EPSILON {
        return fill.opacity(0.0);
    }
    let channel = |f: f32, u: f32| ((f - (1.0 - a) * u) / a).clamp(0.0, 1.0);
    Rgba {
        r: channel(f.r, u.r),
        g: channel(f.g, u.g),
        b: channel(f.b, u.b),
        a,
    }
    .into()
}

/// `color` with its alpha scaled by `opacity`.
fn scaled(color: Hsla, opacity: f32) -> Hsla {
    Hsla { a: color.a * opacity, ..color }
}

/// `color` laid over the opaque `under`, as the renderer blends: opaque.
pub fn over(color: Hsla, under: Hsla) -> Hsla {
    let (c, u) = (color.to_rgb(), under.to_rgb());
    let mix = |c: f32, u: f32| c * color.a + u * (1.0 - color.a);
    Rgba {
        r: mix(c.r, u.r),
        g: mix(c.g, u.g),
        b: mix(c.b, u.b),
        a: 1.0,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use gpui::WindowAppearance;
    use shelbi_app::theme::ThemePreference;

    /// A color as the 8-bit sRGB the screen gets.
    fn rgb8(c: Hsla) -> [u8; 3] {
        let c = c.to_rgb();
        [c.r, c.g, c.b].map(|v| (v * 255.0).round() as u8)
    }

    fn close(a: Hsla, b: Hsla) -> bool {
        rgb8(a).iter().zip(rgb8(b)).all(|(a, b)| a.abs_diff(b) <= 1)
    }

    fn themes() -> Vec<Theme> {
        let mut themes = Vec::new();
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            for contrast in [0, 50, 100] {
                themes.push(Theme::resolve_with(ThemePreference::Auto, contrast, system));
            }
        }
        themes
    }

    #[test]
    fn every_role_has_a_decision() {
        let solid = Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark);
        let changed: Vec<_> = solid.roles().into_iter().filter(|(name, _)| on_glass(name) != OnGlass::Kept).map(|(n, _)| n).collect();
        assert_eq!(
            changed,
            ["canvas", "sidebar", "card", "well", "textbox", "keycap", "workflow_badge", "nav_selected"],
            "the roles that change on the glass (docs/theme.md lists each decision)"
        );
    }

    #[test]
    fn a_tint_over_its_surface_paints_the_solid_color() {
        for solid in themes() {
            for (name, color) in solid.roles() {
                let under = match on_glass(name) {
                    OnGlass::TintOverCanvas => solid.canvas,
                    OnGlass::TintOverSidebar => solid.sidebar,
                    _ => continue,
                };
                let tint = unblend(color, under);
                assert!(close(over(tint, under), color), "{:?} {name}: {tint:?}", solid.appearance);
            }
        }
    }

    #[test]
    fn opaque_is_the_palette_and_glass_lets_every_fill_through() {
        for solid in themes() {
            assert_eq!(solid.with_opacity(1.0), solid, "100% is today's theme, role for role");
            for opacity in [0.99, 0.85, 0.3] {
                let glass = solid.with_opacity(opacity);
                for ((name, want), (_, got)) in solid.roles().into_iter().zip(glass.roles()) {
                    match on_glass(name) {
                        OnGlass::Kept => assert_eq!(got, want, "{name} keeps its color"),
                        OnGlass::Clear => assert_eq!(got.a, 0.0, "{name}"),
                        _ => {
                            assert!(got.a < 1.0, "{:?} {name} at {opacity}: {got:?}", solid.appearance);
                            assert!(got.a <= opacity + f32::EPSILON, "{name}: no thicker than the glass");
                        }
                    }
                }
                // Toward 100% each tint over its opaque surface nears the
                // solid color (no jump at the end of the slider).
                if opacity == 0.99 {
                    for (name, under) in [("card", solid.canvas), ("textbox", solid.canvas), ("nav_selected", solid.sidebar)] {
                        let got = glass.roles().into_iter().find(|(n, _)| *n == name).unwrap().1;
                        let want = solid.roles().into_iter().find(|(n, _)| *n == name).unwrap().1;
                        let painted = over(got, under);
                        let diff = rgb8(painted).iter().zip(rgb8(want)).map(|(a, b)| a.abs_diff(b)).max().unwrap();
                        assert!(diff <= 3, "{name}: {diff}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_nested_fill_paints_its_solid_color_over_its_tinted_parent() {
        for solid in themes() {
            for opacity in [0.85, 0.3] {
                let glass = solid.with_opacity(opacity);
                // The board's workflow badge on a card on the canvas.
                let badge = glass.fill_on(glass.workflow_badge, glass.card);
                // Undo the opacity scaling: at the slider's end the stack
                // paints what the opaque stack does.
                let unscale = |c: Hsla| Hsla { a: c.a / opacity, ..c };
                let card = over(unscale(glass.card), solid.canvas);
                let painted = over(unscale(badge), card);
                assert!(close(card, solid.card), "{:?} card", solid.appearance);
                assert!(close(painted, solid.workflow_badge), "{:?} badge: {painted:?}", solid.appearance);
                assert!(badge.a < 1.0);
            }
            assert_eq!(solid.fill_on(solid.workflow_badge, solid.card), solid.workflow_badge, "opaque: the role");
        }
    }
}
