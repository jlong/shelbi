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
//! machines, settings and command-palette, plus the palette states the
//! frame doesn't draw ([`PALETTE_STATES`], no reference: checked by eye),
//! the sidebar states no frame draws (`state-sidebar-*`: sections
//! collapsed, Zen off, the sidebar hidden), the issue detail's
//! ([`DETAIL_ACTIONS`] hovered, a long body scrolled) and the task panels'
//! (`state-review-*`, `state-workspace-*`: More opening the task detail
//! over the screen, each view's ⌘N, Approve
//! and Reject hovered and pressed), and Machines' (`machines-unreachable`:
//! the remote down, its row selected; `machines-idle-selected`,
//! `machines-review-selected`), Settings' (`state-settings-cli-focused`:
//! a focused textbox), and the form components' sheets
//! (`components/<name>-<theme>.png`, against the components page's PNGs).
//! `SHELBI_VISUAL_ONLY=panels` renders just the Review and Workspace
//! screens and those states; `=settings` just Settings, its state and the
//! components.
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
/// screens and their panels' states; `=settings` only Settings.
const ONLY_VAR: &str = "SHELBI_VISUAL_ONLY";
/// The reference PNGs' size, in points.
const WIDTH: u32 = 1440;
const HEIGHT: u32 = 1024;

/// Every screen, named as its reference PNG.
pub const SCREENS: [&str; 10] = [
    "orchestrator",
    "issues",
    "issue-detail",
    "activity",
    "review",
    "workspace",
    "workspace-idle",
    "machines",
    "settings",
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

/// The components page's sheets, named as its PNGs, with their sizes: every
/// state of the Textbox and the Checkbox, a Segmented Control, a Labeled
/// Control and a Picker; plus a Checkbox List (no PNG: checked by eye).
const COMPONENT_SHEETS: [(&str, f32, f32); 6] = [
    ("textbox", 234.0, 210.0),
    ("checkbox", 126.0, 154.0),
    ("segmented-control", 167.0, 32.0),
    ("labeled-control", 400.0, 56.0),
    ("picker", 400.0, 32.0),
    ("checkbox-list", 200.0, 74.0),
];

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
                    .open_offscreen_window(size(px(WIDTH as f32), px(HEIGHT as f32)), root)
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
                    size: size(px(WIDTH as f32), px(HEIGHT as f32)),
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

    /// Save the window as `<screen>-<theme>.png`, keeping it open.
    fn capture(&mut self, handle: WindowHandle<ShelbiWindow>, screen: &str) {
        self.settle(handle);
        let window: AnyWindowHandle = handle.into();
        let image = self.cx.capture_screenshot(window).expect("render to image");
        let image = if self.native || image.width() == WIDTH {
            image
        } else {
            image::imageops::resize(&image, WIDTH, HEIGHT, image::imageops::FilterType::Triangle)
        };
        let path = self.out.join(format!("{screen}-{}.png", self.theme));
        image.save(&path).expect("save the frame");
        println!("saved {} ({}x{})", path.display(), image.width(), image.height());
    }

    fn settings(&mut self) {
        let handle = self.open();
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_settings(window, cx))
            .unwrap();
        self.save(handle, "settings");

        // The Shelbi CLI field focused: the textbox's ring and glow, the
        // caret in the ring's color.
        let handle = self.open();
        handle
            .update(&mut self.cx, |w, window, cx| {
                w.offscreen_open_settings(window, cx);
                if let Some(settings) = w.offscreen_settings() {
                    settings.update(cx, |v, cx| v.offscreen_focus_cli(window, cx));
                }
            })
            .unwrap();
        self.save(handle, "state-settings-cli-focused");
        self.components();
    }

    /// The form components as the Figma components page draws them, each
    /// at its variant sheet's size and positions:
    /// `components/<name>-<theme>.png`, beside the page's
    /// `components/<name>.png` (Light). Light sits on the page's `#f5f5f5`,
    /// Dark on the canvas.
    fn components(&mut self) {
        let dir = self.out.join("components");
        std::fs::create_dir_all(&dir).expect("create the components directory");
        for (name, w, h) in COMPONENT_SHEETS {
            crate::theme::motion::reset_pointers();
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

        // The orchestrator chat.
        let handle = self.open();
        self.click(handle, orch.clone());
        self.settle(handle);
        self.save(handle, "orchestrator");
        self.terminal_states(&orch);

        // The board, a card selected.
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
        if let (Some(board), _, _) = self.views(handle) {
            board.update(&mut self.cx, |v, cx| {
                v.state_mut().select(1, 0);
                cx.notify();
            });
        }
        self.save(handle, "issues");

        // A card's detail over the board.
        let handle = self.open();
        self.click(handle, View::Issues);
        self.wait_for_board(handle);
        handle
            .update(&mut self.cx, |w, window, cx| w.offscreen_open_task("desktop-app", window, cx))
            .unwrap();
        self.save(handle, "issue-detail");
        self.detail_states();

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

    /// The issue detail over the board, as the frame shows it.
    fn open_detail(&mut self) -> WindowHandle<ShelbiWindow> {
        let handle = self.open();
        self.click(handle, View::Issues);
        self.wait_for_board(handle);
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
