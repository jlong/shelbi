//! One feed per project, however many windows show it.
//!
//! A window subscribes to its project's [`ProjectFeed`] through the
//! [`FeedHub`] (a gpui global) instead of spawning its own: the first
//! subscriber starts the feed (and routes the daemon's change notes to it),
//! later ones share it and are handed its last snapshot and daemon link at
//! once, so a second window on the same project fills in on its first frame.
//! Dropping a [`FeedSubscription`] unsubscribes; the last one out stops the
//! feed. A window that switches projects drops its old subscription and takes
//! a new one, so a switch never leaves a feed (or its threads) behind.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use gpui::{App, Global, Task};

use crate::refresh::{ControlWatcher, FeedEvent, ProjectFeed, SessionSink};

/// Starts a project's feed.
pub type FeedSpawner = Rc<dyn Fn(&str) -> (ProjectFeed, UnboundedReceiver<FeedEvent>)>;

/// The app's project feeds, shared by the windows that show each project.
/// Cheap to clone; UI thread only.
#[derive(Clone)]
pub struct FeedHub {
    inner: Rc<RefCell<Hub>>,
}

impl Global for FeedHub {}

struct Hub {
    spawner: FeedSpawner,
    feeds: HashMap<String, Shared>,
    next_id: u64,
    /// Feeds started, ever (tests count them).
    #[cfg_attr(not(test), allow(dead_code))]
    spawned: usize,
}

/// One project's running feed and the windows it feeds.
struct Shared {
    feed: ProjectFeed,
    subscribers: Vec<(u64, UnboundedSender<FeedEvent>)>,
    /// Replayed to a late subscriber.
    last_snapshot: Option<FeedEvent>,
    last_link: Option<FeedEvent>,
    /// Fans the feed's events out; dropped (so cancelled) with the feed.
    _pump: Option<Task<()>>,
}

impl FeedHub {
    /// The production hub: real feeds, with `control`'s change notes routed
    /// to each.
    pub fn live(control: Option<ControlWatcher>) -> Self {
        Self::with_spawner(Rc::new(move |project: &str| {
            let (feed, events) = ProjectFeed::spawn(project);
            if let Some(control) = &control {
                control.watch(project, &feed);
            }
            (feed, events)
        }))
    }

    /// A hub over `spawner` (tests start canned feeds).
    pub fn with_spawner(spawner: FeedSpawner) -> Self {
        FeedHub {
            inner: Rc::new(RefCell::new(Hub {
                spawner,
                feeds: HashMap::new(),
                next_id: 0,
                spawned: 0,
            })),
        }
    }

    /// The app's hub, installing the live one the first time.
    pub fn global(cx: &mut App) -> FeedHub {
        if !cx.has_global::<FeedHub>() {
            let control = cx
                .try_global::<crate::app::Shell>()
                .and_then(|s| s.control.clone());
            cx.set_global(FeedHub::live(control));
        }
        cx.global::<FeedHub>().clone()
    }

    /// Subscribe to `project`'s feed, starting it if no window has yet.
    pub fn subscribe(
        &self,
        project: &str,
        cx: &mut App,
    ) -> (FeedSubscription, UnboundedReceiver<FeedEvent>) {
        let (tx, rx) = unbounded();
        let started = !self.inner.borrow().feeds.contains_key(project);
        if started {
            let spawner = self.inner.borrow().spawner.clone();
            let (feed, events) = spawner(project);
            let weak = Rc::downgrade(&self.inner);
            let name = project.to_string();
            let pump = cx.spawn(async move |_| {
                let mut events = events;
                while let Some(event) = events.next().await {
                    let Some(inner) = weak.upgrade() else {
                        break;
                    };
                    inner.borrow_mut().broadcast(&name, event);
                }
            });
            let mut hub = self.inner.borrow_mut();
            hub.spawned += 1;
            hub.feeds.insert(
                project.to_string(),
                Shared {
                    feed,
                    subscribers: Vec::new(),
                    last_snapshot: None,
                    last_link: None,
                    _pump: Some(pump),
                },
            );
        }
        let mut hub = self.inner.borrow_mut();
        let id = hub.next_id;
        hub.next_id += 1;
        let shared = hub
            .feeds
            .get_mut(project)
            .expect("the feed was just found or started");
        for event in [&shared.last_link, &shared.last_snapshot].into_iter().flatten() {
            let _ = tx.unbounded_send(event.clone());
        }
        shared.subscribers.push((id, tx));
        let sink = shared.feed.session_sink();
        (
            FeedSubscription {
                hub: Rc::downgrade(&self.inner),
                project: project.to_string(),
                id,
                sink,
            },
            rx,
        )
    }

    /// Each running feed's project and subscriber count, by project.
    #[cfg(test)]
    pub fn subscribers(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = self
            .inner
            .borrow()
            .feeds
            .iter()
            .map(|(p, s)| (p.clone(), s.subscribers.len()))
            .collect();
        out.sort();
        out
    }

    /// How many feeds this hub has started.
    #[cfg(test)]
    pub fn spawned(&self) -> usize {
        self.inner.borrow().spawned
    }
}

impl Hub {
    /// Hand `event` to every window on `project`, remembering the latest
    /// snapshot and link for the next one to subscribe.
    fn broadcast(&mut self, project: &str, event: FeedEvent) {
        let Some(shared) = self.feeds.get_mut(project) else {
            return;
        };
        match &event {
            FeedEvent::Snapshot(_) => shared.last_snapshot = Some(event.clone()),
            FeedEvent::Link(_) => shared.last_link = Some(event.clone()),
            FeedEvent::Session(_) | FeedEvent::Layout(_) => {}
        }
        shared
            .subscribers
            .retain(|(_, tx)| tx.unbounded_send(event.clone()).is_ok());
    }
}

/// A window's hold on its project's feed. Dropping it unsubscribes; the last
/// one dropped stops the feed.
pub struct FeedSubscription {
    hub: Weak<RefCell<Hub>>,
    project: String,
    id: u64,
    sink: SessionSink,
}

impl FeedSubscription {
    /// Ask the feed for a refresh now.
    pub fn request(&self) {
        if let Some(hub) = self.hub.upgrade() {
            if let Some(shared) = hub.borrow().feeds.get(&self.project) {
                shared.feed.request();
            }
        }
    }

    /// Where the window's terminal views report their sessions' events.
    pub fn session_sink(&self) -> SessionSink {
        self.sink.clone()
    }
}

impl Drop for FeedSubscription {
    fn drop(&mut self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        // Taken out under the borrow, dropped after it: stopping the feed
        // cancels its pump, which must not find the hub borrowed.
        let stopped = {
            let mut hub = hub.borrow_mut();
            let last = match hub.feeds.get_mut(&self.project) {
                Some(shared) => {
                    shared.subscribers.retain(|(id, _)| *id != self.id);
                    shared.subscribers.is_empty()
                }
                None => false,
            };
            if last {
                hub.feeds.remove(&self.project)
            } else {
                None
            }
        };
        drop(stopped);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use gpui::TestAppContext;
    use shelbi_app::view::SidebarModel;
    use shelbi_app::Snapshot;

    /// The stop flag of every feed a canned hub started.
    #[derive(Clone, Default)]
    pub(crate) struct Started(Arc<Mutex<Vec<Arc<AtomicBool>>>>);

    impl Started {
        /// Feeds started and not yet dropped (stopped).
        pub(crate) fn running(&self) -> usize {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|stop| !stop.load(Ordering::Acquire))
                .count()
        }
    }

    /// A hub of thread-less feeds that never read (so a window keeps the
    /// skeleton it painted), each one's stop flag recorded.
    pub(crate) fn canned_hub() -> (FeedHub, Started) {
        let started = Started::default();
        let record = started.clone();
        let hub = FeedHub::with_spawner(Rc::new(move |_: &str| {
            let (feed, events, stop) = ProjectFeed::inert();
            record.0.lock().unwrap().push(stop);
            (feed, events)
        }));
        (hub, started)
    }

    #[gpui::test]
    fn windows_on_one_project_share_its_feed(cx: &mut TestAppContext) {
        let (hub, started) = canned_hub();
        let (a, _ra) = cx.update(|cx| hub.subscribe("web", cx));
        let (b, _rb) = cx.update(|cx| hub.subscribe("web", cx));
        assert_eq!(hub.subscribers(), [("web".to_string(), 2)]);
        assert_eq!(hub.spawned(), 1, "the second window shares the first's feed");
        drop(a);
        assert_eq!(hub.subscribers(), [("web".to_string(), 1)]);
        assert_eq!(started.running(), 1);
        drop(b);
        assert!(hub.subscribers().is_empty(), "the last one out stops it");
        assert_eq!(started.running(), 0);
    }

    #[gpui::test]
    fn a_late_subscriber_gets_the_last_snapshot_and_link(cx: &mut TestAppContext) {
        let (hub, _started) = canned_hub();
        let (_a, _ra) = cx.update(|cx| hub.subscribe("web", cx));
        hub.inner.borrow_mut().broadcast(
            "web",
            FeedEvent::Snapshot(Box::new(Snapshot {
                generation: 7,
                sidebar: Some(SidebarModel::skeleton("Web")),
                ..Default::default()
            })),
        );
        hub.inner
            .borrow_mut()
            .broadcast("web", FeedEvent::Link(crate::refresh::DaemonLink::Live));
        let (_b, mut rb) = cx.update(|cx| hub.subscribe("web", cx));
        let first = rb.try_recv().unwrap();
        let second = rb.try_recv().unwrap();
        assert!(matches!(first, FeedEvent::Link(_)));
        assert!(matches!(second, FeedEvent::Snapshot(s) if s.generation == 7));
    }

    #[gpui::test]
    fn a_feeds_events_reach_every_window_on_it(cx: &mut TestAppContext) {
        let (hub, _started) = canned_hub();
        let (a, mut ra) = cx.update(|cx| hub.subscribe("web", cx));
        let (_b, mut rb) = cx.update(|cx| hub.subscribe("web", cx));
        let (_other, mut rother) = cx.update(|cx| hub.subscribe("api", cx));
        // What a session view reports goes through the feed to both windows.
        let sink = a.session_sink();
        sink.forward("orch", &shelbi_client::SessionEvent::Title("busy".into()));
        cx.run_until_parked();
        for rx in [&mut ra, &mut rb] {
            assert!(matches!(rx.try_recv(), Ok(FeedEvent::Session(n)) if n.session == "orch"));
        }
        assert!(rother.try_recv().is_err(), "another project's window hears nothing");
    }
}
