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
}

impl DesktopConfig {
    /// Whether no desktop setting is present (so the block isn't written).
    pub fn is_empty(&self) -> bool {
        self.cli_path.is_none()
            && self.terminal_font_family.is_none()
            && self.terminal_font_size.is_none()
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
    for candidate in [config, env].into_iter().flatten() {
        if !candidate.trim().is_empty() {
            return candidate;
        }
    }
    DEFAULT_EDITOR.to_string()
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

/// Load the user config, apply `edit`, and write it back, keeping every
/// top-level key this build doesn't model (a newer client's settings) instead
/// of dropping it the way a plain [`save_user_config`] round-trip would. Keys
/// the edited config omits on save (a cleared `editor`, an `auto` theme, an
/// empty `desktop:` block) are removed. YAML comments are not preserved.
/// Returns the config as written.
pub fn update_user_config(edit: impl FnOnce(&mut UserConfig)) -> Result<UserConfig> {
    use serde_yaml::{Mapping, Value};

    let path = user_config_path()?;
    let mut raw: Mapping = if path.exists() {
        match serde_yaml::from_str::<Value>(&fs::read_to_string(&path)?)? {
            Value::Mapping(m) => m,
            _ => Mapping::new(),
        }
    } else {
        Mapping::new()
    };
    let mut cfg: UserConfig = serde_yaml::from_value(Value::Mapping(raw.clone()))?;
    edit(&mut cfg);

    let Value::Mapping(known) = serde_yaml::to_value(&cfg)? else {
        unreachable!("UserConfig serializes to a mapping");
    };
    for key in ["keymap", "editor", "theme", "desktop"] {
        raw.remove(key);
    }
    for (key, value) in known {
        raw.insert(key, value);
    }
    ensure_dir(&shelbi_home()?)?;
    atomic_write(&path, serde_yaml::to_string(&Value::Mapping(raw))?.as_bytes())?;
    Ok(cfg)
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
             terminal_font_family: Geist Mono\n  terminal_font_size: 14\n",
        )
        .unwrap();
        assert_eq!(cfg.theme, ThemePreference::Light);
        assert_eq!(
            cfg.desktop.cli_path.as_deref(),
            Some(std::path::Path::new("/opt/shelbi/bin/shelbi"))
        );
        assert_eq!(cfg.desktop.terminal_font_family.as_deref(), Some("Geist Mono"));
        assert_eq!(cfg.desktop.terminal_font_size, Some(14));
        let back: UserConfig = serde_yaml::from_str(&serde_yaml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back, cfg);
        // Defaults write neither key, so the CLI's own saves don't add them.
        let text = serde_yaml::to_string(&UserConfig::default()).unwrap();
        assert!(!text.contains("desktop") && !text.contains("theme"), "{text}");
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
}
