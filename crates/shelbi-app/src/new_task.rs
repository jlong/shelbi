//! The New task dialog, toolkit-free (`new-task-dialog`, Figma `2730:10845`):
//! the draft (title, workflow, status, priority, description, the write-up
//! switch), where the keyboard is, which picker is open, what Return / Esc /
//! Tab do, the validation, the create request, and the orchestrator
//! hand-off after it. The desktop's `views::new_task` and the TUI's
//! `overlay::new_task` paint [`NewTaskForm::model`] and run [`create`] off
//! their UI thread; both show the words below.
//!
//! The controls, in visual (and Tab) order: Title, Workflow, Status,
//! Priority, Description, the "Let Orchestrator write this up" switch,
//! Cancel, Create Task.
//!
//! - **Workflow** lists the project's workflows, the project's
//!   `default_workflow` first chosen.
//! - **Status** lists the chosen workflow's statuses with their category
//!   (each client draws its glyph), the workflow's initial status first
//!   chosen. Changing the workflow keeps the status when the new workflow
//!   declares it, else resets it to the new workflow's initial status.
//! - **Priority** is where the card lands in its status's queue: the end
//!   (the default, where an add always landed) or the top.
//! - **Create Task** is disabled while the title is blank.
//!
//! Creating is one [`CommandKind::AddIssue`], which the registry turns into
//! a [`Mutation::AddIssue`] for the daemon. With the switch on, the
//! orchestrator is then sent [`write_up_message`] for the new id through
//! its session (the way a client types into it); when it isn't running the
//! issue is still created and [`Created::message`] says the write-up wasn't
//! requested.

use shelbi_core::{StatusCategory, Workflow};

use crate::exec::{ExecError, Mutation};
use crate::view::{title_case, NewTaskModel, NewTaskOption};
use crate::CommandKind;

pub use crate::view::NewTaskField as Field;

/// The dialog's title and the line under it.
pub const TITLE: &str = "New task";
pub const SUBTITLE: &str = "Add work for your Orchestrator and agents.";
/// The fields' labels.
pub const TITLE_LABEL: &str = "Title";
pub const WORKFLOW_LABEL: &str = "Workflow";
pub const STATUS_LABEL: &str = "Status";
pub const PRIORITY_LABEL: &str = "Priority";
pub const DESCRIPTION_LABEL: &str = "Description";
/// The fields' placeholders while they're empty.
pub const TITLE_PLACEHOLDER: &str = "What needs doing?";
pub const DESCRIPTION_PLACEHOLDER: &str = "Describe the work. Markdown works here.";
/// The write-up switch's label and the line under it.
pub const WRITE_UP: &str = "Let Orchestrator write this up";
pub const WRITE_UP_HELP: &str = "Expand this draft into a complete task with acceptance criteria.";
/// The footer's hint, per platform convention: ⌘ on the desktop, Ctrl in a
/// terminal (which rarely sees ⌘).
pub const CREATE_HINT: &str = "\u{2318} Return to create";
pub const CREATE_HINT_TERMINAL: &str = "Ctrl+Enter to create";
/// The buttons, and Create Task while the create is out.
pub const CANCEL: &str = "Cancel";
pub const CREATE: &str = "Create Task";
pub const CREATING: &str = "Creating\u{2026}";

/// Where the new card lands in its status's queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Top,
    End,
}

impl Position {
    /// In the Priority picker's order.
    pub const ALL: [Position; 2] = [Position::Top, Position::End];

    pub fn label(self) -> &'static str {
        match self {
            Position::Top => "Top of queue",
            Position::End => "End of queue",
        }
    }
}

/// A status the Status picker lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChoice {
    pub id: String,
    /// As the workflow spells it (`In Progress`).
    pub name: String,
    pub category: StatusCategory,
}

/// A workflow the Workflow picker lists, with its statuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowChoice {
    pub name: String,
    pub statuses: Vec<StatusChoice>,
    /// The index into `statuses` an issue starts in.
    pub initial: usize,
}

impl WorkflowChoice {
    fn from_workflow(wf: &Workflow) -> Self {
        let statuses: Vec<StatusChoice> = wf
            .statuses
            .iter()
            .map(|s| StatusChoice {
                id: s.id.clone(),
                name: s.name.clone(),
                category: s.category,
            })
            .collect();
        let initial = wf
            .resolved_initial_status()
            .and_then(|id| statuses.iter().position(|s| s.id == id))
            .unwrap_or(0);
        WorkflowChoice {
            name: wf.name.clone(),
            statuses,
            initial,
        }
    }

    /// The picker's label (`task` → `Task`).
    pub fn label(&self) -> String {
        title_case(&self.name)
    }
}

/// The picker that's open.
pub use crate::view::NewTaskMenu as Menu;

/// A key the dialog handles. The view decides which keys reach it (Return
/// in the Description and the text fields' editing keys are the field's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Tab,
    BackTab,
    /// Return: the focused control's action; on Title, Create Task.
    Enter,
    /// ⌘Return / Ctrl+Return: Create Task from anywhere.
    CmdEnter,
    Esc,
    Space,
    Up,
    Down,
}

/// What the view does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Run this off the UI thread ([`create`]) and hand the answer to
    /// [`NewTaskForm::finished`].
    Create(CreateRequest),
    /// Close the dialog: cancelled (nothing created).
    Close,
}

/// The create: one [`CommandKind::AddIssue`], and whether to ask the
/// orchestrator to write the new issue up afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    pub command: CommandKind,
    pub write_up: bool,
}

impl CreateRequest {
    /// The daemon mutation the command becomes in `project`, through the
    /// registry's own mapping.
    pub fn mutation(&self, project: &str) -> Mutation {
        match self.command.effect_with_project(project) {
            crate::exec::Effect::Mutate(m) => m,
            other => unreachable!("AddIssue maps to a mutation, not {other:?}"),
        }
    }

    /// The title it creates, for messages.
    pub fn title(&self) -> &str {
        match &self.command {
            CommandKind::AddIssue { title, .. } => title,
            _ => "",
        }
    }
}

/// The dialog's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTaskForm {
    workflows: Vec<WorkflowChoice>,
    /// The project's default workflow's index into `workflows`.
    default_workflow: usize,
    title: String,
    description: String,
    workflow: usize,
    status: usize,
    position: Position,
    write_up: bool,
    focus: Field,
    /// The open picker and the row its keyboard cursor is on.
    menu: Option<(Menu, usize)>,
    creating: bool,
    error: Option<String>,
}

impl NewTaskForm {
    /// A blank draft over `workflows` (the board's loaded ones), on
    /// `default_workflow` (the project's `default_workflow_name`), in its
    /// initial status, at the end of the queue, the switch off, the keyboard
    /// on Title. With no workflows loaded the shipped default stands in.
    pub fn new(workflows: &[Workflow], default_workflow: &str) -> Self {
        let mut choices: Vec<WorkflowChoice> = workflows.iter().map(WorkflowChoice::from_workflow).collect();
        if choices.is_empty() {
            choices.push(WorkflowChoice::from_workflow(&shelbi_core::default_workflow()));
        }
        let default = choices.iter().position(|w| w.name == default_workflow).unwrap_or(0);
        let status = choices[default].initial;
        NewTaskForm {
            workflows: choices,
            default_workflow: default,
            title: String::new(),
            description: String::new(),
            workflow: default,
            status,
            position: Position::End,
            write_up: false,
            focus: Field::Title,
            menu: None,
            creating: false,
            error: None,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn workflows(&self) -> &[WorkflowChoice] {
        &self.workflows
    }

    pub fn workflow(&self) -> &WorkflowChoice {
        &self.workflows[self.workflow]
    }

    pub fn status(&self) -> &StatusChoice {
        &self.workflow().statuses[self.status]
    }

    pub fn position(&self) -> Position {
        self.position
    }

    pub fn write_up(&self) -> bool {
        self.write_up
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    pub fn menu(&self) -> Option<(Menu, usize)> {
        self.menu
    }

    pub fn creating(&self) -> bool {
        self.creating
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Create Task can be pressed: a title, and no create already out.
    pub fn can_create(&self) -> bool {
        !self.creating && !self.title.trim().is_empty()
    }

    /// What a client draws.
    pub fn model(&self) -> NewTaskModel {
        let option = |label: String, category: Option<StatusCategory>, selected: bool| NewTaskOption {
            label,
            category,
            selected,
        };
        NewTaskModel {
            title: self.title.clone(),
            workflow: self.workflow().label(),
            workflows: self
                .workflows
                .iter()
                .enumerate()
                .map(|(i, w)| option(w.label(), None, i == self.workflow))
                .collect(),
            status: self.status().name.clone(),
            status_category: self.status().category,
            statuses: self
                .workflow()
                .statuses
                .iter()
                .enumerate()
                .map(|(i, s)| option(s.name.clone(), Some(s.category), i == self.status))
                .collect(),
            priority: self.position.label().to_string(),
            priorities: Position::ALL
                .iter()
                .map(|p| option(p.label().to_string(), None, *p == self.position))
                .collect(),
            description: self.description.clone(),
            write_up: self.write_up,
            focus: self.focus,
            menu: self.menu,
            can_create: self.can_create(),
            creating: self.creating,
            error: self.error.clone(),
        }
    }

    // ---- edits ------------------------------------------------------------

    /// Editing a field clears the error shown.
    pub fn set_title(&mut self, text: &str) {
        if self.title != text {
            self.title = text.to_string();
            self.error = None;
        }
    }

    pub fn set_description(&mut self, text: &str) {
        if self.description != text {
            self.description = text.to_string();
            self.error = None;
        }
    }

    /// The keyboard went to `field` (a click into it). Closes a picker
    /// that isn't `field`'s.
    pub fn set_focus(&mut self, field: Field) {
        if self.creating {
            return;
        }
        self.focus = field;
        if self.menu.is_some_and(|(m, _)| m.field() != field) {
            self.menu = None;
        }
    }

    /// Pick workflow `i`: its initial status unless it declares the current
    /// status's id, which stays.
    pub fn pick_workflow(&mut self, i: usize) {
        if self.creating || i >= self.workflows.len() {
            return;
        }
        let current = self.status().id.clone();
        self.workflow = i;
        let wf = &self.workflows[i];
        self.status = wf.statuses.iter().position(|s| s.id == current).unwrap_or(wf.initial);
        self.focus = Field::Workflow;
        self.menu = None;
    }

    /// Pick status `i` of the chosen workflow.
    pub fn pick_status(&mut self, i: usize) {
        if self.creating || i >= self.workflow().statuses.len() {
            return;
        }
        self.status = i;
        self.focus = Field::Status;
        self.menu = None;
    }

    pub fn pick_position(&mut self, position: Position) {
        if self.creating {
            return;
        }
        self.position = position;
        self.focus = Field::Priority;
        self.menu = None;
    }

    /// Flip the write-up switch (a click, which takes the keyboard too).
    pub fn toggle_write_up(&mut self) {
        if self.creating {
            return;
        }
        self.write_up = !self.write_up;
        self.focus = Field::WriteUp;
        self.menu = None;
    }

    // ---- pickers ----------------------------------------------------------

    /// How many rows `menu` lists.
    pub fn menu_len(&self, menu: Menu) -> usize {
        match menu {
            Menu::Workflow => self.workflows.len(),
            Menu::Status => self.workflow().statuses.len(),
            Menu::Priority => Position::ALL.len(),
        }
    }

    /// The row `menu`'s current value is on.
    fn chosen(&self, menu: Menu) -> usize {
        match menu {
            Menu::Workflow => self.workflow,
            Menu::Status => self.status,
            Menu::Priority => Position::ALL.iter().position(|p| *p == self.position).unwrap_or(0),
        }
    }

    /// Open `menu` (closing it when it's the one open), its cursor on the
    /// current value. Its field takes the keyboard.
    pub fn toggle_menu(&mut self, menu: Menu) {
        if self.creating {
            return;
        }
        self.focus = menu.field();
        self.menu = match self.menu {
            Some((open, _)) if open == menu => None,
            _ => Some((menu, self.chosen(menu))),
        };
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Choose row `i` of `menu` (a click on it).
    pub fn choose(&mut self, menu: Menu, i: usize) {
        match menu {
            Menu::Workflow => self.pick_workflow(i),
            Menu::Status => self.pick_status(i),
            Menu::Priority => {
                if let Some(p) = Position::ALL.get(i) {
                    self.pick_position(*p);
                }
            }
        }
    }

    /// Move the open picker's cursor to row `i` (the pointer over it).
    pub fn hover_menu(&mut self, i: usize) {
        if let Some((menu, _)) = self.menu {
            if i < self.menu_len(menu) {
                self.menu = Some((menu, i));
            }
        }
    }

    // ---- keys and buttons -------------------------------------------------

    /// A click on a button: what pressing it does.
    pub fn press(&mut self, field: Field) -> Action {
        match field {
            Field::Create => self.submit(),
            Field::Cancel => self.close(),
            Field::WriteUp => {
                self.toggle_write_up();
                Action::Stay
            }
            _ => Action::Stay,
        }
    }

    /// A key.
    pub fn key(&mut self, key: Key) -> Action {
        if let Some((menu, cursor)) = self.menu {
            return self.menu_key(menu, cursor, key);
        }
        match key {
            Key::Tab | Key::BackTab => {
                if !self.creating {
                    let at = Field::ALL.iter().position(|f| *f == self.focus).unwrap_or(0);
                    let n = Field::ALL.len();
                    let next = if key == Key::Tab { (at + 1) % n } else { (at + n - 1) % n };
                    self.focus = Field::ALL[next];
                }
                Action::Stay
            }
            Key::Esc => self.close(),
            Key::CmdEnter => self.submit(),
            Key::Enter => match self.focus {
                Field::Title | Field::Create => self.submit(),
                Field::Cancel => self.close(),
                // Return is the Description's new line (the view lets it
                // through before it gets here).
                Field::Description => Action::Stay,
                Field::WriteUp => self.press(Field::WriteUp),
                f => {
                    self.open_for(f);
                    Action::Stay
                }
            },
            Key::Space => match self.focus {
                Field::Create | Field::Cancel | Field::WriteUp => self.press(self.focus),
                Field::Title | Field::Description => Action::Stay,
                f => {
                    self.open_for(f);
                    Action::Stay
                }
            },
            Key::Up | Key::Down => {
                self.open_for(self.focus);
                Action::Stay
            }
        }
    }

    /// Open the picker of a picker field (a no-op for any other).
    fn open_for(&mut self, field: Field) {
        if let Some(menu) = Menu::of(field) {
            if self.menu.is_none() {
                self.toggle_menu(menu);
            }
        }
    }

    /// A key while `menu` is open: ↑ / ↓ move, Return / Space choose, Esc
    /// closes it (not the dialog), Tab closes it and moves on, ⌘Return
    /// creates.
    fn menu_key(&mut self, menu: Menu, cursor: usize, key: Key) -> Action {
        let n = self.menu_len(menu);
        match key {
            Key::Up => self.menu = Some((menu, cursor.saturating_sub(1))),
            Key::Down => self.menu = Some((menu, (cursor + 1).min(n.saturating_sub(1)))),
            Key::Enter | Key::Space => self.choose(menu, cursor),
            Key::Esc => self.menu = None,
            Key::Tab | Key::BackTab => {
                self.menu = None;
                return self.key(key);
            }
            Key::CmdEnter => {
                self.menu = None;
                return self.submit();
            }
        }
        Action::Stay
    }

    /// Cancel / Esc: never while a create is out.
    fn close(&mut self) -> Action {
        if self.creating {
            Action::Stay
        } else {
            Action::Close
        }
    }

    /// Create Task: the request, when there's a title. A blank title keeps
    /// the dialog (the button is disabled) and puts the keyboard there.
    fn submit(&mut self) -> Action {
        if self.creating {
            return Action::Stay;
        }
        let title = self.title.trim();
        if title.is_empty() {
            self.focus = Field::Title;
            return Action::Stay;
        }
        let body = self.description.trim_end();
        let workflow = (self.workflow != self.default_workflow).then(|| self.workflow().name.clone());
        let command = CommandKind::AddIssue {
            title: title.to_string(),
            body: (!body.trim().is_empty()).then(|| body.to_string()),
            workflow,
            status: Some(self.status().id.clone()),
            top: self.position == Position::Top,
        };
        self.error = None;
        self.menu = None;
        self.creating = true;
        Action::Create(CreateRequest {
            command,
            write_up: self.write_up,
        })
    }

    /// The create came back: `Ok` closes the dialog (the client shows the
    /// outcome's message), an error stays under the form with the draft
    /// kept for another try.
    pub fn finished(&mut self, result: Result<(), String>) -> Action {
        self.creating = false;
        match result {
            Ok(()) => Action::Close,
            Err(e) => {
                self.error = Some(e);
                Action::Stay
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Creating, and the orchestrator hand-off.
// ---------------------------------------------------------------------------

/// Why the write-up request didn't reach the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffError {
    /// The project's orchestrator has no live session.
    NotRunning,
    /// It has one, but the message couldn't be sent.
    Failed(String),
}

/// The seam the hand-off goes through: send `text` into `project`'s
/// orchestrator session as a submitted message. [`SessionMessenger`] is the
/// live one; tests record.
pub trait OrchestratorMessenger {
    fn send(&self, project: &str, text: &str) -> Result<(), HandoffError>;
}

/// What became of the write-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteUp {
    /// The switch was off.
    NotAsked,
    /// The orchestrator was asked.
    Requested,
    /// It isn't running: not asked.
    NotRunning,
    /// Asking failed, or the new id wasn't reported so there was nothing to
    /// name.
    Failed(String),
}

/// A created issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The new issue's id, as the daemon reported it.
    pub id: Option<String>,
    pub title: String,
    pub write_up: WriteUp,
}

impl Created {
    /// The line a client shows (its status line or toast) once the dialog
    /// closes.
    pub fn message(&self) -> String {
        let what = match &self.id {
            Some(id) => format!("Created {id}"),
            None => format!("Created \u{201c}{}\u{201d}", self.title),
        };
        match &self.write_up {
            WriteUp::NotAsked => what,
            WriteUp::Requested => format!("{what}; the Orchestrator is writing it up"),
            WriteUp::NotRunning => {
                format!("{what}. The Orchestrator isn\u{2019}t running, so the write-up wasn\u{2019}t requested")
            }
            WriteUp::Failed(e) => format!("{what}. The write-up wasn\u{2019}t requested: {e}"),
        }
    }
}

/// What the orchestrator is sent for new issue `id`.
pub fn write_up_message(id: &str) -> String {
    format!(
        "Please write up issue {id}, which I just created from the New task dialog: rewrite its \
         body into a complete task with acceptance criteria, using this project's task \
         templates and keeping what my draft asks for. Update it with `shelbi issue edit {id}` \
         and leave its status and position as they are."
    )
}

/// Create `request` in `project`: run its mutation through `add` (the
/// daemon's add, answering with the new id), then, when asked, send the
/// orchestrator [`write_up_message`] through `messenger`. The issue is
/// created whatever the hand-off does; an error is only a failed create.
/// Blocking: run it off the UI thread.
pub fn create(
    project: &str,
    request: &CreateRequest,
    add: impl FnOnce(&Mutation) -> Result<Option<String>, ExecError>,
    messenger: &dyn OrchestratorMessenger,
) -> Result<Created, String> {
    let id = add(&request.mutation(project)).map_err(|e| format!("Couldn\u{2019}t create the task: {e}"))?;
    let write_up = if !request.write_up {
        WriteUp::NotAsked
    } else {
        match &id {
            None => WriteUp::Failed("the new issue\u{2019}s id wasn\u{2019}t reported".to_string()),
            Some(id) => match messenger.send(project, &write_up_message(id)) {
                Ok(()) => WriteUp::Requested,
                Err(HandoffError::NotRunning) => WriteUp::NotRunning,
                Err(HandoffError::Failed(e)) => WriteUp::Failed(e),
            },
        }
    };
    Ok(Created {
        id,
        title: request.title().to_string(),
        write_up,
    })
}

/// [`create`] against the live daemon and the orchestrator's session.
pub fn create_live(project: &str, request: &CreateRequest) -> Result<Created, String> {
    create(
        project,
        request,
        |m| crate::exec_daemon::add_issue(m, &mut |_, _| {}),
        &SessionMessenger,
    )
}

/// The live [`OrchestratorMessenger`]: finds `<project>/orch` among this
/// machine's sessions and sends the text the way a client's terminal view
/// does when the user types into it, a paste then Return, the Return a beat
/// later as its own key so the agent doesn't take it as part of the paste.
pub struct SessionMessenger;

/// The pause between the paste and its Return.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

impl OrchestratorMessenger for SessionMessenger {
    fn send(&self, project: &str, text: &str) -> Result<(), HandoffError> {
        let root = shelbi_state::sessions_dir().map_err(|e| HandoffError::Failed(e.to_string()))?;
        let sessions = shelbi_client::list(&root).map_err(|e| HandoffError::Failed(e.to_string()))?;
        let name = crate::session::discovery_name(project, &crate::session::SessionRef::Orchestrator);
        let Some(session) = shelbi_client::choose_session(&sessions, &name) else {
            return Err(HandoffError::NotRunning);
        };
        let (conn, _events) =
            shelbi_client::Connection::open(&session.sock, None, shelbi_proto::capability::ALL)
                .map_err(|e| HandoffError::Failed(e.to_string()))?;
        conn.paste(text).map_err(|e| HandoffError::Failed(e.to_string()))?;
        std::thread::sleep(SETTLE);
        conn.input(b"\r").map_err(|e| HandoffError::Failed(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
