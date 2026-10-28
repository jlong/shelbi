//! The Workflow Builder's own state, toolkit-free (`dt-wf-editor-builder`).
//! It moved to `shelbi_app::settings::workflow_builder`
//! (`tui-workflow-editor-builder`) so the TUI's Builder selects, draws and
//! steps back the same way; this module re-exports it under its old path.
//! Its tests moved with it
//! (`crates/shelbi-app/tests/settings_workflow_builder.rs`).

pub use shelbi_app::settings::workflow_builder::*;
