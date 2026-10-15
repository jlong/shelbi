//! The window's leading column and the push between its occupants
//! (`dt-review-replaces-sidebar`, `dt-workspace-sidebar`).
//!
//! The column shows the navigation sidebar, or, while a review or a workspace
//! is open, that one's panel in its place, as the TUI's review and workspace
//! panels replace its sidebar. Going in and coming back out are a push
//! between two levels of a navigation stack: the sidebar slides out to the
//! left and fades while the panel slides in from the right and fades in, and
//! the reverse on the way back. With Reduce Motion on, the two crossfade in
//! place instead. The two panels are siblings on the upper level, so going
//! from one to the other goes back through the sidebar: the first panel pops,
//! then the second is pushed.
//!
//! Toolkit-free and clock-injected, so the state machine (entering, leaving,
//! a second toggle mid-transition reversing from where it stands, a panel
//! swapped for the other) is tested without a frame loop. The window asks
//! [`ColumnSwap::frame`] what to draw each frame and keeps requesting frames
//! while it says it is animating.
//!
//! A swap from rest has its clock start at the first frame drawn for it, not
//! at the input (`dt-instant-panel-push`): [`ColumnSwap::pace`] pins it
//! there, one frame in, so that frame already moves, and caps the step to the
//! second frame, so a slow first frame (the panel built, its first layout)
//! never skips the fast opening of the ease-out curve. A swap turned around
//! mid-way runs on the clock from the moment it turned.

use std::time::{Duration, Instant};

/// What the column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupant {
    Sidebar,
    Review,
    Workspace,
}

impl Occupant {
    /// The occupant's level on the transition's axis: 0 the sidebar, 1 a
    /// panel pushed over it.
    fn position(self) -> f32 {
        match self {
            Occupant::Sidebar => 0.0,
            Occupant::Review | Occupant::Workspace => 1.0,
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

/// The step a swap's first frame shows, and the most its second may add: one
/// frame at 60Hz. At 120Hz the second frame comes sooner and is never capped.
pub const FIRST_STEP: Duration = Duration::from_micros(16_667);

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

/// What one frame of the column draws: the sidebar and at most one panel,
/// both only while a swap runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub sidebar: Option<Layer>,
    pub review: Option<Layer>,
    pub workspace: Option<Layer>,
    /// A swap is still running: request another frame.
    pub animating: bool,
}

impl Frame {
    fn with_panel(sidebar: Option<Layer>, panel: Occupant, layer: Option<Layer>, animating: bool) -> Self {
        Frame {
            sidebar,
            review: layer.filter(|_| panel == Occupant::Review),
            workspace: layer.filter(|_| panel == Occupant::Workspace),
            animating,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Transition {
    /// The position it started from (a reversed swap starts mid-way).
    from: f32,
    started: Instant,
    duration: Duration,
    motion: Motion,
    /// The frames [`ColumnSwap::pace`] has seen (it pins the first two), and
    /// when the last of them was drawn.
    paced: u8,
    paced_at: Option<Instant>,
}

/// The column's occupant, the swap under way, if any, and the panel to push
/// once the column is back at the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSwap {
    /// Where the current swap is heading: the sidebar, or [`Self::panel`].
    target: Occupant,
    /// The panel on the upper level (drawn while it is in or moving).
    panel: Occupant,
    /// The other panel, pushed once the column is back at the sidebar.
    queued: Option<(Occupant, Motion)>,
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
            panel: Occupant::Review,
            queued: None,
            transition: None,
        }
    }

    /// The occupant the column shows, or is on its way to (past the sidebar
    /// when one panel is making way for the other).
    pub fn target(&self) -> Occupant {
        self.queued.map_or(self.target, |(panel, _)| panel)
    }

    /// A swap is under way (as of its last [`Self::settle`]).
    #[cfg(test)]
    pub fn is_animating(&self) -> bool {
        self.transition.is_some()
    }

    /// Swap to `to`, starting at `now`. A no-op when that is already the
    /// target. Mid-swap, it reverses from where the column stands, taking the
    /// share of the full duration that is left to cover, so the motion never
    /// jumps. A panel asked for while the other one is in (or on its way)
    /// first sends the column back to the sidebar, then pushes. Returns
    /// whether anything changed.
    pub fn show(&mut self, to: Occupant, motion: Motion, now: Instant) -> bool {
        if to == self.target() {
            return false;
        }
        self.queued = None;
        let from = self.position(now);
        if to != Occupant::Sidebar && to != self.panel && from > 0.0 {
            // The other panel is (partly) in: pop it first.
            self.queued = Some((to, motion));
            if self.target != Occupant::Sidebar {
                self.start(Occupant::Sidebar, from, motion, now);
            }
            return true;
        }
        if to != Occupant::Sidebar {
            self.panel = to;
        }
        self.start(to, from, motion, now);
        true
    }

    fn start(&mut self, to: Occupant, from: f32, motion: Motion, now: Instant) {
        self.target = to;
        let distance = (to.position() - from).abs();
        self.transition = Some(Transition {
            from,
            started: now,
            duration: motion.duration().mul_f32(distance),
            motion,
            // A swap from rest has its clock pinned to its first frames; one
            // that turns around mid-way is already in motion, frames flowing.
            paced: if from == 0.0 || from == 1.0 { 0 } else { 2 },
            paced_at: None,
        });
    }

    /// A frame is being drawn at `now`: pin the clock of a swap from rest to
    /// its first frames. The first frame of such a swap shows [`FIRST_STEP`] of it however
    /// long ago the swap was asked for (the input, a slow frame before it),
    /// and the second adds at most [`FIRST_STEP`] more however long the first
    /// took to draw. From then on the swap runs on the clock. Call it before
    /// [`Self::frame`].
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

    /// Jump to the end of any swap under way (and of a queued push).
    #[cfg(test)]
    pub fn finish(&mut self) {
        self.transition = None;
        if let Some((panel, _)) = self.queued.take() {
            self.panel = panel;
            self.target = panel;
        }
    }

    /// Drop a swap that has run its course by `now`; one that brought the
    /// column back to the sidebar for the other panel starts that push, from
    /// the moment the pop ended.
    pub fn settle(&mut self, now: Instant) {
        let Some(t) = self.transition else {
            return;
        };
        if progress(&t, now) < 1.0 {
            return;
        }
        self.transition = None;
        if let Some((panel, motion)) = self.queued.take() {
            self.panel = panel;
            self.start(panel, 0.0, motion, t.started + t.duration);
            // It follows on from the pop, frames already running: its clock
            // needs no pinning.
            if let Some(next) = self.transition.as_mut() {
                next.paced = 2;
            }
            self.settle(now);
        }
    }

    /// Where the column stands at `now` on the 0 (sidebar) to 1 (panel)
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
                Occupant::Sidebar => Frame::with_panel(Some(Layer::REST), self.panel, None, false),
                panel => Frame::with_panel(None, panel, Some(Layer::REST), false),
            };
        };
        let p = self.position(now);
        let slide = match t.motion {
            Motion::Slide => 1.0,
            Motion::Fade => 0.0,
        };
        Frame::with_panel(
            // The sidebar is the lower level: it leaves to the left.
            Some(Layer {
                offset: -p * slide,
                opacity: 1.0 - p,
            }),
            self.panel,
            // A panel is pushed on: it comes in from the right.
            Some(Layer {
                offset: (1.0 - p) * slide,
                opacity: p,
            }),
            true,
        )
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
    pub workspace: Option<Layer>,
}

/// The leading column for `frame`, or `None` when there is none (the sidebar
/// collapsed, no panel in its place). The column is the user's sidebar width
/// (`user_cols`). A collapsed sidebar is never drawn, but a panel still takes
/// the column, at the default sidebar width until the user drags it
/// (`dragged_cols`), and the column goes again once the panel has left.
pub fn column(frame: &Frame, collapsed: bool, user_cols: u16, dragged_cols: Option<u16>) -> Option<Column> {
    let sidebar = frame.sidebar.filter(|_| !collapsed);
    if sidebar.is_none() && frame.review.is_none() && frame.workspace.is_none() {
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
        workspace: frame.workspace,
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
    fn the_workspace_panel_pushes_in_and_pops_out_like_the_review() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        assert!(swap.show(Occupant::Workspace, Motion::Slide, t0));
        assert_eq!(swap.target(), Occupant::Workspace);
        let f = swap.frame(t0);
        assert_eq!(f.sidebar, Some(Layer { offset: 0.0, opacity: 1.0 }));
        assert_eq!(f.workspace, Some(Layer { offset: 1.0, opacity: 0.0 }));
        assert_eq!(f.review, None, "only the panel going in draws");

        let f = swap.frame(t0 + SLIDE / 2);
        let (s, w) = (f.sidebar.unwrap(), f.workspace.unwrap());
        assert!(s.offset < 0.0 && w.offset > 0.0);
        assert!(close(w.offset - s.offset, 1.0));

        let f = swap.frame(t0 + SLIDE);
        assert_eq!((f.sidebar, f.review, f.workspace), (None, None, Some(Layer::REST)));
        assert!(!f.animating);

        // Back: the reverse.
        let t1 = t0 + SLIDE;
        assert!(swap.show(Occupant::Sidebar, Motion::Slide, t1));
        let f = swap.frame(t1 + ms(100));
        assert!(f.workspace.unwrap().offset > 0.0, "the panel slides out to the right");
        assert!(f.sidebar.unwrap().offset < 0.0, "the sidebar comes back from the left");
        let f = swap.frame(t1 + SLIDE);
        assert_eq!((f.sidebar, f.review, f.workspace), (Some(Layer::REST), None, None));
    }

    #[test]
    fn backing_out_of_a_workspace_mid_push_reverses_from_where_it_stands() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Workspace, Motion::Slide, t0);
        let mid = t0 + ms(80);
        let at = swap.position(mid);
        assert!(swap.show(Occupant::Sidebar, Motion::Slide, mid));
        assert!(close(swap.position(mid), at), "continues from {at}");
        let f = swap.frame(mid + ms(1));
        assert!(f.workspace.is_some() && f.review.is_none(), "the workspace panel leaves");
        let back = SLIDE.mul_f32(at);
        let f = swap.frame(mid + back);
        assert!(!f.animating);
        assert_eq!((f.sidebar, f.workspace), (Some(Layer::REST), None));

        // And in again mid-way back: it turns around again.
        swap.show(Occupant::Workspace, Motion::Slide, mid + back);
        let turn = mid + back + ms(50);
        let at = swap.position(turn);
        swap.show(Occupant::Sidebar, Motion::Slide, turn);
        assert!(swap.show(Occupant::Workspace, Motion::Slide, turn));
        assert!(close(swap.position(turn), at));
    }

    #[test]
    fn going_from_a_workspace_to_a_review_passes_through_the_sidebar() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Workspace, Motion::Slide, t0);
        swap.finish();

        // The review is asked for: the workspace panel pops first.
        assert!(swap.show(Occupant::Review, Motion::Slide, t0));
        assert_eq!(swap.target(), Occupant::Review, "where it is headed");
        assert!(!swap.show(Occupant::Review, Motion::Slide, t0), "already headed there");
        let f = swap.frame(t0 + SLIDE / 2);
        assert!(f.workspace.is_some() && f.review.is_none(), "the workspace leaves first");
        assert!(f.sidebar.is_some());

        // Back at the sidebar, the review is pushed from the moment the pop
        // ended, with no gap.
        let f = swap.frame(t0 + SLIDE);
        assert!(f.animating);
        assert_eq!(f.workspace, None);
        assert_eq!(f.review, Some(Layer { offset: 1.0, opacity: 0.0 }));
        let f = swap.frame(t0 + SLIDE + SLIDE / 2);
        assert!(f.review.unwrap().offset < 1.0 && f.review.unwrap().offset > 0.0);
        let f = swap.frame(t0 + SLIDE * 2);
        assert_eq!((f.sidebar, f.review, f.workspace), (None, Some(Layer::REST), None));

        // And back the other way, through the sidebar too.
        let t1 = t0 + SLIDE * 2;
        swap.show(Occupant::Workspace, Motion::Slide, t1);
        let f = swap.frame(t1 + SLIDE / 2);
        assert!(f.review.is_some() && f.workspace.is_none());
        let f = swap.frame(t1 + SLIDE * 2);
        assert_eq!((f.sidebar, f.review, f.workspace), (None, None, Some(Layer::REST)));
    }

    #[test]
    fn a_frame_that_skips_the_whole_hop_lands_on_the_second_panel() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Review, Motion::Fade, t0);
        swap.finish();
        swap.show(Occupant::Workspace, Motion::Fade, t0);
        let f = swap.frame(t0 + FADE * 3);
        assert_eq!((f.sidebar, f.review, f.workspace), (None, None, Some(Layer::REST)));
        assert!(!f.animating);
    }

    #[test]
    fn changing_course_while_a_hop_is_queued() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Workspace, Motion::Slide, t0);
        swap.finish();
        swap.show(Occupant::Review, Motion::Slide, t0);
        let mid = t0 + ms(60);
        let at = swap.position(mid);

        // Back to the workspace before the pop finished: it turns around and
        // the review is never pushed.
        assert!(swap.show(Occupant::Workspace, Motion::Slide, mid));
        assert!(close(swap.position(mid), at));
        let f = swap.frame(mid + SLIDE);
        assert_eq!((f.sidebar, f.review, f.workspace), (None, None, Some(Layer::REST)));

        // Asking for the review, then the sidebar: the pop runs, nothing follows.
        swap.show(Occupant::Review, Motion::Slide, mid + SLIDE);
        assert!(swap.show(Occupant::Sidebar, Motion::Slide, mid + SLIDE + ms(30)));
        assert_eq!(swap.target(), Occupant::Sidebar);
        let f = swap.frame(mid + SLIDE * 3);
        assert_eq!((f.sidebar, f.review, f.workspace), (Some(Layer::REST), None, None));
    }

    #[test]
    fn a_collapsed_sidebar_lends_its_column_to_a_workspace_panel() {
        let t0 = Instant::now();
        let mut swap = ColumnSwap::new();
        swap.show(Occupant::Workspace, Motion::Slide, t0);
        let c = column(&swap.frame(t0 + SLIDE), true, 40, None).unwrap();
        assert_eq!(c.width_cols, shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH);
        assert_eq!((c.sidebar, c.review, c.workspace), (None, None, Some(Layer::REST)));
    }

    #[test]
    fn the_clock_starts_at_the_first_frame_one_step_in() {
        let mut swap = ColumnSwap::new();
        let input = Instant::now();
        swap.show(Occupant::Review, Motion::Slide, input);

        // The first frame comes 90ms after the input (the panel was built in
        // it): it shows one frame's step of the push, not 90ms of it.
        let first = input + ms(90);
        swap.pace(first);
        let f = swap.frame(first);
        assert!(f.animating);
        let step = ease_out(FIRST_STEP.as_secs_f32() / SLIDE.as_secs_f32());
        assert!(close(f.review.unwrap().opacity, step), "one step in: {:?}", f.review);
        assert!(f.sidebar.unwrap().offset < 0.0, "the first frame already moves");

        // The first frame took 70ms to draw: the second adds one step, not 70ms.
        let second = first + ms(70);
        swap.pace(second);
        let at = swap.position(second);
        let two = ease_out(2.0 * FIRST_STEP.as_secs_f32() / SLIDE.as_secs_f32());
        assert!(close(at, two), "capped to two steps: {at} vs {two}");

        // From then on it runs on the clock: a slow third frame is not capped.
        let third = second + ms(50);
        swap.pace(third);
        let expect = ease_out((2.0 * FIRST_STEP.as_secs_f32() + 0.050) / SLIDE.as_secs_f32());
        assert!(close(swap.position(third), expect));
        let end = third + SLIDE;
        swap.pace(end);
        assert!(!swap.frame(end).animating);
    }

    #[test]
    fn frames_on_time_are_never_held_back() {
        // 60Hz frames: pacing changes nothing past the first step.
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Workspace, Motion::Slide, t0);
        swap.pace(t0);
        let mut now = t0;
        let mut frames = 1;
        while swap.frame(now).animating {
            now += FIRST_STEP;
            swap.pace(now);
            frames += 1;
            assert!(frames < 40, "it ends");
        }
        // 220ms at 16.7ms a frame, the first frame already one step in.
        assert_eq!(frames, 14);
    }

    #[test]
    fn a_swap_turned_around_mid_way_is_not_paced() {
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Review, Motion::Slide, t0);
        swap.pace(t0);
        swap.frame(t0);
        let mid = t0 + ms(60);
        swap.pace(mid);
        let at = swap.position(mid);
        swap.show(Occupant::Sidebar, Motion::Slide, mid);
        swap.pace(mid);
        assert!(close(swap.position(mid), at), "no step added at the turn");
    }

    #[test]
    fn a_hop_through_the_sidebar_keeps_its_clock() {
        // The second panel's push follows on from the pop: pacing it would
        // hold the column at the sidebar for a step.
        let mut swap = ColumnSwap::new();
        let t0 = Instant::now();
        swap.show(Occupant::Workspace, Motion::Slide, t0);
        swap.finish();
        swap.show(Occupant::Review, Motion::Slide, t0);
        swap.pace(t0);
        swap.frame(t0);
        swap.pace(t0 + ms(16));
        swap.frame(t0 + ms(16));
        let pop_end = t0 + SLIDE - FIRST_STEP;
        let later = pop_end + ms(50);
        swap.pace(later);
        swap.frame(later);
        swap.pace(later + ms(16));
        let f = swap.frame(later + ms(16));
        let expect = ease_out(0.066 / SLIDE.as_secs_f32());
        assert!(close(f.review.unwrap().opacity, expect), "{:?} vs {expect}", f.review);
    }

    #[test]
    fn ease_out_starts_fast_and_lands_on_the_ends() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.25) > 0.25);
        assert_eq!(ease_out(2.0), 1.0, "clamped");
    }
}
