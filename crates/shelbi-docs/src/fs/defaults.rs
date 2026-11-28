// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/defaults.rs), MIT in Shelbi.

//! Default file contents written into a freshly-created space.
//!
//! Centralised so the on-disk seed matches every place a space is
//! born from. Edit these to change what new spaces ship with.

/// Default `.csignore` written at the space root on creation.
pub const CSIGNORE: &str = "# ContextStore ignore file\n\
# Patterns follow .gitignore syntax\n\
\n\
# AI agent files\n\
AGENTS.md\n\
CLAUDE.md\n\
\n\
# Dependencies\n\
node_modules/\n\
\n\
# Build output\n\
build/\n\
dist/\n\
.build/\n";

/// Default `.gitignore` written at the space root on creation.
///
/// Tracks the bare minimum: macOS metadata cruft and the
/// machine-local settings file. Users layer their own patterns on top.
pub const GITIGNORE: &str = ".DS_Store\n.contextstore/settings.local.yml\n";

/// Seed `blank.md` template written under `.contextstore/templates/`.
/// Used by document creation when the caller doesn't supply content.
pub const BLANK_TEMPLATE: &str = "# New document\n";
