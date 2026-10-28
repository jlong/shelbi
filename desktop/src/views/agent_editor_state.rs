//! The agent details editor's state, toolkit-free (`dt-agent-details`). It
//! moved to `shelbi_app::settings::agent_editor` (`tui-agent-editor`) so the
//! TUI's editor runs the same draft, changes, Save, Discard and leave
//! prompt; this module re-exports it under its old path. Its tests moved
//! with it (`crates/shelbi-app/src/settings/agent_editor/tests.rs`).

pub use shelbi_app::settings::agent_editor::*;
