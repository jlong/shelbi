//! Customizable keybindings — actions, chord parser, `keys.yml` loader,
//! and platform-aware help-text rendering.

pub mod actions;
pub mod chord;
pub mod display;
pub mod loader;

pub use actions::{
    Action, ActivityAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction,
    ReviewAction, SidebarAction, MODE_NAMES,
};
pub use chord::{ChordParseError, KeyChord};
pub use display::{format_chord, DisplayStyle};
pub use loader::{
    load_keymaps, ErrorKind, KeymapDiagnostic, Keymaps, ModeKeymap, WarningKind, KEYS_FILENAME,
};
