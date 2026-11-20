//! Tracker ownership: which issues on a shared remote tracker belong to the
//! authenticated user, and so may be started by this user's orchestrator.
//!
//! Several people can share one GitHub repository's issues or one Linear team,
//! each running their own
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
//!
//! The project's [`AgentRules`] (`issue_tracker.agent_rules`) adjust the gate:
//! with `assigned_only` off an **unassigned** issue is startable too, and
//! starting it claims it as `--take` would ([`OwnershipGate::claims_on_start`]).
//! A teammate's issue stays off limits either way, and an unresolved login
//! still fails closed (there is no one to assign it to).

use std::fmt;

use serde::{Deserialize, Serialize};

use shelbi_core::{AgentRules, IssueTrackerBackend, IssueTrackerConfig, Result};

use crate::{IssueFile, IssueStore};

/// Who "the user" is for tracker ownership, and hence which issues pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

    /// Whether an issue with these assignees may be started under `rules`: the
    /// user's own, or (with `assigned_only` off) an unassigned one this gate can
    /// claim on start. What the Zen scan and every dispatch path gate on.
    pub fn startable(&self, rules: &AgentRules, tracker_assignees: &[String]) -> bool {
        self.allows(tracker_assignees) || self.claims_on_start(rules, tracker_assignees)
    }

    /// Whether starting an issue with these assignees assigns it to the user
    /// first: `assigned_only` is off, nobody is assigned, and the user's login
    /// is known. Shown on task event lines as `claim=on_start`.
    pub fn claims_on_start(&self, rules: &AgentRules, tracker_assignees: &[String]) -> bool {
        !rules.assigned_only && tracker_assignees.is_empty() && self.login().is_some()
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
            login_check: login_check(None),
            assigned_only_rule: false,
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
    /// What to check when the login didn't resolve ([`login_check`]).
    pub login_check: &'static str,
    /// The project's "Only work on issues assigned to me" rule is why an
    /// unassigned issue was refused (with it off, the start would claim it).
    pub assigned_only_rule: bool,
}

impl NotOwned {
    /// Name `backend`'s own login check in the refusal.
    pub fn for_backend(mut self, backend: IssueTrackerBackend) -> Self {
        self.login_check = login_check(Some(backend));
        self
    }
}

/// What a user runs to see whether their tracker login works: `gh auth
/// status` on GitHub, `shelbi tracker linear status` on Linear, else a
/// generic phrase.
pub fn login_check(backend: Option<IssueTrackerBackend>) -> &'static str {
    match backend {
        Some(IssueTrackerBackend::Github) => "`gh auth status`",
        Some(IssueTrackerBackend::Linear) => "`shelbi tracker linear status`",
        _ => "your issue-tracker login",
    }
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
            TrackerOwner::Unassigned => {
                write!(
                    f,
                    "issue `{id}` is not assigned to anyone on the issue tracker; Shelbi only \
                     starts issues assigned to you. Assign it to yourself on the tracker, or run \
                     `shelbi issue start {id} --take` to claim and start it"
                )?;
                if self.assigned_only_rule {
                    write!(
                        f,
                        ". This is the project's agent rule \"Only work on issues assigned to me\" \
                         (`issue_tracker.agent_rules.assigned_only`); turn it off in Settings ▸ \
                         Properties ▸ Issue tracker to let Shelbi start unassigned issues"
                    )?;
                }
                Ok(())
            }
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
                     {}, then retry",
                    self.login_check
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
/// passes `take`; automatic dispatch never does. With the project's
/// `assigned_only` rule off, an unassigned issue is claimed the same way
/// without `take` ([`OwnershipGate::claims_on_start`]); a teammate's still
/// needs it.
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
    let rules = &cfg.agent_rules;
    if !take && !gate.claims_on_start(rules, &tf.tracker_assignees) {
        let mut refusal = gate
            .check(id, &tf.tracker_assignees)
            .expect_err("a refused issue fails the check")
            .for_backend(cfg.backend);
        refusal.assigned_only_rule =
            rules.assigned_only && refusal.owner == TrackerOwner::Unassigned && gate.login().is_some();
        return Err(refusal.to_string());
    }
    let Some(login) = gate.login().map(str::to_string) else {
        return Err(format!(
            "can't take issue `{id}`: your issue-tracker login could not be resolved (check \
             {}), so Shelbi can't assign it to you",
            login_check(Some(cfg.backend))
        ));
    };
    let after = store
        .claim(id)
        .map_err(|e| format!("taking issue `{id}` (assigning it to @{login}): {e}"))?;
    if !gate.allows(&after) {
        return Err(format!(
            "the issue tracker did not assign `{id}` to @{login} (assignees now: {}); you may \
             lack permission to assign issues on this tracker. Nothing was started",
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
             may lack permission to assign issues on this tracker). Shelbi will not start it \
             until it is assigned to you; assign it on the tracker, or run `shelbi issue start \
             {id} --take`"
        ),
        None => format!(
            "warning: created `{id}` unassigned: your issue-tracker login could not be resolved \
             (check {}). Shelbi will not start it until it is assigned to you",
            login_check(Some(cfg.backend))
        ),
    })
}

/// The `owner=` token for task `task_id`'s event lines, read with no API call
/// from the board index (its assignees and the viewer login). `None` on a
/// filesystem board, or when the open index doesn't carry the issue (a terminal
/// card, or one created since the last tick), so the token is simply omitted.
///
/// An unassigned issue that starting would claim (the project's
/// `assigned_only` rule is off) gets a second token, `owner=unassigned
/// claim=on_start`, so the orchestrator knows a plain `shelbi issue start`
/// takes it. `owner=` itself never changes meaning.
pub fn event_owner_token(project: &str, task_id: &str) -> Option<String> {
    let cfg = crate::load_project(project).ok()?.issue_tracker;
    if !cfg.backend.is_remote() {
        return None;
    }
    let tf = crate::indexed_issue(project, &cfg, task_id)?;
    let gate = gate_from_index(project, &cfg).ok()?;
    let owner = gate.owner(&tf.tracker_assignees)?.token();
    Some(if gate.claims_on_start(&cfg.agent_rules, &tf.tracker_assignees) {
        format!("{owner} {CLAIM_ON_START}")
    } else {
        owner
    })
}

/// The token after `owner=unassigned` on a task event line when starting the
/// issue would claim it (the `assigned_only` rule is off).
pub const CLAIM_ON_START: &str = "claim=on_start";

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
        assert!(unknown.contains("Check your issue-tracker login, then retry"), "{unknown}");
        // Each tracker names its own login check; Linear never mentions `gh`.
        let refused = |backend| {
            OwnershipGate::FailClosed
                .check("t", &logins(&["grace"]))
                .unwrap_err()
                .for_backend(backend)
                .to_string()
        };
        assert!(refused(IssueTrackerBackend::Github).contains("Check `gh auth status`"));
        let linear = refused(IssueTrackerBackend::Linear);
        assert!(linear.contains("Check `shelbi tracker linear status`"), "{linear}");
        assert!(!linear.contains("gh "), "{linear}");

        assert!(gate.check("t", &logins(&["octocat"])).is_ok());
    }

    #[test]
    fn assigned_only_off_makes_unassigned_issues_startable_but_never_a_teammates() {
        let gate = OwnershipGate::RequireAssignee("octocat".into());
        let on = AgentRules::default();
        let off = AgentRules { assigned_only: false, ..AgentRules::default() };
        // On (the default): only the user's own.
        assert!(gate.startable(&on, &logins(&["octocat"])));
        assert!(!gate.startable(&on, &[]));
        assert!(!gate.claims_on_start(&on, &[]));
        // Off: an unassigned issue too, claimed on start; a teammate's never.
        assert!(gate.startable(&off, &[]));
        assert!(gate.claims_on_start(&off, &[]));
        assert!(!gate.startable(&off, &logins(&["alice"])));
        assert!(!gate.claims_on_start(&off, &logins(&["octocat"])), "already mine: nothing to claim");
        // An unknown login can't claim anything, so it still fails closed.
        assert!(!OwnershipGate::FailClosed.startable(&off, &[]));
        // The filesystem board is unrestricted whatever the rules.
        assert!(OwnershipGate::Unrestricted.startable(&on, &logins(&["alice"])));
        assert!(!OwnershipGate::Unrestricted.claims_on_start(&off, &[]), "no login, no claim");
    }

    #[test]
    fn an_unassigned_refusal_names_the_rule_only_when_it_is_the_reason() {
        let gate = OwnershipGate::RequireAssignee("octocat".into());
        let mut refusal = gate.check("t", &[]).unwrap_err();
        assert!(!refusal.to_string().contains("agent rule"));
        refusal.assigned_only_rule = true;
        let text = refusal.to_string();
        assert!(text.contains("\"Only work on issues assigned to me\""), "{text}");
        assert!(text.contains("Settings ▸ Properties ▸ Issue tracker"), "{text}");
        assert!(text.contains("--take"), "the override still shows: {text}");
    }
}
