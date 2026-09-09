# GitHub issue caching and rate limits

## Context

The `github` issue-tracker backend was built for correctness first: every board read is a
full REST list (`GET /repos/{o}/{r}/issues?state=all&per_page=100 --paginate`), every
`list_in_status` is that same full list filtered in memory, and every `get` by id is a
label-filtered list. With ~590 issues on the shelbi board a single list is 6 pages, so 6
requests. Nothing looked at the rate-limit headers, nothing backed off on a 403, and every
process (sidebar, Issues TUI, daemon, each workspace poller thread, every CLI invocation)
read on its own.

What that cost on 2026-09-07 (measured, not estimated):

- The primary REST budget (5,000 requests per hour per token) was exhausted within 6 to 8
  minutes of every hourly reset, all evening. `ps` sampling showed two concurrent full
  lists from `shelbi __sidebar` in a two-second window and per-card label lookups from
  `shelbi __tasks`; the six workspace poller threads each run `list_in_status` on a
  five-second tick, which alone is roughly 6 threads × 720 ticks × 6 pages = 26,000
  requests per hour if nothing throttles them.
- While the budget was at zero every board operation failed: `issue list/show/start/
  resume/move`, `zen probe`, `zen pr-create`, `orchestrator events next`, dispatch, and the
  poller's own reads. Six finished branches took six hours to merge, one per window.
- Failed reads were treated as facts: the review slot was reaped as "orphaned", worker
  ready-markers were cleared as "unloadable" (five finished tasks stranded in
  `in_progress`), the heartbeat reported every workspace idle, and the sidebar rendered
  no review sections while the Issues board, seeded from the persisted snapshot, showed
  the same cards. Three of those are now fixed or filed; the root cause is this plan.

The process-local cache (#1188) and the persisted snapshot warm-up (#1192) reduced
duplicate reads inside one process, but they still fetch the whole history on every
refresh, they are per process not per hub, and the CLI, poller and several sidebar paths
bypass them by constructing a fresh store (`resolve_issue_store`) instead of using
`issue_store_for`.

Facts about the data model that shape the design (from `crates/shelbi-state/src/github_store.rs`):

- Identity is the `shelbi:id/<slug>` label (truncated to `<slug>-<hash8>` past GitHub's
  50-character label cap); status is the single `shelbi:status/<id>` label; a **closed**
  issue always maps to `done` or `canceled` (by `state_reason`).
- Priority, `prefers_machine`, `zen`, `launch` and other per-task fields live in the fenced
  `<!-- shelbi:begin --> … <!-- shelbi:end -->` YAML block inside the body.
- `assigned_to` is never stored on GitHub; it is a local overlay file. Good: it costs no
  requests and this plan leaves it alone.
- `zen.rs` already talks to GitHub through `gh api graphql` for PR provenance, so a GraphQL
  path is not new plumbing.

## Goals

1. An open project with six workspaces, a sidebar and an Issues board idles at a few
   requests per minute, not ten per second.
2. The board (columns, titles, tags, order) is always renderable from local state; a
   GitHub outage or an exhausted budget degrades to "stale, last refreshed HH:MM", never to
   an empty board or a wrong decision.
3. Anything that *acts* on an issue (show, start, resume, move, review load, probe, merge)
   works from a freshly fetched copy of that one issue, so we never operate on a stale
   body or status.
4. One reader per hub, not one per process or per thread.

Non-goals: changing the label/body encoding of issues (a separate plan if we ever want
priority as a label), webhooks, and anything about the `file_system` backend.

## Design

### 1. Two tiers of issue data

**Board index** — what the board, sidebar, pollers and orchestrator need to render and
route: `number`, `id` (from the id label), `title`, `labels` (status + tags), `state`,
`updatedAt`, plus the fenced metadata block (priority etc.). Covers every issue that is
**not** `done`/`canceled`, which on GitHub means every **open** issue. The done column is
history: it is loaded on demand (see §4), never on the refresh cadence.

**Full issue** — everything above plus the whole body, and fetched **for one issue** at the
moment we act on it. Kept in a small per-number cache keyed by `updatedAt`, invalidated
whenever the index shows a newer `updatedAt` for that number.

The index is small (tens of open issues), changes rarely, and is exactly what the
five-second consumers need. The full copy is fetched at human or dispatch cadence, which
is tens of times an hour at most.

### 2. Index fetch: one GraphQL query, incremental after the first

Initial (cold) fetch of all open issues:

```graphql
query BoardIndex($owner: String!, $name: String!, $after: String) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    issues(states: [OPEN], first: 100, after: $after,
           orderBy: { field: UPDATED_AT, direction: DESC }) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state updatedAt body
        labels(first: 10) { nodes { name } }
      }
    }
  }
}
```

Incremental refresh (every tick after that) adds `filterBy: { since: $lastRefresh }` and
drops the `states` filter, so it returns only issues touched since the last successful
refresh, **including ones that were just closed**, which is how done/canceled
transitions reach the index without ever listing history. A quiet board returns zero
nodes.

Why GraphQL rather than REST here:

- We pick the fields. `body` costs bandwidth, not quota (GraphQL points are charged per
  connection requested, not per field), so carrying the metadata block in the index is
  free, and the board can still order by priority.
- It is a separate 5,000-point-per-hour budget from the REST calls that writes still use.
- One page of 100 issues with `labels(first: 10)` costs about 11 points; an incremental
  refresh with no changes costs 1. At a 30-second tick that is under 150 points per hour
  for a quiet board, versus 26,000 REST requests today.
- `rateLimit { remaining resetAt }` rides along in every response for free, which is what
  the governor in §6 reads.

Pull requests never appear because `repository.issues` excludes them; the `is_pull_request`
filter goes away.

### 3. Fresh single-issue fetch on every action

`get(id)` becomes: resolve `id` → `number` through the index (or through the id-label
search below when the id is not in the index, e.g. a done task), then fetch that issue
alone:

```graphql
query Issue($owner: String!, $name: String!, $number: Int!) {
  rateLimit { remaining resetAt }
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      number title state stateReason updatedAt body
      labels(first: 10) { nodes { name } }
    }
  }
}
```

One point. Callers that must see the latest version, and therefore always take this path:
`issue show`, `issue start`/`resume` (the prompt body), `issue move`/`edit`/`prio`
(read-modify-write on the metadata block), review-slot load, `zen probe`, the ready-marker
handoff, and the `review -> done` merge transition. Several ids at once (the orchestrator
drain, `zen scan`) use one query with aliases (`a: issue(number: 1) {...} b: issue(number:
2) {...}`), still one point each and one round trip.

Unknown id (not in the open index): `search(query: "repo:o/r label:\"shelbi:id/<label>\"",
type: ISSUE, first: 2)` — one point — replaces today's label-filtered REST list.

Reads through this path also refresh that issue's entry in the index, so a `move` you
just made is visible to the sidebar on its next paint without waiting for the tick.

### 4. Done and canceled on demand

The Issues board shows `done`; nothing else needs it. The Kanban loads the done column
lazily (first 50, "load more"), through the same GraphQL shape with `states: [CLOSED]`,
cached separately with a long TTL (10 minutes) and never on the poll cadence.
`issue list --status done` and the Zen "done-history" judgment read the same cached page.

### 5. One reader per hub: the daemon owns the refresh

Today the cache lives in each process. The plan makes `shelbi daemon` the only process
that runs the index refresh for an open project:

- The daemon runs one refresh loop per open project (tick from §6), writes the index to
  `<project_dir>/board-index.json` atomically (temp file + rename, same as the snapshot
  today), and appends a `board refreshed=<ts> cost=<n> remaining=<n>` line to events.log
  only when something changed.
- Sidebar, Issues board, workspace pollers, `events next`, `zen scan`, `issue list` and
  every other **list** consumer read the file (mtime is the freshness signal). They never
  call GitHub for a list. The in-process cache becomes a read-through of that file.
- A consumer that needs "fresher than the file" (a user pressing refresh, a CLI command
  that is about to act) asks the daemon over the hub socket for `refresh-board <project>`
  and waits up to two seconds for the mtime to move, or takes the single-issue path from
  §3, which is cheaper and more precise.
- Writes (`add`, `move_status`, `set_fields`, `set_priority`, `cancel`, `add_comment`)
  stay REST through `gh api`, and every write **writes through**: the response body is
  the updated issue, so the caller patches `board-index.json` (and the full-issue cache)
  immediately instead of triggering a refresh. The next incremental refresh confirms it.

With no daemon running (a bare CLI on a machine with no hub), the CLI falls back to the
in-process cache plus the persisted file, exactly as today, so behaviour is unchanged for
scripts.

### 6. Budget governor and outage behaviour

Every response carries the numbers we need: `rateLimit { remaining resetAt cost }` in
GraphQL, `x-ratelimit-remaining` / `x-ratelimit-reset` in REST (run `gh api` with
`--include` and parse the headers; today `run_gh` discards them). The daemon keeps a
per-token `budget.json` and applies:

| GraphQL remaining | index tick | single-issue fetches |
| --- | --- | --- |
| > 2,000 | 30 s | unlimited |
| 500 – 2,000 | 120 s | unlimited |
| 100 – 500 | paused until `resetAt` | user-initiated only |
| < 100 | paused | served from cache, warning printed |

REST (writes) gets the same reserve: below 100 remaining a write is refused with a clear
message naming `resetAt` instead of failing on a 403 deep inside a transition.

Any 403 rate-limit or 429 response parks that budget until its reset time, once, with
one events.log line; there is no retry loop. `gh_retry` (#1204) keeps its role for
secondary limits on bulk writes, with the read policy failing fast as it does now.

Staleness is a first-class state, rendered not hidden: the sidebar footer and Issues
board header show `board 4m stale · quota resets 16:03` whenever the index is older than
two ticks. And the rule that already covers the dev-slot reaper becomes global: **nothing
destructive (reap, orphan, clear a marker, mark idle, auto-load) acts on a stale or failed
read**; those paths need a `BoardState::Warm` index or they skip the tick.

### 7. Request inventory, before and after

Per hour, one open project, six workspaces, sidebar and Issues board open, nobody typing:

| Reader | Today (REST requests) | After (GraphQL points / REST requests) |
| --- | --- | --- |
| Workspace pollers, 6 threads at 5 s | ~26,000 (full list each tick) | 0 (read `board-index.json`) |
| Sidebar tick | ~2,000 | 0 |
| Issues board | full list + per-card label lookups | 0 (+ one done-page on open) |
| Daemon index refresh at 30 s | n/a | ~120 points (1 per quiet tick) |
| `events next` drain | full list per call | 0 |
| **Idle total** | budget gone in ~7 min | ~120 points, 0 REST |
| `issue show` / `start` / `move` / probe / merge | 6–12 requests each | 1 point + the write itself |

## Decisions

- Reads move to GraphQL; writes stay REST via `gh api`. Two budgets, and the write path
  is untouched by this plan.
- The index carries `body` because GraphQL prices connections, not fields; that keeps
  priority ordering without changing how issues are encoded. If bandwidth ever matters,
  the fallback is a `shelbi:prio/<n>` label, which is a separate plan.
- The daemon is the single list reader. Processes without a daemon keep today's
  per-process cache so nothing regresses for scripts.
- Done/canceled are loaded on demand and never on the tick.
- Anything that acts on an issue fetches that issue fresh first. The index is for rendering
  and routing only.
- Stale is a displayed state, not an error, and destructive poller actions require a warm
  index.

## Open questions

- Label count: `labels(first: 10)` is enough for `shelbi:id`, `shelbi:status` and a
  handful of tags; confirm no board relies on more, or raise to 20 (cost 21 points per
  cold page, still fine).
- GHES: GraphQL is available on Enterprise Server, but `filterBy.since` and `rateLimit`
  need a version check on first connect; fall back to REST `since=` polling if absent.
- Several projects on one token share the budget; the governor should be per token,
  hub-wide, not per project.
- `search` for an unknown id is eventually consistent (label changes take seconds to be
  searchable); `add` followed by an immediate `get` should use the number the create
  returned, not the search.

## First slice

Phase 0 — stop the bleeding (one PR, no new plumbing):
- `list_in_status` for non-terminal statuses uses `state=open`; only the Kanban's done
  column and `issue list --status done|canceled` request closed issues.
- Every poll and render path uses `issue_store_for` (cached); `resolve_issue_store` is
  reserved for construction. Covers the sidebar workspaces list, `assigned_review_task_for`,
  and the orchestrator drain.
- Park all reads until reset on the first 403 rate-limit response; log once.
- Acceptance: with the shelbi project open for an hour, `gh api rate_limit` shows core
  usage under 1,500; no `orphaned-*-reaped` or `ready marker names unloadable task` line
  during a forced 403.

Phase 1 — daemon-owned refresh and `board-index.json`:
- Daemon refresh loop, atomic index file, `refresh-board` hub message, consumers switched
  to the file, write-through on mutations.
- Acceptance: with six pollers, sidebar and Issues board running, `ps` never shows a
  `gh api` child of `__sidebar` or `__tasks`; all list traffic originates from the daemon.

Phase 2 — GraphQL index and single-issue fetch:
- `BoardIndex` cold + incremental queries; `Issue` and aliased multi-issue queries;
  id→number map; `search` fallback; done column on demand.
- Acceptance: idle project stays under 200 GraphQL points per hour; `issue show` after an
  out-of-band GitHub edit prints the edit; a `move` appears in the sidebar within one tick
  without a full refresh.

Phase 3 — governor and staleness UI:
- `budget.json`, adaptive tick, reserve floors, stale banners, `shelbi status` and `shelbi
  doctor` report requests per hour and time to reset, fake-`gh` tests covering 403, 429,
  and `resetAt` parking.
- Acceptance: with the budget artificially set to 50, the board keeps rendering, marked
  stale, no destructive poller action fires, and every refused write names the reset time.
