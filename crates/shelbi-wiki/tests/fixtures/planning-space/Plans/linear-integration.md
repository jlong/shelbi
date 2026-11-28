# Linear Integration (first-class issue tracker backend)

Status: approved by John 2026-11-17 ("build a first class Linear integration ... go hard"). Orchestrator-owned plan; implementation on umbrella `linear` (branch `jlong/linear`).

## Product requirements (John, verbatim intent)

1. When a project is connected to Linear, **statuses come from Linear** and are **read-only in Settings**.
2. A Shelbi project has a set of **filters** that define which Linear issues are visible on the board and to Shelbi agents:
   - **Team** (required: every Linear project must connect to exactly one team)
   - **Project** (optional)
   - **Label** (optional)
   - **Assignee** (optional)
3. **New issues are created with these filters applied** (team, project, label, assignee).
4. **Workflows are built from the statuses just as they are now** (workflow files reference status ids; the catalog is Linear's).
5. **Only one external integration** per project: GitHub *or* Linear (or the local file_system board).

## Architecture

All Rust. Store seam: `IssueStore` trait (`crates/shelbi-state/src/issue_store.rs:488`), built in `build_store` (:72). Config stubs already exist: `IssueTrackerBackend::Linear`, `LinearConnection{team}` (`crates/shelbi-core/src/model.rs:880`).

### Transport: `curl` runner, no new crates
The main workspace has no HTTP client and adding one touches `Cargo.lock` (danger path). Linear's GraphQL API (`POST https://api.linear.app/graphql`, header `Authorization: <personal API key>`, JSON body) is called through `curl` with a swappable runner closure, mirroring `GhRunner`. The API key goes to curl on **stdin via `--config -`** (never argv, never logged). `curl -sS -D <hdrfile> --max-time 30`.

### Auth
`LINEAR_API_KEY` env → macOS keychain item (service `shelbi-linear`, account = project or `default`, via `security find-generic-password -w`) → out-of-repo `tokens.yml` (`issue_auth.rs`). `shelbi tracker linear login` stores a key in the keychain after validating it with `viewer { id name email }`. OAuth is a later phase.

### Rate limits
Linear: per-key request + complexity budgets, headers `X-RateLimit-Requests-Remaining/-Limit/-Reset`, `X-RateLimit-Complexity-*`; throttling surfaces as GraphQL error `extensions.code == "RATELIMITED"` (HTTP 400/429). Record into a Linear budget file analogous to `gh_budget` and back off.

### Config shape
```yaml
issue_tracker:
  backend: linear
  linear:
    team: ENG              # REQUIRED: team key (or id)
    project: "Shelbi"      # optional: project name or id
    label: shelbi          # optional: label name or id
    assignee: me           # optional: "me" | user email | user id
```
Resolved ids are cached in `~/.shelbi/projects/<name>/linear.json` (team id, state ids, project/label/user ids). Validation: team non-empty; `github:` and `linear:` blocks are mutually exclusive (exactly the block matching `backend`).

### Status catalog from Linear
Each team workflow state (`team.states { id name type color position }`) becomes a `ProjectStatus`:
- `id` = slug(name) (`In Review` -> `in-review`), `name` = Linear name, `external_id` = Linear state uuid (stable across renames), ordered by `position`.
- default `category` from state `type`: triage/backlog -> backlog; unstarted -> ready; started -> active, except names matching /review|qa|verif|test/i -> handoff; completed -> done; canceled/duplicate -> archived.
- Settings: names/order/add/delete are **read-only** ("Managed by Linear"); the **category** stays editable (it is Shelbi semantics Linear does not know).
- Sync on connect, on `shelbi tracker linear sync`, and on each board refresh when the team's states changed; workflows referencing a vanished status get a validation error naming it.

### Issue mapping
- Visible set = `issues(filter: { team, project?, labels: {some}, assignee? })`, including completed/canceled (for done history), excluding archived.
- Shelbi id = slug stored in the Shelbi attachment metadata if present, else lowercased identifier (`eng-123`).
- Shelbi-only fields (workflow, params, depends_on, pr, estimate, assigned workspace stays in the local overlay as for GitHub) live in a single **Linear attachment** per issue: `attachmentCreate(input:{issueId, url:"https://shelbi.dev/i/<team>/<shelbi-id>", title:"Shelbi", subtitle:<workflow>, metadata:{...}})` (Linear upserts by url). Fallback if attachments are unavailable: fenced block in description, like GitHub.
- Status = `state` (mapped through external_id). Move = `issueUpdate(stateId)`.
- Rank = Linear `sortOrder` (set_priority computes a sortOrder between neighbours).
- Owner: `assignee` vs `viewer` -> `owner=me|other:<name>|unassigned`. `claim` = assign viewer.
- Create = `issueCreate(teamId, title, description, stateId, projectId?, labelIds?, assigneeId?)`; when the assignee filter is unset, assign the viewer (parity with GitHub).
- Comments = `commentCreate` / `issue.comments`.
- Incremental refresh: `updatedAt: { gt: <cursor> }` + full reconcile every N minutes; `poll_changes` emits external moves like GitHub.
- Search: `searchIssues(term, filter)` scoped by the filters.
- PR linking: none in v1 (matches GitHub); Linear's GitHub integration can link via branch name, and Linear's `branchName` is surfaced in `issue show`.

### Surfaces
- CLI: `shelbi tracker linear login|status|sync|teams|projects|labels|users`; `issue add/list/show/move/...` work unchanged; `status`/`doctor` show Linear budget; `issue-store migrate` gains linear.
- Project creation: tracker picker gains Linear (desktop, TUI, CLI wizard): API key -> team (required) -> project/label/assignee (optional) pickers.
- Settings (desktop + TUI): Issue tracker section becomes editable: connect/switch tracker (one external integration), filter pickers; Statuses page read-only with a "Managed by Linear" banner and category-only editing.
- Docs: `site/content/docs/concepts/linear.mdx`, project.mdx table, CLI pages.

## Work breakdown (umbrella `linear` + subtasks)
- **linear (umbrella, foundation)**: config model + validation + exclusivity; `linear_client` (curl runner, auth, errors, rate-limit capture, test-support fake runner + fixtures); typed queries for viewer/teams/states/projects/labels/users; `shelbi tracker linear login|status|teams|projects|labels|users`.
- **linear-store**: `LinearStore: IssueStore` + build_store wiring + cache/board-index identity + ownership.
- **linear-statuses**: status catalog sync, `external_id`, read-only enforcement in WorkflowConfig ops, workflow validation, `tracker linear sync`.
- **linear-settings-desktop**: desktop project-creation + Settings tracker section + read-only statuses UI.
- **linear-settings-tui**: TUI + CLI wizard equivalents.
- **linear-daemon**: daemon refresh/governor, status/doctor, search, events, migrate, GitHub-specific messages generalised. (after linear-store)
- **linear-docs**: site docs. (after the rest)

## Deferred
OAuth app + webhooks (needs a public endpoint), Linear cycles/estimates sync, Jira.
