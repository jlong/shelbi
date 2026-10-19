#!/usr/bin/env bash
# The terminal performance bench, in one command: build the app with the
# `bench` feature (release) and run every case, printing the report and its
# Markdown table. See src/bench/mod.rs for what each case measures.
#
# Usage: desktop/scripts/bench-term.sh [bench options]
#   --input claude,yes,...   inputs to stream (default all four)
#   --source direct|session  where the output comes from (default both)
#   --skip-stream / --skip-idle / --skip-many
#   --json <file>            append the results as one JSON line
#   --label <text>           name the run in the table
#   $SHELBI_BENCH_CLI        the `shelbi` CLI that runs the sandboxed sessions
#                            (default: desktop.cli_path, then PATH)
#
# It opens a window (without taking focus) for about two minutes. Sessions run
# in a sandboxed root under /tmp with the hub's socket variables unset; nothing
# touches ~/.shelbi or the live hub.

set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --features bench
exec ./target/release/shelbi-desktop --bench-term "$@"
