//! The terminal's settings: font family and size, which Option keys are
//! Meta, the ANSI colors the user changed, and when the scrollbar shows.
//! Read from `~/.shelbi/config.yaml` (`desktop.terminal_font_family`,
//! `desktop.terminal_font_size`, `desktop.option_as_meta`,
//! `desktop.terminal_colors`, `desktop.terminal_scrollbar`) and held
//! as a gpui [`Global`]; the settings view sets the global after a save, and
//! every pane observes it, so a change applies to open terminals at once.
//!
//! View ▸ Bigger / Smaller / Actual Size (⌘+ ⌘− ⌘0) step the size through
//! [`TerminalSettings::zoom`]: the global changes at once (every pane
//! re-measures and resizes its PTY) and the new size is written to the same
//! `desktop.terminal_font_size` key the settings view edits.

use std::sync::Arc;

use gpui::{App, Global};
use shelbi_state::{DesktopConfig, OptionAsMeta, TerminalScrollbar};

use super::colors::{self, AnsiOverrides};
use super::metrics::{DEFAULT_FONT_SIZE, PRIMARY_FAMILY};

/// The smallest and largest font size the terminal accepts (points), for a
/// `config.yaml` edited by hand.
pub const FONT_SIZE_RANGE: std::ops::RangeInclusive<u16> = 6..=72;

/// The range ⌘+ and ⌘− and the settings view's size stepper step within
/// (points). A size set in `config.yaml` by hand may lie outside it; a zoom
/// step from there lands back inside, or does nothing when it would move
/// the wrong way.
pub const ZOOM_RANGE: std::ops::RangeInclusive<u16> = 8..=32;

/// One font zoom step: View ▸ Bigger (⌘+), Smaller (⌘−), Actual Size (⌘0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontZoom {
    Bigger,
    Smaller,
    ActualSize,
}

impl FontZoom {
    /// The `desktop.terminal_font_size` after this step from `setting` (what
    /// the config sets now, `None` for the default), or `None` when the step
    /// changes nothing (a range end, or Actual Size with no size set). Actual
    /// Size clears the setting rather than writing the default, so the
    /// default can move later.
    pub fn apply(self, setting: Option<u16>) -> Option<Option<u16>> {
        let current = setting.map_or(DEFAULT_FONT_SIZE as u16, |s| {
            s.clamp(*FONT_SIZE_RANGE.start(), *FONT_SIZE_RANGE.end())
        });
        let (lo, hi) = (*ZOOM_RANGE.start(), *ZOOM_RANGE.end());
        let next = match self {
            FontZoom::ActualSize => return setting.is_some().then_some(None),
            FontZoom::Bigger => (current + 1).clamp(lo, hi),
            FontZoom::Smaller => current.saturating_sub(1).clamp(lo, hi),
        };
        let moved = match self {
            FontZoom::Bigger => next > current,
            _ => next < current,
        };
        moved.then_some(Some(next))
    }
}

/// The terminal settings in effect.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalSettings {
    /// The font family (Source Code Pro unless set; a family that isn't
    /// installed falls back to Source Code Pro when the font is measured).
    pub font_family: String,
    /// The font size in points.
    pub font_size: f32,
    /// The size `config.yaml` sets (`desktop.terminal_font_size`), or `None`
    /// when it is left to the default. `font_size` is this clamped to
    /// [`FONT_SIZE_RANGE`], or the default. The settings view shows it.
    pub font_size_setting: Option<u16>,
    /// Which Option keys are Meta for character keys (Option+B sends
    /// `ESC b`) rather than typing Option characters and dead keys. Option
    /// with a named key (Enter, arrows, Backspace, Delete) is Meta whatever
    /// this says.
    pub option_as_meta: OptionAsMeta,
    /// The ANSI colors the user changed, over the theme's
    /// ([`super::colors`]).
    pub ansi: AnsiOverrides,
    /// When the scrollbar shows: following the system's "Show scroll
    /// bars" (the default), only while scrolling, or always
    /// ([`super::scrollbar`]).
    pub scrollbar: TerminalScrollbar,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: PRIMARY_FAMILY.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            font_size_setting: None,
            option_as_meta: OptionAsMeta::Off,
            ansi: [None; 16],
            scrollbar: TerminalScrollbar::Auto,
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
            font_size_setting: cfg.terminal_font_size,
            option_as_meta: cfg.option_as_meta.unwrap_or(defaults.option_as_meta),
            ansi: colors::from_config(&cfg.terminal_colors),
            scrollbar: cfg.terminal_scrollbar.unwrap_or_default(),
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

    /// Step the font size: every open terminal takes the new size at once,
    /// and it is saved to `config.yaml`. Returns false when the step changes
    /// nothing (a range end).
    pub fn zoom(step: FontZoom, cx: &mut App) -> bool {
        let current = Self::current(cx);
        let Some(setting) = step.apply(current.font_size_setting) else {
            return false;
        };
        let font_size = setting.map_or(DEFAULT_FONT_SIZE, |s| s as f32);
        Self::set(
            Self {
                font_size,
                font_size_setting: setting,
                ..current
            },
            cx,
        );
        SizeWriter::save(setting, cx);
        true
    }
}

/// Writes a zoomed size to `desktop.terminal_font_size`, one write at a
/// time off the UI thread: presses that come faster than the writes coalesce,
/// and the last one lands.
pub struct SizeWriter {
    /// The size still to write.
    pending: Option<Option<u16>>,
    writing: bool,
    write: Arc<dyn Fn(Option<u16>) -> Result<(), String> + Send + Sync>,
}

impl Global for SizeWriter {}

impl Default for SizeWriter {
    fn default() -> Self {
        // Unit tests never write the real config.yaml; a test that checks
        // the writes installs its own (`SizeWriter::install`).
        #[cfg(test)]
        let write = |_: Option<u16>| Ok(());
        #[cfg(not(test))]
        let write = |size: Option<u16>| {
            shelbi_state::update_user_config(|cfg| cfg.desktop.terminal_font_size = size)
                .map(drop)
                .map_err(|e| e.to_string())
        };
        Self {
            pending: None,
            writing: false,
            write: Arc::new(write),
        }
    }
}

impl SizeWriter {
    /// Write sizes with `write` instead of to `config.yaml`.
    #[cfg(test)]
    pub fn install(
        write: impl Fn(Option<u16>) -> Result<(), String> + Send + Sync + 'static,
        cx: &mut App,
    ) {
        cx.set_global(Self {
            write: Arc::new(write),
            ..Self::default()
        });
    }

    fn save(size: Option<u16>, cx: &mut App) {
        let writer = cx.default_global::<Self>();
        writer.pending = Some(size);
        if writer.writing {
            return;
        }
        writer.writing = true;
        cx.spawn(async move |cx| loop {
            let next = cx.update(|cx| {
                let writer = cx.global_mut::<Self>();
                let size = writer.pending.take();
                writer.writing = size.is_some();
                size.map(|size| (size, writer.write.clone()))
            });
            let Some((size, write)) = next else {
                break;
            };
            let result = cx.background_executor().spawn(async move { write(size) }).await;
            if let Err(error) = result {
                tracing::warn!(%error, "could not save the terminal font size");
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_source_code_pro_14_and_option_types_characters() {
        let s = TerminalSettings::from_config(&DesktopConfig::default());
        assert_eq!(s, TerminalSettings::default());
        assert_eq!(s.font_family, "Source Code Pro");
        assert_eq!(s.font_size, 14.0);
        assert_eq!(s.option_as_meta, OptionAsMeta::Off);
        assert_eq!(s.scrollbar, TerminalScrollbar::Auto);
    }

    #[test]
    fn config_values_apply_blank_family_is_default_and_size_clamps() {
        let cfg = DesktopConfig {
            terminal_font_family: Some("  Berkeley Mono ".into()),
            terminal_font_size: Some(15),
            option_as_meta: Some(OptionAsMeta::Right),
            terminal_scrollbar: Some(TerminalScrollbar::Always),
            ..DesktopConfig::default()
        };
        let s = TerminalSettings::from_config(&cfg);
        assert_eq!(
            (s.font_family.as_str(), s.font_size, s.option_as_meta, s.scrollbar),
            ("Berkeley Mono", 15.0, OptionAsMeta::Right, TerminalScrollbar::Always)
        );
        let cfg = DesktopConfig {
            terminal_font_family: Some("   ".into()),
            terminal_font_size: Some(400),
            ..DesktopConfig::default()
        };
        let s = TerminalSettings::from_config(&cfg);
        assert_eq!((s.font_family.as_str(), s.font_size), ("Source Code Pro", 72.0));
    }

    #[test]
    fn zoom_steps_a_point_within_8_to_32_and_actual_size_clears_the_setting() {
        use FontZoom::*;
        // From the default (no setting): 14 -> 15 / 13.
        assert_eq!(Bigger.apply(None), Some(Some(15)));
        assert_eq!(Smaller.apply(None), Some(Some(13)));
        assert_eq!(Bigger.apply(Some(20)), Some(Some(21)));
        // The ends: no change, no error.
        assert_eq!(Bigger.apply(Some(32)), None);
        assert_eq!(Smaller.apply(Some(8)), None);
        assert_eq!(Bigger.apply(Some(31)), Some(Some(32)));
        assert_eq!(Smaller.apply(Some(9)), Some(Some(8)));
        // A size set in Settings outside the range: a step toward the range
        // lands inside it, a step away does nothing.
        assert_eq!(Smaller.apply(Some(40)), Some(Some(32)));
        assert_eq!(Bigger.apply(Some(40)), None);
        assert_eq!(Bigger.apply(Some(6)), Some(Some(8)));
        assert_eq!(Smaller.apply(Some(6)), None);
        // Actual Size clears an override (even one equal to the default) and
        // is a no-op without one.
        assert_eq!(ActualSize.apply(Some(20)), Some(None));
        assert_eq!(ActualSize.apply(Some(14)), Some(None));
        assert_eq!(ActualSize.apply(None), None);
    }

    #[gpui::test]
    fn zoom_applies_at_once_and_saves_the_last_size(cx: &mut gpui::TestAppContext) {
        use std::sync::Mutex;
        let writes = Arc::new(Mutex::new(Vec::new()));
        let log = writes.clone();
        cx.update(|cx| {
            cx.set_global(TerminalSettings::default());
            SizeWriter::install(
                move |size| {
                    log.lock().unwrap().push(size);
                    Ok(())
                },
                cx,
            );
        });
        let zoom = |step, cx: &mut gpui::TestAppContext| {
            cx.update(|cx| TerminalSettings::zoom(step, cx))
        };
        let size = |cx: &mut gpui::TestAppContext| {
            cx.update(|cx| {
                let s = TerminalSettings::current(cx);
                (s.font_size, s.font_size_setting)
            })
        };

        assert!(zoom(FontZoom::Bigger, cx));
        assert!(zoom(FontZoom::Bigger, cx));
        // The global moves before any write lands.
        assert_eq!(size(cx), (16.0, Some(16)));
        cx.run_until_parked();
        // Presses coalesce; the last size is what's saved.
        assert_eq!(writes.lock().unwrap().last(), Some(&Some(16)));

        assert!(zoom(FontZoom::ActualSize, cx));
        assert_eq!(size(cx), (14.0, None));
        cx.run_until_parked();
        assert_eq!(writes.lock().unwrap().last(), Some(&None));
        // Nothing to reset: no change, no write.
        let count = writes.lock().unwrap().len();
        assert!(!zoom(FontZoom::ActualSize, cx));
        cx.run_until_parked();
        assert_eq!(writes.lock().unwrap().len(), count);
    }
}
