//! Views: the gpui renderings of `shelbi-app`'s navigation and view models.
//! Each is filled by its own subtask:
//!
//! - sidebar, project switching, the refresh pipeline into gpui, settings:
//!   **`dt-shell-sidebar`**
//! - chat (the orchestrator's terminal, full height): **`dt-chat-view`**
//! - issues board and task detail: **`dt-issues-board`** (landed:
//!   `issues.rs` draws the board over the toolkit-free `board_state.rs`,
//!   `task_detail.rs` the detail, `markdown.rs` / `external_edit.rs` its
//!   body helpers; drag and drop: **`dt-board-dnd`**)
//! - activity feed and machines: **`dt-activity-machines`**
//! - workspace (agent terminal + header, user shell, remote workspaces):
//!   **`dt-workspace-view`**
//! - review panel with the editor and diff sessions: **`dt-review-view`**
//! - command palette and overlays (error log, review confirm, reject reason,
//!   Zen intro): **`dt-palette-overlays`**
//!
//! - bootstrap progress, failure screens, and the relaunch bar:
//!   **`dt-platform-bootstrap`** (`bootstrap`)
//!
//! Views read view models and invoke registry commands through
//! `crate::commands`; they never hold domain logic of their own.

pub mod bootstrap;
pub mod board_state;
pub mod external_edit;
pub mod issues;
pub mod markdown;
pub mod task_detail;
