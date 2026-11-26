# desktop-linux: the desktop app on Linux (Omarchy)

The umbrella for porting the gpui desktop app to Linux. The goal: an Omarchy
user installs Shelbi from the AUR, launches it from Walker, adds a project and
uses every view without dropping to a terminal, in an app that follows
Omarchy's theme and Linux conventions. The plan is "Shelbi Desktop for
Omarchy" in Claude Docs.

## What landed

One codebase, two platforms. What differs from the Mac sits behind
`src/platform/os/` (`macos.rs`, `linux.rs`); the views and view models are
shared, so parity with the Mac app and the TUI comes from the same code.

| Subtask | What it did |
|---|---|
| `dl-spike` | `cargo build` and a native Wayland window under Hyprland on the pinned gpui-pre 0.3.9 (`docs/gpui.md`) |
| `dl-platform` | The per-OS interface with Linux implementations: CLI discovery from a bare Walker launch, the XDG portal's file chooser, `xdg-open`, freedesktop notifications, Reduce Motion and overlay scroll bars from the settings portal, relaunch through `/proc/self/exe`; Linux CI for `desktop/` |
| `dl-windowing` | Translucency blurred by Hyprland, opaque on high contrast; the app id `dev.shelbi.Desktop`; tabs, New Window and tab tear-off within Wayland's limits (no global positions), Move to Window in the tab menu |
| `dl-omarchy-theme` | With `theme: auto`, the colors from the active Omarchy theme's `colors.toml` and the terminal font from Omarchy's, both followed live (`docs/theme.md`) |
| `dl-input` | Every app chord is Ctrl+Shift+key; terminals keep the plain Ctrl keys; Super stays with Hyprland; the Wayland clipboard and primary selection; file and image drops; the menu bar's client commands in the palette |
| `dl-packaging` | `shelbi-desktop_Linux_x86_64.tar.gz` on every release and the AUR packages `shelbi-bin` and `shelbi-desktop-bin` |
| `dl-parity` | The Mac's parity walk on a packaged install on Omarchy, its fixes and the issues it filed (`docs/status/dl-parity.md`) |

## What's left

- x86_64 only. aarch64 (Asahi) waits for someone to ask.
- `visual-check` (the offscreen Figma QA, fixtures, benches) is macOS-only,
  because gpui-pre's `VisualTestAppContext` is.
- The issues `dl-parity` filed: see its status doc.

## Verification

Each subtask's commit records its own checks. `dl-parity` walked every
command in `docs/parity.md` on Omarchy against packages built from the
umbrella branch.
