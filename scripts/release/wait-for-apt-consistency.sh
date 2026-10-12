#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/release/wait-for-apt-consistency.sh --base-url URL [--suite stable] \
         [--timeout 300] [--interval 15]

Poll a hosted APT repository until the signed InRelease and the index files it
references agree, then exit 0. This guards `apt-get update` against a transient
edge/CDN state where a freshly published InRelease is served alongside a stale
Packages.gz (or the reverse) while a deploy is still propagating, which apt
reports as "File has unexpected size (A != B). Mirror sync in progress?".

For each index file listed in InRelease's SHA256 block (Packages and
Packages.gz) the script fetches the served file and compares its size and
SHA256 against the values InRelease advertises. It retries until every file
agrees or the timeout elapses; on timeout it prints the observed vs. expected
sizes for each mismatching file and exits non-zero.
USAGE
}

base_url=
suite=stable
component=main
arch=amd64
timeout=300
interval=15

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-url)
      base_url=${2:-}
      shift 2
      ;;
    --suite)
      suite=${2:-}
      shift 2
      ;;
    --timeout)
      timeout=${2:-}
      shift 2
      ;;
    --interval)
      interval=${2:-}
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$base_url" ]]; then
  usage
  exit 2
fi

# Trim a trailing slash so URL joins stay clean.
base_url=${base_url%/}
dists_url="$base_url/dists/$suite"

workdir=$(mktemp -d)
trap 'rm -f "$workdir"/* 2>/dev/null || true; rmdir "$workdir" 2>/dev/null || true' EXIT

# Fetch a URL fresh (no intermediary cache) into $1. Returns curl's exit code.
fetch() {
  local out=$1 url=$2
  curl -fsSL \
    -H 'Cache-Control: no-cache' \
    -H 'Pragma: no-cache' \
    -o "$out" \
    "$url"
}

# Index files apt downloads, as they appear in InRelease's checksum block
# (paths are relative to dists/$suite).
index_paths=(
  "$component/binary-$arch/Packages"
  "$component/binary-$arch/Packages.gz"
)

# Expected size/hash for $1 from the SHA256 block of the InRelease in $2.
# Prints "<sha256> <size>" or nothing if the path is absent.
expected_for() {
  local path=$1 release=$2
  awk -v want="$path" '
    /^SHA256:/ { in_sha = 1; next }
    /^[^[:space:]]/ { in_sha = 0 }
    in_sha && NF == 3 && $3 == want { print $1, $2; exit }
  ' "$release"
}

deadline=$(( $(date +%s) + timeout ))
attempt=0

while :; do
  attempt=$((attempt + 1))
  consistent=1
  summary=""

  if ! fetch "$workdir/InRelease" "$dists_url/InRelease"; then
    consistent=0
    summary="could not fetch InRelease from $dists_url/InRelease"
  else
    for path in "${index_paths[@]}"; do
      exp_hash=
      exp_size=
      # `read` returns non-zero at EOF (empty output); tolerate it under set -e.
      read -r exp_hash exp_size < <(expected_for "$path" "$workdir/InRelease") || true
      if [[ -z "${exp_hash:-}" || -z "${exp_size:-}" ]]; then
        consistent=0
        summary+="${summary:+; }$path: not listed in InRelease SHA256 block"
        continue
      fi

      local_file="$workdir/$(basename "$path")"
      if ! fetch "$local_file" "$dists_url/$path"; then
        consistent=0
        summary+="${summary:+; }$path: could not fetch served file"
        continue
      fi

      act_size=$(wc -c < "$local_file" | tr -d '[:space:]')
      act_hash=$(sha256sum "$local_file" | awk '{print $1}')

      if [[ "$act_size" != "$exp_size" || "$act_hash" != "$exp_hash" ]]; then
        consistent=0
        summary+="${summary:+; }$path: served size $act_size vs InRelease $exp_size"
      fi
    done
  fi

  if [[ "$consistent" -eq 1 ]]; then
    echo "APT repository is consistent after ${attempt} attempt(s): InRelease and index files agree."
    exit 0
  fi

  now=$(date +%s)
  if [[ "$now" -ge "$deadline" ]]; then
    echo "::error::APT repository still inconsistent after ${timeout}s (${attempt} attempts): ${summary}" >&2
    echo "The served InRelease and index files did not agree before the deadline." >&2
    echo "This usually means the Vercel deploy serving $base_url has not finished propagating." >&2
    exit 1
  fi

  echo "APT repository not yet consistent (attempt ${attempt}): ${summary}; retrying in ${interval}s..."
  sleep "$interval"
done
