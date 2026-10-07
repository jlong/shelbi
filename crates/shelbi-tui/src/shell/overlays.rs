//! The single-process TUI's in-process overlays (removing-tmux Phase 4d).
//!
//! The shell holds one [`ActiveOverlay`] at a time, feeds it keys/mouse from the
//! one event loop, draws it over the main area, and acts on the [`OverlayEvent`]
//! it yields. Each variant wraps a shared overlay type from [`crate::overlay`],
//! so the in-process overlays and the legacy tmux popups run the exact same
//! rendering and decision logic.
//!
//! What this module owns: routing a key/click/draw to the active overlay,
//! building the palette's [`CommandModel`] from the shell's live sidebar model,
//! and opening each overlay. Turning an activated palette [`Entry`] into a
//! command effect, and running blocking effects off the UI thread, is the
//! shell's job (see [`super`]).

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::widgets::Clear;
use ratatui::Frame;

use std::path::Path;

use shelbi_app::command::{
    CommandModel, EditItem, ProjectItem, ReviewItem, ViewItem, WorkspaceItem,
};
use shelbi_app::exec::EditTarget;
use shelbi_app::nav::View;
use shelbi_app::view::SidebarModel;
use std::collections::HashMap;

use shelbi_core::{Column, ConfigMode};
use shelbi_palette::{Decoration, DecorationColor, Entry};
use shelbi_state::keymap::{DisplayStyle, Keymaps, PopoverAction};
use shelbi_state::IssueFile;

use crate::overlay::palette::ProjectEntry;
use crate::overlay::{self, centered_pct, centered_rect};

/// What the shell should do after feeding input to the active overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayEvent {
    /// Keep the overlay open.
    Stay,
    /// Close the overlay; focus returns to the agent (the main terminal view).
    Close,
    /// Close the overlay and move focus to the sidebar (palette Tab).
    FocusSidebar,
    /// The palette activated this entry; the shell resolves it to an effect.
    RunEntry(Entry),
    /// The review-confirm dialog chose a slot to load the task onto.
    ReviewConfirmed { task_id: String, slot: String },
    /// The review-confirm / reject prompt was cancelled.
    ReviewCancelled,
    /// The reject prompt submitted a reason for the task.
    Rejected { task_id: String, reason: String },
    /// The Zen intro resolved; the shell toggles Zen and/or persists the flag.
    ZenResult {
        project: String,
        confirmed: bool,
        dont_show_again: bool,
    },
    /// The Add-project form submitted; the shell validates these values and
    /// either scaffolds the project (off-thread) or stashes an inline error
    /// back on the form.
    AddProjectSubmit {
        name: String,
        root: String,
        mode: ConfigMode,
    },
    /// The Add-project form was cancelled.
    AddProjectCancel,
}

/// The overlay currently drawn over the main area, with its runtime state.
pub enum ActiveOverlay {
    Palette(overlay::palette::Palette),
    ReviewConfirm {
        task_id: String,
        dialog: overlay::review_confirm::Dialog,
    },
    RejectReason {
        task_id: String,
        prompt: overlay::review_reject::RejectPrompt,
    },
    ErrorLog {
        project: String,
        viewer: overlay::error_log::Viewer,
    },
    ZenIntro {
        project: String,
        state: overlay::zen_intro::IntroState,
    },
    AddProject {
        form: overlay::add_project::Form,
    },
    /// The full task-description popover, opened from the review panel's `More`
    /// link. Reuses the board's task-detail renderer
    /// ([`crate::kanban::render_task_popover_into`]) so the board and the review
    /// show the same box; it owns its own copy of the task so the review content
    /// session keeps running underneath.
    TaskDescription {
        /// Both the task and the keymaps are large; boxing them keeps the other
        /// `ActiveOverlay` variants from paying this variant's size. The keymaps
        /// render the footer's chord hints (the same ones the board popover
        /// shows); keys are dispatched through the shell's live keymaps instead.
        task: Box<IssueFile>,
        scroll: u16,
        keymaps: Box<Keymaps>,
        style: DisplayStyle,
    },
}

impl ActiveOverlay {
    /// Open the command palette over `entries`, with `projects` listed in the
    /// Projects column (empty hides the column).
    pub fn palette(entries: Vec<Entry>, projects: Vec<ProjectEntry>) -> Self {
        ActiveOverlay::Palette(overlay::palette::Palette::new(entries, projects))
    }

    /// Open the "Load for review" dialog for `task_id` over the given (free)
    /// slots. Enter on a queued review / the palette's load-review action opens
    /// this (`rt-tui-review-load-queued`); the review interface's reject path
    /// opens the sibling [`reject_reason`](Self::reject_reason).
    pub fn review_confirm(
        task_id: impl Into<String>,
        title: impl Into<String>,
        slots: Vec<overlay::review_confirm::Slot>,
    ) -> Self {
        ActiveOverlay::ReviewConfirm {
            task_id: task_id.into(),
            dialog: overlay::review_confirm::Dialog::new(title, slots),
        }
    }

    /// Open the "Load for review" dialog as a no-slots report — every review
    /// slot is busy, so there is nothing free to load onto and any key just
    /// dismisses it (`rt-tui-review-load-queued`).
    pub fn review_busy_report(
        task_id: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ActiveOverlay::ReviewConfirm {
            task_id: task_id.into(),
            dialog: overlay::review_confirm::Dialog::informational(title, message),
        }
    }

    /// Open the reject-reason prompt for `task_id`. Opened by the review
    /// interface's reject action (`rt-tui-review`).
    pub fn reject_reason(task_id: impl Into<String>) -> Self {
        ActiveOverlay::RejectReason {
            task_id: task_id.into(),
            prompt: overlay::review_reject::RejectPrompt::new(),
        }
    }

    /// Open the error log for `project`. Snapshots the unread state, then marks
    /// the log read so the sidebar's unread button clears once this closes
    /// (same contract as the tmux popup). Best-effort: a read/mark failure
    /// degrades to an empty view.
    pub fn error_log(project: impl Into<String>) -> Self {
        let project = project.into();
        let rows = shelbi_state::read_errors_with_unread(&project).unwrap_or_default();
        let _ = shelbi_state::mark_errors_read(&project);
        ActiveOverlay::ErrorLog {
            project,
            viewer: overlay::error_log::Viewer::new(rows),
        }
    }

    /// Open the first-run Zen intro for `project`.
    pub fn zen_intro(project: impl Into<String>) -> Self {
        ActiveOverlay::ZenIntro {
            project: project.into(),
            state: overlay::zen_intro::IntroState::default(),
        }
    }

    /// Open the Add-project form, prefilling the repo path from `cwd`.
    pub fn add_project(cwd: &Path) -> Self {
        ActiveOverlay::AddProject {
            form: overlay::add_project::Form::new(cwd),
        }
    }

    /// Open the task-description popover for `task` (the review panel's `More`).
    /// `keymaps` / `style` render the footer's chord hints the same way the
    /// board popover does.
    pub fn task_description(task: IssueFile, keymaps: Keymaps, style: DisplayStyle) -> Self {
        ActiveOverlay::TaskDescription {
            task: Box::new(task),
            scroll: 0,
            keymaps: Box::new(keymaps),
            style,
        }
    }

    /// Replace the palette's entry list on a background refresh (no-op for the
    /// other overlays).
    pub fn refresh_palette(&mut self, entries: Vec<Entry>) {
        if let ActiveOverlay::Palette(p) = self {
            p.set_entries(entries);
        }
    }

    /// Feed one key press. `keymaps` resolves the palette-mode bindings; the
    /// other overlays ignore it.
    pub fn handle_key(&mut self, ev: KeyEvent, keymaps: &Keymaps) -> OverlayEvent {
        match self {
            ActiveOverlay::Palette(p) => {
                let action = crate::keymap::chord_from_event(ev)
                    .and_then(|c| keymaps.palette.dispatch(c));
                match p.handle_key(ev, action) {
                    overlay::palette::PaletteStep::Continue => OverlayEvent::Stay,
                    overlay::palette::PaletteStep::Close => OverlayEvent::Close,
                    overlay::palette::PaletteStep::FocusSidebar => OverlayEvent::FocusSidebar,
                    overlay::palette::PaletteStep::Activate(e) => OverlayEvent::RunEntry(e),
                }
            }
            ActiveOverlay::ReviewConfirm { task_id, dialog } => {
                match dialog.handle_key(ev.code) {
                    overlay::review_confirm::Step::Continue => OverlayEvent::Stay,
                    overlay::review_confirm::Step::Done(outcome) => confirm_event(task_id, outcome),
                }
            }
            ActiveOverlay::RejectReason { task_id, prompt } => match prompt.handle_key(ev) {
                overlay::review_reject::Step::Continue => OverlayEvent::Stay,
                overlay::review_reject::Step::Submit => OverlayEvent::Rejected {
                    task_id: task_id.clone(),
                    reason: prompt.reason(),
                },
                overlay::review_reject::Step::Cancel => OverlayEvent::ReviewCancelled,
            },
            ActiveOverlay::ErrorLog { project, viewer } => match viewer.handle_key(ev) {
                overlay::error_log::ErrorLogOutcome::Continue => OverlayEvent::Stay,
                overlay::error_log::ErrorLogOutcome::Close => OverlayEvent::Close,
                overlay::error_log::ErrorLogOutcome::Clear => {
                    // Clear is a cheap local write; do it here and show the empty
                    // state at once. The shell recomputes the unread count on its
                    // next refresh.
                    let _ = shelbi_state::clear_errors(project);
                    viewer.clear();
                    OverlayEvent::Stay
                }
            },
            ActiveOverlay::ZenIntro { project, state } => {
                match overlay::zen_intro::step_intro(state, ev) {
                    overlay::zen_intro::IntroOutcome::Continue => OverlayEvent::Stay,
                    overlay::zen_intro::IntroOutcome::Cancelled => OverlayEvent::ZenResult {
                        project: project.clone(),
                        confirmed: false,
                        dont_show_again: state.dont_show_again,
                    },
                    overlay::zen_intro::IntroOutcome::Confirmed => OverlayEvent::ZenResult {
                        project: project.clone(),
                        confirmed: true,
                        dont_show_again: state.dont_show_again,
                    },
                }
            }
            ActiveOverlay::AddProject { form } => match form.handle_key(ev) {
                overlay::add_project::Step::Continue => OverlayEvent::Stay,
                overlay::add_project::Step::Cancel => OverlayEvent::AddProjectCancel,
                overlay::add_project::Step::Submit => OverlayEvent::AddProjectSubmit {
                    name: form.name().to_string(),
                    root: form.root().to_string(),
                    mode: form.mode(),
                },
            },
            ActiveOverlay::TaskDescription { scroll, .. } => {
                // The popover is read-only here: Esc/q close it, j/k and the page
                // chords scroll the body. The board's move / open-workspace
                // chords (H/L/o, shown in the shared footer) have no target in a
                // review, so they're inert.
                let action = crate::keymap::chord_from_event(ev)
                    .and_then(|c| keymaps.popover.dispatch(c));
                match action {
                    Some(PopoverAction::Close) => OverlayEvent::Close,
                    Some(PopoverAction::ScrollDown) => {
                        *scroll = scroll.saturating_add(1);
                        OverlayEvent::Stay
                    }
                    Some(PopoverAction::ScrollUp) => {
                        *scroll = scroll.saturating_sub(1);
                        OverlayEvent::Stay
                    }
                    Some(PopoverAction::PageDown) => {
                        *scroll = scroll.saturating_add(10);
                        OverlayEvent::Stay
                    }
                    Some(PopoverAction::PageUp) => {
                        *scroll = scroll.saturating_sub(10);
                        OverlayEvent::Stay
                    }
                    Some(PopoverAction::ScrollHome) => {
                        *scroll = 0;
                        OverlayEvent::Stay
                    }
                    _ => OverlayEvent::Stay,
                }
            }
        }
    }

    /// Feed a mouse event. Only the clickable overlays (the review dialogs and
    /// the error log's wheel) care; everything else is ignored so a stray click
    /// doesn't leak to the agent underneath.
    pub fn handle_mouse(&mut self, m: MouseEvent, area: Rect) -> OverlayEvent {
        let rect = self.rect(area);
        match self {
            ActiveOverlay::ReviewConfirm { task_id, dialog } => {
                if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    match dialog.handle_click(rect, m.column, m.row) {
                        overlay::review_confirm::Step::Continue => OverlayEvent::Stay,
                        overlay::review_confirm::Step::Done(outcome) => {
                            confirm_event(task_id, outcome)
                        }
                    }
                } else {
                    OverlayEvent::Stay
                }
            }
            ActiveOverlay::RejectReason { task_id, prompt } => {
                if m.kind == MouseEventKind::Down(MouseButton::Left) {
                    match prompt.handle_click(rect, m.column, m.row) {
                        overlay::review_reject::Step::Continue => OverlayEvent::Stay,
                        overlay::review_reject::Step::Submit => OverlayEvent::Rejected {
                            task_id: task_id.clone(),
                            reason: prompt.reason(),
                        },
                        overlay::review_reject::Step::Cancel => OverlayEvent::ReviewCancelled,
                    }
                } else {
                    OverlayEvent::Stay
                }
            }
            ActiveOverlay::ErrorLog { viewer, .. } => {
                match m.kind {
                    MouseEventKind::ScrollUp => viewer.scroll_up(1),
                    MouseEventKind::ScrollDown => viewer.scroll_down(1),
                    _ => {}
                }
                OverlayEvent::Stay
            }
            ActiveOverlay::TaskDescription { scroll, .. } => {
                match m.kind {
                    MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(1),
                    MouseEventKind::ScrollDown => *scroll = scroll.saturating_add(1),
                    _ => {}
                }
                OverlayEvent::Stay
            }
            _ => OverlayEvent::Stay,
        }
    }

    /// The rect this overlay occupies within `area` (the main area). Centered;
    /// sizes mirror the tmux popups' dimensions so the layout matches.
    fn rect(&self, area: Rect) -> Rect {
        match self {
            ActiveOverlay::Palette(_) => centered_pct(area, 70, 60, 40, 10),
            ActiveOverlay::ErrorLog { .. } => centered_pct(area, 80, 60, 40, 8),
            ActiveOverlay::RejectReason { .. } => centered_rect(area, 70, 18),
            ActiveOverlay::ReviewConfirm { dialog, .. } => {
                // Height follows the slot count, like the tmux launcher.
                let h = (dialog_rows(dialog) + 6).max(9) as u16;
                centered_rect(area, 60, h)
            }
            ActiveOverlay::ZenIntro { .. } => centered_rect(area, 64, 16),
            // The form fills a generous share of the main area so long repo
            // paths and validation messages never truncate (the tmux dialog
            // filled the whole palette window for the same reason).
            ActiveOverlay::AddProject { .. } => centered_pct(area, 70, 60, 48, 14),
            // The same 80%×80% centered box the board's task popover uses, so
            // the review and the board show the task at the identical size.
            ActiveOverlay::TaskDescription { .. } => crate::kanban::popover_rect(area),
        }
    }

    /// Draw the overlay over `area`. Takes `&mut self` because the error-log
    /// viewer clamps its scroll offset against the rendered line count at draw
    /// time.
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let rect = self.rect(area);
        // Reset every cell in the overlay's rect to the default background
        // before the overlay paints. In the tmux runtime each overlay was its
        // own popup process with a fresh screen, so none of them clears first;
        // drawn in-process over a live view, any cell the overlay doesn't paint
        // (list-row padding, gaps between widgets) would otherwise show text
        // from the view underneath — and `List`'s full-width selection bar only
        // patches the *style* of those cells, so the underlying characters bled
        // into the palette's highlighted row. `Clear` empties the symbols and
        // resets the style (also undoing the shell's dim), giving each overlay
        // the clean, opaque backdrop it assumes.
        f.render_widget(Clear, rect);
        match self {
            ActiveOverlay::Palette(p) => p.render(f, rect),
            ActiveOverlay::ReviewConfirm { dialog, .. } => dialog.render(f, rect),
            ActiveOverlay::RejectReason { prompt, .. } => prompt.render(f, rect),
            ActiveOverlay::ErrorLog { viewer, .. } => overlay::error_log::render(f, rect, viewer),
            ActiveOverlay::ZenIntro { state, .. } => {
                overlay::zen_intro::render_intro(f, rect, state)
            }
            ActiveOverlay::AddProject { form } => form.render(f, rect),
            ActiveOverlay::TaskDescription {
                task,
                scroll,
                keymaps,
                style,
            } => {
                // One-entry column map (the task's own column) — enough for the
                // header's column label/colour; a review task's deps rarely show
                // and resolve to `[missing]` either way, exactly as the board
                // renders a dep it hasn't loaded.
                let columns: HashMap<String, Column> =
                    HashMap::from([(task.task.id.clone(), task.task.column.clone())]);
                crate::kanban::render_task_popover_into(
                    f,
                    rect,
                    Some(task.as_ref()),
                    &columns,
                    scroll,
                    keymaps.as_ref(),
                    *style,
                );
            }
        }
    }
}

/// How many body rows the review-confirm dialog needs (picker: one per slot;
/// confirm/informational: a fixed two). Mirrors the tmux launcher's height math.
fn dialog_rows(dialog: &overlay::review_confirm::Dialog) -> usize {
    if dialog.is_picker() {
        // The driver doesn't expose the slot count directly; the picker is only
        // chosen with >1 slot, so a small fixed allowance keeps the box roomy.
        4
    } else {
        2
    }
}

fn confirm_event(task_id: &str, outcome: overlay::review_confirm::Outcome) -> OverlayEvent {
    match outcome {
        overlay::review_confirm::Outcome::Load(slot) => OverlayEvent::ReviewConfirmed {
            task_id: task_id.to_string(),
            slot,
        },
        overlay::review_confirm::Outcome::Cancel => OverlayEvent::ReviewCancelled,
    }
}

/// Build the palette's [`CommandModel`] from the shell's live sidebar model plus
/// a few light on-disk reads (the other projects and the edit targets). The
/// registry enumerates the palette's commands from this, so populating it is
/// what makes every command reachable.
pub fn build_command_model(project: &str, sidebar: &SidebarModel) -> CommandModel {
    let mut views: Vec<ViewItem> = sidebar
        .nav
        .iter()
        .map(|n| ViewItem {
            id: format!("view:{}", n.view.as_view_id()),
            title: n.label.clone(),
            view: n.view.clone(),
            decoration: nav_decoration(&n.label),
        })
        .collect();

    // Machines is palette-only: it's no longer a sidebar nav row, so add its
    // command here (guarded against duplication should a future nav reintroduce
    // it). The id/title match the historical `view:machines` entry.
    if !views.iter().any(|v| v.view == View::Machines) {
        views.push(ViewItem {
            id: format!("view:{}", View::Machines.as_view_id()),
            title: "Machines".to_string(),
            view: View::Machines,
            decoration: nav_decoration("Machines"),
        });
    }

    let workspaces = sidebar
        .workspaces
        .iter()
        .map(|w| WorkspaceItem {
            name: w.name.clone(),
            subtitle: w.current_task.clone(),
            decoration: None,
        })
        .collect();

    let reviews = sidebar
        .reviews
        .iter()
        .map(|r| ReviewItem {
            task_id: r.task_id.clone(),
            title: r.title.clone(),
            subtitle: None,
            decoration: None,
        })
        .collect();

    let other_projects = shelbi_state::list_projects()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.name != project)
        .map(|p| ProjectItem {
            slug: p.name.clone(),
            label: p.display_label().to_string(),
        })
        .collect();

    CommandModel {
        project: Some(project.to_string()),
        zen_on: !matches!(sidebar.zen_mode, shelbi_state::ZenModeState::Off),
        zen_shortcut: None,
        views,
        workspaces,
        reviews,
        legacy_agents: Vec::new(),
        other_projects,
        edit_targets: edit_targets(project),
    }
}

/// The palette icon decoration for a nav view, from its label: the Figma's
/// twemoji nav glyph (💬 / 📋 / ⚡ / 🖥), rendered in its natural color. A label
/// with no mapped glyph gets `None`, so it falls back to the entry-kind icon.
fn nav_decoration(label: &str) -> Option<Decoration> {
    let glyph = super::sidebar::nav_glyph(label);
    if glyph == "•" {
        return None;
    }
    Some(Decoration {
        glyph: glyph.to_string(),
        color: DecorationColor::Default,
    })
}

/// Build the palette's Projects column from the registered projects on disk.
/// The current project leads (filled `●`), other open projects get the green
/// ring (`○`), and closed ones the gray ring. Enter on a row activates its
/// `action:switch-project:<slug>` entry; the renderer appends the trailing
/// "+ Add project" row itself.
pub fn build_projects_column(current: &str) -> Vec<ProjectEntry> {
    let open: std::collections::HashSet<String> = shelbi_state::list_open_projects()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mut rows: Vec<ProjectEntry> = shelbi_state::list_projects()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let is_current = p.name == current;
            let loaded = is_current || open.contains(&p.name);
            // The current project is the single "active" one (the filled,
            // pulsing disc), matching the Figma.
            ProjectEntry {
                indicator: overlay::palette::project_indicator(loaded, is_current),
                label: p.display_label().to_string(),
                slug: p.name,
            }
        })
        .collect();
    // Lead with the current project, keeping the rest in list order
    // (most-recently-launched first).
    rows.sort_by_key(|p| p.slug != current);
    rows
}

/// The on-disk edit targets that exist for `project`, titled to match the tmux
/// palette's edit openers. Missing targets are skipped so the palette never
/// offers a dead entry.
fn edit_targets(project: &str) -> Vec<EditItem> {
    let mut out = Vec::new();
    let mut push = |target: EditTarget, title: &str, path: Option<std::path::PathBuf>| {
        if path.map(|p| p.exists()).unwrap_or(false) {
            out.push(EditItem {
                target,
                title: title.to_string(),
            });
        }
    };

    push(
        EditTarget::Project,
        "Edit Project Settings",
        project_settings_path(project),
    );
    // Per-agent openers, enumerated from `agents/` and sorted for stability.
    if let Ok(dir) = shelbi_state::agents_dir(project) {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut agents: Vec<String> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            agents.sort();
            for agent in agents {
                let path = dir.join(&agent).join("instructions.md");
                push(
                    EditTarget::Agent(agent.clone()),
                    &format!("Edit {agent} Settings"),
                    Some(path),
                );
            }
        }
    }
    push(
        EditTarget::ZenMode,
        "Edit Zen Mode",
        shelbi_state::zenmode_path(project).ok(),
    );
    push(
        EditTarget::Workflows,
        "Edit Workflows",
        shelbi_state::workflows_dir(project).ok(),
    );
    out
}

/// Resolve the project settings file, preferring an in-repo `project.yaml` over
/// the global `projects/<name>.yaml` (mirrors the tmux palette).
fn project_settings_path(project: &str) -> Option<std::path::PathBuf> {
    if let Ok(dir) = shelbi_state::config_project_dir(project) {
        let in_repo = dir.join("project.yaml");
        if in_repo.exists() {
            return Some(in_repo);
        }
    }
    Some(
        shelbi_state::projects_dir()
            .ok()?
            .join(format!("{project}.yaml")),
    )
}

/// Whether the first-run Zen intro should show before toggling Zen on for
/// `project`: only on an off→on transition, and only while the global
/// `zen_intro_seen` flag is unset. Mirrors the tmux palette's gate.
pub fn should_show_zen_intro(project: &str) -> bool {
    let project_off = shelbi_state::read_state(project)
        .map(|s| s.zen_mode == shelbi_state::ZenModeState::Off)
        .unwrap_or(true);
    if !project_off {
        return false;
    }
    !shelbi_state::read_global_state()
        .map(|s| s.zen_intro_seen)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::style::{Modifier, Style};
    use ratatui::Terminal;
    use shelbi_app::nav::View;
    use shelbi_app::view::{NavItem, ReviewRow, ReviewState, WorkspaceBadge, WorkspaceRow};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// One of every [`ActiveOverlay`] variant, so the backdrop-clearing check
    /// covers them all. The palette carries a selectable entry so its
    /// highlighted row is exercised too.
    fn every_overlay() -> Vec<ActiveOverlay> {
        use shelbi_palette::{Entry, EntryKind};
        let entry = Entry {
            id: "view:tasks".into(),
            label: "Issues".into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        };
        vec![
            ActiveOverlay::palette(vec![entry], Vec::new()),
            ActiveOverlay::review_confirm(
                "T-1",
                "Fix the thing",
                vec![overlay::review_confirm::Slot {
                    name: "review-1".into(),
                    occupant: None,
                }],
            ),
            ActiveOverlay::review_busy_report("T-1", "Fix the thing", "every slot is busy"),
            ActiveOverlay::reject_reason("T-1"),
            ActiveOverlay::ErrorLog {
                project: "alpha".into(),
                viewer: overlay::error_log::Viewer::new(Vec::new()),
            },
            ActiveOverlay::ZenIntro {
                project: "alpha".into(),
                state: overlay::zen_intro::IntroState::default(),
            },
            ActiveOverlay::add_project(Path::new("/tmp/alpha")),
            ActiveOverlay::task_description(
                sample_task(),
                keymaps(),
                shelbi_state::keymap::DisplayStyle::Linux,
            ),
        ]
    }

    /// A minimal review task for the description-popover overlay tests.
    fn sample_task() -> IssueFile {
        let ts = chrono::Utc::now();
        IssueFile {
            task: shelbi_core::Issue {
                id: "t-1".into(),
                title: "Cold-start cache".into(),
                column: shelbi_core::Column::review(),
                priority: 2,
                assigned_to: Some("charlie".into()),
                workflow: None,
                branch: Some("shelbi/cold-start-cache".into()),
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: ts,
                updated_at: ts,
                params: std::collections::BTreeMap::new(),
            },
            body: "## Summary\n\nWarm the application cache during startup.".into(),
            tracker_assignees: Vec::new(),
        }
    }

    /// Draw `ov` the way the shell does: the whole buffer is first filled with a
    /// recognizable sentinel character under the DIM modifier (mimicking the
    /// view and dimmed backdrop beneath an open overlay), then the overlay
    /// renders over it — all in one frame, so the prefill and the overlay share
    /// the one buffer. Returns the painted buffer and the overlay's rect.
    fn draw_over_sentinel(ov: &mut ActiveOverlay) -> (ratatui::buffer::Buffer, Rect) {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut rect = Rect::default();
        term.draw(|f| {
            let area = f.area();
            {
                let buf = f.buffer_mut();
                for y in area.top()..area.bottom() {
                    for x in area.left()..area.right() {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_char('X');
                            cell.set_style(Style::default().add_modifier(Modifier::DIM));
                        }
                    }
                }
            }
            rect = ov.rect(area);
            ov.render(f, area);
        })
        .unwrap();
        (term.backend().buffer().clone(), rect)
    }

    #[test]
    fn every_overlay_clears_the_view_underneath_its_rect() {
        for mut ov in every_overlay() {
            let (buf, rect) = draw_over_sentinel(&mut ov);
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    let cell = &buf[(x, y)];
                    assert_ne!(
                        cell.symbol(),
                        "X",
                        "underlying sentinel leaked through the overlay at ({x},{y})"
                    );
                    // The `Clear` must also drop the backdrop's DIM so the modal
                    // reads at full brightness within its rect.
                    assert!(
                        !cell.modifier.contains(Modifier::DIM),
                        "backdrop DIM leaked through the overlay at ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn palette_selected_row_highlight_holds_only_palette_text() {
        use shelbi_palette::{Entry, EntryKind};
        let entry = Entry {
            id: "action:quit".into(),
            label: "Quit Shelbi".into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        };
        let mut ov = ActiveOverlay::palette(vec![entry], Vec::new());
        let (buf, rect) = draw_over_sentinel(&mut ov);

        // Find the selected row: the one, inside the palette rect, whose cells
        // carry the selection bg.
        let mut found_row = false;
        for y in rect.top()..rect.bottom() {
            let selected_cells: Vec<u16> = (rect.left()..rect.right())
                .filter(|&x| buf[(x, y)].bg == crate::theme::SELECTION_BG)
                .collect();
            if selected_cells.is_empty() {
                continue;
            }
            found_row = true;
            // The highlight bar spans the commands column edge to edge: with no
            // Projects column the commands column is the whole content area,
            // which the borderless panel insets by a one-cell gutter on each
            // side. The report showed underlying error text bleeding into the
            // right of the selected row, so the bar must run from the content's
            // left edge to its last column. Any interior cell without the bar is
            // a wide-glyph continuation cell (empty symbol), never a visible gap.
            let first = *selected_cells.first().unwrap();
            let last = *selected_cells.last().unwrap();
            assert_eq!(
                first,
                rect.left() + 1,
                "the selection bar must start at the content's left edge"
            );
            assert_eq!(
                last,
                rect.right() - 2,
                "the selection bar must reach the content's right edge, got {selected_cells:?}"
            );
            for x in first..=last {
                if buf[(x, y)].bg != crate::theme::SELECTION_BG {
                    assert_eq!(
                        buf[(x, y)].symbol().trim(),
                        "",
                        "a visible gap in the selection bar at ({x},{y})"
                    );
                }
            }
            // No sentinel character survives anywhere in the highlighted row's
            // span inside the rect — only the palette's own text.
            for x in rect.left()..rect.right() {
                assert_ne!(
                    buf[(x, y)].symbol(),
                    "X",
                    "underlying text leaked into the palette's selected row at ({x},{y})"
                );
            }
            let row: String = (rect.left()..rect.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            assert!(row.contains("Quit Shelbi"), "selected row text: {row:?}");
        }
        assert!(found_row, "the palette should render a selected row");
    }

    fn keymaps() -> Keymaps {
        shelbi_state::keymap::load_keymaps(None).0
    }

    #[test]
    fn review_confirm_returns_the_chosen_slot_as_a_value() {
        // A single-slot confirm: `l` loads onto that slot, and the event carries
        // the slot name back to the (future) review flow — no temp file.
        let mut ov = ActiveOverlay::review_confirm(
            "T-12",
            "Fix the thing",
            vec![overlay::review_confirm::Slot {
                name: "review-1".into(),
                occupant: None,
            }],
        );
        let ev = ov.handle_key(key(KeyCode::Char('l')), &keymaps());
        assert_eq!(
            ev,
            OverlayEvent::ReviewConfirmed {
                task_id: "T-12".into(),
                slot: "review-1".into()
            }
        );
    }

    #[test]
    fn review_confirm_cancel_returns_a_cancel_value() {
        let mut ov = ActiveOverlay::review_confirm(
            "T-12",
            "Fix",
            vec![overlay::review_confirm::Slot {
                name: "review-1".into(),
                occupant: None,
            }],
        );
        assert_eq!(
            ov.handle_key(key(KeyCode::Esc), &keymaps()),
            OverlayEvent::ReviewCancelled
        );
    }

    #[test]
    fn reject_reason_returns_the_typed_reason_as_a_value() {
        let mut ov = ActiveOverlay::reject_reason("T-7");
        // Type a reason, then Ctrl-D to submit.
        for c in "null deref".chars() {
            assert_eq!(ov.handle_key(key(KeyCode::Char(c)), &keymaps()), OverlayEvent::Stay);
        }
        let ev = ov.handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &keymaps(),
        );
        assert_eq!(
            ev,
            OverlayEvent::Rejected {
                task_id: "T-7".into(),
                reason: "null deref".into()
            }
        );
    }

    #[test]
    fn reject_reason_cannot_submit_blank_and_cancels_on_esc() {
        let mut ov = ActiveOverlay::reject_reason("T-7");
        // Ctrl-D on an empty reason stays open.
        assert_eq!(
            ov.handle_key(
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                &keymaps()
            ),
            OverlayEvent::Stay
        );
        assert_eq!(
            ov.handle_key(key(KeyCode::Esc), &keymaps()),
            OverlayEvent::ReviewCancelled
        );
    }

    #[test]
    fn task_description_popover_shows_the_body_title_and_footer() {
        // Reuses the board's task popover, so it carries the task title, the
        // metadata header, the markdown body, and the scroll/close footer.
        let mut ov = ActiveOverlay::task_description(
            sample_task(),
            keymaps(),
            shelbi_state::keymap::DisplayStyle::Linux,
        );
        let (buf, rect) = draw_over_sentinel(&mut ov);
        let text: String = (rect.top()..rect.bottom())
            .map(|y| {
                (rect.left()..rect.right())
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Cold-start cache"), "title shown: {text}");
        assert!(text.contains("Warm the application cache"), "body shown: {text}");
        assert!(text.contains("id: t-1"), "metadata header shown: {text}");
        assert!(text.contains("scroll"), "footer hint shown: {text}");
    }

    #[test]
    fn task_description_popover_scrolls_with_jk_and_closes_on_esc() {
        let mut ov = ActiveOverlay::task_description(
            sample_task(),
            keymaps(),
            shelbi_state::keymap::DisplayStyle::Linux,
        );
        // j scrolls down (Stay), k scrolls back.
        assert_eq!(ov.handle_key(key(KeyCode::Char('j')), &keymaps()), OverlayEvent::Stay);
        if let ActiveOverlay::TaskDescription { scroll, .. } = &ov {
            assert_eq!(*scroll, 1, "j scrolled the body down one line");
        } else {
            panic!("expected the task-description overlay");
        }
        assert_eq!(ov.handle_key(key(KeyCode::Char('k')), &keymaps()), OverlayEvent::Stay);
        // Esc closes it (returning to the review panel underneath).
        assert_eq!(ov.handle_key(key(KeyCode::Esc), &keymaps()), OverlayEvent::Close);
    }

    #[test]
    fn palette_tab_focuses_sidebar_and_esc_closes() {
        let mut ov = ActiveOverlay::palette(Vec::new(), Vec::new());
        assert_eq!(
            ov.handle_key(key(KeyCode::Tab), &keymaps()),
            OverlayEvent::FocusSidebar
        );
        assert_eq!(
            ov.handle_key(key(KeyCode::Esc), &keymaps()),
            OverlayEvent::Close
        );
    }

    #[test]
    fn command_model_carries_views_workspaces_and_reviews() {
        let sidebar = SidebarModel {
            project_label: "alpha".into(),
            nav: vec![
                NavItem {
                    label: "Orchestrator".into(),
                    view: View::Session("orch".into()),
                },
                NavItem {
                    label: "Issues".into(),
                    view: View::Issues,
                },
            ],
            workspaces: vec![WorkspaceRow {
                name: "alpha-1".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: Some("T-1".into()),
                agent: Some("developer".into()),
                badge: WorkspaceBadge::Working,
            }],
            reviews: vec![ReviewRow {
                task_id: "T-9".into(),
                title: "Review me".into(),
                branch: "user/T-9".into(),
                location: None,
                workspace: None,
                state: ReviewState::Pending,
            }],
            config_error: None,
            board_loading: false,
            collapsed_machines: Default::default(),
            board_banner: None,
            daemon_version_line: None,
            daemon_version_mismatch: false,
            status_line: String::new(),
            zen_mode: shelbi_state::ZenModeState::On,
            unread_errors: 0,
        };
        let m = build_command_model("alpha", &sidebar);
        assert_eq!(m.project.as_deref(), Some("alpha"));
        assert!(m.zen_on);
        let view_ids: Vec<&str> = m.views.iter().map(|v| v.id.as_str()).collect();
        assert!(view_ids.contains(&"view:orch"));
        assert!(view_ids.contains(&"view:tasks"));
        // Machines is palette-only now: it isn't in the sidebar nav, but
        // `build_command_model` adds its command so Ctrl+P → Machines works.
        assert!(
            view_ids.contains(&"view:machines"),
            "Machines reachable from the palette, got: {view_ids:?}"
        );
        let machines = m
            .views
            .iter()
            .find(|v| v.id == "view:machines")
            .expect("machines command present");
        assert_eq!(machines.view, View::Machines);
        assert_eq!(machines.title, "Machines");
        assert_eq!(m.workspaces[0].name, "alpha-1");
        assert_eq!(m.reviews[0].task_id, "T-9");
    }

    /// Guard against a double entry should a future sidebar nav reintroduce a
    /// Machines row: the palette must still list exactly one Machines command.
    #[test]
    fn command_model_does_not_duplicate_machines_when_nav_has_it() {
        let sidebar = SidebarModel {
            project_label: "alpha".into(),
            nav: vec![
                NavItem {
                    label: "Issues".into(),
                    view: View::Issues,
                },
                NavItem {
                    label: "Machines".into(),
                    view: View::Machines,
                },
            ],
            workspaces: Vec::new(),
            reviews: Vec::new(),
            config_error: None,
            board_loading: false,
            collapsed_machines: Default::default(),
            board_banner: None,
            daemon_version_line: None,
            daemon_version_mismatch: false,
            status_line: String::new(),
            zen_mode: shelbi_state::ZenModeState::Off,
            unread_errors: 0,
        };
        let m = build_command_model("alpha", &sidebar);
        let machines = m.views.iter().filter(|v| v.id == "view:machines").count();
        assert_eq!(machines, 1, "exactly one Machines command");
    }
}
