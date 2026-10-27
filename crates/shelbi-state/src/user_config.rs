//! `~/.shelbi/config.yaml` — per-user UI preferences. Distinct from
//! `~/.shelbi/shelbi.yaml` (the hub config's project index) so a future
//! `shelbi config reset` can wipe UI tweaks without nuking onboarding.
//!
//! Holds the Zen Mode toggle chord chosen by the first-run probe, the review
//! editor, the color theme, and the desktop app's own settings (`desktop:`).
//! Missing file → defaults (`zen_toggle = AltZ`, `theme = auto`); a missing
//! `keymap:` block likewise falls back so older partial files keep loading.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shelbi_core::Result;

use crate::keymap::KeyChord;
use crate::{atomic_write, ensure_dir, shelbi_home};

/// Path to `~/.shelbi/config.yaml` (or `$SHELBI_HOME/config.yaml`).
pub fn user_config_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("config.yaml"))
}

/// Per-user UI preferences. Each block is optional so partial files don't
/// fail to load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub keymap: Keymap,

    /// Editor command the review interface's "Edit in <editor>" view
    /// launches in the task's review worktree. Hub-wide (not per-project)
    /// so a reviewer's editor choice follows them across every project.
    /// Resolution order is [`resolve_editor`]: this setting, then
    /// `$EDITOR`, then `vim`. May be a bare command (`hx`) or a command
    /// with flags (`code --wait`); the display label is derived from the
    /// program name (see [`editor_display_name`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,

    /// Color theme for the TUI and the desktop app (`theme: auto | dark |
    /// light`). A missing key is [`ThemePreference::Auto`], and `auto` is
    /// omitted on save (lean on-disk form, like `editor`). The token values
    /// live in `shelbi_app::theme`.
    #[serde(default, skip_serializing_if = "ThemePreference::is_auto")]
    pub theme: ThemePreference,

    /// Desktop app settings (`desktop:` block). Ignored by the CLI and TUI.
    #[serde(default, skip_serializing_if = "DesktopConfig::is_empty")]
    pub desktop: DesktopConfig,
}

/// The `theme:` setting. `Auto` follows the system appearance where a client
/// can read it (the desktop app) and is dark where it can't (the TUI:
/// terminals don't report their appearance).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreference {
    #[default]
    Auto,
    Dark,
    Light,
}

impl ThemePreference {
    /// True for [`ThemePreference::Auto`], the default.
    pub fn is_auto(&self) -> bool {
        matches!(self, ThemePreference::Auto)
    }
}

/// The desktop app's settings in `config.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopConfig {
    /// The `shelbi` CLI the desktop app runs, overriding its discovery (the
    /// login-shell `PATH`, then the Homebrew prefixes and `~/.cargo/bin`).
    /// A leading `~/` is expanded by the app. Unset means discover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<PathBuf>,

    /// Font family for the desktop app's terminals. Unset means the app's
    /// default (Geist Mono).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_font_family: Option<String>,

    /// Font size, in points, for the desktop app's terminals. Unset means the
    /// app's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_font_size: Option<u16>,

    /// Whether Option acts as Meta in the desktop app's terminals (Option+B
    /// sends `ESC b`, for shell word motion and Claude Code's Option chords)
    /// instead of typing the keyboard layout's Option characters and dead
    /// keys (Option+E, E types `é`): `false` (the macOS default), `true`,
    /// `left`, or `right` (only that Option key is Meta), as in Ghostty.
    /// Unset means `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_as_meta: Option<OptionAsMeta>,

    /// When the desktop app's terminals show their scrollbar: `auto`
    /// (follow macOS's "Show scroll bars"), `scrolling` (only while
    /// scrolling or with the pointer at the edge), or `always` (whenever
    /// there is scrollback). Unset, or a value it doesn't know, means
    /// `auto`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_scrollbar"
    )]
    pub terminal_scrollbar: Option<TerminalScrollbar>,

    /// Which system notifications the desktop app posts (`notifications:`).
    /// Every kind is on unless switched off.
    #[serde(default, skip_serializing_if = "DesktopNotifications::is_empty")]
    pub notifications: DesktopNotifications,

    /// The desktop app's terminal ANSI colors (`terminal_colors:`): a
    /// `#rrggbb` per color the user changed, regular and bold. Unset colors
    /// take the app's palette for the appearance, so only what was changed
    /// is written and the defaults can move.
    #[serde(default, skip_serializing_if = "TerminalColors::is_empty")]
    pub terminal_colors: TerminalColors,

    /// How opaque the desktop app's windows are, in percent: 100 is opaque
    /// and the app clamps lower values to its floor (30), so text stays
    /// readable. Below 100 the window's background surfaces (not its text)
    /// let the desktop show through. A fraction (`0.85`, Ghostty's
    /// `background-opacity` shape) reads as that percentage. Unset means
    /// 100.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_percent"
    )]
    pub window_opacity: Option<u8>,

    /// How much a translucent window blurs what shows through it: a blur
    /// radius in points, 0 (clear) to 50. Unset means 0.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lenient_amount"
    )]
    pub window_blur: Option<u8>,
}

/// Which Option keys act as Meta in the desktop app's terminals
/// (`desktop.option_as_meta`). Written as a YAML bool or `left` / `right`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OptionAsMeta {
    /// Neither: Option types characters and dead keys (`false`).
    #[default]
    Off,
    /// Both Option keys are Meta (`true`).
    Both,
    /// Only the left Option key is Meta; the right one types characters.
    Left,
    /// Only the right Option key is Meta; the left one types characters.
    Right,
}

impl Serialize for OptionAsMeta {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            OptionAsMeta::Off => s.serialize_bool(false),
            OptionAsMeta::Both => s.serialize_bool(true),
            OptionAsMeta::Left => s.serialize_str("left"),
            OptionAsMeta::Right => s.serialize_str("right"),
        }
    }
}

impl<'de> Deserialize<'de> for OptionAsMeta {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = OptionAsMeta;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("true, false, left, or right")
            }

            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<OptionAsMeta, E> {
                Ok(if v { OptionAsMeta::Both } else { OptionAsMeta::Off })
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<OptionAsMeta, E> {
                match v.trim().to_ascii_lowercase().as_str() {
                    "true" | "both" => Ok(OptionAsMeta::Both),
                    "false" => Ok(OptionAsMeta::Off),
                    "left" => Ok(OptionAsMeta::Left),
                    "right" => Ok(OptionAsMeta::Right),
                    other => Err(E::invalid_value(serde::de::Unexpected::Str(other), &self)),
                }
            }
        }
        d.deserialize_any(Visitor)
    }
}

/// When the desktop app's terminals show their scrollbar
/// (`desktop.terminal_scrollbar`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalScrollbar {
    /// Follow System Settings ▸ Appearance ▸ "Show scroll bars".
    #[default]
    Auto,
    /// Only while scrolling or with the pointer at the edge (macOS's
    /// overlay scrollers).
    Scrolling,
    /// Whenever there is scrollback.
    Always,
}

/// `terminal_scrollbar`: `auto`, `scrolling`, or `always` (any case, and
/// macOS's own names `automatic` and `when-scrolling`); anything else is
/// ignored rather than failing the whole file.
fn lenient_scrollbar<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<TerminalScrollbar>, D::Error> {
    let value = serde_yaml::Value::deserialize(d)?;
    let Some(text) = value.as_str() else {
        return Ok(None);
    };
    Ok(match text.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "auto" | "automatic" => Some(TerminalScrollbar::Auto),
        "scrolling" | "when-scrolling" => Some(TerminalScrollbar::Scrolling),
        "always" => Some(TerminalScrollbar::Always),
        _ => None,
    })
}

/// `window_opacity`: a whole percentage, or a fraction up to 1 read as one
/// (`0.85` is 85). Out of range saturates; anything that isn't a number is
/// ignored rather than failing the whole file.
fn lenient_percent<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u8>, D::Error> {
    let value = serde_yaml::Value::deserialize(d)?;
    Ok(value.as_f64().map(|v| {
        let v = if v > 0.0 && v <= 1.0 && value.is_f64() { v * 100.0 } else { v };
        v.round().clamp(0.0, u8::MAX as f64) as u8
    }))
}

/// `window_blur`: a number of points; out of range saturates, and anything
/// that isn't a number is ignored.
fn lenient_amount<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<u8>, D::Error> {
    let value = serde_yaml::Value::deserialize(d)?;
    Ok(value.as_f64().map(|v| v.round().clamp(0.0, u8::MAX as f64) as u8))
}

impl DesktopConfig {
    /// Whether no desktop setting is present (so the block isn't written).
    pub fn is_empty(&self) -> bool {
        self.cli_path.is_none()
            && self.terminal_font_family.is_none()
            && self.terminal_font_size.is_none()
            && self.option_as_meta.is_none()
            && self.terminal_scrollbar.is_none()
            && self.notifications.is_empty()
            && self.terminal_colors.is_empty()
            && self.window_opacity.is_none()
            && self.window_blur.is_none()
    }
}

/// The ANSI colors the user changed in the desktop app's terminals
/// (`desktop.terminal_colors:`), keyed by color name, each with its
/// `regular` (ANSI 0 to 7) and `bold` (the bright eight, 8 to 15) value:
///
/// ```yaml
/// terminal_colors:
///   red:
///     regular: "#e04f52"
///     bold: "#e69589"
/// ```
///
/// The values are kept as written; the app parses them and ignores one that
/// isn't a `#rrggbb` color.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub black: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub red: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub green: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub yellow: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub blue: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub magenta: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub cyan: AnsiPair,
    #[serde(default, skip_serializing_if = "AnsiPair::is_empty")]
    pub white: AnsiPair,
}

/// One color's two values in [`TerminalColors`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnsiPair {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regular: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<String>,
}

impl AnsiPair {
    /// Whether neither value is set (so the color isn't written).
    pub fn is_empty(&self) -> bool {
        self.regular.is_none() && self.bold.is_none()
    }
}

impl TerminalColors {
    /// The colors' names, in ANSI order (black is 0, white 7).
    pub const NAMES: [&'static str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];

    fn pairs(&self) -> [&AnsiPair; 8] {
        [
            &self.black,
            &self.red,
            &self.green,
            &self.yellow,
            &self.blue,
            &self.magenta,
            &self.cyan,
            &self.white,
        ]
    }

    fn pair_mut(&mut self, color: usize) -> &mut AnsiPair {
        match color {
            0 => &mut self.black,
            1 => &mut self.red,
            2 => &mut self.green,
            3 => &mut self.yellow,
            4 => &mut self.blue,
            5 => &mut self.magenta,
            6 => &mut self.cyan,
            _ => &mut self.white,
        }
    }

    /// The value written for ANSI color `slot` (0 to 15: 0 to 7 a color's
    /// regular value, 8 to 15 its bold one), if any.
    pub fn get(&self, slot: usize) -> Option<&str> {
        if slot >= 16 {
            return None;
        }
        let pair = self.pairs()[slot % 8];
        if slot < 8 {
            pair.regular.as_deref()
        } else {
            pair.bold.as_deref()
        }
    }

    /// Set (or, with `None`, clear) ANSI color `slot`'s value. A slot past
    /// 15 is ignored.
    pub fn set(&mut self, slot: usize, value: Option<String>) {
        if slot >= 16 {
            return;
        }
        let pair = self.pair_mut(slot % 8);
        if slot < 8 {
            pair.regular = value;
        } else {
            pair.bold = value;
        }
    }

    /// Whether no color is set (so the block isn't written).
    pub fn is_empty(&self) -> bool {
        self.pairs().iter().all(|p| p.is_empty())
    }
}

/// The desktop app's notification switches (`desktop.notifications:`). A
/// missing key means on, and only a switched-off kind is written, so the
/// block stays absent until the user turns something off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopNotifications {
    /// A workspace's agent stops for the user: it finished a turn and waits
    /// for input, or a permission dialog is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_blocked: Option<bool>,

    /// A task reaches the review column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_ready: Option<bool>,

    /// The orchestrator finishes a turn and waits for an answer, or a
    /// permission dialog is up in its session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_waiting: Option<bool>,
}

impl DesktopNotifications {
    pub fn workspace_blocked(&self) -> bool {
        self.workspace_blocked.unwrap_or(true)
    }

    pub fn review_ready(&self) -> bool {
        self.review_ready.unwrap_or(true)
    }

    pub fn orchestrator_waiting(&self) -> bool {
        self.orchestrator_waiting.unwrap_or(true)
    }

    /// Set each switch, writing only the ones that are off.
    pub fn set(&mut self, workspace_blocked: bool, review_ready: bool, orchestrator_waiting: bool) {
        let off = |on: bool| (!on).then_some(false);
        self.workspace_blocked = off(workspace_blocked);
        self.review_ready = off(review_ready);
        self.orchestrator_waiting = off(orchestrator_waiting);
    }

    /// Whether every switch is at its default (so the block isn't written).
    pub fn is_empty(&self) -> bool {
        self.workspace_blocked.is_none()
            && self.review_ready.is_none()
            && self.orchestrator_waiting.is_none()
    }
}

/// Keymap overrides. Today just the Zen Mode toggle chord — extended as new
/// rebindable keys land.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keymap {
    /// Chord that toggles `state.json::zen_mode` between On and Off.
    /// Default [`ZenToggleChord::AltZ`]; the first-run probe writes a
    /// fallback here when the terminal swallows Alt+Z.
    #[serde(default)]
    pub zen_toggle: ZenToggleChord,
}

/// The fixed set of chords offered by the first-run probe's fallback
/// popup. Skip means "no key bound" — the user can still toggle via
/// `shelbi zen on/off` from the command line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZenToggleChord {
    #[default]
    AltZ,
    CtrlBackslash,
    CtrlG,
    CtrlShiftZ,
    /// Hotkey disabled — no chord toggles Zen Mode.
    None,
}

impl ZenToggleChord {
    /// Human-readable label for the popup chooser and status messages.
    pub fn label(self) -> &'static str {
        match self {
            ZenToggleChord::AltZ => "Alt+Z",
            ZenToggleChord::CtrlBackslash => "Ctrl+\\",
            ZenToggleChord::CtrlG => "Ctrl+G",
            ZenToggleChord::CtrlShiftZ => "Ctrl+Shift+Z",
            ZenToggleChord::None => "(none)",
        }
    }

    /// Compact unicode glyph form for tight shortcut columns (palette
    /// hint, future menu hints). Empty string when the hotkey is
    /// disabled — callers showing a shortcut column should skip the row
    /// entirely in that case.
    pub fn hint(self) -> &'static str {
        match self {
            ZenToggleChord::AltZ => "⌥Z",
            ZenToggleChord::CtrlBackslash => "⌃\\",
            ZenToggleChord::CtrlG => "⌃G",
            ZenToggleChord::CtrlShiftZ => "⇧⌃Z",
            ZenToggleChord::None => "",
        }
    }

    /// Terminal-glyph form for inline keybind hints — `⌥` for Alt,
    /// `^` for Ctrl, `⇧` for Shift, matching how `^P` is rendered in
    /// the sidebar footer. `None` when no chord is bound, so callers
    /// can suppress the hint instead of showing an empty hotkey.
    pub fn glyph(self) -> Option<&'static str> {
        match self {
            ZenToggleChord::AltZ => Some("⌥Z"),
            ZenToggleChord::CtrlBackslash => Some("^\\"),
            ZenToggleChord::CtrlG => Some("^G"),
            ZenToggleChord::CtrlShiftZ => Some("^⇧Z"),
            ZenToggleChord::None => None,
        }
    }

    /// Map a resolved [`KeyChord`] back to the four-value preset enum
    /// used by the sidebar glyph and palette hint. Returns `None` for
    /// arbitrary chords the enum can't represent (e.g. `f6`,
    /// `ctrl-alt-shift-x`) so callers can fall back to a sane default.
    pub fn from_chord(chord: &KeyChord) -> Option<ZenToggleChord> {
        match chord.canonical().as_str() {
            "alt-z" => Some(ZenToggleChord::AltZ),
            "ctrl-\\" => Some(ZenToggleChord::CtrlBackslash),
            "ctrl-g" => Some(ZenToggleChord::CtrlG),
            "ctrl-shift-z" => Some(ZenToggleChord::CtrlShiftZ),
            _ => None,
        }
    }
}

/// Load the user config. Missing file → defaults; that's not an error
/// because the first-run probe is the path that creates the file.
pub fn load_user_config() -> Result<UserConfig> {
    let path = user_config_path()?;
    if !path.exists() {
        return Ok(UserConfig::default());
    }
    let text = fs::read_to_string(&path)?;
    Ok(serde_yaml::from_str(&text)?)
}

/// Fallback editor when neither the config nor `$EDITOR` names one.
pub const DEFAULT_EDITOR: &str = "vim";

/// Resolve the editor command the review interface's "Edit in <editor>"
/// view launches, in precedence order:
///
/// 1. `~/.shelbi/config.yaml::editor` (hub-wide reviewer preference),
/// 2. the `$EDITOR` environment variable,
/// 3. `vim` ([`DEFAULT_EDITOR`]).
///
/// A blank/whitespace value at either layer is skipped rather than
/// launching an empty command. The returned string is the full command
/// (it may carry flags, e.g. `code --wait`); split it before exec.
pub fn resolve_editor() -> String {
    let from_config = load_user_config().ok().and_then(|c| c.editor);
    let from_env = std::env::var("EDITOR").ok();
    resolve_editor_from(from_config, from_env)
}

/// Pure precedence core of [`resolve_editor`]: config editor, then `$EDITOR`,
/// then `vim`, skipping blank/whitespace values at each layer. Split out so
/// the precedence is unit-testable without touching the filesystem or env.
pub(crate) fn resolve_editor_from(config: Option<String>, env: Option<String>) -> String {
    resolve_editor_source(config.as_deref(), env.as_deref()).0
}

/// Which layer of [`resolve_editor`] named the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorSource {
    /// `config.yaml`'s `editor:`.
    Config,
    /// The `$EDITOR` environment variable.
    Env,
    /// Neither: [`DEFAULT_EDITOR`].
    Default,
}

/// [`resolve_editor`]'s precedence over given values, with the layer that
/// won: `config`, then `env`, then `vim`, skipping blank values. A setting
/// shows what Shelbi runs, and what it falls back to without the override
/// (`config` `None`).
pub fn resolve_editor_source(config: Option<&str>, env: Option<&str>) -> (String, EditorSource) {
    for (candidate, source) in [(config, EditorSource::Config), (env, EditorSource::Env)] {
        if let Some(c) = candidate.filter(|c| !c.trim().is_empty()) {
            return (c.to_string(), source);
        }
    }
    (DEFAULT_EDITOR.to_string(), EditorSource::Default)
}

/// Human-facing name of an editor command for the "Edit in <name>"
/// sidebar label — the program's basename, first letter upper-cased
/// (`vim` → `Vim`, `/usr/bin/hx` → `Hx`, `code --wait` → `Code`). Flags
/// and directory components are dropped. Empty input yields an empty
/// string so callers can guard.
pub fn editor_display_name(command: &str) -> String {
    let program = command.split_whitespace().next().unwrap_or("");
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim();
    let mut chars = base.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Atomically write the user config to `~/.shelbi/config.yaml`.
pub fn save_user_config(cfg: &UserConfig) -> Result<()> {
    ensure_dir(&shelbi_home()?)?;
    let path = user_config_path()?;
    atomic_write(&path, serde_yaml::to_string(cfg)?.as_bytes())
}

/// Load the user config, apply `edit`, and write back only what the edit
/// changed. The file is read fresh, so a write never carries stale values
/// for settings it didn't touch, and it is patched as text
/// ([`yaml_edit`](crate::yaml_edit)): comments, key order, keys this build
/// doesn't model (a newer client's settings, at the top level or inside
/// `desktop:`) and the spelling of untouched values all survive. Keys the
/// edited config omits on save (a cleared `editor`, an `auto` theme, an
/// empty `desktop:` block) are removed. An edit that changes nothing writes
/// nothing.
///
/// A file that doesn't read (not YAML, not a mapping, or a value this build
/// can't load) is an error and is left exactly as it is, as is the file when
/// the write fails. Returns the config as written.
pub fn update_user_config(edit: impl FnOnce(&mut UserConfig)) -> Result<UserConfig> {
    use crate::yaml_edit;
    use shelbi_core::Error;

    let path = user_config_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let unreadable = |e: &dyn std::fmt::Display| Error::Other(format!("{} {e}", path.display()));
    let raw = yaml_edit::parse(&text).map_err(|e| unreadable(&e))?;
    if !raw.is_mapping() {
        return Err(unreadable(&"isn't a mapping of settings"));
    }
    let before: UserConfig = serde_yaml::from_value(raw.clone())?;
    let mut after = before.clone();
    edit(&mut after);
    if after == before {
        return Ok(after);
    }
    let target = yaml_edit::merge(
        &raw,
        &serde_yaml::to_value(&before)?,
        &serde_yaml::to_value(&after)?,
        &[],
    );
    let patched = yaml_edit::patch(&text, &target).map_err(|e| unreadable(&e))?;
    ensure_dir(&shelbi_home()?)?;
    atomic_write(&path, patched.text.as_bytes())?;
    Ok(after)
}

/// Write a self-documenting `~/.shelbi/config.yaml` when one doesn't already
/// exist — the scaffold `shelbi init` drops so the hub-wide UI preferences are
/// discoverable inline (see [`shelbi_core::scaffold::CONFIG_YAML`]). Idempotent:
/// an existing file (a real config, or one the first-run Zen probe already
/// wrote) is left untouched. Returns `true` when a file was written.
pub fn scaffold_user_config_if_missing() -> Result<bool> {
    let path = user_config_path()?;
    if path.exists() {
        return Ok(false);
    }
    ensure_dir(&shelbi_home()?)?;
    atomic_write(&path, shelbi_core::scaffold::CONFIG_YAML.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-user-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn desktop_settings_and_theme_round_trip() {
        let cfg: UserConfig = serde_yaml::from_str(
            "theme: light\ndesktop:\n  cli_path: /opt/shelbi/bin/shelbi\n  \
             terminal_font_family: Geist Mono\n  terminal_font_size: 14\n  \
             option_as_meta: left\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, ThemePreference::Light);
        assert_eq!(
            cfg.desktop.cli_path.as_deref(),
            Some(std::path::Path::new("/opt/shelbi/bin/shelbi"))
        );
        assert_eq!(cfg.desktop.terminal_font_family.as_deref(), Some("Geist Mono"));
        assert_eq!(cfg.desktop.terminal_font_size, Some(14));
        assert_eq!(cfg.desktop.option_as_meta, Some(OptionAsMeta::Left));
        let back: UserConfig = serde_yaml::from_str(&serde_yaml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg);
        // Defaults write neither key, so the CLI's own saves don't add them.
        let text = serde_yaml::to_string(&UserConfig::default()).unwrap();
        assert!(!text.contains("desktop") && !text.contains("theme"), "{text}");
    }

    #[test]
    fn desktop_notification_switches_default_on_and_write_only_when_off() {
        let cfg: UserConfig =
            serde_yaml::from_str("desktop:\n  notifications:\n    review_ready: false\n").unwrap();
        let n = &cfg.desktop.notifications;
        assert!(n.workspace_blocked() && n.orchestrator_waiting());
        assert!(!n.review_ready());

        let mut n = DesktopNotifications::default();
        n.set(true, true, true);
        assert!(n.is_empty(), "all on writes nothing");
        n.set(false, true, true);
        let cfg = UserConfig {
            desktop: DesktopConfig {
                notifications: n,
                ..Default::default()
            },
            ..Default::default()
        };
        let text = serde_yaml::to_string(&cfg).unwrap();
        assert!(text.contains("workspace_blocked: false"), "{text}");
        assert!(!text.contains("review_ready"), "{text}");
        let back: UserConfig = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn update_user_config_keeps_unknown_and_shared_keys() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::write(
            home.join("config.yaml"),
            "keymap:\n  zen_toggle: ctrl-g\neditor: hx\nfuture_key:\n  nested: 1\n",
        )
        .unwrap();

        let written = update_user_config(|c| {
            c.theme = ThemePreference::Dark;
            c.desktop.terminal_font_size = Some(13);
        })
        .unwrap();
        assert_eq!(written.theme, ThemePreference::Dark);

        // What another client (the TUI) reads on its next start.
        let back = load_user_config().unwrap();
        assert_eq!(back.keymap.zen_toggle, ZenToggleChord::CtrlG);
        assert_eq!(back.editor.as_deref(), Some("hx"));
        assert_eq!(back.theme, ThemePreference::Dark);
        assert_eq!(back.desktop.terminal_font_size, Some(13));
        let text = std::fs::read_to_string(home.join("config.yaml")).unwrap();
        assert!(text.contains("future_key"), "an unknown key survives: {text}");

        // Clearing a setting removes its key instead of leaving a stale one.
        update_user_config(|c| {
            c.theme = ThemePreference::Auto;
            c.desktop = DesktopConfig::default();
        })
        .unwrap();
        let text = std::fs::read_to_string(home.join("config.yaml")).unwrap();
        assert!(!text.contains("theme") && !text.contains("desktop"), "{text}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn update_user_config_patches_only_what_changed() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = home.join("config.yaml");
        // The scaffold's comments, a desktop block with a key this build
        // doesn't know and a value spelled its own way, and an unknown
        // top-level key.
        let original = format!(
            "{}\ndesktop:\n  window_opacity: 0.85 # Ghostty's shape\n  future_desktop_key: [a, b]\n\
             future_key:\n  nested: 1\n",
            shelbi_core::scaffold::CONFIG_YAML
        );
        std::fs::write(&path, &original).unwrap();

        let written = update_user_config(|c| c.editor = Some("code --wait".into())).unwrap();
        assert_eq!(written.editor.as_deref(), Some("code --wait"));
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "## Color theme for the TUI and the desktop app",
            "# theme: auto",
            "zen_toggle: alt-z",
            "window_opacity: 0.85 # Ghostty's shape",
            "future_desktop_key: [a, b]",
            "future_key:\n  nested: 1",
        ] {
            assert!(text.contains(kept), "{kept:?} survives:\n{text}");
        }
        let back = load_user_config().unwrap();
        assert_eq!(back.editor.as_deref(), Some("code --wait"), "arguments kept");
        assert_eq!(back.desktop.window_opacity, Some(85));

        update_user_config(|c| c.theme = ThemePreference::Light).unwrap();
        update_user_config(|c| c.editor = None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("theme: light"), "{text}");
        assert!(!text.contains("editor: code"), "a cleared editor is removed:\n{text}");
        assert!(text.contains("future_desktop_key") && text.contains("# editor: vim"), "{text}");

        // Back to auto removes the key; what's left is the original file.
        update_user_config(|c| c.theme = ThemePreference::Auto).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        // An edit that changes nothing doesn't write.
        std::fs::write(&path, "theme:   dark   # mine\n").unwrap();
        update_user_config(|c| c.theme = ThemePreference::Dark).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "theme:   dark   # mine\n");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn update_user_config_leaves_an_unreadable_file_alone() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = home.join("config.yaml");
        for bad in ["theme: [dark\n", "just a string\n", "- a\n- b\n", "theme: purple\n"] {
            std::fs::write(&path, bad).unwrap();
            let result = update_user_config(|c| c.editor = Some("hx".into()));
            assert!(result.is_err(), "{bad:?} refuses the edit");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bad, "{bad:?} untouched");
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[cfg(unix)]
    #[test]
    fn update_user_config_reports_a_failed_write_and_keeps_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = home.join("config.yaml");
        std::fs::write(&path, "editor: hx\n").unwrap();
        // A read-only folder: the atomic write's temp file can't be made.
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = update_user_config(|c| c.theme = ThemePreference::Light);
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err(), "the failed write is reported");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "editor: hx\n");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn the_editor_source_names_the_layer_that_won() {
        assert_eq!(
            resolve_editor_source(Some("code --wait"), Some("nvim")),
            ("code --wait".to_string(), EditorSource::Config)
        );
        assert_eq!(resolve_editor_source(Some(" "), Some("nvim")), ("nvim".to_string(), EditorSource::Env));
        assert_eq!(resolve_editor_source(None, Some("")), ("vim".to_string(), EditorSource::Default));
    }

    #[test]
    fn missing_file_yields_defaults() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let cfg = load_user_config().unwrap();
        assert_eq!(cfg.keymap.zen_toggle, ZenToggleChord::AltZ);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn round_trip_preserves_zen_toggle() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlBackslash;
        save_user_config(&cfg).unwrap();
        let back = load_user_config().unwrap();
        assert_eq!(back.keymap.zen_toggle, ZenToggleChord::CtrlBackslash);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn partial_file_falls_back_to_defaults() {
        // A user could write `{}` to disable the wizard's defaults; that
        // should resolve back to AltZ rather than fail to parse.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::write(home.join("config.yaml"), "{}\n").unwrap();
        let cfg = load_user_config().unwrap();
        assert_eq!(cfg.keymap.zen_toggle, ZenToggleChord::AltZ);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn hint_renders_compact_glyph_per_chord_and_empty_for_none() {
        assert_eq!(ZenToggleChord::AltZ.hint(), "⌥Z");
        assert_eq!(ZenToggleChord::CtrlBackslash.hint(), "⌃\\");
        assert_eq!(ZenToggleChord::CtrlG.hint(), "⌃G");
        assert_eq!(ZenToggleChord::CtrlShiftZ.hint(), "⇧⌃Z");
        assert_eq!(ZenToggleChord::None.hint(), "");
    }

    #[test]
    fn scaffolded_config_writes_once_parses_and_never_clobbers() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // First call writes the self-documenting scaffold and it loads as a
        // real UserConfig at its default chord (comments are inert).
        assert!(scaffold_user_config_if_missing().unwrap());
        let path = home.join("config.yaml");
        assert!(path.is_file());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("https://shelbi.dev/docs/configuration/global"));
        let cfg = load_user_config().unwrap();
        assert_eq!(cfg.keymap.zen_toggle, ZenToggleChord::AltZ);

        // Second call is a no-op that preserves whatever is on disk (e.g. a
        // value the first-run probe wrote).
        std::fs::write(&path, "keymap:\n  zen_toggle: ctrl-g\n").unwrap();
        assert!(!scaffold_user_config_if_missing().unwrap());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "keymap:\n  zen_toggle: ctrl-g\n"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn yaml_uses_kebab_case_chord_names() {
        // The on-disk representation should match what a human-edited
        // config.yaml looks like — kebab-case, not Rust-style PascalCase.
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlShiftZ;
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(yaml.contains("ctrl-shift-z"), "got: {yaml}");
    }

    #[test]
    fn editor_precedence_is_config_then_env_then_vim() {
        // Config wins over $EDITOR.
        assert_eq!(
            resolve_editor_from(Some("hx".into()), Some("nano".into())),
            "hx"
        );
        // Blank config falls through to $EDITOR.
        assert_eq!(
            resolve_editor_from(Some("  ".into()), Some("nano".into())),
            "nano"
        );
        // No config, no env → vim.
        assert_eq!(resolve_editor_from(None, None), "vim");
        // Blank env with no config → vim.
        assert_eq!(resolve_editor_from(None, Some(String::new())), "vim");
        // A command with flags is preserved verbatim.
        assert_eq!(
            resolve_editor_from(Some("code --wait".into()), None),
            "code --wait"
        );
    }

    #[test]
    fn editor_display_name_titlecases_the_basename() {
        assert_eq!(editor_display_name("vim"), "Vim");
        assert_eq!(editor_display_name("hx"), "Hx");
        assert_eq!(editor_display_name("/usr/local/bin/nvim"), "Nvim");
        // Flags are dropped; only the program name drives the label.
        assert_eq!(editor_display_name("code --wait"), "Code");
        assert_eq!(editor_display_name(""), "");
    }

    #[test]
    fn theme_round_trips_and_defaults_to_auto() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A missing key (and a missing file) is `auto`.
        assert_eq!(load_user_config().unwrap().theme, ThemePreference::Auto);
        std::fs::write(home.join("config.yaml"), "editor: hx\n").unwrap();
        assert_eq!(load_user_config().unwrap().theme, ThemePreference::Auto);

        // Each value reads back from its lowercase spelling.
        for (text, want) in [
            ("auto", ThemePreference::Auto),
            ("dark", ThemePreference::Dark),
            ("light", ThemePreference::Light),
        ] {
            std::fs::write(home.join("config.yaml"), format!("theme: {text}\n")).unwrap();
            assert_eq!(load_user_config().unwrap().theme, want, "theme: {text}");
        }

        // dark / light survive a save; auto is omitted (it is the default).
        for pref in [ThemePreference::Dark, ThemePreference::Light] {
            let cfg = UserConfig {
                theme: pref,
                ..UserConfig::default()
            };
            save_user_config(&cfg).unwrap();
            assert_eq!(load_user_config().unwrap(), cfg);
        }
        let yaml = serde_yaml::to_string(&UserConfig {
            theme: ThemePreference::Light,
            ..UserConfig::default()
        })
        .unwrap();
        assert!(yaml.contains("theme: light"), "got: {yaml}");
        let yaml = serde_yaml::to_string(&UserConfig::default()).unwrap();
        assert!(!yaml.contains("theme"), "auto omitted: {yaml}");

        // An unknown value is a parse error, not a silent fallback, so
        // `shelbi config lint` reports it.
        assert!(serde_yaml::from_str::<UserConfig>("theme: purple\n").is_err());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn editor_round_trips_through_yaml_and_is_omitted_when_unset() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let cfg = UserConfig {
            editor: Some("hx".into()),
            ..UserConfig::default()
        };
        save_user_config(&cfg).unwrap();
        let back = load_user_config().unwrap();
        assert_eq!(back.editor.as_deref(), Some("hx"));

        // A default config omits the key entirely (lean on-disk form).
        let yaml = serde_yaml::to_string(&UserConfig::default()).unwrap();
        assert!(!yaml.contains("editor"), "unset editor omitted: {yaml}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn terminal_colors_write_only_what_is_set() {
        let mut colors = TerminalColors::default();
        assert!(colors.is_empty());
        colors.set(1, Some("#e04f52".into()));
        colors.set(15, Some("#ffffff".into()));
        colors.set(16, Some("#000000".into()));
        assert_eq!((colors.get(1), colors.get(9), colors.get(15)), (Some("#e04f52"), None, Some("#ffffff")));
        assert_eq!(colors.get(16), None, "past the sixteen");
        let cfg = UserConfig {
            desktop: DesktopConfig {
                terminal_colors: colors.clone(),
                ..DesktopConfig::default()
            },
            ..UserConfig::default()
        };
        let text = serde_yaml::to_string(&cfg).unwrap();
        assert!(text.contains("red:\n      regular: '#e04f52'"), "{text}");
        assert!(!text.contains("black") && !text.contains("bold: null"), "{text}");
        let back: UserConfig = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back.desktop.terminal_colors, colors);

        colors.set(1, None);
        colors.set(15, None);
        assert!(colors.is_empty());
        assert!(DesktopConfig { terminal_colors: colors, ..DesktopConfig::default() }.is_empty());
    }

    #[test]
    fn window_opacity_and_blur_round_trip_and_read_leniently() {
        let cfg = UserConfig {
            desktop: DesktopConfig {
                window_opacity: Some(85),
                window_blur: Some(20),
                ..DesktopConfig::default()
            },
            ..UserConfig::default()
        };
        let text = serde_yaml::to_string(&cfg).unwrap();
        assert!(text.contains("window_opacity: 85") && text.contains("window_blur: 20"), "{text}");
        let back: UserConfig = serde_yaml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
        assert!(!DesktopConfig { window_blur: Some(0), ..DesktopConfig::default() }.is_empty());

        let read = |yaml: &str| {
            let cfg: UserConfig = serde_yaml::from_str(&format!("desktop:\n{yaml}\n")).unwrap();
            (cfg.desktop.window_opacity, cfg.desktop.window_blur)
        };
        assert_eq!(read("  window_opacity: 0.85"), (Some(85), None), "Ghostty's fraction");
        assert_eq!(read("  window_opacity: 1.0"), (Some(100), None));
        assert_eq!(read("  window_opacity: 1"), (Some(1), None), "a whole number is a percentage");
        assert_eq!(read("  window_opacity: 300\n  window_blur: -4"), (Some(255), Some(0)));
        assert_eq!(read("  window_opacity: lots\n  window_blur: 12.4"), (None, Some(12)));
        assert_eq!(read("  window_opacity: ~"), (None, None));
    }

    #[test]
    fn option_as_meta_reads_bools_and_sides() {
        let read = |v: &str| -> OptionAsMeta {
            let cfg: UserConfig = serde_yaml::from_str(&format!("desktop:\n  option_as_meta: {v}\n")).unwrap();
            cfg.desktop.option_as_meta.unwrap()
        };
        assert_eq!(read("false"), OptionAsMeta::Off);
        assert_eq!(read("true"), OptionAsMeta::Both);
        assert_eq!(read("left"), OptionAsMeta::Left);
        assert_eq!(read("Right"), OptionAsMeta::Right);
        assert!(serde_yaml::from_str::<UserConfig>("desktop:\n  option_as_meta: sideways\n").is_err());
        for v in [OptionAsMeta::Off, OptionAsMeta::Both, OptionAsMeta::Left, OptionAsMeta::Right] {
            let text = serde_yaml::to_string(&v).unwrap();
            assert_eq!(serde_yaml::from_str::<OptionAsMeta>(&text).unwrap(), v, "{text}");
        }
        assert_eq!(serde_yaml::to_string(&OptionAsMeta::Both).unwrap().trim(), "true");
    }

    #[test]
    fn terminal_scrollbar_round_trips_and_reads_leniently() {
        for v in [TerminalScrollbar::Auto, TerminalScrollbar::Scrolling, TerminalScrollbar::Always] {
            let cfg = UserConfig {
                desktop: DesktopConfig {
                    terminal_scrollbar: Some(v),
                    ..DesktopConfig::default()
                },
                ..UserConfig::default()
            };
            let text = serde_yaml::to_string(&cfg).unwrap();
            let back: UserConfig = serde_yaml::from_str(&text).unwrap();
            assert_eq!(back, cfg, "{text}");
        }
        let text = serde_yaml::to_string(&DesktopConfig {
            terminal_scrollbar: Some(TerminalScrollbar::Scrolling),
            ..DesktopConfig::default()
        })
        .unwrap();
        assert!(text.contains("terminal_scrollbar: scrolling"), "{text}");

        let read = |v: &str| {
            let cfg: UserConfig = serde_yaml::from_str(&format!("desktop:\n  terminal_scrollbar: {v}\n")).unwrap();
            cfg.desktop.terminal_scrollbar
        };
        assert_eq!(read("auto"), Some(TerminalScrollbar::Auto));
        assert_eq!(read("Automatic"), Some(TerminalScrollbar::Auto));
        assert_eq!(read("when_scrolling"), Some(TerminalScrollbar::Scrolling));
        assert_eq!(read("ALWAYS"), Some(TerminalScrollbar::Always));
        // Unknown values are ignored, not an error that loses the file.
        assert_eq!(read("sometimes"), None);
        assert_eq!(read("3"), None);
        assert_eq!(read("~"), None);
        assert!(DesktopConfig::default().terminal_scrollbar.is_none());
    }
}
