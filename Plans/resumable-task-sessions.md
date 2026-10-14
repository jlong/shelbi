# Resumable Task Sessions

Status: draft 2026-10-14, written by the orchestrator at John's request.
Awaiting John's review before any task is filed.

## Context

Every dispatch today starts a cold agent: a fresh `claude` or `codex` process
in the workspace's worktree, with the task body as its first prompt. That is
right for a new task. It is wrong for rework.

Rework happens constantly. A reviewer bounces a branch for a rebase, a missed
acceptance criterion or a wrong decision; Zen's self-review does the same with
`zen:review-reject`; the heartbeat sweep finds a stalled worker; a quit and
reopen leaves four in-progress workers to be brought back. In each case the
agent that did the work holds the context that makes the rework cheap: which
files it touched and why, what it tried and rejected, what the task's prose
left ambiguous and how it resolved it. A cold relaunch throws that away and
pays for it twice, once in tokens to rebuild the picture from the diff and
once in mistakes made without it.

The live example from today: `dt-shell-sidebar` handed off minutes before a
sibling merged, conflicted in nine files, and was re-dispatched cold with a
rework section appended to its body. The session that wrote those 5,000
lines had 500k tokens of context about them and none of it survives.

Shelbi already has two partial mechanisms, neither of which is enough:

- `shelbi issue resume` relaunches a Claude worker with `--continue`, which
  picks the most recent conversation *in that directory*. It breaks the
  moment the workspace has run another task since (the most recent
  conversation is the wrong one), it does nothing for Codex, and it cannot
  follow a task to a different workspace.
- `shelbi message` appends to a per-task log that a live session's Stop hook
  drains. It only reaches a session that is already running and idle at the
  right moment; it cannot address a session that has exited.

The orchestrator pane solves this for Codex with `codex-thread.json`
(`wake.rs`): Shelbi creates the thread itself, records its exact id, and
relaunches with an explicit resume. This plan extends that idea to every
task on every workspace, for both runners.

### Decisions

| Question | Decision |
|---|---|
| Where the id lives | On the task, in frontmatter, as a `session:` block (runner, id, workspace, cwd, recorded_at). The task is the unit of work that moves between workspaces and bounces between columns; the id must travel with it. |
| When it is written | Once the first dispatch confirms the agent live (the same readiness probe the dispatch already waits for). Never before, so a failed launch leaves no stale id. |
| How Claude's id is known | Shelbi mints a UUID and launches `claude --session-id <uuid>`. No capture, no hook, no race. |
| How Codex's id is known | Codex mints its own id. The worker's SessionStart hook writes the id Codex hands it to `<worktree>/.shelbi/session-id`; the dispatcher reads that file after readiness. Fallback if the hook payload lacks the id: the newest rollout under `~/.codex/sessions/` whose recorded cwd is the worktree. |
| Resume primitive | Claude: `claude --resume <id>`. Codex: `codex resume <id>`. Both replace today's `--continue`. |
| How the rework message arrives | As the startup prompt of the resumed session, through the existing paste-on-resume path (`with_startup_prompt` with `PromptInjectionSpec::paste()`), verified by `send_verified`. Not through `shelbi message`. |
| Rework surface | `shelbi issue rework <id> --message <text>` (or stdin), which appends a dated `## Rework` section to the body, moves the card to the ready status with `reason=rework`, and marks the task as resume-on-dispatch. Dispatch then resumes instead of cold-starting. Zen's bounce path and the review TUI's reject use the same command. |
| Cross-workspace | A task may resume on any open workspace, not just the one that recorded the id. Claude keys its transcripts by cwd, so Shelbi relinks the transcript into the target worktree's project directory before launch and resumes with `--fork-session`, recording the new id. Codex resumes by id regardless of cwd (its picker filters by cwd, its id lookup does not); the spike confirms both. |
| Optional per harness | A runner capability, `session_resume`, on the runner adapter. Claude and Codex set it; any other runner (or a user who disables it in the runner spec) falls back to today's cold start with the rework section in the body. The orchestrator's instructions describe both paths. |
| Fresh start on demand | `shelbi issue start --fresh <id>` clears the `session:` block and cold-starts. The default `start` on a task with a recorded session and a pending rework resumes. |

## Design

### 1. The `session:` block

```yaml
session:
  runner: claude            # claude | codex
  id: 7d2c6c1e-…            # the runner's own session/thread id
  workspace: golf           # where it was recorded
  cwd: /Users/jlong/Workspaces/shelbi/.shelbi/wt/golf
  recorded_at: 2026-10-14T13:34:01Z
  resume_pending: true      # set by `issue rework`, cleared by the next launch
```

`shelbi_state::IssueFields` gains `session: Option<Option<SessionRecord>>`
(set / clear), `NewIssue` does not (a new task never has one). The GitHub
issue store carries it the same way it carries `assigned_to` and
`prefers_machine` today (the hidden frontmatter block in the issue body).
`shelbi issue show` prints it; `shelbi issue list` does not.

### 2. Recording at first dispatch

`start_workspace_on_task` already builds the launch command and waits for
readiness before pasting the prompt. The change:

- Claude: `workspace_launch_command_with_startup_prompt` takes a
  `SessionPlan` (`Fresh { id }`, `Resume { id }`, `Relink { from_cwd, id }`,
  `None`) and the Claude adapter emits `--session-id <id>` for `Fresh`.
- Codex: the deployed `codex.session-start` hook writes the session id from
  its stdin payload to `.shelbi/session-id` (the hook already runs with the
  worktree as cwd and `TASK_ID` in the environment). The dispatcher reads it
  after `wait_for_claude_ready`'s Codex counterpart reports ready.
- Once the session is live and the prompt is confirmed submitted, the
  dispatcher writes the `session:` block. The event line is
  `session task=<id> workspace=<ws> runner=<r> status=recorded id=<uuid>`.

The hook template change is a shipped default, so it ships with a
config-upgrade sniffer that re-deploys the hook into existing worktrees on
the next boot (see [[reference-changing-shipped-defaults-needs-config-upgrade-sniffer]]).

### 3. Resuming

`resume_workspace_on_task` becomes the single resume path and takes the
`session:` block into account:

1. **Same workspace, transcript present**: launch with `--resume <id>` /
   `codex resume <id>`; paste the rework message as the startup prompt.
2. **Different workspace (Claude)**: copy
   `~/.claude/projects/<encoded old cwd>/<id>.jsonl` into
   `~/.claude/projects/<encoded new cwd>/`, launch `--resume <id>
   --fork-session`, record the new id with the new cwd. The copy is a
   relink, not a move; the old transcript stays for the user's own
   `claude --resume`.
3. **Different workspace (Codex)**: `codex resume <id>` directly; record the
   new workspace and cwd.
4. **Transcript missing or runner refuses the id**: log
   `session ... status=unresumable reason=<why>`, clear the block, fall back
   to a cold start with the rework section in the body. Never fail the
   dispatch over a lost transcript.

The resume-from-summary dialog that `ready.rs` already answers for
`--continue` is answered the same way for `--resume`.

### 4. The rework message

`shelbi issue rework <id> --message <text>` (text from the flag or stdin):

- Appends to the body:

  ```
  ## Rework (2026-10-14T14:12Z, orchestrator)
  <text>
  ```

- Sets `session.resume_pending: true`, moves the card to the workflow's
  ready status with `--reason rework:<source>`, and lets the normal
  auto-dispatch pick a workspace (the recording workspace first when it is
  free, then any free dev slot).
- The startup prompt of the resumed session is the rework text, framed:

  ```
  Rework requested on <task-id>. The branch you handed off has been reviewed;
  it is not accepted yet. Continue in this same conversation, you have all
  the context.

  <text>

  When done, run the same checks as before, commit, and hand off again.
  ```

  The task body is not re-sent; the resumed session already has it. A cold
  fallback sends the full body, which now contains the rework section.

Callers: the orchestrator's Zen `review-reject` path, the review TUI's
reject action (its text box becomes the message), the heartbeat's stalled
worker recovery (message: "your session was interrupted, continue"), and
the reopen path, which today sends `dev-resume` with a generic continuation.

### 5. What changes in the orchestrator's instructions

The bounce flow in `zenmode.md` and the orchestrator instructions becomes:
"bounce with `shelbi issue rework <id> --message ...`; do not append rework
to the body by hand and do not `issue start` the task yourself; dispatch
resumes it." The `issue start` kills-the-prior-pane problem (see
[[reference-start-release-kills-unrelated-pane]]) goes away for rework
because the resume path targets the recorded workspace only when it is
free.

### 6. Per-runner matrix

| | Claude Code 2.1 | Codex 0.162 | Other |
|---|---|---|---|
| Set id at launch | `--session-id` | no (Codex mints) | no |
| Capture id | not needed | SessionStart hook payload, else sessions dir scan | no |
| Resume by id | `--resume <id>` | `codex resume <id>` | no |
| Resume from another cwd | relink transcript + `--fork-session` (spike) | by id, picker-only cwd filter (spike) | no |
| Queue a message into a live session | paste via `send_verified` | `codex queue` (candidate for `shelbi message`'s Codex path later) | no |
| Capability flag | `session_resume: true` | `session_resume: true` | `false` → cold fallback |

### 7. Failure handling

- A recorded id whose transcript is gone (user ran `claude` cleanup, or the
  worktree was recreated): cold fallback, as above.
- Two workspaces resuming the same Claude id concurrently would corrupt the
  transcript; the relink path always forks, so an id is live in at most one
  cwd. The dispatch lock per workspace plus the task's single `assigned_to`
  enforce one resume at a time per task.
- The id is secret-free (a UUID) and safe on a GitHub issue body.

## Phasing

**Phase 0, spike (one task, half a day).** In an isolated root with a fake
project: confirm `claude --resume <id>` behavior from a different cwd with
and without the transcript relink and with `--fork-session`; confirm the
Codex SessionStart hook payload carries the session id and that
`codex resume <id>` works from another cwd; record exact findings in
`docs/sessions/resume-spike.md`. Decides whether the relink step is needed.

**Phase 1, record ids.** `session:` block in `shelbi-state`; Claude
`--session-id` at dispatch; Codex hook capture; write-after-ready; event
line; `issue show` output; tests for the field round-trip and the launch
builder.

**Phase 2, resume primitive.** `SessionPlan` in the launch builder; `--resume`
and `codex resume` adapters; `resume_workspace_on_task` reads the block;
relink step if Phase 0 says so; unresumable fallback; `issue start --fresh`.

**Phase 3, rework command and callers.** `shelbi issue rework`; the framed
startup prompt; Zen `review-reject`, review TUI reject, heartbeat stalled
recovery and the reopen resume all route through it; orchestrator
instructions and `zenmode.md` updated; config-upgrade sniffer for the hook
template.

**Phase 4, desktop and docs.** The desktop review view's reject action
calls the same command; site docs for `session:`, `issue rework` and
`--fresh`; the Codex `queue` path for live-session messages as a follow-up.

Phases 1 and 2 are the critical path and can be one task each; 3 splits by
caller.

## Open questions

1. Should `issue rework` default to the recording workspace or to any free
   slot? The plan says recording workspace first because Claude's transcript
   is already there; a busy recording workspace falls through to any free
   slot with the relink.
2. Do we want the orchestrator's own pane on this mechanism too (today it is
   `codex-thread.json` for Codex and `--continue` for Claude)? Probably yes,
   as a Phase 4 item, so a reload resumes the exact orchestrator session.
3. GitHub-backed boards: the `session:` block adds a frontmatter write on
   every first dispatch. One PATCH, well inside budget, but worth a line in
   the gh-budget accounting.

## Acceptance criteria

- [ ] A task's first dispatch records a `session:` block with the runner's
      real id, for both Claude and Codex, and `shelbi issue show` prints it
- [ ] `shelbi issue rework <id> --message ...` on a handed-off task resumes
      the original conversation (the agent can answer a question about an
      earlier decision without re-reading the diff) and receives the message
      as its first prompt
- [ ] The same rework resumed on a different open workspace behaves the same
      way, for both runners
- [ ] A missing transcript degrades to a cold start with the rework section
      in the body, with an `unresumable` event, and never blocks dispatch
- [ ] `issue start --fresh` cold-starts and clears the block; a runner with
      `session_resume: false` always cold-starts
- [ ] Quit and reopen brings in-progress workers back on their exact sessions
