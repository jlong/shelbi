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
            label: "Activity".into(),
            view: View::Activity,
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
                    w.sync_selection();
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

    fn commands(&self) -> Vec<Command> {
        let model = self.window.read_with(&self.cx, |w, _| w.command_model());
        CommandRegistry::new().commands(&model)
    }

    /// The open palette's selected result id and the position of `id` in
    /// its results.
    fn palette_position(&self, id: &str) -> (usize, usize) {
        self.window.read_with(&self.cx, |w, _| {
            let Some(ActiveOverlay::Palette { palette, .. }) = &w.overlay else {
                panic!("the palette is open");
            };
            let results = palette.results();
            let at = results
                .iter()
                .position(|(e, _)| e.id == id)
                .unwrap_or_else(|| panic!("the palette lists {id}"));
            (palette.selected_in(results.len()), at)
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
    assert_eq!(commands.len(), 22, "the fixture offers every kind of command");
    for command in &commands {
        h.focus_window();
        h.key("ctrl-space");
        assert!(h.overlay_open(), "Ctrl+Space opens the palette for {}", command.id());
        h.key(&keystrokes_for(command.title()));
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
