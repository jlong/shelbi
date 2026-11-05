//! The parts of `shelbi issue edit` that write frontmatter by key: `--param` /
//! `--no-param`, and `$EDITOR` on a backend with no local file to open.
//!
//! The file backend's editor mode opens the task file itself. A remote tracker
//! has no such file, so the editor works on a local copy of the issue, rendered
//! as that same frontmatter-plus-body document. Saving sends only what the user
//! changed through the daemon's edit mutation, so a change someone else made
//! while the editor was open survives unless it touched the same field. When a
//! save can't land, the copy is kept and the error says where.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use shelbi_core::Issue;
use shelbi_proto::control::{EditSpec, MutationKind};
use shelbi_state::{IssueFile, IssueStore};

use super::mutate_client;

/// Changed frontmatter keys: `Some` sets the key, `None` removes it.
type Changes = BTreeMap<String, Option<serde_yaml::Value>>;

/// The fields an editor save can't change, and what to do instead. Each one
/// either has a command that runs its side effects or isn't Shelbi's to set.
const READ_ONLY_KEYS: &[(&str, &str)] = &[
    ("id", "an issue's id is permanent"),
    ("column", "use `shelbi issue move`"),
    ("priority", "use `shelbi issue prio`"),
    ("assigned_to", "use `shelbi issue assign` or `unassign`"),
    ("created_at", "set by the tracker"),
    ("updated_at", "set by the tracker"),
];

/// Turn `--param KEY=VALUE` and `--no-param KEY` into the frontmatter changes
/// they ask for. Values stay strings, the only type `{{KEY}}` resolves.
pub(super) fn param_changes(
    set: &[String],
    clear: &[String],
) -> Result<BTreeMap<String, Option<serde_json::Value>>> {
    let mut changes = BTreeMap::new();
    let mut add = |key: &str, value: Option<serde_json::Value>| -> Result<()> {
        check_param_key(key)?;
        if changes.insert(key.to_string(), value).is_some() {
            bail!("param `{key}` is given more than once");
        }
        Ok(())
    };
    for pair in set {
        let Some((key, value)) = pair.split_once('=') else {
            bail!("--param takes KEY=VALUE (got `{pair}`)");
        };
        if value.is_empty() {
            bail!("--param {key}= has no value (--no-param {key} clears it)");
        }
        add(key, Some(serde_json::Value::String(value.to_string())))?;
    }
    for key in clear {
        add(key, None)?;
    }
    Ok(changes)
}

/// Refuse a param name that wouldn't read back as a param.
fn check_param_key(key: &str) -> Result<()> {
    let valid = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !valid {
        bail!("invalid param name `{key}`: use letters, digits, `_`, `-` and `.`");
    }
    if Issue::FIELD_KEYS.contains(&key) {
        bail!("`{key}` is an issue field, not a workflow param: use its own flag or command");
    }
    Ok(())
}

/// Edit an issue that lives on a remote tracker: open a local copy of it in
/// `$EDITOR`, then save what changed.
pub(super) fn edit_on_tracker(project: &str, id: &str, reason: Option<String>) -> Result<()> {
    let store = shelbi_state::issue_store_for(project).map_err(|e| anyhow!(e))?;
    let base = store
        .get(id)
        .map_err(|e| anyhow!(e))?
        .ok_or_else(|| anyhow!("issue `{id}` not found"))?;
    let original = shelbi_state::render_task_file(&base).map_err(|e| anyhow!(e))?;

    let dir = tempfile::Builder::new()
        .prefix("shelbi-issue-")
        .tempdir()
        .context("creating a temp dir for the editor")?;
    let path = dir.path().join(draft_file_name(&base.task.id));
    std::fs::write(&path, &original)
        .with_context(|| format!("writing {}", path.display()))?;

    let outcome = super::launch_editor(&path).and_then(|()| {
        let edited = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        save(project, id, store.as_ref(), &original, &edited, reason)
    });
    outcome.map_err(|e| {
        if draft_was_edited(&path, &original) {
            let kept = dir.keep().join(path.file_name().unwrap_or_default());
            anyhow!("{e:#}\nYour edits are kept in {}", kept.display())
        } else {
            e
        }
    })
}

/// The copy's file name: the id, so the editor's title names the issue, and
/// `.md`, so it highlights as markdown.
fn draft_file_name(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '-' })
        .collect();
    format!("{stem}.md")
}

fn draft_was_edited(path: &Path, original: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|text| text != original)
}

/// Save the edited copy, or do nothing when it says what the original did.
fn save(
    project: &str,
    id: &str,
    store: &dyn IssueStore,
    original: &str,
    edited: &str,
    reason: Option<String>,
) -> Result<()> {
    if edited == original {
        println!("(no change)");
        return Ok(());
    }
    // Compare against the original as parsed from its text, so whatever the
    // render normalized can't read as a change.
    let base = shelbi_state::parse_task_file(original).map_err(|e| anyhow!(e))?;
    let edited = shelbi_state::parse_task_file(edited)
        .map_err(|e| anyhow!("can't read the edited issue: {e}"))?;
    let ours = frontmatter_changes(&base.task, &edited.task)?;
    let Some(spec) = plan_save(&ours, &base, &edited, reason)? else {
        println!("(no change)");
        return Ok(());
    };
    // The editor may have been open for a while. The daemon guards the title
    // and body it's told about (`expect_title` / `expect_body`); the metadata is
    // checked here, against a fresh read.
    if !spec.frontmatter.is_empty() {
        let current = store
            .get(id)
            .map_err(|e| anyhow!(e))?
            .ok_or_else(|| anyhow!("issue `{id}` not found"))?;
        let theirs = frontmatter_changes(&base.task, &current.task)?;
        refuse_overlap(&ours, &theirs)?;
    }
    mutate_client::run_mutation(project, id, MutationKind::Edit(Box::new(spec)))
}

/// Every frontmatter key that differs between two issues, as the change that
/// turns `from` into `to`.
fn frontmatter_changes(from: &Issue, to: &Issue) -> Result<Changes> {
    let from = frontmatter_keys(from)?;
    let to = frontmatter_keys(to)?;
    let mut changes = Changes::new();
    for (key, value) in &to {
        if from.get(key) != Some(value) {
            changes.insert(key.clone(), Some(value.clone()));
        }
    }
    for key in from.keys() {
        if !to.contains_key(key) {
            changes.insert(key.clone(), None);
        }
    }
    Ok(changes)
}

fn frontmatter_keys(issue: &Issue) -> Result<BTreeMap<String, serde_yaml::Value>> {
    let value = serde_yaml::to_value(issue).context("rendering the issue's frontmatter")?;
    serde_yaml::from_value(value).context("rendering the issue's frontmatter")
}

/// The edit that saves `ours` and the body, or `None` when neither changed.
/// Refuses a change to a read-only field, naming every one.
fn plan_save(
    ours: &Changes,
    base: &IssueFile,
    edited: &IssueFile,
    reason: Option<String>,
) -> Result<Option<EditSpec>> {
    let refused: Vec<String> = READ_ONLY_KEYS
        .iter()
        .filter(|(key, _)| ours.contains_key(*key))
        .map(|(key, hint)| format!("`{key}` ({hint})"))
        .collect();
    if !refused.is_empty() {
        bail!("can't change {} from the editor", refused.join(", "));
    }

    let mut spec = EditSpec {
        reason,
        ..Default::default()
    };
    for (key, value) in ours {
        if key == "title" {
            spec.title = Some(edited.task.title.clone());
            spec.expect_title = Some(base.task.title.clone());
        } else {
            let value = value
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .with_context(|| format!("reading `{key}`"))?;
            spec.frontmatter.insert(key.clone(), value);
        }
    }
    // The tracker keeps the body trimmed, so surrounding blank lines aren't an
    // edit.
    if edited.body.trim() != base.body.trim() {
        spec.body_replace = Some(edited.body.clone());
        spec.expect_body = Some(base.body.clone());
    }
    let changed =
        spec.title.is_some() || spec.body_replace.is_some() || !spec.frontmatter.is_empty();
    Ok(changed.then_some(spec))
}

/// Refuse a save that would overwrite a field someone else changed differently
/// while the editor was open.
fn refuse_overlap(ours: &Changes, theirs: &Changes) -> Result<()> {
    let conflicts: Vec<&str> = ours
        .iter()
        .filter(|(key, value)| theirs.get(*key).is_some_and(|other| other != *value))
        .map(|(key, _)| key.as_str())
        .collect();
    if !conflicts.is_empty() {
        bail!(
            "{} changed while the editor was open: nothing was saved",
            conflicts.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shelbi_core::Column;

    fn issue_file(body: &str) -> IssueFile {
        let now = Utc::now();
        IssueFile {
            task: Issue {
                id: "t".into(),
                title: "Title".into(),
                column: Column::todo(),
                priority: 0,
                assigned_to: None,
                workflow: None,
                branch: Some("feat/x".into()),
                pr: None,
                depends_on: vec!["dep".into()],
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: now,
                updated_at: now,
                params: BTreeMap::from([("keep".to_string(), serde_yaml::Value::from(3))]),
            },
            body: body.into(),
            tracker_assignees: Vec::new(),
        }
    }

    fn plan(base: &IssueFile, edited: &IssueFile) -> Result<Option<EditSpec>> {
        let ours = frontmatter_changes(&base.task, &edited.task)?;
        plan_save(&ours, base, edited, None)
    }

    #[test]
    fn nothing_changed_plans_nothing() {
        let base = issue_file("body\n");
        let mut edited = base.clone();
        edited.body = "\nbody\n\n".into();
        assert!(plan(&base, &edited).unwrap().is_none(), "blank lines aren't an edit");
    }

    #[test]
    fn a_save_sends_only_the_keys_that_changed() {
        let base = issue_file("body\n");
        let mut edited = base.clone();
        edited.task.title = "New".into();
        edited.task.branch = None;
        edited.task.workflow = Some("subtask".into());
        edited.task.depends_on = vec!["dep".into(), "other".into()];
        edited.task.params.remove("keep");
        edited
            .task
            .params
            .insert("task".into(), serde_yaml::Value::from("parent"));
        edited.body = "new body\n".into();

        let spec = plan(&base, &edited).unwrap().unwrap();
        assert_eq!(spec.title.as_deref(), Some("New"));
        assert_eq!(spec.expect_title.as_deref(), Some("Title"));
        assert_eq!(spec.body_replace.as_deref(), Some("new body\n"));
        assert_eq!(spec.expect_body.as_deref(), Some("body\n"));
        assert_eq!(
            spec.frontmatter,
            BTreeMap::from([
                ("branch".to_string(), None),
                ("depends_on".to_string(), Some(serde_json::json!(["dep", "other"]))),
                ("keep".to_string(), None),
                ("task".to_string(), Some(serde_json::json!("parent"))),
                ("workflow".to_string(), Some(serde_json::json!("subtask"))),
            ])
        );
    }

    #[test]
    fn every_read_only_field_is_refused_by_name() {
        let base = issue_file("body\n");
        let mut edited = base.clone();
        edited.task.column = Column::done();
        edited.task.priority = 4;
        let err = plan(&base, &edited).unwrap_err().to_string();
        assert!(err.contains("`column` (use `shelbi issue move`)"), "{err}");
        assert!(err.contains("`priority` (use `shelbi issue prio`)"), "{err}");
    }

    #[test]
    fn an_overlap_is_refused_only_when_the_values_differ() {
        let base = issue_file("body\n");
        let mut mine = base.clone();
        mine.task.branch = Some("mine".into());
        let ours = frontmatter_changes(&base.task, &mine.task).unwrap();

        let mut other_field = base.clone();
        other_field.task.workflow = Some("w".into());
        let theirs = frontmatter_changes(&base.task, &other_field.task).unwrap();
        refuse_overlap(&ours, &theirs).unwrap();

        let same = frontmatter_changes(&base.task, &mine.task).unwrap();
        refuse_overlap(&ours, &same).unwrap();

        let mut other_value = base.clone();
        other_value.task.branch = Some("theirs".into());
        let theirs = frontmatter_changes(&base.task, &other_value.task).unwrap();
        let err = refuse_overlap(&ours, &theirs).unwrap_err().to_string();
        assert!(err.contains("`branch` changed"), "{err}");
    }

    #[test]
    fn param_flags_set_and_clear_by_key() {
        let changes = param_changes(
            &["task=parent".to_string(), "expr=a=b".to_string()],
            &["old".to_string()],
        )
        .unwrap();
        assert_eq!(
            changes,
            BTreeMap::from([
                ("expr".to_string(), Some(serde_json::json!("a=b"))),
                ("old".to_string(), None),
                ("task".to_string(), Some(serde_json::json!("parent"))),
            ])
        );
    }

    #[test]
    fn param_flags_refuse_what_wouldnt_read_back_as_a_param() {
        let err = |set: &[&str], clear: &[&str]| {
            let set: Vec<String> = set.iter().map(|s| s.to_string()).collect();
            let clear: Vec<String> = clear.iter().map(|s| s.to_string()).collect();
            param_changes(&set, &clear).unwrap_err().to_string()
        };
        assert!(err(&["task"], &[]).contains("KEY=VALUE"));
        assert!(err(&["task="], &[]).contains("--no-param task"));
        assert!(err(&["=x"], &[]).contains("invalid param name"));
        assert!(err(&["a b=x"], &[]).contains("invalid param name"));
        assert!(err(&["branch=x"], &[]).contains("issue field"));
        assert!(err(&[], &["column"]).contains("issue field"));
        assert!(err(&["a=1", "a=2"], &[]).contains("more than once"));
        assert!(err(&["a=1"], &["a"]).contains("more than once"));
    }

    #[test]
    fn a_draft_is_named_after_the_issue_and_safe_as_a_file_name() {
        assert_eq!(draft_file_name("fix-login"), "fix-login.md");
        assert_eq!(draft_file_name("../#7"), "----7.md");
    }
}
