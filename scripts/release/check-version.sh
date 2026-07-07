#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
manifest_path="$repo_root/crates/shelbi-cli/Cargo.toml"
tag="${GITHUB_REF_NAME:-}"

if [[ -z "$tag" ]]; then
  echo "error: GITHUB_REF_NAME is not set; release version check must run from a tag context." >&2
  exit 1
fi

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: release tag '$tag' is malformed; expected vMAJOR.MINOR.PATCH, for example v0.1.0." >&2
  exit 1
fi

cargo_version="$(
  REPO_ROOT="$repo_root" MANIFEST_PATH="$manifest_path" python3 <<'PY'
import json
import os
import subprocess
import sys

repo_root = os.environ["REPO_ROOT"]
manifest_path = os.path.realpath(os.environ["MANIFEST_PATH"])

metadata = subprocess.check_output(
    ["cargo", "metadata", "--no-deps", "--format-version", "1"],
    cwd=repo_root,
    text=True,
)

for package in json.loads(metadata)["packages"]:
    if os.path.realpath(package["manifest_path"]) == manifest_path:
        print(package["version"])
        break
else:
    print(
        f"error: could not find Cargo package for {manifest_path}",
        file=sys.stderr,
    )
    sys.exit(1)
PY
)"

tag_version="${tag#v}"

if [[ "$cargo_version" != "$tag_version" ]]; then
  echo "error: release tag '$tag' does not match shelbi CLI Cargo version '$cargo_version'." >&2
  echo "       Update crates/shelbi-cli/Cargo.toml or create tag 'v$cargo_version'." >&2
  exit 1
fi

echo "release tag '$tag' matches shelbi CLI Cargo version '$cargo_version'."
