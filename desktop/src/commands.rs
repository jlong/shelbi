//! Exhaustive dispatch over the shared command and view enums.
//!
//! This is the desktop half of the plan's parity rule: every front end
//! matches [`CommandKind`] and [`View`] with **no wildcard arm**, so adding a
//! variant upstream in `shelbi-app` fails to compile here until the desktop
//! handles it. Do not add a `_ =>` arm to either match.
//!
//! In the foundation every arm reaches [`not_implemented`], which logs a
//! warning and asks the [`CommandHost`] (the window) to show the in-window
//! "Not implemented yet: <command id>" banner. Each `dt-*` subtask replaces
//! the arms it implements with the real behavior.
//!
//! The id named in the banner is the command's registry id where the
//! registry has one (`view:tasks`, `workspace:<name>`, `action:toggle-zen`,
//! ...). The issue mutations are not surfaced in the palette, so the
//! registry gives them no id; they get desktop-local `issue:<verb>:<id>`
//! labels here, used only in the banner and the log.

use shelbi_app::{CommandKind, EditTarget, View};

/// What dispatch needs from its caller. The window implements it; tests
/// implement it with a recorder, so dispatch is exercised without a gpui
/// context.
pub trait CommandHost {
    /// Surface that `command_id` is not implemented yet (the window shows a
    /// banner).
    fn not_implemented(&mut self, command_id: &str);
}

/// Run a command. Every [`CommandKind`] variant has its own arm.
pub fn dispatch_command(kind: &CommandKind, host: &mut impl CommandHost) {
    match kind {
        CommandKind::ShowView(view) => dispatch_view(view, host),
        // dt-workspace-view
        CommandKind::FocusWorkspace { workspace } => {
            not_implemented(host, &format!("workspace:{workspace}"))
        }
        // dt-review-view
        CommandKind::LoadReview { task_id } => not_implemented(host, &format!("review:{task_id}")),
        // dt-workspace-view
        CommandKind::FocusSession { session } => not_implemented(host, &format!("agent:{session}")),
        // dt-palette-overlays
        CommandKind::ToggleZen => not_implemented(host, "action:toggle-zen"),
        // dt-palette-overlays
        CommandKind::OpenErrorLog => not_implemented(host, "action:error-log"),
        // dt-shell-sidebar
        CommandKind::SwitchProject { project } => {
            if project.is_empty() {
                not_implemented(host, "action:switch-project")
            } else {
                not_implemented(host, &format!("action:switch-project:{project}"))
            }
        }
        // dt-shell-sidebar
        CommandKind::AddProject => not_implemented(host, "action:add-project"),
        // dt-shell-sidebar
        CommandKind::Detach => not_implemented(host, "action:detach"),
        // dt-shell-sidebar
        CommandKind::DetachOtherClients => not_implemented(host, "action:detach-others"),
        // dt-shell-sidebar
        CommandKind::QuitProject => not_implemented(host, "action:quit-project"),
        // dt-shell-sidebar. Not the app's Cmd+Q: that only closes this
        // client. This is the registry's "stop every agent and shut Shelbi
        // down".
        CommandKind::QuitShelbi => not_implemented(host, "action:quit-shelbi"),
        // dt-palette-overlays
        CommandKind::OpenEditor { target } => match target {
            EditTarget::Project => not_implemented(host, "edit:project"),
            EditTarget::Agent(agent) => not_implemented(host, &format!("edit:agent:{agent}")),
            EditTarget::ZenMode => not_implemented(host, "edit:zenmode"),
            EditTarget::Workflows => not_implemented(host, "edit:workflows"),
        },
        // dt-issues-board / dt-board-dnd
        CommandKind::MoveIssue { id, to_status } => {
            not_implemented(host, &format!("issue:move:{id}:{to_status}"))
        }
        // dt-issues-board
        CommandKind::StartIssue { id } => not_implemented(host, &format!("issue:start:{id}")),
        // dt-issues-board
        CommandKind::AssignIssue { id, workspace } => {
            not_implemented(host, &format!("issue:assign:{id}:{workspace}"))
        }
        // dt-review-view
        CommandKind::ApproveReview { id } => not_implemented(host, &format!("issue:approve:{id}")),
        // dt-review-view. The reason is user prose, so it stays out of the
        // banner and the log.
        CommandKind::RejectReview { id, reason: _ } => {
            not_implemented(host, &format!("issue:reject:{id}"))
        }
        // dt-issues-board
        CommandKind::EditIssue { id } => not_implemented(host, &format!("issue:edit:{id}")),
        // dt-issues-board
        CommandKind::AddIssue { title: _ } => not_implemented(host, "issue:add"),
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
    }
}

/// Show a view in the main area. Every [`View`] variant has its own arm.
pub fn dispatch_view(view: &View, host: &mut impl CommandHost) {
    match view {
        // dt-issues-board
        View::Issues => not_implemented(host, "view:tasks"),
        // dt-activity-machines
        View::Activity => not_implemented(host, "view:activity"),
        // dt-activity-machines
        View::Machines => not_implemented(host, "view:machines"),
        // dt-chat-view (the orchestrator, `orch`) / dt-workspace-view
        View::Session(name) => not_implemented(host, &format!("view:{name}")),
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

    #[derive(Default)]
    struct Recorder(Vec<String>);

    impl CommandHost for Recorder {
        fn not_implemented(&mut self, command_id: &str) {
            self.0.push(command_id.to_string());
        }
    }

    fn id_for(kind: CommandKind) -> String {
        let mut host = Recorder::default();
        dispatch_command(&kind, &mut host);
        assert_eq!(host.0.len(), 1, "{kind:?} must reach the stub exactly once");
        host.0.remove(0)
    }

    /// One sample of every variant. Kept exhaustive by hand; the compiler
    /// keeps `dispatch_command` exhaustive.
    fn every_command() -> Vec<(CommandKind, &'static str)> {
        let s = |v: &str| v.to_string();
        vec![
            (CommandKind::ShowView(View::Issues), "view:tasks"),
            (CommandKind::ShowView(View::Activity), "view:activity"),
            (CommandKind::ShowView(View::Machines), "view:machines"),
            (CommandKind::ShowView(View::Session(s("orch"))), "view:orch"),
            (
                CommandKind::FocusWorkspace {
                    workspace: s("alpha"),
                },
                "workspace:alpha",
            ),
            (CommandKind::LoadReview { task_id: s("t1") }, "review:t1"),
            (CommandKind::FocusSession { session: s("s1") }, "agent:s1"),
            (CommandKind::ToggleZen, "action:toggle-zen"),
            (CommandKind::OpenErrorLog, "action:error-log"),
            (
                CommandKind::SwitchProject {
                    project: String::new(),
                },
                "action:switch-project",
            ),
            (
                CommandKind::SwitchProject { project: s("web") },
                "action:switch-project:web",
            ),
            (CommandKind::AddProject, "action:add-project"),
            (CommandKind::Detach, "action:detach"),
            (CommandKind::DetachOtherClients, "action:detach-others"),
            (CommandKind::QuitProject, "action:quit-project"),
            (CommandKind::QuitShelbi, "action:quit-shelbi"),
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
            (
                CommandKind::ApproveReview { id: s("t1") },
                "issue:approve:t1",
            ),
            (
                CommandKind::RejectReview {
                    id: s("t1"),
                    reason: s("nope"),
                },
                "issue:reject:t1",
            ),
            (CommandKind::EditIssue { id: s("t1") }, "issue:edit:t1"),
            (CommandKind::AddIssue { title: s("New") }, "issue:add"),
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
                "view:tasks",
                "view:activity",
                "view:machines",
                "view:orch",
                "agent:s1",
                "action:error-log",
                "action:detach",
                "edit:project",
                "edit:agent:dev",
                "edit:zenmode",
                "edit:workflows",
            ]
        );
    }

    #[test]
    fn every_command_reaches_the_not_implemented_stub() {
        for (kind, expected) in every_command() {
            assert_eq!(id_for(kind), expected);
        }
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
    fn every_view_reaches_the_stub() {
        for (view, expected) in [
            (View::Issues, "view:tasks"),
            (View::Activity, "view:activity"),
            (View::Machines, "view:machines"),
            (View::Session("orch".into()), "view:orch"),
        ] {
            let mut host = Recorder::default();
            dispatch_view(&view, &mut host);
            assert_eq!(host.0, vec![expected.to_string()]);
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
