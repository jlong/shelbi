#!/bin/sh
# https://shelbi.dev/install.sh
#
# Bootstrap installer for shelbi. Clones the repo into a temp dir and runs
# scripts/install.sh, which builds a release binary and installs it to
# ~/bin/shelbi (override with SHELBI_INSTALL_PATH).
#
# Usage:
#   curl -fsSL https://shelbi.dev/install.sh | sh
#
# Prerequisites (same as scripts/install.sh):
#   - git
#   - cargo / rustup        (https://rustup.rs)
#   - tmux                  (shelbi runs workers in tmux panes)
#   - claude CLI            (the agent runtime shelbi drives)
#
# Notes:
#   - macOS: scripts/install.sh re-signs the binary after copy so the kernel
#     doesn't SIGKILL it ("Killed: 9"). No action needed on your part.
#   - This leaves the clone in a temp dir (TMPDIR or /tmp). It is not needed
#     after install; delete it later, or clone to a permanent path yourself
#     and run scripts/install.sh from there if you want to rebuild in place.
set -e

if ! command -v git >/dev/null 2>&1; then
  echo "error: git is required but was not found on PATH" >&2
  exit 1
fi

TMP=$(mktemp -d)
echo "==> cloning into $TMP"
git clone --depth=1 https://github.com/jlong/shelbi.git "$TMP/shelbi"

cd "$TMP/shelbi"
exec bash scripts/install.sh
