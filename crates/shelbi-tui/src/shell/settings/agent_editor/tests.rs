//! The TUI's agent editor against real files in a throwaway `SHELBI_HOME`
//! (the shared Agents fixture): each configuration group through the keys
//! alone, inherited values, generic runners, instructions edited in place,
//! skills added, imported, inspected and removed, reset and discard, the
//! guards, the leave prompt, on-disk conflicts, failed saves, stale
//! results, and what it draws at small sizes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use shelbi_app::settings::agents::fixture::Home;
use shelbi_core::{ReasoningEffort, RunnerKind};

use super::*;

const PROJECT: &str = "proj";

fn home(tag: &str) -> Home {
    let lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Home::new(&format!("tui-ed-{tag}"), PROJECT, lock)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Press `k` with the built-in nav (arrows, Enter).
fn press(ed: &mut AgentEditorPage, k: KeyEvent) -> EditorAction {
    let nav = Nav {
        up: k.code == KeyCode::Up && k.modifiers.is_empty(),
        down: k.code == KeyCode::Down && k.modifiers.is_empty(),
        activate: k.code == KeyCode::Enter,
    };
    ed.handle_key(k, nav)
}

fn typed(ed: &mut AgentEditorPage, text: &str) {
    for c in text.chars() {
        press(ed, key(KeyCode::Char(c)));
    }
}

fn read(project: &str) -> Result<AgentsConfig, String> {
    AgentsConfig::load(project).map_err(|e| e.to_string())
}

/// Run the editor's waiting work, as the shell does off the UI thread.
/// Returns what the last landing asked.
fn drain(ed: &mut AgentEditorPage) -> Option<Landed> {
    let mut last = None;
    while let Some((ticket, work)) = ed.take_work() {
        let done = run_work(work, &read);
        last = ed.landed(&ticket, done);
    }
    last
}

/// Save (Ctrl+S) and run it.
fn save(ed: &mut AgentEditorPage) -> Option<Landed> {
    assert_eq!(press(ed, ctrl('s')), EditorAction::Work, "a save starts");
    drain(ed)
}

fn editor(name: &str) -> AgentEditorPage {
    let mut ed = AgentEditorPage::open(PROJECT, name, None);
    drain(&mut ed);
    assert!(ed.state().is_some(), "loaded");
    ed
}

fn go(ed: &mut AgentEditorPage, item: Item) {
    assert!(ed.select(&item), "{item:?} is a row");
}

/// Pick the entry labelled `label` in the open list.
fn pick(ed: &mut AgentEditorPage, label: &str) -> EditorAction {
    let Some(Modal::List { entries, .. }) = ed.modal.clone() else {
        panic!("no list open");
    };
    let i = entries.iter().position(|c| c.label == label).unwrap_or_else(|| panic!("no {label} in {entries:?}"));
    ed.choose(&entries, i)
}

fn entries(ed: &AgentEditorPage) -> Vec<Entry> {
    match &ed.modal {
        Some(Modal::List { entries, .. }) => entries.clone(),
        other => panic!("no list open: {other:?}"),
    }
}

fn agent_file(home: &Home, agent: &str, file: &str) -> std::path::PathBuf {
    home.agents().join(agent).join(file)
}

fn on_disk(name: &str) -> shelbi_state::agents_config::Agent {
    AgentsConfig::load(PROJECT).unwrap().agent(name).unwrap().clone()
}

/// A saved user agent `name`.
fn user_agent(name: &str) {
    AgentsConfig::load(PROJECT).unwrap().create_agent(name, Some("Writes docs.")).unwrap().save().unwrap();
}

fn render(ed: &mut AgentEditorPage, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ed.render(f, f.area(), None, "↑↓")).unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn render_buf(ed: &mut AgentEditorPage, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| ed.render(f, f.area(), None, "↑↓")).unwrap();
    term.backend().buffer().clone()
}

fn page_text(ed: &mut AgentEditorPage) -> String {
    ed.rows(110, 40)
        .into_iter()
        .map(|r| r.line.spans.iter().map(|s| s.content.to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn it_opens_after_its_read_with_the_header_and_every_section() {
    let _home = home("open");
    let mut ed = AgentEditorPage::open(PROJECT, "developer", None);
    assert!(render(&mut ed, 100, 30).contains("Loading…"));
    drain(&mut ed);
    let text = page_text(&mut ed);
    for want in [
        "Agent", "Name", "developer", "Description", "Instructions", "Markdown", INSTRUCTIONS_HELPER,
        "Edit instructions.md in your editor", "Shared project instructions", "Skills (", "Add skill",
        "Import skill", "Runner preferences", "Preferred runner", "Model", "Reasoning effort", "Permissions",
        "Workflow assignments", "task / In Progress", "subtask / In Progress", "Manage assignments in Workflows",
        "Reset to default", "Delete agent", APPLY_NOTE,
    ] {
        assert!(text.contains(want), "{want} in:\n{text}");
    }
    // The locked rename and delete say why, from the shared guard.
    assert!(text.contains("Locked:") && text.contains("ships with Shelbi"), "{text}");
    let screen = render(&mut ed, 100, 30);
    // The title bar's labels, on its middle row.
    let header = screen.lines().nth(1).unwrap();
    assert!(header.contains("←") && header.contains("developer   Default"), "{header}");
    assert!(header.contains("Save changes") && header.contains("Discard changes") && header.contains("⋮"), "{header}");
    // A customized one says so; a missing one says it isn't there.
    let mut ed = editor("orchestrator");
    assert!(render(&mut ed, 100, 10).lines().nth(1).unwrap().contains("orchestrator   Customized"));
    let mut gone = AgentEditorPage::open(PROJECT, "nope", None);
    drain(&mut gone);
    assert!(render(&mut gone, 80, 10).contains("There is no agent named nope."));
}

#[test]
fn every_configuration_group_edits_by_keys_alone_and_waits_for_save() {
    let home = home("every");
    let yaml = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
    let mut ed = editor("developer");

    // Description: typed in its row.
    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, key(KeyCode::End));
    press(&mut ed, ctrl('u'));
    typed(&mut ed, "Ships features.");
    press(&mut ed, key(KeyCode::Enter));

    // Instructions: edited in place, every key typing (j, q, digits, Enter).
    go(&mut ed, Item::Area(Area::Instructions));
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.editing(), Some(Area::Instructions));
    press(&mut ed, ctrl('a'));
    for _ in 0..400 {
        press(&mut ed, key(KeyCode::PageDown));
    }
    press(&mut ed, key(KeyCode::End));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "jq 42");
    press(&mut ed, key(KeyCode::Esc));
    assert_eq!(ed.editing(), None, "Esc stops editing, not the editor");

    // Runner, model (a suggestion), effort, permissions: pickers.
    go(&mut ed, Item::Runner);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Codex");
    go(&mut ed, Item::Model);
    press(&mut ed, key(KeyCode::Enter));
    let models: Vec<String> = entries(&ed).into_iter().map(|e| e.label).collect();
    assert!(models.iter().any(|m| m.starts_with("gpt")), "Codex's models: {models:?}");
    pick(&mut ed, &models[1]);
    go(&mut ed, Item::Effort);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "High");
    go(&mut ed, Item::Permissions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Plan");

    assert!(ed.is_dirty());
    assert_eq!(std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap(), yaml, "nothing yet");
    assert_eq!(save(&mut ed), Some(Landed::Wrote));
    let a = on_disk("developer");
    assert_eq!(a.description(), Some("Ships features."));
    assert!(a.instructions().ends_with("\njq 42"), "{:?}", a.instructions());
    assert_eq!(a.preferred_runner(), Some(RunnerKind::Codex));
    assert_eq!(a.model(RunnerKind::Codex), Some(models[1].as_str()));
    assert_eq!(a.reasoning_effort(RunnerKind::Codex), Some(ReasoningEffort::High));
    assert_eq!(a.permissions_mode(), Some("plan"));
    assert!(!ed.is_dirty());

    // The defaults remove the keys again.
    for (item, label) in [
        (Item::Model, RUNNER_DEFAULT_LABEL),
        (Item::Effort, RUNNER_DEFAULT_LABEL),
        (Item::Permissions, "Use project settings"),
        (Item::Runner, "Project default"),
    ] {
        go(&mut ed, item);
        press(&mut ed, key(KeyCode::Enter));
        pick(&mut ed, label);
    }
    save(&mut ed);
    let text = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
    for key in ["model:", "reasoning_effort:", "permissions_mode:", "preferred_runner:"] {
        assert!(!text.lines().any(|l| l.trim_start().starts_with(key)), "{key} in {text}");
    }
}

const RUNNER_DEFAULT_LABEL: &str = "Use runner default";

#[test]
fn a_custom_model_is_typed_and_a_generic_runner_explains_why_there_is_none() {
    let _home = home("custom");
    let mut ed = editor("developer");
    go(&mut ed, Item::Model);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Custom model…");
    assert_eq!(ed.typing(), Some(Text::CustomModel));
    press(&mut ed, ctrl('u'));
    typed(&mut ed, "claude-next-preview");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.cursor_item(), Some(Item::Model));
    assert_eq!(ed.state().unwrap().model(), Some("claude-next-preview"));
    let text = page_text(&mut ed);
    assert!(text.contains("claude-next-preview (this agent’s setting)"), "{text}");
    save(&mut ed);
    assert_eq!(on_disk("developer").model(RunnerKind::Claude), Some("claude-next-preview"));
    // It stays a choice, marked current.
    press(&mut ed, key(KeyCode::Enter));
    assert!(entries(&ed).iter().any(|e| e.label == "claude-next-preview" && e.note == "current"));
    press(&mut ed, key(KeyCode::Esc));

    // Generic: Model and Effort explain instead of offering choices.
    go(&mut ed, Item::Runner);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Generic");
    let text = page_text(&mut ed);
    assert!(text.contains("Generic runners take no model or reasoning effort."), "{text}");
    for item in [Item::Model, Item::Effort] {
        go(&mut ed, item);
        press(&mut ed, key(KeyCode::Enter));
        let list = entries(&ed);
        assert_eq!(list.len(), 1);
        assert!(list[0].inert && list[0].help.as_deref().unwrap().contains("Generic runners"));
        pick(&mut ed, "Nothing to choose");
        assert!(ed.modal.is_none());
    }
}

#[test]
fn what_the_project_sets_shows_as_in_effect() {
    let home = home("inherit");
    let path = home.project_dir().with_extension("yaml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{text}agents:\n  developer:\n    runner: codex\nrunners:\n  codex:\n    command: codex\n    model: gpt-house\n\
             workspace_permissions_mode: acceptEdits\n"
        ),
    )
    .unwrap();
    let mut ed = editor("developer");
    let text = page_text(&mut ed);
    assert!(text.contains("The project runs developer on Codex (agents.developer.runner)"), "{text}");
    assert!(text.contains("The project sets gpt-house for every Codex agent"), "{text}");
    assert!(text.contains("Sessions launch with the project’s Accept edits."), "{text}");
}

#[test]
fn skills_are_added_imported_inspected_and_removed_with_a_confirm() {
    let home = home("skills");
    user_agent("docs");
    let mut ed = editor("docs");
    assert!(page_text(&mut ed).contains(SKILLS_EMPTY));

    // Add skill: name, then a refusal (no description) keeps the form.
    press(&mut ed, key(KeyCode::Char('n')));
    assert_eq!(ed.typing(), Some(Text::SkillName));
    typed(&mut ed, "release");
    press(&mut ed, key(KeyCode::Esc));
    go(&mut ed, Item::SkillAdd);
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.state().unwrap().error_at(&Field::NewSkill).unwrap().contains("needs a description"));
    assert!(ed.items().contains(&Item::SkillAdd), "the form stays open");
    go(&mut ed, Item::Text(Text::SkillDescription));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "Cut a release.");
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.editing(), Some(Area::SkillBody), "Enter moves on to the body");
    typed(&mut ed, "Run the script.");
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "Then tag it.");
    press(&mut ed, key(KeyCode::Esc));
    go(&mut ed, Item::SkillAdd);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.cursor_item(), Some(Item::Skill("release".into())));
    assert!(!ed.items().contains(&Item::SkillAdd), "the form closed");

    // Import: a missing path, then a folder with nested files.
    let src = home.root.join("import").join("triage");
    std::fs::create_dir_all(src.join("refs/deep")).unwrap();
    std::fs::write(src.join("SKILL.md"), "---\nname: triage\ndescription: Sort issues.\n---\nLabel them.\n").unwrap();
    std::fs::write(src.join("refs/deep/labels.md"), "bug, chore\n").unwrap();
    go(&mut ed, Item::Text(Text::Import));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, &home.root.join("nowhere").display().to_string());
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.state().unwrap().error_at(&Field::NewSkill).unwrap().contains("doesn't exist"));
    go(&mut ed, Item::Text(Text::Import));
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, ctrl('u'));
    assert!(ed.paste(&src.display().to_string()));
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.cursor_item(), Some(Item::Skill("triage".into())));
    assert!(page_text(&mut ed).contains("Sort issues."));

    // Inspect shows the draft's files and text.
    press(&mut ed, key(KeyCode::Char('i')));
    let screen = render(&mut ed, 100, 30);
    assert!(screen.contains("Skill triage") && screen.contains("skills/triage/refs/deep/labels.md"), "{screen}");
    assert!(screen.contains("Label them.") && screen.contains("not saved yet"), "{screen}");
    press(&mut ed, key(KeyCode::Esc));

    // Nothing is written before Save; then both, the structure kept.
    assert!(!agent_file(&home, "docs", "skills/release/SKILL.md").exists());
    save(&mut ed);
    let text = std::fs::read_to_string(agent_file(&home, "docs", "skills/release/SKILL.md")).unwrap();
    assert!(text.contains("description: Cut a release.") && text.contains("Run the script.\nThen tag it."), "{text}");
    assert_eq!(std::fs::read_to_string(agent_file(&home, "docs", "skills/triage/refs/deep/labels.md")).unwrap(), "bug, chore\n");

    // Remove asks first, Cancel focused.
    go(&mut ed, Item::Skill("triage".into()));
    press(&mut ed, key(KeyCode::Char('x')));
    assert_eq!(ed.modal_title().as_deref(), Some("Remove skill"));
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.state().unwrap().skills().iter().any(|s| s.name == "triage"), "Enter alone cancels");
    press(&mut ed, key(KeyCode::Char('x')));
    press(&mut ed, key(KeyCode::Char('y')));
    assert!(!ed.state().unwrap().skills().iter().any(|s| s.name == "triage"));
    assert!(agent_file(&home, "docs", "skills/triage/SKILL.md").exists(), "until Save");
    save(&mut ed);
    assert!(!agent_file(&home, "docs", "skills/triage").exists());
}

#[test]
fn an_import_of_a_skill_without_a_description_is_a_field_error() {
    let home = home("bad-import");
    user_agent("docs");
    let mut ed = editor("docs");
    let bad = home.root.join("bad.md");
    std::fs::write(&bad, "No frontmatter.\n").unwrap();
    go(&mut ed, Item::Text(Text::Import));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, &bad.display().to_string());
    press(&mut ed, key(KeyCode::Enter));
    let error = ed.state().unwrap().error_at(&Field::NewSkill).unwrap().to_string();
    assert!(error.contains("has no frontmatter"), "{error}");
    assert!(page_text(&mut ed).contains("frontmatter"), "it shows under the field");
    assert_eq!(ed.text(&Text::Import), bad.display().to_string(), "the path stays to correct");
    assert!(!ed.is_dirty());
}

#[test]
fn reset_goes_into_the_draft_and_discard_restores_the_loaded_agent() {
    let home = home("reset");
    let before = std::fs::read_to_string(agent_file(&home, "orchestrator", "instructions.md")).unwrap();
    let mut ed = editor("orchestrator");
    go(&mut ed, Item::Reset);
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.is_dirty());
    assert!(!ed.instructions.text().contains("release notes"), "the field shows the shipped text");
    assert!(page_text(&mut ed).contains("Already the shipped default."));
    assert_eq!(std::fs::read_to_string(agent_file(&home, "orchestrator", "instructions.md")).unwrap(), before);
    // Discard: the customized files again.
    go(&mut ed, Item::Discard);
    press(&mut ed, key(KeyCode::Enter));
    assert!(!ed.is_dirty());
    assert!(ed.instructions.text().contains("release notes"));
    // Reset and Save: the shipped default on disk.
    go(&mut ed, Item::Reset);
    press(&mut ed, key(KeyCode::Enter));
    save(&mut ed);
    assert!(!std::fs::read_to_string(agent_file(&home, "orchestrator", "instructions.md")).unwrap().contains("release notes"));
    assert!(render(&mut ed, 100, 10).lines().nth(1).unwrap().contains("orchestrator   Default"));
}

#[test]
fn rename_and_delete_follow_the_shared_guards() {
    let home = home("guards");
    // Shipped: the name doesn't type, the reason shows; Delete is dimmed.
    let mut ed = editor("qa");
    go(&mut ed, Item::Text(Text::Name));
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.typing(), None, "a locked name doesn't type");
    go(&mut ed, Item::Delete);
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.modal.is_none(), "a refused delete asks nothing");
    press(&mut ed, key(KeyCode::Char('m')));
    let delete = entries(&ed).into_iter().find(|e| e.label == "Delete agent…").unwrap();
    assert!(delete.inert && delete.help.unwrap().contains("ships with Shelbi"));
    press(&mut ed, key(KeyCode::Esc));

    // The user's own: renamed at Save, deleted after a confirm.
    user_agent("docs");
    let mut ed = editor("docs");
    go(&mut ed, Item::Text(Text::Name));
    press(&mut ed, key(KeyCode::Enter));
    press(&mut ed, ctrl('u'));
    typed(&mut ed, "writer");
    press(&mut ed, key(KeyCode::Enter));
    assert!(!agent_file(&home, "writer", "agent.yaml").exists(), "not before Save");
    save(&mut ed);
    assert_eq!(ed.name(), "writer");
    assert!(agent_file(&home, "writer", "agent.yaml").exists() && !agent_file(&home, "docs", "agent.yaml").exists());
    go(&mut ed, Item::Delete);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(ed.modal_title().as_deref(), Some("Delete agent"));
    assert!(render(&mut ed, 100, 30).contains("can’t be undone."));
    press(&mut ed, key(KeyCode::Enter));
    assert!(agent_file(&home, "writer", "agent.yaml").exists(), "Cancel is focused");
    go(&mut ed, Item::Delete);
    press(&mut ed, key(KeyCode::Enter));
    assert_eq!(press(&mut ed, key(KeyCode::Char('y'))), EditorAction::Work);
    assert_eq!(drain(&mut ed), Some(Landed::Close { wrote: true, notice: None }));
    assert!(!agent_file(&home, "writer", "agent.yaml").exists());
}

#[test]
fn leaving_with_edits_asks_save_discard_or_cancel() {
    let home = home("leave");
    let mut ed = editor("developer");
    // Clean: Back goes at once, to the agent's row.
    assert_eq!(press(&mut ed, key(KeyCode::Esc)), EditorAction::Close(Back::Agent("developer".into())));

    go(&mut ed, Item::Permissions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Plan");
    // Esc asks; Cancel keeps the edits.
    assert_eq!(press(&mut ed, key(KeyCode::Esc)), EditorAction::Stay);
    assert!(render(&mut ed, 100, 30).contains("Save changes to developer?"));
    press(&mut ed, key(KeyCode::Char('c')));
    assert!(ed.is_dirty());

    // Edit YAML asks too; Discard drops the edits, then opens the file.
    assert_eq!(press(&mut ed, key(KeyCode::Char('y'))), EditorAction::Stay);
    assert_eq!(
        press(&mut ed, key(KeyCode::Char('d'))),
        EditorAction::EditFile(agent_file(&home, "developer", "agent.yaml"))
    );
    assert!(!ed.is_dirty());

    // The external instructions editor: guarded the same way.
    go(&mut ed, Item::Text(Text::Description));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, " More.");
    press(&mut ed, key(KeyCode::Enter));
    go(&mut ed, Item::EditInstructions);
    assert_eq!(press(&mut ed, key(KeyCode::Enter)), EditorAction::Stay);
    assert_eq!(press(&mut ed, key(KeyCode::Char('s'))), EditorAction::Work, "Save first");
    assert_eq!(drain(&mut ed), Some(Landed::Then(Leave::Open(AgentFile::Instructions))));
    assert_eq!(ed.then(Leave::Open(AgentFile::Instructions)), EditorAction::EditFile(agent_file(&home, "developer", "instructions.md")));
    assert!(on_disk("developer").description().unwrap().ends_with("More."));

    // An assignment, with edits: the prompt, then the workflow on its status.
    go(&mut ed, Item::Permissions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Auto");
    go(&mut ed, Item::Assignment(0));
    press(&mut ed, key(KeyCode::Enter));
    let then = press(&mut ed, key(KeyCode::Char('d')));
    let assignment = ed.state().unwrap().assignments()[0].clone();
    assert_eq!(then, EditorAction::OpenWorkflow { name: assignment.workflow, status: Some(assignment.status) });

    // Manage assignments and the shared instructions' link.
    go(&mut ed, Item::ManageAssignments);
    assert_eq!(press(&mut ed, key(KeyCode::Enter)), EditorAction::ShowWorkflows);
    go(&mut ed, Item::SharedLink);
    assert_eq!(press(&mut ed, key(KeyCode::Enter)), EditorAction::Close(Back::Shared));
}

#[test]
fn duplicate_edits_the_copy() {
    let home = home("dup");
    let mut ed = editor("review");
    press(&mut ed, key(KeyCode::Char('m')));
    assert_eq!(pick(&mut ed, "Duplicate"), EditorAction::Work);
    assert_eq!(drain(&mut ed), Some(Landed::Wrote));
    assert_eq!(ed.name(), "review-copy");
    assert!(std::fs::read_to_string(agent_file(&home, "review-copy", "instructions.md")).unwrap().contains("release notes"));
}

#[test]
fn a_hand_edit_under_the_save_asks_reload_or_keep_editing() {
    let home = home("conflict");
    let mut ed = editor("developer");
    go(&mut ed, Item::Permissions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Plan");
    let path = agent_file(&home, "developer", "instructions.md");
    std::fs::write(&path, "# By hand\n").unwrap();
    assert_eq!(save(&mut ed), Some(Landed::Stay));
    assert_eq!(ed.modal_title().as_deref(), Some("Changed on disk"));
    // Keep editing (focused): the draft stays and the hand edit too.
    press(&mut ed, key(KeyCode::Enter));
    assert!(ed.is_dirty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# By hand\n");
    // Save again is still refused; Reload takes the files.
    save(&mut ed);
    assert_eq!(ed.modal_title().as_deref(), Some("Changed on disk"));
    assert_eq!(press(&mut ed, key(KeyCode::Char('r'))), EditorAction::Work);
    drain(&mut ed);
    assert!(!ed.is_dirty());
    assert_eq!(ed.instructions.text(), "# By hand\n");
}

#[test]
fn a_failed_save_keeps_the_draft_and_shows_why() {
    let home = home("failed");
    let mut ed = editor("developer");
    go(&mut ed, Item::Area(Area::Instructions));
    press(&mut ed, key(KeyCode::Enter));
    typed(&mut ed, "Mine. ");
    press(&mut ed, key(KeyCode::Esc));
    let dir = home.agents().join("developer");
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, "not a directory").unwrap();
    save(&mut ed);
    assert!(ed.is_dirty() && !ed.busy());
    assert!(ed.instructions.text().starts_with("Mine. "));
    assert!(ed.state().unwrap().error_at(&Field::Save).is_some());
    assert!(render(&mut ed, 100, 30).lines().nth(2).is_some());
}

#[test]
fn back_from_the_users_editor_reloads_a_clean_draft_and_keeps_a_dirty_one() {
    let home = home("reload");
    let mut ed = editor("developer");
    let path = agent_file(&home, "developer", "instructions.md");
    std::fs::write(&path, "# Edited outside\n").unwrap();
    ed.reload();
    drain(&mut ed);
    assert_eq!(ed.instructions.text(), "# Edited outside\n", "a clean draft takes the files");

    go(&mut ed, Item::Permissions);
    press(&mut ed, key(KeyCode::Enter));
    pick(&mut ed, "Plan");
    std::fs::write(&path, "# Again\n").unwrap();
    ed.reload();
    drain(&mut ed);
    assert!(ed.is_dirty(), "edits are kept");
    assert_eq!(ed.instructions.text(), "# Edited outside\n");

    // The agent gone from the files closes the editor with a note.
    let mut ed = editor("qa");
    std::fs::remove_dir_all(home.agents().join("qa")).unwrap();
    ed.reload();
    assert!(matches!(drain(&mut ed), Some(Landed::Close { wrote: false, notice: Some(_) })));
}

#[test]
fn a_stale_result_is_dropped() {
    let _home = home("stale");
    let mut ed = AgentEditorPage::open(PROJECT, "developer", None);
    let (old, work) = ed.take_work().unwrap();
    let first = run_work(work, &read);
    ed.reload();
    assert!(ed.landed(&old, first).is_none(), "a superseded read is dropped");
    drain(&mut ed);
    assert!(ed.state().is_some());
}

#[test]
fn the_focused_row_stays_in_view_at_80x24_and_long_instructions_scroll() {
    let _home = home("scroll");
    let mut ed = editor("developer");
    let n = ed.items().len();
    for i in 0..n {
        ed.cursor = i;
        ed.follow_cursor = true;
        let screen = render(&mut ed, 80, 24);
        // Focus shows as the `›` marker, or as a focused field's fill.
        let marked = screen.lines().skip(2).take(21).any(|l| l.contains('›')) || {
            let buf = render_buf(&mut ed, 80, 24);
            (2..23).any(|y| (0..80).any(|x| buf[(x, y)].bg == crate::theme::palette().input_focus))
        };
        let header = i < 4;
        assert!(header || marked, "row {i} ({:?}) in view:\n{screen}", ed.cursor_item());
    }
    // Tab steps sections.
    ed.cursor = 0;
    press(&mut ed, key(KeyCode::Tab));
    assert_eq!(ed.cursor_item(), Some(Item::Text(Text::Name)));
    press(&mut ed, key(KeyCode::Tab));
    assert_eq!(ed.cursor_item(), Some(Item::Area(Area::Instructions)));

    // A long text: the caret's line stays on screen as it moves.
    let long: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    go(&mut ed, Item::Area(Area::Instructions));
    press(&mut ed, key(KeyCode::Enter));
    ed.instructions = TextArea::from_text(&long);
    for _ in 0..150 {
        press(&mut ed, key(KeyCode::Down));
    }
    let screen = render(&mut ed, 80, 24);
    assert!(screen.contains("line 151"), "{screen}");
    let screen = render(&mut ed, 60, 14);
    assert!(screen.contains("line 151"), "after a resize too:\n{screen}");
}

#[test]
fn renders_without_panicking_at_tiny_sizes() {
    let _home = home("tiny");
    let mut ed = editor("developer");
    for (w, h) in [(1, 1), (5, 3), (20, 6), (40, 10), (80, 24)] {
        render(&mut ed, w, h);
        press(&mut ed, key(KeyCode::Char('m')));
        render(&mut ed, w, h);
        press(&mut ed, key(KeyCode::Esc));
    }
}

#[test]
fn it_reads_in_light_with_a_menu_open() {
    use shelbi_app::theme::Appearance;
    let _home = home("light");
    let mut ed = editor("developer");
    crate::theme::with_appearance(Appearance::Light, || {
        let mut term = Terminal::new(TestBackend::new(110, 40)).unwrap();
        term.draw(|f| ed.render(f, f.area(), None, "↑↓")).unwrap();
        crate::theme::assert_light_readable("agent-editor", term.backend().buffer());
        go(&mut ed, Item::Runner);
        press(&mut ed, key(KeyCode::Enter));
        term.draw(|f| ed.render(f, f.area(), None, "↑↓")).unwrap();
        crate::theme::assert_light_readable("agent-editor-picker", term.backend().buffer());
    });
}

#[test]
fn a_click_on_any_row_of_a_fields_box_types_in_it() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let _home = home("field-click");
    let mut ed = editor("developer");
    let screen = render(&mut ed, 110, 60);
    // Name and Description share the identity row: the label's line.
    let label = screen.lines().position(|l| l.contains("Name") && l.contains("Description")).unwrap_or_else(|| panic!("{screen}"));
    for y in label + 1..=label + 3 {
        press(&mut ed, key(KeyCode::Esc));
        ed.cursor = 0;
        render(&mut ed, 110, 60);
        ed.handle_mouse(MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 40, row: y as u16, modifiers: KeyModifiers::NONE });
        assert_eq!(ed.cursor_item(), Some(Item::Text(Text::Description)), "row {y}");
        // Typed in: the caret's block shows in the box.
        let buf = render_buf(&mut ed, 110, 60);
        assert!((0..110).any(|x| buf[(x, label as u16 + 2)].bg == crate::theme::palette().white), "row {y}: the caret");
    }
}

#[test]
fn clicks_select_then_open_and_answer_menus() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let _home = home("mouse");
    let mut ed = editor("developer");
    let click = |x: u16, y: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    let screen = render(&mut ed, 110, 60);
    let (y, _) = screen.lines().enumerate().find(|(_, l)| l.contains("Preferred runner")).unwrap();
    ed.handle_mouse(click(10, y as u16));
    assert_eq!(ed.cursor_item(), Some(Item::Runner), "a first click selects");
    render(&mut ed, 110, 60);
    ed.handle_mouse(click(10, y as u16));
    assert_eq!(ed.modal_title().as_deref(), Some("Preferred runner"), "a second opens");
    let screen = render(&mut ed, 110, 60);
    // The menu's entry (a note on the page may name Codex too).
    let (y, line) = screen.lines().enumerate().find(|(_, l)| l.contains("│  Codex ")).unwrap();
    let x = line.find(" Codex ").map(|b| line[..b].chars().count()).unwrap() as u16 + 1;
    ed.handle_mouse(click(x, y as u16));
    assert_eq!(ed.state().unwrap().agent().preferred_runner(), Some(RunnerKind::Codex));
    // The wheel scrolls without moving the cursor.
    let wheel = MouseEvent { kind: MouseEventKind::ScrollDown, column: 5, row: 5, modifiers: KeyModifiers::NONE };
    ed.handle_mouse(wheel);
    assert_eq!(ed.cursor_item(), Some(Item::Runner));
}

#[test]
fn instructions_wrap_long_lines_and_move_by_wrapped_row() {
    let _home = home("wrap");
    let mut ed = editor("developer");
    go(&mut ed, Item::Area(Area::Instructions));
    press(&mut ed, key(KeyCode::Enter));
    for _ in 0..400 {
        press(&mut ed, key(KeyCode::PageDown));
    }
    press(&mut ed, key(KeyCode::End));
    press(&mut ed, key(KeyCode::Enter));
    let long = "Always rebase onto origin main before handing off the branch. ".repeat(3);
    typed(&mut ed, long.trim_end());
    let text = page_text(&mut ed);
    let rows: Vec<&str> = text.lines().filter(|l| l.contains("rebase onto")).collect();
    assert!(rows.len() >= 2, "the long line shows on several rows:\n{text}");
    assert!(rows.iter().all(|r| !r.contains("branch.Always")), "breaks fall between words:\n{text}");
    let (line, end) = ed.instructions.cursor();
    let wrap = ed.instructions.wrap().cloned().expect("the draw set the wrap");
    let (_, (caret_row, _)) = ed.instructions.wrapped(&wrap);
    press(&mut ed, key(KeyCode::Up));
    let (up_line, up_col) = ed.instructions.cursor();
    assert_eq!(up_line, line, "one wrapped row up stays in the line");
    assert!(up_col < end);
    assert_eq!(ed.instructions.wrapped(&wrap).1 .0, caret_row - 1);
    assert!(ed.instructions.text().ends_with(long.trim_end()), "no newline added");
}
