//! Project-level status identity, declared in `workflows/statuses.yml`.
//!
//! `statuses.yml` is the single source of truth for **status identity** in
//! a project — every workflow file shrinks to declaring only the per-status
//! `owner` and optional `agent`, referencing this file by `id`. Status
//! declaration order here is the canonical column order rendered by the
//! TUI all-view; workflows may pick a subset but cannot reorder.
//!
//! See `Plans/shared-statuses.md` §1 and §2 for the schema rationale.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::workflow::StatusCategory;
use crate::Error;

/// Top-level shape of `workflows/statuses.yml`.
///
/// Round-trips through serde; call [`ProjectStatuses::validate`] (or
/// [`ProjectStatuses::from_yaml_str`]) before trusting the contents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ProjectStatuses {
    /// Canonical, ordered list of statuses. Ordering is significant — it's
    /// the left-to-right column order in the project-wide TUI all-view.
    /// Workflows reference these by [`ProjectStatus::id`] and inherit
    /// the ordering they see here.
    pub statuses: Vec<ProjectStatus>,
}

/// One entry in `statuses.yml` — the project-wide identity of a status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectStatus {
    /// Stable identifier, referenced from every workflow's `statuses:`
    /// list. Conventional form is lowercase kebab-case.
    pub id: String,

    /// User-facing display label (e.g. `Backlog`, `In Progress`).
    pub name: String,

    /// Closed semantic category — generic code keys off this so a project
    /// that renames a status keeps its semantics intact.
    pub category: StatusCategory,
}

impl ProjectStatuses {
    /// Parse YAML and validate in one step.
    pub fn from_yaml_str(s: &str) -> crate::Result<Self> {
        let ps: ProjectStatuses = serde_yaml::from_str(s)?;
        ps.validate()?;
        Ok(ps)
    }

    /// Semantic validation: at least one entry, unique non-empty ids,
    /// non-empty names. Categories are already constrained by serde's
    /// enum decode.
    pub fn validate(&self) -> crate::Result<()> {
        if self.statuses.is_empty() {
            return Err(invalid("statuses.yml must declare at least one status"));
        }
        let mut seen: HashSet<&str> = HashSet::with_capacity(self.statuses.len());
        for st in &self.statuses {
            if st.id.trim().is_empty() {
                return Err(invalid("statuses.yml: status id must not be empty"));
            }
            if st.name.trim().is_empty() {
                return Err(invalid(format!(
                    "statuses.yml: status `{}`: name must not be empty",
                    st.id
                )));
            }
            if !seen.insert(st.id.as_str()) {
                return Err(invalid(format!(
                    "statuses.yml: duplicate status id `{}`",
                    st.id
                )));
            }
        }
        Ok(())
    }

    /// Look up a status by id. Linear scan; the list is small.
    pub fn get(&self, id: &str) -> Option<&ProjectStatus> {
        self.statuses.iter().find(|s| s.id == id)
    }

    /// Index of a status in the canonical ordering, used to sort
    /// workflow-scoped subsets back into the project-wide column order.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.statuses.iter().position(|s| s.id == id)
    }

    /// All ids in declared order. Useful for error messages that need to
    /// echo the valid set ("expected one of: …").
    pub fn ids(&self) -> Vec<&str> {
        self.statuses.iter().map(|s| s.id.as_str()).collect()
    }
}

/// The canonical six-status default written into `workflows/statuses.yml`
/// when a fresh project is materialized (or when an existing project on
/// reload has no `statuses.yml` and no legacy inline workflows to migrate
/// from). Matches the wireframe in `Plans/shared-statuses.md` §Wireframes.
pub fn default_project_statuses() -> ProjectStatuses {
    ProjectStatuses {
        statuses: vec![
            ProjectStatus {
                id: "backlog".into(),
                name: "Backlog".into(),
                category: StatusCategory::Backlog,
            },
            ProjectStatus {
                id: "todo".into(),
                name: "Todo".into(),
                category: StatusCategory::Ready,
            },
            ProjectStatus {
                id: "in-progress".into(),
                name: "In Progress".into(),
                category: StatusCategory::Active,
            },
            ProjectStatus {
                id: "review".into(),
                name: "Review".into(),
                category: StatusCategory::Handoff,
            },
            ProjectStatus {
                id: "done".into(),
                name: "Done".into(),
                category: StatusCategory::Done,
            },
            ProjectStatus {
                id: "canceled".into(),
                name: "Canceled".into(),
                category: StatusCategory::Archived,
            },
        ],
    }
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::InvalidProjectStatuses(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_YAML: &str = r#"
statuses:
  - id: backlog
    name: Backlog
    category: backlog
  - id: todo
    name: Todo
    category: ready
  - id: in-progress
    name: In Progress
    category: active
  - id: review
    name: Review
    category: handoff
  - id: done
    name: Done
    category: done
  - id: canceled
    name: Canceled
    category: archived
"#;

    #[test]
    fn parses_default_yaml_into_canonical_six() {
        let ps = ProjectStatuses::from_yaml_str(DEFAULT_YAML).expect("parse");
        assert_eq!(ps, default_project_statuses());
    }

    #[test]
    fn round_trips_through_yaml() {
        let ps = default_project_statuses();
        let y = serde_yaml::to_string(&ps).unwrap();
        let back = ProjectStatuses::from_yaml_str(&y).unwrap();
        assert_eq!(ps, back);
    }

    #[test]
    fn rejects_empty_list() {
        let err = ProjectStatuses::from_yaml_str("statuses: []").unwrap_err();
        assert!(
            matches!(err, Error::InvalidProjectStatuses(ref m) if m.contains("at least one")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_duplicate_id() {
        let yaml = r#"
statuses:
  - { id: backlog, name: Backlog, category: backlog }
  - { id: backlog, name: Backlog2, category: ready  }
"#;
        let err = ProjectStatuses::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidProjectStatuses(ref m) if m.contains("duplicate status id")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_blank_id() {
        let yaml = r#"
statuses:
  - { id: "", name: Foo, category: ready }
"#;
        let err = ProjectStatuses::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidProjectStatuses(ref m) if m.contains("id must not be empty")),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_blank_name() {
        let yaml = r#"
statuses:
  - { id: backlog, name: "", category: backlog }
"#;
        let err = ProjectStatuses::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidProjectStatuses(ref m) if m.contains("name must not be empty")),
            "got: {err}"
        );
    }

    #[test]
    fn position_returns_declared_order() {
        let ps = default_project_statuses();
        assert_eq!(ps.position("backlog"), Some(0));
        assert_eq!(ps.position("done"), Some(4));
        assert_eq!(ps.position("ghost"), None);
    }

    #[test]
    fn ids_lists_them_in_order() {
        let ps = default_project_statuses();
        assert_eq!(
            ps.ids(),
            vec!["backlog", "todo", "in-progress", "review", "done", "canceled"]
        );
    }
}
