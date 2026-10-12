# rt-mutations-daemon

Status: landed (review round 1 accepted; follow-up tests added in round 2).

Phase 4a mutation half ("The daemon executes mutations"). The daemon is the
single owner of issue mutations: the mutation logic lives in `shelbi-orchestrator`
(`mutate` module) and runs behind a new daemon **control socket** (separate from
`hub.sock`) with per-issue queuing, expected-state checks, recheck before
irreversible steps (merge/push/dispatch), finish-if-the-client-leaves, and change
notifications to other connected clients. `shelbi issue
move|start|assign|unassign|edit|add` are thin clients; the review approve/reject
paths run over the same socket.

Behind the hidden dev setting `dev.daemon_mutations` (hub config
`~/.shelbi/shelbi.yaml`; env override `SHELBI_DAEMON_MUTATIONS`). Off (default) =
the CLI calls the library directly, byte-identical to today. Fold into the unified
session-backend dev selector at cutover.

`shelbi-app` command/view model is the sibling task `rt-app-model`; this task
implements the production `shelbi_app::execute_mutation` that routes the app's
`Mutation` to the control socket.

## Acceptance-criteria test coverage

- **Mutation logic in `shelbi-orchestrator`, CLI is a thin client** — the `mutate`
  module; `crates/shelbi-cli/src/commands/issue.rs` dropped 4879→~2350 lines.
  Unit coverage: `shelbi-orchestrator` `mutate/tests.rs`, `mutate/start.rs`,
  `mutate/add_edit.rs`, `transition_move_tests.rs`.
- **Setting off = behaves/prints as before** — `mutate_client::run_in_process`
  drives the same `mutate::apply` through a stdout sink; CLI tests
  `shelbi --bin shelbi issue::` stay green.
- **Setting on = daemon, one mutation per issue** — `daemon::control` unit test
  `issue_lock_is_shared_per_key_and_distinct_across_keys` + the race tests below.
- **Approve vs reject, two clients, one wins / other stale / no merge for loser**
  — `daemon::control::tests::approve_against_reject_from_two_clients_one_wins_the_other_is_stale_with_no_merge`
  (stub executor; no git/`gh`).
- **Same issue dispatched twice → exactly one agent** —
  `daemon::control::tests::the_same_issue_started_twice_at_once_starts_exactly_one_agent`
  (stub launcher).
- **Client disconnects mid-merge → merge finishes, status written** —
  `daemon::control::tests::a_merge_crossing_mutation_finishes_after_the_client_disconnects`
  (stub merge runner blocked on a barrier until the client has gone).
- **Recheck before merge/push/dispatch** —
  `shelbi-orchestrator::mutate::tests::apply_move_aborts_before_writing_when_the_recheck_reports_stale`,
  plus `control_socket::a_stale_expected_state_is_rejected_and_nothing_changes`
  (state changed between queue and the step → rejected, no change).
- **Other connected clients notified** —
  `control_socket::other_connected_clients_are_notified_of_a_change` (subscriber
  receives the `Changed` note) and the `broadcast_*` unit test.

Plus the end-to-end `crates/shelbi-cli/tests/control_socket.rs` (real daemon
process over the socket) for the stale gate, two-clients-one-wins, and
finish-after-disconnect on the unstubbed `move` path.
