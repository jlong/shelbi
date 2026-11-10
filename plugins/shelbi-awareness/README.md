# shelbi-awareness

Shelbi's awareness (v2) delivery plugin for a Claude Code orchestrator. Shelbi
stages it into the orchestrator's working directory
(`.claude/shelbi-system-plugins/shelbi-awareness/`) and passes it with
`--plugin-dir` only when the project sets `orchestrator.awareness: awareness`.
It is session-only: nothing is installed into `~/.claude`.

It has two parts:

- **Monitor** (`monitors/monitors.json`) runs `shelbi orchestrator events stream`
  for the whole session. Each board event becomes one compact line, for example
  `shelbi reminder: id=r-1 text="check the tap PR merged"`, and Claude Code
  delivers each line to the orchestrator as a notification. A quiet board prints
  nothing.
- **Hooks** (`hooks/hooks.json`):
  - `UserPromptSubmit` attaches any event the monitor hasn't delivered yet to the
    user's message, so the orchestrator never answers from a stale board.
  - `Stop` keeps the orchestrator going once when an urgent event (a reminder, a
    finished job, an overdue task, a review that's ready) is still queued. It
    never blocks while `stop_hook_active` is set.
  - `SessionStart` adds the Zen policy summary (the first line of `zenmode.md`)
    when Zen Mode is on, and restates how events arrive after a compaction or a
    clear.

All of them drain the project's durable delivery queue (`event-cursor`), and
each one reads, hands over and acks a batch under one per-project lock, so every
event is delivered once. The commands find the project through `SHELBI_PROJECT`,
which the orchestrator session exports, and run the `shelbi` on `PATH`. Each hook
command ends in `|| true`: Claude Code treats a hook's exit code 2 as "block",
and an older `shelbi` that doesn't know the subcommand exits 2 from its argument
parser. The hooks themselves always exit 0.

## Monitor lifetime

The stream has no deadline of its own and runs until the session ends.
Claude Code documents no deadline for plugin monitors either (the 5 and 30 minute
limits apply to watches started with the Monitor tool). This was checked with
Claude Code 2.1.322: a plugin monitor process stayed up past 30 minutes and kept
delivering lines.

The stream exits on its own in two cases, both covered by the hooks:

- its stdout closes (the session is gone), and
- an error it can't recover from, which it reports as one last line.

Claude Code does not restart a plugin monitor that exits, so after such an exit
events still reach the orchestrator with each user message until the session is
restarted.
