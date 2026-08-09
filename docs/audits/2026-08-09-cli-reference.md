# CLI Reference Audit — `site/content/docs/cli/`

**Date:** 2026-08-09
**Scope:** All 18 CLI reference pages under `site/content/docs/cli/`
(agent, attach, config, daemon, events, init, merge, message, open, project,
reload, send, status, task, wizard, workflow, workspace, zen).
**Type:** Audit only. No doc file was modified; a separate fix pass follows triage.

## Ground truth

- Clap parsers in `crates/shelbi-cli/src/commands/` (plus `main.rs`, `wizard.rs`)
  and shared domain types in `crates/shelbi-core/`.
- `--help` output captured from a freshly built binary
  (`cargo build --workspace`, then `./target/debug/shelbi <cmd> [<sub>] --help`
  for every documented command and subcommand — see the command list at the end).

Where a doc claim and the binary's own `--help`/about text disagree with the
parser source, the parser source (behavior) is treated as ground truth and the
stale about-string is noted as supporting evidence (this happened for `status`).

## How to read a finding

- **Severity** — `critical` (documented command/example errors outright or is
  dangerously wrong) · `high` (a flag/subcommand/behavior is added, removed, or
  renamed such that the page materially misleads) · `medium` (wrong
  default/arg-name, incomplete enumeration a reader would rely on) · `low`
  (cosmetic value-name drift, an omitted global flag, an understated nuance).
- **Label** — `CONFIRMED ERROR` (doc contradicts current source/help) vs
  `DRIFT RISK` (technically defensible today but stale, incomplete, or fragile).

---

## Findings

### workflow.mdx

**F1 — `high` — CONFIRMED ERROR**
- **Doc:** `workflow.mdx:14` — "Every project has a built-in `default` workflow:
  the canonical five-status flow (`Backlog → Todo → InProgress → Review → Done`)."
- **Source:** `crates/shelbi-core/src/workflow.rs:665-717` — `default_workflow()`
  ships **six** statuses; the sixth is
  `Status { id: "canceled", name: "Canceled", category: StatusCategory::Archived, … }`
  (lines 710-717). `default_project_statuses()` in
  `crates/shelbi-core/src/statuses.rs:173-208` likewise has six ending in
  `canceled`. The display name is also **"In Progress"** (id `in-progress`), not
  `InProgress`.
- Core description of the canonical flow is factually wrong: it omits `Canceled`
  and mis-renders the InProgress lane.

**F2 — `low` — CONFIRMED ERROR** (same root cause as F1, second occurrence)
- **Doc:** `workflow.mdx:61-62`, echoed at `:66` — `workflow new` "Scaffold a new
  workflow YAML pre-populated with the canonical five-status default."
- **Source:** `crates/shelbi-cli/src/commands/workflow.rs:143-144` — `write_starter`
  doc comment: "the canonical **six-status** default (the five historical lanes
  plus `Canceled`)"; it serializes `default_workflow()` (6 statuses).

### workspace.mdx

**F3 — `high` — CONFIRMED ERROR**
- **Doc:** `workspace.mdx:69-84` — the `list` column table names exactly five
  columns (`NAME`, `HOST`, `RUNNER`, `AGENT`, `STATE`) and the sample output block
  (lines 78-84) renders those same five columns.
- **Source:** `crates/shelbi-cli/src/commands/workspace.rs:547` — header is
  formatted with **six** columns:
  `"{…} {…} {…} {…} {…} {}", "NAME", "HOST", "RUNNER", "AGENT", "INTEG", "STATE"`.
  Rows carry the same `INTEG` (integration tier: `conventional`/`degraded`) cell
  between `AGENT` and `STATE`; asserted by tests at `workspace.rs:942-944`.
- Both the column table and the sample output are stale — the `INTEG` column is
  missing entirely.

**F4 — `medium` — CONFIRMED ERROR**
- **Doc:** `workspace.mdx:75` ("`STATE` … `idle`, or `in_progress: <task-id>`")
  and `workspace.mdx:88-90` ("`STATE` here is board-derived. For the poller's
  live read … use `status`.").
- **Source:** `workspace.rs:362` (`list` → `occupied_idle_workspaces`) →
  `workspace.rs:495` (`probe_workspace_slot`, a live tmux/SSH probe). `list` is
  **not** purely board-derived; it probes panes. STATE also renders
  `review: <task-id>` (`workspace.rs:561-565`), `occupied (user shell)`,
  `orphaned session`, and `unreachable (<reason>)` (`workspace.rs:443-449,
  567-571`) — more values than the two documented.

**F5 — `low` — DRIFT RISK**
- **Doc:** `workspace.mdx:95` and `:119` — `set-runner` positional list named
  `WORKSPACE` / `[WORKSPACE ...]`.
- **Source:** `workspace.rs:70` field is `names: Vec<String>`;
  `shelbi workspace set-runner --help` shows `[NAMES]...`. Value-name drift;
  examples still parse.

### events.mdx

**F6 — `high` — CONFIRMED ERROR**
- **Doc:** `events.mdx:26` — "Cadence comes from the `heartbeat` key in
  `project.yaml` (default `3m`)."
- **Source:** `crates/shelbi-core/src/model.rs:310` —
  `pub const HEARTBEAT_DEFAULT: Duration = Duration::from_secs(60);` (standard
  cadence 60s, backing off toward `HEARTBEAT_MAX_DEFAULT` = 300s at
  `model.rs:316`). The default interval is **60s, not 3m**.

**F7 — `medium` — CONFIRMED ERROR**
- **Doc:** `events.mdx:20` — line shape `<ts> worker=<name> <prev> -> <new>`.
- **Source:** `crates/shelbi-cli/src/commands/events.rs:5-6` — "every line is
  `<rfc3339> workspace=<name> <prev> -> <new>`"; test fixture `events.rs:227`
  uses `workspace=alpha`; the developer agent template emits `workspace=<name>`.
  `worker=` is only a retired pre-rename alias the TUI still parses "for one
  release" (`crates/shelbi-tui/src/activity.rs:816,823`). The current emitted
  field is `workspace=`, so `worker=<name>` as a common line shape is stale.

**F8 — `low` — DRIFT RISK**
- **Doc:** `events.mdx:24-25` — "`heartbeat` is the periodic wake-up the hub
  poller writes when the board is otherwise quiet."
- **Source:** `crates/shelbi-core/src/model.rs:280-285` — cadence is adaptive:
  `interval` is the standard cadence used whenever there is supervisable work in
  flight, and the poller *backs off* (doubling toward `max`) while quiescent. The
  heartbeat also fires during active supervision (suppressed only when a real
  event already landed in the interval), so "when the board is otherwise quiet"
  understates when it is written. Related to #521's stream unification.

### task.mdx

**F9 — `medium` — CONFIRMED ERROR**
- **Doc:** `task.mdx:69` — the `-d, --description` row lists the default body as
  **empty**: "`| string | empty | Task body. Use `shelbi task edit` later if
  omitted. |`".
- **Source:** `crates/shelbi-cli/src/commands/task.rs:139-142` (doc comment: "If
  neither is given, the body defaults to the title") and `task.rs:425`
  `(None, None) => format!("# Task\n\n{}\n", args.title)`. `shelbi task add
  --help` says the same. The body defaults to the **title**, not empty.

### zen.mdx

**F10 — `medium` — CONFIRMED ERROR**
- **Doc:** `zen.mdx:12` — "The toggle subcommands (`on`, `off`, `pause`,
  `status`) flip the project's mode in `state.json` and write a `mode=zen …`
  line to `~/.shelbi/events.log`."
- **Source:** `crates/shelbi-cli/src/commands/zen.rs:289-293` — only
  `On`/`Off`/`Pause` call `set()` → `set_zen_mode` (which flips state and appends
  the event). `ZenCmd::Status => status(&project_name)` and `status()`
  (`zen.rs:470-473`) only read and print — `shelbi zen status --help` is "Show
  the current mode…". `status` is read-only; it flips nothing and writes no event.

**F11 — `low` — DRIFT RISK**
- **Doc:** `zen.mdx:140` (and the pr-create/ci-watch/pr-merge flag tables at
  `:146-154`, `:184-194`, `:214-222`) — documents exactly six `--match-*` flags
  and states "All six `--match-*` flags are required."
- **Source:** `zen.rs:113-114` — `PinnedIdentityArgs` has a **seventh**
  `#[arg(long, value_name = "SHA")] match_published_head_commit: Option<String>`,
  accepted on `pr-create`, `ci-watch`, and `pr-merge` (each flattens
  `PinnedIdentityArgs`; confirmed in all three `--help` captures). It is optional,
  so "six required" is literally true, but the seventh flag is entirely
  undocumented — meaningful because these pinned-identity flags gate
  exact-provenance auto-merge.

**F12 — `low` — DRIFT RISK**
- **Doc:** `zen.mdx:104`, `:128` (and arg-table rows `:123`, `:148`) — positional
  shown as `<ID>` for `zen probe <ID>` and `zen pr-create <ID> …`.
- **Source:** `shelbi zen probe --help` → `Usage: … <TASK_ID>`; parser field is
  `task_id` (`zen.rs:149,159`). Real value name is `<TASK_ID>`. Cosmetic.

**F13 — `low` — DRIFT RISK**
- **Doc:** `zen.mdx:13-14` — event line quoted as
  `mode=zen <prev> -> <new> reason=user:cli`.
- **Source:** `crates/shelbi-core/src/event_log.rs:1298-1300` — the real line is
  `{ts} project={project} mode=zen {prev} -> {new} reason={source}`. The doc
  excerpt omits the leading `project=<project>` scope, which `zen.rs:19-21` calls
  out as load-bearing (keeps one project's toggle from being read by another
  project's orchestrator). Partial/stale sample.

### status.mdx

**F14 — `medium` — DRIFT RISK**
- **Doc:** `status.mdx:14` (repeated in frontmatter `:4` and flag row `:31`) —
  "`--full` … board + workspaces + zen + handoff-presence" (four sections).
- **Source:** `crates/shelbi-cli/src/commands/status.rs:166-193` — `print_full`
  emits **five** sections: `## Board`, `## Workspaces`, `## Zen`, `## Handoff`,
  and `## Daemon` (line 193). The summary path also prints a `daemon:` line
  (line 158). The doc omits the Daemon section. (The binary's own `status --help`
  about-text is stale here too; source is ground truth.)

### merge.mdx

**F15 — `low` — CONFIRMED ERROR**
- **Doc:** `merge.mdx:12` — "it squash-merges the task's branch … using **the
  workspace's commit message**."
- **Source:** `crates/shelbi-cli/src/commands/merge.rs:294`
  `let summary = format!("shelbi: merge {id} from {branch}");` then the Squash arm
  (`merge.rs:297-299`) runs `git merge --squash` + `git commit -m <summary>`. The
  squash commit message is a shelbi-generated summary
  (`shelbi: merge <id> from <branch>`), not the workspace's own commit message.
  (The "squash by default" claim itself is correct: `Project::merge_strategy()`
  defaults to `MergeStrategy::Squash`.)

### message.mdx

**F16 — `low` — DRIFT RISK**
- **Doc:** `message.mdx:8` synopsis `shelbi message [OPTIONS] <ID> <KIND> <BODY>`
  and Arguments table `:69-71` marking each "(required)".
- **Source:** `shelbi message --help` shows `Usage: … [ID] [KIND] [BODY]`
  (optional); `main.rs:107-112` declares `id: Option<String>`,
  `kind: Option<MessageKind>`, `body: Option<String>`. They are optional at the
  clap layer (so `message status …` parses) and only enforced as required at
  runtime (`message.rs:110-124`). Doc presents them as hard-required positionals.

**F17 — `low` — DRIFT RISK**
- **Doc:** `message.mdx:9`, `:83` — `shelbi message status <MSG-ID>`.
- **Source:** `shelbi message status --help` → `Usage: … <MSG_ID>`;
  `message.rs:46` `msg_id: String` → clap renders `<MSG_ID>` (underscore). Doc
  hyphenates. Cosmetic value-name mismatch.

### send.mdx

**F18 — `low` — DRIFT RISK**
- **Doc:** `send.mdx:28` — "`NAME` is resolved against the project YAML's
  `workspaces:` block."
- **Source:** `crates/shelbi-cli/src/commands/send.rs:33` — positional is
  `id: String`; `send --help` shows `<ID>`. The page's own Arguments table
  (`send.mdx:40`) correctly calls it `<ID>`; line 28 uses the wrong name.

**F19 — `low` — DRIFT RISK**
- **Doc:** `send.mdx:28,40` — resolution described only against `workspaces:`.
- **Source:** `send.rs:202-248` `resolve_target` resolves workspace first, then
  falls back to the legacy spawn agent registry (`load_agent`); `send --help`
  states the same. The legacy fallback is unmentioned (likely intentional
  simplification, flagged as drift).

### reload.mdx

**F20 — `low` — DRIFT RISK**
- **Doc:** `reload.mdx:79-81` — the "## Flags" table lists only `-p, --project`.
- **Source:** `shelbi reload --help` — reload also accepts `--root <PATH>` and
  `-y, --yes`. The table presents itself as the flag reference but omits `--root`.
  (Targets, usage `[OPTIONS] [TARGET] [NAME]`, and the valid-target set all match.)

### wizard.mdx

**F21 — `low` — DRIFT RISK**
- **Doc:** `wizard.mdx:8` — usage shown as bare `shelbi wizard` (no options).
- **Source:** `shelbi wizard --help` → `Usage: shelbi wizard [OPTIONS]`; wizard
  accepts `--root`, `-p/--project`, `-y/--yes`. (The `shelbi init -y --runner
  codex` example at `:44` and the `shelbi project add` reference at `:30` are both
  valid.)

### project.mdx

**F22 — `low` — DRIFT RISK**
- **Doc:** `project.mdx:15-18` — "Every subcommand accepts the global `--root
  <PATH>` … and `-p, --project <PROJECT>` flags."
- **Source:** `shelbi project [add|migrate-to-in-repo] --help` — every subcommand
  also accepts `-y, --yes`, meaningful for `add` (accept the detected plan without
  prompts), yet unmentioned. Also `migrate-to-in-repo`'s `-p` uses value name
  `<NAME>`, not `<PROJECT>` (`project.rs:54` `value_name = "NAME"`). The migration
  flag table (`--dry-run`, `--yes`) and idempotent/one-way prose match source.

### config.mdx

**F23 — `low` — DRIFT RISK**
- **Doc:** `config.mdx:118-125` — the `dump-keybindings` synopsis shows no
  options; prose implies manual copy/redirect into `~/.shelbi/keys.yaml`.
- **Source:** `config.rs:60-70` —
  `DumpKeybindings { #[arg(long, short = 'o')] out: Option<PathBuf>, #[arg(long) ] force: bool }`;
  `shelbi config dump-keybindings --help` lists `-o, --out <OUT>` and `--force`.
  Two real flags that write the dump directly to a file are undocumented.

---

## Pages verified clean (no findings)

| Page | Verification notes |
| --- | --- |
| `agent.mdx` | Subcommands `list/show/new/edit`, `<NAME>` required on show/new/edit, name-validation rules (`agent.rs:279-298`), the six shipped agents (orchestrator/developer/review/qa/security/adversarial), and `CUSTOMIZED` column semantics all match source + help. |
| `attach.mdx` | Usage `shelbi attach [OPTIONS] <ID>`, required `<ID>` (`attach.rs:13`), `--root`/`-p` flags, and the `shelbi attach alpha` example all match. |
| `daemon.mdx` | All five subcommands (`run/install/uninstall/status/restart`) + bare default, `--root`/`-p` flags, and every reverse-forward tuning env var (`SHELBI_FORWARD_RETRY_ATTEMPTS`=3 clamp 1..=10, `SHELBI_FORWARD_RETRY_BACKOFF_MS`=250 ≤5000, `SHELBI_TCP_FORWARD_PORT_BASE`=47100, `SHELBI_TCP_FORWARD_PORT_SPAN`=64) verified against `shelbi-ssh`. No stale keepalive/heartbeat claims. |
| `init.mdx` | Global `-y/--yes` before-or-after, `--branch`→`--default-branch` alias, `--remote`→`--github-url` alias, `--runner {claude,codex}`, `--orchestrator-runner`, `--mode {in-repo,global}`, `--pick-up` all match `init.rs:57-112`. Examples parse. |
| `open.mdx` | Usage `shelbi open [OPTIONS] <NAME>`, `<NAME>` required, `--root`/`-p`; hidden `--as-pane`/`--resume` correctly kept out of the flag table. Example parses. |

---

## Summary

| Severity | Count | Confirmed error | Drift risk |
| --- | --- | --- | --- |
| critical | 0 | 0 | 0 |
| high | 3 | 3 | 0 |
| medium | 5 | 4 | 1 |
| low | 15 | 2 | 13 |
| **Total** | **23** | **9** | **14** |

**Pages with findings:** workflow (2), workspace (3), events (3), zen (4), task (1),
status (1), merge (1), message (2), send (2), reload (1), wizard (1), project (1),
config (1).
**Pages clean:** agent, attach, daemon, init, open.

### Highest-priority items

1. **F6 (events.mdx)** — heartbeat default documented as `3m`; source is `60s`.
2. **F3 (workspace.mdx)** — `list` output missing the `INTEG` column (table + sample stale).
3. **F1 (workflow.mdx)** — "canonical five-status" default; source ships six (adds `Canceled`).

These three are confirmed, load-bearing factual errors a reader would act on.

---

## Verification commands

```sh
# Build the workspace (ground-truth binary)
cargo build --workspace

# Capture --help for every documented command and subcommand
BIN=./target/debug/shelbi
$BIN --help
for c in agent attach config daemon events init merge message open project \
         reload send status task wizard workflow workspace zen; do
  $BIN $c --help
  # then each subcommand, e.g.:
  #   $BIN task add --help ; $BIN task start --help ; $BIN task move --help ; …
  #   $BIN zen probe --help ; $BIN zen pr-create --help ; …
  #   $BIN workspace list --help ; $BIN workspace set-runner --help ; …
  #   $BIN config dump-keybindings --help ; $BIN config inventory --help ; …
  #   $BIN daemon status --help ; $BIN events tail --help ; $BIN message status --help ; …
done
```

Subcommand `--help` captured this run: `task {add,list,show,depends,move,assign,
unassign,start,resume,prio,edit,rm}`; `zen {ci-watch,dry-run,off,on,pause,
pr-create,pr-merge,probe,scan,status}`; `config {check,dump-keybindings,inventory,
lint,list-actions}`; `workspace {add,list,rm,set-runner,status,stop}`;
`agent {list,show,new,edit}`; `daemon {install,uninstall,status,restart,run}`;
`events {tail}`; `message {status}`; `project {add,migrate-to-in-repo}`;
`workflow {list,show,new,edit}`; `status {list}`.

Source cross-checked under `crates/shelbi-cli/src/commands/` (per-command `*.rs`,
`mod.rs`, `main.rs`, `wizard.rs`) and `crates/shelbi-core/src/`
(`workflow.rs`, `statuses.rs`, `model.rs`, `event_log.rs`) plus
`crates/shelbi-tui/src/activity.rs` and `crates/shelbi-ssh/src/`.

### Relationship to the prior audit

`docs/documentation-codebase-audit.md` (2026-07-13) was used for dedupe only;
every claim here was re-verified against current source. The events-stream
unification (#521) is the source of the now-stale heartbeat/`worker=` claims in
`events.mdx` (F6, F7, F8).
