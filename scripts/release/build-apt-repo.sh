#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/release/build-apt-repo.sh --repo-dir DIR --deb PATH [--suite stable]

Build and sign Shelbi's static APT repository metadata.

Required environment:
  APT_GPG_PASSPHRASE   Passphrase for the imported APT signing key.

Optional environment:
  APT_GPG_KEY_ID       Fingerprint/key id to use for signing.
  GNUPGHOME            GnuPG home containing the imported private key.
USAGE
}

repo_dir=
deb_path=
suite=stable
component=main
arch=amd64

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo-dir)
      repo_dir=${2:-}
      shift 2
      ;;
    --deb)
      deb_path=${2:-}
      shift 2
      ;;
    --suite)
      suite=${2:-}
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

if [[ -z "$repo_dir" || -z "$deb_path" ]]; then
  usage
  exit 2
fi

if [[ -z "${APT_GPG_PASSPHRASE:-}" ]]; then
  echo "APT_GPG_PASSPHRASE is required" >&2
  exit 2
fi

for bin in apt-ftparchive gzip gpg install; do
  if ! command -v "$bin" >/dev/null 2>&1; then
    echo "$bin is required" >&2
    exit 127
  fi
done

if [[ ! -f "$deb_path" ]]; then
  echo "deb does not exist: $deb_path" >&2
  exit 2
fi

mkdir -p "$repo_dir"
repo_dir=$(cd "$repo_dir" && pwd)
deb_path=$(cd "$(dirname "$deb_path")" && pwd)/$(basename "$deb_path")

pool_dir="$repo_dir/pool/main/s/shelbi"
binary_dir="$repo_dir/dists/$suite/$component/binary-$arch"
release_dir="$repo_dir/dists/$suite"

install -d -m 0755 "$pool_dir" "$binary_dir" "$release_dir"
install -m 0644 "$deb_path" "$pool_dir/"

(
  cd "$repo_dir"
  apt-ftparchive packages "pool/$component/s/shelbi" > "dists/$suite/$component/binary-$arch/Packages"
  gzip -9cn "dists/$suite/$component/binary-$arch/Packages" > "dists/$suite/$component/binary-$arch/Packages.gz"

  release_conf=$(mktemp)
  trap 'rm -f "$release_conf"' EXIT
  cat > "$release_conf" <<EOF
APT::FTPArchive::Release::Origin "Shelbi";
APT::FTPArchive::Release::Label "Shelbi";
APT::FTPArchive::Release::Suite "$suite";
APT::FTPArchive::Release::Codename "$suite";
APT::FTPArchive::Release::Architectures "$arch";
APT::FTPArchive::Release::Components "$component";
APT::FTPArchive::Release::Description "Shelbi APT repository";
EOF

  apt-ftparchive -c "$release_conf" release "dists/$suite" > "dists/$suite/Release"

  rm -f "dists/$suite/InRelease" "dists/$suite/Release.gpg"
  gpg_args=(
    --batch
    --yes
    --pinentry-mode loopback
    --passphrase "$APT_GPG_PASSPHRASE"
  )
  if [[ -n "${APT_GPG_KEY_ID:-}" ]]; then
    gpg_args+=(--local-user "$APT_GPG_KEY_ID")
  fi

  gpg "${gpg_args[@]}" --clearsign \
    -o "dists/$suite/InRelease" \
    "dists/$suite/Release"
  gpg "${gpg_args[@]}" -abs \
    -o "dists/$suite/Release.gpg" \
    "dists/$suite/Release"
)

echo "APT repository metadata signed in $repo_dir"
