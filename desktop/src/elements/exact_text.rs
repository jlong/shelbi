//! A one-line label laid out at its shaped width.
//!
//! gpui's text element rounds its width up to a whole point
//! (`size.width.ceil()` in `TextLayout`), so a run of short labels in a row
//! drifts by up to a point per label. Figma doesn't round: the Navigation
//! component's hotkeys (the modifier and the key, 2pt apart) came out 1.5pt
//! wider than the frame's, moving every right-aligned column beside them
//! (`dt-figma-qa-sidebar`). [`exact_text`] shapes its string in the
//! inherited text style (optionally another family) and asks for exactly
//! that width, its baseline on the inherited font's (an SF ⌘ beside Geist
//! sits on Geist's baseline, as in Figma, not 1pt low). Like a plain string
//! it carries no accessibility role.

use std::panic::Location;

use gpui::{
    App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    Pixels, ShapedLine, SharedString, Style, TextAlign, Window,
};

/// A label of `text` at its exact shaped width.
pub fn exact_text(text: impl Into<SharedString>) -> ExactText {
    ExactText {
        text: text.into(),
        family: None,
    }
}

pub struct ExactText {
    text: SharedString,
    family: Option<SharedString>,
}

impl ExactText {
    /// Shape in `family` instead of the inherited one (the inherited size,
    /// weight and color stay).
    pub fn family(mut self, family: impl Into<SharedString>) -> Self {
        self.family = Some(family.into());
        self
    }
}

impl IntoElement for ExactText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ExactText {
    /// The shaped line, the line height, and how far down to paint it so its
    /// baseline meets the inherited font's.
    type RequestLayoutState = (ShapedLine, Pixels, Pixels);
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
        let mut text_style = window.text_style();
        let rem = window.rem_size();
        let font_size = text_style.font_size.to_pixels(rem);
        let line_height = text_style.line_height_in_pixels(rem);
        // The baseline sits where the inherited font puts it, whatever font
        // the glyphs come from (an override, a fallback), as in Figma.
        let text_system = window.text_system();
        let inherited = text_system.resolve_font(&text_style.font());
        let target = baseline(
            line_height,
            text_system.ascent(inherited, font_size),
            text_system.descent(inherited, font_size).abs(),
        );
        if let Some(family) = &self.family {
            text_style.font_family = family.clone();
        }
        let run = text_style.to_run(self.text.len());
        let line = text_system.shape_line(self.text.clone(), font_size, &[run], None);
        let shift = target - baseline(line_height, line.ascent, line.descent);
        let style = Style {
            flex_shrink: 0.0,
            size: gpui::size(line.width.into(), line_height.into()),
            ..Style::default()
        };
        (window.request_layout(style, [], cx), (line, line_height, shift))
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
        (line, line_height, shift): &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin = bounds.origin + gpui::point(Pixels::ZERO, *shift);
        let _ = line.paint(origin, *line_height, TextAlign::Left, None, window, cx);
    }
}

/// Where gpui puts a line's baseline in a box `line_height` tall: the
/// glyphs' ascent and descent centered.
fn baseline(line_height: Pixels, ascent: Pixels, descent: Pixels) -> Pixels {
    (line_height - ascent - descent) / 2. + ascent
}
