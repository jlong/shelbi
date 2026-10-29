//! `shelbi-desktop --bench-term` (feature `bench`): measure the terminal
//! element under heavy streaming output, in the real app.
//!
//! One command (`scripts/bench-term.sh`, or `cargo run --release --features
//! bench -- --bench-term`) runs every case and prints a report:
//!
//! - **Stream cases**, one per input ([`corpus`]) and [`Source`]:
//!   - `direct`: the input goes straight into a [`TerminalPane`]'s event
//!     channel at 32 MiB/s (about 96 MiB per input), faster than any PTY on
//!     this Mac delivers: the pump, the emulator and the element under a
//!     flood. The grid is then compared with a view fed the same chunks.
//!   - `session`: a real session in a sandboxed Shelbi root (`/tmp/shb.*`,
//!     the hub's socket variables unset) runs `shelbi-desktop --bench-feed`,
//!     which writes the input (about 6 MiB) to its PTY as fast as the PTY
//!     takes it once the bench has attached. The pane attaches through the
//!     app's own connect, so the bytes go PTY -> session (its emulator, its
//!     per-client backpressure) -> socket -> reader thread -> pane -> frames.
//!     The grid is compared with a second client attached for the whole
//!     stream (the way the TUI and `shelbi attach` render it), and with a
//!     fresh emulator built from the session's replay (what a client that
//!     attaches later starts from).
//!
//!   For each: frames per second, frame time percentiles (the window's render
//!   through paint, measured at the root view), dropped frames (display
//!   intervals missed while output was arriving), CPU (this process, all
//!   threads, as a share of one core), throughput, main-thread time spent
//!   feeding the emulator, how often the session had to resync a lagging
//!   client, and the **palette probe**: while output streams, the command
//!   palette (the real overlay, `views::palette`) is opened every 400 ms from
//!   a timer on another thread and the time until a frame paints it is
//!   recorded, which covers the main thread's queue delay as a keypress would
//!   see it.
//! - **Idle**: the pane stays attached to a quiet session; frames painted and
//!   CPU over five seconds. (`scripts/idle-cpu.sh` measures the full app.)
//! - **Many terminals**: six workspace sessions stream a paced Claude Code
//!   capture while the window shows one through the app's real
//!   [`SessionView`]: CPU and frames while a quiet seventh is shown (hidden
//!   views must cost no frames), then the latency of switching the view to
//!   each streaming workspace (switch until a frame paints its content).
//!
//! Options: `--input <name>[,<name>...]` (default all), `--source
//! direct|session` (default both), `--skip-stream`, `--skip-idle`,
//! `--skip-many`, `--json <file>` (append the results), `--label <text>`.
//! The `shelbi` CLI that runs the sessions comes from `$SHELBI_BENCH_CLI`,
//! `desktop.cli_path`, or `PATH`.
//!
//! Helpers, in the same binary: `--bench-feed` (the sessions' program),
//! `--bench-record` (capture a live session for the corpus), and
//! `--bench-client` (the desktop side of `scripts/multiclient-check.sh`).
//!
//! [`TerminalPane`]: crate::elements::terminal::TerminalPane
//! [`SessionView`]: crate::views::session_view::SessionView

pub mod client;
pub mod corpus;
pub mod feed;
pub mod record;
mod report;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::*, px, size, App, AsyncApp, Bounds, Context, Entity, Window,
    WindowBounds, WindowHandle, WindowOptions,
};
use shelbi_app::session::{ConnectFailure, NoopRelauncher, ReconnectPolicy, RetryPolicy, SessionRef};
use shelbi_client::{Connection, SessionEvent, SpawnSpec};
use shelbi_proto::capability;
use shelbi_state::keymap::Keymaps;
use shelbi_term::view::TerminalView;
use shelbi_term::Size;

use crate::elements::terminal::session::{self as pane_session, Connected};
use crate::elements::terminal::{TerminalPane, TerminalSettings};
use crate::theme::Theme;
use crate::views::overlay::{ActiveOverlay, OverlayEvent, OverlayHost};
use crate::views::session_view::{SessionConnector, SessionDeps, SessionView};
use report::{CaseResult, IdleResult, ManyResult, Report};

/// The display's refresh period the dropped-frame count is measured against.
const REFRESH: Duration = Duration::from_micros(16_667);

/// How often the palette probe opens the palette during a flood.
const PROBE_EVERY: Duration = Duration::from_millis(400);

/// How long a stream case may run before it is cut off.
const CASE_TIMEOUT: Duration = Duration::from_secs(90);

/// Output is drained when the feeder is done and nothing arrived for this
/// long.
const QUIET: Duration = Duration::from_millis(400);

/// The many-terminals case: workspaces streaming at once, and each one's
/// paced rate (a busy agent's tool output).
const MANY_SESSIONS: usize = 6;
const MANY_RATE: u64 = 200 * 1024;

/// What the bench was asked to do.
struct Options {
    inputs: Vec<String>,
    sources: Vec<Source>,
    many: bool,
    idle: bool,
    json: Option<PathBuf>,
    label: String,
}

fn parse(args: &[String]) -> Options {
    let mut o = Options {
        inputs: corpus::NAMES.iter().map(|s| s.to_string()).collect(),
        sources: vec![Source::Direct, Source::Session],
        many: true,
        idle: true,
        json: None,
        label: String::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--input" => {
                o.inputs = it
                    .next()
                    .map(|v| v.split(',').map(str::to_string).collect())
                    .unwrap_or_default()
            }
            "--source" => {
                o.sources = match it.next().map(String::as_str) {
                    Some("session") => vec![Source::Session],
                    Some("direct") => vec![Source::Direct],
                    _ => vec![Source::Direct, Source::Session],
                }
            }
            "--skip-stream" => o.inputs.clear(),
            "--skip-many" => o.many = false,
            "--skip-idle" => o.idle = false,
            "--json" => o.json = it.next().map(PathBuf::from),
            "--label" => o.label = it.next().cloned().unwrap_or_default(),
            other => {
                eprintln!("bench-term: unknown option `{other}`");
                std::process::exit(2);
            }
        }
    }
    o
}

/// The sandboxed Shelbi root the bench's sessions live in.
struct Sandbox {
    root: PathBuf,
    cli: PathBuf,
    exe: PathBuf,
    /// Sessions the bench started, killed at the end.
    sockets: RefCell<Vec<PathBuf>>,
}

impl Sandbox {
    /// Make the sandbox and point this process at it. Must run before any
    /// thread starts (it sets environment variables).
    fn create() -> Sandbox {
        // Find the CLI first: `desktop.cli_path` is read from the real root.
        let cli = find_cli().unwrap_or_else(|| {
            eprintln!("bench-term: no `shelbi` CLI found (set SHELBI_BENCH_CLI)");
            std::process::exit(2);
        });
        // Short, under /tmp: a session socket path must fit in 104 bytes.
        let root = PathBuf::from(format!("/tmp/shb.{}", std::process::id()));
        std::fs::create_dir_all(root.join("corpus")).expect("create the sandbox");
        // SAFETY: called from main before any other thread exists.
        unsafe {
            std::env::set_var("SHELBI_ROOT", &root);
            std::env::set_var("SHELBI_HOME", &root);
            for var in [
                "SHELBI_HUB_SOCK",
                "SHELBI_HUB_ADDR",
                "SHELBI_CONTROL_SOCK",
                "SHELBI_REMOTE_HUB_SOCK",
                "SHELBI_MANAGED_CONTEXT",
                "SHELBI_PROJECT",
                "SHELBI_SESSION",
                "TASK_ID",
                "PROJECT",
            ] {
                std::env::remove_var(var);
            }
        }
        Sandbox {
            root,
            cli,
            exe: std::env::current_exe().expect("the bench's own path"),
            sockets: RefCell::new(Vec::new()),
        }
    }

    /// Write `corpus` where the feeder reads it.
    fn corpus_file(&self, corpus: &corpus::Corpus) -> PathBuf {
        let path = self.root.join("corpus").join(format!("{}.bin", corpus.name));
        std::fs::write(&path, &corpus.bytes).expect("write the corpus");
        path
    }

    /// The file whose creation starts a feeder.
    fn go_file(&self, name: &str) -> PathBuf {
        self.root.join(format!("go-{}", name.replace('/', "-")))
    }

    /// Start a session `name` running the feeder over `file` (paced at
    /// `rate` bytes a second, 0 = as fast as the PTY takes it).
    /// `repeat`: write the file that many times, or forever (`None`).
    fn spawn_feeder(&self, name: &str, size: Size, file: &Path, rate: u64, repeat: Option<usize>) -> PathBuf {
        let go = self.go_file(name);
        let _ = std::fs::remove_file(&go);
        let _ = std::fs::remove_file(go.with_extension("done"));
        let mut argv = vec![
            self.exe.display().to_string(),
            "--bench-feed".into(),
            file.display().to_string(),
            rate.to_string(),
            go.display().to_string(),
        ];
        match repeat {
            Some(n) => argv.extend(["--repeat".into(), n.to_string()]),
            None => argv.push("--loop".into()),
        }
        self.spawn(name, size, argv)
    }

    fn spawn(&self, name: &str, size: Size, child_argv: Vec<String>) -> PathBuf {
        let spec = SpawnSpec {
            name: name.to_string(),
            cwd: self.root.clone(),
            cols: size.cols,
            rows: size.rows,
            task: None,
            raw_output_log: false,
            child_argv,
        };
        let spawned = shelbi_client::spawn_with_exe(&self.cli, &spec)
            .unwrap_or_else(|e| panic!("spawn {name}: {e}"));
        self.sockets.borrow_mut().push(spawned.sock.clone());
        spawned.sock
    }

    /// Kill every session the bench started (through its own socket), and
    /// remove the sandbox.
    fn teardown(&self) {
        for sock in self.sockets.borrow().iter() {
            if let Ok((conn, _)) = Connection::open(sock, None, capability::ALL) {
                let _ = conn.kill(Some(libc::SIGKILL));
            }
        }
        std::thread::sleep(Duration::from_millis(300));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn find_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SHELBI_BENCH_CLI") {
        return Some(PathBuf::from(p));
    }
    if let Ok(Some(p)) = crate::platform::discovery::read_setting() {
        return Some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("shelbi"))
            .find(|p| crate::platform::discovery::is_executable_file(p))
    })
}

/// What a frame recorded (root render through the end of paint).
#[derive(Default)]
struct Probe {
    frame_start: Option<Instant>,
    /// (start, end) of each painted frame.
    frames: Vec<(Instant, Instant)>,
    /// A palette open requested at this time, not yet painted.
    palette_requested: Option<Instant>,
    palette_latency: Vec<Duration>,
}

/// What the window shows.
enum Content {
    Empty,
    Pane(Entity<TerminalPane>),
    Session(Entity<SessionView>),
}

/// The bench window's root.
struct BenchRoot {
    content: Content,
    overlay: Option<ActiveOverlay>,
    keymaps: Keymaps,
    probe: Rc<RefCell<Probe>>,
}

impl OverlayHost for BenchRoot {
    fn overlay_mut(&mut self) -> Option<&mut ActiveOverlay> {
        self.overlay.as_mut()
    }

    fn overlay_event(&mut self, _event: OverlayEvent, _window: &mut Window, _cx: &mut Context<Self>) {}
}

impl Render for BenchRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.probe.borrow_mut().frame_start = Some(Instant::now());
        let theme = *cx.global::<Theme>();
        let overlay = self.overlay.as_ref().map(|o| {
            crate::views::overlay::render_layer(o, window.viewport_size(), None, &self.keymaps, &theme, cx)
        });
        let palette_open = self.overlay.is_some();
        let probe = self.probe.clone();
        let content = match &self.content {
            Content::Empty => div().into_any_element(),
            Content::Pane(p) => p.clone().into_any_element(),
            Content::Session(s) => s.clone().into_any_element(),
        };
        div()
            .relative()
            .size_full()
            .bg(theme.canvas)
            .child(content)
            .children(overlay)
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, _, _| {
                        let end = Instant::now();
                        let mut p = probe.borrow_mut();
                        if let Some(start) = p.frame_start.take() {
                            p.frames.push((start, end));
                        }
                        if palette_open {
                            if let Some(asked) = p.palette_requested.take() {
                                p.palette_latency.push(end - asked);
                            }
                        }
                    },
                )
                .absolute()
                .size_0(),
            )
    }
}

/// Run the bench and exit.
pub fn run(args: &[String]) -> ! {
    let options = parse(args);
    let sandbox = Rc::new(Sandbox::create());
    let corpora: Vec<corpus::Corpus> = options
        .inputs
        .iter()
        .map(|n| corpus::load(n).unwrap_or_else(|e| {
            eprintln!("bench-term: {e}");
            std::process::exit(2);
        }))
        .collect();
    println!(
        "bench-term: sandbox {}, CLI {}, inputs {}",
        sandbox.root.display(),
        sandbox.cli.display(),
        options.inputs.join(", ")
    );
    let options = Rc::new(options);
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            crate::assets::load_fonts(cx);
            TerminalSettings::init(cx);
            let probe = Rc::new(RefCell::new(Probe::default()));
            let root_probe = probe.clone();
            let bounds = Bounds {
                origin: point(px(40.0), px(60.0)),
                size: size(px(1000.0), px(700.0)),
            };
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        focus: false,
                        ..Default::default()
                    },
                    move |_, cx| {
                        cx.new(|_| BenchRoot {
                            content: Content::Empty,
                            overlay: None,
                            keymaps: shelbi_state::keymap::load_keymaps(None).0,
                            probe: root_probe,
                        })
                    },
                )
                .expect("open the bench window");
            let sandbox = sandbox.clone();
            cx.spawn(async move |cx| {
                let mut report = Report::new(options.label.clone());
                for &source in &options.sources {
                    for corpus in &corpora {
                        let result = stream_case(corpus, source, &sandbox, window, &probe, cx).await;
                        result.print();
                        report.cases.push(result);
                    }
                }
                if options.idle {
                    let idle = idle_case(window, &probe, cx).await;
                    idle.print();
                    report.idle = Some(idle);
                }
                if options.many {
                    let many = many_case(&sandbox, window, &probe, cx).await;
                    many.print();
                    report.many = Some(many);
                }
                report.print_table();
                if let Some(path) = &options.json {
                    report.append_json(path);
                }
                sandbox.teardown();
                cx.update(|cx| cx.quit());
            })
            .detach();
        });
    std::process::exit(0)
}

/// CPU time (user + system, all threads) used by this process so far.
fn cpu_time() -> Duration {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct we own.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

async fn sleep(cx: &AsyncApp, d: Duration) {
    cx.background_executor().timer(d).await;
}

fn with_root<R>(
    window: WindowHandle<BenchRoot>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&mut BenchRoot, &mut Window, &mut Context<BenchRoot>) -> R,
) -> R {
    window.update(cx, f).expect("the bench window is open")
}

fn pane_stat<R>(
    window: WindowHandle<BenchRoot>,
    cx: &mut AsyncApp,
    f: impl FnOnce(&TerminalPane) -> R,
) -> Option<R> {
    with_root(window, cx, |root, _, cx| {
        let pane = match &root.content {
            Content::Pane(p) => Some(p.clone()),
            Content::Session(s) => s.read(cx).pane().cloned(),
            Content::Empty => None,
        }?;
        Some(f(pane.read(cx)))
    })
}

/// The palette probe: a thread that, every [`PROBE_EVERY`], stamps the time
/// and asks the UI to open the palette, then to close it.
fn start_palette_probe(
    window: WindowHandle<BenchRoot>,
    cx: &mut AsyncApp,
) -> (Arc<std::sync::atomic::AtomicBool>, gpui::Task<()>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = async_channel::unbounded::<Option<Instant>>();
    let thread_stop = stop.clone();
    std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            std::thread::sleep(PROBE_EVERY / 2);
            if tx.send_blocking(Some(Instant::now())).is_err() {
                return;
            }
            std::thread::sleep(PROBE_EVERY / 2);
            if tx.send_blocking(None).is_err() {
                return;
            }
        }
    });
    let task = cx.spawn(async move |cx| {
        while let Ok(msg) = rx.recv().await {
            let _ = window.update(cx, |root, _, cx| {
                match msg {
                    Some(asked) => {
                        let entries = shelbi_app::CommandRegistry::new()
                            .entries(&shelbi_app::command::CommandModel::default());
                        root.overlay = Some(ActiveOverlay::palette(entries, Vec::new(), Default::default()));
                        root.probe.borrow_mut().palette_requested = Some(asked);
                    }
                    None => {
                        root.overlay = None;
                        root.probe.borrow_mut().palette_requested = None;
                    }
                }
                cx.notify();
            });
        }
    });
    (stop, task)
}

/// Where a stream case's output comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A real session in the sandbox: PTY -> session -> socket -> pane. On a
    /// Mac the PTY is the bottleneck (a few MiB/s, less under load), so this
    /// measures the app as a user sees it.
    Session,
    /// Straight into the pane's event channel at [`DIRECT_RATE`], far faster
    /// than any PTY delivers: the element and the pump under a flood they
    /// can't keep up with.
    Direct,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Session => "session",
            Source::Direct => "direct",
        }
    }

    /// About how many bytes a case writes.
    fn total(self) -> usize {
        match self {
            Source::Session => 6 << 20,
            Source::Direct => 96 << 20,
        }
    }
}

/// The direct source's rate: ten times what a Mac PTY gave the session on
/// the loaded dev machine, about what one gives an idle one.
const DIRECT_RATE: u64 = 32 << 20;

/// The direct source's event size: about what a PTY read hands the session.
const DIRECT_CHUNK: usize = 16 * 1024;

/// A session that does nothing with the pane's input (the direct source).
struct NullIo;

impl pane_session::SessionIo for NullIo {
    fn input(&self, _bytes: Vec<u8>) {}
    fn paste(&self, _text: String, _bracketed: bool) {}
    fn resize(&self, _size: Size) {}
}

/// What a stream case compares the pane's grid with at the end.
enum Reference {
    /// A second client on the session.
    Live(Result<LiveReference, String>),
    /// The same chunks fed into a view by the direct source's thread.
    Direct(Arc<std::sync::Mutex<TerminalView>>),
}

/// One input, streamed into the pane.
async fn stream_case(
    corpus: &corpus::Corpus,
    source: Source,
    sandbox: &Sandbox,
    window: WindowHandle<BenchRoot>,
    probe: &Rc<RefCell<Probe>>,
    cx: &mut AsyncApp,
) -> CaseResult {
    let name = format!("bench/{}", corpus.name);
    let repeat = corpus.repeat_for(source.total());
    let total = corpus.bytes.len() * repeat;
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut sock = None;
    let mut direct_tx = None;
    match source {
        Source::Session => {
            let file = sandbox.corpus_file(corpus);
            sock = Some(sandbox.spawn_feeder(&name, corpus.size, &file, 0, Some(repeat)));
            with_root(window, cx, |root, window, cx| {
                let pane = cx.new(|cx| TerminalPane::connect(name.clone(), window, cx));
                root.content = Content::Pane(pane);
                cx.notify();
            });
        }
        Source::Direct => {
            let (tx, rx) = futures::channel::mpsc::unbounded();
            direct_tx = Some(tx);
            with_root(window, cx, |root, window, cx| {
                let connected = Connected {
                    link: Box::new(NullIo),
                    events: rx,
                    size: corpus.size,
                    name: name.clone(),
                    cwd: None,
                };
                let pane = cx.new(|cx| TerminalPane::attached_to(connected, window, cx));
                root.content = Content::Pane(pane);
                cx.notify();
            });
        }
    }
    // Wait for the attach (and our size report) to land.
    let attach_deadline = Instant::now() + Duration::from_secs(30);
    while pane_stat(window, cx, |p| p.is_live()) != Some(true) {
        if Instant::now() > attach_deadline {
            panic!("{name}: the pane never attached");
        }
        sleep(cx, Duration::from_millis(20)).await;
    }
    sleep(cx, Duration::from_millis(500)).await;
    let reference = match &sock {
        // A second client attached through the whole stream, rendering it the
        // way the TUI and `shelbi attach` do (every event fed in order).
        Some(sock) => Reference::Live(LiveReference::attach(sock)),
        None => Reference::Direct(Arc::new(std::sync::Mutex::new(TerminalView::new(corpus.size)))),
    };

    let fed0 = pane_stat(window, cx, |p| p.total_fed()).unwrap_or(0);
    let resyncs0 = pane_stat(window, cx, |p| p.resyncs()).unwrap_or(0);
    let feed0 = pane_stat(window, cx, |p| p.feed_time_total()).unwrap_or_default();
    probe.borrow_mut().frames.clear();
    probe.borrow_mut().palette_latency.clear();
    let (stop_probe, probe_task) = start_palette_probe(window, cx);
    let cpu0 = cpu_time();
    let started = Instant::now();
    match (direct_tx.take(), &reference) {
        (Some(tx), Reference::Direct(_)) => {
            let (bytes, done) = (corpus.bytes.clone(), done.clone());
            std::thread::spawn(move || {
                let chunks = bytes.chunks(DIRECT_CHUNK).cycle().take(repeat * bytes.len().div_ceil(DIRECT_CHUNK));
                let mut sent = 0u64;
                for (seq, chunk) in chunks.enumerate() {
                    sent += chunk.len() as u64;
                    let ev = SessionEvent::Output { seq: seq as u64, data: chunk.to_vec() };
                    if tx.unbounded_send(ev).is_err() {
                        break;
                    }
                    let due = Duration::from_secs_f64(sent as f64 / DIRECT_RATE as f64);
                    if let Some(ahead) = due.checked_sub(started.elapsed()) {
                        std::thread::sleep(ahead);
                    }
                }
                done.store(true, std::sync::atomic::Ordering::Relaxed);
                // Keep the channel open: closing it would end the pane.
                std::mem::forget(tx);
            });
        }
        _ => std::fs::write(sandbox.go_file(&name), b"").expect("start the feeder"),
    }

    let done_file = sandbox.go_file(&name).with_extension("done");
    let mut timed_out = false;
    loop {
        sleep(cx, Duration::from_millis(20)).await;
        let last = pane_stat(window, cx, |p| p.last_fed_at()).flatten();
        let quiet = last.is_some_and(|t| t > started && t.elapsed() >= QUIET);
        let finished = match source {
            Source::Session => done_file.exists(),
            Source::Direct => done.load(std::sync::atomic::Ordering::Relaxed),
        };
        if finished && quiet {
            break;
        }
        if started.elapsed() > CASE_TIMEOUT {
            timed_out = true;
            break;
        }
    }
    let wall = started.elapsed();
    let cpu = cpu_time() - cpu0;
    stop_probe.store(true, std::sync::atomic::Ordering::Relaxed);
    drop(probe_task);
    with_root(window, cx, |root, _, cx| {
        root.overlay = None;
        cx.notify();
    });

    // The stream: from the go signal to the last output fed.
    let drained = pane_stat(window, cx, |p| p.last_fed_at())
        .flatten()
        .unwrap_or(started);
    let stream = drained.saturating_duration_since(started);
    let fed = pane_stat(window, cx, |p| p.total_fed()).unwrap_or(0) - fed0;
    let feed_time = pane_stat(window, cx, |p| p.feed_time_total()).unwrap_or_default() - feed0;
    let p = probe.borrow();
    let frames: Vec<(Instant, Instant)> = p
        .frames
        .iter()
        .copied()
        .filter(|(s, _)| *s >= started && *s <= drained)
        .collect();
    let resyncs = pane_stat(window, cx, |p| p.resyncs()).unwrap_or(0) - resyncs0;
    let mut result = CaseResult::new(
        corpus.name,
        source.name(),
        total,
        fed,
        stream,
        wall,
        cpu,
        &frames,
        REFRESH,
        &p.palette_latency,
        resyncs,
    );
    drop(p);
    result.feed_ms = feed_time.as_secs_f64() * 1000.0;
    result.timed_out = timed_out;
    let reference_view = match reference {
        Reference::Live(Ok(r)) => Ok(r.settle()),
        Reference::Live(Err(e)) => Err(format!("no live reference: {e}")),
        // The same chunks fed in order after the measurement (feeding them
        // alongside would slow the source and count against the pane's CPU).
        Reference::Direct(view) => {
            let mut v = TerminalView::new(Size::new(1, 1));
            std::mem::swap(&mut v, &mut view.lock().unwrap());
            for chunk in corpus.bytes.chunks(DIRECT_CHUNK).cycle().take(repeat * corpus.bytes.len().div_ceil(DIRECT_CHUNK)) {
                v.feed(chunk);
            }
            Ok(v)
        }
    };
    result.grid = match reference_view {
        Ok(view) => pane_stat(window, cx, |pane| report::grid_diff(pane.view(), &view))
            .unwrap_or_else(|| "no pane".into()),
        Err(e) => e,
    };
    if let Some(sock) = &sock {
        result.grid_replay = compare_grid(sock, window, cx);
    }
    result
}

/// A second client on the session for the whole stream: every event fed into
/// a [`TerminalView`] in order, resizes included, as the TUI's pane does.
struct LiveReference {
    view: Arc<std::sync::Mutex<TerminalView>>,
    last: Arc<std::sync::Mutex<Instant>>,
    _conn: Connection,
}

impl LiveReference {
    fn attach(sock: &Path) -> Result<LiveReference, String> {
        let (conn, events) =
            Connection::open(sock, None, capability::ALL).map_err(|e| e.to_string())?;
        let info = conn.info().map_err(|e| e.to_string())?;
        let view = Arc::new(std::sync::Mutex::new(TerminalView::new(Size::new(
            info.cols.max(1),
            info.rows.max(1),
        ))));
        let last = Arc::new(std::sync::Mutex::new(Instant::now()));
        conn.attach(None).map_err(|e| e.to_string())?;
        let rx = events.into_inner();
        let (v, l) = (view.clone(), last.clone());
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                let mut view = v.lock().unwrap();
                match ev {
                    SessionEvent::Resync { replay, .. } => view.feed(&replay),
                    SessionEvent::Output { data, .. } => view.feed(&data),
                    SessionEvent::Resized { cols, rows, .. }
                    | SessionEvent::SizeChanged { cols, rows } => {
                        view.resize(Size::new(cols.max(1), rows.max(1)))
                    }
                    _ => {}
                }
                *l.lock().unwrap() = Instant::now();
            }
        });
        Ok(LiveReference {
            view,
            last,
            _conn: conn,
        })
    }

    /// The reference once it has been quiet for a moment.
    fn settle(self) -> TerminalView {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.last.lock().unwrap().elapsed() < Duration::from_millis(300)
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut view = TerminalView::new(Size::new(1, 1));
        std::mem::swap(&mut view, &mut self.view.lock().unwrap());
        view
    }
}

/// Compare the pane's grid with the session's own state: a fresh attach's
/// replay fed into a new emulator at the session's size.
fn compare_grid(sock: &Path, window: WindowHandle<BenchRoot>, cx: &mut AsyncApp) -> String {
    let reference = match replay_view(sock) {
        Ok(v) => v,
        Err(e) => return format!("no reference: {e}"),
    };
    pane_stat(window, cx, |pane| report::grid_diff(pane.view(), &reference))
        .unwrap_or_else(|| "no pane".into())
}

/// A [`TerminalView`] rebuilt from the session's attach replay.
fn replay_view(sock: &Path) -> Result<TerminalView, String> {
    let (conn, events) = Connection::open(sock, None, capability::ALL).map_err(|e| e.to_string())?;
    let info = conn.info().map_err(|e| e.to_string())?;
    conn.attach(None).map_err(|e| e.to_string())?;
    let rx = events.into_inner();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(SessionEvent::Resync { replay, .. }) => {
                let mut view = TerminalView::new(Size::new(info.cols, info.rows));
                view.feed(&replay);
                return Ok(view);
            }
            Ok(_) => continue,
            Err(e) => return Err(format!("waiting for the replay: {e}")),
        }
    }
}

/// The pane attached to a quiet session: frames and CPU over five seconds.
async fn idle_case(
    window: WindowHandle<BenchRoot>,
    probe: &Rc<RefCell<Probe>>,
    cx: &mut AsyncApp,
) -> IdleResult {
    sleep(cx, Duration::from_secs(1)).await;
    probe.borrow_mut().frames.clear();
    let cpu0 = cpu_time();
    let started = Instant::now();
    sleep(cx, Duration::from_secs(5)).await;
    let wall = started.elapsed();
    let cpu = cpu_time() - cpu0;
    let frames = probe.borrow().frames.len();
    let live = pane_stat(window, cx, |p| p.is_live()).unwrap_or(false);
    IdleResult {
        attached: live,
        wall_s: wall.as_secs_f64(),
        frames,
        cpu_pct: 100.0 * cpu.as_secs_f64() / wall.as_secs_f64(),
    }
}

/// The bench's session connector: the pane's own connect, in the sandbox,
/// timed.
#[derive(Default)]
struct BenchConnector {
    times: Arc<std::sync::Mutex<Vec<Duration>>>,
}

impl SessionConnector for BenchConnector {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        _retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure> {
        let name = shelbi_app::session::discovery_name(project, target);
        let started = Instant::now();
        let connected = pane_session::connect(&name).map_err(ConnectFailure::Message);
        self.times.lock().unwrap().push(started.elapsed());
        connected
    }
}

/// Six streaming workspaces, one quiet; the window shows one at a time
/// through the real [`SessionView`].
async fn many_case(
    sandbox: &Sandbox,
    window: WindowHandle<BenchRoot>,
    probe: &Rc<RefCell<Probe>>,
    cx: &mut AsyncApp,
) -> ManyResult {
    let capture = corpus::load("claude").unwrap_or_else(|_| corpus::load("cargo").unwrap());
    let file = sandbox.corpus_file(&capture);
    let size = capture.size;
    sandbox.spawn(
        "bench/ws/quiet",
        size,
        vec!["/bin/sh".into(), "-c".into(), "echo quiet; exec sleep 3600".into()],
    );
    let names: Vec<String> = (1..=MANY_SESSIONS).map(|i| format!("w{i}")).collect();
    for n in &names {
        sandbox.spawn_feeder(&format!("bench/ws/{n}"), size, &file, MANY_RATE, None);
    }
    let connector = Arc::new(BenchConnector::default());
    let connect_times = connector.times.clone();
    let view = with_root(window, cx, |root, window, cx| {
        let handle = window.window_handle();
        let deps = SessionDeps {
            connector,
            relauncher: Arc::new(NoopRelauncher),
            retry: RetryPolicy::default(),
            reconnect: ReconnectPolicy::default(),
            clock: Rc::new(Instant::now),
        };
        let view = cx.new(|cx| {
            SessionView::new(
                "bench",
                SessionRef::Workspace("quiet".into()),
                "quiet",
                handle,
                deps,
                None,
                cx,
            )
        });
        view.update(cx, |v, cx| v.show(cx));
        root.content = Content::Session(view.clone());
        cx.notify();
        view
    });
    // Until the shown pane paints its first output: (pane up, first output
    // fed, painted, bytes fed by then).
    let wait_live = async |cx: &mut AsyncApp| {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut pane_up = None;
        loop {
            if pane_up.is_none() && pane_stat(window, cx, |_| ()).is_some() {
                pane_up = Some(Instant::now());
            }
            let marks = pane_stat(window, cx, |p| (p.first_content(), p.total_fed()));
            if let Some(((Some(fed), Some(painted)), bytes)) = marks {
                return Some((pane_up.unwrap_or(fed), fed, painted, bytes));
            }
            if Instant::now() > deadline {
                return None;
            }
            sleep(cx, Duration::from_millis(1)).await;
        }
    };
    wait_live(cx).await;
    // Start every feeder, let them get going.
    for n in &names {
        std::fs::write(sandbox.go_file(&format!("bench/ws/{n}")), b"").unwrap();
    }
    sleep(cx, Duration::from_secs(1)).await;

    // Hidden views: the quiet one shown while six stream.
    probe.borrow_mut().frames.clear();
    let cpu0 = cpu_time();
    let t0 = Instant::now();
    sleep(cx, Duration::from_secs(4)).await;
    let hidden_wall = t0.elapsed();
    let hidden_cpu = cpu_time() - cpu0;
    let hidden_frames = probe.borrow().frames.len();

    // Switch to each streaming workspace: how long until it paints.
    let mut switches = Vec::new();
    let mut shown_frames = Vec::new();
    let mut shown_cpu = Vec::new();
    for n in names.iter().chain(std::iter::once(&"quiet".to_string())) {
        let t = Instant::now();
        view.update(cx, |v, cx| v.show_target(SessionRef::Workspace(n.clone()), cx));
        let marks = wait_live(cx).await;
        if let Some((up, fed, painted, bytes)) = marks {
            println!(
                "   switch to {n}: pane up {:.0} ms, first output fed {:.0} ms, painted {:.0} ms ({bytes} bytes of replay)",
                (up - t).as_secs_f64() * 1000.0,
                (fed - t).as_secs_f64() * 1000.0,
                (painted - t).as_secs_f64() * 1000.0,
            );
        }
        switches.push((n.clone(), marks.map(|(_, _, painted, _)| painted - t)));
        if n != "quiet" {
            probe.borrow_mut().frames.clear();
            let cpu0 = cpu_time();
            let t1 = Instant::now();
            sleep(cx, Duration::from_secs(2)).await;
            shown_cpu.push(100.0 * (cpu_time() - cpu0).as_secs_f64() / t1.elapsed().as_secs_f64());
            let p = probe.borrow();
            shown_frames.extend(p.frames.iter().map(|(s, e)| *e - *s));
        }
    }
    with_root(window, cx, |root, _, cx| {
        root.content = Content::Empty;
        cx.notify();
    });
    let connects: Vec<Duration> = connect_times.lock().unwrap().clone();
    ManyResult::new(
        &connects,
        MANY_SESSIONS,
        MANY_RATE,
        hidden_wall,
        hidden_frames,
        hidden_cpu,
        switches,
        &shown_frames,
        &shown_cpu,
    )
}
