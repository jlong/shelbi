//! The Figma `Spinner` (the Step Status component's In Progress variant):
//! a 32pt ring, 4pt thick, three quarters of the way round, fading from
//! transparent at its tail to the progress color at its head, where a 4pt dot
//! rounds it off. It turns clockwise, once a second.
//!
//! Figma draws the ring as an arc with an angular gradient, which gpui
//! can't fill, so the ring is painted as short arcs, each at the gradient's
//! opacity for its angle. Under Reduce Motion gpui shows an animation's end
//! state, so the ring holds still with its head at three o'clock, as drawn.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

use gpui::{
    canvas, div, point, prelude::*, px, Animation, AnimationExt, Bounds, ElementId, Hsla, PathBuilder,
    Pixels, Window,
};

/// The ring's width and height.
pub const SIZE: f32 = 32.0;
/// The ring's thickness (Figma's arc: inner radius 75% of 16).
const THICKNESS: f32 = 4.0;
/// How much of the circle the ring covers (the arc runs from six o'clock
/// clockwise to three).
const SWEEP: f32 = 0.75;
/// Where along the circle the gradient reaches full color (its stops: 0%
/// transparent, 70% and 75% opaque).
const OPAQUE_AT: f32 = 0.7;
/// One turn.
const PERIOD: Duration = Duration::from_secs(1);
/// The short arcs the ring is painted with.
const SEGMENTS: usize = 54;

/// The spinner in `color`. `id` keys its animation.
pub fn spinner(id: impl Into<ElementId>, color: Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(SIZE))
        .with_animation(id, Animation::new(PERIOD).repeat(), move |d, turn| {
            d.child(
                canvas(|_, _, _| {}, move |bounds, _, window, _| paint(bounds, turn, color, window))
                    .size_full(),
            )
        })
}

/// The gradient's opacity at `at` (0 at the tail, [`SWEEP`] at the head).
fn opacity_at(at: f32) -> f32 {
    (at / OPAQUE_AT).clamp(0.0, 1.0)
}

/// Paint the ring in `bounds`, `turn` (0..1) of the way round.
fn paint(bounds: Bounds<Pixels>, turn: f32, color: Hsla, window: &mut Window) {
    let center = bounds.center();
    let radius = SIZE / 2.0 - THICKNESS / 2.0;
    // Screen angles run clockwise from three o'clock; the tail starts at six.
    let start = FRAC_PI_2 + turn * TAU;
    let at = |fraction: f32| {
        let angle = start + fraction * TAU;
        point(
            center.x + px(radius * angle.cos()),
            center.y + px(radius * angle.sin()),
        )
    };
    let step = SWEEP / SEGMENTS as f32;
    for i in 0..SEGMENTS {
        let (from, to) = (i as f32 * step, (i + 1) as f32 * step);
        let alpha = opacity_at((from + to) / 2.0);
        if alpha <= 0.0 {
            continue;
        }
        let mut path = PathBuilder::stroke(px(THICKNESS));
        path.move_to(at(from));
        path.arc_to(point(px(radius), px(radius)), px(0.0), false, true, at(to));
        if let Ok(path) = path.build() {
            window.paint_path(path, color.opacity(alpha));
        }
    }
    // The head's dot, centered on the ring.
    let head = at(SWEEP);
    let dot = THICKNESS / 2.0;
    let mut path = PathBuilder::fill();
    path.move_to(point(head.x + px(dot), head.y));
    path.arc_to(point(px(dot), px(dot)), px(0.0), false, true, point(head.x - px(dot), head.y));
    path.arc_to(point(px(dot), px(dot)), px(0.0), false, true, point(head.x + px(dot), head.y));
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_fades_in_from_its_tail_and_is_full_near_its_head() {
        assert_eq!(opacity_at(0.0), 0.0);
        assert!((opacity_at(0.35) - 0.5).abs() < 1e-6);
        assert_eq!(opacity_at(OPAQUE_AT), 1.0);
        assert_eq!(opacity_at(SWEEP), 1.0);
    }
}
