# dt-machine-readiness-setup: real connection checks and Install CLI

The Add machine and Configure machine forms (`dt-machine-settings-forms`)
had a connection check region that only asked whether SSH answered. It now
runs a real readiness check, and its green "Connection successful" (Figma
`2526:9557`, `2528:9870`) shows only after the remote has verified a
compatible Shelbi CLI, a usable Git and the project's root directory.

## The shared engine

`shelbi_orchestrator::machine_readiness`, on top of `machine` (the
`shelbi machine setup|status` engine):

- **`check(exec, hub, target)`** validates the draft first (a host `ssh`
  takes, never one starting with `-`; an absolute directory, a remote `~`
  refused as the save refuses it), then runs one script over the user's own
  `ssh` (config, keys, the shared ControlMaster): the existing platform and
  `shelbi` probe, Git on a plain `sh`'s PATH (as Shelbi's worktree commands
  run it, else the login shell's, reported as such), and the directory
  (`test`, `cd`, `git rev-parse --show-toplevel`, nothing else). The
  directory is shell-quoted. It returns a structured `Readiness`: `Invalid`,
  `Unreachable` (connect failure or the 30s deadline), `AuthDenied`,
  `HostKey` (new: "Host key verification failed" is no longer read as
  unreachable), `Cancelled`, or `Reached(RemoteReport)` with `CliStatus`,
  `GitStatus` and `DirStatus`. `issues()` lists what needs attention in
  order; `is_ready()` only when there are none. A connection failure carries
  nothing about the remote, so it can never read as a missing CLI.
- **`install_cli(exec, fetch, hub, target)`** runs the unchanged
  `machine::setup` (reuse a compatible install; install the matching release
  at `~/.shelbi/bin/shelbi`; leave an incompatible package-manager binary
  where it is; copy from the hub when the remote has no outbound network)
  and then checks again.
- **`check_host` / `install_cli_on_host`** are the live versions. A check
  takes a stop flag (`SshExec::with_cancel`, which kills the ssh process
  group); an install does not, and runs to its end.
- **`save_verified_record`** writes `machine_state`'s record only for a
  ready check, bound to its host.

`shelbi machine check <name>` prints the same result for a declared machine.

## Records

`MachineRecord` gains an optional `host`. Records are keyed by machine name,
a per-project label, so the machines model ignores a record taken on another
host (`is_for_host`) and the relay finds a labelled machine's binary by host
(`relay_bin_for_host`). Old records without the key read as before.

## The form

- `CheckOutcome` is now `Checked(Readiness)`, `Installed { note, readiness }`
  or `InstallFailed { message, retry }`. `MachineForm::panel()` maps the
  state to the region's tone, title and detail:

| State | Title |
| --- | --- |
| not checked | Not checked yet |
| checking | Checking… (spinner) |
| installing | Installing the Shelbi CLI… (spinner, the host it's installing on) |
| ready | Connection successful (green check), with what an install did first |
| unreachable / auth / host key | Couldn’t connect / SSH couldn’t log in / Host key not trusted (red) |
| CLI missing or incompatible | Shelbi CLI needed, Install CLI offered (amber) |
| unsupported platform | Unsupported platform, no install (amber) |
| Git / directory | Git needed / Project directory needs attention (amber) |
| install failed | Couldn’t install the CLI, Install CLI again unless the connection failed (red) |

- **Binding.** Each request carries its run; an answer counts only for the
  run in flight and the host and directory it was asked for. Editing either
  drops a check (and kills it); an install keeps running and its answer is
  dropped if the draft moved on. One job at a time.
- **Closing.** The owner's `OpenForm` and the view share a `CheckStop`:
  closing (Cancel, Esc, a save, a project switch) kills the running check at
  once and the closed form takes no answer, even though the view lives on
  until the next frame.
- **Recording.** A check of an unsaved draft records nothing. Add / Save
  records the check that verified the draft's host and directory as saved;
  a check or install that verifies a configured machine at its saved host
  records at once. Saving is not gated on a check: a machine can be added
  while it is off.
- The overview badges are unchanged (Local, Checking…, Reachable,
  Unreachable from the live SSH probe); nothing from the form or the stored
  records is shown as a fresh connection result.

## Checks

- `machine_readiness::tests`: target validation runs nothing; quoting;
  every connection failure; ready via PATH and managed copy; missing, old,
  versionless CLI offer install; unsupported platform doesn't; each Git and
  directory problem; install reuse no-op, alongside an old binary, hub-copy
  fallback, failure then retry, unreachable, still needing the directory;
  records only when ready; the script against a real `/bin/sh` and Git.
- `machine_form_state::tests`: stale, raced and superseded answers; install
  through edits; panel titles; field errors; committed identity.
- `app::settings_tests`: a verified Add records once saved and writes no
  config before; Cancel records nothing; an unverified save records nothing;
  a configured machine at its saved host records at once and an edit
  unverifies; an edit or close kills the running check.
- `--visual-figma`: `settings-machine-add` / `-configure` (success) and
  `state-machine-checking`, `-check-failed`, `-needs-cli`, `-installing`,
  `-install-failed`, `-needs-attention`, checked by eye in both themes.
