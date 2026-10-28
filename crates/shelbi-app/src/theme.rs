//! The named color tokens both clients paint with.
//!
//! One [`Theme`] holds every color the TUI and the desktop app use, one field
//! per token, with a dark and a light value. The TUI converts it to ratatui
//! colors (`crates/shelbi-tui/src/theme.rs`) and the desktop to gpui colors
//! (`desktop/src/theme.rs`); both read the same field names, so the two
//! clients read as one product and a token is found in either by one name.
//!
//! # Token names
//!
//! The brand tokens carry their Figma names (`color/background`,
//! `color/gray`, `color/white`, `color/muted`, `color/cyan`, ...). In the light
//! theme a name keeps its *role*, not its hue: `white` is the strongest text,
//! which is near-black on a light background, and `gray` is normal text.
//!
//! The `ansi_*` tokens are the terminal hues the TUI's views use for status,
//! categories, and identity (a workspace's agent color, a task's status
//! category, a warning). In the dark theme they are the terminal's own palette
//! slots, so the TUI paints exactly what it always has and follows the user's
//! terminal colors; their RGB values are what a client without a terminal
//! palette (the desktop) paints, taken from the site's TUI mockups
//! (`site/app/globals.css`, `--tui-*`). In the light theme every token is an
//! explicit RGB, so the TUI doesn't depend on how a light terminal remaps its
//! palette.
//!
//! # Strict monochrome
//!
//! The light theme follows shelbi.dev's light mode: a near-white background,
//! near-black text, a gray ramp, and the same cyan accent and green busy color
//! darkened until text in them clears 4.5:1 against the background. The tests
//! check that ratio for every text token in both themes.
//!
//! No UI toolkit types here (`tests/no_ui_deps.rs`).

pub use shelbi_state::ThemePreference;

/// An sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// A `0xRRGGBB` literal.
    pub const fn hex(value: u32) -> Self {
        Self {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }

    /// WCAG relative luminance, 0.0 (black) to 1.0 (white).
    pub fn relative_luminance(self) -> f64 {
        fn channel(c: u8) -> f64 {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }

    /// WCAG contrast ratio between two colors, 1.0 to 21.0. Order-independent.
    pub fn contrast_ratio(self, other: Rgb) -> f64 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Per-channel linear blend: `t = 0.0` is `self`, `t = 1.0` is `to`.
    pub fn lerp(self, to: Rgb, t: f32) -> Rgb {
        let mix = |a: u8, b: u8| {
            let (a, b) = (f32::from(a), f32::from(b));
            (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
        };
        Rgb::new(mix(self.r, to.r), mix(self.g, to.g), mix(self.b, to.b))
    }
}

/// One of a terminal's 16 named palette colors, in ratatui's naming (`Gray`
/// is ANSI 7, `DarkGray` ANSI 8, `White` ANSI 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ansi {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    DarkGray,
    LightRed,
    LightGreen,
    LightYellow,
    LightBlue,
    LightMagenta,
    LightCyan,
    White,
}

/// A color in the terminal's own palette, which a terminal client paints in
/// place of a token's RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TermColor {
    /// One of the 16 named colors.
    Ansi(Ansi),
    /// An xterm 256-color index.
    Indexed(u8),
}

/// A token's value: the exact color, plus (optionally) the terminal palette
/// slot a terminal client paints instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Swatch {
    /// What a client without a terminal palette paints, and what the TUI
    /// paints when [`Swatch::term`] is `None`.
    pub rgb: Rgb,
    /// The terminal palette slot the TUI paints instead of [`Swatch::rgb`],
    /// so the color follows the user's terminal theme.
    pub term: Option<TermColor>,
}

impl Swatch {
    /// An exact color, painted as-is everywhere.
    pub const fn rgb(hex: u32) -> Self {
        Self {
            rgb: Rgb::hex(hex),
            term: None,
        }
    }

    /// A terminal palette color (`ansi`) whose stand-in off the terminal is
    /// `hex`.
    pub const fn ansi(ansi: Ansi, hex: u32) -> Self {
        Self {
            rgb: Rgb::hex(hex),
            term: Some(TermColor::Ansi(ansi)),
        }
    }

    /// An xterm 256-color index whose stand-in off the terminal is `hex`.
    pub const fn indexed(index: u8, hex: u32) -> Self {
        Self {
            rgb: Rgb::hex(hex),
            term: Some(TermColor::Indexed(index)),
        }
    }
}

/// Which way a theme runs: light text on dark, or dark text on light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Appearance {
    Dark,
    Light,
}

/// Every color token, one field each. `C` is the color type: [`Swatch`] here,
/// a toolkit color after a client's [`Theme::map`] (`Theme<ratatui::Color>`
/// in the TUI, `Theme<gpui::Hsla>` in the desktop).
///
/// The doc on each field names the TUI constant(s) it replaced, so older
/// comments and commits stay findable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Theme<C = Swatch> {
    /// Which way this theme runs. Not a color.
    pub appearance: Appearance,

    // ---- Brand tokens (Figma names) ---------------------------------------
    /// `color/background` (was `BACKGROUND`): the app's base fill, the
    /// desktop sidebar's background. The TUI leaves its background to the
    /// terminal.
    pub background: C,
    /// The main area's fill. The TUI's main area is the terminal's default
    /// background; the desktop paints this (#1c1c1c is the Ghostty default
    /// the dark tokens were tuned against).
    pub canvas: C,
    /// `color/foreground` (was `PALETTE_FG`): primary text in the command
    /// palette and the task-description body.
    pub foreground: C,
    /// `color/gray` (was `TEXT`, `FG_SECONDARY`): normal sidebar text, the
    /// review and workspace panels' secondary text.
    pub gray: C,
    /// `color/white` (was `TEXT_SELECTED`): the strongest text, the selected
    /// nav label. Near-black in the light theme.
    pub white: C,
    /// `color/muted` (was `MUTED`, `PALETTE_MUTED`): section headers,
    /// secondary labels, palette descriptions and hints.
    pub muted: C,
    /// `color/cyan`, a.k.a. `color/accent` (was `ACCENT`, `ACCENT_CYAN`): the
    /// project name, the ready-for-review check, the review status header.
    pub cyan: C,
    /// `color/green` (was `BUSY_GREEN`, `PALETTE_GREEN`,
    /// `PROJECT_STATUS_GREEN`): a working workspace's badge, the palette
    /// cursor, a loaded project's ring, Approve.
    pub green: C,
    /// `color/red` (was `ACTION_RED`): Reject.
    pub red: C,
    /// `color/yellow` (was `STATUS_YELLOW`): the workspace "IN PROGRESS"
    /// label.
    pub yellow: C,
    /// `selection` (was `SELECTION_BG`): the selected / focused row fill
    /// everywhere.
    pub selection: C,
    /// `color/search` (was `SEARCH_BG`, `PALETTE_BG`): the sidebar search box
    /// and the command palette panel.
    pub search: C,
    /// `color/divider` (was `DIVIDER_DIM`): the sidebar's right-edge rule at
    /// rest.
    pub divider: C,
    /// `color/zen`: the `ZEN MODE ON` band behind bold [`Theme::on_color`]
    /// text.
    pub zen: C,

    // ---- Interface tokens -------------------------------------------------
    /// The sidebar divider while hovered or dragged (was `DIVIDER_ACTIVE`).
    pub divider_active: C,
    /// A kanban card's workflow-name chip (was `WORKFLOW_BADGE_BG`); a touch
    /// bluer than [`Theme::selection`] so it reads on a selected card.
    pub workflow_badge_bg: C,
    /// The workflow chip's text (was `WORKFLOW_BADGE_FG`).
    pub workflow_badge_fg: C,
    /// Text on a saturated fill: the alert band, the Zen band, the palette
    /// cursor, the unread-errors button, the Enable Zen button.
    pub on_color: C,
    /// A sidebar footer button under the pointer (Settings, Send Feedback):
    /// the desktop's `background_hover` tint laid over the canvas.
    pub button_hover: C,
    /// The sidebar's unread-errors button fill.
    pub error_button: C,
    /// The unread-errors button under the pointer.
    pub error_button_hover: C,
    /// The error log's unread `●` marker.
    pub error_marker: C,
    /// The one-time notice band (the keyboard-protocol notice).
    pub notice: C,
    /// The bottom status line's fill.
    pub status_bar: C,
    /// Inline code background in rendered markdown.
    pub code_bg: C,
    /// Inline code text in rendered markdown.
    pub code_fg: C,
    /// The active-project pulse at its dimmest (near-invisible on the palette
    /// panel).
    pub pulse_trough: C,
    /// The active-project pulse at its brightest.
    pub pulse_peak: C,

    // ---- Terminal hues ----------------------------------------------------
    /// ANSI white: emphasized text (titles, a selected row's label).
    pub ansi_white: C,
    /// ANSI gray: body text, the Backlog category, an unknown agent.
    pub ansi_gray: C,
    /// ANSI dark gray: dim text, hints, separators, an idle badge.
    pub ansi_dark_gray: C,
    /// ANSI red: errors, a blocked workspace, the session-lost band.
    pub ansi_red: C,
    /// ANSI bright red: failures in the activity feed, the `foxtrot` agent.
    pub ansi_light_red: C,
    /// ANSI green: success, the Done category, a working workspace.
    pub ansi_green: C,
    /// ANSI yellow: warnings, the Active category, awaiting input.
    pub ansi_yellow: C,
    /// ANSI blue: the Ready category, queued work.
    pub ansi_blue: C,
    /// ANSI magenta: the Handoff category, a paused workspace.
    pub ansi_magenta: C,
    /// ANSI cyan: titles, focused borders, the `alpha` agent.
    pub ansi_cyan: C,
}

impl Theme {
    /// The dark theme: exactly the TUI's long-standing values.
    pub fn dark() -> Self {
        use Ansi::*;
        Self {
            appearance: Appearance::Dark,
            background: Swatch::rgb(0x000000),
            canvas: Swatch::rgb(0x1c1c1c),
            foreground: Swatch::rgb(0xc6c6c6),
            gray: Swatch::rgb(0xbababa),
            white: Swatch::rgb(0xffffff),
            muted: Swatch::rgb(0x7a7a7a),
            cyan: Swatch::rgb(0x00a6b2),
            green: Swatch::rgb(0x5acd25),
            red: Swatch::rgb(0xe04f52),
            yellow: Swatch::rgb(0xdbc300),
            selection: Swatch::rgb(0x3f3f3f),
            search: Swatch::rgb(0x292929),
            divider: Swatch::rgb(0x414141),
            zen: Swatch::rgb(0x007f00),

            divider_active: Swatch::ansi(Cyan, 0x00a6b2),
            workflow_badge_bg: Swatch::rgb(0x3a4258),
            workflow_badge_fg: Swatch::rgb(0xdcdfe8),
            on_color: Swatch::ansi(White, 0xffffff),
            button_hover: Swatch::rgb(0x45474e),
            error_button: Swatch::rgb(0x6e4646),
            error_button_hover: Swatch::rgb(0x825454),
            error_marker: Swatch::rgb(0xd27878),
            notice: Swatch::rgb(0x5a461e),
            status_bar: Swatch::rgb(0x282832),
            code_bg: Swatch::indexed(236, 0x303030),
            code_fg: Swatch::indexed(252, 0xd0d0d0),
            pulse_trough: Swatch::rgb(0x002d00),
            pulse_peak: Swatch::rgb(0x00dc00),

            ansi_white: Swatch::ansi(White, 0xffffff),
            ansi_gray: Swatch::ansi(Gray, 0xb8b8b8),
            ansi_dark_gray: Swatch::ansi(DarkGray, 0x7c7c7c),
            ansi_red: Swatch::ansi(Red, 0xe04f52),
            ansi_light_red: Swatch::ansi(LightRed, 0xff6e67),
            ansi_green: Swatch::ansi(Green, 0x5fb56d),
            ansi_yellow: Swatch::ansi(Yellow, 0xdcb767),
            ansi_blue: Swatch::ansi(Blue, 0x4a8fd7),
            ansi_magenta: Swatch::ansi(Magenta, 0xc586c0),
            ansi_cyan: Swatch::ansi(Cyan, 0x4ec9b0),
        }
    }

    /// The light theme: the strict-monochrome inverse of [`Theme::dark`],
    /// after shelbi.dev's light mode. Every token is an explicit RGB.
    pub fn light() -> Self {
        Self {
            appearance: Appearance::Light,
            background: Swatch::rgb(0xf7f7f7),
            canvas: Swatch::rgb(0xffffff),
            foreground: Swatch::rgb(0x262626),
            gray: Swatch::rgb(0x3d3d3d),
            white: Swatch::rgb(0x111111),
            muted: Swatch::rgb(0x6b6b6b),
            cyan: Swatch::rgb(0x007a83),
            green: Swatch::rgb(0x367b16),
            red: Swatch::rgb(0xc23b3e),
            yellow: Swatch::rgb(0x7a6c00),
            selection: Swatch::rgb(0xdadada),
            search: Swatch::rgb(0xececec),
            divider: Swatch::rgb(0xd0d0d0),
            zen: Swatch::rgb(0x007f00),

            divider_active: Swatch::rgb(0x007a83),
            workflow_badge_bg: Swatch::rgb(0xd3d8e4),
            workflow_badge_fg: Swatch::rgb(0x2a3142),
            on_color: Swatch::rgb(0xffffff),
            button_hover: Swatch::rgb(0xe6e8ee),
            error_button: Swatch::rgb(0x6e4646),
            error_button_hover: Swatch::rgb(0x825454),
            error_marker: Swatch::rgb(0xb54848),
            notice: Swatch::rgb(0x5a461e),
            status_bar: Swatch::rgb(0xe4e4e4),
            code_bg: Swatch::rgb(0xececec),
            code_fg: Swatch::rgb(0x1a1a1a),
            pulse_trough: Swatch::rgb(0xd6ecd6),
            pulse_peak: Swatch::rgb(0x367b16),

            ansi_white: Swatch::rgb(0x111111),
            ansi_gray: Swatch::rgb(0x4f4f4f),
            ansi_dark_gray: Swatch::rgb(0x6b6b6b),
            ansi_red: Swatch::rgb(0xc23b3e),
            ansi_light_red: Swatch::rgb(0xc8281c),
            ansi_green: Swatch::rgb(0x1a7f37),
            ansi_yellow: Swatch::rgb(0x8a5f04),
            ansi_blue: Swatch::rgb(0x2563c9),
            ansi_magenta: Swatch::rgb(0xa21caf),
            ansi_cyan: Swatch::rgb(0x0f766e),
        }
    }

    /// The theme for a `theme:` setting. `system_is_dark` is the system
    /// appearance when the client can read it (`None` when it can't, as in a
    /// terminal); `auto` follows it and falls back to dark.
    pub fn for_preference(pref: ThemePreference, system_is_dark: Option<bool>) -> Self {
        match resolve_appearance(pref, system_is_dark) {
            Appearance::Dark => Self::dark(),
            Appearance::Light => Self::light(),
        }
    }

    /// The active-project pulse at animation `phase` (wrapped into
    /// `[0.0, 1.0)`): a raised cosine breathes from [`Theme::pulse_trough`]
    /// up to [`Theme::pulse_peak`] and back once per cycle, easing at both
    /// extremes so it reads as breathing rather than a blink.
    pub fn project_pulse(&self, phase: f32) -> Rgb {
        // Wrap defensively so a raw elapsed fraction (or one that drifted past
        // 1.0 through rounding) still maps onto one clean cycle.
        let phase = phase.rem_euclid(1.0);
        // 0 → 1 → 0 across the cycle, eased at the extremes by the cosine.
        let alpha = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
        self.pulse_trough.rgb.lerp(self.pulse_peak.rgb, alpha)
    }
}

/// The appearance a `theme:` setting resolves to. `system_is_dark` as in
/// [`Theme::for_preference`].
pub fn resolve_appearance(pref: ThemePreference, system_is_dark: Option<bool>) -> Appearance {
    match pref {
        ThemePreference::Dark => Appearance::Dark,
        ThemePreference::Light => Appearance::Light,
        ThemePreference::Auto => match system_is_dark {
            Some(false) => Appearance::Light,
            Some(true) | None => Appearance::Dark,
        },
    }
}

impl<C: Copy> Theme<C> {
    /// Convert every token with `f`: how a client turns the theme into its
    /// toolkit's colors. Exhaustive by construction, so a new token can't be
    /// forgotten by a client.
    pub fn map<D>(&self, mut f: impl FnMut(C) -> D) -> Theme<D> {
        let Theme {
            appearance,
            background,
            canvas,
            foreground,
            gray,
            white,
            muted,
            cyan,
            green,
            red,
            yellow,
            selection,
            search,
            divider,
            zen,
            divider_active,
            workflow_badge_bg,
            workflow_badge_fg,
            on_color,
            button_hover,
            error_button,
            error_button_hover,
            error_marker,
            notice,
            status_bar,
            code_bg,
            code_fg,
            pulse_trough,
            pulse_peak,
            ansi_white,
            ansi_gray,
            ansi_dark_gray,
            ansi_red,
            ansi_light_red,
            ansi_green,
            ansi_yellow,
            ansi_blue,
            ansi_magenta,
            ansi_cyan,
        } = *self;
        Theme {
            appearance,
            background: f(background),
            canvas: f(canvas),
            foreground: f(foreground),
            gray: f(gray),
            white: f(white),
            muted: f(muted),
            cyan: f(cyan),
            green: f(green),
            red: f(red),
            yellow: f(yellow),
            selection: f(selection),
            search: f(search),
            divider: f(divider),
            zen: f(zen),
            divider_active: f(divider_active),
            workflow_badge_bg: f(workflow_badge_bg),
            workflow_badge_fg: f(workflow_badge_fg),
            on_color: f(on_color),
            button_hover: f(button_hover),
            error_button: f(error_button),
            error_button_hover: f(error_button_hover),
            error_marker: f(error_marker),
            notice: f(notice),
            status_bar: f(status_bar),
            code_bg: f(code_bg),
            code_fg: f(code_fg),
            pulse_trough: f(pulse_trough),
            pulse_peak: f(pulse_peak),
            ansi_white: f(ansi_white),
            ansi_gray: f(ansi_gray),
            ansi_dark_gray: f(ansi_dark_gray),
            ansi_red: f(ansi_red),
            ansi_light_red: f(ansi_light_red),
            ansi_green: f(ansi_green),
            ansi_yellow: f(ansi_yellow),
            ansi_blue: f(ansi_blue),
            ansi_magenta: f(ansi_magenta),
            ansi_cyan: f(ansi_cyan),
        }
    }

    /// Every token as `(name, value)`, in declaration order. Built on
    /// [`Theme::map`]'s exhaustive destructure, so it can't miss a field.
    pub fn tokens(&self) -> Vec<(&'static str, C)> {
        let mut out = Vec::new();
        let mut names = TOKEN_NAMES.iter().copied();
        self.map(|c| out.push((names.next().expect("TOKEN_NAMES covers every token"), c)));
        assert!(names.next().is_none(), "TOKEN_NAMES lists a token Theme lacks");
        out
    }
}

/// The token field names, in declaration order (see [`Theme::tokens`]).
const TOKEN_NAMES: &[&str] = &[
    "background",
    "canvas",
    "foreground",
    "gray",
    "white",
    "muted",
    "cyan",
    "green",
    "red",
    "yellow",
    "selection",
    "search",
    "divider",
    "zen",
    "divider_active",
    "workflow_badge_bg",
    "workflow_badge_fg",
    "on_color",
    "button_hover",
    "error_button",
    "error_button_hover",
    "error_marker",
    "notice",
    "status_bar",
    "code_bg",
    "code_fg",
    "pulse_trough",
    "pulse_peak",
    "ansi_white",
    "ansi_gray",
    "ansi_dark_gray",
    "ansi_red",
    "ansi_light_red",
    "ansi_green",
    "ansi_yellow",
    "ansi_blue",
    "ansi_magenta",
    "ansi_cyan",
];

/// A text color a shared view model asks for by token name, so a row built
/// once in `shelbi-app` (an activity row, a machines badge) paints the same
/// token in the TUI and the desktop. Each names a [`Theme`] field; a client
/// resolves it with [`Tone::pick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    White,
    Gray,
    DarkGray,
    Red,
    LightRed,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    /// The workflow badge's foreground (machine tags).
    WorkflowBadge,
}

impl Tone {
    /// The token this tone names in `theme`.
    pub fn pick<C: Copy>(self, theme: &Theme<C>) -> C {
        match self {
            Tone::White => theme.ansi_white,
            Tone::Gray => theme.ansi_gray,
            Tone::DarkGray => theme.ansi_dark_gray,
            Tone::Red => theme.ansi_red,
            Tone::LightRed => theme.ansi_light_red,
            Tone::Green => theme.ansi_green,
            Tone::Yellow => theme.ansi_yellow,
            Tone::Blue => theme.ansi_blue,
            Tone::Magenta => theme.ansi_magenta,
            Tone::Cyan => theme.ansi_cyan,
            Tone::WorkflowBadge => theme.workflow_badge_fg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokens drawn as text on the theme's background (or the canvas).
    const TEXT_TOKENS: &[&str] = &[
        "foreground",
        "gray",
        "white",
        "muted",
        "cyan",
        "green",
        "red",
        "yellow",
        "divider_active",
        "error_marker",
        "ansi_white",
        "ansi_gray",
        "ansi_dark_gray",
        "ansi_red",
        "ansi_light_red",
        "ansi_green",
        "ansi_yellow",
        "ansi_blue",
        "ansi_magenta",
        "ansi_cyan",
    ];

    fn token(theme: &Theme, name: &str) -> Swatch {
        theme
            .tokens()
            .into_iter()
            .find(|(n, _)| *n == name)
            .unwrap_or_else(|| panic!("no token {name}"))
            .1
    }

    fn assert_contrast(theme: &Theme, fg: &str, bg: &str, min: f64) {
        let ratio = token(theme, fg).rgb.contrast_ratio(token(theme, bg).rgb);
        assert!(
            ratio >= min,
            "{:?}: {fg} on {bg} is {ratio:.2}:1, below {min}:1",
            theme.appearance
        );
    }

    #[test]
    fn every_token_is_named_once_and_set_in_both_themes() {
        let (dark, light) = (Theme::dark().tokens(), Theme::light().tokens());
        assert_eq!(dark.len(), TOKEN_NAMES.len());
        let names: Vec<_> = dark.iter().map(|(n, _)| *n).collect();
        let light_names: Vec<_> = light.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, light_names);
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "token names are unique");
        for name in TEXT_TOKENS {
            assert!(names.contains(name), "TEXT_TOKENS names a real token: {name}");
        }
        // Light pins every token to an exact RGB: a light terminal's palette
        // remaps ANSI colors unpredictably.
        for (name, swatch) in &light {
            assert_eq!(swatch.term, None, "light {name} must be an exact RGB");
        }
        // Light is a real inverse, not a copy: the text and fill tokens move.
        for name in ["background", "canvas", "gray", "white", "muted", "selection", "search"] {
            assert_ne!(
                token(&Theme::dark(), name).rgb,
                token(&Theme::light(), name).rgb,
                "{name} differs between themes"
            );
        }
    }

    #[test]
    fn dark_matches_the_tui_values() {
        // The TUI's former `theme.rs` constants, byte for byte.
        let t = Theme::dark();
        let rgb = |s: Swatch| (s.rgb.r, s.rgb.g, s.rgb.b);
        assert_eq!(rgb(t.background), (0, 0, 0));
        assert_eq!(rgb(t.gray), (186, 186, 186));
        assert_eq!(rgb(t.white), (255, 255, 255));
        assert_eq!(rgb(t.muted), (122, 122, 122));
        assert_eq!(rgb(t.cyan), (0, 166, 178));
        assert_eq!(rgb(t.green), (90, 205, 37));
        assert_eq!(rgb(t.selection), (63, 63, 63));
        assert_eq!(rgb(t.search), (41, 41, 41));
        assert_eq!(rgb(t.foreground), (198, 198, 198));
        assert_eq!(rgb(t.red), (224, 79, 82));
        assert_eq!(rgb(t.yellow), (0xdb, 0xc3, 0x00));
        assert_eq!(rgb(t.divider), (65, 65, 65));
        assert_eq!(rgb(t.zen), (0, 127, 0));
        assert_eq!(rgb(t.workflow_badge_bg), (58, 66, 88));
        assert_eq!(rgb(t.workflow_badge_fg), (220, 223, 232));
        assert_eq!(rgb(t.error_button), (110, 70, 70));
        assert_eq!(rgb(t.error_marker), (210, 120, 120));
        assert_eq!(rgb(t.notice), (90, 70, 30));
        assert_eq!(rgb(t.status_bar), (40, 40, 50));
        assert_eq!(t.divider_active.term, Some(TermColor::Ansi(Ansi::Cyan)));
        assert_eq!(t.on_color.term, Some(TermColor::Ansi(Ansi::White)));
        assert_eq!(t.code_bg.term, Some(TermColor::Indexed(236)));
        assert_eq!(t.code_fg.term, Some(TermColor::Indexed(252)));
        assert_eq!(t.ansi_dark_gray.term, Some(TermColor::Ansi(Ansi::DarkGray)));
        // Brand tokens are exact colors in the terminal too.
        for name in ["background", "gray", "white", "muted", "cyan", "green", "selection"] {
            assert_eq!(token(&t, name).term, None, "dark {name} is an exact RGB");
        }
    }

    #[test]
    fn text_tokens_clear_4_5_to_1_in_both_themes() {
        for theme in [Theme::dark(), Theme::light()] {
            for fg in TEXT_TOKENS {
                assert_contrast(&theme, fg, "background", 4.5);
            }
            assert_contrast(&theme, "workflow_badge_fg", "workflow_badge_bg", 4.5);
            assert_contrast(&theme, "code_fg", "code_bg", 4.5);
            // The selected label and the rows under a selection fill.
            for fg in ["white", "gray", "foreground"] {
                assert_contrast(&theme, fg, "selection", 4.5);
                assert_contrast(&theme, fg, "search", 4.5);
                assert_contrast(&theme, fg, "button_hover", 4.5);
            }
        }
        // Light also lands on the white canvas (the desktop's main area) and
        // puts white text on its saturated fills. Dark keeps the TUI's values.
        let light = Theme::light();
        for fg in TEXT_TOKENS {
            assert_contrast(&light, fg, "canvas", 4.5);
        }
        for fill in ["zen", "error_button", "error_button_hover", "notice", "ansi_red", "ansi_green", "green"] {
            assert_contrast(&light, "on_color", fill, 4.5);
        }
        assert_contrast(&light, "ansi_gray", "status_bar", 4.5);
    }

    #[test]
    fn preference_resolution() {
        use ThemePreference::*;
        assert_eq!(Theme::for_preference(Dark, Some(false)), Theme::dark());
        assert_eq!(Theme::for_preference(Light, Some(true)), Theme::light());
        assert_eq!(Theme::for_preference(Dark, None), Theme::dark());
        assert_eq!(Theme::for_preference(Light, None), Theme::light());
        assert_eq!(Theme::for_preference(Auto, Some(true)), Theme::dark());
        assert_eq!(Theme::for_preference(Auto, Some(false)), Theme::light());
        // A client that can't read the system appearance (a terminal) is dark.
        assert_eq!(Theme::for_preference(Auto, None), Theme::dark());
        assert_eq!(ThemePreference::default(), Auto);
    }

    #[test]
    fn map_keeps_every_token_in_place() {
        let t = Theme::light();
        let hexes = t.map(|s| s.rgb);
        assert_eq!(hexes.appearance, Appearance::Light);
        assert_eq!(hexes.canvas, t.canvas.rgb);
        assert_eq!(hexes.ansi_cyan, t.ansi_cyan.rgb);
        let names: Vec<_> = t.tokens().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.first(), Some(&"background"));
        assert_eq!(names.last(), Some(&"ansi_cyan"));
    }

    #[test]
    fn pulse_breathes_from_trough_to_peak() {
        let t = Theme::dark();
        assert_eq!(t.project_pulse(0.0), t.pulse_trough.rgb);
        assert_eq!(t.project_pulse(0.5), t.pulse_peak.rgb);
        assert_eq!(t.project_pulse(1.0), t.project_pulse(0.0), "wraps at 1.0");
        assert_eq!(t.project_pulse(0.25), t.project_pulse(0.75), "symmetric");
        // The dark pulse is the TUI's pure-green breath (g from 45 to 220).
        assert_eq!(t.project_pulse(0.0), Rgb::new(0, 45, 0));
        assert_eq!(t.project_pulse(0.5), Rgb::new(0, 220, 0));
    }

    #[test]
    fn contrast_ratio_matches_wcag_endpoints() {
        let (black, white) = (Rgb::hex(0x000000), Rgb::hex(0xffffff));
        assert!((black.contrast_ratio(white) - 21.0).abs() < 1e-9);
        assert!((white.contrast_ratio(black) - 21.0).abs() < 1e-9);
        assert!((white.contrast_ratio(white) - 1.0).abs() < 1e-9);
    }
}
