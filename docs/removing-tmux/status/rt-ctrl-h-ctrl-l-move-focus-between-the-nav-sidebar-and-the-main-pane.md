# rt-ctrl-h-ctrl-l-move-focus-between-the-nav-sidebar-and-the-main-pane

Status: ready for review.

Added global vim-style focus moves to the single-process TUI: Ctrl+H focuses
the nav sidebar (or the review panel while reviewing), Ctrl+L focuses the main
pane (or the review content view). Routed as keymap actions
(`GlobalAction::FocusSidebar` / `FocusMain`, defaults `ctrl-h` / `ctrl-l`) so
they're rebindable. Intercepted in `ShellState::handle_key` before focus
routing, so the key is never forwarded to the focused session; keymap routing
keeps Ctrl+H distinct from Backspace (no enhancement → Backspace still reaches
the session). Palette footer advertises the chords.

Notes:
- Read John's "ctrl+h/k ... (left/right)" as h/l (k is up, not a horizontal
  move); implemented h/l per the task's read.
- No config-upgrade sniffer needed: keybinding defaults ship embedded in the
  binary (`Action::all`), not in a copied `keys.yaml`, so existing installs get
  the new defaults for free. `keys.yaml` is only materialized on demand via
  `shelbi config dump-keybindings`.
- No `Cargo.lock` change (no MSRV step).
