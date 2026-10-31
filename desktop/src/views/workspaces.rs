//! Settings ▸ Workspaces (`dt-workspace-settings-index`, Figma frame
//! 2523:8879): the project's workspace pool and the machines that host it,
//! as configuration. The runtime Machines view (`views::machines`) stays the
//! place to watch them work; this page is where they're declared.
//!
//! Top to bottom: the Workspaces heading and its description, New workspace
//! beside them, over the workspace table (the name with its worktree under
//! it; the machine with Local or SSH under it; the effective tags, a
//! machine's inherited ones included, or No tags; the effective slot; an
//! edit button and a ⋮ menu with Edit and Remove) and its footnote. Then a
//! rule, and the Machines heading (Add machine) over the machine list (its
//! icon, name, SSH host, root project directory and workspace count; its
//! status badge; Configure) and its footnote.
//!
//! The config is read off the UI thread
//! ([`shelbi_app::settings::workspaces::read`]), and
//! read again each time the page is shown and when the window comes back to
//! the front after an edit sent the user to their editor. Until it lands
//! the table says it's loading; a config that can't be read shows its error
//! and Retry in place of the rows. Each remote's reachability comes from a
//! [`ReachabilityProber`] on its own thread, so a down host reads
//! Unreachable (with SSH's reason) without hiding its configuration or
//! holding up the page.
//!
//! The buttons, rows and menu items ask for a form ([`WorkspacesEvent::Open`]
//! with an [`Intent`]). The window opens the workspace dialogs for the
//! workspace intents (`super::workspace_dialog`); Add machine and Configure
//! open the machine form (`super::machine_form`). A reread that finds the
//! pool changed tells the window
//! ([`WorkspacesEvent::Changed`]) so its sidebar refreshes too.
//!
//! New workspace and each row's edit and ⋮ buttons take the keyboard
//! (Return or Space presses them, Esc hands the keys back to the settings
//! sidebar), and remember which one asked for a dialog: when it closes,
//! focus returns there ([`WorkspacesPage::restore_focus`]), to the renamed
//! row's button after a rename, or to the next row's after a removal.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, ClickEvent, Context, Div, EventEmitter, FocusHandle, FontWeight,
    KeyDownEvent, Role, SharedString, Stateful, Task, Window,
};
use shelbi_app::reachability::{ReachabilityProber, REACHABILITY_BACKOFF_MAX, REACHABILITY_CADENCE};
use shelbi_core::MachineKind;

use super::workflows::{popup, row_button, table, MENU_GAP};
use shelbi_app::settings::workspace_form::Outcome;
use shelbi_app::settings::workspaces::{
    kind_label, read, workspace_count, Intent, Load, MachineRow, MachineStatus, PoolIndex, WorkspaceRow,
    WorkspacesState,
};
use crate::assets::{heroicon, icons};
use crate::elements::placed::placed;
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::Theme;

/// The table's columns (`Workspace columns`, 2523:9166): the machine, the
/// tags, the slot and the buttons; the workspace takes the rest.
const MACHINE_W: f32 = 140.0;
const TAGS_W: f32 = 268.0;
const SLOT_W: f32 = 56.0;
const ACTIONS_W: f32 = 90.0;
/// The tables' corners (`radius/default`).
const TABLE_RADIUS: f32 = 12.0;
/// The headings' title size (Desktop/Heading, 18 / 28 semibold).
const TITLE_SIZE: f32 = 18.0;
/// A machine's icon.
const MACHINE_ICON: f32 = 32.0;
/// How often the page takes the prober's latest results.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What the page tells the settings view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspacesEvent {
    /// Open the form `intent` asks for.
    Open(Intent),
    /// A reread found the pool changed: what the window shows from the
    /// config (its sidebar's workspaces) should follow.
    Changed,
    /// Esc on one of the page's buttons: the keys go back to the settings
    /// sidebar.
    Leave,
}

/// The control a dialog was asked for from, for focus to return to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    New,
    /// A row's edit button (or the row itself).
    Edit(String),
    /// A row's ⋮ button (its menu's items).
    More(String),
}

/// The Workspaces page.
pub struct WorkspacesPage {
    state: WorkspacesState,
    /// The workspace whose ⋮ menu is open.
    menu: Option<String>,
    /// Where the open menu's button last painted (its own click toggles it).
    menu_anchor: style::Anchor,
    /// New workspace's focus, and each row's edit and ⋮ buttons'.
    new_focus: FocusHandle,
    row_focus: HashMap<String, (FocusHandle, FocusHandle)>,
    /// What asked for the open dialog.
    origin: Option<Origin>,
    /// Re-read the config on the next draw (the page was shown again).
    reload: bool,
    /// The remotes' prober, once the project has one; never under test.
    prober: Option<ReachabilityProber>,
    _poll: Option<Task<()>>,
}

impl WorkspacesPage {
    pub fn new(project: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut page = WorkspacesPage {
            state: WorkspacesState::new(project),
            menu: None,
            menu_anchor: Default::default(),
            new_focus: cx.focus_handle(),
            row_focus: HashMap::new(),
            origin: None,
            reload: false,
            prober: None,
            _poll: None,
        };
        // A test's page never reads the real `~/.shelbi`.
        if !cfg!(test) {
            page.load(window, cx);
        }
        page
    }

    /// Read the project's pool off the UI thread (again, to pick up an
    /// edit) and show it.
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.state.project().to_string();
        let read = cx.background_executor().spawn({
            let project = project.clone();
            async move { read(&project) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let index = read.await;
            this.update(cx, |this, cx| this.loaded(&project, index, cx)).ok();
        })
        .detach();
    }

    /// A read landed.
    fn loaded(&mut self, project: &str, index: Result<PoolIndex, String>, cx: &mut Context<Self>) {
        if self.state.loaded(project, index) {
            cx.emit(WorkspacesEvent::Changed);
        }
        self.follow_targets(cx);
        cx.notify();
    }

    /// Re-read the config when next drawn (the page was shown again). A
    /// test's page reads no disk unless told to.
    pub fn reload_when_drawn(&mut self) {
        self.reload = !cfg!(test);
    }

    /// The page's state.
    pub fn state(&self) -> &WorkspacesState {
        &self.state
    }

    /// The page's state: a test folds probe results into it, and a form
    /// that can't open says why on it.
    pub fn state_mut(&mut self) -> &mut WorkspacesState {
        &mut self.state
    }

    /// Open workspace `name`'s ⋮ menu (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_open_menu(&mut self, name: &str, cx: &mut Context<Self>) {
        self.menu = Some(name.to_string());
        cx.notify();
    }

    /// Probe the project's remotes: start the prober for the first one, and
    /// hand it the current set (a no-op when unchanged).
    fn follow_targets(&mut self, cx: &mut Context<Self>) {
        let targets = self.state.probe_targets();
        if self.prober.is_none() {
            if targets.is_empty() || cfg!(test) {
                return;
            }
            self.prober = Some(ReachabilityProber::spawn(
                REACHABILITY_CADENCE,
                REACHABILITY_BACKOFF_MAX,
                |_key: &str, host: &str| super::machines::probe(host),
            ));
            self._poll = Some(cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if this.update(cx, |page, cx| page.take_probes(cx)).is_err() {
                    break;
                }
            }));
        }
        if let Some(prober) = &self.prober {
            prober.set_targets(targets);
        }
    }

    /// Fold the prober's latest results in (its poll timer's tick; the
    /// offscreen harness, whose timers don't run, calls it by hand).
    pub(crate) fn take_probes(&mut self, cx: &mut Context<Self>) {
        if let Some(prober) = &self.prober {
            if self.state.set_reachability(prober.snapshot()) {
                cx.notify();
            }
        }
    }

    /// Ask for the form `intent` names, from `origin`.
    fn open(&mut self, intent: Intent, origin: Origin, cx: &mut Context<Self>) {
        self.menu = None;
        self.origin = Some(origin);
        cx.emit(WorkspacesEvent::Open(intent));
        cx.notify();
    }

    /// Row `name`'s edit and ⋮ buttons' focus.
    fn row_handles(&mut self, name: &str, cx: &mut Context<Self>) -> (FocusHandle, FocusHandle) {
        self.row_focus
            .entry(name.to_string())
            .or_insert_with(|| (cx.focus_handle(), cx.focus_handle()))
            .clone()
    }

    /// A dialog closed (`done`: what it did): give the keyboard back to the
    /// control that asked for it. After a rename that's the renamed row's;
    /// after a removal, the next row's edit button (else the previous one's,
    /// else New workspace).
    pub fn restore_focus(&mut self, done: Option<&Outcome>, window: &mut Window, cx: &mut Context<Self>) {
        let origin = self.origin.take();
        let handle = match (done, origin) {
            (Some(Outcome::Removed(removed)), _) => {
                let rows: Vec<String> = self
                    .state
                    .index()
                    .map(|i| i.workspaces.iter().map(|w| w.name.clone()).collect())
                    .unwrap_or_default();
                let at = rows.iter().position(|n| *n == removed.name);
                let neighbor = at.and_then(|at| rows.get(at + 1).or(at.checked_sub(1).and_then(|p| rows.get(p))));
                match neighbor.cloned() {
                    Some(name) => self.row_handles(&name, cx).0,
                    None => self.new_focus.clone(),
                }
            }
            (_, Some(Origin::Edit(name))) => {
                let name = done.and_then(Outcome::workspace).map(str::to_string).unwrap_or(name);
                self.row_handles(&name, cx).0
            }
            (_, Some(Origin::More(name))) => {
                let name = done.and_then(Outcome::workspace).map(str::to_string).unwrap_or(name);
                self.row_handles(&name, cx).1
            }
            (_, Some(Origin::New) | None) => self.new_focus.clone(),
        };
        window.focus(&handle, cx);
        if let Some(warnings) = done.map(Outcome::warnings).filter(|w| !w.is_empty()) {
            self.state.set_notice(Some(warnings.join(" ")));
        }
        cx.notify();
    }

    /// Whether `handle` holds the keyboard (a test asks where focus went).
    #[cfg(test)]
    pub fn focused(&self, window: &Window) -> Option<String> {
        if self.new_focus.is_focused(window) {
            return Some("new".into());
        }
        self.row_focus.iter().find_map(|(name, (edit, more))| {
            if edit.is_focused(window) {
                Some(format!("edit {name}"))
            } else if more.is_focused(window) {
                Some(format!("more {name}"))
            } else {
                None
            }
        })
    }

    fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.retry();
        self.load(window, cx);
        cx.notify();
    }

    fn toggle_menu(&mut self, name: &str, cx: &mut Context<Self>) {
        self.menu = if self.menu.as_deref() == Some(name) { None } else { Some(name.to_string()) };
        cx.notify();
    }
}

impl EventEmitter<WorkspacesEvent> for WorkspacesPage {}

/// Return or Space, unmodified: a focused button's press.
fn is_press(e: &KeyDownEvent) -> bool {
    let k = &e.keystroke;
    !k.modifiers.modified() && matches!(k.key.as_str(), "enter" | "space")
}

impl WorkspacesPage {
    /// A key on a focused button that opens `intent`: Return or Space opens
    /// it, Esc leaves.
    fn button_key(&mut self, e: &KeyDownEvent, intent: Intent, origin: Origin, cx: &mut Context<Self>) {
        if self.leave_key(e, cx) {
            return;
        }
        if is_press(e) {
            cx.stop_propagation();
            self.open(intent, origin, cx);
        }
    }

    /// Esc on a focused button: the keys go back to the settings sidebar.
    fn leave_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let k = &e.keystroke;
        if k.modifiers.modified() || k.key != "escape" {
            return false;
        }
        cx.stop_propagation();
        self.menu = None;
        cx.emit(WorkspacesEvent::Leave);
        true
    }
}

/// A section's heading (`Machines heading`, 2524:10990): the title over its
/// description, 8pt apart, and the section's button centered beside them.
fn heading(selector: &'static str, title: &'static str, help: &'static str, button: AnyElement, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .gap(px(24.0))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .child(
                    div()
                        .type_style(Type::Heading)
                        .text_size(px(TITLE_SIZE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text_loud)
                        .child(title),
                )
                .child(div().type_style(Type::Small).text_color(theme.form_hint).child(help)),
        )
        .child(button)
}

/// A footnote under a table.
fn footnote(text: &'static str, theme: &Theme) -> Div {
    div().w_full().type_style(Type::Small).text_color(theme.form_hint).child(text)
}

/// A one-line message in place of a table's rows (loading, empty).
fn table_message(selector: &'static str, text: &'static str, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(move || selector.into())
        .px(px(12.0))
        .py(px(16.0))
        .type_style(Type::Small)
        .text_color(theme.form_hint)
        .child(text)
        .into_any_element()
}

/// Text that truncates to one line, its whole as a tooltip.
fn clipped(id: String, text: String) -> Stateful<Div> {
    let selector = id.clone();
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .min_w_0()
        .truncate()
        .tooltip(style::tooltip(text.clone()))
        .child(text)
}

impl WorkspacesPage {
    fn render_workspaces(&mut self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> Div {
        let new_focused = self.new_focus.is_focused(window);
        let new_workspace =
            style::small_icon_button("workspaces-new", icons::PLUS, "New workspace", ButtonKind::Secondary, new_focused, theme)
                .role(Role::Button)
                .aria_label("New workspace")
                .track_focus(&self.new_focus)
                .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                    this.button_key(e, Intent::NewWorkspace, Origin::New, cx)
                }))
                .on_click(cx.listener(|this, _, _, cx| this.open(Intent::NewWorkspace, Origin::New, cx)));
        let headings = div()
            .flex()
            .flex_row()
            .gap(px(24.0))
            .px(px(12.0))
            .py(px(6.0))
            .child(div().flex_1().min_w_0().child("Workspace"))
            .child(div().w(px(MACHINE_W)).flex_none().child("Machine"))
            .child(div().w(px(TAGS_W)).flex_none().child("Tags"))
            .child(div().w(px(SLOT_W)).flex_none().child("Slot"))
            .child(div().w(px(ACTIONS_W)).flex_none());
        let rows: Vec<AnyElement> = match self.state.load() {
            Load::Loading => vec![table_message("workspaces-loading", "Loading workspaces…", theme)],
            Load::Failed(_) => Vec::new(),
            Load::Loaded(index) if index.workspaces.is_empty() => vec![table_message(
                "workspaces-empty",
                "No workspaces yet. New workspace adds one.",
                theme,
            )],
            Load::Loaded(index) => {
                let index = index.clone();
                index
                    .workspaces
                    .iter()
                    .map(|w| self.render_workspace_row(w, theme, window, cx))
                    .collect()
            }
        };
        let body = match self.state.load() {
            Load::Failed(e) => self.render_failure(e, theme, cx).into_any_element(),
            _ => table(headings, rows, theme)
                .rounded(px(TABLE_RADIUS))
                .debug_selector(|| "workspaces-table".into())
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(16.0))
            .child(heading(
                "workspaces-heading",
                "Workspaces",
                "Give agents their own place to work, on this Mac or another machine.",
                new_workspace.into_any_element(),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .gap(px(12.0))
                    .child(body)
                    .child(footnote(
                        "Each workspace has its own worktree. Tags help Shelbi route tasks to the right workspace.",
                        theme,
                    )),
            )
    }

    /// The config couldn't be read: why, and Retry.
    fn render_failure(&self, error: &str, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let retry = style::button("workspaces-retry", "Retry", ButtonKind::Secondary, ButtonSize::Small, false, theme)
            .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx)));
        div()
            .debug_selector(|| "workspaces-failed".into())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .w_full()
            .p(px(16.0))
            .rounded(px(TABLE_RADIUS))
            .border_1()
            .border_color(theme.input_border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        div()
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud)
                            .child("Couldn’t read this project’s workspaces"),
                    )
                    .child(div().type_style(Type::Small).text_color(theme.error).child(error.to_string())),
            )
            .child(retry)
    }

    fn render_workspace_row(&mut self, w: &WorkspaceRow, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let name = w.name.clone();
        let (edit_focus, more_focus) = self.row_handles(&name, cx);
        let ring = |handle: &FocusHandle| handle.is_focused(window).then(|| style::focus_bands(6.0, theme));
        let edit = {
            let name = name.clone();
            let key_name = name.clone();
            row_button(format!("workspace-edit-{name}"), icons::PENCIL_BUTTON, false, theme)
                .relative()
                .role(Role::Button)
                .aria_label(format!("Edit {name}"))
                .track_focus(&edit_focus)
                .children(ring(&edit_focus).into_iter().flatten())
                .tooltip(style::tooltip("Edit workspace"))
                .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                    this.button_key(e, Intent::EditWorkspace(key_name.clone()), Origin::Edit(key_name.clone()), cx)
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.open(Intent::EditWorkspace(name.clone()), Origin::Edit(name.clone()), cx);
                }))
        };
        let more = {
            let open = self.menu.as_deref() == Some(name.as_str());
            let name = name.clone();
            let key_name = name.clone();
            row_button(format!("workspace-more-{name}"), icons::ELLIPSIS_VERTICAL_BUTTON, false, theme)
                .relative()
                .role(Role::Button)
                .aria_label(format!("More actions for {name}"))
                .aria_expanded(open)
                .track_focus(&more_focus)
                .children(ring(&more_focus).into_iter().flatten())
                .tooltip(style::tooltip("More"))
                .when(open, |d| d.child(style::anchor_probe(&self.menu_anchor)))
                .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                    if !this.leave_key(e, cx) && is_press(e) {
                        cx.stop_propagation();
                        this.toggle_menu(&key_name, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.toggle_menu(&name, cx);
                }))
        };
        let menu = (self.menu.as_deref() == Some(name.as_str())).then(|| self.render_menu(&name, theme, cx));
        let machine = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(2.0))
            .w(px(MACHINE_W))
            .child(
                div()
                    .truncate()
                    .type_style(Type::Base)
                    .text_color(theme.form_text)
                    .child(w.machine.clone()),
            )
            .child(
                div()
                    .type_style(Type::XSmall)
                    .text_color(theme.form_hint)
                    .child(kind_label(w.machine_kind)),
            );
        let tags = div()
            .debug_selector({
                let selector = format!("workspace-tags-{name}");
                move || selector.clone()
            })
            .flex()
            .flex_row()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap_x(px(12.0))
            .gap_y(px(4.0))
            .w(px(TAGS_W))
            .type_style(Type::Small)
            .when(w.tags.is_empty(), |d| d.child(div().text_color(theme.form_hint).child("No tags")))
            .children(w.tags.iter().map(|t| {
                let id = SharedString::from(format!("workspace-tag-{name}-{}", t.name));
                let selector = id.clone();
                let tag = div()
                    .id(id)
                    .debug_selector(move || selector.to_string())
                    .text_color(theme.form_text)
                    .child(t.name.clone());
                if t.inherited {
                    tag.tooltip(style::tooltip(format!("From {}", w.machine)))
                } else {
                    tag
                }
            }));
        let slot_help = if w.slot_explicit {
            "Set for this workspace".to_string()
        } else {
            format!("Automatic: its place among {}’s workspaces, from 0", w.machine)
        };
        let slot = div()
            .id(SharedString::from(format!("workspace-slot-{name}")))
            .debug_selector({
                let selector = format!("workspace-slot-{name}");
                move || selector.clone()
            })
            .w(px(SLOT_W))
            .flex_none()
            .type_style(Type::Base)
            .text_color(theme.form_hint)
            .tooltip(style::tooltip(slot_help))
            .child(w.slot.to_string());
        let row_id = SharedString::from(format!("workspace-row-{name}"));
        let selector = row_id.clone();
        let edit_row = name.clone();
        div()
            .id(row_id)
            .debug_selector(move || selector.to_string())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(24.0))
            .px(px(12.0))
            .py(px(16.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open(Intent::EditWorkspace(edit_row.clone()), Origin::Edit(edit_row.clone()), cx)
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        clipped(format!("workspace-name-{name}"), w.name.clone())
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud),
                    )
                    .child(
                        clipped(format!("workspace-path-{name}"), w.path.clone())
                            .type_style(Type::Small)
                            .text_color(theme.form_hint),
                    ),
            )
            .child(machine)
            .child(tags)
            .child(slot)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_none()
                    .justify_end()
                    .gap(px(8.0))
                    .w(px(ACTIONS_W))
                    .child(edit)
                    .child(more)
                    .children(menu),
            )
            .into_any_element()
    }

    /// A workspace's ⋮ menu: Edit, a rule, Remove.
    fn render_menu(&self, name: &str, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let item = |id: String, label: &'static str, intent: Intent, danger: bool, cx: &mut Context<Self>| {
            let row = style::menu_row(SharedString::from(id), false, theme)
                .role(Role::MenuItem)
                .aria_label(label)
                .child(label);
            let row = if danger { row.text_color(theme.error) } else { row };
            let origin = Origin::More(name.to_string());
            row.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.open(intent.clone(), origin.clone(), cx);
            }))
        };
        let edit = item(
            format!("workspace-menu-edit-{name}"),
            "Edit workspace",
            Intent::EditWorkspace(name.to_string()),
            false,
            cx,
        );
        let remove = item(
            format!("workspace-menu-remove-{name}"),
            "Remove workspace…",
            Intent::RemoveWorkspace(name.to_string()),
            true,
            cx,
        );
        let anchor = self.menu_anchor.clone();
        let panel = popup("workspace-menu", theme).on_mouse_down_out(cx.listener(
            move |this, event: &gpui::MouseDownEvent, _, cx| {
                if anchor.get().is_some_and(|b| b.contains(&event.position)) {
                    return;
                }
                this.menu = None;
                cx.notify();
            },
        ));
        let panel = panel
            .child(edit)
            .child(div().flex_none().my_1().mx(px(-4.0)).h(px(1.0)).bg(theme.divider))
            .child(remove);
        deferred(placed(&self.menu_anchor, panel).gap(MENU_GAP).align_right()).with_priority(1)
    }

    fn render_machines(&self, index: &PoolIndex, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let add = style::small_icon_button("machines-add", icons::PLUS, "Add machine", ButtonKind::Secondary, false, theme)
            .on_click(cx.listener(|this, _, _, cx| this.open(Intent::AddMachine, Origin::New, cx)));
        let mut rows: Vec<AnyElement> = Vec::new();
        for (i, m) in index.machines.iter().enumerate() {
            if i > 0 {
                rows.push(div().h(px(1.0)).w_full().bg(theme.input_border).into_any_element());
            }
            rows.push(self.render_machine_row(m, theme, cx));
        }
        div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(16.0))
            .child(heading(
                "machines-heading",
                "Machines",
                "Choose where this project’s workspaces run.",
                add.into_any_element(),
                theme,
            ))
            .child(
                div()
                    .debug_selector(|| "machines-list".into())
                    .flex()
                    .flex_col()
                    .w_full()
                    .rounded(px(TABLE_RADIUS))
                    .border_1()
                    .border_color(theme.input_border)
                    .children(rows),
            )
            .child(footnote(
                "Remote machines connect over SSH using your existing SSH configuration.",
                theme,
            ))
    }

    fn render_machine_row(&self, m: &MachineRow, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let name = m.name.clone();
        let status = self.state.status(m);
        let icon = match m.kind {
            MachineKind::Local => icons::FOLDER,
            MachineKind::Ssh => icons::SERVER_STACK,
        };
        let configure = {
            let name = name.clone();
            style::button(
                SharedString::from(format!("machine-configure-{name}")),
                "Configure",
                ButtonKind::Secondary,
                ButtonSize::Small,
                false,
                theme,
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.open(Intent::ConfigureMachine(name.clone()), Origin::New, cx)
            }))
        };
        let detail = div()
            .flex()
            .flex_row()
            .min_w_0()
            .gap(px(16.0))
            .type_style(Type::Small)
            .text_color(theme.form_hint)
            .children(m.host.clone().map(|h| div().flex_none().child(h)))
            .child(clipped(format!("machine-dir-{name}"), m.work_dir.clone()))
            .child(div().flex_none().child(workspace_count(m.workspaces)));
        let reason = match &status {
            MachineStatus::Unreachable(e) => Some(
                clipped(format!("machine-error-{name}"), e.clone())
                    .type_style(Type::XSmall)
                    .text_color(theme.error),
            ),
            _ => None,
        };
        let badge = {
            let id = SharedString::from(format!("machine-status-{name}"));
            let selector = id.clone();
            div()
                .id(id)
                .debug_selector(move || selector.to_string())
                .flex_none()
                .px(px(4.0))
                .rounded(px(2.0))
                .bg(theme.workflow_badge)
                .type_style(Type::XSmall)
                .text_color(match status {
                    MachineStatus::Unreachable(_) => theme.error,
                    _ => theme.workflow_badge_text,
                })
                .tooltip(style::tooltip(status.help()))
                .child(status.label())
        };
        let selector = format!("machine-row-{name}");
        div()
            .debug_selector(move || selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .min_h(px(88.0))
            .p(px(16.0))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(MACHINE_ICON))
                    .child(heroicon(icon).size(px(MACHINE_ICON)).text_color(theme.icon)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.0))
                    .child(
                        clipped(format!("machine-name-{name}"), m.name.clone())
                            .type_style(Type::BaseSemibold)
                            .text_color(theme.text_loud),
                    )
                    .child(detail)
                    .children(reason),
            )
            .child(badge)
            .child(configure)
            .into_any_element()
    }
}

impl Render for WorkspacesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        if std::mem::take(&mut self.reload) {
            self.load(window, cx);
        }
        let error = self.state.error().map(|e| {
            div()
                .debug_selector(|| "workspaces-error".into())
                .type_style(Type::XSmall)
                .text_color(theme.error)
                .child(e.to_string())
        });
        let notice = self.state.notice().map(|n| {
            div()
                .debug_selector(|| "workspaces-notice".into())
                .type_style(Type::XSmall)
                .text_color(theme.form_hint)
                .child(n.to_string())
        });
        let machines = self.state.index().map(|index| self.render_machines(index, &theme, cx));
        div()
            .id("settings-workspaces")
            .debug_selector(|| "settings-workspaces".into())
            .flex()
            .flex_col()
            .w_full()
            .gap(px(24.0))
            .children(error)
            .children(notice)
            .child(self.render_workspaces(&theme, window, cx))
            .when_some(machines, |d, machines| {
                d.child(div().h(px(1.0)).w_full().bg(theme.divider_strong)).child(machines)
            })
    }
}
