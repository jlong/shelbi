//! Exhaustive dispatch over the shared command and view enums.
//!
//! This is the desktop half of the plan's parity rule: every front end
//! matches [`CommandKind`] and [`View`] with **no wildcard arm**, so adding a
//! variant upstream in `shelbi-app` fails to compile here until the desktop
//! handles it. Do not add a `_ =>` arm to either match.
//!
//! An arm whose subtask hasn't landed reaches [`not_implemented`], which logs
//! a warning and asks the [`CommandHost`] (the window) to show the in-window
//! "Not implemented yet: <command id>" banner. Each `dt-*` subtask replaces
//! the arms it implements with the real behavior. `dt-shell-sidebar` owns view
//! routing (every `View` routes through the window's `ClientState`; a view
//! not yet ported draws the stub in the main area), project switching, and
//! the detach / quit lifecycle commands; `dt-issues-board` owns the Issues
//! view and the board's issue mutations; `dt-activity-machines` the Activity
//! and Machines views; `dt-palette-overlays` the Zen toggle, the error log,
//! add project, loading a queued review onto a slot, and approving /
//! rejecting a review.
//!
//! The id named in the banner is the command's registry id where the
//! registry has one (`view:tasks`, `workspace:<name>`, `action:toggle-zen`,
//! ...). The issue mutations are not surfaced in the palette, so the
//! registry gives them no id; the tests name them with desktop-local
//! `issue:<verb>:<id>` labels.

use std::path::Path;

use shelbi_app::{CommandKind, EditTarget, Mutation, View};

/// What dispatch needs from its caller. The window implements it; tests
/// implement it with a recorder, so dispatch is exercised without a gpui
/// context.
pub trait CommandHost {
    /// Surface that `command_id` is not implemented yet (the window shows a
    /// banner).
    fn not_implemented(&mut self, command_id: &str);
    /// Route the main area to `view` (`ClientState::set_view`) and focus it.
    fn show_view(&mut self, view: &View);
    /// Run an issue mutation (`MoveIssue`, `StartIssue`, `AssignIssue`,
    /// `EditIssue`, `AddIssue`) through the board, which captures the state
    /// the user was looking at and sends it with `execute_mutation` off the UI
    /// thread.
    fn issue_command(&mut self, kind: &CommandKind);
    /// Open (or focus) `project`'s window.
    fn switch_project(&mut self, project: &str);
    /// Show the project switcher.
    fn open_project_switcher(&mut self);
    /// Leave: close the app, leaving every session and agent running.
    fn detach(&mut self);
    /// Tell every other attached client to detach.
    fn detach_other_clients(&mut self);
    /// Confirm, then quit this window's project (its agents stop).
    fn quit_project(&mut self);
    /// Confirm, then quit Shelbi (every agent stops, the daemon exits).
    fn quit_shelbi(&mut self);
    /// Toggle Zen Mode (through the first-run intro on a first off-to-on).
    fn toggle_zen(&mut self);
    /// Open the project's error log.
    fn open_error_log(&mut self);
    /// Open the add-project form.
    fn open_add_project(&mut self);
    /// Open a review task: a queued one asks which slot to load it onto.
    fn load_review(&mut self, task_id: &str);
    /// Approve (accept and merge) a review task.
    fn approve_review(&mut self, id: &str);
    /// Ask for the reason to reject a review task with.
    fn ask_reject_reason(&mut self, id: &str);
    /// Reject a review task with `reason`.
    fn reject_review(&mut self, id: &str, reason: &str);
}

/// Run a command. Every [`CommandKind`] variant has its own arm.
pub fn dispatch_command(kind: &CommandKind, host: &mut impl CommandHost) {
    match kind {
        CommandKind::ShowView(view) => dispatch_view(view, host),
        // dt-workspace-view: the Workspace view. Selecting it attaches the
        // agent; an idle workspace shows the TUI's idle placeholder (nothing
        // is launched from here: the daemon dispatches agents).
        CommandKind::FocusWorkspace { workspace } => {
            host.show_view(&View::Session(workspace.clone()))
        }
        // A queued task asks for its slot here; a serving one opens the
        // review view (dt-review-view).
        CommandKind::LoadReview { task_id } => host.load_review(task_id),
        // Legacy spawned agents: the TUI doesn't show them either ("legacy
        // spawned agents aren't shown here yet").
        CommandKind::FocusSession { session } => not_implemented(host, &format!("agent:{session}")),
        CommandKind::ToggleZen => host.toggle_zen(),
        CommandKind::OpenErrorLog => host.open_error_log(),
        // One window per project: open the target's, or focus it. A bare
        // switch (no target) opens the switcher.
        CommandKind::SwitchProject { project } => {
            if project.is_empty() {
                host.open_project_switcher()
            } else {
                host.switch_project(project)
            }
        }
        CommandKind::AddProject => host.open_add_project(),
        CommandKind::Detach => host.detach(),
        CommandKind::DetachOtherClients => host.detach_other_clients(),
        CommandKind::QuitProject => host.quit_project(),
        // Not the app's Cmd+Q: that only closes this client. This is the
        // registry's "stop every agent and shut Shelbi down".
        CommandKind::QuitShelbi => host.quit_shelbi(),
        // The TUI doesn't open editors from the palette yet either (its status
        // line says so); both clients pick this up together.
        CommandKind::OpenEditor { target } => match target {
            EditTarget::Project => not_implemented(host, "edit:project"),
            EditTarget::Agent(agent) => not_implemented(host, &format!("edit:agent:{agent}")),
            EditTarget::ZenMode => not_implemented(host, "edit:zenmode"),
            EditTarget::Workflows => not_implemented(host, "edit:workflows"),
        },
        // The board's issue mutations (dt-issues-board; drag and drop reuses
        // MoveIssue and adds PrioritizeIssue in dt-board-dnd).
        CommandKind::MoveIssue { .. }
        | CommandKind::StartIssue { .. }
        | CommandKind::AssignIssue { .. }
        | CommandKind::PrioritizeIssue { .. } => host.issue_command(kind),
        // Approve has no confirmation, in either client.
        CommandKind::ApproveReview { id } => host.approve_review(id),
        // The palette's reject carries no reason: ask for one first.
        CommandKind::RejectReview { id, reason } => {
            if reason.trim().is_empty() {
                host.ask_reject_reason(id)
            } else {
                host.reject_review(id, reason)
            }
        }
        CommandKind::EditIssue { .. } | CommandKind::AddIssue { .. } => host.issue_command(kind),
    }
}

/// Whether `kind` asks the daemon to do something (start, change, or stop
/// work), as opposed to only showing what is already there. While a relaunch
/// is required (the CLI was upgraded under the app) these are refused; views,
/// including attached session views, keep working. No wildcard arm.
pub fn sends_to_daemon(kind: &CommandKind) -> bool {
    match kind {
        CommandKind::ShowView(_) => false,
        CommandKind::FocusSession { .. } => false,
        CommandKind::OpenErrorLog => false,
        // Edits a local file in the user's editor.
        CommandKind::OpenEditor { .. } => false,
        // Only this client lets go.
        CommandKind::Detach => false,
        // May launch the workspace's agent.
        CommandKind::FocusWorkspace { .. } => true,
        CommandKind::LoadReview { .. } => true,
        CommandKind::ToggleZen => true,
        // Starts the target project's orchestrator.
        CommandKind::SwitchProject { .. } => true,
        CommandKind::AddProject => true,
        CommandKind::DetachOtherClients => true,
        CommandKind::QuitProject => true,
        CommandKind::QuitShelbi => true,
        CommandKind::MoveIssue { .. } => true,
        CommandKind::StartIssue { .. } => true,
        CommandKind::AssignIssue { .. } => true,
        CommandKind::ApproveReview { .. } => true,
        CommandKind::RejectReview { .. } => true,
        CommandKind::EditIssue { .. } => true,
        CommandKind::AddIssue { .. } => true,
        CommandKind::PrioritizeIssue { .. } => true,
    }
}

/// Run a `Mutation::AddProject` the add-project form produced: validate it
/// (its repo path resolved against `cwd`) and scaffold the project, the way
/// the TUI's form does. Returns the new project's slug. Blocking.
pub fn run_add_project(
    mutation: &Mutation,
    cwd: &Path,
) -> Result<String, shelbi_orchestrator::project_create::AddProjectError> {
    use shelbi_orchestrator::project_create::{add_project, AddProjectError};
    match mutation {
        Mutation::AddProject { name, root, mode } => add_project(name, root, *mode, cwd),
        other => Err(AddProjectError::Failed(format!("not an add-project mutation: {other:?}"))),
    }
}

/// Show a view in the main area. Every [`View`] variant has its own arm; each
/// routes through the window's `ClientState`, and the main area draws the
/// not-implemented stub ([`view_stub_id`]) for a view not yet ported.
pub fn dispatch_view(view: &View, host: &mut impl CommandHost) {
    match view {
        View::Issues => host.show_view(view),
        View::Activity => host.show_view(view),
        View::Machines => host.show_view(view),
        View::Session(_) => host.show_view(view),
    }
}

/// The stub id the main area shows for `view` while its subtask hasn't
/// landed, or `None` once it renders for real. Exhaustive like the dispatch.
pub fn view_stub_id(view: &View) -> Option<String> {
    match view {
        // dt-issues-board: the native board.
        View::Issues => None,
        // dt-activity-machines: the native feed and machines views.
        View::Activity => None,
        View::Machines => None,
        // dt-chat-view: the orchestrator chat.
        View::Session(name) if name == "orch" => None,
        // dt-workspace-view: the Workspace view.
        View::Session(_) => None,
    }
}

/// The shared stub every unimplemented arm calls: log at warn, then let the
/// host show the banner.
fn not_implemented(host: &mut impl CommandHost, command_id: &str) {
    tracing::warn!(
        command = command_id,
        "command not implemented yet in the desktop app"
    );
    host.not_implemented(command_id);
}

/// The banner text for an unimplemented command.
pub fn not_implemented_message(command_id: &str) -> String {
    format!("Not implemented yet: {command_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what dispatch asked of the host: a stub's command id, the
    /// real call as `<call>:<arg>`, or `issue:<kind>` for an issue mutation.
    #[derive(Default)]
    struct Recorder(Vec<String>);

    impl CommandHost for Recorder {
        fn not_implemented(&mut self, command_id: &str) {
            self.0.push(command_id.to_string());
        }

        fn issue_command(&mut self, kind: &CommandKind) {
            self.0.push(format!("issue:{kind:?}"));
        }
        fn show_view(&mut self, view: &View) {
            self.0.push(format!("show:{}", view.as_view_id()));
        }
        fn switch_project(&mut self, project: &str) {
            self.0.push(format!("switch:{project}"));
        }
        fn open_project_switcher(&mut self) {
            self.0.push("switcher".into());
        }
        fn detach(&mut self) {
            self.0.push("detach".into());
        }
        fn detach_other_clients(&mut self) {
            self.0.push("detach-others".into());
        }
        fn quit_project(&mut self) {
            self.0.push("quit-project".into());
        }
        fn quit_shelbi(&mut self) {
            self.0.push("quit-shelbi".into());
        }
        fn toggle_zen(&mut self) {
            self.0.push("toggle-zen".into());
        }
        fn open_error_log(&mut self) {
            self.0.push("error-log".into());
        }
        fn open_add_project(&mut self) {
            self.0.push("add-project".into());
        }
        fn load_review(&mut self, task_id: &str) {
            self.0.push(format!("load-review:{task_id}"));
        }
        fn approve_review(&mut self, id: &str) {
            self.0.push(format!("approve:{id}"));
        }
        fn ask_reject_reason(&mut self, id: &str) {
            self.0.push(format!("reject-reason:{id}"));
        }
        fn reject_review(&mut self, id: &str, reason: &str) {
            self.0.push(format!("reject:{id}:{reason}"));
        }
    }

    fn id_for(kind: CommandKind) -> String {
        let mut host = Recorder::default();
        dispatch_command(&kind, &mut host);
        assert_eq!(host.0.len(), 1, "{kind:?} must reach the host exactly once");
        host.0.remove(0)
    }

    /// One sample of every variant. Kept exhaustive by hand; the compiler
    /// keeps `dispatch_command` exhaustive.
    fn every_command() -> Vec<(CommandKind, &'static str)> {
        let s = |v: &str| v.to_string();
        vec![
            (CommandKind::ShowView(View::Issues), "show:tasks"),
            (CommandKind::ShowView(View::Activity), "show:activity"),
            (CommandKind::ShowView(View::Machines), "show:machines"),
            (CommandKind::ShowView(View::Session(s("orch"))), "show:orch"),
            (
                CommandKind::FocusWorkspace {
                    workspace: s("alpha"),
                },
                "show:alpha",
            ),
            (CommandKind::LoadReview { task_id: s("t1") }, "load-review:t1"),
            (CommandKind::FocusSession { session: s("s1") }, "agent:s1"),
            (CommandKind::ToggleZen, "toggle-zen"),
            (CommandKind::OpenErrorLog, "error-log"),
            (
                CommandKind::SwitchProject {
                    project: String::new(),
                },
                "switcher",
            ),
            (CommandKind::SwitchProject { project: s("web") }, "switch:web"),
            (CommandKind::AddProject, "add-project"),
            (CommandKind::Detach, "detach"),
            (CommandKind::DetachOtherClients, "detach-others"),
            (CommandKind::QuitProject, "quit-project"),
            (CommandKind::QuitShelbi, "quit-shelbi"),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::Project,
                },
                "edit:project",
            ),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::Agent(s("dev")),
                },
                "edit:agent:dev",
            ),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::ZenMode,
                },
                "edit:zenmode",
            ),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::Workflows,
                },
                "edit:workflows",
            ),
            (
                CommandKind::MoveIssue {
                    id: s("t1"),
                    to_status: s("done"),
                },
                "issue:move:t1:done",
            ),
            (CommandKind::StartIssue { id: s("t1") }, "issue:start:t1"),
            (
                CommandKind::AssignIssue {
                    id: s("t1"),
                    workspace: s("alpha"),
                },
                "issue:assign:t1:alpha",
            ),
            (CommandKind::ApproveReview { id: s("t1") }, "approve:t1"),
            (
                CommandKind::RejectReview {
                    id: s("t1"),
                    reason: String::new(),
                },
                "reject-reason:t1",
            ),
            (
                CommandKind::RejectReview {
                    id: s("t1"),
                    reason: s("nope"),
                },
                "reject:t1:nope",
            ),
            (CommandKind::EditIssue { id: s("t1") }, "issue:edit:t1"),
            (CommandKind::AddIssue { title: s("New") }, "issue:add"),
            (
                CommandKind::PrioritizeIssue {
                    id: s("t1"),
                    position: 0,
                },
                "issue:prioritize:t1",
            ),
        ]
    }

    #[test]
    fn only_views_and_local_actions_run_while_a_relaunch_is_pending() {
        let allowed: Vec<&str> = every_command()
            .into_iter()
            .filter(|(kind, _)| !sends_to_daemon(kind))
            .map(|(_, id)| id)
            .collect();
        assert_eq!(
            allowed,
            [
                "show:tasks",
                "show:activity",
                "show:machines",
                "show:orch",
                "agent:s1",
                "error-log",
                "detach",
                "edit:project",
                "edit:agent:dev",
                "edit:zenmode",
                "edit:workflows",
            ]
        );
    }

    #[test]
    fn every_command_reaches_the_host_once() {
        for (kind, expected) in every_command() {
            let reached = id_for(kind.clone());
            if is_board_command(&kind) {
                assert_eq!(reached, format!("issue:{kind:?}"), "{expected} runs on the board");
            } else {
                assert_eq!(reached, expected);
            }
        }
    }

    fn is_board_command(kind: &CommandKind) -> bool {
        matches!(
            kind,
            CommandKind::MoveIssue { .. }
                | CommandKind::StartIssue { .. }
                | CommandKind::AssignIssue { .. }
                | CommandKind::EditIssue { .. }
                | CommandKind::AddIssue { .. }
                | CommandKind::PrioritizeIssue { .. }
        )
    }

    #[test]
    fn registry_ids_match_the_shared_id_scheme() {
        // Where the registry has an id for a command, the banner names that
        // same id, so a user can match the banner to the palette entry.
        for (kind, expected) in every_command() {
            if let Some(parsed) = CommandKind::from_id(expected) {
                assert_eq!(parsed, kind, "{expected} round-trips through the registry");
            }
        }
    }

    #[test]
    fn every_view_routes_and_unported_ones_name_their_stub() {
        for (view, stub) in [
            (View::Issues, None),
            (View::Activity, None),
            (View::Machines, None),
            (View::Session("orch".into()), None),
            (View::Session("alpha".into()), None),
        ] {
            let mut host = Recorder::default();
            dispatch_view(&view, &mut host);
            assert_eq!(host.0, vec![format!("show:{}", view.as_view_id())]);
            assert_eq!(view_stub_id(&view).as_deref(), stub);
        }
    }

    #[test]
    fn banner_names_the_command() {
        assert_eq!(
            not_implemented_message("view:tasks"),
            "Not implemented yet: view:tasks"
        );
    }
}
