//! The window's leading column and the push between its two occupants
//! (`dt-review-replaces-sidebar`).
//!
//! The column shows the navigation sidebar, or, while a review is open, the
//! review panel in its place, as the TUI's review panel replaces its sidebar.
//! Going in and coming back out are a push between two levels of a
//! navigation stack: the sidebar slides out to the left and fades while the
//! panel slides in from the right and fades in, and the reverse on the way
//! back. With Reduce Motion on, the two crossfade in place instead.
//!
//! Toolkit-free and clock-injected, so the state machine (entering, leaving,
//! a second toggle mid-transition reversing from where it stands) is tested
//! without a frame loop. The window asks [`ColumnSwap::frame`] what to draw
//! each frame and keeps requesting frames while it says it is animating.

use std::time::{Duration, Instant};

/// What the column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupant {
    Sidebar,
    Review,
}

impl Occupant {
    /// The occupant's place on the transition's axis: 0 the sidebar, 1 the
    /// review panel.
    fn position(self) -> f32 {
        match self {
            Occupant::Sidebar => 0.0,
            Occupant::Review => 1.0,
        }
    }
}

/// How a swap moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Horizontal slide plus fade.
    Slide,
    /// Crossfade in place (macOS Reduce Motion).
    Fade,
}

impl Motion {
    /// The time a full swap takes.
    pub fn duration(self) -> Duration {
        match self {
            Motion::Slide => SLIDE,
            Motion::Fade => FADE,
        }
    }
}

/// A full slide.
pub const SLIDE: Duration = Duration::from_millis(220);
/// A full crossfade (Reduce Motion): shorter, there is no distance to cover.
pub const FADE: Duration = Duration::from_millis(150);

/// Cubic ease-out: fast off the mark, settling into place.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// One occupant's placement in a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    /// Horizontal offset as a fraction of the column's width: negative is
    /// left of the column, positive right of it.
    pub offset: f32,
    pub opacity: f32,
}

impl Layer {
    const REST: Layer = Layer {
        offset: 0.0,
        opacity: 1.0,
    };
}

/// What one frame of the column draws. Both layers only while a swap runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub sidebar: Option<Layer>,
    pub review: Option<Layer>,
    /// A swap is still running: request another frame.
    pub animating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Transition {
    /// The position it started from (a reversed swap starts mid-way).
    from: f32,
    started: Instant,
    duration: Duration,
    motion: Motion,
}

/// The column's occupant and the swap under way, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSwap {
    target: Occupant,
    transition: Option<Transition>,
}

impl Default for ColumnSwap {
    fn default() -> Self {
        Self::new()
    }
}

impl ColumnSwap {
    /// The sidebar, at rest.
    pub fn new() -> Self {
        Self {
            target: Occupant::Sidebar,
            transition: None,
        }
    }

    /// The occupant the column shows, or is on its way to.
    pub fn target(&self) -> Occupant {
        self.target
    }

    /// A swap is under way (as of its last [`Self::settle`]).
    #[cfg(test)]
    pub fn is_animating(&self) -> bool {
        self.transition.is_some()
    }

    /// Swap to `to`, starting at `now`. A no-op when that is already the
    /// target. Mid-swap, it reverses from where the column stands, taking the
    /// share of the full duration that is left to cover, so the motion never
    /// jumps. Returns whether anything changed.
    pub fn show(&mut self, to: Occupant, motion: Motion, now: Instant) -> bool {
        if to == self.target {
            return false;
        }
        let from = self.position(now);
        self.target = to;
        let distance = (to.position() - from).abs();
        self.transition = Some(Transition {
            from,
            started: now,
            duration: motion.duration().mul_f32(distance),
            motion,
        });
        true
    }

    /// Jump to the end of any swap under way.
    #[cfg(test)]
    pub fn finish(&mut self) {
        self.transition = None;
    }

    /// Drop a swap that has run its course by `now`.
    pub fn settle(&mut self, now: Instant) {
        if self.transition.is_some_and(|t| progress(&t, now) >= 1.0) {
            self.transition = None;
        }
    }

    /// Where the column stands at `now` on the 0 (sidebar) to 1 (review)
    /// axis, eased.
    pub fn position(&self, now: Instant) -> f32 {
        let to = self.target.position();
        match &self.transition {
            None => to,
            Some(t) => t.from + (to - t.from) * ease_out(progress(t, now)),
        }
    }

    /// What to draw at `now`: settles a finished swap first.
    pub fn frame(&mut self, now: Instant) -> Frame {
        self.settle(now);
        let Some(t) = self.transition else {
            return match self.target {
                Occupant::Sidebar => Frame {
                    sidebar: Some(Layer::REST),
                    review: None,
                    animating: false,
                },
                Occupant::Review => Frame {
                    sidebar: None,
                    review: Some(Layer::REST),
                    animating: false,
                },
            };
        };
        let p = self.position(now);
        let slide = match t.motion {
            Motion::Slide => 1.0,
            Motion::Fade => 0.0,
        };
        Frame {
            // The sidebar is the lower level: it leaves to the left.
            sidebar: Some(Layer {
                offset: -p * slide,
                opacity: 1.0 - p,
            }),
            // The review panel is pushed on: it comes in from the right.
            review: Some(Layer {
                offset: (1.0 - p) * slide,
                opacity: p,
            }),
            animating: true,
        }
    }
}

/// What the window's leading column holds in one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    /// The column's width in TUI columns (`sidebar::COLUMN_PX` each), the
    /// same through a swap so the session beside it never moves.
    pub width_cols: u16,
    pub sidebar: Option<Layer>,
    pub review: Option<Layer>,
}

/// The leading column for `frame`, or `None` when there is none (the sidebar
/// collapsed, no review in its place). The column is the user's sidebar
/// width (`user_cols`). A collapsed sidebar is never drawn, but a review
/// still takes the column, at the default sidebar width until the user drags
/// it (`dragged_cols`), and the column goes again once the review has left.
pub fn column(frame: &Frame, collapsed: bool, user_cols: u16, dragged_cols: Option<u16>) -> Option<Column> {
    let sidebar = frame.sidebar.filter(|_| !collapsed);
    if sidebar.is_none() && frame.review.is_none() {
        return None;
    }
    let width_cols = if collapsed {
        dragged_cols.unwrap_or(shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH)
    } else {
        user_cols
    };
    Some(Column {
        width_cols,
        sidebar,
        review: frame.review,
    })
}

/// The linear share of `t` elapsed at `now`, 0 to 1.
fn progress(t: &Transition, now: Instant) -> f32 {
    if t.duration.is_zero() {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(t.started);
    (elapsed.as_secs_f32() / t.duration.as_secs_f32()).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn at_rest_only_the_occupant_draws() {
        let mut swap = ColumnSwap::new();
        let now = Instant::now();
        let f = swap.frame(now);
        assert_eq!(f.sidebar, Some(Layer::REST));
        assert_eq!(f.review, None);
        assert!(!f.animating);
        assert!(!swap.show(Occupant::Sidebar, Motion::Slide, now), "already there");
        assert!(!swap.is_animating());
    }

    #[test]
    fn entering_pushes_the_panel_in_from_the_right_and_the_sidebar_out_left() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        assert!(swap.show(Occupant::Review, Motion::Slide, t0));
        assert_eq!(swap.target(), Occupant::Review);

        // The first frame: the sidebar still in place, the panel just right
        // of the column, invisible.
        let f = swap.frame(t0);
        assert!(f.animating);
        assert_eq!(f.sidebar, Some(Layer { offset: 0.0, opacity: 1.0 }));
        assert_eq!(f.review, Some(Layer { offset: 1.0, opacity: 0.0 }));

        // Mid-way: both move, the sidebar left, the panel in from the right,
        // the opacities crossing; ease-out is past half by half-time.
        let f = swap.frame(t0 + SLIDE / 2);
        let (s, r) = (f.sidebar.unwrap(), f.review.unwrap());
        assert!(s.offset < 0.0 && r.offset > 0.0);
        assert!(r.opacity > 0.5, "ease-out: {}", r.opacity);
        assert!(close(s.opacity + r.opacity, 1.0));
        assert!(close(r.offset - s.offset, 1.0), "the two stay a column apart");

        // Done: only the panel, at rest.
        let f = swap.frame(t0 + SLIDE);
        assert_eq!(f.sidebar, None);
        assert_eq!(f.review, Some(Layer::REST));
        assert!(!f.animating);
        assert!(!swap.is_animating());
    }

    #[test]
    fn leaving_plays_the_reverse() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Review, Motion::Slide, t0);
        swap.finish();
        assert!(swap.show(Occupant::Sidebar, Motion::Slide, t0));

        let f = swap.frame(t0);
        assert_eq!(f.review, Some(Layer { offset: 0.0, opacity: 1.0 }));
        assert_eq!(f.sidebar, Some(Layer { offset: -1.0, opacity: 0.0 }));

        let f = swap.frame(t0 + ms(100));
        let (s, r) = (f.sidebar.unwrap(), f.review.unwrap());
        assert!(r.offset > 0.0, "the panel slides out to the right");
        assert!(s.offset < 0.0 && s.offset > -1.0, "the sidebar comes back from the left");

        let f = swap.frame(t0 + SLIDE);
        assert_eq!(f.sidebar, Some(Layer::REST));
        assert_eq!(f.review, None);
    }

    #[test]
    fn a_second_toggle_mid_swap_reverses_from_where_it_stands() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Review, Motion::Slide, t0);
        let mid = t0 + ms(60);
        let at = swap.position(mid);
        assert!(at > 0.0 && at < 1.0);

        // Back before the push finished: no jump, it turns around.
        assert!(swap.show(Occupant::Sidebar, Motion::Slide, mid));
        assert_eq!(swap.target(), Occupant::Sidebar);
        assert!(close(swap.position(mid), at), "continues from {at}");
        assert!(swap.frame(mid + ms(1)).animating);

        // It covers only the distance left: the share `at` of a full slide.
        let back = SLIDE.mul_f32(at);
        assert!(swap.frame(mid + back - ms(5)).animating);
        let f = swap.frame(mid + back);
        assert!(!f.animating);
        assert_eq!(f.sidebar, Some(Layer::REST));
        assert_eq!(f.review, None);

        // And again the other way, from rest: a full slide.
        swap.show(Occupant::Review, Motion::Slide, t0 + SLIDE);
        assert!(swap.frame(t0 + SLIDE + ms(200)).animating);
        assert!(!swap.frame(t0 + SLIDE * 2).animating);
    }

    #[test]
    fn toggling_back_and_forth_mid_swap_never_jumps() {
        let mut swap = ColumnSwap::new();
        let mut now = Instant::now();
        let mut last = swap.position(now);
        for (i, step) in [30u64, 50, 20, 80, 10, 40].into_iter().enumerate() {
            let to = if i % 2 == 0 { Occupant::Review } else { Occupant::Sidebar };
            swap.show(to, Motion::Slide, now);
            assert!(close(swap.position(now), last), "toggle {i} jumped");
            now += ms(step);
            last = swap.position(now);
        }
    }

    #[test]
    fn reduce_motion_crossfades_in_place() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Review, Motion::Fade, t0);
        let f = swap.frame(t0 + FADE / 2);
        let (s, r) = (f.sidebar.unwrap(), f.review.unwrap());
        assert_eq!((s.offset, r.offset), (0.0, 0.0), "no slide");
        assert!(r.opacity > 0.0 && s.opacity > 0.0);
        assert!(close(s.opacity + r.opacity, 1.0));
        assert!(!swap.frame(t0 + FADE).animating, "a crossfade is shorter");
        assert!(FADE < SLIDE);
    }

    #[test]
    fn the_column_holds_the_sidebar_or_the_review_at_the_users_width() {
        let t0 = Instant::now();
        let mut swap = ColumnSwap::new();
        // No review: the sidebar at the user's width.
        let c = column(&swap.frame(t0), false, 40, None).unwrap();
        assert_eq!((c.width_cols, c.sidebar.is_some(), c.review.is_some()), (40, true, false));

        // A review open: the panel alone, the same width.
        swap.show(Occupant::Review, Motion::Slide, t0);
        let mid = column(&swap.frame(t0 + SLIDE / 2), false, 40, None).unwrap();
        assert_eq!(mid.width_cols, 40, "the width holds through the swap");
        assert!(mid.sidebar.is_some() && mid.review.is_some(), "both while it moves");
        let c = column(&swap.frame(t0 + SLIDE), false, 40, None).unwrap();
        assert_eq!((c.width_cols, c.sidebar.is_some(), c.review.is_some()), (40, false, true));
    }

    #[test]
    fn a_collapsed_sidebar_lends_its_column_to_a_review_and_takes_it_back() {
        let t0 = Instant::now();
        let mut swap = ColumnSwap::new();
        let default = shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH;
        assert_eq!(column(&swap.frame(t0), true, 40, None), None, "collapsed: no column");

        // A review shows at the default width, never the hidden sidebar.
        swap.show(Occupant::Review, Motion::Slide, t0);
        let mid = column(&swap.frame(t0 + SLIDE / 2), true, 40, None).unwrap();
        assert_eq!((mid.width_cols, mid.sidebar), (default, None));
        assert!(mid.review.is_some());
        // Dragged, it follows the drag.
        let c = column(&swap.frame(t0 + SLIDE), true, 40, Some(33)).unwrap();
        assert_eq!(c.width_cols, 33);

        // Back: the panel leaves, then the column goes, still collapsed.
        let t1 = t0 + SLIDE;
        swap.show(Occupant::Sidebar, Motion::Slide, t1);
        assert!(column(&swap.frame(t1 + SLIDE / 2), true, 40, None).is_some());
        assert_eq!(column(&swap.frame(t1 + SLIDE), true, 40, None), None);
    }

    #[test]
    fn ease_out_starts_fast_and_lands_on_the_ends() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.25) > 0.25);
        assert_eq!(ease_out(2.0), 1.0, "clamped");
    }
}
