//! Startup timing and the pre-UI progress lines (`cli-startup-speed`).
//!
//! `shelbi` does a little work on the plain terminal before the shell takes the
//! screen (the cutover open gate, the migration pass and its consent prompt).
//! Each piece runs as a named [`Progress::step`]:
//!
//! - every step logs its duration to `tui.log` (`startup phase=… ms=…`), so a
//!   slow open can be broken down after the fact, TTY or not;
//! - on a terminal, a step still running after [`GRACE`] draws a live spinner
//!   line naming it, and every step from then on leaves a `✓ name  0.4s` line,
//!   under a `Starting Shelbi…` header. A startup where every step is quick
//!   prints nothing at all;
//! - off a terminal (stderr redirected, `TERM=dumb`, `SHELBI_NO_PROGRESS` set)
//!   nothing is drawn, so scripts and logs never see spinner noise.
//!
//! The phases after the shell is up (first frame, daemon, orchestrator) log
//! through [`log_phase`] against the same process clock ([`mark_launch`]).

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A step that finishes inside this never draws, so a fast startup stays
/// silent. Well under the ~300 ms at which a pause starts to read as a hang.
pub const GRACE: Duration = Duration::from_millis(150);
/// Spinner frame interval.
const FRAME: Duration = Duration::from_millis(80);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Width the step names are padded to, so the times line up.
const LABEL_WIDTH: usize = 22;
/// Opt-out for terminals where the spinner is unwanted.
pub const NO_PROGRESS_ENV: &str = "SHELBI_NO_PROGRESS";

static LAUNCHED: OnceLock<Instant> = OnceLock::new();

/// Record the process's launch instant. Called first thing in `main`; later
/// calls are no-ops. [`since_launch`] falls back to the first call to it.
pub fn mark_launch() {
    let _ = LAUNCHED.set(Instant::now());
}

/// Time since [`mark_launch`].
pub fn since_launch() -> Duration {
    LAUNCHED.get_or_init(Instant::now).elapsed()
}

/// Log one startup phase's duration and the time since launch to `tui.log`.
pub fn log_phase(phase: &str, took: Duration) {
    tracing::info!(
        phase,
        ms = took.as_millis() as u64,
        at_ms = since_launch().as_millis() as u64,
        "startup"
    );
}

/// The pre-UI progress reporter. Cheap when disabled: steps still time and
/// log, nothing is drawn and no thread runs.
pub struct Progress {
    shared: Option<Arc<Shared>>,
    ticker: Option<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
}

struct State {
    out: Box<dyn Write + Send>,
    /// The `Starting Shelbi…` header is out, so every later step leaves a line.
    header: bool,
    step: Option<Live>,
    /// A prompt owns the terminal ([`StepHandle::suspend`]): don't draw.
    paused: bool,
    frame: usize,
}

struct Live {
    label: String,
    started: Instant,
    drawn: bool,
}

impl State {
    fn draw_spinner(&mut self) {
        let Some(step) = self.step.as_mut() else {
            return;
        };
        if self.paused || step.started.elapsed() < GRACE {
            return;
        }
        let glyph = SPINNER[self.frame % SPINNER.len()];
        self.frame += 1;
        let line = format!("\r\x1b[2K  {glyph} {}", step.label);
        step.drawn = true;
        if !self.header {
            self.header = true;
            let _ = self.out.write_all("Starting Shelbi…\n".as_bytes());
        }
        let _ = self.out.write_all(line.as_bytes());
        let _ = self.out.flush();
    }

    fn clear_line(&mut self) {
        if self.step.as_ref().is_some_and(|s| s.drawn) {
            let _ = self.out.write_all(b"\r\x1b[2K");
            let _ = self.out.flush();
        }
    }
}

impl Progress {
    /// Draw on stderr when it is a terminal that wants it; otherwise disabled.
    pub fn stderr() -> Self {
        let tty = std::io::stderr().is_terminal();
        let dumb = std::env::var("TERM").is_ok_and(|t| t == "dumb");
        let opted_out = std::env::var_os(NO_PROGRESS_ENV).is_some_and(|v| !v.is_empty());
        if tty && !dumb && !opted_out {
            Self::to_writer(Box::new(std::io::stderr()))
        } else {
            Self::disabled()
        }
    }

    /// Time and log steps, draw nothing.
    pub fn disabled() -> Self {
        Self {
            shared: None,
            ticker: None,
        }
    }

    /// Draw to `out` (tests; [`Progress::stderr`] in production).
    pub fn to_writer(out: Box<dyn Write + Send>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                out,
                header: false,
                step: None,
                paused: false,
                frame: 0,
            }),
            stop: AtomicBool::new(false),
        });
        let ticker_shared = shared.clone();
        let ticker = std::thread::Builder::new()
            .name("shelbi-startup-progress".into())
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
        }
    }

    /// Run `f` as the step `label`: timed and logged always, drawn as a live
    /// spinner line if it outlasts [`GRACE`].
    pub fn step<T>(&self, label: &str, f: impl FnOnce(&StepHandle<'_>) -> T) -> T {
        let started = Instant::now();
        if let Some(shared) = &self.shared {
            lock(&shared.state).step = Some(Live {
                label: label.to_string(),
                started,
                drawn: false,
            });
        }
        let handle = StepHandle {
            progress: self,
            done: Mutex::new(None),
        };
        let out = f(&handle);
        let took = started.elapsed();
        log_phase(label, took);
        if let Some(shared) = &self.shared {
            let mut state = lock(&shared.state);
            let live = state.step.take();
            if state.header {
                let shown = handle
                    .done
                    .into_inner()
                    .unwrap_or_else(|e| e.into_inner())
                    .or_else(|| live.map(|l| l.label))
                    .unwrap_or_else(|| label.to_string());
                let line = format!(
                    "\r\x1b[2K  ✓ {shown:<LABEL_WIDTH$} {:.1}s\n",
                    took.as_secs_f64()
                );
                let _ = state.out.write_all(line.as_bytes());
                let _ = state.out.flush();
            }
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
    }
}

/// The running step, for updating what its line says and for handing the
/// terminal to a prompt.
pub struct StepHandle<'a> {
    progress: &'a Progress,
    done: Mutex<Option<String>>,
}

impl StepHandle<'_> {
    /// Change the live line's text (e.g. a count, or the machine waited on).
    pub fn set_label(&self, label: &str) {
        if let Some(shared) = &self.progress.shared {
            if let Some(step) = lock(&shared.state).step.as_mut() {
                step.label = label.to_string();
            }
        }
    }

    /// What the finished `✓` line says, if not the live label.
    pub fn finish_as(&self, label: &str) {
        *self.done.lock().unwrap_or_else(|e| e.into_inner()) = Some(label.to_string());
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
    }

    #[test]
    fn a_fast_startup_prints_nothing() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()));
        let v = progress.step("project demo", |_| 7);
        progress.step("workspaces", |_| ());
        drop(progress);
        assert_eq!(v, 7);
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn a_slow_step_draws_a_spinner_then_a_timed_check() {
        let buf = Buf::default();
        let progress = Progress::to_writer(Box::new(buf.clone()));
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
        let progress = Progress::to_writer(Box::new(buf.clone()));
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
    fn disabled_draws_nothing_and_still_runs_steps() {
        let progress = Progress::disabled();
        let v = progress.step("slow", |s| {
            s.set_label("still slow");
            s.suspend(|| std::thread::sleep(GRACE + FRAME));
            3
        });
        assert_eq!(v, 3);
    }
}
