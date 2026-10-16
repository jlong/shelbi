//! The TUI's colors: the shared tokens ([`shelbi_app::theme`]) as ratatui
//! colors.
//!
//! The theme is chosen once at startup from `~/.shelbi/config.yaml::theme`
//! ([`install_from_config`]); every surface reads it through [`palette`]
//! (`palette().gray`, `palette().selection`, ...), never a color literal.
//! `auto` is dark here, because a terminal doesn't report its appearance.
//!
//! A token with a terminal palette slot ([`Swatch::term`]) converts to that
//! slot (`Color::DarkGray`, `Color::Indexed(236)`), so the dark theme paints
//! exactly what the TUI always has and follows the user's terminal colors.

use std::cell::Cell;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;

use ratatui::style::Color;
use shelbi_app::theme::{Ansi, Appearance, Swatch, TermColor, Theme, ThemePreference};

/// The theme as ratatui colors, one field per shared token.
pub type Palette = Theme<Color>;

/// One token as a ratatui color: its terminal palette slot when it has one,
/// else its exact RGB.
pub fn to_color(swatch: Swatch) -> Color {
    match swatch.term {
        Some(TermColor::Ansi(ansi)) => ansi_color(ansi),
        Some(TermColor::Indexed(index)) => Color::Indexed(index),
        None => Color::Rgb(swatch.rgb.r, swatch.rgb.g, swatch.rgb.b),
    }
}

fn ansi_color(ansi: Ansi) -> Color {
    match ansi {
        Ansi::Black => Color::Black,
        Ansi::Red => Color::Red,
        Ansi::Green => Color::Green,
        Ansi::Yellow => Color::Yellow,
        Ansi::Blue => Color::Blue,
        Ansi::Magenta => Color::Magenta,
        Ansi::Cyan => Color::Cyan,
        Ansi::Gray => Color::Gray,
        Ansi::DarkGray => Color::DarkGray,
        Ansi::LightRed => Color::LightRed,
        Ansi::LightGreen => Color::LightGreen,
        Ansi::LightYellow => Color::LightYellow,
        Ansi::LightBlue => Color::LightBlue,
        Ansi::LightMagenta => Color::LightMagenta,
        Ansi::LightCyan => Color::LightCyan,
        Ansi::White => Color::White,
    }
}

static DARK_TOKENS: LazyLock<Theme> = LazyLock::new(Theme::dark);
static LIGHT_TOKENS: LazyLock<Theme> = LazyLock::new(Theme::light);
static DARK: LazyLock<Palette> = LazyLock::new(|| DARK_TOKENS.map(to_color));
static LIGHT: LazyLock<Palette> = LazyLock::new(|| LIGHT_TOKENS.map(to_color));

/// The appearance chosen at startup. Unset (tests, or before startup) is dark.
static INSTALLED: OnceLock<Appearance> = OnceLock::new();

thread_local! {
    /// A per-thread override, so a test can render one surface in light
    /// without touching the process-wide choice other tests read.
    static OVERRIDE: Cell<Option<Appearance>> = const { Cell::new(None) };
}

/// Choose the theme for this process from a `theme:` setting. The first call
/// wins; later calls are ignored (the TUI picks its theme once, at startup).
pub fn install(pref: ThemePreference) -> Appearance {
    // A terminal can't report its appearance, so `auto` resolves to dark.
    let appearance = Theme::for_preference(pref, None).appearance;
    *INSTALLED.get_or_init(|| appearance)
}

/// [`install`] from `~/.shelbi/config.yaml::theme`. A missing or unreadable
/// config is `auto`.
pub fn install_from_config() -> Appearance {
    let pref = shelbi_state::load_user_config()
        .map(|c| c.theme)
        .unwrap_or_default();
    install(pref)
}

fn appearance() -> Appearance {
    OVERRIDE
        .with(Cell::get)
        .or_else(|| INSTALLED.get().copied())
        .unwrap_or(Appearance::Dark)
}

/// The resolved theme's colors.
pub fn palette() -> &'static Palette {
    match appearance() {
        Appearance::Dark => &DARK,
        Appearance::Light => &LIGHT,
    }
}

/// The resolved theme's tokens (exact values), for the colors the TUI
/// computes rather than reads (the project pulse).
pub fn tokens() -> &'static Theme {
    match appearance() {
        Appearance::Dark => &DARK_TOKENS,
        Appearance::Light => &LIGHT_TOKENS,
    }
}

/// Run `f` with this thread's theme forced to `appearance` (tests render a
/// surface in light this way).
#[cfg(test)]
pub(crate) fn with_appearance<R>(appearance: Appearance, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Appearance>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|o| o.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|o| o.replace(Some(appearance))));
    f()
}

/// Light-theme audit for render tests: render a surface inside
/// `with_appearance(Appearance::Light, ..)`, then hand its buffer here.
///
/// Fails when a painted cell leans on the terminal palette (a named or
/// indexed color, which a light terminal remaps unpredictably) or when a
/// text cell's foreground falls under 3:1 against what it sits on (its own
/// background, else the light canvas, standing in for a light terminal's
/// default). 3:1 is the floor for every glyph on screen; the 4.5:1 text bar
/// is checked per token in `shelbi_app::theme`. Block glyphs (`▄▀█▏` and the
/// rest of U+2580..U+259F) are shapes painted in their fill color, so they're
/// exempt from the contrast check.
///
/// With `SHELBI_THEME_DUMP=<dir>` set, also writes `<dir>/<name>.html`, the
/// buffer drawn on the light canvas, for checking a surface by eye.
#[cfg(test)]
pub(crate) fn assert_light_readable(name: &str, buf: &ratatui::buffer::Buffer) {
    use shelbi_app::theme::Rgb;

    let light = &*LIGHT_TOKENS;
    let rgb_of = |c: Color, default: Rgb| -> Result<Rgb, String> {
        match c {
            Color::Rgb(r, g, b) => Ok(Rgb::new(r, g, b)),
            Color::Reset => Ok(default),
            other => Err(format!("{other:?}")),
        }
    };
    let (canvas, ink) = (light.canvas.rgb, light.foreground.rgb);
    let mut problems = Vec::new();
    for y in buf.area.top()..buf.area.bottom() {
        for x in buf.area.left()..buf.area.right() {
            let cell = &buf[(x, y)];
            let (fg, bg) = match (rgb_of(cell.fg, ink), rgb_of(cell.bg, canvas)) {
                (Ok(fg), Ok(bg)) => (fg, bg),
                (fg, bg) => {
                    problems.push(format!(
                        "({x},{y}) {:?}: terminal-palette color fg={fg:?} bg={bg:?}",
                        cell.symbol()
                    ));
                    continue;
                }
            };
            let symbol = cell.symbol();
            let is_text = symbol
                .chars()
                .any(|c| !c.is_whitespace() && !('\u{2580}'..='\u{259f}').contains(&c));
            if is_text && !cell.modifier.contains(ratatui::style::Modifier::DIM) {
                let ratio = fg.contrast_ratio(bg);
                if ratio < 3.0 {
                    problems.push(format!(
                        "({x},{y}) {symbol:?}: fg {fg:?} on bg {bg:?} is {ratio:.2}:1"
                    ));
                }
            }
        }
    }
    if let Ok(dir) = std::env::var("SHELBI_THEME_DUMP") {
        dump_html(std::path::Path::new(&dir), name, buf, canvas, ink);
    }
    assert!(
        problems.is_empty(),
        "{name} is not readable in the light theme:\n{}",
        problems.join("\n")
    );
}

/// Write `buf` as an HTML page (cells as styled spans on the light canvas).
#[cfg(test)]
fn dump_html(
    dir: &std::path::Path,
    name: &str,
    buf: &ratatui::buffer::Buffer,
    canvas: shelbi_app::theme::Rgb,
    ink: shelbi_app::theme::Rgb,
) {
    use ratatui::style::Modifier;
    use std::fmt::Write as _;

    let css = |c: Color, default: shelbi_app::theme::Rgb| match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => format!("#{:02x}{:02x}{:02x}", default.r, default.g, default.b),
    };
    let mut html = format!(
        "<!doctype html><meta charset=utf-8><title>{name}</title><body style=\"margin:0;\
         padding:16px;background:{bg}\"><pre style=\"font:14px/1.2 Menlo,monospace;\
         margin:0;color:{fg}\">",
        bg = css(Color::Reset, canvas),
        fg = css(Color::Reset, ink),
    );
    for y in buf.area.top()..buf.area.bottom() {
        for x in buf.area.left()..buf.area.right() {
            let cell = &buf[(x, y)];
            let (mut fg, mut bg) = (css(cell.fg, ink), css(cell.bg, canvas));
            if cell.modifier.contains(Modifier::REVERSED) {
                // The terminal swaps the pair (Reset ends up as its default bg
                // under the glyph), so draw it the way it shows on screen.
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut style = format!("color:{fg};background:{bg}");
            if cell.modifier.contains(Modifier::BOLD) {
                style.push_str(";font-weight:bold");
            }
            if cell.modifier.contains(Modifier::DIM) {
                style.push_str(";opacity:.5");
            }
            let symbol = match cell.symbol() {
                "<" => "&lt;",
                ">" => "&gt;",
                "&" => "&amp;",
                s => s,
            };
            let _ = write!(html, "<span style=\"{style}\">{symbol}</span>");
        }
        html.push('\n');
    }
    html.push_str("</pre>");
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("{name}.html")), html);
}

/// The glyph painted down the sidebar's rightmost column as the resize
/// drag handle and right-edge divider — a full-height left one-eighth block
/// (`▏`, the Figma `color/divider` rule) that reads as a thin border. Colored
/// `divider` at rest and `divider_active` (bold) while hovered or dragged.
pub const DIVIDER_GLYPH: &str = "▏";

/// Full breathing period of the active-project pulse: the fill eases from a
/// dim trough (near-transparent against the palette panel) up to full green
/// and back down once per cycle. Kept close to a calm breath rather than a
/// blink.
pub const PROJECT_PULSE_PERIOD: Duration = Duration::from_millis(1143);

/// Fill color for the active-project pulse at animation `phase` (wrapped into
/// `[0.0, 1.0)`), breathing between the theme's `pulse_trough` and
/// `pulse_peak` (see [`Theme::project_pulse`]). The glyph is a filled disc, so
/// the ring stays green throughout: "green ring + cycling fill".
pub fn project_pulse_color(phase: f32) -> Color {
    let rgb = tokens().project_pulse(phase);
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_converts_to_the_tuis_long_standing_colors() {
        // The dark rendering is byte-identical to the pre-token TUI: every
        // token converts to the exact ratatui color the TUI painted before.
        let p = Theme::dark().map(to_color);
        assert_eq!(p.background, Color::Rgb(0, 0, 0));
        assert_eq!(p.gray, Color::Rgb(186, 186, 186));
        assert_eq!(p.white, Color::Rgb(255, 255, 255));
        assert_eq!(p.muted, Color::Rgb(122, 122, 122));
        assert_eq!(p.cyan, Color::Rgb(0, 166, 178));
        assert_eq!(p.green, Color::Rgb(90, 205, 37));
        assert_eq!(p.selection, Color::Rgb(63, 63, 63));
        assert_eq!(p.search, Color::Rgb(41, 41, 41));
        assert_eq!(p.foreground, Color::Rgb(198, 198, 198));
        assert_eq!(p.red, Color::Rgb(224, 79, 82));
        assert_eq!(p.yellow, Color::Rgb(0xdb, 0xc3, 0x00));
        assert_eq!(p.divider, Color::Rgb(65, 65, 65));
        assert_eq!(p.zen, Color::Rgb(0, 127, 0));
        assert_eq!(p.divider_active, Color::Cyan);
        assert_eq!(p.workflow_badge_bg, Color::Rgb(58, 66, 88));
        assert_eq!(p.workflow_badge_fg, Color::Rgb(220, 223, 232));
        assert_eq!(p.on_color, Color::White);
        assert_eq!(p.error_button, Color::Rgb(110, 70, 70));
        assert_eq!(p.error_marker, Color::Rgb(210, 120, 120));
        assert_eq!(p.notice, Color::Rgb(90, 70, 30));
        assert_eq!(p.status_bar, Color::Rgb(40, 40, 50));
        assert_eq!(p.code_bg, Color::Indexed(236));
        assert_eq!(p.code_fg, Color::Indexed(252));
        assert_eq!(p.ansi_white, Color::White);
        assert_eq!(p.ansi_gray, Color::Gray);
        assert_eq!(p.ansi_dark_gray, Color::DarkGray);
        assert_eq!(p.ansi_red, Color::Red);
        assert_eq!(p.ansi_light_red, Color::LightRed);
        assert_eq!(p.ansi_green, Color::Green);
        assert_eq!(p.ansi_yellow, Color::Yellow);
        assert_eq!(p.ansi_blue, Color::Blue);
        assert_eq!(p.ansi_magenta, Color::Magenta);
        assert_eq!(p.ansi_cyan, Color::Cyan);
    }

    #[test]
    fn light_is_all_exact_rgb() {
        for (name, color) in Theme::light().map(to_color).tokens() {
            assert!(
                matches!(color, Color::Rgb(..)),
                "light {name} must not lean on the terminal palette: {color:?}"
            );
        }
    }

    #[test]
    fn unset_theme_is_dark_and_override_is_per_thread() {
        assert_eq!(palette(), &*DARK);
        with_appearance(Appearance::Light, || {
            assert_eq!(palette(), &*LIGHT);
            // Another thread still sees the process-wide (dark) choice.
            let other = std::thread::spawn(|| palette() == &*DARK).join().unwrap();
            assert!(other);
        });
        assert_eq!(palette(), &*DARK, "override restored");
    }

    #[test]
    fn pulse_troughs_dim_and_peaks_full_green() {
        // phase 0 and 1.0 (wrap) sit at the trough; phase 0.5 at the peak.
        let trough = project_pulse_color(0.0);
        let peak = project_pulse_color(0.5);
        let wrap = project_pulse_color(1.0);
        assert_eq!(trough, wrap, "phase wraps cleanly at 1.0");
        match (trough, peak) {
            (Color::Rgb(0, lo, 0), Color::Rgb(0, hi, 0)) => {
                assert!(lo < hi, "peak green channel must exceed the trough");
                assert!(lo > 0, "trough stays a dim green, not fully black");
                assert!(hi >= 200, "peak reads as full green");
            }
            other => panic!("pulse must be a pure-green Rgb, got {other:?}"),
        }
    }

    #[test]
    fn pulse_is_symmetric_around_the_peak() {
        // The raised cosine is symmetric: a quarter before and after the peak
        // land on the same fill, so the breath in and out match.
        assert_eq!(project_pulse_color(0.25), project_pulse_color(0.75));
    }

    #[test]
    fn pulse_matches_the_pre_token_formula() {
        // The pulse used to be computed inline here; the shared lerp must land
        // on the same green channel at every phase.
        for i in 0..=100 {
            let phase = i as f32 / 100.0;
            let alpha = 0.5 - 0.5 * (phase.rem_euclid(1.0) * std::f32::consts::TAU).cos();
            let g = (45.0 + (220.0 - 45.0) * alpha).round().clamp(0.0, 255.0) as u8;
            assert_eq!(project_pulse_color(phase), Color::Rgb(0, g, 0), "phase {phase}");
        }
    }
}
