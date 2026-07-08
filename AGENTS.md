# Shelbi

Shelbi is an open-source agent orchestrator built on tmux: you talk to one
orchestrator agent, which dispatches tasks to worker agents (Claude Code,
Codex, aider, anything with a CLI) running in parallel tmux panes, locally and
over SSH. The repo is a Rust workspace plus a Next.js marketing/docs site.

## Layout

- `crates/shelbi-cli`: the `shelbi` binary (CLI commands, hub daemon, dispatch)
- `crates/shelbi-core`: shared domain types
- `crates/shelbi-orchestrator`: orchestrator pane bootstrap, system prompt, branch and git-hook logic
- `crates/shelbi-state`: markdown + frontmatter state IO
- `crates/shelbi-tmux`: tmux send-keys / capture-pane abstractions
- `crates/shelbi-tui`: ratatui dashboard
- `crates/shelbi-palette`: fuzzy command palette (Ctrl+Space) for the TUI
- `crates/shelbi-agent`: pluggable agent CLI runners
- `crates/shelbi-ssh`: local/SSH command execution wrapper
- `site/`: Next.js marketing and docs site (has its own AGENTS.md)
- `scripts/`: install script; release scripts in `scripts/release/`
- `packaging/`: Homebrew tap source (`packaging/homebrew-shelbi/`)
- `docs/`: release runbooks
- `.shelbi/wt/`: workspace worktrees. Never touch another workspace's worktree.

## Build, test, lint

- `cargo build --workspace`
- `cargo test --workspace` (some shelbi-orchestrator tests drive a real tmux
  server; they skip silently if `tmux` is not on PATH)
- `cargo clippy --workspace --all-targets -- -D warnings`
- Site: `cd site && npm run lint && npm run build`

## Git discipline

- Never commit on `main`: a Shelbi-managed pre-commit hook rejects it. Work
  happens on task branches cut by dispatch (e.g. `jlong/<task-id>`) or
  `fix/<slug>` branches for hand fixes.
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
