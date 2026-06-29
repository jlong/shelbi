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

if [[ "$(uname -s)" == "Darwin" ]]; then
  echo "==> re-signing (macOS)"
  codesign --remove-signature "$INSTALL_PATH" 2>/dev/null || true
  codesign --sign - "$INSTALL_PATH"
fi

echo "==> $("$INSTALL_PATH" --version)"
