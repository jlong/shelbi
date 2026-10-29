//! The contrast setting (`desktop.contrast`): how far apart the window's
//! surfaces and what is drawn on them sit, 0 to 100%.
//!
//! Like the window opacity ([`super::window`]), it is a resolve-time change
//! to the palette's roles, never an overlay: [`Palette::with_contrast`]
//! moves each role by what it is for ([`Class`]), in proportion to the
//! setting, so 0 is the palette exactly and 100 the strongest step.
//!
//! In the dark theme the surfaces go darker and the text, icons and borders
//! brighter; in the light theme the surfaces go lighter and the text, icons
//! and borders darker. Either way a role only ever moves away from the
//! surfaces it sits on, so no pairing loses contrast as the setting rises
//! (the tests hold every pairing the desktop draws to that):
//!
//! - [`Class::Surface`] (canvas, sidebar, cards, the raised surface, the
//!   textbox well, keycaps, the workflow badge) mixes toward black in Dark
//!   (35% at 100) and toward white in Light (50% at 100). White surfaces
//!   are already as light as they go, so in Light the separation comes
//!   mostly from the text.
//! - [`Class::Text`] (body, loud, muted and form text, the log's grays,
//!   icons) mixes toward white in Dark and black in Light, and its alpha
//!   toward opaque, both by half at 100: `text/muted` at 55% in Dark reads
//!   at 78%.
//! - [`Class::Border`] (dividers, a field's and a box's edge, the raised
//!   ring) mixes toward white in Dark and black in Light by half, and its
//!   alpha doubles at 100: a 10% divider draws at 20%.
//! - [`Class::Hue`] (links, the accent, status icons and text, the log's
//!   hues, focus rings, the buttons' colored labels) mixes toward white in
//!   Dark and black in Light by a fifth at 100: mixing with white or black
//!   keeps a color's hue, so cyan stays cyan, a step brighter on the darker
//!   surfaces (or a step deeper on the light ones).
//! - [`Class::Kept`]: everything translucent laid over a surface (rows,
//!   chips, the button fills and their tints, the selection, glows,
//!   shadows, the scrim) keeps its value, so over a darker surface it
//!   composites darker by itself and its rest / hover / pressed steps stay
//!   as far apart; a checked box keeps its fill so its white mark doesn't
//!   lose contrast.
//!
//! The terminals don't take it: their palette ([`super::TerminalPalette`])
//! is the standard palette's whatever the setting, so a session's colors are
//! the same at every contrast.

use std::ops::RangeInclusive;

use shelbi_app::theme::{Appearance, Rgb};
use shelbi_state::DesktopConfig;

use super::palette::{Palette, Rgba};

/// The setting's range, in percent: 0 is the palette as designed.
pub const CONTRAST_RANGE: RangeInclusive<u8> = 0..=100;

/// The setting in `cfg`, clamped (unset is 0).
pub fn from_config(cfg: &DesktopConfig) -> u8 {
    clamped(cfg.contrast.map_or(0, i32::from))
}

/// `contrast` clamped into [`CONTRAST_RANGE`].
pub fn clamped(contrast: i32) -> u8 {
    contrast.clamp(*CONTRAST_RANGE.start() as i32, *CONTRAST_RANGE.end() as i32) as u8
}

/// Write `contrast` into `cfg`; 0 isn't written.
pub fn apply(contrast: u8, cfg: &mut DesktopConfig) {
    cfg.contrast = (contrast != 0).then_some(contrast);
}

/// What a role is for, which decides how contrast moves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// An opaque surface: away from the text (darker in Dark).
    Surface,
    /// Text and icons: toward the extreme, and more opaque.
    Text,
    /// Rules and edges: toward the extreme, and more opaque.
    Border,
    /// A colored foreground: toward the extreme, hue kept.
    Hue,
    /// A translucent fill over a surface, or a color that must not move.
    Kept,
}

/// How far each class moves at 100%.
const SURFACE_DARK: f32 = 0.35;
const SURFACE_LIGHT: f32 = 0.5;
const TEXT: f32 = 0.5;
const BORDER: f32 = 0.5;
/// A border's alpha at 100% is this many times its own.
const BORDER_ALPHA: f32 = 2.0;
const HUE: f32 = 0.2;

/// Every role's class, by name. `None` for a name that isn't a role (the
/// tests hold every role to an arm here, so a new role must pick one).
pub fn class(role: &str) -> Option<Class> {
    use Class::*;
    Some(match role {
        "canvas" | "sidebar" | "card" | "raised" | "well" | "textbox" | "keycap" | "nav_selected"
        | "workflow_badge" => Surface,
        "text_loud" | "foreground" | "gray" | "form_text" | "form_hint" | "muted" | "log_text" | "table_text"
        | "icon" | "button_icon" | "dark_gray" | "workflow_badge_text" | "link_hover" => Text,
        "divider" | "divider_strong" | "input_border" | "check_border" | "ring" => Border,
        "link" | "accent" | "icon_cyan" | "icon_green" | "icon_yellow" | "icon_purple" | "progress"
        | "focus_ring" | "success_focus_ring" | "danger_focus_ring" | "select_label" | "primary_label"
        | "success_label" | "success" | "warning" | "info" | "error" | "theme_green" | "theme_cyan"
        | "theme_red" | "green" | "yellow" | "blue" | "magenta" | "cyan" | "red" => Hue,
        "toolbar" | "background" | "background_hover" | "background_active" | "select_fill"
        | "select_fill_hover" | "select_fill_active" | "selection" | "search" | "button_keycap_edge"
        | "text_on_color" | "focus_glow" | "success_focus_glow" | "danger_focus_glow" | "check_fill"
        | "check_glow" | "green_tint" | "yellow_tint" | "cyan_tint" | "blue_tint" | "magenta_tint"
        | "red_tint" | "primary_fill" | "primary_fill_hover" | "primary_fill_active" | "success_fill"
        | "success_fill_hover" | "success_fill_active" | "danger_fill" | "danger_fill_hover"
        | "danger_fill_active" | "zen" | "notice" | "row_selected" | "text_selection" | "search_match"
        | "search_current" | "shadow" | "shadow_ring" | "palette_shadow" | "scrim" => Kept,
        _ => return None,
    })
}

impl Palette {
    /// This palette at `contrast` percent (clamped to 100): each role moved
    /// by its [`Class`], in proportion. 0 returns the palette unchanged.
    pub fn with_contrast(&self, contrast: u8) -> Palette {
        let k = f32::from(contrast.min(*CONTRAST_RANGE.end())) / 100.0;
        if k == 0.0 {
            return *self;
        }
        let dark = self.appearance == Appearance::Dark;
        // Text's extreme, and the surfaces' (the other way).
        let (ink, ground) = if dark {
            (Rgb::new(255, 255, 255), Rgb::new(0, 0, 0))
        } else {
            (Rgb::new(0, 0, 0), Rgb::new(255, 255, 255))
        };
        let surface = if dark { SURFACE_DARK } else { SURFACE_LIGHT };
        let alpha_toward_opaque = |a: u8, t: f32| {
            let a = f32::from(a);
            (a + (255.0 - a) * t).round().min(255.0) as u8
        };
        self.map_named(|role, c| match class(role) {
            Some(Class::Surface) => Rgba {
                rgb: c.rgb.lerp(ground, surface * k),
                a: c.a,
            },
            Some(Class::Text) => Rgba {
                rgb: c.rgb.lerp(ink, TEXT * k),
                a: alpha_toward_opaque(c.a, TEXT * k),
            },
            Some(Class::Border) => Rgba {
                rgb: c.rgb.lerp(ink, BORDER * k),
                a: (f32::from(c.a) * (1.0 + (BORDER_ALPHA - 1.0) * k)).round().min(255.0) as u8,
            },
            Some(Class::Hue) => Rgba {
                rgb: c.rgb.lerp(ink, HUE * k),
                a: c.a,
            },
            Some(Class::Kept) | None => c,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::palette::tests::pairings;

    fn palettes() -> [Palette; 2] {
        [Palette::dark(), Palette::light()]
    }

    #[test]
    fn every_role_has_a_class() {
        let unclassified: Vec<_> = Palette::dark()
            .roles()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| class(name).is_none())
            .collect();
        assert!(unclassified.is_empty(), "roles with no contrast class: {unclassified:?}");
    }

    #[test]
    fn zero_is_the_palette_exactly() {
        for p in palettes() {
            assert_eq!(p.with_contrast(0), p);
            assert_eq!(p.with_contrast(255), p.with_contrast(100), "clamped");
        }
    }

    /// The one pairing that dips on the way up: the log's dark gray (a
    /// timestamp) on the selected Machines row's band. It is the only text
    /// drawn darker than its band in Dark (1.21:1 at 0, a listed
    /// exception), so as the darker canvas pulls the band down and the gray
    /// brightens, the two cross (about 1.0 near 15%) before the gray clears
    /// the band; at 100 it reads better than at 0.
    const DIPS: &[(Appearance, &str)] = &[(Appearance::Dark, "dark_gray on selection over canvas")];

    /// Every pairing the desktop draws (text, icons, accents and status
    /// colors on every surface, fill and chip they sit on) holds or gains
    /// contrast at each step up the slider, in both themes, but for
    /// [`DIPS`], which ends above where it started.
    #[test]
    fn no_pairing_loses_contrast_as_the_setting_rises() {
        let mut lost = Vec::new();
        for p in palettes() {
            let (start, end) = (pairings(&p), pairings(&p.with_contrast(100)));
            for ((what, was, _), (_, now, _)) in start.iter().zip(&end) {
                if DIPS.contains(&(p.appearance, what.as_str())) {
                    assert!(now > was, "{what}: {was:.3} at 0, {now:.3} at 100");
                }
            }
            let mut before = start;
            for step in (5..=100).step_by(5) {
                let after = pairings(&p.with_contrast(step));
                for ((what, was, _), (_, now, _)) in before.iter().zip(&after) {
                    // A byte of rounding is not a loss.
                    if now + 0.01 < *was && !DIPS.contains(&(p.appearance, what.as_str())) {
                        lost.push(format!("{:?} {what} at {step}%: {was:.3} -> {now:.3}", p.appearance));
                    }
                }
                before = after;
            }
        }
        assert!(lost.is_empty(), "{}", lost.join("\n"));
    }

    /// Body and muted text, icons and the accent and status hues gain
    /// clearly at 100, and the hues stay readable: as legible as at 0 on
    /// every surface.
    #[test]
    fn text_and_hues_gain_at_full_contrast() {
        for p in palettes() {
            let most = p.with_contrast(100);
            let ratio = |fg: Rgba, bg: Rgba| fg.over(bg.rgb).contrast_ratio(bg.rgb);
            for (name, f) in [
                ("foreground", (|p: &Palette| p.foreground) as fn(&Palette) -> Rgba),
                ("muted", |p| p.muted),
                ("icon", |p| p.icon),
                ("form_hint", |p| p.form_hint),
            ] {
                for (sname, s) in [("canvas", (|p: &Palette| p.canvas) as fn(&Palette) -> Rgba), ("sidebar", |p| p.sidebar)] {
                    let (was, now) = (ratio(f(&p), s(&p)), ratio(f(&most), s(&most)));
                    assert!(now >= was * 1.1, "{:?} {name} on {sname}: {was:.2} -> {now:.2}", p.appearance);
                }
            }
            for (name, was, now) in [
                ("accent", p.accent, most.accent),
                ("link", p.link, most.link),
                ("success", p.success, most.success),
                ("warning", p.warning, most.warning),
                ("error", p.error, most.error),
                ("info", p.info, most.info),
            ] {
                let gain = ratio(now, most.canvas) / ratio(was, p.canvas);
                assert!(gain > 1.0, "{:?} {name}: x{gain:.2}", p.appearance);
            }
        }
    }

    #[test]
    fn dark_darkens_the_surfaces_and_brightens_the_rest_light_the_reverse() {
        let lum = |c: Rgba| c.rgb.relative_luminance();
        let (dark, light) = (Palette::dark(), Palette::light());
        let (d, l) = (dark.with_contrast(50), light.with_contrast(100));
        for (name, was, now) in [("canvas", dark.canvas, d.canvas), ("sidebar", dark.sidebar, d.sidebar)] {
            assert!(lum(now) < lum(was), "dark {name} darker");
        }
        assert!(lum(d.text_loud) > lum(dark.text_loud) && d.foreground.a > dark.foreground.a);
        assert!(lum(d.icon) > lum(dark.icon), "dark icons brighter");
        assert!(d.divider.a > dark.divider.a && lum(d.divider) >= lum(dark.divider), "dark rules brighter");
        assert!(lum(l.sidebar) > lum(light.sidebar), "light sidebar lighter");
        assert!(lum(l.foreground) <= lum(light.foreground) && l.foreground.a > light.foreground.a);
        assert!(lum(l.muted) < lum(light.muted), "light text darker");
        // The fills laid over a surface keep their value.
        assert_eq!((d.background, d.selection, d.check_fill), (dark.background, dark.selection, dark.check_fill));
    }

    #[test]
    fn hues_keep_their_hue() {
        fn hue(c: Rgb) -> f32 {
            let (r, g, b) = (f32::from(c.r), f32::from(c.g), f32::from(c.b));
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            let d = max - min;
            if d == 0.0 {
                return 0.0;
            }
            let h = if max == r {
                ((g - b) / d).rem_euclid(6.0)
            } else if max == g {
                (b - r) / d + 2.0
            } else {
                (r - g) / d + 4.0
            };
            h * 60.0
        }
        for p in palettes() {
            let most = p.with_contrast(100);
            for ((name, was), (_, now)) in p.roles().into_iter().zip(most.roles()) {
                if class(name) == Some(Class::Hue) {
                    let diff = (hue(was.rgb) - hue(now.rgb)).abs();
                    let diff = diff.min(360.0 - diff);
                    assert!(diff < 4.0, "{:?} {name}: {diff:.1}°", p.appearance);
                }
            }
        }
    }

    #[test]
    fn the_setting_round_trips_and_zero_is_not_written() {
        let mut cfg = DesktopConfig::default();
        assert_eq!(from_config(&cfg), 0);
        apply(40, &mut cfg);
        assert_eq!((cfg.contrast, from_config(&cfg)), (Some(40), 40));
        apply(0, &mut cfg);
        assert!(cfg.is_empty());
        cfg.contrast = Some(250);
        assert_eq!(from_config(&cfg), 100, "clamped");
    }
}
