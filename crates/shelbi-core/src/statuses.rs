//! Project-level status identity, declared in `workflows/statuses.yaml`.
//!
//! `statuses.yaml` is the single source of truth for **status identity** in
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

/// Top-level shape of `workflows/statuses.yaml`.
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

/// One entry in `statuses.yaml` — the project-wide identity of a status.
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
    /// non-empty names, and at least one **terminal** category so tasks
    /// can actually complete. Categories are otherwise constrained by
    /// serde's enum decode.
    ///
    /// The terminal check is a hard error: a status set with no `done` or
    /// `archived` category is degenerate — a task could never leave the
    /// board, and every terminal-assuming consumer ([`Task::is_blocked`],
    /// the `Done`-column mapping) would silently have nothing to key off.
    /// Softer coherence issues (a missing `handoff`, a duplicated
    /// single-instance category) surface as non-fatal warnings via
    /// [`ProjectStatuses::category_warnings`] rather than blocking load.
    pub fn validate(&self) -> crate::Result<()> {
        if self.statuses.is_empty() {
            return Err(invalid("statuses.yaml must declare at least one status"));
        }
        let mut seen: HashSet<&str> = HashSet::with_capacity(self.statuses.len());
        for st in &self.statuses {
            if st.id.trim().is_empty() {
                return Err(invalid("statuses.yaml: status id must not be empty"));
            }
            if st.name.trim().is_empty() {
                return Err(invalid(format!(
                    "statuses.yaml: status `{}`: name must not be empty",
                    st.id
                )));
            }
            if !seen.insert(st.id.as_str()) {
                return Err(invalid(format!(
                    "statuses.yaml: duplicate status id `{}`",
                    st.id
                )));
            }
        }
        let has_terminal = self.statuses.iter().any(|st| {
            matches!(
                st.category,
                StatusCategory::Done | StatusCategory::Archived
            )
        });
        if !has_terminal {
            return Err(invalid(
                "statuses.yaml declares no terminal status — at least one status must \
                 have category `done` or `archived` so tasks can reach a completed \
                 state",
            ));
        }
        Ok(())
    }

    /// Non-fatal coherence warnings about the category *set* — surfaced by
    /// the loader (`tracing::warn!`) rather than blocking project load.
    /// Returns an empty vector for a coherent set.
    ///
    /// Generic code assumes a single-instance mapping for the positional
    /// categories: Zen's merge probe and the TUI's `Handoff → Review`
    /// column mapping expect a `handoff` to exist, and the category→column
    /// fallback resolves to the *first* status of a category, so a second
    /// `backlog`/`ready`/`active`/`handoff` status is unreachable by that
    /// path. We warn — not error — because the any-to-any transition policy
    /// (`Plans/workflows.md` §11) deliberately permits non-canonical sets;
    /// this is a guardrail, not a straitjacket.
    pub fn category_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();

        if self.count_category(StatusCategory::Handoff) == 0 {
            out.push(
                "statuses.yaml declares no `handoff` status — the Zen merge probe and \
                 the TUI review lane will have nothing to act on"
                    .to_string(),
            );
        }

        // Positional categories the UI/orchestrator resolve to a single
        // status. Duplicates aren't illegal but the second instance is
        // unreachable by the category→column fallback, so flag it.
        for cat in [
            StatusCategory::Backlog,
            StatusCategory::Ready,
            StatusCategory::Active,
            StatusCategory::Handoff,
        ] {
            if self.count_category(cat) > 1 {
                out.push(format!(
                    "statuses.yaml declares more than one `{cat}` status — generic code \
                     (category→column mapping, Zen probe) keys off the first, so the \
                     others are only reachable by their exact id"
                ));
            }
        }

        out
    }

    /// How many declared statuses carry `cat`.
    fn count_category(&self, cat: StatusCategory) -> usize {
        self.statuses.iter().filter(|s| s.category == cat).count()
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

/// The canonical six-status default written into `workflows/statuses.yaml`
/// when a fresh project is materialized (or when an existing project on
/// reload has no `statuses.yaml` and no legacy inline workflows to migrate
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
    fn rejects_set_with_no_terminal_category() {
        // A set that can never reach `done`/`archived` is degenerate —
        // tasks could never leave the board.
        let yaml = r#"
statuses:
  - { id: backlog, name: Backlog, category: backlog }
  - { id: todo,    name: Todo,    category: ready   }
  - { id: doing,   name: Doing,   category: active  }
"#;
        let err = ProjectStatuses::from_yaml_str(yaml).unwrap_err();
        assert!(
            matches!(err, Error::InvalidProjectStatuses(ref m) if m.contains("no terminal")),
            "got: {err}"
        );
    }

    #[test]
    fn archived_alone_satisfies_terminal_requirement() {
        // `archived` is terminal too — a set with only `archived` (no
        // `done`) still passes the hard check.
        let yaml = r#"
statuses:
  - { id: doing,    name: Doing,    category: active   }
  - { id: canceled, name: Canceled, category: archived }
"#;
        assert!(ProjectStatuses::from_yaml_str(yaml).is_ok());
    }

    #[test]
    fn default_set_has_no_category_warnings() {
        assert!(default_project_statuses().category_warnings().is_empty());
    }

    #[test]
    fn warns_on_missing_handoff() {
        // Straight active -> done with no handoff: valid (has a terminal)
        // but the Zen probe / review lane have nothing to key off.
        let yaml = r#"
statuses:
  - { id: doing, name: Doing, category: active }
  - { id: done,  name: Done,  category: done   }
"#;
        let ps = ProjectStatuses::from_yaml_str(yaml).unwrap();
        let warnings = ps.category_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("no `handoff`")),
            "got: {warnings:?}"
        );
    }

    #[test]
    fn warns_on_duplicate_single_instance_category() {
        let yaml = r#"
statuses:
  - { id: review,   name: Review,   category: handoff }
  - { id: qa,       name: QA,       category: handoff }
  - { id: done,     name: Done,     category: done    }
"#;
        let ps = ProjectStatuses::from_yaml_str(yaml).unwrap();
        let warnings = ps.category_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("more than one `handoff`")),
            "got: {warnings:?}"
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
