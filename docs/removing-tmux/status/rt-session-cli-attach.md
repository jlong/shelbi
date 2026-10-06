# rt-session-cli-attach — In progress

The debug CLI over the session backend plus a rendered single-session attach
client. Plan sections "Phase 1" and "`shelbi attach <workspace>`".

## What lands

- **Debug surface** (`shelbi-cli`, `commands/session_cli.rs`): `shelbi session
  ls | new | kill | send | snapshot`, driving `shelbi-client` /
  `shelbi-session` directly (no tmux). `ls` shows name, short-id, state
  (live/dead, with exit code/signal), size (queried over the socket for live
  sessions), task, and local launch time. `send` delivers via `paste` with an
  optional trailing Enter. A selector resolves a short-id, full name, workspace
  (last name segment), or unambiguous prefix/substring, breaking ties toward
  `--project`.
- **Rendered attach** (`commands/session_attach.rs`): `shelbi session attach
  <session|workspace>`, a full-screen client that drives a `shelbi-term`
  client emulator from the session's replay + live output and paints its grid
  into the terminal. It is **not** raw passthrough: it reports its size so the
  session reflows to fill the terminal when this client is active, and uses
  `viewport::fit` to clip/letterbox the session grid when another client is
  active at a different size. Keys/mouse/paste/focus route through
  `shelbi-term::input` (mouse-ownership policy honored; wheel scrolls Shelbi's
  scrollback, program mouse is forwarded). Detach is Ctrl+] (configurable via
  `--detach-key`), shown in a one-line hint for a few seconds. The terminal is
  restored on detach, child exit, and panic (an RAII guard plus a panic hook).
- **CLI wiring** (`main.rs`): the hidden `__session` process entry moved off the
  `Session` enum variant (now the user-facing `shelbi session` group); it stays
  hidden and parseable.

## Command naming

Shipped as `shelbi session attach <session|workspace>`. The legacy `shelbi
attach` (tmux) is untouched and keeps working until `rt-cutover-delete` renames
this command to `shelbi attach`.

## Config-upgrade

No shipped `*.template` / default config, workflow, or instructions file
changed (new in-code CLI subcommands + a crate dependency), so no
config-upgrade sniffer is needed.

## Tests

- Unit: selector resolution (id/name/segment/prefix/substring, ambiguity,
  project tiebreak), `ls` table formatting and time rendering, detach-key
  parsing/matching/description, cell color+flag → ratatui style mapping, and the
  acceptance case — the same session grid rendered into two viewers of
  different width (one letterboxed, one clipped).
- Integration (`tests/session_cli.rs`): `new | ls | snapshot | send | kill`
  against a real detached `shelbi __session` (a PTY running `cat`), each under
  an isolated `SHELBI_HOME`.
- `attach`'s render/input/viewport are covered by the pure helpers above (a live
  attach needs a TTY, out of scope for CI).
