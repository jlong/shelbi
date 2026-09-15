# Pluggable Task Stores (GitHub Issues Backend)

## Context

Today a project's task board is markdown files on the local filesystem:
`~/.shelbi/projects/<name>/tasks/*.md`, one file per task, read and written by
the CLI, the Kanban TUI, and the orchestrator. In the [[in-repo-vs-global-project-config]]
split those files are **user-local `state`**, not shareable `config`.

That is the *only* task store shelbi knows about — the filesystem shape is
assumed in many call sites. We want the task store to become a **choice per
project**:

- `local` — today's markdown-on-disk board (stays the default; no migration
  forced).
- `github` — the project's tasks *are* GitHub issues in a repo. Moving a card
  moves the issue; the GitHub UI and shelbi stay in sync.

Longer term the same seam supports Jira, Linear, etc. This plan does the seam
plus the first alternative backend (GitHub issues), and nails down the one
thing that is easy to get dangerously wrong: **auth tokens must never be
committable to git.**

The design has four parts: (1) a backend trait, (2) how a project selects a
backend, (3) how a shelbi task maps onto a GitHub issue, and (4) token
management done the modern, safe way.

## Design

### 1. The `TaskStore` trait — the seam

Extract one trait that captures everything the rest of shelbi needs from the
board. Every method returns/accepts the same domain types we already have
(`Task`, status id, workspace name) so callers never learn which backend is
live. Derived from the current CLI + orchestrator operations:

```
trait TaskStore {
    fn list(&self) -> Result<Vec<Task>>;                 // whole board
    fn get(&self, id: &TaskId) -> Result<Option<Task>>;
    fn add(&self, spec: NewTask) -> Result<Task>;        // title, body, workflow, …
    fn move_status(&self, id: &TaskId, to: &StatusId, reason: &str) -> Result<()>;
    fn set_priority(&self, id: &TaskId, pos: PrioMove) -> Result<()>; // top/up/down/set N
    fn set_fields(&self, id: &TaskId, f: TaskFields) -> Result<()>;   // branch, depends_on, prefers_machine, assigned_to
    fn cancel(&self, id: &TaskId, reason: &str) -> Result<()>;
    fn poll_changes(&self, since: Cursor) -> Result<(Vec<Change>, Cursor)>; // feeds the event log
}
```

Two wins fall out immediately:

- **`LocalStore` is a pure refactor.** Wrap today's filesystem code behind the
  trait with zero behavior change — a big, safe, well-tested first PR.
- **The workflow/category layer stays backend-agnostic.** `move_status` speaks
  status *ids*; the [[workflows]] machinery derives categories the same way for
  local or GitHub, so Zen auto-merge, the activity feed, and reaction rules
  don't care which store is live.

### 2. Selecting a backend (per-project config)

A `task_store` block in the project YAML. Absent ⇒ `local`, so every existing
project keeps working untouched.

```yaml
# ~/.shelbi/projects/<name>.yaml  (or in-repo .shelbi/, per the config/state plan)
task_store:
  backend: github            # local (default) | github
  github:
    repo: owner/repo         # where the issues live
    status_labels: prefixed  # shelbi:status/<id>  (see §3)
    priority: body-frontmatter   # body-frontmatter (v1) | projects-v2 (future)
    # NOTE: no token here — auth is resolved out-of-band, see §4
```

The backend block carries only **non-secret** connection facts. The secret is
never a field in any committed file.

### 3. Mapping a shelbi task onto a GitHub issue

One shelbi task ⇔ one GitHub issue. The trick is that a shelbi task has fields
GitHub issues have no native slot for (workflow, branch, depends_on,
prefers_machine, and shelbi's own *workspace* assignment). Split them:

**Native GitHub fields (visible/useful in the GitHub UI):**

| Shelbi concept | GitHub representation |
|---|---|
| Task title / body prose | Issue title / body |
| Status (`todo`, `review`, …) | A single label `shelbi:status/<id>`; moving = swap the label |
| Stable shelbi id (slug) | A label `shelbi:id/<slug>` (round-trip anchor) + issue number |
| Done / Canceled | Issue closed (with the terminal status label recording which) |
| Category | **Not stored** — always derived from status via the workflow |

**Shelbi-only fields → a fenced metadata block in the issue body:**

```
<!-- shelbi:begin -->
​```yaml
workflow: app
branch: jlong/fix-thing
depends_on: [other-task]
prefers_machine: hub
priority: 40
​```
<!-- shelbi:end -->
```

The `shelbi:begin/end` markers mean a human editing the prose part of the issue
never clobbers shelbi metadata, and shelbi rewriting metadata never clobbers
their prose. This is the round-trip-safety mechanism.

**Deliberately NOT mapped to GitHub:**

- **Workspace assignment / in-flight routing** (`assigned_to = alpha`,
  current-dispatch bookkeeping). GitHub `assignees` are real user accounts, not
  shelbi workspaces — overloading them would be wrong and confusing. This is
  ephemeral runtime state; keep it user-local in `~/.shelbi/projects/<name>/`
  exactly as [[in-repo-vs-global-project-config]] classifies runtime state.
  **The GitHub issue holds the durable task definition; ~/.shelbi holds the
  ephemeral routing.** Clean split.

- **Priority ordering.** Issues have no inherent order. v1: an integer in the
  body-frontmatter block, sorted client-side (simple, no extra API surface).
  Future: a **GitHub Projects v2** board with a numeric/position field — that
  also gives non-shelbi teammates a real native kanban, at the cost of the
  heavier GraphQL Projects API.

**Source of truth & sync.** v1: shelbi is the authoritative *writer*, but reads
back so the GitHub UI stays a first-class control surface. `poll_changes`
(driven off the orchestrator's existing heartbeat) pulls external edits —
someone reprioritizes or closes an issue on github.com — and reconciles them
into the event log, so a human closing an issue behaves like moving the card to
Done. The **review-marker handoff still works unchanged**: a workspace writes
its ready-marker file, the poller flips the issue's `shelbi:status/*` label to
the review status. Real-time webhooks are a later optimization (they need an
ingress endpoint); polling via `gh` is enough for v1 and matches the rate we
already operate at.

### 4. Token management — the modern, safe way

Goal: **it must be physically impossible to commit a token to the repo**, and
ideally shelbi never holds a long-lived secret on disk at all. In rough order
of "most modern / safest" to "simplest," with a clear recommended default:

**(a) Recommended default — reuse the `gh` CLI's auth. No token anywhere in
shelbi.** Shelbi already shells out to `gh` for PRs. `gh auth login` stores the
credential in the **OS keychain** (macOS Keychain / libsecret on Linux), not a
plaintext file. The GitHub backend calls `gh api` / `gh issue …` and inherits
that auth. Result: **nothing secret in the repo, nothing secret in ~/.shelbi,
nothing for shelbi to manage.** For a newcomer this is by far the least
foot-gun-prone path — the secret lives in the OS's real secret store, which is
what it's for.

**(b) Environment variable (`GH_TOKEN` / `GITHUB_TOKEN`).** Read at runtime from
the process environment; shelbi never writes it down. Sourced from a shell
profile, `direnv`, or a secrets manager (`op run`, `sops`, Vault). This is the
CI / headless path and composes with everything.

**(c) File-based — but OUTSIDE the repo.** If someone wants a file, put it at
`~/.shelbi/projects/<name>/tokens.yml` — i.e. in user-local state, **not in the
project working tree at all.** Because it physically isn't inside the repo,
there is nothing to `git add`, no `.gitignore` needed, and no way to leak it via
`git add -f`, an editor backup, or a misconfigured tool. `chmod 600`. This is
strictly safer than the in-repo-gitignored file, and it's where all other
per-project runtime state already lives.

**On the "in-repo `tokens.yml` + `.gitignore`" idea (your instinct):** it works,
and it's a *reasonable* pattern, but it's second-best — a gitignored file is
still a plaintext secret sitting *inside* the working tree, one `git add -f`
(or a tool that ignores `.gitignore`) away from being committed. If we ever do
support in-repo tokens for team ergonomics, shelbi must (1) create the file,
(2) write/verify a sibling `.gitignore` that ignores it, **and** (3) refuse to
start with a loud error if `git ls-files` shows the token file is tracked —
belt and suspenders. But prefer (a)/(b)/(c) and treat this as a last resort.
This mirrors the trust lesson in [[config-provenance-and-change-log]] /
"no silent git-hook install": be explicit and safe with the user's repo.

**Resolution order** (first hit wins, so the safe options take precedence):
`GH_TOKEN`/`GITHUB_TOKEN` env → `gh` keychain auth → `~/.shelbi/.../tokens.yml`
→ error with an actionable "run `gh auth login` or set `GH_TOKEN`" message.

**Token *shape* — least privilege, whichever mechanism:**

- Use a **fine-grained personal access token** scoped to the **single repo**,
  with only `Issues: Read/Write` (add `Contents` / `Pull requests` only if
  shelbi also drives branches/PRs in that repo), and an **expiry**. Not a
  classic token with broad `repo` scope.
- For orgs/teams, a **GitHub App installation token** is the most correct option
  (short-lived, auto-rotating, per-repo) — heavier to set up, so note it as a
  later enhancement, not v1.
- Never log the token; redact it in any debug/trace output.

### 5. Rollout phases

1. **Extract the `TaskStore` trait; `LocalStore` implements it.** Pure refactor,
   no behavior change, lots of new tests. Ships alone.
2. **Config plumbing** for `task_store.backend` (still only `local` resolvable).
3. **Token resolution layer** (env → gh → file), standalone and unit-tested,
   with redaction.
4. **`GitHubStore` read path** (`list`/`get`) behind the trait; point the TUI
   and orchestrator reads at the trait.
5. **`GitHubStore` write path** (`add`/`move_status`/`set_fields`/priority) +
   review-marker → status-label promotion.
6. **`poll_changes` reconciliation** for external GitHub edits into the event log.
7. **Migration command** — `shelbi task-store migrate --to github|local`,
   idempotent via the `shelbi:id/<slug>` anchor; one issue per existing task
   file and the reverse.
8. **Later:** Projects v2 priority board, webhooks, and the next backends
   (Jira/Linear) — which now just implement the same trait.

### 6. Open questions

- **Runtime routing placement.** Keep `assigned_to`/`prefers_machine`/in-flight
  purely local even under the GitHub backend? (Leaning yes — §3.)
- **Repo cardinality.** One issues-repo per shelbi project, or several projects
  sharing a repo (disambiguated by a `shelbi:project/<name>` label)?
- **Rate limits / cadence.** `gh` API budget vs. poll interval; back off on the
  quiescent heartbeat like the orchestrator already does.
- **Offline / GitHub-unreachable.** Read-through cache in `~/.shelbi` so the
  board still renders and degrades gracefully when the API is down.
- **Label hygiene.** Auto-create the `shelbi:status/*` label set on first use;
  reconcile if a human renames/deletes one.

---

## Decisions — 2026-08-31 (revises §2–§6; direction from jlong)

These supersede the earlier draft where they conflict.

### D1. Config key is `issue_tracker`, not `task_store`

```yaml
issue_tracker:
  backend: github          # file_system (default) | github | jira | linear | …
  github:
    repo: owner/repo       # REQUIRED — which repo the issues live in
  # jira:
  #   project: PROJ        # each tracker selects a project/repo/team
  # linear:
  #   team: ENG
```

- Default backend renamed `local` → **`file_system`** (today’s markdown board).
- Every remote backend (GitHub/Jira/Linear) can host many projects, so the
  backend block MUST carry a project selector (`repo` for GitHub, `project`
  for Jira, `team`/project for Linear). Still no secret in any committed file.

### D2. Auth via `gh` — confirmed

Reuse the `gh` CLI’s keychain auth as the default (plan §4a). Nothing secret in
the repo or `~/.shelbi`. Env (`GH_TOKEN`) still works for headless/CI. Jira/
Linear get analogous env/keychain resolution when those backends land.

### D3. No local cache — live API calls (drops the read-through cache)

Do **not** mirror issues into `~/.shelbi`. Query the tracker API live whenever we
need issue state. Rationale: a local cache that drifts out of sync is worse than
a live read. This **reverses** the §6 “read-through cache for offline” open
question — accepted consequence: **the board does not render when the tracker is
unreachable; it degrades to an explicit error, not stale data.** (Confirm this
tradeoff is acceptable.)

- Wrinkle to resolve: the orchestrator reaction loop is event-driven, so we still
  need change-detection (human closes an issue / adds a comment → emit an event).
  That needs a small **poll watermark** (last-seen `updated_at` / last comment id
  per issue), NOT a content cache. GitHub supports this cheaply (`since=` on
  issues + comments, issue `updated_at`). So: live reads for content, a
  lightweight sync cursor for “what changed since I last looked.”

### D4. Comments are first-class

Issue comments frequently carry critical context, so:

- `IssueStore` trait gains `list_comments(id)` and `add_comment(id, body)`.
- New CLI: `shelbi issue comment <id> "<text>"` to post a comment from the CLI.
- Comments must reach the **workspace at dispatch** (workspaces don’t run
  shelbi, so the dispatch/body injection must include the issue body **and** its
  comments), and late-arriving comments must be caught at handoff — same lesson
  as “requirements in the task body, not mid-task directives.”
- `poll_changes` surfaces new comments into the event log so the orchestrator is
  aware of them.

### D5. Language: task → issue

Adopt **issue** as the domain noun across the product (`shelbi issue …`, the
Kanban “issues”, config, docs), since every backend — including the file_system
one — is an *issue tracker*. The `TaskStore` trait becomes `IssueStore`; `Task`
→ `Issue`. Blast radius (CLI command surface, TUI labels, docs, site copy) and
back-compat aliasing (`shelbi task` → `shelbi issue`) to be scoped as its own
task before the rename lands.

### Still open
- D3 offline tradeoff confirmation (no board when tracker is down).
- Rename scope: full CLI/TUI/docs/site, and whether `shelbi task` stays as a
  deprecated alias.


### Open items resolved — 2026-08-31

- **D3 offline tradeoff: ACCEPTED.** For an external tracker, no board when the
  tracker is unreachable (degrade to explicit error, never stale data) is the
  intended behavior. No offline fallback cache.
- **Rename scope: full & consistent.** The code must use `issue` consistently
  (no half-task/half-issue). Rollout MAY be split across multiple cards, but the
  end state is a codebase uniformly named `issue` (`IssueStore`, `Issue`,
  `shelbi issue …`, TUI, docs, site). `shelbi task` retained as a thin
  deprecated alias for muscle memory unless dropped later.



### D3 retired — 2026-09-14 (direction from jlong)

**"Render stale, never act stale."** The 2026-08-31 D3 text ("no local cache, no stale
data, no board when the tracker is unreachable") no longer describes the product and
is retired as of 2026-09-14. The daemon-published open-board index is the render
source; when a refresh fails the last index is served and marked stale rather than
replaced by an error. The rule that survives is the acting half: anything that acts on
an issue fetches that issue fresh first, and destructive poller decisions require a
warm index. Recorded in [[github-issues-robustness-and-performance]] under "Decisions
recorded 2026-09-14".

