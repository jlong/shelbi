//! Connect to an Issue tracker end to end (`tc-import`), for GitHub and for
//! Linear against canned trackers: the chooser, Choose project, the agent
//! rules, Review your first import, Importing, and the project window's
//! board after it ("Connected · <project>", the import toast, the ⋮ menu
//! with only Settings). The flow runs in its own window with the project
//! window as its `from`, as `show` opens it; the board's reads are handed
//! in (its workers stopped), standing in for the new tracker's first read.

use std::sync::{Arc, Mutex};

use gpui::{px, AppContext as _, Entity, TestAppContext, VisualContext as _, VisualTestContext};
use shelbi_app::new_project::Tracker;
use shelbi_app::settings::tracker::TrackerService;
use shelbi_app::source::{ProjectHandle, ProjectKey};
use shelbi_app::tracker_connect::flow::{self as copy, Step};
use shelbi_app::tracker_connect::{ConnectTracker, Connected, StatusSync, TrackerChoice};
use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig};

use super::smoke::Harness;
use super::ShelbiWindow;
use crate::views::issues::IssuesView;
use crate::views::tracker_connect::{TrackerConnectView, SIZE};

/// A tracker service over the canned GitHub, Linear and preview answers that
/// records each connection asked for and writes it.
fn service(asked: Arc<Mutex<Vec<ConnectTracker>>>) -> TrackerService {
    TrackerService {
        connect: Arc::new(move |req| {
            asked.lock().unwrap().push(req.clone());
            let linear = req.choice.backend() == IssueTrackerBackend::Linear;
            Ok(Connected {
                backend: req.choice.backend(),
                previous: IssueTrackerBackend::FileSystem,
                file: "alpha.yaml".into(),
                summary: "the tracker".into(),
                statuses: if linear {
                    StatusSync::Synced("6 statuses from the Linear team".into())
                } else {
                    StatusSync::NotNeeded
                },
            })
        }),
        linear: Arc::new(shelbi_app::linear_setup::fixture::answer),
        github: Arc::new(shelbi_app::github_setup::fixture::answer),
        sync_statuses: Arc::new(|| StatusSync::NotNeeded),
        preview: Arc::new(shelbi_app::tracker_connect::fixture::answer),
        local_issues: Arc::new(|| Ok(0)),
        move_issues: Arc::new(shelbi_app::tracker_connect::fixture::moved),
    }
}

struct Flow {
    cx: VisualTestContext,
    view: Entity<TrackerConnectView>,
}

impl Flow {
    fn drawn(&mut self, selector: &'static str) -> bool {
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        assert!(self.drawn(selector), "{selector} is drawn");
        let at = self.cx.debug_bounds(selector).unwrap().center();
        self.cx.simulate_click(at, gpui::Modifiers::none());
        self.cx.run_until_parked();
    }

    /// A card's Connect: the Connected chooser, then Choose project once
    /// its beat runs out.
    fn connect(&mut self, card: &'static str) {
        self.click(card);
        assert_eq!(self.flow(|f| f.step()), Step::Chooser, "the Connected chooser first");
        self.cx.executor().advance_clock(std::time::Duration::from_millis(copy::CONNECTED_BEAT_MS + 100));
        self.cx.run_until_parked();
        assert_eq!(self.flow(|f| f.step()), Step::ChooseProject);
    }

    fn enter(&mut self) {
        self.cx.simulate_keystrokes("enter");
        self.cx.run_until_parked();
    }

    fn flow<R>(&mut self, f: impl FnOnce(&shelbi_app::tracker_connect::flow::TrackerFlow) -> R) -> R {
        self.view.read_with(&self.cx, |v, _| f(v.flow()))
    }
}

/// The project window, its board made and its reads stopped, and the flow
/// opened from it on the local board.
fn open(cx: &mut TestAppContext, asked: Arc<Mutex<Vec<ConnectTracker>>>) -> (Harness, Entity<IssuesView>, Flow) {
    let mut h = Harness::open(cx);
    // Commands run for real (View board shows the board): the tap only
    // records them.
    h.window.update(&mut h.cx, |w, _| w.tap = None);
    let window = h.window.clone();
    let issues = h.cx.update(|w, cx| window.update(cx, |this, cx| this.ensure_issues(w, cx)));
    issues.update(&mut h.cx, |v, cx| {
        v.stop_workers();
        v.fold_read_for_test(crate::views::board_state::tests::read(Vec::new()), cx);
    });
    let from = h.cx.window_handle().downcast::<ShelbiWindow>();
    assert!(from.is_some());
    let options = gpui::WindowOptions {
        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
            gpui::Point::default(),
            gpui::size(px(SIZE.0), px(SIZE.1)),
        ))),
        ..Default::default()
    };
    let handle = ProjectHandle::null(ProjectKey::local("alpha"));
    let service = service(asked);
    let flow_window = cx.update(|cx| {
        cx.open_window(options, |window, cx| cx.new(|cx| TrackerConnectView::new(handle, service, from, window, cx)))
            .unwrap()
    });
    let mut fcx = VisualTestContext::from_window(flow_window.into(), cx);
    let view = flow_window.root(&mut fcx).unwrap();
    fcx.run_until_parked();
    view.update_in(&mut fcx, |v, window, cx| v.loaded(Ok(IssueTrackerConfig::default()), window, cx));
    fcx.run_until_parked();
    (h, issues, Flow { cx: fcx, view })
}

/// The board's first read of the new tracker: `n` open issues.
fn new_tracker_read(backend: IssueTrackerBackend, source: &str, n: usize) -> shelbi_app::board::BoardRead {
    let tasks = (0..n).map(|i| crate::views::board_state::tests::issue(&format!("T{i}"), "todo")).collect();
    let mut read = crate::views::board_state::tests::read(tasks);
    let project = read.project.as_mut().unwrap();
    project.tracker = backend;
    project.connection = Some(source.to_string());
    read
}

/// Walk the flow from Choose project's Continue to the board, for the
/// tracker `source` names, and check every step on the way.
fn rules_review_import_and_board(
    h: &mut Harness,
    issues: &Entity<IssuesView>,
    f: &mut Flow,
    asked: &Arc<Mutex<Vec<ConnectTracker>>>,
    choice: TrackerChoice,
    source: &str,
) {
    let backend = choice.backend();
    let name = copy::tracker_name(if backend == IssueTrackerBackend::Linear { Tracker::Linear } else { Tracker::Github });

    // How should agents handle issues created in <Tracker>? Both on.
    assert_eq!(f.flow(|f| f.step()), Step::AgentRules);
    assert_eq!(f.flow(|f| f.heading()), format!("How should agents handle issues created in {name}?"));
    assert_eq!(f.flow(|f| f.subheading()), format!("These rules apply only to new issues imported from {name}."));
    assert!(f.drawn("tracker-rule-review-on") && f.drawn("tracker-rule-assigned-on"), "both rules start on");
    assert!(f.flow(|f| f.assigned_title()).starts_with("Only work on issues assigned to me ("), "names the account");
    // A click on a card flips its switch; the review rule goes off.
    f.click("tracker-rule-review");
    assert!(f.drawn("tracker-rule-review-off") && f.drawn("tracker-rule-assigned-on"));
    f.enter();

    // Review your first import: the preview's counts and first issues.
    assert_eq!(f.flow(|f| f.step()), Step::ReviewImport);
    assert_eq!(
        f.flow(|f| f.subheading()),
        format!("Import from {source}. Your existing Shelbi tasks stay on this board.")
    );
    assert!(f.drawn("tracker-import-scope") && f.drawn("tracker-import-summary"));
    assert!(f.drawn("tracker-import-issue-0") && f.drawn("tracker-import-issue-2") && f.drawn("tracker-import-closed"));
    assert_eq!(f.flow(|f| f.primary_label()), Some("Import 64 issues".to_string()));
    let key = f.flow(|f| f.previewed().unwrap().first[0].key.clone());
    assert_eq!(key, if backend == IssueTrackerBackend::Linear { "ENG-128" } else { "#128" });
    f.enter();

    // Importing: the connection with the rules as set, written once.
    assert_eq!(f.flow(|f| f.step()), Step::Importing);
    assert_eq!(
        asked.lock().unwrap().as_slice(),
        [ConnectTracker::new(choice, false).with_rules(shelbi_core::AgentRules { require_review: false, assigned_only: true })]
    );
    assert!(f.drawn("tracker-progress-connection") && f.drawn("tracker-progress-import"));
    assert_eq!(f.drawn("tracker-progress-statuses"), backend == IssueTrackerBackend::Linear, "Linear maps its statuses");
    assert_eq!(f.flow(|f| f.import_line()), "Importing 64 issues…");
    assert!(f.flow(|f| f.landed().is_none()), "the bar slides until the board has them");
    assert!(f.drawn("tracker-close"), "Close leaves it running");

    // The project window's board reads the new tracker: the import lands.
    let watch = f.view.read_with(&f.cx, |v, _| v.import_watch()).expect("the board watches the import");
    assert_eq!(watch.read_with(&h.cx, |w, _| w.source().to_string()), source);
    issues.update(&mut h.cx, |v, cx| v.fold_read_for_test(new_tracker_read(backend, source, 61), cx));
    f.cx.run_until_parked();
    assert_eq!(f.flow(|f| (f.landed(), f.import_line())), (Some(61), "Imported 61 issues".to_string()));

    // View board closes the flow and shows the project window's board.
    let windows = f.cx.cx.update(|cx| cx.windows().len());
    f.click("tracker-primary");
    assert_eq!(f.cx.cx.update(|cx| cx.windows().len()), windows - 1, "the flow's window closed");
    assert_eq!(h.window.read_with(&h.cx, |w, _| w.client.view().clone()), super::View::Issues);

    // The board: Connected · <project>, the toast, ⋮ with only Settings.
    h.draw();
    assert!(h.cx.debug_bounds("board-connection").is_some(), "the header names the connection");
    assert!(h.cx.debug_bounds("board-import-toast").is_some(), "the import toast is up");
    assert_eq!(
        issues.read_with(&h.cx, |v, _| v.import_toast().map(|(t, d)| (t.to_string(), d.to_string()))),
        Some(("61 issues imported".to_string(), format!("{source} · Synced just now")))
    );
    h.click_selector("board-more");
    assert!(h.cx.debug_bounds("board-menu-settings").is_some());
    assert!(h.cx.debug_bounds("board-menu-connect-tracker").is_none(), "no Connect once connected");
    h.key("escape");

    // A click on the toast dismisses it.
    h.click_selector("board-import-toast");
    assert!(issues.read_with(&h.cx, |v, _| v.import_toast().is_none()));
}

#[gpui::test]
fn github_connects_and_imports_through_to_the_board(cx: &mut TestAppContext) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (mut h, issues, mut f) = open(cx, asked.clone());
    f.connect("tracker-connect-github");
    f.click("tracker-github-owner");
    f.click("tracker-github-owner-1");
    f.click("tracker-github-repo");
    f.click("tracker-github-repo-0");
    f.enter();
    rules_review_import_and_board(&mut h, &issues, &mut f, &asked, TrackerChoice::Github { repo: "acme/app".into() }, "acme/app");
}

#[gpui::test]
fn linear_connects_and_imports_through_to_the_board(cx: &mut TestAppContext) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (mut h, issues, mut f) = open(cx, asked.clone());
    f.connect("tracker-connect-linear");
    f.click("tracker-connect-linear-team");
    f.click("tracker-connect-linear-team-0");
    let team = f.flow(|f| f.linear().connection());
    assert_eq!(team.team, "ENG");
    f.enter();
    rules_review_import_and_board(&mut h, &issues, &mut f, &asked, TrackerChoice::Linear(team), "ENG");
}
