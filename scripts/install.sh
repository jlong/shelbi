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
