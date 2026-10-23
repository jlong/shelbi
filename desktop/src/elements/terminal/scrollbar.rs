//! The scrollbar: where it sits, where the thumb sits for a scrollback
//! position, which position a dragged thumb or a track click means, and
//! when it shows.
//!
//! - **Geometry** is plain arithmetic over logical pixels (`f32`), so it is
//!   unit-tested without a window. The scrollbar is drawn against the right
//!   edge of the pane's *area* ([`Rail`]): the grid plus the padding its host
//!   puts around it, so the thumb overlays that padding, [`INSET`] from the
//!   edge, and the text grid neither moves nor reflows. The thumb's height
//!   is the visible share of the buffer (rows over history plus rows), at
//!   least [`MIN_THUMB`]; at the live bottom (offset 0) it sits at the
//!   bottom of the rail, at the oldest line at the top.
//! - **Visibility** ([`Visibility`]) follows macOS's overlay scrollers: hidden
//!   at rest, faded in while the terminal scrolls, for [`LINGER`] after, and
//!   while the pointer is over the edge or a drag holds the thumb; then faded
//!   out. With "Show scroll bars: Always" (or `desktop.terminal_scrollbar:
//!   always`) it shows whenever there is scrollback ([`overlay`] decides).
//!   Reduce Motion makes the fades instant.

use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{App, Global};
use shelbi_state::TerminalScrollbar;

use crate::platform::scrollers;
use crate::theme::motion;

/// The scrollbar's width.
pub const WIDTH: f32 = 6.0;

/// Gap between the thumb and the pane area's right edge, and between the
/// thumb's travel and the area's top and bottom.
pub const INSET: f32 = 2.0;

/// The width from the area's right edge where the pointer reveals the
/// scrollbar and a press grabs it instead of reaching the terminal (wider
/// than the thumb, so it's easy to hit). Within the hosts' padding (12 to 24
/// points), so it never covers text.
pub const HIT_WIDTH: f32 = 12.0;

/// The shortest thumb, so a long history still leaves something to grab.
pub const MIN_THUMB: f32 = 24.0;

/// How long an overlay scrollbar stays after the last scroll before it
/// fades out.
pub const LINGER: Duration = Duration::from_secs(1);

/// The fades, on the app's ease ([`motion::ease_out_expo`]): in at a menu's
/// pace, out at the palette's, so it leaves more gently than it comes.
pub const FADE_IN: Duration = motion::MENU_IN;
pub const FADE_OUT: Duration = motion::DIALOG_IN;

/// Where the scrollbar runs, in window coordinates: down the right edge of
/// the pane's area (the grid's bounds grown by its host's padding).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rail {
    /// The area's right edge.
    pub right: f32,
    /// The area's top and bottom.
    pub top: f32,
    pub bottom: f32,
}

/// A rectangle in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// The host's padding around the grid, which the scrollbar overlays.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Padding {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rail {
    /// The rail for a grid spanning `top..bottom` with its right edge at
    /// `right`, inside `padding`.
    pub fn around(right: f32, top: f32, bottom: f32, padding: Padding) -> Rail {
        Rail {
            right: right + padding.right.max(0.0),
            top: top - padding.top.max(0.0),
            bottom: bottom + padding.bottom.max(0.0),
        }
    }

    /// Where the pointer reveals and grabs the scrollbar: [`HIT_WIDTH`]
    /// along the edge, the area's full height.
    pub fn hit(&self) -> Rect {
        Rect {
            x: self.right - HIT_WIDTH,
            y: self.top,
            w: HIT_WIDTH,
            h: self.bottom - self.top,
        }
    }

    /// How far the thumb travels: the area's height less [`INSET`] at
    /// each end.
    pub fn length(&self) -> f32 {
        (self.bottom - self.top - 2.0 * INSET).max(0.0)
    }

    /// A window `y` as a distance along the thumb's travel.
    pub fn along(&self, y: f32) -> f32 {
        y - self.top - INSET
    }

    /// Where `thumb` is drawn: [`INSET`] in from the edge.
    pub fn thumb_rect(&self, thumb: Thumb) -> Rect {
        Rect {
            x: self.right - INSET - WIDTH,
            y: self.top + INSET + thumb.top,
            w: WIDTH,
            h: thumb.height,
        }
    }
}

/// Whether the scrollbar is an overlay (hidden at rest) for `setting`, with
/// the system's "Show scroll bars" resolving to overlay scrollers
/// (`system_overlay`) or not.
pub fn overlay(setting: TerminalScrollbar, system_overlay: bool) -> bool {
    match setting {
        TerminalScrollbar::Auto => system_overlay,
        TerminalScrollbar::Scrolling => true,
        TerminalScrollbar::Always => false,
    }
}

/// The system's "Show scroll bars" as last read ([`scrollers`]): whether it
/// asks for overlay scroll bars. Read at launch, when a window comes to the
/// front, and whenever AppKit says it changed; every pane observes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemScrollbars {
    pub overlay: bool,
}

/// Until it is read (and in tests): overlays, macOS's default on a Mac
/// with a trackpad.
impl Default for SystemScrollbars {
    fn default() -> Self {
        Self { overlay: true }
    }
}

impl Global for SystemScrollbars {}

impl SystemScrollbars {
    /// Whether the system asks for overlay scroll bars.
    pub fn overlay(cx: &App) -> bool {
        cx.try_global::<Self>().copied().unwrap_or_default().overlay
    }

    /// Install `overlay` if it changed (panes repaint).
    pub fn set(overlay: bool, cx: &mut App) {
        if cx.try_global::<Self>().map(|s| s.overlay) != Some(overlay) {
            cx.set_global(Self { overlay });
        }
    }

    /// Read the system's setting now. Tests keep the default, so they
    /// don't depend on the machine's.
    pub fn sync(cx: &mut App) {
        if !cfg!(test) {
            Self::set(scrollers::overlay(), cx);
        }
    }

    /// Read the setting, and read it again whenever AppKit says it changed.
    pub fn follow(cx: &mut App) {
        Self::sync(cx);
        let (changed, mut changes) = futures::channel::mpsc::unbounded();
        scrollers::observe(changed);
        cx.spawn(async move |cx| {
            while changes.next().await.is_some() {
                cx.update(Self::sync);
            }
        })
        .detach();
    }
}

/// What decides whether the scrollbar shows this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Inputs {
    /// Not an overlay: shown whenever there is scrollback.
    pub always: bool,
    /// The pointer is over the edge ([`Rail::hit`]).
    pub hovered: bool,
    /// A drag holds the thumb.
    pub dragging: bool,
    /// Reduce Motion: show and hide at once.
    pub reduce_motion: bool,
}

/// The scrollbar as a frame draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    /// 0 (hidden) to 1.
    pub opacity: f32,
    /// A fade is under way (draw another frame).
    pub animating: bool,
}

/// When the scrollbar shows: the last scroll, and the fade toward shown or
/// hidden. Time is passed in, so tests drive it without a clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct Visibility {
    scrolled_at: Option<Instant>,
    fade: Fade,
}

/// A fade from one opacity to another, or (no start) resting at `to`.
#[derive(Debug, Clone, Copy, Default)]
struct Fade {
    from: f32,
    to: f32,
    start: Option<Instant>,
    duration: Duration,
}

impl Fade {
    fn at(&self, now: Instant) -> f32 {
        let Some(start) = self.start else {
            return self.to;
        };
        let t = now.saturating_duration_since(start).as_secs_f32() / self.duration.as_secs_f32().max(f32::EPSILON);
        if t >= 1.0 {
            self.to
        } else {
            self.from + (self.to - self.from) * motion::ease_out_expo(t)
        }
    }

    fn done(&self, now: Instant) -> bool {
        self.start
            .is_none_or(|start| now.saturating_duration_since(start) >= self.duration)
    }
}

impl Visibility {
    /// The terminal scrolled (the wheel, the keys, the thumb, output while
    /// scrolled back): show the scrollbar, for [`LINGER`] from now.
    pub fn scrolled(&mut self, now: Instant) {
        self.scrolled_at = Some(now);
    }

    /// Whether the scrollbar should be showing at `now`.
    pub fn wanted(&self, now: Instant, inputs: Inputs) -> bool {
        inputs.always
            || inputs.hovered
            || inputs.dragging
            || self
                .scrolled_at
                .is_some_and(|at| now.saturating_duration_since(at) < LINGER)
    }

    /// How long until a scroll's linger runs out on its own (when nothing
    /// else holds the scrollbar), for a timer to draw the fade out.
    pub fn hide_in(&self, now: Instant, inputs: Inputs) -> Option<Duration> {
        if inputs.always || inputs.hovered || inputs.dragging {
            return None;
        }
        let left = LINGER.checked_sub(now.saturating_duration_since(self.scrolled_at?))?;
        (!left.is_zero()).then_some(left)
    }

    /// The scrollbar at `now`: starts a fade when it should now show or
    /// hide (from wherever a fade under way has got to; at once under
    /// Reduce Motion or when it is always shown).
    pub fn frame(&mut self, now: Instant, inputs: Inputs) -> Shown {
        let target = if self.wanted(now, inputs) { 1.0 } else { 0.0 };
        if inputs.reduce_motion || inputs.always {
            // No fades: a change lands at once, and so does a fade already
            // under way (Reduce Motion switched on mid-fade).
            self.fade = Fade { from: target, to: target, start: None, duration: Duration::ZERO };
        } else if target != self.fade.to {
            let from = self.fade.at(now);
            self.fade = Fade {
                from,
                to: target,
                start: Some(now),
                duration: if target > from { FADE_IN } else { FADE_OUT },
            };
        }
        let animating = !self.fade.done(now);
        if !animating {
            self.fade.start = None;
        }
        Shown {
            opacity: self.fade.at(now),
            animating,
        }
    }
}

/// Where the thumb is, in pixels from the top of the track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thumb {
    /// The thumb's top edge.
    pub top: f32,
    /// The thumb's height.
    pub height: f32,
}

/// The thumb for a buffer with `history` lines above `rows` on-screen rows,
/// scrolled `offset` lines up from the live bottom, on a track `track`
/// pixels tall. `None` when there is no history (nothing to scroll).
pub fn thumb(history: usize, offset: usize, rows: usize, track: f32) -> Option<Thumb> {
    if history == 0 || track <= 0.0 {
        return None;
    }
    let total = (history + rows.max(1)) as f32;
    let height = (track * rows.max(1) as f32 / total).clamp(MIN_THUMB.min(track), track);
    let travel = track - height;
    let from_top = (history - offset.min(history)) as f32 / history as f32;
    Some(Thumb {
        top: travel * from_top,
        height,
    })
}

/// The scrollback offset that puts the thumb's top at `top` (a drag). Clamped
/// to the track.
pub fn offset_for_thumb_top(top: f32, history: usize, rows: usize, track: f32) -> usize {
    let Some(t) = thumb(history, 0, rows, track) else {
        return 0;
    };
    let travel = track - t.height;
    if travel <= 0.0 {
        return 0;
    }
    let from_top = (top / travel).clamp(0.0, 1.0);
    (history as f32 * (1.0 - from_top)).round() as usize
}

/// The offset a press at `y` on the track (outside the thumb) jumps to: the
/// thumb centered on the press, as macOS does.
pub fn offset_for_track_click(y: f32, history: usize, rows: usize, track: f32) -> usize {
    let Some(t) = thumb(history, 0, rows, track) else {
        return 0;
    };
    offset_for_thumb_top(y - t.height / 2.0, history, rows, track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_history_no_scrollbar() {
        assert_eq!(thumb(0, 0, 24, 400.0), None);
    }

    #[test]
    fn the_thumb_is_the_visible_share_and_moves_with_the_offset() {
        // 100 history + 100 rows on a 400px track: a 200px thumb.
        let bottom = thumb(100, 0, 100, 400.0).unwrap();
        assert_eq!(bottom, Thumb { top: 200.0, height: 200.0 });
        let top = thumb(100, 100, 100, 400.0).unwrap();
        assert_eq!(top, Thumb { top: 0.0, height: 200.0 });
        let middle = thumb(100, 50, 100, 400.0).unwrap();
        assert_eq!(middle.top, 100.0);
    }

    #[test]
    fn a_long_history_keeps_a_grabbable_thumb() {
        let t = thumb(10_000, 0, 24, 400.0).unwrap();
        assert_eq!(t.height, MIN_THUMB);
        assert_eq!(t.top, 400.0 - MIN_THUMB);
        // A tiny track: the thumb is the track.
        assert_eq!(thumb(10_000, 0, 24, 10.0).unwrap().height, 10.0);
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_an_offset() {
        assert_eq!(offset_for_thumb_top(0.0, 100, 100, 400.0), 100);
        assert_eq!(offset_for_thumb_top(200.0, 100, 100, 400.0), 0);
        assert_eq!(offset_for_thumb_top(100.0, 100, 100, 400.0), 50);
        // Past either end clamps.
        assert_eq!(offset_for_thumb_top(-50.0, 100, 100, 400.0), 100);
        assert_eq!(offset_for_thumb_top(900.0, 100, 100, 400.0), 0);
        // Round trip.
        for offset in [0, 1, 37, 99, 100] {
            let t = thumb(100, offset, 100, 400.0).unwrap();
            assert_eq!(offset_for_thumb_top(t.top, 100, 100, 400.0), offset);
        }
    }

    #[test]
    fn a_track_click_centers_the_thumb_on_the_press() {
        // Thumb 200px: a press at y=100 puts its top at 0, the oldest line.
        assert_eq!(offset_for_track_click(100.0, 100, 100, 400.0), 100);
        assert_eq!(offset_for_track_click(300.0, 100, 100, 400.0), 0);
    }

    #[test]
    fn the_rail_runs_down_the_areas_right_edge_not_the_grids() {
        // The Workspace pane: a grid inset 16 across and 12 down.
        let padding = Padding { top: 12.0, right: 16.0, bottom: 12.0 };
        let rail = Rail::around(1000.0, 100.0, 700.0, padding);
        assert_eq!(rail, Rail { right: 1016.0, top: 88.0, bottom: 712.0 });
        // The thumb hugs the area's edge, 2pt in, not the grid's.
        let t = rail.thumb_rect(Thumb { top: 0.0, height: 50.0 });
        assert_eq!((t.x + t.w, t.w), (1016.0 - INSET, WIDTH));
        assert_eq!(t.y, 88.0 + INSET);
        // The travel is the area's full height less the inset at each end.
        assert_eq!(rail.length(), 624.0 - 2.0 * INSET);
        let bottom = thumb(100, 0, 24, rail.length()).unwrap();
        let b = rail.thumb_rect(bottom);
        assert_eq!(b.y + b.h, 712.0 - INSET);
        // The hit strip hugs the edge, the area's full height, and stays in
        // the padding: it never covers a text column.
        let hit = rail.hit();
        assert_eq!(hit, Rect { x: 1016.0 - HIT_WIDTH, y: 88.0, w: HIT_WIDTH, h: 624.0 });
        assert!(hit.x >= 1000.0, "the hit strip starts right of the grid");
        assert!(hit.contains(1015.0, 400.0) && !hit.contains(999.0, 400.0));
        // The Review pane's 12pt inset still clears the text.
        let review = Rail::around(1000.0, 0.0, 600.0, Padding { right: 12.0, ..padding });
        assert!(review.hit().x >= 1000.0);
        // A host with no padding: the grid's own edge, as before.
        assert_eq!(Rail::around(500.0, 0.0, 300.0, Padding::default()).right, 500.0);
        // Along the rail: window y to travel distance and back.
        assert_eq!(rail.along(88.0 + INSET + 10.0), 10.0);
    }

    #[test]
    fn the_setting_picks_overlay_or_always() {
        use TerminalScrollbar::*;
        assert!(overlay(Auto, true));
        assert!(!overlay(Auto, false), "Automatic with a mouse, or the system's Always");
        assert!(overlay(Scrolling, false) && overlay(Scrolling, true));
        assert!(!overlay(Always, true) && !overlay(Always, false));
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn an_overlay_is_hidden_at_rest_and_shows_while_scrolling() {
        let t0 = Instant::now();
        let overlay = Inputs::default();
        let mut v = Visibility::default();
        assert_eq!(v.frame(t0, overlay), Shown { opacity: 0.0, animating: false });

        // A scroll fades it in.
        v.scrolled(t0);
        let start = v.frame(t0, overlay);
        assert!(start.animating && start.opacity == 0.0);
        let mid = v.frame(at(t0, 50), overlay);
        assert!(mid.animating && mid.opacity > 0.0 && mid.opacity < 1.0, "{mid:?}");
        assert_eq!(v.frame(at(t0, 250), overlay), Shown { opacity: 1.0, animating: false });
        assert_eq!(v.hide_in(at(t0, 250), overlay), Some(Duration::from_millis(750)));

        // Scrolling again keeps it: the linger restarts.
        v.scrolled(at(t0, 900));
        assert_eq!(v.frame(at(t0, 1500), overlay).opacity, 1.0);
        assert!(v.wanted(at(t0, 1899), overlay));

        // About a second after the last scroll it fades out.
        assert!(v.frame(at(t0, 1950), overlay).animating);
        let out = v.frame(at(t0, 2000), overlay);
        assert!(out.animating && out.opacity < 1.0 && out.opacity > 0.0, "{out:?}");
        assert_eq!(v.hide_in(at(t0, 1950), overlay), None);
        assert_eq!(v.frame(at(t0, 1900 + 500), overlay), Shown { opacity: 0.0, animating: false });
    }

    #[test]
    fn hover_and_drag_hold_an_overlay() {
        let t0 = Instant::now();
        let mut v = Visibility::default();
        let hovered = Inputs { hovered: true, ..Inputs::default() };
        v.frame(t0, hovered);
        assert_eq!(v.frame(at(t0, 5000), hovered).opacity, 1.0, "stays while hovered");
        assert_eq!(v.hide_in(at(t0, 5000), hovered), None);
        let dragging = Inputs { dragging: true, ..Inputs::default() };
        assert_eq!(v.frame(at(t0, 6000), dragging).opacity, 1.0, "and while dragging");
        // The drag ends after a scroll (dragging scrolls): it lingers, then goes.
        v.scrolled(at(t0, 6000));
        assert_eq!(v.frame(at(t0, 6100), Inputs::default()).opacity, 1.0);
        assert!(v.frame(at(t0, 7100), Inputs::default()).animating);
        assert_eq!(v.frame(at(t0, 8000), Inputs::default()).opacity, 0.0);
    }

    #[test]
    fn a_fade_out_reversed_midway_fades_back_in_from_where_it_was() {
        let t0 = Instant::now();
        let mut v = Visibility::default();
        let hovered = Inputs { hovered: true, ..Inputs::default() };
        v.frame(t0, hovered);
        v.frame(at(t0, 300), hovered);
        v.frame(at(t0, 300), Inputs::default());
        let mid = v.frame(at(t0, 320), Inputs::default()).opacity;
        let back = v.frame(at(t0, 320), hovered);
        assert!(back.animating);
        assert!((back.opacity - mid).abs() < 1e-6, "no jump: {mid} -> {}", back.opacity);
    }

    #[test]
    fn always_shows_at_once_and_reduce_motion_makes_fades_instant() {
        let t0 = Instant::now();
        let always = Inputs { always: true, ..Inputs::default() };
        let mut v = Visibility::default();
        assert_eq!(v.frame(t0, always), Shown { opacity: 1.0, animating: false });
        assert_eq!(v.frame(at(t0, 60_000), always).opacity, 1.0, "with no scrolling");
        assert_eq!(v.hide_in(t0, always), None);

        let reduced = Inputs { reduce_motion: true, ..Inputs::default() };
        let mut v = Visibility::default();
        v.scrolled(t0);
        assert_eq!(v.frame(t0, reduced), Shown { opacity: 1.0, animating: false });
        assert_eq!(v.frame(at(t0, 1000), reduced), Shown { opacity: 0.0, animating: false });

        // Switched on mid-fade, the fade lands at once.
        let mut v = Visibility::default();
        v.scrolled(t0);
        assert!(v.frame(at(t0, 50), Inputs::default()).animating);
        assert_eq!(v.frame(at(t0, 60), reduced), Shown { opacity: 1.0, animating: false });
    }
}
