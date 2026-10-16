//! Markdown for the task detail.
//!
//! The body itself renders with gpui-kit's Markdown text view
//! (`gpui_kit::component::text::TextView::markdown`), which parses with the
//! `markdown` crate under GitHub-flavored options. This module reads the same
//! parse for the one thing the detail header shows about the body, its
//! checklist progress, and its tests pin that the shipped task-body templates
//! parse into the blocks the view renders (headings, lists, checkboxes, code,
//! links) rather than plain text.

use markdown::mdast::Node;
use markdown::ParseOptions;

/// GitHub-flavored Markdown, as gpui-kit's text view parses it (it adds math,
/// which task bodies don't use).
fn options() -> ParseOptions {
    ParseOptions::gfm()
}

/// `(checked, total)` over the body's task-list items (`- [ ]` / `- [x]`), or
/// `None` when it has none.
pub fn checklist(body: &str) -> Option<(usize, usize)> {
    let root = markdown::to_mdast(body, &options()).ok()?;
    let mut done = 0;
    let mut total = 0;
    walk(&root, &mut |node| {
        if let Node::ListItem(item) = node {
            if let Some(checked) = item.checked {
                total += 1;
                if checked {
                    done += 1;
                }
            }
        }
    });
    (total > 0).then_some((done, total))
}

fn walk(node: &Node, f: &mut impl FnMut(&Node)) {
    f(node);
    if let Some(children) = node.children() {
        for child in children {
            walk(child, f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The block kinds a body parses into, in document order.
    fn kinds(body: &str) -> Vec<String> {
        let root = markdown::to_mdast(body, &options()).unwrap();
        let mut out = Vec::new();
        walk(&root, &mut |node| {
            let kind = match node {
                Node::Heading(h) => format!("h{}", h.depth),
                Node::List(_) => "list".into(),
                Node::ListItem(i) => match i.checked {
                    Some(true) => "checked".into(),
                    Some(false) => "unchecked".into(),
                    None => "item".into(),
                },
                Node::Code(_) => "code".into(),
                Node::InlineCode(_) => "inline-code".into(),
                Node::Link(l) => format!("link:{}", l.url),
                _ => return,
            };
            out.push(kind);
        });
        out
    }

    /// The orchestrator's Feature template (`default_orchestrator.md.template`,
    /// "Task body templates"), filled in.
    const FEATURE: &str = "\
# Board keyboard navigation

Move between columns and cards with the TUI's keys.

## Wireframes

```
| BACKLOG | TO DO | IN PROGRESS |
```

## Technical Details

- Keys come from `keys.yaml`
- See [the plan](https://example.com/plan)

## Acceptance Criteria
- [ ] h/l move between columns
- [x] j/k move within a column
";

    /// The Bug template, filled in.
    const BUG: &str = "\
# Done column flickers

The done column blanks on refresh.

## Current Behavior

1. Open the board
2. Wait a tick

## Expected Behavior

The column keeps its cards.

## Acceptance Criteria
- [ ] The done column never blanks
";

    #[test]
    fn the_feature_template_renders_headings_lists_code_links_and_checkboxes() {
        assert_eq!(
            kinds(FEATURE),
            [
                "h1",
                "h2",
                "code",
                "h2",
                "list",
                "item",
                "inline-code",
                "item",
                "link:https://example.com/plan",
                "h2",
                "list",
                "unchecked",
                "checked",
            ]
        );
        assert_eq!(checklist(FEATURE), Some((1, 2)));
    }

    #[test]
    fn the_bug_template_renders_its_sections() {
        assert_eq!(
            kinds(BUG),
            ["h1", "h2", "list", "item", "item", "h2", "h2", "list", "unchecked"]
        );
        assert_eq!(checklist(BUG), Some((0, 1)));
    }

    #[test]
    fn the_welcome_card_has_a_heading_and_no_checklist() {
        assert_eq!(kinds(shelbi_state::WELCOME_TASK_BODY), ["h1"]);
        assert_eq!(checklist(shelbi_state::WELCOME_TASK_BODY), None);
    }
}
