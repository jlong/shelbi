//! `shelbi config` — discover, dump, and validate keybindings.
//!
//! Three sub-subcommands:
//!
//! - `list-actions` prints every action with its mode, name, description,
//!   and current (post-merge) chord list.
//! - `dump-keybindings` writes the full default keymap as YAML — drop-in
//!   contents for `~/.shelbi/keys.yml`.
//! - `check` validates `~/.shelbi/keys.yml` and reports any diagnostics
//!   the loader surfaces. Non-zero exit on errors; warnings stay quiet.
//!
//! All three inspect the merged [`Keymaps`] from `shelbi-state` so the
//! same logic feeds the TUI dispatcher (when a later task wires it up) and
//! these commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use shelbi_state::keymap::{
    load_keymaps, Action, KeyChord, KeymapDiagnostic, Keymaps, KEYS_FILENAME, MODE_NAMES,
};
use shelbi_state::{shelbi_home, user_config_path};

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print every action with its mode, name, description, and the
    /// chord(s) bound to it after the keys.yml merge.
    ListActions,
    /// Dump the full default keymap as YAML — drop the output into
    /// `~/.shelbi/keys.yml` as a starting point for customization.
    DumpKeybindings {
        /// Write the YAML to this path instead of stdout.
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
    },
    /// Validate `~/.shelbi/keys.yml` and print any errors / warnings.
    /// Exits 1 on errors; warnings still exit 0.
    Check,
}

pub fn run(project: Option<String>, cmd: ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::ListActions => list_actions(project),
        ConfigCmd::DumpKeybindings { out } => dump_keybindings(out),
        ConfigCmd::Check => check(project),
    }
}

/// Resolve the project name without erroring when none is configured —
/// `list-actions` and `check` should still work outside a project (they
/// just skip the per-project override layer).
fn resolve_project(explicit: Option<String>) -> Option<String> {
    crate::commands::require_project(explicit).ok()
}

// ---------------------------------------------------------------------------
// list-actions

fn list_actions(project: Option<String>) -> Result<()> {
    let project_name = resolve_project(project);
    let (keymaps, _diags) = load_keymaps(project_name.as_deref());
    print!("{}", render_actions_table(&keymaps));
    Ok(())
}

/// Pure formatter for `list-actions`. Returns the table text including
/// the header row and a trailing newline after the last row.
fn render_actions_table(keymaps: &Keymaps) -> String {
    let rows: Vec<(String, String, String, String)> = Action::all()
        .map(|a| {
            let chords = chords_for_action(keymaps, a);
            let chord_str = chords
                .iter()
                .map(|c| c.canonical())
                .collect::<Vec<_>>()
                .join(", ");
            (
                a.mode().to_string(),
                a.key_name().to_string(),
                a.description().to_string(),
                chord_str,
            )
        })
        .collect();

    let mode_w = "MODE"
        .len()
        .max(rows.iter().map(|r| r.0.len()).max().unwrap_or(0));
    let action_w = "ACTION"
        .len()
        .max(rows.iter().map(|r| r.1.len()).max().unwrap_or(0));
    let desc_w = "DESCRIPTION"
        .len()
        .max(rows.iter().map(|r| r.2.len()).max().unwrap_or(0));

    let mut out = String::new();
    out.push_str(&format!(
        "{:<mw$}  {:<aw$}  {:<dw$}  {}\n",
        "MODE",
        "ACTION",
        "DESCRIPTION",
        "CHORDS",
        mw = mode_w,
        aw = action_w,
        dw = desc_w,
    ));
    for (m, a, d, c) in &rows {
        out.push_str(&format!(
            "{:<mw$}  {:<aw$}  {:<dw$}  {}\n",
            m,
            a,
            d,
            c,
            mw = mode_w,
            aw = action_w,
            dw = desc_w,
        ));
    }
    out
}

/// Look up the chord list for `action` in the merged `Keymaps`. Returns
/// chords in install order — matches the order the merge applied them, so
/// help-text columns render predictably.
fn chords_for_action(km: &Keymaps, action: Action) -> Vec<KeyChord> {
    match action {
        Action::Global(a) => km.global.by_action.get(&a).cloned().unwrap_or_default(),
        Action::Sidebar(a) => km.sidebar.by_action.get(&a).cloned().unwrap_or_default(),
        Action::Kanban(a) => km.kanban.by_action.get(&a).cloned().unwrap_or_default(),
        Action::Popover(a) => km.popover.by_action.get(&a).cloned().unwrap_or_default(),
        Action::Review(a) => km.review.by_action.get(&a).cloned().unwrap_or_default(),
        Action::Activity(a) => km.activity.by_action.get(&a).cloned().unwrap_or_default(),
        Action::Palette(a) => km.palette.by_action.get(&a).cloned().unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// dump-keybindings

const DUMP_HEADER: &str = "\
# Generated by `shelbi config dump-keybindings`.
# This is the full default keymap. Drop into ~/.shelbi/keys.yml and edit.
# Defaults below ship embedded in the binary — anything you don't override
# stays at its default. Sparse overrides are encouraged: only declare what
# you change.
#
# Per-project overrides go under a `projects:` map, keyed by project name:
#
# projects:
#   shelbi:
#     kanban:
#       move_card_left: alt-h

";

fn dump_keybindings(out: Option<PathBuf>) -> Result<()> {
    let yaml = render_default_keymap();
    match out {
        Some(path) => std::fs::write(&path, &yaml)
            .with_context(|| format!("writing {}", path.display()))?,
        None => print!("{yaml}"),
    }
    Ok(())
}

/// Build the YAML text for the default keymap. Single-chord actions emit
/// as scalars (`quit: ctrl-c`); multi-chord actions emit as inline lists
/// (`quit: [q, ctrl-c]`). All chords are normalized to canonical form so
/// the dump is stable across builds.
fn render_default_keymap() -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(DUMP_HEADER);
    out.push_str("defaults:\n");
    for mode_name in MODE_NAMES {
        out.push_str(&format!("  {mode_name}:\n"));
        for action in Action::all().filter(|a| a.mode() == *mode_name) {
            let chords: Vec<String> = action
                .default_chords()
                .iter()
                .filter_map(|s| KeyChord::parse(s).ok())
                .map(|c| c.canonical())
                .collect();
            match chords.as_slice() {
                [] => {
                    // Author error — every action has at least one default
                    // chord (the foundation test enforces it). Skip rather
                    // than emit malformed YAML.
                }
                [one] => out.push_str(&format!("    {}: {}\n", action.key_name(), one)),
                many => out.push_str(&format!(
                    "    {}: [{}]\n",
                    action.key_name(),
                    many.join(", ")
                )),
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// check

fn check(project: Option<String>) -> Result<()> {
    let project_name = resolve_project(project);
    let (errors, text) = run_check(project_name.as_deref());
    print!("{text}");
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Collect every diagnostic the loader surfaced into a printable block,
/// followed by a one-line `N errors, M warnings` summary. Returns
/// `(error_count, output_text)` so the caller can decide on an exit code
/// and tests can assert on the rendered output without invoking
/// `std::process::exit`.
fn run_check(project_name: Option<&str>) -> (usize, String) {
    let (_keymaps, diags) = load_keymaps(project_name);

    let keys_path = match shelbi_home() {
        Ok(h) => display_path(&h.join(KEYS_FILENAME)),
        Err(_) => KEYS_FILENAME.to_string(),
    };
    let config_path = user_config_path()
        .map(|p| display_path(&p))
        .unwrap_or_else(|_| "config.yaml".to_string());

    let mut out = String::new();
    let mut errors = 0;
    let mut warnings = 0;
    for diag in &diags {
        match diag {
            KeymapDiagnostic::Error {
                message, location, ..
            } => {
                errors += 1;
                out.push_str(&format_diag_line(
                    &keys_path,
                    &config_path,
                    "error",
                    message,
                    location.as_deref(),
                ));
            }
            KeymapDiagnostic::Warning {
                message, location, ..
            } => {
                warnings += 1;
                out.push_str(&format_diag_line(
                    &keys_path,
                    &config_path,
                    "warning",
                    message,
                    location.as_deref(),
                ));
            }
        }
    }
    if !diags.is_empty() {
        out.push('\n');
    }
    let err_word = if errors == 1 { "error" } else { "errors" };
    let warn_word = if warnings == 1 { "warning" } else { "warnings" };
    out.push_str(&format!("{errors} {err_word}, {warnings} {warn_word}\n"));
    (errors, out)
}

/// Render one diagnostic line in `<location>  <level>: <message>` form.
/// Locations of the form `config.yaml` (the legacy zen_toggle warning) are
/// pointed at the user's config file path; everything else is rooted at
/// the keys.yml path with the diagnostic's logical location appended.
fn format_diag_line(
    keys_path: &str,
    config_path: &str,
    level: &str,
    message: &str,
    location: Option<&str>,
) -> String {
    let loc = match location {
        None => keys_path.to_string(),
        Some("config.yaml") => config_path.to_string(),
        Some(l) if l == KEYS_FILENAME => keys_path.to_string(),
        Some(l) => format!("{keys_path}:{l}"),
    };
    format!("{loc}  {level}: {message}\n")
}

/// Tilde-shorten an absolute path for display: `/Users/foo/.shelbi/keys.yml`
/// → `~/.shelbi/keys.yml`. Falls back to the raw display form if no home
/// directory is set or the path doesn't sit under it.
fn display_path(p: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rel) = p.strip_prefix(&home) {
            return format!("~/{}", rel.display());
        }
    }
    p.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Single binary-wide mutex shared across every CLI test that mutates
    // SHELBI_HOME — per-module locks would silently interleave and race on
    // the global env var.
    use crate::commands::test_support::ENV_LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-config-cmd-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- list-actions -----------------------------------------------------

    #[test]
    fn list_actions_includes_every_required_action() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, _) = load_keymaps(None);
        let table = render_actions_table(&km);
        for required in [
            ("global", "quit"),
            ("global", "zen_toggle"),
            ("global", "open_palette"),
            ("sidebar", "nav_up"),
            ("kanban", "move_card_left"),
            ("popover", "close"),
            ("review", "activate"),
            ("activity", "toggle_zen_filter"),
            ("palette", "activate"),
        ] {
            assert!(
                table.lines().any(|l| {
                    let l = l.trim_start();
                    l.starts_with(&format!("{} ", required.0)) && l.contains(required.1)
                }),
                "table missing {required:?}\n---\n{table}"
            );
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_actions_uses_lowercase_hyphenated_chord_syntax() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, _) = load_keymaps(None);
        let table = render_actions_table(&km);
        // global.quit row shows `ctrl-c`.
        let row = table
            .lines()
            .find(|l| l.contains("quit") && l.contains("Quit"))
            .expect("global.quit row");
        assert!(row.contains("ctrl-c"), "expected canonical chord: {row}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_actions_honors_per_project_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(KEYS_FILENAME),
            "projects:\n  shelbi:\n    sidebar:\n      nav_up: w\n",
        )
        .unwrap();

        let (km_proj, _) = load_keymaps(Some("shelbi"));
        let row = render_actions_table(&km_proj)
            .lines()
            .find(|l| l.contains("sidebar") && l.contains("nav_up"))
            .map(str::to_string)
            .expect("sidebar.nav_up row");
        assert!(row.contains(" w"), "project override not applied: {row}");
        assert!(!row.contains(" k"), "default chord should be gone: {row}");

        let (km_other, _) = load_keymaps(Some("other"));
        let other_row = render_actions_table(&km_other)
            .lines()
            .find(|l| l.contains("sidebar") && l.contains("nav_up"))
            .map(str::to_string)
            .expect("sidebar.nav_up row");
        assert!(
            other_row.contains(" k") && other_row.contains("up"),
            "unrelated project should keep defaults: {other_row}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- dump-keybindings -------------------------------------------------

    #[test]
    fn dump_keybindings_contains_header_and_defaults() {
        let yaml = render_default_keymap();
        assert!(yaml.contains("# Generated by `shelbi config dump-keybindings`."));
        assert!(yaml.contains("defaults:"));
        for mode in MODE_NAMES {
            assert!(yaml.contains(&format!("  {mode}:")), "missing mode {mode}");
        }
    }

    #[test]
    fn dump_keybindings_single_chord_is_scalar_multi_is_list() {
        let yaml = render_default_keymap();
        // global.quit defaults to a single chord -> scalar.
        assert!(
            yaml.contains("    quit: ctrl-c\n"),
            "expected scalar form for global.quit:\n{yaml}"
        );
        // sidebar.quit defaults to [q, ctrl-c] -> list.
        assert!(
            yaml.contains("    quit: [q, ctrl-c]\n"),
            "expected list form for sidebar.quit:\n{yaml}"
        );
    }

    #[test]
    fn dump_keybindings_round_trips_to_no_config_baseline() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let (baseline, baseline_diags) = load_keymaps(None);
        assert!(
            baseline_diags.is_empty(),
            "baseline should be clean: {baseline_diags:?}"
        );
        let baseline_table = render_actions_table(&baseline);

        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(KEYS_FILENAME), render_default_keymap()).unwrap();

        let (round_tripped, diags) = load_keymaps(None);
        assert!(
            diags.is_empty(),
            "dumped file should load cleanly: {diags:?}"
        );
        assert_eq!(render_actions_table(&round_tripped), baseline_table);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dump_keybindings_writes_file_when_out_path_given() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        let out = home.join("dump.yml");
        dump_keybindings(Some(out.clone())).unwrap();
        let body = std::fs::read_to_string(&out).unwrap();
        assert!(body.contains("defaults:"));
        assert!(body.contains("    quit: ctrl-c\n"));
    }

    // ---- check ------------------------------------------------------------

    #[test]
    fn check_exits_zero_on_missing_keys_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (errors, text) = run_check(None);
        assert_eq!(errors, 0, "missing file should be clean: {text}");
        assert!(text.contains("0 errors, 0 warnings"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_exits_zero_on_empty_keys_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join(KEYS_FILENAME), "").unwrap();
        let (errors, text) = run_check(None);
        assert_eq!(errors, 0, "empty file should be clean: {text}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_exits_zero_on_valid_keys_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(KEYS_FILENAME),
            "defaults:\n  sidebar:\n    nav_up: w\n",
        )
        .unwrap();
        let (errors, text) = run_check(None);
        assert_eq!(errors, 0, "valid override should be clean: {text}");
        assert!(text.contains("0 errors, 0 warnings"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_errors_on_malformed_yaml() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        // Unterminated mapping → parse error.
        std::fs::write(
            home.join(KEYS_FILENAME),
            "defaults:\n  sidebar:\n    nav_up: [unterminated\n",
        )
        .unwrap();
        let (errors, text) = run_check(None);
        assert!(errors >= 1, "malformed yaml should error: {text}");
        assert!(text.contains("error:"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_errors_on_unknown_chord_syntax() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(KEYS_FILENAME),
            "defaults:\n  global:\n    open_palette: ctrl++p\n",
        )
        .unwrap();
        let (errors, text) = run_check(None);
        assert!(errors >= 1, "unknown chord should error: {text}");
        assert!(text.contains("ctrl++p"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_errors_on_unknown_action_name() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(KEYS_FILENAME),
            "defaults:\n  sidebar:\n    not_a_real_action: w\n",
        )
        .unwrap();
        let (errors, text) = run_check(None);
        assert!(errors >= 1, "unknown action should error: {text}");
        assert!(text.contains("not_a_real_action"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_errors_on_intra_mode_collision() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            home.join(KEYS_FILENAME),
            "defaults:\n  sidebar:\n    nav_up: x\n    nav_down: x\n",
        )
        .unwrap();
        let (errors, text) = run_check(None);
        assert!(errors >= 1, "collision should error: {text}");
        assert!(text.to_lowercase().contains("chord"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_warns_but_exits_zero_on_reserved_chord_rebind() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(&home).unwrap();
        // Rebind global.quit away from ctrl-c — should warn but not error.
        std::fs::write(
            home.join(KEYS_FILENAME),
            "defaults:\n  global:\n    quit: q\n",
        )
        .unwrap();
        let (errors, text) = run_check(None);
        assert_eq!(errors, 0, "reserved-chord rebind should not error: {text}");
        assert!(text.contains("warning:"), "expected warning: {text}");
        assert!(text.contains("ctrl-c"), "warning should mention ctrl-c: {text}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_warns_but_exits_zero_on_legacy_zen_toggle_field() {
        use shelbi_state::{save_user_config, UserConfig, ZenToggleChord};
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut cfg = UserConfig::default();
        cfg.keymap.zen_toggle = ZenToggleChord::CtrlG;
        save_user_config(&cfg).unwrap();

        let (errors, text) = run_check(None);
        assert_eq!(errors, 0, "legacy field should not error: {text}");
        assert!(
            text.contains("warning:") && text.contains("zen_toggle"),
            "expected legacy warning: {text}"
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
