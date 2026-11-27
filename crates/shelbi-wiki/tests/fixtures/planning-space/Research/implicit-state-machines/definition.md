# Implicit state machines: definition and survey rubric

Survey started 2026-11-18 at the user's request. Goal: find every implicit state machine in the Shelbi codebase, judge which ones are risky, and decide which deserve further design thought.

## Definition

A **state machine** is any entity whose behavior depends on which of a finite set of conditions ("states") it is in, and which moves between those conditions in response to events ("transitions").

It is **implicit** when that set of states and the legal transitions between them are not written down in one place as a type plus a transition function. Instead they emerge from code spread around. Typical encodings:

1. **Flag soup.** Several `bool`s / `Option`s on a struct whose valid combinations are the real states (e.g. `started: bool, done: bool, error: Option<..>`; some combos are impossible but representable).
2. **Stringly states.** A `String`/`&str` field compared against literals (`if status == "review"`), or a status read from YAML/JSON/a tracker label.
3. **On-disk markers.** State held as file presence, file contents, pid files, sockets, lock dirs, sentinel/marker files, renamed files, or a field in `state.json`. The "state" is whatever is on disk when someone looks.
4. **External state mirrored locally.** Tracker issue status, PR state, CI state, tmux pane/session liveness, process liveness, git branch/worktree state, read from somewhere else and acted on.
5. **Sequencing/protocol state.** Correctness depends on calls happening in an order (init -> connect -> handshake -> ready), enforced only by convention or by where the code happens to be called.
6. **Timers and retries.** Backoff counters, `last_attempt`, debounce/throttle, timeouts, crash-loop counters, "overdue" logic: time-dependent state.
7. **UI modes.** View/overlay/focus/edit modes, wizard steps, loading/error/empty/loaded, drag states, encoded as several fields or enums matched ad hoc in many places.
8. **Explicit-but-leaky.** An `enum` exists, but transitions are performed by direct assignment in many places with no single transition function, or the enum is incomplete and side-flags carry the rest.

**Not** in scope: a plain enum used as data with no lifecycle; pure functions; simple caches with no behavior change; one-shot `Option` laziness (`OnceLock`).

## What to record per finding

- **Name**: a short title for the machine (e.g. "Workspace dispatch lifecycle").
- **Entity**: what has the state.
- **Encoding**: which of categories 1-8 (may be several).
- **States**: the states you can identify (list them; mark ones that are representable but invalid).
- **Transitions**: who/what moves it between states, with file:line for each writer. Note writers in OTHER crates you notice.
- **Where it lives**: the most important files (file:line).
- **Risk** (high/medium/low) and **why**: concrete failure modes you can see or suspect, such as: invalid combinations representable; transitions from several threads/processes without coordination; on-disk state that can be left half-written by a crash; state duplicated in two places that can drift (memory vs disk, local vs tracker, daemon vs app); missing states (no "failed"/"stuck" state, so errors are silent); timeouts that leave state stranded; restart/reload not restoring it. Cite real code. Mention past bugs if comments or tests reference them.
- **Recommendation**: leave as is / document it / make explicit (enum + single transition fn) / bigger redesign. One or two sentences why.

Precision over volume: a finding needs evidence in code. Merge duplicates (one machine touched from several files is ONE finding). Skip trivial ones, but list them in a short "minor" section at the end so coverage is visible.

## Output

Each slice writes one ContextStore doc, `Research/implicit-state-machines/findings/<slice>.md`, in space `Shelbi`:
- Start with a 3-5 line summary: slice covered, number of findings by risk.
- Then findings ordered high -> low risk, using the fields above.
- End with "Cross-slice leads": machines whose writers/readers live outside your slice (name the other crates/files) so the synthesis can stitch them together.

Slices: orch-core, orch-workspace, state-core, cli-daemon, runtime, app, tui, desktop-views, desktop-shell.
