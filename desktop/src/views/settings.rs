//! The settings view: the desktop's own preferences plus the shared theme.
//!
//! Stored in `~/.shelbi/config.yaml` through `shelbi_state::user_config`:
//!
//! - `desktop.terminal_font_family` / `desktop.terminal_font_size`: the
//!   terminals' font, chosen from a picker of the bundled and installed
//!   monospaced fonts ([`FontPicker`]) and a size stepper (8 to 32 points),
//!   and `desktop.option_as_meta`: which Option keys are Meta in terminals.
//!   The terminal element applies all three at once through its
//!   `TerminalSettings` global, which a change sets at once (before the
//!   write), and which the view follows, so a size changed elsewhere shows
//!   here too;
//! - `theme:` (`auto | dark | light`), shared with the TUI, which reads it on
//!   its next start;
//! - `desktop.cli_path`: the `shelbi` CLI override the platform bootstrap
//!   reads, typed or chosen with Browse.
//!
//! - `desktop.notifications`: which system notifications the app posts.
//!
//! A button opens `keys.yaml` (the key bindings both clients share) in the
//! user's default text editor; below it, any binding macOS already owns (⌘W,
//! ⌘Space, ...) is reported, since it can't reach Shelbi.
//!
//! The rows are the Figma form components (`elements::controls`).
//!
//! Saves go through [`shelbi_state::update_user_config`], which keeps every
//! key this view doesn't own (the zen chord, the review editor, a newer
//! client's settings). Every read and write runs on the background executor.

use std::path::PathBuf;
use std::time::Instant;

use gpui::{
    div, prelude::*, px, Context, Entity, EventEmitter, Focusable, PathPromptOptions, SharedString,
    Subscription, Window,
};
use gpui_kit::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use shelbi_state::{DesktopConfig, DesktopNotifications, OptionAsMeta, ThemePreference, UserConfig};

use super::font_picker::{FontPicker, FontPickerEvent};
use crate::assets::icons;
use crate::elements::controls;
use crate::elements::terminal::metrics::{DEFAULT_FONT_SIZE, PRIMARY_FAMILY};
use crate::elements::terminal::settings::{FONT_SIZE_RANGE, ZOOM_RANGE};
use crate::elements::terminal::TerminalSettings;
use crate::menus::tree::{reserved_conflicts, ShortcutConflict};
use crate::theme::motion::{self, SharedTrack, SlideTrack};
use crate::theme::style::{Type, Typography};
use crate::theme::Theme;

/// The terminal font size a fresh install uses, in whole points.
const DEFAULT_SIZE: u16 = DEFAULT_FONT_SIZE as u16;

/// The size field's width (the frame's, room for `32pt`).
const SIZE_FIELD_W: f32 = 76.0;

/// The values the view edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsValues {
    /// The terminal font family (Source Code Pro unless set).
    pub font_family: String,
    /// The terminal font size in points: within [`ZOOM_RANGE`] when set
    /// here, anywhere in [`FONT_SIZE_RANGE`] when `config.yaml` says so.
    pub font_size: u16,
    pub cli_path: String,
    pub theme: ThemePreference,
    pub option_as_meta: OptionAsMeta,
    pub notifications: Switches,
}

/// The notification switches, as the form shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switches {
    pub workspace_blocked: bool,
    pub review_ready: bool,
    pub orchestrator_waiting: bool,
}

impl Default for Switches {
    fn default() -> Self {
        Switches::from(&DesktopNotifications::default())
    }
}

impl From<&DesktopNotifications> for Switches {
    fn from(n: &DesktopNotifications) -> Self {
        Switches {
            workspace_blocked: n.workspace_blocked(),
            review_ready: n.review_ready(),
            orchestrator_waiting: n.orchestrator_waiting(),
        }
    }
}

/// One notification switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Switch {
    WorkspaceBlocked,
    ReviewReady,
    OrchestratorWaiting,
}

impl Switches {
    fn get(&self, switch: Switch) -> bool {
        match switch {
            Switch::WorkspaceBlocked => self.workspace_blocked,
            Switch::ReviewReady => self.review_ready,
            Switch::OrchestratorWaiting => self.orchestrator_waiting,
        }
    }

    fn toggle(&mut self, switch: Switch) {
        let value = match switch {
            Switch::WorkspaceBlocked => &mut self.workspace_blocked,
            Switch::ReviewReady => &mut self.review_ready,
            Switch::OrchestratorWaiting => &mut self.orchestrator_waiting,
        };
        *value = !*value;
    }
}

impl Default for SettingsValues {
    fn default() -> Self {
        SettingsValues {
            font_family: PRIMARY_FAMILY.to_string(),
            font_size: DEFAULT_SIZE,
            cli_path: String::new(),
            theme: ThemePreference::default(),
            option_as_meta: OptionAsMeta::default(),
            notifications: Switches::default(),
        }
    }
}

/// `size` kept within [`ZOOM_RANGE`] (the stepper's range, and ⌘+ / ⌘−'s).
pub fn clamp_size(size: i32) -> u16 {
    size.clamp(*ZOOM_RANGE.start() as i32, *ZOOM_RANGE.end() as i32) as u16
}

/// The size one stepper step (`delta` points) moves `size` to, or `None`
/// when it can't move that way: at a range end, or from a hand-set size
/// outside the range that the step would only move further away from (the
/// same rule as ⌘+ / ⌘−).
pub fn step_size(size: u16, delta: i32) -> Option<u16> {
    let next = clamp_size(size as i32 + delta);
    let moved = if delta > 0 { next > size } else { next < size };
    moved.then_some(next)
}

impl SettingsValues {
    /// The values a config shows: the terminal font as the terminals use it
    /// (a blank family is Source Code Pro).
    pub fn from_config(cfg: &UserConfig) -> SettingsValues {
        let terminal = TerminalSettings::from_config(&cfg.desktop);
        SettingsValues {
            font_family: terminal.font_family,
            font_size: terminal.font_size.round() as u16,
            cli_path: cfg
                .desktop
                .cli_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            theme: cfg.theme,
            option_as_meta: cfg.desktop.option_as_meta.unwrap_or_default(),
            notifications: Switches::from(&cfg.desktop.notifications),
        }
    }

    /// Write these values into `cfg`. A blank CLI path clears its key; the
    /// default font family and size aren't written (as Option's default
    /// isn't), so `config.yaml` keeps only what was changed.
    pub fn apply(&self, cfg: &mut UserConfig) {
        let blank = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        };
        let family = blank(&self.font_family).filter(|f| f != PRIMARY_FAMILY);
        let size = self.font_size.clamp(*FONT_SIZE_RANGE.start(), *FONT_SIZE_RANGE.end());
        let mut notifications = DesktopNotifications::default();
        let n = self.notifications;
        notifications.set(n.workspace_blocked, n.review_ready, n.orchestrator_waiting);
        cfg.desktop = DesktopConfig {
            cli_path: blank(&self.cli_path).map(PathBuf::from),
            terminal_font_family: family,
            terminal_font_size: (size != DEFAULT_SIZE).then_some(size),
            // Off is the default, so it isn't written.
            option_as_meta: (self.option_as_meta != OptionAsMeta::Off).then_some(self.option_as_meta),
            notifications,
        };
        cfg.theme = self.theme;
    }
}

/// Where the last save stands, shown under the form.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveState {
    Idle,
    Saved,
    Failed(String),
}

/// What the settings view tells its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    /// `config.yaml` was written.
    Saved,
}

/// The settings view.
pub struct SettingsView {
    /// The two segmented controls, measured for their selection's slide.
    theme_track: SharedTrack,
    option_track: SharedTrack,
    font_family: Entity<FontPicker>,
    /// The terminal font size, and the field it is typed into.
    font_size: u16,
    font_size_input: Entity<InputState>,
    cli_path: Entity<InputState>,
    theme: ThemePreference,
    option_as_meta: OptionAsMeta,
    notifications: Switches,
    /// `keys.yaml` bindings macOS already owns, once the keymaps are read.
    conflicts: Vec<ShortcutConflict>,
    /// False until the config has been read, so an early blur can't save
    /// blank values over it.
    loaded: bool,
    state: SaveState,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    pub fn new(project: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let font_family = cx.new(|cx| FontPicker::new(PRIMARY_FAMILY.to_string(), window, cx));
        let font_size_input = cx.new(|cx| InputState::new(window, cx).default_value(size_text(DEFAULT_SIZE)));
        let cli_path = cx.new(|cx| InputState::new(window, cx).placeholder("Found automatically"));
        let subscriptions = vec![
            cx.subscribe_in(&cli_path, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.save(cx);
                }
            }),
            cx.subscribe_in(&font_size_input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.commit_size(window, cx);
                }
            }),
            cx.subscribe(&font_family, |this, _, event: &FontPickerEvent, cx| match event {
                FontPickerEvent::Picked(_) => this.save(cx),
            }),
            // The terminal font changed elsewhere (⌘+ / ⌘−, another
            // window's settings): show it.
            cx.observe_global_in::<TerminalSettings>(window, |this, window, cx| {
                this.follow_terminal_settings(window, cx)
            }),
        ];
        let mut view = SettingsView {
            font_family,
            font_size: DEFAULT_SIZE,
            font_size_input,
            cli_path,
            theme: ThemePreference::Auto,
            option_as_meta: OptionAsMeta::Off,
            notifications: Switches::default(),
            conflicts: Vec::new(),
            loaded: false,
            theme_track: SlideTrack::shared(),
            option_track: SlideTrack::shared(),
            state: SaveState::Idle,
            _subscriptions: subscriptions,
        };
        view.load(window, cx);
        view.load_conflicts(project, cx);
        view
    }

    /// Read the project's merged keymaps off the UI thread and note the
    /// bindings macOS owns.
    fn load_conflicts(&mut self, project: &str, cx: &mut Context<Self>) {
        let project = project.to_string();
        let read = cx.background_executor().spawn(async move {
            reserved_conflicts(&shelbi_state::keymap::load_keymaps(Some(&project)).0)
        });
        cx.spawn(async move |this, cx| {
            let conflicts = read.await;
            this.update(cx, |this, cx| {
                this.conflicts = conflicts;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Flip a notification switch and save.
    fn toggle_switch(&mut self, switch: Switch, cx: &mut Context<Self>) {
        self.notifications.toggle(switch);
        self.save(cx);
        cx.notify();
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
                        this.font_family
                            .update(cx, |p, cx| p.set_family(values.font_family, cx));
                        this.show_size(values.font_size, window, cx);
                        this.cli_path
                            .update(cx, |s, cx| s.set_value(values.cli_path, window, cx));
                        this.theme = values.theme;
                        this.option_as_meta = values.option_as_meta;
                        this.notifications = values.notifications;
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
            font_family: self.font_family.read(cx).family().to_string(),
            font_size: self.font_size,
            cli_path: self.cli_path.read(cx).value().to_string(),
            theme: self.theme,
            option_as_meta: self.option_as_meta,
            notifications: self.notifications,
        }
    }

    /// Show `size` in the size field (unless it is being typed into).
    fn show_size(&mut self, size: u16, window: &mut Window, cx: &mut Context<Self>) {
        self.font_size = size;
        let typing = self.font_size_input.read(cx).focus_handle(cx).is_focused(window);
        if !typing {
            self.font_size_input
                .update(cx, |s, cx| s.set_value(size_text(size), window, cx));
        }
        cx.notify();
    }

    /// Set the font size (clamped into the range) and save it.
    fn set_size(&mut self, size: i32, window: &mut Window, cx: &mut Context<Self>) {
        let size = clamp_size(size);
        self.font_size = size;
        self.font_size_input
            .update(cx, |s, cx| s.set_value(size_text(size), window, cx));
        self.save(cx);
        cx.notify();
    }

    /// Step the size by `delta` points, if it can move that way.
    fn step(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(size) = step_size(self.font_size, delta) {
            self.set_size(size as i32, window, cx);
        }
    }

    /// Take the typed size (`15` or `15pt`): a new number is clamped into
    /// the range and saved, the current size (even a hand-set one outside
    /// the range) is left alone, and anything else puts the current size
    /// back, with its unit.
    fn commit_size(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = parse_size(&self.font_size_input.read(cx).value());
        match typed {
            Some(size) if size != self.font_size as i32 && clamp_size(size) != self.font_size => {
                self.set_size(size, window, cx)
            }
            _ => {
                let size = self.font_size;
                self.font_size_input
                    .update(cx, |s, cx| s.set_value(size_text(size), window, cx));
            }
        }
    }

    /// The terminal settings changed: show their font and size.
    fn follow_terminal_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = TerminalSettings::current(cx);
        let size = settings.font_size.round() as u16;
        if size != self.font_size {
            self.show_size(size, window, cx);
        }
        self.font_family
            .update(cx, |p, cx| p.set_family(settings.font_family, cx));
    }

    /// Apply the form to open terminals at once, then write it off the UI
    /// thread.
    fn save(&mut self, cx: &mut Context<Self>) {
        if !self.loaded {
            return;
        }
        let values = self.values(cx);
        let applied = {
            let mut cfg = UserConfig::default();
            values.apply(&mut cfg);
            TerminalSettings::from_config(&cfg.desktop)
        };
        // Open terminals pick up the font, its size and the Option key now.
        TerminalSettings::set(applied, cx);
        let write = cx.background_executor().spawn(async move {
            shelbi_state::update_user_config(|cfg| values.apply(cfg))
                .map(|_| ())
                .map_err(|e| format!("Couldn't save config.yaml: {e}"))
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(()) => {
                        cx.emit(SettingsEvent::Saved);
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

    /// Choose the `shelbi` binary in a file dialog; the choice fills the
    /// field and is saved.
    fn browse_cli(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.cli_path
                    .update(cx, |s, cx| s.set_value(path.display().to_string(), window, cx));
                this.save(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
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

#[cfg(feature = "visual-check")]
impl SettingsView {
    /// Open the font picker (`--visual-views`).
    pub(crate) fn offscreen_open_font_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.font_family.update(cx, |p, cx| p.open(window, cx));
    }

    /// Whether the font picker is open with the installed families listed.
    pub(crate) fn offscreen_font_picker_ready(&self, cx: &gpui::App) -> bool {
        let picker = self.font_family.read(cx);
        picker.is_open() && picker.fonts_loaded()
    }

    /// Focus the Shelbi CLI field (`--visual-figma`'s focused textbox).
    pub(crate) fn offscreen_focus_cli(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.cli_path.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Whether `config.yaml` has been read into the form.
    pub(crate) fn offscreen_loaded(&self) -> bool {
        self.loaded
    }

    /// Show `theme`, `option` and `size` as chosen, without applying or
    /// saving them (`--visual-views`' states the Figma frame doesn't draw).
    pub(crate) fn offscreen_show(
        &mut self,
        theme: ThemePreference,
        option: OptionAsMeta,
        size: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme = theme;
        self.option_as_meta = option;
        self.show_size(size, window, cx);
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

impl EventEmitter<SettingsEvent> for SettingsView {}

/// A theme choice's segment label.
fn theme_label(value: ThemePreference) -> &'static str {
    match value {
        ThemePreference::Auto => "Auto",
        ThemePreference::Dark => "Dark",
        ThemePreference::Light => "Light",
    }
}

/// An Option-key choice's segment label.
fn option_label(value: OptionAsMeta) -> &'static str {
    match value {
        OptionAsMeta::Off => "Types characters",
        OptionAsMeta::Both => "Meta",
        OptionAsMeta::Left => "Left is Meta",
        OptionAsMeta::Right => "Right is Meta",
    }
}

/// The size field's text: the size and its unit, as the frame writes it
/// (`14pt`).
fn size_text(size: u16) -> String {
    format!("{size}pt")
}

/// A typed size: a whole number of points, with or without the `pt` the
/// field shows.
fn parse_size(text: &str) -> Option<i32> {
    let text = text.trim();
    let text = text.strip_suffix("pt").unwrap_or(text).trim_end();
    text.parse().ok()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // The segmented controls' selections slide to a new choice.
        let now = Instant::now();
        let slide = |track: &SharedTrack, key: String| {
            let mut track = track.borrow_mut();
            track.select(Some(key.into()), now, || cx.reduce_motion());
            let chip = track.frame(now);
            if chip.is_some() {
                window.request_animation_frame();
            }
            chip
        };
        let chips = (
            slide(&self.theme_track, format!("settings-theme-{}", theme_label(self.theme))),
            slide(&self.option_track, format!("settings-option-{}", option_label(self.option_as_meta))),
        );
        let theme_chip = chips.0;
        let chip = |label: &'static str, value: ThemePreference, current: ThemePreference, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("settings-theme-{label}"));
            controls::segment(id.clone(), label, value == current, theme_chip.is_none(), &theme)
                .child(motion::probe(&self.theme_track, id))
                .on_click(cx.listener(move |this, _, window, cx| this.set_theme(value, window, cx)))
        };
        let status: Option<(SharedString, gpui::Hsla)> = match &self.state {
            SaveState::Idle => None,
            SaveState::Saved => Some(("Saved to ~/.shelbi/config.yaml".into(), theme.form_hint)),
            SaveState::Failed(e) => Some((e.clone().into(), theme.error)),
        };
        let current = self.theme;
        let option_as_meta = self.option_as_meta;
        let option_chip_slide = chips.1;
        let option_chip = |label: &'static str, value: OptionAsMeta, cx: &mut Context<Self>| {
            let id = SharedString::from(format!("settings-option-{label}"));
            controls::segment(id.clone(), label, value == option_as_meta, option_chip_slide.is_none(), &theme)
                .child(motion::probe(&self.option_track, id))
                .on_click(cx.listener(move |this, _, _, cx| this.set_option_as_meta(value, cx)))
        };
        let floating = |bounds| motion::floating_chip(bounds, |d| controls::selected_segment(d, &theme));
        // The size stepper: the typed field, then − and +, each dimmed (and
        // inert) at its end of the range; ↑ / ↓ step it too.
        let size = self.font_size;
        let step = |id: &'static str, icon, delta: i32, cx: &mut Context<Self>| {
            let inert = step_size(size, delta).is_none();
            controls::form_icon_button(id, icon, inert, &theme)
                .on_click(cx.listener(move |this, _, window, cx| this.step(delta, window, cx)))
        };
        let size_focused = self.font_size_input.read(cx).focus_handle(cx).is_focused(window);
        let size_field = controls::textbox(
            div().flex_1().min_w_0().child(
                Input::new(&self.font_size_input)
                    .appearance(false)
                    .h_full()
                    .px_0()
                    .type_style(Type::Base)
                    .text_color(theme.form_text),
            ),
            size_focused,
            &theme,
        )
        .id("settings-font-size")
        .debug_selector(|| "settings-font-size".into())
        .w(px(SIZE_FIELD_W))
        .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
            this.step(1, window, cx);
            cx.stop_propagation();
        }))
        .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
            this.step(-1, window, cx);
            cx.stop_propagation();
        }));
        let stepper = div()
            .flex()
            .flex_row()
            .gap(px(controls::CONTROL_GAP))
            .child(size_field)
            .child(step("settings-font-size-down", icons::MINUS, -1, cx))
            .child(step("settings-font-size-up", icons::PLUS, 1, cx));
        let cli_focused = self.cli_path.read(cx).focus_handle(cx).is_focused(window);
        let cli = controls::picker(
            controls::textbox(
                div().flex_1().min_w_0().child(
                    Input::new(&self.cli_path)
                        .appearance(false)
                        .h_full()
                        .px_0()
                        .type_style(Type::Base)
                        .text_color(theme.form_text),
                ),
                cli_focused,
                &theme,
            )
            .debug_selector(|| "settings-cli-path".into())
            .flex_1(),
            controls::form_button("settings-cli-browse", "Browse", &theme)
                .on_click(cx.listener(|this, _, window, cx| this.browse_cli(window, cx))),
        );
        let switches = self.notifications;
        let toggle = |label: &'static str, switch: Switch, cx: &mut Context<Self>| {
            controls::checkbox(format!("settings-notify-{label}"), label, switches.get(switch), false, &theme)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_switch(switch, cx)))
        };
        let conflicts: Vec<String> = self.conflicts.iter().map(ShortcutConflict::message).collect();

        div()
            .id("settings")
            .debug_selector(|| "settings".into())
            .flex()
            .flex_col()
            .gap_6()
            .size_full()
            .p_8()
            .bg(theme.canvas)
            .overflow_y_scroll()
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(
                div()
                    .type_style(Type::Heading)
                    .text_color(theme.text_loud)
                    .child("Settings"),
            )
            .child(controls::labeled_control(
                "Theme",
                controls::segmented_track(&theme)
                    .w(px(controls::CONTROL_W))
                    .child(motion::origin_probe(&self.theme_track))
                    .children(theme_chip.map(floating))
                    .child(chip("Auto", ThemePreference::Auto, current, cx))
                    .child(chip("Dark", ThemePreference::Dark, current, cx))
                    .child(chip("Light", ThemePreference::Light, current, cx)),
                Some("Shared with the TUI, which picks it up on its next start.".into()),
                &theme,
            ))
            .child(controls::labeled_control("Terminal font", self.font_family.clone(), None, &theme))
            .child(controls::labeled_control("Terminal font size", stepper, None, &theme))
            .child(controls::labeled_control(
                "Option key in terminals",
                controls::segmented_track(&theme)
                    .child(motion::origin_probe(&self.option_track))
                    .children(option_chip_slide.map(floating))
                    .child(option_chip("Types characters", OptionAsMeta::Off, cx))
                    .child(option_chip("Meta", OptionAsMeta::Both, cx))
                    .child(option_chip("Left is Meta", OptionAsMeta::Left, cx))
                    .child(option_chip("Right is Meta", OptionAsMeta::Right, cx)),
                Some(
                    "Characters: Option+E, E types é, as in other Mac apps. Meta: \
                     Option+B sends Esc B, for shell word motion and Claude Code’s \
                     Option shortcuts. Option with Enter, the arrows, Backspace, or \
                     Delete is Meta either way."
                        .into(),
                ),
                &theme,
            ))
            .child(controls::labeled_control(
                "Shelbi CLI",
                cli,
                Some(
                    "The `shelbi` binary the app runs. Blank finds it on your login PATH, \
                     in Homebrew, or in ~/.cargo/bin. Takes effect on the next launch."
                        .into(),
                ),
                &theme,
            ))
            .child(
                controls::labeled_control(
                    "Key bindings",
                    controls::form_button("settings-open-keys", "Open keys.yaml", &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.open_keys(cx))),
                    Some("Shared by the TUI and the desktop app.".into()),
                    &theme,
                )
                .children(conflicts.into_iter().map(|line| {
                    div()
                        .debug_selector(|| "settings-shortcut-conflict".into())
                        .type_style(Type::XSmall)
                        .text_color(theme.error)
                        .child(line)
                })),
            )
            .child(controls::labeled_control(
                "Notifications",
                controls::checkbox_list()
                    .child(toggle(
                        "A workspace is waiting for input or permission",
                        Switch::WorkspaceBlocked,
                        cx,
                    ))
                    .child(toggle("A task is ready for review", Switch::ReviewReady, cx))
                    .child(toggle(
                        "The orchestrator is waiting for an answer",
                        Switch::OrchestratorWaiting,
                        cx,
                    )),
                Some(
                    "Not shown for what’s already on screen in the front window. \
                     macOS asks for permission the first time."
                        .into(),
                ),
                &theme,
            ))
            .children(status.map(|(text, color)| div().type_style(Type::XSmall).text_color(color).child(text)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_the_config() {
        let values = SettingsValues {
            font_family: "Berkeley Mono".into(),
            font_size: 15,
            cli_path: "~/bin/shelbi".into(),
            theme: ThemePreference::Light,
            option_as_meta: OptionAsMeta::Left,
            notifications: Switches {
                workspace_blocked: true,
                review_ready: false,
                orchestrator_waiting: true,
            },
        };
        let mut cfg = UserConfig {
            editor: Some("hx".into()),
            ..Default::default()
        };
        values.apply(&mut cfg);
        assert_eq!(cfg.desktop.terminal_font_family.as_deref(), Some("Berkeley Mono"));
        assert_eq!(cfg.desktop.terminal_font_size, Some(15));
        assert_eq!(
            cfg.desktop.cli_path.as_deref(),
            Some(std::path::Path::new("~/bin/shelbi"))
        );
        assert_eq!(cfg.theme, ThemePreference::Light);
        assert_eq!(cfg.desktop.option_as_meta, Some(OptionAsMeta::Left));
        assert_eq!(cfg.editor.as_deref(), Some("hx"), "other keys are untouched");
        let back = SettingsValues::from_config(&cfg);
        assert_eq!(back, values);
        assert_eq!(cfg.desktop.notifications.review_ready, Some(false));
        assert_eq!(cfg.desktop.notifications.workspace_blocked, None, "on is the default");
    }

    #[test]
    fn defaults_are_not_written_and_a_bare_config_shows_them() {
        let mut cfg = UserConfig::default();
        cfg.desktop.terminal_font_size = Some(12);
        cfg.desktop.terminal_font_family = Some("Menlo".into());
        SettingsValues::default().apply(&mut cfg);
        assert!(cfg.desktop.is_empty(), "{:?}", cfg.desktop);

        let shown = SettingsValues::from_config(&UserConfig::default());
        assert_eq!((shown.font_family.as_str(), shown.font_size), ("Source Code Pro", 14));
        assert_eq!(shown, SettingsValues::default());
    }

    #[test]
    fn the_size_field_shows_its_unit_and_takes_a_size_with_or_without_it() {
        assert_eq!(size_text(14), "14pt");
        assert_eq!(parse_size("14pt"), Some(14));
        assert_eq!(parse_size(" 15 "), Some(15));
        assert_eq!(parse_size("16 pt"), Some(16));
        assert_eq!((parse_size("pt"), parse_size("big")), (None, None));
    }

    #[test]
    fn the_stepper_stays_within_8_to_32_and_a_hand_set_size_is_kept() {
        assert_eq!((clamp_size(2), clamp_size(8), clamp_size(32), clamp_size(200)), (8, 8, 32, 32));
        assert_eq!((step_size(14, 1), step_size(14, -1)), (Some(15), Some(13)));
        assert_eq!((step_size(32, 1), step_size(8, -1)), (None, None));
        // From a hand-set 40, − lands back in range and + does nothing.
        assert_eq!((step_size(40, -1), step_size(40, 1)), (Some(32), None));
        assert_eq!((step_size(6, 1), step_size(6, -1)), (Some(8), None));

        let mut cfg = UserConfig::default();
        cfg.desktop.terminal_font_size = Some(40);
        let shown = SettingsValues::from_config(&cfg);
        assert_eq!(shown.font_size, 40);
        shown.apply(&mut cfg);
        assert_eq!(cfg.desktop.terminal_font_size, Some(40), "an unrelated save keeps it");
        cfg.desktop.terminal_font_size = Some(400);
        assert_eq!(SettingsValues::from_config(&cfg).font_size, 72);
    }
}
