# GitHub Issues: reliable synchronization with fewer moving parts

Status: proposed implementation plan  
Reviewed: 2026-09-09  
Code baseline: [Shelbi b66b3b2](https://github.com/jlong/shelbi/tree/b66b3b2f2684fb2120f8bc0878858327b150dfa8)  
Scope: GitHub Issues integration, including board reads, mutations, comments, migration, credentials, and shared API governance. No application code was changed during this review.

This plan builds on [[github-issue-caching-and-rate-limits]] and [[pluggable-task-stores]]. It supersedes their implementation guidance where specified below. Their historical incident measurements are useful context, but are not measurements of the current checkout.

## Recommendation

Finish the move to **one hub-owned issue service**, with one synchronization loop, one publisher of local issue state, and one mutation queue. Use a **long-lived pooled HTTP client** for GitHub requests. Keep `gh` for login and credential discovery, and keep existing PR/git commands outside this change.

Retain GraphQL for open-board snapshots, incremental updates, and bounded batch reads. Use REST for issue mutations, comments, and conditional single-issue verification. Both run through the same HTTP client and governor. The largest improvement comes from eliminating duplicate work and unsafe state transitions. Removing repeated CLI startup is an additional, measurable improvement.

Keep the current file-based storage approach. Do not introduce a database, remote service, generalized provider framework, or webhook hosting requirement to achieve this. Keep user-customized issue metadata, existing task IDs, local workspace assignments, and history-on-demand.

## 1. What the current implementation gets right

The current code is substantially beyond the original live-REST implementation:

- The daemon already publishes an open-board index; ordinary sidebar and board rendering can read it without GitHub calls.
- GraphQL deltas include both open and recently closed issues. Full closed history is normally loaded on demand.
- Read failures can preserve the last board and explicitly report stale data. Several destructive poller decisions already require a warm board.
- Assignment is an inexpensive local overlay, separate from GitHub timestamps.
- Batched issue reads, response budget observations, outage parking, atomic file replacement, and change-only event logging are useful foundations.
- The metadata serializer preserves unknown valid YAML keys, and credentials have redacted types and an out-of-repository storage option.

Preserve those properties while removing the overlapping mechanisms below.

## 2. Findings from this checkout

Paths and line numbers refer to the baseline above. These are code-review findings; the production frequency of each race was not measured.

| Priority | Finding and user impact | Evidence |
| --- | --- | --- |
| P0 | An action read can return an indefinitely stale issue. The full-issue cache accepts an equally old index as proof of freshness without checking its age or stale flag. Successful writes can subsequently publish that old cached issue through the wrapper's extra `get`. | `crates/shelbi-state/src/github_store.rs:1419-1458`; `issue_cache.rs:383-400,487-499` |
| P0 | Snapshot publication has multiple writers. CLI patches, daemon refreshes, history patches, and budget updates use read-modify-replace sequences without a shared cross-process transaction. A valid JSON file can still lose another writer's update. | `board_index.rs:656-729`; `crates/shelbi-cli/src/commands/daemon/board.rs:189-222`; `gh_budget.rs:253-259,355-368`; `done_history.rs:151-172` |
| P0 | Every mutating request receives the same retry policy, including issue/comment creation. A timeout or 5xx after GitHub accepts a POST can produce duplicates when retried. Multi-step operations have no durable recovery record. | `github_store.rs:325-328,824-886,1243-1254`; `gh_retry.rs:164-198` |
| P0 | A status move sets labels before setting GitHub state. If closing fails, an open issue can render as done. Repeating the move can return early because the label already matches. Reopening on GitHub with a stale terminal label also retains terminal interpretation. | `github_store.rs:913-936,2973-2987` |
| P0 | Malformed fenced metadata is stripped and silently replaced with defaults on parse failure. A later priority/edit operation can destroy workflow, dependency, or other settings. | `github_store.rs:3236-3255,1821-1833` |
| P1 | GitHub writes and comment operations resolve IDs through a label-filtered REST list, selecting the first match. Native issues displayed under their number cannot follow that write path without an identity label; duplicate aliases are silently ambiguous. | `github_store.rs:1259-1274` |
| P1 | Two synchronization systems remain. Mutation invalidation starts another full open-board REST refresh; the sidebar separately lists issue deltas and then comments per touched issue. Sidebar reconciliation depends on heartbeat and loses its baseline on restart. | `issue_cache.rs:354-356,503-605`; `crates/shelbi-tui/src/poller.rs:183-190,712-719,806-878` |
| P1 | Incremental state drifts. Rows are merged by mutable Shelbi ID, deletions/transfers produce no removal delta, and no periodic full reconciliation is scheduled. The persisted index has no repository/configuration identity, so changing a project's tracker can reuse the previous repository's rows and cursor. | `github_store.rs:656-675`; `daemon/board.rs:196-207`; `board_index.rs:61-102` |
| P1 | All issue queries truncate labels at ten without checking whether more exist. A missing identity/status label can change task interpretation. A missing next cursor despite `hasNextPage=true` is accepted as completion. | `github_store.rs:2432-2536,1388-1398` |
| P1 | The event poller filters strictly newer timestamps, which can miss a second change in the same timestamp unit. It can also advance its cursor after an event append fails. | `github_store.rs:1199-1221`; `poller.rs:747-749,872-878` |
| P1 | Periodic refreshes run sequentially across projects; manual refresh waiters each issue another refresh. Credential-key memoization can retain success or failure indefinitely. Deliberate 120-second budget throttling conflicts with the default 90-second stale threshold. | `daemon/board.rs:83-94,285-341,382-390`; `gh_budget.rs:455-460`; `board_index.rs:290,317-318` |
| P2 | Each add/move lists repository labels again. One identity label per task makes this list grow with lifetime issue count. Dense priority renumbering performs a GET and PATCH for each changed task. CLI ID allocation still consults local task files for a GitHub-backed project. | `github_store.rs:824-886,946-975,1821-1870`; `crates/shelbi-cli/src/commands/issue.rs:463-476,2305-2316` |

Important clarification: mutation and stale-index helpers preserve `fetched_at`; they do not advance the daemon's watermark. The current daemon also records its timestamp before fetching, which is good. The problem is competing publication, clock assumptions, incomplete reconciliation, and overloaded freshness semantics.

## 3. GitHub API research and implications

### API choice

| Work | Recommended API | Why |
| --- | --- | --- |
| Open-board bootstrap and incremental synchronization | GraphQL `repository.issues`, paginated | Existing efficient shape, issue-only connection, selectable fields, separate primary budget. |
| Several known issues needed together | Bounded GraphQL aliases or `nodes` by recorded node ID | One round trip; enforce batch and label completeness limits. |
| One issue immediately before an action | REST GET by number with an exact cached ETag, or fresh GraphQL when batching | A valid 304 remotely verifies the cached representation; an old board cannot do so. |
| Create/edit/move/close | REST POST/PATCH by native identity | Return the canonical issue; combine compatible fields in one request. |
| External comments | Repository-wide REST comments delta | Avoid fetching comments separately for every touched issue. |
| Closed history | Existing bounded GraphQL pages on demand | Avoid scanning historical cards on the refresh cadence. |
| Missing legacy slug mapping | Exact repository label query, requiring one unambiguous issue | Keep search out of normal identity resolution; never assume a search miss proves absence. |

REST's repository issues endpoint supports state, updated-time filtering, and pages up to 100, and also returns pull requests. Issue PATCH can combine body, labels, state, and state reason. Replacing labels replaces the set; insufficient permissions can silently drop label changes, so validate the returned result. These are reasons to retain an explicit issue adapter rather than expose raw HTTP throughout callers. [GitHub REST issues](https://docs.github.com/en/rest/issues/issues)

GraphQL `IssueFilters.since` is documented as inclusive. Query all states for deltas, and request native node identity. The schema's `clientMutationId` is described as client identification, not a documented idempotency guarantee; changing writes to GraphQL would not establish exactly-once creation. [GitHub GraphQL issues schema](https://docs.github.com/en/graphql/reference/issues#issuefilters)

### Budgets, caching, and concurrency

Authenticated REST normally provides 5,000 requests/hour for a user; multiple credentials acting as the same user do not imply independent quota. GraphQL has a separate primary points budget. Secondary restrictions include shared REST/GraphQL concurrency and content generation, generally 80 content-generating requests/minute and 500/hour, with potentially lower or undisclosed limits. A 7.5-second delay between migrated *issues* is insufficient when each issue generates several writes. Observe actual response headers; do not poll `/rate_limit` routinely. [GitHub REST rate limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)

Correct two assumptions in the older caching plan: 100 issues with one labels connection per issue is roughly 101 connection requests, normalized to about **one** primary GraphQL point, not eleven. Increasing labels from ten to twenty does not imply 21 points. Aliases likewise do not have a fixed one-point-per-issue price. These are formula-based estimates; record returned `rateLimit.cost`. Payload bytes, nested nodes, server compute, and latency still matter even when point cost stays low. A body is not free in those dimensions. GraphQL can return partial data/errors and has node and execution limits. [GitHub GraphQL limits](https://docs.github.com/en/graphql/overview/rate-limits-and-query-limits-for-the-graphql-api)

GitHub recommends serial requests, paced bulk writes, webhooks where practical, and stable conditional GETs. Properly authenticated 304 responses save primary quota; they still involve a network request. ETags belong to the exact URL/query/representation: changing `since` invalidates reuse, and page one's 304 does not verify other pages. Conditional unsafe methods are unsupported unless the endpoint explicitly documents them, so do not promise issue PATCH conflict prevention with `If-Match`. [GitHub API best practices](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api)

Repository comments support `since`, sorting, and pagination. They include issue and PR comments: match returned `issue_url` against recorded issue URLs, and resolve unknown parents through bounded lookups. Track comment IDs/versions independently, including edited comments; do not derive comment completeness solely from issue timestamps. [GitHub issue comments](https://docs.github.com/en/rest/issues/comments#list-issue-comments-for-a-repository)

### Alternatives considered

- **REST-only with ETags:** potentially the cheapest idle primary-quota option for a small, stable one-page board. It still needs correct per-page caching, PR filtering, durable deltas, and reconciliation. It consumes the same primary budget as writes when responses change. Retain it as a capability-selected compatibility implementation, rather than replacing the already useful GraphQL board path on speculation.
- **GraphQL-only:** removes a wire format but introduces label node-ID provisioning and different mutation semantics without fixing synchronization, concurrent edits, or ambiguous POST results. Not the simplest migration.
- **Webhooks/GitHub App:** attractive for a future shared hosted hub, especially at many repositories. The `issues` and `issue_comment` events provide relevant invalidations. A local laptop would additionally need reachable delivery infrastructure, installation setup, and delivery recovery. Do not make that a prerequisite. A later webhook adapter should wake the same reconciler, with polling/reconciliation retained for missed delivery. [GitHub webhook events](https://docs.github.com/en/webhooks/webhook-events-and-payloads#issues)
- **More caches or more parallel requests:** do not address ownership or missing-event bugs. Eliminate redundant requests first.

## 4. Is `gh` the right transport?

It is a good interactive and bootstrap dependency. It is a poor default transport for a frequently running service when each operation starts a new process.

The current `run_gh_with_token` waits on `Command::output()` without a Shelbi deadline (`github_store.rs:1924-1952`). Credential resolution can additionally spawn `gh auth token` for requests when environment credentials are absent. GraphQL pagination invokes a separate `gh` for each page, while REST `gh --paginate` follows pages within a single invocation. Do not conflate those cases.

`gh api` already supports pagination, custom headers, response headers, host selection, and an explicit response-cache duration. It is capable of better behavior than Shelbi currently extracts from it. However, a duration cache is not authoritative freshness, and repeated CLI invocations cannot share an in-memory HTTP connection pool. [GitHub CLI API manual](https://cli.github.com/manual/gh_api)

Use a thin typed GitHub adapter over one reused `reqwest::Client`; its documented design includes connection pooling. Select a version and dependency lock that actually build with Shelbi's Rust 1.80 minimum. Reuse the existing Tokio runtime or a bounded I/O worker boundary; do not turn every filesystem store call async just to change transport. [reqwest Client](https://docs.rs/reqwest/latest/reqwest/struct.Client.html)

Implement typed responses carrying HTTP status, headers, request ID, body, GraphQL errors, and rate observations. This removes parsing of human CLI stderr, JSON-to-JSONL conversion, method inference from CLI flags, repeated auth subprocesses, and duplicate request/probe bookkeeping. Keep one explicit replay policy owned by the service; disable conflicting client middleware retries.

### Scratch experiment

On macOS arm64 with gh 2.96.0, a loopback HTTP server returned 100 issue-shaped records (47,577 bytes) per page. Both clients consumed and parsed the complete JSON. Single-page cases had 60 samples; ten-page cases had 16, after three warmups.

| Injected delay/request | Pages/operation | Client | Median / p95 milliseconds |
| --- | --- | --- | --- |
| 0 ms | 1 | gh with Shelbi's `--jq .[]` format | 22.979 / 27.690 |
| 0 ms | 1 | Pooled Python HTTP control | 0.218 / 0.242 |
| 0 ms | 10 | gh `--paginate --jq .[]` | 34.637 / 35.472 |
| 0 ms | 10 | Pooled Python HTTP control | 2.300 / 2.426 |
| 25 ms | 1 | gh with `--jq .[]` | 51.664 / 53.455 |
| 25 ms | 1 | Pooled Python HTTP control | 30.032 / 30.504 |
| 25 ms | 10 | gh `--paginate --jq .[]` | 337.050 / 345.651 |
| 25 ms | 10 | Pooled Python HTTP control | 295.370 / 300.379 |

Separate gh invocations opened separate connections. Each ten-page invocation reused one connection; the pooled control reused its warmed connection across operations. This agrees with the [gh pagination implementation](https://github.com/cli/cli/blob/v2.96.0/pkg/cmd/api/api.go#L366-L419). The 16-sample p95 is the maximum, so these exploratory percentiles are not stable service targets.

All 1,898 requests were local and verified dummy authentication. No live GitHub requests, real credentials, or repository edits were involved. The companion [[github-issues-transport-benchmark]] preserves methodology, results, and the runnable script in the Shelbi Research folder.

The experiment isolates local overhead. It does not measure GitHub service latency, real TLS, SSH links, rate limits, or a production Rust implementation. Request-count reduction remains the primary performance goal; no fixed production speedup is promised.

## 5. Target architecture

```text
Sidebar / Issues board / pollers
             |
        local snapshot
             ^
             |
CLI / orchestrator -- existing hub socket --> Issue service
                                               |
                        one publisher + per-repository operation queue
                                               |
                              pooled HTTP + shared rate governor
                                               |
                                    GitHub REST / GraphQL
```

Keep this within the current crates initially:

1. **Domain adapter:** GitHub identity, metadata parsing, status mapping, and domain validation. Transport-neutral structures and errors.
2. **GitHub client:** authentication provider, typed HTTP requests/responses, bounded pagination, and endpoint/query shapes.
3. **Issue service:** scheduling, serialization, synchronization cursors, issue/history projection, and mutation recovery.
4. **Snapshot reader:** local state and freshness only. It has no path that secretly refreshes GitHub during a render.

The existing `IssueStore` can remain a caller-facing facade during migration. Introduce explicit semantics for snapshot reads, remotely verified reads, commands, and paged history. Remove claims that a remote write plus a local assignment update is an atomic filesystem-style operation.

### Ownership without requiring a running hub for scripts

When the daemon is present, it owns synchronization and writes for that repository/account context. Callers submit commands or join refresh work over the existing socket. Carry the same stable command ID across socket retries; a lost acknowledgment must return/reconcile the original result instead of creating another intent. Manual refreshes join the in-flight result; they do not queue repeated identical reads.

When no daemon owns the context, a standalone command acquires the same advisory ownership lock and runs the same engine once, then exits. A socket timeout is not proof that ownership is free. If the owner still holds the lock, return a bounded unavailable/busy response instead of starting a competing writer. Reads can always show a matching persisted snapshot with its age.

Share remote records for projects using the same host/repository/credential context where practical, then apply project-specific assignments and workflow interpretation locally. Keep the first implementation narrow: ownership must be correct before adding broader deduplication.

Serialize mutations per issue/repository. Run network work outside the daemon's supervision/event loop. Use fair scheduling across projects and request deadlines, normally one in-flight request per authenticated principal; honor server limits rather than increasing concurrency to disguise redundant work.

### One durable representation

Use one versioned remote-state file per owned context, atomically replaced by its owner. It contains native issue records, the open index, bounded cached history, alias mappings, synchronization checkpoints, and pending event delivery. Assignment markers remain project-local, but bind them to remote context and immutable issue identity so an alias change or repository retarget cannot attach an old assignment to a different card.

Persist:

- Schema version, canonical host/API base, repository node ID, configuration identity, and non-secret credential/principal context.
- Remote issue node ID, current repository/number/URLs, legacy Shelbi alias, parsed fields, raw body, and observed version/hash.
- Separate `sync_cursor`, `last_successful_sync`, `published_at`, `generation`, `next_due`, and `last_full_reconcile`.
- Pending domain events with stable event IDs, and only the small mutation records needed for crash/ambiguous-result recovery.

Publication from a successful mutation changes generation immediately without pretending the whole repository was just synchronized. On identity mismatch, bootstrap the new repository instead of carrying rows/cursors forward. Credential changes revalidate access and budgets; never show another account's cached private data as a successful current read.

The governor remains host/principal-wide, not repository-wide. Multiple user tokens may share quota; key by discovered user or installation identity when available, with a credential fingerprint only as provisional identity. Standalone updates use a cross-process lock. No raw token is persisted in these records. Admission guarantees initially cover migrated issue traffic; existing PR/CI commands and external clients still spend shared quota, so observe remaining budgets and retain a reserve. Legacy Shelbi PR/CI calls can later join admission control without requiring a simultaneous transport rewrite.

## 6. Synchronization algorithm

1. **Bootstrap:** fetch all open issues, with complete required labels and metadata, into a temporary candidate snapshot. Establish a conservative scan-start watermark. Do not publish a partial board as complete.
2. **Incremental pass:** query all states from an overlapping successful checkpoint. Merge by native issue identity, not slug. Closing removes from open membership and updates cached history; alias changes update the alias map without leaving ghost cards.
3. **Pagination:** follow every cursor/Link; reject missing, repeated, or non-advancing cursors and partial GraphQL errors. Bound page count, bytes, and total elapsed time. Do not advance the checkpoint on incomplete work.
4. **Timestamp handling:** use a conservative server-time bound informed by HTTP Date and request duration, with an initial two-minute overlap. Do not depend on the hub clock being correct. If server time is unavailable, retain a safe observed checkpoint rather than inventing progress. Deduplicate repeated observations using native ID plus timestamp and content hash; equal timestamps can still contain different content.
5. **Commit:** persist updated records, cursor, and pending events together. Deliver pending events with stable IDs and acknowledge separately, so restart cannot silently discard an observed status/comment event. Delivery is at least once: consumers must deduplicate event IDs or be idempotent after an append-success/acknowledgment-crash. Polling converges on observed state; it cannot promise to capture every intermediate close/reopen between polls. This is bounded delivery bookkeeping, not a general event-sourcing system.
6. **Comments:** use a separate durable repository-comments cursor with overlap and ID/version deduplication. Emit new-comment versus edited-comment events deliberately. Select an initial baseline at first enablement without replaying all historical comments; resume the saved cursor after restart.
7. **Reconcile:** every ten minutes while active, and after long disconnection, scan the full open set and reconcile known IDs absent from it. Verify missing rows before destructive routing decisions, distinguishing closed/transferred/deleted from lost permissions. Full-set publication is all-or-nothing. This bounds drift from deletion and moving pagination; it is not a claim of a transactionally consistent GitHub list.
8. **History:** keep explicit page completeness and cursor state. A successful close appears immediately in the loaded history projection. A paged terminal list must never masquerade as an exhaustive dependency or migration scan.

Use a common issue selection fragment. Fetch enough labels for the common case, always include `pageInfo`, and fetch overflow before interpreting a card. Merely changing ten to twenty leaves the bug intact. Preserve raw bodies while metadata is stored there; separate rendered summaries from raw records to avoid copying full prose through every UI structure.

Start with 30-second active synchronization, 120 seconds after sustained inactivity, and immediate targeted verification for actions. Pausing or slowing for quota is explicit service state. Derive display freshness from last successful sync and the effective schedule; keep a separate action-specific freshness requirement so slower scheduling never licenses risky stale decisions.

A primary GraphQL quota failure pauses that tier. A secondary limit or network breaker governs the appropriate shared host/principal traffic. Unsupported schema capabilities select a tested REST incremental implementation once per host/capability epoch. Authentication, parsing, and ordinary network errors do not trigger a full REST sweep on every tick.

## 7. Mutation semantics

### Fresh read, minimal write, canonical result

Each command resolves native identity, obtains a remotely verified version when needed, validates domain preconditions, and writes only intended fields. Return a typed result containing the canonical remote issue and local completion state.

A normal status move sends one PATCH with the desired status-label set and corresponding state/reason. Skip only if every relevant remote field already agrees. Interpret a natively reopened issue with an old terminal label as open and needing status reconciliation, rather than leaving it done. Compare the returned labels/state with the requested result so silent permission omissions cannot look successful.

Apply the returned issue to the owned snapshot before reporting completion. Eliminate post-write GETs and whole-board invalidation. Before transmitting any command that couples a remote write with assignment, parking, or event work, durably record its identity, desired remote fields, intended local changes, and completion stage. This includes PATCH operations, not just creation. If the remote write succeeds but local assignment/event publication fails or the process crashes, recover the pending local work and report that distinction.

### Ambiguous results and concurrent authors

Persist a small operation ID and intent before issue/comment creation. Include a durable operation marker in the created content where appropriate. After a lost response, reconcile recent repository issues or the target's comments by that marker; do not blindly repeat POST. An inconclusive result stays `unknown`, with an inspect/reconcile action. No exactly-once promise is made for GitHub or for separate uncoordinated hubs.

Replay safe reads with bounded backoff. Re-evaluate desired-state writes against current remote state before retrying. Definite throttling follows the server's retry/reset information; an ambiguous transport failure is a different outcome.

Serialize Shelbi writes locally, but acknowledge that users can edit on GitHub concurrently. For body edits, preserve the raw original, compare the current body with the edit base, and rebase only the owned metadata block. Malformed/duplicate fences produce an explicit parse/conflict result and a preserved draft, never default replacement. A final GET/PATCH race with an external writer remains possible because GitHub does not offer documented issue-PATCH compare-and-swap. Where stronger protection matters, prefer a native comment for feedback or require explicit conflict resolution.

Preserve unrelated labels. A single PATCH minimizes round trips and intermediate status/state mismatch, but replacing an entire label array can race external label edits. If such races prove common, targeted add/remove label endpoints trade more requests for less unrelated-label replacement; neither creates a cross-request transaction. [GitHub label endpoints](https://docs.github.com/en/rest/issues/labels)

### Remove expensive filesystem assumptions

- Make native GitHub identity the internal key immediately. Keep existing Shelbi slugs as aliases and resolve native numbers for unlabelled issues.
- Allocate slugs through the selected backend, not local markdown files; reject duplicate aliases explicitly. Preserve known aliases for closed tasks independently of cached history pages. An open-board map cannot prove global uniqueness: use collision-resistant generated aliases or native-number-derived aliases, verify explicit legacy aliases remotely, and surface cross-hub conflicts rather than promising globally serialized allocation.
- Cache/provision only the finite status-label set once, refreshing after a missing-label error or config change. Use full pagination at 100 per page when discovery is needed.
- Stop automatically renumbering an entire remote column. Introduce a versioned sparse ordering key, with deterministic ties and occasional explicit compaction. Preserve legacy `priority` reading and define `prio set N` as placement, not N body PATCHes. Gate the new representation on a documented minimum writer version or explicit repository opt-in: old writers ignore ranks and can still renumber priorities, so mixed versions cannot be assumed safe.
- Move dependency and workflow validation into shared command/domain logic. Validate required dependencies through explicit targeted reads, including closed issues, rather than assuming the first history page is complete.
- Make bulk migration resumable per source task and operation. Pace actual content-generating requests through the shared governor; persist the returned native ID immediately.
- Report actual deletion semantics. The GitHub implementation currently closes on `rm` while the CLI reports deletion (`github_store.rs:1083`; `issue.rs:2290`). Keep portable close/cancel behavior, name it accurately, and do not imply that a remote issue was hard-deleted.

Do not immediately remove identity labels from existing repositories. First ship readers for aliases in existing labels and metadata. A later schema phase can store all new aliases in the existing body metadata and stop creating one repository label per issue, after old-client compatibility is resolved. Never mass-delete user-visible labels or rewrite all old bodies merely to adopt the new service.

## 8. Credentials, deadlines, errors, and diagnostics

Keep the existing credential precedence for compatibility. Resolve credentials once per service credential epoch, cache only in memory, and refresh on explicit auth/config changes, token-file changes, a bounded refresh interval, or one 401 recovery attempt. Negative discovery has a short TTL. `gh auth token --hostname` supports explicit host selection. [GitHub CLI auth token](https://cli.github.com/manual/gh_auth_token)

The current GitHub config represents `owner/repo`, and this transport does not consistently pin the host. Make github.com explicit initially; do not imply GHES support from a REST fallback alone. Detect a non-default inherited host and require an explicit supported-host/configuration migration outcome rather than silently retargeting the same repository name to github.com. Add GHES host/API-base/auth/proxy/custom-CA handling only with a supported-version compatibility test. Never send credentials to an unrelated redirect host.

Pin a tested REST version. For the transport migration, explicitly retaining `2022-11-28` avoids combining transport and API behavior changes; GitHub currently documents support through March 10, 2028. Evaluate the newer `2026-03-10` separately. [GitHub API versions](https://docs.github.com/en/rest/about-the-rest-api/api-versions)

Initial deadline targets: three-second connect, fifteen-second request, and a bounded total sync job. The UI receives a two-second pending/queued response instead of blocking on long rate-limit waits; bulk migration can wait with visible progress. Tune these from real GHES/WAN evidence. Cancelled writes may still have been accepted remotely, so cancellation enters the same ambiguous-result handling.

Use typed outcomes: unauthenticated, permission denied, inaccessible/not found, rate limited with retry time, unavailable, invalid metadata, conflict, incomplete response, and unknown mutation result. A private-resource 404 is not definitive deletion. [GitHub REST troubleshooting](https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api)

Extend existing status/doctor output with request counts by endpoint and purpose, actual GraphQL cost, bytes, latency, retry count, last successful sync, next attempt, effective cadence, pending operations, and cache generation. Record one failure/recovery episode; count actual HTTP attempts/pages rather than only top-level CLI invocations. Keep credentials and issue bodies out of logs.

## 9. Performance budget and verification

These are analytical estimates and proposed acceptance targets, not production measurements.

Current default daemon behavior is approximately 120 GraphQL requests/hour/repository. Enabled sidebar reconciliation adds about 60 REST issue polls/hour while active, backing off through 60/120/240/300-second intervals to about 12/hour when settled and quiet (`poller.rs:727-744`). Ten touched issues per minute add about 600 comment-page requests/hour, yielding 660 REST requests/hour in that active example before writes and retries. Each independently completed mutation-triggered refresh adds `P_open` REST pages. Standalone per-process caching can add approximately `180 × P_open` requests/hour/process at its 20-second TTL. Startup reconciliation scans full issue/PR history.

For the proposed service let `P_open = max(1, ceil(open_issues/100))`, assuming complete labels fit the selection and a quiet delta fits one page:

| Scenario | Target network work |
| --- | --- |
| Sidebar + board + six workspace pollers | Zero additional GitHub requests from rendering/poller processes. |
| Active repository at 30-second cadence | About 120 GraphQL delta requests/hour + `6 × P_open` reconciliation pages/hour. |
| Quiet repository at 120-second cadence | About 30 delta requests/hour + `6 × P_open` reconciliation pages/hour, after settling. |
| Comment monitoring at 60 seconds active / 300 seconds idle | About 60 / 12 REST delta requests/hour, plus actual overflow pages and unknown-parent resolution; no per-touched-issue fan-out. Retain today's quiet comment latency unless faster monitoring is explicitly requested. |
| Known-issue status move with provisioned labels | One remotely verified GET and one PATCH; zero list/search/label-discovery calls. |
| Nonterminal create with known alias/order/status state | One issue POST in the later alias schema; initial compatibility mode may add one identity-label POST. Terminal creation/import adds a close PATCH in either mode. |
| Normal reorder after sparse-order migration | One target update with bounded neighbor verification; no full-column rewrite. |
| Twenty concurrent refresh requests | One synchronization job whose result all callers share. |

GraphQL points are measured separately from request counts. Across repositories, these totals add up; reserve foreground write/dispatch budget and adapt idle cadence. Closed history, startup, large deltas, migrations, and PR/CI activity are additional explicitly attributed work.

Acceptance requires reproducible tests of:

- A successful write racing a sync and two competing CLI processes: no lost snapshot, cursor, budget park, or assignment result.
- Stale index plus a remote edit immediately before dispatch/move: fresh verification is mandatory; offline mode never authorizes the action.
- Creation accepted followed by connection loss: no automatic duplicate issue/comment; recovery survives process restart.
- Failure at each boundary of reject/move/unassign: confirmed remote state is retained and remaining local work is repairable.
- Invalid metadata, duplicate aliases, native-number issues, GitHub reopen, and labels beyond ten and beyond one page.
- Equal timestamps, skewed host clock, pagination movement, partial GraphQL errors, bad cursors, deletion/transfer, access loss, repository retargeting, and interrupted bootstrap.
- Durable event delivery after append failure/restart, comment edits, unknown PR comment parents, and bounded duplicate replay.
- Correct 304 body reuse and auth/URL scoping; primary versus secondary limits; no API-family escape from shared throttling.
- One slow repository and concurrent manual refreshes: daemon supervision stays responsive and request counts remain bounded.
- Cached rendering remains available during an outage; a confirmed mutation appears in all open views on their next local paint, targeted within one second.
- A representative 1-hour multi-process workload, reporting requests, actual quota cost, p50/p95 latency, subprocesses, connections, bytes, and recovery behavior.

The eventual implementation should run relevant fixture/integration tests, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and an MSRV build. This review did not run application tests because it changed no application code.

## 10. Delivery sequence and deletion checklist

### Phase A: correctness fixes without a transport rewrite

- [ ] Make action reads explicitly remote-verified and publish canonical mutation responses.
- [ ] Stop blind replay of create/comment POSTs; distinguish unknown outcomes.
- [ ] Combine status/state updates and repair inconsistent reopen/close mappings.
- [ ] Reject malformed metadata and incomplete labels/pages.
- [ ] Support native identity on write paths and reject duplicate aliases.
- [ ] Add focused failure fixtures reproducing the P0 cases.

This phase can land using a bounded `gh` runner if that gets the fixes out sooner. It must not become a second permanent transport policy.

### Phase B: one owner and one synchronization path

- [ ] Introduce the shared service facade, ownership lock, socket commands, and standalone one-shot engine.
- [ ] Consolidate snapshot publication and versioned repository identity.
- [ ] Move sidebar issue/comment reconciliation into the owner; persist cursors and pending events.
- [ ] Add overlap, full reconciliation, effective freshness, and coalesced refresh.
- [ ] Disable every competing index/history/budget writer as soon as service ownership becomes authoritative. Delete mutation-triggered board sweeps, per-process remote refresh threads, and separate sidebar issue polling once their consumers migrate.
- [ ] Remove stale-cache-based action validation and redundant full-issue/global ID caches.

### Phase C: pooled transport and unified governance

- [ ] Replace `GhRunner` with a typed transport seam; port existing fake-response fixtures.
- [ ] Add in-memory credential epochs, explicit host, pinned API version, deadlines, headers, and typed errors.
- [ ] Unify read/write rate observations, secondary cooldown, and attempt-level diagnostics.
- [ ] Delete CLI stderr classification, JSONL plumbing, repeated credential resolution, and ordinary `/rate_limit` probes from issue traffic.
- [ ] Compare the same end-to-end workload before/after; retain rollback to the previous release without dual background pollers.

### Phase D: remove costly data-model assumptions

- [ ] Introduce sparse ordering and migrate readers before writers.
- [ ] Provision finite status labels once; stop identity-label creation only after alias schema compatibility is established.
- [ ] Fix backend-aware slug allocation and shared dependency validation.
- [ ] Make bulk migration resume from confirmed per-task results and shared content pacing.
- [ ] Remove redundant read-only legacy snapshot/history compatibility exports and document remaining compatibility boundaries; competing writers were already disabled in Phase B.

Phases B and C can be prepared in parallel behind narrow interfaces, but enable only one owner and one transport path at a time. Each phase has its own request-count and failure-recovery gate.

Preserve old aliases and unknown metadata; never rewrite existing customized prose automatically. If implementation changes shipped defaults, instructions, workflows, or config templates, pair them with the repository-required config-upgrade sniffer. Use AutoHeal only for deterministic non-lossy rewrites, and NeedsJudgment for ambiguous or customized prose. Version local cache data separately from user configuration.

The completion condition is fewer independent mechanisms: one owner, one synchronizer, one publisher, explicit freshness, and one transport policy. Better API use should make the architecture smaller as well as faster.


## Decisions recorded 2026-09-14

Direction from jlong on the thirteen questions in [[github-issues-robustness-and-performance-review]] §7. Items marked *(orchestrator)* were delegated with the instruction "pick the robust solution"; the pick and its reason are recorded so they can be reversed deliberately.

1. **Stale policy: "Render stale, never act stale."** Retires pluggable-task-stores D3 (dated note added there). The daemon index renders and may be marked stale; every action fetches the issue fresh first; destructive poller decisions require a warm index.
2. **Transport: keep `gh`.** Writes stay REST via `gh api`. Phase C (in-process HTTP client) is deferred behind the `GhRunner` seam with a measured trigger and its own plan.
3. **MSRV: raise `rust-version`** to what `Cargo.lock` already requires and add an MSRV CI job that builds with exactly that toolchain. Ships independently of this plan.
4. *(orchestrator)* **Write ownership: caller-side writes serialized by the existing cross-process file lock** (`acquire_file_lock`), never held across a network call; the daemon re-reads the index under the lock after its GraphQL call and merges its delta. No daemon-owned mutation socket and no "busy" refusal. Reason: a wedged daemon must not be able to block `shelbi issue move` (the 2026-09-08 launchd PATH incident would have), and scripts keep working without a daemon.
5. **Un-migrated native GitHub issues are actionable.** `get_raw` resolves by cached or index number first, accepts all-digit ids, and errors when more than one label matches.
6. **Hand-editing the fenced metadata block on github.com is supported.** Write paths refuse an unparseable block with a typed error; read paths render the issue with a visible warning instead of silently substituting defaults.
7. *(orchestrator)* **Reopen policy: the sync loop interprets and never writes.** An issue open on GitHub that carries a terminal status label renders in `backlog` and emits a reopened event. The stale label is repaired on the next Shelbi-initiated write to that issue. Reason: an autonomous write from the sync loop can fight a human who reopened to comment and re-close, and it spends write budget on a cosmetic repair; rendering in backlog forces re-triage without acting.
8. *(orchestrator)* **Terminal-to-terminal moves are one PATCH** with the status label as authority; GitHub's `state_reason` may lag and is excluded from verification when `state` is unchanged. Reason: a reopen-then-close pair creates a transient open state and a new half-done failure mode, which is the F4 hazard.
9. *(orchestrator)* **Action reads use a REST conditional GET** (`If-None-Match`, 304 exempt from the budget) on the write budget, and write paths publish the returned issue into cache and index. Board snapshots and deltas stay on GraphQL. REST GETs are logged so before-and-after counts exist. Reason: action reads leave the read budget entirely, so they can never again flap the board index stale as they did overnight on 2026-09-08. **Reverses** the caching plan's "reads move to GraphQL" for the single-issue action path only.
10. **State placement stays per project** under `~/.shelbi/projects/<name>/`. Several Shelbi projects on one repository is not a supported configuration.
11. **github.com only.** No GHES host key; GHES stays out of scope.
12. *(orchestrator)* **`events.log` keeps its exactly-once contract.** The emitter advances its cursor only after a successful append and scans the log tail before appending so a crash between append and cursor advance cannot duplicate a line. No event ids and no consumer-side dedup. Reason: producer and log are on the same machine, so exactly-once is achievable without changing the orchestrator drain or the TUIs.
13. *(orchestrator)* **Acceptance gates on the solo hub as it runs.** Correctness scenarios are proven with existing fixtures or a named harness; request counts are measured on the live `~/.shelbi/gh-requests.log` before and after. The modelled shared hub is a documented ceiling, not a gate. Reason: a gate that cannot be measured cannot be passed.

### Decisions reversed or reaffirmed by the above

- Retired: pluggable-task-stores D3 (no local cache, no stale data) by decision 1.
- Reversed: caching plan "reads move to GraphQL" for the single-issue action path only, by decision 9.
- Reaffirmed: "writes stay REST via `gh api`" (decision 2); "nothing regresses for scripts" and the daemon as a soft dependency (decision 4); per-project state placement (decision 10); "anything that acts on an issue fetches it fresh first" (decisions 1 and 9).
- Not adopted: Phase D schema changes (sparse ordering, backend-allocated aliases, identity-label removal), the ownership lock and socket command protocol, the durable operation journal, the pending-event queue, credential epochs, and the per-host state file. Each needs its own plan if revisited.

### Delivery shape adopted

Tier 1 from the review §6 is filed as small independent tasks on the `app` track on 2026-09-14, plus the MSRV raise, the `SHELBI_HOME` test guard as a prerequisite, and a `cstore` task for the plan text fixes. Tier 2 (sidebar reconcile into the daemon) waits for the request log to be re-read after Tier 1 has run.

