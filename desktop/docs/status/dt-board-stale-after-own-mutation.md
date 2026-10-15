# dt-board-stale-after-own-mutation: a card's second change follows its first

## The bug

On the desktop board, two mutations on the same card inside one refresh
window failed: Assign then Start, back to back, refused Start with "it
changed elsewhere". Start succeeded once the board refreshed. Found in the
`dt-parity-audit` hand walk and recorded in `parity.md` under Known gaps.

## Root cause

Every board mutation carries the `(status, revision)` the user was looking at,
and the daemon refuses it if the issue has moved on. The board's only source
for that state was its read, and its read lags its own changes: it comes from
the daemon-owned board index, which picks a write up on a later pass. The
daemon already worked out the issue's post-change state after every mutation
(`ChangeNote`, which it broadcasts to the *other* clients), but the client
that made the change got a bare `Done`. So a follow-up was either held
("a change is already in progress") until a read showed the first change, or,
once a read had moved the card on without reaching the daemon's latest
revision, sent a state the daemon refused.

## The fix

- **The reply carries the landed state** (`shelbi-proto`). `ServerMsg::Done`
  has an optional `state`: the issue's `(status, revision)` right after the
  change, filled from the mutation's `ChangeNote`. It is `None` for every
  non-mutation reply and absent from older daemons; serde defaults it and
  omits it when empty, so old and new clients and daemons read each other
  unchanged and the protocol version stays 6.
- **The client returns it.** `ControlClient::mutate` returns
  `Option<ExpectedState>`, and `shelbi_app::execute_mutation_expecting` passes
  it on. The CLI and `execute_mutation` drop it.
- **The board adopts it** (`views/board_state.rs`). An acked mutation keeps
  the state it landed at (`Pending::landed`), and the card shows in the
  landed status meanwhile. The card's next mutation no longer waits for a
  read: it follows on from the acked one and is sent against the landed
  state. Only a state the reads could still show from before this client's
  own changes (between the read the first change was sent from and the
  landed revision) is replaced; anything else, such as an editor opened
  before another client's change, goes as seen and is still refused.
- **Settling.** An acked change with a landed state settles once a read's
  revision reaches it, so a read that shows the first of two chained changes
  doesn't settle the second. A follow-up keeps its predecessor's overlay and
  the columns it renumbers (the siblings there still wait for a read), and a
  refused follow-up puts the acked change it followed back.

## Decisions

- An optional field on `Done` rather than a new reply variant: a new variant
  would fail to decode in an older client, and a protocol bump would make a
  newer desktop report a stale daemon for a change that degrades cleanly.
  With an older daemon the board behaves as before.
- The revision is compared as time (RFC3339 `updated_at`), not string
  equality, for both settling and the "predates my own change" window: the
  daemon's writes only move it forward, and a read may show an intermediate
  state of a chain.
- The TUI board sends no expected state at all (its own Known gap in
  `parity.md`), so it has nothing to adopt and is unchanged.

## Verification

- `a_mutation_reports_its_landed_state_for_the_next_one`
  (`crates/shelbi-cli/tests/control_socket.rs`): a real daemon reports the
  on-disk state after a move; the pre-change state is refused, the landed
  state is accepted back to back, and after another client's move it is
  refused.
- `an_own_mutation_updates_the_cards_expected_revision`,
  `a_change_made_elsewhere_after_an_own_change_is_still_refused`,
  `a_follow_up_keeps_its_predecessors_column_waiting` (`board_state.rs`):
  Assign then Start with no read between, lagging and partial reads, the
  refusal path, a stale staged edit, and the sibling renumber guard.
- `done_carries_the_post_change_state_and_omits_it_when_absent`
  (`shelbi-proto`): the wire shape both ways.
