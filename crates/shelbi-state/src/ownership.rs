//! Tracker ownership: which issues on a shared remote tracker belong to the
//! authenticated user, and so may be started by this user's orchestrator.
//!
//! Several people can share one GitHub Issues tracker, each running their own
//! Shelbi hub. Without a gate, one person's orchestrator would dispatch any card
//! that reached a `ready` status, including a teammate's. So on a remote tracker
//! an issue is this user's only when its native tracker assignees
//! ([`IssueFile::tracker_assignees`]) include the authenticated login, and every
//! orchestrator-driven start (the Zen scan, `issue start` / `issue resume`, the
//! daemon's active-gate dispatch and review auto-load) goes through the one
//! [`OwnershipGate`] here.
//!
//! The filesystem backend has no ownership concept: its gate is
//! [`OwnershipGate::Unrestricted`] and nothing is filtered, so local boards
//! behave exactly as before. When the login can't be resolved the gate **fails
//! closed** ([`OwnershipGate::FailClosed`]): starting an issue we can't prove is
//! the user's is the one outcome this exists to prevent. Never fail open.
//!
//! A human can claim an issue explicitly with `shelbi issue start <id> --take`
//! ([`ensure_owned_for_dispatch`] with `take`), which assigns it to them on the
//! tracker first. The orchestrator never passes that override on its own.

use std::fmt;

use shelbi_core::{IssueTrackerConfig, Result};

use crate::{IssueFile, IssueStore};

/// Who "the user" is for tracker ownership, and hence which issues pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipGate {
    /// Filesystem backend (or any backend with no tracker-side ownership): no
    /// ownership filtering.
    Unrestricted,
    /// Remote tracker with the authenticated login resolved: an issue passes
    /// only when its tracker assignees include this login (case-insensitive,
    /// matching GitHub login semantics).
    RequireAssignee(String),
    /// Remote tracker whose authenticated login could not be resolved: nothing
    /// passes (fail closed).
    FailClosed,
}

/// Whose an issue is on a remote tracker, relative to the authenticated user.
/// What `issue list` / `show`, the boards and the `owner=` event token show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerOwner {
    /// Assigned to the authenticated user (possibly alongside others).
    Me,
    /// Assigned only to other accounts (their logins).
    Other(Vec<String>),
    /// Nobody is assigned on the tracker.
    Unassigned,
    /// The user's own login is unknown, so it can't be told whether these
    /// assignees include them (an empty list reads as unassigned instead).
    Unknown(Vec<String>),
}

impl TrackerOwner {
    /// Whether the issue is the authenticated user's.
    pub fn is_mine(&self) -> bool {
        matches!(self, TrackerOwner::Me)
    }

    /// The `owner=` value on task event lines: `me`, `other:<login>[,<login>]`,
    /// `unassigned`, or `unknown` (the user's login is unresolved). One token,
    /// so a reaction rule can match it without another call.
    pub fn token(&self) -> String {
        match self {
            TrackerOwner::Me => "me".to_string(),
            TrackerOwner::Other(logins) => format!("other:{}", logins.join(",")),
            TrackerOwner::Unassigned => "unassigned".to_string(),
            TrackerOwner::Unknown(_) => "unknown".to_string(),
        }
    }

    /// A short label for a list column or a board card: `me`, `@alice`,
    /// `@alice, @bob`, or `unassigned`. An unknown owner names the assignees,
    /// since that is all that is known.
    pub fn label(&self) -> String {
        match self {
            TrackerOwner::Me => "me".to_string(),
            TrackerOwner::Other(logins) | TrackerOwner::Unknown(logins) => at_logins(logins),
            TrackerOwner::Unassigned => "unassigned".to_string(),
        }
    }
}

/// `@a, @b` for a non-empty login list.
fn at_logins(logins: &[String]) -> String {
    logins
        .iter()
        .map(|l| format!("@{l}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl OwnershipGate {
    /// Whether an issue carrying these tracker assignees passes the gate.
    pub fn allows(&self, tracker_assignees: &[String]) -> bool {
        match self {
            OwnershipGate::Unrestricted => true,
            OwnershipGate::RequireAssignee(login) => tracker_assignees
                .iter()
                .any(|a| a.eq_ignore_ascii_case(login)),
            OwnershipGate::FailClosed => false,
        }
    }

    /// Whose an issue with these assignees is, or `None` on an unrestricted
    /// (filesystem) board, which has no ownership to show.
    pub fn owner(&self, tracker_assignees: &[String]) -> Option<TrackerOwner> {
        match self {
            OwnershipGate::Unrestricted => None,
            _ if tracker_assignees.is_empty() => Some(TrackerOwner::Unassigned),
            OwnershipGate::RequireAssignee(_) if self.allows(tracker_assignees) => {
                Some(TrackerOwner::Me)
            }
            OwnershipGate::RequireAssignee(_) => {
                Some(TrackerOwner::Other(tracker_assignees.to_vec()))
            }
            OwnershipGate::FailClosed => Some(TrackerOwner::Unknown(tracker_assignees.to_vec())),
        }
    }

    /// The authenticated login, when this gate resolved one.
    pub fn login(&self) -> Option<&str> {
        match self {
            OwnershipGate::RequireAssignee(login) => Some(login),
            _ => None,
        }
    }

    /// Refuse issue `id` unless it passes, with a message that names its
    /// assignees and how to claim it.
    pub fn check(&self, id: &str, tracker_assignees: &[String]) -> std::result::Result<(), NotOwned> {
        if self.allows(tracker_assignees) {
            return Ok(());
        }
        Err(NotOwned {
            id: id.to_string(),
            login: self.login().map(str::to_string),
            owner: self
                .owner(tracker_assignees)
                .unwrap_or(TrackerOwner::Unassigned),
        })
    }
}

/// Why the gate refused an issue. `Display` is the user-facing refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOwned {
    pub id: String,
    /// The authenticated login, when it resolved.
    pub login: Option<String>,
    pub owner: TrackerOwner,
}

impl fmt::Display for NotOwned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = &self.id;
        let you = self
            .login
            .as_deref()
            .map(|l| format!("you (@{l})"))
            .unwrap_or_else(|| "you".to_string());
        match &self.owner {
            TrackerOwner::Me => write!(f, "issue `{id}` is assigned to {you}"),
            TrackerOwner::Other(logins) => write!(
                f,
                "issue `{id}` is assigned to {} on the issue tracker, not to {you}; Shelbi only \
                 starts issues assigned to you. To take it over, run `shelbi issue start {id} \
                 --take` (assigns it to you on the tracker, then starts it)",
                at_logins(logins)
            ),
            TrackerOwner::Unassigned => write!(
                f,
                "issue `{id}` is not assigned to anyone on the issue tracker; Shelbi only starts \
                 issues assigned to you. Assign it to yourself on the tracker, or run `shelbi \
                 issue start {id} --take` to claim and start it"
            ),
            TrackerOwner::Unknown(logins) => {
                let assigned = if logins.is_empty() {
                    String::new()
                } else {
                    format!(" (assigned to {})", at_logins(logins))
                };
                write!(
                    f,
                    "refusing to start issue `{id}`{assigned}: your issue-tracker login could \
                     not be resolved, so Shelbi can't tell whether it is assigned to you. Check \
                     `gh auth status`, then retry"
                )
            }
        }
    }
}

impl std::error::Error for NotOwned {}

/// The gate for a board render or a cadence scan (the Zen scan, the daemon's
/// dispatch passes, event lines): the login comes from the daemon-owned board
/// index (`board-index.json`'s `viewer_login`) with **no** API call, so a
/// heartbeat-cadence caller adds no tracker request per tick. A remote backend
/// whose index carries no login (cold, or a token with no identity) fails
/// closed.
pub fn gate_from_index(project: &str, cfg: &IssueTrackerConfig) -> Result<OwnershipGate> {
    if !cfg.backend.is_remote() {
        return Ok(OwnershipGate::Unrestricted);
    }
    Ok(match crate::read_board_index_viewer_login(project, cfg)? {
        Some(login) if !login.is_empty() => OwnershipGate::RequireAssignee(login),
        _ => OwnershipGate::FailClosed,
    })
}

/// The gate for a one-off dispatch (`issue start`, `issue resume`, a gate
/// dispatch): the store's [`IssueStore::viewer_login`], which reads the board
/// index first and falls back to one live identity read, so a start right after
/// the hub opens (before the index carries a login) still resolves. Fails
/// closed when neither yields a login.
pub fn dispatch_gate(cfg: &IssueTrackerConfig, store: &dyn IssueStore) -> OwnershipGate {
    if !cfg.backend.is_remote() {
        return OwnershipGate::Unrestricted;
    }
    match store.viewer_login() {
        Some(login) if !login.is_empty() => OwnershipGate::RequireAssignee(login),
        _ => OwnershipGate::FailClosed,
    }
}

/// What [`ensure_owned_for_dispatch`] did to let a dispatch through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// The board has no ownership (filesystem): nothing to check.
    NotNeeded,
    /// Already assigned to the user.
    AlreadyMine,
    /// `--take`: the issue was assigned to `login` on the tracker just now.
    Took { login: String },
}

/// The one ownership check every dispatch path runs before starting `tf`:
/// `Ok` when the issue is the user's (or the board has no ownership); with
/// `take`, an issue that isn't yet is first assigned to the user on the
/// tracker. `Err` is the user-facing refusal. Only an explicit human request
/// passes `take`; automatic dispatch never does.
pub fn ensure_owned_for_dispatch(
    cfg: &IssueTrackerConfig,
    store: &dyn IssueStore,
    tf: &IssueFile,
    take: bool,
) -> std::result::Result<Claim, String> {
    let gate = dispatch_gate(cfg, store);
    match &gate {
        OwnershipGate::Unrestricted => return Ok(Claim::NotNeeded),
        _ if gate.allows(&tf.tracker_assignees) => return Ok(Claim::AlreadyMine),
        _ => {}
    }
    let id = &tf.task.id;
    if !take {
        return Err(gate
            .check(id, &tf.tracker_assignees)
            .expect_err("a refused issue fails the check")
            .to_string());
    }
    let Some(login) = gate.login().map(str::to_string) else {
        return Err(format!(
            "can't take issue `{id}`: your issue-tracker login could not be resolved (check \
             `gh auth status`), so Shelbi can't assign it to you"
        ));
    };
    let after = store
        .claim(id)
        .map_err(|e| format!("taking issue `{id}` (assigning it to @{login}): {e}"))?;
    if !gate.allows(&after) {
        return Err(format!(
            "the issue tracker did not assign `{id}` to @{login} (assignees now: {}); you may \
             lack permission to assign issues in this repository. Nothing was started",
            if after.is_empty() { "none".to_string() } else { at_logins(&after) }
        ));
    }
    Ok(Claim::Took { login })
}

/// A warning when an issue Shelbi just created on a remote tracker did not end
/// up assigned to the user (the login was unresolved, or the tracker dropped
/// the assignee), so the create still lands but the user learns why it won't
/// be dispatched. `None` when it is theirs, or the board has no ownership.
pub fn creation_warning(
    cfg: &IssueTrackerConfig,
    store: &dyn IssueStore,
    id: &str,
    tracker_assignees: &[String],
) -> Option<String> {
    let gate = dispatch_gate(cfg, store);
    if gate.allows(tracker_assignees) {
        return None;
    }
    Some(match gate.login() {
        Some(login) => format!(
            "warning: created `{id}`, but the issue tracker did not assign it to @{login} (you \
             may lack permission to assign issues in this repository). Shelbi will not start it \
             until it is assigned to you; assign it on the tracker, or run `shelbi issue start \
             {id} --take`"
        ),
        None => format!(
            "warning: created `{id}` unassigned: your issue-tracker login could not be resolved \
             (check `gh auth status`). Shelbi will not start it until it is assigned to you"
        ),
    })
}

/// The `owner=` token for task `task_id`'s event lines, read with no API call
/// from the board index (its assignees and the viewer login). `None` on a
/// filesystem board, or when the open index doesn't carry the issue (a terminal
/// card, or one created since the last tick), so the token is simply omitted.
pub fn event_owner_token(project: &str, task_id: &str) -> Option<String> {
    let cfg = crate::load_project(project).ok()?.issue_tracker;
    if !cfg.backend.is_remote() {
        return None;
    }
    let tf = crate::indexed_issue(project, &cfg, task_id)?;
    let gate = gate_from_index(project, &cfg).ok()?;
    gate.owner(&tf.tracker_assignees).map(|o| o.token())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logins(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn require_assignee_passes_only_the_users_own_issue() {
        let gate = OwnershipGate::RequireAssignee("octocat".into());
        assert!(gate.allows(&logins(&["octocat"])));
        assert!(gate.allows(&logins(&["hubot", "OctoCat"])), "case-insensitive, co-assigned");
        assert!(!gate.allows(&logins(&["someone-else"])));
        assert!(!gate.allows(&[]));
    }

    #[test]
    fn fail_closed_passes_nothing_and_unrestricted_passes_everything() {
        assert!(!OwnershipGate::FailClosed.allows(&logins(&["octocat"])));
        assert!(!OwnershipGate::FailClosed.allows(&[]));
        assert!(OwnershipGate::Unrestricted.allows(&logins(&["someone-else"])));
        assert!(OwnershipGate::Unrestricted.allows(&[]));
    }

    #[test]
    fn owner_classifies_mine_others_and_unassigned() {
        let gate = OwnershipGate::RequireAssignee("octocat".into());
        assert_eq!(gate.owner(&logins(&["octocat"])), Some(TrackerOwner::Me));
        assert_eq!(
            gate.owner(&logins(&["alice", "bob"])),
            Some(TrackerOwner::Other(logins(&["alice", "bob"])))
        );
        assert_eq!(gate.owner(&[]), Some(TrackerOwner::Unassigned));
        assert_eq!(
            OwnershipGate::FailClosed.owner(&logins(&["alice"])),
            Some(TrackerOwner::Unknown(logins(&["alice"])))
        );
        assert_eq!(OwnershipGate::FailClosed.owner(&[]), Some(TrackerOwner::Unassigned));
        // A filesystem board has no ownership to show.
        assert_eq!(OwnershipGate::Unrestricted.owner(&logins(&["alice"])), None);
    }

    #[test]
    fn owner_tokens_and_labels() {
        assert_eq!(TrackerOwner::Me.token(), "me");
        assert_eq!(TrackerOwner::Other(logins(&["alice", "bob"])).token(), "other:alice,bob");
        assert_eq!(TrackerOwner::Unassigned.token(), "unassigned");
        assert_eq!(TrackerOwner::Unknown(logins(&["alice"])).token(), "unknown");
        assert_eq!(TrackerOwner::Me.label(), "me");
        assert_eq!(TrackerOwner::Other(logins(&["alice", "bob"])).label(), "@alice, @bob");
        assert_eq!(TrackerOwner::Unassigned.label(), "unassigned");
    }

    #[test]
    fn refusals_name_the_assignees_and_the_take_override() {
        let gate = OwnershipGate::RequireAssignee("octocat".into());
        let theirs = gate.check("t", &logins(&["alice"])).unwrap_err().to_string();
        assert!(theirs.contains("assigned to @alice"), "{theirs}");
        assert!(theirs.contains("not to you (@octocat)"), "{theirs}");
        assert!(theirs.contains("shelbi issue start t --take"), "{theirs}");

        let nobody = gate.check("t", &[]).unwrap_err().to_string();
        assert!(nobody.contains("not assigned to anyone"), "{nobody}");
        assert!(nobody.contains("--take"), "{nobody}");

        let unknown = OwnershipGate::FailClosed
            .check("t", &logins(&["octocat"]))
            .unwrap_err()
            .to_string();
        assert!(unknown.contains("login could not be resolved"), "{unknown}");
        assert!(unknown.contains("assigned to @octocat"), "{unknown}");

        assert!(gate.check("t", &logins(&["octocat"])).is_ok());
    }
}
