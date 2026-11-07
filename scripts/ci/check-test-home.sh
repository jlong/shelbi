#!/usr/bin/env bash
# Fail when a test run reached the Shelbi home of the user running it.
#
# Usage:
#   scripts/ci/check-test-home.sh [cargo test args]
#       Run the root workspace suite under a throwaway $HOME, then check it.
#       Safe from a worker pane: the live hub's home, sockets and environment
#       are out of the run's reach, and the live hub's own writes can't be
#       mistaken for a leak.
#   scripts/ci/check-test-home.sh --verify
#       Only check the current $HOME, after a suite that ran under it. This is
#       what CI does, on a runner whose home starts with no Shelbi state.
#
# Why this exists next to the guard in `shelbi_state::root`: that guard covers
# what a test binary does in-process. This check also sees the `shelbi` binaries
# a test spawns, and any path that never went through the resolver.

set -uo pipefail

verify() {
  local home="$1"
  local leaked
  leaked="$(find "$home/.shelbi" -mindepth 1 2>/dev/null)"
  if [ -n "$leaked" ]; then
    echo "error: the test run wrote into the Shelbi home at $home/.shelbi:" >&2
    echo "$leaked" >&2
    echo "Every test must run under its own SHELBI_HOME or SHELBI_ROOT, and every" >&2
    echo "shelbi binary a test spawns must be given one." >&2
    return 1
  fi
  echo "ok: the test run left $home/.shelbi alone"
}

if [ "${1:-}" = "--verify" ]; then
  verify "$HOME"
  exit $?
fi

# Short, because tests bind Unix sockets under paths derived from it and macOS
# caps those near 100 bytes.
sandbox="$(mktemp -d /tmp/sbh-XXXXXX)"
trap 'rm -rf "$sandbox"' EXIT

# The toolchain, gh's login and git's identity stay the real ones; only the
# place a Shelbi home would land moves. The rest is what a worker pane exports,
# which a run must not inherit. gh's token is read now because macOS finds the
# login keychain through $HOME, and a few tests ask gh for it.
gh_token="${GH_TOKEN:-$(gh auth token 2>/dev/null || true)}"
GH_TOKEN="$gh_token" env -u SHELBI_ROOT -u SHELBI_HOME \
  -u SHELBI_HUB_SOCK -u SHELBI_HUB_ADDR -u SHELBI_CONTROL_SOCK \
  -u SHELBI_BIN -u SHELBI_MANAGED_CONTEXT -u SHELBI_AGENT \
  -u TASK_ID -u PROJECT \
  HOME="$sandbox" \
  CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" \
  RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
  GH_CONFIG_DIR="${GH_CONFIG_DIR:-$HOME/.config/gh}" \
  GIT_CONFIG_GLOBAL="${GIT_CONFIG_GLOBAL:-$HOME/.gitconfig}" \
  cargo test --workspace "$@"
tests=$?

verify "$sandbox"
home=$?

[ "$tests" = 0 ] && [ "$home" = 0 ]
