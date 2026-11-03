//! The JSON wire form of the read models a client draws.
//!
//! The remote window (a native window over a project on another machine)
//! runs these readers on the far machine and sends their results back over
//! one SSH relay channel. So every model a client reads — [`SidebarModel`],
//! [`BoardRead`], [`ClosedPage`], [`MachinesModel`], [`ActivityData`],
//! [`ChangelogData`], [`ProjectSummary`], [`SearchCorpus`], the
//! orchestrator's answers in [`crate::source::answers`] (`WorkspaceDetails`,
//! the review targets and slots), and `shelbi-state`'s `BoardReport` —
//! derives `Serialize`/`Deserialize`.
//!
//! Two rules hold for all of them:
//!
//! - **Every field decodes when missing** (`#[serde(default)]`, on the struct
//!   when it has a `Default`, else on each field). A field added later then
//!   never breaks decoding JSON that predates it. Fields whose type has no
//!   `Default` name one of the functions below.
//! - **The shape is pinned.** The tests in this module compare each model's
//!   JSON with a literal, so a change to the wire form fails CI until the
//!   fixture (and, once the relay ships, the protocol version) is updated on
//!   purpose.
//!
//! [`SidebarModel`]: crate::view::SidebarModel
//! [`BoardRead`]: crate::board::BoardRead
//! [`ClosedPage`]: crate::board::ClosedPage
//! [`MachinesModel`]: crate::view::MachinesModel
//! [`ActivityData`]: crate::activity::ActivityData
//! [`ChangelogData`]: crate::changelog::ChangelogData
//! [`ProjectSummary`]: crate::settings::summary::ProjectSummary
//! [`SearchCorpus`]: crate::overlay::palette_search::SearchCorpus

use shelbi_core::{Column, MachineKind, StatusCategory};
use shelbi_state::{OwnershipGate, WorkspaceState};

use crate::board::BoardOutcome;
use crate::exec::EditTarget;

/// A missing `Result` field decodes as `Ok` of the empty value.
pub(crate) fn ok_default<T: Default, E>() -> Result<T, E> {
    Ok(T::default())
}

/// A missing board outcome is the loading state, never an empty board.
pub(crate) fn cold_board() -> BoardOutcome {
    BoardOutcome::Cold { banner: None }
}

/// A missing ownership gate fails closed, as an unreadable one does.
pub(crate) fn fail_closed() -> OwnershipGate {
    OwnershipGate::FailClosed
}

/// A missing machine kind is the hub's own machine.
pub(crate) fn default_machine_kind() -> MachineKind {
    MachineKind::Local
}

/// A missing column is the first one, `backlog`.
pub(crate) fn default_column() -> Column {
    Column::from_status_id("backlog")
}

/// A missing status category is the first one, `Backlog`.
pub(crate) fn default_category() -> StatusCategory {
    StatusCategory::Backlog
}

/// A missing workspace state is the at-rest one: sitting at the prompt.
pub(crate) fn default_workspace_state() -> WorkspaceState {
    WorkspaceState::AwaitingInput
}

/// A missing edit target is the project settings file.
pub(crate) fn default_edit_target() -> EditTarget {
    EditTarget::Project
}

/// The wire form of [`BoardRead::workflows`](crate::board::BoardRead).
///
/// A [`Workflow`]'s own serialization is its on-disk form, which leaves each
/// status's `name` and `category` out: they live in `statuses.yaml`, and the
/// loader fills them back in. A decoded read can't count on having the
/// statuses to resolve against (their load may have failed), so each
/// workflow travels with the identities it was resolved to,
/// `{"workflow": {…}, "statuses": [{"id", "name", "category"}]}`, and
/// decoding puts them back.
pub(crate) mod workflows {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use shelbi_core::{StatusCategory, Workflow};

    #[derive(Serialize)]
    struct WorkflowOut<'a> {
        workflow: &'a Workflow,
        statuses: Vec<IdentityOut<'a>>,
    }

    #[derive(Serialize)]
    struct IdentityOut<'a> {
        id: &'a str,
        name: &'a str,
        category: StatusCategory,
    }

    #[derive(Deserialize)]
    struct WorkflowIn {
        workflow: Workflow,
        #[serde(default)]
        statuses: Vec<IdentityIn>,
    }

    #[derive(Deserialize)]
    struct IdentityIn {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        category: Option<StatusCategory>,
    }

    impl WorkflowIn {
        fn into_workflow(self) -> Workflow {
            let mut workflow = self.workflow;
            for status in &mut workflow.statuses {
                let Some(identity) = self.statuses.iter().find(|i| i.id == status.id) else {
                    continue;
                };
                if let Some(name) = &identity.name {
                    status.name = name.clone();
                }
                if let Some(category) = identity.category {
                    status.category = category;
                }
            }
            workflow
        }
    }

    pub(crate) fn serialize<S: Serializer>(
        workflows: &Result<Vec<Workflow>, String>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let wire: Result<Vec<WorkflowOut<'_>>, &String> = workflows.as_ref().map(|list| {
            list.iter()
                .map(|workflow| WorkflowOut {
                    workflow,
                    statuses: workflow
                        .statuses
                        .iter()
                        .map(|st| IdentityOut {
                            id: &st.id,
                            name: &st.name,
                            category: st.category,
                        })
                        .collect(),
                })
                .collect()
        });
        wire.serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Result<Vec<Workflow>, String>, D::Error> {
        let wire = Result::<Vec<WorkflowIn>, String>::deserialize(d)?;
        Ok(wire.map(|list| list.into_iter().map(WorkflowIn::into_workflow).collect()))
    }
}

#[cfg(test)]
mod tests;
