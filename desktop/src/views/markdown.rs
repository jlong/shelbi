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
//! --image` writes) as figures (a custom block, [`FIGURE`]), its alt text as
//! each caption, a missing file or a remote window's image as the alt text in
//! a placeholder. An image on its own is drawn at the column's width and at
//! most 400pt tall ([`SINGLE_MAX_HEIGHT`]); two or more one after another
//! (one paragraph, or image paragraphs with only blank lines between them,
//! which [`with_galleries`] joins into one) are a gallery: each within 300 by
//! 200pt ([`GALLERY_CELL`]), in rows that wrap. Neither is ever drawn past its
//! own size ([`fit`]). A figure scaled down is a zoom target: a pointing hand
//! over it, and a click hands it to the caller's [`OnImage`] (the
//! Changelog's lightbox, `views::overlay::lightbox`). A right-click on any
//! figure that draws an image hands it over too, with the pointer (the
//! window's image menu, `views::image_menu`); a placeholder has neither.
//! (The task pane's body moved to the in-place editor, `doc_editor`.) Its
//! tests pin that Markdown bodies parse into the blocks the view renders
//! (headings, lists, checkboxes, code, links) rather than plain text.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, img, prelude::*, px, rems, App, CursorStyle, Div, ElementId, Entity, HighlightStyle, HitboxBehavior,
    Hsla, ImgResourceLoader, MouseButton, Pixels, Point, Resource, SharedString, SharedUri, StyleRefinement, Window,
};
use gpui_kit::base::text::{MarkdownNode, MarkdownParseContext, TextView, TextViewState, TextViewStyle};
use markdown::mdast::Node;
use shelbi_app::changelog::{resolve_image, ImageSource};
use shelbi_app::theme::Appearance;

use crate::elements::controls;
use crate::views::overlay::lightbox::LightboxImage;
use crate::theme::style::{Type, Typography};
use crate::theme::Theme;

/// GitHub-flavored Markdown, as gpui-kit's text view parses it (it adds math,
/// which task bodies don't use).
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
/// The room between an image and its caption.
const FIGURE_GAP: f32 = 6.0;
/// The room between figures, down a column and across a gallery's rows.
const FIGURES_GAP: f32 = BLOCK_GAP / 2.0;
/// The tallest an image on its own is drawn (the column bounds its width).
const SINGLE_MAX_HEIGHT: f32 = 400.0;
/// The box, `(width, height)`, each image of a gallery is drawn within.
const GALLERY_CELL: (f32, f32) = (300.0, 200.0);

/// What the pointer asked of a figure's image.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageAction {
    /// A click on a figure drawn smaller than its own size: show it in the
    /// lightbox.
    Open(LightboxImage),
    /// A right-click on a figure that draws its image, at `at` in the window:
    /// its context menu there.
    Menu { source: ImageSource, at: Point<Pixels> },
}

/// What a figure's [`ImageAction`] does (the Changelog's: hand it to the
/// window, for its lightbox or its image menu).
pub type OnImage = Arc<dyn Fn(ImageAction, &mut Window, &mut App) + Send + Sync>;

/// A description as Markdown over a [`TextViewState`] the caller keeps, for
/// the text it highlights (the Changelog's search matches,
/// `TextViewState::set_range_highlights`), selectable, its task lists as
/// checkbox rows, its image paragraphs as figures. `images` is the changelog
/// directory relative image paths resolve against: `None` where it can't be
/// read (a remote window), and every image shows its alt text. `open` is
/// what a click on a figure drawn smaller than its own size, or a right-click
/// on any drawn figure, does; `None` leaves figures inert.
pub fn body_with_state(
    state: &Entity<TextViewState>,
    theme: &Theme,
    images: Option<&Path>,
    open: Option<OnImage>,
) -> TextView {
    let style = text_style(theme);
    let theme = *theme;
    let items = style.clone();
    let dir: Option<Arc<PathBuf>> = images.map(|d| Arc::new(d.to_path_buf()));
    TextView::new(state)
        .style(style)
        .markdown_block_parser(parse_checklist)
        .markdown_block_parser(move |node, _| parse_figures(node, dir.as_deref().map(PathBuf::as_path)))
        .markdown_block_renderer(CHECKLIST, move |node, _, _| render_checklist(node, &items, &theme))
        .markdown_block_renderer(FIGURE, move |node, window, cx| render_figures(node, &theme, open.as_ref(), window, cx))
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

/// `prose` with each run of image paragraphs (the ones [`FIGURE`] claims,
/// with nothing but blank lines between them) joined into one paragraph, so
/// the run parses as one block: a gallery. Any other block between two of
/// them (text, a heading, a paragraph of web images only) ends the run, and
/// each side is laid out on its own. What the Changelog hands its text view.
///
/// Only the room between the paragraphs changes, so the text the view
/// renders (and its search matches) is the same.
pub fn with_galleries(prose: &str) -> std::borrow::Cow<'_, str> {
    let Ok(root) = markdown::to_mdast(prose, &options()) else {
        return prose.into();
    };
    let gaps: Vec<std::ops::Range<usize>> = root
        .children()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .windows(2)
        .filter(|pair| pair.iter().all(|n| image_paragraph(n).is_some()))
        .filter_map(|pair| Some(pair[0].position()?.end.offset..pair[1].position()?.start.offset))
        .filter(|gap| prose.get(gap.clone()).is_some_and(|g| g.trim().is_empty()))
        .collect();
    if gaps.is_empty() {
        return prose.into();
    }
    let mut out = String::with_capacity(prose.len());
    let mut at = 0;
    for gap in gaps {
        out.push_str(&prose[at..gap.start]);
        out.push('\n');
        at = gap.end;
    }
    out.push_str(&prose[at..]);
    out.into()
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

/// How a [`FIGURE`] block lays its images out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// One image: the column's width, at most [`SINGLE_MAX_HEIGHT`] tall,
    /// its caption under it across the column.
    Single,
    /// Two or more: each within [`GALLERY_CELL`], left to right in rows that
    /// wrap, each caption under its image and as wide.
    Gallery,
}

impl Layout {
    /// The layout of a block of `count` images.
    fn of(count: usize) -> Self {
        if count > 1 {
            Layout::Gallery
        } else {
            Layout::Single
        }
    }

    /// The box, `(width, height)`, an image is drawn within; the column's
    /// width bounds it too, as it lays out.
    fn bound(self) -> (f32, f32) {
        match self {
            Layout::Single => (f32::INFINITY, SINGLE_MAX_HEIGHT),
            Layout::Gallery => GALLERY_CELL,
        }
    }
}

/// An image `natural` in size, `(width, height)`, scaled down to fit within
/// `bound` at its aspect ratio, and never up past its own size.
pub(crate) fn fit(natural: (f32, f32), bound: (f32, f32)) -> (f32, f32) {
    let (width, height) = natural;
    if width <= 0.0 || height <= 0.0 {
        return (0.0, 0.0);
    }
    let scale = (bound.0 / width).min(bound.1 / height).min(1.0);
    (width * scale, height * scale)
}

/// A [`FIGURE`] block: one image on its own, or a gallery of them
/// ([`Layout`]), each at its aspect ratio with its alt text as the caption;
/// an image that can't be drawn as its alt text in a bordered placeholder.
fn render_figures(
    node: &MarkdownNode,
    theme: &Theme,
    open: Option<&OnImage>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let figures = node.data::<Vec<Figure>>().cloned().unwrap_or_default();
    let start = node.source_range().map_or(0, |r| r.start);
    let layout = Layout::of(figures.len());
    figure_block(
        layout,
        figures.into_iter().enumerate().map(|(ix, figure)| {
            let state = figure_state(&figure.source, window, cx);
            figure_element(format!("changelog-figure-{start}-{ix}"), &figure, state, layout, open.cloned(), theme)
        }),
    )
}

/// A [`FIGURE`] block's figures: down the column, or a gallery's rows,
/// [`FIGURES_GAP`] between them either way.
fn figure_block(layout: Layout, figures: impl IntoIterator<Item = gpui::Stateful<Div>>) -> Div {
    let block = div().flex().gap(px(FIGURES_GAP)).w_full();
    match layout {
        Layout::Single => block.flex_col(),
        Layout::Gallery => block.flex_row().flex_wrap().items_start(),
    }
    .children(figures)
}

/// One figure of a [`FIGURE`] block, once its source has been tried: the
/// image over its caption, the caption alone while it loads, or the
/// placeholder, within `layout`'s box. Its id is also its debug selector,
/// and its image's is the id with `-image` after it. With `open`, the image
/// is a zoom target while it is drawn smaller than its own size, and a
/// right-click on it asks for its menu ([`framed`]).
fn figure_element(
    id: String,
    figure: &Figure,
    state: FigureState,
    layout: Layout,
    open: Option<OnImage>,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let caption = (!figure.alt.trim().is_empty()).then(|| {
        div()
            .type_style(Type::Small)
            .text_color(theme.muted)
            .child(figure.alt.clone())
    });
    let image = match (&figure.source, state) {
        (ImageSource::File(path), FigureState::Image { width, height }) if width > 0.0 && height > 0.0 => {
            Some((img(path.clone()), width, height))
        }
        (ImageSource::Web(url), FigureState::Image { width, height }) if width > 0.0 && height > 0.0 => {
            Some((img(SharedUri::from(url.clone())), width, height))
        }
        _ => None,
    };
    let bound = layout.bound();
    // The widest the image is drawn: the column narrows it further.
    let widest = image.as_ref().map(|&(_, width, height)| fit((width, height), bound).0);
    let picture = image.map(|(image, width, height)| {
        let zoom = open.map(|open| Zoom {
            image: LightboxImage {
                source: figure.source.clone(),
                width,
                height,
                caption: figure.alt.trim().to_string(),
            },
            open,
        });
        framed(format!("{id}-image"), image, (width, height), widest.unwrap_or(width), zoom, theme)
    });
    let selector = id.clone();
    let out = div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(FIGURE_GAP))
        .w_full();
    // A gallery's cell is as wide as its image (a caption wraps within it),
    // or the whole cell for a placeholder or a caption still waiting on its
    // image.
    let out = match layout {
        Layout::Single => out,
        Layout::Gallery => out.max_w(px(widest.unwrap_or(bound.0))),
    };
    match (picture, state) {
        (Some(picture), _) => out.child(picture).children(caption),
        (None, FigureState::Placeholder(why)) => out.child(placeholder(&figure.alt, why, bound.1, theme)),
        (None, _) => out.children(caption),
    }
}

/// A figure's way into the lightbox and its menu: its image at its own size,
/// and what a click or a right-click hands it to.
struct Zoom {
    image: LightboxImage,
    open: OnImage,
}

/// Whether an image `natural` in size, `(width, height)`, drawn `drawn`, was
/// scaled down to fit, by the column's width or a height limit (and so
/// opens in the lightbox). Half a point of rounding is still its own size.
pub(crate) fn zooms(drawn: (f32, f32), natural: (f32, f32)) -> bool {
    drawn.0 + 0.5 < natural.0 || drawn.1 + 0.5 < natural.1
}

/// An image `natural` in size at the column's width, at most `widest` (its
/// own width, or less to keep it within its layout's box), its height
/// following from its aspect ratio; `id` is its id and debug selector.
///
/// The clamp and the ratio sit on two boxes: on one, taffy works the height
/// out from the column's full width before `max_w` narrows it, so an image
/// narrower than the column got the height of one as wide as the column (a
/// gap under its caption). The outer box settles the width; the inner one,
/// all of it, takes its height from that.
///
/// With `zoom`, the outer box measures itself as it lays out (a canvas over
/// it): drawn smaller than its own size ([`zooms`]), it shows a pointing
/// hand and a click opens it; at its own size a click does nothing. A
/// right-click asks for its menu at any size, and goes no further (no
/// selection, no scroll); a click is left-button only, so it never opens it.
fn framed(
    id: String,
    image: gpui::Img,
    natural: (f32, f32),
    widest: f32,
    zoom: Option<Zoom>,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let (width, height) = natural;
    let selector = id.clone();
    let frame = div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .relative()
        .w_full()
        .max_w(px(widest))
        .child(
            div()
                .w_full()
                .aspect_ratio(width / height)
                .rounded_md()
                .overflow_hidden()
                .border_1()
                .border_color(theme.divider)
                .child(image.size_full()),
        );
    let Some(Zoom { image, open }) = zoom else {
        return frame;
    };
    // Set as the frame lays out, read by its click (events follow the
    // frame that drew them).
    let scaled = Rc::new(Cell::new(false));
    let measured = scaled.clone();
    let target = canvas(
        move |bounds, window, _| {
            let drawn = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
            let zoomed = zooms(drawn, natural);
            measured.set(zoomed);
            zoomed.then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
        },
        |_, hitbox, window, _| {
            if let Some(hitbox) = hitbox {
                window.set_cursor_style(CursorStyle::PointingHand, &hitbox);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let menu = open.clone();
    let source = image.source.clone();
    frame
        .child(target)
        .on_click(move |_, window, cx| {
            if scaled.get() {
                open(ImageAction::Open(image.clone()), window, cx);
            }
        })
        .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
            cx.stop_propagation();
            menu(ImageAction::Menu { source: source.clone(), at: ev.position }, window, cx);
        })
}

/// What a figure shows when there is no image to draw: its alt text, and why
/// in the muted role, at most `tallest` (its layout's box).
fn placeholder(alt: &str, why: &str, tallest: f32, theme: &Theme) -> Div {
    let alt = if alt.trim().is_empty() { "Image" } else { alt };
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .w_full()
        .max_h(px(tallest))
        .overflow_hidden()
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
        // Two images on consecutive lines are one figure block (a gallery).
        assert_eq!(
            figures("![a](a.png)\n![b](b.png)"),
            [vec![pair("a", "a.png"), pair("b", "b.png")]]
        );
        // An image inside prose, or a web image alone, is the text view's.
        assert!(figures("See ![a](a.png) here.").is_empty());
        assert!(figures("![web](https://x.dev/a.png)").is_empty());
    }

    /// The figure blocks a body renders as, once its image paragraphs are
    /// joined into galleries: the alt texts of each.
    fn blocks(body: &str) -> Vec<Vec<String>> {
        let joined = with_galleries(body);
        let root = markdown::to_mdast(&joined, &options()).unwrap();
        root.children()
            .unwrap()
            .iter()
            .filter_map(image_paragraph)
            .map(|images| images.into_iter().map(|(alt, _)| alt).collect())
            .collect()
    }

    #[test]
    fn images_one_after_another_are_one_gallery_and_text_splits_them() {
        let alts = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // One paragraph of images: one block, a gallery.
        assert_eq!(blocks("![a](a.png)\n![b](b.png)\n![c](c.png)\n"), [alts(&["a", "b", "c"])]);
        assert_eq!(Layout::of(3), Layout::Gallery);
        assert_eq!(Layout::of(1), Layout::Single);
        // Image paragraphs with only blank lines between them: one gallery.
        assert_eq!(
            blocks("Intro.\n\n![a](a.png)\n\n![b](b.png)\n\n\n![c](c.png)\n\nOutro.\n"),
            [alts(&["a", "b", "c"])]
        );
        assert_eq!(blocks("![a](a.png)\n\n![b](b.png)\n![c](c.png)"), [alts(&["a", "b", "c"])]);
        assert_eq!(blocks("![a](a.png)\n\n  ![b](b.png)\n"), [alts(&["a", "b"])], "an indented one");
        // Text between them splits the run: each side on its own.
        assert_eq!(
            blocks("![a](a.png)\n\nBetween.\n\n![b](b.png)\n\n![c](c.png)\n"),
            [alts(&["a"]), alts(&["b", "c"])]
        );
        assert_eq!(blocks("![a](a.png)\n\n## Next\n\n![b](b.png)\n"), [alts(&["a"]), alts(&["b"])]);
        // A paragraph of web images only stays the text view's, and splits too.
        assert_eq!(
            blocks("![a](a.png)\n\n![w](https://x.dev/w.png)\n\n![b](b.png)\n"),
            [alts(&["a"]), alts(&["b"])]
        );
        // Nothing to join: the body as written.
        let body = "Text.\n\n![a](a.png)\n\nMore.\n";
        assert!(matches!(with_galleries(body), std::borrow::Cow::Borrowed(b) if b == body));
        // Joining changes only the blank lines between the paragraphs (an
        // indent is the next paragraph's own, and fine on a continuation line).
        assert_eq!(with_galleries("![a](a.png)\n\n![b](b.png)\n\nEnd.\n"), "![a](a.png)\n![b](b.png)\n\nEnd.\n");
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

    /// Figures as the Changelog draws them, across the window's width, with
    /// a row under them: each shot a block of its own (`Layout::Single`), or
    /// all of them one gallery.
    struct Figures(Vec<(f32, f32)>, Layout, Option<OnImage>);

    impl Render for Figures {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark);
            let layout = self.1;
            let open = self.2.clone();
            let figures = self.0.iter().enumerate().map(move |(ix, &(width, height))| {
                let figure = Figure {
                    alt: format!("Shot {ix}"),
                    source: ImageSource::File(PathBuf::from(format!("/nonexistent/shot-{ix}.png"))),
                };
                figure_element(format!("fig-{ix}"), &figure, FigureState::Image { width, height }, layout, open.clone(), &theme)
            });
            let blocks: Vec<Div> = match layout {
                Layout::Single => figures.map(|f| figure_block(Layout::Single, [f])).collect(),
                Layout::Gallery => vec![figure_block(Layout::Gallery, figures)],
            };
            div()
                .flex()
                .flex_col()
                .w_full()
                .child(div().flex().flex_col().gap(px(FIGURES_GAP)).w_full().children(blocks))
                .child(div().debug_selector(|| "after".into()).h(px(10.0)))
        }
    }

    fn bounds_of(vcx: &mut gpui::VisualTestContext, selector: String) -> gpui::Bounds<Pixels> {
        vcx.debug_bounds(Box::leak(selector.into_boxed_str())).unwrap()
    }

    fn resize(vcx: &mut gpui::VisualTestContext, host: &Entity<Figures>, column: f32) {
        vcx.simulate_resize(gpui::size(px(column), px(4000.0)));
        host.update(vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
    }

    /// An image on its own is the column's width, at most 400pt tall, never
    /// past its own size, at its aspect ratio, over its caption, with only
    /// the figure gap between figures, however the column compares with the
    /// images and as the window resizes.
    #[gpui::test]
    fn a_single_figure_fits_the_column_and_400pt_and_heads_its_caption(cx: &mut gpui::TestAppContext) {
        // Crown & Keep's phone screenshots, a wide one, and a small one.
        let shots = vec![(460.0, 1000.0), (750.0, 1000.0), (1200.0, 600.0), (300.0, 200.0)];
        let window = cx.add_window(|_, _| Figures(shots.clone(), Layout::Single, None));
        let host = window.root(cx).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        // Wider than every image, between them, and narrower than all.
        for column in [1400.0_f32, 600.0, 250.0, 1000.0] {
            resize(&mut vcx, &host, column);
            let mut prev_bottom = None;
            for (ix, &(width, height)) in shots.iter().enumerate() {
                let figure = bounds_of(&mut vcx, format!("fig-{ix}"));
                let image = bounds_of(&mut vcx, format!("fig-{ix}-image"));
                let (shown, tall) = fit((width, height), (column, SINGLE_MAX_HEIGHT));
                let at = format!("a {width}x{height} image in a {column}pt column");
                assert!((image.size.width.as_f32() - shown).abs() < 0.5, "{at}: width {:?}, want {shown}", image.size.width);
                assert!((image.size.height.as_f32() - tall).abs() < 0.5, "{at}: height {:?}, want {tall}", image.size.height);
                assert!(image.size.height.as_f32() <= SINGLE_MAX_HEIGHT + 0.5, "{at}: at most 400pt tall");
                assert_eq!(image.left(), figure.left(), "{at}: left-aligned");
                assert_eq!(image.top(), figure.top(), "{at}: the image heads its figure");
                let caption = Type::Small.line_height();
                let want = image.size.height.as_f32() + FIGURE_GAP + caption;
                assert!(
                    (figure.size.height.as_f32() - want).abs() < 0.5,
                    "{at}: the figure is {:?} tall, want image + gap + caption = {want}",
                    figure.size.height
                );
                if let Some(bottom) = prev_bottom {
                    assert_eq!(figure.top(), bottom + px(FIGURES_GAP), "{at}: only the figure gap above it");
                }
                prev_bottom = Some(figure.bottom());
            }
            let after = vcx.debug_bounds("after").unwrap();
            assert_eq!(Some(after.top()), prev_bottom, "{column}pt: nothing under the last figure");
        }
    }

    /// A gallery lays its images left to right, each within 300x200 at its
    /// aspect ratio over its caption, the cell gap between them, and wraps
    /// onto rows the same gap apart.
    #[gpui::test]
    fn a_gallery_wraps_its_figures_in_rows_within_300_by_200(cx: &mut gpui::TestAppContext) {
        // Tall, wide, small, and wide again.
        let shots = vec![(460.0, 1000.0), (1200.0, 600.0), (120.0, 90.0), (1600.0, 900.0)];
        let window = cx.add_window(|_, _| Figures(shots.clone(), Layout::Gallery, None));
        let host = window.root(cx).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let caption = Type::Small.line_height();
        // Room for every cell on one row, and for only the first three.
        for column in [1200.0_f32, 600.0] {
            resize(&mut vcx, &host, column);
            let mut rows: Vec<Vec<gpui::Bounds<Pixels>>> = Vec::new();
            for (ix, &(width, height)) in shots.iter().enumerate() {
                let figure = bounds_of(&mut vcx, format!("fig-{ix}"));
                let image = bounds_of(&mut vcx, format!("fig-{ix}-image"));
                let (shown, tall) = fit((width, height), GALLERY_CELL);
                let at = format!("a {width}x{height} image in a {column}pt gallery");
                assert!((image.size.width.as_f32() - shown).abs() < 0.5, "{at}: width {:?}, want {shown}", image.size.width);
                assert!((image.size.height.as_f32() - tall).abs() < 0.5, "{at}: height {:?}, want {tall}", image.size.height);
                assert_eq!(figure.size.width, image.size.width, "{at}: the cell is as wide as its image");
                assert_eq!(image.origin, figure.origin, "{at}: the image heads its cell");
                let want = tall + FIGURE_GAP + caption;
                assert!((figure.size.height.as_f32() - want).abs() < 0.5, "{at}: caption under the image");
                match rows.last_mut() {
                    Some(row) if row[0].top() == figure.top() => {
                        let prev = row.last().unwrap();
                        assert_eq!(figure.left(), prev.right() + px(FIGURES_GAP), "{at}: the cell gap before it");
                        row.push(figure);
                    }
                    _ => {
                        assert_eq!(figure.left(), px(0.0), "{at}: a row starts at the left");
                        if let Some(row) = rows.last() {
                            let bottom = row.iter().map(|b| b.bottom()).max().unwrap();
                            assert_eq!(figure.top(), bottom + px(FIGURES_GAP), "{at}: the row gap above it");
                        }
                        rows.push(vec![figure]);
                    }
                }
            }
            let lens: Vec<usize> = rows.iter().map(Vec::len).collect();
            // 92+300+120+300 and the gaps fit 1200; 600 holds the first three.
            let want: &[usize] = if column > 1000.0 { &[4] } else { &[3, 1] };
            assert_eq!(lens, want, "{column}pt: cells per row");
            let bottom = rows.last().unwrap().iter().map(|b| b.bottom()).max().unwrap();
            assert_eq!(vcx.debug_bounds("after").unwrap().top(), bottom, "{column}pt: the gallery ends under its last row");
        }
    }

    /// A figure the column or a height limit scaled down opens on a click,
    /// with its image at its own size; one drawn at its own size doesn't, and
    /// the same figure stops opening once the column is wide enough for it.
    #[gpui::test]
    fn only_a_scaled_down_figure_opens_on_a_click(cx: &mut gpui::TestAppContext) {
        let opened = Arc::new(std::sync::Mutex::new(Vec::<LightboxImage>::new()));
        let seen = opened.clone();
        let open: OnImage = Arc::new(move |action, _, _| {
            if let ImageAction::Open(image) = action {
                seen.lock().unwrap().push(image);
            }
        });
        // Wide (under 400pt tall), small, and tall (400pt at 230 wide, in
        // any column).
        let shots = vec![(1200.0, 360.0), (400.0, 300.0), (460.0, 800.0)];
        let window = cx.add_window(|_, _| Figures(shots.clone(), Layout::Single, Some(open)));
        let host = window.root(cx).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        let click = |vcx: &mut gpui::VisualTestContext, column: f32, selector: &'static str| {
            resize(vcx, &host, column);
            let at = vcx.debug_bounds(selector).unwrap().center();
            vcx.simulate_click(at, gpui::Modifiers::none());
            vcx.run_until_parked();
            std::mem::take(&mut *opened.lock().unwrap())
        };
        let got = click(&mut vcx, 600.0, "fig-0-image");
        assert_eq!(
            got,
            [LightboxImage {
                source: ImageSource::File(PathBuf::from("/nonexistent/shot-0.png")),
                width: 1200.0,
                height: 360.0,
                caption: "Shot 0".into(),
            }],
            "the wide shot, scaled to 600pt, opens at its own size"
        );
        assert!(click(&mut vcx, 600.0, "fig-1-image").is_empty(), "the small shot is at its own size");
        assert!(click(&mut vcx, 1400.0, "fig-0-image").is_empty(), "a column wider than the shot");
        assert_eq!(click(&mut vcx, 1199.0, "fig-0-image").len(), 1, "a point narrower is scaled");
        let tall = click(&mut vcx, 1400.0, "fig-2-image");
        assert_eq!(tall.len(), 1, "the tall shot, held to 400pt in a wide column, opens");
        assert_eq!((tall[0].width, tall[0].height), (460.0, 800.0), "at its own size");
    }

    /// A gallery's images are zoom targets when scaled into their cell, and
    /// every drawn one asks for its menu on a right-click.
    #[gpui::test]
    fn a_gallery_image_opens_when_scaled_and_has_its_menu(cx: &mut gpui::TestAppContext) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<ImageAction>::new()));
        let log = seen.clone();
        let open: OnImage = Arc::new(move |action, _, _| log.lock().unwrap().push(action));
        let window = cx.add_window(|_, _| Figures(vec![(1200.0, 600.0), (120.0, 90.0)], Layout::Gallery, Some(open)));
        let host = window.root(cx).unwrap();
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        resize(&mut vcx, &host, 1400.0);
        let take = |vcx: &mut gpui::VisualTestContext| {
            vcx.run_until_parked();
            std::mem::take(&mut *seen.lock().unwrap())
        };
        // In a column wide enough for it, the wide shot is still held to its cell.
        let at = bounds_of(&mut vcx, "fig-0-image".into()).center();
        vcx.simulate_click(at, gpui::Modifiers::none());
        let got = take(&mut vcx);
        assert!(matches!(&got[..], [ImageAction::Open(i)] if i.width == 1200.0), "scaled into its cell: {got:?}");
        let at = bounds_of(&mut vcx, "fig-1-image".into()).center();
        vcx.simulate_click(at, gpui::Modifiers::none());
        assert!(take(&mut vcx).is_empty(), "the small shot is at its own size");
        for ix in 0..2 {
            let at = bounds_of(&mut vcx, format!("fig-{ix}-image")).origin + gpui::point(px(10.0), px(12.0));
            vcx.simulate_mouse_down(at, MouseButton::Right, gpui::Modifiers::none());
            vcx.simulate_mouse_up(at, MouseButton::Right, gpui::Modifiers::none());
            assert_eq!(
                take(&mut vcx),
                [ImageAction::Menu { source: ImageSource::File(PathBuf::from(format!("/nonexistent/shot-{ix}.png"))), at }],
                "gallery image {ix}: its menu at the pointer"
            );
        }
    }

    /// A right-click on any figure that draws its image, scaled down or not,
    /// asks for its menu at the pointer and never opens the lightbox; a
    /// placeholder (a missing file, a remote window's) has no menu, and keeps
    /// to its layout's box.
    #[gpui::test]
    fn a_right_click_on_a_drawn_figure_asks_for_its_menu_and_a_placeholder_none(cx: &mut gpui::TestAppContext) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<ImageAction>::new()));
        let log = seen.clone();
        let open: OnImage = Arc::new(move |action, _, _| log.lock().unwrap().push(action));
        let window = cx.add_window(|_, _| Figures(vec![(1200.0, 600.0), (400.0, 300.0)], Layout::Single, Some(open)));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.simulate_resize(gpui::size(px(600.0), px(4000.0)));
        vcx.run_until_parked();
        let right_click = |vcx: &mut gpui::VisualTestContext, at: Point<Pixels>| {
            vcx.simulate_mouse_down(at, MouseButton::Right, gpui::Modifiers::none());
            vcx.simulate_mouse_up(at, MouseButton::Right, gpui::Modifiers::none());
            vcx.run_until_parked();
            std::mem::take(&mut *seen.lock().unwrap())
        };
        for (ix, what) in [(0, "scaled down"), (1, "at its own size")] {
            let image = bounds_of(&mut vcx, format!("fig-{ix}-image"));
            let at = image.origin + gpui::point(px(10.0), px(12.0));
            assert_eq!(
                right_click(&mut vcx, at),
                [ImageAction::Menu {
                    source: ImageSource::File(PathBuf::from(format!("/nonexistent/shot-{ix}.png"))),
                    at,
                }],
                "{what}: the menu, at the pointer, and no lightbox"
            );
        }

        // Placeholders draw no image, so there is nothing to right-click.
        struct Placeholders(OnImage);
        impl Render for Placeholders {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                let theme = Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark);
                let missing = Figure { alt: "Gone".into(), source: ImageSource::Missing("/nonexistent/gone.png".into()) };
                let remote = Figure { alt: "There".into(), source: ImageSource::Unavailable };
                let placeholders = |layout, at: &str| {
                    figure_block(
                        layout,
                        [
                            figure_element(format!("{at}-0"), &missing, FigureState::Placeholder("Image missing"), layout, Some(self.0.clone()), &theme),
                            figure_element(format!("{at}-1"), &remote, FigureState::Placeholder("Image not available here"), layout, Some(self.0.clone()), &theme),
                        ],
                    )
                };
                div().flex().flex_col().w_full().child(placeholders(Layout::Single, "ph")).child(placeholders(Layout::Gallery, "gal"))
            }
        }
        let log = seen.clone();
        let open: OnImage = Arc::new(move |action, _, _| log.lock().unwrap().push(action));
        let window = cx.add_window(|_, _| Placeholders(open));
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.simulate_resize(gpui::size(px(1000.0), px(2000.0)));
        vcx.run_until_parked();
        for selector in ["ph-0", "ph-1", "gal-0", "gal-1"] {
            assert!(vcx.debug_bounds(Box::leak(format!("{selector}-image").into_boxed_str())).is_none());
            let bounds = bounds_of(&mut vcx, selector.into());
            assert!(right_click(&mut vcx, bounds.center()).is_empty(), "{selector}: a placeholder has no menu");
        }
        assert_eq!(bounds_of(&mut vcx, "ph-0".into()).size.width, px(1000.0), "on its own: the column's width");
        let (cell0, cell1) = (bounds_of(&mut vcx, "gal-0".into()), bounds_of(&mut vcx, "gal-1".into()));
        assert_eq!(cell0.size.width, px(GALLERY_CELL.0), "in a gallery: the cell's width");
        assert_eq!((cell1.top(), cell1.left()), (cell0.top(), cell0.right() + px(FIGURES_GAP)), "side by side");
    }

    #[test]
    fn an_image_scales_down_to_its_box_and_never_up() {
        // Width-bound: wider than the box, in proportion.
        assert_eq!(fit((1200.0, 600.0), (600.0, 400.0)), (600.0, 300.0));
        assert_eq!(fit((1200.0, 600.0), GALLERY_CELL), (300.0, 150.0));
        // Height-bound: taller than the box, narrower in proportion.
        assert_eq!(fit((460.0, 1000.0), (800.0, SINGLE_MAX_HEIGHT)), (184.0, 400.0));
        assert_eq!(fit((460.0, 1000.0), GALLERY_CELL), (92.0, 200.0));
        assert_eq!(fit((600.0, 600.0), (f32::INFINITY, SINGLE_MAX_HEIGHT)), (400.0, 400.0), "no column bound");
        // Both: the tighter one wins.
        assert_eq!(fit((900.0, 900.0), GALLERY_CELL), (200.0, 200.0));
        // Never up past its own size, by either limit.
        assert_eq!(fit((120.0, 90.0), GALLERY_CELL), (120.0, 90.0));
        assert_eq!(fit((300.0, 200.0), GALLERY_CELL), (300.0, 200.0));
        assert_eq!(fit((300.0, 380.0), (f32::INFINITY, SINGLE_MAX_HEIGHT)), (300.0, 380.0));
        assert_eq!(fit((0.0, 10.0), GALLERY_CELL), (0.0, 0.0), "no size, nothing to draw");
    }

    #[test]
    fn a_figure_zooms_only_when_drawn_smaller_than_its_own_size() {
        assert!(zooms((600.0, 300.0), (1200.0, 600.0)));
        assert!(!zooms((1200.0, 600.0), (1200.0, 600.0)));
        assert!(!zooms((1199.6, 599.8), (1200.0, 600.0)), "rounding");
        assert!(!zooms((400.0, 300.0), (300.0, 200.0)));
        // Held by its height: smaller both ways at its aspect ratio, and the
        // height alone is enough to say so.
        assert!(zooms((184.0, 400.0), (460.0, 1000.0)));
        assert!(zooms((460.0, 400.0), (460.0, 1000.0)), "height alone");
        assert!(!zooms((460.0, 999.7), (460.0, 1000.0)), "rounding, by height");
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
