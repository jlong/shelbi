# Release Runbook

This is the operational runbook for publishing a Shelbi release. Shelbi is a
single-maintainer project; there is no release rota, maintainer channel, or
separate signing-key owner. The maintainer performs the steps below.

Shelbi releases are **tag-driven and published by CI**. Pushing a tag that
matches `vMAJOR.MINOR.PATCH` to `jlong/shelbi` runs
[`.github/workflows/release.yml`](../.github/workflows/release.yml), which builds,
verifies, and publishes every artifact. **You do not run `goreleaser release
--clean` by hand** — GoReleaser only runs locally for snapshot dry runs
(`--snapshot`). The tag version must equal the Cargo workspace version for the
`shelbi` binary; `scripts/release/check-version.sh` enforces this in CI and you
can run it locally.

The latest release is `v0.9.0` (2026-08-25). Read the current version from
`Cargo.toml` and use it wherever a version appears below:

```bash
VERSION="$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "shelbi") | .version')"
echo "$VERSION"   # e.g. 0.9.0
```

## The release pipeline

A tag push runs these jobs in `release.yml`:

1. **`validate`** — checks out the repo, validates the tag against the Cargo
   version (`scripts/release/check-version.sh`), captures release metadata, runs
   `cargo test --workspace`, and runs `goreleaser check`.
2. **`smoke`** — cross-builds and runs `shelbi --version` for
   `x86_64-unknown-linux-gnu`. (macOS runners queue too long to gate releases;
   the shipped darwin binaries are cross-compiled in the publish job and the
   maintainer verifies them on real hardware.)
3. **`release`** — runs in the **`release`** environment on `macos-14`
   (darwin targets need a real macOS SDK; Linux cross-compiles from macOS via
   `cargo-zigbuild`). It runs `goreleaser release --clean`, verifies
   `checksums.txt` and the `.deb` exist, runs
   `scripts/release/verify-system-plugin-packages.sh`, and attests the artifacts
   with `actions/attest@v4` over `checksums.txt`.
4. **`homebrew-pr`** and **`apt-publish`** — run in the
   **`release-downstream`** environment, each gated on a repository variable (see
   [Downstream publication](#downstream-publication)).
5. **`apt-verify-install`** — installs Shelbi from the live APT repository and
   asserts the binary and the three system-plugin files are present.
6. **`downstream-publication`** — always runs after the above and records the
   downstream result in the job log.

The `workflow_dispatch` path (a manual run with a synthetic `ref_name` such as
`v0.9.0`) is the **dry-run path**: it runs `validate` and `smoke`, then a
`goreleaser-dry-run` job that builds snapshot artifacts on `macos-14` without
publishing a GitHub Release.

## Permissions and environments

The workflow defaults to `contents: read`. Only the `release` job holds
`contents: write`, `id-token: write`, `attestations: write`, and
`artifact-metadata: write`.

Two GitHub Actions environments scope the publishing jobs and hold their
secrets and variables. Both exist in repository settings:

- **`release`** — the GitHub Release publish job.
- **`release-downstream`** — the Homebrew tap PR, APT publish, and
  downstream-publication jobs.

Neither environment has a protection rule today, so CI runs these jobs without
pausing for a manual gate. If you want a manual gate, add a required-reviewer
rule to either environment in repository settings (Settings → Environments);
the run will then wait for your approval before that job starts.

## Secrets

No long-lived secret is required for GitHub Release creation or artifact
attestation. The `release` job uses the repository `GITHUB_TOKEN` plus
OIDC-backed GitHub artifact attestations. No cosign/Sigstore signing is
configured.

Downstream publication (already provisioned) needs:

- `TAP_GITHUB_TOKEN` — fine-scoped token or GitHub App token that can write to
  and merge PRs on the Homebrew tap (`contents: write` + `pull-requests: write`
  on the tap).
- `APT_REPO_TOKEN` — fine-scoped token or GitHub App token that can write to the
  APT hosting repository.
- `APT_GPG_PRIVATE_KEY` — ASCII-armored private key for signing APT metadata.
- `APT_GPG_PASSPHRASE` — passphrase for the APT private key.

The APT signing fingerprint is **derived** in CI from the imported key by
`scripts/release/export-apt-public-key.sh`; it is not a stored secret. Keep the
APT signing key separate from the Git tag-signing key. Rotate publishing tokens
and the APT key immediately after any suspected exposure or a failed release that
may have printed a secret to logs.

## Local dry run

Before cutting a release, reproduce the CI validation path locally. GoReleaser
snapshot builds need `zig` and `cargo-zigbuild` on your `PATH` for the
cross-compiled targets:

```bash
cargo test --workspace
cargo build --release --bin shelbi
./target/release/shelbi --version
RELEASE_TAG="v$VERSION" scripts/release/check-version.sh
goreleaser check
goreleaser release --snapshot --clean
```

You can also trigger the `release` workflow manually (`workflow_dispatch`) with a
synthetic `ref_name` such as `v$VERSION` to run the same validation and snapshot
build in CI without publishing.

Expected snapshot output under `dist/`:

- `shelbi_Darwin_arm64.tar.gz`
- `shelbi_Darwin_x86_64.tar.gz`
- `shelbi_Linux_x86_64.tar.gz`
- `shelbi_${VERSION}_amd64.deb`
- `checksums.txt`

Each archive and the `.deb` carry the system-plugin bundle; CI asserts this with
`scripts/release/verify-system-plugin-packages.sh dist`, which you can run
locally too.

## Local artifact verification

Verify archive contents and checksums from a snapshot build:

```bash
shasum -a 256 -c dist/checksums.txt
tar -tzf dist/shelbi_Linux_x86_64.tar.gz | grep '^shelbi$'
tar -xzf dist/shelbi_Linux_x86_64.tar.gz -C /tmp
/tmp/shelbi --version
rm /tmp/shelbi
```

Verify the Debian package:

```bash
DEB=$(find dist -name 'shelbi_*_amd64.deb' | head -n 1)
dpkg-deb --info "$DEB"
dpkg-deb --contents "$DEB"
dpkg-deb --field "$DEB" Package | grep '^shelbi$'
dpkg-deb --field "$DEB" Version | grep "^$VERSION"
dpkg-deb --field "$DEB" Architecture | grep '^amd64$'
```

Install and run the package in a clean Debian container:

```bash
docker run --rm -v "$PWD/dist:/dist:ro" debian:bookworm bash -euxo pipefail -c '
  apt-get update
  apt-get install -y /dist/shelbi_*_amd64.deb
  shelbi --version
  command -v shelbi
  test -f /usr/share/shelbi/plugins/update-shelbi-configuration/.claude-plugin/plugin.json
'
```

## Cutting a release

`main` is protected by a PR-only ruleset, so the version bump lands through a
squash-merged pull request, not a direct push.

1. Curate the changelog. `site/content/docs/changelog.mdx` is hand-maintained and
   updates nowhere else: add or finish this version's entry (a feature-level,
   newest-first note dated to when the work landed on `main`, folding internal
   refactors into the capability they enabled).
2. On a branch, bump the Cargo workspace version to the new `VERSION` and include
   the changelog entry. Open a PR and squash-merge it into `main`.
3. Sync `main` and confirm the version:

   ```bash
   git fetch origin main --tags
   git checkout main
   git pull --ff-only origin main
   git status --short   # must print nothing
   VERSION="$(cargo metadata --no-deps --format-version 1 \
     | jq -r '.packages[] | select(.name == "shelbi") | .version')"
   ```

4. Run the [local dry run](#local-dry-run) and
   [artifact verification](#local-artifact-verification).
5. Tag the merged commit and push. Only tags matching `v*.*.*` start the
   publishing path, and the `release` job asserts the tag points at the commit it
   checks out, so tag the exact commit on `main`:

   ```bash
   git tag -a "v$VERSION" -m "Shelbi v$VERSION"
   git push origin "v$VERSION"
   ```

6. Watch the run. No environment approval is configured, so the jobs proceed
   without a manual gate (unless you have added a required-reviewer rule as
   described under [Permissions and environments](#permissions-and-environments)).

## Verifying a published release

After the run completes:

```bash
gh release view "v$VERSION" --repo jlong/shelbi
gh release download "v$VERSION" --repo jlong/shelbi --dir "/tmp/shelbi-v$VERSION"
cd "/tmp/shelbi-v$VERSION"
shasum -a 256 -c checksums.txt
```

Published GitHub Release artifacts:

- Darwin `arm64` archive
- Darwin `x86_64` archive
- Linux `x86_64` archive
- Debian `amd64` package
- `checksums.txt`
- GitHub artifact attestation over `checksums.txt`

Verify the installable paths:

```bash
# Homebrew
brew update
brew install jlong/shelbi/shelbi   # after `brew tap jlong/shelbi`
shelbi --version
brew uninstall shelbi

# APT (the apt-verify-install job runs this in CI; re-run to confirm by hand)
docker run --rm debian:bookworm bash -euxo pipefail -c '
  apt-get update
  apt-get install -y ca-certificates curl gnupg
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://apt.shelbi.dev/shelbi-archive-keyring.gpg \
    -o /etc/apt/keyrings/shelbi-archive-keyring.gpg
  echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/shelbi-archive-keyring.gpg] https://apt.shelbi.dev stable main" \
    > /etc/apt/sources.list.d/shelbi.list
  apt-get update
  apt-get install -y shelbi
  shelbi --version
'
```

The installed version must match `VERSION`.

## Downstream publication

Homebrew and APT publication run as jobs in the release workflow, each gated on a
repository variable so they skip cleanly if unset. Both downstream repositories
are provisioned:

- **Homebrew:** the `homebrew-pr` job runs when the `HOMEBREW_TAP_REPOSITORY`
  variable is set (`jlong/homebrew-shelbi`; users run `brew tap jlong/shelbi`).
  It bumps `Formula/shelbi.rb` with `scripts/release/update-homebrew-formula.rb`,
  opens a PR on the tap, waits for the tap's `test` check with `gh pr checks
  --watch`, and squash-merges it (deleting the branch). If the check goes red,
  the PR conflicts, or the merge is blocked, the job fails loudly. The tap must
  run a check named `test` on PRs and `TAP_GITHUB_TOKEN` must be allowed to
  merge. See [docs/release/homebrew-tap.md](release/homebrew-tap.md).
- **APT:** the `apt-publish` job runs when the `APT_REPO` variable names the
  hosting repository (`jlong/shelbi-apt`). It downloads the `.deb`, imports the
  signing key, builds a signed static repository with
  `scripts/release/build-apt-repo.sh`, asserts the required layout, and pushes to
  the hosting repo. `apt.shelbi.dev` (the `APT_BASE_URL` variable, default
  `https://apt.shelbi.dev`) is served by Vercel from that repository. The
  published layout is:

  ```text
  pool/main/s/shelbi/shelbi_VERSION_amd64.deb
  dists/stable/InRelease
  dists/stable/Release
  dists/stable/Release.gpg
  dists/stable/main/binary-amd64/Packages
  dists/stable/main/binary-amd64/Packages.gz
  shelbi-archive-keyring.gpg
  shelbi-archive-keyring.fingerprint
  ```

  See [docs/release-apt.md](release-apt.md) for the full APT runbook.

## Rollback

### Bad GitHub artifacts

If an artifact is bad **before** the release is announced or a package manager
has consumed it, delete the release and re-run from a corrected tag. Since
publishing is tag-driven, deleting the release and re-pushing the tag re-runs
`release.yml`:

```bash
gh release delete "v$VERSION" --repo jlong/shelbi --yes
git push origin ":refs/tags/v$VERSION"
git tag -d "v$VERSION"
# retag the correct commit on main, then:
git tag -a "v$VERSION" -m "Shelbi v$VERSION"
git push origin "v$VERSION"
```

After public announcement or package-manager publication, do **not** replace or
delete GitHub release assets and never move a published tag. Ship a patch
version instead: bump the Cargo version via a PR, then tag the new version.

### Bad Homebrew formula

Revert the formula commit in the tap and push the revert:

```bash
cd homebrew-tap   # a checkout of jlong/homebrew-shelbi
git pull --ff-only
git log --oneline -- Formula/shelbi.rb
git revert BAD_FORMULA_COMMIT_SHA
brew audit --strict --online Formula/shelbi.rb
git push origin HEAD
```

If the formula points at a bad GitHub artifact, complete the GitHub artifact
rollback first, then re-run or re-trigger the formula bump.

### Bad APT package

Republish APT metadata so the repository no longer advertises the bad version.
Leave the bad `.deb` in `pool/` for auditability unless it is actively harmful.
APT does not automatically downgrade an installed package; once two versions
exist, document the manual downgrade for users:

```bash
apt-cache madison shelbi
sudo apt install shelbi=PREVIOUS_GOOD_VERSION
sudo apt-mark hold shelbi
```

Then publish a fixed patch version through the normal tag-driven flow.

### APT key compromise

Treat a suspected signing-key exposure as a release incident:

1. Freeze APT publication and remove the repository write credentials
   (`APT_REPO_TOKEN`, `APT_GPG_PRIVATE_KEY`, `APT_GPG_PASSPHRASE`) from the
   `release-downstream` environment.
2. Revoke the compromised key if a revocation certificate exists.
3. Generate a new offline signing key and store its revocation certificate
   securely.
4. Export the new public key as `shelbi-archive-keyring.gpg`.
5. Replace `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` (and any deployment
   secret that could have accessed the old private key).
6. Re-sign the repository metadata with the new key.
7. Publish the new keyring at `apt.shelbi.dev`.
8. Publish user-facing migration instructions that replace the old keyring file
   before running `sudo apt update`.

New key material:

```bash
gpg --batch --full-generate-key
gpg --list-secret-keys --keyid-format LONG
NEW_APT_SIGNING_KEY_ID=...   # the new fingerprint
gpg --output shelbi-archive-keyring.gpg --export "$NEW_APT_SIGNING_KEY_ID"
gpg --output shelbi-archive-revocation.asc --gen-revoke "$NEW_APT_SIGNING_KEY_ID"
```

Do not delete the old public key from public history. Users need a clear
migration path from the old key to the new one.
