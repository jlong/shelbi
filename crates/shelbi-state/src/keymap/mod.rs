//! Customizable keybindings — actions, chord parser, `keys.yaml` loader,
//! and platform-aware help-text rendering.

pub mod actions;
pub mod chord;
pub mod contexts;
pub mod display;
pub mod edit;
pub mod key;
pub mod loader;

pub use actions::{
    Action, ActivityAction, DiffAction, GlobalAction, KanbanAction, PaletteAction, PopoverAction,
    SidebarAction, MODE_NAMES,
};
pub use chord::{ChordParseError, KeyChord, Leader};
pub use contexts::{context, KeymapContext, CONTEXTS};
pub use key::{Key, Mods};
pub use display::{format_chord, DisplayStyle};
pub use loader::{
    heal_legacy_zen_toggle, load_keymaps, load_keymaps_with_overlay, project_overlay,
    validate_keymaps_yaml, ErrorKind, KeymapDiagnostic,
    Keymaps, ModeKeymap, WarningKind, KEYS_FILENAME,
};
