//! The desktop smoke test: every registry command through its real UI paths.
//!
//! A headless window (gpui's test context; [`ShelbiWindow::build`], so no
//! feed, no bootstrap, no daemon) over a fixture sidebar that offers every
//! kind of command. Each command is run twice: through the palette (the
//! palette chord, its title typed, the arrows onto it, Enter) and through its
//! menu item (the [`MenuInvoke`](crate::menus::MenuInvoke) the native menu bar
//! dispatches). Both must reach the executor with the calls
//! [`commands::dispatch_command`] makes for the command, which the window's
//! test tap records in place of performing them. The behavior behind each
//! call is the shared scenario tests' (`crates/shelbi-app/tests`) and each
//! view's own tests.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
use shelbi_app::command::{Command, EditItem, ProjectItem};
use shelbi_app::theme::ThemePreference;
use shelbi_app::view::{NavItem, ReviewRow, ReviewState, WorkspaceBadge, WorkspaceRow};

use super::*;
use crate::menus::tree::{MenuAction, WindowFacts};

pub(super) fn fixture_model() -> SidebarModel {
    let mut m = SidebarModel::skeleton("alpha");
    m.nav = vec![
        NavItem {
            label: "Orchestrator".into(),
            view: View::Session("orch".into()),
        },
        NavItem {
            label: "Issues".into(),
            view: View::Issues,
        },
        NavItem {
            label: "Changelog".into(),
            view: View::Changelog,
        },
    ];
    m.workspaces = vec![WorkspaceRow {
        name: "alpha-1".into(),
        machine: "hub".into(),
        is_remote: false,
        current_task: Some("T-1".into()),
        agent: Some("developer".into()),
        badge: WorkspaceBadge::Working,
    }];
    m.reviews = vec![ReviewRow {
        task_id: "T-9".into(),
        title: "Warm the cache".into(),
        branch: "jlong/T-9".into(),
        location: None,
        workspace: None,
        state: ReviewState::Pending,
        number: None,
    }];
    m.board_loading = false;
    m
}

fn fixture_inputs() -> PaletteInputs {
    let edit = |target: EditTarget, title: &str| EditItem {
        target,
        title: title.into(),
    };
    PaletteInputs {
        other_projects: vec![ProjectItem {
            slug: "beta".into(),
            label: "Beta".into(),
        }],
        edit_targets: vec![
            edit(EditTarget::Project, "Edit Project Settings"),
            edit(EditTarget::Agent("developer".into()), "Edit developer Settings"),
            edit(EditTarget::ZenMode, "Edit Zen Mode"),
            edit(EditTarget::Workflows, "Edit Workflows"),
            edit(EditTarget::ChangelogTemplate, "Edit Changelog Template"),
        ],
        projects: Vec::new(),
        others_attached: true,
    }
}

pub(super) struct Harness {
    pub(super) window: Entity<ShelbiWindow>,
    pub(super) cx: VisualTestContext,
    pub(super) tap: Rc<RefCell<Vec<HostCall>>>,
}

impl Harness {
    pub(super) fn open(cx: &mut TestAppContext) -> Harness {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let tap = Rc::new(RefCell::new(Vec::new()));
        let window_tap = tap.clone();
        let handle = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.new(|cx| {
                    let mut w = ShelbiWindow::build("alpha", WindowState::default(), window, cx);
                    w.boot = Boot::Ready(Box::new(inert_ready()));
                    w.model = fixture_model();
                    w.palette_inputs = fixture_inputs();
                    w.tap = Some(window_tap);
                    // A native view with no entity yet: nothing attaches.
                    w.client.set_view(View::Machines);
                    w
                })
            })
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(handle.into(), cx);
        let window = handle.root(&mut vcx).unwrap();
        let mut h = Harness {
            window,
            cx: vcx,
            tap,
        };
        h.draw();
        h
    }

    pub(super) fn draw(&mut self) {
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
    }

    /// Put a line far wider than `input`'s box in it and check it
    /// soft-wraps there: its last char drawn on a row below its first and
    /// inside the box, and no newline added to the value.
    pub(super) fn assert_area_wraps(&mut self, input: &gpui::Entity<gpui_kit::component::input::TextareaState>, what: &str) {
        let long = "Wrap me at the edge of the box, please. ".repeat(10).trim_end().to_string();
        let value = long.clone();
        self.cx.update(|window, cx| input.update(cx, |s, cx| s.set_value(value, window, cx)));
        // The wrap follows the box's laid-out width.
        self.draw();
        self.draw();
        let (first, last, bounds, text) = self.cx.read(|cx| {
            let s = input.read(cx);
            (s.range_to_bounds(&(0..1)), s.range_to_bounds(&(long.len() - 1..long.len())), s.input_bounds(), s.value().to_string())
        });
        let (first, last) = (first.unwrap_or_else(|| panic!("{what} is laid out")), last.unwrap());
        assert!(last.top() > first.top(), "{what}: the long line wraps onto more rows ({first:?}, {last:?})");
        assert!(
            last.right() <= bounds.right() + gpui::px(1.0),
            "{what}: the wrapped text stays in the box ({last:?} in {bounds:?})"
        );
        assert_eq!(text, long, "{what}: wrapping adds no newline");
    }

    pub(super) fn key(&mut self, keystroke: &str) {
        self.cx.simulate_keystrokes(keystroke);
        self.cx.run_until_parked();
    }

    /// Focus the window's own handle (where its key handlers live).
    pub(super) fn focus_window(&mut self) {
        self.cx.update(|window, cx| {
            let handle = self.window.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        self.draw();
    }

    /// Click the middle of the element drawn with `selector`.
    pub(super) fn click_selector(&mut self, selector: &'static str) {
        self.draw();
        let at = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .center();
        self.cx.simulate_click(at, gpui::Modifiers::none());
        self.cx.run_until_parked();
    }

    pub(super) fn take_calls(&self) -> Vec<HostCall> {
        std::mem::take(&mut *self.tap.borrow_mut())
    }

    /// Carry out `calls` as the window's executor would have (the tap only
    /// records them), for a test that needs what a command opens.
    pub(super) fn perform(&mut self, calls: Vec<HostCall>) {
        let window = self.window.clone();
        self.cx.update(|w, cx| {
            window.update(cx, |this, cx| {
                for call in calls {
                    this.perform(call, w, cx);
                }
            })
        });
        self.draw();
    }

    fn commands(&self) -> Vec<Command> {
        let model = self.window.read_with(&self.cx, |w, _| w.command_model());
        CommandRegistry::new().commands(&model)
    }

    /// The palette's name for the command `id` (the desktop's copy, which
    /// renames some registry titles).
    fn palette_label(&self, id: &str) -> String {
        self.window.read_with(&self.cx, |w, _| {
            w.palette_entries()
                .into_iter()
                .find(|e| e.id == id)
                .unwrap_or_else(|| panic!("the palette offers {id}"))
                .label
        })
    }

    /// The open palette's selected result id and the position of `id` in
    /// its results.
    fn palette_position(&self, id: &str) -> (usize, usize) {
        self.window.read_with(&self.cx, |w, _| {
            let Some(ActiveOverlay::Palette { palette, .. }) = &w.overlay else {
                panic!("the palette is open");
            };
            // The selectable rows (a search groups them under headings).
            let items: Vec<Option<String>> = palette
                .rows()
                .iter()
                .filter_map(|r| r.item())
                .map(|i| i.entry().map(|e| e.id))
                .collect();
            let at = items
                .iter()
                .position(|e| e.as_deref() == Some(id))
                .unwrap_or_else(|| panic!("the palette lists {id}"));
            (palette.selected_in(items.len()), at)
        })
    }

    fn overlay_open(&self) -> bool {
        self.window.read_with(&self.cx, |w, _| w.overlay.is_some())
    }
}

/// The calls dispatch makes for `kind`: what the executor should receive.
fn expected_calls(kind: &CommandKind) -> Vec<HostCall> {
    let mut calls = Vec::new();
    commands::dispatch_command(kind, &mut calls);
    calls
}

/// One gpui keystroke per character of `text`.
fn keystrokes_for(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ' ' => "space".to_string(),
            c if c.is_uppercase() => format!("shift-{}", c.to_lowercase()),
            c => c.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[gpui::test]
fn every_registry_command_runs_from_the_palette(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    let commands = h.commands();
    assert_eq!(commands.len(), 30, "the fixture offers every kind of command");
    for command in &commands {
        h.focus_window();
        h.key("ctrl-space");
        assert!(h.overlay_open(), "Ctrl+Space opens the palette for {}", command.id());
        h.key(&keystrokes_for(&h.palette_label(command.id())));
        // Walk the selection onto the entry, as a user would when the query
        // ranks a sibling first.
        for _ in 0..commands.len() {
            let (selected, at) = h.palette_position(command.id());
            if selected == at {
                break;
            }
            assert!(selected < at, "{} is below the selection", command.id());
            h.key("down");
        }
        h.key("enter");
        assert!(!h.overlay_open(), "Enter closes the palette for {}", command.id());
        assert_eq!(
            h.take_calls(),
            expected_calls(command.kind()),
            "{} reached the executor from the palette",
            command.id()
        );
    }
}

#[gpui::test]
fn every_registry_command_runs_from_its_menu_item(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    let commands = h.commands();
    let tree = h.window.read_with(&h.cx, |w, _| {
        menus::tree::build(&w.command_model(), &w.keymaps, WindowFacts::default())
    });
    for command in &commands {
        let item = tree
            .items()
            .into_iter()
            .find(|i| i.enabled && i.action == MenuAction::Command(command.id().to_string()))
            .unwrap_or_else(|| panic!("{} has an enabled menu item", command.id()));
        h.focus_window();
        h.cx.dispatch_action(menus::MenuInvoke {
            target: item.action.clone(),
            generation: 0,
        });
        h.cx.run_until_parked();
        assert_eq!(
            h.take_calls(),
            expected_calls(command.kind()),
            "{} ({}) reached the executor from its menu item",
            command.id(),
            item.title
        );
    }
}

#[gpui::test]
fn a_menu_pick_with_the_palette_up_closes_it_first(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.focus_window();
    h.key("ctrl-space");
    assert!(h.overlay_open());
    h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::Command("view:tasks".into()),
        generation: 0,
    });
    h.cx.run_until_parked();
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), [HostCall::ShowView(View::Issues)]);
}

#[gpui::test]
fn the_font_zoom_chords_and_menu_items_step_the_terminal_font(cx: &mut TestAppContext) {
    use std::sync::{Arc, Mutex};

    use crate::elements::terminal::settings::SizeWriter;
    use crate::menus::tree::AppCommand;

    let mut h = Harness::open(cx);
    let writes = Arc::new(Mutex::new(Vec::new()));
    let log = writes.clone();
    h.cx.update(|_, cx| {
        cx.set_global(TerminalSettings::default());
        SizeWriter::install(
            move |size| {
                log.lock().unwrap().push(size);
                Ok(())
            },
            cx,
        );
    });
    let size = |h: &mut Harness| {
        h.cx.update(|_, cx| {
            let s = TerminalSettings::current(cx);
            (s.font_size, s.font_size_setting)
        })
    };
    h.focus_window();

    // ⌘= and ⌘+ (as a US layout reports it) grow, ⌘− shrinks, ⌘0 resets.
    h.key("cmd-=");
    assert_eq!(size(&mut h), (15.0, Some(15)));
    h.key("cmd-+");
    assert_eq!(size(&mut h), (16.0, Some(16)));
    h.key("cmd--");
    assert_eq!(size(&mut h), (15.0, Some(15)));
    h.key("cmd-0");
    assert_eq!(size(&mut h), (14.0, None));
    assert_eq!(writes.lock().unwrap().last(), Some(&None));

    // An overlay doesn't take the chords.
    h.key("ctrl-space");
    assert!(h.overlay_open());
    h.key("cmd-=");
    assert_eq!(size(&mut h), (15.0, Some(15)));
    assert!(h.overlay_open());
    h.key("escape");

    // The View menu items do the same.
    for (command, want) in [
        (AppCommand::FontBigger, (16.0, Some(16))),
        (AppCommand::FontSmaller, (15.0, Some(15))),
        (AppCommand::FontActualSize, (14.0, None)),
    ] {
        h.cx.dispatch_action(menus::MenuInvoke {
            target: MenuAction::App(command),
            generation: 0,
        });
        h.cx.run_until_parked();
        assert_eq!(size(&mut h), want, "{command:?}");
    }
    assert_eq!(writes.lock().unwrap().last(), Some(&None));
    // No registry command ran.
    assert_eq!(h.take_calls(), []);
}

/// Issues and Changelog entries matching `login`, as the window's board and
/// Changelog page would hold them.
pub(super) fn search_corpus() -> SearchCorpus {
    use shelbi_core::StatusCategory;
    let issues = vec![
        IssueDoc::new("1931", "Fix login redirect loop", "", "In Progress", StatusCategory::Active),
        IssueDoc::new("1944", "Login form validation", "", "Backlog", StatusCategory::Backlog),
        IssueDoc::new("1950", "Faster board", "", "Done", StatusCategory::Done),
    ];
    let entry = ChangelogEntryView::new(
        shelbi_state::changelog::ChangelogEntry {
            id: "2026-10-28-login".into(),
            title: "Login form shipped".into(),
            date: chrono::DateTime::parse_from_rfc3339("2026-10-28T14:30:00-07:00").unwrap(),
            author: "orchestrator".into(),
            issues: vec![],
            prs: vec![shelbi_state::changelog::PrRef::Number(1902)],
            urls: vec![],
            body: "The login form, rebuilt.".into(),
            path: Default::default(),
        },
        Some("jlong/shelbi"),
    );
    SearchCorpus::new(issues, vec![entry])
}

impl Harness {
    /// Open the palette (the window's chord) over [`search_corpus`].
    pub(super) fn open_search_palette(&mut self) {
        self.focus_window();
        self.key("ctrl-space");
        self.window.update(&mut self.cx, |w, _| {
            if let Some(ActiveOverlay::Palette { palette, .. }) = &mut w.overlay {
                palette.set_corpus(search_corpus());
            }
        });
        self.draw();
    }

    fn palette<T>(&self, f: impl FnOnce(&shelbi_app::overlay::palette::Palette) -> T) -> T {
        self.window.read_with(&self.cx, |w, _| {
            let Some(ActiveOverlay::Palette { palette, .. }) = &w.overlay else {
                panic!("the palette is open");
            };
            f(palette)
        })
    }

    fn top_of(&mut self, selector: &'static str) -> f32 {
        self.cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
            .origin
            .y
            .as_f32()
    }
}

#[gpui::test]
fn typing_in_the_palette_searches_issues_and_changelog_entries(cx: &mut TestAppContext) {
    use shelbi_app::overlay::palette_search::SearchFilter;
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, _| {
        w.palette_inputs.projects = vec![PaletteProject {
            slug: "alpha".into(),
            label: "Alpha".into(),
            indicator: shelbi_app::overlay::palette::ProjectIndicator::Active,
        }];
    });
    h.open_search_palette();
    assert!(h.cx.debug_bounds("palette-filters").is_none(), "no filter row before typing");
    assert!(h.cx.debug_bounds("palette-projects").is_some(), "the Projects column, as ever");

    h.key("l o g i n");
    // The filter row, each chip with its count.
    assert!(h.cx.debug_bounds("palette-filters").is_some());
    let counts: Vec<(&str, usize)> = h.palette(|p| p.chips().iter().map(|c| (c.label, c.count)).collect());
    assert_eq!(counts, [("All", 3), ("Issues", 2), ("Changelog Entries", 1), ("Commands", 0)]);
    assert!(h.cx.debug_bounds("palette-projects").is_none(), "the Projects column steps aside");
    // Grouped under All, a heading over each kind, best match first.
    let issues = h.top_of("palette-heading-Issues");
    let entries = h.top_of("palette-heading-Changelog Entries");
    assert!(issues < h.top_of("palette-issue-1931"));
    assert!(h.top_of("palette-issue-1944") < entries);
    assert!(entries < h.top_of("palette-entry-2026-10-28-login"));

    // Tab picks the next filter; a click on a chip picks that one.
    h.key("tab");
    assert_eq!(h.palette(|p| p.filter()), SearchFilter::Issues);
    assert!(h.cx.debug_bounds("palette-entry-2026-10-28-login").is_none());
    h.key("shift-tab");
    assert_eq!(h.palette(|p| p.filter()), SearchFilter::All);
    h.click_selector("palette-filter-Changelog Entries");
    assert_eq!(h.palette(|p| p.filter()), SearchFilter::Changelog);
    // ↑ from the first result reaches the filter row, where ←/→ move.
    h.key("up");
    assert!(h.palette(|p| p.filter_focused()));
    h.key("left");
    assert_eq!(h.palette(|p| p.filter()), SearchFilter::Issues);
    h.key("down");

    // Enter on an issue opens its detail on the board.
    h.key("enter");
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), [HostCall::OpenIssue("1931".into())]);

    // A click on an entry opens the Changelog at it.
    h.open_search_palette();
    h.key("l o g i n");
    h.click_selector("palette-entry-2026-10-28-login");
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), [HostCall::OpenChangelogEntry("2026-10-28-login".into())]);
}

// ---------------------------------------------------------------------------
// The palette by mouse: hover, click-to-run, the wheel, outside clicks
// ---------------------------------------------------------------------------

impl Harness {
    fn bounds_of(&mut self, selector: &'static str) -> gpui::Bounds<gpui::Pixels> {
        self.draw();
        self.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    }

    fn mouse_to(&mut self, at: gpui::Point<gpui::Pixels>) {
        self.cx.simulate_mouse_move(at, None, gpui::Modifiers::none());
        self.cx.run_until_parked();
        self.draw();
    }

    fn click_at(&mut self, at: gpui::Point<gpui::Pixels>) {
        self.cx.simulate_click(at, gpui::Modifiers::none());
        self.cx.run_until_parked();
    }

    fn wheel(&mut self, at: gpui::Point<gpui::Pixels>, dy: f32) {
        self.cx.simulate_event(gpui::ScrollWheelEvent {
            position: at,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(dy))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.cx.run_until_parked();
        self.draw();
        // The hover a scroll moves under a still pointer settles a frame on.
        self.draw();
    }

    /// The id of the open palette's selected result.
    fn selected_id(&self) -> Option<String> {
        self.palette(|p| {
            let row = p.selected_row()?;
            p.rows()[row].item()?.entry().map(|e| e.id)
        })
    }

    fn open_palette(&mut self) {
        self.focus_window();
        self.key("ctrl-space");
        assert!(self.overlay_open(), "Ctrl+Space opens the palette");
        self.draw();
    }

    fn command_calls(&self, id: &str) -> Vec<HostCall> {
        let command = self.commands().into_iter().find(|c| c.id() == id).unwrap_or_else(|| panic!("{id}"));
        expected_calls(command.kind())
    }
}

#[gpui::test]
fn hovering_a_palette_result_selects_it_and_the_keyboard_continues(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    h.window.update(&mut h.cx, |w, _| {
        w.palette_inputs.projects = vec![PaletteProject {
            slug: "beta".into(),
            label: "Beta".into(),
            indicator: shelbi_app::overlay::palette::ProjectIndicator::LoadedIdle,
        }];
    });
    h.open_palette();
    assert_eq!(h.selected_id().as_deref(), Some("view:orch"));
    let activity = h.bounds_of("palette-row-view:activity");
    h.mouse_to(activity.center());
    assert_eq!(h.selected_id().as_deref(), Some("view:activity"), "hover selects the row");
    assert!(h.cx.debug_bounds("palette-selected").is_some_and(|b| activity.intersects(&b)), "drawn as the selection");
    // Its description end is the same row.
    h.mouse_to(gpui::point(activity.right() - gpui::px(8.0), activity.center().y));
    assert_eq!(h.selected_id().as_deref(), Some("view:activity"));
    // The keyboard continues from the hovered row.
    h.key("down");
    assert_eq!(h.selected_id().as_deref(), Some("view:machines"));
    // A Projects row hovers too, and Enter runs it.
    let beta = h.bounds_of("palette-project-Beta");
    h.mouse_to(beta.center());
    assert_eq!(h.palette(|p| p.project_selection()), Some(0), "hover focuses the project");
    // Back over a result, the results have the keys again.
    let at = h.bounds_of("palette-row-view:tasks").center();
    h.mouse_to(at);
    assert_eq!(h.selected_id().as_deref(), Some("view:tasks"));
    assert_eq!(h.palette(|p| p.project_selection()), None);
    h.key("enter");
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), h.command_calls("view:tasks"), "Enter runs the hovered row");
}

#[gpui::test]
fn clicking_a_palette_result_runs_it_and_outside_closes(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    // A click on the name runs the row, as Enter does.
    h.open_palette();
    let row = h.bounds_of("palette-row-view:changelog");
    h.click_at(row.center());
    assert!(!h.overlay_open(), "running a result closes the palette");
    assert_eq!(h.take_calls(), h.command_calls("view:changelog"));
    // So does one on its description, at the row's far end.
    h.open_palette();
    let row = h.bounds_of("palette-row-view:machines");
    h.click_at(gpui::point(row.right() - gpui::px(8.0), row.center().y));
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), h.command_calls("view:machines"));
    // A click on the panel off any row (the query line) does nothing.
    h.open_palette();
    let at = h.bounds_of("palette-query").center();
    h.click_at(at);
    assert!(h.overlay_open(), "the query line keeps it open");
    // One outside the panel closes it, running nothing.
    let panel = h.bounds_of("palette");
    h.click_at(gpui::point(panel.left() + gpui::px(20.0), panel.bottom() + gpui::px(20.0)));
    assert!(!h.overlay_open(), "an outside click closes the palette");
    assert!(h.take_calls().is_empty());
    // While searching, a group heading does nothing.
    h.open_search_palette();
    h.key("l o g i n");
    let at = h.bounds_of("palette-heading-Issues").center();
    h.click_at(at);
    assert!(h.overlay_open(), "a heading isn't a result");
    assert!(h.take_calls().is_empty());
    let at = h.bounds_of("palette-issue-1944").center();
    h.click_at(at);
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), [HostCall::OpenIssue("1944".into())]);
}

#[gpui::test]
fn the_wheel_scrolls_the_palette_and_hover_and_clicks_follow(cx: &mut TestAppContext) {
    let mut h = Harness::open(cx);
    // Short enough that the ten commands scroll.
    h.cx.simulate_resize(gpui::size(gpui::px(900.0), gpui::px(380.0)));
    h.open_palette();
    let results = h.bounds_of("palette-results");
    let last = h.bounds_of("palette-row-action:quit-shelbi");
    assert!(last.top() >= results.bottom(), "the last row starts below the fold");
    let first = h.bounds_of("palette-row-view:orch");
    let at = first.center();
    h.mouse_to(at);
    assert_eq!(h.selected_id().as_deref(), Some("view:orch"));
    // Three rows down.
    h.wheel(at, -3.0 * crate::views::palette::ROW_H);
    let first_after = h.bounds_of("palette-row-view:orch");
    assert!(first_after.top() < first.top(), "the wheel scrolled the results");
    // The row now under the still pointer is the selection.
    let under = [
        ("palette-row-view:tasks", "view:tasks"),
        ("palette-row-view:changelog", "view:changelog"),
        ("palette-row-view:activity", "view:activity"),
        ("palette-row-view:machines", "view:machines"),
        ("palette-row-action:toggle-zen", "action:toggle-zen"),
    ]
    .into_iter()
    .find(|(sel, _)| h.cx.debug_bounds(sel).is_some_and(|b| b.contains(&at)))
    .map(|(_, id)| id)
    .expect("a row sits under the pointer");
    assert_eq!(h.selected_id().as_deref(), Some(under), "hover follows the scroll");
    // Hover and clicks land on the rows where they're drawn now.
    let log = h.bounds_of("palette-row-action:error-log");
    assert!(log.bottom() <= results.bottom() + gpui::px(1.0), "scrolled into view");
    h.mouse_to(log.center());
    assert_eq!(h.selected_id().as_deref(), Some("action:error-log"));
    h.click_at(log.center());
    assert!(!h.overlay_open());
    assert_eq!(h.take_calls(), h.command_calls("action:error-log"));
}
