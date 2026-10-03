# Desktop App (gpui)

Status: revised 2026-10-02 after adversarial review
([[removing-tmux-and-desktop-app-review]]) and a second review by Codex.

## Context

[[removing-tmux]] turns Shelbi into a backend with clients: one small process
per session owns each agent's PTY, the daemon runs the poller and supervision,
and the TUI is one client among several. This plan adds the second client, a
native desktop app built with gpui (the GPU-accelerated Rust UI framework
behind Zed).

The reasons to build it follow from why tmux is being removed. Many of the
developers Shelbi is for do not live in a terminal multiplexer. A window with
a sidebar, a board you can drag cards across, and agent terminals you can click
between is a shape they already know.

### Decisions

| Question | Decision |
|---|---|
| Relationship to the TUI | Siblings at feature parity. Both are first-class clients of the same daemon and sessions. |
| First release scope | Full parity with the TUI: chat with the orchestrator, issues board, activity, machines, agent terminals, palette, review flow. |
| First release platform | macOS on Apple silicon only. Linux desktop is a later release. Intel Macs keep the TUI. |
| Public release | Held until full parity. No preview package. |
| Distribution | Homebrew cask `shelbi-desktop`, separate from the CLI formula `shelbi`, which it depends on. The app is "Shelbi" in the Dock. |
| macOS floor | The current and previous major versions at release time. |
| Framework | gpui. How to depend on it is decided in the spike. |
| Design process | Designed in code during the spike, starting from the TUI's layout. No mockup phase. |

### What this plan depends on

Everything below comes from [[removing-tmux]] and must exist first:

- **Phase 1 there** (session processes, protocol, `shelbi-client`,
  `shelbi-term`) before the spike here can start.
- **Phase 3 there** (poller in the daemon) before the app is usable without a
  TUI running.
- **Phase 4a there** (mutations executed by the daemon, and the `shelbi-app`
  model with its command registry). This is the big one. The first draft of this
  plan called it an extraction from the TUI; the review found there is no
  registry to extract and that the CLI and TUI do not even share move logic
  today. It is new design, and it is built during the TUI rewrite so the
  model is written once.
- **Phase 5 there** (remote hosts) for remote workspaces to appear.

Real feature work here starts when Phase 4 there has settled.

## Architecture

```
 +------------------------+        +------------------------+
 | shelbi-tui (ratatui)   |        | shelbi-desktop (gpui)  |
 |  widgets, key handling |        |  views, elements, menus|
 +-----------+------------+        +-----------+------------+
             |                                 |
             +---------------+-----------------+
                             |
                 +-----------v-----------+
                 | shelbi-app            |   no UI toolkit types
                 |  navigation, commands |   (built in removing-tmux
                 |  view models, refresh |    Phase 4a)
                 +-----+-----------+-----+
                       |           |
     +-----------------v--+   +----v-----------------------+
     | shelbi-state,      |   | shelbi-client, shelbi-term |
     | shelbi-orchestrator|   | sessions, emulation,       |
     | files + mutations  |   | selection, search, input   |
     +--------------------+   +----------------------------+
```

### What `shelbi-app` gives the desktop

- **Navigation state:** current project and view, sidebar contents.
- **The command registry:** every user action as a typed command with an
  availability check and an executor. The palette, key bindings, and menu
  items all invoke commands. Commands are distinct from view-local navigation
  (cursor up, scroll down), which each front end handles itself.
- **View models:** board, activity, machines, review panel, task detail,
  error log.
- **Mutations as commands to the daemon.** Moving, starting, assigning,
  approving, rejecting, editing, and adding issues are sent to the daemon,
  which runs one per issue at a time and rechecks the issue's state before
  any irreversible step. The app never runs a merge or a dispatch itself.
  It sends the state it was looking at with each command, and shows the
  daemon's refusal when another client got there first. A merge started from
  the app completes even if the app is closed.
- **Background refresh.** All refresh and command execution runs off the UI
  thread and publishes snapshots. This matters more here than in the TUI: the
  current refresh path runs synchronous SSH, git, and a daemon probe, which
  on gpui's foreground thread would freeze the window.

### Refresh

The TUI polls today (a 200 ms tick, a 750 ms refresh); nothing watches files.
The app uses three sources, all feeding `shelbi-app` off the UI thread:

- Change notifications pushed by the daemon (added in the tmux plan). This is
  the only source that sees GitHub-backed boards, which change through the
  daemon's index and not through task files.
- Session events (title, exit) from `shelbi-client`.
- Polling as a fallback when the daemon is unreachable.

Updates cross into gpui's foreground executor over an async channel.

### Keeping parity honest

The first draft proposed a test that every command "has a binding or menu
path". That passes with a menu item wired to nothing. Instead:

- **Exhaustive match per front end.** Each front end handles the command
  enum with no wildcard arm, so a new command fails to compile until both
  clients handle it.
- **Headless scenario tests on `shelbi-app`** asserting model and event
  effects. These cover behavior once for both clients.
- **A smoke test per front end** that invokes every command through its real
  UI path (ratatui's test backend; gpui's test context).
- **A checklist of view-model fields** each renderer must display.

Platform affordances may differ. Drag and drop, native menus, and system
notifications are desktop-only; the commands under them are shared.

### Relationship to the CLI package

The app is a client and does not embed the daemon or the session process.

- **Finding the CLI.** An app launched from Finder does not get the shell's
  PATH. The app resolves the user's login-shell environment (the same capture
  the tmux plan uses for sessions), probes both Homebrew prefixes and
  `~/.cargo/bin`, and offers a settings override. The resolved environment is
  also what `git`, `gh`, and `ssh` run under.
- It asks `shelbi` to start the daemon if needed.
- **Versions must match exactly**, as the daemon's existing mutation guard
  requires. App and CLI are released together at one version, so a mismatch
  means a partial upgrade, and there are two cases:
  - *The app is newer than the CLI:* show both versions and the
    `brew upgrade shelbi` command.
  - *The CLI was upgraded while the app was running:* `brew upgrade`
    replaces the app on disk but the running process is still the old one.
    The daemon tells it so; the app shows a relaunch prompt and sends no
    commands until relaunched. Viewing sessions keeps working meanwhile,
    because the session protocol is compatible across versions.
- Test: upgrade the CLI with the old app and an old worker session running.

## The terminal element

The orchestrator chat and every agent view are terminals, so this is the core
of the app and the largest piece of new rendering code.

- **A custom gpui element** that paints the cell grid from `shelbi-term`:
  background runs, shaped text runs in a monospace font, cursor, selection,
  search highlights. Repaint is driven by damage from the emulator.
- **Input** goes through `shelbi-term`'s encoder, shared with the TUI. The
  desktop receives real key events with full modifiers, so keys such as
  Shift+Enter work without depending on an outer terminal's keyboard
  protocol.
- **Mouse** follows the same rule as the TUI: forwarded to a program that has
  asked for it, with Shift+wheel and Shift+drag reserved for Shelbi's own
  scrollback and selection.
- **IME and dead keys** through gpui's input handler.
- **Clickable paths and URLs**, opening the browser or the user's editor.
- **Accessibility.** gpui integrates AccessKit, but a custom-painted grid
  exposes nothing by default. The element publishes at least the visible text
  and cursor position. State plainly in the docs what is and is not
  supported.

**Licensing and prior art.** gpui is Apache-2.0. Zed's own terminal crates
(`terminal`, `terminal_view`) are GPL-3.0-or-later and Shelbi is MIT, so their
code cannot be copied or depended on. There is permissive prior art to study:
`gpui-terminal` (MIT or Apache-2.0), built on an older gpui and
`alacritty_terminal`. Start from it as a reference, not a dependency.

## Views

| View | Desktop form |
|---|---|
| Sidebar | Projects, the Chat / Issues / Activity nav, the workspace list with live state, zen indicator. Resizable, collapsible. |
| Chat | The orchestrator's terminal session, full height. |
| Issues | Native kanban: columns from the workflow's statuses, cards with title, assignee, branch, dependency and state badges. Drag to move, through the shared move command. Keyboard navigation matching the TUI. |
| Task detail | Rendered markdown body with frontmatter fields; edit in place or open in the user's editor. |
| Activity | The event feed, filterable by project, workspace, and kind. |
| Machines | Workspaces by machine with live state. |
| Workspace | The agent's terminal, with a header showing task, branch, machine, and state. An optional user shell beside it. |
| Review | The review panel as native UI; the editor and the diff as terminal sessions in a split, matching the TUI. A native diff view is a later improvement, not part of parity. |
| Palette | An overlay listing commands from the registry with the existing fuzzy matcher (`shelbi-palette`). |
| Overlays | Error log, review confirm, reject reason, zen intro. |
| Settings | Font, theme, key bindings, CLI path. Edits the same config files the TUI reads. |

**Windowing.** One window per project, with a project switcher in the sidebar
and the palette. Native menus on macOS, every item backed by a registry
command.

**Notifications.** A system notification when a workspace becomes blocked, a
task reaches review, or the orchestrator needs an answer. gpui provides
system notifications on its current main branch.

**Look.** Follow the direction set for the website ([[shelbi-website]]):
strict monochrome, Geist. Light and dark themes from one set of tokens, shared
in name with the TUI's theme so the two clients feel like one product.

## Build and packaging

**Its own workspace.** `shelbi-desktop` lives in the main repository but in a
**separate Cargo workspace** (listed under `exclude` in the root workspace,
with its own `Cargo.lock` and `rust-toolchain.toml`), depending on the shared
crates by path.

The first draft kept it in the main workspace and excluded it from one CI
command. That misses the others: Linux CI also runs `cargo build`, `clippy`,
and `test` with `--workspace` (`app-ci.yml:54-68`), and release validation
runs `cargo test --workspace` (`release.yml:71`). And even with every command
patched, one shared lockfile means gpui's dependency tree can pull shared
dependencies to versions that no longer build on the CLI's Rust 1.88 floor.

A separate workspace settles both: no existing workspace command sees the
desktop crate, and the CLI's lockfile and toolchain are untouched by gpui.

- A desktop CI job on a macOS runner builds, lints, and tests that
  workspace.
- The main CI job keeps a locked build on Rust 1.88, which now also proves
  that changes made to shared crates for the desktop's sake have not raised
  the CLI's floor.
- The version cannot be inherited across workspaces, so the release version
  check (`scripts/release/check-version.sh`) asserts that the desktop crate's
  version equals the workspace version and the tag.

**Depending on gpui.** There is no clean channel: the crates.io release is a
year old, and Zed's main branch has split gpui into platform crates that are
not published. The spike tries both live options and chooses on evidence:

| Channel | For | Against |
|---|---|---|
| `gpui-pre` snapshot crates with the `gpui-kit` widget library | Fast start; dock, lists, inputs, markdown ready-made; publishable | Weekly churn; depends on one outside maintainer |
| Git dependency on the Zed repository at a pinned revision | Authoritative; upgrade on your own schedule | No widget library; heavy checkout; `shelbi-desktop` cannot go to crates.io |

Default if both work: `gpui-pre` with `gpui-kit`. Either way, gpui types stay
out of `shelbi-app` so churn is confined to one crate.

**macOS.** An arm64 `.app` bundle signed with Developer ID, hardened runtime,
notarized and stapled, zipped, attached to the GitHub release. Same pipeline
as the 32pixels Mac apps; the Shelbi repository is public, so the artifact
can live on its own releases.

**Homebrew.** A cask `shelbi-desktop` in the existing tap with
`depends_on formula: "shelbi"` and `depends_on arch: :arm64`. The release job
updates its version and checksum alongside the formula.

**Updates.** Through Homebrew. No in-app updater; the app shows a notice when
a newer release exists.

## Phases

### Phase 0: Spike

- A gpui window with one terminal element attached to a session running
  Claude Code. Type, scroll, resize, select, Shift+Enter, mouse in
  full-screen mode.
- Try both gpui channels and decide. Evaluate `gpui-kit` for inputs, lists,
  menus, and markdown.
- Set up the separate workspace, toolchain, and CI job, and confirm the main
  workspace still builds locked on Rust 1.88.
- Read `gpui-terminal` for the element's structure.
- Work out the look in the running app. There is no mockup phase.
- Record build time and binary size.

Exit criteria: the terminal feels as good as a native terminal, the gpui
channel is chosen, and CI builds the crate without slowing the main job.

### Phase 1: App shell

- Window, sidebar, project list and switching, theme, settings storage.
- Login-environment capture, CLI discovery, daemon start, version check, and
  an error state for each of those failing.
- Chat view using the spike's terminal element.
- The exhaustive command match, with unimplemented commands failing loudly.

### Phase 2: Terminal element to completion

- Scrollback, selection, search, clipboard, IME, links, fonts, mouse
  forwarding, accessibility text.
- Workspace view, user shell, remote workspaces.
- The app and a TUI attached to the same session at once, at different
  sizes.
- Performance pass under heavy streaming output.

### Phase 3: Issues, task detail, machines

- Board with keyboard navigation and drag and drop, including the refusal
  path when another client moved the card first.
- Task detail with markdown rendering and editing.
- Machines view.

### Phase 4: Activity, palette, overlays

- Activity feed and filters.
- Palette, error log, zen toggle and intro.
- Native menus generated from the registry.
- Notifications.

### Phase 5: Review

- Review panel, editor and diff sessions, approve and reject with reason.
- Review workspaces: the running server's state and URL.

### Phase 6: Packaging

- Bundle, sign, notarize; a macOS arm64 build job in the release workflow.
- Cask and release-job update.
- An install test on a clean machine, in the style of the existing
  `apt-verify-install` job.

### Phase 7: Parity audit and release

- Walk every command in both clients; the per-front-end smoke tests become
  required checks.
- Rewrite the product docs: `Product/vision.md` says "Shelbi is not becoming
  a desktop app" and lists desktop apps among the things Shelbi deliberately
  is not. Update it, `Product/principles.md`, and `Product/positioning.md`
  to describe one backend with two clients.

## Later: Linux desktop

Deferred to its own release so that Linux graphics variance does not hold up
the first one. Recorded here so the findings are not lost:

- **Channels wanted:** AUR, apt, Homebrew, and a tarball or AppImage.
  Homebrew casks now work on Linux, so one cask can cover both platforms. The
  AUR package needs a matching `shelbi-bin` for the CLI. For Omarchy
  specifically, its own package repository may matter more than the AUR.
- **Omarchy is a named risk.** Zed's Linux documentation names it for a GPU
  driver launch failure, and a Hyprland "no window appears" issue was closed
  as not planned. Test on real AMD and NVIDIA hardware before promising it.
- **Window decorations.** gpui apps draw their own title bar on GNOME.
- **Rendering** is wgpu with Vulkan and GL backends.
- **glibc floor.** A binary built on a current Ubuntu runner will not run on
  older distributions. Build in an old-glibc container and state the floor.
- **Menus.** Native menu bars are a macOS feature; on Linux the app draws its
  own.

The CLI's Linux channels are unaffected by this deferral. Adding an AUR
package for the CLI belongs in [[release-distribution-homebrew-apt]].

## Risks

- **gpui is pre-1.0 with no stable release channel.** Its API changes and its
  documentation is thin; Zed's source is the practical reference. Pin
  exactly, upgrade on purpose, keep gpui types in one crate.
- **The parity tax.** Every UI feature now needs two front ends. The shared
  model reduces this to rendering work, but it is a permanent cost on every
  feature and should be accepted knowingly.
- **The terminal element** is new rendering code that Zed spent years
  refining. Phases 0 and 2 are sized for it.
- **Full parity before first release is a long road** with no outside
  feedback, by decision. Phases 1 through 5 each produce something runnable,
  so dogfooding starts at Phase 2, and the crate sits in the public
  repository for anyone who wants to build it.
- **Review parity** depends on the tmux plan's Phase 4e, the least defined
  part of that plan.

## Out of scope

- Linux and Windows builds, and Intel Macs, in the first release.
- An in-app updater.
- A native code editor or native diff viewer.
- A hosted or networked mode. The app talks to the local daemon and
  sessions, and reaches remotes the way the hub does, over SSH.
- Mobile.
