# tui-settings-machine-readiness: the connection check in the TUI

The TUI's Add machine and Configure machine forms (`tui-settings-machine-forms`)
said a draft was Not checked and had no way to check it. They now run the
desktop's readiness check (`dt-machine-readiness-setup`): Test connection
checks the draft as typed, lists what it found per stage, and offers Install
CLI when that can help. The check and its region moved out of the desktop so
both clients run one implementation, over the one SSH engine
(`shelbi_orchestrator::machine`); there is no second SSH path.

## Shared

- **`shelbi_app::settings::machine_check`** (new), the desktop's
  `machine_form_state` check region and `machine_form::CheckStop`, moved:
  - `ReadinessForm`: the shared draft with the check over it (`Deref` to
    `machine_form::MachineForm`); `begin_check` / `finish_check` / `set`
    bind a result to the draft's host and directory, `stop_check` (new) stops
    a running check, `panel()` says the region's tone, title and detail, and
    `stages()` (new) lists SSH, Platform, Shelbi CLI, Git and Project
    directory with each one's state (waiting, running, passed, needs
    attention, failed, not checked) and a few words of result.
  - `Report` (replaces the desktop's use of the orchestrator's `Readiness`
    in the form): how far SSH got (`Reach`: reached, invalid, unreachable,
    timed out, login refused, host key, cancelled), the stages, the
    actionable messages, whether Install CLI helps, and the verified binary.
    `is_ready()` needs SSH reached, every stage passed and a verified
    binary: the only state shown as "Connection successful".
  - `CheckOutcome::{Checked, Installed { note, changed, report },
    InstallFailed}`; `side_effect(host)` says what an install changed on the
    machine for someone who no longer sees the form.
  - `ReadinessRequest` carries `record_as` (the configured machine, when the
    draft still names its saved host) so a check's record decision no longer
    needs the form that asked; `SaveVerified` is what a save started now
    records once it commits.
  - `ReadinessService { check, record }` with `run` (check, then record what
    it verified of a saved machine), `CheckStop`, and `validate_target`
    (moved from the orchestrator; `usable_ssh_host` moved to
    `shelbi_core::pool`).
- **`shelbi_orchestrator::machine_readiness`**:
  - `Readiness::TimedOut` (new): the 30s deadline is no longer read as
    "couldn't connect".
  - `report(&Readiness, &target)` turns the engine's answer into the shared
    `Report`; on a platform with no release the CLI's message (which offers
    Install CLI) gives way to the platform's.
  - `install_outcome` / `install_note` / `install_failure`, moved from the
    desktop (the copy says "this computer" off macOS).
  - `service()` (live: the user's `ssh`, the hub's `curl` for the no-network
    copy, `machine_state` records) and `service_with(connect, fetch, record,
    hub)` for scripted tests; `record_verified`.
  - `fake` (feature `test-support`): scripted remotes (`Remote`, `Script`
    with a `hold` that waits for the stop flag, `connector`) and hub fetches
    (`Fetch`), used by this crate's tests and the TUI's.

## The desktop

`views/machine_form_state.rs` is now the shared form (`MachineForm` is
`ReadinessForm` with Choose…) and its samples, built through the real
`report()`. `views/machine_form.rs` uses the shared `CheckStop`, the live
service's `check`, and the shared record decisions. No behavior changes,
except that a check that times out says "Connection timed out" rather than
"Couldn’t connect".

## The TUI

The SSH form's Connection region (`shell/settings/machine_form.rs`):

```
Connection         ✓ Connection successful
                   ✓ SSH               Connected and logged in
                   ✓ Platform          Linux x86_64
                   ✓ Shelbi CLI        shelbi 0.11.0 at
                                       /home/me/.shelbi/bin/shelbi
                   ✓ Git               git version 2.43.0
                   ✓ Project directory Checkout root
                   Installed Shelbi 0.11.0 at ~/.shelbi/bin/shelbi. Shelbi,
                   Git, and project directory are ready.
                   [ Test again ]
```

- **Test connection / Test again** (Tab to it, Enter or Space, or a click)
  checks the draft exactly as typed, saved or not. Before any check every
  stage says Not checked. While it runs the title spins with the seconds
  elapsed, SSH says Connecting to the host, the rest wait for it, and the
  button is **Stop check**. A failure says which stage and what to do: SSH
  couldn’t log in, Connection timed out, Couldn’t connect, Host key not
  trusted, Unsupported platform, Shelbi CLI needed (missing, too old, too
  new, no version), Git needed, Project directory needs attention; anything
  after a failed SSH stage is "Not checked", never a pass.
- **Install CLI** shows only when the check found no compatible CLI on a
  platform with a release (or an install failed in a way trying again can
  fix). It runs the shared setup (reuse a compatible install; install the
  matching release at `~/.shelbi/bin/shelbi`; leave a package-managed binary
  where it is; copy the release from this computer when the machine has no
  outbound network), then checks again. It can't be stopped: it runs to its
  end. Git, cloning and SSH config are never touched.
- **Off the loop**: the shell runs each request on its own thread
  (`SettingsAction::RunCheck` → `SettingsMsg::CheckDone`), so keys keep
  working; they all go to the modal form (nothing reaches a session). The
  page redraws while one runs, for the spinner and clock.
- **Binding**: an answer counts only for the form that asked (each form has
  an id) and the run in flight. Editing the host or directory, Stop check,
  Esc / Cancel, leaving Settings or switching project stops a check (its
  `ssh` is killed) and drops its answer, which records nothing either (in
  both clients); a new form is never answered by an old one's.
- **Nothing is saved** by a check or an install. Add machine / Save changes
  saves; if a check verified the draft's host, its binary is recorded
  (`machine_state`, off the UI thread) once the save commits. A check of a
  configured machine at its saved host records at once, as on the desktop.
- **Install side effects** are said even when nobody is looking at the
  result: closing the form during an install says it keeps running, and when
  it lands the page says what it did ("On gpu.lan: Installed Shelbi … It
  stays there; the machine’s configuration is only saved by Add machine or
  Save changes."); cancelling after one says the same; an install whose
  draft moved on says it on the form; after a project switch it goes to the
  status line.

## Intentional differences from the desktop

- The TUI lists the stages; the desktop shows the Figma region (icon, title,
  detail), the same panel text.
- The TUI has Stop check; the desktop disables Test while a check runs and
  stops it when the dialog closes or the draft moves.
- The TUI's page says what an install did after its form closed; the
  desktop's dialog doesn't outlive its view.

## Tests

- `shelbi-app` `settings::machine_check::tests`: validation, binding (stale,
  raced, superseded), install through edits, Stop check, panel titles for
  every failure, success needing every stage, records only for a saved
  machine (at once) or a verified save, the service's record, side effects,
  `CheckStop`, keys.
- `shelbi-orchestrator` `machine_readiness::tests`: the existing engine
  tests, plus the report for a ready machine, each connection failure (stages
  after SSH not checked), each remote problem as its own stage, and the
  scripted service: check then install then record, the hub-copy fallback
  beside a package binary, a failed install and a login failure, a stopped
  check; the install notes moved from the desktop.
- `shelbi-tui` `shell::settings::workspaces::tests` (the live service over
  scripted remotes): the unsaved draft checked with every stage drawn,
  each failure's stage and title with Install CLI only when offered, install
  then recheck with a failure retried and the no-network copy, Stop / edit /
  close stopping the check and dropping its answer, install side effects
  after close / cancel / a moved draft / a project switch, records after a
  save, a narrow terminal.
- `shelbi-tui` `shell::tests::a_machine_check_and_install_run_off_the_loop_and_save_nothing`:
  real key events with the remote held, keys still reaching the form, then
  the install, nothing written to the config before Add machine, the record
  after it.
- The desktop's `machine_form_state` samples through the real report, and
  its existing form and settings tests over the shared state.
