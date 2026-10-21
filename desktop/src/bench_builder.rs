//! `shelbi-desktop --bench-builder` (feature `visual-check`): time the
//! workflow editor's Builder frames on a 20-status, 40-transition workflow
//! (`dt-wf-editor-builder`).
//!
//! Like `--bench-board` it points `SHELBI_HOME` at a fresh short `/tmp`
//! directory (unsetting the hub and control socket variables so nothing
//! reaches a live hub), writes the Workflows fixture there with the `big`
//! workflow ([`crate::views::workflow_graph::big_fixture`]), opens it in
//! the real window shell offscreen at 1440x1024, and drives it
//! synchronously: each step dispatches one input through the window, then
//! draws a frame; the step's time is the input's dispatch plus the whole
//! window's render, layout and paint.
//!
//! The sweeps, each reported as frame-time percentiles:
//!
//! - `idle`: a redraw with nothing changed.
//! - `pan`: a trackpad swipe across the canvas.
//! - `zoom`: ⌘-scroll in and out about the pointer.
//! - `hover`: the pointer moved over the nodes (their handles show).
//! - `draw`: a handle dragged across the canvas and over nodes.
//!
//! ```sh
//! cargo run --release --features visual-check -- --bench-builder [--steps 120]
//! ```

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    point, px, size, AnyWindowHandle, AppContext, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, PlatformInput, Point, ScrollDelta, ScrollWheelEvent, TouchPhase, VisualTestAppContext, WindowHandle,
};
use shelbi_app::view::SidebarModel;

use crate::app::ShelbiWindow;
use crate::views::settings_nav::SettingsPage;
use crate::views::workflow_graph::{big_fixture, Graph, Side};

const PROJECT: &str = "bench";
const WIDTH: f32 = 1440.0;
const HEIGHT: f32 = 1024.0;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    ms(sorted[((sorted.len() - 1) as f64 * p).round() as usize])
}

struct Bench {
    cx: VisualTestAppContext,
    handle: WindowHandle<ShelbiWindow>,
}

impl Bench {
    fn window(&self) -> AnyWindowHandle {
        self.handle.into()
    }

    fn step(&mut self, input: Option<PlatformInput>) -> Duration {
        let start = Instant::now();
        if let Some(input) = input {
            self.cx
                .update_window(self.window(), |_, window, cx| {
                    window.dispatch_event(input, cx);
                })
                .unwrap();
        }
        self.cx
            .update_window(self.window(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        start.elapsed()
    }

    fn mouse(&mut self, at: Point<Pixels>, held: bool) -> Duration {
        self.step(Some(PlatformInput::MouseMove(MouseMoveEvent {
            position: at,
            pressed_button: held.then_some(MouseButton::Left),
            modifiers: Modifiers::default(),
        })))
    }

    fn scroll(&mut self, at: Point<Pixels>, dx: f32, dy: f32, modifiers: Modifiers, phase: TouchPhase) -> Duration {
        self.step(Some(PlatformInput::ScrollWheel(ScrollWheelEvent {
            position: at,
            delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
            modifiers,
            touch_phase: phase,
        })))
    }

    /// The graph frame's origin in the window and the zoom, as last drawn.
    fn frame(&mut self) -> (Point<Pixels>, f32) {
        self.handle
            .update(&mut self.cx, |w, _, cx| {
                let editor = w.offscreen_settings().and_then(|s| s.read(cx).editor()).expect("the editor shows");
                editor.read(cx).bench_frame()
            })
            .unwrap()
    }
}

/// Run it and exit.
pub fn run(args: &[String]) -> ! {
    let mut steps = 120;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--steps" => steps = it.next().and_then(|v| v.parse().ok()).expect("--steps needs a number"),
            other => panic!("unknown option `{other}`"),
        }
    }
    let home = PathBuf::from(format!("/tmp/shbbuild-{}", std::process::id()));
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
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: Bench\nrepo: ''\ndefault_branch: main\ndefault_workflow: task\norchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\nmachines:\n  - name: hub\n    kind: local\n    \
             work_dir: {}\nworkspaces:\n  - name: alpha\n    machine: hub\n",
            home.join("work").display()
        ),
    )
    .unwrap();
    let project_dir = projects.join(PROJECT);
    crate::views::workflows_state::fixture::write(&project_dir);
    std::fs::write(project_dir.join("workflows/statuses.yaml"), big_fixture::statuses()).unwrap();
    std::fs::write(project_dir.join("workflows/big.yaml"), big_fixture::workflow()).unwrap();

    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.update(|cx| {
        crate::visual_check::install_theme(cx);
        crate::assets::load_fonts(cx);
        crate::elements::terminal::TerminalSettings::init(cx);
    });
    let (review, _workspace) = crate::bench_push::fake_deps(Duration::ZERO, 1);
    let mut model = SidebarModel::skeleton(PROJECT);
    model.board_loading = false;
    let handle = cx
        .open_offscreen_window(size(px(WIDTH), px(HEIGHT)), move |window, cx| {
            cx.new(|cx| ShelbiWindow::offscreen(PROJECT, model, review, Rc::new(Instant::now), window, cx))
        })
        .expect("open the offscreen window");
    let mut b = Bench { cx, handle };
    b.handle
        .update(&mut b.cx, |w, window, cx| w.offscreen_open_settings(SettingsPage::Workflows, window, cx))
        .unwrap();
    b.step(None);
    // The editor reads the workflows off the UI thread.
    let load = Instant::now();
    loop {
        b.handle
            .update(&mut b.cx, |w, window, cx| {
                if w.offscreen_settings().is_some_and(|s| s.read(cx).editor().is_none()) {
                    w.offscreen_edit_workflow("big", window, cx);
                }
            })
            .unwrap();
        b.cx.run_until_parked();
        b.step(None);
        let loaded = b
            .handle
            .update(&mut b.cx, |w, _, cx| {
                w.offscreen_settings()
                    .and_then(|s| s.read(cx).editor())
                    .is_some_and(|e| e.read(cx).state().is_some_and(|s| s.workflow().statuses.len() == 20))
            })
            .unwrap();
        if loaded {
            break;
        }
        if load.elapsed() > Duration::from_secs(30) {
            panic!("the big workflow never loaded");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let first = b.step(None);
    b.step(None);
    println!("bench-builder: 20 statuses, 40 transitions; first frame {:.1} ms", ms(first));

    let graph = b
        .handle
        .update(&mut b.cx, |w, _, cx| {
            let editor = w.offscreen_settings().and_then(|s| s.read(cx).editor()).expect("the editor shows");
            let s = editor.read(cx).state().expect("loaded");
            Graph::of(s.workflow(), s.catalog())
        })
        .unwrap();
    let mut sweeps: Vec<(&str, Vec<Duration>)> = Vec::new();

    let idle = (0..steps).map(|_| b.step(None)).collect();
    sweeps.push(("idle", idle));

    let canvas_mid = point(px(WIDTH / 2.0), px(HEIGHT / 2.0));
    let mut pan = Vec::new();
    for i in 0..steps {
        let dy = if i < steps / 2 { -8.0 } else { 8.0 };
        let phase = if i == 0 { TouchPhase::Started } else { TouchPhase::Moved };
        pan.push(b.scroll(canvas_mid, 3.0, dy, Modifiers::default(), phase));
    }
    b.scroll(canvas_mid, 0.0, 0.0, Modifiers::default(), TouchPhase::Ended);
    sweeps.push(("pan", pan));

    let mut zoom = Vec::new();
    for i in 0..steps {
        let dy = if i < steps / 2 { -4.0 } else { 4.0 };
        zoom.push(b.scroll(canvas_mid, 0.0, dy, Modifiers::command(), TouchPhase::Moved));
    }
    sweeps.push(("zoom", zoom));

    // Hover: the pointer over each node's middle in turn.
    let (origin, z) = b.frame();
    let at = |p: crate::views::workflow_graph::Pt| point(origin.x + px(p.x * z), origin.y + px(p.y * z));
    let mut hover = Vec::new();
    for i in 0..steps {
        let n = &graph.nodes[i % graph.nodes.len()];
        hover.push(b.mouse(at(n.rect.center()), false));
    }
    sweeps.push(("hover", hover));

    // Draw: hover the first node, press its right handle, drag over the
    // others.
    let first_node = graph.nodes[0].rect;
    b.mouse(at(first_node.center()), false);
    let start = at(first_node.side(Side::Right));
    b.step(Some(PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position: start,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    })));
    let mut draw = Vec::new();
    for i in 0..steps {
        let n = &graph.nodes[(i / 4) % graph.nodes.len()];
        let c = n.rect.center();
        let wobble = (i % 4) as f32 * 9.0;
        draw.push(b.mouse(at(crate::views::workflow_graph::Pt::new(c.x + wobble, c.y)), true));
    }
    b.step(Some(PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position: start,
        modifiers: Modifiers::default(),
        click_count: 1,
    })));
    sweeps.push(("draw", draw));

    println!(
        "\n{:<8} {:>7} {:>8} {:>8} {:>8} {:>8} {:>9} {:>9}",
        "sweep", "frames", "p50 ms", "p90 ms", "p99 ms", "max ms", "> 8.3 ms", "> 16.7 ms"
    );
    for (label, mut f) in sweeps {
        f.sort();
        let over = |limit: f64| f.iter().filter(|d| ms(**d) > limit).count();
        println!(
            "{:<8} {:>7} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>9} {:>9}",
            label,
            f.len(),
            pct(&f, 0.5),
            pct(&f, 0.9),
            pct(&f, 0.99),
            pct(&f, 1.0),
            over(8.33),
            over(16.67)
        );
    }
    let _ = std::fs::remove_dir_all(&home);
    std::process::exit(0);
}
