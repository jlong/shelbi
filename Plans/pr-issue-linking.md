# PR ↔ Issue Linking

## Context

With `issue_tracker.backend: github`, every card on the board *is* a GitHub
issue. But nothing Shelbi opens ever says so. A PR it creates carries the
worker's authored body plus this footer:

```
---

Auto-opened by Shelbi — review at: /Users/jlong/.shelbi/projects/shelbi/tasks/<id>.md
```

Two problems with that, both visible on today's merges:

1. **No closing keyword, so no link.** GitHub never associates the PR with its
   issue. The issue's Development sidebar is empty, the PR doesn't show what it
   closes, and merging the PR outside Shelbi (from the GitHub UI, which is the
   normal thing for a human to do) leaves the issue open and the card stranded
   in `review`.

2. **The footer points at a file that does not exist.** `task_path` is
   `tasks_dir(project)/<id>.md` unconditionally — it has no backend awareness
   (`crates/shelbi-state/src/lib.rs`). On a GitHub-backed board no such file is
   written, so the one pointer we do emit is dead. The issue URL is what the
   reviewer actually wants.

This plan adds a machine-owned reference trailer to the PR body, picks the
notation from where the PR actually lands, and replaces the dead footer path.

## What the code does today

Established by reading the tree at `3d76247`:

- **One chokepoint.** `compose_pr_body(host, worktree, task_body, task_path)`
  (`crates/shelbi-orchestrator/src/git.rs:912`) builds the body for *both* PR
  paths: the `open_pr` transition action
  (`crates/shelbi-orchestrator/src/actions.rs:363`) and `zen pr-create`
  (`crates/shelbi-orchestrator/src/zen.rs:538`). A third, older path —
  `shelbi merge` (`crates/shelbi-cli/src/commands/merge.rs:157`) — composes its
  own text via `derive_pr_text` and is out of scope here, but should end up on
  the same trailer eventually.

- **`Issue` has no issue number.** The struct carries `id`, `title`, `column`,
  `branch`, `depends_on`, `params`… and nothing backend-specific. The number
  lives only inside `GitHubStore`, behind the private
  `resolve_number(&self, id) -> Result<Option<i64>>`.

- **Resolving a number is normally free.** `resolve_number` is cache-first:
  process-local `id → number` cache, then the published `board-index.json`
  map (identity-guarded against a retargeted repo), and only on a cold miss
  does it fall through to `search_number`, which costs one API call.

- **Shelbi already closes the issue itself.** The `review -> done` transition
  merges and the store's `move_status` to a terminal column closes the issue.
  So the keyword is not what closes the card in the normal path.

- **Not every PR targets the default branch.** `resolve_pr_target`
  (`actions.rs`) returns, in order: an explicit transition `target:` override;
  else the branch of the first non-`done` parent in `depends_on`; else the
  project base branch. And the subtask workflows set their own base outright —
  `app-feature-subtask.yaml` has `base_branch: feature/{{feature}}`,
  `site-update-subtask.yaml` has `update/{{update}}`, `subtask.yaml` has
  `task/{{task}}`.

That last point is the one that shapes the whole design.

## Design

### 1. The constraint that decides the notation

**GitHub only auto-closes a linked issue when the PR is merged into the
repository's default branch.** A closing keyword on a PR that targets
`feature/foo` creates the link but never fires, and it does not fire later
either: when the umbrella PR merges to `main`, the keyword lives on a
*different* PR that was merged elsewhere, so the subtask issues stay open.

So a single blanket `Closes #N` on every PR would be a promise Shelbi can't
keep for exactly the workflows where tracking matters most. Instead, pick the
notation from the resolved target:

| Resolved PR base | Trailer | Effect |
| --- | --- | --- |
| project default branch | `Closes #N` | links + auto-closes on merge |
| anything else (umbrella, chain parent, `target:` override) | `Part of #N` | links, no false promise |

The test must be against the **resolved** target from `resolve_pr_target`, not
the workflow's declared `git.base_branch` — a task in a `base_branch: main`
workflow still retargets to a parent's branch when it has a live `depends_on`.

Use `Closes` rather than `Fixes` or `Resolves`: GitHub treats all three
identically, and `Fixes` reads wrong on a feature card. One keyword, used
consistently, is easier to grep for later.

### 2. A backend-agnostic external reference

`compose_pr_body` currently receives no store handle and no number, so the
reference has to come from somewhere. Rather than plumb a bare `i64` through
(which bakes GitHub into a signature shared with the filesystem backend), add
an accessor to the `IssueStore` trait with a `None` default:

```rust
/// The tracker-side identity of `id` on a remote backend: the number a PR
/// body references and the URL a human opens. `None` on a backend whose
/// cards have no external identity (the filesystem store), so callers
/// degrade to today's behavior rather than branching on backend kind.
fn external_ref(&self, _id: &str) -> Result<Option<ExternalRef>> {
    Ok(None)
}
```

```rust
pub struct ExternalRef {
    /// Bare reference token as it appears in a PR body: `#1332`.
    pub token: String,
    /// Fully-qualified form for a cross-repository reference:
    /// `jlong/shelbi#1332`.
    pub qualified: String,
    /// Canonical web URL for the human-facing footer.
    pub url: String,
    /// Whether this backend supports GitHub-style closing keywords in a PR
    /// body at all. Linear and Jira link through their own mechanisms
    /// (magic words, Smart Commits), so they get the reference without the
    /// keyword until those are modeled properly.
    pub supports_closing_keyword: bool,
}
```

`GitHubStore` implements it over the existing `resolve_number`, so it inherits
the cache-first path and costs nothing in the warm case. `FileSystemStore`
keeps the default. This matches the direction in [[pluggable-task-stores]]:
backends differ in identity, not in the caller's control flow.

### 3. Where the trailer goes, and who owns it

Shelbi owns the trailer, not the worker. `compose_pr_body` grows two
parameters — the resolved `ExternalRef` and whether the PR targets the default
branch — and emits:

```
<worker-authored body, or the task body as today>

---

Closes #1332

Auto-opened by Shelbi — review at: https://github.com/jlong/shelbi/issues/1332
```

Machine-owned matters for three reasons: a worker that forgets the keyword
silently loses the link; a worker that guesses the number gets it wrong (the
number isn't in its task body); and two workers following the template
inconsistently give you a board where only some issues close.

Which means `pr-template.md` needs a line telling the author **not** to write a
closing keyword or issue reference — Shelbi appends it. Without that, the
shipped template and the new trailer will race and you'll get `Closes #N`
twice, sometimes with different numbers.

For the same reason, `compose_pr_body` should strip a leading/trailing
closing-keyword line the worker wrote anyway, rather than trusting the
template to be followed.

### 4. Best-effort, never blocking

Every existing input to `compose_pr_body` is best-effort: a missing
`.shelbi/pr-body.md` silently falls back to the task body, and an unreadable
worktree degrades rather than erroring. The reference must behave the same
way. A cold-miss `resolve_number` that falls through to `search_number` can
fail outright inside a rate-limit window, and the handoff must not break over a
footer. On any failure: omit the trailer, keep the URL-less footer, open the PR.

This is a real path, not a theoretical one — see
[[github-issue-caching-and-rate-limits]] for how often the 403 window bites.

### 5. Fix the dead footer path in the same change

Replace `review at: <local tasks/ path>` with the `ExternalRef` URL when one
exists, keeping the local path only for the filesystem backend where it is
genuinely correct. This is a two-line change riding along with a change that
already has the ref in hand, and it removes a pointer that is currently wrong
on every PR this project opens.

## Open questions

1. **Backfill existing open PRs?** `open_pr` no-ops when a PR is already open,
   and `zen pr-create` realigns an auto-created PR without necessarily
   rewriting its body. PRs opened before this ships would keep an
   unreferenced body. A one-shot `gh pr edit --body` backfill is possible but
   rewrites human-visible text; leaving them alone is defensible. **Leaning:
   leave them.**

2. **Should the reference also go in the PR title or branch name?** Not for
   GitHub — the body is the documented mechanism. Flagging it because Linear
   keys off the *branch name* and Jira off commit messages, so if
   [[pluggable-task-stores]] grows those backends, `ExternalRef` may need to
   say *where* its reference belongs, not just what it says.

3. **Cross-repo form.** `zen pr-create` passes an explicit `--repo`. When the
   PR repo and the issue repo are the same (always true today) bare `#N` is
   correct; `qualified` is carried for the day they diverge. Worth deciding
   whether to just always emit the qualified form for safety.

4. **Does a keyword-close and Shelbi's own close fight?** Both resolve to
   closed-as-completed, and whichever lands first makes the other a no-op, so
   this should be benign convergence. Worth one integration test rather than
   an assumption.

## Acceptance criteria

- [ ] A PR opened for a card whose resolved base is the project default branch carries `Closes #<n>` with the card's real issue number.
- [ ] A PR opened for a subtask whose resolved base is an umbrella branch carries `Part of #<n>` and no closing keyword.
- [ ] A task with a live `depends_on` parent gets `Part of`, even under a workflow whose declared `git.base_branch` is the default branch.
- [ ] On the filesystem backend, the PR body is byte-identical to today's output.
- [ ] A number lookup that fails (rate limit, unknown id) omits the trailer and still opens the PR.
- [ ] The footer links the issue URL on a GitHub board, and the local task path on a filesystem board.
- [ ] A worker-authored body that already contains a closing keyword does not produce a duplicate.
- [ ] `pr-template.md` instructs the author not to write an issue reference, and the shipped default carries that line.
- [ ] Merging a `Closes #N` PR into the default branch closes the issue, and the card reconciles to `done` rather than stranding in `review`.

## See also

- [[workflows]] §12 — `transitions:`, `actions:`, and the `target:` override
- [[pluggable-task-stores]] — the store trait this extends
- [[github-issue-caching-and-rate-limits]] — why the lookup must be best-effort
- [[zen-mode]] — `zen pr-create` is the second caller of `compose_pr_body`
