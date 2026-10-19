//! Disclosure folds: a collapsible list's body closing to nothing and
//! opening back to its height, its header's chevron turning with it.
//!
//! [`Folds`] keeps the motion only: whether a body is collapsed stays with
//! its owner (the sidebar's collapsed sections, the shared model's collapsed
//! machine groups), which calls [`Folds::toggle`] when the user flips one and
//! asks [`Folds::openness`] each frame. A fold runs on Taskpage's curve
//! ([`ease_out_expo`]) for [`FOLD`]; flipped again mid-fold it turns around
//! from where it is, so nothing jumps. Under Reduce Motion, and for anything
//! that wasn't a toggle (a restore, a live update), the body is simply at
//! its end state.
//!
//! [`Folds::body`] draws a body at an openness: its content at its natural
//! height inside a clip that is that fraction of it (fading with it), the
//! content measured as it paints so the clip knows the height to scale.
//! [`chevron`] turns a disclosure chevron between pointing down (open) and
//! right (collapsed).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{canvas, div, prelude::*, px, Div, Transformation};

use super::motion::ease_out_expo;

/// How long a body takes to open or close all the way: quick, like a
/// native outline view's disclosure.
pub const FOLD: Duration = Duration::from_millis(220);

/// A turnaround from close to the end takes at least this share of
/// [`FOLD`], so it doesn't snap back.
const MIN_SHARE: f32 = 0.4;

/// The folds running for bodies keyed by `K`, and each body's natural
/// height as last painted.
#[derive(Debug)]
pub struct Folds<K: Ord + Clone + 'static> {
    running: BTreeMap<K, Fold>,
    heights: Rc<RefCell<BTreeMap<K, f32>>>,
}

impl<K: Ord + Clone + 'static> Default for Folds<K> {
    fn default() -> Self {
        Folds {
            running: BTreeMap::new(),
            heights: Rc::default(),
        }
    }
}

/// One body opening or closing: from an openness (0 closed, 1 open) at
/// `start`, toward collapsed or open.
#[derive(Debug, Clone, Copy)]
struct Fold {
    start: Instant,
    from: f32,
    collapsing: bool,
    duration: Duration,
}

impl Fold {
    fn target(&self) -> f32 {
        if self.collapsing {
            0.0
        } else {
            1.0
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32();
        t.min(1.0)
    }

    fn openness(&self, now: Instant) -> f32 {
        self.from + (self.target() - self.from) * ease_out_expo(self.progress(now))
    }

    fn done(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }
}

/// The resting openness of a body that is (or isn't) collapsed.
fn rest(collapsed: bool) -> f32 {
    if collapsed {
        0.0
    } else {
        1.0
    }
}

impl<K: Ord + Clone + 'static> Folds<K> {
    /// The body `key` was just flipped to `collapsed`: fold it there from
    /// where it is now (mid-fold included). Under `reduce_motion` it is
    /// there at once.
    pub fn toggle(&mut self, key: K, collapsed: bool, now: Instant, reduce_motion: bool) {
        let from = match self.running.get(&key) {
            Some(fold) if !fold.done(now) => fold.openness(now),
            _ => rest(!collapsed),
        };
        if reduce_motion {
            self.running.remove(&key);
            return;
        }
        let share = (rest(collapsed) - from).abs().max(MIN_SHARE);
        self.running.insert(
            key,
            Fold {
                start: now,
                from,
                collapsing: collapsed,
                duration: FOLD.mul_f32(share),
            },
        );
    }

    /// How open body `key` is at `now`: 0 (closed) to 1 (open), eased. A
    /// fold heading somewhere other than `collapsed` (the owner changed it
    /// without a toggle) counts for nothing.
    pub fn openness(&self, key: &K, collapsed: bool, now: Instant) -> f32 {
        match self.running.get(key) {
            Some(fold) if fold.collapsing == collapsed && !fold.done(now) => fold.openness(now),
            _ => rest(collapsed),
        }
    }

    /// Whether body `key` is still on its way to `collapsed` at `now` (a
    /// collapsing body is still drawn).
    pub fn is_folding(&self, key: &K, collapsed: bool, now: Instant) -> bool {
        self.running
            .get(key)
            .is_some_and(|fold| fold.collapsing == collapsed && !fold.done(now))
    }

    /// Whether any fold is still running at `now` (the window keeps drawing
    /// frames).
    pub fn animating(&self, now: Instant) -> bool {
        self.running.values().any(|fold| !fold.done(now))
    }

    /// Forget the folds that have finished by `now`.
    pub fn settle(&mut self, now: Instant) {
        self.running.retain(|_, fold| !fold.done(now));
    }

    /// Stop every fold where it ends (a restore, a project switch: what the
    /// owner now says is drawn as is).
    pub fn clear(&mut self) {
        self.running.clear();
    }

    /// Body `key`'s `content` (a column of rows) at `openness`: open, the
    /// content as is; otherwise clipped to that share of its natural height
    /// and faded by it. The content is measured each frame it paints, so a
    /// fold starts from the height it last had and follows rows that come
    /// and go mid-fold.
    pub fn body(&self, key: K, openness: f32, content: Div) -> Div {
        let heights = self.heights.clone();
        let measured = {
            let key = key.clone();
            canvas(
                move |bounds, _, _| {
                    heights.borrow_mut().insert(key, bounds.size.height.as_f32());
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };
        let content = content.relative().flex_none().child(measured);
        if openness >= 1.0 {
            return content;
        }
        let natural = self.heights.borrow().get(&key).copied().unwrap_or(0.0);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .overflow_hidden()
            .h(px(natural * openness.max(0.0)))
            .opacity(openness.clamp(0.0, 1.0))
            .child(content)
    }
}

/// A disclosure chevron drawn pointing down, turned for `openness`: down
/// when open, right when closed, in between mid-fold.
pub fn chevron(openness: f32) -> Transformation {
    Transformation::rotate(gpui::radians(chevron_angle(openness)))
}

/// The chevron's turn at `openness`, in radians: 0 open, a quarter turn
/// counterclockwise closed.
fn chevron_angle(openness: f32) -> f32 {
    -std::f32::consts::FRAC_PI_2 * (1.0 - openness.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_fold_closes_on_the_curve_and_settles() {
        let mut f: Folds<u8> = Folds::default();
        let t0 = Instant::now();
        assert_eq!(f.openness(&1, false, t0), 1.0, "open at rest");
        f.toggle(1, true, t0, false);
        assert_eq!(f.openness(&1, true, t0), 1.0, "starts where it was");
        assert!(f.is_folding(&1, true, t0));
        let early = f.openness(&1, true, t0 + ms(40));
        let late = f.openness(&1, true, t0 + ms(150));
        assert!(early < 0.75 && early > 0.0, "fast out: {early}");
        assert!(late < early && late > 0.0, "{late}");
        assert!(f.animating(t0 + ms(150)));
        assert_eq!(f.openness(&1, true, t0 + FOLD), 0.0, "closed");
        assert!(!f.animating(t0 + FOLD));
        assert!(!f.is_folding(&1, true, t0 + FOLD));
        f.settle(t0 + FOLD);
        assert!(f.running.is_empty());
    }

    #[test]
    fn a_fold_opens_from_closed() {
        let mut f: Folds<u8> = Folds::default();
        let t0 = Instant::now();
        f.toggle(1, false, t0, false);
        assert_eq!(f.openness(&1, false, t0), 0.0);
        assert!(f.openness(&1, false, t0 + ms(100)) > 0.5);
        assert_eq!(f.openness(&1, false, t0 + FOLD), 1.0);
    }

    #[test]
    fn flipping_mid_fold_turns_around_without_a_jump() {
        let mut f: Folds<u8> = Folds::default();
        let t0 = Instant::now();
        f.toggle(1, true, t0, false);
        let at = t0 + ms(60);
        let before = f.openness(&1, true, at);
        f.toggle(1, false, at, false);
        let after = f.openness(&1, false, at);
        assert!((before - after).abs() < 1e-6, "{before} -> {after}");
        // It heads back open from there, and gets there quicker than a
        // whole fold (it has less of the way to go).
        assert!(f.openness(&1, false, at + ms(30)) > after);
        let share = (1.0 - after).max(MIN_SHARE);
        assert_eq!(f.openness(&1, false, at + FOLD.mul_f32(share)), 1.0);
        assert!(f.animating(at + FOLD.mul_f32(share) - ms(1)));
    }

    #[test]
    fn reduce_motion_lands_at_once() {
        let mut f: Folds<u8> = Folds::default();
        let t0 = Instant::now();
        f.toggle(1, true, t0, true);
        assert_eq!(f.openness(&1, true, t0), 0.0);
        assert!(!f.animating(t0));
        // Mid-fold, too.
        f.toggle(1, false, t0, false);
        f.toggle(1, true, t0 + ms(50), true);
        assert_eq!(f.openness(&1, true, t0 + ms(50)), 0.0);
        assert!(!f.animating(t0 + ms(50)));
    }

    #[test]
    fn clear_and_an_owner_change_end_a_fold() {
        let mut f: Folds<u8> = Folds::default();
        let t0 = Instant::now();
        f.toggle(1, true, t0, false);
        // The owner says open again without a toggle: no fold to draw.
        assert_eq!(f.openness(&1, false, t0 + ms(50)), 1.0);
        assert!(!f.is_folding(&1, false, t0 + ms(50)));
        f.clear();
        assert_eq!(f.openness(&1, true, t0 + ms(50)), 0.0);
        assert!(!f.animating(t0 + ms(50)));
    }

    #[test]
    fn the_chevron_turns_with_the_fold() {
        assert_eq!(chevron_angle(1.0), 0.0, "down when open");
        assert_eq!(chevron_angle(0.0), -std::f32::consts::FRAC_PI_2, "right when closed");
        assert_eq!(chevron_angle(0.5), -std::f32::consts::FRAC_PI_4);
    }
}
