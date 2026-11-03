# rw-remote-config-writes

Phase P8 of the remote-window plan (ContextStore `Shelbi`,
`Plans/remote-session-window.md`, sections 8.3, 8.4 and 11): the remote
window's Settings saves, machine checks, Edit and image drops do the right
thing for a project whose files live on another machine. Still behind the
P6 dev setting (`desktop.native_remote` / `SHELBI_NATIVE_REMOTE=1`).

## What landed

- **Config saves are a plan the daemon writes** (`SaveConfig`, control
  protocol v8). `WorkflowConfig::save_plan` / `AgentsConfig::save_plan`
  (shelbi-state `config_write`) turn a save into data: a content hash per
  file or directory it touches (as loaded), then the ordered writes. The
  client builds the text with its own comment-preserving patcher; the daemon
  (`config_write::apply`) refuses a path outside the surface's config,
  re-parses every YAML it writes with its own loader, refuses the whole plan
  when a hash doesn't match (`MutationError::ConfigChanged`), and writes each
  file atomically. Settings saves go through `shelbi_app::settings::store`:
  a window's `ProjectHandle` sends the plan to the project's daemon (this
  machine's for a local window, the far one over the relay for a remote
  window) and reads the model back through the window's source. The TUI's
  `store::Local` does the same against this machine's daemon. With no local
  daemon running the plan is written directly.
- **"Changed on devbox, reload."** A far edit since the window read the
  file refuses the save: "Changed on devbox since it was loaded: ...
  Discard to reload them." The pages reload the far files on their own.
- **Workspace and machine changes, machine checks** run on the far hub
  (`ClientMsg::AppRequest` / `ServerMsg::Reply`, bodies in `shelbi_app::far`;
  the far side is `shelbi_orchestrator::far`). The workspace dialog gets
  `far::pool_service`, the machine form `Engine::far` (`far::machine_service`
  and `far::readiness_service`). A check the user stops answers "stopped" at
  once and is never recorded; a ready one is recorded in the far hub's state.
  `machine_reachability` was already answered far side by the `query`
  service (P4).
- **Edit** in a remote window (the `edit:*` commands, Settings' Edit YAML,
  a path Cmd+clicked in a far terminal) asks the far daemon for an editor
  session (`<project>/edit/<key>`, the far user's login shell running the
  hub config's `editor:` command, else `$EDITOR`, else vim) and shows it in a
  window of its own (`views::far_editor`), "Editing task.yaml on devbox".
  Quitting the editor closes the window. Far paths are never opened as local
  files.
- **Drops** onto a far terminal are uploaded with the relay's new `put`
  service into a fresh private directory under the far user's temp directory
  (`$XDG_RUNTIME_DIR` or `$TMPDIR`, else `~/.shelbi/tmp/drops`), and the far
  paths are pasted. A folder, or a file over 64 MB, is refused with a note
  over the pane.
- **Add Project** stays unavailable from a remote window.

## Tests

- `app::remote_tests` (desktop): a workflow save writes the far file with its
  comment kept, a far edit is refused naming the host (page and editor), Edit
  opens a far editor session, a drop lands on the far machine, and the machine
  form checks and saves on the far hub.
- `shelbi-orchestrator/tests/remote_sessions.rs`: the window attaches by name
  to the editor session the far side started.
- `shelbi-client/tests/relay_services_e2e.rs`: `put` round trip, settings
  writes for another project refused by the relay.
- shelbi-state `config_write` and `workflow_config` tests, the daemon's
  `control` tests, `shelbi_app::far` and `shelbi_orchestrator::far`.
