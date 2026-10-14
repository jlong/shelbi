//! `shelbi-desktop --visual-review <dir>` (feature `visual-check`): the
//! Review view rendered offscreen (real Metal frames, as `--visual-workspace`
//! does) over fake sessions and a fake daemon, one frame per state, plus a
//! check that each state shows what it should:
//!
//! - `review-serving.png`: a serving review, the panel beside the reviewer;
//! - `review-diff.png`: View Diff, the daemon's diff session;
//! - `review-loading.png`: a slot still loading (waiting for its health
//!   check), no URL location yet;
//! - `review-merging.png`: an approve in flight, the panel inert;
//! - `review-description.png`: the task description (More).

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedSender};
use gpui::{px, size, AnyWindowHandle, AppContext, Entity, VisualTestAppContext, WindowHandle};
use shelbi_app::review::{ActiveView, PanelEffect, ReviewInfo, ReviewTarget};
use shelbi_app::session::{self as app_session, ConnectFailure, ReconnectPolicy, Relauncher, RetryPolicy, SessionRef};
use shelbi_app::theme::ThemePreference;
use shelbi_app::view::{ReviewRow, ReviewState};
use shelbi_app::ReviewSessionOp;
use shelbi_client::SessionEvent;
use shelbi_term::Size;

use crate::elements::terminal::session::{Connected, SessionIo};
use crate::theme::Theme;
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

struct Run {
    cx: VisualTestAppContext,
    handle: WindowHandle<ReviewView>,
    window: AnyWindowHandle,
    out: PathBuf,
    failures: usize,
}

/// Run it, saving frames under `out`; exits non-zero if a check failed.
pub fn run(out: PathBuf) -> ! {
    std::fs::create_dir_all(&out).expect("create the output directory");
    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.set_global(Theme::resolve(ThemePreference::Dark, gpui::WindowAppearance::Dark));
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::assets::load_fonts(cx);
    });
    let sessions = Arc::new(Sessions::default());
    sessions.screens.lock().unwrap().extend([
        ("demo/ws/review-1".to_string(), REVIEWER_SCREEN.as_bytes().to_vec()),
        ("demo/review/review-1/diff".to_string(), DIFF_SCREEN.as_bytes().to_vec()),
    ]);
    let handle = cx
        .open_offscreen_window(size(px(1200.0), px(700.0)), move |window, cx| {
            let handle = window.window_handle();
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
            cx.new(|cx| ReviewView::new("demo", handle, deps, None, cx))
        })
        .expect("open the offscreen window");
    let window: AnyWindowHandle = handle.into();
    let mut run = Run {
        cx,
        handle,
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
        let r = self.handle.update(&mut self.cx, |v, _, cx| f(v, cx)).unwrap();
        self.settle();
        r
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

    fn view(&mut self) -> Entity<ReviewView> {
        self.handle.update(&mut self.cx, |_, _, cx| cx.entity()).unwrap()
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
        // A serving review.
        self.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        let model = self.view().read_with(&self.cx, |v, _| v.model().cloned());
        self.expect(
            "a serving review shows its server, health check and URL",
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

        // The description.
        self.update(|v, cx| v.apply(PanelEffect::ShowDescription, cx));
        self.save("review-description.png");
    }
}
