//! The settings view: the desktop's own preferences plus the shared theme.
//!
//! Stored in `~/.shelbi/config.yaml` through `shelbi_state::user_config`:
//!
//! - `desktop.terminal_font_family` / `desktop.terminal_font_size`: the
//!   terminals' font, and `desktop.option_as_meta`: which Option keys are
//!   Meta in terminals (the terminal element applies all three at once
//!   through its `TerminalSettings` global);
//! - `theme:` (`auto | dark | light`), shared with the TUI, which reads it on
//!   its next start;
//! - `desktop.cli_path`: the `shelbi` CLI override the platform bootstrap
//!   reads.
//!
//! A button opens `keys.yaml` (the key bindings both clients share) in the
//! user's default text editor.
//!
//! Saves go through [`shelbi_state::update_user_config`], which keeps every
//! key this view doesn't own (the zen chord, the review editor, a newer
//! client's settings). Every read and write runs on the background executor.

use std::path::PathBuf;

use gpui::{div, prelude::*, px, Context, Entity, FontWeight, SharedString, Subscription, Window};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use shelbi_state::{DesktopConfig, OptionAsMeta, ThemePreference, UserConfig};

use crate::elements::terminal::TerminalSettings;
use crate::theme::Theme;

/// The values the view edits, parsed from the inputs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsValues {
    pub font_family: String,
    pub font_size: String,
    pub cli_path: String,
    pub theme: ThemePreference,
    pub option_as_meta: OptionAsMeta,
}

impl SettingsValues {
    /// The values a config shows.
    pub fn from_config(cfg: &UserConfig) -> SettingsValues {
        SettingsValues {
            font_family: cfg.desktop.terminal_font_family.clone().unwrap_or_default(),
            font_size: cfg
                .desktop
                .terminal_font_size
                .map(|s| s.to_string())
                .unwrap_or_default(),
            cli_path: cfg
                .desktop
                .cli_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            theme: cfg.theme,
            option_as_meta: cfg.desktop.option_as_meta.unwrap_or_default(),
        }
    }

    /// Write these values into `cfg`. Blank fields clear their key. Errors on
    /// a font size that isn't a whole number of points in 6..=72.
    pub fn apply(&self, cfg: &mut UserConfig) -> Result<(), String> {
        let blank = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        let size = match blank(&self.font_size) {
            None => None,
            Some(s) => match s.parse::<u16>() {
                Ok(n) if (6..=72).contains(&n) => Some(n),
                _ => return Err(format!("Font size must be a number from 6 to 72, not `{s}`.")),
            },
        };
        cfg.desktop = DesktopConfig {
            cli_path: blank(&self.cli_path).map(PathBuf::from),
            terminal_font_family: blank(&self.font_family),
            terminal_font_size: size,
            // Off is the default, so it isn't written.
            option_as_meta: (self.option_as_meta != OptionAsMeta::Off).then_some(self.option_as_meta),
        };
        cfg.theme = self.theme;
        Ok(())
    }
}

/// Where the last save stands, shown under the form.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveState {
    Idle,
    Saved,
    Failed(String),
}

/// The settings view.
pub struct SettingsView {
    font_family: Entity<InputState>,
    font_size: Entity<InputState>,
    cli_path: Entity<InputState>,
    theme: ThemePreference,
    option_as_meta: OptionAsMeta,
    /// False until the config has been read, so an early blur can't save
    /// blank values over it.
    loaded: bool,
    state: SaveState,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let font_family = input("Geist Mono", window, cx);
        let font_size = input("13", window, cx);
        let cli_path = input("Found automatically", window, cx);
        let subscriptions = [&font_family, &font_size, &cli_path]
            .into_iter()
            .map(|state| {
                cx.subscribe_in(state, window, |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.save(cx);
                    }
                })
            })
            .collect();
        let mut view = SettingsView {
            font_family,
            font_size,
            cli_path,
            theme: ThemePreference::Auto,
            option_as_meta: OptionAsMeta::Off,
            loaded: false,
            state: SaveState::Idle,
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view
    }

    /// Read the config off the UI thread and fill the form.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read = cx
            .background_executor()
            .spawn(async { shelbi_state::load_user_config() });
        cx.spawn_in(window, async move |this, cx| {
            let cfg = read.await;
            this.update_in(cx, |this, window, cx| {
                match cfg {
                    Ok(cfg) => {
                        let values = SettingsValues::from_config(&cfg);
                        let set = |state: &Entity<InputState>, value: String, window: &mut Window, cx: &mut Context<Self>| {
                            state.update(cx, |s, cx| s.set_value(value, window, cx));
                        };
                        set(&this.font_family, values.font_family, window, cx);
                        set(&this.font_size, values.font_size, window, cx);
                        set(&this.cli_path, values.cli_path, window, cx);
                        this.theme = values.theme;
                        this.option_as_meta = values.option_as_meta;
                        this.loaded = true;
                    }
                    Err(e) => {
                        this.state = SaveState::Failed(format!("Couldn't read config.yaml: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn values(&self, cx: &Context<Self>) -> SettingsValues {
        SettingsValues {
            font_family: self.font_family.read(cx).value().to_string(),
            font_size: self.font_size.read(cx).value().to_string(),
            cli_path: self.cli_path.read(cx).value().to_string(),
            theme: self.theme,
            option_as_meta: self.option_as_meta,
        }
    }

    /// Validate and write the form off the UI thread.
    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.loaded {
            return;
        }
        let values = self.values(cx);
        // Validate on the UI thread so a bad value is reported without a write.
        if let Err(message) = values.apply(&mut UserConfig::default()) {
            self.state = SaveState::Failed(message);
            cx.notify();
            return;
        }
        let applied = {
            let mut cfg = UserConfig::default();
            let _ = values.apply(&mut cfg);
            TerminalSettings::from_config(&cfg.desktop)
        };
        let write = cx.background_executor().spawn(async move {
            let mut invalid = None;
            let result = shelbi_state::update_user_config(|cfg| {
                if let Err(e) = values.apply(cfg) {
                    invalid = Some(e);
                }
            });
            match (result, invalid) {
                (_, Some(e)) => Err(e),
                (Ok(_), None) => Ok(()),
                (Err(e), None) => Err(format!("Couldn't save config.yaml: {e}")),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(()) => {
                        // Open terminals pick up the font and Option key now.
                        TerminalSettings::set(applied, cx);
                        SaveState::Saved
                    }
                    Err(e) => SaveState::Failed(e),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Pick a theme: it applies to the app now and is saved for next time
    /// (and for the TUI's next start).
    fn set_theme(&mut self, theme: ThemePreference, window: &Window, cx: &mut Context<Self>) {
        self.theme = theme;
        crate::theme::apply_preference(theme, window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Pick what Option does in terminals; saved and applied at once.
    fn set_option_as_meta(&mut self, value: OptionAsMeta, cx: &mut Context<Self>) {
        self.option_as_meta = value;
        self.save(cx);
        cx.notify();
    }

    /// Open `keys.yaml` in the default text editor, creating a commented stub
    /// first when there is none (an editor won't open a missing file).
    fn open_keys(&mut self, cx: &mut Context<Self>) {
        let opened = cx.background_executor().spawn(async { open_keys_yaml() });
        cx.spawn(async move |this, cx| {
            if let Err(e) = opened.await {
                this.update(cx, |this, cx| {
                    this.state = SaveState::Failed(e);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

/// `~/.shelbi/keys.yaml`, the shared key bindings.
pub fn keys_yaml_path() -> Option<PathBuf> {
    shelbi_state::shelbi_home()
        .ok()
        .map(|home| home.join(shelbi_state::keymap::KEYS_FILENAME))
}

/// The stub a missing `keys.yaml` starts as: comments only, so it changes no
/// binding.
const KEYS_STUB: &str = "# Shelbi key bindings, shared by the TUI and the desktop app.\n\
# Override a built-in chord per mode; unlisted actions keep their defaults.\n\
# `shelbi config lint` checks this file.\n";

fn open_keys_yaml() -> Result<(), String> {
    let path = keys_yaml_path().ok_or("No Shelbi home directory.")?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, KEYS_STUB).map_err(|e| format!("Couldn't create keys.yaml: {e}"))?;
    }
    let status = std::process::Command::new("open")
        .arg("-t")
        .arg(&path)
        .status()
        .map_err(|e| format!("Couldn't open keys.yaml: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Couldn't open keys.yaml in a text editor.".into())
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let field = |label: &'static str, hint: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_color(theme.white).child(label))
                .child(div().w(px(360.0)).child(Input::new(input)))
                .child(div().text_xs().text_color(theme.muted).child(hint))
        };
        let chip = |label: &'static str, value: ThemePreference, current: ThemePreference, cx: &mut Context<Self>| {
            let selected = value == current;
            div()
                .id(label)
                .debug_selector(move || format!("settings-theme-{label}"))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| {
                    d.bg(theme.selection)
                        .text_color(theme.white)
                        .font_weight(FontWeight::BOLD)
                })
                .when(!selected, |d| d.hover(|s| s.bg(theme.search)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(value, window, cx)))
        };
        let status: Option<(SharedString, gpui::Hsla)> = match &self.state {
            SaveState::Idle => None,
            SaveState::Saved => Some(("Saved to ~/.shelbi/config.yaml".into(), theme.muted)),
            SaveState::Failed(e) => Some((e.clone().into(), theme.red)),
        };
        let current = self.theme;
        let option_as_meta = self.option_as_meta;
        let option_chip = |label: &'static str, value: OptionAsMeta, cx: &mut Context<Self>| {
            let selected = value == option_as_meta;
            div()
                .id(label)
                .debug_selector(move || format!("settings-option-{label}"))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| {
                    d.bg(theme.selection)
                        .text_color(theme.white)
                        .font_weight(FontWeight::BOLD)
                })
                .when(!selected, |d| d.hover(|s| s.bg(theme.search)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_option_as_meta(value, cx)))
        };

        div()
            .id("settings")
            .debug_selector(|| "settings".into())
            .flex()
            .flex_col()
            .gap_6()
            .size_full()
            .p_8()
            .overflow_y_scroll()
            .text_sm()
            .text_color(theme.gray)
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.white)
                    .child("Settings"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(theme.white).child("Theme"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .child(chip("Auto", ThemePreference::Auto, current, cx))
                            .child(chip("Dark", ThemePreference::Dark, current, cx))
                            .child(chip("Light", ThemePreference::Light, current, cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted)
                            .child("Shared with the TUI, which picks it up on its next start."),
                    ),
            )
            .child(field(
                "Terminal font",
                "The font family for terminals. Blank uses Geist Mono.",
                &self.font_family,
            ))
            .child(field(
                "Terminal font size",
                "In points. Blank uses the default.",
                &self.font_size,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(theme.white).child("Option key in terminals"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .child(option_chip("Types characters", OptionAsMeta::Off, cx))
                            .child(option_chip("Meta", OptionAsMeta::Both, cx))
                            .child(option_chip("Left is Meta", OptionAsMeta::Left, cx))
                            .child(option_chip("Right is Meta", OptionAsMeta::Right, cx)),
                    )
                    .child(div().text_xs().text_color(theme.muted).child(
                        "Characters: Option+E, E types é, as in other Mac apps. Meta: \
                         Option+B sends Esc B, for shell word motion and Claude Code's \
                         Option shortcuts. Option with Enter, the arrows, Backspace, or \
                         Delete is Meta either way.",
                    )),
            )
            .child(field(
                "Shelbi CLI",
                "The `shelbi` binary the app runs. Blank finds it on your login PATH, \
                 in Homebrew, or in ~/.cargo/bin. Takes effect on the next launch.",
                &self.cli_path,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_color(theme.white).child("Key bindings"))
                    .child(
                        div()
                            .id("settings-open-keys")
                            .debug_selector(|| "settings-open-keys".into())
                            .w_auto()
                            .self_start()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(theme.search)
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.selection))
                            .child("Open keys.yaml")
                            .on_click(cx.listener(|this, _, _, cx| this.open_keys(cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted)
                            .child("Shared by the TUI and the desktop app."),
                    ),
            )
            .children(status.map(|(text, color)| div().text_color(color).child(text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_the_config() {
        let values = SettingsValues {
            font_family: "Berkeley Mono".into(),
            font_size: " 14 ".into(),
            cli_path: "~/bin/shelbi".into(),
            theme: ThemePreference::Light,
            option_as_meta: OptionAsMeta::Left,
        };
        let mut cfg = UserConfig {
            editor: Some("hx".into()),
            ..Default::default()
        };
        values.apply(&mut cfg).unwrap();
        assert_eq!(cfg.desktop.terminal_font_family.as_deref(), Some("Berkeley Mono"));
        assert_eq!(cfg.desktop.terminal_font_size, Some(14));
        assert_eq!(
            cfg.desktop.cli_path.as_deref(),
            Some(std::path::Path::new("~/bin/shelbi"))
        );
        assert_eq!(cfg.theme, ThemePreference::Light);
        assert_eq!(cfg.desktop.option_as_meta, Some(OptionAsMeta::Left));
        assert_eq!(cfg.editor.as_deref(), Some("hx"), "other keys are untouched");
        let back = SettingsValues::from_config(&cfg);
        assert_eq!(back.font_size, "14");
        assert_eq!(back.font_family, "Berkeley Mono");
        assert_eq!(back.cli_path, "~/bin/shelbi");
        assert_eq!(back.option_as_meta, OptionAsMeta::Left);
    }

    #[test]
    fn blank_fields_clear_their_keys_and_bad_sizes_are_refused() {
        let mut cfg = UserConfig::default();
        cfg.desktop.terminal_font_size = Some(12);
        SettingsValues::default().apply(&mut cfg).unwrap();
        assert!(cfg.desktop.is_empty());

        for bad in ["big", "0", "200", "13.5"] {
            let values = SettingsValues {
                font_size: bad.into(),
                ..Default::default()
            };
            assert!(values.apply(&mut UserConfig::default()).is_err(), "{bad}");
        }
    }
}
