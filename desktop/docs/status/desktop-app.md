# desktop-app (umbrella): foundation

## What landed

- `desktop/` as its own Cargo workspace (`shelbi-desktop`, version 0.10.0,
  `publish = false`), own `Cargo.lock`, Rust pinned to 1.99.0, macOS floor
  15.0 (`.cargo/config.toml`). The root `Cargo.toml` excludes it; the root
  `Cargo.lock` is unchanged.
- gpui channel: `gpui-pre =0.3.9` + `gpui-kit =0.7.3` (see `../gpui.md`).
- `cargo run` opens one window titled "Shelbi", with "Shelbi" in the menu bar
  (via an embedded Info.plist, see `../gpui.md` "Known quirks"): a black
  sidebar with Orchestrator / Issues / Activity / Machines nav rows (Heroicons),
  the `DIVIDER_DIM` rule, and the main area. Cmd+Q quits the client.
- `src/commands.rs`: `dispatch_command` / `dispatch_view` match every
  `CommandKind` and `View` variant with no wildcard. Every arm logs a warning
  and shows "Not implemented yet: <command id>" in a banner at the top of the
  main area (dismiss with the x). The sidebar rows reach `ShowView` for every
  `View`.
- `src/theme.rs`: the TUI's token names with its dark values as gpui colors,
  plus one desktop-only `main_background` (#1c1c1c).
- Icons are Heroicons (v2.2.0), vendored with their MIT license under
  `assets/icons/heroicons/`. Since `dt-icon-sizes` every icon renders at
  24, 20 or 16px from the matching set (24 outline, 20 solid, 16 solid).
- `.github/workflows/desktop-ci.yml` (manual only, `macos-14`) and the
  desktop version assertion in `scripts/release/check-version.sh`.

## What's left

At the foundation, everything the `dt-*` subtasks owned. All of them have
landed; `dt-parity-audit` removed the not-implemented stub described above
and recorded parity with the TUI in `../parity.md`. The status of every
subtask is indexed in `../README.md`; what remains open is the "Known gaps"
list in `../parity.md`.

## Decisions

- **Rust 1.99.0**, the newest stable at the time, rather than the CLI's 1.88:
  weekly gpui snapshots track recent Zed, which tracks recent Rust.
- **`gpui-pre` renamed to `gpui` in `Cargo.toml`**, so source reads `gpui::`
  and a switch to a Zed git pin is a manifest-only change.
- **Window not wrapped in gpui-kit's `Root` yet**; `gpui_kit::init` runs so
  the component layer is ready. The first subtask using a gpui-kit dialog or
  notification adds `Root`.
- **Transparent title bar**, with the sidebar running under the traffic
  lights (the usual macOS sidebar shape). `dt-shell-sidebar` can revisit.
- **Banner ids** follow the registry's palette ids (`view:tasks`,
  `workspace:<name>`, `action:toggle-zen`, ...). The issue mutations have no
  registry id, so they get desktop-local `issue:<verb>:<id>` labels.
- **`check-version.sh` resolves `desktop/Cargo.toml` from the working
  directory**, like its root check, because `release-apt.yml` runs main's
  copy of the script inside the tag's checkout; a tag without `desktop/`
  skips the desktop check.

## Verification

Build time and binary size (clean, Apple M4, details in `../gpui.md`):

| Profile | Clean build | Binary |
|---|---|---|
| debug | 3m 14s | 85 MB |
| release | 6m 17s | 12 MB |

Checks, all run locally on an arm64 Mac:

- `desktop/`: `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` (7 tests: the exhaustive dispatch over every command and
  view, theme values, Heroicon loading).
- Root: `cargo check --workspace --locked`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`, root `Cargo.lock` unchanged, `no_ui_deps` passes.
- `scripts/release/check-version.sh`: passes with `RELEASE_TAG=v0.10.0`;
  fails with a mismatched tag, and fails when `desktop/Cargo.toml` says
  0.10.9.
- By hand: `cargo run` opens the "Shelbi" window (title and menu bar),
  sidebar/main split in the dark tokens; clicking a nav row selects it,
  shows "Not implemented yet: view:orch" (etc.), and logs a `WARN` line.
