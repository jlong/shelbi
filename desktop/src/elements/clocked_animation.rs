//! gpui's one-shot `with_animation`, on the executor's clock.
//!
//! gpui's animation reads the real clock (`Instant::now`), which a test can
//! only move by sleeping and can't stop: a stalled thread walks the animation
//! on, so where it stands in any one frame depends on how busy the machine is.
//! This one reads [`gpui::BackgroundExecutor::now`], the real clock in the app
//! and the test clock under test, so `advance_clock` puts it at an exact point
//! and nothing else moves it. That's also the clock a `timer` runs on, so a
//! slide and the timer that ends it can't disagree in a test.
//!
//! The rest is as gpui has it: it starts on the first frame that draws it,
//! keeps its start for as long as every frame draws it, and with Reduce Motion
//! sits at its end and asks for no frames.

use std::panic::Location;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels,
    Window,
};

/// `element` animated once over `duration`: `animator` gets it at each frame's
/// eased progress, 0 to 1.
pub fn clocked_animation<E: IntoElement + 'static>(
    id: impl Into<ElementId>,
    element: E,
    duration: Duration,
    easing: fn(f32) -> f32,
    animator: impl Fn(E, f32) -> E + 'static,
) -> ClockedAnimation<E> {
    ClockedAnimation { id: id.into(), element: Some(element), animator: Box::new(animator), duration, easing }
}

/// The element [`clocked_animation`] returns.
pub struct ClockedAnimation<E> {
    id: ElementId,
    element: Option<E>,
    animator: Box<dyn Fn(E, f32) -> E>,
    duration: Duration,
    easing: fn(f32) -> f32,
}

impl<E: IntoElement + 'static> IntoElement for ClockedAnimation<E> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<E: IntoElement + 'static> Element for ClockedAnimation<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        let now = cx.background_executor().now();
        window.with_element_state(id.expect("the animation has an id"), |started: Option<Instant>, window| {
            let started = started.unwrap_or(now);
            let progress = if cx.reduce_motion() {
                1.0
            } else {
                progress(now.saturating_duration_since(started), self.duration)
            };
            if progress < 1.0 {
                window.request_animation_frame();
            }
            let element = self.element.take().expect("layout runs once per frame");
            let mut element = (self.animator)(element, (self.easing)(progress)).into_any_element();
            ((element.request_layout(window, cx), element), started)
        })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        element: &mut AnyElement,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

/// How far through `duration` an animation is, `elapsed` in: 1 at its end and
/// after (a zero `duration` has already ended).
fn progress(elapsed: Duration, duration: Duration) -> f32 {
    if elapsed >= duration {
        1.0
    } else {
        elapsed.as_secs_f32() / duration.as_secs_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_runs_from_zero_to_one_and_stays() {
        let d = Duration::from_millis(200);
        assert_eq!(progress(Duration::ZERO, d), 0.0);
        assert_eq!(progress(d / 2, d), 0.5);
        assert_eq!(progress(d, d), 1.0);
        assert_eq!(progress(d * 3, d), 1.0);
        assert_eq!(progress(Duration::ZERO, Duration::ZERO), 1.0);
    }
}
