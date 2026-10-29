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
    assert_eq!(commands.len(), 28, "the fixture offers every kind of command");
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
