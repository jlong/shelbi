#!/usr/bin/env bash
# Build shelbi and install it.
#
# Default install location: $HOME/bin/shelbi
# Override with: SHELBI_INSTALL_PATH=/somewhere/else ./scripts/install.sh
#
# Why the codesign dance on macOS:
#   `cargo build` produces a binary with an ad-hoc embedded code signature.
#   `cp` modifies the file enough to invalidate it, and on subsequent exec
#   the kernel SIGKILLs the process ("Killed: 9") with no useful error.
#   Re-signing ad-hoc after the copy restores it. Linux/Windows unaffected.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_PATH="${SHELBI_INSTALL_PATH:-$HOME/bin/shelbi}"

# Parse flags. --no-daemon leaves the daemon untouched; by default we restart
# an already-running daemon onto the freshly built binary (useful in CI,
# containers, or anywhere the user wants to drive the daemon manually).
install_daemon=1
for arg in "$@"; do
  case "$arg" in
    --no-daemon) install_daemon=0 ;;
    -h|--help)
      cat <<'USAGE'
Usage: scripts/install.sh [--no-daemon]

Builds shelbi in release mode and installs it to $SHELBI_INSTALL_PATH
(default: $HOME/bin/shelbi).

The hub daemon is started on demand (the first time you open a project) and
exits when no project is open, so there is no service to register. If a daemon
is already running when you upgrade, it keeps serving the OLD binary until it
restarts, so this script restarts it onto the new binary.

Options:
  --no-daemon   Leave the daemon completely untouched. An already-running
                daemon keeps serving the previous binary until it next
                restarts (or you run `shelbi daemon restart`).
  -h, --help    Show this message.
USAGE
      exit 0
      ;;
    *)
      echo "error: unknown argument: $arg" >&2
      echo "       run \`$0 --help\` for usage." >&2
      exit 1
      ;;
  esac
done

cd "$REPO_ROOT"

# ----------------------------------------------------------------------------
# Prompt for the shelbi root directory (the global state dir that holds
# projects, agents, logs, state.json). The path is baked into the binary as
# the default; at runtime it can still be overridden with $SHELBI_ROOT or
# `shelbi --root <path>`.
#
# Non-interactive runs (no TTY on stdin, or SHELBI_DEFAULT_ROOT already
# exported) skip the prompt and use the caller's value (or the default).

default_root="$HOME/.shelbi"
shelbi_root="${SHELBI_DEFAULT_ROOT:-}"

if [[ -z "$shelbi_root" ]]; then
  echo "==> shelbi root directory"
  echo
  echo "  Where should shelbi store global state (projects, agents, logs, state.json)?"
  echo "  This path is baked into the binary as the default; you can override at"
  echo "  runtime with \$SHELBI_ROOT or \`shelbi --root <path>\`."
  echo
  if [[ -t 0 ]]; then
    read -r -p "  Shelbi root? [$default_root]: " entered || entered=""
    shelbi_root="${entered:-$default_root}"
  else
    echo "  (non-interactive: using $default_root)"
    shelbi_root="$default_root"
  fi
fi

# Expand a leading ~ against $HOME (the prompt accepts `~/foo`).
case "$shelbi_root" in
  "~")        shelbi_root="$HOME" ;;
  "~/"*)      shelbi_root="$HOME/${shelbi_root#~/}" ;;
esac

# Validate: absolute path with a writable parent. We don't require the path
# itself to exist — shelbi creates it on first use via `ensure_root_subdirs`.
if [[ "$shelbi_root" != /* ]]; then
  echo "error: shelbi root must be an absolute path (got: $shelbi_root)" >&2
  exit 1
fi
parent_dir="$(dirname "$shelbi_root")"
if [[ ! -d "$parent_dir" ]]; then
  echo "error: parent directory $parent_dir does not exist" >&2
  exit 1
fi
if [[ ! -w "$parent_dir" ]]; then
  echo "error: parent directory $parent_dir is not writable" >&2
  exit 1
fi

echo "==> baking shelbi root: $shelbi_root"
export SHELBI_DEFAULT_ROOT="$shelbi_root"

echo "==> building release"
cargo build --release

echo "==> installing to $INSTALL_PATH"
mkdir -p "$(dirname "$INSTALL_PATH")"
cp target/release/shelbi "$INSTALL_PATH"

INSTALL_PREFIX="$(dirname "$(dirname "$INSTALL_PATH")")"
PLUGIN_PARENT="$INSTALL_PREFIX/share/shelbi/plugins"
ASSET_ROOT="$PLUGIN_PARENT/update-shelbi-configuration"
echo "==> installing system plugin to $ASSET_ROOT"
mkdir -p "$PLUGIN_PARENT"
STAGED_ASSET_ROOT="$(mktemp -d "$PLUGIN_PARENT/.update-shelbi-configuration.XXXXXX")"
cleanup_staged_asset() {
  rm -rf "$STAGED_ASSET_ROOT"
}
trap cleanup_staged_asset EXIT
cp -R plugins/update-shelbi-configuration/. "$STAGED_ASSET_ROOT"

# Replace the complete bundle so files removed by an upgrade cannot remain in
# the active plugin. If this is interrupted between removal and rename, the
# binary's embedded fallback keeps Shelbi usable until the install is retried.
rm -rf "$ASSET_ROOT"
mv "$STAGED_ASSET_ROOT" "$ASSET_ROOT"
trap - EXIT

if [[ "$(uname -s)" == "Darwin" ]]; then
  echo "==> re-signing (macOS)"
  codesign --remove-signature "$INSTALL_PATH" 2>/dev/null || true
  codesign --sign - "$INSTALL_PATH"
fi

echo "==> $("$INSTALL_PATH" --version)"

# ----------------------------------------------------------------------------
# The hub daemon is started on demand and has no installed service. The only
# thing an upgrade needs to do is restart an already-running daemon onto the
# freshly built binary — otherwise it keeps serving the OLD binary we just
# replaced (the "stale binary hides merged fixes" failure mode) until it next
# restarts. A daemon that isn't running is left down; it starts on demand the
# next time a project is opened. `--no-daemon` skips even the restart.
#
# `daemon status` always exits 0, so we key off its printed marker:
# `shelbi daemon: running`. A not-running daemon matches neither and is left be.
daemon_status="$("$INSTALL_PATH" daemon status 2>/dev/null || true)"
if [[ "$daemon_status" =~ shelbi\ daemon:\ running ]]; then
  if [[ $install_daemon -eq 1 ]]; then
    echo
    echo "==> restarting the running daemon onto the new binary"
    # `daemon restart` stops the running daemon and starts a fresh one on this
    # binary directly (no supervisor), also retiring any leftover launchd/
    # systemd unit from a previous install so it can't respawn the old binary.
    "$INSTALL_PATH" daemon restart
  else
    echo
    echo "note: a hub daemon is running on the previous binary; it will keep"
    echo "      serving it until it restarts. Run \`shelbi daemon restart\` to"
    echo "      put it on the new binary (--no-daemon: not touching it)."
  fi
else
  echo
  echo "==> hub daemon is on-demand; it starts the next time you open a project"
fi
