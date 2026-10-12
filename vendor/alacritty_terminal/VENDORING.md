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

Kept intentionally minimal and read-only. Re-apply these when bumping the
vendored version.

1. **`src/term/mod.rs` — read-only state accessors** (search `SHELBI FORK`):
   `Term::inactive_grid`, `scroll_region`, `tabs`, `active_charset`,
   `keyboard_mode_stack`, `inactive_keyboard_mode_stack`. Additive, no behavior
   change.
2. **`src/term/mod.rs` — `TabStops` made `pub`** so `Term::tabs()` can expose
   it. Its fields stay private; callers read stops via its `Index<Column>`
   impl.
3. **`Cargo.toml` — removed the `[[test]] name = "ref"` target**, and the
   `tests/` directory is not vendored. Upstream's reference suite is backed by a
   large recording tree (`tests/ref/*.recording`) that Shelbi does not run. The
   library builds unaffected. `src/grid/tests.rs` (a `#[cfg(test)]` unit module)
   is retained.

The **write** side — installing serialized state into a fresh `Term` (setters
or a `from_state` constructor) — is deliberately NOT added here. That is the
Phase 1 serializer/deserializer (`rt-term`), the largest single piece of that
phase.
