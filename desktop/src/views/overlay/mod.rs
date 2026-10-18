//! The overlay layer: the command palette and the five dialogs (error log,
//! review load, reject reason, Zen intro, add project).
//!
//! One overlay is open at a time, held by the window as an [`ActiveOverlay`]
//! and mirrored into its `ClientState` as a `shelbi_app::nav::Overlay`. Each
//! variant wraps the shared state machine from `shelbi_app::overlay`, the same
//! one the TUI drives, so keys mean the same thing in both clients; this crate
//! only paints them and turns what they resolve to ([`OverlayEvent`]) into
//! work. While an overlay is open it takes every key; Esc closes it and focus
//! returns where it was.
//!
//! [`ActiveOverlay`] and its key routing are toolkit-free (tested without a
//! window). [`render_layer`] draws the open overlay over a dimmed backdrop;
//! the per-overlay views live beside this file and in `views::palette`.

pub mod add_project;
pub mod error_log;
pub mod review_confirm;
pub mod review_reject;
pub mod zen_intro;

use gpui::{div, prelude::*, px, AnyElement, Context, Hsla, MouseButton, ScrollHandle, Window};
use shelbi_app::nav;
use shelbi_app::overlay::add_project::{self as form, Form};
use shelbi_app::overlay::error_log::{ErrorLogOutcome, Viewer};
use shelbi_app::overlay::palette::{Palette, PaletteStep, ProjectEntry};
use shelbi_app::overlay::review_confirm::{self as confirm, Dialog, Slot};
use shelbi_app::overlay::review_reject::{self as reject, RejectPrompt};
use shelbi_app::overlay::zen_intro::{self as zen, IntroOutcome, IntroState};
use shelbi_core::ConfigMode;
use shelbi_palette::Entry;
use shelbi_state::keymap::{Key, KeyChord, Keymaps};

use crate::theme::style::{self, ButtonKind};
use crate::theme::Theme;
use crate::theme::style::{Type, Typography};

/// What the window should do after an overlay took input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayEvent {
    /// Keep the overlay open.
    Stay,
    /// Close it; focus returns where it was.
    Close,
    /// Close it and focus the sidebar (the palette's Tab).
    FocusSidebar,
    /// The palette activated this entry: close, then run its command.
    RunEntry(Entry),
    /// The review-load dialog chose a slot.
    LoadReview { task_id: String, slot: String },
    /// The reject prompt submitted a reason.
    Rejected { task_id: String, reason: String },
    /// The Zen intro resolved.
    ZenResult {
        project: String,
        confirmed: bool,
        dont_show_again: bool,
    },
    /// The add-project form submitted; the window validates and scaffolds.
    AddProjectSubmit {
        name: String,
        root: String,
        mode: ConfigMode,
    },
    /// The error log's `c`: clear the project's on-disk log.
    ClearErrors { project: String },
    /// Put this text on the clipboard (the error log's copy).
    Copy(String),
}

/// The open overlay and its state.
pub enum ActiveOverlay {
    Palette {
        palette: Palette,
        scroll: ScrollHandle,
    },
    /// `viewer` is `None` until the log has been read (off the UI thread).
    ErrorLog {
        project: String,
        viewer: Option<Viewer>,
        scroll: ScrollHandle,
    },
    ReviewConfirm { task_id: String, dialog: Dialog },
    RejectReason {
        task_id: String,
        prompt: RejectPrompt,
    },
    ZenIntro { project: String, state: IntroState },
    /// `busy` while a submit is validating and scaffolding.
    AddProject { form: Form, busy: bool },
}

impl ActiveOverlay {
    pub fn palette(entries: Vec<Entry>, projects: Vec<ProjectEntry>) -> Self {
        ActiveOverlay::Palette {
            // Tab crosses to the Projects column, as the frame's footer says
            // (the TUI's Tab still goes to the sidebar).
            palette: Palette::new(entries, projects).tab_switches_columns(),
            scroll: ScrollHandle::new(),
        }
    }

    pub fn error_log(project: impl Into<String>) -> Self {
        ActiveOverlay::ErrorLog {
            project: project.into(),
            viewer: None,
            scroll: ScrollHandle::new(),
        }
    }

    /// The "Load for review" dialog over `free` slots; with none free, the
    /// report the TUI shows (none configured, or every slot busy).
    pub fn review_confirm(
        task_id: impl Into<String>,
        title: impl Into<String>,
        free: Vec<Slot>,
        total_slots: usize,
    ) -> Self {
        let dialog = if free.is_empty() && total_slots > 0 {
            Dialog::informational(title, BUSY_SLOTS)
        } else {
            Dialog::new(title, free)
        };
        ActiveOverlay::ReviewConfirm {
            task_id: task_id.into(),
            dialog,
        }
    }

    pub fn reject_reason(task_id: impl Into<String>) -> Self {
        ActiveOverlay::RejectReason {
            task_id: task_id.into(),
            prompt: RejectPrompt::new(),
        }
    }

    pub fn zen_intro(project: impl Into<String>) -> Self {
        ActiveOverlay::ZenIntro {
            project: project.into(),
            state: IntroState::default(),
        }
    }

    pub fn add_project(cwd: &std::path::Path) -> Self {
        ActiveOverlay::AddProject {
            form: Form::new(cwd),
            busy: false,
        }
    }

    /// The navigation model's name for this overlay.
    pub fn nav(&self) -> nav::Overlay {
        match self {
            ActiveOverlay::Palette { .. } => nav::Overlay::Palette,
            ActiveOverlay::ErrorLog { .. } => nav::Overlay::ErrorLog,
            ActiveOverlay::ReviewConfirm { task_id, .. } => nav::Overlay::ReviewConfirm {
                task_id: task_id.clone(),
            },
            ActiveOverlay::RejectReason { task_id, .. } => nav::Overlay::RejectReason {
                task_id: task_id.clone(),
            },
            ActiveOverlay::ZenIntro { .. } => nav::Overlay::ZenIntro,
            ActiveOverlay::AddProject { .. } => nav::Overlay::AddProject,
        }
    }

    /// Whether a paste's line breaks are Enter (the reject prompt) rather
    /// than dropped.
    pub fn takes_newlines(&self) -> bool {
        matches!(self, ActiveOverlay::RejectReason { .. })
    }

    /// Feed one key. `keymaps` resolves the palette's bindings (and the chords
    /// that toggle it closed).
    pub fn handle_key(&mut self, chord: KeyChord, keymaps: &Keymaps) -> OverlayEvent {
        match self {
            ActiveOverlay::Palette { palette, scroll } => {
                // The chord that opened the palette closes it again.
                if shelbi_app::overlay::opens_palette(keymaps, &chord) {
                    return OverlayEvent::Close;
                }
                let action = keymaps.palette.dispatch(chord);
                let step = palette.handle_key(chord, action);
                let count = palette.results().len();
                if count > 0 {
                    scroll.scroll_to_item(palette.selected_in(count));
                }
                palette_event(step)
            }
            ActiveOverlay::ErrorLog {
                project,
                viewer,
                scroll,
            } => match viewer {
                // Still reading: only Esc / q close.
                None => match chord.code {
                    Key::Esc | Key::Char('q') => OverlayEvent::Close,
                    _ => OverlayEvent::Stay,
                },
                Some(viewer) => {
                    // The wheel scrolls natively; start from where it left off.
                    let line = error_log::LINE_H;
                    viewer.set_scroll((-scroll.offset().y.as_f32() / line).round().max(0.0) as usize);
                    let outcome = viewer.handle_key(chord);
                    let max_lines = (scroll.max_offset().y.as_f32() / line).ceil().max(0.0) as usize;
                    viewer.clamp(max_lines, 0);
                    scroll.set_offset(gpui::point(px(0.0), px(-(viewer.scroll() as f32) * line)));
                    match outcome {
                        ErrorLogOutcome::Continue => OverlayEvent::Stay,
                        ErrorLogOutcome::Close => OverlayEvent::Close,
                        ErrorLogOutcome::Clear => OverlayEvent::ClearErrors {
                            project: project.clone(),
                        },
                    }
                }
            },
            ActiveOverlay::ReviewConfirm { task_id, dialog } => {
                confirm_event(task_id, dialog.handle_key(chord))
            }
            ActiveOverlay::RejectReason { task_id, prompt } => {
                let step = prompt.handle_key(chord);
                reject_event(task_id, prompt, step)
            }
            ActiveOverlay::ZenIntro { project, state } => {
                let outcome = zen::step_intro(state, chord);
                zen_event(project, state, outcome)
            }
            ActiveOverlay::AddProject { form, busy } => {
                if *busy {
                    return OverlayEvent::Stay;
                }
                match form.handle_key(chord) {
                    form::Step::Continue => OverlayEvent::Stay,
                    form::Step::Cancel => OverlayEvent::Close,
                    form::Step::Submit => OverlayEvent::AddProjectSubmit {
                        name: form.name().to_string(),
                        root: form.root().to_string(),
                        mode: form.mode(),
                    },
                }
            }
        }
    }

    /// Whether a click on the backdrop closes this overlay. The palette and
    /// the error log close; the dialogs that hold typed input or a pending
    /// decision ignore a stray click.
    pub fn closes_on_backdrop(&self) -> bool {
        matches!(
            self,
            ActiveOverlay::Palette { .. } | ActiveOverlay::ErrorLog { .. }
        )
    }
}

/// The report when every review slot is busy (the TUI's line).
pub const BUSY_SLOTS: &str = "Every review slot is busy — free one to load this task.";

pub(crate) fn palette_event(step: PaletteStep) -> OverlayEvent {
    match step {
        PaletteStep::Continue => OverlayEvent::Stay,
        PaletteStep::Close => OverlayEvent::Close,
        PaletteStep::FocusSidebar => OverlayEvent::FocusSidebar,
        PaletteStep::Activate(entry) => OverlayEvent::RunEntry(entry),
    }
}

pub(crate) fn confirm_event(task_id: &str, step: confirm::Step) -> OverlayEvent {
    match step {
        confirm::Step::Continue => OverlayEvent::Stay,
        confirm::Step::Done(confirm::Outcome::Cancel) => OverlayEvent::Close,
        confirm::Step::Done(confirm::Outcome::Load(slot)) => OverlayEvent::LoadReview {
            task_id: task_id.to_string(),
            slot,
        },
    }
}

pub(crate) fn reject_event(task_id: &str, prompt: &RejectPrompt, step: reject::Step) -> OverlayEvent {
    match step {
        reject::Step::Continue => OverlayEvent::Stay,
        reject::Step::Cancel => OverlayEvent::Close,
        reject::Step::Submit => OverlayEvent::Rejected {
            task_id: task_id.to_string(),
            reason: prompt.reason(),
        },
    }
}

pub(crate) fn zen_event(project: &str, state: &IntroState, outcome: IntroOutcome) -> OverlayEvent {
    match outcome {
        IntroOutcome::Continue => OverlayEvent::Stay,
        IntroOutcome::Cancelled | IntroOutcome::Confirmed => OverlayEvent::ZenResult {
            project: project.to_string(),
            confirmed: outcome == IntroOutcome::Confirmed,
            dont_show_again: state.dont_show_again,
        },
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The window an overlay is drawn into: it owns the [`ActiveOverlay`] and acts
/// on what it resolves to.
pub trait OverlayHost: 'static + Sized {
    fn overlay_mut(&mut self) -> Option<&mut ActiveOverlay>;
    fn overlay_event(&mut self, event: OverlayEvent, window: &mut Window, cx: &mut Context<Self>);
}

/// Run `f` on the open overlay, then hand what it resolved to the host. The
/// overlays' click handlers go through this.
pub fn with_overlay<H: OverlayHost>(
    host: &mut H,
    window: &mut Window,
    cx: &mut Context<H>,
    f: impl FnOnce(&mut ActiveOverlay) -> OverlayEvent,
) {
    let Some(overlay) = host.overlay_mut() else {
        return;
    };
    let event = f(overlay);
    host.overlay_event(event, window, cx);
    cx.notify();
}

/// The dimmed backdrop's color: the palette's scrim (Taskpage's slate-700
/// at partial strength; black-700 in dark).
fn backdrop(theme: &Theme) -> Hsla {
    theme.scrim
}

/// Draw `overlay` over the whole window: a dimmed backdrop that swallows the
/// mouse, with the overlay's panel on it. `size` is the window's size in
/// pixels, which the panels size themselves against (as the TUI's overlays
/// size against the terminal).
///
/// `entering` is the entrance's progress (0..=1, eased) while the overlay
/// slides in (Taskpage's slide-up-and-fade for the panel, a fade for the
/// scrim), `None` once it is in.
pub fn render_layer<H: OverlayHost>(
    overlay: &ActiveOverlay,
    size: gpui::Size<gpui::Pixels>,
    entering: Option<f32>,
    keymaps: &Keymaps,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let (panel, top_aligned) = match overlay {
        ActiveOverlay::Palette { palette, scroll } => (
            crate::views::palette::render(palette, scroll, size, keymaps, theme, cx),
            true,
        ),
        ActiveOverlay::ErrorLog { viewer, scroll, .. } => {
            (error_log::render(viewer.as_ref(), scroll, size, theme, cx), false)
        }
        ActiveOverlay::ReviewConfirm { dialog, .. } => {
            (review_confirm::render(dialog, theme, cx), false)
        }
        ActiveOverlay::RejectReason { prompt, .. } => {
            (review_reject::render(prompt, theme, cx), false)
        }
        ActiveOverlay::ZenIntro { state, .. } => (zen_intro::render(state, theme, cx), false),
        ActiveOverlay::AddProject { form, busy } => {
            (add_project::render(form, *busy, size, theme, cx), false)
        }
    };
    let closes = overlay.closes_on_backdrop();
    let phase = entering.unwrap_or(1.0);
    let panel = div()
        .relative()
        .top(px((1.0 - phase) * crate::theme::motion::SLIDE_PX))
        .opacity(phase)
        .child(panel);
    div()
        .id("overlay-layer")
        .debug_selector(|| "overlay-layer".into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .bg(backdrop(theme).opacity(phase))
        .flex()
        .flex_col()
        .items_center()
        .when(top_aligned, |d| d.justify_start().pt(px(palette_top(size.height.as_f32()))))
        .when(!top_aligned, |d| d.justify_center())
        .font_family("Geist")
        .type_style(Type::Base)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this: &mut H, _, window, cx| {
                if closes {
                    with_overlay(this, window, cx, |_| OverlayEvent::Close);
                }
            }),
        )
        .child(panel)
        .into_any_element()
}

/// The palette's offset from the top of a window `window_h` tall: the
/// frame's 200pt, a fifth of the way down a shorter window, at least 40pt.
pub fn palette_top(window_h: f32) -> f32 {
    (window_h * 0.2).clamp(40.0, 200.0).round()
}

/// A panel: the overlay's raised surface (Taskpage's ring and shadow), that
/// stops clicks on it from reaching the backdrop.
pub fn panel(id: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    style::dialog(
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .flex_col()
            .overflow_hidden(),
        theme,
    )
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A dialog button: a Taskpage button of `kind`, wearing the focus ring
/// while it has the keyboard. A shared label's TUI brackets (`[ Load ]`)
/// are the terminal's button shape, so they're dropped here.
pub fn button(
    id: &'static str,
    label: &str,
    focused: bool,
    kind: ButtonKind,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    style::button(id, button_label(label), kind, focused, theme)
}

/// A shared button label without the TUI's brackets.
pub fn button_label(label: &str) -> String {
    label
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_string()
}

/// The panel's title line (the TUI's border title).
pub fn title(text: impl Into<gpui::SharedString>, theme: &Theme) -> gpui::Div {
    style::heading(text, theme).px_4().pt_4().pb_1()
}

/// The hint line at a panel's foot.
pub fn hint(text: impl Into<gpui::SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .px_4()
        .pb_3()
        .pt_1()
        .type_style(Type::XSmall)
        .text_color(theme.muted)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_palette::EntryKind;
    use shelbi_state::keymap::Mods;

    fn keymaps() -> Keymaps {
        shelbi_state::keymap::validate_keymaps_yaml("", None).0
    }

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    fn typed(o: &mut ActiveOverlay, text: &str) {
        for c in text.chars() {
            assert_eq!(
                o.handle_key(KeyChord::new(Key::Char(c), Mods::NONE), &keymaps()),
                OverlayEvent::Stay
            );
        }
    }

    fn entry(id: &str, label: &str) -> Entry {
        Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        }
    }

    #[test]
    fn the_palette_runs_the_selected_entry_and_toggles_closed_on_its_chord() {
        let mut o = ActiveOverlay::palette(
            vec![entry("view:tasks", "Issues"), entry("action:error-log", "Open error log")],
            Vec::new(),
        );
        assert_eq!(o.nav(), nav::Overlay::Palette);
        typed(&mut o, "err");
        assert_eq!(
            o.handle_key(chord("enter"), &keymaps()),
            OverlayEvent::RunEntry(entry("action:error-log", "Open error log"))
        );
        for close in ["esc", "ctrl-p", "ctrl-space", "super-k"] {
            assert_eq!(o.handle_key(chord(close), &keymaps()), OverlayEvent::Close, "{close}");
        }
        // No Projects column: Tab hands focus to the sidebar.
        assert_eq!(o.handle_key(chord("tab"), &keymaps()), OverlayEvent::FocusSidebar);
    }

    #[test]
    fn tab_crosses_into_the_projects_column_and_back() {
        let project = ProjectEntry {
            slug: "web".into(),
            label: "Web".into(),
            indicator: shelbi_app::overlay::palette::ProjectIndicator::Unloaded,
        };
        let mut o = ActiveOverlay::palette(vec![entry("view:tasks", "Issues")], vec![project]);
        assert_eq!(o.handle_key(chord("tab"), &keymaps()), OverlayEvent::Stay);
        let ActiveOverlay::Palette { palette, .. } = &o else { unreachable!() };
        assert_eq!(palette.project_selection(), Some(0));
        assert_eq!(
            o.handle_key(chord("enter"), &keymaps()),
            OverlayEvent::RunEntry(entry("action:switch-project:web", "Web"))
        );
        o.handle_key(chord("tab"), &keymaps());
        let ActiveOverlay::Palette { palette, .. } = &o else { unreachable!() };
        assert_eq!(palette.project_selection(), None, "back on the commands");
    }

    #[test]
    fn the_error_log_waits_for_its_read_then_scrolls_and_clears() {
        let mut o = ActiveOverlay::error_log("alpha");
        assert_eq!(o.nav(), nav::Overlay::ErrorLog);
        assert_eq!(o.handle_key(chord("c"), &keymaps()), OverlayEvent::Stay, "nothing to clear yet");
        if let ActiveOverlay::ErrorLog { viewer, .. } = &mut o {
            *viewer = Some(Viewer::new(Vec::new()));
        }
        assert_eq!(o.handle_key(chord("j"), &keymaps()), OverlayEvent::Stay);
        assert_eq!(
            o.handle_key(chord("c"), &keymaps()),
            OverlayEvent::ClearErrors {
                project: "alpha".into()
            }
        );
        assert_eq!(o.handle_key(chord("q"), &keymaps()), OverlayEvent::Close);
        assert!(o.closes_on_backdrop());
    }

    #[test]
    fn the_review_dialog_loads_the_chosen_slot_or_reports_busy_slots() {
        let free = vec![
            Slot {
                name: "review-1".into(),
                occupant: None,
            },
            Slot {
                name: "review-2".into(),
                occupant: None,
            },
        ];
        let mut o = ActiveOverlay::review_confirm("T-1", "Fix it", free, 2);
        assert_eq!(
            o.nav(),
            nav::Overlay::ReviewConfirm {
                task_id: "T-1".into()
            }
        );
        o.handle_key(chord("down"), &keymaps());
        assert_eq!(
            o.handle_key(chord("enter"), &keymaps()),
            OverlayEvent::LoadReview {
                task_id: "T-1".into(),
                slot: "review-2".into()
            }
        );
        let mut busy = ActiveOverlay::review_confirm("T-1", "Fix it", Vec::new(), 2);
        let ActiveOverlay::ReviewConfirm { dialog, .. } = &busy else {
            unreachable!()
        };
        assert_eq!(dialog.empty_message(), BUSY_SLOTS);
        assert_eq!(busy.handle_key(chord("x"), &keymaps()), OverlayEvent::Close);
        let none = ActiveOverlay::review_confirm("T-1", "Fix it", Vec::new(), 0);
        let ActiveOverlay::ReviewConfirm { dialog, .. } = &none else {
            unreachable!()
        };
        assert_eq!(dialog.empty_message(), confirm::NO_WORKSPACE);
        assert!(!none.closes_on_backdrop());
    }

    #[test]
    fn the_reject_prompt_submits_its_reason() {
        let mut o = ActiveOverlay::reject_reason("T-2");
        assert!(o.takes_newlines());
        typed(&mut o, "fails");
        o.handle_key(chord("enter"), &keymaps());
        typed(&mut o, "on CI");
        assert_eq!(
            o.handle_key(chord("ctrl-d"), &keymaps()),
            OverlayEvent::Rejected {
                task_id: "T-2".into(),
                reason: "fails\non CI".into()
            }
        );
        assert_eq!(o.handle_key(chord("esc"), &keymaps()), OverlayEvent::Close);
    }

    #[test]
    fn the_zen_intro_reports_its_answer_and_checkbox() {
        let mut o = ActiveOverlay::zen_intro("alpha");
        o.handle_key(chord("tab"), &keymaps());
        o.handle_key(chord("space"), &keymaps());
        o.handle_key(KeyChord::new(Key::BackTab, Mods::SHIFT), &keymaps());
        assert_eq!(
            o.handle_key(chord("enter"), &keymaps()),
            OverlayEvent::ZenResult {
                project: "alpha".into(),
                confirmed: true,
                dont_show_again: true
            }
        );
        assert_eq!(
            ActiveOverlay::zen_intro("alpha").handle_key(chord("esc"), &keymaps()),
            OverlayEvent::ZenResult {
                project: "alpha".into(),
                confirmed: false,
                dont_show_again: false
            }
        );
    }

    #[test]
    fn the_add_project_form_submits_its_values_and_holds_while_busy() {
        let mut o = ActiveOverlay::add_project(std::path::Path::new("/tmp/web"));
        assert_eq!(o.nav(), nav::Overlay::AddProject);
        typed(&mut o, "Web");
        assert_eq!(
            o.handle_key(chord("enter"), &keymaps()),
            OverlayEvent::AddProjectSubmit {
                name: "Web".into(),
                root: "/tmp/web".into(),
                mode: ConfigMode::Global
            }
        );
        if let ActiveOverlay::AddProject { busy, .. } = &mut o {
            *busy = true;
        }
        assert_eq!(o.handle_key(chord("esc"), &keymaps()), OverlayEvent::Stay);
    }
}

/// The overlays rendered in a gpui test window.
#[cfg(test)]
mod render_tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use shelbi_app::command::{ProjectItem, ReviewItem};
    use shelbi_app::command_model::command_model_from;
    use shelbi_app::overlay::palette::ProjectIndicator;
    use shelbi_app::view::{NavItem, SidebarModel};
    use shelbi_app::{CommandRegistry, View};

    /// A window that draws only the overlay layer, recording what it resolves to.
    struct Fixture {
        overlay: Option<ActiveOverlay>,
        events: Vec<OverlayEvent>,
    }

    impl OverlayHost for Fixture {
        fn overlay_mut(&mut self) -> Option<&mut ActiveOverlay> {
            self.overlay.as_mut()
        }
        fn overlay_event(&mut self, event: OverlayEvent, _: &mut Window, _: &mut Context<Self>) {
            if event != OverlayEvent::Stay {
                self.events.push(event);
            }
        }
    }

    impl Render for Fixture {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::resolve(
                shelbi_state::ThemePreference::Dark,
                gpui::WindowAppearance::Dark,
            );
            let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
            let size = window.viewport_size();
            div()
                .relative()
                .size_full()
                .children(self.overlay.as_ref().map(|o| render_layer(o, size, None, &keymaps, &theme, cx)))
        }
    }

    fn open(cx: &mut TestAppContext, overlay: ActiveOverlay) -> (gpui::Entity<Fixture>, VisualTestContext) {
        let window = cx.update(|cx| {
            cx.open_window(
                gpui::WindowOptions {
                    window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds {
                        origin: gpui::point(px(0.0), px(0.0)),
                        size: gpui::size(px(1200.0), px(800.0)),
                    })),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|_| Fixture {
                        overlay: Some(overlay),
                        events: Vec::new(),
                    })
                },
            )
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let root = window.root(&mut vcx).unwrap();
        vcx.run_until_parked();
        (root, vcx)
    }

    /// The palette over a fixture state's registry entries.
    fn fixture_palette() -> ActiveOverlay {
        let mut sidebar = SidebarModel::skeleton("alpha");
        sidebar.board_loading = false;
        sidebar.nav = vec![
            NavItem {
                label: "Orchestrator".into(),
                view: View::Session("orch".into()),
            },
            NavItem {
                label: "Issues".into(),
                view: View::Issues,
            },
        ];
        let mut model = command_model_from(
            "alpha",
            &sidebar,
            false,
            vec![ProjectItem {
                slug: "beta".into(),
                label: "Beta".into(),
            }],
            Vec::new(),
        );
        model.reviews.push(ReviewItem {
            task_id: "T-9".into(),
            title: "Ship it".into(),
            subtitle: None,
            decoration: None,
        });
        let projects = vec![
            PaletteProject("alpha", "Alpha", ProjectIndicator::Active),
            PaletteProject("beta", "Beta", ProjectIndicator::Unloaded),
        ]
        .into_iter()
        .map(|PaletteProject(slug, label, indicator)| ProjectEntry {
            slug: slug.into(),
            label: label.into(),
            indicator,
        })
        .collect();
        ActiveOverlay::palette(
            crate::views::palette::present(CommandRegistry::new().entries(&model)),
            projects,
        )
    }

    struct PaletteProject(&'static str, &'static str, ProjectIndicator);

    #[gpui::test]
    fn the_palette_lists_the_registrys_entries_in_order(cx: &mut TestAppContext) {
        let (_, mut cx) = open(cx, fixture_palette());
        let ids = [
            "view:orch",
            "view:tasks",
            "view:machines",
            "action:toggle-zen",
            "action:error-log",
            "action:detach",
            "action:quit-project",
            "action:quit-shelbi",
        ];
        let mut last = None;
        for id in ids {
            let selector: &'static str = Box::leak(format!("palette-row-{id}").into_boxed_str());
            let bounds = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{id} is drawn"));
            if let Some(prev) = last {
                assert!(bounds.top() > prev, "{id} is below the row before it");
            }
            last = Some(bounds.top());
        }
        // Hidden-until-typed entries aren't drawn on a blank query.
        // The review rows, too: the frame's list is the commands alone.
        for hidden in ["action:switch-project:beta", "action:approve-review:T-9", "review:T-9"] {
            let selector: &'static str = Box::leak(format!("palette-row-{hidden}").into_boxed_str());
            assert!(cx.debug_bounds(selector).is_none(), "{hidden} waits for a query");
        }
        // The first row is selected; the Projects column and footer show.
        let first = cx.debug_bounds("palette-row-view:orch").unwrap();
        let selected = cx.debug_bounds("palette-selected").unwrap();
        assert!(
            selected.top() >= first.top() && selected.bottom() <= first.bottom(),
            "the selection marks the first row"
        );
        assert!(cx.debug_bounds("palette-project-Alpha").is_some());
        assert!(cx.debug_bounds("palette-project-Add project").is_some());
        assert!(cx.debug_bounds("palette-footer").is_some());
        // Top-aligned, not centered.
        let panel = cx.debug_bounds("palette").unwrap();
        let window_h = cx.update(|window, _| window.viewport_size().height.as_f32());
        assert_eq!(panel.top(), px(palette_top(window_h)));
    }

    #[gpui::test]
    fn no_match_sits_in_the_first_rows_slot(cx: &mut TestAppContext) {
        let mut palette = fixture_palette();
        let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        for c in "xyzzy".chars() {
            let chord = KeyChord::new(Key::Char(c), shelbi_state::keymap::Mods::NONE);
            assert_eq!(palette.handle_key(chord, &keymaps), OverlayEvent::Stay);
        }
        let (_, mut cx) = open(cx, palette);
        let query = cx.debug_bounds("palette-query").unwrap();
        let message = cx.debug_bounds("palette-no-match").unwrap();
        // Right under the query line, as the first row would be, though the
        // Projects column makes the band taller.
        assert_eq!(message.top(), query.bottom());
        assert_eq!(message.size.height, px(crate::views::palette::ROW_H));
        assert!(cx.debug_bounds("palette-project-Add project").unwrap().bottom() > message.bottom());
    }

    #[gpui::test]
    fn clicking_a_palette_row_runs_it(cx: &mut TestAppContext) {
        let (root, mut cx) = open(cx, fixture_palette());
        let zen = cx.debug_bounds("palette-row-action:toggle-zen").unwrap();
        cx.simulate_click(zen.center(), Modifiers::none());
        let events = root.read_with(&cx, |f, _| f.events.clone());
        let [OverlayEvent::RunEntry(entry)] = events.as_slice() else {
            panic!("one run, got {events:?}");
        };
        assert_eq!(entry.id, "action:toggle-zen");
    }

    #[gpui::test]
    fn the_backdrop_closes_the_palette_but_not_a_form(cx: &mut TestAppContext) {
        let (root, mut vcx) = open(cx, fixture_palette());
        vcx.simulate_click(gpui::point(px(5.0), px(790.0)), Modifiers::none());
        assert_eq!(root.read_with(&vcx, |f, _| f.events.clone()), [OverlayEvent::Close]);

        let (root, mut vcx) = open(cx, ActiveOverlay::reject_reason("T-1"));
        vcx.simulate_click(gpui::point(px(5.0), px(790.0)), Modifiers::none());
        assert!(root.read_with(&vcx, |f, _| f.events.is_empty()), "a stray click keeps the reason");
    }

    #[gpui::test]
    fn each_dialog_draws_its_controls(cx: &mut TestAppContext) {
        let one_slot = vec![Slot {
            name: "review".into(),
            occupant: None,
        }];
        let two_slots = vec![
            Slot {
                name: "review-1".into(),
                occupant: None,
            },
            Slot {
                name: "review-2".into(),
                occupant: None,
            },
        ];
        let cases: Vec<(ActiveOverlay, Vec<&'static str>)> = vec![
            (
                ActiveOverlay::review_confirm("T-1", "Fix it", one_slot, 1),
                vec!["review-confirm", "review-confirm-message", "review-confirm-load", "review-confirm-cancel"],
            ),
            (
                ActiveOverlay::review_confirm("T-1", "Fix it", two_slots, 2),
                vec!["review-confirm-slot-review-1", "review-confirm-slot-review-2", "review-confirm-load"],
            ),
            (
                ActiveOverlay::review_confirm("T-1", "Fix it", Vec::new(), 2),
                vec!["review-confirm-message", "review-confirm-dismiss"],
            ),
            (
                ActiveOverlay::reject_reason("T-1"),
                vec!["review-reject", "reject-text", "reject-caret", "reject-submit", "reject-cancel"],
            ),
            (
                ActiveOverlay::zen_intro("alpha"),
                vec!["zen-intro", "zen-checkbox", "zen-cancel", "zen-enable"],
            ),
            (
                ActiveOverlay::add_project(std::path::Path::new("/tmp/web")),
                vec!["add-project", "add-project-name", "add-project-root", "add-project-config", "add-project-status"],
            ),
            (ActiveOverlay::error_log("alpha"), vec!["error-log", "error-log-copy", "error-log-clear"]),
        ];
        for (overlay, selectors) in cases {
            let (_, mut vcx) = open(cx, overlay);
            for selector in selectors {
                assert!(vcx.debug_bounds(selector).is_some(), "{selector} is drawn");
            }
        }
    }

    #[gpui::test]
    fn the_error_log_marks_unread_rows(cx: &mut TestAppContext) {
        let mut overlay = ActiveOverlay::error_log("alpha");
        if let ActiveOverlay::ErrorLog { viewer, .. } = &mut overlay {
            let entry = |ts: &str, message: &str| shelbi_state::ErrorLogEntry {
                ts: ts.into(),
                message: message.into(),
                source: None,
            };
            *viewer = Some(Viewer::new(vec![
                (entry("2026-10-14T10:00:00Z", "newest"), true),
                (entry("2026-10-13T10:00:00Z", "older"), false),
            ]));
        }
        let (root, mut cx) = open(cx, overlay);
        assert!(cx.debug_bounds("error-log-row-0").is_some());
        assert!(cx.debug_bounds("error-log-unread-0").is_some());
        assert!(cx.debug_bounds("error-log-unread-1").is_none());
        let clear = cx.debug_bounds("error-log-clear").unwrap();
        cx.simulate_click(clear.center(), Modifiers::none());
        assert_eq!(
            root.read_with(&cx, |f, _| f.events.clone()),
            [OverlayEvent::ClearErrors {
                project: "alpha".into()
            }]
        );
    }

    #[gpui::test]
    fn clicking_the_dialogs_buttons_resolves_them(cx: &mut TestAppContext) {
        let (root, mut vcx) = open(cx, ActiveOverlay::zen_intro("alpha"));
        let enable = vcx.debug_bounds("zen-enable").unwrap();
        vcx.simulate_click(enable.center(), Modifiers::none());
        assert_eq!(
            root.read_with(&vcx, |f, _| f.events.clone()),
            [OverlayEvent::ZenResult {
                project: "alpha".into(),
                confirmed: true,
                dont_show_again: false
            }]
        );

        let slots = vec![Slot {
            name: "review".into(),
            occupant: None,
        }];
        let (root, mut vcx) = open(cx, ActiveOverlay::review_confirm("T-1", "Fix it", slots, 1));
        let load = vcx.debug_bounds("review-confirm-load").unwrap();
        vcx.simulate_click(load.center(), Modifiers::none());
        assert_eq!(
            root.read_with(&vcx, |f, _| f.events.clone()),
            [OverlayEvent::LoadReview {
                task_id: "T-1".into(),
                slot: "review".into()
            }]
        );
    }
}
