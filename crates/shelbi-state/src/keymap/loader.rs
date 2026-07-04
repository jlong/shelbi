//! `~/.shelbi/keys.yaml` loader and three-layer merge.
//!
//! Layers, lowest → highest priority:
//!
//! 1. Embedded built-ins from each [`Action::default_chords`].
//! 2. `keys.yaml::defaults.<mode>.<action>` if present.
//! 3. `keys.yaml::projects.<project>.<mode>.<action>` if present.
//!
//! Before the merge runs, [`load_keymaps`] performs a one-shot migration:
//! if `config.yaml::keymap.zen_toggle` names a chord other than the
//! built-in default (and `keys.yaml::defaults.global.zen_toggle` isn't
//! already set), the chord is copied into `keys.yaml`, the legacy
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

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::Hash;
use std::path::Path;

use crossterm::event::KeyEvent;
use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use super::actions::{
    Action, ActivityAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction,
    SidebarAction, MODE_NAMES,
};
use super::chord::KeyChord;
use crate::user_config::{load_user_config, save_user_config, ZenToggleChord};
use crate::{atomic_write, shelbi_home};

/// Filename under `$SHELBI_HOME` for user-authored key overrides.
pub const KEYS_FILENAME: &str = "keys.yaml";

/// Final, merged keymaps for every mode. Constructed by [`load_keymaps`]
/// from the three-layer merge — callers should treat fields as read-only
/// after construction.
#[derive(Debug, Clone, Default)]
pub struct Keymaps {
    pub global: ModeKeymap<GlobalAction>,
    pub sidebar: ModeKeymap<SidebarAction>,
    pub kanban: ModeKeymap<KanbanAction>,
    pub popover: ModeKeymap<PopoverAction>,
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
    /// hint. Prefers the keys.yaml-resolved binding (so once the legacy
    /// `config.yaml::keymap.zen_toggle` migrates into keys.yaml the glyph
    /// matches what the user actually pressed); falls back to
    /// `legacy_fallback` when the chord doesn't match one of the four
    /// preset variants (e.g. the user bound `f6` directly in keys.yaml)
    /// or is unbound entirely.
    ///
    /// The fallback is the chord the caller already read from
    /// `~/.shelbi/config.yaml` (typically via the first-run probe). On
    /// fresh installs that's the right answer; after a successful
    /// legacy migration the keys.yaml lookup wins and the fallback is
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
    /// migrated because `keys.yaml::defaults.global.zen_toggle` already
    /// holds an explicit value. The keys.yaml entry wins; the user is
    /// asked to remove the legacy field by hand so the two configs stop
    /// disagreeing.
    LegacyZenToggleField,
    /// One-shot migration succeeded: the legacy
    /// `config.yaml::keymap.zen_toggle` chord was written into
    /// `keys.yaml::defaults.global.zen_toggle` and the legacy field was
    /// reset to its default. Emitted once on the migrating startup; the
    /// next load sees the legacy field at its default and stays silent.
    LegacyZenToggleMigrated,
    /// A chord bound in a mode that consults `global` first (sidebar,
    /// kanban, activity) also matches a global binding, so the
    /// per-mode binding can never fire — global dispatch wins. Emitted
    /// only when a user override introduced the shadow (F12).
    ShadowedByGlobal,
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

/// `mode -> action -> raw value` — one layer of the merge.
///
/// The leaf is an untyped [`Value`] rather than a typed `ChordSpec` so a
/// single mistyped scalar (`nav_up: 5`, `zen_toggle: true`) fails only
/// that one entry — with a located diagnostic — instead of poisoning the
/// whole-file deserialize and reverting every override (F13). The
/// value → chord conversion happens per entry in [`value_to_chords`].
type ModeMap = HashMap<String, HashMap<String, Value>>;

/// Top-level structure of `keys.yaml`. Both blocks are optional so a file
/// with just `defaults` or just `projects` still parses.
#[derive(Debug, Default, Deserialize)]
struct KeysFile {
    #[serde(default)]
    defaults: Option<ModeMap>,
    #[serde(default)]
    projects: Option<HashMap<String, ModeMap>>,
}

/// Convert one raw `keys.yaml` value into a chord override. The on-disk
/// form can be:
///
/// - YAML null → `Ok(None)`: fall back to the layer below (no override).
/// - a scalar string (`alt-z`) → one chord.
/// - a list (`[k, up]`) → several chords for the same action; an empty
///   list is a deliberate unbind (`Ok(Some(vec![]))`).
/// - anything else (a number, bool, mapping, or a non-string list item) →
///   `Err(message)`, so the caller can emit a located diagnostic for just
///   this entry instead of failing the whole file (F13).
fn value_to_chords(value: &Value) -> Result<Option<Vec<String>>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(vec![s.clone()])),
        Value::Sequence(seq) => {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    other => {
                        return Err(format!(
                            "expected a chord string in the list, found {}",
                            value_type_name(other)
                        ));
                    }
                }
            }
            Ok(Some(out))
        }
        other => Err(format!(
            "expected a chord string or a list of chord strings, found {}",
            value_type_name(other)
        )),
    }
}

/// Human-readable name for a `serde_yaml::Value` variant, used in the
/// mistyped-entry diagnostics from [`value_to_chords`].
fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a list",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
    }
}

// ---------------------------------------------------------------------------
// Loader.

/// Load the merged keymaps for `project_name` (or just `defaults`+builtin
/// if `None`). Always returns a usable [`Keymaps`]; any errors from the
/// user's `keys.yaml` are reported via the diagnostic list and the
/// affected actions silently keep their built-in defaults.
///
/// As a side effect, runs the one-shot legacy-zen-toggle migration first
/// (see [`migrate_legacy_zen_toggle`]). The migration rewrites `keys.yaml`
/// and `config.yaml` on disk, so subsequent calls observe the migrated
/// state and stay silent.
pub fn load_keymaps(project_name: Option<&str>) -> (Keymaps, Vec<KeymapDiagnostic>) {
    let mut diags = Vec::new();

    // One-shot: rename a legacy `~/.shelbi/keys.yml` to the canonical
    // `keys.yaml` before any read below, so the rest of this load — and
    // the zen-toggle migration that also rewrites the file — observes and
    // writes only `.yaml`.
    migrate_keys_extension();

    // One-shot: migrate the legacy config.yaml field into keys.yaml before
    // the merge so the merge picks up the migrated chord on this very
    // call. Emits the migration warning on success — the next startup
    // sees the legacy field at its default and stays silent.
    if let Some(chord) = migrate_legacy_zen_toggle() {
        let msg = format!(
            "migrated config.yaml::keymap.zen_toggle (`{chord}`) into \
             keys.yaml::defaults.global.zen_toggle; the legacy field is no \
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

    // Layer 2/3: load keys.yaml if present. Missing file is fine.
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
    //   - keys.yaml already overrides `defaults.global.zen_toggle`: the
    //     two configs disagree — keys.yaml wins, but emit the deprecation
    //     warning so the user knows to remove the dead legacy field.
    //   - keys.yaml doesn't override: forward the chord for this load.
    //     (In the common path the migration above already rewrote
    //     keys.yaml so this branch becomes inert on subsequent loads.)
    if let Some(legacy) = legacy_zen_toggle_chord() {
        if zen_overridden_in_defaults {
            let warn_msg = "config.yaml::keymap.zen_toggle is deprecated; \
                 keys.yaml::defaults.global.zen_toggle is already set — \
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
                // F10: dedupe within one action's list so `[k, up, k]`
                // doesn't self-collide (two occurrences of the same chord
                // for the *same* action isn't a real collision).
                Ok(c) if ok.contains(&c) => {}
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
        // F3: distinguish "explicit unbind" from "everything failed to
        // parse". An empty on-disk list (`action: []`) is a deliberate
        // unbind and stays empty. A non-empty list that produced no valid
        // chords means every entry was a typo — fall back to built-ins so
        // the action isn't left dead by accident (the per-chord
        // ParseError diagnostics above are the signal distinguishing the
        // two: a real unbind emits none).
        if ok.is_empty() && !chords.is_empty() {
            ok = action
                .default_chords()
                .iter()
                .filter_map(|s| KeyChord::parse(s).ok())
                .collect();
        }
        parsed.insert(*action, ok);
    }

    // Collision detection per mode. Two *different* actions in the same
    // mode bound to the same chord → revert both to their defaults and
    // emit an Error.
    //
    // F4: reverting a colliding action to its defaults can itself create a
    // fresh collision (the reverted default now equal to a third action's
    // surviving override). A single pass would miss that, and because
    // `build_keymaps` used to iterate a HashMap the survivor was picked by
    // random order. So iterate to a fixed point: keep re-scanning until a
    // full pass reverts nothing. Each revert strictly moves an action from
    // an override to its (fixed) built-in default, so this converges in at
    // most one revert per action. `reported` dedupes the diagnostic per
    // (mode, chord) so an unresolvable defaults-level clash is surfaced
    // once rather than every pass.
    //
    // Drive the per-mode scan off the canonical `MODE_NAMES` so a
    // newly-added mode is covered automatically instead of silently
    // skipping collision checks until someone remembers to extend a
    // duplicated local list.
    let mut reported: HashSet<(String, String)> = HashSet::new();
    loop {
        let mut changed = false;
        for &mode in MODE_NAMES {
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
                if reported.insert((mode.to_string(), chord.canonical())) {
                    // Sort so the message is deterministic regardless of
                    // HashMap iteration order.
                    let mut names: Vec<String> =
                        actions.iter().map(|a| a.key_name().to_string()).collect();
                    names.sort();
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
                }
                for a in &actions {
                    let defaults: Vec<KeyChord> = a
                        .default_chords()
                        .iter()
                        .filter_map(|s| KeyChord::parse(s).ok())
                        .collect();
                    if parsed.get(a) != Some(&defaults) {
                        parsed.insert(*a, defaults);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
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

    // F12: a chord bound in a mode that consults `global` first is dead if
    // it also matches a global binding — global dispatch wins before the
    // per-mode lookup runs. (Popover and palette are modal and skip global
    // entirely, so they're exempt.) Warn, same shape as the reserved-chord
    // warning above.
    //
    // Subtract the shadows already present in the pure-default baseline:
    // the built-in `sidebar.quit` deliberately shares `ctrl-c` with
    // `global.quit`, and that overlap is benign (both quit). Reporting only
    // shadows the *user* introduced keeps a fresh install — and a
    // full-config dump/round-trip — silent while still flagging a genuine
    // dead binding like `sidebar.refresh: ctrl-p`.
    let baseline_shadows = global_shadows(&default_keymap_chords());
    for (action, chord, global_action) in global_shadows(&parsed) {
        if baseline_shadows
            .iter()
            .any(|(a, c, _)| *a == action && *c == chord)
        {
            continue;
        }
        diags.push(KeymapDiagnostic::warn(
            WarningKind::ShadowedByGlobal,
            format!(
                "chord `{}` for `{}.{}` is also bound to global `{}`; the \
                 global binding fires first, so this per-mode binding is dead",
                chord.canonical(),
                action.mode(),
                action.key_name(),
                global_action.key_name(),
            ),
            Some(format!("{}.{}", action.mode(), action.key_name())),
        ));
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
        for (key_name, value) in entries {
            let Some(action) = actions_in_mode.iter().find(|a| a.key_name() == key_name) else {
                diags.push(KeymapDiagnostic::err(
                    ErrorKind::UnknownAction,
                    format!("unknown action `{mode_name}.{key_name}`"),
                    Some(format!("{scope}.{mode_name}.{key_name}")),
                ));
                continue;
            };
            match value_to_chords(value) {
                Ok(Some(list)) => {
                    staged.insert(*action, list);
                }
                Ok(None) => {
                    // Explicit YAML null — fall back to the layer below.
                    // No-op for staged; nothing to insert.
                }
                Err(msg) => {
                    // F13: a single mistyped entry (`nav_up: 5`,
                    // `zen_toggle: true`) is reported here and skipped;
                    // every other override in the file still applies.
                    diags.push(KeymapDiagnostic::err(
                        ErrorKind::ParseError,
                        format!("invalid override `{mode_name}.{key_name}`: {msg}"),
                        Some(format!("{scope}.{mode_name}.{key_name}")),
                    ));
                }
            }
        }
    }
}

fn build_keymaps(parsed: &HashMap<Action, Vec<KeyChord>>) -> Keymaps {
    let mut km = Keymaps::default();
    // Iterate in `Action::all()` order rather than the HashMap's random
    // order so that, should any residual same-mode conflict survive the
    // collision pass, the chord binds to the same action on every load
    // (combined with `insert_into`'s refuse-to-overwrite guard). No
    // HashMap-iteration-order-dependent bindings.
    for action in Action::all() {
        let Some(chords) = parsed.get(&action) else {
            continue;
        };
        match action {
            Action::Global(a) => insert_into(&mut km.global, a, chords),
            Action::Sidebar(a) => insert_into(&mut km.sidebar, a, chords),
            Action::Kanban(a) => insert_into(&mut km.kanban, a, chords),
            Action::Popover(a) => insert_into(&mut km.popover, a, chords),
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
        match map.bindings.get(c) {
            // Another action already claimed this chord. The collision
            // pass should have prevented this, but refuse to clobber so a
            // residual conflict resolves deterministically (first action
            // in `Action::all()` order wins) rather than by HashMap order.
            Some(existing) if *existing != action => continue,
            _ => {
                map.bindings.insert(*c, action);
            }
        }
    }
    map.by_action.insert(action, chords.to_vec());
}

/// Modes whose key handlers consult `global` before their own bindings, so
/// a chord shared with a global binding never reaches the mode. Popover and
/// palette are modal and skip `global`, so they're deliberately excluded.
const GLOBAL_CONSULTING_MODES: &[&str] = &["sidebar", "kanban", "activity"];

/// Every action's built-in chord list, parsed. The baseline the F12 shadow
/// check subtracts so intentional default overlaps (e.g. `sidebar.quit`
/// sharing `ctrl-c` with `global.quit`) don't warn.
fn default_keymap_chords() -> HashMap<Action, Vec<KeyChord>> {
    let mut map = HashMap::new();
    for action in Action::all() {
        let chords: Vec<KeyChord> = action
            .default_chords()
            .iter()
            .filter_map(|s| KeyChord::parse(s).ok())
            .collect();
        map.insert(action, chords);
    }
    map
}

/// Find `(mode_action, chord, global_action)` triples where a chord bound
/// in a global-consulting mode also matches a `global` binding — i.e. dead
/// bindings shadowed by global dispatch. Iterates in `Action::all()` order
/// for a stable diagnostic sequence.
fn global_shadows(chords: &HashMap<Action, Vec<KeyChord>>) -> Vec<(Action, KeyChord, Action)> {
    let mut global_chords: HashMap<KeyChord, Action> = HashMap::new();
    for (action, cs) in chords {
        if action.mode() == "global" {
            for c in cs {
                global_chords.insert(*c, *action);
            }
        }
    }
    let mut out = Vec::new();
    for action in Action::all() {
        if !GLOBAL_CONSULTING_MODES.contains(&action.mode()) {
            continue;
        }
        let Some(cs) = chords.get(&action) else {
            continue;
        };
        for c in cs {
            if let Some(global_action) = global_chords.get(c) {
                out.push((action, *c, *global_action));
            }
        }
    }
    out
}

/// One-shot rename of a legacy `~/.shelbi/keys.yml` to the canonical
/// `~/.shelbi/keys.yaml`. Shelbi standardized every config file on the
/// `.yaml` extension; this converts a file written by a
/// pre-standardization binary in place so the loader reads and writes
/// only `.yaml` afterward.
///
/// Safety mirrors the project-scoped `migrate_statuses_extension`: rename
/// only when the legacy `.yml` exists **and** the canonical `.yaml` does
/// not, so a `.yaml` a newer binary already wrote is never clobbered;
/// `fs::rename` is atomic so no data is dropped. When both exist the
/// legacy `.yml` is left in place and a warning is logged. Best-effort and
/// idempotent — [`load_keymaps`] runs it first so the subsequent read
/// observes the canonical `.yaml`.
fn migrate_keys_extension() {
    let Ok(home) = shelbi_home() else {
        return;
    };
    let legacy = home.join("keys.yml");
    if !legacy.exists() {
        return;
    }
    let canonical = home.join(KEYS_FILENAME);
    if canonical.exists() {
        tracing::warn!(
            "both {} and {} exist — leaving the legacy `.yml` in place; \
             shelbi reads `keys.yaml`. Remove the stale `.yml` to silence this.",
            legacy.display(),
            canonical.display(),
        );
        return;
    }
    if let Err(e) = fs::rename(&legacy, &canonical) {
        tracing::warn!(
            "failed to migrate {} to {}: {e}",
            legacy.display(),
            canonical.display(),
        );
    }
}

/// Read `~/.shelbi/keys.yaml` if it exists. Parse errors get reported as
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
/// (AltZ — which is also the keys.yaml built-in, so the warning would
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
/// into `keys.yaml::defaults.global.zen_toggle`, then reset the legacy
/// `config.yaml` field to its default so subsequent loads stop seeing
/// the disagreement.
///
/// Returns `Some(chord_string)` when the migration ran (so the caller can
/// emit the one-time notice), `None` otherwise. Cases that skip migration:
///
/// - Legacy field is at its built-in default (AltZ) or unbound (None).
///   Nothing to move.
/// - `keys.yaml::defaults.global.zen_toggle` is already set. The keys.yaml
///   value wins; leaving the legacy field to be flagged by the existing
///   [`WarningKind::LegacyZenToggleField`] path is the right surface for
///   the user to act on manually.
/// - `keys.yaml` exists but is malformed. We don't want to clobber a file
///   we couldn't parse; the loader will report the parse error separately
///   and we leave the legacy field as the source of truth.
/// - Any IO write fails. We fall back to the legacy compat shim so the
///   chord still works in memory and emit a warning on the next load
///   too. Best-effort — a one-time migration warning is preferable to a
///   broken Zen toggle.
fn migrate_legacy_zen_toggle() -> Option<String> {
    // Step 1: read the legacy field. Bail early on the no-op cases.
    let chord = legacy_zen_toggle_chord()?;

    // Step 2: refuse to migrate when keys.yaml exists but is unreadable
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
    // existing keys.yaml tree, preserving every other key.
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

    // Step 5: write keys.yaml. If serialization or IO fails, bail and
    // leave the legacy field as the runtime source of truth.
    let yaml = serde_yaml::to_string(&root).ok()?;
    atomic_write(&path, yaml.as_bytes()).ok()?;

    // Step 6: reset the legacy field so the next startup is silent.
    // Keep the file on disk — `zen_probe::ensure_zen_keymap` treats a
    // missing config.yaml as "first run" and would otherwise re-prompt
    // the user with the fallback chooser they already escaped from.
    //
    // If this write fails, keys.yaml already has the chord — the next
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

/// Read keys.yaml as an untyped `serde_yaml::Value` so the migration can
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

/// Read `defaults.global.zen_toggle` from a parsed keys.yaml value, if any
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
            "expected no diagnostics with no keys.yaml, got {diags:?}"
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
            home.join("keys.yaml"),
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
    fn load_keymaps_migrates_legacy_keys_yml_and_applies_it() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        // A pre-standardization user with overrides under the legacy .yml.
        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  sidebar:\n    nav_up: w\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "{diags:?}");

        // The file was renamed to the canonical .yaml...
        assert!(
            !home.join("keys.yml").exists(),
            "legacy keys.yml should be renamed away"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("keys.yaml")).unwrap(),
            "defaults:\n  sidebar:\n    nav_up: w\n",
            "content preserved across the rename"
        );
        // ...and its override took effect on this very load.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_keymaps_leaves_legacy_keys_yml_when_yaml_exists() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        // Both present: the canonical .yaml wins and the legacy .yml is
        // left untouched (never deleted) so no user data is lost.
        std::fs::write(home.join("keys.yml"), "defaults:\n  sidebar:\n    nav_up: q\n").unwrap();
        std::fs::write(home.join("keys.yaml"), "defaults:\n  sidebar:\n    nav_up: w\n").unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "{diags:?}");

        assert_eq!(
            std::fs::read_to_string(home.join("keys.yml")).unwrap(),
            "defaults:\n  sidebar:\n    nav_up: q\n",
            "legacy .yml must be left untouched when .yaml already exists"
        );
        // The `.yaml` binds nav_up to `w`, the stale `.yml` to `q`. `w`
        // resolving to NavUp is only possible if the `.yaml` — not the
        // `.yml` — was the file that got read.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp),
            "the canonical .yaml must be authoritative, not the stale .yml"
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
            home.join("keys.yaml"),
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

        // keys.yaml now contains the migrated entry.
        let keys_text = std::fs::read_to_string(home.join("keys.yaml")).unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(&keys_text).unwrap();
        let migrated = parsed
            .get("defaults")
            .and_then(|v| v.get("global"))
            .and_then(|v| v.get("zen_toggle"))
            .and_then(|v| v.as_str());
        assert_eq!(migrated, Some("ctrl-g"), "keys.yaml content: {keys_text}");

        // Legacy config field has been reset to its built-in default —
        // so the second load is silent and the keys.yaml binding still
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
        // A pre-existing keys.yaml must keep its other overrides intact
        // after the legacy zen_toggle migration spliced into it. We're
        // not surgical-text-editing the file, so explicitly assert the
        // other-mode entry survives the round-trip.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
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

        // The merged keys.yaml on disk reflects both overrides.
        let keys_text = std::fs::read_to_string(home.join("keys.yaml")).unwrap();
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
            home.join("keys.yaml"),
            "defaults:\n  global:\n    zen_toggle: ctrl-\\\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        // New field wins — the two configs disagree so migration must
        // refuse to clobber the keys.yaml value.
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
            "migration must not run when keys.yaml is already set: {diags:?}"
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

        // No config.yaml, no keys.yaml.
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
        // first-run probe — translating it into keys.yaml would mean
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
        // the actual binding lives in keys.yaml. The sidebar glyph must
        // reflect the keys.yaml binding, not the legacy default — that
        // mismatch is the UI face of the parallel-config problem this
        // task is fixing.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  global:\n    zen_toggle: ctrl-g\n",
        )
        .unwrap();

        let (km, _diags) = load_keymaps(None);
        // Legacy fallback is AltZ; keys.yaml resolver wins with CtrlG.
        assert_eq!(
            km.zen_toggle_chord(ZenToggleChord::AltZ),
            ZenToggleChord::CtrlG
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_toggle_chord_resolver_falls_back_for_non_preset_chord() {
        // When the user binds an arbitrary chord in keys.yaml that the
        // four-value preset enum can't represent (e.g. `f6`), the
        // resolver hands back the legacy fallback so the sidebar can
        // still pick a sensible glyph.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
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
            home.join("keys.yaml"),
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
            home.join("keys.yaml"),
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
            home.join("keys.yaml"),
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
            home.join("keys.yaml"),
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

    #[test]
    fn empty_list_unbinds_without_reverting_to_default() {
        // F3: `action: []` is a deliberate unbind. It must NOT fall back to
        // the built-in chords, and it must emit no diagnostic (the empty
        // list is intentional, not a failed parse).
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  sidebar:\n    nav_up: []\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "unbind should be silent, got {diags:?}");
        // Both former defaults are gone — nav_up is unbound.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            None
        );
        assert!(km.sidebar.first_chord_for(SidebarAction::NavUp).is_none());
        // A sibling action still keeps its default.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn all_chords_failing_to_parse_reverts_to_default_with_diagnostic() {
        // F3: the other side of the coin — a non-empty list where every
        // entry is a typo falls back to built-ins (so the action isn't left
        // dead), and emits a ParseError distinguishing it from an unbind.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  sidebar:\n    nav_up: [Up, gg]\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(
            diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Error { kind: ErrorKind::ParseError, .. }
            )),
            "expected ParseError, got {diags:?}"
        );
        // Fell back to the built-in `k` / `up`.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn duplicate_chord_in_one_list_does_not_self_collide() {
        // F10: `[k, up, k]` names the same chord twice for one action. That
        // is a dedupe case, not a collision — no Collision diagnostic, and
        // the override sticks (isn't discarded back to defaults).
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  sidebar:\n    nav_up: [k, up, k]\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(
            !diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Error { kind: ErrorKind::Collision, .. }
            )),
            "duplicate chord in one list must not self-collide, got {diags:?}"
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        // The reverse index is deduped too.
        assert_eq!(km.sidebar.chords_for(SidebarAction::NavUp).len(), 2);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn mistyped_scalar_affects_only_that_entry_not_whole_file() {
        // F13: a wrong-typed scalar (`nav_up: 5`) used to fail the whole
        // KeysFile deserialize and revert every override. Now it's a
        // per-entry located diagnostic; sibling overrides still apply.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "\
defaults:
  sidebar:
    nav_up: 5
    nav_down: s
  global:
    zen_toggle: true
",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        // The two bad entries are reported…
        let parse_errs = diags
            .iter()
            .filter(|d| matches!(
                d,
                KeymapDiagnostic::Error { kind: ErrorKind::ParseError, .. }
            ))
            .count();
        assert_eq!(parse_errs, 2, "expected two per-entry errors, got {diags:?}");
        // …the bad entries revert to their defaults…
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        assert_eq!(
            km.global.dispatch(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)),
            Some(GlobalAction::ZenToggle)
        );
        // …and the sibling override on the same file still took effect.
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn mistyped_list_item_reports_and_skips_that_entry() {
        // F13: a non-string item inside the list is also a per-entry error.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  sidebar:\n    nav_up: [w, 3]\n",
        )
        .unwrap();
        let (km, diags) = load_keymaps(None);
        assert!(
            diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Error { kind: ErrorKind::ParseError, .. }
            )),
            "expected ParseError, got {diags:?}"
        );
        // Whole entry skipped → reverts to default (not partially applied).
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cascading_collision_revert_converges_deterministically() {
        // F4: nav_up and nav_down both override to `x` (collide → both
        // revert to defaults). nav_down's default is `j`/`down`; a third
        // action, activate, is overridden to `j` — which now collides with
        // the reverted nav_down default. A single pass would miss it and
        // `j` would bind to whichever action HashMap iteration reached
        // last. The fixed-point loop must revert activate too and land on a
        // stable result every load.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "\
defaults:
  sidebar:
    nav_up: x
    nav_down: x
    activate: j
",
        )
        .unwrap();

        // Load repeatedly; the resolved binding for `j` must be identical
        // every time (no HashMap-order dependence).
        let mut seen = None;
        for _ in 0..20 {
            let (km, _diags) = load_keymaps(None);
            let j = km
                .sidebar
                .dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
            match seen {
                None => seen = Some(j),
                Some(prev) => assert_eq!(prev, j, "binding for `j` is nondeterministic"),
            }
        }
        // After both collision rounds, nav_down and activate are back at
        // their defaults, so `j` maps to nav_down (activate's default is
        // enter/space).
        let (km, diags) = load_keymaps(None);
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(SidebarAction::NavDown)
        );
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(SidebarAction::Activate)
        );
        // `x` is unbound (both original colliders reverted away from it).
        assert_eq!(
            km.sidebar.dispatch(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
        // Two distinct collisions were reported (x, and j).
        let collisions = diags
            .iter()
            .filter(|d| matches!(
                d,
                KeymapDiagnostic::Error { kind: ErrorKind::Collision, .. }
            ))
            .count();
        assert_eq!(collisions, 2, "expected two collision reports, got {diags:?}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn mode_chord_shadowed_by_global_warns() {
        // F12: sidebar consults `global` first, so binding `sidebar.refresh`
        // to `ctrl-p` (global.open_palette) makes the sidebar binding dead.
        // Warn.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  sidebar:\n    refresh: ctrl-p\n",
        )
        .unwrap();

        let (_km, diags) = load_keymaps(None);
        assert!(
            diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning { kind: WarningKind::ShadowedByGlobal, .. }
            )),
            "expected ShadowedByGlobal warning, got {diags:?}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn modal_mode_chord_matching_global_does_not_warn() {
        // F12: popover and palette are modal — they never consult `global`,
        // so a chord they share with a global binding is NOT dead and must
        // not warn. palette.close defaults already include `ctrl-p`; make
        // the overlap explicit via an override to exercise the gate.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&home).unwrap();
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  palette:\n    activate: ctrl-p\n",
        )
        .unwrap();

        let (_km, diags) = load_keymaps(None);
        assert!(
            !diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning { kind: WarningKind::ShadowedByGlobal, .. }
            )),
            "modal mode must not warn on global overlap, got {diags:?}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn default_config_does_not_warn_on_shared_ctrl_c() {
        // F12 gate: the built-in `sidebar.quit` deliberately shares
        // `ctrl-c` with `global.quit`. An untouched config must stay silent
        // — the warning only fires for user-introduced shadows.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (_km, diags) = load_keymaps(None);
        assert!(
            !diags.iter().any(|d| matches!(
                d,
                KeymapDiagnostic::Warning { kind: WarningKind::ShadowedByGlobal, .. }
            )),
            "default config must not warn on shared ctrl-c, got {diags:?}"
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
