//! In-process change-notification bus (Phase 3, `rt-daemon-poller`).
//!
//! The daemon pushes change notifications to connected clients so they need not
//! poll (`docs/removing-tmux/phase3-daemon.md`, "Pushed change notifications").
//! The producers (the board refresher and the poller) and the consumer (the
//! `subscribe` socket handler in `shelbi daemon`) live in different crates, so
//! the bus is a process-global broadcast here in `shelbi-state`, which both
//! depend on.
//!
//! It is a plain fan-out over `std::sync::mpsc`: [`subscribe_changes`] registers
//! a receiver and [`publish_change`] sends a clone to every live subscriber,
//! pruning any whose receiver has been dropped (its socket handler exited). When
//! no one is subscribed — the sidebar process, or a daemon with no attached
//! client — publishing is a cheap no-op, so producers can publish
//! unconditionally without knowing whether a client is listening.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// A layout change the daemon's poller/supervision produces as the *session*
/// half of a split operation (`rt-daemon-layout-split`;
/// `docs/removing-tmux/phase3-daemon.md`, "Layout leaves the poller"). The
/// daemon drives the session (start/stop/restart) and then emits one of these so
/// a client arranges its own view — on the tmux runtime the sidebar reacts by
/// doing today's pane/window work; the single-process TUI will do the same
/// layout in-process. It deliberately carries **no tmux details**: only what a
/// client needs to place the session. A client that was not connected when the
/// event fired is not sent a backlog — it reads current layout state on connect
/// (see [`crate`]-external `review_layout_state`) and lays itself out from that.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "layout", rename_all = "kebab-case")]
pub enum LayoutEvent {
    /// Supervision restarted a crashed orchestrator session. A client places or
    /// heals the dashboard (on tmux, re-runs `ensure_dashboard`).
    OrchestratorRestarted,
    /// A resumed/loaded review slot's panel should be built beside its agent
    /// pane (the layout half of the poller's stranded-slot resume).
    ReviewOpened { workspace: String, task: String },
    /// An accepted/out-of-band-merged review task's slot should be closed and
    /// freed in the UI (the layout half of freeing the slot).
    ReviewClosed { workspace: String, task: String },
    /// A review agent found alive but parked outside a window (a diff/editor
    /// swap collapsed its window) should be recovered into a fresh window.
    ReviewAgentRecovered { workspace: String },
}

/// A change the daemon pushes to subscribed clients so a UI can refresh the
/// affected view without polling. Serialized as one NDJSON line on the hub
/// socket; `change` tags the variant so a client can switch on it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "change", rename_all = "kebab-case")]
pub enum ChangeNotification {
    /// `project`'s board changed — a task moved, was added, or was removed.
    Board { project: String },
    /// `workspace`'s observed status changed in `project`.
    Workspace { project: String, workspace: String },
    /// A layout change in `project` the daemon produced; `event` says what a
    /// client should place. Nested rather than flattened so the layout variants
    /// evolve independently of the top-level `change` tag.
    Layout { project: String, event: LayoutEvent },
}

impl ChangeNotification {
    /// The project this change belongs to. Lets a subscriber filter the
    /// process-global bus down to the one project it cares about (a per-project
    /// UI client, or a test isolating itself from concurrent publishers).
    pub fn project(&self) -> &str {
        match self {
            ChangeNotification::Board { project } => project,
            ChangeNotification::Workspace { project, .. } => project,
            ChangeNotification::Layout { project, .. } => project,
        }
    }

    /// The layout event this notification carries, if it is a layout change.
    /// `None` for board/workspace changes, so a client that only arranges layout
    /// can ignore the rest with one match.
    pub fn layout(&self) -> Option<&LayoutEvent> {
        match self {
            ChangeNotification::Layout { event, .. } => Some(event),
            _ => None,
        }
    }

    /// Render as a single newline-terminated NDJSON line for the hub socket.
    /// Serialization of this small, fixed-shape enum cannot fail; a defensive
    /// fallback keeps the signature infallible rather than panicking a daemon
    /// handler on the impossible case.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        s
    }

    /// Parse one NDJSON line (as written by [`to_line`](Self::to_line)) back into
    /// a notification; `None` on a malformed line so a subscribing client can
    /// skip noise without erroring. Keeps the wire parse here beside `to_line`
    /// and the enum so a client crate (the sidebar) need not depend on
    /// `serde_json`. Leading/trailing whitespace (the newline) is tolerated.
    pub fn from_line(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }
}

/// Process-global registry of subscriber senders.
static BUS: OnceLock<Mutex<Vec<Sender<ChangeNotification>>>> = OnceLock::new();

fn bus() -> &'static Mutex<Vec<Sender<ChangeNotification>>> {
    BUS.get_or_init(|| Mutex::new(Vec::new()))
}

/// A live subscription to the change bus. Dropping it unregisters the receiver
/// implicitly: the next [`publish_change`] that tries to send to its sender
/// gets a `SendError` and prunes it.
pub struct ChangeSubscription {
    rx: Receiver<ChangeNotification>,
}

impl ChangeSubscription {
    /// Block up to `timeout` for the next change. `None` on timeout (so a caller
    /// can re-check a stop flag) and on a disconnected bus (which never happens
    /// while the process lives, since the bus is `'static`).
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ChangeNotification> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Non-blocking poll for a pending change, if any.
    pub fn try_recv(&self) -> Option<ChangeNotification> {
        self.rx.try_recv().ok()
    }
}

/// Register a subscriber and return its subscription. Each subscriber gets every
/// change published after it subscribes.
pub fn subscribe_changes() -> ChangeSubscription {
    let (tx, rx) = mpsc::channel();
    bus()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(tx);
    ChangeSubscription { rx }
}

/// Fan a change out to every live subscriber, pruning any whose receiver has
/// been dropped. Best-effort and lock-brief: the clones are cheap and the send
/// never blocks (an unbounded channel), so a slow consumer can't stall a
/// producer here.
pub fn publish_change(change: ChangeNotification) {
    let mut subs = bus().lock().unwrap_or_else(|p| p.into_inner());
    subs.retain(|tx| tx.send(change.clone()).is_ok());
}

/// Publish a [`LayoutEvent`] for `project`. The convenience wrapper the poller
/// and supervision use so a layout split reads as one call; a no-op when no
/// client is subscribed, exactly like [`publish_change`].
pub fn publish_layout(project: impl Into<String>, event: LayoutEvent) {
    publish_change(ChangeNotification::Layout {
        project: project.into(),
        event,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Drain `sub` until a change for `project` arrives, or `within` elapses.
    ///
    /// The bus is process-global, so a change published by a concurrent test
    /// (the board refresher, `append_workspace_event`) reaches this subscriber
    /// too. Each test publishes under a project name unique to it and filters
    /// on that name here, so the assertion is independent of whatever else is
    /// on the bus at the same time — no race on which publish lands first.
    fn recv_for(sub: &ChangeSubscription, project: &str, within: Duration) -> Option<ChangeNotification> {
        let deadline = Instant::now() + within;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match sub.recv_timeout(remaining) {
                Some(c) if c.project() == project => return Some(c),
                Some(_) => continue, // a foreign project's change — keep draining
                None => return None,
            }
        }
    }

    #[test]
    fn to_line_is_tagged_ndjson() {
        let line = ChangeNotification::Board {
            project: "p".into(),
        }
        .to_line();
        assert!(line.ends_with('\n'));
        assert_eq!(line.trim(), r#"{"change":"board","project":"p"}"#);
        let round: ChangeNotification = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(round, ChangeNotification::Board { project: "p".into() });
    }

    #[test]
    fn layout_event_round_trips_as_nested_ndjson() {
        // The layout variant nests its typed event under `event` rather than
        // flattening it, so the top-level `change` tag and the inner `layout`
        // tag never collide and each evolves independently.
        let n = ChangeNotification::Layout {
            project: "p".into(),
            event: LayoutEvent::ReviewOpened {
                workspace: "rev".into(),
                task: "t-1".into(),
            },
        };
        let line = n.to_line();
        assert!(line.ends_with('\n'));
        assert_eq!(
            line.trim(),
            r#"{"change":"layout","project":"p","event":{"layout":"review-opened","workspace":"rev","task":"t-1"}}"#
        );
        let round: ChangeNotification = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(round, n);
        assert_eq!(round.project(), "p");
        assert_eq!(
            round.layout(),
            Some(&LayoutEvent::ReviewOpened {
                workspace: "rev".into(),
                task: "t-1".into()
            })
        );
    }

    #[test]
    fn non_layout_changes_carry_no_layout_event() {
        assert_eq!(
            ChangeNotification::Board { project: "p".into() }.layout(),
            None
        );
    }

    #[test]
    fn orchestrator_restarted_serializes_without_fields() {
        // A fieldless layout variant is just its tag — the client switches on it.
        let line = ChangeNotification::Layout {
            project: "p".into(),
            event: LayoutEvent::OrchestratorRestarted,
        }
        .to_line();
        assert_eq!(
            line.trim(),
            r#"{"change":"layout","project":"p","event":{"layout":"orchestrator-restarted"}}"#
        );
    }

    #[test]
    fn publish_layout_reaches_a_subscriber() {
        let project = "change-bus-layout-publish";
        let sub = subscribe_changes();
        publish_layout(
            project,
            LayoutEvent::ReviewAgentRecovered {
                workspace: "rev".into(),
            },
        );
        let got = recv_for(&sub, project, Duration::from_secs(1))
            .expect("subscriber receives the layout change");
        assert_eq!(
            got.layout(),
            Some(&LayoutEvent::ReviewAgentRecovered {
                workspace: "rev".into()
            })
        );
    }

    #[test]
    fn a_subscriber_receives_a_published_change() {
        // Project name unique to this test so a concurrent publisher on the
        // process-global bus can't be mistaken for our change.
        let project = "change-bus-subscriber-receives";
        let sub = subscribe_changes();
        publish_change(ChangeNotification::Workspace {
            project: project.into(),
            workspace: "alpha".into(),
        });
        let got = recv_for(&sub, project, Duration::from_secs(1))
            .expect("subscriber receives the published change");
        assert_eq!(
            got,
            ChangeNotification::Workspace {
                project: project.into(),
                workspace: "alpha".into()
            }
        );
    }

    #[test]
    fn a_dropped_subscriber_is_pruned_on_next_publish() {
        let sub = subscribe_changes();
        drop(sub);
        // The publish prunes the dead sender; it must not panic and the bus
        // stays usable for a fresh subscriber.
        publish_change(ChangeNotification::Board {
            project: "change-bus-pruned-dead".into(),
        });
        let live = subscribe_changes();
        let project = "change-bus-pruned-live";
        publish_change(ChangeNotification::Board {
            project: project.into(),
        });
        assert_eq!(
            recv_for(&live, project, Duration::from_secs(1)),
            Some(ChangeNotification::Board {
                project: project.into()
            })
        );
    }
}
