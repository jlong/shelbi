//! Views: the gpui renderings of `shelbi-app`'s navigation and view models.
//! Each is filled by its own subtask:
//!
//! - sidebar, project switching, the refresh pipeline into gpui, settings:
//!   **`dt-shell-sidebar`**
//! - chat (the orchestrator's terminal, full height): **`dt-chat-view`**
//! - issues board and task detail: **`dt-issues-board`** (drag and drop and
//!   the refusal path: **`dt-board-dnd`**)
//! - activity feed and machines: **`dt-activity-machines`**
//! - workspace (agent terminal + header, user shell, remote workspaces):
//!   **`dt-workspace-view`**
//! - review panel with the editor and diff sessions: **`dt-review-view`**
//! - command palette and overlays (error log, review confirm, reject reason,
//!   Zen intro): **`dt-palette-overlays`**
//!
//! Views read view models and invoke registry commands through
//! `crate::commands`; they never hold domain logic of their own.
