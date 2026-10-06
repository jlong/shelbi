# rt-cutover-instructions

**Status:** done (pending review)

Rewrote the shipped agent instruction templates to drive the session backend
instead of tmux, moved the review-ready event onto a session name, and added a
config-upgrade rule that heals a project's own forked copies.

- Templates/skill: `default_orchestrator.md.template` (`tmux send-keys` ->
  `shelbi session send`; heartbeat `tmux capture-pane` -> `shelbi session
  snapshot <workspace>`), `default_review.md.template` and
  `skills/load_run_detection.SKILL.md` (dropped the `tmux new-window` serve
  alternative, keeping the background `&` form). No shipped template or skill
  mentions tmux now.
- Review events: `ReviewReadyEvent.pane` (a tmux `session:window` target) is now
  `session`, carrying the review slot's agent session name `<project>/ws/<slot>`
  (derived via `session_process_backend::session_name`). `pane=` -> `session=`
  on the `review-ready` line; consumers (orchestrator drain metadata, tests)
  updated.
- Config-upgrade: `sniff_tmux_commands` + `heal_tmux_instructions` keyed on a
  shared `TMUX_INSTRUCTION_REWRITES` table. An exact legacy passage auto-heals;
  an edited one is reported needs-judgment with the proposed fix and left
  untouched. Content-based, idempotent, disclosed on `events.log`.
