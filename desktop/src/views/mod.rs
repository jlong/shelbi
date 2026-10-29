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
//! - the project list the switcher's menu and the palette's Projects column
//!   share, with its open-project dots: **`dt-switcher-palette-icons`**
//!   (`project_menu`)
//! - the review panel taking the sidebar's column, with the push between
//!   them: **`dt-review-replaces-sidebar`** (`column_swap`)
//! - the Workspace and Review panels drawn to the Navigation component's
//!   task states, with the pieces they share: **`dt-figma-task-panels`**
//!   (`task_panel`)
//! - Settings split into pages, with its own sidebar in the column:
//!   **`dt-settings-sections`** (`settings_nav`)
//! - Settings ▸ Workflows and the workflow editor: **`dt-wf-settings-index`**
//!   (`workflows`), **`dt-wf-editor-form`** (`workflow_editor`, its Form),
//!   **`dt-wf-editor-builder`** (`workflow_builder`, the Builder canvas over
//!   `workflow_graph`'s layout and `workflow_builder_state`, both shared
//!   with the TUI's Builder since `tui-workflow-editor-builder`)
//! - Settings ▸ Workspaces, the pool and its machines as configuration:
//!   **`dt-workspace-settings-index`** (`workspaces`, over
//!   `shelbi_app::settings::workspaces`); its New / Edit workspace dialogs
//!   and the removal confirmation: **`dt-workspace-settings-forms`**
//!   (`workspace_dialog`, over `shelbi_app::settings::workspace_form`; both
//!   states shared with the TUI since `tui-settings-workspaces`)
//!
//! - bootstrap progress, failure screens, and the relaunch bar:
//!   **`dt-platform-bootstrap`** (`bootstrap`)
//! - the first launch's CLI check, install, and Ready before the Add Project
//!   wizard: **`dt-wizard-cli-setup`** (`cli_setup`)
//! - the new-release notice: **`dt-packaging`** (`update`)
//! - About Shelbi and the Acknowledgements window it opens:
//!   **`dt-about-bundled-software`** (`about`, `bundled_software`, over
//!   `crate::licenses`)
//! - Contact Us, the feedback dialog the sidebar footer's megaphone and
//!   Help ▸ Send Feedback… open: **`dt-feedback-dialog`** (`feedback`, over
//!   the shared `shelbi_app::feedback` and `crate::platform::feedback`)
//!
//! Views read view models and invoke registry commands through
//! `crate::commands`; they never hold domain logic of their own.

pub mod about;
pub mod add_project;
pub mod activity;
pub mod activity_labels;
mod activity_list;
pub mod activity_reader;
pub mod activity_state;
pub mod agents;
pub mod agents_state;
pub mod agent_editor;
pub mod agent_editor_state;
pub mod board_list;
pub mod board_scroll;
pub mod bootstrap;
pub mod bundled_software;
pub mod cli_setup;
pub mod board_state;
pub mod chat;
pub mod column_swap;
pub mod doc_editor;
pub mod session_view;
pub mod workspace;
pub mod workspace_layout;
pub mod external_edit;
pub mod feedback;
pub mod font_picker;
pub mod issues;
pub mod keybindings;
pub mod machines;
pub mod machines_state;
pub mod machine_form;
pub mod machine_form_state;
pub mod overlay;
pub mod pane_session;
pub mod pane_split;
pub mod palette;
pub mod project_menu;
pub mod review;
pub mod task_detail;
pub mod task_panel;
pub mod settings;
pub mod settings_nav;
pub mod terminal_preview;
pub mod sidebar;
pub mod sidebar_reveal;
pub mod update;
pub mod workflows;
pub mod workflows_state;
pub mod workspace_dialog;
pub mod workspaces;
pub mod workflow_editor;
pub mod workflow_editor_state;
mod workflow_builder;
pub mod workflow_builder_state;
pub mod workflow_graph;
