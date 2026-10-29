//! Markdown for the task pane.
//!
//! The body renders with gpui-kit's Markdown text view (gpui-base's
//! `TextView::markdown`), which parses with the `markdown` crate under
//! GitHub-flavored options, styled after the Figma Task pane frames
//! ([`body`]): headings in `Desktop/Heading` (h1) and `Desktop/Base/Semibold`
//! (the rest: "Description", "Acceptance criteria") in the loud text role,
//! 12pt over what they head; the body in `Desktop/Base/Regular`, 20pt
//! between blocks; and a task list drawn as Figma's checkbox rows (a custom
//! block, [`CHECKLIST`]). Its tests pin that the shipped task-body templates
//! parse into the blocks the view renders (headings, lists, checkboxes,
//! code, links) rather than plain text.

use gpui::{
    div, prelude::*, px, rems, Div, ElementId, HighlightStyle, Hsla,
    SharedString, StyleRefinement,
};
use gpui_kit::base::text::{MarkdownNode, MarkdownParseContext, TextView, TextViewStyle};
use markdown::mdast::Node;
use shelbi_app::theme::Appearance;

use crate::elements::controls;
use crate::theme::style::{Type, Typography};
use crate::theme::Theme;

/// GitHub-flavored Markdown, as gpui-kit's text view parses it (it adds math,
/// which task bodies don't use).
#[cfg(test)]
fn options() -> markdown::ParseOptions {
    markdown::ParseOptions::gfm()
}

/// The room between two blocks of the body (the Task pane's 20pt section
/// gap), in rems of gpui's 16px.
const BLOCK_GAP: f32 = 20.0;
/// The room under a heading, before what it heads.
const HEADING_GAP: f32 = 12.0;
/// The room between a task list's rows.
const ROW_GAP: f32 = 12.0;

/// The custom block a task list renders as.
const CHECKLIST: &str = "shelbi-checklist";

/// The detail body: `text` as Markdown in the Figma Issue detail's styles,
/// selectable, its task lists as checkbox rows.
pub fn body(id: impl Into<ElementId>, text: impl Into<SharedString>, theme: &Theme) -> TextView {
    let style = text_style(theme);
    let theme = *theme;
    let items = style.clone();
    TextView::markdown(id, text)
        .style(style)
        .markdown_block_parser(parse_checklist)
        .markdown_block_renderer(CHECKLIST, move |node, _, _| render_checklist(node, &items, &theme))
}

/// The body's text style: foreground body text, loud headings, the palette's
/// link, selection and rules.
fn text_style(theme: &Theme) -> TextViewStyle {
    let loud = theme.text_loud;
    TextViewStyle::default()
        .with_foreground(theme.foreground)
        .with_muted_foreground(theme.muted)
        .with_link(theme.link)
        .with_selection(theme.text_selection)
        .with_code_background(theme.background)
        .with_border(theme.divider)
        .with_paragraph_gap(rems(BLOCK_GAP / 16.0))
        .with_inline_code(HighlightStyle {
            background_color: Some(theme.background),
            ..Default::default()
        })
        .with_heading(move |level| heading(level, loud))
        .with_dark(theme.appearance == Appearance::Dark)
}

/// A heading of `level`: `Desktop/Heading` for the first level,
/// `Desktop/Base/Semibold` below it (the pane's "Acceptance criteria"), in
/// the loud role. A heading drops the text view's section gaps for the
/// pane's 12pt under it (the block above already leaves its gap).
fn heading(level: u8, color: Hsla) -> StyleRefinement {
    let ty = if level == 1 { Type::Heading } else { Type::BaseSemibold };
    StyleRefinement::default()
        .type_style(ty)
        .text_color(color)
        .pt(px(0.0))
        .pb(px(HEADING_GAP))
}

/// One row of a task list.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChecklistItem {
    checked: bool,
    /// The item's text, as Markdown (it can hold code or a link).
    markdown: String,
}

/// A task list as the [`CHECKLIST`] block, when every item of the list is a
/// task-list item holding at most one paragraph. Any other list (a plain
/// one, a mixed one, one with nested blocks) stays the text view's own.
fn parse_checklist(node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
    let items = checklist_items(node, |n| cx.node_source(n))?;
    let text = items
        .iter()
        .map(|item| format!("[{}] {}", if item.checked { "x" } else { " " }, item.markdown))
        .collect::<Vec<_>>()
        .join("\n");
    let source = cx.node_source(node).unwrap_or_default().to_string();
    Some(MarkdownNode::new(CHECKLIST, items).text(text).markdown(source))
}

fn checklist_items<'a>(node: &Node, source: impl Fn(&Node) -> Option<&'a str>) -> Option<Vec<ChecklistItem>> {
    let Node::List(list) = node else {
        return None;
    };
    if list.children.is_empty() {
        return None;
    }
    list.children
        .iter()
        .map(|child| {
            let Node::ListItem(item) = child else {
                return None;
            };
            let checked = item.checked?;
            let markdown = match item.children.as_slice() {
                [] => String::new(),
                [paragraph @ Node::Paragraph(_)] => source(paragraph)?.to_string(),
                _ => return None,
            };
            Some(ChecklistItem { checked, markdown })
        })
        .collect()
}

/// A task list as the frame draws it: one 18pt row per item, 12pt apart,
/// each a 14pt checkbox and the item's text.
fn render_checklist(node: &MarkdownNode, style: &TextViewStyle, theme: &Theme) -> Div {
    let items = node.data::<Vec<ChecklistItem>>().cloned().unwrap_or_default();
    let start = node.source_range().map_or(0, |r| r.start);
    div()
        .flex()
        .flex_col()
        .gap(px(ROW_GAP))
        .w_full()
        .children(items.into_iter().enumerate().map(|(ix, item)| {
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap(px(8.0))
                .w_full()
                .min_h(px(Type::Base.line_height()))
                .child(checkbox(item.checked, theme))
                .child(
                    div().flex_1().min_w_0().child(
                        TextView::markdown(
                            ElementId::Name(SharedString::from(format!("checklist-{start}-{ix}"))),
                            item.markdown,
                        )
                        .style(style.clone())
                        .selectable(true),
                    ),
                )
        }))
}

/// The Checkbox's check (the design system's, as the Task pane draws an
/// acceptance criterion), centered on the item's first line.
fn checkbox(checked: bool, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(Type::Base.line_height()))
        .child(controls::check(checked, false, theme))
}

/// `(checked, total)` over the body's task-list items (`- [ ]` / `- [x]`), or
/// `None` when it has none.
#[cfg(test)]
fn checklist(body: &str) -> Option<(usize, usize)> {
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

#[cfg(test)]
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

    /// The task lists of `body` the checkbox block claims, as `(checked,
    /// text)` rows, one list per entry.
    fn claimed(body: &str) -> Vec<Vec<(bool, String)>> {
        let root = markdown::to_mdast(body, &options()).unwrap();
        let source = |n: &Node| {
            let p = n.position()?;
            body.get(p.start.offset..p.end.offset)
        };
        let mut out = Vec::new();
        walk(&root, &mut |node| {
            if let Some(items) = checklist_items(node, source) {
                out.push(items.into_iter().map(|i| (i.checked, i.markdown)).collect());
            }
        });
        out
    }

    #[test]
    fn a_task_list_renders_as_checkbox_rows_with_its_inline_markdown() {
        assert_eq!(
            claimed(FEATURE),
            [vec![
                (false, "h/l move between columns".to_string()),
                (true, "j/k move within a column".to_string()),
            ]]
        );
        assert_eq!(
            claimed("- [ ] `cargo test` passes\n- [x] see [docs](https://x.dev)\n"),
            [vec![
                (false, "`cargo test` passes".to_string()),
                (true, "see [docs](https://x.dev)".to_string()),
            ]]
        );
    }

    #[test]
    fn plain_mixed_and_nested_lists_stay_the_text_views_own() {
        assert!(claimed("- one\n- two\n").is_empty());
        assert!(claimed("- [ ] one\n- two\n").is_empty());
        assert!(claimed("- [ ] one\n  - [x] nested\n").len() == 1, "only the inner list is claimed");
        assert!(claimed("- [ ] one\n\n  more\n").is_empty());
    }

    /// The header's count and the rows the body draws agree.
    #[test]
    fn the_checklist_count_matches_the_rows() {
        for body in [FEATURE, BUG] {
            let rows: Vec<(bool, String)> = claimed(body).concat();
            let done = rows.iter().filter(|(checked, _)| *checked).count();
            assert_eq!(checklist(body), Some((done, rows.len())));
        }
    }
}
