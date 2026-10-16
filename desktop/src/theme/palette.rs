//! The desktop's own palette: Taskpage's hues and semantic roles.
//!
//! The TUI keeps the shared tokens ([`shelbi_app::theme`]); the desktop paints
//! from these roles instead, after the Taskpage design system (32pixels'
//! `tailwind.config.js`). Taskpage is light only, so [`Palette::light`] is
//! Taskpage as is and [`Palette::dark`] is derived: neutral surfaces from the
//! black scale, near-white text, and every hue moved to a lighter step so it
//! keeps its contrast. `desktop/docs/theme.md` lists every role with both
//! values; the tests check WCAG AA for every text role on every surface it
//! is drawn on.
//!
//! A role names what a color is for, not its hue: views ask for
//! `surface_recessed` or `text_quiet`, never for slate-100.

use shelbi_app::theme::{Appearance, Rgb};

/// Taskpage's hue scales (`tailwind.config.js`), 100 (lightest) to 700.
pub mod hues {
    use super::Rgba;

    pub const WHITE: Rgba = Rgba::hex(0xffffff);

    /// The neutral scale Taskpage calls `black`.
    pub const BLACK: [Rgba; 7] = scale([
        0xeaebec, 0xd5d6da, 0xabadb5, 0x777a86, 0x585c6a, 0x2e3345, 0x131418,
    ]);
    pub const SLATE: [Rgba; 7] = scale([
        0xf8f8fa, 0xf2f3f7, 0xeceef3, 0xe3e5ed, 0xacb4cf, 0x7b85a7, 0x444c65,
    ]);
    pub const GREEN: [Rgba; 7] = scale([
        0xeffaef, 0xe2f6e0, 0xbbebb7, 0x86d580, 0x5ac351, 0x4aaa42, 0x00753d,
    ]);
    pub const GOLD: [Rgba; 7] = scale([
        0xfff0bc, 0xfde89e, 0xffdd64, 0xffca0d, 0xe0b109, 0xba9101, 0x856700,
    ]);
    pub const CYAN: [Rgba; 7] = scale([
        0xe0f5fd, 0xd0f0fc, 0xb0e6fb, 0x62cdf6, 0x31ade9, 0x1989c4, 0x004973,
    ]);
    pub const ORANGE: [Rgba; 7] = scale([
        0xffddbd, 0xffc896, 0xffaf65, 0xff8f27, 0xf96a03, 0xc35619, 0x9d4100,
    ]);
    pub const BLUE: [Rgba; 7] = scale([
        0xdce7fe, 0xcadbfe, 0xb9cffd, 0x8fb4ff, 0x1757fd, 0x2830f1, 0x0007a7,
    ]);
    pub const PINK: [Rgba; 7] = scale([
        0xfddceb, 0xfcc4dd, 0xff9dc9, 0xff66ab, 0xf21478, 0xdb0a60, 0xb40041,
    ]);
    pub const PURPLE: [Rgba; 7] = scale([
        0xf5e7f9, 0xf3d3fc, 0xf2beff, 0xe99bfc, 0xe374ff, 0xb247e7, 0x7019c0,
    ]);
    pub const RED: [Rgba; 7] = scale([
        0xffd9d9, 0xffbebe, 0xff8484, 0xff3737, 0xdb0000, 0xaa0101, 0x6c0000,
    ]);

    /// Step `n` (100..=700) of a scale.
    pub const fn step(scale: &[Rgba; 7], n: usize) -> Rgba {
        scale[n / 100 - 1]
    }

    const fn scale(hex: [u32; 7]) -> [Rgba; 7] {
        let mut out = [WHITE; 7];
        let mut i = 0;
        while i < 7 {
            out[i] = Rgba::hex(hex[i]);
            i += 1;
        }
        out
    }
}

/// An sRGB color with straight alpha: how a role stores its value before a
/// client turns it into a toolkit color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub rgb: Rgb,
    /// 0 (transparent) to 255 (opaque).
    pub a: u8,
}

impl Rgba {
    /// An opaque `0xRRGGBB`.
    pub const fn hex(value: u32) -> Self {
        Self {
            rgb: Rgb::hex(value),
            a: 0xff,
        }
    }

    /// This color at `percent` opacity (Tailwind's `black-600/10`).
    pub const fn alpha(self, percent: u8) -> Self {
        Self {
            rgb: self.rgb,
            a: ((percent as u16 * 255 + 50) / 100) as u8,
        }
    }

    /// `#rrggbb`, or `#rrggbbaa` when translucent (how `docs/theme.md` lists
    /// it).
    #[cfg(test)]
    pub fn css(self) -> String {
        let Rgb { r, g, b } = self.rgb;
        if self.a == 0xff {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{:02x}", self.a)
        }
    }

    /// The opaque color this one paints over `under`.
    pub fn over(self, under: Rgb) -> Rgb {
        under.lerp(self.rgb, f32::from(self.a) / 255.0)
    }
}

/// Declares [`Palette`]: one field per role, plus the exhaustive `map` and
/// the `roles` listing the tests and the docs walk.
macro_rules! palette {
    ($( $(#[$doc:meta])+ $role:ident, )+) => {
        /// Every role the desktop paints with, one field each. `C` is the
        /// color type: [`Rgba`] here, `gpui::Hsla` after [`Palette::map`].
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct Palette<C = Rgba> {
            /// Which way this palette runs. Not a color.
            pub appearance: Appearance,
            $( $(#[$doc])+ pub $role: C, )+
        }

        impl<C: Copy> Palette<C> {
            /// Convert every role with `f`. Exhaustive by construction.
            pub fn map<D>(&self, mut f: impl FnMut(C) -> D) -> Palette<D> {
                Palette {
                    appearance: self.appearance,
                    $( $role: f(self.$role), )+
                }
            }

            /// Every role and its value, in declaration order.
            #[cfg(test)]
            pub fn roles(&self) -> Vec<(&'static str, C)> {
                vec![ $( (stringify!($role), self.$role), )+ ]
            }
        }
    };
}

palette! {
    // ---- Surfaces ---------------------------------------------------------
    /// The main area: the window's content surface and the terminal's
    /// background. Taskpage's `background-depth-0`.
    surface,
    /// The leading column (sidebar, review and workspace panels), the board's
    /// columns: one step recessed from [`Palette::surface`].
    surface_recessed,
    /// A well inside a surface: text fields, inline code, the empty slot of a
    /// drag.
    surface_sunken,
    /// A surface lifted off the one under it: cards, menus, popovers, the
    /// palette, dialogs.
    surface_raised,

    // ---- Fills (hover, pressed, quiet buttons) ----------------------------
    /// A row's hover, a menu row's hover, the search box.
    fill_subtle,
    /// The secondary button, an active icon button, a neutral chip.
    fill,
    /// An icon button's hover, the secondary button's hover.
    fill_strong,

    // ---- Controls (buttons, dropdowns, segmented controls, the search box) --
    /// A control's resting fill: the secondary button, a dropdown, the search
    /// box, a segmented control's track. Taskpage's slate-300.
    control_fill,
    /// A control's hover fill (slate-400).
    control_fill_hover,
    /// A control while pressed or open (a dropdown with its menu showing).
    control_fill_pressed,
    /// A control's 1px boundary. Invisible in light (Taskpage's controls are
    /// fill only, and their labels identify them); in dark it is what makes
    /// the control visible, so it clears 3:1 on every surface.
    control_border,
    /// The boundary on hover and while pressed.
    control_border_hover,
    /// A text field's 1px boundary: 3:1 in both themes, since a field has no
    /// label inside it to identify it.
    input_border,
    /// The selected tab or segment: a lifted surface over the track.
    tab_selected,
    /// The selected segment's 1px ring.
    tab_selected_ring,
    /// A keycap in a shortcut hint (Taskpage's `Shortcut`).
    keycap,
    /// The keycap's 1px bottom edge.
    keycap_edge,

    // ---- Text ---------------------------------------------------------------
    /// Headings, titles, the selected row's label.
    text_loud,
    /// Body text and labels.
    text,
    /// Secondary text: descriptions, timestamps, section labels, hints.
    text_quiet,
    /// Placeholders, disabled labels and decorative separators. Exempt from
    /// the contrast rule (WCAG excludes disabled and decorative text).
    text_faint,
    /// Text on a saturated fill (the brand button, the Zen band).
    text_on_color,

    // ---- Icons --------------------------------------------------------------
    /// Icons at rest.
    icon,
    /// De-emphasized icons (chevrons, hints).
    icon_secondary,

    // ---- Rules --------------------------------------------------------------
    /// Dividers and hairline borders.
    rule,
    /// A stronger rule: a dragged divider, an input's border.
    rule_strong,

    // ---- Identity -----------------------------------------------------------
    /// The brand fill (pink-600): the primary button.
    brand,
    /// The primary button's hover.
    brand_hover,
    /// The primary button's boundary on hover: the hover fill in light; in
    /// dark the lighter pink, since the darker hover fill alone falls under
    /// 3:1 on the dark surfaces.
    brand_hover_border,
    /// The brand as a foreground on a surface: the current-selection
    /// indicator.
    brand_text,
    /// Taskpage's accent (blue).
    accent,
    /// A link.
    link,
    /// A hovered link.
    link_hover,
    /// The keyboard-focus ring.
    focus_ring,
    /// The project title in the sidebar (Shelbi's cyan wordmark, moved onto
    /// Taskpage's cyan).
    wordmark,

    // ---- Meaning ------------------------------------------------------------
    /// Success, done, approve.
    success,
    /// Warnings, in progress, awaiting input.
    warning,
    /// Ready, informational.
    info,
    /// Errors, blocked, reject.
    error,

    // ---- Hues: text-safe foreground, and the chip tint behind it -----------
    /// Green as text or a glyph: a working workspace, Done.
    green,
    /// A green chip's background.
    green_tint,
    /// Gold as text or a glyph: Active, warnings.
    gold,
    /// A gold chip's background.
    gold_tint,
    /// Cyan as text or a glyph: ready for review.
    cyan,
    /// A cyan chip's background.
    cyan_tint,
    /// Blue as text or a glyph: the Ready category, queued work.
    blue,
    /// A blue chip's background.
    blue_tint,
    /// Pink as text (not the brand fill).
    pink,
    /// A pink chip's background.
    pink_tint,
    /// Purple as text or a glyph: Handoff, a paused workspace.
    purple,
    /// A purple chip's background.
    purple_tint,
    /// Red as text or a glyph.
    red,
    /// A red chip's background.
    red_tint,
    /// Orange as text or a glyph: failures in the activity feed.
    orange,
    /// A orange chip's background.
    orange_tint,
    /// A neutral chip's background (workflow names, machine tags).
    neutral_tint,

    // ---- Bands and solid fills ----------------------------------------------
    /// The `ZEN MODE ON` band (with [`Palette::text_on_color`]).
    zen,
    /// The unread-errors button and other solid danger fills.
    error_fill,
    /// A danger button's boundary (3:1 on every surface it sits on).
    danger_border,
    /// A soft warning band (a status or a notice over the main area).
    notice,

    // ---- Selection ----------------------------------------------------------
    /// The selected nav row in the leading column: Taskpage's raised white
    /// chip (light), a lifted surface (dark).
    nav_selected,
    /// The 1px ring around [`Palette::nav_selected`].
    nav_selected_ring,
    /// The nav chip's 2px drop (`shadow-nav`).
    nav_shadow,
    /// A selected or keyboard-current row on a raised or main surface (the
    /// palette, a list, a board card).
    row_selected,
    /// Selected text (the terminal's selection highlight).
    text_selection,
    /// A search match's highlight behind text.
    search_match,
    /// The current search match's highlight.
    search_current,

    // ---- Elevation ----------------------------------------------------------
    /// The ring around a raised surface (menus, popovers, cards).
    ring,
    /// A raised surface's shadow (`shadow-lg`).
    shadow,
    /// The backdrop behind a modal overlay.
    scrim,
}

impl Palette {
    /// Taskpage as is.
    pub fn light() -> Self {
        use hues::*;
        let ink = step(&BLACK, 600);
        Self {
            appearance: Appearance::Light,
            surface: WHITE,
            surface_recessed: step(&SLATE, 100),
            surface_sunken: step(&SLATE, 200),
            surface_raised: WHITE,

            fill_subtle: step(&SLATE, 200),
            fill: step(&SLATE, 300),
            fill_strong: step(&SLATE, 400),

            control_fill: step(&SLATE, 300),
            control_fill_hover: step(&SLATE, 400),
            control_fill_pressed: step(&SLATE, 500).alpha(45),
            // Fill only, as in Taskpage: the boundary takes the fill's color.
            control_border: step(&SLATE, 300),
            control_border_hover: step(&SLATE, 400),
            input_border: step(&SLATE, 600),
            tab_selected: WHITE,
            tab_selected_ring: ink.alpha(8),
            keycap: WHITE,
            keycap_edge: step(&SLATE, 600).alpha(30),

            text_loud: ink,
            text: ink,
            // Taskpage's `quiet` is black-600 at 65%, which lands at 4.3:1 on
            // white; 72% clears AA on every light surface.
            text_quiet: ink.alpha(72),
            text_faint: ink.alpha(40),
            text_on_color: WHITE,

            icon: step(&SLATE, 600),
            icon_secondary: Rgba::hex(0xc8cdde),

            rule: ink.alpha(10),
            rule_strong: ink.alpha(15),

            brand: step(&PINK, 600),
            // Taskpage hovers at brand/80, which drops white text to 4:1; the
            // next step down keeps it AA.
            brand_hover: step(&PINK, 700),
            brand_hover_border: step(&PINK, 700),
            brand_text: step(&PINK, 600),
            accent: step(&BLUE, 700),
            link: step(&BLUE, 600),
            link_hover: step(&PINK, 600),
            focus_ring: step(&PINK, 600),
            wordmark: CYAN_650,

            success: step(&GREEN, 700),
            warning: step(&GOLD, 700),
            info: CYAN_650,
            error: step(&RED, 600),

            green: step(&GREEN, 700),
            green_tint: step(&GREEN, 200),
            gold: step(&GOLD, 700),
            gold_tint: step(&GOLD, 100),
            cyan: CYAN_650,
            cyan_tint: step(&CYAN, 100),
            blue: step(&BLUE, 600),
            blue_tint: step(&BLUE, 100),
            pink: step(&PINK, 700),
            pink_tint: step(&PINK, 100),
            purple: step(&PURPLE, 700),
            purple_tint: step(&PURPLE, 100),
            red: step(&RED, 600),
            red_tint: step(&RED, 100),
            orange: step(&ORANGE, 700),
            orange_tint: step(&ORANGE, 100),
            neutral_tint: step(&SLATE, 300),

            zen: step(&GREEN, 700),
            error_fill: step(&RED, 500),
            danger_border: step(&RED, 500),
            notice: step(&GOLD, 100),

            nav_selected: WHITE,
            nav_selected_ring: Rgba::hex(0x000000).alpha(3),
            nav_shadow: Rgba::hex(0x141947).alpha(5),
            row_selected: step(&SLATE, 300),
            text_selection: step(&BLUE, 200),
            search_match: step(&GOLD, 200),
            search_current: step(&GOLD, 400),

            ring: ink.alpha(5),
            shadow: Rgba::hex(0x000000).alpha(10),
            // Taskpage's `bg-slate-700/40 backdrop-blur-sm`; gpui has no
            // backdrop blur, so the scrim is a little stronger instead.
            scrim: step(&SLATE, 700).alpha(50),
        }
    }

    /// The derived dark theme: the same family on the black scale.
    pub fn dark() -> Self {
        use hues::*;
        let ink = step(&SLATE, 400);
        let tint = |scale: &[Rgba; 7]| step(scale, 500).alpha(18);
        Self {
            appearance: Appearance::Dark,
            // Lifted steps between black-700 (#131418) and black-600
            // (#2e3345); the leading column sits a step darker than the main
            // area, as slate-100 sits under white in the light theme.
            surface: DARK_SURFACE,
            surface_recessed: step(&BLACK, 700),
            surface_sunken: Rgba::hex(0x0f1014),
            surface_raised: DARK_RAISED,

            fill_subtle: ink.alpha(5),
            fill: ink.alpha(8),
            fill_strong: ink.alpha(12),

            // A lighter fill than the hover fills, and a boundary that clears
            // 3:1 on every dark surface: a control reads as a control.
            control_fill: ink.alpha(10),
            control_fill_hover: ink.alpha(16),
            control_fill_pressed: ink.alpha(22),
            control_border: ink.alpha(42),
            control_border_hover: ink.alpha(58),
            input_border: ink.alpha(42),
            tab_selected: DARK_TAB,
            tab_selected_ring: ink.alpha(50),
            keycap: ink.alpha(14),
            keycap_edge: Rgba::hex(0x000000).alpha(60),

            text_loud: step(&SLATE, 100),
            text: ink,
            text_quiet: ink.alpha(62),
            text_faint: ink.alpha(35),
            text_on_color: WHITE,

            icon: step(&SLATE, 500),
            icon_secondary: step(&SLATE, 600),

            rule: ink.alpha(8),
            rule_strong: ink.alpha(14),

            // The fill stays pink-600 so white text on it keeps 5:1; as a
            // foreground on dark it moves up to pink-400.
            brand: step(&PINK, 600),
            brand_hover: step(&PINK, 700),
            brand_hover_border: step(&PINK, 400),
            brand_text: step(&PINK, 400),
            accent: step(&BLUE, 400),
            link: step(&BLUE, 400),
            link_hover: step(&PINK, 400),
            focus_ring: step(&PINK, 400),
            wordmark: step(&CYAN, 400),

            success: step(&GREEN, 400),
            warning: step(&GOLD, 400),
            info: step(&CYAN, 400),
            error: step(&RED, 300),

            green: step(&GREEN, 400),
            green_tint: tint(&GREEN),
            gold: step(&GOLD, 400),
            gold_tint: tint(&GOLD),
            cyan: step(&CYAN, 400),
            cyan_tint: tint(&CYAN),
            blue: step(&BLUE, 400),
            blue_tint: tint(&BLUE),
            pink: step(&PINK, 400),
            pink_tint: tint(&PINK),
            purple: step(&PURPLE, 400),
            purple_tint: tint(&PURPLE),
            red: step(&RED, 300),
            red_tint: tint(&RED),
            orange: step(&ORANGE, 400),
            orange_tint: tint(&ORANGE),
            neutral_tint: ink.alpha(10),

            zen: DARK_ZEN,
            // Red-500, not -600: white text keeps 5:1 and the fill keeps
            // 3:1 on the main surface; the border carries it on raised ones.
            error_fill: step(&RED, 500),
            danger_border: step(&RED, 400),
            notice: step(&GOLD, 500).alpha(16),

            nav_selected: DARK_SELECTED,
            nav_selected_ring: ink.alpha(14),
            nav_shadow: Rgba::hex(0x000000).alpha(30),
            row_selected: ink.alpha(10),
            text_selection: Rgba::hex(0x2c3a5e),
            search_match: Rgba::hex(0x4d4320),
            search_current: Rgba::hex(0x8a6d00),

            ring: ink.alpha(8),
            shadow: Rgba::hex(0x000000).alpha(40),
            scrim: step(&BLACK, 700).alpha(70),
        }
    }

    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => Self::dark(),
            Appearance::Light => Self::light(),
        }
    }
}

/// Between cyan-600 and cyan-700: cyan-600 (#1989c4) is 3.9:1 on white, too
/// light for text, and cyan-700 (#004973) reads as navy.
const CYAN_650: Rgba = Rgba::hex(0x0c699c);
/// The dark main surface: 20% of the way from black-700 to black-600.
const DARK_SURFACE: Rgba = Rgba::hex(0x181a20);
/// A raised dark surface (cards, menus): 40% of the way.
const DARK_RAISED: Rgba = Rgba::hex(0x22252e);
/// The dark selected nav row: 60% of the way, a clear lift above
/// [`DARK_RAISED`].
const DARK_SELECTED: Rgba = Rgba::hex(0x2a2e3a);
/// The dark selected tab or segment: a step above the selected nav row, so
/// it stands off its track.
const DARK_TAB: Rgba = Rgba::hex(0x3a3f4f);
/// The dark Zen band: between green-700 and green-600, so white text keeps
/// 5:1 and the band keeps 3:1 against the dark surfaces.
const DARK_ZEN: Rgba = Rgba::hex(0x00803f);

/// The project badges' fills (the switcher's initials): Taskpage hues dark
/// enough for white initials, picked per project by [`badge_fill`].
pub const PROJECT_BADGES: [Rgba; 8] = [
    hues::step(&hues::PINK, 600),
    hues::step(&hues::BLUE, 600),
    hues::step(&hues::PURPLE, 700),
    hues::step(&hues::GREEN, 700),
    CYAN_650,
    hues::step(&hues::ORANGE, 700),
    hues::step(&hues::RED, 600),
    hues::step(&hues::GOLD, 700),
];

/// The badge fill for a project, chosen from its name (FNV-1a), so a
/// project keeps its color across launches and machines.
pub fn badge_fill(name: &str) -> Rgba {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    PROJECT_BADGES[hash as usize % PROJECT_BADGES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const AA: f64 = 4.5;

    /// Text roles and the surfaces each is drawn on.
    /// A text role (named), its color, and the named backgrounds it's drawn on.
    type Case = (&'static str, Rgba, Vec<(&'static str, Rgba)>);

    fn text_cases(p: &Palette) -> Vec<Case> {
        let surfaces = vec![
            ("surface", p.surface),
            ("surface_recessed", p.surface_recessed),
            ("surface_sunken", p.surface_sunken),
            ("surface_raised", p.surface_raised),
            ("nav_selected", p.nav_selected),
        ];
        // Hover and selection fills sit over a surface; check them over the
        // one they're used on most (the leading column and a raised panel).
        let mut filled = surfaces.clone();
        for (name, fill) in [
            ("fill_subtle", p.fill_subtle),
            ("fill", p.fill),
            ("row_selected", p.row_selected),
        ] {
            filled.push((name, flatten(fill, p.surface_recessed)));
            filled.push((name, flatten(fill, p.surface_raised)));
        }
        let mut cases = vec![
            ("text_loud", p.text_loud, filled.clone()),
            ("text", p.text, filled.clone()),
            ("text_quiet", p.text_quiet, filled.clone()),
        ];
        for (name, color) in [
            ("wordmark", p.wordmark),
            ("success", p.success),
            ("warning", p.warning),
            ("info", p.info),
            ("error", p.error),
            ("link", p.link),
            ("accent", p.accent),
            ("green", p.green),
            ("gold", p.gold),
            ("cyan", p.cyan),
            ("blue", p.blue),
            ("pink", p.pink),
            ("purple", p.purple),
            ("red", p.red),
            ("orange", p.orange),
        ] {
            cases.push((name, color, surfaces.clone()));
        }
        // A chip's text on its own tint, the tint over a raised card.
        for (name, color, tint) in [
            ("green on green_tint", p.green, p.green_tint),
            ("gold on gold_tint", p.gold, p.gold_tint),
            ("cyan on cyan_tint", p.cyan, p.cyan_tint),
            ("blue on blue_tint", p.blue, p.blue_tint),
            ("pink on pink_tint", p.pink, p.pink_tint),
            ("purple on purple_tint", p.purple, p.purple_tint),
            ("red on red_tint", p.red, p.red_tint),
            ("orange on orange_tint", p.orange, p.orange_tint),
            ("text on neutral_tint", p.text, p.neutral_tint),
            ("gold on notice", p.gold, p.notice),
            ("text on notice", p.text, p.notice),
        ] {
            cases.push((
                name,
                color,
                vec![
                    ("over surface_raised", flatten(tint, p.surface_raised)),
                    ("over surface", flatten(tint, p.surface)),
                ],
            ));
        }
        // Text on a saturated fill.
        cases.push((
            "text_on_color",
            p.text_on_color,
            vec![
                ("brand", p.brand),
                ("brand_hover", flatten(p.brand_hover, p.surface)),
                ("zen", p.zen),
                ("error_fill", p.error_fill),
            ],
        ));
        cases
    }

    fn flatten(c: Rgba, under: Rgba) -> Rgba {
        Rgba {
            rgb: c.over(under.rgb),
            a: 0xff,
        }
    }

    #[test]
    fn every_text_role_clears_aa_on_its_surfaces() {
        for p in [Palette::light(), Palette::dark()] {
            let mut failures = Vec::new();
            for (name, fg, backgrounds) in text_cases(&p) {
                for (bg_name, bg) in backgrounds {
                    assert_eq!(bg.a, 0xff, "{bg_name} must be flattened");
                    let ratio = fg.over(bg.rgb).contrast_ratio(bg.rgb);
                    if ratio < AA {
                        failures.push(format!("{name} on {bg_name}: {ratio:.2}"));
                    }
                }
            }
            assert!(
                failures.is_empty(),
                "{:?} below {AA}:1:\n{}",
                p.appearance,
                failures.join("\n")
            );
        }
    }

    #[test]
    fn icons_clear_the_non_text_contrast() {
        // WCAG 1.4.11: 3:1 for meaningful graphics.
        for p in [Palette::light(), Palette::dark()] {
            for bg in [p.surface, p.surface_recessed, p.surface_raised] {
                let ratio = p.icon.over(bg.rgb).contrast_ratio(bg.rgb);
                assert!(ratio >= 3.0, "{:?} icon {ratio:.2}", p.appearance);
                let ring = p.focus_ring.over(bg.rgb).contrast_ratio(bg.rgb);
                assert!(ring >= 3.0, "{:?} focus ring {ring:.2}", p.appearance);
            }
        }
    }

    /// The surfaces a control sits on: the main area, the leading column,
    /// and a raised card, menu or dialog.
    fn control_surfaces(p: &Palette) -> [(&'static str, Rgba); 3] {
        [
            ("surface", p.surface),
            ("surface_recessed", p.surface_recessed),
            ("surface_raised", p.surface_raised),
        ]
    }

    fn ratio(fg: Rgba, bg: Rgba) -> f64 {
        fg.over(bg.rgb).contrast_ratio(bg.rgb)
    }

    /// Every control state on every surface: a label at 4.5:1 on the
    /// control's fill, and (dark) the control's outer edge at 3:1 against the
    /// surface (WCAG 1.4.11). Light keeps Taskpage's fill-only controls, whose
    /// labels identify them, so its edges aren't held to 3:1; its labels are.
    #[test]
    fn controls_clear_contrast_in_every_state() {
        for p in [Palette::light(), Palette::dark()] {
            let dark = p.appearance == Appearance::Dark;
            let mut failures = Vec::new();
            let mut check = |what: String, r: f64, min: f64| {
                if r < min {
                    failures.push(format!("{what}: {r:.2} < {min}"));
                }
            };
            // (state, fill, border)
            let states = [
                ("rest", p.control_fill, p.control_border),
                ("hover", p.control_fill_hover, p.control_border_hover),
                ("pressed", p.control_fill_pressed, p.control_border_hover),
            ];
            for (sname, s) in control_surfaces(&p) {
                for (state, fill, border) in states {
                    let filled = flatten(fill, s);
                    let edge = flatten(border, filled);
                    for (lname, label) in [("text_loud", p.text_loud), ("text", p.text)] {
                        check(format!("{lname} on {state} control over {sname}"), ratio(label, filled), AA);
                    }
                    // An icon at rest in the icon role; hovered or pressed it
                    // turns loud (Taskpage's `hover:text-loud`).
                    let icon = if state == "rest" { p.icon } else { p.text_loud };
                    check(format!("icon on {state} control over {sname}"), ratio(icon, filled), 3.0);
                    if dark {
                        check(format!("{state} control edge on {sname}"), ratio(edge, s), 3.0);
                    }
                }
                // The selected segment over its track.
                let track = flatten(p.control_fill, s);
                let tab = flatten(p.tab_selected, track);
                check(format!("text_loud on tab_selected over {sname}"), ratio(p.text_loud, tab), AA);
                check(format!("text on the track over {sname}"), ratio(p.text, track), AA);
                if dark {
                    let ring = flatten(p.tab_selected_ring, tab);
                    check(format!("tab_selected ring on the track over {sname}"), ratio(ring, track), 3.0);
                }
                assert!(
                    p.tab_selected.over(track.rgb).relative_luminance() > track.rgb.relative_luminance(),
                    "{:?}: the selected segment is lifted above its track",
                    p.appearance
                );
                // Filled buttons: the label on the fill in every state, and
                // the edge against the surface.
                for (name, fill, border) in [
                    ("brand", p.brand, p.brand),
                    ("brand_hover", p.brand_hover, p.brand_hover_border),
                    ("error_fill", p.error_fill, p.danger_border),
                ] {
                    check(format!("text_on_color on {name}"), ratio(p.text_on_color, fill), AA);
                    if dark {
                        check(format!("{name} button edge on {sname}"), ratio(border, s), 3.0);
                    }
                }
                // A text field's boundary, in both themes.
                check(format!("input_border on {sname}"), ratio(p.input_border, s), 3.0);
                // The focus ring around any control.
                check(format!("focus_ring on {sname}"), ratio(p.focus_ring, s), 3.0);
                // The Zen band, a status band on the leading column.
                if dark {
                    check(format!("zen band on {sname}"), ratio(p.zen, s), 3.0);
                }
            }
            // The search box's keycap: its label on the keycap over the box.
            let boxed = flatten(p.control_fill, p.surface_recessed);
            check("text on keycap".into(), ratio(p.text, flatten(p.keycap, boxed)), AA);
            // The sidebar's selected chip stands off the column in dark.
            if dark {
                let chip = flatten(p.nav_selected, p.surface_recessed);
                let ring = flatten(p.nav_selected_ring, chip);
                assert!(ratio(ring, p.surface_recessed) > ratio(chip, p.surface_recessed));
            }
            assert!(
                failures.is_empty(),
                "{:?} controls:\n{}",
                p.appearance,
                failures.join("\n")
            );
        }
    }

    #[test]
    fn project_badges_carry_white_initials() {
        for fill in PROJECT_BADGES {
            let r = ratio(Rgba::hex(0xffffff), fill);
            assert!(r >= AA, "white on {}: {r:.2}", fill.css());
        }
        assert_eq!(badge_fill("shelbi"), badge_fill("shelbi"), "stable");
        let distinct: std::collections::HashSet<_> = ["shelbi", "contextstore", "taskpage", "demo"]
            .iter()
            .map(|n| badge_fill(n))
            .collect();
        assert!(distinct.len() >= 3, "names spread over the hues");
    }

    #[test]
    fn light_is_taskpage_as_is() {
        let p = Palette::light();
        assert_eq!(p.surface, Rgba::hex(0xffffff));
        assert_eq!(p.surface_recessed, Rgba::hex(0xf8f8fa), "slate-100");
        assert_eq!(p.text, Rgba::hex(0x2e3345), "black-600");
        assert_eq!(p.icon, Rgba::hex(0x7b85a7), "slate-600");
        assert_eq!(p.brand, Rgba::hex(0xdb0a60), "pink-600");
        assert_eq!(p.nav_selected, Rgba::hex(0xffffff), "the white chip");
        assert_eq!(p.rule, Rgba::hex(0x2e3345).alpha(10));
    }

    #[test]
    fn dark_runs_on_the_black_scale() {
        let p = Palette::dark();
        assert_eq!(p.surface_recessed, Rgba::hex(0x131418), "black-700");
        // Every surface is a neutral between black-700 and black-600.
        let (lo, hi) = (Rgb::hex(0x0e0f12), Rgb::hex(0x2e3345));
        for (name, s) in [
            ("surface", p.surface),
            ("surface_sunken", p.surface_sunken),
            ("surface_raised", p.surface_raised),
            ("nav_selected", p.nav_selected),
        ] {
            let l = s.rgb.relative_luminance();
            assert!(
                l >= lo.relative_luminance() && l <= hi.relative_luminance(),
                "{name}"
            );
        }
        // The leading column sits a step darker than the main area, and a
        // raised surface a step lighter.
        let lum = |c: Rgba| c.rgb.relative_luminance();
        assert!(lum(p.surface_recessed) < lum(p.surface));
        assert!(lum(p.surface) < lum(p.surface_raised));
        assert!(lum(p.surface_raised) < lum(p.nav_selected));
    }

    #[test]
    fn alpha_rounds_tailwind_percentages() {
        assert_eq!(Rgba::hex(0).alpha(10).a, 0x1a);
        assert_eq!(Rgba::hex(0).alpha(100).a, 0xff);
        assert_eq!(Rgba::hex(0).alpha(0).a, 0);
    }

    /// `docs/theme.md` lists every role with its light and dark value, so the
    /// doc can't drift from the palette. Run with `--nocapture` and
    /// `SHELBI_PRINT_THEME_TABLE=1` to print the table rows.
    #[test]
    fn the_theme_doc_lists_every_role() {
        let doc = include_str!("../../docs/theme.md");
        let (light, dark) = (Palette::light(), Palette::dark());
        let mut missing = Vec::new();
        for ((role, l), (_, d)) in light.roles().into_iter().zip(dark.roles()) {
            let row = format!("| `{role}` | `{}` | `{}` |", l.css(), d.css());
            if std::env::var_os("SHELBI_PRINT_THEME_TABLE").is_some() {
                println!("{row}");
            }
            if !doc.lines().any(|line| line.starts_with(&row)) {
                missing.push(row);
            }
        }
        assert!(
            missing.is_empty(),
            "docs/theme.md is missing (or has stale) rows:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn roles_lists_every_field_once() {
        let roles = Palette::light().roles();
        let mut names: Vec<_> = roles.iter().map(|(n, _)| *n).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n);
    }
}
