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
use ratatui::Frame;

use shelbi_app::command::{
    CommandModel, EditItem, ProjectItem, ReviewItem, ViewItem, WorkspaceItem,
};
use shelbi_app::exec::EditTarget;
use shelbi_app::view::SidebarModel;
use shelbi_palette::Entry;
use shelbi_state::keymap::Keymaps;

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
}

/// The overlay currently drawn over the main area, with its runtime state.
pub enum ActiveOverlay {
    Palette(overlay::palette::Palette),
    // The two review overlays are opened by the review interface (rt-tui-review,
    // Phase 4e), which isn't wired in this subtask; they are complete and
    // unit-tested here (they return their result as a value), so the
    // not-yet-constructed warning is expected until 4e lands.
    #[allow(dead_code)]
    ReviewConfirm {
        task_id: String,
        dialog: overlay::review_confirm::Dialog,
    },
    #[allow(dead_code)]
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
}

impl ActiveOverlay {
    /// Open the command palette over `entries` for `project_label`.
    pub fn palette(project_label: &str, entries: Vec<Entry>) -> Self {
        ActiveOverlay::Palette(overlay::palette::Palette::new(project_label, entries))
    }

    /// Open the "Load for review" dialog for `task_id` over the given slots.
    /// Opened by the review interface (Phase 4e); see the enum note.
    #[allow(dead_code)]
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

    /// Open the reject-reason prompt for `task_id`. Opened by the review
    /// interface (Phase 4e); see the enum note.
    #[allow(dead_code)]
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
        }
    }

    /// Draw the overlay over `area`. Takes `&mut self` because the error-log
    /// viewer clamps its scroll offset against the rendered line count at draw
    /// time.
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let rect = self.rect(area);
        match self {
            ActiveOverlay::Palette(p) => p.render(f, rect),
            ActiveOverlay::ReviewConfirm { dialog, .. } => dialog.render(f, rect),
            ActiveOverlay::RejectReason { prompt, .. } => prompt.render(f, rect),
            ActiveOverlay::ErrorLog { viewer, .. } => overlay::error_log::render(f, rect, viewer),
            ActiveOverlay::ZenIntro { state, .. } => {
                overlay::zen_intro::render_intro(f, rect, state)
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
    let views = sidebar
        .nav
        .iter()
        .map(|n| ViewItem {
            id: format!("view:{}", n.view.as_view_id()),
            title: n.label.clone(),
            view: n.view.clone(),
            decoration: None,
        })
        .collect();

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
        zen_on: sidebar.zen_on,
        zen_shortcut: None,
        views,
        workspaces,
        reviews,
        legacy_agents: Vec::new(),
        other_projects,
        edit_targets: edit_targets(project),
    }
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
    use shelbi_app::nav::View;
    use shelbi_app::view::{NavItem, ReviewRow, WorkspaceRow};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
    fn palette_tab_focuses_sidebar_and_esc_closes() {
        let mut ov = ActiveOverlay::palette("alpha", Vec::new());
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
                    label: "Chat".into(),
                    view: View::Session("orch".into()),
                },
                NavItem {
                    label: "Issues".into(),
                    view: View::Issues,
                },
            ],
            workspaces: vec![WorkspaceRow {
                name: "alpha-1".into(),
                current_task: Some("T-1".into()),
                agent: Some("developer".into()),
            }],
            reviews: vec![ReviewRow {
                task_id: "T-9".into(),
                title: "Review me".into(),
            }],
            zen_on: true,
            unread_errors: 0,
        };
        let m = build_command_model("alpha", &sidebar);
        assert_eq!(m.project.as_deref(), Some("alpha"));
        assert!(m.zen_on);
        let view_ids: Vec<&str> = m.views.iter().map(|v| v.id.as_str()).collect();
        assert!(view_ids.contains(&"view:orch"));
        assert!(view_ids.contains(&"view:tasks"));
        assert_eq!(m.workspaces[0].name, "alpha-1");
        assert_eq!(m.reviews[0].task_id, "T-9");
    }
}
