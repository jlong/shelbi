//! Markdown for the Changelog page.
//!
//! An entry's description renders with gpui-kit's Markdown text view
//! (gpui-base's `TextView`), which parses with the `markdown` crate under
//! GitHub-flavored options, in the styles the Figma Task pane frames set for
//! a body ([`body_with_state`]): headings in `Desktop/Heading` (h1) and
//! `Desktop/Base/Semibold` (the rest) in the loud text role, 12pt over what
//! they head; the body in `Desktop/Base/Regular`, 20pt between blocks; and a
//! task list drawn as Figma's checkbox rows (a custom block, [`CHECKLIST`]);
//! and a paragraph of images (`![alt](assets/…)`, what `shelbi changelog add
//! --image` writes) as figures (a custom block, [`FIGURE`]): each image at the
//! column's width (never past its own), its alt text as the caption, a
//! missing file or a remote window's image as the alt text in a placeholder.
//! (The task pane's body moved to the in-place editor, `doc_editor`.) Its
//! tests pin that Markdown bodies parse into the blocks the view renders
//! (headings, lists, checkboxes, code, links) rather than plain text.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    div, img, prelude::*, px, rems, App, Div, ElementId, Entity, HighlightStyle, Hsla, ImgResourceLoader, Resource,
    SharedString, SharedUri, StyleRefinement, Window,
};
use gpui_kit::base::text::{MarkdownNode, MarkdownParseContext, TextView, TextViewState, TextViewStyle};
use markdown::mdast::Node;
use shelbi_app::changelog::{resolve_image, ImageSource};
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
/// The custom block a paragraph of images renders as.
const FIGURE: &str = "shelbi-figure";
/// The room between an image and its caption, and between figures.
const FIGURE_GAP: f32 = 6.0;

/// A description as Markdown over a [`TextViewState`] the caller keeps, for
/// the text it highlights (the Changelog's search matches,
/// `TextViewState::set_range_highlights`), selectable, its task lists as
/// checkbox rows, its image paragraphs as figures. `images` is the changelog
/// directory relative image paths resolve against: `None` where it can't be
/// read (a remote window), and every image shows its alt text.
pub fn body_with_state(state: &Entity<TextViewState>, theme: &Theme, images: Option<&Path>) -> TextView {
    let style = text_style(theme);
    let theme = *theme;
    let items = style.clone();
    let dir: Option<Arc<PathBuf>> = images.map(|d| Arc::new(d.to_path_buf()));
    TextView::new(state)
        .style(style)
        .markdown_block_parser(parse_checklist)
        .markdown_block_parser(move |node, _| parse_figures(node, dir.as_deref().map(PathBuf::as_path)))
        .markdown_block_renderer(CHECKLIST, move |node, _, _| render_checklist(node, &items, &theme))
        .markdown_block_renderer(FIGURE, move |node, window, cx| render_figures(node, &theme, window, cx))
}

/// One image of a [`FIGURE`] block: its alt text and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Figure {
    alt: String,
    source: ImageSource,
}

/// The images of a paragraph that holds nothing else (blank text and line
/// breaks aside), as `(alt, url)`. `None` for any other node, and for a
/// paragraph of web images only, which the text view draws itself.
fn image_paragraph(node: &Node) -> Option<Vec<(String, String)>> {
    let Node::Paragraph(paragraph) = node else {
        return None;
    };
    let mut images = Vec::new();
    for child in &paragraph.children {
        match child {
            Node::Image(image) => images.push((image.alt.clone(), image.url.clone())),
            Node::Text(text) if text.value.trim().is_empty() => {}
            Node::Break(_) => {}
            _ => return None,
        }
    }
    let local = images
        .iter()
        .any(|(_, url)| !(url.starts_with("https://") || url.starts_with("http://")));
    (local && !images.is_empty()).then_some(images)
}

/// A paragraph of images as the [`FIGURE`] block, each resolved against
/// `dir` (the parse runs off the UI thread, so the file checks do too). Its
/// text is the captions, so a search finds them.
fn parse_figures(node: &Node, dir: Option<&Path>) -> Option<MarkdownNode> {
    let figures: Vec<Figure> = image_paragraph(node)?
        .into_iter()
        .map(|(alt, url)| Figure {
            source: resolve_image(dir, &url),
            alt,
        })
        .collect();
    let text = figures
        .iter()
        .map(|f| f.alt.as_str())
        .filter(|a| !a.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Some(MarkdownNode::new(FIGURE, figures).text(text))
}

/// What a figure draws once its source has been tried.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FigureState {
    /// The image, at its own size in pixels.
    Image { width: f32, height: f32 },
    /// Still loading: the caption alone, until it lands.
    Loading,
    /// No image to draw, and why.
    Placeholder(&'static str),
}

fn figure_state(source: &ImageSource, window: &mut Window, cx: &mut App) -> FigureState {
    let resource = match source {
        ImageSource::File(path) => Resource::Path(Arc::from(path.as_path())),
        ImageSource::Web(url) => Resource::Uri(SharedUri::from(url.clone())),
        ImageSource::Missing(_) => return FigureState::Placeholder("Image missing"),
        ImageSource::Unavailable => return FigureState::Placeholder("Image not available here"),
    };
    // The same cache the image element reads, so the load happens once.
    match window.use_asset::<ImgResourceLoader>(&resource, cx) {
        Some(Ok(image)) => {
            let size = image.size(0);
            FigureState::Image {
                width: size.width.0 as f32,
                height: size.height.0 as f32,
            }
        }
        Some(Err(_)) => FigureState::Placeholder("Image couldn\u{2019}t be read"),
        None => FigureState::Loading,
    }
}

/// A [`FIGURE`] block: each image scaled to the column's width (never past
/// its own, keeping its aspect ratio) with its alt text as the caption; an
/// image that can't be drawn as its alt text in a bordered placeholder.
fn render_figures(node: &MarkdownNode, theme: &Theme, window: &mut Window, cx: &mut App) -> Div {
    let figures = node.data::<Vec<Figure>>().cloned().unwrap_or_default();
    let start = node.source_range().map_or(0, |r| r.start);
    div()
        .flex()
        .flex_col()
        .gap(px(BLOCK_GAP / 2.0))
        .w_full()
        .children(figures.into_iter().enumerate().map(|(ix, figure)| {
            let state = figure_state(&figure.source, window, cx);
            let id = format!("changelog-figure-{start}-{ix}");
            let caption = (!figure.alt.trim().is_empty()).then(|| {
                div()
                    .type_style(Type::Small)
                    .text_color(theme.muted)
                    .child(figure.alt.clone())
            });
            let picture = match (&figure.source, state) {
                (ImageSource::File(path), FigureState::Image { width, height }) if height > 0.0 => {
                    Some(framed(img(path.clone()), width, height, theme))
                }
                (ImageSource::Web(url), FigureState::Image { width, height }) if height > 0.0 => {
                    Some(framed(img(SharedUri::from(url.clone())), width, height, theme))
                }
                _ => None,
            };
            let mut out = div().id(SharedString::from(id.clone())).debug_selector(move || id.clone());
            out = out.flex().flex_col().gap(px(FIGURE_GAP)).w_full();
            match (picture, state) {
                (Some(picture), _) => out.child(picture).children(caption),
                (None, FigureState::Placeholder(why)) => out.child(placeholder(&figure.alt, why, theme)),
                (None, _) => out.children(caption),
            }
            .into_any_element()
        }))
}

/// An image at the column's width, at most its own, its height following
/// from its aspect ratio.
fn framed(image: gpui::Img, width: f32, height: f32, theme: &Theme) -> Div {
    div()
        .w_full()
        .max_w(px(width))
        .aspect_ratio(width / height)
        .rounded_md()
        .overflow_hidden()
        .border_1()
        .border_color(theme.divider)
        .child(image.size_full())
}

/// What a figure shows when there is no image to draw: its alt text, and why
/// in the muted role.
fn placeholder(alt: &str, why: &str, theme: &Theme) -> Div {
    let alt = if alt.trim().is_empty() { "Image" } else { alt };
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .w_full()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(theme.divider)
        .child(div().type_style(Type::Base).text_color(theme.foreground).child(alt.to_string()))
        .child(div().type_style(Type::Small).text_color(theme.muted).child(why.to_string()))
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

    /// The figures a body's paragraphs claim, as `(alt, url)` lists.
    fn figures(body: &str) -> Vec<Vec<(String, String)>> {
        let root = markdown::to_mdast(body, &options()).unwrap();
        let mut out = Vec::new();
        walk(&root, &mut |node| out.extend(image_paragraph(node)));
        out
    }

    #[test]
    fn a_paragraph_of_images_is_a_figure_and_mixed_ones_stay_text() {
        let pair = |a: &str, u: &str| (a.to_string(), u.to_string());
        assert_eq!(
            figures("Before.\n\n![Jobs sheet](assets/e/jobs.png)\n"),
            [vec![pair("Jobs sheet", "assets/e/jobs.png")]]
        );
        // Two images on consecutive lines are one figure block.
        assert_eq!(
            figures("![a](a.png)\n![b](b.png)"),
            [vec![pair("a", "a.png"), pair("b", "b.png")]]
        );
        // An image inside prose, or a web image alone, is the text view's.
        assert!(figures("See ![a](a.png) here.").is_empty());
        assert!(figures("![web](https://x.dev/a.png)").is_empty());
    }

    #[test]
    fn figures_resolve_against_the_changelog_dir() {
        let dir = std::env::temp_dir().join(format!("shelbi-desktop-fig-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets/e")).unwrap();
        std::fs::write(dir.join("assets/e/a.png"), b"x").unwrap();
        let root = markdown::to_mdast("![A](assets/e/a.png)\n![B](assets/e/gone.png)", &options()).unwrap();
        let paragraph = root.children().unwrap()[0].clone();
        let node = parse_figures(&paragraph, Some(&dir)).unwrap();
        let got = node.data::<Vec<Figure>>().unwrap();
        assert_eq!(got[0].source, ImageSource::File(dir.join("assets/e/a.png")));
        assert_eq!(got[1].source, ImageSource::Missing(dir.join("assets/e/gone.png")));
        assert_eq!(node.as_text(), "A\nB", "the captions are its text");
        // A remote window has no directory: every image is its alt text.
        let node = parse_figures(&paragraph, None).unwrap();
        assert!(node.data::<Vec<Figure>>().unwrap().iter().all(|f| f.source == ImageSource::Unavailable));
        let _ = std::fs::remove_dir_all(&dir);
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
