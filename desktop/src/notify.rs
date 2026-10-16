//! System notifications: when to post one, and when not to.
//!
//! A window's [`Tracker`] watches its project's refresh pipeline (the sidebar
//! snapshots its feed delivers, and the orchestrator session's title) and
//! turns *transitions* into [`Notice`]s:
//!
//! - a workspace's agent stops for the user: its badge enters
//!   `AwaitingInput` (it finished a turn) or `AwaitingPermission` (a dialog);
//! - a task reaches the review column;
//! - the orchestrator stops for the user: its session's `shelbi:<state>`
//!   title marker goes from working to idle (a finished turn, the same signal
//!   a workspace's `?` badge reads) or blocked (a dialog).
//!
//! The first snapshot only primes the tracker, so opening a window never
//! replays what was already waiting. [`decide`] then applies the user's
//! switches, suppresses a notice whose item is on screen in the frontmost
//! window, and holds back a repeat of the same item inside [`COOLDOWN`] (a
//! badge that flaps between working and waiting would otherwise ping on every
//! flap). Pure: no gpui, no clock (the caller passes `now`).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use shelbi_app::view::{SidebarModel, WorkspaceBadge};
use shelbi_state::{DesktopNotifications, WorkspaceState};

/// The minimum gap between two notices for the same item.
pub const COOLDOWN: Duration = Duration::from_secs(120);

/// What a notice is about; where clicking it goes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    /// A dev workspace: focus it.
    Workspace(String),
    /// A task in review: open its detail on the board.
    Review(String),
    /// The orchestrator: show the chat.
    Orchestrator,
}

/// Which preference switch governs a notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    WorkspaceBlocked,
    ReviewReady,
    OrchestratorWaiting,
}

/// One notification to post (if [`decide`] agrees).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub project: String,
    pub kind: Kind,
    pub target: Target,
    pub title: String,
    pub body: String,
}

impl Notice {
    /// The notification's tag: stable per item, so a newer notice for the
    /// same item replaces the older one, and a click names what to open.
    pub fn tag(&self) -> String {
        tag(&self.project, &self.target)
    }
}

/// `shelbi/<project>/<ws|review|orch>[/<name>]`.
pub fn tag(project: &str, target: &Target) -> String {
    match target {
        Target::Workspace(name) => format!("shelbi/{project}/ws/{name}"),
        Target::Review(id) => format!("shelbi/{project}/review/{id}"),
        Target::Orchestrator => format!("shelbi/{project}/orch"),
    }
}

/// The project and target a [`tag`] names, or `None` for a foreign tag.
pub fn parse_tag(tag: &str) -> Option<(String, Target)> {
    let rest = tag.strip_prefix("shelbi/")?;
    let mut parts = rest.splitn(3, '/');
    let project = parts.next().filter(|p| !p.is_empty())?.to_string();
    let target = match (parts.next()?, parts.next()) {
        ("ws", Some(name)) if !name.is_empty() => Target::Workspace(name.to_string()),
        ("review", Some(id)) if !id.is_empty() => Target::Review(id.to_string()),
        ("orch", None) => Target::Orchestrator,
        _ => return None,
    };
    Some((project, target))
}

/// What a tracker update produced: notices to consider, and the tags of items
/// that stopped waiting (their notification is stale and can go).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub notices: Vec<Notice>,
    pub cleared: Vec<String>,
}

/// One project's last-seen state.
#[derive(Debug, Clone)]
pub struct Tracker {
    project: String,
    /// `None` until the first loaded snapshot primes the tracker.
    workspaces: Option<HashMap<String, WorkspaceBadge>>,
    reviews: Option<HashSet<String>>,
    orchestrator: Option<WorkspaceState>,
    posted: HashMap<String, Instant>,
}

impl Tracker {
    pub fn new(project: &str) -> Self {
        Tracker {
            project: project.to_string(),
            workspaces: None,
            reviews: None,
            orchestrator: None,
            posted: HashMap::new(),
        }
    }

    /// Take a fresh sidebar snapshot.
    pub fn observe_sidebar(&mut self, model: &SidebarModel) -> Changes {
        let mut changes = Changes::default();
        // A cold skeleton (the board not read yet) says nothing about state.
        if model.board_loading {
            return changes;
        }
        let label = &model.project_label;

        let next: HashMap<String, WorkspaceBadge> = model
            .workspaces
            .iter()
            .map(|w| (w.name.clone(), w.badge))
            .collect();
        if let Some(prev) = &self.workspaces {
            for w in &model.workspaces {
                let before = prev.get(&w.name).copied();
                let target = Target::Workspace(w.name.clone());
                if entered_blocked(before, w.badge) {
                    let task = w.current_task.as_deref().unwrap_or("its task");
                    let (title, body) = if w.badge == WorkspaceBadge::AwaitingPermission {
                        (
                            format!("{} needs permission", w.name),
                            format!("{label}: a permission dialog is open on {task}."),
                        )
                    } else {
                        (
                            format!("{} is waiting for you", w.name),
                            format!("{label}: {task} finished a turn and needs input."),
                        )
                    };
                    changes.notices.push(self.notice(Kind::WorkspaceBlocked, target, title, body));
                } else if before.is_some_and(is_blocked) && !is_blocked(w.badge) {
                    changes.cleared.push(tag(&self.project, &target));
                }
            }
        }
        self.workspaces = Some(next);

        let next: HashSet<String> = model.reviews.iter().map(|r| r.task_id.clone()).collect();
        if let Some(prev) = &self.reviews {
            for r in &model.reviews {
                if !prev.contains(&r.task_id) {
                    changes.notices.push(self.notice(
                        Kind::ReviewReady,
                        Target::Review(r.task_id.clone()),
                        format!("Ready for review: {}", r.title),
                        format!("{label}: {} reached review.", r.task_id),
                    ));
                }
            }
            for gone in prev.difference(&next) {
                changes
                    .cleared
                    .push(tag(&self.project, &Target::Review(gone.clone())));
            }
        }
        self.reviews = Some(next);
        changes
    }

    /// Take the orchestrator session's latest title (its `shelbi:<state>`
    /// marker). `label` is the project's display name.
    pub fn observe_orchestrator(&mut self, title: &str, label: &str) -> Changes {
        let mut changes = Changes::default();
        let Some(next) = shelbi_state::parse_pane_title_state(title) else {
            return changes;
        };
        let before = self.orchestrator.replace(next);
        let waiting = |s: WorkspaceState| {
            matches!(s, WorkspaceState::AwaitingInput | WorkspaceState::Blocked)
        };
        match before {
            // A finished turn, or a dialog mid-turn.
            Some(WorkspaceState::Working) if waiting(next) => {
                let (title, body) = if next == WorkspaceState::Blocked {
                    (
                        "The orchestrator needs permission".to_string(),
                        format!("{label}: a permission dialog is open in the orchestrator."),
                    )
                } else {
                    (
                        "The orchestrator is waiting for you".to_string(),
                        format!("{label}: it finished a turn and may need an answer."),
                    )
                };
                changes.notices.push(self.notice(
                    Kind::OrchestratorWaiting,
                    Target::Orchestrator,
                    title,
                    body,
                ));
            }
            Some(prev) if waiting(prev) && next == WorkspaceState::Working => {
                changes.cleared.push(tag(&self.project, &Target::Orchestrator));
            }
            _ => {}
        }
        changes
    }

    /// When a notice for `tag` was last posted.
    pub fn last_posted(&self, tag: &str) -> Option<Instant> {
        self.posted.get(tag).copied()
    }

    /// Record that a notice for `tag` was posted at `now`.
    pub fn record_posted(&mut self, tag: String, now: Instant) {
        self.posted.insert(tag, now);
    }

    fn notice(&self, kind: Kind, target: Target, title: String, body: String) -> Notice {
        Notice {
            project: self.project.clone(),
            kind,
            target,
            title,
            body,
        }
    }
}

fn is_blocked(badge: WorkspaceBadge) -> bool {
    matches!(
        badge,
        WorkspaceBadge::AwaitingInput | WorkspaceBadge::AwaitingPermission
    )
}

/// Whether a workspace going `before` → `after` just stopped for the user: it
/// entered a waiting badge from a known, non-waiting one, or a dialog opened
/// while it waited. An unknown or still-loading `before` only primes.
fn entered_blocked(before: Option<WorkspaceBadge>, after: WorkspaceBadge) -> bool {
    match before {
        None | Some(WorkspaceBadge::Loading) => false,
        Some(WorkspaceBadge::AwaitingInput) => after == WorkspaceBadge::AwaitingPermission,
        Some(b) => !is_blocked(b) && is_blocked(after),
    }
}

/// What the notice's window is showing, as far as notices care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Showing {
    /// A workspace's session.
    Workspace(String),
    /// The Issues board (where review cards sit).
    Board,
    /// The orchestrator chat.
    Orchestrator,
    /// Anything else (activity, machines, settings).
    Other,
}

/// The notice's window, at the moment of the notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    /// The window is the key window of the frontmost app.
    pub frontmost: bool,
    pub showing: Showing,
}

/// The verdict on one notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Post,
    /// The user switched this kind off.
    Disabled,
    /// Its item is already on screen in the frontmost window.
    OnScreen,
    /// The same item was notified within [`COOLDOWN`].
    Cooldown,
}

/// Whether to post `notice`.
pub fn decide(
    notice: &Notice,
    prefs: &DesktopNotifications,
    screen: &Screen,
    last_posted: Option<Instant>,
    now: Instant,
) -> Decision {
    let enabled = match notice.kind {
        Kind::WorkspaceBlocked => prefs.workspace_blocked(),
        Kind::ReviewReady => prefs.review_ready(),
        Kind::OrchestratorWaiting => prefs.orchestrator_waiting(),
    };
    if !enabled {
        return Decision::Disabled;
    }
    if screen.frontmost && on_screen(&notice.target, &screen.showing) {
        return Decision::OnScreen;
    }
    if last_posted.is_some_and(|t| now.saturating_duration_since(t) < COOLDOWN) {
        return Decision::Cooldown;
    }
    Decision::Post
}

fn on_screen(target: &Target, showing: &Showing) -> bool {
    match (target, showing) {
        (Target::Workspace(a), Showing::Workspace(b)) => a == b,
        (Target::Review(_), Showing::Board) => true,
        (Target::Orchestrator, Showing::Orchestrator) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::view::{ReviewRow, ReviewState, WorkspaceRow};

    fn ws(name: &str, badge: WorkspaceBadge) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            machine: "local".into(),
            is_remote: false,
            current_task: Some("T-9".into()),
            agent: None,
            badge,
        }
    }

    fn review(id: &str) -> ReviewRow {
        ReviewRow {
            task_id: id.into(),
            title: format!("Task {id}"),
            branch: String::new(),
            location: None,
            workspace: None,
            state: ReviewState::Pending,
        }
    }

    fn model(workspaces: Vec<WorkspaceRow>, reviews: Vec<ReviewRow>) -> SidebarModel {
        SidebarModel {
            project_label: "Web".into(),
            workspaces,
            reviews,
            board_loading: false,
            ..SidebarModel::skeleton("web")
        }
    }

    fn targets(c: &Changes) -> Vec<Target> {
        c.notices.iter().map(|n| n.target.clone()).collect()
    }

    #[test]
    fn the_first_snapshot_only_primes() {
        let mut t = Tracker::new("web");
        let c = t.observe_sidebar(&model(
            vec![ws("alpha", WorkspaceBadge::AwaitingPermission)],
            vec![review("T-1")],
        ));
        assert_eq!(c, Changes::default());
    }

    #[test]
    fn a_loading_skeleton_neither_primes_nor_notifies() {
        let mut t = Tracker::new("web");
        let mut cold = model(vec![ws("alpha", WorkspaceBadge::Working)], vec![]);
        cold.board_loading = true;
        assert_eq!(t.observe_sidebar(&cold), Changes::default());
        // Still unprimed: the first loaded snapshot primes.
        let c = t.observe_sidebar(&model(vec![ws("alpha", WorkspaceBadge::AwaitingInput)], vec![]));
        assert!(c.notices.is_empty());
    }

    #[test]
    fn a_workspace_that_stops_for_the_user_is_noticed_once() {
        let mut t = Tracker::new("web");
        t.observe_sidebar(&model(vec![ws("alpha", WorkspaceBadge::Working)], vec![]));
        let c = t.observe_sidebar(&model(vec![ws("alpha", WorkspaceBadge::AwaitingInput)], vec![]));
        assert_eq!(targets(&c), [Target::Workspace("alpha".into())]);
        let n = &c.notices[0];
        assert_eq!(n.kind, Kind::WorkspaceBlocked);
        assert_eq!(n.title, "alpha is waiting for you");
        assert_eq!(n.body, "Web: T-9 finished a turn and needs input.");
        assert_eq!(n.tag(), "shelbi/web/ws/alpha");
        // Still waiting: no repeat.
        let c = t.observe_sidebar(&model(vec![ws("alpha", WorkspaceBadge::AwaitingInput)], vec![]));
        assert!(c.notices.is_empty());
        // A dialog opening while it waits is worth a second notice.
        let c =
            t.observe_sidebar(&model(vec![ws("alpha", WorkspaceBadge::AwaitingPermission)], vec![]));
        assert_eq!(c.notices[0].title, "alpha needs permission");
        // Back to work: the stale notification is cleared.
        let c = t.observe_sidebar(&model(vec![ws("alpha", WorkspaceBadge::Working)], vec![]));
        assert_eq!(c.cleared, ["shelbi/web/ws/alpha"]);
    }

    #[test]
    fn idle_paused_and_loading_workspaces_are_quiet() {
        let mut t = Tracker::new("web");
        t.observe_sidebar(&model(
            vec![ws("a", WorkspaceBadge::Loading), ws("b", WorkspaceBadge::Working)],
            vec![],
        ));
        let c = t.observe_sidebar(&model(
            vec![ws("a", WorkspaceBadge::AwaitingInput), ws("b", WorkspaceBadge::Paused)],
            vec![],
        ));
        assert!(c.notices.is_empty(), "{c:?}");
        let c = t.observe_sidebar(&model(
            vec![ws("a", WorkspaceBadge::Idle), ws("b", WorkspaceBadge::Idle)],
            vec![],
        ));
        assert!(c.notices.is_empty());
    }

    #[test]
    fn a_task_reaching_review_is_noticed() {
        let mut t = Tracker::new("web");
        t.observe_sidebar(&model(vec![], vec![review("T-1")]));
        let c = t.observe_sidebar(&model(vec![], vec![review("T-1"), review("T-2")]));
        assert_eq!(targets(&c), [Target::Review("T-2".into())]);
        assert_eq!(c.notices[0].title, "Ready for review: Task T-2");
        // It leaves review: its notification goes.
        let c = t.observe_sidebar(&model(vec![], vec![review("T-1")]));
        assert_eq!(c.cleared, ["shelbi/web/review/T-2"]);
    }

    #[test]
    fn the_orchestrator_finishing_a_turn_is_noticed() {
        let mut t = Tracker::new("web");
        // Unknown before: primes.
        assert!(t.observe_orchestrator("✳ Claude shelbi:idle", "Web").notices.is_empty());
        assert!(t.observe_orchestrator("shelbi:working", "Web").notices.is_empty());
        let c = t.observe_orchestrator("✳ Claude shelbi:idle", "Web");
        assert_eq!(targets(&c), [Target::Orchestrator]);
        assert_eq!(c.notices[0].title, "The orchestrator is waiting for you");
        // A title with no marker changes nothing.
        assert_eq!(t.observe_orchestrator("vim", "Web"), Changes::default());
        assert_eq!(t.observe_orchestrator("shelbi:working", "Web").cleared, ["shelbi/web/orch"]);
        let c = t.observe_orchestrator("shelbi:blocked", "Web");
        assert_eq!(c.notices[0].title, "The orchestrator needs permission");
    }

    fn notice(target: Target, kind: Kind) -> Notice {
        Notice {
            project: "web".into(),
            kind,
            target,
            title: String::new(),
            body: String::new(),
        }
    }

    #[test]
    fn decide_suppresses_what_is_on_screen_in_the_frontmost_window() {
        let prefs = DesktopNotifications::default();
        let now = Instant::now();
        let n = notice(Target::Workspace("alpha".into()), Kind::WorkspaceBlocked);
        let screen = |frontmost, showing| Screen { frontmost, showing };

        let on = screen(true, Showing::Workspace("alpha".into()));
        assert_eq!(decide(&n, &prefs, &on, None, now), Decision::OnScreen);
        // In the background, or showing something else: post.
        let behind = screen(false, Showing::Workspace("alpha".into()));
        assert_eq!(decide(&n, &prefs, &behind, None, now), Decision::Post);
        let other = screen(true, Showing::Workspace("beta".into()));
        assert_eq!(decide(&n, &prefs, &other, None, now), Decision::Post);

        let r = notice(Target::Review("T-1".into()), Kind::ReviewReady);
        assert_eq!(decide(&r, &prefs, &screen(true, Showing::Board), None, now), Decision::OnScreen);
        assert_eq!(decide(&r, &prefs, &screen(true, Showing::Orchestrator), None, now), Decision::Post);

        let o = notice(Target::Orchestrator, Kind::OrchestratorWaiting);
        assert_eq!(
            decide(&o, &prefs, &screen(true, Showing::Orchestrator), None, now),
            Decision::OnScreen
        );
        assert_eq!(decide(&o, &prefs, &screen(true, Showing::Other), None, now), Decision::Post);
    }

    #[test]
    fn decide_honors_the_switches_and_the_cooldown() {
        let now = Instant::now();
        let screen = Screen {
            frontmost: false,
            showing: Showing::Other,
        };
        let n = notice(Target::Review("T-1".into()), Kind::ReviewReady);
        let mut prefs = DesktopNotifications::default();
        prefs.set(true, false, true);
        assert_eq!(decide(&n, &prefs, &screen, None, now), Decision::Disabled);

        let prefs = DesktopNotifications::default();
        let just_now = Some(now - Duration::from_secs(5));
        assert_eq!(decide(&n, &prefs, &screen, just_now, now), Decision::Cooldown);
        let long_ago = Some(now - COOLDOWN - Duration::from_secs(1));
        assert_eq!(decide(&n, &prefs, &screen, long_ago, now), Decision::Post);
    }

    #[test]
    fn tags_round_trip() {
        for target in [
            Target::Workspace("alpha".into()),
            Target::Review("dt/menus-notifications".into()),
            Target::Orchestrator,
        ] {
            let t = tag("web", &target);
            assert_eq!(parse_tag(&t), Some(("web".into(), target)));
        }
        assert_eq!(parse_tag("gpui-system-notification-0"), None);
        assert_eq!(parse_tag("shelbi/web/ws"), None);
        assert_eq!(parse_tag("shelbi/web/orch/extra"), None);
    }
}
