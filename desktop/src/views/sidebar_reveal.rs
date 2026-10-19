//! The sidebar's hide and show (`dt-sidebar-toggle-animation`).
//!
//! Toggle sidebar slides the leading column out to the left, its content at
//! its full width and clipped (never laid out at an in-between width), while
//! the main area's leading edge follows it and the main area grows into the
//! room; showing it plays the reverse. The motion runs on Taskpage's
//! [`ease_out_expo`] over [`REVEAL`]. A second toggle mid-way turns it around
//! from where it stands, covering only the distance back. Reduce Motion makes
//! it instant, and a window that opens collapsed starts at rest.
//!
//! Toolkit-free and clock-injected, like the column's push
//! ([`super::column_swap`]): the window asks [`SidebarReveal::frame`] where
//! the sidebar stands each frame, and [`place`] how wide the column draws and
//! where its content sits in it. Whatever the column holds (the sidebar, a
//! panel in its place, another view's sidebar) slides the same way.

use std::time::{Duration, Instant};

use crate::theme::motion::ease_out_expo;
use crate::views::column_swap::FIRST_STEP;

/// A full hide or show.
pub const REVEAL: Duration = Duration::from_millis(250);

/// Where the sidebar stands in one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RevealFrame {
    /// 0 hidden to 1 shown, eased.
    pub position: f32,
    /// A hide or show is still running: request another frame.
    pub animating: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Transition {
    /// The position it started from (a reversed one starts mid-way).
    from: f32,
    started: Instant,
    duration: Duration,
    /// The frames [`SidebarReveal::pace`] has seen (it pins the first two),
    /// and when the last of them was drawn.
    paced: u8,
    paced_at: Option<Instant>,
}

/// Whether the sidebar is shown (or on its way there), and the hide or show
/// under way, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarReveal {
    shown: bool,
    transition: Option<Transition>,
}

impl SidebarReveal {
    /// At rest, shown or hidden: a window opening collapsed doesn't animate.
    pub fn new(shown: bool) -> Self {
        Self {
            shown,
            transition: None,
        }
    }

    /// Where the sidebar is headed.
    pub fn shown(&self) -> bool {
        self.shown
    }

    /// A hide or show is under way (as of its last [`Self::frame`]).
    #[cfg(test)]
    pub fn is_animating(&self) -> bool {
        self.transition.is_some()
    }

    /// Head for `shown`, starting at `now`. Mid-way, it turns around from
    /// where the sidebar stands and takes the share of [`REVEAL`] left to
    /// cover, so it never jumps; under `reduce_motion` it lands at once.
    /// Returns whether anything changed.
    pub fn set(&mut self, shown: bool, now: Instant, reduce_motion: bool) -> bool {
        if shown == self.shown {
            return false;
        }
        let from = self.position(now);
        self.shown = shown;
        let distance = (target(shown) - from).abs();
        self.transition = (!reduce_motion && distance > 0.0).then(|| Transition {
            from,
            started: now,
            duration: REVEAL.mul_f32(distance),
            // One from rest has its clock pinned to its first frames; one
            // that turns around mid-way is already in motion.
            paced: if from == 0.0 || from == 1.0 { 0 } else { 2 },
            paced_at: None,
        });
        true
    }

    /// A frame is being drawn at `now`: pin the clock of a hide or show from
    /// rest to its first frames, as the column's push does
    /// ([`super::column_swap::ColumnSwap::pace`]). The first frame shows
    /// [`FIRST_STEP`] of it however late it comes after the toggle, and the
    /// second adds at most [`FIRST_STEP`] more, so a slow first layout of the
    /// main area at its new width never skips the curve's fast opening. Call
    /// it before [`Self::frame`].
    pub fn pace(&mut self, now: Instant) {
        let Some(t) = self.transition.as_mut() else {
            return;
        };
        match (t.paced, t.paced_at) {
            (0, _) => t.started = now.checked_sub(FIRST_STEP).unwrap_or(now),
            (1, Some(last)) => {
                let gap = now.saturating_duration_since(last);
                if gap > FIRST_STEP {
                    t.started += gap - FIRST_STEP;
                }
            }
            _ => return,
        }
        t.paced += 1;
        t.paced_at = Some(now);
    }

    /// Where the sidebar stands at `now`, 0 hidden to 1 shown, eased.
    pub fn position(&self, now: Instant) -> f32 {
        let to = target(self.shown);
        match &self.transition {
            None => to,
            Some(t) => t.from + (to - t.from) * ease_out_expo(progress(t, now)),
        }
    }

    /// Where the sidebar stands at `now`: drops a hide or show that has run
    /// its course first.
    pub fn frame(&mut self, now: Instant) -> RevealFrame {
        if self.transition.is_some_and(|t| progress(&t, now) >= 1.0) {
            self.transition = None;
        }
        RevealFrame {
            position: self.position(now),
            animating: self.transition.is_some(),
        }
    }
}

fn target(shown: bool) -> f32 {
    if shown {
        1.0
    } else {
        0.0
    }
}

/// The linear share of `t` elapsed at `now`, 0 to 1.
fn progress(t: &Transition, now: Instant) -> f32 {
    if t.duration.is_zero() {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(t.started);
    (elapsed.as_secs_f32() / t.duration.as_secs_f32()).min(1.0)
}

/// How the leading column draws in one frame, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// The column's width in the layout: what the main area makes room for.
    pub visible: f32,
    /// The width its content is laid out at, the same through the motion.
    pub content: f32,
    /// The content's offset in the column (0 or negative): sliding out to
    /// the left, its right edge stays on the column's.
    pub offset: f32,
    /// How much of the collapsed main area's titlebar row (room for the
    /// traffic lights and the show button) is open, 0 to 1.
    pub titlebar: f32,
}

/// Place the column for a sidebar at `position` (0 hidden, 1 shown) headed
/// for `shown`: `shown_px` is the column's width with the sidebar shown,
/// `hidden_px` its width with the sidebar hidden (`None`: there is no column,
/// no panel holding it). `None` when nothing draws.
///
/// With no column on the hidden side, the content slides out to the left at
/// its full width and the column narrows to nothing. With a panel holding
/// the column either way, only the width moves, between the two (the panel
/// at the width it is headed for).
pub fn place(shown_px: f32, hidden_px: Option<f32>, position: f32, shown: bool) -> Option<Placement> {
    let p = position.clamp(0.0, 1.0);
    match hidden_px {
        None => {
            let visible = shown_px * p;
            (p > 0.0).then_some(Placement {
                visible,
                content: shown_px,
                offset: visible - shown_px,
                titlebar: 1.0 - p,
            })
        }
        Some(hidden_px) => Some(Placement {
            visible: hidden_px + (shown_px - hidden_px) * p,
            content: if shown { shown_px } else { hidden_px },
            offset: 0.0,
            titlebar: 0.0,
        }),
    }
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
    fn it_starts_at_rest_either_way() {
        let now = Instant::now();
        let mut shown = SidebarReveal::new(true);
        assert_eq!(shown.frame(now), RevealFrame { position: 1.0, animating: false });
        let mut hidden = SidebarReveal::new(false);
        assert_eq!(hidden.frame(now), RevealFrame { position: 0.0, animating: false });
        assert!(!hidden.set(false, now, false), "already there");
        assert!(!hidden.is_animating());
    }

    #[test]
    fn hiding_slides_from_shown_to_hidden_and_settles() {
        let mut reveal = SidebarReveal::new(true);
        let t0 = Instant::now();
        assert!(reveal.set(false, t0, false));
        assert!(!reveal.shown());

        let f = reveal.frame(t0);
        assert!(f.animating);
        assert_eq!(f.position, 1.0, "the first instant is where it stood");

        // Ease-out expo: well past half by a quarter of the way.
        let f = reveal.frame(t0 + REVEAL / 4);
        assert!(f.animating);
        assert!(f.position < 0.5 && f.position > 0.0, "{}", f.position);

        let f = reveal.frame(t0 + REVEAL);
        assert_eq!(f, RevealFrame { position: 0.0, animating: false });
        assert!(!reveal.is_animating());
    }

    #[test]
    fn showing_plays_the_reverse() {
        let mut reveal = SidebarReveal::new(false);
        let t0 = Instant::now();
        assert!(reveal.set(true, t0, false));
        let mid = reveal.frame(t0 + REVEAL / 2);
        assert!(mid.animating && mid.position > 0.5 && mid.position < 1.0);
        assert_eq!(reveal.frame(t0 + REVEAL), RevealFrame { position: 1.0, animating: false });
    }

    #[test]
    fn a_second_toggle_mid_way_reverses_from_where_it_stands() {
        let mut reveal = SidebarReveal::new(true);
        let t0 = Instant::now();
        reveal.set(false, t0, false);
        let mid = t0 + ms(30);
        let at = reveal.position(mid);
        assert!(at > 0.0 && at < 1.0);

        // Back before it finished: no jump, it turns around.
        assert!(reveal.set(true, mid, false));
        assert!(close(reveal.position(mid), at), "continues from {at}");
        let next = reveal.frame(mid + ms(8));
        assert!(next.animating && next.position > at, "heading back: {}", next.position);

        // It covers only the distance left: the share 1 - at of a full one.
        let back = REVEAL.mul_f32(1.0 - at);
        assert!(reveal.frame(mid + back - ms(2)).animating);
        assert_eq!(reveal.frame(mid + back), RevealFrame { position: 1.0, animating: false });
    }

    #[test]
    fn toggling_back_and_forth_never_jumps() {
        let mut reveal = SidebarReveal::new(true);
        let mut now = Instant::now();
        let mut last = reveal.position(now);
        for (i, step) in [20u64, 40, 10, 90, 5, 60].into_iter().enumerate() {
            reveal.set(i % 2 == 1, now, false);
            assert!(close(reveal.position(now), last), "toggle {i} jumped");
            now += ms(step);
            last = reveal.frame(now).position;
            assert!((0.0..=1.0).contains(&last));
        }
    }

    #[test]
    fn reduce_motion_lands_at_once() {
        let mut reveal = SidebarReveal::new(true);
        let t0 = Instant::now();
        assert!(reveal.set(false, t0, true));
        assert_eq!(reveal.frame(t0), RevealFrame { position: 0.0, animating: false });
        assert!(reveal.set(true, t0, true));
        assert_eq!(reveal.frame(t0), RevealFrame { position: 1.0, animating: false });

        // Turned on mid-way: the turn lands at once too.
        reveal.set(false, t0, false);
        let mid = t0 + ms(40);
        assert!(reveal.frame(mid).animating);
        reveal.set(true, mid, true);
        assert_eq!(reveal.frame(mid), RevealFrame { position: 1.0, animating: false });
    }

    #[test]
    fn the_clock_starts_at_the_first_frame_one_step_in() {
        let mut reveal = SidebarReveal::new(true);
        let input = Instant::now();
        reveal.set(false, input, false);

        // The first frame comes 80ms after the toggle (the main area laid
        // out at its new width): one frame's step of the motion, not 80ms.
        let first = input + ms(80);
        reveal.pace(first);
        let step = 1.0 - ease_out_expo(FIRST_STEP.as_secs_f32() / REVEAL.as_secs_f32());
        assert!(close(reveal.frame(first).position, step));

        // It took 60ms to draw: the second adds one step, not 60ms.
        let second = first + ms(60);
        reveal.pace(second);
        let two = 1.0 - ease_out_expo(2.0 * FIRST_STEP.as_secs_f32() / REVEAL.as_secs_f32());
        assert!(close(reveal.position(second), two));

        // Then the clock runs: it settles a full REVEAL after its pinned start.
        let end = second + REVEAL;
        reveal.pace(end);
        assert!(!reveal.frame(end).animating);
    }

    #[test]
    fn a_turn_mid_way_is_not_paced() {
        let mut reveal = SidebarReveal::new(true);
        let t0 = Instant::now();
        reveal.set(false, t0, false);
        reveal.pace(t0);
        let mid = t0 + ms(40);
        reveal.pace(mid);
        let at = reveal.position(mid);
        reveal.set(true, mid, false);
        reveal.pace(mid);
        assert!(close(reveal.position(mid), at), "no step added at the turn");
    }

    #[test]
    fn the_sidebar_slides_out_at_its_full_width() {
        // Shown: the column at the sidebar's width, the content in place.
        let p = place(280.0, None, 1.0, true).unwrap();
        assert_eq!(p, Placement { visible: 280.0, content: 280.0, offset: 0.0, titlebar: 0.0 });

        // Part way: the column narrower, the content still 280 wide, slid
        // left so its right edge stays on the column's.
        let p = place(280.0, None, 0.25, false).unwrap();
        assert!(close(p.visible, 70.0));
        assert_eq!(p.content, 280.0, "never laid out narrower");
        assert!(close(p.offset + p.content, p.visible));
        assert!(close(p.titlebar, 0.75));

        // Hidden: no column at all.
        assert_eq!(place(280.0, None, 0.0, false), None);
    }

    #[test]
    fn a_panel_holding_the_column_only_changes_width() {
        // The sidebar shown at 400, a panel in the column at 280 collapsed.
        let p = place(400.0, Some(280.0), 0.5, false).unwrap();
        assert!(close(p.visible, 340.0));
        assert_eq!((p.content, p.offset, p.titlebar), (280.0, 0.0, 0.0));
        let p = place(400.0, Some(280.0), 0.0, false).unwrap();
        assert_eq!(p.visible, 280.0, "collapsed: the panel's width");
        let p = place(400.0, Some(280.0), 0.5, true).unwrap();
        assert_eq!(p.content, 400.0, "headed for the shown width");
    }
}
