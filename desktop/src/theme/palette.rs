//! The desktop's own palette: the Figma design system's tokens as semantic
//! roles.
//!
//! The TUI keeps the shared tokens ([`shelbi_app::theme`]); the desktop paints
//! from these roles instead, after John's Figma file (`Shelbi`, fileKey
//! `pu3hxHgWoCDPCzJ4u6zTQK`). Each role's two values are a `03 Desktop`
//! variable's Dark and Light modes ([`figma`]), or, where Figma draws a color
//! without one (the sidebar tint, the raised surface), the value measured
//! from the frame. `desktop/docs/theme.md` lists every role with
//! both values and which token or frame it comes from; the tests check WCAG
//! AA for every text role on every surface it is drawn on, and list where a
//! Figma value falls short instead of shifting it.
//!
//! A role names what a color is for, not its hue: views ask for `sidebar` or
//! `muted`, never for `#acb4cf`.

use shelbi_app::theme::{Appearance, Rgb};

/// The Figma variables, one `(dark, light)` pair each: the `03 Desktop`
/// collection's Dark and Light modes, each constant documented with its
/// semantic Figma name (`surface/*`, `text/*`, `icon/*`, `stroke/*`,
/// `button/*`, `focus/*`, `checkbox/*`). Translucent tokens keep their
/// straight alpha: `button/default/background` is a slate overlay that tints
/// whatever it sits on, as in Figma.
///
/// Figma stores a variable's opacity as a 32-bit float; a byte here is that
/// float times 255, rounded to the nearest (so 70% is `b2` and 90% `e5`,
/// as Figma's own hex reads them, while 50% is `80`).
/// `token_alphas_are_figmas_opacities` holds every percentage-defined token
/// to that convention.
pub mod figma {
    use super::Rgba;

    type Pair = (Rgba, Rgba);

    const fn pair(dark: u32, light: u32) -> Pair {
        (Rgba::hexa(dark), Rgba::hexa(light))
    }

    /// `surface/background` (`Slate/900` / white): the main surface and the
    /// terminal's background.
    pub const CANVAS: Pair = pair(0x1e2330ff, 0xffffffff);
    /// `surface/depth-0` (`Black/950` / white), and
    /// `checkbox/default/background`, which aliases it: the Textbox well, an
    /// unchecked box, a board card.
    pub const DEPTH_0: Pair = pair(0x020303ff, 0xffffffff);
    /// `button/default/background` (also `surface/hover` and
    /// `surface/depth-20`): `Slate/400` at 18% in Dark, 12% in Light, the
    /// overlay fill (controls, chips, badges).
    pub const BACKGROUND: Pair = pair(0xacb4cf2e, 0xacb4cf1f);
    /// `button/default/background-hover`: `Slate/400` at 28% / 22%.
    pub const BACKGROUND_HOVER: Pair = pair(0xacb4cf47, 0xacb4cf38);
    /// `button/default/background-active`: `Slate/400` at 40% / 32%.
    pub const BACKGROUND_ACTIVE: Pair = pair(0xacb4cf66, 0xacb4cf52);
    /// `button/select/background`: the Button's Select style (a filter
    /// that is set), `Cyan/400` at 18% / 12%.
    pub const SELECT_FILL: Pair = pair(0x62cdf62e, 0x62cdf61f);
    /// `button/select/background-hover`: 28% / 22%.
    pub const SELECT_FILL_HOVER: Pair = pair(0x62cdf647, 0x62cdf638);
    /// `button/select/background-active`: 40% / 32%.
    pub const SELECT_FILL_ACTIVE: Pair = pair(0x62cdf666, 0x62cdf652);
    /// `button/select/text` (and `/icon`): `Cyan/300` in Dark, `Cyan/600`
    /// in Light.
    pub const SELECT_TEXT: Pair = pair(0xb0e6fbff, 0x1989c4ff);
    /// `surface/depth-10`: the Issues Header's band, `Slate/400` at 10% /
    /// 8% over the canvas.
    pub const DEPTH_10: Pair = pair(0xacb4cf1a, 0xacb4cf14);
    /// `surface/selection`: `Slate/400` at 55% / 40%, the selected segment,
    /// the terminal's selection.
    pub const SELECTION: Pair = pair(0xacb4cf8c, 0xacb4cf66);
    /// `surface/depth-30`: `Slate/400` at 25% / 16%, a search match, the
    /// Activity filter chips, the segmented track.
    pub const SEARCH: Pair = pair(0xacb4cf40, 0xacb4cf29);
    /// `stroke/light`: `Slate/400` at 10%, dividers.
    pub const DIVIDER: Pair = pair(0xacb4cf1a, 0xacb4cf1a);
    /// `stroke/medium`: `Slate/400` at 20%, the board's rules.
    pub const DIVIDER_STRONG: Pair = pair(0xacb4cf33, 0xacb4cf33);
    /// `stroke/dark`: `Slate/400` at 30% / 50%, a textbox's 1px edge.
    pub const DIVIDER_FORM: Pair = pair(0xacb4cf4d, 0xacb4cf80);
    /// `shadow/hard`: the buttons' Hotkey drop (`Black/950` at 55% in Dark,
    /// `Slate/400` at 60% in Light).
    pub const SEARCH_DETAIL: Pair = pair(0x0203038c, 0xacb4cf99);
    /// `text/foreground`: body text, `Slate/100` at 85% in Dark, `Black/950`
    /// at 70% in Light.
    pub const FOREGROUND: Pair = pair(0xf2f3f7d9, 0x020303b2);
    /// `text/bold` (and `button/default/text`, which aliases it): loud
    /// text, `Slate/100` in Dark, `Black/950` in Light.
    pub const WHITE: Pair = pair(0xf2f3f7ff, 0x020303ff);
    /// `text/muted`: quiet text, `Slate/100` at 55% in Dark, `Slate/600`
    /// at 50% in Light.
    pub const MUTED: Pair = pair(0xf2f3f78c, 0x444c6580);
    /// `icon/primary`: `Slate/400` at 90% in Dark, `Slate/500` at 90% in
    /// Light; icons at rest, and the secondary text (hotkeys, keycaps) the
    /// frames draw in the same color.
    pub const GRAY: Pair = pair(0xacb4cfe5, 0x7b85a7e5);
    /// `checkbox/select/background` (and `/border`, `/focus-ring`): a
    /// checked box, `Cyan/500` in Dark, `Pink/500` in Light.
    pub const CHECKBOX_SELECT: Pair = pair(0x31ade9ff, 0xf21478ff);
    /// `checkbox/select/focus-glow`: `#12adff` at 20% in Dark, `Pink/400`
    /// at 20% in Light.
    pub const CHECKBOX_SELECT_GLOW: Pair = pair(0x12adff33, 0xff66ab33);
    /// `checkbox/default/border`: an unchecked box's edge, `Slate/400` at
    /// 20% in Dark (`stroke/medium`), 50% in Light (`stroke/dark`). Lighter
    /// than a textbox's edge in Dark.
    pub const CHECKBOX_BORDER: Pair = pair(0xacb4cf33, 0xacb4cf80);
    /// `icon/select`: the selected nav icon, the accent.
    pub const ICON_SELECT: Pair = pair(0x62cdf6ff, 0xf21478ff);
    /// `icon/cyan`, `icon/green`, `icon/yellow`, `icon/purple`.
    pub const ICON_CYAN: Pair = pair(0x62cdf6ff, 0x31ade9ff);
    /// `progress/default`: `icon/cyan` in Dark, `icon/red` (`Pink/500`) in
    /// Light. (`progress/transparent` is the same at 0%.)
    pub const PROGRESS: Pair = pair(0x62cdf6ff, 0xf21478ff);
    pub const ICON_GREEN: Pair = pair(0x31d924ff, 0x5ac351ff);
    pub const ICON_YELLOW: Pair = pair(0xffdd64ff, 0xffca0dff);
    pub const ICON_PURPLE: Pair = pair(0xe374ffff, 0xb247e7ff);
    /// `button/default/icon`: the Default button's icon, `Slate/400` in
    /// Dark, `Slate/500` in Light, both opaque. Its own token, louder than
    /// `icon/primary` (90%) and `icon/secondary` (60%).
    pub const BUTTON_ICON: Pair = pair(0xacb4cfff, 0x7b85a7ff);
    /// `text/green`: success text, the Zen pill's label.
    pub const TEXT_GREEN: Pair = pair(0x31d924ff, 0x00753dff);
    /// `text/link` (`text/cyan` is the same pair).
    pub const TEXT_LINK: Pair = pair(0x30c6ffff, 0x1989c4ff);
    /// `other/green`: green output in the terminal, the Machines states.
    pub const THEME_GREEN: Pair = pair(0x5acd25ff, 0x367b16ff);
    /// `other/cyan`: cyan output in the terminal.
    pub const THEME_CYAN: Pair = pair(0x00a6b2ff, 0x007a83ff);
    /// `other/red` (and `text/ansi_red`): red output in the terminal.
    pub const THEME_RED: Pair = pair(0xe04f52ff, 0xc23b3eff);
    /// `text/ansi_*`: the Activity log's colors (status tags, actors,
    /// timestamps), and the light terminal's ANSI slots. (The dark
    /// terminal's sixteen are `04 Terminal`'s, `theme::SENSIBLE`.)
    pub const ANSI_GREEN: Pair = pair(0x5acd25ff, 0x1a7f37ff);
    pub const ANSI_YELLOW: Pair = pair(0xdbc300ff, 0x8a5f04ff);
    pub const ANSI_BLUE: Pair = pair(0x5893d6ff, 0x2563c9ff);
    pub const ANSI_MAGENTA: Pair = pair(0xd259d7ff, 0xa21cafff);
    pub const ANSI_CYAN: Pair = pair(0x00a6b2ff, 0x0f766eff);
    pub const ANSI_DARK_GRAY: Pair = pair(0x666666ff, 0x6b6b6bff);
    /// `focus/default/ring`: `Cyan/400` in Dark, `Cyan/500` in Light.
    pub const FOCUS_RING: Pair = pair(0x62cdf6ff, 0x31ade9ff);
    /// `focus/default/glow`: `#12adff` at 20%. (The Button component's
    /// Default and Primary focus variants still bind the older
    /// `focus/default/focus-glow`, `VariableID:2220:20442`, which resolves
    /// to the same value.)
    pub const FOCUS_GLOW: Pair = pair(0x12adff33, 0x12adff33);
    /// `focus/success/ring`: `Green/500` in both modes, the Success
    /// button's focus ring.
    pub const SUCCESS_FOCUS_RING: Pair = pair(0x5ac351ff, 0x5ac351ff);
    /// `focus/success/glow`: `Green/400` at 20% in Dark, 15% in Light.
    pub const SUCCESS_FOCUS_GLOW: Pair = pair(0x86d58033, 0x86d58026);
    /// `focus/danger/ring`: `Pink/500` in both modes, the Danger button's
    /// focus ring.
    pub const DANGER_FOCUS_RING: Pair = pair(0xf21478ff, 0xf21478ff);
    /// `focus/danger/glow`: `Pink/400` at 25% in Dark, 10% in Light.
    pub const DANGER_FOCUS_GLOW: Pair = pair(0xff66ab40, 0xff66ab1a);
    /// `button/primary/background`: the Button's Primary style (a dialog's
    /// confirming action), `Cyan/400` at 18% / 12%, the Select style's
    /// values under their own names.
    pub const PRIMARY_FILL: Pair = pair(0x62cdf62e, 0x62cdf61f);
    /// `button/primary/background-hover`: 28% / 22%.
    pub const PRIMARY_FILL_HOVER: Pair = pair(0x62cdf647, 0x62cdf638);
    /// `button/primary/background-active`: 40% / 32%. (The component's
    /// Active variant binds the rest fill; the variable is the press.)
    pub const PRIMARY_FILL_ACTIVE: Pair = pair(0x62cdf666, 0x62cdf652);
    /// `button/primary/text` and `button/primary/icon`: `Cyan/300` in Dark,
    /// `Cyan/600` in Light.
    pub const PRIMARY_TEXT: Pair = pair(0xb0e6fbff, 0x1989c4ff);
    /// `button/success/background` (and `surface/green`): `Green/500` at
    /// 18% / 12%.
    pub const SUCCESS_FILL: Pair = pair(0x5ac3512e, 0x5ac3511f);
    /// `button/success/background-hover`: 28% / 22%.
    pub const SUCCESS_FILL_HOVER: Pair = pair(0x5ac35147, 0x5ac35138);
    /// `button/success/background-active`: 40% / 32%.
    pub const SUCCESS_FILL_ACTIVE: Pair = pair(0x5ac35166, 0x5ac35152);
    /// `button/success/text` (and `/icon`): `Green/400` in Dark, `Green/700`
    /// in Light.
    pub const SUCCESS_LABEL: Pair = pair(0x86d580ff, 0x00753dff);
    /// `button/danger/background`: `Pink/500` at 18% / 12%.
    pub const DANGER_FILL: Pair = pair(0xf214782e, 0xf214781f);
    /// `button/danger/background-hover`: 28% / 22%.
    pub const DANGER_FILL_HOVER: Pair = pair(0xf2147847, 0xf2147838);
    /// `button/danger/background-active`: 40% / 32%.
    pub const DANGER_FILL_ACTIVE: Pair = pair(0xf2147866, 0xf2147852);
    /// `button/danger/text` (and `/icon`): `Pink/400` in Dark, `Pink/700`
    /// in Light.
    pub const DANGER_TEXT: Pair = pair(0xff66abff, 0xb40041ff);
    /// `surface/overlay`: the backdrop behind the palette and dialogs,
    /// `Black/950` at 35% in Dark, `Slate/600` at 65% in Light.
    pub const SCRIM: Pair = pair(0x02030359, 0x444c65a6);
    /// `shadow/raised card`'s colors (an effect style): the three drops,
    /// and the 1px ring.
    pub const SHADOW: Pair = pair(0x0a1e5105, 0x0a1e5105);
    pub const SHADOW_RING: Pair = pair(0x0a1e5108, 0x0a1e5108);
    /// The Command palette's drop (an effect, `0 10 15 -3`, not a
    /// variable): black at 25% in both modes.
    pub const PALETTE_SHADOW: Pair = pair(0x00000040, 0x00000040);

    /// Colors with no `03 Desktop` variable, measured from the 1x frames
    /// (1 px = 1 pt) or kept from a frame's binding to a variable outside
    /// the collection, and named for where they appear.
    pub mod measured {
        use super::{pair, Pair};

        /// The Navigation component's fill: `Slate/400` at 10% over the
        /// dark canvas, at about 7% over white.
        pub const SIDEBAR: Pair = pair(0x2c3140ff, 0xf9f9fcff);
        /// The command palette's surface: the sidebar's tint.
        pub const RAISED: Pair = pair(0x2c3140ff, 0xf9f9fcff);
        /// The dark Issue detail's 1px edge (`stroke/light`'s value), the
        /// light one the raised-card ring.
        pub const RING: Pair = pair(0xacb4cf1a, 0x0a1e5108);
        /// `#7b85a7` (`Slate/500`), opaque in both modes: the Activity and
        /// Machines frames' titles, actor, runner and task. Those frames
        /// bind a `text/gray` that isn't in `03 Desktop`, so the value is
        /// the frames' own.
        pub const LOG_GRAY: Pair = pair(0x7b85a7ff, 0x7b85a7ff);
        /// A card's workflow badge and its label. The frames bind
        /// `other/workflow_badge_*`, which isn't in `03 Desktop`; the values
        /// are the frames' own.
        pub const WORKFLOW_BADGE_BG: Pair = pair(0x3a4258ff, 0xd3d8e4ff);
        pub const WORKFLOW_BADGE_FG: Pair = pair(0xdcdfe8ff, 0x2a3142ff);
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

    /// This color at another's alpha: a hue at
    /// `button/default/background`'s opacity is its chip tint.
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
    /// background (`surface/background`).
    canvas,
    /// The leading column: the sidebar, the review and workspace panels
    /// (the Navigation component's tint).
    sidebar,
    /// A board card: near black on the dark canvas, white in Light
    /// (`surface/depth-0`).
    card,
    /// A surface lifted over the window: the command palette, menus,
    /// popovers, tooltips.
    raised,
    /// A command box's well (the bootstrap screen: `surface/background`,
    /// inside a divider).
    well,
    /// The board's toolbar band over the canvas (the Issues Header's
    /// `surface/depth-10`).
    toolbar,
    /// The Textbox component's well, and an unchecked box's fill
    /// (`surface/depth-0`, `checkbox/default/background`).
    textbox,

    // ---- Overlay fills (the `button/default/background` family) ------------
    /// The overlay fill at rest: buttons, dropdowns, chips, count badges,
    /// the search box, a hovered row (`button/default/background`,
    /// `surface/hover`).
    background,
    /// The overlay fill hovered (`button/default/background-hover`).
    background_hover,
    /// The overlay fill pressed, or a control while its menu is open
    /// (`button/default/background-active`).
    background_active,
    /// A set filter's chip: the Button's Select style at rest
    /// (`button/select/background`).
    select_fill,
    /// The Select style hovered (`button/select/background-hover`).
    select_fill_hover,
    /// The Select style pressed, or while its menu is open
    /// (`button/select/background-active`).
    select_fill_active,
    /// The Select style's label (`button/select/text`).
    select_label,
    /// The selected segment of a segmented control (`surface/selection`).
    selection,
    /// The Activity filter chips' fill, the segmented track
    /// (`surface/depth-30`).
    search,
    /// A keycap in a shortcut hint (the Button component's Hotkey:
    /// `surface/background`).
    keycap,
    /// A keycap's 1px bottom edge inside a button (the Issue detail's
    /// action buttons, `shadow/hard`).
    button_keycap_edge,

    // ---- Text ---------------------------------------------------------------
    /// Headings, titles, the selected row's label (`text/bold`).
    text_loud,
    /// Body text and labels (`text/foreground`).
    foreground,
    /// Secondary text: hotkeys, metadata (`icon/primary`'s color, as the
    /// frames draw it).
    gray,
    /// A form control's text: a textbox's value, an unselected segment or
    /// checkbox (`text/foreground`, kept a role of its own).
    form_text,
    /// A form's quiet text: a labeled control's helper, a textbox's
    /// placeholder, the board toolbar's `Filter:` and its search's
    /// placeholder (`text/muted`, kept a role of its own).
    form_hint,
    /// Quiet text: section headers, branches, timestamps, hints, idle
    /// states, placeholders (`text/muted`).
    muted,
    /// Text on a saturated fill (a project badge's initials,
    /// `other/on_color`).
    text_on_color,
    /// The Activity log's titles and its `you` actor (the Activity frame's
    /// `#7b85a7`).
    log_text,
    /// A Machines workspace row's runner and task (the Machines frame's
    /// `#7b85a7`).
    table_text,

    // ---- Icons --------------------------------------------------------------
    /// Icons at rest (`icon/primary`).
    icon,
    /// A button's icon (`button/default/icon`: opaque, a step louder than
    /// `icon`).
    button_icon,
    /// The selected nav icon: cyan in Dark, pink in Light (`icon/select`).
    accent,
    /// A ready-for-review check (`icon/cyan`).
    icon_cyan,
    /// A working workspace (`icon/green`).
    icon_green,
    /// A workspace awaiting input (`icon/yellow`).
    icon_yellow,
    /// A review status glyph (`icon/purple`).
    icon_purple,
    /// A running step's spinner, fading out along its tail
    /// (`progress/default`).
    progress,

    // ---- Rules --------------------------------------------------------------
    /// Dividers and hairline borders (`stroke/light`).
    divider,
    /// The board's stronger rules (`stroke/medium`).
    divider_strong,
    /// A textbox's 1px edge, and the other form fields' (`stroke/dark`).
    input_border,
    /// An unchecked box's 1px edge (`checkbox/default/border`): lighter
    /// than a textbox's in Dark.
    check_border,

    // ---- Links and focus ----------------------------------------------------
    /// A link, the "More" action (`text/link`).
    link,
    /// A hovered link (`text/bold`).
    link_hover,
    /// The keyboard-focus ring (`focus/default/ring`).
    focus_ring,
    /// The glow outside the ring (`focus/default/glow`).
    focus_glow,
    /// A focused Success button's ring (`focus/success/ring`).
    success_focus_ring,
    /// A focused Success button's glow (`focus/success/glow`).
    success_focus_glow,
    /// A focused Danger button's ring (`focus/danger/ring`).
    danger_focus_ring,
    /// A focused Danger button's glow (`focus/danger/glow`).
    danger_focus_glow,
    /// A checked box (`checkbox/select/background`), and a focused one's
    /// ring.
    check_fill,
    /// A focused checked box's glow (`checkbox/select/focus-glow`).
    check_glow,

    // ---- Meaning ------------------------------------------------------------
    /// Success, done, approve (`text/green`).
    success,
    /// Warnings, in progress (`text/ansi_yellow`).
    warning,
    /// Ready, informational (`text/ansi_blue`).
    info,
    /// Errors, blocked, reject (`button/danger/text`).
    error,

    // ---- Log and terminal hues (Activity's tags, ANSI slots) ---------------
    /// Green output in the terminal (`other/green`).
    theme_green,
    /// Cyan output in the terminal (`other/cyan`).
    theme_cyan,
    /// Red output in the terminal (`other/red`).
    theme_red,
    /// `[done]` (`text/ansi_green`).
    green,
    /// `[in progress]` (`text/ansi_yellow`).
    yellow,
    /// `[ready]`, queued work (`text/ansi_blue`).
    blue,
    /// `[review]`, handoff (`text/ansi_magenta`).
    magenta,
    /// A working actor in the log (`text/ansi_cyan`).
    cyan,
    /// Timestamps in the log (`text/ansi_dark_gray`).
    dark_gray,
    /// Errors and failures as text (`button/danger/text`).
    red,

    // ---- Chip tints: each hue at `button/default/background`'s opacity ----
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
    /// A workflow name's badge on a card (the frames' own).
    workflow_badge,
    /// The workflow badge's label (the frames' own).
    workflow_badge_text,

    // ---- Buttons (the Button component's Primary, Success, Danger styles) --
    /// The Primary button: a dialog's confirming action
    /// (`button/primary/background`).
    primary_fill,
    /// The Primary button hovered (`button/primary/background-hover`).
    primary_fill_hover,
    /// The Primary button pressed (`button/primary/background-active`).
    primary_fill_active,
    /// The Primary button's label and icon (`button/primary/text`).
    primary_label,
    /// The Success button, and the Zen pill (`button/success/background`).
    success_fill,
    /// The Success button hovered (`button/success/background-hover`).
    success_fill_hover,
    /// The Success button pressed (`button/success/background-active`).
    success_fill_active,
    /// The Success button's label and icon (`button/success/text`).
    success_label,
    /// The Danger button, and the unread-errors button
    /// (`button/danger/background`).
    danger_fill,
    /// The Danger button hovered (`button/danger/background-hover`).
    danger_fill_hover,
    /// The Danger button pressed (`button/danger/background-active`).
    danger_fill_active,

    // ---- Bands --------------------------------------------------------------
    /// The `Zen mode on` pill (with [`Palette::success`] text):
    /// `surface/green`.
    zen,
    /// A soft warning band (a status or a notice over the main area):
    /// `icon/yellow` at `button/default/background`'s opacity.
    notice,

    // ---- Selection ----------------------------------------------------------
    /// The selected nav row: `surface/background`, lifted with the
    /// raised-card shadow.
    nav_selected,
    /// A selected or keyboard-current row in a list (Activity, the palette):
    /// `surface/hover` in Dark, `surface/selection` in Light, as drawn.
    row_selected,
    /// Selected text (the terminal's selection highlight,
    /// `surface/selection`).
    text_selection,
    /// A search match's highlight behind text (`surface/depth-30`).
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
    /// The backdrop behind a modal overlay (`surface/overlay`).
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
            card: pick(DEPTH_0),
            raised: pick(RAISED),
            well: canvas,
            toolbar: pick(DEPTH_10),
            textbox: pick(DEPTH_0),

            background,
            background_hover: pick(BACKGROUND_HOVER),
            background_active: pick(BACKGROUND_ACTIVE),
            select_fill: pick(SELECT_FILL),
            select_fill_hover: pick(SELECT_FILL_HOVER),
            select_fill_active: pick(SELECT_FILL_ACTIVE),
            select_label: pick(SELECT_TEXT),
            selection: pick(SELECTION),
            search: pick(SEARCH),
            keycap: canvas,
            button_keycap_edge: pick(SEARCH_DETAIL),

            text_loud: pick(WHITE),
            foreground: pick(FOREGROUND),
            gray: pick(GRAY),
            form_text: pick(FOREGROUND),
            form_hint: pick(MUTED),
            muted: pick(MUTED),
            text_on_color: Rgba::hex(0xffffff),
            log_text: pick(LOG_GRAY),
            table_text: pick(LOG_GRAY),

            icon: pick(GRAY),
            button_icon: pick(BUTTON_ICON),
            accent: pick(ICON_SELECT),
            icon_cyan: pick(ICON_CYAN),
            icon_green: pick(ICON_GREEN),
            icon_yellow: pick(ICON_YELLOW),
            icon_purple: pick(ICON_PURPLE),
            progress: pick(PROGRESS),

            divider: pick(DIVIDER),
            divider_strong: pick(DIVIDER_STRONG),
            input_border: pick(DIVIDER_FORM),
            check_border: pick(CHECKBOX_BORDER),

            link: pick(TEXT_LINK),
            link_hover: pick(WHITE),
            focus_ring: pick(FOCUS_RING),
            focus_glow: pick(FOCUS_GLOW),
            success_focus_ring: pick(SUCCESS_FOCUS_RING),
            success_focus_glow: pick(SUCCESS_FOCUS_GLOW),
            danger_focus_ring: pick(DANGER_FOCUS_RING),
            danger_focus_glow: pick(DANGER_FOCUS_GLOW),
            check_fill: pick(CHECKBOX_SELECT),
            check_glow: pick(CHECKBOX_SELECT_GLOW),

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

            primary_fill: pick(PRIMARY_FILL),
            primary_fill_hover: pick(PRIMARY_FILL_HOVER),
            primary_fill_active: pick(PRIMARY_FILL_ACTIVE),
            primary_label: pick(PRIMARY_TEXT),
            success_fill: pick(SUCCESS_FILL),
            success_fill_hover: pick(SUCCESS_FILL_HOVER),
            success_fill_active: pick(SUCCESS_FILL_ACTIVE),
            success_label: pick(SUCCESS_LABEL),
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
    ("dark", "magenta on"),
    ("dark", "cyan on"),
    ("dark", "red on red_tint"),
    ("dark", "text_loud on selection"),
    ("dark", "theme_green on selection"),
    ("dark", "theme_cyan on selection"),
    ("dark", "success_label on hover button"),
    ("dark", "success_label on pressed button"),
    ("dark", "error on rest button"),
    ("dark", "error on hover button"),
    ("dark", "error on pressed button"),
    ("dark", "input_border on"),
    ("dark", "text_on_color on check_fill"),
    ("dark", "check_border on"),
    ("dark", "table_text on"),
    ("dark", "form_hint on search box"),
    ("dark", "select_label on"),
    ("dark", "primary_label on"),
    ("light", "muted on"),
    ("light", "gray on"),
    ("light", "log_text on"),
    ("light", "icon on"),
    ("light", "link on"),
    ("light", "green on green_tint"),
    ("light", "success_label on pressed button"),
    ("light", "error on pressed button"),
    ("light", "focus_ring on"),
    ("light", "success_focus_ring on"),
    ("light", "dark_gray on selection"),
    ("light", "theme_green on selection"),
    ("light", "theme_cyan on selection"),
    ("light", "input_border on"),
    ("light", "form_hint on"),
    ("light", "check_border on"),
    ("light", "table_text on"),
    ("light", "select_label on"),
    ("light", "primary_label on"),
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
        // A Machines row's runner and task, on the canvas (the selected
        // row's band is checked below).
        check("table_text on canvas".into(), p.table_text, p.canvas, AA);
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
                    "primary_label",
                    p.primary_label,
                    [p.primary_fill, p.primary_fill_hover, p.primary_fill_active],
                ),
                (
                    "success_label",
                    p.success_label,
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
            check(
                format!("button_icon on rest button over {sname}"),
                p.button_icon,
                flatten(p.background, s),
                NON_TEXT,
            );
            check(format!("focus_ring on {sname}"), p.focus_ring, s, NON_TEXT);
            check(format!("success_focus_ring on {sname}"), p.success_focus_ring, s, NON_TEXT);
            check(format!("danger_focus_ring on {sname}"), p.danger_focus_ring, s, NON_TEXT);
            check(format!("input_border on {sname}"), p.input_border, s, NON_TEXT);
        }
        // The board toolbar: `Filter:` and the search's placeholder on its
        // band, a chip's label and icon on each fill over it.
        let band = flatten(p.toolbar, p.canvas);
        check("form_hint on toolbar".into(), p.form_hint, band, AA);
        check("form_hint on search box over toolbar".into(), p.form_hint, flatten(p.background, band), AA);
        check("icon on search box over toolbar".into(), p.icon, flatten(p.background, band), NON_TEXT);
        check("button_icon on rest button over toolbar".into(), p.button_icon, flatten(p.background, band), NON_TEXT);
        for (state, fill) in ["rest", "hover", "pressed"]
            .into_iter()
            .zip([p.background, p.background_hover, p.background_active])
        {
            check(format!("text_loud on {state} button over toolbar"), p.text_loud, flatten(fill, band), AA);
        }
        for (state, fill) in ["rest", "hover", "pressed"]
            .into_iter()
            .zip([p.select_fill, p.select_fill_hover, p.select_fill_active])
        {
            check(format!("select_label on {state} select over toolbar"), p.select_label, flatten(fill, band), AA);
        }
        // The form components, on the canvas they sit on: a textbox's value
        // in its well, an unselected segment on the track, a checkbox's
        // label, a helper and a placeholder.
        let track = flatten(p.search, p.canvas);
        check("form_text on textbox".into(), p.form_text, p.textbox, AA);
        check("form_text on segment track".into(), p.form_text, track, AA);
        check("form_text on canvas".into(), p.form_text, p.canvas, AA);
        check("text_loud on selected segment".into(), p.text_loud, p.canvas, AA);
        check("form_hint on canvas".into(), p.form_hint, p.canvas, AA);
        check("form_hint on textbox".into(), p.form_hint, p.textbox, AA);
        // Non-text: a textbox's and a box's edge, a checked box and its mark.
        check("input_border on textbox".into(), p.input_border, p.textbox, NON_TEXT);
        check("check_border on textbox".into(), p.check_border, p.textbox, NON_TEXT);
        check("check_fill on canvas".into(), p.check_fill, p.canvas, NON_TEXT);
        check("text_on_color on check_fill".into(), p.text_on_color, p.check_fill, NON_TEXT);
        // The selected Machines row: every cell on the selection band.
        let band = flatten(p.selection, p.canvas);
        for (name, fg) in [
            ("table_text", p.table_text),
            ("muted", p.muted),
            ("dark_gray", p.dark_gray),
            ("theme_green", p.theme_green),
            ("theme_cyan", p.theme_cyan),
        ] {
            check(format!("{name} on selection over canvas"), fg, band, AA);
        }
        // The Zen pill paints the canvas under its tint (the Navigation
        // component's measured fill), so "Zen mode on" reads on that.
        check(
            "success on zen over canvas".into(),
            p.success,
            flatten(p.zen, p.canvas),
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
        // `text/foreground`, `text/bold`, `text/muted`: the shared tokens,
        // no longer a frame's sampled override.
        both(d.foreground, l.foreground, (0xf2f3f7d9, 0x020303b2));
        both(d.text_loud, l.text_loud, (0xf2f3f7ff, 0x020303ff));
        both(d.link_hover, l.link_hover, (0xf2f3f7ff, 0x020303ff));
        both(d.gray, l.gray, (0xacb4cfe5, 0x7b85a7e5));
        both(d.icon, l.icon, (0xacb4cfe5, 0x7b85a7e5));
        both(d.muted, l.muted, (0xf2f3f78c, 0x444c6580));
        // `button/default/icon`: opaque in both modes, its own token, not
        // `icon/primary` (90%) or `icon/secondary` (60%).
        both(d.button_icon, l.button_icon, (0xacb4cfff, 0x7b85a7ff));
        // The Activity log's `text/ansi_*` (Dark changed with the design
        // system; Light as before).
        both(d.green, l.green, (0x5acd25ff, 0x1a7f37ff));
        both(d.yellow, l.yellow, (0xdbc300ff, 0x8a5f04ff));
        both(d.blue, l.blue, (0x5893d6ff, 0x2563c9ff));
        both(d.magenta, l.magenta, (0xd259d7ff, 0xa21cafff));
        both(d.cyan, l.cyan, (0x00a6b2ff, 0x0f766eff));
        both(d.dark_gray, l.dark_gray, (0x666666ff, 0x6b6b6bff));
        both(d.warning, l.warning, (0xdbc300ff, 0x8a5f04ff));
        both(d.info, l.info, (0x5893d6ff, 0x2563c9ff));
        // The Machines frame's `text/gray`, opaque in both modes.
        both(d.table_text, l.table_text, (0x7b85a7ff, 0x7b85a7ff));
        both(d.accent, l.accent, (0x62cdf6ff, 0xf21478ff));
        both(d.progress, l.progress, (0x62cdf6ff, 0xf21478ff));
        both(d.focus_ring, l.focus_ring, (0x62cdf6ff, 0x31ade9ff));
        both(d.focus_glow, l.focus_glow, (0x12adff33, 0x12adff33));
        // The Success and Danger buttons' own focus (`focus/success/*`,
        // `focus/danger/*`).
        both(d.success_focus_ring, l.success_focus_ring, (0x5ac351ff, 0x5ac351ff));
        both(d.success_focus_glow, l.success_focus_glow, (0x86d58033, 0x86d58026));
        both(d.danger_focus_ring, l.danger_focus_ring, (0xf21478ff, 0xf21478ff));
        both(d.danger_focus_glow, l.danger_focus_glow, (0xff66ab40, 0xff66ab1a));
        // The form components (the dark Settings frame, the components page).
        both(d.form_text, l.form_text, (0xf2f3f7d9, 0x020303b2));
        both(d.form_hint, l.form_hint, (0xf2f3f78c, 0x444c6580));
        // The Button's Select style and the Issues Header's band.
        both(d.select_fill, l.select_fill, (0x62cdf62e, 0x62cdf61f));
        both(d.select_label, l.select_label, (0xb0e6fbff, 0x1989c4ff));
        // The Button's Primary style (a dialog's confirming action).
        both(d.primary_fill, l.primary_fill, (0x62cdf62e, 0x62cdf61f));
        both(d.primary_fill_hover, l.primary_fill_hover, (0x62cdf647, 0x62cdf638));
        both(d.primary_fill_active, l.primary_fill_active, (0x62cdf666, 0x62cdf652));
        both(d.primary_label, l.primary_label, (0xb0e6fbff, 0x1989c4ff));
        both(d.background_active, l.background_active, (0xacb4cf66, 0xacb4cf52));
        both(d.toolbar, l.toolbar, (0xacb4cf1a, 0xacb4cf14));
        // `stroke/dark` for a textbox, `checkbox/default/border` for an
        // unchecked box: the same in Light, apart in Dark.
        both(d.input_border, l.input_border, (0xacb4cf4d, 0xacb4cf80));
        both(d.check_border, l.check_border, (0xacb4cf33, 0xacb4cf80));
        assert_ne!(d.check_border, d.input_border);
        both(d.textbox, l.textbox, (0x020303ff, 0xffffffff));
        both(d.check_fill, l.check_fill, (0x31ade9ff, 0xf21478ff));
        both(d.check_glow, l.check_glow, (0x12adff33, 0xff66ab33));
        // The Button's styles stay apart even where values coincide:
        // Default (slate), Primary and Select (cyan), Success (green),
        // Danger (pink), each at 18 / 28 / 40% in Dark, 12 / 22 / 32% in
        // Light.
        both(d.background_hover, l.background_hover, (0xacb4cf47, 0xacb4cf38));
        both(d.select_fill_hover, l.select_fill_hover, (0x62cdf647, 0x62cdf638));
        both(d.select_fill_active, l.select_fill_active, (0x62cdf666, 0x62cdf652));
        both(d.success_fill, l.success_fill, (0x5ac3512e, 0x5ac3511f));
        both(d.success_fill_hover, l.success_fill_hover, (0x5ac35147, 0x5ac35138));
        both(d.success_fill_active, l.success_fill_active, (0x5ac35166, 0x5ac35152));
        both(d.success_label, l.success_label, (0x86d580ff, 0x00753dff));
        both(d.danger_fill, l.danger_fill, (0xf214782e, 0xf214781f));
        both(d.danger_fill_hover, l.danger_fill_hover, (0xf2147847, 0xf2147838));
        both(d.danger_fill_active, l.danger_fill_active, (0xf2147866, 0xf2147852));
        both(d.error, l.error, (0xff66abff, 0xb40041ff));
        both(d.zen, l.zen, (0x5ac3512e, 0x5ac3511f));
        // `surface/overlay`.
        both(d.scrim, l.scrim, (0x02030359, 0x444c65a6));
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

    /// Every token Figma defines as a hue at a percentage carries Figma's
    /// own byte for it: the percentage as Figma's 32-bit float, times 255,
    /// rounded (70% is `b2`, 90% `e5`, 50% `80`).
    #[test]
    fn token_alphas_are_figmas_opacities() {
        fn byte(percent: u8) -> u8 {
            (f64::from(f32::from(percent) / 100.0) * 255.0).round() as u8
        }
        use figma::*;
        let (slate100, black, slate400, slate500, slate600) = (0xf2f3f7, 0x020303, 0xacb4cf, 0x7b85a7, 0x444c65);
        let (cyan400, green500, pink500, pink400, glow) = (0x62cdf6, 0x5ac351, 0xf21478, 0xff66ab, 0x12adff);
        let green400 = 0x86d580;
        /// A hue and the percentage Figma sets it at.
        type At = (u32, u8);
        // (token, Dark, Light)
        let tokens: &[(&str, (Rgba, Rgba), At, At)] = &[
            ("text/foreground", FOREGROUND, (slate100, 85), (black, 70)),
            ("text/bold", WHITE, (slate100, 100), (black, 100)),
            ("text/muted", MUTED, (slate100, 55), (slate600, 50)),
            ("icon/primary", GRAY, (slate400, 90), (slate500, 90)),
            ("button/default/icon", BUTTON_ICON, (slate400, 100), (slate500, 100)),
            ("button/default/background", BACKGROUND, (slate400, 18), (slate400, 12)),
            ("button/default/background-hover", BACKGROUND_HOVER, (slate400, 28), (slate400, 22)),
            ("button/default/background-active", BACKGROUND_ACTIVE, (slate400, 40), (slate400, 32)),
            ("button/primary/background", PRIMARY_FILL, (cyan400, 18), (cyan400, 12)),
            ("button/primary/background-hover", PRIMARY_FILL_HOVER, (cyan400, 28), (cyan400, 22)),
            ("button/primary/background-active", PRIMARY_FILL_ACTIVE, (cyan400, 40), (cyan400, 32)),
            ("button/select/background", SELECT_FILL, (cyan400, 18), (cyan400, 12)),
            ("button/select/background-hover", SELECT_FILL_HOVER, (cyan400, 28), (cyan400, 22)),
            ("button/select/background-active", SELECT_FILL_ACTIVE, (cyan400, 40), (cyan400, 32)),
            ("button/success/background", SUCCESS_FILL, (green500, 18), (green500, 12)),
            ("button/success/background-hover", SUCCESS_FILL_HOVER, (green500, 28), (green500, 22)),
            ("button/success/background-active", SUCCESS_FILL_ACTIVE, (green500, 40), (green500, 32)),
            ("button/danger/background", DANGER_FILL, (pink500, 18), (pink500, 12)),
            ("button/danger/background-hover", DANGER_FILL_HOVER, (pink500, 28), (pink500, 22)),
            ("button/danger/background-active", DANGER_FILL_ACTIVE, (pink500, 40), (pink500, 32)),
            ("surface/depth-10", DEPTH_10, (slate400, 10), (slate400, 8)),
            ("surface/depth-30", SEARCH, (slate400, 25), (slate400, 16)),
            ("surface/selection", SELECTION, (slate400, 55), (slate400, 40)),
            ("surface/overlay", SCRIM, (black, 35), (slate600, 65)),
            ("stroke/light", DIVIDER, (slate400, 10), (slate400, 10)),
            ("stroke/medium", DIVIDER_STRONG, (slate400, 20), (slate400, 20)),
            ("stroke/dark", DIVIDER_FORM, (slate400, 30), (slate400, 50)),
            ("shadow/hard", SEARCH_DETAIL, (black, 55), (slate400, 60)),
            ("checkbox/default/border", CHECKBOX_BORDER, (slate400, 20), (slate400, 50)),
            ("checkbox/select/focus-glow", CHECKBOX_SELECT_GLOW, (glow, 20), (pink400, 20)),
            ("focus/default/glow", FOCUS_GLOW, (glow, 20), (glow, 20)),
            ("focus/success/ring", SUCCESS_FOCUS_RING, (green500, 100), (green500, 100)),
            ("focus/success/glow", SUCCESS_FOCUS_GLOW, (green400, 20), (green400, 15)),
            ("focus/danger/ring", DANGER_FOCUS_RING, (pink500, 100), (pink500, 100)),
            ("focus/danger/glow", DANGER_FOCUS_GLOW, (pink400, 25), (pink400, 10)),
        ];
        for (name, (dark, light), (dh, dp), (lh, lp)) in tokens {
            assert_eq!((dark.rgb, dark.a), (Rgb::hex(*dh), byte(*dp)), "{name} Dark");
            assert_eq!((light.rgb, light.a), (Rgb::hex(*lh), byte(*lp)), "{name} Light");
        }
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
