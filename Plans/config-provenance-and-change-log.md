# Config Provenance and Change Log

## Context

Shelbi ships default configuration — agent instructions (`agents/<role>/instructions.md`), `zenmode.md`, workflows, keys/YAML — and copies it into each project at `shelbi init`. After that the project owns its copy; users and the orchestrator edit it freely. To let existing projects adopt improvements to the shipped defaults, a version-agnostic **config-upgrade self-heal pass** (`crates/shelbi-cli/src/commands/config_upgrade.rs`) runs on hub start / `shelbi reload`: content-sniffing detectors classify each surface `AutoHeal` (applied automatically) or `NeedsJudgment` (written to a findings file the orchestrator ingests at boot).

The weakness: content sniffers detect *old wording*, but they cannot tell a **deliberate customization** from a **stale un-updated default**. Findings are `NeedsJudgment` so nothing blindly reverts, but the orchestrator's judgment is *uninformed* — it has no record of intent, so it can wrongly "heal" an intentional edit back to the shipped default. This is a live risk: this project's orchestrator instructions and `zenmode.md` were hand-edited (review-slot exclusion, the zen finalize policy) and a future sniffer could flag those as divergent with nothing to signal they were intentional.

We want self-heal to **never revert an intentional change**, and to have an auditable record of every config change — whether made by a human, the orchestrator, or shelbi itself.

## Design

Two complementary layers on top of the existing config-upgrade pass. They do **not** replace the targeted content sniffers (see Decisions) — they make them safe and add a generic net.

### 1. Three-way provenance hashes (per config surface)

For each Shelbi-owned surface already enumerated in `config_surfaces.rs` (zenmode, per-agent instructions, workflows, keys/YAML), track a content hash and compare **three** values, git-merge-base style:

- **base** — hash of the shipped default *at the time the project last accepted it* (stored per surface).
- **ours** — current project config hash.
- **theirs** — current shipped-default hash (from the binary).

Decision table on boot:

| ours vs base | theirs vs base | meaning | action |
|---|---|---|---|
| == | != | default advanced, user never touched it | offer / candidate AutoHeal |
| != | == | user customized, default unchanged | leave alone, never flag |
| != | != | both moved | `NeedsJudgment` with a diff |
| ours == theirs | — | in sync | nothing |

A bare "project-hash != default-hash → flag" is explicitly rejected: it fires on every customized project at every default bump and trains the orchestrator to ignore the signal. The **baseline** is what tells you *which side moved* — the only thing that decides whether the orchestrator should be bothered at all.

Hash: md5 or sha256 (change-detection, not security). Normalize trailing whitespace before hashing to avoid trivial-diff noise; otherwise exact content.

### 2. Actor-tagged change log (the "why" layer)

Append-only structured log; each entry: `{ ts, surface, actor, summary, resulting_hash }`.

- `actor`: `shelbi-auto` (auto-heal, `--apply-finding`, launch-time self-heal such as the `settings.local.json` merge, migrations), `orchestrator` (agent hand-edit), `user` (direct human edit — best-effort; caught on next boot when the hash matches no logged entry).
- **Both automated and manual changes are logged.** Shelbi stamps an entry + updates provenance on every config write it performs (it already emits a `config-upgrade` events.log line — a small step to also append here). The orchestrator appends an entry for each hand-edit.
- `resulting_hash` links the log to provenance: a divergence whose current hash matches a logged deliberate change is *explained* → suppress the finding. A hash matching **no** logged entry AND differing from base is *unexplained* → the precise `NeedsJudgment` trigger.

Provenance hashes are ground truth for *what* changed; the log is the *why*. Making shelbi the authoritative writer on automated writes keeps the trail from silently dropping entries; the agent covers hand edits.

### 3. Integration with findings

Every config-upgrade finding carries `diverged_from_baseline: y/n` and `explained_by_log: y/n`, so the orchestrator decides fast and never reverts an intentional edit. Content sniffers still run for *targeted* migrations (deterministic `AutoHeal` + precise fix text); provenance + log is the shared context that keeps both honest.

## Storage

Per-project, under `~/.shelbi/projects/<name>/` (not the repo, not `.claude/`):

- `config-provenance.json` — `{ surface_id: { base_hash, current_hash, last_synced_ts } }`.
- `config-log.jsonl` — append-only entries (a human-readable render is derivable).

`base` is seeded at `shelbi init` (= shipped-default hash at init) and re-stamped whenever the project accepts an update, so the three-way comparison stays meaningful.

## Decisions

- **Provenance + log live alongside the content sniffers, permanently — not a replacement.** Provenance answers "did this diverge, and which side moved"; sniffers answer "here is the specific known migration and its deterministic fix." Dropping sniffers would lose targeted `AutoHeal` and precise fix instructions. (User, 2026-08-16.)
- **All config changes are logged — automated and manual.** Shelbi is the authoritative writer for its own writes; the orchestrator logs hand edits. (User, 2026-08-16.)

## First slice

1. Hashing + `config-provenance.json`, seeded at init and re-stamped on every shelbi config write.
2. `config-log.jsonl` written by shelbi on every automated write; documented append contract for the orchestrator.
3. Three-way decision feeding the existing findings file (`diverged_from_baseline` / `explained_by_log` fields).
4. Orchestrator instructions: consult the log + provenance before acting on any config finding; append a log entry on every hand-edit.
5. `AGENTS.md`: document the provenance/log contract alongside the existing "Changing shipped defaults" guardrail.

## Related

- `Plans/in-repo-vs-global-project-config.md` — the config vs. state split this builds on.
- `Plans/zen-mode.md` — the orchestrator's self-heal / judgment role.
- Shipped guardrail: `AGENTS.md` "Changing shipped defaults (existing installs don't get them for free)".
