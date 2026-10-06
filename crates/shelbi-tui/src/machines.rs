//! The machines view.
//!
//! Each machine declared by the project with its workspaces, each workspace's
//! observed state and current task, and — where [`rt-machine-setup`] has landed —
//! the resolved remote `shelbi` path and version. It replaces the tmux runtime's
//! `while true; do shelbi workspace list; sleep 5; done` shell loop with a real
//! ratatui view driven by the same background refresh the other native views use.
//!
//! Like [`crate::kanban`] and [`crate::activity`], this exposes one
//! [`render_full`] used by **both** runtimes: the standalone [`crate::run_machines`]
//! process (the tmux runtime) and the single-process TUI shell's native view share
//! this one implementation. The read/apply split ([`MachinesApp::read_data`] /
//! [`MachinesApp::apply_data`]) lets the shell do the IO on its background worker
//! and fold the result in on the UI thread, so a slow read never blocks the loop.
//!
//! Observed workspace state comes from the hub poller's persisted
//! [`shelbi_state::WorkspaceStatus`] (no live SSH probe on the render path); remote
//! reachability is surfaced at the durability level `rt-machine-setup` provides —
//! the recorded binary path/version, or a prompt to run `shelbi machine setup`.
//!
//! [`rt-machine-setup`]: shelbi_state::machine_state

use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use shelbi_core::{Host, Machine, MachineKind};
use shelbi_orchestrator::machine::{probe_reachability, Reachability, SshExec};
use shelbi_state::machine_state::MachineRecord;
use shelbi_state::WorkspaceState;

use crate::reachability::{ReachabilityProber, REACHABILITY_BACKOFF_MAX, REACHABILITY_CADENCE};
use crate::theme;

/// Wall-clock bound on one reachability probe. Short so a wedged host fails fast
/// into [`Reachability::Unreachable`] rather than holding the prober thread for
/// the install-sized default `SshExec` deadline.
const REACHABILITY_PROBE_DEADLINE: Duration = Duration::from_secs(10);

/// One machine and the workspaces that live on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineEntry {
    pub name: String,
    pub kind: MachineKind,
    /// SSH host, for a remote machine.
    pub host: Option<String>,
    pub is_local: bool,
    /// Effective capability tags declared on the machine.
    pub tags: Vec<String>,
    /// The resolved remote binary (path + version), when `rt-machine-setup` has
    /// run for this machine. `None` for a local machine or a remote that has not
    /// been set up yet.
    pub remote: Option<MachineRecord>,
    /// Live reachability of a remote, published by the background prober and
    /// folded in on refresh. A local machine is always reachable and never
    /// probed, so this stays [`Reachability::Reachable`] for it and is not shown.
    pub reachability: Reachability,
    pub workspaces: Vec<WorkspaceRow>,
}

/// A workspace row under a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub name: String,
    /// The poller's last observed state, or `None` when the workspace has no
    /// status file yet (idle / never dispatched).
    pub state: Option<WorkspaceState>,
    /// The task id the workspace is currently working, if any.
    pub current_task: Option<String>,
}

/// The data one refresh reads. Produced by [`MachinesApp::read_data`] (off the UI
/// thread in the shell) and folded in by [`MachinesApp::apply_data`].
#[derive(Debug, Clone, Default)]
pub struct MachinesData {
    pub display_name: Option<String>,
    pub machines: Vec<MachineEntry>,
}

/// The machines view's state: the machine/workspace rows plus the selection the
/// user navigates. Selection addresses workspace rows only (machine headers are
/// not selectable); [`selected_workspace`](Self::selected_workspace) is what the
/// host opens on Enter.
pub struct MachinesApp {
    pub project_name: String,
    display_name: Option<String>,
    machines: Vec<MachineEntry>,
    /// Flattened `(machine_idx, workspace_idx)` for every selectable workspace
    /// row, in render order. Rebuilt on every [`apply_data`](Self::apply_data).
    selectable: Vec<(usize, usize)>,
    selected: usize,
    status_line: String,
    last_refresh: Instant,
    /// Background reachability prober, when live-reachability is enabled
    /// ([`enable_reachability`](Self::enable_reachability)). `None` keeps the
    /// view entirely probe-free (unit tests, and any caller that doesn't want
    /// SSH traffic).
    prober: Option<ReachabilityProber>,
    /// Set by the standalone process's quit chord; the shell ignores it (its
    /// own event loop owns quit).
    pub should_quit: bool,
}

impl MachinesApp {
    pub fn new(project_name: impl Into<String>) -> Self {
        MachinesApp {
            project_name: project_name.into(),
            display_name: None,
            machines: Vec::new(),
            selectable: Vec::new(),
            selected: 0,
            status_line: String::new(),
            last_refresh: Instant::now(),
            prober: None,
            should_quit: false,
        }
    }

    /// Turn on live reachability probing with the real SSH probe. Both runtimes
    /// (the standalone `__machines` process and the in-process shell) call this
    /// once, right after construction; from then on every refresh folds the
    /// prober's latest results into the remote machines' headers. The probe runs
    /// on the prober's own thread, never the UI thread or the shell's refresh
    /// worker.
    pub fn enable_reachability(&mut self) {
        self.enable_reachability_with(
            REACHABILITY_CADENCE,
            REACHABILITY_BACKOFF_MAX,
            real_reachability_probe,
        );
    }

    /// Enable reachability with an explicit cadence/backoff and an injected probe
    /// — the seam the tests drive with a deterministic stub instead of real SSH.
    pub fn enable_reachability_with<F>(&mut self, cadence: Duration, backoff_max: Duration, probe: F)
    where
        F: Fn(&str, &str) -> Reachability + Send + Sync + 'static,
    {
        self.prober = Some(ReachabilityProber::spawn(cadence, backoff_max, probe));
        // Seed targets from whatever we already hold so a probe starts without
        // waiting for the next refresh.
        self.push_prober_targets();
    }

    /// Refresh at most every 500ms — the standalone loop's cadence, matching the
    /// old `workspace list` shell loop's feel without a per-tick disk sweep.
    pub fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= std::time::Duration::from_millis(500) {
            self.refresh();
        }
    }

    /// The display label for the title bar (project display name or slug).
    pub fn display_label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.project_name)
    }

    pub fn machines(&self) -> &[MachineEntry] {
        &self.machines
    }

    /// Read the machines/workspaces/records off disk. Pure IO: safe to call on a
    /// background worker. The hub poller's persisted workspace status supplies the
    /// observed state (no live SSH probe here), so this is cheap and never blocks
    /// on the network.
    pub fn read_data(project_name: &str) -> MachinesData {
        let Ok(project) = shelbi_state::load_project(project_name) else {
            // A project whose YAML can't be read shows an empty machine list
            // rather than failing the refresh — the status line carries the why.
            return MachinesData::default();
        };
        let records = shelbi_state::machine_state::load_machine_records();

        let machines = project
            .machines
            .iter()
            .map(|m: &Machine| {
                let is_local = m.kind == MachineKind::Local;
                let remote = if is_local {
                    None
                } else {
                    records.get(&m.name).cloned()
                };
                let workspaces = project
                    .workspaces
                    .iter()
                    .filter(|w| w.machine == m.name)
                    .map(|w| {
                        let status = shelbi_state::load_workspace_status(&w.name)
                            .ok()
                            .flatten();
                        WorkspaceRow {
                            name: w.name.clone(),
                            state: status.as_ref().map(|s| s.state),
                            current_task: status.and_then(|s| s.current_task),
                        }
                    })
                    .collect();
                MachineEntry {
                    name: m.name.clone(),
                    kind: m.kind,
                    host: m.host.clone(),
                    is_local,
                    tags: m.tags.clone(),
                    remote,
                    // A local machine is the hub itself — always reachable, never
                    // probed. A remote starts Unknown ("checking") until the
                    // background prober's first result folds in on a later refresh.
                    reachability: if is_local {
                        Reachability::Reachable
                    } else {
                        Reachability::Unknown
                    },
                    workspaces,
                }
            })
            .collect();

        MachinesData {
            display_name: project.display_name.clone().or_else(|| project.label.clone()),
            machines,
        }
    }

    /// Fold a freshly-read [`MachinesData`] in, rebuilding the selectable row list
    /// and clamping the selection so it never points past the last workspace.
    pub fn apply_data(&mut self, data: MachinesData) {
        self.display_name = data.display_name;
        self.machines = data.machines;
        self.rebuild_selectable();
        // Keep the prober's target set current (cheap, a no-op when unchanged),
        // then fold its latest results into the remote headers. Both touch only
        // in-memory state — no SSH on this path.
        self.push_prober_targets();
        self.fold_reachability();
        self.last_refresh = Instant::now();
    }

    /// Hand the prober the current set of remote `(machine, ssh_host)` targets.
    /// Local machines are deliberately excluded, so they are never probed and
    /// generate no SSH traffic.
    fn push_prober_targets(&self) {
        let Some(prober) = &self.prober else {
            return;
        };
        let targets = self
            .machines
            .iter()
            .filter(|m| !m.is_local)
            .map(|m| {
                // Mirror `Machine::host()`: an ssh machine with no explicit host
                // is reached by its name.
                let host = m.host.clone().unwrap_or_else(|| m.name.clone());
                (m.name.clone(), host)
            })
            .collect();
        prober.set_targets(targets);
    }

    /// Overlay the prober's latest reachability onto the remote machines. A
    /// remote with no result yet keeps its `Unknown` ("checking") state.
    fn fold_reachability(&mut self) {
        let Some(prober) = &self.prober else {
            return;
        };
        let snapshot = prober.snapshot();
        for m in &mut self.machines {
            if m.is_local {
                continue;
            }
            if let Some(r) = snapshot.get(&m.name) {
                m.reachability = r.clone();
            }
        }
    }

    /// Refresh in place (read + apply). Used by the standalone process, whose own
    /// loop may block on the read; the shell drives [`read_data`](Self::read_data)
    /// / [`apply_data`](Self::apply_data) off the UI thread instead.
    pub fn refresh(&mut self) {
        let data = Self::read_data(&self.project_name);
        self.apply_data(data);
    }

    fn rebuild_selectable(&mut self) {
        self.selectable.clear();
        for (mi, m) in self.machines.iter().enumerate() {
            for wi in 0..m.workspaces.len() {
                self.selectable.push((mi, wi));
            }
        }
        let max = self.selectable.len().saturating_sub(1);
        if self.selected > max {
            self.selected = max;
        }
    }

    pub fn selectable_count(&self) -> usize {
        self.selectable.len()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn nav_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn nav_down(&mut self) {
        let max = self.selectable.len().saturating_sub(1);
        self.selected = (self.selected + 1).min(max);
    }

    /// Move the selection to the workspace row at `index` (used by click
    /// hit-testing); clamps into range.
    pub fn select(&mut self, index: usize) {
        let max = self.selectable.len().saturating_sub(1);
        self.selected = index.min(max);
    }

    /// The currently selected workspace name, or `None` when there are no
    /// workspaces. This is what the host opens on Enter.
    pub fn selected_workspace(&self) -> Option<&str> {
        let (mi, wi) = self.selectable.get(self.selected)?;
        self.machines
            .get(*mi)
            .and_then(|m| m.workspaces.get(*wi))
            .map(|w| w.name.as_str())
    }

    pub fn status_line(&self) -> &str {
        &self.status_line
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status_line = msg.into();
    }
}

/// The real reachability probe: an SSH `true` to the host with a short
/// deadline, through the orchestrator's probe + the shared `shelbi_ssh`
/// transport (same ControlMaster + reverse forward the poller uses). Runs on
/// the prober thread.
fn real_reachability_probe(_machine: &str, host: &str) -> Reachability {
    let target = Host::Ssh {
        host: host.to_string(),
    };
    let exec = SshExec::with_deadline(target, host.to_string(), REACHABILITY_PROBE_DEADLINE);
    probe_reachability(&exec)
}

// ---------------------------------------------------------------------------
// Rendering (shared by the standalone process and the in-process shell)
// ---------------------------------------------------------------------------

/// The reachability badge shown next to a remote machine's `ssh <host>` label.
/// One span so both runtimes render it identically through [`machine_header_line`].
fn reachability_span(r: &Reachability) -> Span<'static> {
    let (text, color) = match r {
        Reachability::Reachable => ("● reachable".to_string(), Color::Green),
        Reachability::Unknown => ("○ checking…".to_string(), Color::DarkGray),
        Reachability::Unreachable { error } => {
            (format!("● unreachable: {}", truncate(error, 40)), Color::Red)
        }
    };
    Span::styled(format!("  {text}"), Style::default().fg(color))
}

/// A short badge for an observed workspace state.
fn state_badge(state: Option<WorkspaceState>) -> (&'static str, Color) {
    match state {
        Some(WorkspaceState::Working) => ("working", Color::Green),
        Some(WorkspaceState::AwaitingInput) => ("awaiting", Color::Yellow),
        Some(WorkspaceState::Blocked) => ("blocked", Color::Red),
        Some(WorkspaceState::Paused) => ("paused", Color::Magenta),
        Some(WorkspaceState::Serving) => ("serving", Color::Cyan),
        None => ("idle", Color::DarkGray),
    }
}

/// Render the whole machines view into `area`. Shared by both runtimes.
pub fn render_full(f: &mut Frame, app: &mut MachinesApp, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(2)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_title(f, app, outer[0]);
    render_body(f, app, outer[2]);
    render_footer(f, app, outer[3]);
}

fn render_title(f: &mut Frame, app: &MachinesApp, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            "Machines",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {}", app.display_label()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(title), area);
}

fn render_body(f: &mut Frame, app: &MachinesApp, area: Rect) {
    if app.machines.is_empty() {
        let msg = if app.status_line.is_empty() {
            "No machines declared for this project."
        } else {
            &app.status_line
        };
        let p = Paragraph::new(Line::from(Span::styled(
            msg.to_string(),
            Style::default().fg(Color::DarkGray),
        )))
        .wrap(Wrap { trim: true });
        f.render_widget(p, area);
        return;
    }

    let selected = app.selectable.get(app.selected).copied();
    let mut lines: Vec<Line> = Vec::new();
    for (mi, m) in app.machines.iter().enumerate() {
        lines.push(machine_header_line(m));
        if m.workspaces.is_empty() {
            lines.push(Line::from(Span::styled(
                "    (no workspaces)".to_string(),
                Style::default().fg(Color::DarkGray),
            )));
        }
        for (wi, w) in m.workspaces.iter().enumerate() {
            let is_selected = selected == Some((mi, wi));
            lines.push(workspace_line(w, is_selected, area.width));
        }
        lines.push(Line::from(String::new())); // blank between machines
    }

    f.render_widget(Paragraph::new(lines), area);
}

fn machine_header_line(m: &MachineEntry) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        m.name.clone(),
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
    )];
    // Location: local, or the ssh host.
    let loc = if m.is_local {
        "local".to_string()
    } else {
        match &m.host {
            Some(h) => format!("ssh {h}"),
            None => "ssh".to_string(),
        }
    };
    spans.push(Span::styled(
        format!("  {loc}"),
        Style::default().fg(Color::DarkGray),
    ));
    // Live reachability sits right next to the `ssh <host>` label — remotes only;
    // a local machine is the hub and is never probed.
    if !m.is_local {
        spans.push(reachability_span(&m.reachability));
    }
    if !m.tags.is_empty() {
        spans.push(Span::styled(
            format!("  [{}]", m.tags.join(", ")),
            Style::default().fg(theme::WORKFLOW_BADGE_FG),
        ));
    }
    // Remote binary resolution (rt-machine-setup), or a prompt to set it up.
    if !m.is_local {
        match &m.remote {
            Some(rec) => {
                let color = if rec.compatible { Color::Green } else { Color::Red };
                let suffix = if rec.compatible { "" } else { " (incompatible)" };
                spans.push(Span::styled(
                    format!("  {} v{}{}", rec.path, rec.version, suffix),
                    Style::default().fg(color),
                ));
            }
            None => spans.push(Span::styled(
                "  not set up — run `shelbi machine setup`".to_string(),
                Style::default().fg(Color::Yellow),
            )),
        }
    }
    Line::from(spans)
}

fn workspace_line(w: &WorkspaceRow, selected: bool, width: u16) -> Line<'static> {
    let (badge, badge_color) = state_badge(w.state);
    let pointer = if selected { "▸ " } else { "  " };
    let base = if selected {
        Style::default().bg(theme::SELECTION_BG)
    } else {
        Style::default()
    };
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(format!("  {pointer}"), base),
        Span::styled(
            format!("{:<18}", truncate(&w.name, 18)),
            base.fg(Color::White),
        ),
        Span::styled(format!("{badge:<9}"), base.fg(badge_color)),
    ];
    let task = match &w.current_task {
        Some(t) => t.clone(),
        None => "-".to_string(),
    };
    spans.push(Span::styled(task, base.fg(Color::Gray)));
    // Pad the selection highlight to the full row width so it reads as a bar.
    if selected {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let pad = (width as usize).saturating_sub(used);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), base));
        }
    }
    Line::from(spans)
}

fn render_footer(f: &mut Frame, app: &MachinesApp, area: Rect) {
    let hint = if app.status_line.is_empty() {
        "↑/↓ select · Enter open · r refresh".to_string()
    } else {
        app.status_line.clone()
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

/// Truncate `s` to `max` display chars, adding an ellipsis when it overflows.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        let mut out: String = s.chars().take(keep).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: MachineKind, workspaces: Vec<WorkspaceRow>) -> MachineEntry {
        MachineEntry {
            name: name.into(),
            kind,
            host: if kind == MachineKind::Ssh {
                Some(format!("{name}.local"))
            } else {
                None
            },
            is_local: kind == MachineKind::Local,
            tags: vec![],
            remote: None,
            reachability: Reachability::Unknown,
            workspaces,
        }
    }

    fn ws(name: &str, state: Option<WorkspaceState>, task: Option<&str>) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            state,
            current_task: task.map(str::to_string),
        }
    }

    fn app_with(machines: Vec<MachineEntry>) -> MachinesApp {
        let mut app = MachinesApp::new("demo");
        app.apply_data(MachinesData {
            display_name: Some("Demo".into()),
            machines,
        });
        app
    }

    #[test]
    fn selection_addresses_only_workspace_rows_across_machines() {
        let app = app_with(vec![
            entry(
                "local",
                MachineKind::Local,
                vec![
                    ws("local-1", Some(WorkspaceState::Working), Some("T-1")),
                    ws("local-2", None, None),
                ],
            ),
            entry(
                "gpu",
                MachineKind::Ssh,
                vec![ws("gpu-1", Some(WorkspaceState::Serving), None)],
            ),
        ]);
        assert_eq!(app.selectable_count(), 3);
        assert_eq!(app.selected_workspace(), Some("local-1"));
    }

    #[test]
    fn nav_moves_through_all_workspaces_and_clamps() {
        let mut app = app_with(vec![
            entry(
                "local",
                MachineKind::Local,
                vec![ws("local-1", None, None), ws("local-2", None, None)],
            ),
            entry("gpu", MachineKind::Ssh, vec![ws("gpu-1", None, None)]),
        ]);
        app.nav_down();
        assert_eq!(app.selected_workspace(), Some("local-2"));
        app.nav_down();
        assert_eq!(app.selected_workspace(), Some("gpu-1"), "crosses machines");
        app.nav_down(); // clamps at the last row
        assert_eq!(app.selected_workspace(), Some("gpu-1"));
        app.nav_up();
        app.nav_up();
        app.nav_up(); // clamps at the first
        assert_eq!(app.selected_workspace(), Some("local-1"));
    }

    #[test]
    fn apply_data_clamps_a_now_out_of_range_selection() {
        let mut app = app_with(vec![entry(
            "local",
            MachineKind::Local,
            vec![ws("a", None, None), ws("b", None, None), ws("c", None, None)],
        )]);
        app.nav_down();
        app.nav_down();
        assert_eq!(app.selected_workspace(), Some("c"));
        // A later refresh with fewer workspaces must not strand the selection.
        app.apply_data(MachinesData {
            display_name: Some("Demo".into()),
            machines: vec![entry("local", MachineKind::Local, vec![ws("a", None, None)])],
        });
        assert_eq!(app.selected_index(), 0);
        assert_eq!(app.selected_workspace(), Some("a"));
    }

    #[test]
    fn empty_project_has_no_selectable_rows() {
        let app = app_with(vec![]);
        assert_eq!(app.selectable_count(), 0);
        assert_eq!(app.selected_workspace(), None);
    }

    #[test]
    fn state_badges_cover_every_state() {
        for st in [
            None,
            Some(WorkspaceState::Working),
            Some(WorkspaceState::AwaitingInput),
            Some(WorkspaceState::Blocked),
            Some(WorkspaceState::Paused),
            Some(WorkspaceState::Serving),
        ] {
            let (label, _) = state_badge(st);
            assert!(!label.is_empty());
        }
    }

    // ---- reachability --------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn ssh_entry(name: &str) -> MachineEntry {
        entry(name, MachineKind::Ssh, vec![ws("w1", None, None)])
    }

    /// Concatenate a header line's span text, so a test can assert what the one
    /// shared renderer ([`machine_header_line`], used by both runtimes) emits.
    fn header_text(m: &MachineEntry) -> String {
        machine_header_line(m)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn remote_header_shows_each_reachability_state() {
        let mut m = ssh_entry("gpu");

        m.reachability = Reachability::Unknown;
        assert!(header_text(&m).contains("checking"));

        m.reachability = Reachability::Reachable;
        let t = header_text(&m);
        assert!(t.contains("ssh gpu.local"), "keeps the ssh host label: {t}");
        assert!(t.contains("reachable"));

        m.reachability = Reachability::Unreachable {
            error: "connection refused".to_string(),
        };
        let t = header_text(&m);
        assert!(t.contains("unreachable"));
        assert!(t.contains("connection refused"), "surfaces the error: {t}");
    }

    #[test]
    fn local_header_has_no_reachability_badge() {
        let m = entry("hub", MachineKind::Local, vec![]);
        let t = header_text(&m);
        assert!(t.contains("local"));
        assert!(!t.contains("reachable"));
        assert!(!t.contains("checking"));
    }

    #[test]
    fn remote_reachability_folds_in_and_recovers() {
        // A stub probe that reports down until the flag flips.
        let up = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let up_probe = Arc::clone(&up);
        let mut app = MachinesApp::new("demo");
        app.enable_reachability_with(
            Duration::from_millis(20),
            Duration::from_millis(80),
            move |_m, _h| {
                if up_probe.load(Ordering::Acquire) {
                    Reachability::Reachable
                } else {
                    Reachability::Unreachable {
                        error: "connection refused".to_string(),
                    }
                }
            },
        );

        let data = || MachinesData {
            display_name: Some("Demo".into()),
            machines: vec![ssh_entry("gpu")],
        };

        // Re-applying the read (as the real refresh loop does) folds the latest
        // probe result into the entry. Poll until it shows the host down.
        let down = wait_for_reachability(&mut app, data, |r| {
            matches!(r, Reachability::Unreachable { .. })
        });
        match down {
            Reachability::Unreachable { error } => assert!(error.contains("refused")),
            other => panic!("expected Unreachable, got {other:?}"),
        }

        // Bring the host back; the next refresh folds in Reachable.
        up.store(true, Ordering::Release);
        wait_for_reachability(&mut app, data, |r| matches!(r, Reachability::Reachable));
    }

    #[test]
    fn local_machines_are_never_probed() {
        // The probe stub counts its calls; a local-only project must trigger
        // none (and no SSH traffic).
        let calls = Arc::new(AtomicUsize::new(0));
        let probe_calls = Arc::clone(&calls);
        let mut app = MachinesApp::new("demo");
        app.enable_reachability_with(
            Duration::from_millis(20),
            Duration::from_millis(80),
            move |_m, _h| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Reachability::Reachable
            },
        );
        app.apply_data(MachinesData {
            display_name: Some("Demo".into()),
            machines: vec![entry("hub", MachineKind::Local, vec![ws("w", None, None)])],
        });
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(
            calls.load(Ordering::Acquire),
            0,
            "a local-only project must not probe anything"
        );
    }

    /// Re-apply `data` on a short poll until the first machine's reachability
    /// satisfies `pred`, returning it (panics on timeout). Models the refresh
    /// loop that folds prober results in on each `apply_data`.
    fn wait_for_reachability(
        app: &mut MachinesApp,
        data: impl Fn() -> MachinesData,
        pred: impl Fn(&Reachability) -> bool,
    ) -> Reachability {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            app.apply_data(data());
            let r = app.machines()[0].reachability.clone();
            if pred(&r) {
                return r;
            }
            if Instant::now() >= deadline {
                panic!("reachability did not satisfy predicate within 2s (was {r:?})");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
