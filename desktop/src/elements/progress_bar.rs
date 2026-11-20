//! A thin progress bar (07-importing): a 6pt rounded track with a fill. A
//! known fraction fills that much of it; an unknown one (indeterminate)
//! slides a segment back and forth. Under Reduce Motion gpui shows an
//! animation's end state, so the segment holds still at the track's start.

use std::time::Duration;

use gpui::{div, prelude::*, px, Animation, AnimationExt, Div, ElementId, Hsla};

/// The bar's height.
pub const HEIGHT: f32 = 6.0;
/// The sliding segment's share of the track while indeterminate.
const SEGMENT: f32 = 0.3;
/// One sweep there and back.
const PERIOD: Duration = Duration::from_millis(1600);

/// A bar `width` wide: `fraction` (0..=1) of it filled in `color` over
/// `track`, or with `None` a segment sliding to and fro. `id` keys the
/// animation and is the bar's debug selector (`{id}-fill` on the fill).
pub fn progress_bar(id: &'static str, fraction: Option<f32>, width: f32, color: Hsla, track: Hsla) -> Div {
    let bar = div()
        .debug_selector(move || id.into())
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(HEIGHT))
        .rounded_full()
        .overflow_hidden()
        .bg(track);
    let fill = |left: f32, w: f32| {
        div()
            .debug_selector(move || format!("{id}-fill"))
            .absolute()
            .top_0()
            .left(px(left))
            .w(px(w))
            .h_full()
            .rounded_full()
            .bg(color)
    };
    match fraction {
        Some(f) => bar.child(fill(0.0, width * f.clamp(0.0, 1.0))),
        None => {
            let seg = width * SEGMENT;
            bar.child(fill(0.0, seg).with_animation(
                ElementId::Name(format!("{id}-slide").into()),
                Animation::new(PERIOD).repeat(),
                move |d, t| {
                    // There and back: at the start again when t reaches 1.
                    let at = 1.0 - (2.0 * t - 1.0).abs();
                    d.left(px(at * (width - seg)))
                },
            ))
        }
    }
}
