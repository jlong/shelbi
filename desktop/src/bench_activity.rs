//! `shelbi-desktop --bench-activity` (feature `visual-check`): time the
//! Activity feed's first paint and its frames on a large events log
//! (`dt-activity-perf`).
//!
//! Like `--bench-board` it points `SHELBI_HOME` at a fresh short `/tmp`
//! directory (unsetting the hub and control socket variables so nothing
//! reaches a live hub) and writes a fixture there: a project with `--tasks
//! <n>` task files (default 500; one in ten is left unwritten, so its id
//! never resolves, like a deleted task) and an `events.log` of `--events
//! <n>` lines (default 6,000) over the last `--days <n>` days (default 6):
//! each task's lifecycle (promoted, dispatched, rebased, building, workspace
//! pings, pushed, handed off, merged, done, its worktree freed) across five
//! workspaces, interleaved by time. The real window shell
//! ([`ShelbiWindow`]) shows the feed offscreen at the Figma frames'
//! 1440x1024, and the bench drives it synchronously: each step dispatches
//! one input through the window, then draws a frame, and the step's time is
//! the dispatch plus the whole window's render, layout and paint on the main
//! thread.
//!
//! What it reports:
//!
//! - `first rows`: from the view's creation (the sidebar's Activity click)
//!   to the first frame that paints rows, with the feed's own first-page
//!   read. Polled by hand, as fast as the bench can (the app folds a read
//!   the moment it lands), so it is the read plus the fold plus the frame.
//! - `titles`: until every task row of that first page reads its title.
//! - Then the window is grown to the whole log (as "Load older" a dozen
//!   times would) and the sweeps run, each as frame-time percentiles:
//!   `idle` (a redraw with nothing changed), `refresh` (the 750ms refresh's
//!   read arriving, the same log as before, folded, then a frame: the read
//!   itself is off the UI thread and not timed), `refresh new` (the same
//!   with a new line appended to the log first), `scroll` (a trackpad swipe
//!   down the feed and back), `hover` (the pointer down the feed), `select`
//!   (the keyboard's selection stepped down the feed).
//! - `prefetched`: a second window, open 500ms (its reader started on open,
//!   as a live window's does), then Activity shown: the first frame's time
//!   and the rows on it.
//!
//! ```sh
//! cargo run --release --features visual-check -- --bench-activity [--events 6000] [--tasks 500] [--days 6] [--steps 120] [--opacity 85 --blur 20]
//! ```

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gpui::{
    point, px, size, AnyWindowHandle, AppContext, Modifiers, MouseMoveEvent, Pixels, PlatformInput, Point,
    ScrollDelta, ScrollWheelEvent, TouchPhase, VisualTestAppContext, WindowHandle,
};
use shelbi_app::activity::RowTarget;
use shelbi_app::nav::View;
use shelbi_app::view::{SidebarModel, SidebarTarget};
use shelbi_core::{Column, Issue};

use crate::app::{HostCall, ShelbiWindow};
use crate::views::activity::ActivityView;

const PROJECT: &str = "bench";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 1024.0;
const WORKSPACES: [&str; 5] = ["alpha", "bravo", "charlie", "delta", "echo"];

struct Options {
    events: usize,
    tasks: usize,
    days: i64,
    steps: usize,
    /// The window's translucency (`--opacity <percent>`, `--blur <points>`),
    /// opaque by default.
    look: crate::theme::window::Translucency,
}

fn parse(args: &[String]) -> Options {
    let mut o = Options {
        events: 6000,
        tasks: 500,
        days: 6,
        steps: 120,
        look: Default::default(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |what: &str| -> usize {
            it.next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{what} needs a number"))
        };
        match a.as_str() {
            "--events" => o.events = num("--events"),
            "--opacity" => o.look = crate::theme::window::Translucency::clamped(num("--opacity") as i32, o.look.blur.into()),
            "--blur" => o.look = crate::theme::window::Translucency::clamped(o.look.opacity.into(), num("--blur") as i32),
            "--tasks" => o.tasks = num("--tasks"),
            "--days" => o.days = num("--days") as i64,
            "--steps" => o.steps = num("--steps"),
            other => panic!("unknown option `{other}`"),
        }
    }
    o
}

// ---- the fixture ------------------------------------------------------------

/// A small deterministic generator, so every run writes the same log.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

const WORDS: [&str; 32] = [
    "board", "column", "session", "terminal", "review", "workspace", "agent", "daemon", "sidebar",
    "palette", "render", "scroll", "cache", "index", "branch", "merge", "remote", "machine",
    "settings", "theme", "keyboard", "filter", "history", "detail", "status", "workflow", "queue",
    "frame", "layout", "pointer", "selection", "activity",
];

fn title(rng: &mut Lcg) -> String {
    let n = 3 + rng.below(10) as usize;
    let words: Vec<&str> = (0..n).map(|_| WORDS[rng.below(WORDS.len() as u64) as usize]).collect();
    let mut s = words.join(" ");
    s[..1].make_ascii_uppercase();
    s
}

fn task_id(i: usize) -> String {
    format!("task-{i}")
}

/// One task's lifecycle as event lines (minus the timestamps): each with its
/// offset in seconds from the task's start.
fn lifecycle(rng: &mut Lcg, id: &str, ws: &str) -> Vec<(i64, String)> {
    let p = format!("project={PROJECT}");
    let branch = format!("jlong/{id}");
    let mut at = 0i64;
    let mut step = |rng: &mut Lcg, lo: u64, hi: u64| {
        at += (lo + rng.below(hi - lo)) as i64;
        at
    };
    let mut out = vec![
        (step(rng, 1, 60), format!("{p} task={id} workflow=app backlog -> todo reason=user:board from_category=backlog to_category=ready")),
        (step(rng, 30, 600), format!("dispatch {p} task={id} workspace={ws} status=confirmed detail=seed_busy_observed")),
        (step(rng, 1, 5), format!("{p} task={id} workflow=app todo -> in_progress reason=orchestrator:auto-dispatch_workspace={ws}_agent=developer from_category=ready to_category=active")),
        (step(rng, 1, 30), format!("rebase task={id} workspace={ws} branch={branch} base=main status=up-to-date detail=base_sha=ddaa22b")),
        (step(rng, 60, 900), format!("{p} workspace={ws} working -> awaiting_input")),
        (step(rng, 30, 300), format!("{p} workspace={ws} awaiting_input -> working")),
        (step(rng, 300, 2400), format!("push task={id} workspace={ws} branch={branch} status=pushed detail=ff_(new)..f5cd0ea")),
        (step(rng, 1, 5), format!("{p} task={id} workflow=app in_progress -> review reason=workspace:ready-marker from_category=active to_category=handoff")),
    ];
    // Most land; some sit in review.
    if rng.below(10) < 8 {
        out.push((step(rng, 300, 3600), format!("merge task={id} workspace={ws} base=main status=ok detail=pr:597:09638f6a")));
        out.push((step(rng, 1, 5), format!("{p} task={id} workflow=app review -> done reason=user:accept_merge from_category=handoff to_category=done")));
        out.push((step(rng, 1, 5), format!("worktree-detach task={id} workspace={ws} detached-from={branch} status=ok")));
    }
    out
}

fn write_fixtures(home: &Path, opts: &Options, now: DateTime<Utc>) {
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    let workspaces: String = WORKSPACES
        .iter()
        .map(|w| format!("  - name: {w}\n    machine: hub\n"))
        .collect();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: Bench
repo: ''
default_branch: main
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
agents:
  developer:
    runner: claude
machines:
  - name: hub
    kind: local
    work_dir: {work}
workspaces:
{workspaces}",
            work = home.join("work").display()
        ),
    )
    .unwrap();
    let mut rng = Lcg(0xac71);
    // Task files, but one in ten is missing (its id never resolves).
    for i in 0..opts.tasks {
        if i % 10 == 9 {
            continue;
        }
        let task = Issue {
            id: task_id(i),
            title: title(&mut rng),
            column: Column::done(),
            priority: 1,
            assigned_to: None,
            workflow: Some("app".into()),
            branch: Some(format!("jlong/{}", task_id(i))),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: Default::default(),
        };
        shelbi_state::save_task(PROJECT, &task, "A fixture task.\n").expect("save a fixture task");
    }
    // Each task's lifecycle from a start spread over the span, then all of
    // them by time, the newest `--events` kept.
    let span = opts.days * 86_400;
    let mut lines: Vec<(i64, String)> = Vec::new();
    let mut i = 0;
    while lines.len() < opts.events + opts.events / 10 {
        let id = task_id(i % opts.tasks.max(1));
        let ws = WORKSPACES[i % WORKSPACES.len()];
        let start = rng.below(span as u64) as i64;
        for (offset, line) in lifecycle(&mut rng, &id, ws) {
            if start + offset < span {
                lines.push((start + offset, line));
            }
        }
        i += 1;
    }
    lines.sort_by_key(|(at, _)| *at);
    let keep = lines.len().saturating_sub(opts.events);
    let mut log = std::io::BufWriter::new(std::fs::File::create(home.join("events.log")).unwrap());
    let origin = now - chrono::Duration::seconds(span);
    for (at, line) in &lines[keep..] {
        let ts = origin + chrono::Duration::seconds(*at);
        writeln!(log, "{} {line}", ts.to_rfc3339()).unwrap();
    }
}

// ---- measuring ---------------------------------------------------------------

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    ms(sorted[((sorted.len() - 1) as f64 * p).round() as usize])
}

struct Sweep {
    label: &'static str,
    frames: Vec<Duration>,
}

fn report(sweeps: &[Sweep]) {
    println!(
        "\n{:<14} {:>7} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9}",
        "sweep", "frames", "p50 ms", "p90 ms", "p99 ms", "max ms", "> 8.3 ms", "> 16.7 ms"
    );
    for s in sweeps {
        let mut f = s.frames.clone();
        f.sort();
        let over = |limit: f64| f.iter().filter(|d| ms(**d) > limit).count();
        println!(
            "{:<14} {:>7} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>9} {:>9}",
            s.label,
            f.len(),
            pct(&f, 0.5),
            pct(&f, 0.9),
            pct(&f, 0.99),
            pct(&f, 1.0),
            over(8.33),
            over(16.67),
        );
    }
}

struct Bench {
    cx: VisualTestAppContext,
    handle: WindowHandle<ShelbiWindow>,
    feed: gpui::Entity<ActivityView>,
}

impl Bench {
    fn window(&self) -> AnyWindowHandle {
        self.handle.into()
    }

    fn draw(&mut self) {
        self.cx
            .update_window(self.window(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    }

    /// One step: `input` dispatched through the window, then a frame.
    fn step(&mut self, input: Option<PlatformInput>) -> Duration {
        let start = Instant::now();
        if let Some(input) = input {
            self.cx
                .update_window(self.window(), |_, window, cx| {
                    window.dispatch_event(input, cx);
                })
                .unwrap();
        }
        self.draw();
        start.elapsed()
    }

    fn mouse_to(&mut self, at: Point<Pixels>) -> Duration {
        self.step(Some(PlatformInput::MouseMove(MouseMoveEvent {
            position: at,
            pressed_button: None,
            modifiers: Modifiers::default(),
        })))
    }

    fn swipe(&mut self, at: Point<Pixels>, dy: f32, phase: TouchPhase) -> Duration {
        self.step(Some(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
        })))
    }

    /// A whole trackpad gesture of `steps` deltas, each step's frame timed.
    fn gesture(&mut self, at: Point<Pixels>, dy: f32, steps: usize, out: &mut Vec<Duration>) {
        self.mouse_to(at);
        for i in 0..steps {
            let phase = if i == 0 { TouchPhase::Started } else { TouchPhase::Moved };
            out.push(self.swipe(at, dy, phase));
        }
        out.push(self.swipe(at, 0.0, TouchPhase::Ended));
    }

    /// Drain the feed's refresher (the poll timer doesn't run offscreen).
    fn poll(&mut self) {
        self.cx.run_until_parked();
        self.feed.update(&mut self.cx, |v, cx| v.poll(cx));
    }

    fn rows(&self) -> usize {
        self.feed.read_with(&self.cx, |v, _| v.state().feed().rows().count())
    }

    /// Task rows still showing their id for a title.
    fn untitled(&self) -> usize {
        self.feed.read_with(&self.cx, |v, _| {
            v.state()
                .feed()
                .rows()
                .filter(|r| matches!(&r.target, Some(RowTarget::Task(id)) if *id == r.title))
                .count()
        })
    }

    fn loaded_events(&self) -> usize {
        self.feed.read_with(&self.cx, |v, _| v.state().loaded_events())
    }
}

/// Run it and exit.
pub fn run(args: &[String]) -> ! {
    let opts = parse(args);
    let home = PathBuf::from(format!("/tmp/shabench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the sandbox home");
    // SAFETY: single-threaded here, before gpui or any reader thread starts.
    unsafe {
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_CONTROL_SOCK");
        std::env::remove_var("SHELBI_HUB_ADDR");
        std::env::remove_var("SHELBI_PROJECT");
    }
    let t = Instant::now();
    write_fixtures(&home, &opts, Utc::now());
    println!(
        "bench-activity: {} events over {} days, {} tasks, written in {:.0} ms",
        opts.events,
        opts.days,
        opts.tasks,
        ms(t.elapsed())
    );

    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.update(|cx| {
        crate::visual_check::install_theme(cx);
        crate::theme::window::set(opts.look, cx);
        crate::assets::load_fonts(cx);
        crate::elements::terminal::TerminalSettings::init(cx);
    });
    let (review, workspace) = crate::bench_push::fake_deps(Duration::ZERO, 1);
    let mut model = SidebarModel::skeleton(PROJECT);
    model.board_loading = false;
    let handle = cx
        .open_offscreen_window(size(px(WIDTH), px(HEIGHT)), move |window, cx| {
            cx.new(|cx| {
                let mut w = ShelbiWindow::offscreen(PROJECT, model, review, Rc::new(Instant::now), window, cx);
                w.bench_workspace_deps(workspace);
                w
            })
        })
        .expect("open the offscreen window");
    let window: AnyWindowHandle = handle.into();
    cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx)).unwrap();

    // The sidebar's Activity click: the view is made here, its first read
    // starts, and the bench polls until a frame paints rows.
    let open = Instant::now();
    handle
        .update(&mut cx, |w, _, cx| {
            w.offscreen_select_target(&SidebarTarget::View(View::Activity), cx)
        })
        .unwrap();
    handle
        .update(&mut cx, |w, window, cx| w.bench_perform(HostCall::ShowView(View::Activity), window, cx))
        .unwrap();
    let feed = handle
        .update(&mut cx, |w, _, _| w.offscreen_views().1)
        .unwrap()
        .expect("the Activity view");
    let mut b = Bench { cx, handle, feed };
    let mut frames = 0;
    let first_rows = loop {
        b.poll();
        b.draw();
        frames += 1;
        if b.rows() > 0 {
            break open.elapsed();
        }
        if open.elapsed() > Duration::from_secs(60) {
            panic!("the feed never loaded");
        }
    };
    let first_count = b.rows();
    // Titles fill in as the refresh resolves them, a batch a pass; drive
    // the refresh back to back rather than on its 750ms cadence.
    let titles = loop {
        if b.untitled() <= first_count / 10 + 1 {
            break open.elapsed();
        }
        if open.elapsed() > Duration::from_secs(120) {
            panic!("the titles never resolved ({} left)", b.untitled());
        }
        b.feed.update(&mut b.cx, |v, cx| v.bench_refresh(cx));
        b.draw();
    };
    println!(
        "first rows {:.1} ms ({} rows, {} frames polled); titles {:.0} ms (refreshing back to back)",
        ms(first_rows),
        first_count,
        frames,
        ms(titles)
    );

    // The whole log, as "Load older" over and over would load it.
    let load = Instant::now();
    b.feed.update(&mut b.cx, |v, cx| v.bench_window(opts.events + 1000, cx));
    while b.loaded_events() < opts.events {
        b.poll();
        if load.elapsed() > Duration::from_secs(60) {
            panic!("the whole log never loaded ({} events)", b.loaded_events());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Its titles fill in a batch a read; let them settle before timing.
    let mut quiet = Instant::now();
    let mut untitled = b.untitled();
    while quiet.elapsed() < Duration::from_millis(1500) {
        b.poll();
        let now = b.untitled();
        if now != untitled {
            untitled = now;
            quiet = Instant::now();
        }
        if load.elapsed() > Duration::from_secs(120) {
            panic!("the whole log's titles never settled ({untitled} left)");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let first = b.step(None);
    b.step(None);
    println!(
        "whole log: {} events, {} rows ({} untitled), loaded and settled in {:.0} ms; first frame {:.1} ms",
        b.loaded_events(),
        b.rows(),
        b.untitled(),
        ms(load.elapsed()),
        ms(first)
    );

    let steps = opts.steps;
    let mut sweeps = Vec::new();

    let frames = (0..steps).map(|_| b.step(None)).collect();
    sweeps.push(Sweep { label: "idle", frames });

    // The 750ms refresh, reading the log as it was.
    let mut frames = Vec::new();
    for _ in 0..steps.min(30) {
        let fold = b.feed.update(&mut b.cx, |v, cx| v.bench_refresh(cx));
        let start = Instant::now();
        b.draw();
        frames.push(fold + start.elapsed());
    }
    sweeps.push(Sweep { label: "refresh", frames });

    // A new event each refresh.
    let mut frames = Vec::new();
    let log_path = home.join("events.log");
    for i in 0..steps.min(30) {
        let mut log = std::fs::OpenOptions::new().append(true).open(&log_path).unwrap();
        let ws = WORKSPACES[i % WORKSPACES.len()];
        writeln!(log, "{} project={PROJECT} workspace={ws} working -> awaiting_input", Utc::now().to_rfc3339())
            .unwrap();
        drop(log);
        let fold = b.feed.update(&mut b.cx, |v, cx| v.bench_refresh(cx));
        let start = Instant::now();
        b.draw();
        frames.push(fold + start.elapsed());
    }
    sweeps.push(Sweep { label: "refresh new", frames });

    // Down the feed, then back up.
    let over_feed = point(px(860.0), px(560.0));
    let mut frames = Vec::new();
    b.gesture(over_feed, -60.0, steps, &mut frames);
    b.gesture(over_feed, 60.0, steps, &mut frames);
    sweeps.push(Sweep { label: "scroll", frames });

    // The pointer down the feed's rows.
    let mut frames = Vec::new();
    for i in 0..steps * 2 {
        let y = 140.0 + (i as f32 * 7.0) % (HEIGHT - 200.0);
        frames.push(b.mouse_to(point(px(860.0), px(y))));
    }
    sweeps.push(Sweep { label: "hover", frames });

    // The keyboard's selection down the feed.
    let mut frames = Vec::new();
    for _ in 0..steps {
        let start = Instant::now();
        b.cx.dispatch_keystroke(b.window(), gpui::Keystroke::parse("j").unwrap());
        b.draw();
        frames.push(start.elapsed());
    }
    sweeps.push(Sweep { label: "select", frames });

    report(&sweeps);

    // A live window starts the feed's reader when it opens: Activity shown a
    // moment later paints its rows on its first frame.
    let model = SidebarModel::skeleton(PROJECT);
    let (review, workspace) = crate::bench_push::fake_deps(Duration::ZERO, 1);
    let second = b
        .cx
        .open_offscreen_window(size(px(WIDTH), px(HEIGHT)), move |window, cx| {
            cx.new(|cx| {
                let mut w = ShelbiWindow::offscreen(PROJECT, model, review, Rc::new(Instant::now), window, cx);
                w.bench_workspace_deps(workspace);
                w.bench_prefetch_activity();
                w
            })
        })
        .expect("open the second window");
    std::thread::sleep(Duration::from_millis(500));
    let open = Instant::now();
    second
        .update(&mut b.cx, |w, _, cx| w.offscreen_select_target(&SidebarTarget::View(View::Activity), cx))
        .unwrap();
    second
        .update(&mut b.cx, |w, window, cx| w.bench_perform(HostCall::ShowView(View::Activity), window, cx))
        .unwrap();
    let feed = second
        .update(&mut b.cx, |w, _, _| w.offscreen_views().1)
        .unwrap()
        .expect("the Activity view");
    b.cx.update_window(second.into(), |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    let rows = feed.read_with(&b.cx, |v, _| v.state().feed().rows().count());
    println!(
        "\nprefetched (a window open 500 ms, then Activity): first frame {:.1} ms, {rows} rows on it",
        ms(open.elapsed())
    );
    let _ = std::fs::remove_dir_all(&home);
    std::process::exit(0)
}
