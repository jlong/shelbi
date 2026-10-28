//! The workflow editor's state, toolkit-free (`dt-wf-editor-form`). It
//! moved to `shelbi_app::settings::workflow_editor`
//! (`tui-workflow-editor-form`) so the TUI's editor runs the same draft,
//! changes, Save, Discard and leave prompt; this module re-exports it under
//! its old path. Its tests moved with it
//! (`crates/shelbi-app/tests/settings_workflow_editor.rs`).

pub use shelbi_app::settings::workflow_editor::*;
