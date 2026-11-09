//! The one progress printer for work done on the plain terminal, outside the
//! TUI's frame: opening a project (`Starting Shelbi…`, see [`crate::startup`])
//! and quitting (`Shutting down Shelbi…`, see [`crate::quit_progress`]).
//!
//! ```text
//! Shutting down Shelbi…
//!   ✓ asked orchestrator for handoff   1.1s
//!   ✓ stopped 5 sessions               0.3s
//!   ✗ stopped sessions on devbox       6.0s  gave up waiting on devbox
//!   ⠋ waiting for daemon to exit       0.4s
//! ```
//!
//! The rules, the same for every command:
//!
//! - a step that finishes inside [`GRACE`] draws nothing, so quick work stays
//!   silent. Once a step outlasts it, the title goes out, a live spinner line
//!   names the running step, and every step from then on leaves a
//!   `✓ name  0.4s` line;
//! - a step that gives up always prints a `✗ … gave up waiting on <what>` line,
//!   naming what it stopped waiting on (as a plain `shelbi:` line on stderr
//!   when drawing is off), since that is news the user needs either way;
//! - drawing happens only on a terminal: not when stderr is redirected, not
//!   under `TERM=dumb`, not with [`NO_PROGRESS_ENV`] set;
//! - a prompt can take the terminal mid-step ([`StepHandle::suspend`]): the
//!   spinner is cleared and nothing draws until it is done.
//!
//! Steps may overlap and may be reported from any thread, either as a closure
//! ([`Progress::step`]) or by id as they start and finish elsewhere
//! ([`Progress::start`] / [`Progress::done`] / [`Progress::gave_up`], or
//! [`Progress::apply`] for a step the daemon reported).

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use shelbi_proto::control::{Step, StepState};

/// A step that finishes inside this never draws, so quick work stays silent.
/// Well under the ~300 ms at which a pause starts to read as a hang.
pub const GRACE: Duration = Duration::from_millis(150);
/// Spinner frame interval.
const FRAME: Duration = Duration::from_millis(80);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Width the step names are padded to, so the times line up.
const LABEL_WIDTH: usize = 32;
/// Opt-out for terminals where the spinner is unwanted.
pub const NO_PROGRESS_ENV: &str = "SHELBI_NO_PROGRESS";

/// Called with each finished step's label and duration, drawn or not (startup
/// logs its phases to `tui.log` this way).
pub type StepLog = fn(&str, Duration);

/// The progress printer (see the module docs). Cheap when disabled: steps still
/// run and log, nothing is drawn and no thread runs. Dropping it clears the
/// spinner.
pub struct Progress {
    shared: Option<Arc<Shared>>,
    ticker: Option<JoinHandle<()>>,
    log: Option<StepLog>,
    /// Ids for [`Progress::step`]'s anonymous steps.
    next_id: AtomicUsize,
}

struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
}

struct State {
    out: Box<dyn Write + Send>,
    title: String,
    /// The title is out, so every later step leaves a line.
    header: bool,
    /// Running steps by id, in start order.
    running: Vec<Live>,
    /// A prompt owns the terminal ([`StepHandle::suspend`]): don't draw.
    paused: bool,
    frame: usize,
    /// A spinner line is on screen (and must be cleared first).
    drawn: bool,
}

struct Live {
    id: String,
    label: String,
    started: Instant,
}

impl State {
    fn header(&mut self) {
        if !self.header {
            self.header = true;
            let _ = writeln!(self.out, "{}", self.title);
        }
    }

    fn clear_line(&mut self) {
        if self.drawn {
            let _ = self.out.write_all(b"\r\x1b[2K");
            self.drawn = false;
        }
        let _ = self.out.flush();
    }

    /// Draw the spinner line for the latest running step, once any running
    /// step has outlasted [`GRACE`].
    fn draw_spinner(&mut self) {
        if self.paused || !self.running.iter().any(|s| s.started.elapsed() >= GRACE) {
            return;
        }
        let Some(step) = self.running.last() else {
            return;
        };
        let glyph = SPINNER[self.frame % SPINNER.len()];
        let more = match self.running.len() {
            1 => String::new(),
            n => format!("  (+{} more)", n - 1),
        };
        let line = format!("  {glyph} {}{more}", columns(&step.label, step.started.elapsed()));
        self.frame += 1;
        self.header();
        self.clear_line();
        let _ = self.out.write_all(line.as_bytes());
        self.drawn = true;
        let _ = self.out.flush();
    }

    /// End step `id` with the finished `line`, printed when the list is
    /// showing (or `force`d, or the step itself outlasted [`GRACE`]).
    fn finish(&mut self, id: &str, elapsed: Duration, line: &str, force: bool) {
        self.running.retain(|s| s.id != id);
        if self.header || force || elapsed >= GRACE {
            self.header();
            self.clear_line();
            let _ = writeln!(self.out, "  {line}");
        }
        self.draw_spinner();
        let _ = self.out.flush();
    }
}

impl Progress {
    /// Draw on stderr under `title` when stderr is a terminal that wants it;
    /// otherwise disabled.
    pub fn stderr(title: &str) -> Self {
        let tty = std::io::stderr().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let opted_out = std::env::var_os(NO_PROGRESS_ENV).is_some_and(|v| !v.is_empty());
        if tty && !dumb && !opted_out {
            Self::to_writer(Box::new(std::io::stderr()), title)
        } else {
            Self::disabled()
        }
    }

    /// Run and log steps, draw nothing.
    pub fn disabled() -> Self {
        Self {
            shared: None,
            ticker: None,
            log: None,
            next_id: AtomicUsize::new(0),
        }
    }

    /// Draw to `out` under `title` (tests; [`Progress::stderr`] in production).
    pub fn to_writer(out: Box<dyn Write + Send>, title: &str) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                out,
                title: title.to_string(),
                header: false,
                running: Vec::new(),
                paused: false,
                frame: 0,
                drawn: false,
            }),
            stop: AtomicBool::new(false),
        });
        let ticker_shared = shared.clone();
        let ticker = std::thread::Builder::new()
            .name("shelbi-progress".into())
            .spawn(move || {
                while !ticker_shared.stop.load(Ordering::Acquire) {
                    std::thread::park_timeout(FRAME);
                    if ticker_shared.stop.load(Ordering::Acquire) {
                        break;
                    }
                    lock(&ticker_shared.state).draw_spinner();
                }
            })
            .ok();
        Self {
            shared: Some(shared),
            ticker,
            log: None,
            next_id: AtomicUsize::new(0),
        }
    }

    /// Also hand every finished step to `log`.
    pub fn with_log(mut self, log: StepLog) -> Self {
        self.log = Some(log);
        self
    }

    /// Whether anything has been drawn (the title is out).
    pub fn shown(&self) -> bool {
        self.shared.as_ref().is_some_and(|s| lock(&s.state).header)
    }

    /// Print a closing `line` under the list, if the list is showing.
    pub fn footer(&self, line: &str) {
        if let Some(shared) = &self.shared {
            let mut state = lock(&shared.state);
            if state.header {
                state.clear_line();
                let _ = writeln!(state.out, "{line}");
                let _ = state.out.flush();
            }
        }
    }

    /// Step `id` began, labelled `label` while it runs.
    pub fn start(&self, id: &str, label: &str) {
        if let Some(shared) = &self.shared {
            let mut state = lock(&shared.state);
            state.running.retain(|s| s.id != id);
            state.running.push(Live {
                id: id.to_string(),
                label: label.to_string(),
                started: Instant::now(),
            });
        }
    }

    /// Step `id` finished after `elapsed`; `label` is its finished wording.
    pub fn done(&self, id: &str, label: &str, elapsed: Duration, note: Option<&str>) {
        if let Some(log) = self.log {
            log(label, elapsed);
        }
        if let Some(shared) = &self.shared {
            let note = note.map(|n| format!("  ({n})")).unwrap_or_default();
            let line = format!("✓ {}{note}", columns(label, elapsed));
            lock(&shared.state).finish(id, elapsed, &line, false);
        }
    }

    /// Step `id` stopped waiting on `waiting_on` after `elapsed`. Always shown.
    pub fn gave_up(&self, id: &str, label: &str, elapsed: Duration, waiting_on: &str) {
        if let Some(log) = self.log {
            log(label, elapsed);
        }
        match &self.shared {
            Some(shared) => {
                let line = format!(
                    "✗ {}  gave up waiting on {waiting_on}",
                    columns(label, elapsed)
                );
                lock(&shared.state).finish(id, elapsed, &line, true);
            }
            None => eprintln!(
                "shelbi: {label}: gave up waiting on {waiting_on} after {}",
                seconds(elapsed)
            ),
        }
    }

    /// Apply a step the daemon reported.
    pub fn apply(&self, step: &Step) {
        match &step.state {
            StepState::Started => self.start(&step.id, &step.label),
            StepState::Done { elapsed_ms, note } => self.done(
                &step.id,
                &step.label,
                Duration::from_millis(*elapsed_ms),
                note.as_deref(),
            ),
            StepState::GaveUp {
                elapsed_ms,
                waiting_on,
            } => self.gave_up(
                &step.id,
                &step.label,
                Duration::from_millis(*elapsed_ms),
                waiting_on,
            ),
        }
    }

    /// Run `f` as the step `label`: timed and logged always, drawn as a live
    /// spinner line if it outlasts [`GRACE`]. The handle can relabel it, say
    /// what its finished line reads, give up on it, or lend the terminal to a
    /// prompt.
    pub fn step<T>(&self, label: &str, f: impl FnOnce(&StepHandle<'_>) -> T) -> T {
        let id = format!("step-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let started = Instant::now();
        self.start(&id, label);
        let handle = StepHandle {
            progress: self,
            id: &id,
            end: Mutex::new(End::Done(label.to_string())),
        };
        let out = f(&handle);
        let took = started.elapsed();
        match handle.end.into_inner().unwrap_or_else(|e| e.into_inner()) {
            End::Done(shown) => self.done(&id, &shown, took, None),
            End::GaveUp(shown, waiting_on) => self.gave_up(&id, &shown, took, &waiting_on),
        }
        out
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Ordering::Release);
        }
        if let Some(t) = self.ticker.take() {
            t.thread().unpark();
            let _ = t.join();
        }
        if let Some(shared) = &self.shared {
            lock(&shared.state).clear_line();
        }
    }
}

/// How a [`Progress::step`] ends.
enum End {
    /// `✓` with this label.
    Done(String),
    /// `✗` with this label, having given up on the second.
    GaveUp(String, String),
}

/// The running step, for updating what its line says and for handing the
/// terminal to a prompt.
pub struct StepHandle<'a> {
    progress: &'a Progress,
    id: &'a str,
    end: Mutex<End>,
}

impl StepHandle<'_> {
    /// Change the live line's text (e.g. a count, or the machine waited on).
    pub fn set_label(&self, label: &str) {
        if let Some(shared) = &self.progress.shared {
            let mut state = lock(&shared.state);
            if let Some(step) = state.running.iter_mut().find(|s| s.id == self.id) {
                step.label = label.to_string();
            }
        }
    }

    /// What the finished `✓` line says, if not the label it started with.
    pub fn finish_as(&self, label: &str) {
        *self.end.lock().unwrap_or_else(|e| e.into_inner()) = End::Done(label.to_string());
    }

    /// End as a `✗` line reading `label`, naming `waiting_on` as what it
    /// stopped waiting for.
    pub fn gave_up(&self, label: &str, waiting_on: &str) {
        *self.end.lock().unwrap_or_else(|e| e.into_inner()) =
            End::GaveUp(label.to_string(), waiting_on.to_string());
    }

    /// Clear the spinner and stop drawing while `f` owns the terminal (a
    /// `[y/N]` prompt), then resume.
    pub fn suspend<T>(&self, f: impl FnOnce() -> T) -> T {
        let Some(shared) = &self.progress.shared else {
            return f();
        };
        {
            let mut state = lock(&shared.state);
            state.clear_line();
            state.paused = true;
        }
        let out = f();
        lock(&shared.state).paused = false;
        out
    }
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `label` padded to the label column, then the elapsed time.
fn columns(label: &str, elapsed: Duration) -> String {
    format!("{label:<LABEL_WIDTH$} {}", seconds(elapsed))
}

/// An elapsed time for people: `0.3s`, `12.0s`.
pub fn seconds(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buf {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
        /// The lines a terminal would end up showing: each `\r\x1b[2K`
        /// erases back to the line start.
        fn screen(&self) -> Vec<String> {
            self.text()
                .split('\n')
                .map(|l| l.rsplit("\r\x1b[2K").next().unwrap_or("").to_string())
                .filter(|l| !l.is_empty())
                .collect()
        }
    }

    fn step(id: &str, label: &str, state: StepState) -> Step {
        Step {
            id: id.into(),
            label: label.into(),
            state,
        }
    }

    // --- Startup (`Starting Shelbi…`) ---

    #[test]
    fn a_fast_startup_prints_nothing() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()), "Starting Shelbi…");
        let v = progress.step("project demo", |_| 7);
        progress.step("workspaces", |_| ());
        assert!(!progress.shown());
        progress.footer("never printed");
        drop(progress);
        assert_eq!(v, 7);
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn a_slow_step_draws_a_spinner_then_a_timed_check() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()), "Starting Shelbi…");
        progress.step("remote machines (devbox)", |s| {
            std::thread::sleep(GRACE + FRAME * 3);
            s.finish_as("remote machines: devbox unreachable");
        });
        // Once the header is out, a quick later step still gets its line.
        progress.step("project demo", |_| ());
        drop(progress);
        let text = buf.text();
        assert!(text.starts_with("Starting Shelbi…\n"), "{text:?}");
        assert!(text.contains("⠋ remote machines (devbox)"), "{text:?}");
        assert!(text.contains("✓ remote machines: devbox unreachable"), "{text:?}");
        assert!(text.contains("✓ project demo"), "{text:?}");
        assert!(text.ends_with("s\n"), "{text:?}");
    }

    #[test]
    fn suspend_clears_the_spinner_and_draws_nothing_meanwhile() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()), "Starting Shelbi…");
        progress.step("remote machines", |s| {
            std::thread::sleep(GRACE + FRAME * 2);
            s.suspend(|| {
                let before = buf.text();
                assert!(before.ends_with("\r\x1b[2K"), "line cleared: {before:?}");
                std::thread::sleep(FRAME * 3);
                assert_eq!(buf.text(), before, "drew over the prompt");
            });
        });
    }

    #[test]
    fn disabled_draws_nothing_and_still_runs_and_logs_steps() {
        static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
        fn log(label: &str, _: Duration) {
            LOGGED.lock().unwrap().push(label.to_string());
        }
        let progress = Progress::disabled().with_log(log);
        let v = progress.step("slow", |s| {
            s.set_label("still slow");
            s.suspend(|| std::thread::sleep(GRACE + FRAME));
            3
        });
        assert_eq!(v, 3);
        assert!(!progress.shown());
        assert_eq!(*LOGGED.lock().unwrap(), vec!["slow".to_string()]);
    }

    // --- Quit (`Shutting down Shelbi…`), steps reported by the daemon ---

    #[test]
    fn daemon_steps_print_a_line_each_with_their_time_and_a_gave_up_names_its_wait() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()), "Shutting down Shelbi…");
        // Quick, before anything showed: silent, like a quick startup step.
        progress.apply(&step("jobs", "stopping background jobs", StepState::Started));
        progress.apply(&step(
            "jobs",
            "stopped background jobs",
            StepState::Done {
                elapsed_ms: 20,
                note: None,
            },
        ));
        progress.apply(&step("handoff", "asking orchestrator for handoff", StepState::Started));
        progress.apply(&step(
            "handoff",
            "asked orchestrator for handoff",
            StepState::Done {
                elapsed_ms: 1100,
                note: None,
            },
        ));
        progress.apply(&step("sessions", "stopping sessions", StepState::Started));
        progress.apply(&step(
            "sessions",
            "stopped 5 sessions",
            StepState::Done {
                elapsed_ms: 30,
                note: None,
            },
        ));
        progress.apply(&step("remote:devbox", "stopping sessions on devbox", StepState::Started));
        progress.apply(&step(
            "remote:devbox",
            "stopped sessions on devbox",
            StepState::GaveUp {
                elapsed_ms: 6000,
                waiting_on: "devbox".into(),
            },
        ));
        progress.footer("Done in 7.2s.");
        drop(progress);

        let screen = buf.screen();
        assert_eq!(screen[0], "Shutting down Shelbi…", "{screen:#?}");
        assert!(screen[1].starts_with("  ✓ asked orchestrator for handoff"), "{screen:#?}");
        assert!(screen[1].ends_with(" 1.1s"), "{screen:#?}");
        assert!(screen[2].starts_with("  ✓ stopped 5 sessions"), "{screen:#?}");
        assert!(screen[3].starts_with("  ✗ stopped sessions on devbox"), "{screen:#?}");
        assert!(screen[3].ends_with("6.0s  gave up waiting on devbox"), "{screen:#?}");
        assert_eq!(screen[4], "Done in 7.2s.");
        assert_eq!(screen.len(), 5, "{screen:#?}");
    }

    #[test]
    fn a_gave_up_shows_even_when_nothing_else_did() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()), "Shutting down Shelbi…");
        progress.step("waiting for daemon to exit", |s| {
            s.gave_up("stopped the daemon", "the hub daemon (pid 12)")
        });
        assert!(progress.shown());
        drop(progress);
        let screen = buf.screen();
        assert_eq!(screen[0], "Shutting down Shelbi…");
        assert!(
            screen[1].starts_with("  ✗ stopped the daemon")
                && screen[1].ends_with("gave up waiting on the hub daemon (pid 12)"),
            "{screen:#?}"
        );
    }

    #[test]
    fn the_spinner_names_the_latest_step_and_how_many_run_beside_it() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()), "Quitting demo…");
        progress.start("jobs", "stopping background jobs");
        progress.start("handoff", "asking orchestrator for handoff");
        std::thread::sleep(GRACE + FRAME * 3);
        let text = buf.text();
        assert!(
            text.contains("asking orchestrator for handoff") && text.contains("(+1 more)"),
            "{text:?}"
        );
        progress.done("handoff", "asked orchestrator for handoff", Duration::from_millis(900), None);
        drop(progress);
        let text = buf.text();
        assert!(text.contains("✓ asked orchestrator for handoff"), "{text:?}");
        assert!(text.ends_with("\r\x1b[2K"), "the spinner is erased at the end: {text:?}");
    }
}
