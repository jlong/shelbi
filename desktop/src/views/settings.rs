//! The settings view: the desktop's own preferences plus the shared theme,
//! one page at a time (`dt-settings-sections`): General (the theme, the CLI,
//! notifications), Keybindings (`keys.yaml`) and Terminal (the font, its
//! size, the Option key). The settings sidebar picks the page
//! ([`super::settings_nav`]); the view draws it.
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
//! - `desktop.window_opacity` / `desktop.window_blur`: the windows'
//!   translucency, two sliders under Theme. A slider applies to every open
//!   window as it moves ([`crate::theme::window`]) and is saved when it is
//!   let go; Reduce Transparency forces the windows opaque and disables them.
//! - `desktop.notifications`: which system notifications the app posts.
//! - `desktop.terminal_colors`: the ANSI colors the user changed, regular
//!   and bold for each of the eight, typed as hex. A valid color applies to
//!   every terminal at once (through the same `TerminalSettings` global) and
//!   is saved; a color equal to the theme's isn't written, and Reset clears
//!   them all. Text that isn't a color marks its field and isn't saved.
//!   Beside the controls, a read-only terminal draws the Terminal frame's
//!   Claude Code session ([`super::terminal_preview`]) with the font, size
//!   and colors as they are now.
//!
//! The Keybindings page is its own view ([`KeybindingsPage`]): the editor
//! for `keys.yaml`, the key bindings both clients share. So is the project's
//! Workflows page ([`WorkflowsPage`]): its workflows and status catalog,
//! saved through `shelbi_state::workflow_config`.
//!
//! The rows are the Figma form components (`elements::controls`).
//!
//! Saves go through [`shelbi_state::update_user_config`], which keeps every
//! key this view doesn't own (the zen chord, the review editor, a newer
//! client's settings). Every read and write runs on the background executor.

use std::path::PathBuf;
use std::time::Instant;

use gpui::{
    div, prelude::*, px, App, Context, Entity, EventEmitter, Focusable, PathPromptOptions,
    SharedString, Subscription, Window,
};
use gpui_kit::component::input::{Input, InputEvent, InputState, MoveDown, MoveUp};
use shelbi_state::{DesktopConfig, DesktopNotifications, OptionAsMeta, ThemePreference, UserConfig};
use shelbi_term::render::Rgb as TermRgb;

use super::font_picker::{FontPicker, FontPickerEvent};
use super::keybindings::{KeybindingsEvent, KeybindingsPage};
use super::settings_nav::SettingsPage;
use super::terminal_preview;
use super::workflows::{WorkflowsEvent, WorkflowsPage};
use crate::assets::icons;
use crate::elements::controls::{self, Slider, SliderEvent};
use crate::elements::terminal::colors::{self, AnsiOverrides};
use crate::elements::terminal::metrics::{DEFAULT_FONT_SIZE, PRIMARY_FAMILY};
use crate::elements::terminal::settings::{FONT_SIZE_RANGE, ZOOM_RANGE};
use crate::elements::terminal::{TerminalPane, TerminalSettings};
use crate::theme::motion::{self, SharedTrack, SlideTrack};
use crate::theme::style::{self, ButtonKind, Type, Typography};
use crate::theme::window::{self as translucency, Translucency, WindowLook, BLUR_RANGE, OPACITY_RANGE};
use crate::theme::{rgb_hsla, Theme};

/// The ANSI colors' Reset: the Button component at Small.
const SMALL_BUTTON_H: f32 = 28.0;

/// The terminal font size a fresh install uses, in whole points.
const DEFAULT_SIZE: u16 = DEFAULT_FONT_SIZE as u16;

/// The size field's width (the frame's, room for `32pt`).
const SIZE_FIELD_W: f32 = 76.0;

/// The ANSI colors table's color-name column (`2263:4841`).
const ANSI_NAME_W: f32 = 80.0;

/// The gap between the Terminal page's controls and its preview, and the
/// preview's inset inside its border (`2264:7447`).
const PREVIEW_GAP: f32 = 32.0;
const PREVIEW_INSET: f32 = 24.0;

/// The Terminal frame's controls column, top to bottom (`2269:4844`): the
/// preview is never shorter.
const TERMINAL_COLUMN_H: f32 = 694.0;

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
    /// The ANSI colors the user changed (`None` keeps the theme's).
    pub ansi: AnsiOverrides,
    /// The windows' opacity and blur.
    pub translucency: Translucency,
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
            ansi: [None; 16],
            translucency: Translucency::default(),
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
            ansi: terminal.ansi,
            translucency: Translucency::from_config(&cfg.desktop),
        }
    }

    /// Write these values into `cfg`. A blank CLI path clears its key; the
    /// default font family and size aren't written (as Option's default
    /// isn't), nor are colors left to the theme, so `config.yaml` keeps only
    /// what was changed.
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
            terminal_colors: colors::to_config(&self.ansi),
            window_opacity: None,
            window_blur: None,
        };
        self.translucency.apply(&mut cfg.desktop);
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
#[derive(Debug, Clone)]
pub enum SettingsEvent {
    /// `config.yaml` was written.
    Saved,
    /// `keys.yaml` was written (the Keybindings page), giving these merged
    /// keymaps.
    KeysSaved(Box<shelbi_state::keymap::Keymaps>),
    /// The page hands the keys back to the settings sidebar (Esc in an empty
    /// search).
    Leave,
    /// Open workflow `name`'s editor (Settings ▸ Workflows' edit button, a
    /// row's click, a workflow just created).
    EditWorkflow(String),
}

/// The settings view.
pub struct SettingsView {
    /// The page shown.
    page: SettingsPage,
    /// The two segmented controls, measured for their selection's slide.
    theme_track: SharedTrack,
    option_track: SharedTrack,
    font_family: Entity<FontPicker>,
    /// The terminal font size, and the field it is typed into.
    font_size: u16,
    font_size_input: Entity<InputState>,
    cli_path: Entity<InputState>,
    theme: ThemePreference,
    /// The windows' translucency, and its two sliders.
    translucency: Translucency,
    opacity_slider: Entity<Slider>,
    blur_slider: Entity<Slider>,
    option_as_meta: OptionAsMeta,
    notifications: Switches,
    /// The ANSI colors the user changed, the sixteen fields they are typed
    /// in (regular 0 to 7, then bold 8 to 15), and which of those hold text
    /// that isn't a color.
    ansi: AnsiOverrides,
    ansi_inputs: Vec<Entity<InputState>>,
    ansi_invalid: [bool; 16],
    /// The Terminal page's preview: a read-only terminal.
    preview: Entity<TerminalPane>,
    /// A test's view saves here, never to `~/.shelbi`.
    #[cfg(test)]
    saved: UserConfig,
    /// The Keybindings page.
    keybindings: Entity<KeybindingsPage>,
    /// The project's Workflows page.
    workflows: Entity<WorkflowsPage>,
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
        let defaults = cx.global::<Theme>().terminal.ansi;
        let ansi_inputs: Vec<_> = (0..16)
            .map(|slot| cx.new(|cx| InputState::new(window, cx).default_value(colors::hex(defaults[slot]))))
            .collect();
        let preview = cx.new(|cx| {
            TerminalPane::canned("Preview".into(), terminal_preview::stream, window, cx)
        });
        let keybindings = cx.new(|cx| KeybindingsPage::new(project, window, cx));
        let workflows = cx.new(|cx| WorkflowsPage::new(project, window, cx));
        let range = |r: &std::ops::RangeInclusive<u8>| *r.start() as i32..=*r.end() as i32;
        let initial = Translucency::default();
        let opacity_slider = cx.new(|cx| {
            Slider::new("settings-window-opacity", initial.opacity.into(), range(&OPACITY_RANGE), (1, 10), "%", cx)
        });
        let blur_slider =
            cx.new(|cx| Slider::new("settings-window-blur", initial.blur.into(), range(&BLUR_RANGE), (1, 5), "", cx));
        let mut subscriptions = vec![
            cx.subscribe(&opacity_slider, |this, _, event: &SliderEvent, cx| {
                this.slide(|t, v| t.opacity = v, *event, cx)
            }),
            cx.subscribe(&blur_slider, |this, _, event: &SliderEvent, cx| {
                this.slide(|t, v| t.blur = v, *event, cx)
            }),
            // Reduce Transparency changed, or another window's sliders
            // moved: show it.
            cx.observe_global::<WindowLook>(|this, cx| this.follow_window_look(cx)),
            cx.subscribe(&keybindings, |_, _, event: &KeybindingsEvent, cx| match event {
                KeybindingsEvent::Saved(keymaps) => cx.emit(SettingsEvent::KeysSaved(keymaps.clone())),
                KeybindingsEvent::Leave => cx.emit(SettingsEvent::Leave),
            }),
            cx.subscribe(&workflows, |_, _, event: &WorkflowsEvent, cx| match event {
                WorkflowsEvent::Edit(name) => cx.emit(SettingsEvent::EditWorkflow(name.clone())),
            }),
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
            // The appearance changed: the colors left to the theme are its
            // new ones.
            cx.observe_global_in::<Theme>(window, |this, window, cx| this.show_colors(window, cx)),
        ];
        for (slot, input) in ansi_inputs.iter().enumerate() {
            subscriptions.push(cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => this.color_edited(slot, cx),
                    InputEvent::Blur | InputEvent::PressEnter { .. } => this.color_committed(slot, window, cx),
                    _ => {}
                }
            }));
        }
        let mut view = SettingsView {
            page: SettingsPage::default(),
            font_family,
            font_size: DEFAULT_SIZE,
            font_size_input,
            cli_path,
            theme: ThemePreference::Auto,
            translucency: initial,
            opacity_slider,
            blur_slider,
            option_as_meta: OptionAsMeta::Off,
            notifications: Switches::default(),
            ansi: [None; 16],
            ansi_inputs,
            ansi_invalid: [false; 16],
            preview,
            #[cfg(test)]
            saved: UserConfig::default(),
            keybindings,
            workflows,
            loaded: false,
            theme_track: SlideTrack::shared(),
            option_track: SlideTrack::shared(),
            state: SaveState::Idle,
            _subscriptions: subscriptions,
        };
        view.follow_window_look(cx);
        // A test's window never reads the real `~/.shelbi`.
        if !cfg!(test) {
            view.load(window, cx);
        }
        view
    }

    /// The page shown.
    #[cfg(test)]
    pub fn page(&self) -> SettingsPage {
        self.page
    }

    /// Fill the form from `cfg` as reading `config.yaml` does (a test's
    /// view reads no disk), which lets it save.
    #[cfg(test)]
    pub fn load_config(&mut self, cfg: UserConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.fill(&cfg, window, cx);
        self.saved = cfg;
    }

    /// The config as a test's view last saved it.
    #[cfg(test)]
    pub fn saved(&self) -> &UserConfig {
        &self.saved
    }

    /// Type `text` into color field `slot` (0 to 7 regular, 8 to 15 bold)
    /// as the user would, then leave it.
    #[cfg(test)]
    pub fn type_color(&mut self, slot: usize, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ansi_inputs[slot].update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        self.color_edited(slot, cx);
        self.color_committed(slot, window, cx);
    }

    /// The Terminal page's preview pane.
    #[cfg(test)]
    pub fn preview(&self) -> Entity<TerminalPane> {
        self.preview.clone()
    }

    /// Whether Reset has anything to undo.
    #[cfg(test)]
    pub fn can_reset(&self) -> bool {
        self.colors_customized()
    }

    /// A color field's text.
    #[cfg(test)]
    pub fn color_text(&self, slot: usize, cx: &App) -> String {
        self.ansi_inputs[slot].read(cx).value().to_string()
    }

    /// The Keybindings page's view.
    #[cfg(any(test, feature = "visual-check"))]
    pub(crate) fn keybindings(&self) -> Entity<KeybindingsPage> {
        self.keybindings.clone()
    }

    /// The Workflows page's view.
    #[cfg(test)]
    pub(crate) fn workflows(&self) -> Entity<WorkflowsPage> {
        self.workflows.clone()
    }

    /// Show `page`.
    pub fn show_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.page != page {
            // A capture on a page out of sight would take its keys.
            self.keybindings.update(cx, |p, cx| p.cancel_capture(cx));
            // The files may have been edited by hand (Edit YAML) since.
            if page == SettingsPage::Workflows {
                self.workflows.update(cx, |p, _| p.reload_when_drawn());
            }
            self.page = page;
            cx.notify();
        }
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
                    Ok(cfg) => this.fill(&cfg, window, cx),
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

    /// Fill the form from `cfg`, and let it save from now on.
    fn fill(&mut self, cfg: &UserConfig, window: &mut Window, cx: &mut Context<Self>) {
        let values = SettingsValues::from_config(cfg);
        self.font_family
            .update(cx, |p, cx| p.set_family(values.font_family, cx));
        self.show_size(values.font_size, window, cx);
        self.cli_path
            .update(cx, |s, cx| s.set_value(values.cli_path, window, cx));
        self.theme = values.theme;
        self.show_translucency(values.translucency, cx);
        // What config.yaml says is what the windows show.
        translucency::set(values.translucency, cx);
        self.option_as_meta = values.option_as_meta;
        self.notifications = values.notifications;
        self.ansi = values.ansi;
        self.ansi_invalid = [false; 16];
        self.show_colors(window, cx);
        self.loaded = true;
    }

    fn values(&self, cx: &Context<Self>) -> SettingsValues {
        SettingsValues {
            font_family: self.font_family.read(cx).family().to_string(),
            font_size: self.font_size,
            cli_path: self.cli_path.read(cx).value().to_string(),
            theme: self.theme,
            option_as_meta: self.option_as_meta,
            notifications: self.notifications,
            ansi: self.ansi,
            translucency: self.translucency,
        }
    }

    /// A slider moved (`set` writes its value into the setting): every
    /// window follows at once, and letting go saves it.
    fn slide(&mut self, set: fn(&mut Translucency, u8), event: SliderEvent, cx: &mut Context<Self>) {
        let (SliderEvent::Changed(v) | SliderEvent::Committed(v)) = event;
        let mut next = self.translucency;
        set(&mut next, v.clamp(0, u8::MAX as i32) as u8);
        let next = Translucency::clamped(next.opacity.into(), next.blur.into());
        if next != self.translucency {
            self.translucency = next;
            translucency::set(next, cx);
            cx.notify();
        }
        if matches!(event, SliderEvent::Committed(_)) {
            self.save(cx);
        }
    }

    /// Show `t` on the sliders.
    fn show_translucency(&mut self, t: Translucency, cx: &mut Context<Self>) {
        self.translucency = t;
        self.opacity_slider.update(cx, |s, cx| s.set_value(t.opacity.into(), cx));
        self.blur_slider.update(cx, |s, cx| s.set_value(t.blur.into(), cx));
        cx.notify();
    }

    /// The installed look changed: show its setting (once the config is
    /// read), and disable the sliders under Reduce Transparency.
    fn follow_window_look(&mut self, cx: &mut Context<Self>) {
        let look = WindowLook::current(cx);
        if self.loaded && look.setting != self.translucency {
            self.show_translucency(look.setting, cx);
        }
        for slider in [&self.opacity_slider, &self.blur_slider] {
            slider.update(cx, |s, cx| s.set_disabled(look.reduce_transparency, cx));
        }
        cx.notify();
    }

    /// The opacity slider.
    #[cfg(test)]
    pub fn opacity_slider(&self) -> Entity<Slider> {
        self.opacity_slider.clone()
    }

    /// The blur slider.
    #[cfg(test)]
    pub fn blur_slider(&self) -> Entity<Slider> {
        self.blur_slider.clone()
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
        if settings.ansi != self.ansi {
            self.ansi = settings.ansi;
            self.show_colors(window, cx);
        }
    }

    /// The theme's ANSI colors for the appearance now.
    fn default_colors(cx: &App) -> [TermRgb; 16] {
        cx.global::<Theme>().terminal.ansi
    }

    /// The color `slot` shows: the user's, else the theme's.
    fn color(&self, slot: usize, cx: &App) -> TermRgb {
        self.ansi[slot].unwrap_or(Self::default_colors(cx)[slot])
    }

    /// Whether any color differs from the theme's (what Reset undoes), or
    /// a field holds text that isn't a color.
    fn colors_customized(&self) -> bool {
        self.ansi.iter().any(Option::is_some) || self.ansi_invalid.contains(&true)
    }

    /// Show each color in its field, except one being typed in or holding
    /// text that isn't a color.
    fn show_colors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for slot in 0..16 {
            let input = self.ansi_inputs[slot].clone();
            let typing = input.read(cx).focus_handle(cx).is_focused(window);
            if typing || self.ansi_invalid[slot] {
                continue;
            }
            let text = colors::hex(self.color(slot, cx));
            if input.read(cx).value() != text.as_str() {
                input.update(cx, |s, cx| s.set_value(text, window, cx));
            }
        }
        cx.notify();
    }

    /// A color field's text changed: a color applies at once and is saved
    /// (unless it is the one already shown); anything else marks the field.
    fn color_edited(&mut self, slot: usize, cx: &mut Context<Self>) {
        let text = self.ansi_inputs[slot].read(cx).value().to_string();
        match colors::parse_hex(&text) {
            None => self.ansi_invalid[slot] = true,
            Some(color) => {
                self.ansi_invalid[slot] = false;
                // The theme's own color isn't the user's: it isn't written.
                let value = (color != Self::default_colors(cx)[slot]).then_some(color);
                if value != self.ansi[slot] {
                    self.ansi[slot] = value;
                    self.save(cx);
                }
            }
        }
        cx.notify();
    }

    /// Enter in, or focus out of, a color field: a color is written back
    /// the way the table shows them (`#E04F52`); text that isn't one stays,
    /// marked, for the user to fix.
    fn color_committed(&mut self, slot: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.ansi_invalid[slot] {
            return;
        }
        let text = colors::hex(self.color(slot, cx));
        self.ansi_inputs[slot].update(cx, |s, cx| s.set_value(text, window, cx));
    }

    /// Put every color back to the theme's and save.
    fn reset_colors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ansi = [None; 16];
        self.ansi_invalid = [false; 16];
        let defaults = Self::default_colors(cx);
        for (input, color) in self.ansi_inputs.clone().iter().zip(defaults) {
            input.update(cx, |s, cx| s.set_value(colors::hex(color), window, cx));
        }
        self.save(cx);
        cx.notify();
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
        // Open terminals pick up the font, its size, the Option key and the
        // colors now.
        TerminalSettings::set(applied, cx);
        #[cfg(test)]
        self.save_in_memory(values, cx);
        #[cfg(not(test))]
        self.write(values, cx);
    }

    /// Keep `values` as a test's saved config.
    #[cfg(test)]
    fn save_in_memory(&mut self, values: SettingsValues, cx: &mut Context<Self>) {
        values.apply(&mut self.saved);
        self.state = SaveState::Saved;
        cx.emit(SettingsEvent::Saved);
        cx.notify();
    }

    /// Write `values` to `config.yaml` off the UI thread.
    #[cfg(not(test))]
    fn write(&mut self, values: SettingsValues, cx: &mut Context<Self>) {
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

        let shown = self.page;
        let page = div()
            .id("settings")
            .debug_selector(|| "settings".into())
            .flex()
            .flex_col()
            .gap_6()
            .size_full()
            .p_8()
            .bg(theme.nested(theme.canvas))
            .overflow_y_scroll()
            .type_style(Type::Base)
            .text_color(theme.foreground)
            .child(div().absolute().size_0().debug_selector(move || page_selector(shown).into()));
        let page = match shown {
            SettingsPage::General => page
                .child(controls::labeled_control(
                    "Theme",
                    // The frame's track hugs its items (2238:25533).
                    controls::segmented_track(&theme)
                        .child(motion::origin_probe(&self.theme_track))
                        .children(theme_chip.map(floating))
                        .child(chip("Auto", ThemePreference::Auto, current, cx))
                        .child(chip("Dark", ThemePreference::Dark, current, cx))
                        .child(chip("Light", ThemePreference::Light, current, cx)),
                    Some("Shared with the TUI, which picks it up on its next start.".into()),
                    &theme,
                ))
                .child(self.render_translucency(&theme, cx))
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
                )),
            // The Keybindings frame (2252:28172): its own view, the list
            // scrolling under its header rather than the whole page.
            SettingsPage::Keybindings => page.overflow_hidden().child(self.keybindings.clone()),
            // The Workflows frame (2301:5824): the page scrolls.
            SettingsPage::Workflows => page.child(self.workflows.clone()),
            // The Terminal frame (2252:28494): the controls in a column the
            // picker's width, the Option helper a line per sentence; the
            // preview fills the rest of the page beside them.
            SettingsPage::Terminal => page.child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .gap(px(PREVIEW_GAP))
                    .min_h(px(TERMINAL_COLUMN_H))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .gap_6()
                            .w(px(controls::CONTROL_W))
                            .child(controls::labeled_control("Terminal font", self.font_family.clone(), None, &theme))
                            .child(controls::labeled_control("Terminal font size", stepper, None, &theme))
                            .child(self.render_ansi(&theme, window, cx))
                            .child(
                                controls::labeled_control(
                                    "Option key in terminals",
                                    controls::segmented_track(&theme)
                                        .child(motion::origin_probe(&self.option_track))
                                        .children(option_chip_slide.map(floating))
                                        .child(option_chip("Types characters", OptionAsMeta::Off, cx))
                                        .child(option_chip("Meta", OptionAsMeta::Both, cx))
                                        .child(option_chip("Left is Meta", OptionAsMeta::Left, cx))
                                        .child(option_chip("Right is Meta", OptionAsMeta::Right, cx)),
                                    None,
                                    &theme,
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .child(controls::helper(
                                            "Characters: Option+E, E types é, as in other Mac apps.",
                                            &theme,
                                        ))
                                        .child(
                                            controls::helper(
                                                "Meta: Option+B sends Esc B, for shell word motion and Claude Code’s \
                                                 Option shortcuts.",
                                                &theme,
                                            )
                                            .mb(px(4.0)),
                                        )
                                        .child(controls::helper(
                                            "Option with Enter, the arrows, Backspace, or Delete is Meta either way.",
                                            &theme,
                                        )),
                                ),
                            ),
                    )
                    .child(self.render_preview(&theme)),
            ),
        };
        // `config.yaml`'s save line belongs to the pages that write it.
        let status = status.filter(|_| !matches!(shown, SettingsPage::Keybindings | SettingsPage::Workflows));
        page.children(status.map(|(text, color)| div().type_style(Type::XSmall).text_color(color).child(text)))
    }
}

impl SettingsView {
    /// Window transparency and Background blur: a slider each, with a note
    /// in place of their helpers while Reduce Transparency holds the
    /// windows opaque.
    fn render_translucency(&self, theme: &Theme, cx: &App) -> impl IntoElement {
        let reduced = WindowLook::current(cx).reduce_transparency;
        let help = |text: &'static str| (!reduced).then(|| SharedString::from(text));
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(controls::labeled_control(
                "Window transparency",
                self.opacity_slider.clone(),
                help("How much of the desktop shows through the window. 100% is opaque."),
                theme,
            ))
            .child(controls::labeled_control(
                "Background blur",
                self.blur_slider.clone(),
                help("Blurs what shows through. 0 is a clear see-through."),
                theme,
            ))
            .when(reduced, |d| {
                d.child(
                    div()
                        .debug_selector(|| "settings-reduce-transparency".into())
                        .mt(px(-18.0))
                        .child(controls::helper(
                            "Reduce Transparency is on (System Settings ▸ Accessibility ▸ Display), \
                             so windows are opaque.",
                            theme,
                        )),
                )
            })
    }

    /// The ANSI colors editor (`2263:4808`): the title with Reset, the
    /// helper, then a row per color with its regular and bold fields.
    fn render_ansi(&self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reset = if self.colors_customized() {
            style::button("settings-ansi-reset", "Reset", ButtonKind::Secondary, false, theme)
                .on_click(cx.listener(|this, _, window, cx| this.reset_colors(window, cx)))
        } else {
            style::disabled_button("settings-ansi-reset", "Reset", ButtonKind::Secondary, theme)
        };
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(controls::CONTROL_GAP))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .type_style(Type::BaseMedium)
                    .text_color(theme.text_loud)
                    .child("ANSI colors"),
            )
            .child(reset.h(px(SMALL_BUTTON_H)));
        let column = |label: &'static str| div().flex_1().min_w_0().child(label);
        let labels = div()
            .flex()
            .flex_row()
            .gap(px(12.0))
            .type_style(Type::XSmallMedium)
            .text_color(theme.muted)
            .child(div().w(px(ANSI_NAME_W)).flex_none().child("Color"))
            .child(column("Regular"))
            .child(column("Bold"));
        let rows = colors::NAMES.iter().enumerate().map(|(color, name)| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .w(px(ANSI_NAME_W))
                        .flex_none()
                        .type_style(Type::Base)
                        .text_color(theme.foreground)
                        .child(*name),
                )
                .child(self.color_field(color, theme, window, cx))
                .child(self.color_field(color + 8, theme, window, cx))
        });
        let invalid = self.ansi_invalid.contains(&true);
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .w_full()
            .child(header)
            .child(
                div()
                    .type_style(Type::XSmall)
                    .text_color(theme.muted)
                    .child("Regular and bold colors used by terminal applications."),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .child(labels)
                    .child(div().flex().flex_col().gap(px(4.0)).children(rows)),
            )
            .when(invalid, |d| {
                d.child(
                    div()
                        .debug_selector(|| "settings-ansi-invalid".into())
                        .type_style(Type::XSmall)
                        .text_color(theme.error)
                        .child("A color is six hex digits, like #E04F52."),
                )
            })
    }

    /// Color field `slot`: the Textbox with the color's swatch before its
    /// hex (`2264:4775`), in the error edge while its text isn't a color.
    fn color_field(&self, slot: usize, theme: &Theme, window: &Window, cx: &App) -> impl IntoElement {
        let input = &self.ansi_inputs[slot];
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let swatch = div()
            .flex_none()
            .size(px(16.0))
            .rounded(px(2.0))
            .border_1()
            .border_color(theme.divider_strong)
            .bg(rgb_hsla(self.color(slot, cx)));
        let content = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(8.0))
            .child(swatch)
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(input)
                        .appearance(false)
                        .h_full()
                        .px_0()
                        .type_style(Type::Base)
                        .text_color(theme.foreground),
                ),
            );
        let field = if self.ansi_invalid[slot] {
            controls::invalid_textbox(content, focused, theme)
        } else {
            controls::textbox(content, focused, theme)
        };
        let (name, weight) = (colors::NAMES[slot % 8], if slot < 8 { "regular" } else { "bold" });
        let selector = format!("settings-ansi-{}-{weight}", name.to_lowercase());
        field.debug_selector(move || selector.clone()).flex_1().min_w_0()
    }

    /// The preview (`2264:7445`): its label over the read-only terminal, in
    /// a 1px border 24px around the grid.
    fn render_preview(&self, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(6.0))
            .child(
                div()
                    .type_style(Type::BaseMedium)
                    .text_color(theme.text_loud)
                    .child("Preview"),
            )
            .child(
                div()
                    .debug_selector(|| "settings-terminal-preview".into())
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .p(px(PREVIEW_INSET))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(theme.divider_strong)
                    // The terminal's background, as a terminal pane on the
                    // page shows it (the page's own glass while translucent).
                    .bg(theme.nested(rgb_hsla(theme.terminal.background)))
                    .overflow_hidden()
                    .child(self.preview.clone()),
            )
    }
}

/// The selector of a page's marker (tests find the page shown by it).
pub fn page_selector(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::General => "settings-page-general",
        SettingsPage::Keybindings => "settings-page-keybindings",
        SettingsPage::Terminal => "settings-page-terminal",
        SettingsPage::Workflows => "settings-page-workflows",
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
            ansi: {
                let mut ansi = [None; 16];
                ansi[0] = Some(TermRgb::hex(0x000000));
                ansi[9] = Some(TermRgb::hex(0xe69589));
                ansi
            },
            translucency: Translucency { opacity: 85, blur: 20 },
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
        let ansi = &cfg.desktop.terminal_colors;
        assert_eq!((ansi.black.regular.as_deref(), ansi.red.bold.as_deref()), (Some("#000000"), Some("#E69589")));
        assert_eq!((ansi.black.bold.as_ref(), ansi.green.regular.as_ref()), (None, None), "only what was changed");
        assert_eq!((cfg.desktop.window_opacity, cfg.desktop.window_blur), (Some(85), Some(20)));
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
        cfg.desktop.terminal_colors.set(3, Some("#dbc300".into()));
        (cfg.desktop.window_opacity, cfg.desktop.window_blur) = (Some(60), Some(10));
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
