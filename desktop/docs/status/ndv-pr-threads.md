# ndv-pr-threads: the task's PR in the diff view

Part of `native-diff-view`. The model, the reads and the writes are
`shelbi_app::diff::pr`; each client draws the same rows and its own panel.

## What landed

- **The description block** above the first file: number, title, author, branches,
  verdicts, the checks tally with what failed, open threads, and the description.
  `o` folds it to its first two rows. It shows only above files, so an empty table
  still means loading, failed or empty to both clients.
- **The `i` panel**: every check (what needs looking at first), every review with
  its first line, the threads, the description. `j` `k` `Ctrl-d` `Ctrl-u` `g` `G`
  move, `r` reads again, `i` `q` `Esc` close.
- **GitHub's threads at their lines.** `Enter` and `r` reply, `x` resolves or
  reopens, `o` opens and closes, `y` copies, `]c` and `[c` reach them. Each ends in
  `[unresolved]`, `[resolved]` or `[pending]`.
- **`C`**: new comments go to the viewer's pending review on GitHub, or stay local
  (the default). **`R`**: the review's editor, with `Tab` choosing Comment, Approve
  or Request changes, and `Enter` submitting it.
- **A worker of its own** (`diff-pr`): it reads on open, on `r`, after a write and
  every 60s while the view is on screen. Writes run on it in order. A read that
  finds the same publishes nothing.

## Decisions

- **Threads go through the rows comments use.** A thread becomes a `Comment` whose
  id starts `gh:`, kept beside the task's in `DiffState::shown` and never saved. The
  clients, `]c`, `y` and the cursor's place come for free; only what a key does
  differs, by asking whether a thread is GitHub's.
- **A thread is placed by text**, the last line of its comment's diff hunk, nearest
  the line GitHub names. A workspace ahead of its last push still shows a thread on
  its code. Text that's gone puts it at the top of its file; so does a file-level
  thread, without the "outdated".
- **Verdicts follow Skim**: a reviewer's latest approval, request or dismissal counts.
  A later comment doesn't withdraw an approval, and a dismissal leaves none.
  Neutral and skipped checks count as passed, and the panel says which they were.
- **Governed like every GraphQL read** (`shelbi_state::graphql_read`): a parked budget
  or a dead network answers without spawning `gh`. Writes (`graphql_write`) refuse up
  front when parked, and use the create policy, so a dropped connection doesn't post
  twice. `-f` carries all strings: `-F body=@file` would read a file and post it.
- **Words GitHub refuses aren't lost.** A new comment stays as a local one. A reply or
  a review goes back to its editor, or to the clipboard if another mode has the
  keyboard by then (`DiffSession::take_effects`).
- **`gh` runs on the project's machine**, not the worktree's, as the token is there.
  A far window reads through that machine's daemon (`DiffOp::PrLookup` and friends).
- **Unresolved threads open with their replies showing, and resolved ones closed.**
  The wireframe shows the first; GitHub does the second.
- **Comments on lines that aren't pushed** go to GitHub as written, and GitHub refuses
  them with its own reason. Then it's kept as a local comment.

## What's left

- Skim's `e` and `d` on your own GitHub comment, `Ctrl-D` to discard a pending review,
  and `gc` (the conversation view) aren't here. `d` on a GitHub thread says so.
- `gY` sends the task's own comments, not GitHub's threads.
- The first 100 threads, 50 comments each, 100 reviews and 100 checks. The panel says
  when there are more threads than that.
- The block's fold isn't remembered between views.
- GitHub Enterprise: a PR's host is ignored, as in the issue store.
- In the offscreen desktop render the code font draws `↳` much like `(`. Local replies
  show the same, so it predates this.

## Verified

- `crates/shelbi-app/src/diff/pr_tests.rs`: placement, the tally, verdicts, wording,
  and every `gh` call's arguments over a stubbed `GhApi`.
- `state_tests.rs` (`mod pr`): every key. `tests/diff_pr.rs`: a whole session on real
  git with GitHub held back, refusing, or rate limited.
- Each client's tests: `shell/diff_view/tests.rs` (screens as text),
  `views/diff_view/tests.rs` (keys and clicks), and `--visual-views` (`diff-pr*.png`).
- Against GitHub, read only: `fetch` parsed a real PR, and all nine GraphQL documents
  passed schema validation (the mutations with a made-up node id, answered
  `NOT_FOUND`). **No mutation has run against a real PR**, as that would post to one.
