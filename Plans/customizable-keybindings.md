# Customizable Keybindings

## Context

Shelbi's TUI today hardcodes every keystroke. Five event loops in `shelbi-tui/src/lib.rs` (sidebar, kanban, review, activity, popover) plus the palette `picker_loop` in `shelbi-cli/src/commands/palette.rs` each contain a flat `match KeyCode { ... }` body. The only user-configurable chord is `keymap.zen_toggle` in `~/.shelbi/config.yaml`, and even that is a single enum of four pre-approved options — added because some terminals swallow Alt+Z and we needed an escape hatch.

The pain:

- **Conflicts with system-level chords.** A user whose terminal eats `Ctrl+P` or whose tmux config rebinds `Ctrl+Space` has no way to relocate Shelbi's bindings.
- **Per-mode overlap surprises.** `K`/`J` are scroll-body in review but reorder-card in kanban. A user moving between modes can't choose a consistent vocabulary.
- **Adding a new action means picking a key that no current handler uses anywhere.** That ratchet is tight enough that we've started avoiding adding bindings.
- **Per-project conflicts.** A project that uses `Ctrl+P` heavily can't tell Shelbi to use something else just for that workspace.

What good looks like: Shelbi behaves like a serious modal editor — every binding is a key in a YAML map, defaults ship with the binary, the user writes overrides for what they want different, and `shelbi config keybindings` shows them exactly what is bound where.

There is already a clean seam. `UserConfig` in `shelbi-state/src/user_config.rs` loads `~/.shelbi/config.yaml` via `serde_yaml`. The same loader can pick up a new sibling file `~/.shelbi/keys.yml` (separate from `config.yaml` so general settings and keybindings stay independently editable). The first-run probe pattern from `zen_probe::ensure_zen_keymap` is the right precedent for terminal-compat fallback.

## Design

### 1. Action vocabulary

Every keystroke handler dispatches to a named **action**. The action set is finite, declared once in `shelbi-tui/src/keymap/actions.rs`, and grouped by mode:

```rust
pub enum Action {
    Global(GlobalAction),
    Sidebar(SidebarAction),
    Kanban(KanbanAction),
    Popover(PopoverAction),
    Review(ReviewAction),
    Activity(ActivityAction),
    Palette(PaletteAction),
}

pub enum GlobalAction { Quit, ZenToggle, OpenPalette }
pub enum KanbanAction { NavLeft, NavRight, NavUp, NavDown,
                       MoveCardLeft, MoveCardRight, ReorderUp, ReorderDown,
                       OpenPopover, Refresh }
// ...etc
```

Naming convention: `snake_case`, mode-implied (a `ReviewAction::NavUp` and a `KanbanAction::NavUp` are distinct symbols; the mode is part of the action's full identity).

A given action carries a `description` and a `default_chord` (or a small list of default chords — `j` AND `Down` both fire `NavDown` by default). Both are `const` data on the enum, surfaced by `shelbi config list-actions`.

### 2. Config shape

Keybindings live in their own file, `~/.shelbi/keys.yml`, separate from `~/.shelbi/config.yaml`. Reasons to split:

- Keybindings are the kind of thing users tinker with, copy from gists, sync to dotfiles. A dedicated file is easier to share without exposing the rest of their general config.
- The existing `config.yaml` schema stays small and stable.
- A missing `keys.yml` is the common case (everyone starts on defaults); having it be its own file means most users never have to touch it.

Schema:

```yaml
defaults:
  global:
    quit: ctrl-c
    zen_toggle: alt-z          # existing field migrates here from config.yaml
    open_palette: ctrl-space   # was tmux-level Ctrl+P; see §5

  sidebar:
    quit: [q, ctrl-c]          # list = multiple chords for the same action
    nav_up: [k, up]
    nav_down: [j, down]
    activate: enter
    refresh: r

  kanban:
    nav_left: [h, left]
    nav_right: [l, right]
    nav_up: [k, up]
    nav_down: [j, down]
    move_card_left: H
    move_card_right: L
    reorder_up: [K, shift-up]
    reorder_down: [J, shift-down]
    open_popover: [enter, space]
    refresh: r

  popover: { ... }
  review: { ... }
  activity: { ... }
  palette: { ... }

projects:
  shelbi:
    kanban:
      move_card_left: alt-h     # overrides defaults.kanban.move_card_left for this project only
      move_card_right: alt-l
  shelbi-website:
    global:
      open_palette: ctrl-shift-space   # this project's `Ctrl+P` is taken by something else
```

**Overlay, not replacement.** Anything the user doesn't write keeps the default. The block above is *not* what the user needs to write — it's what the merged in-memory state can look like. A real user file might be:

```yaml
defaults:
  global:
    open_palette: ctrl-shift-space
  kanban:
    move_card_left: alt-h
    move_card_right: alt-l

projects:
  side-project:
    global:
      open_palette: f12
```

…and everything else stays at the shipped default. Defaults are embedded in the binary; missing `keys.yml` = no error.

**Merge order (lowest → highest priority):**

1. Embedded built-in defaults.
2. `keys.yml::defaults` (the user's global preferences).
3. `keys.yml::projects.<current-project>` (per-project override).

Each layer is sparse and overrides only the keys it sets. A project block can also explicitly set an action to `null` to fall back to the layer below (rarely useful, but it's the natural inverse of "list = multiple chords").

**Why nested by mode rather than flat list of `{mode, chord, action}` records:**

- Reads like a settings file, not a database dump.
- The vast majority of bindings are one-chord-per-action; the YAML aliasing handles the rare "multiple chords for one action" case via a list value.
- Easy to spot what's missing — a mode that only lists half its actions is visibly incomplete.

### 3. Chord syntax

Match the existing `keymap.zen_toggle` convention — lowercase, hyphen-separated. Parser lives in `shelbi-state/src/keymap/chord.rs` and produces a `(KeyCode, KeyModifiers)` tuple.

Grammar:

```
chord  := (modifier '-')* keyname
modifier := ctrl | alt | shift | super
keyname  := single character | named-key

named-key := up | down | left | right
           | enter | space | esc | tab | back-tab
           | backspace | delete | insert
           | home | end | page-up | page-down
           | f1 .. f12
```

Cases:

- `j` → `KeyCode::Char('j')`, no modifiers.
- `J` is equivalent to `shift-j` — uppercase letters auto-imply Shift. Both spellings parse to the same chord.
- `ctrl-c`, `alt-z`, `ctrl-shift-space` — modifiers in any order, joined by hyphens.

**Single chord only — no sequences.** `gg`/`dap`/`<leader>x` style multi-key sequences are explicitly out of scope. They would require a held-buffer + timeout dispatcher layer that's a different subsystem entirely, and Shelbi's navigation surface is shallow enough that one-chord-per-action covers everything. The parser rejects any chord with more than one base keyname.

### 4. The dispatcher

Replace each `handle_*_key` function's inline match with a lookup against a per-mode `Keymap` table built once at startup:

```rust
pub struct ModeKeymap {
    bindings: HashMap<KeyChord, Action>,
}

impl ModeKeymap {
    pub fn dispatch(&self, key: KeyEvent) -> Option<Action> {
        self.bindings.get(&KeyChord::from(key)).copied()
    }
}
```

The handler shrinks to:

```rust
fn handle_kanban_key(app: &mut App, key: KeyEvent, km: &Keymaps) -> Outcome {
    match km.kanban.dispatch(key) {
        Some(Action::Kanban(KanbanAction::NavLeft))  => app.kanban_nav_left(),
        Some(Action::Kanban(KanbanAction::NavRight)) => app.kanban_nav_right(),
        // ...
        None => Outcome::Unhandled,
    }
}
```

The `match Action { ... }` arm bodies are the semantic actions — those stay hardcoded. Only the chord-to-action lookup is data-driven.

The `Keymaps` struct is built once per session by merging built-ins → `defaults` → `projects.<current>`. The current project name comes from the same place the orchestrator already reads it (project YAML).

**Global chords** (`Quit`, `ZenToggle`) are checked **before** the mode-specific table in every loop, matching today's behavior where `Ctrl+C` and the Zen-toggle chord can fire from any view. This is also why `ctrl-c` is reserved (§8).

**Mode stacking.** When the popover is open, the popover keymap is checked first; only if it returns `Unhandled` does the kanban keymap get a look. Same priority order the current code uses via the `app.popover_is_open()` guard, just expressed declaratively.

### 5. The palette and tmux-level bindings

The palette is a separate process — `shelbi popup` — invoked via a tmux binding installed by `shelbi-orchestrator` (`bind-key -n C-p if-shell ... run-shell "shelbi popup"`). Two consequences:

1. The popup process loads the same `~/.shelbi/keys.yml`, so its picker keybindings (close, navigate, select) come from the same config. No new plumbing needed.

2. The chord that **opens** the palette is bound at the tmux layer, not in Shelbi. Changing it means rewriting the orchestrator's tmux bind on next `shelbi reload`. The orchestrator already runs `tmux bind-key` during session bootstrap — it just needs to read the configured `global.open_palette` chord (after project merge) and use that instead of the hardcoded `C-p`.

   Edge case: if the user's chord can't be expressed as a tmux key string, fall back to the default and warn at startup. Tmux key syntax is liberal (`M-z` for Alt+Z, `C-Space`, etc.) — the chord parser gains a `to_tmux_key()` method that returns `Option<String>`.

   Since the palette chord can also be overridden per-project, the tmux binding gets re-applied whenever the user switches between Shelbi projects.

### 6. Help text generation

The sidebar today renders a hardcoded help line `^P palette  q quit`. Replace with a function that generates the help string from the active keymap *and* the host platform:

```rust
fn help_string(km: &Keymaps, style: DisplayStyle) -> String {
    format!(
        "{} palette  {} quit",
        format_chord(km.global.first_chord_for(GlobalAction::OpenPalette), style),
        format_chord(km.sidebar.first_chord_for(SidebarAction::Quit), style),
    )
}
```

**Display style is platform-detected at startup**, no user setting:

- macOS: `⌘ ⌥ ⌃ ⇧` (e.g. `⌥Z`, `⇧↑`, `⌃C`).
- Linux / other: `Ctrl+`, `Alt+`, `Shift+` (e.g. `Alt+Z`, `Shift+Up`, `Ctrl+C`).

Detection at startup via `cfg!(target_os = "macos")`. If a future user pushes back on this, adding `display_style: mac | linux` to `keys.yml` is a one-line follow-up — but ship with the auto-detect default.

Single-character bindings are rendered as-is in either style (`q` is `q` everywhere). Named keys (`Up`, `Enter`) follow the modifier style — Mac shows `↑` `⏎`, Linux shows `Up` `Enter`.

This is non-trivial work because today some of those help strings are baked into the static layout. Worth a dedicated subtask within Phase 2.

### 7. Discoverability commands

Three new CLI commands under `shelbi config`:

- **`shelbi config list-actions`** — prints every action with mode, name, description, and current binding(s), formatted for the host platform. One per line, columnar:

  ```
  global    quit               Quit Shelbi              ⌃C
  global    zen_toggle         Toggle Zen Mode          ⌥Z
  global    open_palette       Open command palette     ⌃Space
  sidebar   nav_up             Move selection up        k, ↑
  sidebar   nav_down           Move selection down      j, ↓
  ...
  ```

- **`shelbi config dump-keybindings`** — writes the full default keymap as YAML to stdout (or `--out PATH`). The user pipes it into their `keys.yml` as a starting point for customization.

- **`shelbi config check`** — validates `~/.shelbi/keys.yml`: parse errors, unknown action names, unknown chord syntax, and intra-mode chord collisions. Exits non-zero only on **errors**, never on warnings, so it's safe to drop into a Git pre-commit hook for users who keep their dotfiles in version control. Warnings (reserved-chord rebind attempts, deprecated legacy `zen_toggle` field still in `config.yaml`) print to stderr but exit 0.

### 8. Conflict and error handling

Three classes of feedback at config-load time:

1. **Parse error** (unknown chord syntax, unknown action name, malformed YAML). Emit a clear stderr message with line number, fall back to the default for that field. Don't crash. **Counts as an error** for `shelbi config check` exit code.

2. **Intra-mode chord collision** (two actions in the same mode bound to the same chord, post-merge). Warn loudly, keep the default for both, prompt the user to fix. **Counts as an error.**

3. **Reserved-chord rebind attempt.** Two chords are reserved:

   - `ctrl-c` always quits. The kill-switch must work.
   - The configured `global.open_palette` must always parse to a tmux-expressible key (or the tmux binding gets the default).

   Reserved-chord violations are **warnings, not errors** — the user can override but the system reminds them on every startup until they remove it. `shelbi config check` still exits 0.

### 9. Backwards compatibility

Today's `keymap.zen_toggle: alt-z` field in `~/.shelbi/config.yaml` stays valid. At load time:

1. Load `~/.shelbi/keys.yml` if present.
2. If `keys.yml::defaults.global.zen_toggle` is unset *and* `config.yaml::keymap.zen_toggle` is set, copy the legacy value into the in-memory keymap.
3. Emit a deprecation warning to stderr the first time per session: *"keymap.zen_toggle in config.yaml is deprecated; move to keys.yml::defaults.global.zen_toggle. The legacy field will be removed in a future release."*

`shelbi config check` flags the legacy field with a migration suggestion. No breaking changes to existing configs.

### 10. First-run probe (scoped)

The existing `zen_probe::ensure_zen_keymap` runs once on first launch to detect whether Alt+Z reaches the program (some terminals eat it). The same probe shape would be useful for every new modifier-bearing default — but running probes for `Ctrl+Space`, `Shift+Up`, etc. on first launch would make startup a five-question wizard.

Decision: keep the probe scoped to `zen_toggle` only for now. If a user's `Ctrl+Space` doesn't reach the program, they discover that on first use (the chord doesn't open the palette) and either rebind in `keys.yml` or fall back to the CLI. We can add per-action probes later if real users hit this.

## Rollout

Two phases, each independently shippable. Phase 1 makes everything in-process configurable; Phase 2 closes the loop on the tmux-level chord and the static help strings.

**Phase 1 — In-process configurability.**

- Add `shelbi-tui/src/keymap/{actions.rs, chord.rs, table.rs}` modules.
- Define the `Action` enum and the per-mode action enums with `default_chord` + `description` const data.
- Implement the chord parser (lowercase hyphenated grammar, modifier normalization, named-key vocabulary, single-chord enforcement).
- Refactor the five existing event loops to dispatch via per-mode `Keymap` tables built at startup. Hardcoded behavior remains identical when no `keys.yml` is present (defaults match today's bindings 1:1).
- Add the `~/.shelbi/keys.yml` loader in `shelbi-state`. Implement the three-layer merge: built-ins → `defaults` → `projects.<current>`. Project name comes from the active session's project YAML.
- Forward-compat shim: copy legacy `config.yaml::keymap.zen_toggle` into `global.zen_toggle` if the new field is unset. Print deprecation warning.
- Add `shelbi config list-actions`, `dump-keybindings`, and `check` CLI commands.
- Conflict / parse-error handling per §8. All errors go to stderr at startup; bad config never blocks launch.
- Document the new `keys.yml` schema in a comment header that `shelbi config dump-keybindings` prints at the top of its output.

After Phase 1: users can rebind every in-process chord, globally or per-project. The palette opener (`Ctrl+P` → tmux-bound) stays hardcoded; static help strings (`^P palette  q quit`) still display the defaults in ASCII.

**Phase 2 — Tmux integration + platform-aware help text.**

- Teach `shelbi-orchestrator`'s tmux-bootstrap to read the merged `global.open_palette` chord (defaults + project) and bind that instead of hardcoded `C-p`. Re-apply on project switch.
- Add `chord.to_tmux_key()` with fallback-on-unrepresentable behavior.
- Replace every hardcoded help string in the TUI with a `format_chord_for(action, platform_style)` lookup driven by the active keymap.
- Implement platform-aware display (`⌥Z` on Mac, `Alt+Z` on Linux) via `cfg!(target_os = "macos")` detection at startup.
- Document the `shelbi reload` requirement when changing the palette chord.

After Phase 2: every visible keystroke in Shelbi reflects the user's config, rendered in the convention native to their OS.

## Decisions

- **Config file: `~/.shelbi/keys.yml`** (separate from `config.yaml`). Schema is two top-level keys: `defaults` (user's preferred bindings) and `projects` (map keyed by project name for per-project overrides). One file owns all keybinding config, hierarchically.
- **Config shape: nested by mode**, with `global` for chords that fire in any view. Flat list-of-records was considered and rejected as harder to read.
- **Three-layer merge** (lowest → highest precedence): embedded built-in defaults → `keys.yml::defaults` → `keys.yml::projects.<current>`. Each layer is sparse; only declared keys override.
- **Chord syntax: lowercase, hyphen-separated**, modifiers any order. Matches existing `zen_toggle` convention. `J` and `shift-j` both parse to the same chord.
- **Single chord only — multi-key sequences (Vim `gg`, `dap`, `<leader>x`) are out of scope.** Not deferred, not planned. Shelbi's navigation surface is shallow enough that one-chord-per-action covers everything; the held-buffer + timeout dispatcher is complexity we don't need.
- **Overlay semantics, not replacement.** User config only needs to declare what changes; defaults fill in the rest. Embedded defaults; missing `keys.yml` = no error.
- **Per-action multiple chords** expressed as YAML list. `nav_down: [j, down]` means both fire the same action.
- **Reserved chords: `ctrl-c` for quit, and the configured palette opener** must parse to a tmux-expressible key. Other chord choices are unrestricted — the user can break their own setup; `shelbi config dump-keybindings` and `shelbi config check` exist to recover.
- **Conflicts (two actions, same chord, same mode, post-merge) warn at startup and keep the default for both.** Don't crash, don't silently pick one. Counts as an error for `shelbi config check`.
- **`shelbi config check` exit code: errors only.** Parse failures, unknown actions, and chord collisions exit non-zero. Reserved-chord warnings and deprecated-field warnings print to stderr but exit 0. Safe to drop into a Git pre-commit hook.
- **Display style: platform-detected**, not user-configurable. Mac → `⌘ ⌥ ⌃ ⇧`. Linux/other → `Ctrl+ Alt+ Shift+`. Adding `display_style:` to `keys.yml` is a one-line follow-up if anyone asks; ship with auto-detect.
- **Backwards compat for legacy `config.yaml::keymap.zen_toggle`**: copied forward into `keys.yml::defaults.global.zen_toggle` at load time if the new field is unset. Deprecation warning on stderr; `shelbi config check` flags it.
- **Palette opener configurability deferred to Phase 2** because it crosses the tmux boundary. Phase 1 ships in-process rebinding only.
- **First-run probe stays scoped to `zen_toggle`.** Don't run a five-question wizard for every modifier-bearing default. Users discover terminal-eaten chords on first use and rebind in `keys.yml`.
- **Help text generation deferred to Phase 2.** Phase 1 leaves the static `^P palette  q quit` line in place; Phase 2 replaces it with a config-driven `format_chord` lookup.

## Open questions

_None remaining — plan is ready to execute. Implementation-level details (the exact wording of the deprecation warning, the precise columnar widths of `shelbi config list-actions`, etc.) will surface during Phase 1 but don't need pre-commitment._