//! `shelbi-desktop --bench-push` (feature `visual-check`): time the leading
//! column's push (`dt-instant-panel-push`) in a real on-screen window.
//!
//! The window is the real shell ([`ShelbiWindow`]) over a fixture sidebar,
//! fake review and workspace daemons and fake sessions, as `--visual-review`
//! uses, but drawn by the display's own frame clock. Each run opens a review
//! (the `LoadReview` host call a click or Enter dispatches), goes Back, opens
//! a workspace (`ShowView`), and goes Back again. The fake daemon answers a
//! review resolve after `--latency-ms` (the real one reads the issue tracker,
//! often over the network), and every session attaches with a `--replay-kb`
//! replay, so the attach costs what a real one does on the main thread.
//!
//! For each push it reports: the time from the input to the first frame the
//! column moves in, the frames drawn before it, and, through the push until
//! it settles, each frame's main-thread time (the window's render through
//! the end of paint) and the interval between frames. Then the same for the
//! half second after it settles (the session attaching).
//!
//! Options: `--runs <n>` (default 5), `--latency-ms <n>` (default 400),
//! `--replay-kb <n>` (default 256), `--resolve <project> <task>` (time the
//! real resolve of `task`, read-only, and use its median as the latency),
//! `--offscreen` (no display needed: a hidden window drawn on a 60Hz timer
//! of the bench's own, for a locked screen; see [`drive_ticks`]).
//!
//! ```sh
//! cargo run --release --features visual-check -- --bench-push
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedSender};
use gpui::{prelude::*, px, size, App, AsyncApp, Bounds, WindowBounds, WindowHandle, WindowKind, WindowOptions};
use shelbi_app::review::{ReviewInfo, ReviewTarget};
use shelbi_app::session::{self as app_session, ConnectFailure, ReconnectPolicy, Relauncher, RetryPolicy, SessionRef};
use shelbi_app::view::{ReviewRow, ReviewState, SidebarModel, WorkspaceBadge, WorkspaceDetailsInfo, WorkspaceRow};
use shelbi_app::nav::View;
use shelbi_app::{ReviewSessionOp, WorkspaceSessionOp};
use shelbi_client::SessionEvent;
use shelbi_term::Size;

use crate::app::{HostCall, ShelbiWindow};
use crate::elements::terminal::session::{Connected, SessionIo};
use crate::views::review::{ReviewDeps, ReviewOps};
use crate::views::session_view::{SessionConnector, SessionDeps};
use crate::views::workspace::{WorkspaceDeps, WorkspaceOps};

/// One frame: render start, paint end, where the column stood, and whether
/// the push was still running.
#[derive(Debug, Clone, Copy)]
pub struct FrameMark {
    pub start: Instant,
    pub end: Instant,
    pub position: f32,
    pub animating: bool,
}

/// Every frame the window drew.
#[derive(Default)]
pub struct Probe {
    open: Option<(Instant, f32, bool)>,
    pub frames: Vec<FrameMark>,
}

impl Probe {
    pub fn begin(&mut self, start: Instant, position: f32, animating: bool) {
        self.open = Some((start, position, animating));
    }

    pub fn end(&mut self, end: Instant) {
        if let Some((start, position, animating)) = self.open.take() {
            self.frames.push(FrameMark {
                start,
                end,
                position,
                animating,
            });
        }
    }
}

struct NullIo;
impl SessionIo for NullIo {
    fn input(&self, _bytes: Vec<u8>) {}
    fn paste(&self, _text: String, _bracketed: bool) {}
    fn resize(&self, _size: Size) {}
}

/// Every session attaches at once and replays `replay`.
struct Sessions {
    replay: Vec<u8>,
    streams: Mutex<Vec<UnboundedSender<SessionEvent>>>,
}

impl SessionConnector for Sessions {
    fn connect(&self, project: &str, target: &SessionRef, _retry: RetryPolicy) -> Result<Connected, ConnectFailure> {
        let name = app_session::discovery_name(project, target);
        let (tx, rx) = unbounded();
        tx.unbounded_send(SessionEvent::Resync {
            seq: 0,
            replay: self.replay.clone(),
        })
        .unwrap();
        self.streams.lock().unwrap().push(tx);
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

/// A daemon that answers a resolve after `latency`.
struct Ops {
    latency: Duration,
}

impl ReviewOps for Ops {
    fn resolve(&self, _project: &str, _task: &str) -> Result<ReviewTarget, String> {
        std::thread::sleep(self.latency);
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

impl WorkspaceOps for Ops {
    fn details(&self, _project: &str, _workspace: &str) -> Result<WorkspaceDetailsInfo, String> {
        std::thread::sleep(self.latency);
        Ok(WorkspaceDetailsInfo {
            task: Some(("search-index".into(), "Index the search corpus incrementally".into())),
            body: BODY.into(),
            branch: Some("jlong/search-index".into()),
            worktree: "/Users/me/src/demo/.shelbi/wt/alpha".into(),
            editor_name: "Vim".into(),
        })
    }
    fn session_op(&self, _project: &str, _workspace: &str, _op: WorkspaceSessionOp) -> Result<(), String> {
        Ok(())
    }
    fn reveal(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }
}

const BODY: &str = "## Summary\n\nWarm the application cache during startup so the first request is \
fast, reusing the same data every later request reads.\n\n## Acceptance criteria\n\n\
- [x] Cache warms on boot\n- [ ] Startup stays under 2s\n";

fn info() -> ReviewInfo {
    ReviewInfo {
        task_id: "cold-start".into(),
        slot: "review-1".into(),
        title: "Warm the application cache at startup".into(),
        body: BODY.into(),
        worktree: "/Users/me/src/demo/.shelbi/wt/review-1".into(),
        editor_name: "Vim".into(),
        review_url: Some("http://localhost:4101".into()),
        has_health_check: true,
        remote_machine: None,
    }
}

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
    m.reviews = vec![ReviewRow {
        task_id: "cold-start".into(),
        title: "Warm the application cache at startup".into(),
        branch: "jlong/cold-start".into(),
        location: Some("hub:4101".into()),
        workspace: Some("review-1".into()),
        state: ReviewState::Serving,
    }];
    m.board_loading = false;
    m
}

/// About `kb` KiB of a busy agent's scrollback: styled lines of prose and
/// code, as a session's replay carries.
fn replay(kb: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(kb * 1024);
    let mut n = 0usize;
    while out.len() < kb * 1024 {
        let line = match n % 4 {
            0 => format!("\x1b[38;5;208m⏺\x1b[0m Reading \x1b[1msrc/cache/warm_{n}.rs\x1b[0m to check the boot path ({n})\r\n"),
            1 => format!("  \x1b[32m+\x1b[0m    cache::warm(&config, Shard::new({n}))?;  \x1b[2m// keep the first request fast\x1b[0m\r\n"),
            2 => format!("  \x1b[31m-\x1b[0m    let db = Db::open(&config.db).context(\"opening the store {n}\")?;\r\n"),
            _ => format!("\x1b[36m⎿\x1b[0m  Found {n} matches across \x1b[4m{}\x1b[0m files; the warm path reads each shard once.\r\n", n % 97),
        };
        out.extend_from_slice(line.as_bytes());
        n += 1;
    }
    out
}

struct Options {
    runs: usize,
    latency: Duration,
    replay_kb: usize,
    offscreen: bool,
}

fn parse(args: &[String]) -> Options {
    let mut o = Options {
        runs: 5,
        latency: Duration::from_millis(400),
        replay_kb: 256,
        offscreen: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |what: &str| -> u64 {
            it.next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{what} needs a number"))
        };
        match a.as_str() {
            "--runs" => o.runs = num("--runs") as usize,
            "--latency-ms" => o.latency = Duration::from_millis(num("--latency-ms")),
            "--replay-kb" => o.replay_kb = num("--replay-kb") as usize,
            "--offscreen" => o.offscreen = true,
            "--resolve" => {
                let project = it.next().expect("--resolve <project> <task>").clone();
                let task = it.next().expect("--resolve <project> <task>").clone();
                o.latency = time_resolve(&project, &task);
            }
            other => panic!("unknown option `{other}`"),
        }
    }
    o
}

/// The real resolve of `task` (read-only), timed five times; the median.
fn time_resolve(project: &str, task: &str) -> Duration {
    use crate::views::review::LiveOps;
    let mut times = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        let r = LiveOps.resolve(project, task);
        times.push(t.elapsed());
        if let Err(e) = r {
            eprintln!("resolve {task}: {e}");
        }
    }
    times.sort();
    println!(
        "real resolve of {project}/{task}: {} (median {:.0} ms)",
        times.iter().map(|t| format!("{:.0}", ms(*t))).collect::<Vec<_>>().join(", "),
        ms(times[2])
    );
    times[2]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// One push's measurements.
struct Push {
    label: &'static str,
    /// Input to the end of the first frame that moved the column.
    first_move: Option<Duration>,
    /// Frames drawn after the input before the column moved.
    still_frames: usize,
    /// Main-thread time of each frame through the push.
    cpu: Vec<Duration>,
    /// Intervals between the push's frames (from the input, then frame to
    /// frame).
    intervals: Vec<Duration>,
    /// Main-thread time of each frame in the half second after it settled.
    after: Vec<Duration>,
}

fn measure(label: &'static str, input: Instant, frames: &[FrameMark], leaving: bool) -> Push {
    let after_input: Vec<&FrameMark> = frames.iter().filter(|f| f.start >= input).collect();
    let rest = if leaving { 1.0 } else { 0.0 };
    let moved = after_input.iter().position(|f| (f.position - rest).abs() > 1e-4);
    let settled = after_input
        .iter()
        .enumerate()
        .skip(moved.unwrap_or(0))
        .find(|(_, f)| !f.animating)
        .map(|(i, _)| i);
    let push_end = settled.unwrap_or(after_input.len().saturating_sub(1));
    let push = &after_input[..(push_end + 1).min(after_input.len())];
    let mut intervals = Vec::new();
    let mut last = input;
    for f in push {
        intervals.push(f.start.saturating_duration_since(last));
        last = f.start;
    }
    let settle_time = push.last().map(|f| f.end).unwrap_or(input);
    let after = after_input
        .iter()
        .skip(push_end + 1)
        .filter(|f| f.start < settle_time + Duration::from_millis(500))
        .map(|f| f.end - f.start)
        .collect();
    Push {
        label,
        first_move: moved.map(|i| after_input[i].end - input),
        still_frames: moved.unwrap_or(after_input.len()),
        cpu: push.iter().map(|f| f.end - f.start).collect(),
        intervals,
        after,
    }
}

fn pct(v: &[Duration], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort();
    ms(s[((s.len() - 1) as f64 * p).round() as usize])
}

fn report(pushes: &[Push], refresh: Duration) {
    println!(
        "\ndisplay refresh ≈ {:.1} ms (median frame interval while animating)\n",
        ms(refresh)
    );
    println!(
        "{:<16} {:>14} {:>6} {:>8} {:>8} {:>9} {:>8} {:>8} {:>10}",
        "push", "first move ms", "still", "cpu p50", "cpu max", "> refresh", "gap max", "frames", "after max"
    );
    for p in pushes {
        let over = p.cpu.iter().filter(|c| **c > refresh).count();
        let gap_max = p.intervals.iter().skip(1).max().copied().unwrap_or_default();
        println!(
            "{:<16} {:>14} {:>6} {:>8.1} {:>8.1} {:>9} {:>8.1} {:>8} {:>10.1}",
            p.label,
            p.first_move.map_or("never".to_string(), |d| format!("{:.1}", ms(d))),
            p.still_frames,
            pct(&p.cpu, 0.5),
            pct(&p.cpu, 1.0),
            over,
            ms(gap_max),
            p.cpu.len(),
            pct(&p.after, 1.0),
        );
    }
}

/// The deps the bench window runs on.
fn deps(opts: &Options) -> (ReviewDeps, WorkspaceDeps) {
    let sessions = Arc::new(Sessions {
        replay: replay(opts.replay_kb),
        streams: Mutex::new(Vec::new()),
    });
    let ops = Arc::new(Ops { latency: opts.latency });
    let session_deps = Rc::new(move || SessionDeps {
        connector: sessions.clone(),
        relauncher: sessions.clone(),
        retry: RetryPolicy::default(),
        reconnect: ReconnectPolicy::default(),
        clock: Rc::new(Instant::now),
    });
    let review = {
        let d = session_deps.clone();
        ReviewDeps {
            ops: ops.clone(),
            sessions: Rc::new(move |_task| d()),
        }
    };
    let workspace = WorkspaceDeps {
        ops,
        sessions: session_deps,
    };
    (review, workspace)
}

/// What the bench does, and when (ms after the window is up): each run opens
/// a review, goes back, opens a workspace and goes back.
#[derive(Clone)]
enum Step {
    Perform(HostCall),
    Back,
}

fn script(runs: usize) -> Vec<(u64, &'static str, Step, bool)> {
    let mut steps = Vec::new();
    let mut at = 1000;
    for _ in 0..runs {
        for (label, back, call) in [
            ("review in", "review back", HostCall::LoadReview("cold-start".into())),
            ("workspace in", "workspace back", HostCall::ShowView(View::Session("alpha".into()))),
        ] {
            steps.push((at, label, Step::Perform(call), false));
            at += 1400;
            steps.push((at, back, Step::Back, true));
            at += 700;
        }
    }
    steps
}

fn take(step: Step, w: &mut ShelbiWindow, window: &mut gpui::Window, cx: &mut gpui::Context<ShelbiWindow>) {
    match step {
        Step::Perform(call) => w.bench_perform(call, window, cx),
        Step::Back => w.bench_back(window, cx),
    }
}

fn finish(frames: &[FrameMark], pushes: Vec<(&'static str, Instant, bool)>) {
    println!("{} frame(s) drawn", frames.len());
    if frames.len() < 10 {
        println!("the window hardly drew: is the screen locked or asleep? (try --offscreen)");
    }
    let measured: Vec<Push> = pushes
        .into_iter()
        .map(|(label, input, leaving)| measure(label, input, frames, leaving))
        .collect();
    let mut intervals: Vec<Duration> = frames
        .windows(2)
        .filter(|w| w[0].animating && w[1].animating)
        .map(|w| w[1].start - w[0].start)
        .collect();
    intervals.sort();
    let refresh = intervals.get(intervals.len() / 2).copied().unwrap_or(FRAME);
    report(&measured, refresh);
}

/// A 60Hz display's frame.
const FRAME: Duration = Duration::from_micros(16_667);

/// Run it and exit.
pub fn run(args: &[String]) -> ! {
    let opts = parse(args);
    println!(
        "bench-push: {} run(s), resolve latency {:.0} ms, replay {} KiB, {}",
        opts.runs,
        ms(opts.latency),
        opts.replay_kb,
        if opts.offscreen { "hidden window drawn at 60Hz by the bench" } else { "on screen" }
    );
    let probe = Rc::new(RefCell::new(Probe::default()));
    let (review, workspace) = deps(&opts);
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            crate::assets::load_fonts(cx);
            crate::elements::terminal::TerminalSettings::init(cx);
            let p = probe.clone();
            let bounds = Bounds::centered(None, size(px(1200.0), px(760.0)), cx);
            let handle = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        // Above every window, so it is never occluded (macOS
                        // stops drawing an occluded window), without taking
                        // focus from whatever the user is doing.
                        kind: WindowKind::PopUp,
                        focus: false,
                        show: !opts.offscreen,
                        ..Default::default()
                    },
                    move |window, cx| {
                        cx.new(|cx| ShelbiWindow::bench("demo", sidebar_model(), review, workspace, p, window, cx))
                    },
                )
                .expect("open the bench window");
            let (runs, offscreen) = (opts.runs, opts.offscreen);
            let probe = probe.clone();
            cx.spawn(async move |cx| {
                let pushes = if offscreen {
                    drive_ticks(handle, runs, cx).await
                } else {
                    drive(handle, runs, cx).await
                };
                finish(&probe.borrow().frames, pushes);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    std::process::exit(0)
}

/// Run the script against the real display's frames.
async fn drive(
    handle: WindowHandle<ShelbiWindow>,
    runs: usize,
    cx: &mut AsyncApp,
) -> Vec<(&'static str, Instant, bool)> {
    let mut pushes = Vec::new();
    let started = Instant::now();
    for (at, label, step, leaving) in script(runs) {
        let due = started + Duration::from_millis(at);
        cx.background_executor()
            .timer(due.saturating_duration_since(Instant::now()))
            .await;
        let input = Instant::now();
        handle.update(cx, |w, window, cx| take(step, w, window, cx)).ok();
        pushes.push((label, input, leaving));
    }
    cx.background_executor().timer(Duration::from_millis(700)).await;
    pushes
}

/// Run the script with no display (a locked screen, CI): the window is
/// hidden, so macOS never draws it, and the bench draws it itself on a 60Hz
/// timer of its own. The timer fires on the main thread, so work that holds
/// the main thread between frames (a background result landing, a session's
/// replay fed) makes the next frame late, as it would miss a vsync.
async fn drive_ticks(
    handle: WindowHandle<ShelbiWindow>,
    runs: usize,
    cx: &mut AsyncApp,
) -> Vec<(&'static str, Instant, bool)> {
    let mut steps = script(runs).into_iter().peekable();
    let mut pushes = Vec::new();
    let started = Instant::now();
    let end = started + Duration::from_millis(steps.clone().last().map_or(0, |s| s.0) + 700);
    let mut next = started;
    while next < end {
        cx.background_executor()
            .timer(next.saturating_duration_since(Instant::now()))
            .await;
        let tick = Instant::now();
        while let Some((_, label, step, leaving)) = steps.next_if(|s| started + Duration::from_millis(s.0) <= tick) {
            let input = Instant::now();
            handle.update(cx, |w, window, cx| take(step, w, window, cx)).ok();
            pushes.push((label, input, leaving));
        }
        // Through the untyped handle: drawing renders the root view, which
        // a typed update would hold.
        cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))
            .ok();
        let done = Instant::now();
        next += FRAME;
        if next < done {
            // Overran: the next vsync it can make.
            next += FRAME * ((done - next).as_micros() / FRAME.as_micros() + 1) as u32;
        }
    }
    pushes
}
