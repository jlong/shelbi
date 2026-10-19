# dt-packaging: a signed, notarized Shelbi.app, its release job, and its cask

## What landed

- **`scripts/bundle.sh`** builds the distributable app: `cargo build --release
  --locked --target aarch64-apple-darwin`, then `target/bundle/Shelbi.app`
  (executable `Contents/MacOS/Shelbi`, `Info.plist` from the new
  `macos/Shelbi.app.plist` with the version from `Cargo.toml`, `Shelbi.icns`
  cut from the icon master with `sips` + `iconutil`, `PkgInfo`). It bundles no
  CLI. Then it signs with Developer ID, inside-out (any nested Mach-O, deepest
  first, then the bundle; never `--deep`) with `--options runtime
  --timestamp`, notarizes (`notarytool submit --wait`), staples, validates
  (`stapler validate`, `spctl --assess`), runs the bundled binary's
  `--print-version` as a smoke check, and zips with `ditto -c -k --keepParent`
  to `Shelbi-<version>-arm64.zip` plus `.sha256` (sha256sum format). It writes
  `bundle-result.env` (and `$GITHUB_OUTPUT` in CI): `version`, `zip`,
  `sha256`, `signed` (`developer-id` | `adhoc`), `notarized`.
  - Each step runs only when its credentials are present, and the script ends
    by naming the build: `SIGNED + NOTARIZED + STAPLED`, `SIGNED, NOT
    NOTARIZED`, or `UNSIGNED (ad-hoc)`. Absent credentials are a loud warning
    (a `::warning::` annotation in Actions).
  - Notary credentials: `NOTARY_PROFILE` (a keychain profile), or an App Store
    Connect API key (`NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`, and the `.p8` as a
    path in `NOTARY_KEY_PATH` or base64 in `NOTARY_KEY_P8`).
  - Credentials that are present but wrong fail the script, and
    `REQUIRE_NOTARIZATION=1` (set by CI once all secrets exist) fails anything
    short of notarized.
- **The icon**: `assets/icon/shelbi.svg` (source) and `shelbi-1024.png` (the
  committed master), rendered by `scripts/render-icon.sh` (ImageMagick).
- **`--print-version`** (`src/platform/print_version.rs`): prints
  `Shelbi <version>` and the CLI check (`shelbi CLI: <path> <version>
  (matches)`), then exits 0 on a match and 1 otherwise. No window, no daemon.
  It reuses bootstrap's discovery (`bootstrap::find_cli`, factored out of
  `bootstrap::run`).
- **Update notice** (`src/platform/update.rs`, `src/views/update.rs`): once a
  day, on the background executor, `curl` asks
  `api.github.com/repos/jlong/shelbi/releases/latest`. When the tag is newer
  than the app, every project window shows a bar atop its main area:
  "Shelbi X is available (you have Y). Upgrade with `brew upgrade --cask
  shelbi-desktop`" with a copy button and a dismiss button (dismissing hides
  it in every window until the next launch). Otherwise it shows nothing. The
  answer is cached in `~/.shelbi/desktop-update-check.json` (`checked_at`,
  `latest`); a failed ask still counts as the day's check and keeps the last
  answer. `SHELBI_DESKTOP_UPDATE_URL` points the check at a fixture (any URL
  curl takes, `file://` included; it bypasses the cache both ways), and
  `SHELBI_DESKTOP_UPDATE_CHECK=0` turns it off. New icon:
  `arrow-down-circle` (Heroicons v2.2.0 outline).
- **Release workflow** (`.github/workflows/release.yml`):
  - `desktop-release` (new): after `release` on a tag push, or after
    `validate` + `smoke` on a `workflow_dispatch` dry run. It runs on
    `macos-15` in the `release` environment, checks for all five secrets
    (`MACOS_CERTIFICATE_P12`, `MACOS_CERTIFICATE_PASSWORD`, `NOTARY_KEY_P8`,
    `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`), imports the `.p12` into a temporary
    keychain, runs `bundle.sh`, uploads the zip and `.sha256` as a workflow
    artifact, and attaches them to the GitHub release when notarized (on a dry
    run it prints what it would attach). The keychain is deleted at the end.
    Without the secrets: a warning annotation, an unsigned build, green.
  - `homebrew-pr` (changed): now `needs: [release, desktop-release]` with
    `always() && needs.release.result == 'success'`, so a failed desktop job
    never holds back the formula. A new step, run only when `desktop-release`
    notarized, downloads the release's `.sha256` and runs
    `scripts/release/update-homebrew-cask.rb` into the same tap checkout. The
    PR step adds `Casks/shelbi-desktop.rb` and mentions the cask in the PR
    body.
  - `desktop-verify-install` (new): on a clean `macos-15` runner after the tap
    PR merges (notarized builds only). It taps, installs
    `<tap>/shelbi <tap>/shelbi-desktop`, runs `spctl --assess` and `stapler
    validate`, clears the quarantine, and runs `open -W --stdout <file> -a
    /Applications/Shelbi.app --args --print-version`, asserting
    `Shelbi <version>` and `(matches)`.
  - `downstream-publication` reports the desktop result too.
- **Cask generator** `scripts/release/update-homebrew-cask.rb` (a sibling of
  the formula script): `--tap-dir`, `--version`, `--sha256` or
  `--sha256-file` (checks the file names the expected zip), `--github-repo`,
  `--tap`, and `--url` (local testing only). It writes
  `Casks/shelbi-desktop.rb`, creating `Casks/`. The cask:
  `depends_on arch: :arm64`, `formula: "jlong/shelbi/shelbi"`,
  `macos: :sequoia`; `app "Shelbi.app"`; `zap trash:`
  `~/.shelbi/desktop-update-check.json` and
  `~/.shelbi/projects/*/desktop-window.json`; plus a `livecheck` on GitHub
  releases. The URL keeps `#{version}` literal, so a bump touches only
  `version` and `sha256`.
- **Docs**: `desktop/README.md` (Packaging, Update notice, architecture map),
  `docs/release.md` (the new jobs, permissions, and the five secrets: what
  each holds and how to produce it), and `docs/release/homebrew-tap.md` (the
  cask, how users install it, tap CI, the by-hand flow). `docs/gpui.md` notes
  that the bundle (with `CFBundleIdentifier`) launches without the
  notification abort the bare binary hits.

## Decisions

- **Only a notarized zip is attached to the release.** The task says to
  attach the zip, and to run the cask step only when notarized. I also gated
  the attach on notarization: an ad-hoc zip downloaded from a release page is
  quarantined and Gatekeeper reports it as damaged, so publishing one helps no
  one. Un-notarized builds are still kept as workflow artifacts.
- **`macos-15` for both desktop jobs, not `macos-14`.** The app's floor is
  macOS 15 (`LSMinimumSystemVersion`, `MACOSX_DEPLOYMENT_TARGET`), bundle.sh
  runs the bundled binary as a smoke check, and the cask's `depends_on
  macos: :sequoia` refuses to install on 14.
- **All five secrets, or no signing at all** (as the task says: skip sign and
  notarize when any is absent). With all five, `REQUIRE_NOTARIZATION=1` makes
  a mistyped identity or a rejected submission fail the job rather than ship
  an unsigned app.
- **`depends_on formula: "jlong/shelbi/shelbi"`, fully qualified** (the task
  wrote `"shelbi"`), so a homebrew-core formula of that name could never
  satisfy it. The tap name comes from `HOMEBREW_TAP_REPOSITORY`
  (`owner/homebrew-x` → `owner/x`).
- **`depends_on macos: :sequoia`, not `">= :sequoia"`.** Current Homebrew
  deprecates the string form ("Use `depends_on macos: :sequoia` instead"),
  and the symbol already means "this or later". The stanza order is the one
  `brew style` requires.
- **Users install with both names**: `brew install jlong/shelbi/shelbi
  jlong/shelbi/shelbi-desktop`. Current Homebrew (7.x) only loads a
  third-party tap's formulae and casks that were named on the command line
  or trusted with `brew trust`. Naming only the cask stopped with "Refusing
  to load formula jlong/shelbi/shelbi from untrusted tap". The verify job
  does the same.
- **The verify job clears the quarantine after `spctl` passes.** A
  quarantined first launch waits on macOS's "downloaded from the Internet"
  confirmation (seen here: it appeared on screen for the quarantined test
  copy), which nobody clicks on a runner.
- **The icon** is the brand's pixel "S" (the shape of `site/app/icon.svg`)
  redrawn on an exact 10x9 grid so it stays crisp at 16px: `#D5D5D5` on a
  black rounded square, the site icon's two colors, on the 1024 macOS grid.
  The PNG master is committed so bundling needs only `sips`/`iconutil`.
- **The update check shells out to `/usr/bin/curl`** rather than adding an
  HTTP client for one GET a day; `file://` fixtures come free with it. The
  bar sits in the main column rather than across the window top, where it
  would collide with the transparent titlebar's traffic lights.
- **`--print-version` stops before the daemon**: starting one on a build or
  CI machine has side effects, and the daemon's version follows the CLI's.
- **Desktop secrets live on the `release` environment** (or the repository).
  `desktop-release` declares `environment: release`, like `release`. Neither
  environment has a protection rule, so a dispatch dry run on a branch still
  runs.

## Verification

All on this machine (macOS 26.6.2, Apple silicon), 2026-10-14.

- From `desktop/`, after rebasing onto `a2790602` (dt-board-dnd): `cargo
  build` ok; `cargo clippy --all-targets -- -D warnings` clean; `cargo test`:
  150 passed, 0 failed (128 before the rebase; new:
  `platform::update` 7, `platform::print_version` 3; the existing bootstrap
  tests still pass after `find_cli` was factored out). The `update` tests
  include a real `curl` fetch of a `file://` fixture (notice for v99, silence
  for v0.10.0) and the cache/interval policy with an injected clock.
- No crate under `crates/` changed and neither `Cargo.lock` moved (`git diff
  origin/jlong/desktop-app...HEAD -- crates Cargo.lock desktop/Cargo.lock` is
  empty), so the root-workspace checks don't apply.
- `scripts/release/check-version.sh` with `RELEASE_TAG=v0.10.0`: passes for
  both packages.
- **Signed, notarized, stapled build** (acceptance 1): `desktop/scripts/bundle.sh`
  with the Developer ID identity from the login keychain and the team's App
  Store Connect API key passed by path (`NOTARY_KEY_PATH`). One submission:
  **id `c870f853-7d9f-4fa5-be08-13f71ae12fa3`, status `Accepted`**. Stapled;
  `stapler validate` ok; `spctl --assess --type exec`: `accepted,
  source=Notarized Developer ID`. `codesign -dv`: `Identifier=dev.shelbi.desktop`,
  `flags=0x10000(runtime)`, timestamped, `TeamIdentifier=6L5M94R7K7`. Zip
  sha256 `a48595622034bbb1c16f7f922eb0078cdc31b893565356311d5a3b14adb7fe80`.
  Release build: 30 minutes cold and 10 minutes incremental here under a
  load average of ~70 (other workers building).
- **Without credentials** (acceptance 1): with no notary credentials it
  printed the `NOT NOTARIZED` warning and `SIGNED, NOT NOTARIZED`. With no
  identity (`SIGN_IDENTITY` naming a cert that isn't there), `UNSIGNED
  (ad-hoc)`.
- **Launched from the bundle** (acceptance 2): `open -n` the bundle in a
  sandbox `SHELBI_HOME` (`env -i`, so no hub socket leaked in). The window
  reached the "can't find the shelbi command" bootstrap screen (with
  `desktop.cli_path` pointed at a missing path, so no daemon started), and the
  app did not hit the notification abort. `lsappinfo`: `LSDisplayName=Shelbi`,
  `dev.shelbi.desktop`; `NSRunningApplication`: `localizedName=Shelbi`, regular
  (Dock) activation policy, and its icon is the Shelbi mark (macOS 26 adds its
  glass treatment).
- **Release workflow dry run** (acceptance 3): pushing a scratch branch and
  dispatching `release.yml` was **refused by this session's permission
  classifier**, so it was not run on GitHub. Instead I ran the
  `desktop-release` job's own `run:` scripts, extracted from `release.yml`,
  with the dry-run inputs and no secrets. The emulation gave the
  missing-secrets warning annotation (`sign=false`); bundle.sh produced an
  `UNSIGNED` build with outputs `signed=adhoc notarized=false`; both
  upload-artifact globs matched; and the attach step logged "Not attaching
  ... not notarized". The attach step on a notarized dry run prints "Dry run:
  would attach Shelbi-0.10.0-arm64.zip and Shelbi-0.10.0-arm64.zip.sha256 to
  v0.10.0". The emulation caught a real bug: bundle.sh exited 1 after a
  successful run (an `EXIT` trap's failing `&&` under `set -e`), now fixed.
  `actionlint` 1.7.7 on `release.yml`: only the two findings that predate
  this change (SC2129 in `validate`, the `artifact-metadata` scope). Still
  to do: a real dispatch,
  `gh workflow run release.yml --ref <branch> -f ref_name=v0.10.0`.
- **Cask** (acceptance 4): the generator wrote the cask with version 0.10.0
  and the notarized zip's sha256 into a scratch copy of `jlong/homebrew-shelbi`
  (alongside the formula script, as `homebrew-pr` runs them), and refuses a
  `.sha256` for another version. With a throwaway Homebrew (HEAD, its own
  prefix, cache, and trust store): `brew style` reports no offenses and
  `brew audit --strict --cask` passes. **The `brew install` itself was not
  completed here.** The throwaway prefix's formula install aborted with
  Homebrew's "Inherited sandbox permits writes to .../brew" (the agent
  sandbox), and the session's permission classifier refused the workarounds.
  Installing into the real `/opt/homebrew` would have put a second `shelbi`
  on the PATH the live hub uses, so I didn't. What was verified instead: the
  notarized zip, unpacked with `ditto` and given a download quarantine, passes
  `spctl` (`source=Notarized Developer ID`) and showed macOS's "Apple checked
  it for malicious software" first-launch prompt. Then `open -W --stdout ... -a
  Shelbi.app --args --print-version` printed `Shelbi 0.10.0` / `shelbi CLI:
  .../shelbi 0.10.0 (matches)` and exited. This is what
  `desktop-verify-install` asserts. Still to do: a real `brew install` from a
  local tap copy, or the first tagged release's `desktop-verify-install`.
- **Update notice** (acceptance 5): from the bundle and the debug build,
  sandbox `SHELBI_HOME`, `SHELBI_DESKTOP_UPDATE_URL=file://.../release.json`.
  With `v0.11.0` the bar appears atop the main area ("Shelbi 0.11.0 is
  available (you have 0.10.0). Upgrade with `brew upgrade --cask
  shelbi-desktop`"). With `v0.10.0` there is no bar. An earlier unpinned
  launch asked the real API and cached `latest: 0.10.0` (silent).

## What's left

- A real `workflow_dispatch` dry run of `release.yml` on GitHub, and a real
  `brew install` of the cask from a local tap (both blocked in this session;
  see Verification).
- The five secrets in the `release` environment (names in
  `docs/release.md`). Until they exist, tags ship no desktop zip and no cask.
- The tap's CI audits only `Formula/**`. `docs/release/homebrew-tap.md` has
  the steps to add `Casks/**` with a cask audit and install. That's a change
  to `jlong/homebrew-shelbi`, not made here.
- Notifications (`dt-menus-notifications`): the bundle carries
  `NSUserNotificationAlertStyle` and a usage string, and now launches with a
  bundle identifier, so `UNUserNotificationCenter` should work from the
  bundle.
- The relaunch bar still spans the window top and overlaps the traffic lights
  (pre-existing, from `dt-platform-bootstrap`); the update bar avoids that by
  sitting in the main column.
