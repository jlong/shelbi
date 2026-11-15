# prereq-check: the startup requirements check

## The change

Shelbi depends on command-line tools it doesn't ship: `git`, `gh`, a coding
agent (`claude` or `codex`, either one) and, on macOS, `brew`. A missing one
used to surface much later, as a failed workspace, PR or agent launch. Now
every launch of either client checks them first and, when one is missing,
says so with the exact command to install it.

One shared model, `shelbi_app::prereqs`, holds the tools, the copy, the row
order, the checks and the skip confirmation's list; the desktop
(`views/prereqs.rs`) and the TUI (`shelbi_tui::prereqs`) only render it.

```
 ● ● ●
                  A few tools are missing
      Install them with the commands below, then check again.
 ┌──────────────────────────────────────────────────────────┐ ▐
 │ ✓ Git                              2.39.5 · /usr/bin/git │ ▐
 └──────────────────────────────────────────────────────────┘ ▐
 ┌──────────────────────────────────────────────────────────┐
 │ ✕ GitHub CLI                                             │
 │   Not found on your PATH. Install it with Homebrew, then │
 │   check again.                                           │
 │   ┌────────────────────────────────────────────┬────────┐│
 │   │ brew install gh                            │ ⧉ Copy ││
 │   └────────────────────────────────────────────┴────────┘│
 │   Already installed somewhere else?  Locate gh…          │
 ░░░░░░░░░░░░░░░░░░ fade to background ░░░░░░░░░░░░░░░░░░░░
                                 [Skip for now] [Check again]
```

## Where a tool is looked for

A binary the user located (`config.yaml`'s `tools:` block) first, then the
login shell's `PATH` (`$SHELL -l -i -c env`, the capture the daemon and the
desktop's bootstrap already use), then the process's own. A Finder-launched
app's `PATH` lacks `/opt/homebrew/bin` and `~/.local/bin`; its login shell's
doesn't. On macOS `/usr/bin/git` only counts once Apple's Command Line Tools
are installed (`xcode-select -p`), and is never run before that: the stub
opens the installer's dialog.

A found row shows `<version> · <path>` (`<bin> --version`'s first
version-looking token, the home folder as `~`), or the path alone when
`--version` fails.

## When it shows

- **Desktop, first launch** (no window to restore): the wizard window opens
  on "Checking your tools", rows completing one by one with the Spinner on
  the one in flight. All found: it becomes the CLI setup by itself (no
  stop). Something missing: the Missing screen; Continue (once Check again
  or Locate finds everything) or Skip anyway goes on to the CLI setup.
- **Desktop, every other launch**: the windows open as usual, and the tools
  are checked off the UI thread, existence only (nothing is run). Only a
  missing tool opens the requirements window, in front; Continue or Skip
  anyway closes it. A launch with everything installed is not slowed.
- **TUI** (`shelbi` with no subcommand, in a terminal): a check of the
  terminal's own `PATH`, existence only, before anything else. All found:
  nothing is drawn. Otherwise the full-screen requirements step runs the
  full check (the login `PATH` too) and goes on by itself if that finds
  everything; Esc leaves `shelbi`.

Skip anyway never sticks: the next launch checks again.

## Rows

Git, GitHub CLI, Coding agent, Homebrew; with Git or Homebrew missing,
dependency order instead (Git, Homebrew, GitHub CLI, Coding agent: Homebrew
needs the Command Line Tools and installs the rest). Linux has no Homebrew
row, shows "install with your package manager" for Git, GitHub's install
guide for `gh`, and `npm install -g` for the agents.

## Locate

Locate… (desktop: the path field with Browse…, a native open panel; TUI:
`l`, a path field, Return) checks the path: an executable file whose
`--version` succeeds. A good one shows "<bin> version X" in green, counts as
found, and is saved to `config.yaml`:

```yaml
tools:
  gh: /Users/me/.local/bin/gh
```

The located binaries' directories also join the end of every login
environment Shelbi builds (`shelbi_core::login_env::set_path_extension`,
installed from both binaries' `main`): the daemon's own environment, every
session's, tool detection's. So `git`, `gh`, `claude` and `codex` run by
name resolve to the located binary.

## Not done

- A tool located while the daemon runs reaches new sessions at once, but the
  daemon's own process environment (applied once at its start) only after
  the daemon restarts.
- The TUI process's own `git`/`gh` calls use the terminal's `PATH`, not the
  extension; a terminal launch has the tools there anyway (the check passed
  on that `PATH`, or the user located a tool the terminal can't see, in
  which case those in-process calls still miss it).
- A located binary whose file name isn't the tool's (`gh-2.62`) is counted
  by the check, but name-based spawns won't find it through `PATH`.

## Verify

- `cargo test -p shelbi-app prereqs`, `cargo test -p shelbi-tui prereqs`,
  `cargo test -p shelbi-state tool_paths`, and from `desktop/`,
  `cargo test prereqs`.
- `SHELBI_VISUAL_ONLY=prereqs shelbi-desktop --visual-figma <dir>` renders
  the six frames (`prereq-checking`, `-missing`, `-fresh-mac`, `-locate`,
  `-ready`, `-skip`) in both themes.
- TUI by hand, in a sandbox: `env -i HOME=$HOME TERM=xterm-256color
  SHELL=/bin/sh PATH=/usr/bin:/bin SHELBI_HOME=<tmp> shelbi`.
