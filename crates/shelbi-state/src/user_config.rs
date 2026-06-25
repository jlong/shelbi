//! `~/.shelbi/config.yaml` — per-user UI preferences. Distinct from
//! `~/.shelbi/shelbi.yaml` (assistant identity, wizard state) so a future
//! `shelbi config reset` can wipe UI tweaks without nuking onboarding.
//!
//! Currently holds the Zen Mode toggle chord chosen by the first-run probe.
//! Missing file → defaults (`zen_toggle = AltZ`); a missing `keymap:` block
//! likewise falls back so older partial files keep loading.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use shelbi_core::Result;

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

/// Atomically write the user config to `~/.shelbi/config.yaml`.
pub fn save_user_config(cfg: &UserConfig) -> Result<()> {
    ensure_dir(&shelbi_home()?)?;
    let path = user_config_path()?;
    atomic_write(&path, serde_yaml::to_string(cfg)?.as_bytes())
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
    fn yaml_uses_kebab_case_chord_names() {
        // The on-disk representation should match what a human-edited
        // config.yaml looks like — kebab-case, not Rust-style PascalCase.
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlShiftZ;
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(yaml.contains("ctrl-shift-z"), "got: {yaml}");
    }
}
