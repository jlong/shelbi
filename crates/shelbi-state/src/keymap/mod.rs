//! Customizable keybindings — actions, chord parser, and `keys.yml`
//! loader. Wired into the TUI handlers by subsequent tasks; this module
//! is dead code on its own.

pub mod actions;
pub mod chord;
pub mod loader;

pub use actions::{
    Action, ActivityAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction,
    ReviewAction, SidebarAction, MODE_NAMES,
};
pub use chord::{ChordParseError, KeyChord};
pub use loader::{
    load_keymaps, ErrorKind, KeymapDiagnostic, Keymaps, ModeKeymap, WarningKind, KEYS_FILENAME,
};
