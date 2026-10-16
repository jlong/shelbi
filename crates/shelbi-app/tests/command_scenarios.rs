//! Headless scenario tests: every registry command, once, for both clients.
//!
//! Each test builds a [`CommandModel`] fixture, finds the command the way a
//! palette or menu does (by its registry id, through
//! [`CommandRegistry::resolve`]), invokes it, and asserts the [`Effect`] it
//! produces and the change it makes to the model. The TUI and the desktop app
//! both consume these effects unchanged, so the behavior asserted here is
//! asserted once for both; each client's smoke test then proves its own UI
//! path (palette Enter, the desktop's menus) reaches the same effects.
//!
//! The [`Scenario`] executor applies an effect's client-local half to a
//! [`ClientState`] the way both clients do (a view change focuses the main
//! area; the error log, add-project and reject-reason flows open their
//! overlay; a switch moves the client to the other project) and records the
//! rest: the lifecycle effects a host performs and the daemon mutations. The
//! flows that collect input before reaching the daemon (load a queued review,
//! reject with a reason, add a project) are driven through the same shared
//! state machines the clients drive.
//!
//! [`every_command_kind_has_a_scenario`] keeps this file complete: it matches
//! [`CommandKind`] with no wildcard arm, so a new command fails to compile
//! until it has a scenario here.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use shelbi_app::command::{
    CommandModel, EditItem, ProjectItem, ReviewItem, ViewItem, WorkspaceItem,
};
use shelbi_app::command_model::edit_target_path;
use shelbi_app::overlay::{add_project, review_reject};
use shelbi_app::review::{LoadStep, ReviewInfo, ReviewLoader, ReviewTarget};
use shelbi_app::{
    ClientState, CommandKind, CommandRegistry, EditTarget, Effect, ExecOutcome, Executor, Focus,
    Mutation, Overlay, View,
};
use shelbi_core::ConfigMode;
use shelbi_state::keymap::{Key, KeyChord, Mods};

const PROJECT: &str = "alpha";

/// A project with every kind of row the registry enumerates: the three nav
/// views, a workspace, a review, another project, every edit target, and a
/// second attached client.
fn fixture() -> CommandModel {
    let view = |id: &str, title: &str, view: View| ViewItem {
        id: id.into(),
        title: title.into(),
        view,
        decoration: None,
    };
    CommandModel {
        project: Some(PROJECT.into()),
        zen_on: false,
        zen_shortcut: None,
        views: vec![
            view("view:orch", "Orchestrator", View::Session("orch".into())),
            view("view:tasks", "Issues", View::Issues),
            view("view:activity", "Activity", View::Activity),
            view("view:machines", "Machines", View::Machines),
        ],
        workspaces: vec![WorkspaceItem {
            name: "alpha-1".into(),
            subtitle: Some("T-1".into()),
            decoration: None,
        }],
        reviews: vec![ReviewItem {
            task_id: "T-9".into(),
            title: "Warm the cache".into(),
            subtitle: None,
            decoration: None,
        }],
        other_projects: vec![ProjectItem {
            slug: "beta".into(),
            label: "Beta".into(),
        }],
        edit_targets: vec![
            EditItem {
                target: EditTarget::Project,
                title: "Edit Project Settings".into(),
            },
            EditItem {
                target: EditTarget::Agent("developer".into()),
                title: "Edit developer Settings".into(),
            },
            EditItem {
                target: EditTarget::ZenMode,
                title: "Edit Zen Mode".into(),
            },
            EditItem {
                target: EditTarget::Workflows,
                title: "Edit Workflows".into(),
            },
        ],
        other_clients_attached: true,
        // The desktop's model: it opens windows, so New Window is listed.
        multi_window: true,
    }
}

/// What a host did with an effect besides changing the [`ClientState`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Performed {
    /// A daemon mutation was sent.
    Mutation(Mutation),
    /// A lifecycle or host action ran (detach, quit, open an editor, load a
    /// review): the effect itself.
    Host(Effect),
    /// Only the client state changed.
    Nothing,
}

/// The reference executor: one client's state plus a log of what reached the
/// host and the daemon.
struct Scenario {
    model: CommandModel,
    client: ClientState,
    effects: Vec<Effect>,
    performed: Vec<Performed>,
}

impl Scenario {
    fn new() -> Self {
        let mut client = ClientState::new(PROJECT);
        // Both clients land on the orchestrator chat with the sidebar focused
        // once the user steps back to it; start from a known non-default spot.
        client.set_view(View::Session("orch".into()));
        client.focus_sidebar();
        Scenario {
            model: fixture(),
            client,
            effects: Vec::new(),
            performed: Vec::new(),
        }
    }

    /// Invoke the command with registry id `id` (the palette's Enter, a menu
    /// item) and return its effect.
    fn invoke(&mut self, id: &str) -> Effect {
        let command = CommandRegistry::new()
            .resolve(id, &self.model)
            .unwrap_or_else(|| panic!("the registry offers {id}"));
        // The id round-trips, so a client that only kept the id (a menu item)
        // recovers the same command.
        assert_eq!(CommandKind::from_id(id).as_ref(), Some(command.kind()), "{id}");
        let effect = command.effect(PROJECT);
        self.run(effect.clone()).expect("the scenario executor never fails");
        effect
    }

    fn last(&self) -> &Performed {
        self.performed.last().expect("something was performed")
    }
}

impl Executor for Scenario {
    fn run(&mut self, effect: Effect) -> ExecOutcome {
        self.effects.push(effect.clone());
        let performed = match &effect {
            Effect::ShowView(view) => {
                self.client.set_view(view.clone());
                self.client.focus_main();
                Performed::Nothing
            }
            Effect::FocusWorkspace { workspace, .. } => {
                self.client.set_view(View::Session(workspace.clone()));
                self.client.focus_main();
                Performed::Nothing
            }
            Effect::OpenErrorLog { .. } => {
                self.client.open_overlay(Overlay::ErrorLog);
                Performed::Nothing
            }
            Effect::AddProject => {
                self.client.open_overlay(Overlay::AddProject);
                Performed::Nothing
            }
            // A bare switch opens the switcher (the palette's Projects column
            // in the TUI, the sidebar's switcher in the desktop app).
            Effect::SwitchProject { project } if project.is_empty() => {
                self.client.open_overlay(Overlay::Palette);
                Performed::Nothing
            }
            Effect::SwitchProject { project } => {
                self.client.switch_project(project.clone());
                Performed::Host(effect.clone())
            }
            // The palette's reject has no reason yet: ask for one.
            Effect::Mutate(Mutation::RejectReview { id, reason, .. }) if reason.trim().is_empty() => {
                self.client.open_overlay(Overlay::RejectReason {
                    task_id: id.clone(),
                });
                Performed::Nothing
            }
            Effect::Mutate(m) => Performed::Mutation(m.clone()),
            Effect::LoadReview { .. }
            | Effect::OpenEditor { .. }
            | Effect::Detach
            | Effect::DetachOtherClients
            | Effect::QuitProject { .. }
            | Effect::QuitShelbi
            | Effect::NewWindow { .. } => Performed::Host(effect.clone()),
        };
        self.performed.push(performed);
        Ok(())
    }
}

fn chord(c: char) -> KeyChord {
    KeyChord::new(Key::Char(c), Mods::NONE)
}

fn type_into(text: &str, mut feed: impl FnMut(KeyChord)) {
    for c in text.chars() {
        feed(chord(c));
    }
}

/// Exhaustive over [`CommandKind`], no wildcard: naming the scenario that
/// covers each variant. A new command fails to compile here until it has one.
fn scenario_for(kind: &CommandKind) -> &'static str {
    match kind {
        CommandKind::ShowView(_) => "nav_views_show_and_focus_the_main_area",
        CommandKind::FocusWorkspace { .. } => "a_workspace_opens_its_session_view",
        CommandKind::LoadReview { .. } => "a_review_resolves_then_opens_or_asks_for_a_slot",
        CommandKind::ToggleZen => "the_zen_toggle_is_a_daemon_mutation_and_relabels",
        CommandKind::OpenErrorLog => "the_error_log_opens_its_overlay",
        CommandKind::SwitchProject { .. } => "switch_project_moves_the_client_or_opens_the_switcher",
        CommandKind::AddProject => "add_project_collects_the_form_then_mutates",
        CommandKind::NewWindow => "new_window_reaches_the_host_on_a_multi_window_client",
        CommandKind::Detach => "detach_and_quit_reach_the_host",
        CommandKind::DetachOtherClients => "detach_and_quit_reach_the_host",
        CommandKind::QuitProject => "detach_and_quit_reach_the_host",
        CommandKind::QuitShelbi => "detach_and_quit_reach_the_host",
        CommandKind::OpenEditor { .. } => "edit_targets_open_their_file_in_the_editor",
        CommandKind::ApproveReview { .. } => "approve_is_sent_at_once",
        CommandKind::RejectReview { .. } => "reject_asks_for_a_reason_then_sends_it",
        CommandKind::MoveIssue { .. }
        | CommandKind::StartIssue { .. }
        | CommandKind::AssignIssue { .. }
        | CommandKind::EditIssue { .. }
        | CommandKind::AddIssue { .. }
        | CommandKind::PrioritizeIssue { .. } => "board_mutations_carry_the_project",
    }
}

#[test]
fn every_command_kind_has_a_scenario() {
    // Every command the registry offers for the full fixture maps to a
    // scenario (the match above is the compile-time half of this guard).
    let commands = CommandRegistry::new().commands(&fixture());
    assert!(!commands.is_empty());
    for c in &commands {
        assert!(!scenario_for(c.kind()).is_empty(), "{}", c.id());
    }
    // And the fixture is full: every registry-listed kind appears in it.
    let ids: Vec<&str> = commands.iter().map(|c| c.id()).collect();
    assert_eq!(
        ids,
        [
            "view:orch",
            "view:tasks",
            "view:activity",
            "view:machines",
            "action:toggle-zen",
            "workspace:alpha-1",
            "review:T-9",
            "action:approve-review:T-9",
            "action:reject-review:T-9",
            "action:error-log",
            "action:switch-project",
            "action:switch-project:beta",
            "action:add-project",
            "action:new-window",
            "edit:project",
            "edit:agent:developer",
            "edit:zenmode",
            "edit:workflows",
            "action:detach",
            "action:detach-others",
            "action:quit-project",
            "action:quit-shelbi",
        ]
    );
}

#[test]
fn nav_views_show_and_focus_the_main_area() {
    for (id, view) in [
        ("view:tasks", View::Issues),
        ("view:activity", View::Activity),
        ("view:machines", View::Machines),
        ("view:orch", View::Session("orch".into())),
    ] {
        let mut s = Scenario::new();
        s.client.set_view(View::Issues);
        s.client.focus_sidebar();
        assert_eq!(s.invoke(id), Effect::ShowView(view.clone()), "{id}");
        assert_eq!(s.client.view(), &view, "{id}");
        assert_eq!(s.client.focus(), Focus::Main, "{id}");
        assert_eq!(s.client.remembered_view(PROJECT), Some(&view), "{id} is remembered");
        assert_eq!(s.last(), &Performed::Nothing, "{id} is client-local");
    }
}

#[test]
fn a_workspace_opens_its_session_view() {
    let mut s = Scenario::new();
    assert_eq!(
        s.invoke("workspace:alpha-1"),
        Effect::FocusWorkspace {
            project: PROJECT.into(),
            workspace: "alpha-1".into()
        }
    );
    assert_eq!(s.client.view(), &View::Session("alpha-1".into()));
    assert_eq!(s.client.focus(), Focus::Main);
}

fn review_info() -> ReviewInfo {
    ReviewInfo {
        task_id: "T-9".into(),
        slot: "review-1".into(),
        title: "Warm the cache".into(),
        body: String::new(),
        worktree: "/w/review-1".into(),
        editor_name: "Vim".into(),
        review_url: None,
        has_health_check: false,
        remote_machine: None,
    }
}

#[test]
fn a_review_resolves_then_opens_or_asks_for_a_slot() {
    let mut s = Scenario::new();
    let effect = s.invoke("review:T-9");
    assert_eq!(
        effect,
        Effect::LoadReview {
            project: PROJECT.into(),
            task_id: "T-9".into()
        }
    );
    assert_eq!(s.last(), &Performed::Host(effect));

    // Both clients run the shared loader for it. A serving review opens.
    let mut loader = ReviewLoader::default();
    assert_eq!(loader.open("T-9"), LoadStep::Resolve("T-9".into()));
    assert_eq!(
        loader.resolved("T-9", Ok(ReviewTarget::Serving(review_info()))),
        LoadStep::Open(Box::new(review_info()))
    );

    // A queued one asks which free slot to load onto, loads it, then opens
    // when the daemon announces it serving.
    let mut loader = ReviewLoader::default();
    loader.open("T-9");
    let step = loader.resolved(
        "T-9",
        Ok(ReviewTarget::Queued {
            title: "Warm the cache".into(),
            free_slots: vec!["review-1".into()],
            total_slots: 1,
        }),
    );
    assert_eq!(
        step,
        LoadStep::AskSlot {
            task: "T-9".into(),
            title: "Warm the cache".into(),
            free_slots: vec!["review-1".into()],
        }
    );
    s.client.open_overlay(Overlay::ReviewConfirm {
        task_id: "T-9".into(),
    });
    assert_eq!(
        loader.slot_chosen("T-9", "review-1"),
        LoadStep::Load {
            task: "T-9".into(),
            slot: "review-1".into()
        }
    );
    assert_eq!(loader.loaded("T-9", Ok(())), LoadStep::None);
    assert_eq!(loader.review_opened("T-9"), LoadStep::Resolve("T-9".into()));

    // Every slot busy: nothing loads.
    let mut loader = ReviewLoader::default();
    loader.open("T-9");
    assert!(matches!(
        loader.resolved(
            "T-9",
            Ok(ReviewTarget::Queued {
                title: "Warm the cache".into(),
                free_slots: vec![],
                total_slots: 1,
            })
        ),
        LoadStep::Busy { .. }
    ));
}

#[test]
fn the_zen_toggle_is_a_daemon_mutation_and_relabels() {
    let mut s = Scenario::new();
    let title = |m: &CommandModel| {
        CommandRegistry::new()
            .resolve("action:toggle-zen", m)
            .unwrap()
            .title()
            .to_string()
    };
    assert_eq!(title(&s.model), "Turn Zen Mode on");
    let effect = s.invoke("action:toggle-zen");
    let toggle = Mutation::ToggleZen {
        project: PROJECT.into(),
    };
    assert_eq!(effect, Effect::Mutate(toggle.clone()));
    assert_eq!(s.last(), &Performed::Mutation(toggle));
    // The next refresh reports Zen on; the same entry now turns it off.
    s.model.zen_on = true;
    assert_eq!(title(&s.model), "Turn Zen Mode off");
    assert_eq!(s.client.view(), &View::Session("orch".into()), "the view stays put");
}

#[test]
fn the_error_log_opens_its_overlay() {
    let mut s = Scenario::new();
    assert_eq!(
        s.invoke("action:error-log"),
        Effect::OpenErrorLog {
            project: PROJECT.into()
        }
    );
    assert_eq!(s.client.overlay(), Some(&Overlay::ErrorLog));
}

#[test]
fn switch_project_moves_the_client_or_opens_the_switcher() {
    let mut s = Scenario::new();
    s.client.set_view(View::Activity);
    assert_eq!(
        s.invoke("action:switch-project:beta"),
        Effect::SwitchProject {
            project: "beta".into()
        }
    );
    assert_eq!(s.client.project(), Some("beta"));
    assert_eq!(s.client.view(), &View::default_for_project(), "a first visit lands on the default");
    assert_eq!(s.client.remembered_view(PROJECT), Some(&View::Activity));

    let mut s = Scenario::new();
    assert_eq!(
        s.invoke("action:switch-project"),
        Effect::SwitchProject {
            project: String::new()
        }
    );
    assert_eq!(s.client.project(), Some(PROJECT), "a bare switch moves nowhere");
    assert_eq!(s.client.overlay(), Some(&Overlay::Palette));
}

#[test]
fn add_project_collects_the_form_then_mutates() {
    let mut s = Scenario::new();
    assert_eq!(s.invoke("action:add-project"), Effect::AddProject);
    assert_eq!(s.client.overlay(), Some(&Overlay::AddProject));

    // The shared form: the path prefills to the working directory; type a
    // name and submit.
    let mut form = add_project::Form::new(Path::new("/src/web"));
    type_into("Web App", |c| {
        form.handle_key(c);
    });
    assert_eq!(
        form.handle_key(KeyChord::new(Key::Enter, Mods::NONE)),
        add_project::Step::Submit
    );
    let m = Mutation::AddProject {
        name: form.name().to_string(),
        root: form.root().to_string(),
        mode: form.mode(),
    };
    s.run(Effect::Mutate(m.clone())).unwrap();
    assert_eq!(
        s.last(),
        &Performed::Mutation(Mutation::AddProject {
            name: "Web App".into(),
            root: "/src/web".into(),
            mode: ConfigMode::Global,
        })
    );
}

#[test]
fn detach_and_quit_reach_the_host() {
    for (id, effect) in [
        ("action:detach", Effect::Detach),
        ("action:detach-others", Effect::DetachOtherClients),
        (
            "action:quit-project",
            Effect::QuitProject {
                project: PROJECT.into(),
            },
        ),
        ("action:quit-shelbi", Effect::QuitShelbi),
    ] {
        let mut s = Scenario::new();
        assert_eq!(s.invoke(id), effect, "{id}");
        assert_eq!(s.last(), &Performed::Host(effect), "{id}");
        assert_eq!(s.client.project(), Some(PROJECT), "{id} leaves the client state alone");
        assert_eq!(s.client.overlay(), None, "{id}");
    }
    // Detach Other Clients is only offered while another client is attached.
    let mut m = fixture();
    m.other_clients_attached = false;
    assert!(CommandRegistry::new().resolve("action:detach-others", &m).is_none());
}

#[test]
fn new_window_reaches_the_host_on_a_multi_window_client() {
    // The desktop opens another window on the current project; this client's
    // own state (its project, view and focus) is untouched.
    let mut s = Scenario::new();
    let effect = Effect::NewWindow {
        project: PROJECT.into(),
    };
    assert_eq!(s.invoke("action:new-window"), effect);
    assert_eq!(s.last(), &Performed::Host(effect));
    assert_eq!(s.client.project(), Some(PROJECT));
    assert_eq!(s.client.view(), &View::Session("orch".into()));
    assert_eq!(s.client.overlay(), None);
    // The TUI's model never lists it: it is one terminal, with no window to
    // open.
    let mut m = fixture();
    m.multi_window = false;
    assert!(CommandRegistry::new().resolve("action:new-window", &m).is_none());
}

/// A scratch Shelbi root shared by the tests in this binary (set once, so
/// parallel tests never race on it).
fn scratch_home() -> &'static Path {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("shelbi-app-scenarios-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Set once, before any reader. `SHELBI_ROOT` wins over
        // `SHELBI_HOME`, so a pane that exports it can't leak in.
        std::env::set_var("SHELBI_ROOT", &dir);
        dir
    })
}

#[test]
fn edit_targets_open_their_file_in_the_editor() {
    scratch_home();
    let projects = shelbi_state::projects_dir().unwrap();
    let base = projects.join(PROJECT);
    for (id, target, path) in [
        (
            "edit:project",
            EditTarget::Project,
            projects.join(format!("{PROJECT}.yaml")),
        ),
        (
            "edit:agent:developer",
            EditTarget::Agent("developer".into()),
            base.join("agents").join("developer").join("instructions.md"),
        ),
        ("edit:zenmode", EditTarget::ZenMode, base.join("zenmode.md")),
        ("edit:workflows", EditTarget::Workflows, base.join("workflows")),
    ] {
        let mut s = Scenario::new();
        let effect = s.invoke(id);
        assert_eq!(effect, Effect::OpenEditor { target: target.clone() }, "{id}");
        assert_eq!(s.last(), &Performed::Host(effect), "{id} runs on the host");
        // Both clients open this one path.
        assert_eq!(edit_target_path(PROJECT, &target), Some(path), "{id}");
        assert_eq!(s.client.view(), &View::Session("orch".into()), "{id} keeps the view");
    }
}

#[test]
fn approve_is_sent_at_once() {
    let mut s = Scenario::new();
    let approve = Mutation::ApproveReview {
        project: PROJECT.into(),
        id: "T-9".into(),
    };
    assert_eq!(s.invoke("action:approve-review:T-9"), Effect::Mutate(approve.clone()));
    assert_eq!(s.last(), &Performed::Mutation(approve), "no confirmation in either client");
}

#[test]
fn reject_asks_for_a_reason_then_sends_it() {
    let mut s = Scenario::new();
    let effect = s.invoke("action:reject-review:T-9");
    assert_eq!(
        effect,
        Effect::Mutate(Mutation::RejectReview {
            project: PROJECT.into(),
            id: "T-9".into(),
            reason: String::new(),
        })
    );
    assert_eq!(s.last(), &Performed::Nothing, "nothing is sent without a reason");
    assert_eq!(
        s.client.overlay(),
        Some(&Overlay::RejectReason {
            task_id: "T-9".into()
        })
    );

    // The shared prompt: type a reason, submit with Ctrl+D.
    let mut prompt = review_reject::RejectPrompt::new();
    type_into("flaky test", |c| {
        prompt.handle_key(c);
    });
    assert_eq!(
        prompt.handle_key(KeyChord::new(Key::Char('d'), Mods::CONTROL)),
        review_reject::Step::Submit
    );
    s.client.close_overlay();
    let reject = Mutation::RejectReview {
        project: PROJECT.into(),
        id: "T-9".into(),
        reason: prompt.reason(),
    };
    s.run(Effect::Mutate(reject.clone())).unwrap();
    assert_eq!(
        s.last(),
        &Performed::Mutation(Mutation::RejectReview {
            project: PROJECT.into(),
            id: "T-9".into(),
            reason: "flaky test".into(),
        })
    );
}

#[test]
fn board_mutations_carry_the_project() {
    // The board's mutations aren't palette entries (they act on a card), so
    // the board builds the kind; the effect is the same for both clients.
    let s = |v: &str| v.to_string();
    for (kind, mutation) in [
        (
            CommandKind::MoveIssue {
                id: s("T-1"),
                to_status: s("done"),
            },
            Mutation::MoveIssue {
                project: s(PROJECT),
                id: s("T-1"),
                to_status: s("done"),
            },
        ),
        (
            CommandKind::StartIssue { id: s("T-1") },
            Mutation::StartIssue {
                project: s(PROJECT),
                id: s("T-1"),
            },
        ),
        (
            CommandKind::AssignIssue {
                id: s("T-1"),
                workspace: s("alpha-1"),
            },
            Mutation::AssignIssue {
                project: s(PROJECT),
                id: s("T-1"),
                workspace: s("alpha-1"),
            },
        ),
        (
            CommandKind::EditIssue { id: s("T-1") },
            Mutation::EditIssue {
                project: s(PROJECT),
                id: s("T-1"),
                title: None,
                body: None,
            },
        ),
        (
            CommandKind::AddIssue { title: s("New") },
            Mutation::AddIssue {
                project: s(PROJECT),
                title: s("New"),
                workflow: None,
            },
        ),
        (
            CommandKind::PrioritizeIssue {
                id: s("T-1"),
                position: 0,
            },
            Mutation::PrioritizeIssue {
                project: s(PROJECT),
                id: s("T-1"),
                position: 0,
            },
        ),
    ] {
        let mut sc = Scenario::new();
        let effect = kind.effect_with_project(PROJECT);
        assert_eq!(effect, Effect::Mutate(mutation.clone()), "{kind:?}");
        sc.run(effect).unwrap();
        assert_eq!(sc.last(), &Performed::Mutation(mutation), "{kind:?}");
        // Not in the palette or the menus: no registry id parses to them.
        assert!(CommandRegistry::new()
            .commands(&fixture())
            .iter()
            .all(|c| c.kind() != &kind));
    }
}
