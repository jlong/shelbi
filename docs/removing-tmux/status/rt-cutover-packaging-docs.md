# rt-cutover-packaging-docs — In review

Phase 6 cutover, the packaging / docs / site / CI slice: nothing shipped,
documented, or depicted claims tmux any more, and CI gains the two jobs the
plan calls for. Plan sections: "Phase 6: Cutover" (Packaging, Docs, CI), "What
gets deleted" (last bullet), "Compatibility with old sessions", "Effect on
other plans".

Base contained the umbrella foundation (`crates/shelbi-proto`,
`docs/removing-tmux/README.md`); branch cut clean from `origin/jlong/remove-tmux`.

## What landed

### Packaging
- **deb** (`.goreleaser.yaml`): dropped the `tmux (>= 3.2)` dependency and the
  "built on tmux" description.
- **Homebrew formula** (`scripts/release/update-homebrew-formula.rb`): removed
  `depends_on "tmux"` and the tmux description.
- **Crate manifests**: `shelbi-cli` package description retitled off tmux.
- `scripts/install.sh` already installs no daemon unit (the units were retired
  in Phase 3 / `rt-daemon-lifecycle`); it only restarts a running on-demand
  daemon, so the "installs no daemon unit" criterion was already met and is
  left unchanged.
- Wizard preflight already carries no tmux row (removed in `rt-cutover-delete`
  #1505); the "No tmux" stop is gone. No change needed.

### Repository docs
- `README.md`, `AGENTS.md` (+ its `CLAUDE.md` symlink), and
  `docs/release/homebrew-tap.md` rewritten for the new runtime: Shelbi runs its
  own sessions, `shelbi attach`, needs only `git` + an agent CLI, runs fine
  nested in tmux or Screen. `AGENTS.md` crate layout updated (dropped the
  deleted `shelbi-tmux`, added `shelbi-proto` / `shelbi-session` /
  `shelbi-client` / `shelbi-term` / `shelbi-app`) and the test note fixed.

### Site (`site/`)
- `rg -i tmux site/` now returns only deliberate mentions: "runs fine inside
  tmux or Screen" (install page + the new changelog entry), the dated changelog
  history (kept — see below), and herdr's still-true competitor descriptions
  ("tmux for agents", "tmux-style detach", the keyboard-first tmux/zellij
  paradigm).
- Removed the `merge` CLI reference page (deleted command) and fixed every
  inbound link (docs index card, `zen.mdx`, `configuration/project.mdx`), plus
  the stale `shelbi merge` prose in `first-task.mdx` and `project.mdx`.
- Added CLI reference pages for `session`, `machine`, and `relay`, each verified
  against the built binary's `--help`. (No `spawn`/`archive`/`tail`/`popup`
  pages existed to remove.)
- Rewrote the hero, feature grid (the "Made with tmux" card + its green-status-
  bar vignette became an accurate "Attach from anywhere" / `shelbi attach`
  depiction), docs index, `llms.ts` tagline, concepts (workspaces,
  review-workspaces, orchestrator, agents), getting-started, the remaining CLI
  pages, the comparison pages, and `public/install.sh`.
- `cd site && npm run lint && npm run build` pass.
- App depictions verified against the real TUI/CLI (labels, commands, the
  `Ctrl+]` detach hint, session naming), not invented.

### CI (`.github/workflows/app-ci.yml`, sensitive path — kept minimal)
- **Smoke job** (`smoke`): launches the single-process TUI inside **tmux** and
  inside **GNU Screen** via `scripts/ci/tui-smoke.sh`, reaping everything under
  bounded waits so the job can never hang. Since `rt-tui-headless-startup-block`
  (#1512) draws the first frame immediately, both muxes now **hard-assert**: a
  non-empty rendered frame within ~2s and a clean exit 0 on the quit key. tmux
  reads the frame with `capture-pane`; Screen logs the raw byte stream to a file
  (its `hardcopy` misses the alternate screen the shell enters) and greps for a
  printable glyph once escapes are stripped.
  - **Rework 2 (Screen-only flake):** the Screen render check gave up ~0.1s in
    because the loop ran `alive || break`, and Screen spawns its detached child
    asynchronously, so `alive` is false for the first fraction of a second on a
    healthy launch. Fixed by polling the full window (never breaking early on a
    not-yet-spawned pane; only a recorded exit ends the wait), reading both the
    rc `logfile` and a fallback `screenlog.*`, and dumping diagnostics
    (`screen -ls`, SCREENDIR, captured bytes + stripped head) on failure so any
    residual Linux-Screen difference is self-describing. tmux and Screen both
    green locally.
- **Old-session compatibility harness**
  (`crates/shelbi-cli/tests/old_session_compat.rs`): spawns a real detached
  `shelbi __session` from a chosen release binary and drives the full frozen
  core with the current client — hello, attach-with-replay (via a second
  client), output, input, resize, snapshot, kill, exited. Runs under the
  existing `cargo test` job and **passes against the current build**. Adding a
  release is one line (append a `release(label, path)` entry in `releases()`),
  or at run time via `$SHELBI_COMPAT_BINARIES` (`label=path` pairs, `:`-
  separated) so CI can point at downloaded release binaries without editing the
  file. The list holds only the current build today.

### Carried-over cleanup
- Deleted `spikes/remove-tmux/` (its Phase 0 findings are preserved under
  `docs/removing-tmux/phase0/`) and dropped `spikes` from the workspace
  `Cargo.toml` `exclude` list — the item `rt-cutover-delete`'s destructive-action
  guard had blocked.

## Decisions (calls I made / confirmed with the user)
- **Changelog is history.** The dated entries that describe what shipped on tmux
  (the June 23 2026 launch entry, the tmux popover / window / targets entries)
  are left intact; rewriting them would falsify the record. Added one new top
  entry announcing the cutover. Confirmed with the user.
- **Cursor.** "No mentions of Cursor" is treated as a style rule for new copy
  only; the existing `vs/cursor-background-agents.mdx` page and conductor's
  Cursor mentions are left as-is. Confirmed with the user.
- **Smoke-job scope.** Bounded start/teardown smoke (never hangs CI) rather than
  a render assertion, because the dashboard produces no first frame in a
  headless environment. Confirmed with the user.
- **`shelbi merge` prose.** Fixed the three remaining prose references to the
  removed command (not strictly tmux, but shipping docs that tell users to run a
  deleted command would be a regression).

## Follow-up to file
- **`rt-tui-headless-startup-block`** — landed as #1512 (`ac8133dc`): the TUI now
  draws its first frame before the daemon/dashboard/caps work. The smoke job was
  tightened to hard-assert render + exit against it (Rework 1), and this branch
  is rebased onto it.

## Verification
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test -p shelbi --test old_session_compat`: pass.
- `cd site && npm run lint && npm run build`: pass.
- `Cargo.lock` unchanged (no new/bumped dependency), so no MSRV re-check needed.
- Did **not** run the full `cargo test --workspace` locally (CI is the source of
  truth for the full suite, per the developer instructions); relied on
  build + clippy + the targeted new test.

## Effect on other plans (still-applicable follow-ups; those docs live outside
the repo and were not edited)
- **release-distribution-homebrew-apt.md** — the `Depends:` line and the
  install-script daemon-unit step are now reflected in the shipped files.
- **getting-started-experience-the-60-second-wizard.md** — the tmux preflight
  row and the "No tmux" stop are gone from the wizard.
- **customizable-keybindings.md** — §5 and Phase 2 (the tmux-level palette
  chord, `chord.to_tmux_key()`) are now moot.
- **review-workspaces.md** — §10 (the tmux pane model for a running server)
  needs rewriting in terms of sessions.
- **configurable-review-nav-items.md** — the `popup` display mode and the
  `respawn-pane` path map to overlays and sessions.
- **worker-orchestrator-communication.md** / **workflow-transition-hooks.md** —
  `tmux send-keys` and the "new helper in `shelbi-tmux`" references map to the
  session `paste` path.
