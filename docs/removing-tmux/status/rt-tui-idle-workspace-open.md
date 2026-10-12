# rt-tui-idle-workspace-open — idle-workspace placeholder in the shell

**Status:** implemented, behind the `session_backend` dev flag. tmux runtime
unchanged.

On main every workspace has a tmux pane even when idle, so opening it always
showed something. In the single-process shell an idle workspace has no
`<project>/ws/<name>` session, so the attach found nothing and the main area was
left as it was — and while a review was open, the review interface (which draws
over the main area regardless of `main_view`) kept showing, so opening an idle
workspace looked like a no-op. Now opening an idle workspace replaces the main
area with a placeholder for that workspace; the sidebar selection and the main
view always agree.

Behavior:
- **Idle** (a declared workspace with no session at all): the main area shows a
  placeholder with the workspace's name, machine and branch, an "Idle — no
  running session" line, and how to start work (dispatch a ready task from the
  Issues board or the command palette).
- **Exited** (a session that died and left `final.txt`): shows its last output
  line, exactly as the orchestrator view already did (the `Failed` placeholder).
- **Auto-attach**: once a session for the shown workspace starts, a workspace
  change notification naming it retries the attach, so it attaches without the
  user re-selecting the row. Scoped to the shown workspace so unrelated board
  churn never flickers the placeholder.
- Navigating to any session or native view now drops an open review interface,
  so the new main view is actually visible (it used to render on top).

Surface:
- `shelbi-orchestrator::workspace`: `resolve_idle_workspace` +
  `IdleWorkspaceIdentity` (machine + local worktree branch via
  `worktree_current_branch`; branch resolved for local machines only — a remote
  branch would need an SSH round-trip we don't block an idle-open on).
- shell `shell/session.rs`: `Connector::connect` now returns
  `Result<Connected, ConnectFailure>` (`Idle(IdleInfo)` / `Message(String)`);
  new `Slot::Idle` / `MainState::Idle`; `SessionManager::retry_if_stale`.
- shell `shell/mod.rs`: `render_idle_workspace` + pure `idle_placeholder_lines`;
  `ShellState::retry_shown_workspace`; `show` drops the review interface when
  switching to a session / native view.
- shell `shell/changes.rs`: the change subscriber forwards a `ChangeWake`
  (`Board` / `Workspace(name)`) so the retry is scoped to the shown workspace.

Tests: idle workspace → `MainState::Idle` with identity; placeholder line
content (full / remote-no-branch / config-unloadable); dead session → `Failed`
with last line; a change for the shown workspace retries (unrelated one does
not); navigating away drops the review; `retry_if_stale` only retries idle/
failed slots; `worktree_current_branch` reads HEAD / tolerates missing-repo /
detached HEAD.

Deferred (noted for review):
- A remote idle workspace shows name + machine but no branch (local-only branch
  resolution, to keep an idle-open snappy and avoid an SSH hang).
