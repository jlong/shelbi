//! One line of text turned 90° clockwise, reading top to bottom, as Figma
//! labels the board's collapsed column (`dt-collapsed-column-rotated`).
//!
//! gpui can't rotate text: glyphs go to the scene as axis-aligned sprites,
//! and only an SVG takes a transformation. So [`rotated_text`] shapes the
//! line with gpui's own shaper (CoreText, so its length and any ellipsis
//! cut agree with how the same text lays out unrotated), traces each
//! glyph's outline from the font file, turned, into an SVG path, and
//! paints that as a monochrome sprite in the text's color, the way gpui
//! paints a glyph or an icon. The bundled Geist faces supply the outlines;
//! a character they lack comes from a system face that has it.
//!
//! The element is a line height wide and as tall as the shaped line. With
//! [`RotatedText::truncate`] it may shrink to the room it's given (a flex
//! column's leftover height), and then ends in an ellipsis where it's cut.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::panic::Location;
use std::sync::{Arc, LazyLock};

use gpui::{
    px, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    Pixels, ShapedLine, SharedString, Style, TextStyle, TransformationMatrix, Window,
};

use crate::theme::style::{Type, UI_FAMILY};

/// The ellipsis a cut label ends with.
const ELLIPSIS: &str = "…";
/// How many traced labels the path cache keeps before it starts over.
const CACHE_LIMIT: usize = 256;

/// `text` in `ty` and `color`, turned 90° clockwise.
pub fn rotated_text(text: impl Into<SharedString>, ty: Type, color: Hsla) -> RotatedText {
    RotatedText {
        text: text.into(),
        ty,
        color,
        truncate: false,
    }
}

pub struct RotatedText {
    text: SharedString,
    ty: Type,
    color: Hsla,
    truncate: bool,
}

impl RotatedText {
    /// Shrink to the room the layout gives, ending in an ellipsis when the
    /// whole line doesn't fit. Without it the element keeps its full length.
    pub fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }

    fn text_style(&self, window: &Window) -> TextStyle {
        let mut style = window.text_style();
        style.font_family = UI_FAMILY.into();
        style.font_weight = self.ty.weight();
        style.font_size = px(self.ty.size()).into();
        style.line_height = px(self.ty.line_height()).into();
        style.color = self.color;
        style
    }
}

impl IntoElement for RotatedText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RotatedText {
    /// The whole line, shaped.
    type RequestLayoutState = ShapedLine;
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
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = self.text_style(window);
        let line = shape(&style, self.text.clone(), window);
        let mut layout = Style::default();
        layout.size.width = px(self.ty.line_height()).into();
        layout.size.height = line.width.into();
        if self.truncate {
            layout.flex_shrink = 1.0;
            layout.min_size.height = px(0.0).into();
        } else {
            layout.flex_shrink = 0.0;
        }
        (window.request_layout(layout, [], cx), line)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _line: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        line: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let style = self.text_style(window);
        let Some(line) = fit(line, bounds.size.height, &style, window) else {
            return;
        };
        let line_height = px(self.ty.line_height());
        let length = line.width.ceil();
        let key = SharedString::from(format!(
            "rotated-text/{}/{}/{}/{}/{}",
            style.font_family, style.font_weight.0, self.ty.size(), self.ty.line_height(), line.text
        ));
        let svg = traced(&key, || trace(&line, &style, line_height, length, window));
        let bounds = Bounds::new(bounds.origin, gpui::size(line_height, length));
        let _ = window.paint_svg(bounds, key, Some(&svg), TransformationMatrix::unit(), self.color, cx);
    }
}

/// `text` shaped in `style`.
fn shape(style: &TextStyle, text: SharedString, window: &Window) -> ShapedLine {
    let size = style.font_size.to_pixels(window.rem_size());
    let run = style.to_run(text.len());
    window.text_system().shape_line(text, size, &[run], None)
}

/// `line` as it fits in `room`: whole when it fits, else cut to the longest
/// start that fits beside the ellipsis (without trailing spaces or
/// punctuation), or `None` when not even the ellipsis fits.
fn fit(line: &ShapedLine, room: Pixels, style: &TextStyle, window: &Window) -> Option<ShapedLine> {
    // Layout rounds to device pixels; a line that "overflows" by less than
    // that fits.
    if line.width <= room + px(0.5) {
        return Some(line.clone());
    }
    let ellipsis = shape(style, ELLIPSIS.into(), window);
    if ellipsis.width > room {
        return None;
    }
    let text = &line.text;
    let mut cut = line.index_for_x(room - ellipsis.width).unwrap_or(text.len());
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let kept = text[..cut].trim_end_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
    Some(shape(style, format!("{kept}{ELLIPSIS}").into(), window))
}

thread_local! {
    /// Traced labels by their sprite key, so a label is traced once, not
    /// every frame (the sprite atlas only asks for it once per size, but
    /// `paint_svg` takes the data up front).
    static TRACED: RefCell<HashMap<SharedString, Arc<[u8]>>> = RefCell::new(HashMap::new());
}

/// The SVG traced for `key`, tracing it with `trace` the first time.
fn traced(key: &SharedString, trace: impl FnOnce() -> String) -> Arc<[u8]> {
    TRACED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(svg) = cache.get(key) {
            return svg.clone();
        }
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        let svg: Arc<[u8]> = trace().into_bytes().into();
        cache.insert(key.clone(), svg.clone());
        svg
    })
}

/// `line`'s glyph outlines as an SVG `line_height` wide and `length` tall:
/// the line turned 90° clockwise, its start at the top and the tops of its
/// letters to the right.
fn trace(line: &ShapedLine, style: &TextStyle, line_height: Pixels, length: Pixels, window: &Window) -> String {
    let font_size = f32::from(line.font_size);
    let width = f32::from(line_height);
    // Where gpui puts the baseline in a line box (`ShapedLine::paint`).
    let baseline = f32::from((line_height - line.ascent - line.descent) / 2.0 + line.ascent);
    let primary = window.text_system().resolve_font(&style.font());
    let ui = ttf_parser::Face::parse(crate::assets::ui_face(style.font_weight), 0).ok();
    let mut path = String::new();
    for run in &line.runs {
        for glyph in &run.glyphs {
            let x = f32::from(glyph.position.x);
            let mut pen = Pen {
                path: &mut path,
                origin: (x, baseline),
                scale: 0.0,
                width,
            };
            match &ui {
                Some(face) if run.font_id == primary => {
                    pen.scale = font_size / f32::from(face.units_per_em());
                    face.outline_glyph(ttf_parser::GlyphId(glyph.id.0 as u16), &mut pen);
                }
                // Another face drew this glyph (CoreText's fallback for a
                // character Geist lacks): trace the character from a system
                // face that has it.
                _ => {
                    if let Some(ch) = line.text.get(glyph.index..).and_then(|s| s.chars().next()) {
                        fallback_outline(ch, font_size, &mut pen);
                    }
                }
            }
        }
    }
    let length = f32::from(length);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{length}\" \
         viewBox=\"0 0 {width} {length}\"><path d=\"{path}\"/></svg>"
    )
}

/// The system's fonts, loaded the first time a label needs a character the
/// bundled faces lack.
static SYSTEM_FONTS: LazyLock<fontdb::Database> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    db
});

/// Trace `ch` from the first system face that has it.
fn fallback_outline(ch: char, font_size: f32, pen: &mut Pen) {
    for face in SYSTEM_FONTS.faces() {
        let traced = SYSTEM_FONTS.with_face_data(face.id, |data, index| {
            let face = ttf_parser::Face::parse(data, index).ok()?;
            let glyph = face.glyph_index(ch)?;
            pen.scale = font_size / f32::from(face.units_per_em());
            face.outline_glyph(glyph, pen)
        });
        if traced.flatten().is_some() {
            return;
        }
    }
}

/// Writes a glyph's outline into an SVG path, from font units (y up, at the
/// glyph's origin on the baseline) to the turned label's points.
struct Pen<'a> {
    path: &'a mut String,
    /// The glyph's origin in the unturned line box: its x along the line
    /// and the baseline's y.
    origin: (f32, f32),
    /// Points per font unit.
    scale: f32,
    /// The label's width: the line box's height.
    width: f32,
}

impl Pen<'_> {
    fn point(&mut self, x: f32, y: f32) {
        let (along, across) = turn(self.origin, self.scale, self.width, x, y);
        let _ = write!(self.path, "{along:.2} {across:.2} ");
    }
}

/// A font-unit point of a glyph at `origin` (in the unturned line box) as
/// the turned label's (x, y): the line's x runs down the label, and its
/// top (y = 0) lands at the label's right edge.
fn turn(origin: (f32, f32), scale: f32, width: f32, x: f32, y: f32) -> (f32, f32) {
    let line_x = origin.0 + x * scale;
    let line_y = origin.1 - y * scale;
    (width - line_y, line_x)
}

impl ttf_parser::OutlineBuilder for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.push('M');
        self.point(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.push('L');
        self.point(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.push('Q');
        self.point(x1, y1);
        self.point(x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.push('C');
        self.point(x1, y1);
        self.point(x2, y2);
        self.point(x, y);
    }

    fn close(&mut self) {
        self.path.push('Z');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn the_line_turns_clockwise_reading_down() {
        // A 16pt line box, baseline at 12: the glyph's origin sits 12pt from
        // the label's right edge, at its x down the label.
        let origin = (30.0, 12.0);
        assert_eq!(turn(origin, 0.01, 16.0, 0.0, 0.0), (4.0, 30.0));
        // Up the glyph (font y) is right on the label.
        assert_eq!(turn(origin, 0.01, 16.0, 0.0, 800.0), (12.0, 30.0));
        // Along the glyph (font x) is down the label.
        assert_eq!(turn(origin, 0.01, 16.0, 500.0, 0.0), (4.0, 35.0));
    }

    #[test]
    fn a_traced_label_is_kept() {
        let key = SharedString::from("rotated-text/test/kept");
        let mut traces = 0;
        for _ in 0..2 {
            let svg = traced(&key, || {
                traces += 1;
                "<svg/>".into()
            });
            assert_eq!(&*svg, b"<svg/>");
        }
        assert_eq!(traces, 1);
    }

    #[gpui::test]
    fn a_label_that_fits_is_whole_and_one_that_doesnt_is_cut(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|window, _| {
            let label = rotated_text("NEEDS PRODUCT REVIEW", Type::XSmallSemibold, gpui::black());
            let style = label.text_style(window);
            let line = shape(&style, label.text.clone(), window);
            assert!(line.width > px(0.0));

            let whole = fit(&line, line.width + px(4.0), &style, window).unwrap();
            assert_eq!(whole.text, line.text);

            let room = line.width / 2.0;
            let cut = fit(&line, room, &style, window).unwrap();
            let kept = cut.text.strip_suffix(ELLIPSIS).expect("ends in the ellipsis");
            assert!(line.text.starts_with(kept), "{:?}", cut.text);
            assert!(!kept.ends_with(' '), "{:?}", cut.text);
            assert!(cut.width <= room + px(0.5), "{:?} > {room:?}", cut.width);

            assert!(fit(&line, px(0.5), &style, window).is_none(), "no room for the ellipsis");
        });
    }

    #[gpui::test]
    fn a_label_traces_a_path_as_long_as_its_line(cx: &mut TestAppContext) {
        let cx = cx.add_empty_window();
        cx.update(|window, _| {
            let label = rotated_text("BACKLOG", Type::XSmallSemibold, gpui::black());
            let style = label.text_style(window);
            let line = shape(&style, label.text.clone(), window);
            let svg = trace(&line, &style, px(16.0), line.width.ceil(), window);
            assert!(svg.starts_with("<svg"), "{svg}");
            assert!(svg.contains("width=\"16\""), "{svg}");
            assert!(svg.contains("<path d=\"M"), "a glyph was traced: {svg}");
        });
    }
}
