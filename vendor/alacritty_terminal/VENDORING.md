# Vendored: alacritty_terminal

Client-side terminal emulator for Shelbi sessions, chosen in the
`rt-spike-emulator-replay` Phase 0 spike. See
`docs/removing-tmux/phase0/emulator-replay.md` for the decision and rationale.

## Provenance

- **Crate:** `alacritty_terminal`
- **Version:** 0.26.0 (from crates.io)
- **Upstream VCS:** https://github.com/alacritty/alacritty, commit
  `94e7c8874e526b1e67b349d9ba30ddf81669119e`, `path_in_vcs: alacritty_terminal`
- **License:** Apache-2.0 (`LICENSE-APACHE`, retained)

## Why vendored (not a crates.io dependency)

Attach replay reconstructs a session's **full** emulator state in a freshly
connected client: both screen buffers, both saved cursors, the scroll region,
tab stops, charsets, every mode (including the kitty keyboard-protocol stack),
and history. `alacritty_terminal` models all of this, but upstream keeps most
of it in **private fields with no accessor** (`Term::inactive_grid`,
`scroll_region`, `tabs`, `active_charset`, `keyboard_mode_stack`,
`inactive_keyboard_mode_stack`). A registry dependency therefore cannot serialize
the full state. Vendoring lets us widen exactly what replay needs. It also pins
the crate: `alacritty_terminal` is pre-1.0 and ships breaking releases, so the
fork turns an external churn risk into a deliberate, reviewed upgrade. (Zed
vendors a fork for the same reason.)

## Divergences from upstream 0.26.0

Kept small. Items 1 to 4 are read-only (accessors, a removed test target).
Item 5 is a build-surface change: it removes modules from the default build
but changes no code inside them. Re-apply all of these when bumping the
vendored version.

1. **`src/term/mod.rs` — read-only state accessors** (search `SHELBI FORK`):
   `Term::inactive_grid`, `scroll_region`, `tabs`, `active_charset`,
   `keyboard_mode_stack`, `inactive_keyboard_mode_stack`. Additive, no behavior
   change.
2. **`src/term/mod.rs` — `TabStops` made `pub`** so `Term::tabs()` can expose
   it. Its fields stay private; callers read stops via its `Index<Column>`
   impl.
3. **`src/selection.rs` — `Selection::anchor()`** (search `SHELBI FORK`): a
   read-only accessor for the point a selection started at. `shelbi-term`
   keeps the user's selection in `Term::selection` (so it rotates with
   scrolling and clears when the text under it is erased) and needs the
   rotated anchor to re-anchor a drag that crosses back over its start.
   Additive, no behavior change.
4. **`Cargo.toml` — removed the `[[test]] name = "ref"` target**, and the
   `tests/` directory is not vendored. Upstream's reference suite is backed by a
   large recording tree (`tests/ref/*.recording`) that Shelbi does not run. The
   library builds unaffected. `src/grid/tests.rs` (a `#[cfg(test)]` unit module)
   is retained.
5. **`tty` feature (off by default) gating the PTY host.** Approved by the
   owner on 2026-11-09.
   - **What it does.** `src/lib.rs` puts `pub mod event_loop`, `pub mod sync`
     and `pub mod tty` behind `#[cfg(feature = "tty")]` (search
     `SHELBI FORK`). `Cargo.toml` adds the `tty` feature and marks
     `optional = true` on the crates only those modules use: `home`, `libc`,
     `parking_lot`, `polling`, and on unix `rustix`, `rustix-openpty`,
     `signal-hook` (0.4), and on Windows `miow`, `piper`, `windows-sys`. The
     module sources are untouched.
   - **Why.** Upstream compiles its own PTY host (spawning a shell, the
     `EventLoop` reader thread, `FairMutex`) unconditionally. Shelbi owns PTYs
     through `portable-pty` and drives `Term` headless, so none of that code is
     reachable from Shelbi, yet it put `polling`, `rustix-openpty` and a second
     `signal-hook` (0.4, next to 0.3 from crossterm/termwiz) into every
     production build. With the feature off, those leave both lockfiles' trees
     for `alacritty_terminal` (the desktop app still gets `polling` from gpui).
     `pub mod thread` stays ungated: it is std-only and costs nothing.
   - **Who may enable it.** Nothing in `crates/` or `desktop/` enables it, and
     nothing references `alacritty_terminal::{tty, event_loop, sync}`. Enable
     it only if Shelbi ever wants upstream's PTY host back.
   - **Dropping it on the next re-vendor.** Either re-apply it (copy the
     `tty = [...]` feature block and the `optional = true` lines into the new
     `Cargo.toml`, check upstream did not add a new dependency used only by
     these modules, and re-add the three `#[cfg(feature = "tty")]` lines in
     `src/lib.rs`), or drop it by taking upstream's `Cargo.toml` and
     `src/lib.rs` as they are. Dropping it brings the crates above back into
     the lockfiles but changes no behavior. To verify a re-application, build
     the crate standalone both ways (`cargo test` and
     `cargo test --features tty`) and confirm `cargo tree -i polling` in the
     root workspace comes up empty. Upstream's `tty/unix.rs` trips three
     `clippy::question_mark` lints on current clippy when the feature is on;
     that is upstream code, not part of this divergence.

The **write** side — installing serialized state into a fresh `Term` (setters
or a `from_state` constructor) — is deliberately NOT added here. That is the
Phase 1 serializer/deserializer (`rt-term`), the largest single piece of that
phase.
