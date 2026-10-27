//! `shelbi-desktop --visual-figma <dir> [--2x]` (feature `visual-check`):
//! every screen of John's Figma mockups (the frames listed in
//! `~/.shelbi/projects/shelbi/design-refs/desktop-figma/README.md`) rendered
//! offscreen through the real window shell, fed with the mockups' own
//! content, so `scripts/figma-diff.sh` can measure the styling against the
//! reference PNGs rather than the data.
//!
//! Like `--visual-views` it points `SHELBI_HOME` at a fresh short `/tmp`
//! directory (unsetting the hub and control socket variables so nothing
//! reaches a live hub) and writes the fixture project there: "My project",
//! its board, workspace statuses, machines and `events.log`. Like
//! `--visual-review` it builds the real [`ShelbiWindow`] over a fixture
//! sidebar, a fake daemon and fake sessions, whose terminals replay the
//! mockups' transcripts (redrawn at the pane's size, so their footers sit on
//! the last rows as in the mockups). The Activity clock is pinned and remote
//! hosts answer reachable without a probe, so nothing depends on the time of
//! day or the network.
//!
//! Each screen gets a fresh 1440x1024 window and is saved as
//! `<screen>-<theme>.png`, named as the reference PNGs are: orchestrator,
//! issues, issue-detail, activity, review, workspace, workspace-idle,
//! machines, the four Settings pages (settings-general,
//! settings-keybindings, settings-terminal, settings-workflows against
//! `workflows/1-index.png`, settings-workflow-builder{,-status,-drawing,
//! -new,-added} against `workflows/2-builder.png`, `3-…`, `5-…`, `6-…` and
//! `7-…`, and settings-workflow-form against
//! `workflows/4-workflow-form.png` in the frame's 1440x1283 window,
//! settings-agents against `agents/1-agents-index.png`,
//! settings-agent-details against `agents/2-agent-details.png`,
//! settings-workspaces against the Workspaces frame 2523:8879) and
//! command-palette, plus the
//! palette states the
//! frame doesn't draw ([`PALETTE_STATES`], no reference: checked by eye),
//! the sidebar states no frame draws (`state-sidebar-*`: sections
//! collapsed, Zen off, the sidebar hidden), the issue detail's
//! ([`DETAIL_ACTIONS`] hovered, a long body scrolled) and the task panels'
//! (`state-review-*`, `state-workspace-*`: More opening the task detail
//! over the screen, each view's ⌘N, Approve
//! and Reject hovered and pressed), and Machines' (`machines-unreachable`:
//! the remote down, its row selected; `machines-idle-selected`,
//! `machines-review-selected`), Settings' (`state-settings-cli-focused`:
//! a focused textbox; `state-keybindings-*`: the bindings editor's capture,
//! conflict, search, picker and customized states; `state-workflow-form-*`:
//! the workflow editor edited, its leave prompt, and its form's foot;
//! `state-agents-*`: an agent's ⋮ menu, a reset waiting to be confirmed,
//! and the shared instructions' editor; `state-agent-details-*`: the agent
//! editor edited with its ⋮ menu open, its Model picker, the Add skill
//! form, its leave prompt, a user agent with skills, and its Delete
//! confirm;
//! `state-workflow-builder-*`: the Builder fitted, zoomed out with the
//! keyboard's focus ring, a hot handle, an action row dragged and its
//! handle focused, and a 20-status workflow), the board toolbar's (`state-issues-unfiltered`: no
//! filter set; `state-issues-search`: `/` and a query typed, the cards
//! narrowed; `state-issues-workspace-open` / `-workflow-open`: a filter
//! chip's menu open), and the form components' sheets
//! (`components/<name>-<theme>.png`, against the components page's PNGs;
//! `button-sizes` has none: each Button size with each content mix, pixel
//! checked). `SHELBI_VISUAL_ONLY=buttons` renders just the components and
//! the Load for review dialog's focus states.
//! `about-<theme>.png` and `bundled-software-<theme>.png` are About Shelbi
//! and Acknowledgements at their frames' sizes (with `state-bundled-*`);
//! `SHELBI_VISUAL_ONLY=about` renders just those;
//! `SHELBI_VISUAL_ONLY=workspaces` renders just Settings ▸ Workspaces.
//! `add-project-*-<theme>.png` are the Add Project wizard's screens 01, 02,
//! 03 (`add-project-writing-config`, `-creating-agents`,
//! `-adding-workflows`) and 04 (`add-project-ready`) at 800 × 580;
//! `SHELBI_VISUAL_ONLY=add-project` renders just those.
//! `cli-check`, `cli-install` and `cli-ready-<theme>.png` are the first
//! launch's CLI setup screens before them; `SHELBI_VISUAL_ONLY=cli-setup`.
//! `SHELBI_VISUAL_ONLY=panels` renders just the Review and Workspace
//! screens and those states; `=settings` just Settings, its state and the
//! components; `=issues` just the board, its states and the issue detail.
//! With `SHELBI_VISUAL_THEME` unset
//! it runs once per theme. Frames are saved at 1x (the references' scale),
//! downsampled from the window's backing scale; `--2x` opens each window on
//! a Retina display instead and keeps the native 2x frame for close
//! inspection. It prints one line per check and exits
//! non-zero if any failed.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, TimeZone, Utc};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use gpui::{
    px, size, AnyWindowHandle, AppContext, Bounds, VisualTestAppContext, WindowBounds, WindowHandle, WindowKind,
    WindowOptions,
};
use shelbi_app::nav::View;
use shelbi_app::review::{ReviewInfo, ReviewTarget};
use shelbi_app::session::{self as app_session, ConnectFailure, IdleInfo, ReconnectPolicy, Relauncher, RetryPolicy, SessionRef};
use shelbi_app::view::{
    ReviewRow, ReviewState, SidebarModel, SidebarTarget, WorkspaceBadge, WorkspaceDetailsInfo, WorkspaceRow,
};
use shelbi_app::{ReviewSessionOp, WorkspaceSessionOp};
use shelbi_client::SessionEvent;
use shelbi_core::{Column, Issue};
use shelbi_term::Size;

use crate::app::{HostCall, NativeViews, ShelbiWindow};
use crate::elements::terminal::session::{Connected, SessionIo};
use crate::views::review::{ReviewDeps, ReviewOps};
use crate::views::session_view::{SessionConnector, SessionDeps};
use crate::views::workspace::{WorkspaceDeps, WorkspaceOps};

const PROJECT: &str = "my-project";
/// `SHELBI_VISUAL_ONLY=panels` renders only the Review and Workspace
/// screens and their panels' states; `=settings` only Settings; `=issues`
/// only the board and the issue detail.
const ONLY_VAR: &str = "SHELBI_VISUAL_ONLY";
/// The reference PNGs' size, in points.
const WIDTH: u32 = 1440;
const HEIGHT: u32 = 1024;
/// The Workflow Form frame's height (2252:29281): its window is taller.
const FORM_HEIGHT: u32 = 1283;

/// An edit to the open workflow editor.
type EditFn = dyn Fn(
    &mut crate::views::workflow_editor::WorkflowEditor,
    &mut gpui::Window,
    &mut gpui::Context<crate::views::workflow_editor::WorkflowEditor>,
);

/// Every screen, named as its reference PNG.
pub const SCREENS: [&str; 12] = [
    "orchestrator",
    "issues",
    "issue-detail",
    "activity",
    "review",
    "workspace",
    "workspace-idle",
    "machines",
    "settings-general",
    "settings-keybindings",
    "settings-terminal",
    "command-palette",
];

/// The palette states the frame doesn't draw: the keys typed into the open
/// palette, and the render's name (beside the screens, with no reference).
pub const PALETTE_STATES: [(&str, &str); 4] = [
    ("e r r", "command-palette-search"),
    ("x y z z y", "command-palette-no-match"),
    ("tab", "command-palette-projects"),
    ("tab down", "command-palette-project-selected"),
];

/// The issue detail's actions, left to right, by the name of the render
/// with each one hovered (`issue-detail-hover-<name>`).
pub const DETAIL_ACTIONS: [&str; 6] = ["move-left", "move-right", "start", "assign", "edit", "editor"];

// ---- the form components ---------------------------------------------------

/// The components page's sheets, named as its PNGs, with their sizes: the
/// Button's states, every state of the Textbox and the Checkbox, a
/// Segmented Control, a Labeled Control and a Picker; plus the Button's
/// sizes and content mixes (`button-sizes`, pixel-checked) and a Checkbox
/// List (no PNG: checked by eye).
const COMPONENT_SHEETS: [(&str, f32, f32); 8] = [
    ("buttons", 580.0, 498.0),
    ("button-sizes", 1024.0, 744.0),
    ("textbox", 234.0, 210.0),
    ("checkbox", 126.0, 154.0),
    ("segmented-control", 167.0, 32.0),
    ("labeled-control", 400.0, 56.0),
    ("picker", 400.0, 32.0),
    ("checkbox-list", 200.0, 74.0),
];

/// The `buttons` sheet's rows, top to bottom: each kind label only at Base,
/// then the Review panel's Approve and Reject (Small, a left icon), the
/// task panels' Open (X-Small) and an issue detail action (X-Small, a left
/// icon and a Hotkey).
const BUTTON_SHEET_ROWS: [SheetButton; 10] = {
    use crate::theme::style::ButtonKind::*;
    use crate::theme::style::ButtonSize::*;
    [
        SheetButton(Primary, Base, SheetContent::Label("Button")),
        SheetButton(Success, Base, SheetContent::Label("Button")),
        SheetButton(Secondary, Base, SheetContent::Label("Button")),
        SheetButton(Danger, Base, SheetContent::Label("Button")),
        SheetButton(Select, Base, SheetContent::Label("Button")),
        SheetButton(Quiet, Base, SheetContent::Label("Button")),
        SheetButton(Success, Small, SheetContent::Left("Approve")),
        SheetButton(Danger, Small, SheetContent::Left("Reject")),
        SheetButton(Secondary, XSmall, SheetContent::Label("Open")),
        SheetButton(Secondary, XSmall, SheetContent::Hotkey("Move")),
    ]
};

/// The `buttons` sheet's columns, left to right: at rest, focused, focused
/// and hovered, focused and pressed, and disabled.
const BUTTON_SHEET_COLUMNS: [SheetState; 5] = [
    SheetState::Rest,
    SheetState::Focused,
    SheetState::FocusedHover,
    SheetState::FocusedActive,
    SheetState::Disabled,
];

/// A button on a sheet: its style, size and content.
#[derive(Debug, Clone, Copy)]
struct SheetButton(crate::theme::style::ButtonKind, crate::theme::style::ButtonSize, SheetContent);

/// A sheet button's content, with its label.
#[derive(Debug, Clone, Copy)]
enum SheetContent {
    Label(&'static str),
    Left(&'static str),
    Right(&'static str),
    Both(&'static str),
    /// A left icon, the label and a Hotkey (the issue detail's actions).
    Hotkey(&'static str),
    /// Both icons around the label and a Hotkey.
    All(&'static str),
}

impl SheetContent {
    /// Every mix the `button-sizes` sheet shows.
    const ALL: [SheetContent; 6] = [
        SheetContent::Label("Button"),
        SheetContent::Left("Button"),
        SheetContent::Right("Button"),
        SheetContent::Both("Button"),
        SheetContent::Hotkey("Button"),
        SheetContent::All("Button"),
    ];

    /// The component's content at `size`: X-Small takes the 18pt drawings.
    fn at(self, size: crate::theme::style::ButtonSize) -> crate::theme::style::ButtonContent {
        use crate::assets::icons;
        use crate::theme::style::{ButtonContent, ButtonSize};
        let (left, right) = match (size, self) {
            (ButtonSize::XSmall, _) => (icons::ARROW_LEFT_BUTTON, icons::ARROW_RIGHT_BUTTON),
            (_, SheetContent::Left("Approve")) => (icons::CHECK, icons::CHEVRON_DOWN),
            (_, SheetContent::Left("Reject")) => (icons::X_MARK, icons::CHEVRON_DOWN),
            _ => (icons::PLUS, icons::CHEVRON_DOWN),
        };
        match self {
            SheetContent::Label(l) => ButtonContent::label(l),
            SheetContent::Left(l) => ButtonContent::label(l).left(left),
            SheetContent::Right(l) => ButtonContent::label(l).right(right),
            SheetContent::Both(l) => ButtonContent::label(l).left(left).right(right),
            SheetContent::Hotkey(l) => ButtonContent::label(l).left(left).hotkey("H"),
            SheetContent::All(l) => ButtonContent::label(l).left(left).hotkey("⌘S").right(right),
        }
    }
}

/// A column of the `buttons` sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SheetState {
    Rest,
    Focused,
    FocusedHover,
    FocusedActive,
    Disabled,
}

impl SheetButton {
    fn kind(self) -> crate::theme::style::ButtonKind {
        self.0
    }

    fn height(self) -> f32 {
        self.1.height()
    }

    fn render(self, id: String, state: SheetState, theme: &crate::theme::Theme) -> gpui::AnyElement {
        use crate::theme::style;
        use gpui::IntoElement;
        let SheetButton(kind, size, content) = self;
        if state == SheetState::Disabled {
            style::disabled_content_button(id, content.at(size), kind, size, theme).into_any_element()
        } else {
            style::content_button(id, content.at(size), kind, size, state != SheetState::Rest, theme).into_any_element()
        }
    }
}

/// The `button-sizes` sheet's columns: the styles (Select is the Figma
/// Select flag), then Default disabled.
const SIZE_SHEET_COLUMNS: [(crate::theme::style::ButtonKind, bool); 6] = {
    use crate::theme::style::ButtonKind::*;
    [(Secondary, false), (Primary, false), (Success, false), (Danger, false), (Select, false), (Secondary, true)]
};

/// The `button-sizes` sheet's rows: each size (X-Small, Small, Base) with
/// each [`SheetContent::ALL`].
fn size_sheet_rows() -> Vec<SheetButton> {
    use crate::theme::style::ButtonSize;
    [ButtonSize::XSmall, ButtonSize::Small, ButtonSize::Base]
        .into_iter()
        .flat_map(|size| SheetContent::ALL.map(|c| SheetButton(crate::theme::style::ButtonKind::Secondary, size, c)))
        .collect()
}

fn size_sheet_x(j: usize) -> f32 {
    17.0 + 168.0 * j as f32
}

fn size_sheet_y(i: usize) -> f32 {
    17.0 + 40.0 * i as f32
}

/// The `buttons` sheet's column `j` left.
fn button_sheet_x(j: usize) -> f32 {
    17.0 + 112.0 * j as f32
}

/// The `buttons` sheet's row `i` top.
fn button_sheet_y(i: usize) -> f32 {
    17.0 + 48.0 * i as f32
}

/// The `buttons` sheet's button at row `i`, column `j`.
fn button_sheet_id(i: usize, j: usize) -> String {
    format!("sheet-button-{i}-{j}")
}

/// One component sheet, by name.
struct Sheet(&'static str);

impl gpui::Render for Sheet {
    fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use crate::elements::controls;
        use gpui::{div, IntoElement, ParentElement, Styled};
        let theme = *cx.global::<crate::theme::Theme>();
        let page = if theme.is_dark() {
            theme.canvas
        } else {
            gpui::rgb(0xf5f5f5).into()
        };
        // A variant at its sheet position (the sheet's 1px dashed edge, then
        // 16px).
        let at = |x: f32, y: f32, el: gpui::AnyElement| div().absolute().left(px(x)).top(px(y)).child(el);
        let text = |s: &'static str, placeholder: bool| {
            div()
                .text_color(if placeholder { theme.form_hint } else { theme.form_text })
                .child(s)
        };
        let textbox = |focused: bool, placeholder: bool| {
            controls::textbox(text("Textbox", placeholder), focused, &theme)
                .w(px(200.0))
                .into_any_element()
        };
        let checkbox = |selected: bool, focused: bool| {
            controls::checkbox("sheet-checkbox", "Checkbox", selected, focused, &theme).into_any_element()
        };
        let sheet = div().relative().size_full().bg(page);
        match self.0 {
            "buttons" => sheet.children(BUTTON_SHEET_ROWS.iter().enumerate().flat_map(|(i, row)| {
                BUTTON_SHEET_COLUMNS.iter().enumerate().map(move |(j, state)| {
                    at(button_sheet_x(j), button_sheet_y(i), row.render(button_sheet_id(i, j), *state, &theme))
                })
            })),
            "button-sizes" => sheet.children(size_sheet_rows().into_iter().enumerate().flat_map(|(i, row)| {
                SIZE_SHEET_COLUMNS.iter().enumerate().map(move |(j, &(kind, disabled))| {
                    let state = if disabled { SheetState::Disabled } else { SheetState::Rest };
                    let b = SheetButton(kind, row.1, row.2);
                    at(size_sheet_x(j), size_sheet_y(i), b.render(format!("size-button-{i}-{j}"), state, &theme))
                })
            })),
            "textbox" => sheet
                .child(at(17.0, 17.0, textbox(false, true)))
                .child(at(17.0, 65.0, textbox(true, true)))
                .child(at(17.0, 113.0, textbox(false, false)))
                .child(at(17.0, 161.0, textbox(true, false))),
            "checkbox" => sheet
                .child(at(17.0, 17.0, checkbox(false, false)))
                .child(at(17.0, 51.0, checkbox(false, true)))
                .child(at(17.0, 85.0, checkbox(true, false)))
                .child(at(17.0, 119.0, checkbox(true, true))),
            "segmented-control" => sheet.child(
                controls::segmented_track(&theme)
                    .child(controls::segment("sheet-item-1", "Item", false, true, &theme))
                    .child(controls::segment("sheet-item-2", "Item", false, true, &theme))
                    .child(controls::segment("sheet-item-3", "Item", false, true, &theme)),
            ),
            "labeled-control" => sheet.child(
                controls::labeled_control("Label", textbox(false, true), None, &theme)
                    .w(px(controls::CONTROL_W)),
            ),
            "picker" => sheet.child(controls::picker(
                controls::textbox(text("Textbox", true), false, &theme).flex_1(),
                controls::form_button("sheet-browse", "Browse", &theme),
            )),
            _ => sheet.child(
                controls::checkbox_list()
                    .w(px(200.0))
                    .child(checkbox(false, false))
                    .child(checkbox(false, false))
                    .child(checkbox(false, false)),
            ),
        }
    }
}

// ---- sessions -------------------------------------------------------------

/// A terminal screen drawn for a size: `(cols, rows)` to the bytes a session
/// would have written.
type Screen = Arc<dyn Fn(u16, u16) -> String + Send + Sync>;

/// A fake session's input side: each resize the pane asks for becomes the
/// session's own resize, then the screen redrawn at that size.
struct ScreenIo {
    tx: UnboundedSender<SessionEvent>,
    screen: Screen,
    last: Mutex<Option<Size>>,
}

impl SessionIo for ScreenIo {
    fn input(&self, _bytes: Vec<u8>) {}
    fn paste(&self, _text: String, _bracketed: bool) {}
    fn resize(&self, size: Size) {
        let mut last = self.last.lock().unwrap();
        if *last == Some(size) {
            return;
        }
        *last = Some(size);
        let _ = self.tx.unbounded_send(SessionEvent::Resized {
            seq: 0,
            cols: size.cols,
            rows: size.rows,
        });
        let _ = self.tx.unbounded_send(SessionEvent::Output {
            seq: 0,
            data: redraw(&self.screen, size.cols, size.rows),
        });
    }
}

/// Clear the screen and scrollback, home the cursor, draw `screen`, hide
/// the cursor (the mockups draw none).
fn redraw(screen: &Screen, cols: u16, rows: u16) -> Vec<u8> {
    format!("\x1b[0m\x1b[H\x1b[2J\x1b[3J{}\x1b[?25l", screen(cols, rows)).into_bytes()
}

/// Sessions by discovery name: the idle ones answer idle, the rest attach
/// and draw their screen (an empty one when the mockups show none).
#[derive(Default)]
struct Sessions {
    idle: Vec<String>,
    screens: Vec<(String, Screen)>,
}

impl SessionConnector for Sessions {
    fn connect(&self, project: &str, target: &SessionRef, _retry: RetryPolicy) -> Result<Connected, ConnectFailure> {
        let name = app_session::discovery_name(project, target);
        if self.idle.contains(&name) {
            return Err(ConnectFailure::Idle(IdleInfo {
                name: target.workspace().unwrap_or_default().into(),
                machine: "hub".into(),
                branch: None,
            }));
        }
        let screen = self
            .screens
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| Arc::new(|_, _| String::new()));
        let size = Size::new(120, 40);
        let (tx, rx) = unbounded();
        tx.unbounded_send(SessionEvent::Resync {
            seq: 0,
            replay: redraw(&screen, size.cols, size.rows),
        })
        .unwrap();
        Ok(Connected {
            link: Box::new(ScreenIo {
                tx,
                screen,
                last: Mutex::new(Some(size)),
            }),
            events: rx,
            size,
            name,
            cwd: None,
        })
    }
}

impl Relauncher for Sessions {
    fn relaunch(&self, _target: &SessionRef, _reopen: bool) -> Result<(), String> {
        Ok(())
    }
}

// ---- the daemon -----------------------------------------------------------

struct Ops;

/// A worktree under the mockups' checkout (`~/Workspaces/shelbi`), so the
/// panels abbreviate it as they do there.
fn worktree(name: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/me".into());
    format!("{home}/Workspaces/shelbi/.shelbi/wt/{name}")
}

const UMBRELLA_TITLE: &str = "Desktop application (umbrella)";
const UMBRELLA_BODY: &str = "Build a native desktop client for Shelbi. This should share state with the TUI \
through a persistent daemon running in the background. Use Alacritty for the terminal. Use GPUI for the \
interface. Don’t stop until every view matches the TUI and the reviews pass.\n\n## Acceptance criteria\n\n\
- [x] Native sidebar and project navigation\n- [x] Terminal rendering, selection, search, and keyboard input\n\
- [x] Issues board and task details\n- [x] Workspace and review views with live sessions\n\
- [ ] Final visual review and packaging\n";

impl ReviewOps for Ops {
    fn resolve(&self, _project: &str, _task: &str) -> Result<ReviewTarget, String> {
        Ok(ReviewTarget::Serving(review_info()))
    }
    fn session_op(&self, _project: &str, _task: &str, _op: ReviewSessionOp) -> Result<(), String> {
        Ok(())
    }
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn reveal(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }

    fn shell_op(&self, _project: &str, _slot: &str, _op: shelbi_app::WorkspaceSessionOp) -> Result<(), String> {
        Ok(())
    }
}

impl WorkspaceOps for Ops {
    fn details(&self, _project: &str, workspace: &str) -> Result<WorkspaceDetailsInfo, String> {
        Ok(if workspace == "alpha" {
            WorkspaceDetailsInfo {
                task: Some(("desktop-app".into(), UMBRELLA_TITLE.into())),
                body: UMBRELLA_BODY.into(),
                branch: Some("jlong/desktop-app".into()),
                worktree: worktree(workspace),
                editor_name: "Vi".into(),
            }
        } else {
            WorkspaceDetailsInfo {
                branch: Some("main".into()),
                worktree: worktree(workspace),
                editor_name: "Vi".into(),
                ..WorkspaceDetailsInfo::default()
            }
        })
    }
    fn session_op(&self, _project: &str, _workspace: &str, _op: WorkspaceSessionOp) -> Result<(), String> {
        Ok(())
    }
    fn reveal(&self, _path: &str) -> Result<(), String> {
        Ok(())
    }
}

fn review_info() -> ReviewInfo {
    ReviewInfo {
        task_id: "desktop-app".into(),
        slot: "review".into(),
        title: UMBRELLA_TITLE.into(),
        body: UMBRELLA_BODY.into(),
        worktree: worktree("review"),
        editor_name: "Vi".into(),
        review_url: Some("http://localhost:4101".into()),
        has_health_check: true,
        remote_machine: None,
    }
}

// ---- the sidebar ----------------------------------------------------------

fn review_row(id: &str, title: &str, branch: &str, state: ReviewState) -> ReviewRow {
    ReviewRow {
        task_id: id.into(),
        title: title.into(),
        branch: branch.into(),
        location: None,
        workspace: (state == ReviewState::Serving).then(|| "review".into()),
        state,
        // The mockups number every review #401.
        number: Some(401),
    }
}

fn workspace_row(name: &str, task: Option<&str>, badge: WorkspaceBadge) -> WorkspaceRow {
    WorkspaceRow {
        name: name.into(),
        machine: "hub".into(),
        is_remote: false,
        current_task: task.map(str::to_string),
        agent: task.map(|_| "developer".into()),
        badge,
    }
}

/// The mockups' sidebar: one review ready, three queued, four workspaces,
/// Zen Mode on.
fn sidebar_model() -> SidebarModel {
    let mut m = SidebarModel::skeleton(PROJECT);
    m.project_label = "My project".into();
    m.reviews = vec![
        review_row("desktop-app", UMBRELLA_TITLE, "jlong/desktop-app", ReviewState::Serving),
        review_row("session-reconnect", "Session reconnect", "jlong/session-reconnect", ReviewState::Pending),
        review_row("fix-sidebar-collapse", "Fix sidebar collapse state", "jlong/fix-sidebar-collapse", ReviewState::Pending),
        review_row("session-reconnect-2", "Session reconnect", "jlong/session-reconnect", ReviewState::Pending),
    ];
    m.workspaces = vec![
        workspace_row("alpha", Some("desktop-app"), WorkspaceBadge::Working),
        workspace_row("bravo", None, WorkspaceBadge::Idle),
        workspace_row("charlie", Some("retry-agent"), WorkspaceBadge::AwaitingInput),
        workspace_row("delta", None, WorkspaceBadge::Idle),
    ];
    m.zen_mode = shelbi_state::ZenModeState::On;
    m.board_loading = false;
    m
}

/// The sidebar with charlie idle: the Workspace idle frame's charlie.
fn idle_charlie_model() -> SidebarModel {
    let mut m = sidebar_model();
    if let Some(row) = m.workspaces.iter_mut().find(|w| w.name == "charlie") {
        *row = workspace_row("charlie", None, WorkspaceBadge::Idle);
    }
    m
}

// ---- the terminals --------------------------------------------------------

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const OFF: &str = "\x1b[0m";

fn paint(sgr: &str, text: &str) -> String {
    format!("{sgr}{text}{OFF}")
}

/// `text` on row `row` (1-based; past the bottom clamps to the last row).
fn at_row(row: u16, text: &str) -> String {
    format!("\x1b[{row};1H\x1b[2K{text}")
}

/// `text` on row `row`, ending at the last column.
fn at_row_right(row: u16, cols: u16, text: &str) -> String {
    let col = cols.saturating_sub(text.chars().count() as u16) + 1;
    format!("\x1b[{row};{col}H{}", paint(DIM, text))
}

fn rule(width: u16) -> String {
    paint(DIM, &"─".repeat(width as usize))
}

fn lines(lines: &[String]) -> String {
    lines.join("\r\n")
}

/// The Orchestrator mockup: Codex's header box, the dispatch log, its
/// composer and footer on the last four rows.
fn orchestrator_screen(cols: u16, rows: u16) -> String {
    const INNER: usize = 55;
    let boxed = |text: &str, style: &str| {
        let pad = INNER.saturating_sub(text.chars().count() + 1);
        format!("{DIM}│{OFF} {}{}{DIM}│{OFF}", paint(style, text), " ".repeat(pad))
    };
    let body = lines(&[
        paint(DIM, &format!("╭{}╮", "─".repeat(INNER))),
        boxed(">_ OpenAI Codex", BOLD),
        boxed("model:     gpt-5.3-codex", ""),
        boxed("directory: ~/Workspaces/my-project", ""),
        boxed("", ""),
        paint(DIM, &format!("╰{}╯", "─".repeat(INNER))),
        String::new(),
        paint(BOLD, "› Take the next ready issues and get them moving."),
        String::new(),
        "• I’ll check the board and available workspaces, then dispatch the ready work.".into(),
        String::new(),
        "• Ran shelbi issue list --project shelbi".into(),
        paint(DIM, "  └ 3 ready issues · 2 available developer workspaces"),
        String::new(),
        "• Ran shelbi workspace list".into(),
        paint(DIM, "  └ alpha    hub    idle"),
        paint(DIM, "    bravo    hub    idle"),
        paint(DIM, "    charlie  hub    idle"),
        String::new(),
        "• Dispatched two issues:".into(),
        paint(GREEN, "  • alpha: Desktop application"),
        paint(GREEN, "  • bravo: Session reconnect"),
        String::new(),
        "  Both agents are running in isolated worktrees. I’ll follow their progress and".into(),
        "  bring completed work into review when it’s ready.".into(),
        String::new(),
        String::new(),
        paint(CYAN, "• alpha completed its work. The desktop application is ready for review."),
        paint(DIM, "  Open it from Ready for Review in the sidebar."),
    ]);
    let footer = [
        at_row(rows - 3, &rule(cols)),
        at_row(rows - 2, &paint(DIM, "› Ask Shelbi to do anything")),
        at_row(rows - 1, &rule(cols)),
        at_row(rows, &paint(DIM, "? for shortcuts")),
        at_row_right(rows, cols, "100% context left"),
    ];
    body + &footer.concat()
}

/// The Workspace mockup: alpha's agent adding project switching, still
/// working.
fn workspace_screen(_cols: u16, rows: u16) -> String {
    let bullet = |text: &str| format!("• {text}");
    let body = lines(&[
        paint(BOLD, "› Add project switching to the desktop app."),
        String::new(),
        bullet("I’ll inspect the project catalog and the window’s navigation flow."),
        String::new(),
        bullet("Explored"),
        paint(DIM, "  └ Read projects.rs, app.rs, sidebar.rs"),
        paint(DIM, "    Search SwitchTo in desktop/src"),
        String::new(),
        bullet("The project picker already knows about loaded and unloaded projects."),
        "  I’m wiring the selected project into the current window and preserving its".into(),
        "  sidebar state so the transition stays consistent.".into(),
        String::new(),
        bullet("Edited desktop/src/app.rs (+48 -12)"),
        paint(GREEN, "  + route the selected project through the existing window model"),
        paint(GREEN, "  + refresh the sidebar and reconnect the orchestrator session"),
        paint(RED, "  - always create a second project window"),
        String::new(),
        bullet("Running cargo test --manifest-path desktop/Cargo.toml"),
        paint(DIM, "  └ test projects::tests::switch_project ... ok"),
        paint(DIM, "    test app::tests::restores_sidebar_selection ... ok"),
    ]);
    body + &at_row(rows.saturating_sub(3), &paint(DIM, "• Working…  (esc to interrupt)"))
}

/// The Review mockup: Claude Code's verification of the umbrella.
fn reviewer_screen(cols: u16, rows: u16) -> String {
    let body = lines(&[
        paint(BOLD, "✻ Welcome to Claude Code"),
        String::new(),
        "❯ Review jlong/desktop-app against its acceptance criteria.".into(),
        String::new(),
        "● I’ve checked the desktop app and its shared state integration.".into(),
        String::new(),
        paint(BOLD, "  Verification"),
        paint(GREEN, "  ✓ Desktop build, tests, and clippy pass"),
        paint(GREEN, "  ✓ The main workspace checks pass"),
        paint(GREEN, "  ✓ Orchestrator, Issues, Activity, and Machines render correctly"),
        paint(GREEN, "  ✓ Workspace and review terminals attach and reconnect"),
        String::new(),
        "● The preview is running at http://localhost:4101.".into(),
        "  Open the browser to check the app, or View Diff to inspect the changes.".into(),
        String::new(),
        paint(CYAN, "  Ready for your review."),
    ]);
    let width = cols.min(102);
    let footer = [
        at_row(rows - 3, &rule(width)),
        at_row(rows - 2, "❯ "),
        at_row(rows - 1, &rule(width)),
        at_row(rows, &paint(DIM, "? for shortcuts")),
    ];
    body + &footer.concat()
}

fn sessions() -> Arc<Sessions> {
    let name = |s: &str| format!("{PROJECT}/{s}");
    Arc::new(Sessions {
        idle: vec![name("ws/charlie"), name("ws/bravo"), name("ws/delta")],
        screens: vec![
            (name("orch"), Arc::new(orchestrator_screen) as Screen),
            (name("ws/alpha"), Arc::new(workspace_screen)),
            (name("ws/review"), Arc::new(reviewer_screen)),
        ],
    })
}

// ---- the run --------------------------------------------------------------

struct Run {
    cx: VisualTestAppContext,
    now: Rc<Cell<Instant>>,
    sessions: Arc<Sessions>,
    out: PathBuf,
    theme: &'static str,
    native: bool,
    /// `--2x` found no Retina display (said once; not looked for again).
    warned: bool,
    failures: usize,
    /// The sidebar model the next window opens with.
    sidebar: SidebarModel,
    /// The next window's height ([`HEIGHT`] but for the Workflow Form).
    height: u32,
}

/// Run it for `theme`, saving `<screen>-<theme>.png` under `out`; exits
/// non-zero if a check failed.
pub fn run(out: PathBuf, theme: &'static str, native: bool) -> ! {
    std::fs::create_dir_all(&out).expect("create the output directory");
    let home = PathBuf::from(format!("/tmp/shbfig-{}", std::process::id()));
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
    let now = pinned_now();
    write_fixtures(&home, now);
    crate::views::activity::pin_clock(now);
    crate::views::machines::stub_reachability(shelbi_core::Reachability::Reachable);

    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.update(|cx| {
        crate::visual_check::install_theme(cx);
        crate::assets::load_fonts(cx);
        crate::elements::terminal::TerminalSettings::init(cx);
        let entry = |name: &str, label: &str| crate::projects::ProjectEntry {
            name: name.into(),
            label: label.into(),
        };
        cx.set_global(crate::projects::Catalog::with(
            vec![
                entry(PROJECT, "My project"),
                entry("shelbi", "Shelbi"),
                entry("contextstore", "ContextStore"),
                entry("32pixels", "32pixels"),
            ],
            Vec::new(),
        ));
    });
    let mut run = Run {
        cx,
        now: Rc::new(Cell::new(Instant::now())),
        sessions: sessions(),
        out,
        theme,
        native,
        warned: false,
        failures: 0,
        sidebar: sidebar_model(),
        height: HEIGHT,
    };
    run.all();
    let _ = std::fs::remove_dir_all(&home);
    println!(
        "\n{} check(s) failed; {theme} frames saved under {}",
        run.failures,
        run.out.display()
    );
    std::process::exit(if run.failures == 0 { 0 } else { 1 })
}

/// `--visual-figma-fixtures <root>`: the mockups' content (the projects,
/// the board, the Activity events, the workspaces' states) written into
/// `root` as a Shelbi root, timed from the real clock so the Activity ages
/// read as drawn while the real app shows it. `scripts/figma-window-walk.sh`
/// opens it in the bundled app. Exits.
pub fn export_fixtures(root: PathBuf) -> ! {
    std::fs::create_dir_all(&root).expect("create the Shelbi root");
    // SAFETY: single-threaded here, before any reader thread starts.
    unsafe {
        std::env::set_var("SHELBI_HOME", &root);
        std::env::set_var("SHELBI_ROOT", &root);
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_CONTROL_SOCK");
        std::env::remove_var("SHELBI_HUB_ADDR");
        std::env::remove_var("SHELBI_PROJECT");
    }
    write_fixtures(&root, Utc::now());
    println!("fixtures for project {PROJECT} written under {}", root.display());
    std::process::exit(0)
}

/// 15:00 local on 2026-10-17, the mockups' "today".
fn pinned_now() -> DateTime<Utc> {
    Local
        .with_ymd_and_hms(2026, 10, 17, 15, 0, 0)
        .single()
        .expect("a local time")
        .with_timezone(&Utc)
}

impl Run {
    /// A fresh window on the fixture project.
    fn open(&mut self) -> WindowHandle<ShelbiWindow> {
        // A pressed button in the last window must not stay pressed here.
        crate::theme::motion::reset_pointers();
        let retina = if self.native && !self.warned { self.open_on_retina() } else { None };
        if self.native && retina.is_none() && !self.warned {
            self.warned = true;
            println!("      --2x: no display draws at 2x; frames come at the main display's scale");
        }
        let handle = match retina {
            Some(handle) => handle,
            None => {
                let root = self.root();
                self.cx
                    .open_offscreen_window(size(px(WIDTH as f32), px(self.height as f32)), root)
                    .expect("open the offscreen window")
            }
        };
        self.settle(handle);
        handle
    }

    /// `--2x`: a window on the first display whose backing scale is 2 (it
    /// shows there while it renders, above other windows, without taking
    /// focus), or `None` when no display is Retina.
    fn open_on_retina(&mut self) -> Option<WindowHandle<ShelbiWindow>> {
        for display in self.cx.update(|cx| cx.displays()) {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: display.bounds().origin,
                    size: size(px(WIDTH as f32), px(self.height as f32)),
                })),
                display_id: Some(display.id()),
                kind: WindowKind::PopUp,
                focus: false,
                show: true,
                ..Default::default()
            };
            let root = self.root();
            let Ok(handle) = self.cx.update(|cx| cx.open_window(options, root)) else {
                continue;
            };
            let window: AnyWindowHandle = handle.into();
            // The backing scale lands with the first draw on its display.
            self.settle(handle);
            let scale = self.cx.update_window(window, |_, w, _| w.scale_factor()).unwrap_or(1.0);
            if scale >= 2.0 {
                return Some(handle);
            }
            let _ = self.cx.update_window(window, |_, w, _| w.remove_window());
        }
        None
    }

    /// The window's root: the real shell over the fixture sidebar, the fake
    /// daemon and sessions, on the run's clock.
    fn root(&self) -> impl FnOnce(&mut gpui::Window, &mut gpui::App) -> gpui::Entity<ShelbiWindow> + 'static {
        let sessions = self.sessions.clone();
        let now = self.now.clone();
        let model = self.sidebar.clone();
        move |window, cx| {
            let session_deps = {
                let sessions = sessions.clone();
                Rc::new(move || SessionDeps {
                    connector: sessions.clone(),
                    relauncher: sessions.clone(),
                    retry: RetryPolicy::default(),
                    reconnect: ReconnectPolicy::default(),
                    clock: Rc::new(Instant::now),
                })
            };
            let review = {
                let d = session_deps.clone();
                ReviewDeps {
                    ops: Arc::new(Ops),
                    sessions: Rc::new(move |_task| d()),
                }
            };
            let workspace = WorkspaceDeps {
                ops: Arc::new(Ops),
                sessions: session_deps.clone(),
            };
            cx.new(|cx| {
                ShelbiWindow::figma(
                    PROJECT,
                    model,
                    review,
                    workspace,
                    session_deps,
                    Rc::new(move || now.get()),
                    window,
                    cx,
                )
            })
        }
    }

    fn perform(&mut self, handle: WindowHandle<ShelbiWindow>, call: HostCall) {
        handle
            .update(&mut self.cx, |w, window, cx| w.bench_perform(call, window, cx))
            .unwrap();
    }

    /// Show `view` as a click on its sidebar row does: the row selected,
    /// then the view.
    fn click(&mut self, handle: WindowHandle<ShelbiWindow>, view: View) {
        let target = match &view {
            View::Session(name) if name != "orch" => SidebarTarget::Workspace(name.clone()),
            _ => SidebarTarget::View(view.clone()),
        };
        handle
            .update(&mut self.cx, |w, _, cx| w.offscreen_select_target(&target, cx))
            .unwrap();
        self.perform(handle, HostCall::ShowView(view));
    }

    /// Let background work land, move the transitions' clock past any slide
    /// or fade, and repaint.
    fn settle(&mut self, handle: WindowHandle<ShelbiWindow>) {
        let window: AnyWindowHandle = handle.into();
        for _ in 0..3 {
            for _ in 0..10 {
                self.cx.run_until_parked();
                std::thread::sleep(Duration::from_millis(10));
            }
            // The views' poll timers don't run under the offscreen
            // dispatcher: drain their reads by hand.
            let (issues, activity, machines) = self.views(handle);
            if let Some(v) = issues {
                v.update(&mut self.cx, |v, cx| v.poll(cx));
            }
            if let Some(v) = activity {
                v.update(&mut self.cx, |v, cx| v.poll(cx));
            }
            if let Some(v) = machines {
                v.update(&mut self.cx, |v, cx| v.poll(cx));
            }
            self.now.set(self.now.get() + Duration::from_secs(1));
            self.cx
                .update_window(window, |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
        }
        self.cx.run_until_parked();
    }

    /// Settle until `done` or the deadline.
    fn wait(
        &mut self,
        handle: WindowHandle<ShelbiWindow>,
        deadline: Duration,
        mut done: impl FnMut(&mut VisualTestAppContext) -> bool,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            self.settle(handle);
            if done(&mut self.cx) {
                return true;
            }
        }
        false
    }

    fn expect(&mut self, name: &str, ok: bool) {
        println!("{}  {name}", if ok { "ok  " } else { "FAIL" });
        if !ok {
            self.failures += 1;
        }
    }

    /// The palette open over `under`, with the frame's Projects column, after
    /// `keys` (gpui keystroke syntax; none for the frame's state), saved as
    /// `screen`.
    fn palette(&mut self, under: &View, keys: &str, screen: &str) {
        let handle = self.open();
        self.click(handle, under.clone());
        self.settle(handle);
        handle
            .update(&mut self.cx, |w, window, cx| {
                // The frame's Projects column: two open, one not.
                use shelbi_app::overlay::palette::{ProjectEntry, ProjectIndicator};
                let project = |slug: &str, label: &str, indicator| ProjectEntry {
                    slug: slug.into(),
                    label: label.into(),
                    indicator,
                };
                w.offscreen_palette_projects(vec![
                    project("shelbi", "Shelbi", ProjectIndicator::LoadedIdle),
                    project("contextstore", "ContextStore", ProjectIndicator::LoadedIdle),
                    project("32pixels", "32pixels", ProjectIndicator::Unloaded),
                ]);
                w.offscreen_open_overlay(None, window, cx)
            })
            .unwrap();
        if !keys.is_empty() {
            self.settle(handle);
            self.cx.simulate_keystrokes(handle.into(), keys);
        }
        self.save(handle, screen);
    }

    /// The Machines view with every remote answering `reachability` and
    /// workspace row `selected` selected, saved as `screen`.
    fn machines(&mut self, screen: &str, reachability: shelbi_core::Reachability, selected: usize) {
        crate::views::machines::stub_reachability(reachability.clone());
        let handle = self.open();
        self.click(handle, View::Machines);
        let machines = self.views(handle).2.expect("the Machines view");
        let loaded = self.wait(handle, Duration::from_secs(10), |cx| {
            machines.read_with(cx, |v, _| {
                v.state().model().workspace_count() == 5
                    && v.state().model().machines.get(1).is_some_and(|m| m.reachability == reachability)
            })
        });
        if let Some(e) = machines.read_with(&self.cx, |v, _| v.state().error().map(str::to_string)) {
            println!("      read error: {e}");
        }
        self.expect(&format!("{screen}: five workspaces, the gpu box {reachability:?}"), loaded);
        machines.update(&mut self.cx, |v, cx| {
            v.state_mut().select_row(selected);
            cx.notify();
        });
        self.save(handle, screen);
        crate::views::machines::stub_reachability(shelbi_core::Reachability::Reachable);
    }

    /// Save the window as `<screen>-<theme>.png` and close it.
    fn save(&mut self, handle: WindowHandle<ShelbiWindow>, screen: &str) {
        self.capture(handle, screen);
        self.cx
            .update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();
        self.cx.run_until_parked();
    }

    /// Save the window as `<screen>-<theme>.png` as it draws at the run's
    /// clock now (no settle: a transition stays where it is).
    fn capture_frame(&mut self, handle: WindowHandle<ShelbiWindow>, screen: &str) {
        let window: AnyWindowHandle = handle.into();
        self.cx
            .update_window(window, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
        self.write_capture(window, screen);
    }

    /// Save the window as `<screen>-<theme>.png`, keeping it open.
    fn capture(&mut self, handle: WindowHandle<ShelbiWindow>, screen: &str) {
        self.settle(handle);
        let window: AnyWindowHandle = handle.into();
        self.write_capture(window, screen);
    }

    fn write_capture(&mut self, window: AnyWindowHandle, screen: &str) {
        let image = self.cx.capture_screenshot(window).expect("render to image");
        // Down to 1x, keeping the aspect: a window taller than the display
        // (the Workflow Form's 1283pt on a laptop) comes back clamped to it.
        let image = if self.native || image.width() == WIDTH {
            image
        } else {
            let height = image.height() * WIDTH / image.width();
            image::imageops::resize(&image, WIDTH, height, image::imageops::FilterType::Triangle)
        };
        let path = self.out.join(format!("{screen}-{}.png", self.theme));
        image.save(&path).expect("save the frame");
        println!("saved {} ({}x{})", path.display(), image.width(), image.height());
    }

    fn settings(&mut self) {
        use crate::views::settings_nav::SettingsPage;
        // Each page with the settings sidebar in the column (General is the
        // Settings frame, 2165:545).
        for (page, screen) in [
            (SettingsPage::General, "settings-general"),
            (SettingsPage::Keybindings, "settings-keybindings"),
            (SettingsPage::Terminal, "settings-terminal"),
        ] {
            let handle = self.open();
            handle
                .update(&mut self.cx, |w, window, cx| w.offscreen_open_settings(page, window, cx))
                .unwrap();
            self.save(handle, screen);
        }

        // The Shelbi CLI field focused: the textbox's ring and glow, the
        // caret in the ring's color.
        let handle = self.open();
        handle
            .update(&mut self.cx, |w, window, cx| {
                w.offscreen_open_settings(SettingsPage::General, window, cx);
                if let Some(settings) = w.offscreen_settings() {
                    settings.update(cx, |v, cx| v.offscreen_focus_cli(window, cx));
                }
            })
            .unwrap();
        self.save(handle, "state-settings-cli-focused");
        self.settings_workflows();
        self.settings_agents();
        self.settings_workspaces();
        self.keybindings_states();
        self.components();
    }

    /// Settings ▸ Workflows (the Index frame, 2301:5824, reference
    /// `workflows/1-index.png`) over the frame's workflows and statuses,
    /// written for this screen only: the other screens keep the built-in
    /// workflow.
    fn settings_workflows(&mut self) {
        use crate::views::settings_nav::SettingsPage;
        let workflows = shelbi_state::workflows_dir(PROJECT).expect("the workflows directory");
        let project_dir = workflows.parent().expect("the project directory").to_path_buf();
        let agents = project_dir.join("agents");
        let had_agents = agents.exists();
        crate::views::workflows_state::fixture::write(&project_dir);
        let handle = self.open();
        handle
            .update(&mut self.cx, |w, window, cx| {
                w.offscreen_open_settings(SettingsPage::Workflows, window, cx)
            })
            .unwrap();
        self.settle(handle);
        self.save(handle, "settings-workflows");
        self.workflow_builder();
        self.workflow_form();
        let _ = std::fs::remove_dir_all(&workflows);
        if !had_agents {
            let _ = std::fs::remove_dir_all(&agents);
        }
    }

    /// Settings ▸ Agents (the Agents frame, 2377:41040, reference
    /// `agents/1-agents-index.png`) over the frame's agents
    /// (`agents_state::fixture`: the shipped six, the orchestrator and the
    /// review agent customized, the Workflows fixture assigning them), and
    /// its states no frame draws (`state-agents-*`, checked by eye): the
    /// orchestrator's ⋮ menu (Delete disabled, Reset offered), a delete
    /// waiting to be confirmed, and the shared instructions' editor. Written
    /// for these screens only.
    fn settings_agents(&mut self) {
        use crate::views::agents_state::Confirm;
        use crate::views::settings_nav::SettingsPage;
        let workflows = shelbi_state::workflows_dir(PROJECT).expect("the workflows directory");
        let project_dir = workflows.parent().expect("the project directory").to_path_buf();
        let agents = project_dir.join("agents");
        let had_agents = agents.exists();
        crate::views::agents_state::fixture::write(&project_dir, PROJECT);
        let open = |run: &mut Self| {
            let handle = run.open();
            handle
                .update(&mut run.cx, |w, window, cx| w.offscreen_open_settings(SettingsPage::Agents, window, cx))
                .unwrap();
            run.settle(handle);
            handle
        };
        let page = |run: &mut Self, handle: WindowHandle<ShelbiWindow>| {
            handle
                .update(&mut run.cx, |w, _, cx| w.offscreen_settings().map(|v| v.read(cx).agents()))
                .unwrap()
                .expect("Settings is open")
        };
        let handle = open(self);
        let loaded = page(self, handle).read_with(&self.cx, |p, _| p.state().agents().len());
        self.expect("settings-agents: the six shipped agents", loaded == 6);
        self.save(handle, "settings-agents");

        let handle = open(self);
        let agents_page = page(self, handle);
        agents_page.update(&mut self.cx, |p, cx| p.offscreen_open_menu("orchestrator", cx));
        self.save(handle, "state-agents-menu");

        let handle = open(self);
        let agents_page = page(self, handle);
        agents_page.update(&mut self.cx, |p, cx| {
            p.offscreen_ask(Confirm::Reset("orchestrator".into()), cx)
        });
        self.save(handle, "state-agents-confirm");

        let handle = open(self);
        let agents_page = page(self, handle);
        handle
            .update(&mut self.cx, |_, window, cx| {
                agents_page.update(cx, |p, cx| p.offscreen_edit_shared(window, cx))
            })
            .unwrap();
        self.save(handle, "state-agents-shared-editor");

        self.agent_details();

        let _ = std::fs::remove_dir_all(&workflows);
        if !had_agents {
            let _ = std::fs::remove_dir_all(&agents);
        }
    }

    /// Settings ▸ Workspaces (the Workspaces frame, 2523:8879) over the
    /// fixture project's pool: four workspaces on the hub and one on the gpu
    /// box, which answers reachable. The frame's names, paths and slots are
    /// its own sample data; the layout and styling are what compare. Its
    /// states no frame draws (`state-workspaces-*`, checked by eye): the gpu
    /// box unreachable, and a workspace's ⋮ menu.
    fn settings_workspaces(&mut self) {
        use crate::views::settings_nav::SettingsPage;
        use crate::views::workspaces_state::MachineStatus;
        let down = shelbi_core::Reachability::Unreachable {
            error: "ssh: connect to host gpu.lan port 22: Operation timed out".into(),
        };
        for (screen, reachability, want, menu) in [
            ("settings-workspaces", shelbi_core::Reachability::Reachable, "Reachable", false),
            ("state-workspaces-unreachable", down, "Unreachable", false),
            ("state-workspaces-menu", shelbi_core::Reachability::Reachable, "Reachable", true),
        ] {
            crate::views::machines::stub_reachability(reachability);
            let handle = self.open();
            handle
                .update(&mut self.cx, |w, window, cx| {
                    w.offscreen_open_settings(SettingsPage::Workspaces, window, cx)
                })
                .unwrap();
            let page = handle
                .update(&mut self.cx, |w, _, cx| w.offscreen_settings().map(|v| v.read(cx).workspaces()))
                .unwrap()
                .expect("Settings is open");
            let ready = self.wait(handle, Duration::from_secs(10), |cx| {
                page.update(cx, |p, cx| {
                    p.take_probes(cx);
                    let state = p.state();
                    state.index().is_some_and(|i| {
                        i.workspaces.len() == 5
                            && i.machines.get(1).is_some_and(|m| {
                                let status = state.status(m);
                                status != MachineStatus::Checking && status.label() == want
                            })
                    })
                })
            });
            self.expect(&format!("{screen}: five workspaces, the gpu box {want}"), ready);
            if menu {
                page.update(&mut self.cx, |p, cx| p.offscreen_open_menu("alpha", cx));
            }
            self.save(handle, screen);
        }
        crate::views::machines::stub_reachability(shelbi_core::Reachability::Reachable);
    }

    /// The agent details editor (the Agent Details frame, 2405:6965,
    /// reference `agents/2-agent-details.png`) over the fixture's developer,
    /// and its states no frame draws (`state-agent-details-*`, checked by
    /// eye): an edit made with the ⋮ menu open, a picker open, the Add skill
    /// form, the leave prompt, and a user agent with skills and no
    /// assignments.
    fn agent_details(&mut self) {
        use crate::views::agent_editor::Menu;
        use crate::views::agent_editor_state::Change;
        type AgentFn = dyn Fn(
            &mut crate::views::agent_editor::AgentEditor,
            &mut gpui::Window,
            &mut gpui::Context<crate::views::agent_editor::AgentEditor>,
        );
        let open = |run: &mut Self, name: &str| {
            use crate::views::settings_nav::SettingsPage;
            let handle = run.open();
            handle
                .update(&mut run.cx, |w, window, cx| w.offscreen_open_settings(SettingsPage::Agents, window, cx))
                .unwrap();
            run.settle(handle);
            handle
                .update(&mut run.cx, |w, window, cx| w.offscreen_edit_agent(name, window, cx))
                .unwrap();
            run.settle(handle);
            handle
        };
        let edit = |run: &mut Self, handle: WindowHandle<ShelbiWindow>, f: &AgentFn| {
            handle
                .update(&mut run.cx, |w, window, cx| {
                    let editor = w.offscreen_settings().and_then(|s| s.read(cx).agent_editor()).expect("the editor shows");
                    editor.update(cx, |e, cx| f(e, window, cx));
                })
                .unwrap();
            run.settle(handle);
        };

        let handle = open(self, "developer");
        let shown = handle
            .update(&mut self.cx, |w, _, cx| {
                w.offscreen_settings().and_then(|s| s.read(cx).agent_editor()).is_some_and(|e| e.read(cx).state().is_some())
            })
            .unwrap();
        self.expect("settings-agent-details: the developer opens", shown);
        self.save(handle, "settings-agent-details");

        let handle = open(self, "developer");
        edit(self, handle, &|e, window, cx| {
            e.make(Change::Permissions(Some("acceptEdits".into())), window, cx);
            e.offscreen_open_menu(Menu::More, cx);
        });
        self.save(handle, "state-agent-details-menu");

        let handle = open(self, "developer");
        edit(self, handle, &|e, _, cx| e.offscreen_open_menu(Menu::Model, cx));
        self.save(handle, "state-agent-details-model");

        let handle = open(self, "developer");
        edit(self, handle, &|e, window, cx| e.offscreen_add_skill("release-notes", "Write the release notes.", window, cx));
        self.save(handle, "state-agent-details-add-skill");

        let handle = open(self, "orchestrator");
        edit(self, handle, &|e, window, cx| {
            e.make(Change::Reset, window, cx);
            e.back(cx);
        });
        self.save(handle, "state-agent-details-leave");

        // A user agent: no badge, skills listed, no assignments, Delete
        // offered (and confirmed here).
        let config = shelbi_state::agents_config::AgentsConfig::load(PROJECT)
            .and_then(|c| c.create_agent("docs", Some("Keeps the docs current.")))
            .and_then(|c| {
                c.add_skill("docs", "changelog", "Update CHANGELOG.md for a merged change.", "")
                    .and_then(|c| c.add_skill("docs", "screenshots", "Retake the site's screenshots.", ""))
            })
            .and_then(|c| c.save());
        self.expect("state-agent-details-user: the agent is written", config.is_ok());
        let handle = open(self, "docs");
        self.save(handle, "state-agent-details-user");
        let handle = open(self, "docs");
        edit(self, handle, &|e, _, cx| e.offscreen_ask_delete(cx));
        self.save(handle, "state-agent-details-delete");
    }

    /// The workflow editor's Form (the Workflow Form frame, 2252:29281,
    /// reference `workflows/4-workflow-form.png`, 1283pt tall) over the
    /// fixture's Task, and its states no frame draws (`state-workflow-*`,
    /// checked by eye): an edit made (Discard shows, Save enabled), a
    /// transition's Details open with a refused timeout, the leave prompt,
    /// and the form's foot with Review running the branch.
    fn workflow_form(&mut self) {
        use crate::views::workflow_editor_state::{Change, DetailsForm};
        self.height = FORM_HEIGHT;
        let open = |run: &mut Run| {
            let handle = run.open_workflow_editor("task");
            run.edit_workflow(handle, &|e, window, cx| e.show_mode(crate::views::workflow_editor_state::Mode::Form, window, cx));
            handle
        };
        let edit = |run: &mut Run, handle: WindowHandle<ShelbiWindow>, f: &EditFn| run.edit_workflow(handle, f);

        let handle = open(self);
        let shown = handle
            .update(&mut self.cx, |w, _, cx| w.offscreen_settings().is_some_and(|s| s.read(cx).editing()))
            .unwrap();
        self.expect("settings-workflow-form: the editor holds the window", shown);
        self.save(handle, "settings-workflow-form");

        let handle = open(self);
        edit(self, handle, &|e, window, cx| {
            e.make(Change::Owner { id: "backlog".into(), owner: shelbi_core::Owner::Agent }, window, cx);
            e.make(
                Change::Details {
                    index: 0,
                    form: DetailsForm { ready_timeout: "soon".into(), ..DetailsForm::default() },
                },
                window,
                cx,
            );
            e.offscreen_open_details(1, window, cx);
        });
        self.save(handle, "state-workflow-form-edited");

        let handle = open(self);
        edit(self, handle, &|e, window, cx| {
            e.make(Change::Description("Branch work, reviewed.".into()), window, cx);
            e.back(cx);
        });
        self.save(handle, "state-workflow-form-leave-prompt");

        // The form's foot: Git, Review running the branch, Automation and
        // Delete workflow.
        let handle = open(self);
        edit(self, handle, &|e, window, cx| {
            e.make(Change::ReviewMode(crate::views::workflow_editor_state::ReviewMode::Serve), window, cx);
            e.offscreen_scroll_to_bottom();
        });
        self.save(handle, "state-workflow-form-bottom");
        self.height = HEIGHT;
    }

    /// A fresh window with Settings ▸ Workflows' workflow `name` open in the
    /// editor (on the Builder, where it opens).
    fn open_workflow_editor(&mut self, name: &str) -> WindowHandle<ShelbiWindow> {
        use crate::views::settings_nav::SettingsPage;
        let handle = self.open();
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_settings(SettingsPage::Workflows, window, cx))
            .unwrap();
        self.settle(handle);
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_edit_workflow(name, window, cx))
            .unwrap();
        self.settle(handle);
        handle
    }

    /// Run `f` on the open workflow editor, then settle.
    fn edit_workflow(&mut self, handle: WindowHandle<ShelbiWindow>, f: &EditFn) {
        handle
            .update(&mut self.cx, |w, window, cx| {
                let editor = w.offscreen_settings().and_then(|s| s.read(cx).editor()).expect("the editor shows");
                editor.update(cx, |e, cx| f(e, window, cx));
            })
            .unwrap();
        self.settle(handle);
    }

    /// The workflow editor's Builder over the fixture's Task, in each
    /// Workflow Builder frame's state (references `workflows/2-builder.png`,
    /// `3-builder-status-selected.png`, `5-drawing-transition.png`,
    /// `6-new-transition.png`, `7-transition-added.png`), and the states no
    /// frame draws (`state-workflow-builder-*`, checked by eye): a
    /// transition opened from its status (its back button), a hovered handle
    /// past its node's edge, an action row dragged and its handle focused,
    /// fitted, zoomed
    /// out with the keyboard's focus ring, and a 20-status, 40-transition
    /// workflow.
    fn workflow_builder(&mut self) {
        use crate::views::workflow_builder_state::{NewTransition, Selection};
        use crate::views::workflow_graph::{Graph, Pt, Side};
        use gpui::{point, Modifiers, MouseButton};
        use shelbi_core::TransitionAction::{Merge, PushBranch};

        let handle = self.open_workflow_editor("task");
        let shown = handle
            .update(&mut self.cx, |w, _, cx| w.offscreen_settings().is_some_and(|s| s.read(cx).editing()))
            .unwrap();
        self.expect("settings-workflow-builder: the editor opens on the Builder", shown);
        self.save(handle, "settings-workflow-builder");

        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            e.builder_mut().select_status("review");
            cx.notify();
        });
        self.save(handle, "settings-workflow-builder-status");

        // Dragging from In Progress's bottom handle, over Done's left side.
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            let s = e.state().expect("loaded");
            let g = Graph::of(s.workflow(), s.catalog());
            let from = g.rect("in-progress").unwrap().side(Side::Bottom);
            let to = g.rect("done").unwrap().side(Side::Left);
            let b = e.builder_mut();
            b.select_status("in-progress");
            b.begin_draw("in-progress", Some(Side::Bottom), Some(from));
            b.draw_to(to, &g);
            cx.notify();
        });
        self.save(handle, "settings-workflow-builder-drawing");

        fn new() -> NewTransition {
            NewTransition {
                from: "in-progress".into(),
                to: "done".into(),
                actions: vec![PushBranch, Merge],
            }
        }
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            e.builder_mut().selection = Some(Selection::New(new()));
            cx.notify();
        });
        self.save(handle, "settings-workflow-builder-new");

        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, window, cx| {
            e.builder_mut().selection = Some(Selection::New(new()));
            e.add_drawn_transition(window, cx);
        });
        let added = handle
            .update(&mut self.cx, |w, _, cx| {
                let editor = w.offscreen_settings().and_then(|s| s.read(cx).editor()).expect("the editor shows");
                let e = editor.read(cx);
                e.state().is_some_and(|s| s.transitions().len() == 5)
            })
            .unwrap();
        self.expect("settings-workflow-builder-added: the transition is in the draft", added);
        self.save(handle, "settings-workflow-builder-added");

        // New transition's first action row (its handle at frame 6's
        // 1123,426) dragged over the second (y 462): the ghost at the
        // pointer, the row under it highlighted; and its handle clicked, the
        // focus ring on it (⌥↑ / ⌥↓ step it).
        let first = point(px(1123.0), px(426.0));
        let second = point(px(1123.0), px(462.0));
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            e.builder_mut().selection = Some(Selection::New(new()));
            cx.notify();
        });
        self.cx.simulate_mouse_move(handle.into(), first, None, Modifiers::none());
        self.cx.simulate_mouse_down(handle.into(), first, MouseButton::Left, Modifiers::none());
        self.cx.simulate_mouse_move(handle.into(), point(first.x, first.y + px(6.0)), Some(MouseButton::Left), Modifiers::none());
        self.cx.simulate_mouse_move(handle.into(), second, Some(MouseButton::Left), Modifiers::none());
        self.settle(handle);
        self.save(handle, "state-workflow-builder-action-drag");
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            e.builder_mut().selection = Some(Selection::New(new()));
            cx.notify();
        });
        self.cx.simulate_mouse_move(handle.into(), first, None, Modifiers::none());
        self.cx.simulate_mouse_down(handle.into(), first, MouseButton::Left, Modifiers::none());
        self.cx.simulate_mouse_up(handle.into(), first, MouseButton::Left, Modifiers::none());
        // Off the row, so the focus ring shows without the hover.
        self.cx.simulate_mouse_move(handle.into(), point(px(700.0), px(950.0)), None, Modifiers::none());
        self.settle(handle);
        self.save(handle, "state-workflow-builder-action-focused");

        // Review's Done transition, opened from its status: the back button.
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            let b = e.builder_mut();
            b.select_status("review");
            b.open_transition(1);
            cx.notify();
        });
        self.save(handle, "state-workflow-builder-back");

        // The pointer on In Progress's right handle, past the node's edge:
        // the handles still show, that one hot.
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| {
            let s = e.state().expect("loaded");
            let g = Graph::of(s.workflow(), s.catalog());
            let side = g.rect("in-progress").unwrap().side(Side::Right);
            let b = e.builder_mut();
            b.hover = Some("in-progress".into());
            b.hover_to(Pt::new(side.x + 4.0, side.y), &g, 1.0);
            cx.notify();
        });
        self.save(handle, "state-workflow-builder-handle-hot");

        // Fit, and 50% with the keyboard on Review.
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, _, cx| e.fit_canvas(cx));
        self.save(handle, "state-workflow-builder-fit");
        let handle = self.open_workflow_editor("task");
        self.edit_workflow(handle, &|e, window, cx| {
            e.zoom_canvas(0.5, cx);
            e.focus_canvas(window, cx);
            e.builder_mut().focus = Some("review".into());
        });
        self.save(handle, "state-workflow-builder-zoomed");

        // Twenty statuses and forty transitions, fitted.
        let workflows = shelbi_state::workflows_dir(PROJECT).expect("the workflows directory");
        std::fs::write(workflows.join("statuses.yaml"), crate::views::workflow_graph::big_fixture::statuses()).unwrap();
        std::fs::write(workflows.join("big.yaml"), crate::views::workflow_graph::big_fixture::workflow()).unwrap();
        let handle = self.open_workflow_editor("big");
        self.edit_workflow(handle, &|e, _, cx| e.fit_canvas(cx));
        self.save(handle, "state-workflow-builder-big");
        let _ = std::fs::remove_file(workflows.join("big.yaml"));
        std::fs::write(workflows.join("statuses.yaml"), crate::views::workflows_state::fixture::STATUSES).unwrap();
    }

    /// The Keybindings page's states no frame draws (`state-keybindings-*`,
    /// checked by eye): a keycap capturing, a capture adding a chord, a
    /// conflict held, the search and the context picker, and customized
    /// bindings (Reset all enabled).
    fn keybindings_states(&mut self) {
        use crate::views::keybindings::KeybindingsPage;
        use crate::views::keybindings_state::Slot;
        use crate::views::settings_nav::SettingsPage;
        use gpui::{Context, Entity, Window};
        use shelbi_state::keymap::{Action, GlobalAction, SidebarAction};
        type Act = Box<dyn Fn(&mut KeybindingsPage, &mut Window, &mut Context<KeybindingsPage>)>;
        let nav_up = Action::Sidebar(SidebarAction::NavUp);
        let states: Vec<(&str, Act)> = vec![
            ("capture", Box::new(move |p, _, cx| p.offscreen_capture(nav_up, Slot::Chord(0), cx))),
            (
                "add",
                Box::new(|p, _, cx| p.offscreen_capture(Action::Global(GlobalAction::OpenPalette), Slot::New, cx)),
            ),
            (
                "conflict",
                Box::new(move |p, _, cx| {
                    p.offscreen_capture(nav_up, Slot::Chord(0), cx);
                    p.offscreen_key("r", cx);
                }),
            ),
            ("search", Box::new(|p, window, cx| p.offscreen_search("column", window, cx))),
            ("picker", Box::new(|p, _, cx| p.offscreen_open_picker(cx))),
            (
                "customized",
                Box::new(|p, _, cx| {
                    p.offscreen_show("defaults:\n  sidebar:\n    nav_up: [w, up]\n    refresh: []\n", cx)
                }),
            ),
        ];
        for (name, act) in states {
            let handle = self.open();
            handle
                .update(&mut self.cx, |w, window, cx| {
                    w.offscreen_open_settings(SettingsPage::Keybindings, window, cx)
                })
                .unwrap();
            // After the page has read `keys.yaml`, which would replace a
            // state set before it.
            self.settle(handle);
            handle
                .update(&mut self.cx, |w, window, cx| {
                    let page: Option<Entity<KeybindingsPage>> = w.offscreen_settings().map(|s| s.read(cx).keybindings());
                    if let Some(page) = page {
                        page.update(cx, |p, cx| act(p, window, cx));
                    }
                })
                .unwrap();
            self.save(handle, &format!("state-keybindings-{name}"));
        }
    }

    /// The form components as the Figma components page draws them, each
    /// at its variant sheet's size and positions:
    /// About Shelbi and Acknowledgements (`dt-about-bundled-software`), each
    /// in a window of its frame's size: `about` and `bundled-software`
    /// (Source Code Pro selected, as the frame draws it), plus the states
    /// no frame draws: `state-bundled-first` (as it opens, the first entry
    /// selected), `state-bundled-search` (`mit` typed), `state-bundled-crates`
    /// (the long tail's entry) and `state-bundled-no-match`. The frames are
    /// in `about/` as `about.png` and `bundled-software.png` (Dark only).
    fn about_windows(&mut self) {
        use crate::views::about::AboutView;
        use crate::views::bundled_software::{BundledSoftwareView, SIZE};

        let (w, h) = crate::views::about::SIZE;
        let handle = self
            .cx
            .open_offscreen_window(size(px(w), px(h)), |window, cx| cx.new(|cx| AboutView::new(window, cx)))
            .expect("open the offscreen window");
        self.settle_sheet(handle.into());
        self.write_sized(handle.into(), "about", w as u32);
        let _ = self.cx.update_window(handle.into(), |_, window, _| window.remove_window());

        let entries = crate::licenses::entries();
        let index = |name: &str| entries.iter().position(|e| e.name == name).expect("an entry");
        let states: [(&str, &str, Option<usize>); 5] = [
            ("bundled-software", "", Some(index("Source Code Pro"))),
            ("state-bundled-first", "", None),
            ("state-bundled-search", "mit", None),
            ("state-bundled-crates", "", Some(index("Rust crates"))),
            ("state-bundled-no-match", "zzzz", None),
        ];
        for (screen, query, select) in states {
            let handle = self
                .cx
                .open_offscreen_window(size(px(SIZE.0), px(SIZE.1)), |window, cx| {
                    cx.new(|cx| BundledSoftwareView::new(window, cx))
                })
                .expect("open the offscreen window");
            let view = handle.entity(&self.cx).expect("the view");
            let window: AnyWindowHandle = handle.into();
            self.cx
                .update_window(window, |_, window, cx| {
                    view.update(cx, |v, cx| {
                        if !query.is_empty() {
                            v.set_query(query, window, cx);
                        }
                        if let Some(ix) = select {
                            v.select(ix, cx);
                        }
                    })
                })
                .unwrap();
            // The license parses off the UI thread: draw until it has.
            self.settle_sheet(window);
            self.settle_sheet(window);
            if screen == "bundled-software" {
                let selected = view.read_with(&self.cx, |v, _| v.browser().selected());
                self.expect("bundled-software: Source Code Pro selected", selected == select);
            }
            if screen == "state-bundled-first" {
                let selected = view.read_with(&self.cx, |v, _| v.browser().selected());
                self.expect("bundled-software: opens on the first entry", selected == Some(0));
            }
            self.write_sized(window, screen, SIZE.0 as u32);
            let _ = self.cx.update_window(window, |_, window, _| window.remove_window());
        }
    }

    /// The Add Project wizard (`dt-add-project-wizard`) in an 800 × 580
    /// window, over a canned folder check (nothing reads the disk): `add-
    /// project-folder` (no folder yet, Figma 2481:8574), `add-project-
    /// folder-selected` (`~/Projects/my-project`, a Git repository,
    /// 2481:8810) and `add-project-settings-in-repo` / `-global` (each card
    /// selected, 2483:8653 / 2483:8897). `SHELBI_VISUAL_ONLY=add-project`
    /// renders just these.
    fn add_project_windows(&mut self) {
        use crate::views::add_project::{AddProjectView, Engine, SIZE};
        use shelbi_app::add_project_wizard::{FolderCheck, Focus, Origin, Repo};
        use shelbi_core::ConfigMode;

        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/Users/me"));
        let folder = home.join("Projects/my-project");
        let engine = Engine {
            check: Arc::new({
                let home = home.clone();
                move |_, root, _| {
                    let path = shelbi_orchestrator::project_create::absolutize(&home, Path::new(root.trim()));
                    FolderCheck {
                        in_repo_dir: path.join(".shelbi"),
                        global_dir: Some(home.join(".shelbi/projects/my-project")),
                        path,
                        name: Some("My project".into()),
                        slug: Some("my-project".into()),
                        repo: Some(Repo::Git),
                        problem: None,
                    }
                }
            }),
            create: Arc::new(|_, _, _| Err(shelbi_orchestrator::project_create::AddProjectError::Failed("offscreen".into()))),
            refresh: Arc::new(|| {}),
        };
        let states: [(&str, Option<ConfigMode>, bool); 4] = [
            ("add-project-folder", None, false),
            ("add-project-folder-selected", None, true),
            ("add-project-settings-in-repo", Some(ConfigMode::InRepo), true),
            ("add-project-settings-global", Some(ConfigMode::Global), true),
        ];
        for (screen, mode, pick) in states {
            let engine = engine.clone();
            let handle = self
                .cx
                .open_offscreen_window(size(px(SIZE.0), px(SIZE.1)), |window, cx| {
                    cx.new(|cx| {
                        AddProjectView::new(Origin::Workspace, None, home.clone(), engine, Rc::new(|_, _, _| Ok(())), window, cx)
                    })
                })
                .expect("open the offscreen window");
            let view = handle.entity(&self.cx).expect("the view");
            let window: AnyWindowHandle = handle.into();
            if pick {
                self.cx
                    .update_window(window, |_, window, cx| {
                        view.update(cx, |v, cx| v.folder_picked(Some(&folder), window, cx))
                    })
                    .unwrap();
                self.settle_sheet(window);
            }
            if let Some(mode) = mode {
                self.cx
                    .update_window(window, |_, window, cx| {
                        view.update(cx, |v, cx| {
                            v.click(Focus::Primary, window, cx);
                            v.click(Focus::Card(mode), window, cx);
                        })
                    })
                    .unwrap();
            }
            self.settle_sheet(window);
            self.settle_sheet(window);
            self.write_sized(window, screen, SIZE.0 as u32);
            let _ = self.cx.update_window(window, |_, window, _| window.remove_window());
        }

        // Screen 03 (`dt-project-creation-checklist`): each stage running,
        // the ones before it checked (2484:8673, 2514:45915, 2514:45972);
        // `-failed` (no Figma frame) stopped in Creating agents. Screen 04
        // (`dt-wizard-workflow-summary`): `add-project-ready` as Figma's
        // 2484:8897 draws it, and with no frames `-ready-warned` (the commit
        // guard's disclosure under the cards), `-opening` (Okay pressed, the
        // project opening) and `-open-failed` (the open failed, Okay to try
        // again). Reduce Motion holds the spinners at their drawn angle.
        use crate::views::add_project::Report;
        use shelbi_app::add_project_wizard::Stage;
        use shelbi_orchestrator::project_create::AddProjectError;
        // A frame's reports, and how the run ended (if it has).
        type Run = (Vec<Report>, Option<Result<String, AddProjectError>>);
        let up_to = |i: usize| {
            let mut reports = Vec::new();
            for stage in &Stage::ALL[..i] {
                reports.push(Report::Started(*stage));
                reports.push(Report::Finished(*stage));
            }
            reports
        };
        let running = |i: usize| -> Run {
            let mut reports = up_to(i);
            reports.push(Report::Started(Stage::ALL[i]));
            (reports, None)
        };
        let mut failed = running(1).0;
        failed.push(Report::Failed(
            Stage::Agents,
            "Permission denied (os error 13) writing ~/.shelbi/projects/my-project/agents".into(),
        ));
        let mut created = up_to(3);
        created.insert(
            0,
            Report::Warning(shelbi_orchestrator::githook::hub_branch_guard_disclosure(&home.join("Projects/my-project"))),
        );
        let ready = || -> Run { (up_to(3), Some(Ok("my-project".into()))) };
        // Okay pressed: `None` still opening, `Some` the open's failure.
        let open_failed: Option<Option<&str>> = Some(Some("Shelbi couldn’t open a window for “my-project”."));
        let frames: [(&str, Run, Option<Option<&str>>); 8] = [
            ("add-project-writing-config", running(0), None),
            ("add-project-creating-agents", running(1), None),
            ("add-project-adding-workflows", running(2), None),
            ("add-project-failed", (failed, Some(Err(AddProjectError::Failed("failed".into())))), None),
            ("add-project-ready", ready(), None),
            ("add-project-ready-warned", (created, Some(Ok("my-project".into()))), None),
            ("add-project-opening", ready(), Some(None)),
            ("add-project-open-failed", ready(), open_failed),
        ];
        self.cx.update(|cx| cx.set_reduce_motion(true));
        for (screen, (reports, result), open) in frames {
            let engine = engine.clone();
            let handle = self
                .cx
                .open_offscreen_window(size(px(SIZE.0), px(SIZE.1)), |window, cx| {
                    cx.new(|cx| {
                        AddProjectView::new(Origin::Workspace, None, home.clone(), engine, Rc::new(|_, _, _| Ok(())), window, cx)
                    })
                })
                .expect("open the offscreen window");
            let view = handle.entity(&self.cx).expect("the view");
            let window: AnyWindowHandle = handle.into();
            self.cx
                .update_window(window, |_, window, cx| {
                    view.update(cx, |v, cx| v.folder_picked(Some(&folder), window, cx))
                })
                .unwrap();
            self.settle_sheet(window);
            self.cx
                .update_window(window, |_, window, cx| {
                    view.update(cx, |v, cx| v.preview_creating(reports, result, window, cx))
                })
                .unwrap();
            if let Some(failure) = open {
                self.cx
                    .update_window(window, |_, window, cx| {
                        view.update(cx, |v, cx| v.preview_open(failure, window, cx))
                    })
                    .unwrap();
            }
            self.settle_sheet(window);
            self.write_sized(window, screen, SIZE.0 as u32);
            let _ = self.cx.update_window(window, |_, window, _| window.remove_window());
        }
        self.cx.update(crate::theme::motion::sync_reduce_motion);
    }

    /// The first launch's CLI setup (`dt-wizard-cli-setup`) in the wizard's
    /// 800 × 580, over scripted bootstrap states (nothing runs): `cli-
    /// check` (looking for the CLI, Figma 2478:8507), `cli-install` (an
    /// install's checks, 2480:8522), `cli-ready` (2480:8577), and `cli-
    /// failed` (an offline install; no frame).
    /// `SHELBI_VISUAL_ONLY=cli-setup` renders just these.
    fn cli_setup_windows(&mut self) {
        use crate::platform::install::Step;
        use crate::platform::{BootstrapEvent, BootstrapState};
        use crate::views::add_project::SIZE;
        use crate::views::cli_setup::{CliSetupView, Deps};

        let screens: [(&str, Vec<BootstrapState>); 4] = [
            ("cli-check", vec![BootstrapState::FindingCli]),
            (
                "cli-install",
                vec![
                    BootstrapState::InstallingCli { step: Step::Installing },
                    BootstrapState::FindingCli,
                ],
            ),
            ("cli-ready", vec![BootstrapState::Ready(crate::app::inert_ready())]),
            // No frame: the offline install's failure screen, for a look.
            (
                "cli-failed",
                vec![BootstrapState::Failed(crate::platform::Failure::InstallFailed(
                    crate::platform::install::InstallError::new(
                        crate::platform::install::InstallErrorKind::Network,
                        "The CLI couldn't be downloaded from GitHub.",
                    )
                    .with_detail([
                        "https://github.com/jlong/shelbi/releases/download/v0.11.0/checksums.txt".to_string(),
                        "curl: (6) Could not resolve host: github.com".to_string(),
                    ]),
                ))],
            ),
        ];
        for (screen, states) in screens {
            let deps = Deps {
                start: Rc::new(move |_| {
                    let (tx, rx) = async_channel::unbounded();
                    for state in states.clone() {
                        let _ = tx.send_blocking(BootstrapEvent::State(state));
                    }
                    rx
                }),
                plan: Arc::new(Vec::new),
                next: Rc::new(|_, _, _| {}),
                dwell: Duration::from_secs(3600),
            };
            let handle = self
                .cx
                .open_offscreen_window(size(px(SIZE.0), px(SIZE.1)), |window, cx| {
                    cx.new(|cx| CliSetupView::new(deps, window, cx))
                })
                .expect("open the offscreen window");
            let window: AnyWindowHandle = handle.into();
            self.settle_sheet(window);
            self.write_sized(window, screen, SIZE.0 as u32);
            let _ = self.cx.update_window(window, |_, window, _| window.remove_window());
        }
    }

    /// Run and draw a window that isn't the shell a few times over.
    fn settle_sheet(&mut self, window: AnyWindowHandle) {
        for _ in 0..5 {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(20));
            self.cx
                .update_window(window, |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
        }
        self.cx.run_until_parked();
    }

    /// Save `window` as `<screen>-<theme>.png`, at 1x unless `--2x`.
    fn write_sized(&mut self, window: AnyWindowHandle, screen: &str, width: u32) {
        let image = self.cx.capture_screenshot(window).expect("render to image");
        let image = if self.native || image.width() == width {
            image
        } else {
            let height = image.height() * width / image.width();
            image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle)
        };
        let path = self.out.join(format!("{screen}-{}.png", self.theme));
        image.save(&path).expect("save the frame");
        println!("saved {} ({}x{})", path.display(), image.width(), image.height());
    }

    /// `components/<name>-<theme>.png`, beside the page's
    /// `components/<name>.png` (Light). Light sits on the page's `#f5f5f5`,
    /// Dark on the canvas.
    fn components(&mut self) {
        let dir = self.out.join("components");
        std::fs::create_dir_all(&dir).expect("create the components directory");
        for (name, w, h) in COMPONENT_SHEETS {
            crate::theme::motion::reset_pointers();
            if name == "buttons" {
                for i in 0..BUTTON_SHEET_ROWS.len() {
                    for (j, state) in BUTTON_SHEET_COLUMNS.iter().enumerate() {
                        match state {
                            SheetState::FocusedHover => crate::theme::motion::hold_pointer(button_sheet_id(i, j), false),
                            SheetState::FocusedActive => crate::theme::motion::hold_pointer(button_sheet_id(i, j), true),
                            _ => {}
                        }
                    }
                }
            }
            let handle = self
                .cx
                .open_offscreen_window(size(px(w), px(h)), |_, cx| cx.new(|_| Sheet(name)))
                .expect("open the offscreen window");
            let window: AnyWindowHandle = handle.into();
            for _ in 0..3 {
                self.cx.run_until_parked();
                self.cx
                    .update_window(window, |_, window, cx| window.draw(cx).clear(cx))
                    .unwrap();
            }
            let image = self.cx.capture_screenshot(window).expect("render to image");
            if name == "buttons" {
                // At the backing scale, before any downsampling blurs the
                // 1px ring into its neighbors.
                self.expect_button_focus(&image, image.width() as f32 / w);
            }
            if name == "button-sizes" {
                self.expect_button_sizes(&image, image.width() as f32 / w);
            }
            let image = if self.native || image.width() == w as u32 {
                image
            } else {
                image::imageops::resize(&image, w as u32, h as u32, image::imageops::FilterType::Triangle)
            };
            let path = dir.join(format!("{name}-{}.png", self.theme));
            image.save(&path).expect("save the sheet");
            println!("saved {} ({}x{})", path.display(), image.width(), image.height());
            self.cx
                .update_window(window, |_, window, _| window.remove_window())
                .unwrap();
            self.cx.run_until_parked();
        }
    }

    /// On the `buttons` sheet, every button's fill and focus against the
    /// theme's own colors, at mid-height: 3pt in from its left edge (fill,
    /// not label) its state's fill over the page, focused or not (hovered
    /// and pressed included), so focus never tints the inside; 0.5pt out
    /// the style's ring under its glow, and 3pt out the glow alone (Success
    /// green, Danger pink, the rest the default cyan); at rest and disabled,
    /// the page untouched outside it, and disabled the fill at 40%.
    fn expect_button_focus(&mut self, image: &image::RgbaImage, scale: f32) {
        use crate::theme::style::button_colors;
        use gpui::Hsla;
        let theme = self.cx.update(|cx| *cx.global::<crate::theme::Theme>());
        let pixel = |x: f32, y: f32| image.get_pixel((x * scale) as u32, (y * scale) as u32).0;
        let page = pixel(2.0, 2.0);
        // Straight alpha over an opaque color, as the renderer blends.
        let over = |c: Hsla, under: [u8; 4]| -> [u8; 4] {
            let c = c.to_rgb();
            let mix = |v: f32, u: u8| (v * 255.0 * c.a + f32::from(u) * (1.0 - c.a)).round() as u8;
            [mix(c.r, under[0]), mix(c.g, under[1]), mix(c.b, under[2]), 255]
        };
        let close = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 3);
        let check = |run: &mut Self, what: String, got: [u8; 4], want: [u8; 4]| {
            run.expect(&format!("buttons: {what} (got {got:?}, want {want:?})"), close(got, want));
        };
        for (i, row) in BUTTON_SHEET_ROWS.iter().enumerate() {
            let c = button_colors(row.kind(), &theme);
            let y = button_sheet_y(i) + row.height() / 2.0;
            for (j, state) in BUTTON_SHEET_COLUMNS.iter().enumerate() {
                let x = button_sheet_x(j);
                let what = format!("{row:?} {state:?}");
                let fill = match state {
                    SheetState::Rest | SheetState::Focused => c.fill,
                    SheetState::FocusedHover => c.hover,
                    SheetState::FocusedActive => c.pressed,
                    SheetState::Disabled => c.fill.opacity(0.4),
                };
                check(self, format!("{what} fill"), pixel(x + 3.0, y), over(fill, page));
                match state {
                    SheetState::Rest | SheetState::Disabled => {
                        check(self, format!("{what} has no ring"), pixel(x - 0.5, y), page);
                        check(self, format!("{what} has no glow"), pixel(x - 3.0, y), page);
                    }
                    _ => {
                        let ring = over(c.focus.glow, over(c.focus.ring, page));
                        check(self, format!("{what} ring"), pixel(x - 0.5, y), ring);
                        check(self, format!("{what} glow"), pixel(x - 3.0, y), over(c.focus.glow, page));
                        check(self, format!("{what} glow ends at 4pt"), pixel(x - 4.5, y), page);
                    }
                }
            }
        }
    }

    /// On the `button-sizes` sheet, each button's height in pixels: 8pt in
    /// (past the corner, clear of the glyphs), its style's fill 1.5pt inside
    /// its top and bottom edges (40% for the disabled column) and the page
    /// 1.5pt outside them, so X-Small is 24 tall, Small 28 and Base 32
    /// whatever it holds.
    fn expect_button_sizes(&mut self, image: &image::RgbaImage, scale: f32) {
        use crate::theme::style::button_colors;
        let theme = self.cx.update(|cx| *cx.global::<crate::theme::Theme>());
        let pixel = |x: f32, y: f32| image.get_pixel((x * scale) as u32, (y * scale) as u32).0;
        let page = pixel(2.0, 2.0);
        let over = |c: gpui::Hsla, under: [u8; 4]| -> [u8; 4] {
            let c = c.to_rgb();
            let mix = |v: f32, u: u8| (v * 255.0 * c.a + f32::from(u) * (1.0 - c.a)).round() as u8;
            [mix(c.r, under[0]), mix(c.g, under[1]), mix(c.b, under[2]), 255]
        };
        let close = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 3);
        for (i, row) in size_sheet_rows().into_iter().enumerate() {
            for (j, &(kind, disabled)) in SIZE_SHEET_COLUMNS.iter().enumerate() {
                let fill = button_colors(kind, &theme).fill;
                let fill = over(if disabled { fill.opacity(0.4) } else { fill }, page);
                let (x, top, h) = (size_sheet_x(j) + 8.0, size_sheet_y(i), row.height());
                let what = format!("button-sizes: {:?} {:?} {kind:?}{}", row.1, row.2, if disabled { " disabled" } else { "" });
                for (edge, y, want) in [
                    ("top inside", top + 1.5, fill),
                    ("bottom inside", top + h - 1.5, fill),
                    ("above", top - 1.5, page),
                    ("below", top + h + 1.5, page),
                ] {
                    let got = pixel(x, y);
                    self.expect(&format!("{what} {edge} (got {got:?}, want {want:?})"), close(got, want));
                }
            }
        }
    }

    /// The Load for review dialog (one free slot, the shape John's
    /// screenshot shows) with the keyboard on Load, as it opens, then on
    /// Cancel: the focused button keeps its own fill and label (Load the
    /// Primary style) and only gains the ring and glow.
    fn review_confirm_states(&mut self, under: &View) {
        use crate::views::overlay::ActiveOverlay;
        use shelbi_app::overlay::review_confirm::Slot;
        let slot = Slot {
            name: "review".into(),
            occupant: None,
        };
        let title = "Adopt Command Grille as the Shelbi desktop app icon";
        for (tabs, screen) in [(0, "state-review-confirm-load-focused"), (1, "state-review-confirm-cancel-focused")] {
            let handle = self.open();
            self.click(handle, under.clone());
            self.settle(handle);
            let mut overlay = ActiveOverlay::review_confirm("dt-command-grille-icon", title, vec![slot.clone()], 1);
            if let ActiveOverlay::ReviewConfirm { dialog, .. } = &mut overlay {
                for _ in 0..tabs {
                    dialog.handle_key(shelbi_state::keymap::KeyChord::parse("tab").expect("a chord"));
                }
            }
            handle
                .update(&mut self.cx, |w, window, cx| w.offscreen_open_overlay(Some(overlay), window, cx))
                .unwrap();
            self.save(handle, screen);
        }
    }

    fn all(&mut self) {
        let orch = View::Session("orch".into());
        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "panels") {
            // Just the task panels, for QA on them.
            self.review(&orch, "review", |_, _| {});
            self.workspace(&orch, "workspace", |_, _| {});
            self.panel_states(&orch);
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "settings") {
            self.settings();
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "workspaces") {
            self.settings_workspaces();
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "issues") {
            self.issues();
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "about") {
            self.about_windows();
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "add-project") {
            self.add_project_windows();
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "cli-setup") {
            self.cli_setup_windows();
            return;
        }

        if std::env::var(ONLY_VAR).is_ok_and(|v| v == "buttons") {
            self.components();
            self.review_confirm_states(&orch);
            return;
        }

        // The orchestrator chat.
        let handle = self.open();
        self.click(handle, orch.clone());
        self.settle(handle);
        self.save(handle, "orchestrator");
        self.terminal_states(&orch);

        self.issues();

        // The Activity feed, the review row selected.
        let handle = self.open();
        self.click(handle, View::Activity);
        let activity = self.views(handle).1.expect("the Activity view");
        let loaded = self.wait(handle, Duration::from_secs(10), |cx| {
            activity.read_with(cx, |v, _| v.state().feed().rows().count() >= 8)
        });
        self.expect("activity: the eight mockup events load", loaded);
        let verbs: Vec<String> = activity.read_with(&self.cx, |v, _| {
            v.state().feed().rows().map(|r| v.state().line(r).verb).collect()
        });
        println!("      rows: {verbs:?}");
        self.expect(
            "activity: the mockup's verbs, available and started included",
            verbs == ["merged", "promoted", "finished", "is building", "is building", "available", "finished", "started"],
        );
        self.expect(
            "activity: newest first, from the zen merge",
            verbs.first().map(String::as_str) == Some("merged"),
        );
        activity.update(&mut self.cx, |v, cx| {
            // Today, then its rows: the third row is the review's.
            v.state_mut().select_item(3);
            cx.notify();
        });
        self.save(handle, "activity");

        self.review(&orch, "review", |_, _| {});
        self.workspace(&orch, "workspace", |_, _| {});
        self.panel_states(&orch);
        // The idle frame shows charlie idle (no task), while every other
        // frame's sidebar has it awaiting input on a task: the mockups
        // disagree, and an idle page belongs to a task-less workspace.
        self.sidebar = idle_charlie_model();
        let handle = self.open();
        self.click(handle, orch.clone());
        self.click(handle, View::Session("charlie".into()));
        let page = handle
            .update(&mut self.cx, |w, _, _| w.offscreen_column())
            .map(|f| f.workspace.is_none())
            .unwrap_or(false);
        self.expect("workspace-idle: the sidebar stays, no panel pushes in", page);
        self.save(handle, "workspace-idle");
        self.sidebar = sidebar_model();

        // Machines, the remote reachable. Its frame titles the two
        // in-progress tasks shorter than the Issues frame does: the mockups
        // disagree, so retitle them for this screen only.
        let titles: Vec<(&str, String)> = MACHINES_TITLES
            .iter()
            .map(|(id, title)| (*id, retitle(id, title)))
            .collect();
        self.machines("machines", shelbi_core::Reachability::Reachable, 0);
        // States the frame doesn't draw: the remote down (its row selected),
        // and an idle row and the review slot selected.
        let down = shelbi_core::Reachability::Unreachable {
            error: "ssh: connect to host gpu.lan port 22: Operation timed out".into(),
        };
        self.machines("machines-unreachable", down, 4);
        self.machines("machines-idle-selected", shelbi_core::Reachability::Reachable, 2);
        self.machines("machines-review-selected", shelbi_core::Reachability::Reachable, 3);
        for (id, title) in &titles {
            retitle(id, title);
        }

        self.settings();

        // The palette over the orchestrator.
        self.palette(&orch, "", "command-palette");
        // The states the frame doesn't draw (no reference; checked by eye):
        // a query with matches, one with none, Tab into the Projects column,
        // and a project row selected there.
        for (keys, screen) in PALETTE_STATES {
            self.palette(&orch, keys, screen);
        }
        self.sidebar_states(&orch);
        self.review_confirm_states(&orch);
        self.about_windows();
        self.add_project_windows();
        self.cli_setup_windows();

        let missing: Vec<&str> = SCREENS
            .iter()
            .copied()
            .filter(|s| !self.out.join(format!("{s}-{}.png", self.theme)).exists())
            .collect();
        self.expect(&format!("all {} screens saved", SCREENS.len()), missing.is_empty());
    }

    /// The Review panel in the sidebar's column, the reviewer beside it,
    /// saved as `screen` after `then` acts on the open window.
    fn review(&mut self, orch: &View, screen: &str, then: impl FnOnce(&mut Self, WindowHandle<ShelbiWindow>)) {
        let handle = self.open();
        self.click(handle, orch.clone());
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_review(review_info(), window, cx))
            .unwrap();
        self.settle(handle);
        let review = handle.update(&mut self.cx, |w, _, _| w.offscreen_review()).unwrap();
        self.expect(&format!("{screen}: the panel opens"), review.is_some());
        if let Some(review) = review {
            review.update(&mut self.cx, |v, cx| {
                v.update_row(Some(sidebar_model().reviews[0].clone()), cx)
            });
        }
        self.settle(handle);
        then(self, handle);
        self.save(handle, screen);
    }

    /// A working workspace, saved as `screen` after `then` acts on the
    /// open window.
    fn workspace(&mut self, orch: &View, screen: &str, then: impl FnOnce(&mut Self, WindowHandle<ShelbiWindow>)) {
        let handle = self.open();
        self.click(handle, orch.clone());
        self.click(handle, View::Session("alpha".into()));
        self.settle(handle);
        then(self, handle);
        self.save(handle, screen);
    }

    /// The task panels' states the frames don't draw
    /// (`state-{review,workspace}-<name>-<theme>.png`; no reference, checked
    /// by eye): the task detail More opens over the screen, each view picked
    /// with its ⌘N, and Approve and Reject hovered and pressed.
    fn panel_states(&mut self, orch: &View) {
        use gpui::{point, Modifiers, MouseButton};

        // Points inside the frames' controls (State=Review / Workspace):
        // More at (18, 230), Approve 91x28 at (12, 560), Reject 80x28 at
        // (115, 560).
        let more = point(px(30.0), px(236.0));
        let approve = point(px(57.0), px(574.0));
        let reject = point(px(155.0), px(574.0));
        let click = |run: &mut Self, handle: WindowHandle<ShelbiWindow>, at| {
            run.cx.simulate_click(handle.into(), at, Modifiers::none());
            run.settle(handle);
        };
        // More, then the detail over the screen once the board has its issue.
        let more_detail = |screen: &'static str| {
            move |run: &mut Self, handle: WindowHandle<ShelbiWindow>| {
                click(run, handle, more);
                let over = handle.read_with(&run.cx, |w, cx| w.offscreen_detail_over(cx)).unwrap_or(false);
                run.expect(&format!("{screen}: More opens the task detail over the screen"), over);
                let board = run.views(handle).0;
                let loaded = run.wait(handle, Duration::from_secs(10), |cx| {
                    board.as_ref().is_some_and(|b| b.read_with(cx, |v, _| v.detail_loaded()))
                });
                run.expect(&format!("{screen}: the detail loads its issue"), loaded);
            }
        };
        let keys = |keys: &'static str| {
            move |run: &mut Self, handle: WindowHandle<ShelbiWindow>| {
                run.cx.simulate_keystrokes(handle.into(), keys);
                run.settle(handle);
            }
        };

        self.review(orch, "state-review-more", more_detail("state-review-more"));
        for n in 2..=5 {
            let chord: &'static str = ["", "", "cmd-2", "cmd-3", "cmd-4", "cmd-5"][n];
            self.review(orch, &format!("state-review-view-{n}"), keys(chord));
        }
        for (name, at) in [("approve", approve), ("reject", reject)] {
            self.review(orch, &format!("state-review-{name}-hover"), |run, handle| {
                run.cx.simulate_mouse_move(handle.into(), at, None, Modifiers::none());
                run.settle(handle);
            });
            self.review(orch, &format!("state-review-{name}-pressed"), |run, handle| {
                run.cx.simulate_mouse_move(handle.into(), at, None, Modifiers::none());
                run.cx.simulate_mouse_down(handle.into(), at, MouseButton::Left, Modifiers::none());
                run.settle(handle);
            });
        }

        // The workspace panel's worktree row is 8pt taller, so More is too.
        self.workspace(orch, "state-workspace-more", more_detail("state-workspace-more"));
        for n in 2..=4 {
            let chord: &'static str = ["", "", "cmd-2", "cmd-3", "cmd-4"][n];
            self.workspace(orch, &format!("state-workspace-view-{n}"), keys(chord));
        }
    }

    /// The terminal states the frames don't draw, over the orchestrator
    /// chat: a selection, the find bar, "Jump to bottom", and the font a
    /// step bigger and smaller (`state-<name>-<theme>.png`; no reference to
    /// diff against, they are checked by eye).
    fn terminal_states(&mut self, orch: &View) {
        use crate::elements::terminal::{FontZoom, TerminalPane, TerminalSettings};
        use shelbi_term::selection::Point as GridPoint;

        let open = |run: &mut Self| -> (WindowHandle<ShelbiWindow>, Option<gpui::Entity<TerminalPane>>) {
            let handle = run.open();
            run.click(handle, orch.clone());
            run.settle(handle);
            let pane = handle.update(&mut run.cx, |w, _, cx| w.offscreen_chat_pane(cx)).unwrap();
            (handle, pane)
        };

        // "Both agents are running…" selected into the next line.
        let (handle, pane) = open(self);
        self.expect("states: the orchestrator's terminal attaches", pane.is_some());
        let Some(pane) = pane else { return };
        pane.update(&mut self.cx, |p, cx| {
            let view = p.terminal_view_mut();
            view.begin_selection(GridPoint::new(23, 2));
            view.update_selection(GridPoint::new(24, 30));
            cx.notify();
        });
        self.save(handle, "state-orchestrator-selection");

        // The find bar, its matches highlighted.
        let (handle, pane) = open(self);
        if let Some(pane) = pane {
            pane.update(&mut self.cx, |p, cx| p.offscreen_find("issues", cx));
        }
        self.save(handle, "state-orchestrator-find");

        // Scrolled back past new output: "Jump to bottom".
        let (handle, pane) = open(self);
        if let Some(pane) = pane {
            pane.update(&mut self.cx, |p, cx| {
                p.feed("\r\n".repeat(40).as_bytes(), cx);
                p.terminal_view_mut().scroll_up(40);
                cx.notify();
            });
        }
        self.save(handle, "state-orchestrator-jump");

        // ⌘+ twice and ⌘− twice: the grid keeps its inset at each size.
        for (name, step) in [("zoom-in", FontZoom::Bigger), ("zoom-out", FontZoom::Smaller)] {
            let (handle, pane) = open(self);
            self.cx.update(|cx| {
                TerminalSettings::zoom(step, cx);
                TerminalSettings::zoom(step, cx);
            });
            // The re-measured grid's size goes out after a debounce.
            self.settle(handle);
            if let Some(pane) = pane {
                pane.update(&mut self.cx, |p, _| p.offscreen_flush_resize());
            }
            self.save(handle, &format!("state-orchestrator-{name}"));
            self.cx.update(|cx| {
                TerminalSettings::zoom(FontZoom::ActualSize, cx);
            });
        }
    }

    /// The sidebar states the frames don't draw, over the orchestrator chat
    /// (`state-sidebar-<name>-<theme>.png`; no reference, checked by eye):
    /// the review sections collapsed, every section collapsed, Zen off, and
    /// the sidebar hidden.
    fn sidebar_states(&mut self, orch: &View) {
        use crate::views::sidebar::Section;

        for (name, sections) in [
            ("reviews-collapsed", &[Section::Ready, Section::Queued][..]),
            ("all-collapsed", &[Section::Ready, Section::Queued, Section::Workspaces][..]),
        ] {
            let handle = self.open();
            self.click(handle, orch.clone());
            handle
                .update(&mut self.cx, |w, _, cx| {
                    for section in sections {
                        w.offscreen_toggle_section(*section, cx);
                    }
                })
                .unwrap();
            self.save(handle, &format!("state-sidebar-{name}"));
        }

        // The Queued for review section folding closed, and back open, a
        // few frames in (`state-sidebar-fold-<closing|opening>-<ms>`).
        for (name, start_collapsed) in [("closing", false), ("opening", true)] {
            let handle = self.open();
            self.click(handle, orch.clone());
            if start_collapsed {
                handle
                    .update(&mut self.cx, |w, _, cx| w.offscreen_toggle_section(Section::Queued, cx))
                    .unwrap();
                self.settle(handle);
            }
            handle
                .update(&mut self.cx, |w, _, cx| w.offscreen_toggle_section(Section::Queued, cx))
                .unwrap();
            let start = self.now.get();
            for ms in [0, 40, 90, 220] {
                self.now.set(start + Duration::from_millis(ms));
                self.capture_frame(handle, &format!("state-sidebar-fold-{name}-{ms}"));
            }
            self.save(handle, &format!("state-sidebar-fold-{name}-settled"));
        }

        self.sidebar.zen_mode = shelbi_state::ZenModeState::Off;
        let handle = self.open();
        self.click(handle, orch.clone());
        self.save(handle, "state-sidebar-zen-off");
        self.sidebar = sidebar_model();

        let handle = self.open();
        self.click(handle, orch.clone());
        handle
            .update(&mut self.cx, |w, _, cx| w.offscreen_toggle_sidebar(cx))
            .unwrap();
        self.save(handle, "state-sidebar-hidden");
    }

    fn views(&mut self, handle: WindowHandle<ShelbiWindow>) -> NativeViews {
        handle.update(&mut self.cx, |w, _, _| w.offscreen_views()).unwrap()
    }

    fn column_counts(&mut self, handle: WindowHandle<ShelbiWindow>) -> Vec<usize> {
        let Some(board) = self.views(handle).0 else {
            return Vec::new();
        };
        board.update(&mut self.cx, |v, _| {
            v.state_mut().model().columns.iter().map(|c| c.cards.len()).collect()
        })
    }

    /// The board, a card selected, as the Issues frame shows it; the
    /// toolbar's undrawn states; then a card's detail over the board.
    fn issues(&mut self) {
        let handle = self.open();
        self.click(handle, View::Issues);
        let loaded = self.wait_for_board(handle);
        self.expect("issues: the board loads the mockup's 19 open cards", loaded);
        let counts = self.column_counts(handle);
        println!("      columns: {counts:?}");
        self.expect(
            "issues: the column counts read 10 / 6 / 2 / 1",
            counts.iter().take(4).copied().eq([10, 6, 2, 1]),
        );
        self.capture(handle, "state-issues-unfiltered");
        self.frame_filter(handle);
        let filtered = self.column_counts(handle);
        self.expect(
            "issues: the task filter keeps every card",
            filtered.iter().take(4).copied().eq([10, 6, 2, 1]),
        );
        if let (Some(board), _, _) = self.views(handle) {
            board.update(&mut self.cx, |v, cx| {
                v.state_mut().select(1, 0);
                cx.notify();
            });
        }
        self.capture(handle, "issues");

        // Each filter chip with its menu open: the chip in the Select style
        // whether or not its filter is set (Workspace: unset; Workflow: set).
        for (screen, workspace) in [("state-issues-workspace-open", true), ("state-issues-workflow-open", false)] {
            if let (Some(board), _, _) = self.views(handle) {
                board.update(&mut self.cx, |v, cx| {
                    let state = v.state_mut();
                    if workspace {
                        state.open_workspace_dropdown();
                    } else {
                        state.open_workflow_dropdown();
                    }
                    cx.notify();
                });
            }
            self.capture(handle, screen);
            if let (Some(board), _, _) = self.views(handle) {
                board.update(&mut self.cx, |v, cx| {
                    v.state_mut().mode = crate::views::board_state::Mode::Board;
                    cx.notify();
                });
            }
        }

        // `/`, then a query: the search narrows the cards as it is typed.
        self.cx.simulate_keystrokes(handle.into(), "/ t e r m i n a l");
        self.settle(handle);
        let counts = self.column_counts(handle);
        println!("      searched: {counts:?}");
        self.expect(
            "issues: `terminal` narrows the board to its two cards",
            counts.iter().sum::<usize>() == 2,
        );
        self.save(handle, "state-issues-search");

        // A card's detail over the board.
        let handle = self.open();
        self.click(handle, View::Issues);
        self.wait_for_board(handle);
        self.frame_filter(handle);
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_task("desktop-app", window, cx))
            .unwrap();
        self.save(handle, "issue-detail");
        self.detail_states();

    }

    /// Set the board's workflow filter to `task`, as the Issues frames'
    /// Header shows it (quietly: no status note in the footer).
    fn frame_filter(&mut self, handle: WindowHandle<ShelbiWindow>) {
        if let (Some(board), _, _) = self.views(handle) {
            board.update(&mut self.cx, |v, cx| v.restore_workflow_filter(Some("task".into()), cx));
        }
        self.settle(handle);
    }

    /// The issue detail over the board, as the frame shows it.
    fn open_detail(&mut self) -> WindowHandle<ShelbiWindow> {
        let handle = self.open();
        self.click(handle, View::Issues);
        self.wait_for_board(handle);
        self.frame_filter(handle);
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_task("desktop-app", window, cx))
            .unwrap();
        self.settle(handle);
        handle
    }

    /// The detail's states the frame doesn't draw (no reference; checked by
    /// eye): each action hovered ([`DETAIL_ACTIONS`]), then a body too long
    /// for the modal, paged down.
    fn detail_states(&mut self) {
        let handle = self.open_detail();
        let window: AnyWindowHandle = handle.into();
        let image = self.cx.capture_screenshot(window).expect("render to image");
        eprintln!("DEBUG capture {}x{}", image.width(), image.height());
        let scale = image.width() as f32 / WIDTH as f32;
        // The action row's top: the modal (85% of the window down, centered)
        // plus its 1pt edge, the 52pt title row and the 176pt field table.
        let top = HEIGHT as f32 * 0.075 + 1.0 + 52.0 + 176.0;
        let buttons = detail_buttons(&image, scale, top + 2.0);
        self.expect(
            &format!("detail states: the {} actions found on the row", DETAIL_ACTIONS.len()),
            buttons.len() == DETAIL_ACTIONS.len(),
        );
        for (name, x) in DETAIL_ACTIONS.iter().zip(buttons) {
            let at = gpui::point(px(x), px(top + 13.0));
            self.cx.simulate_mouse_move(window, at, None, gpui::Modifiers::none());
            self.capture(handle, &format!("issue-detail-hover-{name}"));
        }
        // Park the pointer off the actions: the next window opens under it.
        self.cx
            .simulate_mouse_move(window, gpui::point(px(0.0), px(0.0)), None, gpui::Modifiers::none());
        self.cx
            .update_window(window, |_, window, _| window.remove_window())
            .unwrap();

        // The umbrella's body four times over: more than the modal holds.
        let long = format!(
            "{DESKTOP_APP_BODY}{}",
            DESKTOP_APP_BODY.replace("# Desktop application", "## Desktop application").repeat(3)
        );
        save_task("desktop-app", "Desktop application", &long, Column::review(), 1, Some("review"));
        let handle = self.open_detail();
        self.cx.simulate_keystrokes(handle.into(), "pagedown");
        self.save(handle, "issue-detail-scrolled");
        save_task("desktop-app", "Desktop application", DESKTOP_APP_BODY, Column::review(), 1, Some("review"));
    }

    fn wait_for_board(&mut self, handle: WindowHandle<ShelbiWindow>) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            self.settle(handle);
            if self.column_counts(handle).iter().take(4).sum::<usize>() >= 19 {
                return true;
            }
        }
        false
    }
}

/// The horizontal centers (in points) of the buttons on the detail's action
/// row: each run of non-canvas pixels on row `y` with canvas on both sides
/// (the modal's edge and the scrim beyond it run on to the window's edge or
/// are wider than any button).
fn detail_buttons(image: &image::RgbaImage, scale: f32, y: f32) -> Vec<f32> {
    let row = (y * scale) as u32;
    // The modal's empty bottom is the canvas.
    let canvas = *image.get_pixel(image.width() / 2, (HEIGHT as f32 * 0.92 * scale) as u32);
    let mut out = Vec::new();
    let mut start: Option<u32> = None;
    let mut after_canvas = false;
    for x in 0..image.width() {
        let is_canvas = *image.get_pixel(x, row) == canvas;
        if is_canvas {
            if let Some(s) = start.take() {
                let width = (x - s) as f32 / scale;
                if (40.0..=150.0).contains(&width) {
                    out.push((s + x) as f32 / 2.0 / scale);
                }
            }
        } else if after_canvas {
            start = Some(x);
        }
        after_canvas = is_canvas;
    }
    out
}

// ---- the fixture project --------------------------------------------------

/// A card: id, title, body (its first paragraph is the card's excerpt).
struct Card(&'static str, &'static str, &'static str);

const BACKLOG: [Card; 10] = [
    Card("theme-docs", "Write the theme docs for the desktop palette", "Document the light and dark palettes, token roles, and contrast requirements for the desktop UI."),
    Card("board-widths", "Sync the board’s column widths with the TUI", "Keep columns readable as the window narrows. Match the TUI’s minimum widths and horizontal scrolling."),
    Card("agent-usage", "Show agent usage and cost by workspace", "Track tokens and estimated cost for each agent run, with a per-workspace breakdown in the activity view."),
    Card("terminal-search", "Search terminal scrollback", "Find previous commands and agent output with incremental search, match counts, and next/previous navigation."),
    Card("notifications", "Add notifications for completed work", "Notify when work is ready for review or an agent needs input. Respect the system notification settings."),
    Card("window-state", "Restore window layout on launch", "Remember the window size, sidebar width, and open panels between launches. Keep restored windows on-screen."),
    Card("ssh-errors", "Make SSH connection errors actionable", "Say which host failed and why, and offer the command that fixes it."),
    Card("session-export", "Export a session transcript", "Save a workspace’s terminal output as text or HTML for sharing."),
    Card("machine-setup", "Set up a remote machine from the app", "Run the machine setup flow from the Machines view."),
    Card("task-templates", "Start issues from templates", "Offer the project’s issue templates when creating a new issue."),
];

const TODO: [Card; 6] = [
    Card("desktop-palette", "Restyle the command palette", "Group commands by context, show keyboard shortcuts, and make the selected result easier to scan."),
    Card("sidebar-keyboard", "Improve keyboard navigation in the sidebar", "Move between sections with the arrow keys and restore focus to the last selected workspace."),
    Card("terminal-scroll", "Preserve terminal scroll position", "Keep the viewport anchored while new output arrives. Follow new output only when scrolled to the bottom."),
    Card("close-workspace", "Confirm before closing a busy workspace", "Warn when an agent is still running and offer to leave it in the background instead of stopping the session."),
    Card("retry-agent", "Retry failed agent starts", "Keep the startup error visible and let users retry with the same agent, model, and working directory."),
    Card("saved-filters", "Remember issue filters per project", "Save board filters per project and restore them when returning, including the selected workflow and workspace."),
];

const IN_PROGRESS: [Card; 2] = [
    Card("session-reconnect", "Handle reconnects after a daemon restart", "Reconnect to running sessions after the hub restarts, preserving terminal output and the active workspace."),
    Card("project-switcher", "Add project switching to the desktop app", "Switch between recent projects without restarting the app. Restore each project’s last open workspace."),
];

/// The umbrella's body, as its detail shows it. (The mockups' card excerpt
/// for it, "Build a native desktop client with an issue board, live agent
/// terminals, and a queue for reviewing completed work.", differs from the
/// detail's; the fixture follows the detail.)
const DESKTOP_APP_BODY: &str = "# Desktop application\n\nBuild a native desktop client for Shelbi using the shared application model. \
The desktop should expose the same orchestrator, workspaces, issues, review, activity, and machines as the TUI.\n\n\
## Acceptance criteria\n\n\
- [x] Native sidebar and project navigation\n\
- [x] Terminal rendering, selection, search, and keyboard input\n\
- [x] Issues board and task details\n\
- [x] Workspace and review views with live sessions\n\
- [ ] Final visual review and packaging\n\n\
## Implementation notes\n\n\
The desktop is its own Rust workspace. Shared state and orchestration stay in the existing crates; the GPUI \
client owns presentation and native interaction.\n";

fn day(d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, d, 12, 0, 0).single().expect("a fixture date")
}

/// The in-progress tasks' titles as the Machines frame draws them.
const MACHINES_TITLES: [(&str, &str); 2] = [
    ("project-switcher", "Add project switching"),
    ("session-reconnect", "Session reconnect"),
];

/// Give fixture task `id` a new title; the old one comes back.
fn retitle(id: &str, title: &str) -> String {
    let mut file = shelbi_state::load_task(PROJECT, id).expect("a fixture task");
    let old = std::mem::replace(&mut file.task.title, title.into());
    shelbi_state::save_task(PROJECT, &file.task, &file.body).expect("retitle a fixture task");
    old
}

fn save_task(id: &str, title: &str, body: &str, column: Column, priority: i64, assigned: Option<&str>) {
    let task = Issue {
        id: id.into(),
        title: title.into(),
        column,
        priority: priority as _,
        assigned_to: assigned.map(str::to_string),
        workflow: Some("task".into()),
        branch: Some(format!("jlong/{id}")),
        depends_on: vec![],
        prefers_machine: None,
        zen: None,
        launch: None,
        created_at: day(12),
        updated_at: day(16),
        params: Default::default(),
    };
    shelbi_state::save_task(PROJECT, &task, body).expect("save a fixture task");
}

fn write_fixtures(home: &Path, now: DateTime<Utc>) {
    let projects = home.join("projects");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: Shelbi
repo: ''
default_branch: main
orchestrator:
  runner: codex
agent_runners:
  codex:
    command: codex
    flags: []
  claude:
    command: claude
    flags: []
agents:
  developer:
    runner: codex
  review:
    runner: claude
machines:
  - name: hub
    kind: local
    work_dir: {work}
  - name: gpu
    kind: ssh
    host: gpu.lan
    work_dir: /home/me
    tags: [gpu, linux]
workspaces:
  - name: alpha
    machine: hub
  - name: bravo
    machine: hub
  - name: charlie
    machine: hub
  - name: review
    machine: hub
    tags: [review]
    slot: 1
  - name: gpu-1
    machine: gpu
",
            work = home.join("work").display()
        ),
    )
    .unwrap();

    for (column, cards) in [
        (Column::backlog(), &BACKLOG[..]),
        (Column::todo(), &TODO[..]),
        (Column::in_progress(), &IN_PROGRESS[..]),
    ] {
        for (i, Card(id, title, body)) in cards.iter().enumerate() {
            let assigned = match *id {
                "session-reconnect" => Some("bravo"),
                "project-switcher" => Some("alpha"),
                _ => None,
            };
            save_task(id, title, body, column.clone(), i as i64 + 1, assigned);
        }
    }
    save_task("desktop-app", "Desktop application", DESKTOP_APP_BODY, Column::review(), 1, Some("review"));
    save_task(
        "theme-tokens",
        "Share theme tokens across clients",
        "Move the color tokens into the shared application model so both clients paint from one palette.",
        Column::done(),
        1,
        Some("alpha"),
    );

    let status = |workspace: &str, task: Option<&str>, state| {
        shelbi_state::save_workspace_status(&shelbi_state::WorkspaceStatus {
            workspace: workspace.into(),
            current_task: task.map(str::to_string),
            state,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();
    };
    status("alpha", Some("project-switcher"), shelbi_state::WorkspaceState::Working);
    status("bravo", Some("session-reconnect"), shelbi_state::WorkspaceState::Working);
    status("review", Some("desktop-app"), shelbi_state::WorkspaceState::Serving);
    // The review slot has confirmed its server (Machines: `review`, not
    // `starting`).
    let review_marker = home.join("work/.shelbi/wt/review/.shelbi");
    std::fs::create_dir_all(&review_marker).unwrap();
    std::fs::write(review_marker.join("shelbi-review-loaded"), "desktop-app\n").unwrap();

    shelbi_state::machine_state::save_machine_record(
        "gpu",
        Some(shelbi_state::machine_state::MachineRecord {
            path: "/usr/local/bin/shelbi".into(),
            version: "0.10.0".into(),
            compatible: true,
            source: shelbi_state::machine_state::SOURCE_PATH.into(),
            checked_at: now,
        }),
    )
    .unwrap();

    // The Activity mockup's eight events, newest first there. charlie's
    // "available" is its worktree released after a handoff, and the
    // daemon's "started" its own line (the desktop labels both).
    let at = |mins: i64| (now - chrono::Duration::minutes(mins)).to_rfc3339();
    let p = format!("project={PROJECT}");
    let events = [
        format!("{} daemon started", at(24 * 60 + 50)),
        format!("{} {p} task=theme-tokens workflow=task in-progress -> review reason=workspace:ready-marker", at(24 * 60 + 20)),
        format!("{} worktree-detach task=sidebar-collapse workspace=charlie detached-from=jlong/fix-sidebar-collapse status=ok", at(40)),
        format!("{} {p} task=project-switcher workflow=task todo -> in-progress reason=orchestrator:auto-dispatch", at(25)),
        format!("{} {p} task=session-reconnect workflow=task todo -> in-progress reason=orchestrator:auto-dispatch", at(18)),
        format!("{} {p} task=desktop-app workflow=task in-progress -> review reason=workspace:ready-marker", at(12)),
        format!("{} {p} task=desktop-palette workflow=task backlog -> todo reason=user:board", at(6)),
        format!("{} {p} task=theme-tokens workflow=task review -> done reason=orchestrator:zen-merge_pr=594", at(2)),
    ];
    std::fs::write(home.join("events.log"), events.join("\n") + "\n").unwrap();

    // Settings: the frame's Shelbi CLI path. Its font and size predate the
    // Source Code Pro default, so those keep theirs.
    std::fs::write(home.join("config.yaml"), "desktop:\n  cli_path: /opt/homebrew/bin/shelbi\n").unwrap();
}
