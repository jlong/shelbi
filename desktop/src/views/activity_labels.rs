//! The desktop's wording and coloring of an Activity row, over the shared
//! feed row (`shelbi_app::activity::FeedRow`), after the Figma Activity
//! frame. The TUI renders the shared strings as they are; these labels are
//! the desktop's alone:
//!
//! - **Actors** are colored by kind: `zen` green, `you` in the log's gray,
//!   machine sources (a host, the daemon, a workspace's state ping) muted, a
//!   review workspace (one tagged `review`) cyan, and every other workspace
//!   in its own hue.
//! - **Tags** name where the task is now, not where that one event left it:
//!   a finished task reads `[review]` until it merges, then `[done]`. The
//!   status is the task's latest transition among the loaded events.
//!   `ready for review` reads `review`, as drawn.
//! - **A freed workspace** (its worktree released after a handoff,
//!   `worktree-detach … status=ok`) reads `available`, "Workspace is ready
//!   for another issue".
//! - **The daemon's own lines** (`daemon <action> [reason=…]`) read
//!   `system <action> Shelbi daemon`, the reason on the second line.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use shelbi_app::activity::{category_tone, Event, FeedRow, Pill, RowTarget, Secondary};
use shelbi_app::theme::Tone;
use shelbi_core::StatusCategory;

/// Who a row is about, for the actor column's color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// Zen Mode's own actions.
    Zen,
    /// The human at the board.
    You,
    /// A machine source: a host, the daemon, a workspace's state change.
    Machine,
    /// A review workspace.
    Review,
    /// A worker workspace, in its hue.
    Workspace(Tone),
}

/// One row as the desktop words it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub actor: Option<(String, Actor)>,
    pub verb: String,
    /// The verb's tone: the shared dim verb tone, or a system row's status
    /// tint (`unreachable` reads red).
    pub verb_tone: Tone,
    pub title: String,
    pub title_tone: Tone,
    pub trail: Option<(String, Tone)>,
    pub secondary: Option<Secondary>,
    /// The bracketed status tag: its text and tone.
    pub tag: Option<(String, Tone)>,
    pub time: String,
}

/// What a row's labels read beyond the row itself: each task's status now,
/// and which workspaces review.
#[derive(Debug, Clone, Default)]
pub struct Labels {
    /// Each task's latest status category among the loaded events.
    statuses: HashMap<String, StatusCategory>,
    /// The project's `review`-tagged workspaces.
    reviewers: HashSet<String>,
}

impl Labels {
    /// Take each task's status from its latest transition in `events`
    /// (oldest first).
    pub fn set_events(&mut self, events: &[Event]) {
        self.statuses.clear();
        for ev in events {
            if let Event::Issue { id, to, .. } = ev {
                self.statuses.insert(id.clone(), to.category());
            }
        }
    }

    pub fn set_reviewers(&mut self, reviewers: HashSet<String>) {
        self.reviewers = reviewers;
    }

    /// `row` in the desktop's words.
    pub fn line(&self, row: &FeedRow) -> Line {
        let mut line = Line {
            actor: row
                .chip
                .as_ref()
                .map(|c| (c.label.clone(), self.actor(&c.label, c.bold, c.tone))),
            verb: row.verb.clone(),
            verb_tone: row.verb_tone,
            title: row.title.clone(),
            title_tone: row.title_tone,
            trail: row.trail.clone(),
            secondary: row.secondary.clone(),
            tag: row.pill.as_ref().map(|pill| self.tag(pill, row.target.as_ref())),
            time: row.time.clone(),
        };
        if is_daemon(row) {
            // `daemon  event  started` / `session lost (not aqua)`.
            let (action, reason) = match row.title.split_once(" (") {
                Some((action, rest)) => (action, Some(rest.trim_end_matches(')'))),
                None => (row.title.as_str(), None),
            };
            line.actor = Some(("system".into(), Actor::Machine));
            line.verb = action.to_string();
            line.title = "Shelbi daemon".into();
            line.secondary = reason.map(|r| Secondary::Detail(r.to_string()));
        } else if is_freed_workspace(row) {
            if let Some((_, actor)) = &mut line.actor {
                *actor = Actor::Machine;
            }
            line.verb = "available".into();
            line.verb_tone = Tone::Gray;
            line.title = "Workspace is ready for another issue".into();
            line.secondary = None;
        }
        line
    }

    /// Hash what [`Self::line`] reads beyond `row` itself (its task's status
    /// now, whether its workspace reviews), so a row whose words change with
    /// them reads as changed (`dt-activity-perf`'s list keys).
    pub fn hash_context<H: Hasher>(&self, row: &FeedRow, h: &mut H) {
        if row.pill.is_some() {
            if let Some(RowTarget::Task(id)) = &row.target {
                self.statuses.get(id).hash(h);
            }
        }
        if let Some(chip) = row.chip.as_ref().filter(|c| c.bold) {
            self.reviewers.contains(&chip.label).hash(h);
        }
    }

    fn tag(&self, pill: &Pill, target: Option<&RowTarget>) -> (String, Tone) {
        let now = match target {
            Some(RowTarget::Task(id)) => self.statuses.get(id),
            _ => None,
        };
        match now {
            Some(&category) => (tag_text(category).to_string(), category_tone(category)),
            None if pill.text == "ready for review" => ("review".to_string(), pill.tone),
            None => (pill.text.clone(), pill.tone),
        }
    }

    /// The actor kind behind a chip. Agent chips are bold (a workspace, in
    /// the shared tint); the rest are machine or board labels.
    fn actor(&self, label: &str, bold: bool, tone: Tone) -> Actor {
        if bold {
            if self.reviewers.contains(label) {
                Actor::Review
            } else {
                Actor::Workspace(workspace_tone(label, tone))
            }
        } else {
            match label {
                "zen" => Actor::Zen,
                "you" => Actor::You,
                _ => Actor::Machine,
            }
        }
    }
}

/// The tag text for a status category.
pub fn tag_text(category: StatusCategory) -> &'static str {
    match category {
        StatusCategory::Backlog => "backlog",
        StatusCategory::Ready => "ready",
        StatusCategory::Active => "in progress",
        StatusCategory::Handoff => "review",
        StatusCategory::Done => "done",
        StatusCategory::Archived => "archived",
    }
}

/// A workspace's hue as the Figma frame draws it: alpha cyan, bravo blue.
/// The shared tints give bravo magenta, the `[review]` tag's color, so the
/// desktop trades bravo's and echo's; the rest keep the shared tint.
fn workspace_tone(name: &str, shared: Tone) -> Tone {
    match name {
        "bravo" => Tone::Blue,
        "echo" => Tone::Magenta,
        _ => shared,
    }
}

/// The daemon's own line: the shared feed's `daemon` chip and `event` verb.
fn is_daemon(row: &FeedRow) -> bool {
    row.verb == "event" && row.chip.as_ref().is_some_and(|c| !c.bold && c.label == "daemon")
}

/// A workspace whose worktree was released after its handoff: the shared
/// feed's `detached` row in the ok (gray) tone.
fn is_freed_workspace(row: &FeedRow) -> bool {
    row.verb == "detached" && row.verb_tone == Tone::Gray
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use shelbi_app::activity::{build_feed, parse_event_line, ActivityFilter, TaskCache};

    fn labels(events: &[Event]) -> Labels {
        let mut labels = Labels::default();
        labels.set_events(events);
        labels.set_reviewers(HashSet::from(["review".to_string()]));
        labels
    }

    fn lines(log: &[&str]) -> Vec<Line> {
        let events: Vec<Event> = log.iter().map(|l| parse_event_line(l)).collect();
        let now = Utc.with_ymd_and_hms(2026, 10, 17, 15, 0, 0).unwrap();
        let feed = build_feed(&events, &TaskCache::new(), &ActivityFilter::default(), now);
        let labels = labels(&events);
        feed.rows().map(|r| labels.line(r)).collect()
    }

    #[test]
    fn a_finished_task_reads_review_until_it_merges() {
        let finished = "2026-10-17T14:00:00Z project=p task=t1 workflow=task in-progress -> review reason=workspace:ready-marker";
        let merged = "2026-10-17T14:50:00Z project=p task=t1 workflow=task review -> done reason=orchestrator:zen-merge_pr=7";
        let before = lines(&[finished]);
        assert_eq!(before[0].tag, Some(("review".into(), Tone::Magenta)));
        let after = lines(&[finished, merged]);
        // Newest first: the merge, then the finish, both done now.
        assert_eq!(after[0].verb, "merged");
        assert_eq!(after[1].verb, "finished");
        assert_eq!(after[1].tag, Some(("done".into(), Tone::Green)));
    }

    #[test]
    fn the_tags_follow_the_frame() {
        let rows = lines(&[
            "2026-10-17T14:00:00Z project=p task=a workflow=task backlog -> todo reason=user:board",
            "2026-10-17T14:01:00Z project=p task=b workflow=task todo -> in-progress reason=orchestrator:auto-dispatch",
        ]);
        assert_eq!(rows[0].tag, Some(("in progress".into(), Tone::Yellow)));
        assert_eq!(rows[1].tag, Some(("ready".into(), Tone::Blue)));
    }

    #[test]
    fn actors_are_colored_by_kind() {
        let rows = lines(&[
            "2026-10-17T14:00:00Z project=p task=a workflow=task backlog -> todo reason=user:board",
            "2026-10-17T14:01:00Z project=p task=b workflow=task review -> done reason=orchestrator:zen-merge_pr=7",
            "2026-10-17T14:02:00Z project=p workspace=charlie working -> awaiting_input",
        ]);
        assert_eq!(rows[0].actor, Some(("charlie".into(), Actor::Machine)));
        assert_eq!(rows[1].actor, Some(("zen".into(), Actor::Zen)));
        assert_eq!(rows[2].actor, Some(("you".into(), Actor::You)));
        let labels = labels(&[]);
        assert_eq!(labels.actor("review", true, Tone::Gray), Actor::Review);
        assert_eq!(labels.actor("alpha", true, Tone::Cyan), Actor::Workspace(Tone::Cyan));
        assert_eq!(labels.actor("bravo", true, Tone::Magenta), Actor::Workspace(Tone::Blue));
    }

    #[test]
    fn a_released_worktree_reads_available() {
        let rows = lines(&[
            "2026-10-17T14:00:00Z worktree-detach task=t1 workspace=charlie detached-from=jlong/t1 status=ok",
            "2026-10-17T14:01:00Z worktree-detach task=t2 workspace=delta branch=jlong/t2 status=failed reason=worktree-detach-failed detail=busy",
        ]);
        // The failure keeps the shared wording.
        assert_eq!(rows[0].verb, "detached");
        let freed = &rows[1];
        assert_eq!(freed.actor, Some(("charlie".into(), Actor::Machine)));
        assert_eq!(freed.verb, "available");
        assert_eq!(freed.title, "Workspace is ready for another issue");
        assert_eq!(freed.secondary, None);
    }

    #[test]
    fn the_daemon_reads_as_the_system() {
        // A task line between them, so the two daemon lines don't fold.
        let rows = lines(&[
            "2026-10-17T14:00:00Z daemon started",
            "2026-10-17T14:00:30Z project=p task=a workflow=task backlog -> todo reason=user:board",
            "2026-10-17T14:01:00Z daemon session-lost reason=not-aqua",
        ]);
        assert_eq!(rows[0].actor, Some(("system".into(), Actor::Machine)));
        assert_eq!(rows[0].verb, "session lost");
        assert_eq!(rows[0].title, "Shelbi daemon");
        assert_eq!(rows[0].secondary, Some(Secondary::Detail("not aqua".into())));
        assert_eq!(rows[2].verb, "started");
        assert_eq!(rows[2].secondary, None);
    }
}
