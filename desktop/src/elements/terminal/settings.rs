//! The terminal's settings: font family and size, and which Option keys are
//! Meta. Read from `~/.shelbi/config.yaml` (`desktop.terminal_font_family`,
//! `desktop.terminal_font_size`, `desktop.option_as_meta`) and held
//! as a gpui [`Global`]; the settings view sets the global after a save, and
//! every pane observes it, so a change applies to open terminals at once.

use gpui::{App, Global};
use shelbi_state::{DesktopConfig, OptionAsMeta};

use super::metrics::{DEFAULT_FONT_SIZE, PRIMARY_FAMILY};

/// The smallest and largest font size the terminal accepts (points), the
/// same range the settings view validates.
pub const FONT_SIZE_RANGE: std::ops::RangeInclusive<u16> = 6..=72;

/// The terminal settings in effect.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSettings {
    /// The font family (Geist Mono unless set; a family that isn't installed
    /// falls back to Geist Mono when the font is measured).
    pub font_family: String,
    /// The font size in points.
    pub font_size: f32,
    /// Which Option keys are Meta for character keys (Option+B sends
    /// `ESC b`) rather than typing Option characters and dead keys. Option
    /// with a named key (Enter, arrows, Backspace, Delete) is Meta whatever
    /// this says.
    pub option_as_meta: OptionAsMeta,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: PRIMARY_FAMILY.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            option_as_meta: OptionAsMeta::Off,
        }
    }
}

impl Global for TerminalSettings {}

impl TerminalSettings {
    /// The settings `config.yaml`'s `desktop:` block asks for: blank or
    /// missing keys take the defaults, an out-of-range size is clamped.
    pub fn from_config(cfg: &DesktopConfig) -> Self {
        let defaults = Self::default();
        Self {
            font_family: cfg
                .terminal_font_family
                .as_deref()
                .map(str::trim)
                .filter(|f| !f.is_empty())
                .map_or(defaults.font_family, str::to_string),
            font_size: cfg.terminal_font_size.map_or(defaults.font_size, |s| {
                s.clamp(*FONT_SIZE_RANGE.start(), *FONT_SIZE_RANGE.end()) as f32
            }),
            option_as_meta: cfg.option_as_meta.unwrap_or(defaults.option_as_meta),
        }
    }

    /// The settings in effect (the defaults until the config is read).
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }

    /// Install the defaults, then read the config off the UI thread and
    /// install what it says.
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
        let read = cx
            .background_executor()
            .spawn(async { shelbi_state::load_user_config() });
        cx.spawn(async move |cx| {
            match read.await {
                Ok(cfg) => {
                    let settings = Self::from_config(&cfg.desktop);
                    cx.update(|cx| Self::set(settings, cx));
                }
                Err(e) => tracing::warn!(error = %e, "could not read the terminal settings"),
            }
        })
        .detach();
    }

    /// Install `settings` if they differ (observers re-measure the font).
    pub fn set(settings: Self, cx: &mut App) {
        if cx.try_global::<Self>() != Some(&settings) {
            cx.set_global(settings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_geist_mono_13_and_option_types_characters() {
        let s = TerminalSettings::from_config(&DesktopConfig::default());
        assert_eq!(s, TerminalSettings::default());
        assert_eq!(s.font_family, "Geist Mono");
        assert_eq!(s.font_size, 13.0);
        assert_eq!(s.option_as_meta, OptionAsMeta::Off);
    }

    #[test]
    fn config_values_apply_blank_family_is_default_and_size_clamps() {
        let cfg = DesktopConfig {
            terminal_font_family: Some("  Berkeley Mono ".into()),
            terminal_font_size: Some(15),
            option_as_meta: Some(OptionAsMeta::Right),
            ..DesktopConfig::default()
        };
        let s = TerminalSettings::from_config(&cfg);
        assert_eq!(
            (s.font_family.as_str(), s.font_size, s.option_as_meta),
            ("Berkeley Mono", 15.0, OptionAsMeta::Right)
        );
        let cfg = DesktopConfig {
            terminal_font_family: Some("   ".into()),
            terminal_font_size: Some(400),
            ..DesktopConfig::default()
        };
        let s = TerminalSettings::from_config(&cfg);
        assert_eq!((s.font_family.as_str(), s.font_size), ("Geist Mono", 72.0));
    }
}
