//! The visual runs' shared setup (feature `visual-check`): the output
//! directory, the once-per-theme re-run and the theme a run paints with.
//! Cross-platform (unlike the offscreen modes, which need gpui's
//! macOS-only `VisualTestAppContext`), because `--visual-figma-fixtures`
//! writes the run's theme into its `config.yaml`.

use std::path::{Path, PathBuf};

use shelbi_app::theme::ThemePreference;

use crate::theme::Theme;

/// Parse `--visual-check <dir>` (and the other `--visual-*` modes' dir).
///
/// Every visual run renders in both themes: with `SHELBI_VISUAL_THEME`
/// unset, this runs the same command twice as child processes, once per
/// theme (`dark`, then `light`), and exits with the combined result; each
/// child renders into `<dir>/<theme>/` and paints with [`theme`]. Set the
/// variable to render one theme only.
pub fn out_dir(arg: Option<String>) -> PathBuf {
    let (dir, theme) = per_theme(arg);
    dir.join(theme)
}

/// [`out_dir`]'s re-run, for a mode that names its frames by theme itself
/// (`--visual-figma`'s `<screen>-<theme>.png`): the directory as given and
/// the theme this run paints.
pub fn per_theme(arg: Option<String>) -> (PathBuf, &'static str) {
    let dir = arg
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("visual-check").to_path_buf());
    if let Some(theme) = theme_name() {
        return (dir, theme);
    }
    let mut failed = false;
    for theme in ["dark", "light"] {
        println!("==== {theme} theme ====");
        let status = std::process::Command::new(std::env::current_exe().expect("own path"))
            .args(std::env::args_os().skip(1))
            .env(THEME_VAR, theme)
            .status()
            .expect("run the themed visual check");
        failed |= !status.success();
    }
    std::process::exit(if failed { 1 } else { 0 })
}

const THEME_VAR: &str = "SHELBI_VISUAL_THEME";

fn theme_name() -> Option<&'static str> {
    match std::env::var(THEME_VAR).ok()?.as_str() {
        "light" => Some("light"),
        _ => Some("dark"),
    }
}

/// Install the run's theme with gpui-kit's widgets in step, as the app does
/// (the views' harnesses; the terminal check needs only [`theme`]).
pub fn install_theme(cx: &mut gpui::App) {
    gpui_kit::init(cx);
    crate::theme::install(theme(), cx);
}

/// The theme a visual run paints with: `SHELBI_VISUAL_THEME` (dark unless
/// `light`), at `SHELBI_VISUAL_CONTRAST` percent contrast (0 unless set),
/// its surfaces at `SHELBI_VISUAL_OPACITY` percent (opaque unless set: a
/// translucent window's frames, for QA).
pub fn theme() -> Theme {
    let contrast = std::env::var("SHELBI_VISUAL_CONTRAST")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map_or(0, crate::theme::contrast::clamped);
    let theme = if theme_name() == Some("light") {
        Theme::resolve_with(ThemePreference::Light, contrast, gpui::WindowAppearance::Light)
    } else {
        Theme::resolve_with(ThemePreference::Dark, contrast, gpui::WindowAppearance::Dark)
    };
    let opacity = std::env::var("SHELBI_VISUAL_OPACITY")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map(|v| crate::theme::window::Translucency::clamped(v, 0).alpha());
    theme.with_opacity(opacity.unwrap_or(1.0))
}
