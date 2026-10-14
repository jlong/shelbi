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
finished work back for you to review. Each workspace is a persistent Shelbi
session with its own git worktree; Shelbi switches that worktree to each task's
branch so tasks can be reviewed and merged independently. Attach to any session
from any terminal with `shelbi attach <workspace>`. A workspace machine needs
`git` and its configured agent CLI, and nothing else: Shelbi runs its own
sessions, so there is no dependency beyond the binary.

## Install

Shelbi publishes prebuilt releases for macOS (Apple silicon and Intel) and
Ubuntu amd64.

### macOS: Homebrew

```bash
brew tap jlong/shelbi
brew trust jlong/shelbi
brew install shelbi
```

### macOS: the desktop app

Shelbi also has a native macOS app (Apple silicon, macOS 15 or later), a
second client over the same daemon and sessions as the TUI. It needs the CLI
above; the cask depends on it.

```bash
brew install --cask shelbi-desktop
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

`git` and an agent CLI such as `claude` or `codex` on the hub and every remote
workspace machine. Shelbi runs its own sessions, so no terminal multiplexer is
required; it also runs fine inside tmux or Screen if you prefer one.

## Documentation

The docs at [shelbi.dev](https://shelbi.dev) are the source of truth.

- [Getting started](https://shelbi.dev/docs/guides/getting-started): install, first project, first task
- [Concepts](https://shelbi.dev/docs/concepts/orchestrator): orchestrator, workspaces, agents, workflows
- [Workflows](https://shelbi.dev/docs/guides/understanding-workflows): feature-branch, trunk-based, forking, git-flow
- [Configuration](https://shelbi.dev/docs/configuration/project): project and task config reference

## Contributing

Shelbi is a Cargo workspace under `crates/`, a separate Cargo workspace for
the macOS desktop app under `desktop/`, and a Next.js site under `site/`.

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd site && npm ci && npm run lint && npm run build
```

The desktop app builds and tests from its own directory, on its own pinned
toolchain (`desktop/rust-toolchain.toml`); the root `--workspace` commands
don't see it:

```bash
cd desktop
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
```

See [AGENTS.md](AGENTS.md) for the crate layout and contributor conventions,
and [desktop/README.md](desktop/README.md) for the desktop app.
Bugs and feature requests: <https://github.com/jlong/shelbi/issues>.

## License

MIT. See [LICENSE](LICENSE).
