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
//!   body helpers; drag and drop: **`dt-board-dnd`**; which gesture scrolls
//!   the board and which a column: **`dt-board-scroll-axes`**,
//!   `board_scroll.rs`; each column a virtualized list so a board of a
//!   thousand cards stays at frame rate: **`dt-board-perf`**, `board_list.rs`)
//! - activity feed and machines: **`dt-activity-machines`** (landed:
//!   `activity.rs` over the toolkit-free `activity_state.rs`, `machines.rs`
//!   over `machines_state.rs`, both from `shelbi-app`'s shared models)
//! - workspace (agent terminal + header, user shell, remote workspaces):
//!   **`dt-workspace-view`** (landed: `workspace.rs`, its split math in
//!   `workspace_layout.rs`; the attach/restart/reconnect machine both it and
//!   the chat use is `session_view.rs`)
//! - review panel with the editor and diff sessions: **`dt-review-view`**
//!   (landed: `review.rs`, over `shelbi_app::review`'s panel model, its
//!   navigation, and the load flow the window drives)
//! - command palette and overlays (error log, review load, reject reason,
//!   Zen intro, add project): **`dt-palette-overlays`** (landed: `palette.rs`
//!   and `overlay/`, over the shared `shelbi_app::overlay` state machines)
//! - the review panel taking the sidebar's column, with the push between
//!   them: **`dt-review-replaces-sidebar`** (`column_swap`)
//! - the Workspace and Review panels drawn to the Navigation component's
//!   task states, with the pieces they share: **`dt-figma-task-panels`**
//!   (`task_panel`)
//! - Settings split into pages, with its own sidebar in the column:
//!   **`dt-settings-sections`** (`settings_nav`)
//!
//! - bootstrap progress, failure screens, and the relaunch bar:
//!   **`dt-platform-bootstrap`** (`bootstrap`)
//! - the new-release notice: **`dt-packaging`** (`update`)
//!
//! Views read view models and invoke registry commands through
//! `crate::commands`; they never hold domain logic of their own.

pub mod activity;
pub mod activity_labels;
pub mod activity_state;
pub mod board_list;
pub mod board_scroll;
pub mod bootstrap;
pub mod board_state;
pub mod chat;
pub mod column_swap;
pub mod session_view;
pub mod workspace;
pub mod workspace_layout;
pub mod external_edit;
pub mod font_picker;
pub mod issues;
pub mod machines;
pub mod machines_state;
pub mod markdown;
pub mod overlay;
pub mod palette;
pub mod review;
pub mod task_detail;
pub mod task_panel;
pub mod settings;
pub mod settings_nav;
pub mod sidebar;
pub mod update;
