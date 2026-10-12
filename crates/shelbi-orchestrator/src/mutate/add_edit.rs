//! `add` and `edit`, ported from `shelbi-cli`'s `commands/issue.rs`.
//!
//! The client resolves everything that needs its own filesystem/stdin/`$EDITOR`
//! — reading piped stdin or `--body-file`, choosing the single body source, and
//! launching `$EDITOR` for a flagless `edit` — and hands the already-resolved
//! text to [`AddSpec`]/[`EditSpec`]. Everything else (id generation, body
//! composition, substitutions, field validation, the store write, and the wake
//! event) lives here, so the daemon and the in-process path behave identically.

use std::str::FromStr;

use shelbi_core::{
    validate_branch, validate_task_id, validate_workflow_name, Column, Issue, StatusCategory,
    Workflow, MAX_TASK_ID_LEN,
};
use shelbi_proto::control::{AddSpec, EditSpec, SubOp};

use super::{issue_store, load_issue, MutateError, OutputSink, Recheck};

/// Create a new issue. Returns the created id (for the change notification).
pub(crate) fn add(
    project: &str,
    spec: &AddSpec,
    sink: &mut dyn OutputSink,
    _recheck: Recheck<'_>,
) -> Result<String, MutateError> {
    let column = Column::from_str(&spec.status).map_err(MutateError::backend)?;
    let id = match &spec.id {
        Some(id) => {
            validate_task_id(id).map_err(MutateError::backend)?;
            if shelbi_state::task_path(project, id)
                .map_err(MutateError::backend)?
                .exists()
            {
                return Err(MutateError::Backend(format!("issue id `{id}` already exists")));
            }
            id.clone()
        }
        None => generate_unique_id(project, &spec.title)?,
    };

    if let Some(name) = spec.workflow.as_deref() {
        validate_workflow_name(name).map_err(MutateError::backend)?;
    }
    // Body precedence is resolved on the client into `spec.body`; here it is
    // simply "use it, or default to the title".
    let body = match &spec.body {
        Some(b) => format!("{}\n", b.trim_end()),
        None => format!("{}\n", spec.title),
    };

    let store = issue_store(project)?;
    let new = shelbi_state::NewIssue {
        id: id.clone(),
        title: spec.title.clone(),
        column: column.clone(),
        body,
        workflow: spec.workflow.clone(),
        branch: spec.branch.clone(),
        depends_on: dedup_preserving_order(spec.depends_on.clone()),
        prefers_machine: spec.prefers_machine.clone(),
        zen: None,
        launch: None,
        params: std::collections::BTreeMap::new(),
        priority: None,
    };
    let created = store.add(new).map_err(MutateError::backend)?;

    // Wake the orchestrator when a card is created directly into an agent-owned
    // status (not the quiet backlog inbox). Best-effort — the card is already
    // persisted, so a failed append only loses the wake signal.
    if column.category() != StatusCategory::Backlog {
        let project_yaml = shelbi_state::load_project(project).ok();
        let workflow_name = project_yaml
            .as_ref()
            .map(|p| shelbi_state::resolve_task_workflow_name(p, &created).to_string())
            .unwrap_or_else(|| created.workflow_or_default().to_string());
        if let Err(e) = shelbi_state::append_task_event(
            project,
            &created.id,
            &workflow_name,
            column.clone(),
            column.clone(),
            "user:cli:add",
        ) {
            sink.warn(&format!("warning: append_task_event failed: {e}"));
        }
    }

    sink.out(&format!(
        "✓ {} created in {column} (priority {})",
        created.id, created.priority
    ));
    Ok(created.id)
}

/// Revise an issue's content (non-interactive). The `$EDITOR` path stays on the
/// client; this is only reached once the client has resolved body sources.
pub(crate) fn edit(
    project: &str,
    id: &str,
    spec: &EditSpec,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    let has_body_source = spec.body_replace.is_some() || spec.body_append.is_some();
    if !spec.subs.is_empty() && has_body_source {
        return Err(MutateError::Backend(
            "--sub/--sub-regex operate in place and can't be combined with a whole-body \
             edit (--body/--body-file/stdin/--append)"
                .to_string(),
        ));
    }

    let store = issue_store(project)?;
    let mut tf = load_issue(store.as_ref(), id)?;
    let mut fields: Vec<&str> = Vec::new();

    // Validate every touched field BEFORE mutating, so invalid input leaves the
    // issue untouched.
    if let Some(name) = spec.workflow.as_deref() {
        validate_workflow_name(name).map_err(MutateError::backend)?;
        let path = shelbi_state::workflow_path(project, name).map_err(MutateError::backend)?;
        if !path.exists() {
            return Err(MutateError::Backend(format!(
                "workflow `{name}` does not exist (no {}) — create it first or pick an \
                 existing workflow",
                path.display()
            )));
        }
    }
    if let Some(branch) = spec.branch.as_deref() {
        validate_branch(branch).map_err(MutateError::backend)?;
    }

    // Recheck before any write (edit has no irreversible git step, but a queued
    // edit that raced an out-of-band move is refused rather than clobbering).
    recheck()?;

    // Compute the new body (substitutions OR a whole-body source).
    if !spec.subs.is_empty() {
        let (new_body, report) = apply_substitutions(&tf.body, &spec.subs, spec.allow_no_match)?;
        for (label, count) in &report {
            sink.out(&format!("  {label}: {count} replacement(s)"));
        }
        tf.body = new_body;
        fields.push("body");
    } else if let Some(raw) = &spec.body_replace {
        tf.body = format!("{}\n", raw.trim_end());
        fields.push("body");
    } else if let Some(raw) = &spec.body_append {
        tf.body = append_body(&tf.body, raw);
        fields.push("body");
    }

    // Apply the frontmatter field edits.
    if let Some(title) = &spec.title {
        tf.task.title = title.clone();
        fields.push("title");
    }
    if let Some(name) = &spec.workflow {
        tf.task.workflow = Some(name.clone());
        fields.push("workflow");
    }
    if let Some(branch) = &spec.branch {
        tf.task.branch = Some(branch.clone());
        fields.push("branch");
    }
    match &spec.prefers_machine {
        Some(Some(machine)) => {
            tf.task.prefers_machine = Some(machine.clone());
            fields.push("prefers_machine");
        }
        Some(None) => {
            tf.task.prefers_machine = None;
            fields.push("prefers_machine");
        }
        None => {}
    }

    if fields.is_empty() {
        sink.out("(no change)");
        return Ok(());
    }

    let mut updates = shelbi_state::IssueFields::default();
    if fields.contains(&"title") {
        updates.title = Some(tf.task.title.clone());
    }
    if fields.contains(&"workflow") {
        updates.workflow = Some(tf.task.workflow.clone());
    }
    if fields.contains(&"branch") {
        updates.branch = Some(tf.task.branch.clone());
    }
    if fields.contains(&"prefers_machine") {
        updates.prefers_machine = Some(tf.task.prefers_machine.clone());
    }
    if fields.contains(&"body") {
        updates.body = Some(tf.body.clone());
    }
    store.set_fields(id, updates).map_err(MutateError::backend)?;

    let fields_csv = fields.join(",");
    let reason = spec.reason.as_deref().unwrap_or("user:cli");
    if let Err(e) = shelbi_state::append_task_edit_event(project, id, &fields_csv, reason) {
        sink.warn(&format!("warning: append_task_edit_event failed: {e}"));
    }

    sink.out(&format!("✓ {id} edited ({fields_csv})"));
    if task_column_is_active(project, &tf.task) {
        sink.warn(&format!(
            "warning: `{id}` is in an active status ({}) — this edit will NOT reach the \
             running worker until the issue is re-dispatched (`shelbi issue start`)",
            tf.task.column
        ));
    }
    Ok(())
}

/// Append `text` to `existing`, with a blank-line separator (the `--append`
/// composition from the former `resolve_body_edit`).
fn append_body(existing: &str, text: &str) -> String {
    let text = text.trim_end();
    let mut b = existing.to_string();
    if !b.is_empty() {
        if !b.ends_with('\n') {
            b.push('\n');
        }
        if !b.ends_with("\n\n") {
            b.push('\n');
        }
    }
    b.push_str(text);
    b.push('\n');
    b
}

/// Apply ordered literal/regex substitutions to `body`, returning the new body
/// plus a per-op replacement count. Errors (writing nothing) on an invalid
/// regex, an empty literal/pattern, or — unless `allow_no_match` — a zero-match
/// substitution.
fn apply_substitutions(
    body: &str,
    ops: &[SubOp],
    allow_no_match: bool,
) -> Result<(String, Vec<(String, usize)>), MutateError> {
    let mut current = body.to_string();
    let mut report: Vec<(String, usize)> = Vec::new();
    for op in ops {
        let (label, count, next) = match op {
            SubOp::Literal { from, to } => {
                if from.is_empty() {
                    return Err(MutateError::Backend("--sub OLD must not be empty".to_string()));
                }
                let count = current.matches(from.as_str()).count();
                let next = current.replace(from.as_str(), to);
                (format!("`{from}` → `{to}`"), count, next)
            }
            SubOp::Regex {
                pattern,
                replacement,
            } => {
                if pattern.is_empty() {
                    return Err(MutateError::Backend(
                        "--sub-regex PATTERN must not be empty".to_string(),
                    ));
                }
                let re = regex::Regex::new(pattern).map_err(|e| {
                    MutateError::Backend(format!("invalid --sub-regex pattern `{pattern}`: {e}"))
                })?;
                let count = re.find_iter(&current).count();
                let next = re.replace_all(&current, replacement.as_str()).into_owned();
                (format!("/{pattern}/ → `{replacement}`"), count, next)
            }
        };
        if count == 0 && !allow_no_match {
            return Err(MutateError::Backend(format!(
                "substitution {label} matched zero occurrences — the issue body is \
                 unchanged (pass --allow-no-match to permit a no-op substitution)"
            )));
        }
        current = next;
        report.push((label, count));
    }
    Ok((current, report))
}

/// Whether the issue's current column is an `active`-category status.
fn task_column_is_active(project: &str, issue: &Issue) -> bool {
    if Column::core().contains(&issue.column) {
        return issue.column.category() == StatusCategory::Active;
    }
    resolve_workflow_quiet(project, issue)
        .and_then(|wf| {
            wf.status(issue.column.as_str())
                .map(|s| s.category == StatusCategory::Active)
        })
        .unwrap_or(false)
}

/// Load the issue's workflow without emitting a warning (the active-status hint
/// is advisory; a load failure just makes it read as non-active).
fn resolve_workflow_quiet(project: &str, issue: &Issue) -> Option<Workflow> {
    let project_yaml = shelbi_state::load_project(project).ok();
    let name = project_yaml
        .as_ref()
        .map(|p| shelbi_state::resolve_task_workflow_name(p, issue))
        .unwrap_or_else(|| issue.workflow_or_default());
    shelbi_state::load_workflow(project, name).ok()
}

/// Slugify a title to a kebab-case id, appending `-2`, `-3`, … on collision.
fn generate_unique_id(project: &str, title: &str) -> Result<String, MutateError> {
    let base = slugify(title);
    if base.is_empty() {
        return Err(MutateError::Backend(format!(
            "could not generate id from title `{title}` — pass --id explicitly"
        )));
    }
    let issues = shelbi_state::tasks_dir(project).map_err(MutateError::backend)?;
    let mut candidate = base.clone();
    let mut n: u32 = 2;
    while issues.join(format!("{candidate}.md")).exists() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    if candidate.len() > MAX_TASK_ID_LEN {
        return Err(MutateError::Backend(format!(
            "title is too long: it slugifies to a {}-byte id (max {MAX_TASK_ID_LEN}) — \
             the generated workspace branch would exceed GitHub's 255-byte ref limit. \
             Shorten the title or pass --id with an explicit shorter id.",
            candidate.len(),
        )));
    }
    Ok(candidate)
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_hyphen = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Stable de-dup preserving first-occurrence order.
fn dedup_preserving_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  --Trim--  "), "trim");
        assert_eq!(slugify("a_b c"), "a-b-c");
    }

    #[test]
    fn dedup_preserves_first_occurrence_order() {
        assert_eq!(
            dedup_preserving_order(vec!["a".into(), "a".into(), "b".into(), "a".into()]),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn apply_substitutions_literal_then_regex_in_order() {
        let (body, report) = apply_substitutions(
            "foo foo bar",
            &[
                SubOp::Literal {
                    from: "foo".into(),
                    to: "baz".into(),
                },
                SubOp::Regex {
                    pattern: r"ba(\w)".into(),
                    replacement: "B$1".into(),
                },
            ],
            false,
        )
        .unwrap();
        assert_eq!(body, "Bz Bz Br");
        assert_eq!(report[0].1, 2);
        assert_eq!(report[1].1, 3);
    }

    #[test]
    fn apply_substitutions_rejects_zero_match_unless_allowed() {
        let ops = [SubOp::Literal {
            from: "nope".into(),
            to: "x".into(),
        }];
        assert!(apply_substitutions("abc", &ops, false).is_err());
        assert!(apply_substitutions("abc", &ops, true).is_ok());
    }

    #[test]
    fn append_body_adds_blank_line_separator() {
        assert_eq!(append_body("Line one", "Added"), "Line one\n\nAdded\n");
        assert_eq!(append_body("", "Added"), "Added\n");
        assert_eq!(append_body("Para\n\n", "Added"), "Para\n\nAdded\n");
    }
}
