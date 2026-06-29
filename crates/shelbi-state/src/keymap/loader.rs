//! `~/.shelbi/keys.yml` loader and three-layer merge.
//!
//! Layers, lowest → highest priority:
//!
//! 1. Embedded built-ins from each [`Action::default_chords`].
//! 2. `keys.yml::defaults.<mode>.<action>` if present.
//! 3. `keys.yml::projects.<project>.<mode>.<action>` if present.
//!
//! Before the merge runs, [`load_keymaps`] performs a one-shot migration:
//! if `config.yaml::keymap.zen_toggle` names a chord other than the
//! built-in default (and `keys.yml::defaults.global.zen_toggle` isn't
//! already set), the chord is copied into `keys.yml`, the legacy
//! `config.yaml` field is reset, and a one-time migration notice fires.
//! Subsequent startups are silent because the legacy field is back at its
//! default.
//!
//! Finally, intra-mode chord collisions (two actions in the same mode
//! bound to the same chord) trigger an Error diagnostic AND both
//! colliding actions revert to their built-in defaults.
//!
//! The merge replaces — it does not union. Setting
//! `projects.shelbi.sidebar.nav_up: [w]` over a `defaults.sidebar.nav_up:
//! [k, up]` yields `[w]`, not `[k, up, w]`.

use std::collections::HashMap;
use std::fs;
use std::hash::Hash;
use std::path::Path;

use crossterm::event::KeyEvent;
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use super::actions::{
    Action, ActivityAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction, ReviewAction,
    SidebarAction,
};
use super::chord::KeyChord;
use crate::user_config::{load_user_config, save_user_config, ZenToggleChord};
use crate::{atomic_write, shelbi_home};

/// Filename under `$SHELBI_HOME` for user-authored key overrides.
pub const KEYS_FILENAME: &str = "keys.yml";

/// Final, merged keymaps for every mode. Constructed by [`load_keymaps`]
/// from the three-layer merge — callers should treat fields as read-only
/// after construction.
#[derive(Debug, Clone, Default)]
pub struct Keymaps {
    pub global: ModeKeymap<GlobalAction>,
    pub sidebar: ModeKeymap<SidebarAction>,
    pub kanban: ModeKeymap<KanbanAction>,
    pub popover: ModeKeymap<PopoverAction>,
    pub review: ModeKeymap<ReviewAction>,
    pub activity: ModeKeymap<ActivityAction>,
    pub palette: ModeKeymap<PaletteAction>,
}

/// Per-mode binding map: chord → action plus a reverse index of action →
/// chord list so dispatchers can render hints without scanning.
#[derive(Debug, Clone)]
pub struct ModeKeymap<A: Copy + Eq + Hash> {
    pub bindings: HashMap<KeyChord, A>,
    /// Stable insertion-ordered chord list per action, for help rendering.
    pub by_action: HashMap<A, Vec<KeyChord>>,
}

impl<A: Copy + Eq + Hash> Default for ModeKeymap<A> {
    fn default() -> Self {
        ModeKeymap {
            bindings: HashMap::new(),
            by_action: HashMap::new(),
        }
    }
}

impl Keymaps {
    /// Resolve the Zen Mode toggle chord into the legacy four-value
    /// [`ZenToggleChord`] enum used by the sidebar glyph and palette
    /// hint. Prefers the keys.yml-resolved binding (so once the legacy
    /// `config.yaml::keymap.zen_toggle` migrates into keys.yml the glyph
    /// matches what the user actually pressed); falls back to
    /// `legacy_fallback` when the chord doesn't match one of the four
    /// preset variants (e.g. the user bound `f6` directly in keys.yml)
    /// or is unbound entirely.
    ///
    /// The fallback is the chord the caller already read from
    /// `~/.shelbi/config.yaml` (typically via the first-run probe). On
    /// fresh installs that's the right answer; after a successful
    /// legacy migration the keys.yml lookup wins and the fallback is
    /// only used for non-preset bindings.
    pub fn zen_toggle_chord(&self, legacy_fallback: ZenToggleChord) -> ZenToggleChord {
        match self.global.first_chord_for(GlobalAction::ZenToggle) {
            Some(c) => ZenToggleChord::from_chord(c).unwrap_or(legacy_fallback),
            None => ZenToggleChord::None,
        }
    }
}

impl<A: Copy + Eq + Hash> ModeKeymap<A> {
    /// Look up the action bound to the chord that fired. The lookup folds
    /// in the implicit-Shift fallback some terminals produce for
    /// uppercase characters (a `Char('A')` with no SHIFT mod is treated
    /// as `Char('a') + SHIFT`).
    pub fn dispatch(&self, key: KeyEvent) -> Option<A> {
        let chord = KeyChord::from_event(key);
        if let Some(a) = self.bindings.get(&chord) {
            return Some(*a);
        }
        // Some terminals deliver `Char('A')` without setting SHIFT.
        // Normalize that to the `shift-a` form before giving up.
        if let crossterm::event::KeyCode::Char(c) = chord.code {
            if c.is_ascii_uppercase() {
                let alt = KeyChord {
                    code: crossterm::event::KeyCode::Char(c.to_ascii_lowercase()),
                    mods: chord.mods | crossterm::event::KeyModifiers::SHIFT,
                };
                if let Some(a) = self.bindings.get(&alt) {
                    return Some(*a);
                }
            }
        }
        None
    }

    /// First registered chord for `action`, in the order the chords were
    /// installed during the merge — useful for compact hint columns where
    /// only one chord can be shown.
    pub fn first_chord_for(&self, action: A) -> Option<&KeyChord> {
        self.by_action.get(&action).and_then(|v| v.first())
    }

    /// Every chord registered for `action`, in install order. Borrowed
    /// references so callers can format without cloning.
    pub fn chords_for(&self, action: A) -> Vec<&KeyChord> {
        self.by_action
            .get(&action)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
}

/// Diagnostic emitted by [`load_keymaps`]. Bad config never blocks the
/// caller — the loader always returns a usable [`Keymaps`] alongside the
/// diagnostic list. Callers (the wizard, the TUI entry points) render
/// these to stderr with an `error:` / `warning:` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapDiagnostic {
    Error {
        kind: ErrorKind,
        message: String,
        location: Option<String>,
    },
    Warning {
        kind: WarningKind,
        message: String,
        location: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    ParseError,
    UnknownAction,
    UnknownChord,
    Collision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningKind {
    ReservedChordRebind,
    /// Legacy `config.yaml::keymap.zen_toggle` was set but couldn't be
    /// migrated because `keys.yml::defaults.global.zen_toggle` already
    /// holds an explicit value. The keys.yml entry wins; the user is
    /// asked to remove the legacy field by hand so the two configs stop
    /// disagreeing.
    LegacyZenToggleField,
    /// One-shot migration succeeded: the legacy
    /// `config.yaml::keymap.zen_toggle` chord was written into
    /// `keys.yml::defaults.global.zen_toggle` and the legacy field was
    /// reset to its default. Emitted once on the migrating startup; the
    /// next load sees the legacy field at its default and stays silent.
    LegacyZenToggleMigrated,
}

impl KeymapDiagnostic {
    fn err(kind: ErrorKind, message: impl Into<String>, location: Option<String>) -> Self {
        KeymapDiagnostic::Error {
            kind,
            message: message.into(),
            location,
        }
    }
    fn warn(kind: WarningKind, message: impl Into<String>, location: Option<String>) -> Self {
        KeymapDiagnostic::Warning {
            kind,
            message: message.into(),
            location,
        }
    }
}

// ---------------------------------------------------------------------------
// On-disk schema (lenient — every field optional, scalar shorthand allowed).

/// `mode -> action -> chords` — one layer of the merge.
type ModeMap = HashMap<String, HashMap<String, ChordSpec>>;

/// Top-level structure of `keys.yml`. Both blocks are optional so a file
/// with just `defaults` or just `projects` still parses.
#[derive(Debug, Default, Deserialize)]
struct KeysFile {
    #[serde(default)]
    defaults: Option<ModeMap>,
    #[serde(default)]
    projects: Option<HashMap<String, ModeMap>>,
}

/// One action's chord override. The on-disk form can be:
///
/// - a scalar string (`alt-z`) → single chord
/// - a list (`[k, up]`) → multiple chords for the same action
/// - YAML null → fall back to the layer below (no override)
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChordSpec {
    None,
    One(String),
    Many(Vec<String>),
}

impl ChordSpec {
    /// `None` for "fall through to the lower layer", `Some(vec)` for an
    /// explicit override (which may be empty — meaning "unbind").
    fn to_chords(&self) -> Option<Vec<String>> {
        match self {
            ChordSpec::None => None,
            ChordSpec::One(s) => Some(vec![s.clone()]),
            ChordSpec::Many(v) => Some(v.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Loader.

/// Load the merged keymaps for `project_name` (or just `defaults`+builtin
/// if `None`). Always returns a usable [`Keymaps`]; any errors from the
/// user's `keys.yml` are reported via the diagnostic list and the
/// affected actions silently keep their built-in defaults.
///
/// As a side effect, runs the one-shot legacy-zen-toggle migration first
/// (see [`migrate_legacy_zen_toggle`]). The migration rewrites `keys.yml`
/// and `config.yaml` on disk, so subsequent calls observe the migrated
/// state and stay silent.
pub fn load_keymaps(project_name: Option<&str>) -> (Keymaps, Vec<KeymapDiagnostic>) {
    let mut diags = Vec::new();

    // One-shot: migrate the legacy config.yaml field into keys.yml before
    // the merge so the merge picks up the migrated chord on this very
    // call. Emits the migration warning on success — the next startup
    // sees the legacy field at its default and stays silent.
    if let Some(chord) = migrate_legacy_zen_toggle() {
        let msg = format!(
            "migrated config.yaml::keymap.zen_toggle (`{chord}`) into \
             keys.yml::defaults.global.zen_toggle; the legacy field is no \
             longer read"
        );
        diags.push(KeymapDiagnostic::warn(
            WarningKind::LegacyZenToggleMigrated,
            msg,
            Some("config.yaml".into()),
        ));
    }

    // Layer 1: embedded defaults — every action gets its built-in chords.
    let mut staged: HashMap<Action, Vec<String>> = HashMap::new();
    for action in Action::all() {
        staged.insert(action, action.default_chords().iter().map(|s| s.to_string()).collect());
    }

    // Layer 2/3: load keys.yml if present. Missing file is fine.
    let file = read_keys_file(&mut diags);

    if let Some(ref f) = file {
        if let Some(defaults) = &f.defaults {
            apply_overrides("defaults", defaults, &mut staged, &mut diags);
        }
    }

    // Track whether the user explicitly set defaults.global.zen_toggle.
    // The legacy config.yaml::keymap.zen_toggle compat shim only fires
    // when this is false.
    let zen_overridden_in_defaults = file
        .as_ref()
        .and_then(|f| f.defaults.as_ref())
        .and_then(|m| m.get("global"))
        .map(|g| g.contains_key("zen_toggle"))
        .unwrap_or(false);

    if let (Some(name), Some(f)) = (project_name, file.as_ref()) {
        if let Some(projects) = &f.projects {
            if let Some(p) = projects.get(name) {
                let scope = format!("projects.{name}");
                apply_overrides(&scope, p, &mut staged, &mut diags);
            }
        }
    }

    // Legacy compat: `config.yaml::keymap.zen_toggle` -> `global.zen_toggle`.
    // Two cases:
    //   - keys.yml already overrides `defaults.global.zen_toggle`: the
    //     two configs disagree — keys.yml wins, but emit the deprecation
    //     warning so the user knows to remove the dead legacy field.
    //   - keys.yml doesn't override: forward the chord for this load.
    //     (In the common path the migration above already rewrote
    //     keys.yml so this branch becomes inert on subsequent loads.)
    if let Some(legacy) = legacy_zen_toggle_chord() {
        if zen_overridden_in_defaults {
            let warn_msg = "config.yaml::keymap.zen_toggle is deprecated; \
                 keys.yml::defaults.global.zen_toggle is already set — \
                 remove the legacy field from config.yaml";
            diags.push(KeymapDiagnostic::warn(
                WarningKind::LegacyZenToggleField,
                warn_msg,
                Some("config.yaml".into()),
            ));
        } else {
            staged.insert(Action::Global(GlobalAction::ZenToggle), vec![legacy]);
        }
    }

    // Parse every chord string into a real KeyChord, dropping the ones
    // that fail to parse (those actions fall back to defaults below).
    let mut parsed: HashMap<Action, Vec<KeyChord>> = HashMap::new();
    for (action, chords) in &staged {
        let mut ok = Vec::with_capacity(chords.len());
        for raw in chords {
            match KeyChord::parse(raw) {
                Ok(c) => ok.push(c),
                Err(e) => {
                    let location = Some(format!("{}.{}", action.mode(), action.key_name()));
                    diags.push(KeymapDiagnostic::err(
                        ErrorKind::ParseError,
                        format!("invalid chord `{raw}`: {e}"),
                        location,
                    ));
                }
            }
        }
        if ok.is_empty() {
            // All overrides failed — fall back to built-ins. Built-ins
            // are author-controlled, so this can't fail in production.
            ok = action
                .default_chords()
                .iter()
                .filter_map(|s| KeyChord::parse(s).ok())
                .collect();
        }
        parsed.insert(*action, ok);
    }

    // Collision detection per mode. Two actions in the same mode bound
    // to the same chord → revert both to their defaults and emit an Error.
    let modes = ["global", "sidebar", "kanban", "popover", "review", "activity", "palette"];
    for mode in modes {
        let mut by_chord: HashMap<KeyChord, Vec<Action>> = HashMap::new();
        for (action, chords) in &parsed {
            if action.mode() != mode {
                continue;
            }
            for c in chords {
                by_chord.entry(*c).or_default().push(*action);
            }
        }
        for (chord, actions) in by_chord {
            if actions.len() < 2 {
                continue;
            }
            let names: Vec<String> = actions.iter().map(|a| a.key_name().to_string()).collect();
            diags.push(KeymapDiagnostic::err(
                ErrorKind::Collision,
                format!(
                    "chord `{}` is bound to multiple actions ({}) in `{mode}`; \
                     reverting them to defaults",
                    chord.canonical(),
                    names.join(", ")
                ),
                Some(mode.to_string()),
            ));
            for a in &actions {
                let defaults: Vec<KeyChord> = a
                    .default_chords()
                    .iter()
                    .filter_map(|s| KeyChord::parse(s).ok())
                    .collect();
                parsed.insert(*a, defaults);
            }
        }
    }

    // Reserved-chord check: `ctrl-c` must always quit. If the merged
    // `global.quit` chord list no longer contains it (the user rebound the
    // action away), emit a warning but don't block — the user may have an
    // intentional reason (e.g. a Git pre-commit hook that allows it).
    if let Ok(ctrl_c) = KeyChord::parse("ctrl-c") {
        let quit_has_ctrl_c = parsed
            .get(&Action::Global(GlobalAction::Quit))
            .map(|v| v.contains(&ctrl_c))
            .unwrap_or(false);
        if !quit_has_ctrl_c {
            diags.push(KeymapDiagnostic::warn(
                WarningKind::ReservedChordRebind,
                "chord `ctrl-c` is reserved (must always quit); \
                 keep it in defaults.global.quit",
                Some("defaults.global.quit".into()),
            ));
        }
    }

    let keymaps = build_keymaps(&parsed);
    (keymaps, diags)
}

/// Apply one layer of overrides (defaults or a project block) onto the
/// staged chord map. Unknown modes / unknown action keys produce
/// diagnostics but don't poison the merge — every other override still
/// applies.
fn apply_overrides(
    scope: &str,
    layer: &ModeMap,
    staged: &mut HashMap<Action, Vec<String>>,
    diags: &mut Vec<KeymapDiagnostic>,
) {
    for (mode_name, entries) in layer {
        let actions_in_mode: Vec<Action> =
            Action::all().filter(|a| a.mode() == mode_name).collect();
        if actions_in_mode.is_empty() {
            diags.push(KeymapDiagnostic::err(
                ErrorKind::UnknownAction,
                format!("unknown mode `{mode_name}`"),
                Some(format!("{scope}.{mode_name}")),
            ));
            continue;
        }
        for (key_name, spec) in entries {
            let Some(action) = actions_in_mode.iter().find(|a| a.key_name() == key_name) else {
                diags.push(KeymapDiagnostic::err(
                    ErrorKind::UnknownAction,
                    format!("unknown action `{mode_name}.{key_name}`"),
                    Some(format!("{scope}.{mode_name}.{key_name}")),
                ));
                continue;
            };
            match spec.to_chords() {
                Some(list) => {
                    staged.insert(*action, list);
                }
                None => {
                    // Explicit YAML null — fall back to the layer below.
                    // No-op for staged; nothing to insert.
                }
            }
        }
    }
}

fn build_keymaps(parsed: &HashMap<Action, Vec<KeyChord>>) -> Keymaps {
    let mut km = Keymaps::default();
    for (action, chords) in parsed {
        match *action {
            Action::Global(a) => insert_into(&mut km.global, a, chords),
            Action::Sidebar(a) => insert_into(&mut km.sidebar, a, chords),
            Action::Kanban(a) => insert_into(&mut km.kanban, a, chords),
            Action::Popover(a) => insert_into(&mut km.popover, a, chords),
            Action::Review(a) => insert_into(&mut km.review, a, chords),
            Action::Activity(a) => insert_into(&mut km.activity, a, chords),
            Action::Palette(a) => insert_into(&mut km.palette, a, chords),
        }
    }
    km
}

fn insert_into<A: Copy + Eq + Hash>(
    map: &mut ModeKeymap<A>,
    action: A,
    chords: &[KeyChord],
) {
    for c in chords {
        map.bindings.insert(*c, action);
    }
    map.by_action.insert(action, chords.to_vec());
}

/// Read `~/.shelbi/keys.yml` if it exists. Parse errors get reported as
/// diagnostics and the caller falls through to built-ins.
fn read_keys_file(diags: &mut Vec<KeymapDiagnostic>) -> Option<KeysFile> {
    let path = match shelbi_home() {
        Ok(h) => h.join(KEYS_FILENAME),
        Err(_) => return None,
    };
    if !path.exists() {
        return None;
    }
    let text = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            diags.push(KeymapDiagnostic::err(
                ErrorKind::ParseError,
                format!("reading {}: {e}", path.display()),
                Some(KEYS_FILENAME.into()),
            ));
            return None;
        }
    };
    match serde_yaml::from_str::<KeysFile>(&text) {
        Ok(f) => Some(f),
        Err(e) => {
            diags.push(KeymapDiagnostic::err(
                ErrorKind::ParseError,
                format!("parsing {}: {e}", path.display()),
                Some(KEYS_FILENAME.into()),
            ));
            None
        }
    }
}

/// Translate the legacy `config.yaml::keymap.zen_toggle` enum into the
/// canonical chord string. `None` means the field was at its default
/// (AltZ — which is also the keys.yml built-in, so the warning would
/// just be noise). The probe stores AltZ when the user accepts the
/// default, so we treat "default value" as "user didn't actively pick
/// a fallback".
fn legacy_zen_toggle_chord() -> Option<String> {
    let cfg = load_user_config().ok()?;
    let chord = match cfg.keymap.zen_toggle {
        ZenToggleChord::AltZ => return None, // matches built-in; no migration needed.
        ZenToggleChord::CtrlBackslash => "ctrl-\\",
        ZenToggleChord::CtrlG => "ctrl-g",
        ZenToggleChord::CtrlShiftZ => "ctrl-shift-z",
        ZenToggleChord::None => return None, // "no chord" is not a chord; no migration.
    };
    Some(chord.to_string())
}

/// One-shot migration: copy a non-default `config.yaml::keymap.zen_toggle`
/// into `keys.yml::defaults.global.zen_toggle`, then reset the legacy
/// `config.yaml` field to its default so subsequent loads stop seeing
/// the disagreement.
///
/// Returns `Some(chord_string)` when the migration ran (so the caller can
/// emit the one-time notice), `None` otherwise. Cases that skip migration:
///
/// - Legacy field is at its built-in default (AltZ) or unbound (None).
///   Nothing to move.
/// - `keys.yml::defaults.global.zen_toggle` is already set. The keys.yml
///   value wins; leaving the legacy field to be flagged by the existing
///   [`WarningKind::LegacyZenToggleField`] path is the right surface for
///   the user to act on manually.
/// - `keys.yml` exists but is malformed. We don't want to clobber a file
///   we couldn't parse; the loader will report the parse error separately
///   and we leave the legacy field as the source of truth.
/// - Any IO write fails. We fall back to the legacy compat shim so the
///   chord still works in memory and emit a warning on the next load
///   too. Best-effort — a one-time migration warning is preferable to a
///   broken Zen toggle.
fn migrate_legacy_zen_toggle() -> Option<String> {
    // Step 1: read the legacy field. Bail early on the no-op cases.
    let chord = legacy_zen_toggle_chord()?;

    // Step 2: refuse to migrate when keys.yml exists but is unreadable
    // or unparseable. The user's hand-authored content matters more than
    // a clean reconciliation; the loader's separate parse-error
    // diagnostic will surface the underlying problem.
    let path = match shelbi_home() {
        Ok(h) => h.join(KEYS_FILENAME),
        Err(_) => return None,
    };
    let mut root = match read_keys_yml_value(&path) {
        ReadKeysYml::Empty => Value::Mapping(Mapping::new()),
        ReadKeysYml::Parsed(v) => v,
        ReadKeysYml::Unreadable => return None,
    };

    // Step 3: if the user has explicitly set defaults.global.zen_toggle,
    // we don't have a clean place to put the legacy chord — both configs
    // disagree and the user has to pick. Let the LegacyZenToggleField
    // warning fire from the caller.
    if keys_yml_defaults_zen_toggle(&root).is_some() {
        return None;
    }

    // Step 4: splice defaults.global.zen_toggle = <chord> into the
    // existing keys.yml tree, preserving every other key.
    let root_map = match root.as_mapping_mut() {
        Some(m) => m,
        None => {
            // The file parsed as a YAML scalar / sequence at the top
            // level (not a mapping). That's not a shape the loader
            // expects, so don't overwrite — let the parse error path
            // surface and the user decide.
            return None;
        }
    };
    let defaults = upsert_mapping(root_map, "defaults")?;
    let global = upsert_mapping(defaults, "global")?;
    global.insert(
        Value::String("zen_toggle".into()),
        Value::String(chord.clone()),
    );

    // Step 5: write keys.yml. If serialization or IO fails, bail and
    // leave the legacy field as the runtime source of truth.
    let yaml = serde_yaml::to_string(&root).ok()?;
    atomic_write(&path, yaml.as_bytes()).ok()?;

    // Step 6: reset the legacy field so the next startup is silent.
    // Keep the file on disk — `zen_probe::ensure_zen_keymap` treats a
    // missing config.yaml as "first run" and would otherwise re-prompt
    // the user with the fallback chooser they already escaped from.
    //
    // If this write fails, keys.yml already has the chord — the next
    // load will re-trigger the migration warning (and retry), but the
    // chord still resolves correctly. Worst case is a repeat notice,
    // not a broken binding.
    let mut cfg = load_user_config().unwrap_or_default();
    cfg.keymap.zen_toggle = ZenToggleChord::default();
    let _ = save_user_config(&cfg);

    Some(chord)
}

enum ReadKeysYml {
    /// File doesn't exist OR exists but is empty/whitespace-only.
    Empty,
    /// File parsed cleanly as a YAML value.
    Parsed(Value),
    /// File exists but couldn't be read or parsed. Migration must skip.
    Unreadable,
}

/// Read keys.yml as an untyped `serde_yaml::Value` so the migration can
/// mutate it without losing entries it doesn't know about. Missing or
/// empty file is treated as an empty mapping; parse errors are caller-
/// visible as `Unreadable` so the migration can refuse to clobber.
fn read_keys_yml_value(path: &Path) -> ReadKeysYml {
    if !path.exists() {
        return ReadKeysYml::Empty;
    }
    let text = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return ReadKeysYml::Unreadable,
    };
    if text.trim().is_empty() {
        return ReadKeysYml::Empty;
    }
    match serde_yaml::from_str::<Value>(&text) {
        Ok(v) => ReadKeysYml::Parsed(v),
        Err(_) => ReadKeysYml::Unreadable,
    }
}

/// Read `defaults.global.zen_toggle` from a parsed keys.yml value, if any
/// value is present (a YAML `null` counts as "explicitly cleared", which
/// the loader treats as "fall through to a lower layer" — same semantics
/// as the typed `ChordSpec::None` deserialize path). Returns the raw
/// `serde_yaml::Value` so the caller can distinguish "not set" from
/// "set to null".
fn keys_yml_defaults_zen_toggle(root: &Value) -> Option<&Value> {
    root.as_mapping()?
        .get(Value::String("defaults".into()))?
        .as_mapping()?
        .get(Value::String("global".into()))?
        .as_mapping()?
        .get(Value::String("zen_toggle".into()))
}

/// Ensure `parent[key]` exists and is a mapping. If the entry is missing
/// it gets created; if it already exists as something other than a
/// mapping (scalar, sequence), we refuse — surgically rewriting a non-
/// mapping value would lose information.
fn upsert_mapping<'a>(parent: &'a mut Mapping, key: &str) -> Option<&'a mut Mapping> {
    let k = Value::String(key.into());
    if !parent.contains_key(&k) {
        parent.insert(k.clone(), Value::Mapping(Mapping::new()));
    }
    let entry = parent.get_mut(&k)?;
    entry.as_mapping_mut()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use crate::user_config::UserConfig;
    use crate::{ensure_dir, save_user_config};
    use crossterm::event::{KeyCode, KeyModifiers};

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-keys-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn builtin_defaults_match_live_handler_behavior() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, diags) = load_keymaps(None);
        assert!(
            diags.is_empty(),
            "expected no diagnostics with no keys.yml, got {diags:?}"
        );

        // Spot-check a representative chord from every mode against the
        // hardcoded mappings in shelbi-tui/src/lib.rs and palette.rs.
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(GlobalAction::Quit)
        );
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)),
            Some(GlobalAction::ZenToggle)
        );
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(GlobalAction::OpenPalette)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        assert_eq!(
            km.kanban.dispatch(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            Some(KanbanAction::MoveCardLeft)
        );
        assert_eq!(
            km.kanban.dispatch(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(KanbanAction::ReorderUp)
        );
        assert_eq!(
            km.popover.dispatch(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(PopoverAction::Close)
        );
        assert_eq!(
            km.review.dispatch(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(ReviewAction::Activate)
        );
        assert_eq!(
            km.activity.dispatch(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)),
            Some(ActivityAction::ToggleZenFilter)
        );
        assert_eq!(
            km.palette.dispatch(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(PaletteAction::Close)
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn defaults_block_sparsely_overrides_built_in() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  sidebar:\n    nav_up: w\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "{diags:?}");
        // nav_up is now `w` (replaces — not unions — the default).
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        // The old defaults are gone.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            None
        );
        // Other actions still have their defaults.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn project_block_sparsely_overrides_defaults_and_builtin() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "\
defaults:
  sidebar:
    nav_up: w
projects:
  shelbi:
    sidebar:
      nav_down: s
",
        )
        .unwrap();

        let (km, _) = load_keymaps(Some("shelbi"));
        // defaults layer's override survives where projects didn't touch.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        // project's override replaces the default.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            None
        );

        // For an unrelated project the override does NOT apply.
        let (km_other, _) = load_keymaps(Some("other"));
        assert_eq!(
            km_other.sidebar.dispatch(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km_other.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn legacy_zen_toggle_field_migrates_into_keys_yml_and_warns_once() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlG;
        save_user_config(&cfg).unwrap();

        let (km, diags) = load_keymaps(None);
        // Migrated chord wins on the very first load.
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(GlobalAction::ZenToggle)
        );
        // Alt+Z is no longer the binding.
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)),
            None
        );
        // One-time migration notice fires.
        assert!(
            diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleMigrated,
                    ..
                }
            )),
            "expected LegacyZenToggleMigrated warning, got {diags:?}"
        );

        // keys.yml now contains the migrated entry.
        let keys_text = std::fs::read_to_string(home.join("keys.yml")).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&keys_text).unwrap();
        let migrated = parsed
            .get("defaults")
            .and_then(|v| v.get("global"))
            .and_then(|v| v.get("zen_toggle"))
            .and_then(|v| v.as_str());
        assert_eq!(migrated, Some("ctrl-g"), "keys.yml content: {keys_text}");

        // Legacy config field has been reset to its built-in default —
        // so the second load is silent and the keys.yml binding still
        // wins.
        let cfg_after = load_user_config().unwrap();
        assert_eq!(cfg_after.keymap.zen_toggle, ZenToggleChord::AltZ);

        let (km2, diags2) = load_keymaps(None);
        assert!(
            !diags2.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleMigrated
                        | WarningKind::LegacyZenToggleField,
                    ..
                }
            )),
            "second load should be silent, got {diags2:?}"
        );
        assert_eq!(
            km2.global.dispatch(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(GlobalAction::ZenToggle)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn legacy_migration_preserves_other_keys_yml_entries() {
        // A pre-existing keys.yml must keep its other overrides intact
        // after the legacy zen_toggle migration spliced into it. We're
        // not surgical-text-editing the file, so explicitly assert the
        // other-mode entry survives the round-trip.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  sidebar:\n    nav_up: w\n",
        )
        .unwrap();
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlG;
        save_user_config(&cfg).unwrap();

        let (km, _diags) = load_keymaps(None);
        // The pre-existing nav_up override survives the migration.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        // And the migrated zen_toggle is in effect.
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(GlobalAction::ZenToggle)
        );

        // The merged keys.yml on disk reflects both overrides.
        let keys_text = std::fs::read_to_string(home.join("keys.yml")).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&keys_text).unwrap();
        assert_eq!(
            parsed
                .get("defaults")
                .and_then(|v| v.get("sidebar"))
                .and_then(|v| v.get("nav_up"))
                .and_then(|v| v.as_str()),
            Some("w"),
            "{keys_text}"
        );
        assert_eq!(
            parsed
                .get("defaults")
                .and_then(|v| v.get("global"))
                .and_then(|v| v.get("zen_toggle"))
                .and_then(|v| v.as_str()),
            Some("ctrl-g"),
            "{keys_text}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn new_field_wins_but_legacy_warning_still_fires_when_keys_yml_already_set() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlG;
        save_user_config(&cfg).unwrap();

        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  global:\n    zen_toggle: ctrl-\\\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        // New field wins — the two configs disagree so migration must
        // refuse to clobber the keys.yml value.
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)),
            Some(GlobalAction::ZenToggle)
        );
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            None
        );
        // Warning fires telling the user to remove the legacy field.
        assert!(
            diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleField,
                    ..
                }
            )),
            "expected LegacyZenToggleField warning, got {diags:?}"
        );
        // The migrate-and-rewrite path must NOT have fired here.
        assert!(
            !diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleMigrated,
                    ..
                }
            )),
            "migration must not run when keys.yml is already set: {diags:?}"
        );
        // The legacy field is preserved on disk so the user can see and
        // remove it; we don't silently clobber when there's a conflict.
        let cfg_after = load_user_config().unwrap();
        assert_eq!(cfg_after.keymap.zen_toggle, ZenToggleChord::CtrlG);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn fresh_default_config_emits_no_zen_toggle_diagnostics() {
        // Regression for the underlying bug: orchestrator startup with a
        // default config (or no config) must not surface a `zen_toggle`
        // warning. The legacy compat path treats AltZ as "user didn't
        // override anything" — and the migration only fires for non-
        // default values.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // No config.yaml, no keys.yml.
        let (_km, diags) = load_keymaps(None);
        assert!(
            diags.iter().all(|d| !matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleField
                        | WarningKind::LegacyZenToggleMigrated,
                    ..
                }
            )),
            "fresh install: {diags:?}"
        );

        // Default-valued config.yaml on disk.
        save_user_config(&UserConfig::default()).unwrap();
        let (_km, diags) = load_keymaps(None);
        assert!(
            diags.iter().all(|d| !matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleField
                        | WarningKind::LegacyZenToggleMigrated,
                    ..
                }
            )),
            "default config: {diags:?}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn legacy_none_chord_does_not_migrate() {
        // `ZenToggleChord::None` is the "skip the hotkey" outcome of the
        // first-run probe — translating it into keys.yml would mean
        // unbinding the chord, which the migration deliberately doesn't
        // do (the user can still toggle via `shelbi zen on/off`, so the
        // legacy field staying at None is a benign no-op).
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::None;
        save_user_config(&cfg).unwrap();

        let (_km, diags) = load_keymaps(None);
        assert!(
            diags.iter().all(|d| !matches!(
                d,
                KeymapDiagnostic::Warning {
                    kind: WarningKind::LegacyZenToggleField
                        | WarningKind::LegacyZenToggleMigrated,
                    ..
                }
            )),
            "skip-chord should be silent: {diags:?}"
        );
        // Legacy field is unchanged.
        let cfg_after = load_user_config().unwrap();
        assert_eq!(cfg_after.keymap.zen_toggle, ZenToggleChord::None);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_toggle_chord_resolver_prefers_keys_yml_over_legacy() {
        // After migration, `cfg.keymap.zen_toggle` is reset to AltZ but
        // the actual binding lives in keys.yml. The sidebar glyph must
        // reflect the keys.yml binding, not the legacy default — that
        // mismatch is the UI face of the parallel-config problem this
        // task is fixing.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  global:\n    zen_toggle: ctrl-g\n",
        )
        .unwrap();

        let (km, _diags) = load_keymaps(None);
        // Legacy fallback is AltZ; keys.yml resolver wins with CtrlG.
        assert_eq!(
            km.zen_toggle_chord(ZenToggleChord::AltZ),
            ZenToggleChord::CtrlG
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_toggle_chord_resolver_falls_back_for_non_preset_chord() {
        // When the user binds an arbitrary chord in keys.yml that the
        // four-value preset enum can't represent (e.g. `f6`), the
        // resolver hands back the legacy fallback so the sidebar can
        // still pick a sensible glyph.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  global:\n    zen_toggle: f6\n",
        )
        .unwrap();

        let (km, _diags) = load_keymaps(None);
        assert_eq!(
            km.zen_toggle_chord(ZenToggleChord::CtrlBackslash),
            ZenToggleChord::CtrlBackslash
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn intra_mode_collision_reverts_to_defaults_and_errors() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        // Bind both nav_up and nav_down to the same chord.
        std::fs::write(
            home.join("keys.yml"),
            "\
defaults:
  sidebar:
    nav_up: x
    nav_down: x
",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        let coll = diags.iter().any(|d| matches!(
            d,
            KeymapDiagnostic::Error {
                kind: ErrorKind::Collision,
                ..
            }
        ));
        assert!(coll, "expected Collision diagnostic, got {diags:?}");

        // Both colliding actions revert to defaults.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        // The colliding chord itself is no longer bound.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn unknown_action_in_keys_yml_emits_diagnostic_but_does_not_block() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  sidebar:\n    bogus_action: w\n",
        )
        .unwrap();
        let (km, diags) = load_keymaps(None);
        // Default bindings still loaded.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        assert!(diags.iter().any(|d| matches!(
            d,
            KeymapDiagnostic::Error { kind: ErrorKind::UnknownAction, .. }
        )));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn unknown_mode_in_keys_yml_emits_diagnostic_but_does_not_block() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  bogus_mode:\n    nav_up: w\n",
        )
        .unwrap();
        let (km, diags) = load_keymaps(None);
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        assert!(diags.iter().any(|d| matches!(
            d,
            KeymapDiagnostic::Error { kind: ErrorKind::UnknownAction, .. }
        )));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn null_falls_back_to_layer_below() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yml"),
            "\
defaults:
  sidebar:
    nav_up: w
projects:
  shelbi:
    sidebar:
      nav_up: null
",
        )
        .unwrap();
        let (km, diags) = load_keymaps(Some("shelbi"));
        assert!(diags.is_empty(), "{diags:?}");
        // null in project falls back to defaults' `w`.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn missing_file_yields_pure_defaults_no_diagnostics() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "{diags:?}");
        // chords_for / first_chord_for round-trip a sample.
        let first = km.sidebar.first_chord_for(SidebarAction::NavUp).unwrap();
        assert_eq!(first, &chord("k"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn every_default_chord_parses_and_round_trips_canonical_form() {
        // Acceptance criterion: every chord in the embedded defaults
        // table must parse, and `parse(canonical(parse(s)?)?)? == parse(s)?`.
        for action in Action::all() {
            for raw in action.default_chords() {
                let a = KeyChord::parse(raw).unwrap_or_else(|e| {
                    panic!("default chord {raw} for {action:?} failed to parse: {e}")
                });
                let canon = a.canonical();
                let b = KeyChord::parse(&canon).unwrap_or_else(|e| {
                    panic!("canonical {canon} for {action:?} failed to re-parse: {e}")
                });
                assert_eq!(
                    a, b,
                    "round trip broken for {action:?}: {raw} → {canon}"
                );
            }
        }
    }

    #[test]
    fn dispatch_handles_uppercase_letter_without_shift_modifier() {
        // Some terminals report `KeyCode::Char('J')` with NONE mods. The
        // dispatcher must still hit a `shift-j` binding.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, _) = load_keymaps(None);
        assert_eq!(
            km.kanban.dispatch(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE)),
            Some(KanbanAction::ReorderDown)
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
