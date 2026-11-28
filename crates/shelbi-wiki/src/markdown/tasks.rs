// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-markdown/src/tasks.rs), MIT in Shelbi.

//! GFM checkbox tasks: enumeration plus substring-matching
//! `check` / `uncheck` toggles.

use comrak::nodes::NodeValue;
use comrak::Arena;

use crate::markdown::comrak_options;

/// A GFM checkbox task in a markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    /// Whether the task is checked (`- [x]`) or unchecked (`- [ ]`).
    pub checked: bool,

    /// Text after the `- [ ] ` prefix, with inline formatting flattened
    /// to plain text.
    pub text: String,

    /// 1-based line index of the task line in the source document.
    pub line: usize,
}

/// Every checkbox task in `content`, in source order.
pub fn list(content: &str) -> Vec<Task> {
    let arena = Arena::new();
    let options = comrak_options();
    let root = comrak::parse_document(&arena, content, &options);

    let mut tasks = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        if let NodeValue::TaskItem(marker) = &data.value {
            // comrak's TaskItem records the checkbox character (`x`,
            // `X`, or space) on the `symbol` field; `None` means the
            // GFM extension didn't recognize this as a task.
            let checked = marker.symbol.is_some_and(|c| c != ' ');
            let text = task_text(node);
            tasks.push(Task {
                checked,
                text,
                line: data.sourcepos.start.line,
            });
        }
    }
    tasks
}

/// Mark the task whose visible text contains `needle` (case-insensitive)
/// as checked (`- [x] ...`). Returns the updated document.
///
/// Errors with [`crate::markdown::MarkdownError::TaskNotFound`] when nothing
/// matches and [`crate::markdown::MarkdownError::AmbiguousTask`] when more than
/// one task matches the substring.
pub fn check(content: &str, needle: &str) -> crate::markdown::Result<String> {
    toggle(content, needle, true)
}

/// Mark the task whose visible text contains `needle` (case-insensitive)
/// as unchecked (`- [ ] ...`). Returns the updated document.
///
/// Same matching + ambiguity rules as [`check`].
pub fn uncheck(content: &str, needle: &str) -> crate::markdown::Result<String> {
    toggle(content, needle, false)
}

fn toggle(content: &str, needle: &str, check: bool) -> crate::markdown::Result<String> {
    let tasks = list(content);
    let lowered = needle.to_lowercase();
    let matches: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.text.to_lowercase().contains(&lowered))
        .collect();

    match matches.len() {
        0 => Err(crate::markdown::MarkdownError::TaskNotFound(needle.to_string())),
        1 => {
            let task = matches[0];
            let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
            let idx = task.line.saturating_sub(1);
            let original = lines[idx].clone();
            lines[idx] = if check {
                original.replacen("- [ ] ", "- [x] ", 1)
            } else {
                // Swift treats both `- [x]` and `- [X]` as checked.
                original
                    .replacen("- [x] ", "- [ ] ", 1)
                    .replacen("- [X] ", "- [ ] ", 1)
            };
            Ok(lines.join("\n"))
        }
        n => Err(crate::markdown::MarkdownError::AmbiguousTask {
            needle: needle.to_string(),
            count: n,
        }),
    }
}

fn task_text<'a>(
    item: &'a comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>,
) -> String {
    let mut out = String::new();
    for child in item.descendants().skip(1) {
        // skip(1) avoids re-visiting the TaskItem node itself
        let data = child.data.borrow();
        match &data.value {
            NodeValue::Text(t) => out.push_str(t),
            NodeValue::Code(c) => out.push_str(&c.literal),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_returns_checked_and_unchecked_tasks() {
        let md = "- [ ] todo one\n- [x] done two\n- [ ] todo three\n";
        let tasks = list(md);
        assert_eq!(tasks.len(), 3);
        assert!(!tasks[0].checked);
        assert_eq!(tasks[0].text, "todo one");
        assert!(tasks[1].checked);
        assert_eq!(tasks[1].text, "done two");
        assert!(!tasks[2].checked);
        assert_eq!(tasks[2].text, "todo three");
    }

    #[test]
    fn list_returns_empty_when_no_tasks_present() {
        assert!(list("# Just headings\n\nNo tasks here.\n").is_empty());
    }

    #[test]
    fn list_includes_tasks_in_nested_lists() {
        let md = "\
- top
  - [ ] nested
  - [x] also nested
";
        let tasks = list(md);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].text, "nested");
        assert!(!tasks[0].checked);
        assert_eq!(tasks[1].text, "also nested");
        assert!(tasks[1].checked);
    }

    #[test]
    fn list_carries_line_indices() {
        let md = "header\n\n- [ ] one\n- [x] two\n";
        let tasks = list(md);
        assert_eq!(tasks[0].line, 3);
        assert_eq!(tasks[1].line, 4);
    }

    // ─── Swift fixture port: MarkdownTasksTests ─────────────────────

    const TASK_DOCUMENT: &str = "\
# Todo

- [ ] Launch CLI
- [x] Ship editor
- [ ] Add search
- [X] Fix bug";

    #[test]
    fn list_finds_all_swift_fixture() {
        assert_eq!(list(TASK_DOCUMENT).len(), 4);
    }

    #[test]
    fn list_identifies_status_swift_fixture() {
        let tasks = list(TASK_DOCUMENT);
        assert_eq!(tasks[0].text, "Launch CLI");
        assert!(!tasks[0].checked);
        assert_eq!(tasks[1].text, "Ship editor");
        assert!(tasks[1].checked);
        assert_eq!(tasks[2].text, "Add search");
        assert!(!tasks[2].checked);
        assert_eq!(tasks[3].text, "Fix bug");
        assert!(tasks[3].checked);
    }

    #[test]
    fn list_handles_empty_swift_fixture() {
        assert!(list("# No tasks here").is_empty());
    }

    #[test]
    fn check_marks_complete_swift_fixture() {
        let updated = check(TASK_DOCUMENT, "Launch CLI").expect("check");
        assert!(updated.contains("- [x] Launch CLI"));
        assert!(!updated.contains("- [ ] Launch CLI"));
    }

    #[test]
    fn check_substring_match_swift_fixture() {
        let updated = check(TASK_DOCUMENT, "search").expect("check");
        assert!(updated.contains("- [x] Add search"));
    }

    #[test]
    fn check_case_insensitive_swift_fixture() {
        let updated = check(TASK_DOCUMENT, "LAUNCH CLI").expect("check");
        assert!(updated.contains("- [x] Launch CLI"));
    }

    #[test]
    fn check_throws_on_ambiguous_swift_fixture() {
        let doc = "- [ ] Build CLI\n- [ ] Test CLI tool";
        let err = check(doc, "CLI").expect_err("ambiguous");
        match err {
            crate::markdown::MarkdownError::AmbiguousTask { count, .. } => assert_eq!(count, 2),
            other => panic!("expected AmbiguousTask, got {other:?}"),
        }
    }

    #[test]
    fn check_throws_on_not_found_swift_fixture() {
        let err = check(TASK_DOCUMENT, "nonexistent").expect_err("not found");
        assert!(matches!(err, crate::markdown::MarkdownError::TaskNotFound(_)));
    }

    #[test]
    fn uncheck_marks_incomplete_swift_fixture() {
        let updated = uncheck(TASK_DOCUMENT, "Ship editor").expect("uncheck");
        assert!(updated.contains("- [ ] Ship editor"));
        assert!(!updated.contains("- [x] Ship editor"));
    }

    #[test]
    fn uncheck_handles_uppercase_x_swift_fixture() {
        let updated = uncheck(TASK_DOCUMENT, "Fix bug").expect("uncheck");
        assert!(updated.contains("- [ ] Fix bug"));
        assert!(!updated.contains("- [X] Fix bug"));
    }
}
