# dt-wizard-cli-setup: the first launch's CLI setup

## The change

A Mac without the `shelbi` CLI (the app's zip on its own, no Homebrew
formula) used to reach a failure screen telling the user to run `brew
install`. A first launch with no project skipped bootstrap altogether and
opened the Add Project wizard, which then had no CLI to work with.

Now a first launch opens the unnumbered "Let's get Shelbi ready" screens
(Figma "CLI Check" 2478:8507, "CLI Install" 2480:8522, "CLI Ready"
2480:8577) in the wizard's 800 × 580 window:

```
 ● ● ●                                   ● ● ●                                   ● ● ●
        Let's get Shelbi ready                  Let's get Shelbi ready                  Let's get Shelbi ready

       Checking for Shelbi CLI…                    Installing CLI…                           Ready ✓
       ━━━━─────────────────────                ━━━━━━━━━━━━━━━━──────               ━━━━━━━━━━━━━━━━━━━━━━
```

Bootstrap runs as before (login env, discovery, daemon, control
connection, version and protocol checks). When discovery finds nothing and
nothing configures a CLI, it installs the release matching the app, then
runs discovery again and checks the new CLI like any other. Ready shows only
when bootstrap reached `Ready`, then the window moves on by itself: with no
project registered it becomes the Add Project wizard's 01 (the same window,
its root replaced); with a project (made from a terminal meanwhile) the
launch's normal windows open and this one closes. A returning user's launch
(saved windows) never sees these screens.

## How it is built

- **`platform/install.rs`**: the installer. The plan is the app's own
  version (`v<APP_VERSION>`, never "latest"), `shelbi_Darwin_<arch>.tar.gz`
  and `checksums.txt` from `github.com/jlong/shelbi/releases/download`,
  installed at `~/bin/shelbi` and `~/share/shelbi/plugins/update-shelbi-
  configuration/` (the prefix `scripts/install.sh` uses, where the CLI looks
  for its system plugin). Steps: download (checksums first), verify the
  SHA-256, unpack with `/usr/bin/tar` into a private staging directory,
  check the package (a regular `shelbi` file, the three plugin files the
  release check requires, `--version` names the app's version), then
  publish: the plugin swapped in by rename, the executable copied to a fresh
  file and renamed over `~/bin/shelbi` (a new inode: macOS kills a signed
  binary rewritten in place), then the installed CLI run again. Any failure
  puts back what was there. A process guard plus an advisory lock file
  (`~/share/shelbi/.desktop-install.lock`) make a second job fail as Busy;
  the next install sweeps an interrupted one's leftovers. The download is
  `/usr/bin/curl` (HTTPS only for the official release),
  `SHELBI_DESKTOP_CLI_DOWNLOAD_URL` swaps the base for a fixture.
- **`platform/bootstrap.rs`**: `BootstrapState::InstallingCli { step }`
  between `FindingCli` and a second `FindingCli`; `Failure::InstallFailed`.
  `may_install` keeps the old diagnosis whenever something configures a CLI:
  a `desktop.cli_path` that isn't there, or an unreadable `config.yaml` (it
  might set one). A found CLI of the wrong version keeps its version or
  protocol screen; it is never installed over. `Ready::on_login_path` says
  whether a terminal finds the CLI by name.
- **`platform/discovery.rs`**: `~/bin` joins the known locations (after
  `~/.cargo/bin`), so the install is found on this launch and every later
  one. **`platform/env.rs`** `with_dir_on_path`: the found CLI's directory
  is appended to the children's `PATH` (the daemon, `shelbi` commands).
- **`views/cli_setup.rs`**: the screens. A pure `status(state,
  installing)` gives the label and the bar (the frames' ten segments: Check
  1, Install 7, Ready 10), and the checks after an install stay "Installing
  CLI…" so a finished download never reads as ready. A failure shows
  bootstrap's failure screen for it (`views/bootstrap.rs` gained the install
  failures' copy) with Try Again, also Return; a retry only starts from a
  failure, and a stale run's events are dropped. The event listener
  outlives the view, so the daemon's relaunch and detach pushes still reach
  the app after the window became the wizard; the project windows reuse the
  run's result (`app::share_ready`) instead of bootstrapping again.
- **`views/add_project.rs`**: `replace_root` makes the setup window the
  wizard (title "Add Project", origin `FirstLaunch`); the single-instance
  record holds any window, so Add Project raises the setup window too.
- **`views/settings.rs`**: when the running CLI isn't on the login `PATH`,
  the Shelbi CLI helper says so and gives the `export PATH=…` line. The app
  never edits shell startup files.

## Decisions

- **Install only when nothing points elsewhere.** A configured path that
  misses, an unreadable config, or an incompatible CLI keep their actionable
  screens; overwriting them silently would hide the user's own setup.
- **No auto-update of a CLI the app installed.** A desktop-installed CLI
  that falls behind an upgraded app shows the usual version screen. The
  installer's rollback is there for a reinstall over earlier files (a source
  install's plugin, a broken `~/bin/shelbi`).
- **The children's `PATH` gets the CLI's directory**, not a fixed `~/bin`:
  that covers the install and a Homebrew prefix missing from the login
  shell alike.
- **Ready shows for 0.7 s before moving on**, so the check mark is seen.
  The bar's fractions follow bootstrap's real steps; nothing advances on a
  timer.
- **The failure screen is bootstrap's own**, scrolled inside the 800 × 580
  window (Figma has no frame for it); `cli-failed` in the visual harness
  shows it.

## Verified

- `cargo build`, `cargo clippy --all-targets -- -D warnings` (also with
  `--features visual-check`), `cargo test` (799 passed).
- Installer tests on temporary homes with a goreleaser-shaped fixture: a
  first install, checksum mismatch and an unlisted archive, network and 404
  failures, a package missing a plugin file or of another version, an
  unwritable `~/bin` (rolled back, then a retry installs), a failed
  post-install check (the previous CLI and plugin restored), Busy under a
  held lock and the in-process guard, and an interrupted install's
  leftovers swept.
- Bootstrap tests (scripted platform): a found CLI is never installed over;
  a first install then rediscovery; a failed install, then a retry; a
  configured path and an unreadable config not installed over; an
  incompatible CLI keeps its verdict; an install discovery still can't find.
- Integration tests (real discovery, curl over `file://`, tar, daemon start
  and control hello against a stand-in daemon): a missing CLI installed and
  reaching Ready, then a relaunch finding it with the release gone; an
  unreachable release failing with nothing installed.
- View tests: check then Add Project, install then Add Project, failure,
  Return retries once, stale events dropped, a returning user restoring the
  launch windows, Ready shared with the project windows.
- `cargo test live_release -- --ignored`: the real v0.11.0 release from
  GitHub installed into a temporary home (checksum, plugin layout,
  `--version`).
- The real app with a sandboxed `HOME` and Shelbi root and no CLI anywhere:
  first launch installed v0.11.0 from GitHub into the sandbox's `~/bin`,
  started its daemon and reached Ready; a relaunch found it in `~/bin` and
  installed nothing. (Window screenshots weren't possible here; the states
  come from the app's log.)
- `SHELBI_VISUAL_ONLY=cli-setup --visual-figma` against the three frames:
  0.80%, 0.73% and 0.67% of pixels differ, all in the title (Figma's
  Geist 1.2 is wider), the status label's antialiasing, and the traffic
  lights an offscreen window doesn't draw. Light mode uses the tokens'
  light values (the `progress/default` fill is Pink/500).
