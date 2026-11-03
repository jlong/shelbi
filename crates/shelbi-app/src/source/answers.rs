//! Answers a [`ProjectSource`](super::ProjectSource) returns whose readers live
//! in `shelbi-orchestrator`. The types live here, not beside their readers, so
//! the toolkit-free trait can name them (this crate must not depend on the
//! orchestrator); the orchestrator re-exports each at its old path.
//!
//! Serializable like the rest of the read models (`crate::wire`): every field
//! decodes when missing, so a field added later still decodes.

use serde::{Deserialize, Serialize};

/// Everything a client's workspace view shows about a workspace, on any
/// machine: where it lives, its worktree, and the task it is working.
///
/// The JSON shape is pinned by `workspace_details_json_shape_is_pinned`
/// (`shelbi-orchestrator`'s `workspace_session` tests).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceDetails {
    /// The machine the workspace is declared on.
    pub machine: String,
    /// Whether that machine is reached over SSH.
    pub is_remote: bool,
    /// Absolute workspace worktree path (on the workspace's machine).
    pub worktree: String,
    /// A remote worktree's `<ssh host>:<path>` address (what Reveal copies,
    /// since there's no local folder to show). `None` for a local workspace.
    pub remote_address: Option<String>,
    /// Display name of the resolved editor (`Vim`, `Helix`, `Zed`, …): for a
    /// remote workspace, the terminal editor its Edit pane runs there.
    pub editor_name: String,
    /// Which of Edit and View Diff open an app instead of a pane (the
    /// orchestrator's `tools::app_tools`). Neither, for a remote workspace: an
    /// app opens local worktrees only, so both open panes on its machine.
    pub apps: shelbi_state::AppTools,
    /// The branch the workspace is on: its in-progress task's branch, else
    /// the worktree's checked-out branch (read on its machine). `None` when
    /// unknown.
    pub branch: Option<String>,
    /// The in-progress task on this workspace, from the published board index.
    pub task: Option<Box<shelbi_state::IssueFile>>,
}

/// Everything a client needs to build its review panel for a task, resolved
/// from durable state in one place (so the client stays thin and the slot
/// resolution matches the spawn path).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewOpenInfo {
    /// The review slot (workspace) the task is loaded on.
    pub slot: String,
    /// Absolute review worktree path (shown truncated, revealed on click).
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, `Zed`, …).
    pub editor_name: String,
    /// Which of Edit and View Diff open an app instead of a pane.
    pub apps: shelbi_state::AppTools,
    /// Whether the workflow declares a review URL (gates the Browser entry).
    pub has_review_url: bool,
    /// The review URL itself (the workflow's `review:` template with the
    /// slot's `$PORT` / `$SLOT` substituted), when declared.
    pub review_url: Option<String>,
    /// Whether the workflow declares a `ready:` health check for the review
    /// server.
    pub has_health_check: bool,
    /// The slot's machine when it is remote (reached over SSH), `None` for a
    /// local slot.
    pub remote_machine: Option<String>,
    /// The reviewed task as read from the issue store, for the panel's title /
    /// description preview and the full-description popover. `None` when the
    /// store couldn't produce it (the panel degrades to no task info). Boxed
    /// (it's ~500 bytes) to keep the [`ReviewOpenTarget`] variants balanced.
    pub task: Option<Box<shelbi_state::IssueFile>>,
}

/// How a client should open a review-column task, resolved from durable state
/// in one read (`rt-tui-review-load-queued`): either it is already loaded on a
/// review slot (open the interface straight away) or it is still queued and
/// must be loaded onto a slot first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOpenTarget {
    /// The task is on a review slot; open the interface with these inputs.
    Serving(ReviewOpenInfo),
    /// The task is a review-column task not yet on a review slot; the client
    /// must load it onto one (the slot picker, then a daemon
    /// [`Load`](shelbi_proto::control::ReviewSessionOp::Load)). `title` is the
    /// card title the picker shows.
    Queued {
        #[serde(default)]
        title: String,
    },
}

/// One review-tagged workspace and the active task (if any) currently loaded
/// on it. The "load onto which review workspace?" picker lists *every* review
/// slot with its free/occupied state; eviction lets a load target an occupied
/// slot, so the picker must show them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewSlot {
    /// The `review`-tagged workspace name.
    pub name: String,
    /// The task currently loaded on this slot, or `None` when it's free.
    pub occupant: Option<ReviewSlotOccupant>,
}

/// The task currently occupying a [`ReviewSlot`]: its id (what eviction acts
/// on) and title (what the picker shows in quotes).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewSlotOccupant {
    pub task_id: String,
    pub title: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(value: &T, json: &str) {
        let encoded = serde_json::to_string(value).unwrap();
        assert_eq!(encoded, json);
        let decoded: T = serde_json::from_str(&encoded).unwrap();
        assert_eq!(&decoded, value);
    }

    #[test]
    fn review_slots_round_trip_and_decode_from_nothing() {
        let slot = ReviewSlot {
            name: "rev-1".into(),
            occupant: Some(ReviewSlotOccupant {
                task_id: "t-7".into(),
                title: "Fix it".into(),
            }),
        };
        round_trip(&slot, r#"{"name":"rev-1","occupant":{"task_id":"t-7","title":"Fix it"}}"#);
        assert_eq!(serde_json::from_str::<ReviewSlot>("{}").unwrap(), ReviewSlot::default());
    }

    #[test]
    fn review_open_targets_round_trip() {
        round_trip(
            &ReviewOpenTarget::Queued { title: "Queued one".into() },
            r#"{"queued":{"title":"Queued one"}}"#,
        );
        let serving = ReviewOpenTarget::Serving(ReviewOpenInfo {
            slot: "rev-1".into(),
            worktree: "/w".into(),
            editor_name: "Vim".into(),
            review_url: Some("http://localhost:4001".into()),
            has_review_url: true,
            ..Default::default()
        });
        let decoded: ReviewOpenTarget =
            serde_json::from_str(&serde_json::to_string(&serving).unwrap()).unwrap();
        assert_eq!(decoded, serving);
        // A field added later is missing from an older answer: it decodes.
        let older: ReviewOpenTarget = serde_json::from_str(r#"{"serving":{"slot":"rev-2"}}"#).unwrap();
        assert!(matches!(older, ReviewOpenTarget::Serving(ref i) if i.slot == "rev-2"));
    }
}
