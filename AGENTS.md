# Shelbi

Shelbi is an open-source agent orchestrator built on tmux: you talk to one
orchestrator agent, which dispatches tasks to worker agents (Claude Code,
Codex, aider, anything with a CLI) running in parallel tmux panes, locally and
over SSH. The repo is a Rust workspace plus a Next.js marketing/docs site.

## Layout

- `crates/shelbi-cli`: the `shelbi` binary (command routing, wizard, hub daemon)
- `crates/shelbi-core`: shared domain types
- `crates/shelbi-orchestrator`: orchestrator bootstrap, dispatch, workspace
  lifecycle, workflow actions, and git integration
- `crates/shelbi-state`: markdown + frontmatter state IO
- `crates/shelbi-tmux`: tmux send-keys / capture-pane abstractions
- `crates/shelbi-tui`: ratatui dashboard
- `crates/shelbi-palette`: fuzzy command palette (Ctrl+P by default) for the TUI
- `crates/shelbi-agent`: pluggable agent CLI runners
- `crates/shelbi-ssh`: local/SSH command execution wrapper
- `site/`: Next.js marketing and docs site (has its own AGENTS.md)
- `scripts/`: install script; release scripts in `scripts/release/`
- `docs/`: release runbooks
- `<machine.work_dir>/.shelbi/wt/`: persistent workspace worktrees. Never touch
  another workspace's worktree.

## Build, test, lint

- `cargo build --workspace`
- `cargo test --workspace` (some shelbi-orchestrator tests drive a real tmux
  server; they skip silently if `tmux` is not on PATH)
- `cargo clippy --workspace --all-targets -- -D warnings`
- Site: `cd site && npm run lint && npm run build`

## Changing shipped defaults (existing installs don't get them for free)

Shelbi copies its default agent instructions, `zenmode.md`, workflows, keys, and
other config into each project at `shelbi init`. After that the project owns its
copy — users edit them freely and Shelbi's self-heal preserves those edits. So
**editing a shipped default template only reaches NEW projects.** Every
already-initialized project keeps its forked copy untouched.

When you change a default that existing projects should adopt, pair the template
edit with a **config-upgrade sniffer** so existing installs self-heal on next
boot (`crates/shelbi-cli/src/commands/config_upgrade.rs`; surfaces in
`config_surfaces.rs` already include `zenmode.md`, per-agent `instructions.md`,
workflows, and the global/project YAMLs):

- **Auto-healable** (deterministic, non-lossy rewrite): add an `AutoHeal` sniffer
  plus its write-back in `config_upgrade_apply.rs`. The on-start pass applies it
  and discloses a `config-upgrade` line per project on `events.log`.
- **Needs judgment** (user-customizable prose, ambiguous, or potentially lossy —
  most instruction / `zenmode.md` changes): add a `NeedsJudgment` sniffer. It is
  written to the findings file the orchestrator ingests at boot, and the
  orchestrator repairs its own copy with judgment, preserving customizations.
  When unsure, classify `NeedsJudgment` rather than risk a lossy auto-heal.

A PR that edits a `*.template` / default config (or a shipped workflow /
instructions file) but adds no config-upgrade sniffer is incomplete for existing
users. Ship both.

## Git discipline

- Never commit on `main`. At `shelbi init` (with disclosure) Shelbi installs a
  context-scoped pre-commit guard in the hub checkout; it blocks commits to a
  protected branch ONLY from inside a Shelbi-managed agent pane (which exports
  `SHELBI_MANAGED_CONTEXT`), so a human's plain-shell commits are never
  governed. It's refresh-only on later opens (never silently created), removable
  with `shelbi guard uninstall`, cleaned up on teardown, and never overwrites a
  user-authored hook. Work happens on task branches cut by dispatch (e.g.
  `jlong/<task-id>`) or `fix/<slug>` branches for hand fixes.
- `main` is protected: changes land only via squash-merged PRs.

## Conventions

- "Shelbi" capitalized in prose; lowercase `shelbi` for the binary, paths, and
  commands.
- No em dashes in site copy or docs prose.
- Releases: the tag `vX.Y.Z` must equal the workspace Cargo version (enforced
  by `scripts/release/check-version.sh`). Runbook: `docs/release.md`.

## Special branches

- `docs/planning` is an orphan branch holding the ContextStore mirror
  (markdown only, no code). Never rebase or merge it with `main`.
