//! Settings ▸ Agents' state, toolkit-free (`dt-agents-settings-page`): the
//! project's agents and shared instructions as `shelbi_state::agents_config`
//! loads them, the edits the page makes to them, and the guards that disable
//! an action up front.
//!
//! Saving works as on the Workflows page ([`super::workflows_state`]): every
//! edit is an [`Edit`] applied to the model and saved at once, one save at a
//! time off the UI thread, an edit made during a save queued behind it. An
//! edit the model refuses, or a save that fails, writes nothing; the page
//! reloads the files and shows the error where the edit was made
//! ([`Target`]).

use std::collections::VecDeque;

use shelbi_core::{RunnerKind, StatusCategory};
use shelbi_state::agents_config::{AgentSummary, AgentsConfig, Provenance};

pub use super::workflows_state::sentence;

/// A runner's name, as the table and the details page show it.
pub fn runner_label(runner: RunnerKind) -> &'static str {
    match runner {
        RunnerKind::Claude => "Claude Code",
        RunnerKind::Codex => "Codex",
        RunnerKind::Generic => "Generic",
    }
}

/// One change the page makes, applied through the model's API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    CreateAgent { name: String },
    DuplicateAgent { from: String, name: String },
    ResetAgent { name: String },
    DeleteAgent { name: String },
    SetSharedInstructions { text: String },
}

impl Edit {
    /// `config` with the edit made, or why the model refused it.
    pub fn apply(&self, config: &AgentsConfig) -> Result<AgentsConfig, String> {
        match self {
            Edit::CreateAgent { name } => config.create_agent(name, None),
            Edit::DuplicateAgent { from, name } => config.duplicate_agent(from, name),
            Edit::ResetAgent { name } => config.reset_to_default(name),
            Edit::DeleteAgent { name } => config.delete_agent(name),
            Edit::SetSharedInstructions { text } => config.set_shared_instructions(text),
        }
        .map_err(|e| e.to_string())
    }

    /// Where its error shows.
    pub fn target(&self) -> Target {
        match self {
            Edit::CreateAgent { .. } | Edit::DuplicateAgent { .. } => Target::NewAgent,
            Edit::ResetAgent { name } | Edit::DeleteAgent { name } => Target::Agent(name.clone()),
            Edit::SetSharedInstructions { .. } => Target::Shared,
        }
    }
}

/// Where an error shows: under the row it's about, under the table's new
/// row, or under the shared instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Agent(String),
    NewAgent,
    Shared,
    /// Loading: under the page's header.
    Page,
}

/// What one save did, handed back to the UI thread.
#[derive(Debug)]
pub struct Committed {
    pub edit: Edit,
    /// The model as now on disk (`None` if it couldn't be reloaded after a
    /// failure: the page keeps the one it had).
    pub config: Option<AgentsConfig>,
    pub error: Option<String>,
    /// Comment lines a file lost to the edit (each `file: line`).
    pub dropped_comments: Vec<String>,
}

/// Apply `edit` to `config` and save it, off the UI thread. A refusal or a
/// failed save writes nothing; the model is reloaded from disk then, so the
/// page shows the files as they are (a conflict means they changed under
/// it).
pub fn commit(project: &str, config: &AgentsConfig, edit: Edit) -> Committed {
    let saved = edit
        .apply(config)
        .and_then(|next| next.save().map_err(|e| e.to_string()));
    match saved {
        Ok(saved) => Committed {
            dropped_comments: saved
                .dropped_comments
                .iter()
                .flat_map(|(path, lines)| {
                    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    lines.iter().map(move |l| format!("{file}: {}", l.trim()))
                })
                .collect(),
            edit,
            config: Some(saved.config),
            error: None,
        },
        Err(e) => Committed {
            edit,
            config: AgentsConfig::load(project).ok(),
            error: Some(sentence(&e)),
            dropped_comments: Vec::new(),
        },
    }
}

/// A row's action that waits for the user to confirm it (it can't be
/// undone from the page).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    Reset(String),
    Delete(String),
}

impl Confirm {
    pub fn agent(&self) -> &str {
        match self {
            Confirm::Reset(name) | Confirm::Delete(name) => name,
        }
    }

    /// The edit confirming makes.
    pub fn edit(&self) -> Edit {
        match self {
            Confirm::Reset(name) => Edit::ResetAgent { name: name.clone() },
            Confirm::Delete(name) => Edit::DeleteAgent { name: name.clone() },
        }
    }
}

/// A workflow status the agent is assigned, once however many workflows
/// assign it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChip {
    pub id: String,
    pub name: String,
    pub category: Option<StatusCategory>,
    /// The workflows that assign it, in the order the model lists them.
    pub workflows: Vec<String>,
}

/// `summary`'s assigned statuses, one chip per status id, in the order
/// they're first assigned.
pub fn status_chips(summary: &AgentSummary) -> Vec<StatusChip> {
    let mut chips: Vec<StatusChip> = Vec::new();
    for a in &summary.assignments {
        match chips.iter_mut().find(|c| c.id == a.status) {
            Some(chip) => chip.workflows.push(a.workflow.clone()),
            None => chips.push(StatusChip {
                id: a.status.clone(),
                name: a.status_name.clone(),
                category: a.category,
                workflows: vec![a.workflow.clone()],
            }),
        }
    }
    chips
}

/// The page's state.
#[derive(Debug)]
pub struct AgentsState {
    project: String,
    /// `None` until the first load lands.
    config: Option<AgentsConfig>,
    /// The last error, where it shows.
    error: Option<(Target, String)>,
    /// Comments the last save dropped, shown until the next edit.
    notice: Option<String>,
    /// An edit is being saved.
    saving: bool,
    /// Edits made while one was saving, in order.
    queue: VecDeque<Edit>,
    /// The new row being typed into, if any: `Some(None)` a new agent,
    /// `Some(Some(from))` a duplicate of `from`.
    pub draft: Option<Option<String>>,
    /// The row action waiting to be confirmed.
    pub confirm: Option<Confirm>,
    /// The shared instructions are open in the editor.
    pub editing_shared: bool,
}

impl AgentsState {
    pub fn new(project: &str) -> Self {
        AgentsState {
            project: project.to_string(),
            config: None,
            error: None,
            notice: None,
            saving: false,
            queue: VecDeque::new(),
            draft: None,
            confirm: None,
            editing_shared: false,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn config(&self) -> Option<&AgentsConfig> {
        self.config.as_ref()
    }

    /// The first load (or a reload) landed.
    pub fn loaded(&mut self, config: Result<AgentsConfig, String>) {
        match config {
            Ok(config) => {
                if self.confirm.as_ref().is_some_and(|c| config.agent(c.agent()).is_none()) {
                    self.confirm = None;
                }
                self.config = Some(config);
            }
            Err(e) => self.error = Some((Target::Page, format!("Couldn't read the agents: {e}"))),
        }
    }

    /// The error shown at `target`.
    pub fn error_at(&self, target: &Target) -> Option<&str> {
        self.error.as_ref().filter(|(t, _)| t == target).map(|(_, e)| e.as_str())
    }

    /// Show `message` at `target` (an Edit YAML that couldn't open).
    pub fn set_error(&mut self, target: Target, message: String) {
        self.error = Some((target, message));
    }

    /// Drop the error shown at `target`, if that's where it is.
    pub fn clear_error(&mut self, target: &Target) {
        if self.error.as_ref().is_some_and(|(t, _)| t == target) {
            self.error = None;
        }
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn saving(&self) -> bool {
        self.saving
    }

    /// Make `edit`: the save to start now (the model to apply it to), or
    /// `None` while another save runs (it is queued and starts when that
    /// one lands) or before the model has loaded.
    pub fn request(&mut self, edit: Edit) -> Option<(AgentsConfig, Edit)> {
        self.error = None;
        self.notice = None;
        self.confirm = None;
        let config = self.config.clone()?;
        if self.saving {
            self.queue.push_back(edit);
            return None;
        }
        self.saving = true;
        Some((config, edit))
    }

    /// A save landed: take its model and error, and hand back the next
    /// queued save, if any.
    pub fn committed(&mut self, done: Committed) -> Option<(AgentsConfig, Edit)> {
        if let Some(config) = done.config {
            self.config = Some(config);
        }
        if let Some(e) = done.error {
            self.error = Some((done.edit.target(), e));
        } else {
            match done.edit {
                Edit::CreateAgent { .. } | Edit::DuplicateAgent { .. } => self.draft = None,
                Edit::SetSharedInstructions { .. } => self.editing_shared = false,
                Edit::ResetAgent { .. } | Edit::DeleteAgent { .. } => {}
            }
        }
        if !done.dropped_comments.is_empty() {
            self.notice = Some(format!(
                "The save dropped these comments: {}",
                done.dropped_comments.join("; ")
            ));
        }
        self.saving = false;
        let next = self.queue.pop_front()?;
        self.saving = true;
        Some((self.config.clone()?, next))
    }

    /// The table's rows: shipped agents in their shipped order, then the
    /// user's by name (the model's order).
    pub fn agents(&self) -> Vec<AgentSummary> {
        self.config.as_ref().map(AgentsConfig::summaries).unwrap_or_default()
    }

    /// Why agent `name` can't be deleted (shipped, or named by a workflow
    /// status or the project's runner pin); `None` lets the menu offer it.
    pub fn delete_blocked(&self, name: &str) -> Option<String> {
        let config = self.config.as_ref()?;
        config.check_delete(name).err().map(|e| sentence(&e.to_string()))
    }

    /// Why agent `name` can't be reset: it isn't shipped, or it already is
    /// the shipped default.
    pub fn reset_blocked(&self, name: &str) -> Option<String> {
        let config = self.config.as_ref()?;
        if !config.can_reset(name) {
            Some("Only the agents Shelbi ships have a default to reset to.".into())
        } else if config.provenance(name) == Provenance::Shipped {
            Some("Already the shipped default.".into())
        } else {
            None
        }
    }

    /// Ask to confirm `confirm` (Reset or Delete from a row's ⋮ menu),
    /// unless its guard refuses it.
    pub fn ask(&mut self, confirm: Confirm) {
        let blocked = match &confirm {
            Confirm::Reset(name) => self.reset_blocked(name),
            Confirm::Delete(name) => self.delete_blocked(name),
        };
        if blocked.is_none() {
            self.error = None;
            self.confirm = Some(confirm);
        }
    }

    /// The name a duplicate of `from` is offered: `<from>-copy`, then
    /// `-copy-2`, `-copy-3`, … to be unique.
    pub fn duplicate_name(&self, from: &str) -> String {
        let base = format!("{from}-copy");
        let taken = |name: &str| {
            self.config
                .as_ref()
                .is_some_and(|c| c.agent(name).is_some() || c.unreadable().iter().any(|u| u.name == name))
        };
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    /// The shared instructions as loaded (empty when there are none).
    pub fn shared_instructions(&self) -> &str {
        self.config.as_ref().map_or("", AgentsConfig::shared_instructions)
    }
}

/// How many lines of the shared instructions the preview shows.
pub const PREVIEW_LINES: usize = 8;

/// One block of the shared instructions' preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewBlock {
    /// A heading's text (drawn heavier) rather than a paragraph's.
    pub heading: bool,
    /// Its lines: a paragraph's soft-wrapped lines joined into one, each
    /// list item its own.
    pub lines: Vec<String>,
}

/// The shared instructions' first `max` lines as the preview draws them,
/// lightly formatted: blocks split at blank lines, headings without their
/// `#`s, list items as `•` lines, a paragraph's wrapped lines joined,
/// `**`/`` ` `` marks and HTML comments dropped. The flag is whether
/// anything was left out.
pub fn preview(markdown: &str, max: usize) -> (Vec<PreviewBlock>, bool) {
    let text = strip_comments(markdown);
    let mut blocks: Vec<PreviewBlock> = Vec::new();
    let mut open = false;
    let mut fence = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            fence = !fence;
            open = false;
            continue;
        }
        if line.trim().is_empty() {
            open = false;
            continue;
        }
        let trimmed = line.trim_start();
        if !fence && trimmed.starts_with('#') {
            let title = trimmed.trim_start_matches('#').trim();
            blocks.push(PreviewBlock { heading: true, lines: vec![plain(title)] });
            open = false;
            continue;
        }
        let item = (!fence)
            .then(|| list_item(trimmed))
            .flatten();
        match (item, open, blocks.last_mut()) {
            (Some(item), true, Some(block)) => block.lines.push(format!("• {}", plain(item))),
            (Some(item), _, _) => {
                blocks.push(PreviewBlock { heading: false, lines: vec![format!("• {}", plain(item))] });
                open = true;
            }
            // A wrapped line continues the paragraph or the list item.
            (None, true, Some(block)) if !fence => {
                let last = block.lines.last_mut().expect("an open block has a line");
                last.push(' ');
                last.push_str(&plain(trimmed));
            }
            (None, true, Some(block)) => block.lines.push(line.to_string()),
            (None, _, _) => {
                let text = if fence { line.to_string() } else { plain(trimmed) };
                blocks.push(PreviewBlock { heading: false, lines: vec![text] });
                open = true;
            }
        }
    }
    let mut shown = Vec::new();
    let mut count = 0;
    let mut cut = false;
    for mut block in blocks {
        if count == max {
            cut = true;
            break;
        }
        if count + block.lines.len() > max {
            block.lines.truncate(max - count);
            cut = true;
        }
        count += block.lines.len();
        shown.push(block);
        if cut {
            break;
        }
    }
    (shown, cut)
}

/// `line`'s text when it's a list item (`- `, `* `, `+ `, `1. `).
fn list_item(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    (digits > 0)
        .then(|| line[digits..].strip_prefix(". "))
        .flatten()
}

/// `text` without its emphasis and code marks.
fn plain(text: &str) -> String {
    text.replace("**", "").replace('`', "")
}

/// `text` without its `<!-- … -->` comments.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The Agents frame's content (`--visual-figma`, and the tests): the
/// Workflows fixture's catalog and workflows (which assign the
/// orchestrator, developer and review agents), the shipped agents as
/// `shelbi init` writes them with the orchestrator's and the review agent's
/// instructions edited (the frame's two Customized badges), and the frame's
/// shared instructions.
#[cfg(any(test, feature = "visual-check"))]
pub(crate) mod fixture {
    use std::path::Path;

    use super::super::workflows_state::fixture::{STATUSES, SUBTASK, TASK};

    /// The frame's shared instructions.
    pub const PREAMBLE: &str = "\
You are working on Shelbi, an agent orchestrator built with Rust.

Keep changes focused on the assigned task. Follow the project’s conventions and run the relevant checks before handing work back.

Work on a task branch in your assigned workspace. Preserve unrelated changes and never commit directly to main.
";

    /// Write the frame's `workflows/` and `agents/` into project
    /// `project`'s directory `project_dir` (under the current
    /// `SHELBI_HOME`).
    pub fn write(project_dir: &Path, project: &str) {
        let workflows = project_dir.join("workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("statuses.yaml"), STATUSES).unwrap();
        std::fs::write(workflows.join("task.yaml"), TASK).unwrap();
        std::fs::write(workflows.join("subtask.yaml"), SUBTASK).unwrap();
        let agents = project_dir.join("agents");
        let _ = std::fs::remove_dir_all(&agents);
        shelbi_state::materialize_default_agents(project).expect("the shipped agents");
        for agent in ["orchestrator", "review"] {
            let path = agents.join(agent).join("instructions.md");
            let mut text = std::fs::read_to_string(&path).unwrap();
            text.push_str("\nKeep this project's release notes current.\n");
            std::fs::write(path, text).unwrap();
        }
        std::fs::create_dir_all(agents.join("_shared")).unwrap();
        std::fs::write(agents.join("_shared").join("preamble.md"), PREAMBLE).unwrap();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::workflows_state::tests::{Home, PROJECT};
    use super::*;

    /// A throwaway home holding the Agents fixture.
    pub(crate) fn home(tag: &str) -> Home {
        home_for(tag, PROJECT)
    }

    /// [`home`] with the fixture project named `project`.
    pub(crate) fn home_for(tag: &str, project: &str) -> Home {
        let home = Home::with_project(&format!("agents-{tag}"), project);
        fixture::write(&home.root.join("projects").join(project), project);
        home
    }

    fn agents_dir(home: &Home) -> std::path::PathBuf {
        home.root.join("projects").join(PROJECT).join("agents")
    }

    fn loaded() -> AgentsState {
        let mut state = AgentsState::new(PROJECT);
        state.loaded(AgentsConfig::load(PROJECT).map_err(|e| e.to_string()));
        state
    }

    /// Make `edit` the way the page does.
    fn make(state: &mut AgentsState, edit: Edit) {
        let mut job = state.request(edit);
        while let Some((config, edit)) = job {
            let done = commit(PROJECT, &config, edit);
            job = state.committed(done);
        }
    }

    #[test]
    fn the_table_shows_the_projects_agents() {
        let _home = home("rows");
        let state = loaded();
        let rows = state.agents();
        let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["orchestrator", "developer", "review", "qa", "security", "adversarial"], "shipped order");
        let row = |name: &str| rows.iter().find(|r| r.name == name).unwrap().clone();
        assert_eq!(row("orchestrator").provenance, Provenance::Customized);
        assert_eq!(row("review").provenance, Provenance::Customized);
        assert_eq!(row("developer").provenance, Provenance::Shipped);
        assert_eq!(row("developer").preferred_runner, Some(RunnerKind::Claude));
        assert!(row("developer").runner_recommended);
        assert_eq!(row("review").skills, 1, "the review agent ships a skill");
        assert_eq!(row("developer").skills, 0);

        let chips = status_chips(&row("orchestrator"));
        assert_eq!(
            chips.iter().map(|c| (c.name.as_str(), c.category)).collect::<Vec<_>>(),
            [("Backlog", Some(StatusCategory::Backlog)), ("To Do", Some(StatusCategory::Ready))]
        );
        let developer = status_chips(&row("developer"));
        assert_eq!(developer.len(), 1, "In Progress once though task and subtask both assign it");
        assert_eq!(developer[0].workflows, ["subtask", "task"]);
        assert!(status_chips(&row("qa")).is_empty(), "Not assigned");
        assert_eq!(state.shared_instructions(), fixture::PREAMBLE);
    }

    #[test]
    fn new_and_duplicate_agents_persist() {
        let home = home("new");
        let mut state = loaded();
        state.draft = Some(None);
        make(&mut state, Edit::CreateAgent { name: "docs".into() });
        assert_eq!(state.draft, None, "the new row closes once it's created");
        let dir = agents_dir(&home).join("docs");
        assert!(std::fs::read_to_string(dir.join("agent.yaml")).unwrap().contains("name: docs"));
        assert!(dir.join("instructions.md").exists());
        let docs = state.agents().into_iter().find(|r| r.name == "docs").unwrap();
        assert_eq!(docs.provenance, Provenance::UserCreated);

        assert_eq!(state.duplicate_name("review"), "review-copy");
        make(&mut state, Edit::DuplicateAgent { from: "review".into(), name: "review-copy".into() });
        assert_eq!(state.duplicate_name("review"), "review-copy-2");
        let copy = agents_dir(&home).join("review-copy");
        assert!(std::fs::read_to_string(copy.join("instructions.md")).unwrap().contains("release notes"));
        assert_eq!(state.agents().iter().find(|r| r.name == "review-copy").unwrap().skills, 1);
    }

    #[test]
    fn a_bad_name_shows_on_the_new_row_and_keeps_it_open() {
        let _home = home("bad-name");
        let mut state = loaded();
        state.draft = Some(None);
        make(&mut state, Edit::CreateAgent { name: "developer".into() });
        assert_eq!(state.error_at(&Target::NewAgent), Some("An agent named `developer` already exists"));
        assert_eq!(state.draft, Some(None));
    }

    #[test]
    fn shipped_and_assigned_agents_cant_be_deleted() {
        let home = home("delete");
        let mut state = loaded();
        assert!(state.delete_blocked("developer").is_some_and(|e| e.contains("ships with Shelbi")));
        state.ask(Confirm::Delete("developer".into()));
        assert_eq!(state.confirm, None, "a blocked delete asks nothing");

        make(&mut state, Edit::CreateAgent { name: "docs".into() });
        assert_eq!(state.delete_blocked("docs"), None);
        // A workflow status naming it blocks it.
        let task = home.workflows().join("task.yaml");
        let text = std::fs::read_to_string(&task).unwrap();
        std::fs::write(&task, text.replace("agent: review", "agent: docs")).unwrap();
        state.loaded(AgentsConfig::load(PROJECT).map_err(|e| e.to_string()));
        assert!(
            state.delete_blocked("docs").is_some_and(|e| e.contains("workflow `task` status `review`")),
            "{:?}",
            state.delete_blocked("docs")
        );
        std::fs::write(&task, text).unwrap();
        state.loaded(AgentsConfig::load(PROJECT).map_err(|e| e.to_string()));

        state.ask(Confirm::Delete("docs".into()));
        assert_eq!(state.confirm, Some(Confirm::Delete("docs".into())));
        let edit = state.confirm.as_ref().unwrap().edit();
        make(&mut state, edit);
        assert_eq!(state.confirm, None);
        assert!(!agents_dir(&home).join("docs").exists());
        assert!(state.agents().iter().all(|r| r.name != "docs"));
    }

    #[test]
    fn reset_puts_a_customized_agent_back() {
        let home = home("reset");
        let mut state = loaded();
        assert_eq!(state.reset_blocked("developer").as_deref(), Some("Already the shipped default."));
        assert_eq!(state.reset_blocked("orchestrator"), None);
        make(&mut state, Edit::CreateAgent { name: "docs".into() });
        assert!(state.reset_blocked("docs").is_some_and(|e| e.contains("ships")));

        state.ask(Confirm::Reset("orchestrator".into()));
        let edit = state.confirm.as_ref().unwrap().edit();
        make(&mut state, edit);
        let text = std::fs::read_to_string(agents_dir(&home).join("orchestrator/instructions.md")).unwrap();
        assert!(!text.contains("release notes"), "the edit is gone");
        let row = state.agents().into_iter().find(|r| r.name == "orchestrator").unwrap();
        assert_eq!(row.provenance, Provenance::Shipped, "no Customized badge");
    }

    #[test]
    fn shared_instructions_save_and_a_hand_edit_is_not_overwritten() {
        let home = home("shared");
        let mut state = loaded();
        state.editing_shared = true;
        make(&mut state, Edit::SetSharedInstructions { text: "Be kind.\n".into() });
        assert!(!state.editing_shared, "the editor closes once it's saved");
        let path = agents_dir(&home).join("_shared/preamble.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Be kind.\n");
        assert_eq!(state.shared_instructions(), "Be kind.\n");

        std::fs::write(&path, "By hand.\n").unwrap();
        state.editing_shared = true;
        make(&mut state, Edit::SetSharedInstructions { text: "Mine.\n".into() });
        assert!(state.error_at(&Target::Shared).is_some_and(|e| e.starts_with("Changed on disk")));
        assert!(state.editing_shared, "the editor stays open with the text");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "By hand.\n");
        assert_eq!(state.shared_instructions(), "By hand.\n", "reloaded");
    }

    #[test]
    fn an_edit_made_while_one_saves_waits_its_turn() {
        let _home = home("queue");
        let mut state = loaded();
        let (config, first) = state.request(Edit::CreateAgent { name: "a".into() }).unwrap();
        assert!(state.request(Edit::CreateAgent { name: "b".into() }).is_none());
        let (config2, second) = state.committed(commit(PROJECT, &config, first)).expect("the queued one");
        assert!(config2.agent("a").is_some(), "builds on the first save");
        assert!(state.committed(commit(PROJECT, &config2, second)).is_none());
        let back = AgentsConfig::load(PROJECT).unwrap();
        assert!(back.agent("a").is_some() && back.agent("b").is_some());
    }

    #[test]
    fn the_preview_shows_the_first_lines_lightly_formatted() {
        let (blocks, cut) = preview(fixture::PREAMBLE, PREVIEW_LINES);
        assert!(!cut);
        assert_eq!(blocks.len(), 3);
        assert!(blocks.iter().all(|b| !b.heading && b.lines.len() == 1));

        let text = "<!-- hidden\nnote -->\n## Hub safety\n\nYou are **inside** a\nlive hub.\n\n- Never run `stop`.\n- Never\n  kill.\n\n1. First\n";
        let (blocks, cut) = preview(text, PREVIEW_LINES);
        assert!(!cut);
        assert_eq!(
            blocks,
            [
                PreviewBlock { heading: true, lines: vec!["Hub safety".into()] },
                PreviewBlock { heading: false, lines: vec!["You are inside a live hub.".into()] },
                PreviewBlock { heading: false, lines: vec!["• Never run stop.".into(), "• Never kill.".into()] },
                PreviewBlock { heading: false, lines: vec!["• First".into()] },
            ]
        );
        let (blocks, cut) = preview(text, 3);
        assert!(cut);
        assert_eq!(blocks.iter().map(|b| b.lines.len()).sum::<usize>(), 3);
        assert_eq!(preview("", PREVIEW_LINES), (vec![], false));
    }
}
