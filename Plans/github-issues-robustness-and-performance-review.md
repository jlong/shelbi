# Review of "GitHub Issues: reliable synchronization with fewer moving parts"

Status: review of the Codex plan dated 2026-09-09
Reviewed: 2026-09-09, by the Shelbi orchestrator
Plan under review: [[github-issues-robustness-and-performance]]
Code baseline checked: Shelbi b66b3b2 (the plan's baseline), plus main at 4f20a3c for "already fixed" checks

## Verdict

The diagnosis is sound and the medicine is oversized. Every one of the twelve findings is real, with exact citations, and the API research is accurate apart from two corrections. But the plan rates the findings for a shared, hosted hub, not for one developer running one hub, and it answers them with a re-architecture (about thirteen new mechanisms against about eight deletions) when the confirmed problems at their real priority fit inside four existing files using primitives the crate already has. It also silently reverses at least nine recorded decisions, including two you made explicitly on August 31.

Recommendation in one line: execute an expanded Phase A now as small independent PRs on the existing `gh` runner, treat the deletion half of Phase B as the second slice, and split the transport rewrite (Phase C) and the data-model changes (Phase D) into their own plans that must earn their place after the request counts are measured.

## How this was checked

- Every finding was verified against an exact extract of b66b3b2 by one reader, then attacked by two skeptics each (one re-reading the code from scratch, one arguing impact for a solo hub). Twelve readers, twenty-four skeptics; no code-path skeptic overturned a verdict.
- Every API claim was checked against the cited GitHub, `gh`, and crate documentation with fresh fetches.
- The transport benchmark was re-run from the companion note's script on this machine (loopback only, dummy token, gh 2.96.0), twice, plus three cells the note did not measure.
- Seven independent reviewers then judged the architecture from separate lenses (scope, sequencing, sync and mutation semantics, transport, consistency with standing decisions, performance budget, and a steelman for the plan), followed by a completeness critic.
- Scratch material lives under the session scratchpad (`review/out/digest.md` for the finding-by-finding evidence, `review/out/panel.json` for the panel, `review/bench/` for the benchmark runs). Nothing in the hub checkout was modified.

## 1. The findings hold; the priorities do not

All twelve findings are confirmed against the baseline with exact citations. F9 is partial: the ten-label truncation is real, but the "missing next cursor" break requires a response GitHub does not send. Nothing in the findings was changed by the one commit on main since the baseline.

For a solo hub the impact skeptics re-rated them as follows. The blast radius on this hub is what changed, not the code reading.

| Finding | Plan | Verified | Why |
| --- | --- | --- | --- |
| F1 stale action reads, write-through re-publishes the pre-write copy | P0 | P1 | Real on every read-then-write CLI path; the sidebar lags one daemon tick (30 s, 120 s throttled) after each move or prio; a move to done is never spliced into done history. Unbounded only while the daemon is parked. Highest verified item. |
| F2 multi-writer snapshot without a lock | P0 | P2 | Lost updates are cache-level and self-heal on the next tick. The crate already ships the flock primitive (`lib.rs:738`); this is a small fix, not a redesign. |
| F3 blind POST replay can duplicate a create | P0 | P2 | Needs a post-commit 5xx or connection drop on `issue add` or `issue comment`; the `shelbi:id/<slug>` label is already the idempotency marker for issues. |
| F4 labels set before state; early return hides a half move | P0 | P2 | Rarely reached, but silent and permanent when it is. One PATCH with labels plus state plus `state_reason` closes it. |
| F5 malformed metadata silently replaced by defaults | P0 | P2 | Needs an external malformed edit; the fix is a few lines and restores parity with the file backend, which already errors. |
| F6 label-filtered id resolution, first match wins | P1 | P2 | Only bites un-migrated native issues; depends on whether those are meant to be actionable (decision below). |
| F7 two synchronization systems | P1 | P2 | The user-visible symptom (duplicate reconcile events) was fixed today in #1254; the remaining half is pure deletable duplicate work. |
| F8 incremental drift, no repo identity in the index | P1 | P2 and P3 | Drift half is P2 (hard deletes, transfers); the repo-identity half is P3 on its own. |
| F9 labels truncated at ten, null cursor accepted | P1 | P3 | Shelbi never produces more than ten labels; the cursor case needs a non-conformant response. |
| F10 same-second changes missed, cursor advances on append failure | P1 | P3 | Sub-second window that must coincide with a poll; append failure needs a disk fault. |
| F11 sequential refresh, waiter storms, credential memo, threshold mismatch | P1 | P3, except the 90 s stale threshold versus the 120 s throttled cadence, which is a P2 one-line fix that visibly flickers "stale" 25 percent of the time in the middle budget band |
| F12 label listing per add and move, dense renumbering | P2 | P3 | The REST budget was idle on the live hub (4,823 of 5,000 remaining after 46 writes in an hour). |

Two things the plan's table understates. The write-through no-op in F1 fires on the most common flows in the product, so its everyday cost is larger than the "indefinitely" framing suggests even though its worst case is smaller. And F11's threshold mismatch is the one live defect in that bundle; it deserves its own line.

## 2. API research: accurate, three corrections

Claims C1 through C10 were checked against the cited pages. All are accurate except:

- C6. The repository comments endpoint's `issue_url` does not distinguish pull request comments from issue comments; it is the same shape for both. The documented discriminator is the `pull_request` key on the parent issue object, which Shelbi already lists. The sync design in section 6 step 6 relies on this and needs the join.
- C9. The reqwest MSRV sentence rests on a false premise. The latest reqwest (0.13.5) needs Rust 1.85. The newest release under 1.80 is 0.13.3, and even that pulls a `rustls-platform-verifier` needing 1.85 unless the lock is generated with the fallback resolver on a newer cargo. More importantly, Shelbi's committed `Cargo.lock` already contains host crates needing 1.85 and 1.88 (clap 4.6, indexmap 2.14, darling 0.23, home 0.5.12, and others), and CI has no MSRV job. The declared 1.80 floor is neither enforced nor met today. The plan should say so and pick one path: raise `rust-version` to what the lock needs, or enforce 1.80 for real.
- Section 4's "reuse the existing Tokio runtime" is also wrong in effect. Tokio is declared in the workspace dependency table but no crate uses it and it is absent from `Cargo.lock`. Adding reqwest adds an async runtime and a TLS stack to a binary that has neither.

Smaller precision points worth carrying into the text: the 304 exemption applies only to authenticated conditional requests and says nothing about secondary limits; `state_reason` is ignored unless `state` changes; label, assignee, milestone, and type changes are silently dropped without push access; the `gh` source citation for one-connection-per-invocation is `api.go` lines 382 to 404 and 420 to 457 in v2.96.0, and the reuse comes from Go's default transport keep-alive, not from gh itself.

## 3. Benchmark: reproduced, but it measures spawn, not pooling

The note's numbers reproduce on this machine in direction and magnitude, cell for cell on the first run. Separate `gh` invocations open separate connections; `--paginate` reuses one. Three additions change the interpretation:

- A bare `gh --version` costs about 21 ms, which is essentially the whole zero-latency single-page cost. Process spawn is the variable, not connection setup.
- Unpooled versus pooled Python differed by about 0.1 ms on loopback. Pooling only matters over real TLS, which the benchmark cannot see.
- `gh auth token` costs about 43 ms whether it succeeds from the environment or fails. A runner that resolves credentials per request pays roughly 65 to 70 ms of local overhead per operation before any network time, and the write retry closure re-resolves per attempt.

Against real GitHub latency of 100 to 500 ms per request, the per-request penalty of keeping `gh` is a flat 22 to 40 ms once the credential is cached. The larger, cheaper win is caching the credential and cutting request counts, neither of which needs a transport rewrite. The zero-millisecond gh cells are also noisy by up to 2x between runs; the note's absolute numbers should be read as plus or minus 2x, the ratios are robust.

## 4. What the plan gets right

- The findings table. It is a careful, accurate reading of the code, and the citations are usable as work items as they stand.
- Delete the mutation-triggered open-board REST sweep and the per-process `board-snapshot.json` refresh thread. Pure deletion; nothing reads that snapshot on the render paths any more.
- One PATCH carrying labels, state, and `state_reason`, then verify the returned issue. Closes F4 and removes a write helper.
- Stop blind replay of creates; treat an ambiguous POST outcome as unknown rather than retrying.
- Always fetch fresh before acting. This is the caching plan's own standing decision; the F1 cache check defeats it and should go.
- Refuse malformed metadata on write paths instead of defaulting it.
- Stamp the index with repository identity and schema version, and schedule a periodic cold read so deletes and transfers stop leaving ghosts. This is the one reconciliation mechanism in the plan that is essential.
- Add `pageInfo` to the label fragment and fetch overflow; reject a missing cursor. Free while the fragment is being edited.
- Bound the `gh` child with a deadline. One wedged `gh` currently stalls every project's refresh because the daemon runs projects sequentially on one thread.
- The sync-loop rules in section 6 (overlapping since window with a server-time bound, dedup on native id plus timestamp plus content hash, separate comment cursor, ten-minute full reconcile) are the right closures for F8 and F10 and match GitHub's documented behavior.
- Keep `gh` for login and credential discovery. This preserves decision D2.

## 5. Where I disagree

1. "Fewer moving parts" holds for Phase A and the deletion half of Phase B, and fails for the whole. The plan introduces an ownership lock, a socket command protocol with command ids, a standalone one-shot engine, a durable operation journal, a pending-event queue with acknowledgements, sparse ordering keys with a writer-version gate, an alias schema, a typed transport, credential epochs, a principal-keyed governor, a consolidated versioned remote-state file, a repository-comments cursor, a fair scheduler, and a four-layer module split. Most of these are subsystems with their own persistence format. Every confirmed finding at its verified priority is fixable inside `github_store.rs`, `issue_cache.rs`, `board_index.rs`, and `daemon/board.rs` using the flock helper, the daemon as the existing single reader, the existing `refresh-board` socket verb, and `events.log` as the existing at-least-once stream.

2. Phase C bundles three separable changes and attributes all the benefit to the transport swap. Credential caching per process and a typed status-and-headers layer are cheap on `gh` itself (`--include`, env token, the header parsers that already exist in `gh_budget.rs`) and deliver the larger share of the measured savings. What only an in-process client buys is the spawn and a warm TLS connection in long-lived processes, which after Phase B's own traffic targets is seconds per hour of background latency nobody sees. Phase C also carries the false MSRV and Tokio premises above, replaces the `GhRunner` test seam and every fake-gh fixture (the most expensive line in the phase, priced as one checkbox), and reverses "writes stay REST via `gh api`" without naming it.

3. The plan says it supersedes the earlier plans "where specified" and then never names a reversal. It silently overrides: the written D3 record (no local cache, no stale data; the caching plan already walked this back in practice but never retired the text), "writes stay REST via `gh api`", "nothing regresses for scripts" and the daemon as a soft dependency (the ownership lock returns "busy" when a wedged daemon holds it, which the launchd PATH incident on September 8 would have triggered), "reads move to GraphQL" for the single-issue action path (section 3 moves action reads to REST conditional GETs, onto the write budget), the caching plan's explicit non-goal of changing the label and body encoding (sparse ordering, alias schema, no identity labels), and per-project state placement under `~/.shelbi/projects/<name>/` (replaced by one file per host, repository, and credential shared across projects). Each of these is a decision, not an implementation detail, and belongs in a "decisions reversed" ledger for you to accept or refuse.

4. Section 9 models the wrong traffic. Its baseline paragraph is arithmetically right from the constants (30 s refresh gives 120 per hour, and so on), but the hub keeps a request log (`~/.shelbi/gh-requests.log`) that the plan did not read. On the day the plan was written the daemon's refresh ran at 25 to 42 per hour, throttled into the 120 s band almost all day, while single-issue fetches and id searches from long-lived TUI processes ran at 2,500 to 4,500 GraphQL requests per hour on a three-to-five card board. That is the traffic that flapped the index stale about thirty times an hour overnight. The rebuilt binary (#1244, installed today) cut it, but the structure that produced it is exactly F1 and F7 and is the most valuable thing to fix. The plan has no target row for per-tick verification reads, and REST GET reads are not logged at all, so its before-and-after gates cannot be measured today.

5. Four semantic rules in sections 6 and 7 would produce bugs as written. Verification "compare returned labels and state with the request" collides with GitHub ignoring `state_reason` unless `state` changes, so every done-to-canceled move fails verification or keeps the wrong reason. The reopen-with-stale-terminal-label rule does not say which column the card renders in or whether the sync loop writes the repair (an automatic write from the sync loop is an autonomy boundary you should choose, not inherit). At-least-once event delivery with consumer-side dedup changes the `events.log` contract for the orchestrator drain; ack-equals-append with a tail scan keeps the contract exactly-once with no consumer change. The extra creation marker for issues is redundant with the id label; keep it only for comments.

6. Phase D is right in principle and not worth it now. Sparse ordering, backend-allocated aliases, and stopping identity-label creation are one-way doors on the issue-body schema. The only motivation with teeth (F12 label-list growth) is fixed for free by native-identity writes plus memoizing the finite status-label set. Identity-label removal is the one door with a bad rollback story.

7. The ownership model creates a new outage mode. Daemon-owned mutations over the socket, with a "busy" refusal while the owner holds the advisory lock, means a wedged daemon blocks `shelbi issue move` from the CLI. Per-operation file locks around the existing caller-side writes keep scripts working without a daemon and close F2 with a fraction of the machinery.

## 6. Recommended shape

Tier 1, now, on the existing `gh` runner, as ordered small PRs each with its fixtures and each independently revertible:

1. F4: one PATCH with labels, state, and `state_reason`; the early return compares GitHub state as well as the label; `column()` stops treating open plus terminal label as done.
2. F1 and the F7 sweep: gate `cached_issue_if_fresh` on a non-stale, in-threshold index (or delete it); write paths parse the returned issue into the cache and index so the wrapper's post-write get serves the post-write copy; delete `invalidate()`'s REST sweep and `kick_refresh`.
3. F2: `acquire_file_lock` around every read-modify-replace in `board_index.rs`, `done_history.rs`, and `gh_budget.rs`, and the daemon re-reads under the lock after its GraphQL call and merges its delta; never hold the lock across the network.
4. F3: method-aware retry disposition so a POST that may have been sent returns a typed unknown-outcome error naming `shelbi issue show <id>`; rate-limit retries stay because they are pre-commit.
5. F5 and F9: write paths error on an unparseable metadata block; `pageInfo` on labels with overflow fetch; null-cursor guard.
6. F8: `repo` and `schema_version` on `BoardIndex`, bootstrap on mismatch, cold read every ten minutes.
7. F11: threshold off the effective tick interval written into the index; F6: `get_raw` resolves via the cached or index number first, accepts all-digit ids, errors on more than one label match.
8. Credential and runner hygiene from Phase C, without the new dependency: memoize the token process-wide (stores are rebuilt per operation, so per-instance is useless) with 401 invalidation and pass it as `GH_TOKEN` to every child; `--include` on every call and classify on status plus rate-limit headers; a child-process deadline; pin `X-GitHub-Api-Version: 2022-11-28`; log REST GETs so a before-and-after exists.

Tier 2, as its own plan after Tier 1 has run for a while and the request log has been re-read: move the sidebar's issue and comment reconcile into the daemon (re-porting #1254), persist its cursor, and coalesce manual refreshes. This is the "one owner" idea at the size the confirmed findings justify.

Gated or deferred: Phase C behind the existing `GhRunner` seam with a measured trigger (for example, socket-routed action latency above a threshold, or a shared hub concentrating hundreds of requests per hour in one process), and only after the MSRV decision; Phase D split into its own plan with its own justification, keeping here only status-label provisioning and the `rm` naming fix (the GitHub backend closes on `rm` while the CLI says "deleted").

Text fixes before any dispatch: substitute the verified priorities for the P0 and P1 labels (priority labels drive Zen sequencing); fix C6, the MSRV sentence, and the Tokio sentence; add the decisions-reversed ledger; re-anchor section 9 on the live request log with a verification-reads row; tag each acceptance bullet as provable with existing fixtures, needs a named harness, or soak.

## 7. Decisions only you can make

1. Retire or reaffirm D3. The written record still says no stale data and no offline cache. The caching plan and this plan both assume "render stale, never act stale". If that is now policy, say so in pluggable-task-stores with a date.
2. Transport. Does "writes stay REST via `gh api`" still stand? Keeping `gh` as the request path is compatible with everything in Tier 1; replacing it is a separate decision with a dependency, runtime, TLS, and test-fixture cost.
3. MSRV. Raise `rust-version` to what the lock already needs (1.88) and add an MSRV CI job, or enforce 1.80 with a regenerated lock and downgrades. This exists independently of the plan and gates any HTTP client choice.
4. Write ownership. Caller-side writes serialized by a cross-process flock (keeps scripts daemon-free), or daemon-owned mutations over the socket with a "busy" refusal when the owner is wedged.
5. Are un-migrated native GitHub issues, shown under their number, meant to be actionable from Shelbi? Yes means the small `get_raw` change ships in Tier 1; no means F6 becomes a clear error.
6. Is hand-editing the fenced metadata block on github.com a supported workflow? Yes means F5 also needs a read-side warning; no means reject-on-write is enough.
7. Reopen policy. When a done or canceled issue is reopened on GitHub, does Shelbi only interpret it (render, emit an event) or auto-repair the status label from the sync loop? And which column is the target?
8. Terminal-to-terminal moves. Accept that GitHub keeps the old `state_reason` with the label as authority, or spend two PATCHes for an accurate reason.
9. Action-read path. Keep the shipped GraphQL single-issue fetch on the read budget with the F1 check fixed, or move action reads to REST conditional GETs on the write budget as section 3 proposes.
10. State placement and multi-project repos. Per-project files as today, or one file per host, repository, and credential shared across projects, which first requires deciding whether several Shelbi projects on one repo is supported at all.
11. Host scope. github.com only, or GHES in scope with an explicit host key and a compatibility test.
12. `events.log` contract. Exactly-once lines as today, or event ids with at-least-once delivery and consumer dedup.
13. Acceptance scenario. Gate on the solo hub as it runs (one open page, a handful of cards, single-digit writes per hour) or on the modelled shared hub.

## 8. What the completeness critic added

A final reviewer read the panel's output against the code and the live hub and found four things nobody had exercised, plus corrections to the panel itself.

Gaps in the plan that change the cost of Phase B:

- **The "existing hub socket" has no request-reply client.** `refresh-board` is implemented server-side, but nothing outside the daemon sends it. The only socket clients today are the one-line event emitter, the best-effort `message-pushed`, the version hello, and the orchestrator feed. "Callers submit commands over the existing socket" is a new wire format, a new client library with deadlines and command-id retry, and a version gate (a new CLI sending a mutation verb to an older daemon gets silence). It is not reuse.
- **Seven of the eight projects on this hub are file_system-backed**, and the daemon's board loop only runs for remote projects. Three plan items are trait-level changes (explicit read semantics, shared dependency and workflow validation, backend-allocated slugs) that the file backend must implement or be carved out of, and github-only socket-routed writes create exactly the dual code path the principles reject. The plan needs a file_system section.
- **Coupled operations live in the caller, not the store.** `shelbi issue move` does the remote write, then the `events.log` append, and rolls the move back with a second remote write if the append fails; transition actions (merges, worktree commands) run in the caller too. Daemon-owned mutations therefore either move git merges into the launchd daemon (the process whose PATH broke on September 8) or split the operation journal across two processes with no stated owner. Phase B was priced as if the store write were the whole operation.
- **Tests leak into the live hub.** A fixture `board-index.json` for a nonexistent `test-project` was written under `~/.shelbi/projects/` at 12:57Z today by the unit tests. It carries no repository identity, so nothing can tell it from a real board, which is F8 with a live artifact. Every multi-process acceptance harness the plan proposes will write into the same live hub until a suite-wide `SHELBI_HOME` guard exists. That guard is a Phase A prerequisite.

Smaller gaps: off-index single-issue reads have no cache at all, and one caller (the review-pane heal loop) polls that path at 5 Hz while the per-tick handoff `get` hits it for closed ids; this is a better explanation of the overnight storm than the activity feed, which already negative-caches misses. Migration resumability is already shipped in `issue_migrate.rs` and should leave Phase D; the real migration defect is pacing per issue instead of per content-creating request. And the ownership lock is per machine, which the plan never says; forwarded hub sockets would let a remote host mutate issues for the first time.

Corrections to the panel and to the fact sheet it worked from:

- Tokio is declared in the workspace table but no crate uses it and it is not in `Cargo.lock` (already reflected in section 2 above).
- The F2 lock must cover the read as well as the publish: the daemon re-reads the index under the lock after its GraphQL call and merges its delta onto that copy. Locking only the publish leaves the merge base stale.
- The credential memo must be process-global. Stores are rebuilt per operation, so a per-instance memo saves nothing on the dominant per-get paths.
- A version-mismatched daemon already refuses every mutation today, so "daemon health as a write prerequisite" is partly the status quo; only a stopped daemon permits the direct-file fallback. The plan's "busy" refusal would still widen that window to a wedged daemon.
- Several reviewer numbers (fixture counts, the negative-TTL quota effect, a `--paginate --slurp` timing) were asserted rather than measured and should not be quoted.

## Appendix: method and materials

- Fact-check: 46 agents (12 verifiers, 24 skeptics, 10 doc checks), every one completed. Judgment: 7 lenses plus a completeness critic, all completed.
- Two corrections the critic made to the fact sheet the panel received: Tokio is not an actual dependency, and `event_log.rs` predates #1254 (that PR added 249 lines to an existing 5,511-line file).
- Benchmark: `review/bench/` in the session scratchpad; `benchmark.py` is byte-identical to the note's script, `benchmark_hardened.py` adds only `GH_NO_UPDATE_NOTIFIER` and env isolation, `results_run1.json`, `results_run2.json`, `results_ext.json`, `summary.txt`.
- No live GitHub requests were made by the benchmark; doc checks used public documentation only.