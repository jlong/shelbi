//! `shelbi-desktop --bench-board` (feature `visual-check`): time the Issues
//! board's frames on a large board (`dt-board-perf`).
//!
//! Like `--visual-figma` it points `SHELBI_HOME` at a fresh short `/tmp`
//! directory (unsetting the hub and control socket variables so nothing
//! reaches a live hub) and writes a fixture project there: `--open <n>` open
//! issues across Backlog, Todo, In Progress and Review and `--done <n>` done
//! ones (default 80 and 1,100), with titles and excerpts of varied length,
//! some blocked. The real window shell ([`ShelbiWindow`]) shows the board
//! offscreen at the Figma frames' 1440x1024, and the bench drives it
//! synchronously: each step dispatches one input through the window (as the
//! platform would), then draws a frame, and the step's time is the input's
//! dispatch plus the whole window's render, layout and paint on the main
//! thread. No display is needed, so the numbers don't depend on a locked
//! screen or what else is on it.
//!
//! The sweeps, each reported as frame-time percentiles:
//!
//! - `idle`: a redraw with nothing changed (what any notify elsewhere in the
//!   window costs the board).
//! - `refresh`: the 750 ms refresh's reads arriving (read off the UI thread,
//!   not timed) and folded in, then a frame. Most refreshes read the board as
//!   it was.
//! - `refresh chg`: the same, folded as reads of a changed board (the model
//!   rebuilt).
//! - `scroll board`: a trackpad swipe across the columns.
//! - `scroll column`: a trackpad swipe down the Done column and back up.
//! - `hover`: the pointer moved down the Done column and across the board.
//! - `select`: the keyboard's selection stepped down the Done column.
//!
//! ```sh
//! cargo run --release --features visual-check -- --bench-board [--done 1100] [--open 80] [--steps 120] [--opacity 85 --blur 20]
//! ```

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use gpui::{
    point, px, size, AnyWindowHandle, AppContext, Modifiers, MouseMoveEvent, Pixels, PlatformInput, Point,
    ScrollDelta, ScrollWheelEvent, TouchPhase, VisualTestAppContext, WindowHandle,
};
use shelbi_app::nav::View;
use shelbi_app::view::{SidebarModel, SidebarTarget};
use shelbi_core::{Column, Issue};

use crate::app::{HostCall, ShelbiWindow};
use crate::views::issues::IssuesView;

const PROJECT: &str = "bench";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 1024.0;

struct Options {
    done: usize,
    open: usize,
    steps: usize,
    /// The window's translucency (`--opacity <percent>`, `--blur <points>`),
    /// opaque by default.
    look: crate::theme::window::Translucency,
}

fn parse(args: &[String]) -> Options {
    let mut o = Options {
        done: 1100,
        open: 80,
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
            "--done" => o.done = num("--done"),
            "--opacity" => o.look = crate::theme::window::Translucency::clamped(num("--opacity") as i32, o.look.blur.into()),
            "--blur" => o.look = crate::theme::window::Translucency::clamped(o.look.opacity.into(), num("--blur") as i32),
            "--open" => o.open = num("--open"),
            "--steps" => o.steps = num("--steps"),
            other => panic!("unknown option `{other}`"),
        }
    }
    o
}

// ---- the fixture ------------------------------------------------------------

/// A small deterministic generator, so every run draws the same board.
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

const WORDS: [&str; 40] = [
    "board", "column", "session", "terminal", "review", "workspace", "agent", "daemon", "sidebar",
    "palette", "render", "scroll", "cache", "index", "branch", "merge", "remote", "machine",
    "settings", "theme", "keyboard", "filter", "history", "detail", "status", "workflow", "queue",
    "frame", "layout", "pointer", "selection", "drag", "drop", "notice", "summary", "title",
    "project", "window", "client", "server",
];

fn words(rng: &mut Lcg, n: usize) -> String {
    let mut out: Vec<&str> = (0..n).map(|_| WORDS[rng.below(WORDS.len() as u64) as usize]).collect();
    if let Some(first) = out.first_mut() {
        // Capitalized like a title or a sentence.
        let s: &'static str = first;
        let cap = s[..1].to_uppercase() + &s[1..];
        return std::iter::once(cap)
            .chain(out[1..].iter().map(|w| w.to_string()))
            .collect::<Vec<_>>()
            .join(" ");
    }
    String::new()
}

fn save(id: &str, rng: &mut Lcg, column: Column, priority: u32, depends_on: Vec<String>) {
    let title_words = 3 + rng.below(12) as usize;
    let excerpt_words = match rng.below(5) {
        0 => 0,
        1 => 6 + rng.below(6) as usize,
        _ => 14 + rng.below(40) as usize,
    };
    let mut body = String::new();
    if excerpt_words > 0 {
        body.push_str(&words(rng, excerpt_words));
        body.push_str(".\n\n");
    }
    body.push_str("## Acceptance criteria\n\n- [ ] It works\n- [x] It is tested\n");
    let day = 1 + (rng.below(28) as u32);
    let task = Issue {
        id: id.into(),
        title: words(rng, title_words),
        column,
        priority,
        assigned_to: None,
        workflow: Some("task".into()),
        branch: Some(format!("jlong/{id}")),
        depends_on,
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: Utc.with_ymd_and_hms(2026, 9, day, 12, 0, 0).single().unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 10, day.min(17), 12, 0, 0).single().unwrap(),
        params: Default::default(),
    };
    shelbi_state::save_task(PROJECT, &task, &body).expect("save a fixture task");
}

fn write_fixtures(home: &Path, opts: &Options) {
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
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
  - name: alpha
    machine: hub
  - name: bravo
    machine: hub
",
            work = home.join("work").display()
        ),
    )
    .unwrap();
    let mut rng = Lcg(0x5eed);
    // The open issues: half in Backlog, the rest spread on.
    let open_columns = [
        (Column::backlog(), 50),
        (Column::todo(), 25),
        (Column::in_progress(), 15),
        (Column::review(), 10),
    ];
    let mut n = 0;
    for (column, share) in open_columns {
        let count = opts.open * share / 100;
        for i in 0..count {
            // Every seventh open issue waits on another open one (blocked).
            let deps = if n > 0 && n % 7 == 0 { vec![format!("open-{}", n - 1)] } else { vec![] };
            save(&format!("open-{n}"), &mut rng, column.clone(), i as u32 + 1, deps);
            n += 1;
        }
    }
    for i in 0..opts.done {
        let deps = if i > 0 && i % 9 == 0 { vec![format!("done-{}", i - 1)] } else { vec![] };
        save(&format!("done-{i}"), &mut rng, Column::done(), 1, deps);
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
    board: gpui::Entity<IssuesView>,
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

    fn swipe(&mut self, at: Point<Pixels>, dx: f32, dy: f32, phase: TouchPhase) -> Duration {
        self.step(Some(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: phase,
        })))
    }

    /// A whole trackpad gesture of `steps` deltas, each step's frame timed.
    fn gesture(&mut self, at: Point<Pixels>, dx: f32, dy: f32, steps: usize, out: &mut Vec<Duration>) {
        self.mouse_to(at);
        for i in 0..steps {
            let phase = if i == 0 { TouchPhase::Started } else { TouchPhase::Moved };
            out.push(self.swipe(at, dx, dy, phase));
        }
        out.push(self.swipe(at, 0.0, 0.0, TouchPhase::Ended));
    }

    fn done_column(&mut self) -> usize {
        self.board.read_with(&self.cx, |v, _| {
            v.bench_model_columns()
                .iter()
                .position(|(cat, _)| *cat == shelbi_core::StatusCategory::Done)
                .expect("a Done column")
        })
    }

    /// Column `ci`'s rectangle in the window, as last drawn.
    fn column_rect(&mut self, ci: usize) -> gpui::Bounds<Pixels> {
        self.board
            .read_with(&self.cx, |v, _| v.bench_column_rect(ci))
            .expect("the column was drawn")
    }
}

/// Run it and exit.
pub fn run(args: &[String]) -> ! {
    let opts = parse(args);
    let home = PathBuf::from(format!("/tmp/shbbench-{}", std::process::id()));
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
    write_fixtures(&home, &opts);
    println!(
        "bench-board: {} open + {} done issues written in {:.0} ms",
        opts.open,
        opts.done,
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
    handle
        .update(&mut cx, |w, _, cx| {
            w.offscreen_select_target(&SidebarTarget::View(View::Issues), cx)
        })
        .unwrap();
    handle
        .update(&mut cx, |w, window, cx| w.bench_perform(HostCall::ShowView(View::Issues), window, cx))
        .unwrap();
    cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx)).unwrap();
    let board = handle
        .update(&mut cx, |w, _, _| w.offscreen_views().0)
        .unwrap()
        .expect("the Issues view");

    // Wait for both reads to land (the poll timer doesn't run offscreen).
    let total = opts.open + opts.done;
    let load = Instant::now();
    loop {
        cx.run_until_parked();
        board.update(&mut cx, |v, cx| v.poll(cx));
        let shown = board.read_with(&cx, |v, _| {
            v.bench_model_columns().iter().map(|(_, n)| n).sum::<usize>()
        });
        if shown >= total * 95 / 100 {
            break;
        }
        if load.elapsed() > Duration::from_secs(60) {
            let why = board.read_with(&cx, |v, _| v.bench_status());
            panic!("the board never loaded ({shown} of {total} cards): {why:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut b = Bench { cx, handle, board };
    // A first frame (the cold layout), then a settled one.
    let first = b.step(None);
    b.step(None);
    let counts = b.board.read_with(&b.cx, |v, _| v.bench_model_columns());
    println!(
        "board loaded in {:.0} ms; columns {:?}; first frame {:.1} ms",
        ms(load.elapsed()),
        counts.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
        ms(first)
    );

    let steps = opts.steps;
    let mut sweeps = Vec::new();

    // Idle redraws.
    let frames = (0..steps).map(|_| b.step(None)).collect();
    sweeps.push(Sweep { label: "idle", frames });

    // The 750 ms refresh: both reads arrive (read off the UI thread, not
    // timed) and are folded in, then a frame. Most reads find the board as
    // it was; `refresh chg` folds them as if it had changed.
    for (label, changed) in [("refresh", false), ("refresh chg", true)] {
        let mut frames = Vec::new();
        for _ in 0..steps.min(30) {
            let fold = b.board.update(&mut b.cx, |v, cx| v.bench_refresh(changed, cx));
            let start = Instant::now();
            b.draw();
            frames.push(fold + start.elapsed());
        }
        sweeps.push(Sweep { label, frames });
    }

    // Across the board, then back.
    let done = b.done_column();
    let first_col = b.column_rect(0);
    let over_board = point(first_col.center().x, px(500.0));
    let mut frames = Vec::new();
    b.gesture(over_board, -24.0, 0.0, steps / 2, &mut frames);
    b.gesture(over_board, 24.0, 0.0, steps / 2, &mut frames);
    // Leave the Done column in view.
    let mut scratch = Vec::new();
    b.gesture(over_board, -400.0, 0.0, 10, &mut scratch);
    sweeps.push(Sweep { label: "scroll board", frames });

    // Down the Done column, then back up.
    let col = b.column_rect(done);
    let over_done = point(col.center().x, px(600.0));
    let mut frames = Vec::new();
    b.gesture(over_done, 0.0, -60.0, steps, &mut frames);
    b.gesture(over_done, 0.0, 60.0, steps, &mut frames);
    sweeps.push(Sweep { label: "scroll column", frames });

    // The pointer down the Done column, then across the board.
    let mut frames = Vec::new();
    for i in 0..steps {
        let y = 120.0 + (i as f32 * 7.0) % (HEIGHT - 200.0);
        frames.push(b.mouse_to(point(col.center().x, px(y))));
    }
    for i in 0..steps {
        let x = 300.0 + (i as f32 * 9.0) % (WIDTH - 320.0);
        frames.push(b.mouse_to(point(px(x), px(400.0))));
    }
    sweeps.push(Sweep { label: "hover", frames });

    // The keyboard's selection down the Done column.
    b.board.update(&mut b.cx, |v, cx| v.bench_select(done, 0, cx));
    b.step(None);
    let mut frames = Vec::new();
    for _ in 0..steps {
        let start = Instant::now();
        b.cx.dispatch_keystroke(b.window(), gpui::Keystroke::parse("down").unwrap());
        b.draw();
        frames.push(start.elapsed());
    }
    sweeps.push(Sweep { label: "select", frames });

    report(&sweeps);
    let _ = std::fs::remove_dir_all(&home);
    std::process::exit(0)
}
