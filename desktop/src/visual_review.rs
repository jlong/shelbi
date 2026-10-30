//! `shelbi-desktop --visual-review <dir>` (feature `visual-check`): the
//! Review view rendered offscreen (real Metal frames, as `--visual-workspace`
//! does) in the real window shell over fake sessions and a fake daemon, one
//! frame per state, plus a check that each state shows what it should:
//!
//! - `review-sidebar.png`: the sidebar before the review opens;
//! - `sidebar-switcher-mid.png` and `sidebar-switcher.png`: the project
//!   switcher's menu sliding in over the sidebar, then open with the
//!   keyboard a row down;
//! - `sidebar-slide-mid.png` and `sidebar-slide.png`: the nav selection chip
//!   part-way to Issues, then landed;
//! - `review-pending.png`: the panel the moment a review is asked for, before
//!   the load flow resolves it (the row's title, placeholders for the rest);
//! - `review-push-mid.png`: half-way through the push in (the sidebar
//!   leaving left, the panel coming in from the right, clipped to the column);
//! - `review-serving.png`: a serving review, the panel in the sidebar's
//!   column beside the reviewer;
//! - `review-diff.png`: View Diff, the daemon's diff session;
//! - `review-loading.png`: a slot still loading (waiting for its health
//!   check), no URL location yet;
//! - `review-merging.png`: an approve in flight, the panel inert;
//! - `review-back-mid.png` and `review-back.png`: Back, half-way and done
//!   (the sidebar as it was);
//! - the overlays over the shell: `palette-entering.png` (the palette
//!   sliding in), `palette.png`, `dialog-review-confirm.png`,
//!   `dialog-reject.png`, `dialog-zen.png` and
//!   `dialog-error-log.png`.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedSender};
use gpui::{px, size, AnyWindowHandle, AppContext, Entity, VisualTestAppContext, WindowHandle};
use shelbi_app::review::{ActiveView, PanelEffect, ReviewInfo, ReviewTarget};
use shelbi_app::session::{self as app_session, ConnectFailure, ReconnectPolicy, Relauncher, RetryPolicy, SessionRef};
use shelbi_app::view::{ReviewRow, ReviewState, SidebarModel, WorkspaceBadge, WorkspaceRow};
use shelbi_app::ReviewSessionOp;
use shelbi_client::SessionEvent;
use shelbi_term::Size;

use crate::app::ShelbiWindow;
use crate::elements::terminal::session::{Connected, SessionIo};
use crate::views::column_swap::SLIDE;
use crate::views::review::{ReviewDeps, ReviewOps, ReviewView};
use crate::views::session_view::{SessionConnector, SessionDeps};

struct NullIo;
impl SessionIo for NullIo {
    fn input(&self, _bytes: Vec<u8>) {}
    fn paste(&self, _text: String, _bracketed: bool) {}
    fn resize(&self, _size: Size) {}
}

/// Sessions by name, each replaying `screens[name]`.
#[derive(Default)]
struct Sessions {
    screens: Mutex<Vec<(String, Vec<u8>)>>,
    streams: Mutex<Vec<(String, UnboundedSender<SessionEvent>)>>,
}

impl SessionConnector for Sessions {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        _retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure> {
        let name = app_session::discovery_name(project, target);
        let (tx, rx) = unbounded();
        let screen = self
            .screens
            .lock()
            .unwrap()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        tx.unbounded_send(SessionEvent::Resync {
            seq: 0,
            replay: screen,
        })
        .unwrap();
        self.streams.lock().unwrap().push((name.clone(), tx));
        Ok(Connected {
            link: Box::new(NullIo),
            events: rx,
            size: Size::new(120, 40),
            name,
            cwd: None,
        })
    }
}

impl Relauncher for Sessions {
    fn relaunch(&self, _target: &SessionRef, _reopen: bool) -> Result<(), String> {
        Ok(())
    }
}

struct Ops;

impl ReviewOps for Ops {
    fn resolve(&self, _project: &str, _task: &str) -> Result<ReviewTarget, String> {
        Ok(ReviewTarget::Serving(info()))
    }
    fn session_op(&self, _project: &str, _task: &str, _op: ReviewSessionOp) -> Result<(), String> {
        Ok(())
    }
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn reveal(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }

    fn shell_op(&self, _project: &str, _slot: &str, _op: shelbi_app::WorkspaceSessionOp) -> Result<(), String> {
        Ok(())
    }
}

fn info() -> ReviewInfo {
    ReviewInfo {
        task_id: "cold-start".into(),
        slot: "review-1".into(),
        title: "Warm the application cache at startup".into(),
        body: "## Summary\n\nWarm the application cache during startup so the first request is \
               fast, reusing the same data every later request reads.\n\n## Acceptance criteria\n\n\
               - [x] Cache warms on boot\n- [ ] Startup stays under 2s\n"
            .into(),
        worktree: "/Users/me/src/demo/.shelbi/wt/review-1".into(),
        editor_name: "Vim".into(),
        apps: Default::default(),
        review_url: Some("http://localhost:4101".into()),
        has_health_check: true,
        remote_machine: None,
    }
}

fn row(state: ReviewState) -> ReviewRow {
    ReviewRow {
        task_id: "cold-start".into(),
        title: "Warm the application cache at startup".into(),
        branch: "jlong/cold-start".into(),
        location: (state == ReviewState::Serving).then(|| "hub:4101".into()),
        workspace: Some("review-1".into()),
        state,
        number: None,
    }
}

const REVIEWER_SCREEN: &str = "\x1b[1m╭──────────────────────────────────────────────╮\x1b[0m\r\n\
\x1b[1m│\x1b[0m \x1b[38;5;208m✻\x1b[0m Welcome to Claude Code                     \x1b[1m│\x1b[0m\r\n\
\x1b[1m╰──────────────────────────────────────────────╯\x1b[0m\r\n\r\n\
> Review jlong/cold-start against its acceptance criteria\r\n\r\n\
\x1b[38;5;208m⏺\x1b[0m The server is up on http://localhost:4101.\r\n\
\x1b[38;5;208m⏺\x1b[0m Both criteria hold: the cache warms on boot, startup is 1.4s.\r\n";

const DIFF_SCREEN: &str = "\x1b[1mdiff --git a/src/boot.rs b/src/boot.rs\x1b[0m\r\n\
\x1b[36m@@ -12,6 +12,7 @@\x1b[0m fn start(config: &Config) -> Result<()> {\r\n\
     let db = Db::open(&config.db)?;\r\n\
\x1b[32m+    cache::warm(&config)?;\x1b[0m\r\n\
     serve(db, config.port)\r\n";

fn sidebar_model() -> SidebarModel {
    let mut m = SidebarModel::skeleton("demo");
    m.workspaces = vec![WorkspaceRow {
        name: "alpha".into(),
        machine: "hub".into(),
        is_remote: false,
        current_task: Some("search-index".into()),
        agent: Some("developer".into()),
        badge: WorkspaceBadge::Working,
    }];
    m.reviews = vec![row(ReviewState::Serving)];
    m.board_loading = false;
    m
}

struct Run {
    cx: VisualTestAppContext,
    handle: WindowHandle<ShelbiWindow>,
    now: Rc<Cell<Instant>>,
    window: AnyWindowHandle,
    out: PathBuf,
    failures: usize,
}

/// Run it, saving frames under `out`; exits non-zero if a check failed.
pub fn run(out: PathBuf) -> ! {
    std::fs::create_dir_all(&out).expect("create the output directory");
    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.update(|cx| {
        crate::visual_check::install_theme(cx);
        crate::assets::load_fonts(cx);
        let entry = |name: &str, label: &str| crate::projects::ProjectEntry {
            name: name.into(),
            label: label.into(),
        };
        cx.set_global(crate::projects::Catalog::with(
            vec![entry("demo", "demo"), entry("shelbi", "Shelbi"), entry("contextstore", "ContextStore")],
            Vec::new(),
        ));
    });
    let sessions = Arc::new(Sessions::default());
    sessions.screens.lock().unwrap().extend([
        ("demo/ws/review-1".to_string(), REVIEWER_SCREEN.as_bytes().to_vec()),
        ("demo/review/review-1/diff".to_string(), DIFF_SCREEN.as_bytes().to_vec()),
    ]);
    let now = Rc::new(Cell::new(Instant::now()));
    let clock = now.clone();
    let handle = cx
        .open_offscreen_window(size(px(1200.0), px(700.0)), move |window, cx| {
            let deps = ReviewDeps {
                ops: Arc::new(Ops),
                sessions: Rc::new(move |_task| SessionDeps {
                    connector: sessions.clone(),
                    relauncher: sessions.clone(),
                    retry: RetryPolicy::default(),
                    reconnect: ReconnectPolicy::default(),
                    clock: Rc::new(Instant::now),
                }),
            };
            cx.new(|cx| ShelbiWindow::offscreen("demo", sidebar_model(), deps, Rc::new(move || clock.get()), window, cx))
        })
        .expect("open the offscreen window");
    let window: AnyWindowHandle = handle.into();
    let mut run = Run {
        cx,
        handle,
        now,
        window,
        out,
        failures: 0,
    };
    run.all();
    println!("\n{} check(s) failed; frames saved under {}", run.failures, run.out.display());
    std::process::exit(if run.failures == 0 { 0 } else { 1 })
}

impl Run {
    fn update<R>(&mut self, f: impl FnOnce(&mut ReviewView, &mut gpui::Context<ReviewView>) -> R) -> R {
        let view = self.view();
        let r = view.update(&mut self.cx, f);
        self.settle();
        r
    }

    /// Move the column transition's clock on.
    fn advance(&mut self, by: Duration) {
        self.now.set(self.now.get() + by);
        self.settle();
    }

    /// Let background work land and repaint.
    fn settle(&mut self) {
        for _ in 0..20 {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.cx
            .update_window(self.window, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        self.cx.run_until_parked();
    }

    /// Which of the sidebar and the review panel the column draws now.
    fn column(&mut self) -> (bool, bool) {
        let f = self.handle.update(&mut self.cx, |w, _, _| w.offscreen_column()).unwrap();
        (f.sidebar.is_some(), f.review.is_some())
    }

    fn view(&mut self) -> Entity<ReviewView> {
        self.handle
            .update(&mut self.cx, |w, _, _| w.offscreen_review())
            .unwrap()
            .expect("a review is open")
    }

    fn expect(&mut self, name: &str, ok: bool) {
        if ok {
            println!("ok    {name}");
        } else {
            self.failures += 1;
            println!("FAIL  {name}");
        }
    }

    fn save(&mut self, name: &str) {
        self.settle();
        let image = self.cx.capture_screenshot(self.window).expect("render to image");
        let path = self.out.join(name);
        image.save(&path).expect("save the frame");
        println!("saved {}", path.display());
    }

    fn all(&mut self) {
        // The sidebar, then the push in. The window shows Machines, which has
        // no row: the Orchestrator row is selected as the keys would.
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_select(0, cx)).unwrap();
        self.settle();
        let column = self.column();
        self.expect("the sidebar holds the column", column == (true, false));
        self.save("review-sidebar.png");

        // The project switcher: sliding in, then open a row down.
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_switcher(true, cx)).unwrap();
        self.advance(Duration::from_millis(50));
        self.save("sidebar-switcher-mid.png");
        self.advance(Duration::from_millis(400));
        self.handle
            .update(&mut self.cx, |w, window, cx| {
                w.offscreen_switcher_key(shelbi_state::keymap::Key::Down, window, cx)
            })
            .unwrap();
        self.settle();
        self.save("sidebar-switcher.png");
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_switcher(false, cx)).unwrap();
        self.advance(Duration::from_millis(400));

        // The nav chip sliding from Orchestrator to Issues, and back.
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_select(1, cx)).unwrap();
        self.advance(Duration::from_millis(70));
        self.save("sidebar-slide-mid.png");
        self.advance(Duration::from_millis(600));
        self.save("sidebar-slide.png");
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_select(0, cx)).unwrap();
        self.advance(Duration::from_millis(600));

        // Asked for: the panel goes in at once, before the load flow has
        // resolved it, with the row's title and placeholders.
        self.handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_pending("cold-start", window, cx))
            .unwrap();
        self.advance(SLIDE);
        self.advance(SLIDE);
        let pending = self
            .view()
            .read_with(&self.cx, |v, _| v.is_pending() && v.model().is_some_and(|m| m.title == row(ReviewState::Serving).title));
        self.expect("asked for, the panel shows the row's title and placeholders", pending);
        self.save("review-pending.png");
        self.handle
            .update(&mut self.cx, |w, window, cx| w.bench_back(window, cx))
            .unwrap();
        self.advance(SLIDE);
        self.advance(SLIDE);
        self.handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_review(info(), window, cx))
            .unwrap();
        self.advance(SLIDE / 2);
        let column = self.column();
        self.expect("mid-push both share the column", column == (true, true));
        self.save("review-push-mid.png");
        self.advance(SLIDE);

        // A serving review.
        self.update(|v, cx| v.update_row(Some(row(ReviewState::Serving)), cx));
        let column = self.column();
        self.expect("the panel stands in the sidebar's column", column == (false, true));
        let model = self.view().read_with(&self.cx, |v, _| v.model().cloned());
        self.expect(
            "a serving review's model carries its server, health check and URL",
            model.as_ref().is_some_and(|m| {
                m.server.state == ReviewState::Serving
                    && m.server.location.as_deref() == Some("hub:4101")
                    && m.review_url.is_some()
            }),
        );
        self.save("review-serving.png");

        // View Diff.
        self.update(|v, cx| v.apply(PanelEffect::ShowDiff, cx));
        let active = self.view().read_with(&self.cx, |v, _| v.panel().map(|p| p.active()));
        self.expect("View Diff shows the diff session", active == Some(ActiveView::Diff));
        self.save("review-diff.png");
        self.update(|v, cx| v.apply(PanelEffect::ShowChat, cx));

        // Still loading.
        self.update(|v, cx| v.update_row(Some(row(ReviewState::Loading)), cx));
        let state = self.view().read_with(&self.cx, |v, _| v.model().map(|m| m.server.state));
        self.expect("a loading slot reads loading", state == Some(ReviewState::Loading));
        self.save("review-loading.png");
        self.update(|v, cx| v.update_row(Some(row(ReviewState::Serving)), cx));

        // An approve in flight.
        self.update(|v, cx| v.set_merging(true, cx));
        let merging = self.view().read_with(&self.cx, |v, _| v.is_merging());
        self.expect("an approve makes the panel inert", merging);
        self.save("review-merging.png");
        self.update(|v, cx| v.set_merging(false, cx));

        // Back: the reverse push, then the sidebar as it was.
        self.update(|v, cx| v.apply(PanelEffect::Back, cx));
        self.advance(SLIDE / 2);
        self.save("review-back-mid.png");
        self.advance(SLIDE);
        let column = self.column();
        self.expect("Back gives the column back to the sidebar", column == (true, false));
        self.save("review-back.png");

        // Toggle sidebar: it slides out at its full width, clipped, the main
        // area following its edge, then back in.
        use crate::views::sidebar_reveal::REVEAL;
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_toggle_sidebar(cx)).unwrap();
        self.settle();
        self.advance(Duration::from_millis(30));
        let at = self.handle.update(&mut self.cx, |w, _, _| w.offscreen_reveal()).unwrap();
        self.expect("hiding, the sidebar is part way out", at.animating && at.position > 0.0 && at.position < 1.0);
        self.save("sidebar-hide-mid.png");
        self.advance(REVEAL);
        let at = self.handle.update(&mut self.cx, |w, _, _| w.offscreen_reveal()).unwrap();
        self.expect("hidden, the sidebar has settled", !at.animating && at.position == 0.0);
        self.save("sidebar-hidden.png");
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_toggle_sidebar(cx)).unwrap();
        self.settle();
        self.advance(Duration::from_millis(30));
        self.save("sidebar-show-mid.png");
        // Turned around mid-way: it heads back out from where it stands.
        let before = self.handle.update(&mut self.cx, |w, _, _| w.offscreen_reveal()).unwrap();
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_toggle_sidebar(cx)).unwrap();
        self.advance(Duration::from_millis(10));
        let after = self.handle.update(&mut self.cx, |w, _, _| w.offscreen_reveal()).unwrap();
        self.expect(
            "a second toggle mid-way turns it around",
            after.animating && after.position < before.position && after.position > 0.0,
        );
        self.save("sidebar-reverse-mid.png");
        self.advance(REVEAL);
        self.handle.update(&mut self.cx, |w, _, cx| w.offscreen_toggle_sidebar(cx)).unwrap();
        self.settle();
        self.advance(REVEAL);
        let at = self.handle.update(&mut self.cx, |w, _, _| w.offscreen_reveal()).unwrap();
        self.expect("shown again, the sidebar has settled", !at.animating && at.position == 1.0);
        self.save("sidebar-shown.png");

        // The overlays over the shell.
        use crate::views::overlay::ActiveOverlay;
        use shelbi_app::overlay::error_log::Viewer;
        use shelbi_app::overlay::review_confirm::Slot;
        let slot = |name: &str, occupant: Option<&str>| Slot {
            name: name.into(),
            occupant: occupant.map(str::to_string),
        };
        let mut log = ActiveOverlay::error_log("demo");
        if let ActiveOverlay::ErrorLog { viewer, .. } = &mut log {
            let entry = |ts: &str, message: &str| shelbi_state::ErrorLogEntry {
                ts: ts.into(),
                message: message.into(),
                source: None,
            };
            *viewer = Some(Viewer::new(vec![
                (entry("2026-10-15T09:12:03Z", "ssh gpu.lan: connection refused"), true),
                (entry("2026-10-15T08:40:51Z", "review-1: health check timed out after 30s"), false),
            ]));
        }
        self.handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_overlay(None, window, cx))
            .unwrap();
        self.advance(Duration::from_millis(60));
        self.save("palette-entering.png");
        self.handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_close_overlay(window, cx))
            .unwrap();
        for (name, overlay) in [
            ("palette.png", None),
            (
                "dialog-review-confirm.png",
                Some(ActiveOverlay::review_confirm(
                    "cold-start",
                    "Warm the application cache at startup",
                    vec![slot("review-1", None), slot("review-2", Some("fix-login"))],
                    2,
                )),
            ),
            ("dialog-reject.png", Some(ActiveOverlay::reject_reason("cold-start"))),
            ("dialog-zen.png", Some(ActiveOverlay::zen_intro("demo"))),
            ("dialog-error-log.png", Some(log)),
        ] {
            self.handle
                .update(&mut self.cx, |w, window, cx| w.offscreen_open_overlay(overlay, window, cx))
                .unwrap();
            self.advance(Duration::from_millis(500));
            self.save(name);
            self.handle
                .update(&mut self.cx, |w, window, cx| w.offscreen_close_overlay(window, cx))
                .unwrap();
        }
    }
}
