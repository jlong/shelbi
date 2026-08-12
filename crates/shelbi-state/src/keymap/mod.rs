//! Customizable keybindings — actions, chord parser, `keys.yaml` loader,
//! and platform-aware help-text rendering.

pub mod actions;
pub mod chord;
pub mod display;
pub mod loader;

pub use actions::{
    Action, ActivityAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction,
    SidebarAction, MODE_NAMES,
};
pub use chord::{ChordParseError, KeyChord};
pub use display::{format_chord, DisplayStyle};
pub use loader::{
    heal_legacy_zen_toggle, load_keymaps, validate_keymaps_yaml, ErrorKind, KeymapDiagnostic,
    Keymaps, ModeKeymap, WarningKind, KEYS_FILENAME,
};
