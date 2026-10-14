//! Durable clarification questions
//! (`<project_dir>/questions/<task-id>/<question-id>.md`).
//!
//! A worker asks the user something by sending `request-clarification` to the
//! hub. The events stream only has room for a short preview of the question
//! (one bounded line per record), so the hub daemon writes the full text here
//! first and the event line points at it. The orchestrator reads it back
//! through `shelbi orchestrator events next` (the `question` record), or
//! `shelbi question show`, and a reply sent with
//! `shelbi message <task> reply --in-response-to <id>` stamps `answered_at`, so
//! `shelbi question list` can tell a reopened orchestrator which questions are
//! still open.
//!
//! Files are grouped per task because question ids are minted by the worker
//! and are only unique per task (every worker's first question is `q-001`).
//! A question whose `project` the hub can't resolve lands in the hub-level
//! `<root>/questions/` instead of being dropped.
//!
//! Each file is markdown with YAML frontmatter, the same shape as task files:
//!
//! ```text
//! ---
//! id: q-001
//! task: feat-x
//! project: shelbi
//! workspace: charlie
//! kind: clarification
//! asked_at: 2026-10-14T10:00:00Z
//! ---
//! <the full question text>
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::{Error, Result};

/// Directory name under the project state dir (or the hub root, for a question
/// with no resolvable project).
pub const QUESTIONS_DIR: &str = "questions";

/// The `kind` every worker clarification is stored with.
pub const CLARIFICATION_KIND: &str = "clarification";

/// Frontmatter of a stored question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionMeta {
    /// Question id as stored: the worker's id, sanitized for use as a file
    /// name, plus a `-N` suffix when the worker reused an id for a different
    /// question on the same task.
    pub id: String,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub kind: String,
    pub asked_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
    /// `msg_id` of the `shelbi message ... --in-response-to` reply that
    /// answered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_msg_id: Option<String>,
    /// The optional `context` excerpt the worker sent alongside the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// A stored question: its frontmatter, full text, and the file it lives in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    #[serde(flatten)]
    pub meta: QuestionMeta,
    pub text: String,
    pub path: PathBuf,
}

impl Question {
    pub fn is_answered(&self) -> bool {
        self.meta.answered_at.is_some()
    }
}

/// A question as received from a worker, before it is stored.
#[derive(Debug, Clone, Copy)]
pub struct NewQuestion<'a> {
    /// The project the question belongs to, or `None` to store it in the
    /// hub-level questions dir.
    pub project: Option<&'a str>,
    pub task: &'a str,
    pub id: &'a str,
    pub workspace: Option<&'a str>,
    pub text: &'a str,
    pub context: Option<&'a str>,
}

/// Questions root for `project`, or the hub-level root for `None`.
pub fn questions_root(project: Option<&str>) -> Result<PathBuf> {
    match project {
        Some(p) => Ok(crate::project_dir(p)?.join(QUESTIONS_DIR)),
        None => Ok(crate::shelbi_home()?.join(QUESTIONS_DIR)),
    }
}

/// Sanitize a worker-supplied id to the characters the events log keeps for
/// identifiers, then reject anything that is not a single normal path
/// component (`.`/`..`), so neither a task id nor a question id can escape the
/// questions dir. The events line carries the same sanitized form, so the id
/// an operator copies from the stream always names the file.
pub fn sanitize_question_id(raw: &str) -> String {
    crate::event_log::sanitize_field(raw)
}

fn checked_component(kind: &str, raw: &str) -> Result<String> {
    let s = sanitize_question_id(raw);
    if s.is_empty() {
        return Err(Error::Other(format!("empty {kind}")));
    }
    crate::ensure_flat_path_component(kind, &s)?;
    Ok(s)
}

/// Path of the file for `(task, id)` under `project`'s questions root.
pub fn question_path(project: Option<&str>, task: &str, id: &str) -> Result<PathBuf> {
    let task = checked_component("task id", task)?;
    let id = checked_component("question id", id)?;
    Ok(questions_root(project)?.join(task).join(format!("{id}.md")))
}

/// Store a worker's question durably and return it as stored.
///
/// Re-sending the identical question under the same id is a retry and returns
/// the existing record untouched (its `asked_at` and any answer survive). A
/// *different* question under an id the task already used is stored as
/// `<id>-2`, `<id>-3`, … so the earlier text is never overwritten; the caller
/// reports the returned id on the events stream.
pub fn record_question(q: NewQuestion<'_>) -> Result<Question> {
    let task = checked_component("task id", q.task)?;
    let base_id = checked_component("question id", q.id)?;
    let dir = questions_root(q.project)?.join(&task);

    let mut n = 1u32;
    let (id, path) = loop {
        let id = if n == 1 {
            base_id.clone()
        } else {
            format!("{base_id}-{n}")
        };
        let path = dir.join(format!("{id}.md"));
        match read_question(&path)? {
            None => break (id, path),
            Some(existing) if existing.text == q.text => return Ok(existing),
            Some(_) => n += 1,
        }
    };

    let question = Question {
        meta: QuestionMeta {
            id,
            task,
            project: q.project.map(str::to_string),
            workspace: q.workspace.map(str::to_string),
            kind: CLARIFICATION_KIND.to_string(),
            asked_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            answered_at: None,
            answer_msg_id: None,
            context: q.context.filter(|c| !c.is_empty()).map(str::to_string),
        },
        text: q.text.to_string(),
        path,
    };
    write_question(&question)?;
    Ok(question)
}

fn write_question(q: &Question) -> Result<()> {
    let front = serde_yaml::to_string(&q.meta)?;
    // The body always gets exactly one trailing newline, which `read_question`
    // strips, so the text round-trips byte for byte.
    let doc = format!("---\n{front}---\n{}\n", q.text);
    crate::atomic_write(&q.path, doc.as_bytes())
}

/// Read one question file. `Ok(None)` when the file does not exist.
pub fn read_question(path: &Path) -> Result<Option<Question>> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(crate::annotate_io_error(path, e).into()),
    };
    let (front, body) = crate::split_frontmatter(&raw).ok_or_else(|| {
        Error::Other(format!("question file has no frontmatter: {}", path.display()))
    })?;
    let meta: QuestionMeta = serde_yaml::from_str(front)?;
    let text = body.strip_suffix('\n').unwrap_or(body).to_string();
    Ok(Some(Question {
        meta,
        text,
        path: path.to_path_buf(),
    }))
}

/// Load `(task, id)` from `project`'s questions, falling back to the hub-level
/// root (where a question lands when its project could not be resolved).
pub fn load_question(project: Option<&str>, task: &str, id: &str) -> Result<Option<Question>> {
    if let Some(q) = read_question(&question_path(project, task, id)?)? {
        return Ok(Some(q));
    }
    if project.is_some() {
        return read_question(&question_path(None, task, id)?);
    }
    Ok(None)
}

/// Every question under one root, oldest first. A file that fails to parse is
/// skipped (with a warning) rather than hiding every other question.
pub fn list_questions(project: Option<&str>) -> Result<Vec<Question>> {
    let root = questions_root(project)?;
    let mut out = Vec::new();
    let task_dirs = match fs::read_dir(&root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(crate::annotate_io_error(&root, e).into()),
    };
    for task_dir in task_dirs {
        let task_dir = task_dir?.path();
        if !task_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&task_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            match read_question(&path) {
                Ok(Some(q)) => out.push(q),
                Ok(None) => {}
                Err(e) => tracing::warn!(path = %path.display(), error = %e, "skipping unreadable question file"),
            }
        }
    }
    out.sort_by(|a, b| {
        (a.meta.asked_at.as_str(), a.meta.task.as_str(), a.meta.id.as_str()).cmp(&(
            b.meta.asked_at.as_str(),
            b.meta.task.as_str(),
            b.meta.id.as_str(),
        ))
    });
    Ok(out)
}

/// Record that `(task, id)` was answered by message `msg_id`. Returns the
/// updated question, or `None` when no such question is stored (a reply to a
/// question asked before questions were persisted, or a mistyped id). An
/// already-answered question keeps its first answer.
pub fn mark_question_answered(
    project: Option<&str>,
    task: &str,
    id: &str,
    msg_id: &str,
) -> Result<Option<Question>> {
    let Some(mut q) = load_question(project, task, id)? else {
        return Ok(None);
    };
    if q.meta.answered_at.is_none() {
        q.meta.answered_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
        q.meta.answer_msg_id = Some(msg_id.to_string());
        write_question(&q)?;
    }
    Ok(Some(q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    struct Home {
        dir: PathBuf,
    }
    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn fresh_home() -> Home {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-questions-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SHELBI_HOME", &dir);
        Home { dir }
    }

    fn ask<'a>(project: Option<&'a str>, task: &'a str, id: &'a str, text: &'a str) -> NewQuestion<'a> {
        NewQuestion {
            project,
            task,
            id,
            workspace: Some("charlie"),
            text,
            context: None,
        }
    }

    #[test]
    fn long_question_round_trips_in_full() {
        let _g = TEST_LOCK.lock().unwrap();
        let _home = fresh_home();
        // 2 KB with newlines, quotes, and a `---` line that must not be read
        // as a frontmatter fence on the way back.
        let text = format!(
            "The spec assumes behavior the TUI does not have.\n---\n\"Defaults\": {}\nend",
            "x".repeat(2048)
        );
        let q = record_question(ask(Some("demo"), "feat-x", "q-001", &text)).unwrap();
        assert_eq!(q.meta.id, "q-001");
        assert!(q.path.ends_with("projects/demo/questions/feat-x/q-001.md"), "{:?}", q.path);

        let back = load_question(Some("demo"), "feat-x", "q-001").unwrap().unwrap();
        assert_eq!(back.text, text);
        assert_eq!(back.meta.workspace.as_deref(), Some("charlie"));
        assert_eq!(back.meta.kind, CLARIFICATION_KIND);
        assert!(!back.is_answered());
    }

    #[test]
    fn identical_resend_is_idempotent_and_a_reused_id_never_overwrites() {
        let _g = TEST_LOCK.lock().unwrap();
        let _home = fresh_home();
        let first = record_question(ask(Some("demo"), "t", "q-001", "first?")).unwrap();
        let retry = record_question(ask(Some("demo"), "t", "q-001", "first?")).unwrap();
        assert_eq!(retry, first);

        let second = record_question(ask(Some("demo"), "t", "q-001", "second?")).unwrap();
        assert_eq!(second.meta.id, "q-001-2");
        assert_eq!(
            load_question(Some("demo"), "t", "q-001").unwrap().unwrap().text,
            "first?"
        );
        // The same id on another task is a different question, not a reuse.
        let other = record_question(ask(Some("demo"), "u", "q-001", "other?")).unwrap();
        assert_eq!(other.meta.id, "q-001");
    }

    #[test]
    fn ids_cannot_escape_the_questions_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let _home = fresh_home();
        assert!(record_question(ask(Some("demo"), "..", "q", "?")).is_err());
        assert!(record_question(ask(Some("demo"), "t", "..", "?")).is_err());
        // Separators fold to `_` (the events-log identifier rule), so the id
        // stays one component and matches what the event line carries.
        let q = record_question(ask(Some("demo"), "t", "a/b c", "?")).unwrap();
        assert_eq!(q.meta.id, "a_b_c");
        assert!(q.path.ends_with("questions/t/a_b_c.md"));
    }

    #[test]
    fn answering_marks_the_question_and_list_reports_open_ones() {
        let _g = TEST_LOCK.lock().unwrap();
        let _home = fresh_home();
        record_question(ask(Some("demo"), "t", "q-1", "one?")).unwrap();
        record_question(ask(Some("demo"), "t", "q-2", "two?")).unwrap();

        let answered = mark_question_answered(Some("demo"), "t", "q-1", "m-9")
            .unwrap()
            .unwrap();
        assert!(answered.is_answered());
        assert_eq!(answered.meta.answer_msg_id.as_deref(), Some("m-9"));
        // A second reply keeps the first answer.
        let again = mark_question_answered(Some("demo"), "t", "q-1", "m-10")
            .unwrap()
            .unwrap();
        assert_eq!(again.meta.answer_msg_id.as_deref(), Some("m-9"));
        assert!(mark_question_answered(Some("demo"), "t", "nope", "m").unwrap().is_none());

        let all = list_questions(Some("demo")).unwrap();
        assert_eq!(all.len(), 2);
        let open: Vec<_> = all.iter().filter(|q| !q.is_answered()).collect();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].meta.id, "q-2");
    }

    #[test]
    fn unscoped_questions_are_found_through_a_project_lookup() {
        let _g = TEST_LOCK.lock().unwrap();
        let _home = fresh_home();
        let q = record_question(ask(None, "t", "q-1", "where?")).unwrap();
        assert!(q.path.ends_with("questions/t/q-1.md"));
        assert!(!q.path.to_string_lossy().contains("projects"));
        assert_eq!(
            load_question(Some("demo"), "t", "q-1").unwrap().unwrap().text,
            "where?"
        );
        assert!(mark_question_answered(Some("demo"), "t", "q-1", "m-1")
            .unwrap()
            .unwrap()
            .is_answered());
        assert_eq!(list_questions(None).unwrap().len(), 1);
        assert!(list_questions(Some("demo")).unwrap().is_empty());
    }
}
