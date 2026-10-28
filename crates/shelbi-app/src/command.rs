//! The command registry.
//!
//! This replaces the palette's string-prefix dispatch — a convention where
//! an [`shelbi_palette::Entry`]'s `id` was a `namespace:payload` string
//! matched with `strip_prefix` — with a real registry of typed commands.
//!
//! Each [`Command`] has:
//! - a stable **id** (kept byte-for-byte compatible with the old palette id
//!   scheme, so the tmux palette can be driven from the registry without a
//!   migration),
//! - a **title** shown to the user,
//! - **typed arguments** carried in its [`CommandKind`] rather than packed
//!   into a string,
//! - an **availability** check against the current [`CommandModel`], and
//! - an **[`Effect`]** it produces when invoked, which the host runs
//!   through the [`Executor`](crate::exec::Executor) seam.
//!
//! The registry is enumerated from a [`CommandModel`] snapshot the caller
//! builds from live state (its sidebar rows, Zen state, project list, and
//! on-disk edit targets). [`CommandRegistry::entries`] turns the available
//! commands into palette entries; [`CommandRegistry::resolve`] turns an id
//! back into a [`Command`]. Both are what let the existing palette read the
//! registry instead of its own hand-rolled table.

use shelbi_palette::{Decoration, DecorationColor, Entry, EntryKind};

use crate::exec::{EditTarget, Effect, Mutation};
use crate::nav::View;

/// What a command does, with its typed arguments inline. The project a
/// command acts on is contextual (the client's current project) and is
/// supplied to [`Command::effect`] rather than stored per command, except
/// for [`CommandKind::SwitchProject`], whose target is a *different*
/// project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandKind {
    /// Show a native or session view in the main area.
    ShowView(View),
    /// Focus a workspace, launching its pane lazily if needed.
    FocusWorkspace { workspace: String },
    /// Load a task into a review slot and show it.
    LoadReview { task_id: String },
    /// Toggle Zen Mode for the current project.
    ToggleZen,
    /// Open the per-project error log.
    OpenErrorLog,
    /// Open Settings (the desktop app's ⌘,; the TUI's sidebar `,`).
    OpenSettings,
    /// Send Feedback: open Contact Us (the sidebar footer's megaphone in
    /// both clients; the desktop's Help ▸ Send Feedback…).
    SendFeedback,
    /// Switch to another project.
    SwitchProject { project: String },
    /// Begin the add-project flow.
    AddProject,
    /// Open another window on the current project. Offered only by a client
    /// that has windows to open ([`CommandModel::multi_window`]).
    NewWindow,
    /// Detach this client (leave Shelbi running in the background).
    Detach,
    /// Detach every other client attached to this session.
    DetachOtherClients,
    /// Quit the current project.
    QuitProject,
    /// Quit Shelbi entirely.
    QuitShelbi,
    /// Open an editable config/instructions target.
    OpenEditor { target: EditTarget },
    // ---- issue mutations (the generic set; not surfaced in the tmux
    // palette today, carried here so the single-process TUI / desktop app
    // bind them from one definition) ------------------------------------
    MoveIssue { id: String, to_status: String },
    StartIssue { id: String },
    AssignIssue { id: String, workspace: String },
    ApproveReview { id: String },
    RejectReview { id: String, reason: String },
    EditIssue { id: String },
    AddIssue { title: String },
    /// Reorder an issue within its column: land it at absolute slot
    /// `position` (0 is the top) of the column's priority order. The board
    /// computes the slot from the unfiltered column
    /// ([`crate::board::reorder_slot`]).
    PrioritizeIssue { id: String, position: u32 },
}

/// A registry command: metadata + typed arguments. Built by the registry
/// from a [`CommandModel`]; convertible to a palette [`Entry`] and back via
/// its [`id`](Command::id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    id: String,
    title: String,
    kind: CommandKind,
    entry_kind: EntryKind,
    decoration: Option<Decoration>,
    subtitle: Option<String>,
    shortcut: Option<String>,
    hidden_until_query: bool,
}

impl Command {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn kind(&self) -> &CommandKind {
        &self.kind
    }

    pub fn hidden_until_query(&self) -> bool {
        self.hidden_until_query
    }

    /// True when invoking this command yields a daemon-routable
    /// [`Effect::Mutate`] — the subset `rt-mutations-daemon` will route to
    /// the daemon.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self.kind,
            CommandKind::ToggleZen
                | CommandKind::AddProject
                | CommandKind::MoveIssue { .. }
                | CommandKind::StartIssue { .. }
                | CommandKind::AssignIssue { .. }
                | CommandKind::ApproveReview { .. }
                | CommandKind::RejectReview { .. }
                | CommandKind::EditIssue { .. }
                | CommandKind::AddIssue { .. }
                | CommandKind::PrioritizeIssue { .. }
        )
    }

    /// Build the [`Effect`] this command produces. `project` is the
    /// client's current project; it is ignored for
    /// [`CommandKind::SwitchProject`], which carries its own target.
    ///
    /// Commands that need the host to collect input first
    /// ([`CommandKind::AddProject`], [`CommandKind::RejectReview`]) produce
    /// a placeholder-free effect only when the registry already has the
    /// input; `AddProject` yields [`Effect::AddProject`] (the host runs the
    /// form, then issues [`Mutation::AddProject`]).
    pub fn effect(&self, project: &str) -> Effect {
        self.kind.effect_with_project(project)
    }
}

impl CommandKind {
    /// Build the [`Effect`] for this kind, binding `project` where the
    /// effect needs the current project. This is the half of dispatch a
    /// host reaches after [`CommandKind::from_id`] has recovered the typed
    /// command from an entry id.
    pub fn effect_with_project(&self, project: &str) -> Effect {
        let p = || project.to_string();
        match self {
            CommandKind::ShowView(v) => Effect::ShowView(v.clone()),
            CommandKind::FocusWorkspace { workspace } => Effect::FocusWorkspace {
                project: p(),
                workspace: workspace.clone(),
            },
            CommandKind::LoadReview { task_id } => Effect::LoadReview {
                project: p(),
                task_id: task_id.clone(),
            },
            CommandKind::ToggleZen => Effect::Mutate(Mutation::ToggleZen { project: p() }),
            CommandKind::OpenErrorLog => Effect::OpenErrorLog { project: p() },
            CommandKind::OpenSettings => Effect::OpenSettings,
            CommandKind::SendFeedback => Effect::SendFeedback,
            CommandKind::SwitchProject { project } => Effect::SwitchProject {
                project: project.clone(),
            },
            CommandKind::AddProject => Effect::AddProject,
            CommandKind::NewWindow => Effect::NewWindow { project: p() },
            CommandKind::Detach => Effect::Detach,
            CommandKind::DetachOtherClients => Effect::DetachOtherClients,
            CommandKind::QuitProject => Effect::QuitProject { project: p() },
            CommandKind::QuitShelbi => Effect::QuitShelbi,
            CommandKind::OpenEditor { target } => Effect::OpenEditor {
                target: target.clone(),
            },
            CommandKind::MoveIssue { id, to_status } => Effect::Mutate(Mutation::MoveIssue {
                project: p(),
                id: id.clone(),
                to_status: to_status.clone(),
            }),
            CommandKind::StartIssue { id } => Effect::Mutate(Mutation::StartIssue {
                project: p(),
                id: id.clone(),
            }),
            CommandKind::AssignIssue { id, workspace } => Effect::Mutate(Mutation::AssignIssue {
                project: p(),
                id: id.clone(),
                workspace: workspace.clone(),
            }),
            CommandKind::ApproveReview { id } => Effect::Mutate(Mutation::ApproveReview {
                project: p(),
                id: id.clone(),
            }),
            CommandKind::RejectReview { id, reason } => Effect::Mutate(Mutation::RejectReview {
                project: p(),
                id: id.clone(),
                reason: reason.clone(),
            }),
            CommandKind::EditIssue { id } => Effect::Mutate(Mutation::EditIssue {
                project: p(),
                id: id.clone(),
                title: None,
                body: None,
            }),
            CommandKind::AddIssue { title } => Effect::Mutate(Mutation::AddIssue {
                project: p(),
                title: title.clone(),
                workflow: None,
            }),
            CommandKind::PrioritizeIssue { id, position } => {
                Effect::Mutate(Mutation::PrioritizeIssue {
                    project: p(),
                    id: id.clone(),
                    position: *position,
                })
            }
        }
    }
}

impl Command {
    /// Convert to a palette [`Entry`]. The entry's `label` is the title;
    /// the fuzzy matcher runs over it.
    pub fn to_entry(&self) -> Entry {
        Entry {
            id: self.id.clone(),
            label: self.title.clone(),
            kind: self.entry_kind,
            subtitle: self.subtitle.clone(),
            shortcut: self.shortcut.clone(),
            decoration: self.decoration.clone(),
            hidden_until_query: self.hidden_until_query,
        }
    }
}

// ---------------------------------------------------------------------------
// The model the registry enumerates against.
// ---------------------------------------------------------------------------

/// A sidebar nav builtin (Chat / Tasks / Activity): the view it shows plus
/// its id, title, and decoration. `id` is kept because the historical
/// palette ids (`view:orch`, `view:tasks`, `view:activity`) are not
/// derivable from the [`View`] alone (the orchestrator "chat" is a session
/// view whose builtin name is `orch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewItem {
    pub id: String,
    pub title: String,
    pub view: View,
    pub decoration: Option<Decoration>,
}

/// A dev workspace row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceItem {
    pub name: String,
    pub subtitle: Option<String>,
    pub decoration: Option<Decoration>,
}

/// A review task row (ready or queued).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub task_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub decoration: Option<Decoration>,
}

/// Another registered project (switch target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectItem {
    pub slug: String,
    pub label: String,
}

/// An on-disk edit target that exists for this project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditItem {
    pub target: EditTarget,
    pub title: String,
}

/// A snapshot of the state the registry enumerates commands from. The
/// caller builds this from its live view (sidebar rows, Zen state, project
/// list, edit targets). Enumeration is a pure function of this snapshot, so
/// the registry has no IO and the mapping is unit-testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandModel {
    /// The current project slug, or `None` before one is selected.
    pub project: Option<String>,
    /// Whether Zen Mode is currently on (drives the toggle's label).
    pub zen_on: bool,
    /// The Zen toggle's hotkey hint, if bound (e.g. `⌥Z`).
    pub zen_shortcut: Option<String>,
    pub views: Vec<ViewItem>,
    pub workspaces: Vec<WorkspaceItem>,
    pub reviews: Vec<ReviewItem>,
    /// Other registered projects (switch targets) — excludes the current
    /// project.
    pub other_projects: Vec<ProjectItem>,
    pub edit_targets: Vec<EditItem>,
    /// Whether any *other* client is attached to this session right now. Gates
    /// the "Detach Other Clients" command (hidden when this is the only client).
    pub other_clients_attached: bool,
    /// Whether this client opens more than one window (the desktop app).
    /// Gates "New Window": the TUI is one terminal, so it never lists it.
    pub multi_window: bool,
}

/// The one-line description shown beside a nav view in the command palette's
/// second column. Keyed off the [`View`] so the orchestrator chat, the issues
/// board, the activity log, and the machines view each read in the palette's
/// voice (the Figma design fixes the first three verbatim). Workspace and other
/// session views carry their own contextual subtitle, so they get `None` here.
fn view_description(view: &View) -> Option<&'static str> {
    match view {
        View::Session(name) if name == "orch" => {
            Some("Talk with the Orchestrator to manage Shelbi")
        }
        View::Issues => Some("Queue up and manage work for Shelbi"),
        View::Activity => Some("See what's happened recently"),
        View::Machines => Some("See the machines running your agents"),
        View::Session(_) => None,
    }
}

// ---------------------------------------------------------------------------
// The registry.
// ---------------------------------------------------------------------------

/// The command registry. Stateless — every method is a pure function of the
/// [`CommandModel`] passed in — so a caller can rebuild entries each frame
/// without holding registry state.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandRegistry;

impl CommandRegistry {
    pub fn new() -> Self {
        CommandRegistry
    }

    /// Every available command for `model`, in the order the palette lists
    /// them: the nav views, then the Zen toggle, then workspaces, reviews,
    /// the review decisions (approve / reject), the error-log action, Settings, Send Feedback, switch-project targets,
    /// add-project, the edit targets, and finally the quit actions.
    ///
    /// "Available" folds in the existence gating the palette did by
    /// construction (a workspace/review/agent/edit entry only appears when
    /// the model has it) plus the few genuine predicates (switch-project
    /// needs a target project; quit-project needs a selected project).
    pub fn commands(&self, model: &CommandModel) -> Vec<Command> {
        let mut out = Vec::new();

        // Nav views.
        for v in &model.views {
            out.push(Command {
                id: v.id.clone(),
                title: v.title.clone(),
                kind: CommandKind::ShowView(v.view.clone()),
                entry_kind: EntryKind::View,
                decoration: v.decoration.clone(),
                subtitle: view_description(&v.view).map(str::to_string),
                shortcut: None,
                hidden_until_query: false,
            });
        }

        // Zen toggle. Label flips with current state.
        out.push(Command {
            id: "action:toggle-zen".to_string(),
            title: if model.zen_on {
                "Turn Zen Mode off".to_string()
            } else {
                "Turn Zen Mode on".to_string()
            },
            kind: CommandKind::ToggleZen,
            entry_kind: EntryKind::Action,
            decoration: None,
            subtitle: Some("Shelbi does the human parts of your workflow".to_string()),
            shortcut: model.zen_shortcut.clone(),
            hidden_until_query: false,
        });

        // Workspaces. Hidden until the user types: the default (empty-query)
        // list stays commands + projects, and a workspace only surfaces once a
        // query matches its name (e.g. `alp` → `alpha`).
        for w in &model.workspaces {
            out.push(Command {
                id: format!("workspace:{}", w.name),
                title: w.name.clone(),
                kind: CommandKind::FocusWorkspace {
                    workspace: w.name.clone(),
                },
                entry_kind: EntryKind::View,
                decoration: w.decoration.clone(),
                subtitle: w.subtitle.clone(),
                shortcut: None,
                hidden_until_query: true,
            });
        }

        // Review tasks.
        for r in &model.reviews {
            out.push(Command {
                id: format!("review:{}", r.task_id),
                title: r.title.clone(),
                kind: CommandKind::LoadReview {
                    task_id: r.task_id.clone(),
                },
                entry_kind: EntryKind::View,
                decoration: r.decoration.clone(),
                subtitle: r.subtitle.clone(),
                shortcut: None,
                hidden_until_query: false,
            });
        }

        // Review decisions, one pair per review task. Hidden until the user
        // types (the review rows above are the everyday entries): `appr` or
        // the task's title surfaces them. Approve runs straight away (it has no
        // confirmation in either client); reject needs a reason first, so the
        // registry carries an empty one and the host opens the reason prompt.
        for r in &model.reviews {
            out.push(Command {
                id: format!("action:approve-review:{}", r.task_id),
                title: format!("Approve {}", r.title),
                kind: CommandKind::ApproveReview {
                    id: r.task_id.clone(),
                },
                entry_kind: EntryKind::Action,
                decoration: None,
                subtitle: Some("Accept the review and merge it".to_string()),
                shortcut: None,
                hidden_until_query: true,
            });
            out.push(Command {
                id: format!("action:reject-review:{}", r.task_id),
                title: format!("Reject {}", r.title),
                kind: CommandKind::RejectReview {
                    id: r.task_id.clone(),
                    reason: String::new(),
                },
                entry_kind: EntryKind::Action,
                decoration: None,
                subtitle: Some("Send it back with a reason".to_string()),
                shortcut: None,
                hidden_until_query: true,
            });
        }

        // Error log.
        out.push(Command {
            id: "action:error-log".to_string(),
            title: "Open error log".to_string(),
            kind: CommandKind::OpenErrorLog,
            entry_kind: EntryKind::Action,
            decoration: Some(Decoration {
                glyph: "📜".into(),
                color: DecorationColor::Default,
            }),
            subtitle: Some("Review recent errors and warnings".to_string()),
            shortcut: None,
            hidden_until_query: false,
        });

        // Settings: always offered (its Global pages need no project). The
        // Open in editor commands below stay as the advanced path.
        out.push(Command {
            id: "action:settings".to_string(),
            title: "Settings".to_string(),
            kind: CommandKind::OpenSettings,
            entry_kind: EntryKind::Action,
            decoration: Some(Decoration {
                glyph: "⚙️".into(),
                color: DecorationColor::Default,
            }),
            subtitle: Some("Change how Shelbi looks and works".to_string()),
            shortcut: None,
            hidden_until_query: false,
        });

        // Send Feedback: always offered, like Settings beside it in the
        // sidebar footer. The host opens Contact Us.
        out.push(Command {
            id: "action:send-feedback".to_string(),
            title: "Send Feedback".to_string(),
            kind: CommandKind::SendFeedback,
            entry_kind: EntryKind::Action,
            decoration: Some(Decoration {
                glyph: "📣".into(),
                color: DecorationColor::Default,
            }),
            subtitle: Some("Contact Us about a bug, a feature, or anything else".to_string()),
            shortcut: None,
            hidden_until_query: false,
        });

        // Switch-project: the generic entry plus one per other project.
        // Hidden until the user types, matching the palette.
        out.push(Command {
            id: "action:switch-project".to_string(),
            title: "Switch Project".to_string(),
            kind: CommandKind::SwitchProject {
                project: String::new(),
            },
            entry_kind: EntryKind::Action,
            decoration: None,
            subtitle: Some("Jump to another project".to_string()),
            shortcut: None,
            hidden_until_query: true,
        });
        for p in &model.other_projects {
            out.push(Command {
                id: format!("action:switch-project:{}", p.slug),
                title: format!("Switch to {} project", p.label),
                kind: CommandKind::SwitchProject {
                    project: p.slug.clone(),
                },
                entry_kind: EntryKind::Action,
                decoration: None,
                subtitle: Some(format!("Open the {} project", p.label)),
                shortcut: None,
                hidden_until_query: true,
            });
        }

        // Add project (hidden until query).
        out.push(Command {
            id: "action:add-project".to_string(),
            title: "Add project".to_string(),
            kind: CommandKind::AddProject,
            entry_kind: EntryKind::Action,
            decoration: None,
            subtitle: Some("Register a new project with Shelbi".to_string()),
            shortcut: None,
            hidden_until_query: true,
        });

        // New window (hidden until query), only for a client that has them.
        if model.multi_window {
            out.push(Command {
                id: "action:new-window".to_string(),
                title: "New Window".to_string(),
                kind: CommandKind::NewWindow,
                entry_kind: EntryKind::Action,
                decoration: None,
                subtitle: Some("Open another window on this project".to_string()),
                shortcut: None,
                hidden_until_query: true,
            });
        }

        // Edit targets (hidden until query).
        for e in &model.edit_targets {
            out.push(Command {
                id: edit_id(&e.target),
                title: e.title.clone(),
                kind: CommandKind::OpenEditor {
                    target: e.target.clone(),
                },
                entry_kind: EntryKind::Action,
                decoration: None,
                subtitle: None,
                shortcut: None,
                hidden_until_query: true,
            });
        }

        // Detach / quit actions, structurally last. The three subtitles draw the
        // line the user needs: detach keeps everything running; quit stops it.

        // Detach this client — the same client-local stop `q` performs.
        out.push(Command {
            id: "action:detach".to_string(),
            title: "Detach".to_string(),
            kind: CommandKind::Detach,
            entry_kind: EntryKind::Action,
            decoration: Some(Decoration {
                glyph: "🛸".into(),
                color: DecorationColor::Default,
            }),
            subtitle: Some("Leave Shelbi running in the background".to_string()),
            shortcut: None,
            hidden_until_query: false,
        });
        // Detach every other attached client — only when there is one.
        if model.other_clients_attached {
            out.push(Command {
                id: "action:detach-others".to_string(),
                title: "Detach Other Clients".to_string(),
                kind: CommandKind::DetachOtherClients,
                entry_kind: EntryKind::Action,
                decoration: None,
                subtitle: Some(
                    "Disconnect every other window attached to this session".to_string(),
                ),
                shortcut: None,
                hidden_until_query: false,
            });
        }

        // Quit actions — only when a project is open for the project-scoped one.
        if model.project.is_some() {
            out.push(Command {
                id: "action:quit-project".to_string(),
                title: "Quit Project".to_string(),
                kind: CommandKind::QuitProject,
                entry_kind: EntryKind::Action,
                decoration: Some(Decoration {
                    glyph: "🏁".into(),
                    color: DecorationColor::Default,
                }),
                subtitle: Some("Stop this project's agents and close it".to_string()),
                shortcut: None,
                hidden_until_query: false,
            });
        }
        out.push(Command {
            id: "action:quit-shelbi".to_string(),
            title: "Quit Shelbi".to_string(),
            kind: CommandKind::QuitShelbi,
            entry_kind: EntryKind::Action,
            decoration: Some(Decoration {
                glyph: "🏎️".into(),
                color: DecorationColor::Default,
            }),
            subtitle: Some("Stop every agent and shut Shelbi down".to_string()),
            shortcut: None,
            hidden_until_query: false,
        });

        out
    }

    /// The available commands as palette [`Entry`] values — what feeds
    /// [`shelbi_palette::search`]. This is how the palette's entry list is
    /// driven from the registry instead of its own `build_entries`.
    pub fn entries(&self, model: &CommandModel) -> Vec<Entry> {
        self.commands(model).iter().map(Command::to_entry).collect()
    }

    /// Resolve a palette entry `id` back to its [`Command`] for the given
    /// model. Returns `None` when no available command has that id (the
    /// entry referenced state that has since vanished).
    pub fn resolve(&self, id: &str, model: &CommandModel) -> Option<Command> {
        self.commands(model).into_iter().find(|c| c.id == id)
    }
}

impl CommandKind {
    /// Parse a palette entry `id` back into a typed [`CommandKind`] — the
    /// inverse of the registry's id scheme. This is what lets a palette's
    /// dispatch read the registry instead of its own `strip_prefix` ladder:
    /// an id string becomes a typed command with its arguments recovered,
    /// and [`Command::effect`] turns that into the effect to run.
    ///
    /// Returns `None` for an unrecognized id. Note the order of the checks:
    /// the longer `action:switch-project:<slug>` and `edit:agent:<name>`
    /// prefixes are tried before their bare forms.
    pub fn from_id(id: &str) -> Option<CommandKind> {
        if let Some(name) = id.strip_prefix("view:") {
            return Some(CommandKind::ShowView(View::from_view_id(name)));
        }
        if let Some(name) = id.strip_prefix("workspace:") {
            return Some(CommandKind::FocusWorkspace {
                workspace: name.to_string(),
            });
        }
        if let Some(task_id) = id.strip_prefix("review:") {
            return Some(CommandKind::LoadReview {
                task_id: task_id.to_string(),
            });
        }
        if let Some(slug) = id.strip_prefix("action:switch-project:") {
            return Some(CommandKind::SwitchProject {
                project: slug.to_string(),
            });
        }
        if let Some(task) = id.strip_prefix("action:approve-review:") {
            return Some(CommandKind::ApproveReview {
                id: task.to_string(),
            });
        }
        if let Some(task) = id.strip_prefix("action:reject-review:") {
            return Some(CommandKind::RejectReview {
                id: task.to_string(),
                reason: String::new(),
            });
        }
        if let Some(agent) = id.strip_prefix("edit:agent:") {
            return Some(CommandKind::OpenEditor {
                target: EditTarget::Agent(agent.to_string()),
            });
        }
        match id {
            "action:toggle-zen" => Some(CommandKind::ToggleZen),
            "action:error-log" => Some(CommandKind::OpenErrorLog),
            "action:settings" => Some(CommandKind::OpenSettings),
            "action:send-feedback" => Some(CommandKind::SendFeedback),
            "action:switch-project" => Some(CommandKind::SwitchProject {
                project: String::new(),
            }),
            "action:add-project" => Some(CommandKind::AddProject),
            "action:new-window" => Some(CommandKind::NewWindow),
            "action:detach" => Some(CommandKind::Detach),
            "action:detach-others" => Some(CommandKind::DetachOtherClients),
            "action:quit-project" => Some(CommandKind::QuitProject),
            "action:quit-shelbi" => Some(CommandKind::QuitShelbi),
            "edit:project" => Some(CommandKind::OpenEditor {
                target: EditTarget::Project,
            }),
            "edit:zenmode" => Some(CommandKind::OpenEditor {
                target: EditTarget::ZenMode,
            }),
            "edit:workflows" => Some(CommandKind::OpenEditor {
                target: EditTarget::Workflows,
            }),
            _ => None,
        }
    }
}

/// The palette id for an edit target, preserving the historical scheme.
fn edit_id(target: &EditTarget) -> String {
    match target {
        EditTarget::Project => "edit:project".to_string(),
        EditTarget::Agent(a) => format!("edit:agent:{a}"),
        EditTarget::ZenMode => "edit:zenmode".to_string(),
        EditTarget::Workflows => "edit:workflows".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{Effect, Mutation};

    fn model() -> CommandModel {
        CommandModel {
            project: Some("alpha".into()),
            zen_on: false,
            zen_shortcut: Some("⌥Z".into()),
            views: vec![
                ViewItem {
                    id: "view:orch".into(),
                    title: "Chat".into(),
                    view: View::Session("orch".into()),
                    decoration: None,
                },
                ViewItem {
                    id: "view:tasks".into(),
                    title: "Issues".into(),
                    view: View::Issues,
                    decoration: None,
                },
                ViewItem {
                    id: "view:activity".into(),
                    title: "Activity".into(),
                    view: View::Activity,
                    decoration: None,
                },
            ],
            workspaces: vec![WorkspaceItem {
                name: "alpha-1".into(),
                subtitle: Some("developer".into()),
                decoration: None,
            }],
            reviews: vec![ReviewItem {
                task_id: "T-12".into(),
                title: "Fix the thing".into(),
                subtitle: Some("local:9222".into()),
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
            ],
            // Default the fixture to "another client attached" so both detach
            // commands are present (and covered by the round-trip test); the
            // gating is exercised separately below.
            other_clients_attached: true,
            multi_window: true,
        }
    }

    #[test]
    fn registry_holds_every_palette_command() {
        let reg = CommandRegistry::new();
        let ids: Vec<String> = reg.commands(&model()).iter().map(|c| c.id().into()).collect();
        for want in [
            "view:orch",
            "view:tasks",
            "view:activity",
            "action:toggle-zen",
            "workspace:alpha-1",
            "review:T-12",
            "action:approve-review:T-12",
            "action:reject-review:T-12",
            "action:error-log",
            "action:settings",
            "action:send-feedback",
            "action:switch-project",
            "action:switch-project:beta",
            "action:add-project",
            "action:new-window",
            "edit:project",
            "edit:agent:developer",
            "action:detach",
            "action:detach-others",
            "action:quit-project",
            "action:quit-shelbi",
        ] {
            assert!(ids.contains(&want.to_string()), "missing command id {want}");
        }
    }

    #[test]
    fn detach_and_quit_actions_carry_their_emoji_glyphs() {
        // The four action rows show the same twemoji-style icons the nav views
        // do: a scroll, a flying saucer, a chequered flag, and a racing car.
        // Their color is `Default` so the emoji renders in its own colors.
        let reg = CommandRegistry::new();
        let cmds = reg.commands(&model());
        let glyph = |id: &str| -> Decoration {
            cmds.iter()
                .find(|c| c.id() == id)
                .unwrap_or_else(|| panic!("missing command {id}"))
                .decoration
                .clone()
                .unwrap_or_else(|| panic!("command {id} has no decoration"))
        };
        for (id, want) in [
            ("action:error-log", "📜"),
            ("action:send-feedback", "📣"),
            ("action:detach", "🛸"),
            ("action:quit-project", "🏁"),
            ("action:quit-shelbi", "🏎️"),
        ] {
            let dec = glyph(id);
            assert_eq!(dec.glyph, want, "glyph for {id}");
            assert_eq!(dec.color, DecorationColor::Default, "color for {id}");
        }

        // "Detach Other Clients" is deliberately left without an icon.
        assert!(
            glyph_is_none(&cmds, "action:detach-others"),
            "detach-others stays undecorated"
        );
    }

    fn glyph_is_none(cmds: &[Command], id: &str) -> bool {
        cmds.iter()
            .find(|c| c.id() == id)
            .map(|c| c.decoration.is_none())
            .unwrap_or(false)
    }

    #[test]
    fn new_window_is_offered_only_by_a_multi_window_client() {
        let reg = CommandRegistry::new();
        let mut m = model();
        m.multi_window = true;
        let cmd = reg.resolve("action:new-window", &m).expect("offered");
        assert_eq!(cmd.kind(), &CommandKind::NewWindow);
        assert_eq!(
            cmd.effect("alpha"),
            Effect::NewWindow {
                project: "alpha".into()
            }
        );
        assert_eq!(
            CommandKind::from_id("action:new-window"),
            Some(CommandKind::NewWindow)
        );
        // The TUI is one terminal: it has no window to open.
        m.multi_window = false;
        assert!(reg.resolve("action:new-window", &m).is_none());
    }

    #[test]
    fn detach_is_always_offered_and_detach_others_only_when_others_are_attached() {
        // Detach (keep running) is a sibling of Quit Shelbi — always available.
        // Detach Other Clients is shown only when another client is attached.
        let reg = CommandRegistry::new();
        let mut m = model();

        m.other_clients_attached = true;
        let ids: Vec<String> = reg.commands(&m).iter().map(|c| c.id().into()).collect();
        assert!(ids.contains(&"action:detach".to_string()));
        assert!(ids.contains(&"action:detach-others".to_string()));

        m.other_clients_attached = false;
        let ids: Vec<String> = reg.commands(&m).iter().map(|c| c.id().into()).collect();
        assert!(ids.contains(&"action:detach".to_string()), "detach stays");
        assert!(
            !ids.contains(&"action:detach-others".to_string()),
            "detach-others hidden when this is the only client"
        );
    }

    #[test]
    fn detach_effects_are_client_local_and_daemon_detach() {
        let reg = CommandRegistry::new();
        let m = model();
        assert_eq!(reg.resolve("action:detach", &m).unwrap().effect("alpha"), Effect::Detach);
        assert_eq!(
            reg.resolve("action:detach-others", &m).unwrap().effect("alpha"),
            Effect::DetachOtherClients
        );
    }

    #[test]
    fn zen_toggle_label_flips_with_state() {
        let reg = CommandRegistry::new();
        let mut m = model();
        m.zen_on = false;
        assert_eq!(
            reg.resolve("action:toggle-zen", &m).unwrap().title(),
            "Turn Zen Mode on"
        );
        m.zen_on = true;
        assert_eq!(
            reg.resolve("action:toggle-zen", &m).unwrap().title(),
            "Turn Zen Mode off"
        );
    }

    #[test]
    fn typed_args_survive_the_round_trip_through_entries() {
        let reg = CommandRegistry::new();
        let m = model();
        // The review entry id carries the task id; resolve recovers the
        // typed LoadReview arg.
        let entries = reg.entries(&m);
        let review = entries.iter().find(|e| e.id == "review:T-12").unwrap();
        let cmd = reg.resolve(&review.id, &m).unwrap();
        assert_eq!(
            cmd.kind(),
            &CommandKind::LoadReview {
                task_id: "T-12".into()
            }
        );
    }

    #[test]
    fn prioritize_issue_is_a_daemon_mutation_with_the_slot() {
        let kind = CommandKind::PrioritizeIssue {
            id: "t1".into(),
            position: 2,
        };
        assert_eq!(
            kind.effect_with_project("alpha"),
            Effect::Mutate(Mutation::PrioritizeIssue {
                project: "alpha".into(),
                id: "t1".into(),
                position: 2,
            })
        );
        let cmd = Command {
            id: String::new(),
            title: String::new(),
            kind,
            entry_kind: EntryKind::Action,
            decoration: None,
            subtitle: None,
            shortcut: None,
            hidden_until_query: false,
        };
        assert!(cmd.is_mutating());
    }

    #[test]
    fn effects_are_built_with_the_current_project() {
        let reg = CommandRegistry::new();
        let m = model();
        let p = m.project.clone().unwrap();

        let zen = reg.resolve("action:toggle-zen", &m).unwrap();
        assert!(zen.is_mutating());
        assert_eq!(
            zen.effect(&p),
            Effect::Mutate(Mutation::ToggleZen {
                project: "alpha".into()
            })
        );

        let ws = reg.resolve("workspace:alpha-1", &m).unwrap();
        assert!(!ws.is_mutating());
        assert_eq!(
            ws.effect(&p),
            Effect::FocusWorkspace {
                project: "alpha".into(),
                workspace: "alpha-1".into()
            }
        );

        // Switch-project carries its own target, not the current project.
        let switch = reg.resolve("action:switch-project:beta", &m).unwrap();
        assert_eq!(
            switch.effect(&p),
            Effect::SwitchProject {
                project: "beta".into()
            }
        );

        // A view command maps straight to a navigation effect.
        let tasks = reg.resolve("view:tasks", &m).unwrap();
        assert_eq!(tasks.effect(&p), Effect::ShowView(View::Issues));
        let chat = reg.resolve("view:orch", &m).unwrap();
        assert_eq!(chat.effect(&p), Effect::ShowView(View::Session("orch".into())));
    }

    #[test]
    fn edit_targets_map_to_historical_ids_and_recover() {
        let reg = CommandRegistry::new();
        let m = model();
        let cmd = reg.resolve("edit:agent:developer", &m).unwrap();
        assert_eq!(
            cmd.effect("alpha"),
            Effect::OpenEditor {
                target: EditTarget::Agent("developer".into())
            }
        );
    }

    #[test]
    fn hidden_until_query_matches_the_palette_tiers() {
        let reg = CommandRegistry::new();
        let m = model();
        let cmds = reg.commands(&m);
        let hidden = |id: &str| cmds.iter().find(|c| c.id() == id).unwrap().hidden_until_query();
        // Power-user shortcuts are hidden until a query is typed. Workspaces
        // join this tier: the empty-query list stays commands + projects, and a
        // workspace only surfaces once a query matches its name.
        assert!(hidden("action:switch-project"));
        assert!(hidden("action:switch-project:beta"));
        assert!(hidden("action:add-project"));
        assert!(hidden("edit:project"));
        assert!(hidden("workspace:alpha-1"));
        // Everyday entries stay visible on a blank query.
        assert!(!hidden("view:tasks"));
        assert!(!hidden("action:toggle-zen"));
        assert!(!hidden("review:T-12"));
        assert!(!hidden("action:quit-shelbi"));
        assert!(!hidden("action:settings"));
        assert!(!hidden("action:send-feedback"));
    }

    #[test]
    fn settings_is_offered_with_or_without_a_project() {
        let reg = CommandRegistry::new();
        let mut m = model();
        let cmd = reg.resolve("action:settings", &m).expect("offered");
        assert_eq!(cmd.kind(), &CommandKind::OpenSettings);
        assert!(!cmd.is_mutating());
        assert_eq!(cmd.effect("alpha"), Effect::OpenSettings);
        m.project = None;
        assert!(reg.resolve("action:settings", &m).is_some(), "its Global pages need no project");
        let entries = reg.entries(&model());
        let hits = shelbi_palette::search(&entries, "settings");
        assert_eq!(hits[0].0.id, "action:settings", "it outranks the Edit … Settings openers");
    }

    #[test]
    fn send_feedback_is_offered_with_or_without_a_project() {
        let reg = CommandRegistry::new();
        let mut m = model();
        let cmd = reg.resolve("action:send-feedback", &m).expect("offered");
        assert_eq!(cmd.title(), "Send Feedback", "the desktop's label");
        assert_eq!(cmd.kind(), &CommandKind::SendFeedback);
        assert_eq!(CommandKind::from_id("action:send-feedback"), Some(CommandKind::SendFeedback));
        assert!(!cmd.is_mutating(), "the dialog sends on its own");
        assert_eq!(cmd.effect("alpha"), Effect::SendFeedback);
        m.project = None;
        assert!(reg.resolve("action:send-feedback", &m).is_some(), "it needs no project");
        let entries = reg.entries(&model());
        let hits = shelbi_palette::search(&entries, "feedback");
        assert_eq!(hits[0].0.id, "action:send-feedback");
    }

    #[test]
    fn empty_query_excludes_workspaces_but_typing_surfaces_them() {
        // End-to-end through the matcher the palette uses: a blank query leaves
        // the workspace out of the default list, and typing a prefix of its
        // name brings it back, scored like any other entry.
        let reg = CommandRegistry::new();
        let entries = reg.entries(&model());

        let blank = shelbi_palette::search(&entries, "");
        assert!(
            blank.iter().all(|(e, _)| e.id != "workspace:alpha-1"),
            "a blank query must not list workspaces"
        );
        // ...while the everyday commands and reviews still show.
        assert!(blank.iter().any(|(e, _)| e.id == "view:tasks"));
        assert!(blank.iter().any(|(e, _)| e.id == "review:T-12"));

        let typed = shelbi_palette::search(&entries, "alph");
        assert!(
            typed.iter().any(|(e, _)| e.id == "workspace:alpha-1"),
            "typing a workspace-name prefix must surface the workspace"
        );
    }

    #[test]
    fn quit_project_absent_without_a_selected_project() {
        let reg = CommandRegistry::new();
        let mut m = model();
        m.project = None;
        let cmds = reg.commands(&m);
        let ids: Vec<&str> = cmds.iter().map(|c| c.id()).collect();
        assert!(!ids.contains(&"action:quit-project"));
        // Quit Shelbi is always offered.
        assert!(ids.contains(&"action:quit-shelbi"));
    }

    #[test]
    fn from_id_recovers_every_registry_command_kind() {
        // For every command the registry emits, parsing its id back must
        // reproduce the same typed kind — this is the registry-driven
        // replacement for the palette's `strip_prefix` dispatch ladder. The
        // bare `action:switch-project` entry carries an empty target, which
        // matches the registry's placeholder.
        let reg = CommandRegistry::new();
        for cmd in reg.commands(&model()) {
            let parsed = CommandKind::from_id(cmd.id())
                .unwrap_or_else(|| panic!("from_id failed for {}", cmd.id()));
            assert_eq!(&parsed, cmd.kind(), "kind mismatch for id {}", cmd.id());
        }
    }

    #[test]
    fn review_decisions_are_offered_per_review_and_hidden_until_typed() {
        let reg = CommandRegistry::new();
        let m = model();
        let approve = reg.resolve("action:approve-review:T-12", &m).unwrap();
        assert_eq!(approve.title(), "Approve Fix the thing");
        assert!(approve.hidden_until_query());
        assert!(approve.is_mutating());
        assert_eq!(
            approve.effect("alpha"),
            Effect::Mutate(Mutation::ApproveReview {
                project: "alpha".into(),
                id: "T-12".into()
            })
        );
        let reject = reg.resolve("action:reject-review:T-12", &m).unwrap();
        assert_eq!(reject.title(), "Reject Fix the thing");
        // The reason is collected by the host's prompt, not the registry.
        assert_eq!(
            reject.kind(),
            &CommandKind::RejectReview {
                id: "T-12".into(),
                reason: String::new()
            }
        );
        // Typing surfaces them; a blank query doesn't.
        let entries = reg.entries(&m);
        let blank = shelbi_palette::search(&entries, "");
        assert!(blank.iter().all(|(e, _)| !e.id.starts_with("action:approve-review")));
        let typed = shelbi_palette::search(&entries, "reject fix");
        assert_eq!(typed[0].0.id, "action:reject-review:T-12");
    }

    #[test]
    fn from_id_rejects_unknown_ids() {
        assert!(CommandKind::from_id("not-a-command").is_none());
        assert!(CommandKind::from_id("bogus:thing").is_none());
    }

    #[test]
    fn resolve_returns_none_for_vanished_state() {
        let reg = CommandRegistry::new();
        let m = model();
        // A review that is no longer on the board can't be resolved.
        assert!(reg.resolve("review:T-999", &m).is_none());
    }

    #[test]
    fn entries_fuzzy_search_through_the_registry() {
        // End-to-end: the palette would feed registry entries to
        // `shelbi_palette::search`. A multi-word query hits the title.
        let reg = CommandRegistry::new();
        let entries = reg.entries(&model());
        let hits = shelbi_palette::search(&entries, "zen");
        assert!(hits.iter().any(|(e, _)| e.id == "action:toggle-zen"));
    }
}
