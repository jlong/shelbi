//! Wrapped text clamped to a number of lines, its last line filled up to an
//! ellipsis, as Figma clamps the task panels' description.
//!
//! gpui's own clamp (`text_ellipsis()` + `line_clamp(n)`) picks where to cut
//! by summing per-character advances (no kerning), which runs wider than the
//! shaped text: its estimate wraps a full line one word early, so the last
//! line starts a word late and ends short ("Don't…" where Figma draws
//! "Don't stop until…", `dt-figma-qa-task-panels`). [`clamped_text`] wraps
//! with the real shaper, finds where the last allowed line starts, and fits
//! as much of the rest as the line holds beside the ellipsis. It paints in
//! the inherited text style and, like a plain string, carries no
//! accessibility role.

use std::cell::RefCell;
use std::panic::Location;
use std::rc::Rc;

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, SharedString, Style, TextAlign, TextRun, Window, WrappedLine,
};

/// The ellipsis a clamped text ends with.
const ELLIPSIS: &str = "…";

/// `text` wrapped to its width and clamped to `lines` lines.
pub fn clamped_text(text: impl Into<SharedString>, lines: usize) -> ClampedText {
    ClampedText {
        text: text.into(),
        lines: lines.max(1),
    }
}

pub struct ClampedText {
    text: SharedString,
    lines: usize,
}

impl IntoElement for ClampedText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The lines the last measure shaped, and their line height.
type Shaped = Rc<RefCell<(Vec<WrappedLine>, Pixels)>>;

impl Element for ClampedText {
    type RequestLayoutState = Shaped;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text_style = window.text_style();
        let rem = window.rem_size();
        let font_size = text_style.font_size.to_pixels(rem);
        let line_height = text_style.line_height_in_pixels(rem);
        let run = text_style.to_run(0);
        let shaped: Shaped = Rc::new(RefCell::new((Vec::new(), line_height)));
        let text = self.text.clone();
        let max_lines = self.lines;
        let state = shaped.clone();
        let layout_id = window.request_measured_layout(Style::default(), move |known, available, window, _| {
            let width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                _ => None,
            });
            let lines = clamp(&text, width, max_lines, font_size, &run, window);
            let height = line_height * lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum::<usize>();
            let line_width = lines.iter().map(|l| l.width()).fold(Pixels::ZERO, Pixels::max);
            *state.borrow_mut() = (lines, line_height);
            gpui::size(width.unwrap_or(line_width), height)
        });
        (layout_id, shaped)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        shaped: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (lines, line_height) = &*shaped.borrow();
        let mut origin = bounds.origin;
        for line in lines {
            let _ = line.paint(origin, *line_height, TextAlign::Left, Some(bounds), window, cx);
            origin.y += line.size(*line_height).height;
        }
    }
}

/// `text` shaped in `run`'s style, wrapped to `width` and, past `max_lines`
/// lines, cut so the last allowed line holds as much as fits beside the
/// ellipsis.
fn clamp(
    text: &SharedString,
    width: Option<Pixels>,
    max_lines: usize,
    font_size: Pixels,
    run: &TextRun,
    window: &mut Window,
) -> Vec<WrappedLine> {
    let runs = |len: usize| [TextRun { len, ..run.clone() }];
    let shape = |text: SharedString, wrap: Option<Pixels>, clamp: Option<usize>, window: &mut Window| {
        let len = text.len();
        window
            .text_system()
            .shape_text(text, font_size, &runs(len), wrap, clamp)
            .map(|lines| lines.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    let mut lines = shape(text.clone(), width, None, window);
    if let (Some(width), Some(cut)) = (width, clamp_point(&lines, max_lines)) {
        let ellipsis = window
            .text_system()
            .shape_line(ELLIPSIS.into(), font_size, &runs(ELLIPSIS.len()), None)
            .width;
        let tail: SharedString = text[cut.start..cut.end].to_string().into();
        let tail_line = window
            .text_system()
            .shape_line(tail.clone(), font_size, &runs(tail.len()), None);
        // The longest start of the tail that fits beside the ellipsis.
        let mut fit = tail_line.index_for_x(width - ellipsis).unwrap_or(tail.len());
        while !tail.is_char_boundary(fit) {
            fit -= 1;
        }
        let kept = tail[..fit].trim_end_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
        let clamped: SharedString = format!("{}{kept}{ELLIPSIS}", &text[..cut.start]).into();
        lines = shape(clamped, Some(width), Some(max_lines), window);
    }
    lines
}

/// Where the clamp cuts the text shaped as `lines` (one per paragraph): the
/// byte range of the last allowed line, from its start to its paragraph's
/// end, or `None` when the text fits in `max_lines`.
fn clamp_point(lines: &[WrappedLine], max_lines: usize) -> Option<std::ops::Range<usize>> {
    let total: usize = lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum();
    if total <= max_lines {
        return None;
    }
    // Each shaped paragraph is one `\n`-separated piece of `text`.
    let mut seen = 0;
    let mut paragraph_start = 0;
    for line in lines {
        let rows = line.wrap_boundaries().len() + 1;
        if seen + rows >= max_lines {
            let row = max_lines - 1 - seen;
            return Some(paragraph_start + row_start(line, row)..paragraph_start + line.len());
        }
        seen += rows;
        paragraph_start += line.len() + 1;
    }
    None
}

/// The byte offset in `line`'s paragraph where its wrapped row `row` starts.
fn row_start(line: &WrappedLine, row: usize) -> usize {
    match row.checked_sub(1) {
        None => 0,
        Some(ix) => {
            let boundary = line.wrap_boundaries()[ix];
            line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, TestAppContext};

    const BODY: &str = "Build a native desktop client for Shelbi. This should share state with the TUI \
        through a persistent daemon running in the background. Use Alacritty for the terminal. Use GPUI \
        for the interface. Don\u{2019}t stop until it\u{2019}s done.";

    fn rows(lines: &[WrappedLine]) -> usize {
        lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum()
    }

    #[gpui::test]
    fn the_last_line_holds_all_it_can_beside_the_ellipsis(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|window, _| {
            let run = window.text_style().to_run(0);
            let (size, width) = (px(12.0), px(244.0));
            let runs = |len: usize| [TextRun { len, ..run.clone() }];
            let full = window
                .text_system()
                .shape_text(BODY.into(), size, &runs(BODY.len()), Some(width), None)
                .unwrap();
            assert!(rows(&full) > 5, "the fixture overflows five lines at this width");

            let lines = clamp(&SharedString::from(BODY), Some(width), 5, size, &run, window);
            assert_eq!(rows(&lines), 5);
            let shown = lines[0].text.to_string();
            let kept = shown.strip_suffix(ELLIPSIS).expect("it ends with the ellipsis");
            assert!(BODY.starts_with(kept), "{kept:?}");
            // The first four lines wrap as the whole text does, so the last
            // starts where the text's fifth line does.
            let start = row_start(&lines[0], 4);
            assert_eq!(start, row_start(&full[0], 4), "{shown:?}");
            // One more word wouldn't fit beside the ellipsis.
            let rest = &BODY[kept.len()..];
            let space = &rest[..rest.len() - rest.trim_start().len()];
            let next_word = rest.trim_start().split(' ').next().unwrap();
            let more = format!("{}{space}{next_word}{ELLIPSIS}", &BODY[start..kept.len()]);
            let wider = window
                .text_system()
                .shape_line(more.clone().into(), size, &runs(more.len()), None)
                .width;
            assert!(wider > width, "{more:?} fits in {width:?} ({wider:?})");
        });
    }

    #[gpui::test]
    fn text_that_fits_is_left_whole(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|window, _| {
            let run = window.text_style().to_run(0);
            let text = SharedString::from("Build a native desktop client.");
            let lines = clamp(&text, Some(px(244.0)), 5, px(12.0), &run, window);
            assert_eq!(rows(&lines), 1);
            assert_eq!(lines[0].text, text);
        });
    }
}
