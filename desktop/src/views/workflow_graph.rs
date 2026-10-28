//! The Workflow Builder's graph, toolkit-free (`dt-wf-editor-builder`). It
//! moved to `shelbi_app::settings::workflow_graph`
//! (`tui-workflow-editor-builder`) so the TUI's Builder lays out and
//! navigates the same graph; this module re-exports it under its old path.

pub use shelbi_app::settings::workflow_graph::*;
