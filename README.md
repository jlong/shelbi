# Shelbi

```
   ▄▀▀▀▀▀▄   ▀▀    ▀▀  ▀▀▀▀▀▀▀   ▀▀   ▀▀▀▀▀▀▀▀▀▀▄   ▀▀▀▀▀
  ▀▀        ▀▀    ▀▀  ▀▀        ▀▀        ▀▀    ▀▀   ▀▀
  ▀▀▀▀▄    ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀    ▀▀      ▀▀▀▀▀▀▀▀▄    ▀▀
▄     ▀▀  ▀▀    ▀▀  ▀▀        ▀▀        ▀▀     ▀▀  ▀▀
 ▀▀▀▀▀▀  ▀▀    ▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀
```

> An open-source, personal agent orchestrator for the terminal.

[GitHub](https://github.com/jlong/shelbi) · [Docs](https://shelbi.dev)

## What is Shelbi

You talk to one agent, the orchestrator. It writes your work up as tasks,
dispatches them to worker agents (Claude Code, Codex, aider, anything with a
CLI) running in parallel across your machines, locally or over SSH, and brings
finished work back for you to review. Each task runs on its own git branch in
its own worktree, so you can review and merge independently. All you need on a
worker's machine is `tmux` and your agent CLI.

## Install

Shelbi publishes prebuilt releases for macOS and Ubuntu.

### macOS: Homebrew

```bash
brew tap jlong/shelbi
brew trust jlong/shelbi
brew install shelbi
```

### Ubuntu: APT

```bash
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://apt.shelbi.dev/shelbi-archive-keyring.gpg \
  | sudo tee /etc/apt/keyrings/shelbi-archive-keyring.gpg >/dev/null

echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/shelbi-archive-keyring.gpg] https://apt.shelbi.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/shelbi.list >/dev/null

sudo apt update
sudo apt install shelbi
```

For build-from-source, manual artifact verification, and the full walkthrough,
see the [install guide](https://shelbi.dev/docs/guides/getting-started/install).

## First run

Run `shelbi` in your repo. With no projects configured it drops you into a
setup wizard, then launches the TUI. See
[getting started](https://shelbi.dev/docs/guides/getting-started) for the walkthrough.

## Requirements

`tmux` (≥ 3.2), `git`, and an agent CLI such as `claude` or `codex` on every
machine a worker runs on.

## Documentation

The docs at [shelbi.dev](https://shelbi.dev) are the source of truth.

- [Getting started](https://shelbi.dev/docs/guides/getting-started): install, first project, first task
- [Concepts](https://shelbi.dev/docs/concepts/orchestrator): orchestrator, workspaces, agents, workflows
- [Workflows](https://shelbi.dev/docs/guides/understanding-workflows): feature-branch, trunk-based, forking, git-flow
- [Configuration](https://shelbi.dev/docs/configuration/project): project and task config reference

## Contributing

Shelbi is a Cargo workspace under `crates/` plus a Next.js site under `site/`.

```bash
cargo build --workspace
cargo test --workspace
./scripts/install.sh
```

See [CLAUDE.md](CLAUDE.md) for the crate layout and build, test, and lint
conventions. Bugs and feature requests: <https://github.com/jlong/shelbi/issues>.

## License

MIT. See [LICENSE](LICENSE).
