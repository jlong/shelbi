//! Live progress view for the destructive quit paths (Quit Project /
//! Quit Shelbi).
//!
//! Both quit paths used to run silently after the confirmation popover
//! closed: the popup blanked, sessions and panes vanished with no
//! indication of what was happening, and the long wait while each
//! orchestrator composed its `handoff.md` showed a bare, apparently-hung
//! window. This module replaces that with a spinner + running status line
//! that names each teardown step as it executes.
//!
//! ## How the guarantee is preserved
//!
//! Teardown historically ran via `tmux run-shell -b` so the kills survive
//! the popup process dying mid-teardown — the popup's pane lives inside a
//! `shelbi-<name>` session, and killing that session SIGHUPs this process.
//! A foreground progress UI is in tension with that: the thing rendering
//! progress is one of the things being killed.
//!
//! We resolve it by splitting the work:
//!
//! - **Enumerable steps run in the foreground** on a worker thread — the
//!   handoff wait, per-workspace pane kills, and the `_shelbi-<name>` stash
//!   kills. None of these touch the session the popup lives in, so they're
//!   safe to run synchronously while the main thread renders their progress.
//! - **The self-session kill runs detached** via `tmux run-shell -b` as the
//!   very last act, after the terminal is restored. That snippet also
//!   re-kills the stashes (idempotent) as a backstop, so if the UI is
//!   interrupted before it fires, a re-run — or the `-b` script itself —
//!   still leaves no stranded `shelbi-*` / `_shelbi-*` sessions.
//!
//! The confirmation popover and its dirty-workspace warnings are untouched;
//! this is strictly what happens *after* the user confirms.

use std::io::Write;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame, Terminal,
};

use shelbi_core::{MachineKind, Project, WorkspaceSpec};
use shelbi_orchestrator::handoff::HandoffOutcome;
use shelbi_orchestrator::workspace as orch_workspace;

/// Braille spinner frames, advanced once per render tick.
const SPINNER: [&str; 10] = [
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];
/// Render cadence. Slow enough to be cheap, fast enough that the spinner
/// reads as motion and the handoff elapsed-seconds hint stays current.
const FRAME: Duration = Duration::from_millis(120);
/// How long the final "done" frame lingers so completion is unambiguous
/// before the view closes (or the self-kill tears it down).
const DONE_HOLD: Duration = Duration::from_millis(750);

/// Lifecycle of one teardown step, driving both the glyph and the label
/// tint in the render.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    /// Not started yet — dim label, no glyph.
    Pending,
    /// In progress — spinner.
    Active,
    /// Completed successfully — green check.
    Done,
    /// Nothing to do (e.g. an orchestrator pane that wasn't running, or a
    /// workspace whose machine/addr couldn't be resolved).
    Skipped,
}

/// One orchestrator we've asked to write its `handoff.md`. `started` backs
/// the "(12s)" elapsed hint while the write is pending.
struct HandoffLine {
    project: String,
    state: Step,
    started: Instant,
}

/// A named teardown step (a workspace pane, the stash, or the main session).
struct NamedStep {
    label: String,
    state: Step,
}

/// Per-project teardown progress. For a single-project quit the model holds
/// exactly one of these.
struct ProjectProgress {
    name: String,
    state: Step,
    workspaces: Vec<NamedStep>,
    stash: NamedStep,
    main: NamedStep,
}

/// The full render model, mutated by the worker thread and read by the
/// render loop under one mutex.
struct Model {
    title: String,
    handoffs: Vec<HandoffLine>,
    projects: Vec<ProjectProgress>,
    /// True for Quit Shelbi (project headers + dividers), false for the
    /// single-project quit (flat workspace list).
    multi: bool,
    done: bool,
}

/// Cloneable handle the worker uses to report progress. All mutation goes
/// through the narrow setters below so an out-of-range index (a project
/// whose YAML changed mid-teardown) is ignored rather than panicking the
/// worker.
#[derive(Clone)]
struct Progress(Arc<Mutex<Model>>);

impl Progress {
    fn lock(&self) -> MutexGuard<'_, Model> {
        // A worker panic mid-mutation would poison the lock; recover the
        // inner model rather than cascading the panic into the render loop,
        // which still needs to restore the terminal and fire the backstop.
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_handoff(&self, i: usize, state: Step) {
        if let Some(h) = self.lock().handoffs.get_mut(i) {
            h.state = state;
        }
    }

    fn set_project(&self, pi: usize, state: Step) {
        if let Some(p) = self.lock().projects.get_mut(pi) {
            p.state = state;
        }
    }

    fn set_workspace(&self, pi: usize, wi: usize, state: Step) {
        if let Some(w) = self
            .lock()
            .projects
            .get_mut(pi)
            .and_then(|p| p.workspaces.get_mut(wi))
        {
            w.state = state;
        }
    }

    fn set_stash(&self, pi: usize, state: Step) {
        if let Some(p) = self.lock().projects.get_mut(pi) {
            p.stash.state = state;
        }
    }

    fn set_main(&self, pi: usize, state: Step) {
        if let Some(p) = self.lock().projects.get_mut(pi) {
            p.main.state = state;
        }
    }

    fn finish(&self) {
        self.lock().done = true;
    }
}

/// Marks the model done on drop, so the render loop always terminates — even
/// if the worker thread unwinds partway through. Without this, a panic in the
/// worker (which runs on its own thread and wouldn't abort the process) would
/// leave `done` false forever and hang the main thread's render loop.
struct FinishGuard(Progress);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

// ---------------------------------------------------------------------------
// Public entry points — called from the palette after the confirm popover.

/// Tear down one project with a live progress view. Restores the terminal
/// itself before firing the detached self-kill, so the caller must NOT
/// double-restore.
pub fn quit_project_with_progress<B>(term: &mut Terminal<B>, project: &str) -> Result<()>
where
    B: ratatui::backend::Backend + Write,
{
    let model = Progress(Arc::new(Mutex::new(Model {
        title: format!("Quitting \"{project}\"…"),
        handoffs: vec![HandoffLine {
            project: project.to_string(),
            state: Step::Active,
            started: Instant::now(),
        }],
        projects: vec![project_skeleton(project)],
        multi: false,
        done: false,
    })));

    let worker = {
        let model = model.clone();
        let project = project.to_string();
        thread::spawn(move || worker_quit_project(&model, &project))
    };
    render_until_done(term, &model)?;
    let _ = worker.join();
    restore(term)?;

    // Land the attached client on another project before the old session
    // dies, then hand the self-kill to `run-shell -b` so it survives this
    // process's imminent exit.
    if let Some(target) = super::quit_project::next_session_target(project) {
        let _ = super::run_tmux(["switch-client", "-t", &target]);
    }
    let script = super::quit_project::build_project_teardown_script(project);
    let _ = super::run_tmux(["run-shell", "-b", &script]);
    Ok(())
}

/// Tear down every shelbi session on this host with a live progress view,
/// iterating projects and their workspaces. Restores the terminal itself
/// before firing the detached backstop, so the caller must NOT
/// double-restore.
pub fn quit_shelbi_with_progress<B>(term: &mut Terminal<B>) -> Result<()>
where
    B: ratatui::backend::Backend + Write,
{
    let names = super::quit_shelbi::project_names();
    if names.is_empty() {
        // Nothing live — re-running `shelbi` right after a quit lands here.
        // Skip the whole show rather than flash an empty frame.
        return Ok(());
    }

    let handoffs = names
        .iter()
        .map(|name| HandoffLine {
            project: name.clone(),
            state: Step::Active,
            started: Instant::now(),
        })
        .collect();
    let projects = names.iter().map(|n| project_skeleton(n)).collect();
    let model = Progress(Arc::new(Mutex::new(Model {
        title: "Quitting Shelbi…".to_string(),
        handoffs,
        projects,
        multi: true,
        done: false,
    })));

    let worker = {
        let model = model.clone();
        let names = names.clone();
        thread::spawn(move || worker_quit_shelbi(&model, &names))
    };
    render_until_done(term, &model)?;
    let _ = worker.join();
    restore(term)?;

    // Backstop: re-kill the stashes (idempotent) plus every main session and
    // a `detach-client` flush, detached so it outlives this process getting
    // SIGHUP'd when its own host session is killed.
    let script = super::quit_shelbi::build_local_teardown_script(&names);
    if !script.is_empty() {
        let _ = super::run_tmux(["run-shell", "-b", &script]);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Worker threads — the actual teardown, reporting progress as it goes.

fn worker_quit_project(progress: &Progress, project: &str) {
    let _finish = FinishGuard(progress.clone());
    request_handoff(progress, 0, project);

    progress.set_project(0, Step::Active);
    teardown_project(progress, 0, project, "user:quit-project");
    progress.set_project(0, Step::Done);
}

fn worker_quit_shelbi(progress: &Progress, names: &[String]) {
    let _finish = FinishGuard(progress.clone());
    // Fan the handoff requests out so multiple orchestrators write in
    // parallel. Each is bounded inside `request_orchestrator_handoff` (a
    // capped verified-submit window plus the 30s file-poll timeout), so the
    // worst-case wait stays bounded regardless of how many projects are live —
    // and, crucially, the per-project tmux paste is now staged through a
    // per-invocation-unique buffer, so parallel handoffs no longer race on one
    // shared buffer. The render loop shows each line's spinner + elapsed hint
    // while its thread is in flight.
    let handoff_threads: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let progress = progress.clone();
            let name = name.clone();
            thread::spawn(move || request_handoff(&progress, i, &name))
        })
        .collect();
    for t in handoff_threads {
        let _ = t.join();
    }

    for (pi, name) in names.iter().enumerate() {
        progress.set_project(pi, Step::Active);
        teardown_project(progress, pi, name, "user:quit-shelbi");
        progress.set_project(pi, Step::Done);
    }
}

/// Ask one orchestrator to write its handoff, flipping its progress line to
/// the outcome once the (blocking) request returns.
fn request_handoff(progress: &Progress, i: usize, project: &str) {
    let outcome = shelbi_orchestrator::handoff::request_orchestrator_handoff(project);
    progress.set_handoff(i, handoff_step(&outcome));
}

/// A written or native-thread handoff is a real save; every other outcome
/// (no pane, timeout, send failed) is "nothing captured" — shown as skipped
/// rather than an error, matching the best-effort semantics the caller has
/// always had.
fn handoff_step(outcome: &shelbi_core::Result<HandoffOutcome>) -> Step {
    match outcome {
        Ok(HandoffOutcome::Written { .. }) | Ok(HandoffOutcome::NativeThread) => Step::Done,
        _ => Step::Skipped,
    }
}

/// The enumerable, foreground-safe teardown for one project: clear the zen
/// crash heartbeat, kill each workspace pane, kill the `_shelbi-<name>`
/// stash, and append the close event. The main session is left for the
/// detached backstop — but marked Done in the model since it's killed within
/// the same breath, right after the "done" frame shows.
fn teardown_project(progress: &Progress, pi: usize, project: &str, reason: &str) {
    let _ = shelbi_state::zen_clear_crash(project);

    if let Ok(p) = shelbi_state::load_project(project) {
        for (wi, workspace) in p.workspaces.iter().enumerate() {
            progress.set_workspace(pi, wi, Step::Active);
            progress.set_workspace(pi, wi, kill_one_workspace(&p, workspace));
        }

        // Remove the Shelbi-managed commit guard from the hub checkout so
        // nothing Shelbi installed lingers after the user tears the project
        // down — the trust fix requires it not persist past use. Best-effort,
        // and a user-authored hook is never touched (SkippedForeignHook).
        if let Some(hub) = p.machines.iter().find(|m| matches!(m.kind, MachineKind::Local)) {
            let _ = shelbi_orchestrator::githook::uninstall_hub_branch_guard(&hub.work_dir);
        }
    }

    // The stash never hosts the popup, so killing it in the foreground is
    // safe and gives the progress view a concrete step. The backstop re-kills
    // it (idempotent) if the UI is interrupted before we get here.
    progress.set_stash(pi, Step::Active);
    let _ = super::run_tmux(["kill-session", "-t", &format!("_shelbi-{project}")]);
    progress.set_stash(pi, Step::Done);

    // The main session is the one the popup lives in; its actual kill is
    // deferred to the detached backstop. Mark it Done so the completed view
    // reads cleanly — the `-b` script runs within the second.
    progress.set_main(pi, Step::Done);

    let _ = shelbi_state::append_project_event(project, "closed", reason);
}

/// Kill one workspace pane (local window or remote SSH session), returning
/// the step outcome. A workspace whose machine or tmux addr can't be
/// resolved is Skipped rather than blocking the rest of the teardown; a
/// resolved one is Done even if the SSH kill errored (an unreachable host is
/// best-effort, same as the pre-progress code).
fn kill_one_workspace(project: &Project, workspace: &WorkspaceSpec) -> Step {
    let Some(machine) = project.machine(&workspace.machine) else {
        return Step::Skipped;
    };
    let host = machine.host();
    let Ok(addr) = orch_workspace::workspace_tmux_addr(project, workspace) else {
        return Step::Skipped;
    };
    let _ = orch_workspace::kill_workspace_pane(&host, &addr, &workspace.name);
    Step::Done
}

/// Seed a project's skeleton (all steps Pending) from its declared
/// workspaces so the render has a stable layout to flip in place. A project
/// whose YAML fails to load still gets its stash/main rows — the session is
/// real and still torn down.
fn project_skeleton(name: &str) -> ProjectProgress {
    let workspaces = shelbi_state::load_project(name)
        .map(|p| {
            p.workspaces
                .iter()
                .map(|w| NamedStep {
                    label: w.name.clone(),
                    state: Step::Pending,
                })
                .collect()
        })
        .unwrap_or_default();
    ProjectProgress {
        name: name.to_string(),
        state: Step::Pending,
        workspaces,
        stash: NamedStep {
            label: "stash sessions".to_string(),
            state: Step::Pending,
        },
        main: NamedStep {
            label: "main session".to_string(),
            state: Step::Pending,
        },
    }
}

// ---------------------------------------------------------------------------
// Render loop.

/// Redraw the progress view on each frame until the worker signals done,
/// then hold the completed frame briefly so the user registers it.
fn render_until_done<B>(term: &mut Terminal<B>, progress: &Progress) -> Result<()>
where
    B: ratatui::backend::Backend + Write,
{
    let mut frame = 0usize;
    loop {
        let done = {
            let model = progress.lock();
            term.draw(|f| render(f, &model, frame))?;
            model.done
        };
        if done {
            break;
        }
        thread::sleep(FRAME);
        frame = frame.wrapping_add(1);
    }
    thread::sleep(DONE_HOLD);
    Ok(())
}

fn render(f: &mut Frame, model: &Model, frame: usize) {
    let area = inset(f.area());
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        model.title.clone(),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::raw(""));

    // While any orchestrator is still writing, the handoff phase owns the
    // view — this is the wait that used to show a blank window.
    let handoff_pending = model.handoffs.iter().any(|h| h.state == Step::Active);
    if handoff_pending {
        render_handoff(&mut lines, model, frame);
    } else {
        render_teardown(&mut lines, model, frame);
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        status_line(model, handoff_pending),
        Style::default().fg(Color::DarkGray),
    )));

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_handoff(lines: &mut Vec<Line>, model: &Model, frame: usize) {
    lines.push(header_line(
        Step::Active,
        "Saving handoff notes".to_string(),
        frame,
    ));
    for h in &model.handoffs {
        let who = if model.multi {
            format!("{} orchestrator", h.project)
        } else {
            "orchestrator".to_string()
        };
        let body = match h.state {
            Step::Active => {
                format!("{who} writing handoff.md…   ({}s)", h.started.elapsed().as_secs())
            }
            Step::Done => format!("{who} handed off"),
            Step::Skipped => format!("{who} — no handoff to save"),
            Step::Pending => format!("{who} …"),
        };
        lines.push(step_line(5, body, h.state, frame));
    }
}

fn render_teardown(lines: &mut Vec<Line>, model: &Model, frame: usize) {
    let indent = if model.multi { 5 } else { 0 };
    let last = model.projects.len().saturating_sub(1);
    for (pi, p) in model.projects.iter().enumerate() {
        if model.multi {
            let verb = if p.state == Step::Done {
                "Closed"
            } else {
                "Closing"
            };
            lines.push(header_line(
                p.state,
                format!("{verb} project \"{}\"", p.name),
                frame,
            ));
        }
        for w in &p.workspaces {
            lines.push(step_line(
                indent,
                format!("workspace {}", w.label),
                w.state,
                frame,
            ));
        }
        lines.push(step_line(indent, p.stash.label.clone(), p.stash.state, frame));
        lines.push(step_line(indent, p.main.label.clone(), p.main.state, frame));
        if model.multi && pi < last {
            lines.push(Line::from(Span::styled(
                "─".repeat(32),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
}

/// Bottom status line — the "you are here" summary under the step list.
fn status_line(model: &Model, handoff_pending: bool) -> String {
    if model.done {
        return if model.multi {
            let n = model.projects.len();
            format!("Closed {n} project{}.", plural(n))
        } else {
            "Closed.".to_string()
        };
    }
    if handoff_pending {
        return "Waiting for agents to hand off before teardown…".to_string();
    }
    if model.multi {
        let closed = model
            .projects
            .iter()
            .filter(|p| p.state == Step::Done)
            .count();
        format!("{closed} of {} projects closed", model.projects.len())
    } else {
        let n = model.projects.first().map(|p| p.workspaces.len()).unwrap_or(0);
        format!("Tearing down {n} workspace{}…", plural(n))
    }
}

/// A top-level line ("Saving handoff notes", "Closing project …") — glyph
/// then two spaces then the label, no indent.
fn header_line(state: Step, label: String, frame: usize) -> Line<'static> {
    let (g, c) = glyph(state, frame);
    Line::from(vec![
        Span::styled(format!("{g}  "), Style::default().fg(c)),
        Span::styled(label, Style::default().fg(Color::White)),
    ])
}

/// An indented step line — glyph then the label, tinted by state.
fn step_line(indent: usize, label: String, state: Step, frame: usize) -> Line<'static> {
    let (g, c) = glyph(state, frame);
    let label_color = if state == Step::Pending {
        Color::DarkGray
    } else {
        Color::Gray
    };
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(format!("{g} "), Style::default().fg(c)),
        Span::styled(label, Style::default().fg(label_color)),
    ])
}

/// Glyph + color for a step state. Active spins; done is a green check;
/// skipped is a dim mid-dot; pending is blank (label carries the meaning).
fn glyph(state: Step, frame: usize) -> (String, Color) {
    match state {
        Step::Active => (SPINNER[frame % SPINNER.len()].to_string(), Color::Cyan),
        Step::Done => ("✓".to_string(), Color::Green),
        Step::Skipped => ("·".to_string(), Color::DarkGray),
        Step::Pending => (" ".to_string(), Color::DarkGray),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Inset the popup surface by a small margin so the view doesn't sit flush
/// against the popup border.
fn inset(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(2),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(1),
    }
}

/// Leave raw mode + the alt-screen and show the cursor. Mirrors the
/// palette's own `restore_terminal`; kept local so the progress path owns
/// the exact moment the terminal is handed back (right before the detached
/// self-kill fires).
fn restore<B>(term: &mut Terminal<B>) -> Result<()>
where
    B: ratatui::backend::Backend + Write,
{
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(multi: bool, projects: Vec<ProjectProgress>, handoffs: Vec<HandoffLine>) -> Model {
        Model {
            title: "t".into(),
            handoffs,
            projects,
            multi,
            done: false,
        }
    }

    fn handoff(state: Step) -> HandoffLine {
        HandoffLine {
            project: "alpha".into(),
            state,
            started: Instant::now(),
        }
    }

    fn proj(name: &str, state: Step, ws: usize) -> ProjectProgress {
        ProjectProgress {
            name: name.into(),
            state,
            workspaces: (0..ws)
                .map(|i| NamedStep {
                    label: format!("w{i}"),
                    state: Step::Pending,
                })
                .collect(),
            stash: NamedStep {
                label: "stash sessions".into(),
                state: Step::Pending,
            },
            main: NamedStep {
                label: "main session".into(),
                state: Step::Pending,
            },
        }
    }

    #[test]
    fn glyph_spins_only_while_active() {
        // Active advances with the frame; the terminal states are stable.
        assert_ne!(glyph(Step::Active, 0).0, glyph(Step::Active, 1).0);
        assert_eq!(glyph(Step::Done, 0).0, glyph(Step::Done, 99).0);
        assert_eq!(glyph(Step::Done, 0).0, "✓");
        assert_eq!(glyph(Step::Pending, 0).0, " ");
    }

    #[test]
    fn handoff_outcome_maps_written_and_native_to_done() {
        assert_eq!(
            handoff_step(&Ok(HandoffOutcome::Written {
                path: "/tmp/h.md".into()
            })),
            Step::Done
        );
        assert_eq!(handoff_step(&Ok(HandoffOutcome::NativeThread)), Step::Done);
        // Every "nothing captured" variant degrades to Skipped, not an error.
        assert_eq!(handoff_step(&Ok(HandoffOutcome::PaneNotAlive)), Step::Skipped);
        assert_eq!(handoff_step(&Ok(HandoffOutcome::Timeout)), Step::Skipped);
    }

    #[test]
    fn status_line_names_the_handoff_wait() {
        let m = model(false, vec![proj("alpha", Step::Pending, 2)], vec![handoff(Step::Active)]);
        assert_eq!(
            status_line(&m, true),
            "Waiting for agents to hand off before teardown…"
        );
    }

    #[test]
    fn status_line_counts_workspaces_for_single_project() {
        let m = model(false, vec![proj("alpha", Step::Active, 3)], vec![handoff(Step::Done)]);
        assert_eq!(status_line(&m, false), "Tearing down 3 workspaces…");
        let one = model(false, vec![proj("alpha", Step::Active, 1)], vec![handoff(Step::Done)]);
        assert_eq!(status_line(&one, false), "Tearing down 1 workspace…");
    }

    #[test]
    fn status_line_counts_closed_projects_for_quit_shelbi() {
        let m = model(
            true,
            vec![
                proj("alpha", Step::Done, 0),
                proj("bravo", Step::Active, 0),
                proj("charlie", Step::Pending, 0),
            ],
            vec![handoff(Step::Done)],
        );
        assert_eq!(status_line(&m, false), "1 of 3 projects closed");
    }

    #[test]
    fn status_line_reports_completion_when_done() {
        let mut m = model(true, vec![proj("alpha", Step::Done, 0)], vec![handoff(Step::Done)]);
        m.done = true;
        assert_eq!(status_line(&m, false), "Closed 1 project.");

        let mut single = model(false, vec![proj("alpha", Step::Done, 0)], vec![handoff(Step::Done)]);
        single.done = true;
        assert_eq!(status_line(&single, false), "Closed.");
    }

    #[test]
    fn setters_ignore_out_of_range_indices() {
        // A project whose YAML changed mid-teardown must not panic the worker.
        let p = Progress(Arc::new(Mutex::new(model(
            false,
            vec![proj("alpha", Step::Pending, 1)],
            vec![handoff(Step::Active)],
        ))));
        p.set_workspace(9, 9, Step::Done);
        p.set_project(9, Step::Done);
        p.set_stash(9, Step::Done);
        p.set_main(9, Step::Done);
        p.set_handoff(9, Step::Done);
        // The real slots still update.
        p.set_workspace(0, 0, Step::Done);
        assert_eq!(p.lock().projects[0].workspaces[0].state, Step::Done);
    }
}
