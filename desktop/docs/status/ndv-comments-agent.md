# ndv-comments-agent: comments, sending them to the agent, revert

Part of `native-diff-view`. The state and the workers are `shelbi_app::diff`, so
the TUI and the desktop behave the same; each client adds only how it asks for
the Reject prompt.

## What landed

- **Threads**: Skim's keys over the foundation's comments. `o` now shows a
  multi-line comment or reply in full (`ThreadPart::More`), and both clients read
  a thread's text from one place (`diff::rows::thread_text`).
- **`gY` in a workspace**: one message to the workspace's agent session, through
  the verified-submit path `shelbi send` uses (`shelbi_orchestrator::agent_send`).
  Threads are marked sent only once it landed. A send that fails says why in the
  status bar and leaves them unsent.
- **`gY` in a review**: opens Reject with its reason pre-filled. Reject from the
  panel or the palette starts from the same comments. Threads are marked sent when
  the reject lands, and only those still in the reason.
- **`gA` in a review**: sends them to the Reviewer agent instead.
- **`gr` / `gR` in a workspace**: revert the hunk or file under the cursor.
- **Replies**: `shelbi diff comment reply`, through the hub for a worker on
  another machine. An open view looks at the store every 2s, a review's too, so a
  reply shows with no refresh.

## Decisions

- **What revert does** (q-001, answered by the user). Uncommitted work restores
  `HEAD` (worktree and index). Committed work is undone as an uncommitted change:
  the reverse written to the worktree, commits untouched, so `git checkout` takes
  it back. A file or hunk that's part of each reverses both the same way. Only
  what the worktree can't take cleanly is refused, with the reason in the status
  bar: a conflicting edit on those lines (`git apply` lands whole or not at all),
  a change that moved since it was drawn (the fingerprint), or a merge, rebase or
  cherry-pick in progress.
- **Two steps, so the confirm is honest.** `gr` asks the backend what the revert
  would do first, because only git knows whose work it is. The answer brings the
  question to the status bar. `y` or `Enter` goes ahead, anything else keeps the
  work.
- **Keys**: `gA`, `gr` and `gR` are new, in the `diff` context and rebindable.
  Skim has no revert, and its `x` is the PR threads' resolve, so they sit beside
  `gY`. The palette and `?` list only what applies to the target.
- **An agent's reply doesn't reopen a thread.** A person's does. Otherwise `gY`
  would send the agent its own answer back.
- **The message names each thread and the reply command**, with project and task,
  so an agent whose instructions predate this can still answer. The shipped
  developer instructions teach it too, with a `NeedsJudgment` sniffer
  (`DEV_INSTRUCTIONS_DIFF_COMMENT_REPLY_MISSING`) for existing projects.
- **A reject's comments stay unsent until it lands**, so a refused one loses
  nothing.
- **Comments don't need the worktree.** `comment_access` reaches a task's store
  by task id alone, since Reject from the palette knows no slot.
- **Which runner's parser verifies a send**: the project's baseline runner, as in
  `shelbi send`. A slot doesn't name one.

## What's left

- The confirm is a status-bar line, not a dialog, as `D`'s is.
- `shelbi diff comment add`: an agent can answer a thread but not start one.

## Verified

- `crates/shelbi-app/tests/diff_revert.rs`: every revert shape against real git
  (committed, uncommitted, mixed, rename, binary, new, deleted, CRLF, no newline
  at EOF, conflicts, a merge in progress), and `gY`, `gA` and a reply through a
  whole session. `comments.rs` and `rows.rs` cover formatting, anchors and threads.
- `crates/shelbi-tui/src/shell/diff_view/tests.rs`, `views/diff_view/tests.rs`:
  each client's keys and the Reject hand-off.
- Not walked by hand against a live agent session.
