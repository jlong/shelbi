//! The desktop's own palette: the Figma design system's tokens as semantic
//! roles.
//!
//! The TUI keeps the shared tokens ([`shelbi_app::theme`]); the desktop paints
//! from these roles instead, after John's Figma file (`Shelbi`, fileKey
//! `pu3hxHgWoCDPCzJ4u6zTQK`). Each role's two values are the Figma variable's
//! Dark and Light modes ([`figma`]), or, where Figma draws a color without a
//! variable (the sidebar tint, the board's cards, the scrim), the value
//! measured from the frame. `desktop/docs/theme.md` lists every role with
//! both values and which token or frame it comes from; the tests check WCAG
//! AA for every text role on every surface it is drawn on, and list where a
//! Figma value falls short instead of shifting it.
//!
//! A role names what a color is for, not its hue: views ask for `sidebar` or
//! `muted`, never for `#acb4cf`.

use shelbi_app::theme::{Appearance, Rgb};

/// The Figma variables, by their Figma names, one `(dark, light)` pair each.
/// Translucent tokens keep their alpha: `theme.background` is a slate overlay
/// that tints whatever it sits on, as in Figma.
pub mod figma {
    use super::Rgba;

    type Pair = (Rgba, Rgba);

    const fn pair(dark: u32, light: u32) -> Pair {
        (Rgba::hexa(dark), Rgba::hexa(light))
    }

    /// `theme.canvas`: the main surface and the terminal's background.
    pub const CANVAS: Pair = pair(0x1e2330ff, 0xffffffff);
    /// `theme.background`: the overlay fill (controls, chips, badges).
    pub const BACKGROUND: Pair = pair(0xacb4cf2e, 0xacb4cf1f);
    /// `button/default/background-hover`. Figma defines it in the Light
    /// mode only (`#acb4cf38`, `theme.background` + 10 points); Dark takes
    /// the same step over its own rest (18% → 28%).
    pub const BACKGROUND_HOVER: Pair = pair(0xacb4cf47, 0xacb4cf38);
    /// `button/default/background-active`: another 10 points (Dark derived
    /// the same way).
    pub const BACKGROUND_ACTIVE: Pair = pair(0xacb4cf61, 0xacb4cf52);
    /// `theme.selection`: the selected segment, the terminal's selection.
    pub const SELECTION: Pair = pair(0xacb4cf8c, 0xacb4cf66);
    /// `theme.search`: a search match, the Activity filter chips.
    pub const SEARCH: Pair = pair(0xacb4cf40, 0xacb4cf29);
    /// `theme.divider` (Orchestrator, Activity, Settings, Issue detail).
    pub const DIVIDER: Pair = pair(0xacb4cf1a, 0xacb4cf1a);
    /// `theme.divider` as the Issues frame overrides it (the board's rules).
    pub const DIVIDER_STRONG: Pair = pair(0xacb4cf33, 0xacb4cf33);
    /// `theme.foreground`: body text.
    pub const FOREGROUND: Pair = pair(0xf2f3f7b2, 0x020303b2);
    /// `theme.white`: loud text (near black in Light, despite the name).
    pub const WHITE: Pair = pair(0xf8f8faff, 0x020303ff);
    /// `theme.gray`: secondary text, hotkeys, icons.
    pub const GRAY: Pair = pair(0xacb4cfe5, 0x7b85a7e5);
    /// `theme.gray` as the Settings frame overrides it: `#7b85a7`, opaque,
    /// in both modes (a field's value, an unselected choice, a form
    /// button's label).
    pub const GRAY_SETTINGS: Pair = pair(0x7b85a7ff, 0x7b85a7ff);
    /// `theme.muted`: quiet text (section headers, branches, hints).
    pub const MUTED: Pair = pair(0x777a86ff, 0x444c65a6);
    /// `theme.gray` as the Activity frame sets it: `#7b85a7`, opaque, in
    /// both modes (the log's titles and the `you` actor).
    pub const GRAY_ACTIVITY: Pair = pair(0x7b85a7ff, 0x7b85a7ff);
    /// `icon/select`: the selected nav icon, the accent.
    pub const ICON_SELECT: Pair = pair(0x62cdf6ff, 0xf21478ff);
    pub const ICON_CYAN: Pair = pair(0x62cdf6ff, 0x31ade9ff);
    pub const ICON_GREEN: Pair = pair(0x31d924ff, 0x5ac351ff);
    pub const ICON_YELLOW: Pair = pair(0xffdd64ff, 0xffca0dff);
    pub const ICON_PURPLE: Pair = pair(0xe374ffff, 0xb247e7ff);
    /// `text/green`: success text, the Zen pill's label.
    pub const TEXT_GREEN: Pair = pair(0x31d924ff, 0x00753dff);
    /// `text/link` (Dark from the Review frame, Light from its light frame;
    /// `text/cyan` in the palette frames is the same pair).
    pub const TEXT_LINK: Pair = pair(0x30c6ffff, 0x1989c4ff);
    /// `theme.green`: green output in the terminal.
    pub const THEME_GREEN: Pair = pair(0x5acd25ff, 0x367b16ff);
    /// `theme.cyan`: cyan output in the terminal.
    pub const THEME_CYAN: Pair = pair(0x00a6b2ff, 0x007a83ff);
    /// `theme.red`: red output in the terminal (the Workspace frame's
    /// removed line).
    pub const THEME_RED: Pair = pair(0xe04f52ff, 0xc23b3eff);
    /// `theme.ansi_*`: the Activity log's colors (status tags, actors,
    /// timestamps) and the terminal's ANSI slots.
    pub const ANSI_GREEN: Pair = pair(0x5fb56dff, 0x1a7f37ff);
    pub const ANSI_YELLOW: Pair = pair(0xdcb767ff, 0x8a5f04ff);
    pub const ANSI_BLUE: Pair = pair(0x4a8fd7ff, 0x2563c9ff);
    pub const ANSI_MAGENTA: Pair = pair(0xc586c0ff, 0xa21cafff);
    pub const ANSI_CYAN: Pair = pair(0x4ec9b0ff, 0x0f766eff);
    pub const ANSI_DARK_GRAY: Pair = pair(0x7c7c7cff, 0x6b6b6bff);
    pub const WORKFLOW_BADGE_BG: Pair = pair(0x3a4258ff, 0xd3d8e4ff);
    pub const WORKFLOW_BADGE_FG: Pair = pair(0xdcdfe8ff, 0x2a3142ff);
    pub const FOCUS_RING: Pair = pair(0x62cdf6ff, 0x31ade9ff);
    pub const FOCUS_GLOW: Pair = pair(0x12adff33, 0x12adff33);
    /// The Button component's Success style: `#5ac351` at the
    /// `theme.background` / hover / active alphas (Light defined, Dark at
    /// Dark's alphas).
    pub const SUCCESS_FILL: Pair = pair(0x5ac3512e, 0x5ac3511f);
    pub const SUCCESS_FILL_HOVER: Pair = pair(0x5ac35147, 0x5ac35138);
    pub const SUCCESS_FILL_ACTIVE: Pair = pair(0x5ac35161, 0x5ac35152);
    /// The Danger style: `#f21478` at the same alphas.
    pub const DANGER_FILL: Pair = pair(0xf214782e, 0xf214781f);
    pub const DANGER_FILL_HOVER: Pair = pair(0xf2147847, 0xf2147838);
    pub const DANGER_FILL_ACTIVE: Pair = pair(0xf2147861, 0xf2147852);
    /// The Danger style's label: `#b40041` in Light; Figma has no Dark
    /// value, so Dark takes `focus/danger/glow`'s pink (`#ff66ab`), which
    /// reads on the dark surfaces.
    pub const DANGER_TEXT: Pair = pair(0xff66abff, 0xb40041ff);
    /// `shadow/raised card`'s colors: the three drops, and the 1px ring.
    pub const SHADOW: Pair = pair(0x0a1e5105, 0x0a1e5105);
    pub const SHADOW_RING: Pair = pair(0x0a1e5108, 0x0a1e5108);
    /// The Command palette's drop (an effect, `0 10 15 -3`, not a
    /// variable): black at 25% in both modes.
    pub const PALETTE_SHADOW: Pair = pair(0x00000040, 0x00000040);

    /// Colors Figma draws without a variable, measured from the 1x frames
    /// (1 px = 1 pt) and named for where they appear.
    pub mod measured {
        use super::{pair, Pair};

        /// The Navigation component's fill: `#acb4cf` at 10% over the dark
        /// canvas, at about 7% over white.
        pub const SIDEBAR: Pair = pair(0x2c3140ff, 0xf9f9fcff);
        /// The Issues board's cards: near black on the dark canvas, white
        /// (with the raised-card shadow) in Light.
        pub const CARD: Pair = pair(0x020303ff, 0xffffffff);
        /// The command palette's surface: the sidebar's tint.
        pub const RAISED: Pair = pair(0x2c3140ff, 0xf9f9fcff);
        /// The dark Issue detail's 1px edge (`#acb4cf` at ~5% would read as
        /// nothing; it is `theme.divider`), the light one the raised-card
        /// ring.
        pub const RING: Pair = pair(0xacb4cf1a, 0x0a1e5108);
        /// The palette's scrim: near black at 35% over the dark window,
        /// `theme.muted`'s light value (`#444c65` at 65%) over the light one.
        pub const SCRIM: Pair = pair(0x02030359, 0x444c65a6);
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

    /// `0xRRGGBBAA`, as Figma writes a translucent token.
    pub const fn hexa(value: u32) -> Self {
        Self {
            rgb: Rgb::hex(value >> 8),
            a: (value & 0xff) as u8,
        }
    }

    /// This color at `percent` opacity.
    pub const fn alpha(self, percent: u8) -> Self {
        Self {
            rgb: self.rgb,
            a: ((percent as u16 * 255 + 50) / 100) as u8,
        }
    }

    /// This color at another's alpha: a hue at `theme.background`'s
    /// opacity is its chip tint.
    pub const fn with_alpha_of(self, other: Rgba) -> Self {
        Self {
            rgb: self.rgb,
            a: other.a,
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
    /// background (`theme.canvas`).
    canvas,
    /// The leading column: the sidebar, the review and workspace panels
    /// (the Navigation component's tint).
    sidebar,
    /// A board card: near black on the dark canvas, white in Light.
    card,
    /// A surface lifted over the window: the command palette, menus,
    /// popovers, tooltips.
    raised,
    /// A text field's well (Settings: the canvas, inside a divider).
    well,

    // ---- Overlay fills (the `theme.background` family) ---------------------
    /// The overlay fill at rest: buttons, dropdowns, chips, count badges,
    /// the search box, a hovered row (`theme.background`).
    background,
    /// The overlay fill hovered (`button/default/background-hover`).
    background_hover,
    /// The overlay fill pressed, or a control while its menu is open
    /// (`button/default/background-active`).
    background_active,
    /// The selected segment of a segmented control (`theme.selection`).
    selection,
    /// The Activity filter chips' fill (`theme.search`).
    search,
    /// A keycap in a shortcut hint (the Button component's Hotkey: the
    /// canvas).
    keycap,
    /// The keycap's 1px bottom edge.
    keycap_edge,

    // ---- Text ---------------------------------------------------------------
    /// Headings, titles, the selected row's label (`theme.white`).
    text_loud,
    /// Body text and labels (`theme.foreground`).
    foreground,
    /// Secondary text: hotkeys, metadata (`theme.gray`).
    gray,
    /// A settings control's text: a field's value, an unselected choice, a
    /// form button's label (`theme.gray` as the Settings frame overrides
    /// it).
    form_text,
    /// Quiet text: section headers, branches, timestamps, hints, idle
    /// states, placeholders (`theme.muted`).
    muted,
    /// Text on a saturated fill (a project badge's initials).
    text_on_color,
    /// The Activity log's titles and its `you` actor (the Activity frame's
    /// `theme.gray`).
    log_text,

    // ---- Icons --------------------------------------------------------------
    /// Icons at rest (`theme.gray`).
    icon,
    /// The selected nav icon and the current-selection marker: cyan in
    /// Dark, pink in Light (`icon/select`).
    accent,
    /// A ready-for-review check (`icon/cyan`).
    icon_cyan,
    /// A working workspace (`icon/green`).
    icon_green,
    /// A workspace awaiting input (`icon/yellow`).
    icon_yellow,
    /// A review status glyph (`icon/purple`).
    icon_purple,

    // ---- Rules --------------------------------------------------------------
    /// Dividers and hairline borders (`theme.divider`).
    divider,
    /// The board's stronger rules (the Issues frame's `theme.divider`).
    divider_strong,
    /// A text field's 1px boundary (Settings: `theme.divider`).
    input_border,

    // ---- Links and focus ----------------------------------------------------
    /// A link, the "More" action (`text/link`).
    link,
    /// A hovered link (`theme.white`).
    link_hover,
    /// The keyboard-focus ring (`focus/default/ring`).
    focus_ring,
    /// The glow outside the ring (`focus/default/focus-glow`).
    focus_glow,

    // ---- Meaning ------------------------------------------------------------
    /// Success, done, approve (`text/green`).
    success,
    /// Warnings, in progress (`theme.ansi_yellow`).
    warning,
    /// Ready, informational (`theme.ansi_blue`).
    info,
    /// Errors, blocked, reject (the Danger button's label).
    error,

    // ---- Log and terminal hues (Activity's tags, ANSI slots) ---------------
    /// Green output in the terminal (`theme.green`).
    theme_green,
    /// Cyan output in the terminal (`theme.cyan`).
    theme_cyan,
    /// Red output in the terminal (`theme.red`).
    theme_red,
    /// `[done]` (`theme.ansi_green`).
    green,
    /// `[in progress]` (`theme.ansi_yellow`).
    yellow,
    /// `[ready]`, queued work (`theme.ansi_blue`).
    blue,
    /// `[review]`, handoff (`theme.ansi_magenta`).
    magenta,
    /// A working actor in the log (`theme.ansi_cyan`).
    cyan,
    /// Timestamps in the log (`theme.ansi_dark_gray`).
    dark_gray,
    /// Errors and failures as text (the Danger label).
    red,

    // ---- Chip tints: each hue at `theme.background`'s opacity --------------
    /// A green chip's background.
    green_tint,
    /// A yellow chip's background.
    yellow_tint,
    /// A cyan chip's background.
    cyan_tint,
    /// A blue chip's background.
    blue_tint,
    /// A magenta chip's background.
    magenta_tint,
    /// A red chip's background.
    red_tint,
    /// A workflow name's badge on a card (`theme.workflow_badge_bg`).
    workflow_badge,
    /// The workflow badge's label (`theme.workflow_badge_fg`).
    workflow_badge_text,

    // ---- Buttons (the Button component's Success and Danger styles) --------
    /// The Success button, and the Zen pill.
    success_fill,
    /// The Success button hovered.
    success_fill_hover,
    /// The Success button pressed.
    success_fill_active,
    /// The Danger button, and the unread-errors button.
    danger_fill,
    /// The Danger button hovered.
    danger_fill_hover,
    /// The Danger button pressed.
    danger_fill_active,

    // ---- Bands --------------------------------------------------------------
    /// The `Zen mode on` pill (with [`Palette::success`] text).
    zen,
    /// A soft warning band (a status or a notice over the main area):
    /// `icon/yellow` at `theme.background`'s opacity.
    notice,

    // ---- Selection ----------------------------------------------------------
    /// The selected nav row: the canvas, lifted with the raised-card shadow.
    nav_selected,
    /// A selected or keyboard-current row in a list (Activity, the palette):
    /// `theme.background` in Dark, `theme.selection` in Light, as drawn.
    row_selected,
    /// Selected text (the terminal's selection highlight, `theme.selection`).
    text_selection,
    /// A search match's highlight behind text (`theme.search`).
    search_match,
    /// The current search match's highlight: `icon/yellow` at 40%, where
    /// Figma draws no current match.
    search_current,

    // ---- Elevation ----------------------------------------------------------
    /// The 1px edge around a raised surface (menus, popovers, dialogs).
    ring,
    /// `shadow/raised card`'s drop color.
    shadow,
    /// `shadow/raised card`'s 1px ring.
    shadow_ring,
    /// The command palette's drop, over the scrim.
    palette_shadow,
    /// The backdrop behind a modal overlay.
    scrim,
}

impl Palette {
    /// The Figma Light mode.
    pub fn light() -> Self {
        Self::from_tokens(Appearance::Light, |(_, light)| light)
    }

    /// The Figma Dark mode.
    pub fn dark() -> Self {
        Self::from_tokens(Appearance::Dark, |(dark, _)| dark)
    }

    /// Every role from the token pair `pick` chooses a side of.
    fn from_tokens(appearance: Appearance, pick: impl Fn((Rgba, Rgba)) -> Rgba) -> Self {
        use figma::measured::*;
        use figma::*;
        let background = pick(BACKGROUND);
        let tint = |hue: Rgba| hue.with_alpha_of(background);
        let (green, yellow, blue, magenta, cyan, red) = (
            pick(ANSI_GREEN),
            pick(ANSI_YELLOW),
            pick(ANSI_BLUE),
            pick(ANSI_MAGENTA),
            pick(ANSI_CYAN),
            pick(DANGER_TEXT),
        );
        let canvas = pick(CANVAS);
        Self {
            appearance,
            canvas,
            sidebar: pick(SIDEBAR),
            card: pick(CARD),
            raised: pick(RAISED),
            well: canvas,

            background,
            background_hover: pick(BACKGROUND_HOVER),
            background_active: pick(BACKGROUND_ACTIVE),
            selection: pick(SELECTION),
            search: pick(SEARCH),
            keycap: canvas,
            // The Button component's Hotkey drop: `#acb4cf` at 60%.
            keycap_edge: Rgba::hexa(0xacb4cf99),

            text_loud: pick(WHITE),
            foreground: pick(FOREGROUND),
            gray: pick(GRAY),
            form_text: pick(GRAY_SETTINGS),
            muted: pick(MUTED),
            text_on_color: Rgba::hex(0xffffff),
            log_text: pick(GRAY_ACTIVITY),

            icon: pick(GRAY),
            accent: pick(ICON_SELECT),
            icon_cyan: pick(ICON_CYAN),
            icon_green: pick(ICON_GREEN),
            icon_yellow: pick(ICON_YELLOW),
            icon_purple: pick(ICON_PURPLE),

            divider: pick(DIVIDER),
            divider_strong: pick(DIVIDER_STRONG),
            input_border: pick(DIVIDER),

            link: pick(TEXT_LINK),
            link_hover: pick(WHITE),
            focus_ring: pick(FOCUS_RING),
            focus_glow: pick(FOCUS_GLOW),

            success: pick(TEXT_GREEN),
            warning: yellow,
            info: blue,
            error: red,

            theme_green: pick(THEME_GREEN),
            theme_cyan: pick(THEME_CYAN),
            theme_red: pick(THEME_RED),
            green,
            yellow,
            blue,
            magenta,
            cyan,
            dark_gray: pick(ANSI_DARK_GRAY),
            red,

            green_tint: tint(green),
            yellow_tint: tint(yellow),
            cyan_tint: tint(cyan),
            blue_tint: tint(blue),
            magenta_tint: tint(magenta),
            red_tint: tint(red),
            workflow_badge: pick(WORKFLOW_BADGE_BG),
            workflow_badge_text: pick(WORKFLOW_BADGE_FG),

            success_fill: pick(SUCCESS_FILL),
            success_fill_hover: pick(SUCCESS_FILL_HOVER),
            success_fill_active: pick(SUCCESS_FILL_ACTIVE),
            danger_fill: pick(DANGER_FILL),
            danger_fill_hover: pick(DANGER_FILL_HOVER),
            danger_fill_active: pick(DANGER_FILL_ACTIVE),

            zen: pick(SUCCESS_FILL),
            notice: tint(pick(ICON_YELLOW)),

            nav_selected: canvas,
            row_selected: match appearance {
                Appearance::Dark => background,
                Appearance::Light => pick(SELECTION),
            },
            text_selection: pick(SELECTION),
            search_match: pick(SEARCH),
            search_current: pick(ICON_YELLOW).alpha(40),

            ring: pick(RING),
            shadow: pick(SHADOW),
            shadow_ring: pick(SHADOW_RING),
            palette_shadow: pick(PALETTE_SHADOW),
            scrim: pick(SCRIM),
        }
    }

    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Dark => Self::dark(),
            Appearance::Light => Self::light(),
        }
    }
}

/// The project badges' fills (the switcher's initials). Figma draws no
/// badge, so these stay the hues they were, dark enough for white initials,
/// picked per project by [`badge_fill`].
pub const PROJECT_BADGES: [Rgba; 8] = [
    Rgba::hex(0xdb0a60),
    Rgba::hex(0x2830f1),
    Rgba::hex(0x7019c0),
    Rgba::hex(0x00753d),
    Rgba::hex(0x0c699c),
    Rgba::hex(0x9d4100),
    Rgba::hex(0xaa0101),
    Rgba::hex(0x856700),
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

/// Where a Figma value falls short of WCAG where it is drawn. The design
/// keeps these values (match Figma, list the shortfall); the contrast tests
/// fail on any shortfall not covered here, and on an entry that no longer
/// covers one, and `docs/theme.md` lists each entry with its worst ratio and
/// why. `(theme, pairing prefix)`: an entry covers every pairing whose name
/// starts with it (`muted on` covers muted on every surface).
#[cfg(test)]
pub const CONTRAST_EXCEPTIONS: &[(&str, &str)] = &[
    ("dark", "muted on"),
    ("dark", "dark_gray on"),
    ("dark", "log_text on"),
    ("dark", "info on"),
    ("dark", "blue on"),
    ("dark", "gray on background"),
    ("dark", "gray on row_selected"),
    ("dark", "magenta on magenta_tint"),
    ("dark", "red on red_tint"),
    ("dark", "text_loud on selection"),
    ("dark", "gray on selection"),
    ("dark", "theme_green on selection"),
    ("dark", "theme_cyan on selection"),
    ("dark", "success on hover button"),
    ("dark", "success on pressed button"),
    ("dark", "error on rest button"),
    ("dark", "error on hover button"),
    ("dark", "error on pressed button"),
    ("dark", "input_border on"),
    ("dark", "form_text on"),
    ("light", "muted on"),
    ("light", "gray on"),
    ("light", "log_text on"),
    ("light", "icon on"),
    ("light", "link on"),
    ("light", "green on green_tint"),
    ("light", "success on pressed button"),
    ("light", "error on pressed button"),
    ("light", "focus_ring on"),
    ("light", "dark_gray on selection"),
    ("light", "theme_green on selection"),
    ("light", "theme_cyan on selection"),
    ("light", "input_border on"),
    ("light", "form_text on"),
];

#[cfg(test)]
mod tests {
    use super::*;

    const AA: f64 = 4.5;
    const NON_TEXT: f64 = 3.0;

    fn flatten(c: Rgba, under: Rgba) -> Rgba {
        Rgba {
            rgb: c.over(under.rgb),
            a: 0xff,
        }
    }

    fn ratio(fg: Rgba, bg: Rgba) -> f64 {
        fg.over(bg.rgb).contrast_ratio(bg.rgb)
    }

    fn theme_name(p: &Palette) -> &'static str {
        match p.appearance {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }

    /// Every pairing the desktop draws, as `(what on what, ratio, minimum)`.
    fn pairings(p: &Palette) -> Vec<(String, f64, f64)> {
        let mut out = Vec::new();
        let mut check = |what: String, fg: Rgba, bg: Rgba, min: f64| {
            assert_eq!(bg.a, 0xff, "{what}: the background must be flattened");
            out.push((what, ratio(fg, bg), min));
        };
        let surfaces = [
            ("canvas", p.canvas),
            ("sidebar", p.sidebar),
            ("card", p.card),
            ("raised", p.raised),
        ];
        // Rows and chips sit on a surface; check them over the surfaces
        // they're drawn on.
        let mut backgrounds: Vec<(String, Rgba)> =
            surfaces.iter().map(|(n, c)| (n.to_string(), *c)).collect();
        for (name, fill) in [("background", p.background), ("row_selected", p.row_selected)] {
            for (sname, s) in [("sidebar", p.sidebar), ("canvas", p.canvas), ("raised", p.raised)] {
                backgrounds.push((format!("{name} over {sname}"), flatten(fill, s)));
            }
        }
        for (name, fg) in [
            ("text_loud", p.text_loud),
            ("foreground", p.foreground),
            ("gray", p.gray),
            ("muted", p.muted),
        ] {
            for (bname, bg) in &backgrounds {
                check(format!("{name} on {bname}"), fg, *bg, AA);
            }
        }
        for (name, fg) in [
            ("link", p.link),
            ("success", p.success),
            ("warning", p.warning),
            ("info", p.info),
            ("error", p.error),
            ("green", p.green),
            ("yellow", p.yellow),
            ("blue", p.blue),
            ("magenta", p.magenta),
            ("cyan", p.cyan),
            ("dark_gray", p.dark_gray),
            ("red", p.red),
        ] {
            for (sname, s) in surfaces {
                check(format!("{name} on {sname}"), fg, s, AA);
            }
        }
        // The log's titles, on the canvas and on its selected row; the
        // pickers' labels on their fill.
        check("log_text on canvas".into(), p.log_text, p.canvas, AA);
        check(
            "log_text on row_selected over canvas".into(),
            p.log_text,
            flatten(p.row_selected, p.canvas),
            AA,
        );
        check(
            "log_text on search over canvas".into(),
            p.log_text,
            flatten(p.search, p.canvas),
            AA,
        );
        // The Machines states and reachability: drawn on the canvas only.
        for (name, fg) in [("theme_green", p.theme_green), ("theme_cyan", p.theme_cyan)] {
            check(format!("{name} on canvas"), fg, p.canvas, AA);
        }
        // A chip's text on its tint, over a card and the canvas.
        for (name, fg, tint) in [
            ("green on green_tint", p.green, p.green_tint),
            ("yellow on yellow_tint", p.yellow, p.yellow_tint),
            ("cyan on cyan_tint", p.cyan, p.cyan_tint),
            ("blue on blue_tint", p.blue, p.blue_tint),
            ("magenta on magenta_tint", p.magenta, p.magenta_tint),
            ("red on red_tint", p.red, p.red_tint),
            ("text_loud on notice", p.text_loud, p.notice),
        ] {
            for (sname, s) in [("card", p.card), ("canvas", p.canvas)] {
                check(format!("{name} over {sname}"), fg, flatten(tint, s), AA);
            }
        }
        check(
            "workflow_badge_text on workflow_badge".into(),
            p.workflow_badge_text,
            p.workflow_badge,
            AA,
        );
        // Buttons: the label on every state's fill, over each surface.
        for (sname, s) in [("canvas", p.canvas), ("sidebar", p.sidebar), ("raised", p.raised)] {
            for (label, fg, fills) in [
                (
                    "text_loud",
                    p.text_loud,
                    [p.background, p.background_hover, p.background_active],
                ),
                (
                    "success",
                    p.success,
                    [p.success_fill, p.success_fill_hover, p.success_fill_active],
                ),
                (
                    "error",
                    p.error,
                    [p.danger_fill, p.danger_fill_hover, p.danger_fill_active],
                ),
            ] {
                for (state, fill) in ["rest", "hover", "pressed"].into_iter().zip(fills) {
                    check(format!("{label} on {state} button over {sname}"), fg, flatten(fill, s), AA);
                }
            }
            let segment = flatten(p.selection, s);
            check(format!("text_loud on selection over {sname}"), p.text_loud, segment, AA);
            check(format!("gray on keycap over {sname}"), p.gray, flatten(p.keycap, s), AA);
            // Non-text (WCAG 1.4.11): icons, the focus ring, a field's edge.
            check(format!("icon on {sname}"), p.icon, s, NON_TEXT);
            check(format!("focus_ring on {sname}"), p.focus_ring, s, NON_TEXT);
            check(format!("input_border on {sname}"), p.input_border, s, NON_TEXT);
        }
        // Settings' controls, on the canvas they sit on: a field's value, an
        // unselected choice, and a form button's label on its fill.
        check("form_text on canvas".into(), p.form_text, p.well, AA);
        check("form_text on form button".into(), p.form_text, flatten(p.search, p.canvas), AA);
        // The selected Machines row: every cell on the selection band.
        let band = flatten(p.selection, p.canvas);
        for (name, fg) in [
            ("gray", p.gray),
            ("muted", p.muted),
            ("dark_gray", p.dark_gray),
            ("theme_green", p.theme_green),
            ("theme_cyan", p.theme_cyan),
        ] {
            check(format!("{name} on selection over canvas"), fg, band, AA);
        }
        check(
            "success on zen over sidebar".into(),
            p.success,
            flatten(p.zen, p.sidebar),
            AA,
        );
        check(
            "accent on nav_selected".into(),
            p.accent,
            p.nav_selected,
            NON_TEXT,
        );
        out
    }

    /// The pairings below their minimum, as exception keys.
    fn shortfalls(p: &Palette) -> Vec<(String, f64, f64)> {
        let theme = theme_name(p);
        pairings(p)
            .into_iter()
            .filter(|(_, r, min)| r < min)
            .map(|(what, r, min)| (format!("{theme}: {what}"), r, min))
            .collect()
    }

    fn covered(key: &str) -> bool {
        CONTRAST_EXCEPTIONS
            .iter()
            .any(|(theme, prefix)| key.starts_with(&format!("{theme}: {prefix}")))
    }

    #[test]
    fn contrast_shortfalls_are_exactly_the_documented_exceptions() {
        let mut found = Vec::new();
        for p in [Palette::dark(), Palette::light()] {
            found.extend(shortfalls(&p));
        }
        let unlisted: Vec<String> = found
            .iter()
            .filter(|(k, _, _)| !covered(k))
            .map(|(k, r, min)| format!("{k}: {r:.2} < {min}"))
            .collect();
        let stale: Vec<String> = CONTRAST_EXCEPTIONS
            .iter()
            .map(|(theme, prefix)| format!("{theme}: {prefix}"))
            .filter(|entry| !found.iter().any(|(k, _, _)| k.starts_with(entry.as_str())))
            .collect();
        assert!(
            unlisted.is_empty() && stale.is_empty(),
            "shortfalls no exception covers:\n{}\nexceptions that cover nothing: {stale:?}",
            unlisted.join("\n")
        );
    }

    /// The worst ratio under each exception, for `docs/theme.md`. Run with
    /// `--nocapture` and `SHELBI_PRINT_THEME_TABLE=1` to print them.
    #[test]
    fn the_theme_doc_lists_every_contrast_exception() {
        let doc = include_str!("../../docs/theme.md");
        let mut found = Vec::new();
        for p in [Palette::dark(), Palette::light()] {
            found.extend(shortfalls(&p));
        }
        let mut missing = Vec::new();
        for (theme, prefix) in CONTRAST_EXCEPTIONS {
            let entry = format!("{theme}: {prefix}");
            let worst = found
                .iter()
                .filter(|(k, _, _)| k.starts_with(entry.as_str()))
                .map(|(_, r, _)| *r)
                .fold(f64::INFINITY, f64::min);
            let row = format!("| {theme} | {prefix} | {worst:.2} |");
            if std::env::var_os("SHELBI_PRINT_THEME_TABLE").is_some() {
                println!("{row}");
            }
            if !doc.lines().any(|l| l.starts_with(row.as_str())) {
                missing.push(row);
            }
        }
        assert!(missing.is_empty(), "docs/theme.md is missing:\n{}", missing.join("\n"));
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
    fn roles_are_the_figma_tokens() {
        let (d, l) = (Palette::dark(), Palette::light());
        let both = |role: Rgba, other: Rgba, want: (u32, u32)| {
            assert_eq!((role, other), (Rgba::hexa(want.0), Rgba::hexa(want.1)));
        };
        both(d.canvas, l.canvas, (0x1e2330ff, 0xffffffff));
        both(d.background, l.background, (0xacb4cf2e, 0xacb4cf1f));
        both(d.divider, l.divider, (0xacb4cf1a, 0xacb4cf1a));
        both(d.foreground, l.foreground, (0xf2f3f7b2, 0x020303b2));
        both(d.text_loud, l.text_loud, (0xf8f8faff, 0x020303ff));
        both(d.gray, l.gray, (0xacb4cfe5, 0x7b85a7e5));
        both(d.muted, l.muted, (0x777a86ff, 0x444c65a6));
        both(d.accent, l.accent, (0x62cdf6ff, 0xf21478ff));
        both(d.focus_ring, l.focus_ring, (0x62cdf6ff, 0x31ade9ff));
        both(d.focus_glow, l.focus_glow, (0x12adff33, 0x12adff33));
        both(d.workflow_badge, l.workflow_badge, (0x3a4258ff, 0xd3d8e4ff));
        both(d.success, l.success, (0x31d924ff, 0x00753dff));
        both(d.theme_green, l.theme_green, (0x5acd25ff, 0x367b16ff));
        both(d.theme_cyan, l.theme_cyan, (0x00a6b2ff, 0x007a83ff));
        both(d.theme_red, l.theme_red, (0xe04f52ff, 0xc23b3eff));
        both(d.link, l.link, (0x30c6ffff, 0x1989c4ff));
        // The frames' surfaces: a lighter sidebar than the dark canvas,
        // near-black cards on it; white and a gray-blue tint in Light.
        both(d.sidebar, l.sidebar, (0x2c3140ff, 0xf9f9fcff));
        both(d.card, l.card, (0x020303ff, 0xffffffff));
        let lum = |c: Rgba| c.rgb.relative_luminance();
        assert!(lum(d.card) < lum(d.canvas) && lum(d.canvas) < lum(d.sidebar));
        assert!(lum(l.sidebar) < lum(l.canvas));
    }

    #[test]
    fn the_sidebar_is_the_slate_overlay_over_the_canvas() {
        // Dark: #acb4cf at 10% over #1e2330, to the unit.
        let s = Rgba::hexa(0xacb4cf1a).over(Palette::dark().canvas.rgb);
        let d = Palette::dark().sidebar.rgb;
        for (a, b) in [(s.r, d.r), (s.g, d.g), (s.b, d.b)] {
            assert!(a.abs_diff(b) <= 1, "{s:?} vs {d:?}");
        }
    }

    #[test]
    fn hexa_reads_figma_eight_digit_tokens() {
        let c = Rgba::hexa(0xacb4cf2e);
        assert_eq!(c.rgb, Rgb::hex(0xacb4cf));
        assert_eq!(c.a, 0x2e);
        assert_eq!(c.css(), "#acb4cf2e");
        assert_eq!(Rgba::hexa(0x1e2330ff).css(), "#1e2330");
        assert_eq!(Rgba::hex(0).alpha(10).a, 0x1a);
    }

    /// `docs/theme.md` lists every role with its dark and light value, so
    /// the doc can't drift from the palette. Run with `--nocapture` and
    /// `SHELBI_PRINT_THEME_TABLE=1` to print the table rows.
    #[test]
    fn the_theme_doc_lists_every_role() {
        let doc = include_str!("../../docs/theme.md");
        let (dark, light) = (Palette::dark(), Palette::light());
        let mut missing = Vec::new();
        for ((role, d), (_, l)) in dark.roles().into_iter().zip(light.roles()) {
            let row = format!("| `{role}` | `{}` | `{}` |", d.css(), l.css());
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
