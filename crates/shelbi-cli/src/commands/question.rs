//! `shelbi question <subcommand>` — read the clarification questions workers
//! have asked, from the hub's durable store (`shelbi_state::questions`).
//!
//! The events stream carries only a preview of each question; this is where
//! the orchestrator (or a human) reads one in full, and where a reopened
//! orchestrator finds the questions nobody has answered yet. Answering stays
//! `shelbi message <task> reply --in-response-to <id> "…"`, which marks the
//! stored question answered.

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use shelbi_state::questions::{self, Question};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum QuestionCmd {
    /// List the project's clarification questions, open ones by default.
    List {
        /// Only questions asked by this task.
        #[arg(long)]
        task: Option<String>,
        /// Include answered questions.
        #[arg(long)]
        all: bool,
        /// Print the questions (full text included) as a JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Print one question in full.
    Show {
        /// Question id, as on the events line (`question=<id>`). Accepts
        /// `<task>/<id>` too.
        id: String,
        /// Task that asked it. Needed only when two tasks used the same id.
        #[arg(long)]
        task: Option<String>,
        /// Print the question as JSON.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(project_opt: Option<String>, cmd: QuestionCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        QuestionCmd::List { task, all, json } => {
            let mut qs = all_questions(&project)?;
            qs.retain(|q| all || !q.is_answered());
            if let Some(task) = task.as_deref() {
                let task = questions::sanitize_question_id(task);
                qs.retain(|q| q.meta.task == task);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&qs)?);
            } else {
                print!("{}", render_list(&qs, all));
            }
            Ok(())
        }
        QuestionCmd::Show { id, task, json } => {
            let (task, id) = match (task, id.split_once('/')) {
                (Some(task), _) => (Some(task), id),
                (None, Some((task, id))) => (Some(task.to_string()), id.to_string()),
                (None, None) => (None, id),
            };
            let q = find_question(&project, task.as_deref(), &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&q)?);
            } else {
                print!("{}", render_show(&q));
            }
            Ok(())
        }
    }
}

/// The project's questions plus the hub-level ones (stored there when the
/// asking worker's project couldn't be resolved), oldest first.
fn all_questions(project: &str) -> Result<Vec<Question>> {
    let mut qs = questions::list_questions(Some(project)).map_err(|e| anyhow!(e))?;
    qs.extend(questions::list_questions(None).map_err(|e| anyhow!(e))?);
    qs.sort_by(|a, b| a.meta.asked_at.cmp(&b.meta.asked_at));
    Ok(qs)
}

fn find_question(project: &str, task: Option<&str>, id: &str) -> Result<Question> {
    if let Some(task) = task {
        return questions::load_question(Some(project), task, id)
            .map_err(|e| anyhow!(e))?
            .ok_or_else(|| anyhow!("no question `{id}` for task `{task}` in project `{project}`"));
    }
    let id = questions::sanitize_question_id(id);
    let mut matches: Vec<Question> = all_questions(project)?
        .into_iter()
        .filter(|q| q.meta.id == id)
        .collect();
    match matches.len() {
        0 => bail!("no question `{id}` in project `{project}` (see `shelbi question list --all`)"),
        1 => Ok(matches.remove(0)),
        _ => {
            let tasks: Vec<&str> = matches.iter().map(|q| q.meta.task.as_str()).collect();
            bail!(
                "question id `{id}` was used by several tasks ({}); pass `--task <task>` \
                 or `<task>/{id}`",
                tasks.join(", ")
            )
        }
    }
}

fn status(q: &Question) -> &'static str {
    if q.is_answered() {
        "answered"
    } else {
        "open"
    }
}

/// First line of the question, cut to `max` chars.
fn preview(text: &str, max: usize) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if first.chars().count() > max {
        let cut: String = first.chars().take(max).collect();
        format!("{cut}…")
    } else if text.trim().lines().count() > 1 {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

fn render_list(qs: &[Question], all: bool) -> String {
    if qs.is_empty() {
        return if all {
            "no questions\n".into()
        } else {
            "no open questions\n".into()
        };
    }
    let mut out = String::new();
    for q in qs {
        out.push_str(&format!(
            "{:<10} {}/{}  {}  {}  {}\n",
            status(q),
            q.meta.task,
            q.meta.id,
            q.meta.asked_at,
            q.meta.workspace.as_deref().unwrap_or("-"),
            preview(&q.text, 80),
        ));
    }
    out
}

fn render_show(q: &Question) -> String {
    let mut out = format!("question {} (task {})\n", q.meta.id, q.meta.task);
    if let Some(ws) = &q.meta.workspace {
        out.push_str(&format!("workspace: {ws}\n"));
    }
    out.push_str(&format!("asked:     {}\n", q.meta.asked_at));
    match (&q.meta.answered_at, &q.meta.answer_msg_id) {
        (Some(at), Some(msg)) => out.push_str(&format!("answered:  {at} ({msg})\n")),
        (Some(at), None) => out.push_str(&format!("answered:  {at}\n")),
        _ => out.push_str("status:    open\n"),
    }
    out.push_str(&format!("file:      {}\n", q.path.display()));
    if let Some(ctx) = &q.meta.context {
        out.push_str(&format!("context:   {ctx}\n"));
    }
    out.push('\n');
    out.push_str(&q.text);
    out.push('\n');
    if !q.is_answered() {
        out.push_str(&format!(
            "\nanswer with: shelbi message {} reply --in-response-to {} \"<answer>\"\n",
            q.meta.task, q.meta.id
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;
    use shelbi_state::questions::{mark_question_answered, record_question, NewQuestion};

    struct Root {
        _tmp: tempfile::TempDir,
    }
    impl Drop for Root {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_ROOT");
        }
    }

    fn setup() -> (std::sync::MutexGuard<'static, ()>, Root) {
        let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::env::remove_var("SHELBI_HOME");
        std::env::set_var("SHELBI_ROOT", tmp.path());
        (guard, Root { _tmp: tmp })
    }

    fn ask(project: Option<&str>, task: &str, id: &str, text: &str) -> Question {
        record_question(NewQuestion {
            project,
            task,
            id,
            workspace: Some("charlie"),
            text,
            context: None,
        })
        .unwrap()
    }

    #[test]
    fn show_finds_by_id_and_disambiguates_reused_ids() {
        let (_g, _root) = setup();
        ask(Some("demo"), "a", "q-001", "first line\nsecond line");
        ask(Some("demo"), "b", "q-001", "other");
        ask(Some("demo"), "b", "q-002", "only");
        ask(None, "c", "q-009", "hub-level");

        assert_eq!(find_question("demo", None, "q-002").unwrap().text, "only");
        assert_eq!(find_question("demo", None, "q-009").unwrap().meta.task, "c");
        let err = find_question("demo", None, "q-001").unwrap_err().to_string();
        assert!(err.contains("a, b") && err.contains("--task"), "{err}");
        assert_eq!(
            find_question("demo", Some("a"), "q-001").unwrap().text,
            "first line\nsecond line"
        );
        assert!(find_question("demo", None, "q-404").is_err());
    }

    #[test]
    fn list_hides_answered_questions_unless_all() {
        let (_g, _root) = setup();
        ask(Some("demo"), "a", "q-1", "open one?");
        ask(Some("demo"), "a", "q-2", "answered one?");
        mark_question_answered(Some("demo"), "a", "q-2", "m-1").unwrap();

        let mut qs = all_questions("demo").unwrap();
        qs.retain(|q| !q.is_answered());
        let out = render_list(&qs, false);
        assert!(out.contains("open       a/q-1"), "{out}");
        assert!(!out.contains("q-2"), "{out}");
        assert_eq!(render_list(&[], false), "no open questions\n");

        let full = render_show(&find_question("demo", None, "q-1").unwrap());
        assert!(full.contains("open one?"), "{full}");
        assert!(full.contains("shelbi message a reply --in-response-to q-1"), "{full}");
        let answered = render_show(&find_question("demo", None, "q-2").unwrap());
        assert!(answered.contains("(m-1)") && !answered.contains("answer with"), "{answered}");
    }

    #[test]
    fn preview_takes_the_first_line_and_marks_elision() {
        assert_eq!(preview("short", 80), "short");
        assert_eq!(preview("one\ntwo", 80), "one …");
        assert_eq!(preview(&"x".repeat(100), 10), format!("{}…", "x".repeat(10)));
    }
}
