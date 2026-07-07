# Release Runbook

Shelbi releases are created from Git tags that match `vMAJOR.MINOR.PATCH`.
The tag version must match the Cargo package version for the `shelbi` binary.

## Maintainer Dry Run

Before tagging, run the same validation path that the release workflow uses:

```bash
cargo test --workspace
cargo build --release --bin shelbi
./target/release/shelbi --version
GITHUB_REF_NAME=v0.1.0 scripts/release/check-version.sh
goreleaser check
goreleaser release --snapshot --clean
```

Maintainers can also run the `release` workflow manually with `workflow_dispatch`
and a synthetic `ref_name` such as `v0.1.0`. That path validates the tag/version,
runs tests, smoke-tests supported targets, and builds snapshot artifacts without
publishing a GitHub Release.

## Publishing

1. Update the Cargo workspace version.
2. Run the dry-run checks above.
3. Create a signed release tag, for example `git tag -s v0.1.0`.
4. Push the tag. Only tags matching `v*.*.*` start the publishing path.

The release job publishes:

- macOS arm64 and x86_64 tarballs.
- Linux x86_64 tarball.
- Debian `.deb` package for amd64.
- `checksums.txt`.
- GitHub artifact attestations generated from `checksums.txt`.

## Permissions And Protection

The workflow defaults to `contents: read`. The publishing job is the only job
with `contents: write`, `id-token: write`, `attestations: write`, and
`artifact-metadata: write`.

Configure a protected `release` environment before publishing real tags. Require
maintainer approval for that environment. Use a separate protected
`release-downstream` environment for Homebrew and APT publication.

## Required Secrets

No long-lived secret is required for GitHub Release creation or artifact
attestation. The workflow uses the repository `GITHUB_TOKEN` plus OIDC-backed
GitHub artifact attestations.

Downstream publication needs these secrets once the external repositories exist:

- `TAP_GITHUB_TOKEN`: fine-scoped token or GitHub App token that can write only
  to the Homebrew tap repository.
- `APT_REPO_TOKEN`: fine-scoped token or GitHub App token that can write only to
  the APT hosting repository.
- `APT_GPG_PRIVATE_KEY`: ASCII-armored private key for signing APT metadata.
- `APT_GPG_PASSPHRASE`: passphrase for the APT private key.

Keep the APT signing key separate from maintainer Git tag-signing keys.

## Downstream Publication

Homebrew and APT publication are deliberately gated until the downstream repos,
domains, and secrets are provisioned. The release workflow has a downstream
preparation job that records the required state after GitHub Release publication.

For Homebrew, publish a formula in a dedicated tap such as
`jlong/homebrew-shelbi` or `shelbi/homebrew-shelbi` that points to the GitHub
Release tarballs and their SHA256 checksums.

For APT, publish the `.deb` into a signed static repository with this shape:

```text
pool/main/s/shelbi/shelbi_VERSION_amd64.deb
dists/stable/InRelease
dists/stable/Release
dists/stable/Release.gpg
dists/stable/main/binary-amd64/Packages
dists/stable/main/binary-amd64/Packages.gz
shelbi-archive-keyring.gpg
```

APT repository metadata must be signed with the APT signing key.
