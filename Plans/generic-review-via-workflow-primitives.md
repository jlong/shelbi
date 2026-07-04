# Generic Review via Workflow Primitives

**Status:** design locked with jlong 2026-07-03. Ready to break into
implementation tasks. Author: Orchestrator.

## North star (definition of done)

**After this plan ships, there is NO review-specific code in the crates.**
`grep -rniE "review" crates/ --include=*.rs` should find nothing but incidental
prose — no `Review`-named types, functions, config, routing branches, CLI
commands, or TUI sections. "Review" becomes purely a *configuration* of generic
primitives: a workspace carries a **tag**, a **status** routes work to a
tagged workspace, and **transitions run commands** (setup/serve, readiness,
teardown). The shipped `review` agent stays only as a *default agent* (content,
user-editable), referenced by no special-case code.

## Motivation

Review is currently a bespoke feature baked into the core: a `review:` config
block, a `WorkspaceRole` enum, `is_review()` branching, dedicated
`review.rs`/`load_review_workspace` loading, a `shelbi review` command, and
hardcoded TUI "Ready/Queued for Review" sections. That assumes a web dev server
(ports + HTTP probe) and hardwires "review" throughout. Shelbi should express
review as composition of generic workflow primitives so it works for any project
type and carries zero review-specific machinery.

## The vision (jlong)

> A Review column is simply a **Status** that uses a **workspace with a
> particular tag**, plus a **transition rule** that runs a set of commands
> (install libraries, start a server) and a **teardown transition** that runs a
> set of commands. Workspaces have a **`tag`** key (not `role`). `is_review` and
> all review-specific code go away.

## Review-specific surface to REMOVE (and its generic replacement)

| Remove (review-specific)                                                                             | Replace with (generic)                                                                                            |
| ---------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| `review:` block — `ReviewConfig`/`ReadyProbe` (base\_port, port\_stride, setup, serve, ready\_probe) | transition `run` + `ready`/`ready_timeout` commands; `$SLOT` env                                                  |
| `WorkspaceRole { Dev, Review }` enum + `role` field                                                  | workspace **`tag: Option<String>`**                                                                               |
| `WorkspaceSpec::is_review()`                                                                         | generic tag test at call sites / a status↔tag match                                                               |
| `Project::review_workspaces()` / `dev_workspaces()`                                                  | generic `workspaces_with_tag(tag)` / `workspaces_untagged()` (or query by the status's required tag)              |
| `review.rs`, `workspace::load_review_workspace`, `review_workspace_port`                             | generic "load a task onto a workspace matching the status's tag, then run the status's enter-transition commands" |
| poller `maybe_promote_to_review` review-specifics                                                    | generic transition handling (already partly generalized by the shipped transition marker)                         |
| `shelbi review <id>` command                                                                         | generic load/dispatch onto a tagged workspace (command removed or renamed to a tag-neutral form)                  |
| TUI hardcoded "Ready for Review" / "Queued for Review" sections + Review column special rendering    | render from status/category generically (labels come from the status name)                                        |

## Generic design (locked decisions)

### A. Workspaces carry a `tag` (remove `role`/`is_review`)

```rust
pub struct WorkspaceSpec {
    pub name: String, pub machine: String, pub runner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,      // e.g. "review"; None = normal slot
}
```

- Remove `WorkspaceRole`, the `role` field, and `is_review()`. Routing is by tag.

- **Back-compat:** tolerate a legacy `role:` key on read, mapping `role: Review`
  (any case) → `tag: "review"`, `role: Dev` → untagged. Canonical/scaffold/docs
  use `tag`; `role` is never written. No file migration (ContextStore precedent).

### B. A Status routes to a tagged workspace

```yaml
statuses:
  - id: review           # just a status; nothing special about the name
    owner: user
    workspace_tag: review # route tasks here to a workspace tagged `review`
```

- Dispatch/loader picks a free workspace whose `tag` matches. The status name is
  arbitrary; no code branches on "review".

### C. Transitions run command sets, with a `$SLOT` env contract

```yaml
transitions:
  - from: in-progress
    to: review
    run:                                            # commands on ENTER
      - npm install
      - npm run dev -- --port $((3000 + $SLOT))     # server; holds the pane
    ready: curl -sf localhost:$((3000 + $SLOT))     # readiness: must exit 0
    ready_timeout: 90s
  - from: review
    to: done
    actions: [merge, delete_branch]
    run:                                            # teardown on EXIT
      - <stop server / cleanup>
```

- **`$SLOT`**: each workspace carries a numeric `slot` key; Shelbi exposes it as
  `$SLOT` to transition commands, which do their own math (`3000 + $SLOT`). No
  bespoke `$PORT`. Also inject `$SHELBI_TASK`/`$SHELBI_BRANCH`/`$SHELBI_WORKTREE`/`$SHELBI_MACHINE`.

- Commands run in the task's worktree on the workspace's machine, **host-routed
  via** **`shelbi_ssh::run`** (works on devbox).

- **`ready`** **+** **`ready_timeout`** (default 90s): poll until exit 0, then mark serving.

- **Teardown is not magic**: it's the `run:` commands on the declared exit
  transition (`review → done`, `review → in-progress` on bounce). No auto hook.
  (A `to: "*"` wildcard transition may be worth supporting so one teardown rule
  covers all exits — Open items.)

- `actions` (git primitives) and `run` (shell) compose on one edge.

- Serve foreground/background mechanics: the long-running server holds the
  workspace's pane; the transition is "entered" once launched; `ready` decides
  serving. Exact mechanics finalized in Phase 1.

## Backward compatibility (tolerate, don't migrate)

- Legacy `review:` blocks are **silently ignored** (ContextStore precedent).

- Legacy `role:` keys map to `tag` on read (see §A). No on-disk migration; users'
  files are never rewritten.

- Existing tagged (`role: review`) workspaces keep working via the alias.

## Open items (resolve during implementation)

- `to: "*"` wildcard transitions (jlong): a status change requires a declared
  transition; a wildcard target lets one teardown rule cover all exits. Confirm
  whether unlisted edges stay any-to-any.

- Serve foreground/background + teardown mechanics (§C).

- Default `slot` assignment when a workspace omits it (declaration order?).

- `shelbi review` fate: remove vs. rename to a tag-neutral load command.

- TUI: how status/category drive the (formerly review) sidebar sections + labels.

## Implementation phases

- **Phase 1 (app): generic workflow primitives.** Workspace `tag` + `slot`
  (remove `role`/`WorkspaceRole`/`is_review`, with legacy `role:` tolerance);
  status `workspace_tag`; transition `run`/`ready`/`ready_timeout` executed
  host-routed with the `$SLOT`/`$SHELBI_*` env; generic tag queries replacing
  `review_workspaces()`/`dev_workspaces()`. Tests: tag routing, enter/exit
  ordering, env, ready timeout, legacy `role:` load.

- **Phase 2 (app): delete the review-specific code.** Remove `ReviewConfig`/
  `ReadyProbe`, `review.rs`/`load_review_workspace`/`review_workspace_port`, the
  poller's review-specifics, `shelbi review`; reroute loading + serving through
  the generic tag + transition-command path; genericize the TUI sidebar sections
  and column rendering to be status/category-driven. **Exit check:** `grep -rniE "review" crates/ --include=*.rs` returns only incidental prose. Tests.

- **Phase 3 (docs): rewrite** `concepts/review-workspaces.mdx`,
  `configuration/workflow.mdx`, `guides/getting-started/review-workspaces.mdx`,
  and any `role`/`is_review`/`review:` references to the generic tag+transition
  model; document `tag`, `slot`, `$SLOT`, `run`/`ready`/teardown, `workspace_tag`.

## Files likely touched

- `crates/shelbi-core/src/model.rs` — WorkspaceSpec (`tag`, `slot`, remove
  `role`/`WorkspaceRole`/`is_review`), Transition (`run`/`ready`/`ready_timeout`),
  Status (`workspace_tag`), remove `ReviewConfig`/`ReadyProbe`, generic tag queries.

- `crates/shelbi-orchestrator/src/{transition,review,workspace,poller?}.rs` — run
  commands on edges; delete review\.rs / load\_review\_workspace; generic tag load.

- `crates/shelbi-tui/src/{poller,app,kanban,sidebar,review}.rs` — remove review
  special-casing; status/category-driven rendering.

- `crates/shelbi-cli/src/…` — remove `shelbi review`; `tag`/`slot` in wizard/scaffold.

- Docs as in Phase 3.

## Relationship to existing plans / work

- Supersedes `Plans/review-workspaces.md` (the serving model + review-specific machinery).

- Builds on `Plans/workflow-transition-hooks.md` and the shipped agent send-back /
  transition-marker feature (edges already carry actions + can be triggered; this
  adds command execution, teardown, `$SLOT`, workspace tags, and removes all
  review special-casing).
