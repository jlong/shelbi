//! The active Omarchy theme as the desktop's palette.
//!
//! On Omarchy (a Linux desktop built on Hyprland), every app takes its
//! colors from one theme: `omarchy-theme-set <name>` stages the theme's
//! files and swaps them into `~/.local/state/omarchy/current/theme/` (the
//! old directory removed, the staged one renamed into place), with the
//! theme's name in `current/theme.name`. Its `colors.toml` names about two
//! dozen colors (`background`, `foreground`, `accent`, `selection`, the
//! ANSI hues and their bright variants, `mode = "dark" | "light"`).
//!
//! While the `theme:` setting is `auto`, the desktop paints from that file:
//! [`derive`] maps Omarchy's colors onto every role of [`Palette`] (deriving
//! the hover and pressed fills, the borders and the tints Omarchy doesn't
//! define, by the rules `desktop/docs/theme.md` records), and
//! [`meet_contrast`] then moves any text, icon or edge that falls short of
//! the pairing rules `pairing_list` checks until it passes, so every theme
//! Omarchy ships reads as well as the built-in one. The terminal takes the
//! theme's own terminal colors, as Omarchy's Alacritty and Ghostty templates
//! lay them out ([`terminal`]).
//!
//! [`follow`] polls the file (a few cheap `stat`s four times a second) and
//! re-resolves the theme when it changes, so a switch recolors every window
//! within a second. It also follows Omarchy's monospace font
//! (`omarchy-font-set` rewrites fontconfig's `monospace` alias; [`font`]),
//! which the terminal uses unless `desktop.terminal_font_family` names one.
//!
//! Off Omarchy (no `current/theme/colors.toml`), nothing changes: the
//! built-in Shelbi palette.

use std::path::{Path, PathBuf};
use std::cell::RefCell;
use std::time::{Duration, SystemTime};

use shelbi_app::theme::{Appearance, Rgb};
use shelbi_term::render::Rgb as TermRgb;

use super::palette::{pairing_list, Palette, Rgba};
use super::TerminalPalette;

/// The colors an Omarchy `colors.toml` names. A theme that leaves one out
/// gets it derived from the others (see [`OmarchyColors::parse`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OmarchyColors {
    pub mode: Appearance,
    pub accent: Rgb,
    pub selection: Rgb,
    pub muted: Rgb,
    pub background: Rgb,
    pub dark_background: Rgb,
    pub darker_background: Rgb,
    pub lighter_background: Rgb,
    pub foreground: Rgb,
    pub dark_foreground: Rgb,
    pub light_foreground: Rgb,
    pub bright_foreground: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub cyan: Rgb,
    pub blue: Rgb,
    pub magenta: Rgb,
    pub bright_red: Rgb,
    pub bright_yellow: Rgb,
    pub bright_green: Rgb,
    pub bright_cyan: Rgb,
    pub bright_blue: Rgb,
    pub bright_magenta: Rgb,
}

impl OmarchyColors {
    /// Read a `colors.toml`: `key = "#rrggbb"` lines (comments, blank lines
    /// and keys this doesn't use are skipped). `background`, `foreground`
    /// and `accent` are required; without them this isn't a theme. The rest
    /// fall back to what Omarchy's own templates would show: a missing
    /// `mode` is read from the background's luminance, a missing shade is
    /// the nearest one the theme has, a missing hue the built-in palette's,
    /// a missing bright hue the regular one.
    pub fn parse(text: &str) -> Option<Self> {
        let mut values = std::collections::HashMap::new();
        let mut mode = None;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let Some(value) = quoted(value.trim()) else { continue };
            match key.trim() {
                "mode" => mode = Some(value.to_string()),
                key => {
                    if let Some(rgb) = hex(value) {
                        values.insert(key.to_string(), rgb);
                    }
                }
            }
        }
        let get = |k: &str| values.get(k).copied();
        let background = get("background")?;
        let foreground = get("foreground")?;
        let accent = get("accent")?;
        let mode = match mode.as_deref() {
            Some("light") => Appearance::Light,
            Some("dark") => Appearance::Dark,
            _ if background.relative_luminance() > 0.5 => Appearance::Light,
            _ => Appearance::Dark,
        };
        let builtin = Palette::for_appearance(mode);
        let dark_background = get("dark_background").unwrap_or(background);
        let darker_background = get("darker_background").unwrap_or(dark_background);
        let dark_foreground = get("dark_foreground").unwrap_or_else(|| background.lerp(foreground, 0.5));
        let hue = |k: &str, fallback: Rgba| get(k).unwrap_or(fallback.rgb);
        let (red, yellow, green, cyan, blue, magenta) = (
            hue("red", builtin.red),
            hue("yellow", builtin.yellow),
            hue("green", builtin.green),
            hue("cyan", builtin.cyan),
            hue("blue", builtin.blue),
            hue("magenta", builtin.magenta),
        );
        Some(Self {
            mode,
            accent,
            selection: get("selection").unwrap_or_else(|| background.lerp(foreground, 0.2)),
            muted: get("muted").unwrap_or(dark_foreground),
            background,
            dark_background,
            darker_background,
            lighter_background: get("lighter_background").unwrap_or_else(|| background.lerp(foreground, 0.08)),
            foreground,
            dark_foreground,
            light_foreground: get("light_foreground").unwrap_or(foreground),
            bright_foreground: get("bright_foreground").unwrap_or(foreground),
            red,
            yellow,
            green,
            cyan,
            blue,
            magenta,
            bright_red: get("bright_red").unwrap_or(red),
            bright_yellow: get("bright_yellow").unwrap_or(yellow),
            bright_green: get("bright_green").unwrap_or(green),
            bright_cyan: get("bright_cyan").unwrap_or(cyan),
            bright_blue: get("bright_blue").unwrap_or(blue),
            bright_magenta: get("bright_magenta").unwrap_or(magenta),
        })
    }
}

/// A `"quoted"` TOML string's contents (anything after it, a comment, is
/// ignored).
fn quoted(value: &str) -> Option<&str> {
    let value = value.strip_prefix('"')?;
    Some(&value[..value.find('"')?])
}

/// `#rrggbb` (any case) as a color.
fn hex(value: &str) -> Option<Rgb> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(Rgb::hex)
}

/// What the desktop paints from while an Omarchy theme is active: the
/// palette ([`palette`]) and the terminal's colors ([`terminal`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemTheme {
    pub palette: Palette,
    pub terminal: TerminalPalette,
}

impl SystemTheme {
    pub fn from_colors(colors: &OmarchyColors) -> Self {
        let palette = palette(colors);
        Self {
            palette,
            terminal: terminal(colors, &palette),
        }
    }
}

/// The palette for an Omarchy theme: [`derive`], then [`meet_contrast`].
pub fn palette(colors: &OmarchyColors) -> Palette {
    meet_contrast(derive(colors))
}

/// Every role from Omarchy's colors, before the contrast pass. The rules
/// (`desktop/docs/theme.md`, "Omarchy themes"):
///
/// - Surfaces are the theme's backgrounds: the main area `background`, the
///   leading column `dark_background`; in Dark a card and a textbox's well
///   `darker_background` and a raised surface `lighter_background`, in Light
///   both white-like (`background`) and the raised surface `dark_background`.
/// - The neutral fills (buttons, hovered and selected rows, the search box,
///   the toolbar band, dividers and edges) are the secondary-text gray as a
///   tint (as the built-in palette's are its icon slate), at the built-in palette's opacities (Dark 18 / 28 / 40% for
///   rest, hover and pressed, Light 12 / 22 / 32%), so they work over any of
///   the surfaces and over the window's glass.
/// - Omarchy's `selection` becomes the tint that paints it over the
///   background (the selected segment, a selected row in Light, text
///   selection), so a translucent window still shows through it.
/// - Text: titles `bright_foreground`, body `foreground`, quiet text
///   `dark_foreground`, secondary text and icons halfway between the two.
/// - The accent is the selected nav icon, links, the focus ring, a checked
///   box, the Primary and Select buttons; the hues are the meanings
///   (green success, yellow warning, blue info, red errors, the log's tags).
///   Each tinted fill is its hue at the neutral fill's opacities.
/// - Shadows keep the built-in values; the scrim is `darker_background`
///   (Dark) or `foreground` (Light) at 35 / 40%.
pub fn derive(c: &OmarchyColors) -> Palette {
    let dark = c.mode == Appearance::Dark;
    let builtin = Palette::for_appearance(c.mode);
    let o = |rgb: Rgb| Rgba { rgb, a: 0xff };
    let at = |rgb: Rgb, percent: u8| o(rgb).alpha(percent);
    let (rest, hover, active) = if dark { (18, 28, 40) } else { (12, 22, 32) };
    // Secondary text and icons, halfway from the quiet text to the body;
    // as in the built-in palette (`icon/primary`'s slate), it is also the
    // hue of every neutral fill.
    let gray_rgb = c.dark_foreground.lerp(c.foreground, 0.5);
    let neutral = gray_rgb;
    let canvas = o(c.background);
    let background = at(neutral, rest);
    let selection = tint_over(c.selection, c.background);
    let gray = o(gray_rgb);
    let muted = o(c.dark_foreground);
    let well = if dark { o(c.darker_background) } else { canvas };
    let tint = |hue: Rgb| at(hue, rest);
    // A hued button's label is its own hue, so its fills stay lighter than
    // the neutral ones: at the built-in 40% the label can't reach AA on its
    // pressed fill without washing out to white.
    let (hue_rest, hue_hover, hue_active) = if dark { (14, 20, 26) } else { (10, 16, 22) };
    let fills = |hue: Rgb| (at(hue, hue_rest), at(hue, hue_hover), at(hue, hue_active));
    let (accent_rest, accent_hover, accent_active) = fills(c.accent);
    let (green_rest, green_hover, green_active) = fills(c.green);
    let (red_rest, red_hover, red_active) = fills(c.red);
    Palette {
        appearance: c.mode,
        canvas,
        sidebar: o(c.dark_background),
        card: well,
        raised: if dark { o(c.lighter_background) } else { o(c.dark_background) },
        well: canvas,
        toolbar: at(neutral, if dark { 10 } else { 8 }),
        textbox: well,

        background,
        background_hover: at(neutral, hover),
        background_active: at(neutral, active),
        select_fill: accent_rest,
        select_fill_hover: accent_hover,
        select_fill_active: accent_active,
        select_label: o(c.accent),
        selection,
        search: at(neutral, if dark { 25 } else { 16 }),
        keycap: canvas,
        button_keycap_edge: if dark { at(c.darker_background, 55) } else { at(c.dark_foreground, 60) },

        text_loud: o(c.bright_foreground),
        foreground: o(c.foreground),
        gray,
        form_text: o(c.foreground),
        form_hint: muted,
        muted,
        // On the accent: the background reads on it the way the theme's own
        // selected items do.
        text_on_color: canvas,
        log_text: gray,
        table_text: gray,
        code_bg: if dark { o(c.darker_background) } else { o(c.dark_background) },
        code_fg: o(c.foreground),

        icon: gray,
        button_icon: o(c.dark_foreground.lerp(c.foreground, 0.75)),
        accent: o(c.accent),
        icon_cyan: o(c.cyan),
        icon_green: o(c.green),
        icon_yellow: o(c.yellow),
        icon_purple: o(c.magenta),
        progress: o(c.accent),

        divider: at(neutral, 10),
        divider_strong: at(neutral, 20),
        input_border: at(neutral, if dark { 30 } else { 50 }),
        check_border: at(neutral, if dark { 20 } else { 50 }),

        link: o(c.accent),
        link_hover: o(c.bright_foreground),
        focus_ring: o(c.accent),
        focus_glow: at(c.accent, 20),
        success_focus_ring: o(c.green),
        success_focus_glow: at(c.green, if dark { 20 } else { 15 }),
        danger_focus_ring: o(c.red),
        danger_focus_glow: at(c.red, if dark { 25 } else { 10 }),
        check_fill: o(c.accent),
        check_glow: at(c.accent, 20),

        success: o(c.green),
        warning: o(c.yellow),
        info: o(c.blue),
        error: o(c.red),

        theme_green: o(c.green),
        theme_cyan: o(c.cyan),
        theme_red: o(c.red),
        green: o(c.green),
        yellow: o(c.yellow),
        blue: o(c.blue),
        magenta: o(c.magenta),
        cyan: o(c.cyan),
        dark_gray: muted,
        red: o(c.red),

        green_tint: tint(c.green),
        yellow_tint: tint(c.yellow),
        cyan_tint: tint(c.cyan),
        blue_tint: tint(c.blue),
        magenta_tint: tint(c.magenta),
        red_tint: tint(c.red),
        workflow_badge: if dark { o(c.lighter_background) } else { o(c.darker_background) },
        workflow_badge_text: o(c.foreground),

        primary_fill: accent_rest,
        primary_fill_hover: accent_hover,
        primary_fill_active: accent_active,
        primary_label: o(c.accent),
        success_fill: green_rest,
        success_fill_hover: green_hover,
        success_fill_active: green_active,
        success_label: o(c.green),
        danger_fill: red_rest,
        danger_fill_hover: red_hover,
        danger_fill_active: red_active,

        zen: green_rest,
        notice: tint(c.yellow),

        nav_selected: canvas,
        row_selected: if dark { background } else { selection },
        text_selection: selection,
        search_match: at(neutral, if dark { 25 } else { 16 }),
        search_current: at(c.yellow, 40),

        ring: at(neutral, if dark { 10 } else { 6 }),
        shadow: builtin.shadow,
        shadow_ring: builtin.shadow_ring,
        palette_shadow: builtin.palette_shadow,
        pane_shadow: if dark { at(c.darker_background, 55) } else { at(c.dark_foreground, 60) },
        scrim: if dark { at(c.darker_background, 35) } else { at(c.foreground, 40) },
    }
}

/// The least opaque tint that paints `solid` over `under` (the inverse of
/// [`Rgba::over`]): per channel the alpha that reaches it with the tint at 0
/// or 255, the largest of the three.
fn tint_over(solid: Rgb, under: Rgb) -> Rgba {
    let need = |f: u8, u: u8| -> f32 {
        let (f, u) = (f32::from(f), f32::from(u));
        if f > u {
            (f - u) / (255.0 - u)
        } else if f < u {
            (u - f) / u
        } else {
            0.0
        }
    };
    let a = need(solid.r, under.r)
        .max(need(solid.g, under.g))
        .max(need(solid.b, under.b))
        .clamp(0.0, 1.0);
    if a <= f32::EPSILON {
        return Rgba { rgb: solid, a: 0 };
    }
    let channel = |f: u8, u: u8| {
        let (f, u) = (f32::from(f), f32::from(u));
        (u + (f - u) / a).round().clamp(0.0, 255.0) as u8
    };
    Rgba {
        rgb: Rgb::new(channel(solid.r, under.r), channel(solid.g, under.g), channel(solid.b, under.b)),
        a: (a * 255.0).round() as u8,
    }
}

/// Move every role drawn in front (text, icons, edges, rings) that falls
/// short of a pairing's minimum just far enough to meet it: toward white
/// when it is lighter than what it sits on, toward black when darker (or the
/// other way when that end can't reach it), its alpha toward opaque as it
/// goes. Surfaces and fills never move, so the theme's backgrounds stay its
/// own. Repeats until nothing falls short, since a role moved for one
/// background can be a background itself (a checked box under its mark).
pub fn meet_contrast(mut p: Palette) -> Palette {
    for _ in 0..32 {
        let short: Vec<_> = pairing_list(&p).into_iter().filter(|x| x.ratio() < x.min).collect();
        if short.is_empty() {
            break;
        }
        for x in short {
            let role = x.fg_role().to_string();
            p = p.map_named(|name, c| if name == role { raise(c, x.bg.rgb, x.min) } else { c });
        }
    }
    p
}

/// `c` moved the least distance that reaches `min` (with a small margin
/// over rounding) on the opaque `bg`.
fn raise(c: Rgba, bg: Rgb, min: f64) -> Rgba {
    let target = min + 0.02;
    let passes = |c: Rgba| c.over(bg).contrast_ratio(bg) >= target;
    if passes(c) {
        return c;
    }
    let (white, black) = (Rgb::hex(0xffffff), Rgb::hex(0x000000));
    let lighter = c.over(bg).relative_luminance() >= bg.relative_luminance();
    let ends = if lighter { [white, black] } else { [black, white] };
    for end in ends {
        for step in 1..=200u16 {
            let t = f32::from(step) / 200.0;
            let a = f32::from(c.a) + (255.0 - f32::from(c.a)) * t;
            let moved = Rgba {
                rgb: c.rgb.lerp(end, t),
                a: a.round() as u8,
            };
            if passes(moved) {
                return moved;
            }
        }
    }
    c
}

/// The terminal's colors for an Omarchy theme, as Omarchy's own terminal
/// templates (`alacritty.toml`, `ghostty.conf`) lay them out: the default
/// foreground and background are `foreground` and `background`, the cursor
/// `bright_foreground`; ANSI black is the background, white the
/// foreground, bright black `muted` and bright white `bright_foreground`.
/// The selection and the search highlights are the palette's, flattened.
pub fn terminal(c: &OmarchyColors, palette: &Palette) -> TerminalPalette {
    let t = |rgb: Rgb| TermRgb::new(rgb.r, rgb.g, rgb.b);
    let flat = |role: Rgba| t(role.over(c.background));
    TerminalPalette {
        foreground: t(c.foreground),
        bold_foreground: t(c.bright_foreground),
        dim_foreground: flat(palette.muted),
        background: t(c.background),
        cursor: t(c.bright_foreground),
        cursor_text: t(c.background),
        selection: flat(palette.text_selection),
        search_match: flat(palette.search_match),
        search_current: flat(palette.search_current),
        ansi: [
            c.background,
            c.red,
            c.green,
            c.yellow,
            c.blue,
            c.magenta,
            c.cyan,
            c.foreground,
            c.muted,
            c.bright_red,
            c.bright_green,
            c.bright_yellow,
            c.bright_blue,
            c.bright_magenta,
            c.bright_cyan,
            c.bright_foreground,
        ]
        .map(t),
    }
}

// ---- The live theme ---------------------------------------------------------

/// Omarchy's state directory (`$XDG_STATE_HOME/omarchy/current`, else
/// `~/.local/state/omarchy/current`). Never in a test build: tests paint
/// the built-in palette whatever the machine runs. Never on macOS, which
/// Omarchy doesn't run on.
pub fn state_dir() -> Option<PathBuf> {
    if cfg!(test) || cfg!(target_os = "macos") {
        return None;
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("omarchy/current"))
}

/// The active theme's colors file under `dir`.
fn colors_path(dir: &Path) -> PathBuf {
    dir.join("theme/colors.toml")
}

/// The theme in `dir` now, and its name (`theme.name`), or `None` off
/// Omarchy or when its colors file can't be read as a theme.
pub fn load(dir: &Path) -> Option<(String, SystemTheme)> {
    let text = std::fs::read_to_string(colors_path(dir)).ok()?;
    let colors = OmarchyColors::parse(&text)?;
    let name = std::fs::read_to_string(dir.join("theme.name"))
        .map(|n| n.trim().to_string())
        .unwrap_or_default();
    Some((name, SystemTheme::from_colors(&colors)))
}

/// The active theme, read once (startup).
pub fn load_current() -> Option<SystemTheme> {
    let (name, theme) = load(&state_dir()?)?;
    set_name(Some(name));
    Some(theme)
}

// What the follow loop last read. The loop's updates, the settings view and
// the terminal settings all run on the UI thread, so these are per thread:
// each gpui test (one thread each) keeps its own.
thread_local! {
    static NAME: RefCell<Option<String>> = const { RefCell::new(None) };
    static FONT: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn set_name(name: Option<String>) {
    NAME.with(|n| *n.borrow_mut() = name);
}

/// The active Omarchy theme's name (`tokyo-night`), or `None` off Omarchy.
pub fn current_name() -> Option<String> {
    NAME.with(|n| n.borrow().clone())
}

/// Omarchy's monospace font family (`JetBrainsMono Nerd Font`), or `None`
/// off Omarchy or before it has been read. The terminal uses it unless
/// `desktop.terminal_font_family` is set.
pub fn font() -> Option<String> {
    FONT.with(|f| f.borrow().clone())
}

/// Omarchy's monospace font as `omarchy-font-current` reads it: fontconfig's
/// `monospace` match, the first of its family names.
fn read_font() -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["monospace", "-f", "%{family}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    first_family(&String::from_utf8_lossy(&out.stdout))
}

/// The first family of an `fc-match` alias list (`A,A NF`).
fn first_family(list: &str) -> Option<String> {
    let first = list.lines().next()?.split(',').next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

/// Where `omarchy-font-set` writes the `monospace` alias.
fn fontconfig_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config.join("fontconfig/fonts.conf"))
}

/// What changes when a file is replaced or rewritten: a theme switch swaps
/// the whole directory (a new inode), an edit rewrites the file.
type Signature = Option<(SystemTime, u64, u64)>;

fn signature(path: &Path) -> Signature {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let ino = 0;
    Some((meta.modified().ok()?, ino, meta.len()))
}

/// How often the theme and font files are checked: a switch shows within a
/// quarter second of Omarchy finishing it.
const POLL: Duration = Duration::from_millis(250);

/// Follow the active Omarchy theme and font while the app runs: re-resolve
/// the theme when `colors.toml` changes (every window repaints), and the
/// terminal settings when the monospace font does. Does nothing off
/// Omarchy. Call once at startup, after the theme and the terminal settings
/// are installed.
pub fn follow(cx: &mut gpui::App) {
    let Some(dir) = state_dir() else { return };
    watch(dir, fontconfig_path(), read_font, cx);
}

/// [`follow`] for the state directory `dir`, with the fontconfig file
/// `fonts` and the font reader `read_font` (tests pass their own).
fn watch(dir: PathBuf, fonts: Option<PathBuf>, read_font: fn() -> Option<String>, cx: &mut gpui::App) {
    cx.spawn(async move |cx| {
        let colors = colors_path(&dir);
        let mut theme_seen: Option<Signature> = None;
        let mut font_seen: Option<(Signature, bool)> = None;
        loop {
            let now = signature(&colors);
            if theme_seen != Some(now) {
                theme_seen = Some(now);
                let loaded = load(&dir);
                let present = loaded.is_some();
                if let Some((name, _)) = &loaded {
                    tracing::info!(theme = %name, "following the Omarchy theme");
                }
                set_name(loaded.as_ref().map(|(n, _)| n.clone()));
                let system = loaded.map(|(_, t)| t);
                cx.update(|cx| super::set_system(system, cx));
                // The font only means something while Omarchy is there.
                font_seen = font_seen.filter(|(_, was)| *was == present);
            }
            let present = theme_seen.flatten().is_some();
            let font_now = fonts.as_deref().and_then(signature);
            if font_seen != Some((font_now, present)) {
                font_seen = Some((font_now, present));
                let family = if present {
                    cx.background_executor().spawn(async move { read_font() }).await
                } else {
                    None
                };
                let changed = FONT.with(|f| f.replace(family.clone()) != family);
                if changed {
                    tracing::info!(font = ?font(), "following the Omarchy font");
                    cx.update(crate::elements::terminal::TerminalSettings::reload);
                }
            }
            cx.background_executor().timer(POLL).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::palette::{AA, NON_TEXT};

    /// The `colors.toml` of every theme Omarchy ships
    /// (`/usr/share/omarchy/themes/*/colors.toml`, Omarchy 3.x, MIT).
    /// `osaka-jade` ships no `colors.toml`, so Omarchy itself has no colors
    /// for it and the app keeps its own palette there.
    pub const SHIPPED: &[(&str, &str)] = &[
        ("catppuccin", include_str!("omarchy_themes/catppuccin.toml")),
        ("catppuccin-latte", include_str!("omarchy_themes/catppuccin-latte.toml")),
        ("ethereal", include_str!("omarchy_themes/ethereal.toml")),
        ("everforest", include_str!("omarchy_themes/everforest.toml")),
        ("flexoki-light", include_str!("omarchy_themes/flexoki-light.toml")),
        ("gruvbox", include_str!("omarchy_themes/gruvbox.toml")),
        ("hackerman", include_str!("omarchy_themes/hackerman.toml")),
        ("jade-horse", include_str!("omarchy_themes/jade-horse.toml")),
        ("kanagawa", include_str!("omarchy_themes/kanagawa.toml")),
        ("last-horizon", include_str!("omarchy_themes/last-horizon.toml")),
        ("lumon", include_str!("omarchy_themes/lumon.toml")),
        ("lupine", include_str!("omarchy_themes/lupine.toml")),
        ("matte-black", include_str!("omarchy_themes/matte-black.toml")),
        ("miasma", include_str!("omarchy_themes/miasma.toml")),
        ("nord", include_str!("omarchy_themes/nord.toml")),
        ("retro-82", include_str!("omarchy_themes/retro-82.toml")),
        ("ristretto", include_str!("omarchy_themes/ristretto.toml")),
        ("rose-pine", include_str!("omarchy_themes/rose-pine.toml")),
        ("solitude", include_str!("omarchy_themes/solitude.toml")),
        ("starship", include_str!("omarchy_themes/starship.toml")),
        ("tokyo-night", include_str!("omarchy_themes/tokyo-night.toml")),
        ("vantablack", include_str!("omarchy_themes/vantablack.toml")),
        ("white", include_str!("omarchy_themes/white.toml")),
    ];

    fn colors(name: &str) -> OmarchyColors {
        let text = SHIPPED.iter().find(|(n, _)| *n == name).unwrap().1;
        OmarchyColors::parse(text).unwrap()
    }

    #[test]
    fn parses_a_colors_toml() {
        let c = colors("tokyo-night");
        assert_eq!(c.mode, Appearance::Dark);
        assert_eq!(c.background, Rgb::hex(0x1a1b26));
        assert_eq!(c.accent, Rgb::hex(0x7aa2f7));
        assert_eq!(c.bright_magenta, Rgb::hex(0xbb9af7));
        assert_eq!(colors("catppuccin-latte").mode, Appearance::Light);
        // Upper-case hex and a leading comment line.
        assert_eq!(colors("flexoki-light").background, Rgb::hex(0xfffcf0));
        assert_eq!(colors("jade-horse").background, Rgb::hex(0x142620));
    }

    #[test]
    fn a_partial_theme_derives_what_it_leaves_out() {
        let c = OmarchyColors::parse("background = \"#ffffff\"\nforeground = \"#000000\"\naccent = \"#ff0000\"\n").unwrap();
        assert_eq!(c.mode, Appearance::Light, "a white background with no mode is light");
        assert_eq!(c.dark_background, c.background);
        assert_eq!(c.bright_red, c.red);
        assert!(OmarchyColors::parse("foreground = \"#000000\"\n").is_none());
        assert!(OmarchyColors::parse("not a theme").is_none());
    }

    /// The acceptance bar: on every theme Omarchy ships, every pairing the
    /// desktop draws meets its minimum (AA for text, 3:1 for icons, rings
    /// and edges), with no exceptions.
    #[test]
    fn every_shipped_theme_passes_every_contrast_pairing() {
        let mut failures = Vec::new();
        for (name, text) in SHIPPED {
            let p = palette(&OmarchyColors::parse(text).unwrap());
            for x in pairing_list(&p) {
                if x.ratio() < x.min {
                    failures.push(format!("{name}: {} {:.2} < {}", x.what, x.ratio(), x.min));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The terminal's default foreground and dim text read on its background
    /// on every shipped theme (the ANSI hues are the theme's own, as its
    /// other terminals show them).
    #[test]
    fn every_shipped_theme_has_a_readable_terminal() {
        let t = |c: TermRgb| Rgb::new(c.r, c.g, c.b);
        for (name, text) in SHIPPED {
            let theme = SystemTheme::from_colors(&OmarchyColors::parse(text).unwrap());
            let term = theme.terminal;
            let bg = t(term.background);
            let fg = t(term.foreground).contrast_ratio(bg);
            assert!(fg >= AA, "{name}: foreground {fg:.2}");
            let dim = t(term.dim_foreground).contrast_ratio(bg);
            assert!(dim >= NON_TEXT, "{name}: dim {dim:.2}");
        }
    }

    /// The contrast pass moves only what it must: surfaces and fills are the
    /// theme's own, and a role that already passes keeps its color.
    #[test]
    fn the_contrast_pass_keeps_the_themes_surfaces_and_passing_roles() {
        for (name, text) in SHIPPED {
            let c = OmarchyColors::parse(text).unwrap();
            let (before, after) = (derive(&c), palette(&c));
            for role in ["canvas", "sidebar", "card", "raised", "background", "selection", "toolbar", "workflow_badge"] {
                let get = |p: &Palette| p.roles().into_iter().find(|(n, _)| *n == role).unwrap().1;
                assert_eq!(get(&before), get(&after), "{name}: {role} moved");
            }
            assert_eq!(after.canvas.rgb, c.background, "{name}");
        }
        // Tokyo Night's body text passes as Omarchy draws it.
        let c = colors("tokyo-night");
        assert_eq!(palette(&c).foreground.rgb, c.foreground);
    }

    #[test]
    fn every_pairing_names_a_role_in_front() {
        let p = palette(&colors("tokyo-night"));
        let roles: Vec<_> = p.roles().into_iter().map(|(n, _)| n).collect();
        for x in pairing_list(&p) {
            assert!(roles.contains(&x.fg_role()), "{}: {}", x.what, x.fg_role());
        }
    }

    #[test]
    fn the_terminal_follows_omarchys_template() {
        let c = colors("tokyo-night");
        let term = SystemTheme::from_colors(&c).terminal;
        let t = |rgb: Rgb| TermRgb::new(rgb.r, rgb.g, rgb.b);
        assert_eq!(term.background, t(c.background));
        assert_eq!(term.foreground, t(c.foreground));
        assert_eq!(term.cursor, t(c.bright_foreground));
        assert_eq!(term.ansi[0], t(c.background));
        assert_eq!(term.ansi[1], t(c.red));
        assert_eq!(term.ansi[8], t(c.muted));
        assert_eq!(term.ansi[15], t(c.bright_foreground));
    }

    #[test]
    fn selection_is_a_tint_that_paints_omarchys_selection() {
        for (name, text) in SHIPPED {
            let c = OmarchyColors::parse(text).unwrap();
            let painted = tint_over(c.selection, c.background).over(c.background);
            for (a, b) in [(painted.r, c.selection.r), (painted.g, c.selection.g), (painted.b, c.selection.b)] {
                assert!(a.abs_diff(b) <= 1, "{name}: {painted:?} vs {:?}", c.selection);
            }
        }
    }

    /// A switch (Omarchy's swap of the whole theme directory) recolors the
    /// app on the next poll, the font follows, and Omarchy going away brings
    /// the built-in palette back.
    #[gpui::test]
    fn a_theme_switch_recolors_the_app_on_the_next_poll(cx: &mut gpui::TestAppContext) {
        use crate::theme::{hsla, Theme};
        use gpui::WindowAppearance;
        use shelbi_app::theme::ThemePreference;

        let dir = std::env::temp_dir().join(format!("shelbi-omarchy-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let switch_to = |name: &str| {
            let next = dir.join("next-theme");
            std::fs::create_dir_all(&next).unwrap();
            let text = SHIPPED.iter().find(|(n, _)| *n == name).unwrap().1;
            std::fs::write(next.join("colors.toml"), text).unwrap();
            let _ = std::fs::remove_dir_all(dir.join("theme"));
            std::fs::rename(&next, dir.join("theme")).unwrap();
            std::fs::write(dir.join("theme.name"), name).unwrap();
        };
        switch_to("tokyo-night");
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Auto, WindowAppearance::Dark));
            gpui_kit::init(cx);
            crate::elements::terminal::TerminalSettings::init(cx);
            watch(dir.clone(), None, || Some("Omarchy Mono".into()), cx);
        });
        let canvas = |cx: &mut gpui::TestAppContext| cx.update(|cx| cx.global::<Theme>().canvas);
        cx.run_until_parked();
        assert_eq!(canvas(cx), hsla(Rgba::hex(0x1a1b26)));
        assert_eq!(current_name().as_deref(), Some("tokyo-night"));
        assert_eq!(font().as_deref(), Some("Omarchy Mono"));

        switch_to("catppuccin-latte");
        cx.executor().advance_clock(POLL);
        cx.run_until_parked();
        assert_eq!(canvas(cx), hsla(Rgba::hex(0xeff1f5)));
        assert!(!cx.update(|cx| cx.global::<Theme>().is_dark()));

        std::fs::remove_dir_all(dir.join("theme")).unwrap();
        cx.executor().advance_clock(POLL);
        cx.run_until_parked();
        let builtin = Theme::resolve(ThemePreference::Auto, WindowAppearance::Dark);
        assert_eq!(cx.update(|cx| *cx.global::<Theme>()), builtin);
        assert_eq!(font(), None, "no Omarchy, no Omarchy font");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_the_first_fc_match_family() {
        assert_eq!(first_family("JetBrainsMono Nerd Font,JetBrainsMono NF\n").as_deref(), Some("JetBrainsMono Nerd Font"));
        assert_eq!(first_family("\n"), None);
    }

    #[test]
    fn loads_a_state_directory() {
        let dir = std::env::temp_dir().join(format!("shelbi-omarchy-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("theme")).unwrap();
        assert!(load(&dir).is_none(), "no colors.toml is off Omarchy");
        std::fs::write(dir.join("theme/colors.toml"), SHIPPED[0].1).unwrap();
        std::fs::write(dir.join("theme.name"), "catppuccin\n").unwrap();
        let (name, theme) = load(&dir).unwrap();
        assert_eq!(name, "catppuccin");
        assert_eq!(theme.palette.canvas.rgb, Rgb::hex(0x1e1e2e));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state_dir().is_none(), "tests never read the machine's theme");
    }
}
