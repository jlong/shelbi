# Shelbi APT Repository Runbook

Shelbi's Ubuntu packages are published through a signed static APT repository.

## Public Endpoint

- APT repository: `https://apt.shelbi.dev/`
- Hosting repository: `jlong/shelbi-apt` (set the `APT_REPO` repository
  variable). Served by Vercel — see [Hosting](#hosting).
- Suite: `stable`
- Component: `main`
- Architecture: `amd64`

The repository is static and must contain:

```text
shelbi-archive-keyring.gpg
shelbi-archive-keyring.fingerprint
pool/main/s/shelbi/*.deb
dists/stable/InRelease
dists/stable/Release
dists/stable/Release.gpg
dists/stable/main/binary-amd64/Packages
dists/stable/main/binary-amd64/Packages.gz
```

## Hosting

`apt.shelbi.dev` is served by **Vercel** (not GitHub Pages). The `shelbi-apt`
Vercel project (team `32pixels`) is git-connected to the `jlong/shelbi-apt`
repository, so every publish commit (see [Publish Flow](#publish-flow))
auto-deploys the static tree. Vercel provisions and renews the TLS certificate
for `apt.shelbi.dev` automatically (Let's Encrypt), so the endpoint always
serves a valid per-hostname cert.

DNS: `apt.shelbi.dev` is a `CNAME` to the target shown in the project's Domains
settings (a `*.vercel-dns-*.com` host). There is no `CNAME` file in the repo and
no GitHub Pages configuration. Pages previously served the domain with only a
`*.github.io` fallback cert, which failed hostname verification — the reason the
install-verify job failed on every release before this migration.

A `vercel.json` at the repo root serves the tree verbatim (no `cleanUrls` or
`trailingSlash` rewrites) and sets cache headers: `pool/**` (versioned `.deb`s)
is cached immutably; `dists/**` metadata (`Release`, `InRelease`, `Packages`) is
`must-revalidate` so `apt` never fetches a stale index. Vercel also purges its
CDN on each deploy.

## Signing Key

Use a dedicated OpenPGP key for APT repository metadata only. Do not reuse a
developer tag-signing key.

Generate the key on a trusted maintainer machine:

```bash
export GNUPGHOME="$(mktemp -d)"
chmod 700 "$GNUPGHOME"
gpg --batch --pinentry-mode loopback --passphrase "$APT_GPG_PASSPHRASE" \
  --quick-generate-key "Shelbi APT Repository <release@shelbi.dev>" rsa4096 sign 2y
gpg --batch --pinentry-mode loopback --passphrase "$APT_GPG_PASSPHRASE" \
  --armor --export-secret-keys > shelbi-apt-private.asc
gpg --export > shelbi-archive-keyring.gpg
gpg --fingerprint "Shelbi APT Repository"
```

Store `shelbi-apt-private.asc` as the `APT_GPG_PRIVATE_KEY` GitHub Actions
secret and its passphrase as `APT_GPG_PASSPHRASE`. The release workflow exports
and publishes `shelbi-archive-keyring.gpg` and
`shelbi-archive-keyring.fingerprint` into the APT repository on every publish.

## Required Secrets

Configure these in the main Shelbi repository:

- `APT_REPO_TOKEN`: token or GitHub App token with write access to `shelbi/apt`.
- `APT_GPG_PRIVATE_KEY`: ASCII-armored private key for the dedicated APT key.
- `APT_GPG_PASSPHRASE`: passphrase for that private key.

Repository variables:

- `APT_REPO`: the hosting repository, for example `jlong/shelbi-apt`. The
  `apt-publish` job in the release workflow only runs when this is set; the
  manual backfill workflow defaults to `<owner>/shelbi-apt`.
- `APT_BASE_URL`: override the public URL, default `https://apt.shelbi.dev`.

## Publish Flow

The `apt-publish` job in `.github/workflows/release.yml` runs after the GitHub
Release is published on a `v*` tag push, once `APT_REPO` and the secrets above
are provisioned. `.github/workflows/release-apt.yml` provides the same path as
a manual backfill, dispatched with a release tag. (A `release: published`
trigger would not work for the automated path: GitHub Releases created with the
workflow `GITHUB_TOKEN` do not start new workflow runs.) The publish path:

1. Verifies the `v*` tag matches the workspace Cargo version.
2. Downloads the release `.deb` matching `shelbi_*_amd64.deb`.
3. Copies it into `pool/main/s/shelbi/`.
4. Generates `Packages` and `Packages.gz`.
5. Generates `Release` with `Origin`, `Label`, `Suite`, `Codename`,
   `Architectures`, `Components`, and `Description`.
6. Signs `Release` into `InRelease` and `Release.gpg`.
7. Commits and pushes the static repository.
8. Verifies Ubuntu can install with `apt update && apt install shelbi`.

Daemon/service auto-install is intentionally out of scope. The package installs
the `shelbi` binary; users explicitly opt into `shelbi daemon install`.
