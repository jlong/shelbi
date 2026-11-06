//! Workspace slot occupancy: the one answer to "is this slot busy, and with
//! what?".
//!
//! A slot has no stored state of its own. Whether it is busy follows from which
//! open card names it in `assigned_to` and from that card's **workflow-resolved
//! status category** (never a status literal, so a renamed active status or a
//! custom agent-owned gate counts like the stock `in-progress`). Every dispatch
//! path and every view asks here and picks the variants that matter to it:
//!
//! - **May a dispatch take over this slot?** [`SlotOccupancy::is_busy`].
//! - **May a pane on this slot be killed for another card's sake?** Only when it
//!   is [`SlotOccupancy::Idle`]: any card naming the slot could be the one its
//!   pane is working on, because a lagging index can show it in an older column.
//! - **May this card's review be placed here, or the slot be renamed away?**
//!   [`SlotOccupancy::is_claimed`].
//!
//! Live-session facts (a user shell, an orphan, an unreachable machine) are not
//! part of this answer. They need a probe per slot, which a sidebar tick can't
//! afford, so the callers that need them layer them on top.

use std::collections::{HashMap, HashSet};

use shelbi_core::{Issue, Project, StatusCategory, Workflow};

use crate::{IssueFile, Result};

/// What occupies one workspace slot.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SlotOccupancy {
    /// No open card names the slot.
    #[default]
    Idle,
    /// An `active`-category card: a developer or an agent-owned gate.
    Working { issue: Issue },
    /// A `handoff`-category card on a `review`-tagged slot: a loaded review.
    Serving { issue: Issue },
    /// A `handoff`-category card still naming a non-review slot. That is the dev
    /// slot that built it: a queued review keeps its `assigned_to`, and the
    /// slot's worker was closed at the ready handoff, so the slot is reusable.
    Released { issue: Issue },
    /// A `backlog`/`ready` card assigned ahead of its start. It has no worker.
    PreAssigned { issue: Issue },
}

impl SlotOccupancy {
    /// A worker is running (or due to run) here, so a dispatch must not take the
    /// slot over and a view reports it busy.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Working { .. } | Self::Serving { .. })
    }

    /// [`Self::is_busy`], or holding a handed-off card. A review can load onto
    /// an untagged slot when its status declares no tags, so a loader or a pool
    /// edit can't assume a handoff card on such a slot has no live pane.
    pub fn is_claimed(&self) -> bool {
        self.is_busy() || matches!(self, Self::Released { .. })
    }

    /// The card behind this occupancy, or `None` when idle.
    pub fn issue(&self) -> Option<&Issue> {
        match self {
            Self::Idle => None,
            Self::Working { issue }
            | Self::Serving { issue }
            | Self::Released { issue }
            | Self::PreAssigned { issue } => Some(issue),
        }
    }

    /// [`Self::issue`], by value.
    pub fn into_issue(self) -> Option<Issue> {
        match self {
            Self::Idle => None,
            Self::Working { issue }
            | Self::Serving { issue }
            | Self::Released { issue }
            | Self::PreAssigned { issue } => Some(issue),
        }
    }
}

/// How one card holds the slot it names, strongest first: when a diverged board
/// names a slot from several cards, the strongest hold is the slot's occupancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Hold {
    Working,
    Serving,
    Released,
    PreAssigned,
}

/// Every assigned open card on a board, classified. Build it once per decision
/// or per refresh; it reads each workflow it needs one time.
#[derive(Debug, Clone, Default)]
pub struct BoardOccupancy {
    /// Cards in board order. Terminal and unassigned cards are dropped.
    cards: Vec<(IssueFile, Hold)>,
}

impl BoardOccupancy {
    /// Read `project`'s open board with each card's owner resolved from the
    /// local assignment overlay, which leads the published index. A cold board
    /// reads as all idle: dispatch still can't double-book, because it persists
    /// the assignment before it spawns.
    pub fn read(project: &Project) -> Result<Self> {
        let mut board =
            crate::read_board_with_cfg(&project.name, &project.issue_tracker)?.into_issues();
        crate::fold_assignment_overlay(&project.name, &project.issue_tracker, &mut board);
        Ok(Self::from_board(project, board))
    }

    /// Classify `board`. The caller has already resolved its owners
    /// ([`crate::fold_assignment_overlay`]) if the board came from the index.
    pub fn from_board(project: &Project, board: Vec<IssueFile>) -> Self {
        let mut workflows: HashMap<String, Option<Workflow>> = HashMap::new();
        let cards = board
            .into_iter()
            .filter_map(|tf| {
                let hold = hold_of(project, &tf.task, &mut workflows)?;
                Some((tf, hold))
            })
            .collect();
        Self { cards }
    }

    /// What occupies `workspace`.
    pub fn of(&self, workspace: &str) -> SlotOccupancy {
        self.occupancy(workspace, None)
    }

    /// What occupies `workspace`, leaving out `task_id`: the question a dispatch
    /// of that card asks, since a card never blocks itself.
    pub fn of_other_than(&self, workspace: &str, task_id: &str) -> SlotOccupancy {
        self.occupancy(workspace, Some(task_id))
    }

    fn occupancy(&self, workspace: &str, exclude: Option<&str>) -> SlotOccupancy {
        let strongest = self
            .cards
            .iter()
            .filter(|(tf, _)| {
                tf.task.assigned_to.as_deref() == Some(workspace)
                    && Some(tf.task.id.as_str()) != exclude
            })
            .min_by_key(|(_, hold)| *hold);
        let Some((tf, hold)) = strongest else {
            return SlotOccupancy::Idle;
        };
        let issue = tf.task.clone();
        match hold {
            Hold::Working => SlotOccupancy::Working { issue },
            Hold::Serving => SlotOccupancy::Serving { issue },
            Hold::Released => SlotOccupancy::Released { issue },
            Hold::PreAssigned => SlotOccupancy::PreAssigned { issue },
        }
    }

    /// The slots a dispatch must leave alone ([`SlotOccupancy::is_busy`]),
    /// leaving out `exclude`'s own hold.
    pub fn busy_slots(&self, exclude: Option<&str>) -> HashSet<String> {
        self.slots_held(exclude, |hold| matches!(hold, Hold::Working | Hold::Serving))
    }

    /// The slots a loader must leave alone ([`SlotOccupancy::is_claimed`]),
    /// leaving out `exclude`'s own hold.
    pub fn claimed_slots(&self, exclude: Option<&str>) -> HashSet<String> {
        self.slots_held(exclude, |hold| hold != Hold::PreAssigned)
    }

    fn slots_held(&self, exclude: Option<&str>, holds: impl Fn(Hold) -> bool) -> HashSet<String> {
        self.cards
            .iter()
            .filter(|(tf, hold)| holds(*hold) && Some(tf.task.id.as_str()) != exclude)
            .filter_map(|(tf, _)| tf.task.assigned_to.clone())
            .collect()
    }

    /// The [`SlotOccupancy::Working`] cards, in board order.
    pub fn working(&self) -> impl Iterator<Item = &IssueFile> {
        self.cards_with(Hold::Working)
    }

    /// The [`SlotOccupancy::Serving`] cards, in board order.
    pub fn serving(&self) -> impl Iterator<Item = &IssueFile> {
        self.cards_with(Hold::Serving)
    }

    fn cards_with(&self, wanted: Hold) -> impl Iterator<Item = &IssueFile> {
        self.cards
            .iter()
            .filter(move |(_, hold)| *hold == wanted)
            .map(|(tf, _)| tf)
    }
}

/// How `task` holds the slot it names, or `None` when it holds none.
fn hold_of(
    project: &Project,
    task: &Issue,
    workflows: &mut HashMap<String, Option<Workflow>>,
) -> Option<Hold> {
    let workspace = task.assigned_to.as_deref()?;
    match status_category(project, task, workflows) {
        StatusCategory::Active => Some(Hold::Working),
        StatusCategory::Handoff => {
            let on_review_slot = project
                .workspace(workspace)
                .is_some_and(|w| project.effective_tags(w).contains("review"));
            Some(if on_review_slot {
                Hold::Serving
            } else {
                Hold::Released
            })
        }
        StatusCategory::Backlog | StatusCategory::Ready => Some(Hold::PreAssigned),
        StatusCategory::Done | StatusCategory::Archived => None,
    }
}

/// `task`'s status category through its workflow. A status the workflow doesn't
/// declare keeps its stock category, as a legacy `in-progress` card does under a
/// workflow that renamed it. When the workflow won't load and the built-in one
/// doesn't know the status either, the card could be a running gate, so it counts
/// as active: refusing a dispatch is recoverable and killing its worker is not.
fn status_category(
    project: &Project,
    task: &Issue,
    workflows: &mut HashMap<String, Option<Workflow>>,
) -> StatusCategory {
    let name = crate::resolve_task_workflow_name(project, task);
    let workflow = workflows
        .entry(name.to_string())
        .or_insert_with(|| crate::load_workflow(&project.name, name).ok());
    let status = task.column.as_str();
    match workflow {
        Some(workflow) => workflow
            .status(status)
            .map_or_else(|| task.column.category(), |s| s.category),
        None => shelbi_core::default_workflow()
            .status(status)
            .map_or(StatusCategory::Active, |s| s.category),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::Column;

    const PROJECT: &str = r#"
name: occ
repo: /tmp/occ
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: hub
    kind: local
    work_dir: /tmp/occ
workspaces:
  - { name: alpha, machine: hub }
  - { name: beta, machine: hub }
  - { name: review-1, machine: hub, tags: [review] }
"#;

    /// The project as `load_project` returns it: `name` comes from the config's
    /// file name, not its YAML, and a workflow can't load without it.
    fn project() -> Project {
        let mut project: Project = serde_yaml::from_str(PROJECT).unwrap();
        project.name = "occ".into();
        project
    }

    fn card(id: &str, column: &str, workspace: Option<&str>) -> IssueFile {
        let now = chrono::Utc::now();
        IssueFile {
            task: Issue {
                id: id.into(),
                title: id.into(),
                column: Column::from_status_id(column),
                priority: 0,
                assigned_to: workspace.map(str::to_string),
                workflow: None,
                branch: None,
                pr: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: now,
                updated_at: now,
                params: Default::default(),
            },
            body: String::new(),
            tracker_assignees: Vec::new(),
        }
    }

    /// Run `f` with a temporary `SHELBI_HOME`, holding the crate's env lock.
    fn with_home(f: impl FnOnce()) {
        let _guard = crate::test_lock::LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-occupancy-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&home).unwrap();
        let previous = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", &home);
        f();
        match previous {
            Some(value) => std::env::set_var("SHELBI_HOME", value),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A workflow with an agent-owned gate and a renamed primary active status,
    /// as `<project>/workflows/default.yaml`.
    fn write_custom_workflow() {
        let dir = crate::workflows_dir("occ").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("statuses.yaml"),
            r#"
statuses:
  - { id: backlog,            name: Backlog,    category: backlog }
  - { id: todo,               name: Todo,       category: ready }
  - { id: coding,             name: Coding,     category: active }
  - { id: adversarial-review, name: Gate,       category: active }
  - { id: signoff,            name: Signoff,    category: handoff }
  - { id: done,               name: Done,       category: done }
"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("default.yaml"),
            r#"
name: default
statuses:
  - { id: backlog,            owner: user }
  - { id: todo,               owner: agent, agent: orchestrator }
  - { id: coding,             owner: agent, agent: developer }
  - { id: adversarial-review, owner: agent, agent: adversarial-review }
  - { id: signoff,            owner: user }
  - { id: done,               owner: user }
"#,
        )
        .unwrap();
    }

    #[test]
    fn categories_decide_the_occupancy_not_status_names() {
        with_home(|| {
            write_custom_workflow();
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![
                    card("gate", "adversarial-review", Some("alpha")),
                    card("renamed", "coding", Some("beta")),
                    card("loaded", "signoff", Some("review-1")),
                ],
            );
            assert!(matches!(occ.of("alpha"), SlotOccupancy::Working { .. }));
            assert!(matches!(occ.of("beta"), SlotOccupancy::Working { .. }));
            assert!(matches!(occ.of("review-1"), SlotOccupancy::Serving { .. }));
            assert_eq!(
                occ.busy_slots(None),
                ["alpha", "beta", "review-1"].map(String::from).into()
            );
        });
    }

    #[test]
    fn a_queued_review_doesnt_keep_its_dev_slot_busy() {
        with_home(|| {
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![card("queued", "review", Some("alpha"))],
            );
            let slot = occ.of("alpha");
            assert!(matches!(slot, SlotOccupancy::Released { .. }));
            assert!(!slot.is_busy(), "the dev worker closed at the handoff");
            assert!(slot.is_claimed());
            assert!(occ.busy_slots(None).is_empty());
            assert_eq!(occ.claimed_slots(None), ["alpha".to_string()].into());
        });
    }

    #[test]
    fn a_preassigned_card_names_the_slot_without_occupying_it() {
        with_home(|| {
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![card("next", "todo", Some("alpha"))],
            );
            let slot = occ.of("alpha");
            assert!(matches!(slot, SlotOccupancy::PreAssigned { .. }));
            assert!(!slot.is_claimed());
            assert_eq!(slot.issue().map(|i| i.id.as_str()), Some("next"));
        });
    }

    #[test]
    fn a_card_never_occupies_a_slot_against_itself() {
        with_home(|| {
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![card("self", "in-progress", Some("alpha"))],
            );
            assert_eq!(occ.of_other_than("alpha", "self"), SlotOccupancy::Idle);
            assert!(occ.busy_slots(Some("self")).is_empty());
            assert!(occ.of("alpha").is_busy());
        });
    }

    #[test]
    fn the_strongest_hold_wins_on_a_diverged_board() {
        with_home(|| {
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![
                    card("preassigned", "todo", Some("alpha")),
                    card("queued", "review", Some("alpha")),
                    card("live", "in-progress", Some("alpha")),
                ],
            );
            assert_eq!(
                occ.of("alpha").issue().map(|i| i.id.as_str()),
                Some("live")
            );
            assert_eq!(
                occ.of_other_than("alpha", "live").issue().map(|i| i.id.as_str()),
                Some("queued")
            );
        });
    }

    #[test]
    fn terminal_and_unassigned_cards_occupy_nothing() {
        with_home(|| {
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![
                    card("finished", "done", Some("alpha")),
                    card("unrouted", "in-progress", None),
                ],
            );
            assert_eq!(occ.of("alpha"), SlotOccupancy::Idle);
            assert_eq!(occ.working().count(), 0);
        });
    }

    #[test]
    fn a_stock_status_keeps_its_category_under_a_workflow_that_renamed_it() {
        with_home(|| {
            write_custom_workflow();
            let occ = BoardOccupancy::from_board(
                &project(),
                vec![card("legacy", "in-progress", Some("alpha"))],
            );
            assert!(occ.of("alpha").is_busy());
        });
    }

    #[test]
    fn an_unreadable_workflow_never_reads_a_custom_status_as_free() {
        with_home(|| {
            let dir = crate::workflows_dir("occ").unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("broken.yaml"), "name: [not, a, name\n").unwrap();
            let mut gate = card("gate", "adversarial-review", Some("alpha"));
            gate.task.workflow = Some("broken".into());
            let mut ready = card("next", "todo", Some("beta"));
            ready.task.workflow = Some("broken".into());
            let occ = BoardOccupancy::from_board(&project(), vec![gate, ready]);
            assert!(occ.of("alpha").is_busy(), "could be a running gate");
            assert!(!occ.of("beta").is_busy(), "a stock status is still known");
        });
    }
}
