#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/release/export-apt-public-key.sh --output DIR

Import APT_GPG_PRIVATE_KEY into a temporary keyring, export the public key as
shelbi-archive-keyring.gpg, and write shelbi-archive-keyring.fingerprint.

Required environment:
  APT_GPG_PRIVATE_KEY  ASCII-armored private key.
USAGE
}

output_dir=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --output)
      output_dir=${2:-}
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

if [[ -z "$output_dir" ]]; then
  usage
  exit 2
fi

if [[ -z "${APT_GPG_PRIVATE_KEY:-}" ]]; then
  echo "APT_GPG_PRIVATE_KEY is required" >&2
  exit 2
fi

if ! command -v gpg >/dev/null 2>&1; then
  echo "gpg is required" >&2
  exit 127
fi

tmp_gnupg=$(mktemp -d)
trap 'rm -rf "$tmp_gnupg"' EXIT
chmod 700 "$tmp_gnupg"

printf '%s\n' "$APT_GPG_PRIVATE_KEY" | gpg --batch --homedir "$tmp_gnupg" --import
fingerprint=$(
  gpg --batch --homedir "$tmp_gnupg" --with-colons --fingerprint \
    | awk -F: '/^fpr:/ { print $10; exit }'
)

if [[ -z "$fingerprint" ]]; then
  echo "could not determine APT key fingerprint" >&2
  exit 1
fi

install -d -m 0755 "$output_dir"
gpg --batch --homedir "$tmp_gnupg" --export "$fingerprint" > "$output_dir/shelbi-archive-keyring.gpg"
printf '%s\n' "$fingerprint" > "$output_dir/shelbi-archive-keyring.fingerprint"

echo "$fingerprint"
