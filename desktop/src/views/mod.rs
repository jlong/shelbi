//! Views: the gpui renderings of `shelbi-app`'s navigation and view models.
//! Each is filled by its own subtask:
//!
//! - sidebar, project switching, the refresh pipeline into gpui, settings:
//!   **`dt-shell-sidebar`**
//! - chat (the orchestrator's terminal, full height): **`dt-chat-view`**
//!   (landed: `chat.rs`)
//! - issues board and task detail: **`dt-issues-board`** (landed:
//!   `issues.rs` draws the board over the toolkit-free `board_state.rs`,
//!   `task_detail.rs` the detail, `markdown.rs` / `external_edit.rs` its
//!   body helpers; drag and drop: **`dt-board-dnd`**)
//! - activity feed and machines: **`dt-activity-machines`** (landed:
//!   `activity.rs` over the toolkit-free `activity_state.rs`, `machines.rs`
//!   over `machines_state.rs`, both from `shelbi-app`'s shared models)
//! - workspace (agent terminal + header, user shell, remote workspaces):
//!   **`dt-workspace-view`**
//! - review panel with the editor and diff sessions: **`dt-review-view`**
//! - command palette and overlays (error log, review confirm, reject reason,
//!   Zen intro): **`dt-palette-overlays`**
//!
//! - bootstrap progress, failure screens, and the relaunch bar:
//!   **`dt-platform-bootstrap`** (`bootstrap`)
//! - the new-release notice: **`dt-packaging`** (`update`)
//!
//! Views read view models and invoke registry commands through
//! `crate::commands`; they never hold domain logic of their own.

pub mod activity;
pub mod activity_state;
pub mod bootstrap;
pub mod board_state;
pub mod chat;
pub mod external_edit;
pub mod issues;
pub mod machines;
pub mod machines_state;
pub mod markdown;
pub mod task_detail;
pub mod settings;
pub mod sidebar;
pub mod update;
