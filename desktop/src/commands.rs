//! Exhaustive dispatch over the shared command and view enums.
//!
//! This is the desktop half of the plan's parity rule: every front end
//! matches [`CommandKind`] and [`View`] with **no wildcard arm**, so adding a
//! variant upstream in `shelbi-app` fails to compile here until the desktop
//! handles it. Do not add a `_ =>` arm to either match.
//!
//! Every arm is real: there is no not-implemented path. A command either
//! routes to a view, runs through the board, or asks the [`CommandHost`] (the
//! window) for one specific action, which the window performs with its
//! context in hand. `dt-parity-audit` removed the foundation's stub once the
//! last arms (`OpenEditor`, and `FocusSession`, retired upstream with the
//! tmux-era legacy agents it focused) were done.

use std::path::Path;

use shelbi_app::{CommandKind, EditTarget, Mutation, View};

/// What dispatch needs from its caller. The window implements it; tests
/// implement it with a recorder, so dispatch is exercised without a gpui
/// context.
pub trait CommandHost {
    /// Route the main area to `view` (`ClientState::set_view`) and focus it.
    fn show_view(&mut self, view: &View);
    /// Run an issue mutation (`MoveIssue`, `StartIssue`, `AssignIssue`,
    /// `EditIssue`, `AddIssue`) through the board, which captures the state
    /// the user was looking at and sends it with `execute_mutation` off the UI
    /// thread.
    fn issue_command(&mut self, kind: &CommandKind);
    /// Switch this window to `project` (in place: no new window).
    fn switch_project(&mut self, project: &str);
    /// Open another window on this window's project.
    fn new_window(&mut self);
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
    /// Open an `edit:*` target (project settings, an agent's instructions,
    /// `zenmode.md`, the workflows directory) in the user's editor.
    fn open_editor(&mut self, target: &EditTarget);
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
        // A serving task opens the Review view (dt-review-view); a queued one
        // asks which free slot to load it onto first.
        CommandKind::LoadReview { task_id } => host.load_review(task_id),
        CommandKind::ToggleZen => host.toggle_zen(),
        CommandKind::OpenErrorLog => host.open_error_log(),
        // The window switches in place (dt-switch-project-in-window). A bare
        // switch (no target) opens the switcher.
        CommandKind::SwitchProject { project } => {
            if project.is_empty() {
                host.open_project_switcher()
            } else {
                host.switch_project(project)
            }
        }
        CommandKind::AddProject => host.open_add_project(),
        // File ▸ New Window (⌘⇧N): another window, on this project, that
        // switches on its own.
        CommandKind::NewWindow => host.new_window(),
        CommandKind::Detach => host.detach(),
        CommandKind::DetachOtherClients => host.detach_other_clients(),
        CommandKind::QuitProject => host.quit_project(),
        // Not the app's Cmd+Q: that only closes this client. This is the
        // registry's "stop every agent and shut Shelbi down".
        CommandKind::QuitShelbi => host.quit_shelbi(),
        // The same path the TUI opens (`edit_target_path`), in the user's
        // editor: a terminal editor in Terminal.app, a GUI one directly.
        CommandKind::OpenEditor { target } => host.open_editor(target),
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
        CommandKind::OpenErrorLog => false,
        // Edits a local file in the user's editor.
        CommandKind::OpenEditor { .. } => false,
        // Only this client lets go.
        CommandKind::Detach => false,
        // Another window on a project already open.
        CommandKind::NewWindow => false,
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

/// Run a `Mutation::AddProject` the add-project wizard produced: validate it
/// (its repo path resolved against `cwd`) and scaffold the project, reporting
/// each stage and warning to `out`. `attempt` says whether this finishes an
/// earlier failed run of the same request. Returns the new project's slug.
/// Blocking.
pub fn run_add_project(
    mutation: &Mutation,
    attempt: shelbi_orchestrator::project_create::Attempt,
    cwd: &Path,
    out: &mut dyn shelbi_orchestrator::project_create::ScaffoldReporter,
) -> Result<String, shelbi_orchestrator::project_create::AddProjectError> {
    use shelbi_orchestrator::project_create::{add_project, AddProjectError};
    match mutation {
        Mutation::AddProject { name, root, mode } => add_project(name, root, *mode, cwd, attempt, out),
        other => Err(AddProjectError::Failed(format!("not an add-project mutation: {other:?}"))),
    }
}

/// Show a view in the main area. Every [`View`] variant has its own arm; each
/// routes through the window's `ClientState`, and the main area draws it
/// natively (the board, the feed, the machines, the chat, a workspace).
pub fn dispatch_view(view: &View, host: &mut impl CommandHost) {
    match view {
        View::Issues => host.show_view(view),
        View::Activity => host.show_view(view),
        View::Machines => host.show_view(view),
        View::Session(_) => host.show_view(view),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what dispatch asked of the host: the call as `<call>:<arg>`,
    /// or `issue:<kind>` for an issue mutation.
    #[derive(Default)]
    struct Recorder(Vec<String>);

    impl CommandHost for Recorder {
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
        fn new_window(&mut self) {
            self.0.push("new-window".into());
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
        fn open_editor(&mut self, target: &EditTarget) {
            let id = match target {
                EditTarget::Project => "edit:project".to_string(),
                EditTarget::Agent(agent) => format!("edit:agent:{agent}"),
                EditTarget::ZenMode => "edit:zenmode".to_string(),
                EditTarget::Workflows => "edit:workflows".to_string(),
            };
            self.0.push(format!("editor:{id}"));
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
            (CommandKind::NewWindow, "new-window"),
            (CommandKind::Detach, "detach"),
            (CommandKind::DetachOtherClients, "detach-others"),
            (CommandKind::QuitProject, "quit-project"),
            (CommandKind::QuitShelbi, "quit-shelbi"),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::Project,
                },
                "editor:edit:project",
            ),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::Agent(s("dev")),
                },
                "editor:edit:agent:dev",
            ),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::ZenMode,
                },
                "editor:edit:zenmode",
            ),
            (
                CommandKind::OpenEditor {
                    target: EditTarget::Workflows,
                },
                "editor:edit:workflows",
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
                "error-log",
                "new-window",
                "detach",
                "editor:edit:project",
                "editor:edit:agent:dev",
                "editor:edit:zenmode",
                "editor:edit:workflows",
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
        // Where the registry has an id for a command, the recorded call names
        // that same id.
        for (kind, expected) in every_command() {
            let expected = expected.strip_prefix("editor:").unwrap_or(expected);
            if let Some(parsed) = CommandKind::from_id(expected) {
                assert_eq!(parsed, kind, "{expected} round-trips through the registry");
            }
        }
    }

    #[test]
    fn every_view_routes_to_the_main_area() {
        for view in [
            View::Issues,
            View::Activity,
            View::Machines,
            View::Session("orch".into()),
            View::Session("alpha".into()),
        ] {
            let mut host = Recorder::default();
            dispatch_view(&view, &mut host);
            assert_eq!(host.0, vec![format!("show:{}", view.as_view_id())]);
        }
    }
}
