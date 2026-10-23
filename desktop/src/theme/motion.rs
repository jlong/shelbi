//! Taskpage's motion, in gpui: its easing curve, the spring its nav
//! selection slides on, the sliding selection itself, and the color fades
//! its controls hover with.
//!
//! - Menus, popovers and the palette open with Taskpage's slide-and-fade:
//!   8px of travel and opacity on `cubic-bezier(0.16, 1, 0.3, 1)`
//!   ([`ease_out_expo`]), 200ms for a menu ([`MENU_IN`]), 400ms for the
//!   palette ([`DIALOG_IN`]).
//! - The selected row's chip slides to a newly selected row on a spring with
//!   a slight overshoot ([`NAV_SPRING`], framer-motion's `bounce: 0.25`),
//!   tracked by a [`SlideTrack`].
//! - Clickable things fade their fill on hover and press ([`fill_layer`])
//!   instead of switching it, over about 200ms (Taskpage's
//!   `transition-colors`).
//!
//! Reduce Motion (System Settings ▸ Accessibility ▸ Display) makes every one
//! of them instant: [`sync_reduce_motion`] hands the system setting to gpui,
//! whose animations then render their end state, and a [`SlideTrack`] is told
//! when it retargets.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, prelude::*, px, AnimationExt, App, Bounds, Div, Hsla, Pixels, Point, Rgba,
    SharedString, SpringAnimation, SpringConfig, SpringState, Stateful,
};

/// A menu's slide-and-fade in (the switcher, a dropdown): Taskpage's
/// `slide-up-and-fade` at the menu's pace.
pub const MENU_IN: Duration = Duration::from_millis(200);
/// The palette's slide-and-fade in (Taskpage's 400ms).
pub const DIALOG_IN: Duration = Duration::from_millis(400);
/// How far a menu or dialog travels as it fades in.
pub const SLIDE_PX: f32 = 8.0;

/// The nav selection's spring: a damping ratio of 0.75 (framer-motion's
/// `bounce: 0.25`) at 22 rad/s, so the chip reaches its row in about 200ms
/// with a 3% overshoot and settles within half a pixel by about 350ms.
pub const NAV_SPRING: SpringConfig = SpringConfig::new(484.0, 33.0, 1.0);
/// A hover or press fade: critically damped (no overshoot), settled in about
/// 200ms.
pub const FADE_SPRING: SpringConfig = SpringConfig::new(900.0, 60.0, 1.0);
/// A slide settles once it is within half a pixel of its row.
const SETTLE_PX: f32 = 0.5;

/// Taskpage's `cubic-bezier(0.16, 1, 0.3, 1)`: fast out, a long soft
/// landing.
pub fn ease_out_expo(t: f32) -> f32 {
    cubic_bezier(0.16, 1.0, 0.3, 1.0, t)
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` at time `t` (0..=1): solve the curve's
/// x for `t` (Newton, then bisection), and return its y there.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let d_bez = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = t;
    for _ in 0..8 {
        let err = bez(x1, x2, s) - t;
        if err.abs() < 1e-5 {
            return bez(y1, y2, s);
        }
        let d = d_bez(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    s = t;
    for _ in 0..32 {
        let x = bez(x1, x2, s);
        if (x - t).abs() < 1e-5 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    bez(y1, y2, s)
}

/// Hand the system's Reduce Motion setting to gpui, so its own animations
/// (the hover fades, the palette's pulse) render their end state. Cheap
/// enough to call on every window activation.
pub fn sync_reduce_motion(cx: &mut App) {
    cx.set_reduce_motion(crate::platform::motion::reduce_motion());
}

/// The rows a selection chip can slide between, measured as they paint, and
/// the chip's motion between them.
///
/// The chip is drawn inline under its row while it rests (so it scrolls and
/// clips with the row, exactly), and as a floating layer under the rows
/// while it slides: [`SlideTrack::select`] starts a slide when the selection
/// moves to another measured row, and [`SlideTrack::frame`] gives the
/// floating chip's bounds each frame until it settles. Rows report their
/// bounds through [`probe`], the layer its origin through [`origin_probe`].
#[derive(Debug, Default)]
pub struct SlideTrack {
    /// Each row's bounds in window coordinates, from the last frame it
    /// painted in.
    measured: HashMap<SharedString, Bounds<Pixels>>,
    /// The floating layer's origin, in window coordinates.
    origin: Option<Point<Pixels>>,
    target: Option<SharedString>,
    motion: Option<Slide>,
}

/// A slide under way: the spring's state (x, y, width, height) when it last
/// retargeted.
#[derive(Debug, Clone, Copy)]
struct Slide {
    start: Instant,
    from: [SpringState; 4],
}

pub type SharedTrack = Rc<RefCell<SlideTrack>>;

impl SlideTrack {
    pub fn shared() -> SharedTrack {
        Rc::new(RefCell::new(Self::default()))
    }

    /// Point the chip at `key` (`None`: no row is selected). When it moves
    /// from one measured row to another, the chip slides from where it is
    /// (mid-slide included, keeping its velocity); otherwise, and under
    /// `reduce_motion` (asked only on a change), it jumps.
    pub fn select(&mut self, key: Option<SharedString>, now: Instant, reduce_motion: impl FnOnce() -> bool) {
        if key == self.target {
            return;
        }
        let from = self
            .motion
            .and_then(|m| self.target.as_ref().and_then(|t| self.measured.get(t)).map(|to| state_at(m, rect(*to), now)))
            .or_else(|| {
                let old = self.target.as_ref().and_then(|t| self.measured.get(t))?;
                Some(rect(*old).map(|position| SpringState {
                    position,
                    velocity: 0.0,
                }))
            });
        let lands = key.as_ref().is_some_and(|k| self.measured.contains_key(k));
        self.motion = match from {
            Some(from) if lands && !reduce_motion() => Some(Slide { start: now, from }),
            _ => None,
        };
        self.target = key;
    }

    /// The floating chip's bounds now, relative to the layer, while a slide
    /// runs; `None` once it has settled (the row draws its own chip again).
    pub fn frame(&mut self, now: Instant) -> Option<Bounds<Pixels>> {
        let motion = self.motion?;
        let to = self.target.as_ref().and_then(|t| self.measured.get(t)).copied();
        let (Some(to), Some(origin)) = (to, self.origin) else {
            self.motion = None;
            return None;
        };
        let target = rect(to);
        let state = state_at(motion, target, now);
        let settled = state
            .iter()
            .zip(target)
            .all(|(s, t)| NAV_SPRING.is_settled(*s, t, SETTLE_PX));
        if settled {
            self.motion = None;
            return None;
        }
        let [x, y, w, h] = state.map(|s| s.position);
        Some(Bounds::new(
            gpui::point(px(x) - origin.x, px(y) - origin.y),
            gpui::size(px(w.max(0.0)), px(h.max(0.0))),
        ))
    }

    #[cfg(test)]
    pub fn is_sliding(&self) -> bool {
        self.motion.is_some()
    }

    fn measure(&mut self, key: SharedString, bounds: Bounds<Pixels>) {
        self.measured.insert(key, bounds);
    }
}

fn rect(b: Bounds<Pixels>) -> [f32; 4] {
    [
        b.origin.x.as_f32(),
        b.origin.y.as_f32(),
        b.size.width.as_f32(),
        b.size.height.as_f32(),
    ]
}

fn state_at(m: Slide, target: [f32; 4], now: Instant) -> [SpringState; 4] {
    let dt = now.saturating_duration_since(m.start).as_secs_f32();
    let mut out = m.from;
    for (s, t) in out.iter_mut().zip(target) {
        *s = NAV_SPRING.step(*s, t, dt);
    }
    out
}

/// An invisible element filling its row that records the row's bounds into
/// `track` under `key` each frame. The row needs `.relative()`.
pub fn probe(track: &SharedTrack, key: impl Into<SharedString>) -> impl IntoElement {
    let (track, key) = (track.clone(), key.into());
    canvas(
        move |bounds, _, _| track.borrow_mut().measure(key, bounds),
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// An invisible element at the floating layer's top-left that records its
/// origin. The layer needs `.relative()`.
pub fn origin_probe(track: &SharedTrack) -> impl IntoElement {
    let track = track.clone();
    canvas(
        move |bounds, _, _| track.borrow_mut().origin = Some(bounds.origin),
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_0()
}

/// The floating chip at `bounds` (from [`SlideTrack::frame`]), painted by
/// `paint`: absolutely positioned in the layer.
pub fn floating_chip(bounds: Bounds<Pixels>, paint: impl FnOnce(Div) -> Div) -> Div {
    paint(
        div()
            .debug_selector(|| "sliding-chip".into())
            .absolute()
            .left(bounds.origin.x)
            .top(bounds.origin.y)
            .w(bounds.size.width)
            .h(bounds.size.height),
    )
}

// ---- Hover and press fades -------------------------------------------------

/// What the pointer is doing to each fading element, by key. The UI thread
/// owns it; listeners write it and refresh the window.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Pointer {
    hovered: bool,
    pressed: bool,
}

thread_local! {
    static POINTERS: RefCell<HashMap<SharedString, Pointer>> = RefCell::new(HashMap::new());
}

/// Forget every control's pointer state, so a fresh offscreen window starts
/// with nothing hovered or pressed (the state is keyed by id, not window).
#[cfg(feature = "visual-check")]
pub fn reset_pointers() {
    POINTERS.with(|p| p.borrow_mut().clear());
}

/// Hold `key` hovered, and pressed too if `pressed`, as if under the
/// pointer: the components sheet's buttons in their hover and press fills
/// (a fill layer mounted after this starts at that fill).
#[cfg(feature = "visual-check")]
pub fn hold_pointer(key: impl Into<SharedString>, pressed: bool) {
    set_pointer(&key.into(), |p| {
        p.hovered = true;
        p.pressed = pressed;
    });
}

fn pointer(key: &SharedString) -> Pointer {
    POINTERS.with(|p| p.borrow().get(key).copied().unwrap_or_default())
}

fn set_pointer(key: &SharedString, f: impl FnOnce(&mut Pointer)) -> bool {
    POINTERS.with(|p| {
        let mut map = p.borrow_mut();
        let entry = map.entry(key.clone()).or_default();
        let before = *entry;
        f(entry);
        *entry != before
    })
}

/// A fill that fades between `rest`, `hover` and `pressed` as the pointer
/// enters, leaves and presses `el`: a layer under `el`'s content (added as
/// its first child) with `el`'s own corner `radius`. `el` must not set a
/// background of its own (the layer is it), and gets `.relative()`.
pub fn fill_layer(
    el: Stateful<Div>,
    key: impl Into<SharedString>,
    radius: Pixels,
    [rest, hover, pressed]: [Hsla; 3],
) -> Stateful<Div> {
    let key: SharedString = key.into();
    let state = pointer(&key);
    let target = if state.pressed {
        2.0
    } else if state.hovered {
        1.0
    } else {
        0.0
    };
    let (k1, k2, k3) = (key.clone(), key.clone(), key.clone());
    let layer = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .rounded(radius)
        .with_spring(
            SharedString::from(format!("{key}-fill")),
            SpringAnimation::new(FADE_SPRING).to(target).with_epsilon(0.01),
            move |d, v: f32| {
                let color = if v <= 1.0 {
                    mix(rest, hover, v.clamp(0.0, 1.0))
                } else {
                    mix(hover, pressed, (v - 1.0).clamp(0.0, 1.0))
                };
                d.bg(color)
            },
        );
    // The layer goes in first so the content paints over it.
    el.relative()
        .on_hover(move |hovered, window, _| {
            let h = *hovered;
            if set_pointer(&k1, |p| {
                p.hovered = h;
                if !h {
                    p.pressed = false;
                }
            }) {
                window.refresh();
            }
        })
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, _| {
            if set_pointer(&k2, |p| p.pressed = true) {
                window.refresh();
            }
        })
        .on_mouse_up(gpui::MouseButton::Left, move |_, window, _| {
            if set_pointer(&k3, |p| p.pressed = false) {
                window.refresh();
            }
        })
        .child(layer)
}

/// `a` toward `b` by `t`, channel by channel (alpha included), so a fade from
/// transparent doesn't pass through black.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b): (Rgba, Rgba) = (a.into(), b.into());
    let l = |x: f32, y: f32| x + (y - x) * t;
    Rgba {
        r: l(a.r, b.r),
        g: l(a.g, b.g),
        b: l(a.b, b.b),
        a: l(a.a, b.a),
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taskpages_curve_starts_fast_and_lands_softly() {
        assert_eq!(ease_out_expo(0.0), 0.0);
        assert!((ease_out_expo(1.0) - 1.0).abs() < 1e-4);
        // Most of the travel in the first quarter.
        assert!(ease_out_expo(0.25) > 0.75, "{}", ease_out_expo(0.25));
        let mut last = 0.0;
        for i in 1..=20 {
            let v = ease_out_expo(i as f32 / 20.0);
            assert!(v >= last, "monotonic");
            last = v;
        }
    }

    #[test]
    fn the_nav_spring_overshoots_slightly_and_settles_in_time() {
        let mut s = SpringState {
            position: 0.0,
            velocity: 0.0,
        };
        let mut peak = 0.0f32;
        let mut settled_at = None;
        for frame in 1..=60 {
            s = NAV_SPRING.step(s, 100.0, 1.0 / 120.0);
            peak = peak.max(s.position);
            if settled_at.is_none() && NAV_SPRING.is_settled(s, 100.0, SETTLE_PX) {
                settled_at = Some(frame);
            }
        }
        assert!(peak > 100.5 && peak < 106.0, "a slight overshoot: {peak}");
        let ms = settled_at.expect("settles") as f32 * 1000.0 / 120.0;
        assert!((200.0..=420.0).contains(&ms), "settles in {ms}ms");
    }

    fn b(y: f32) -> Bounds<Pixels> {
        Bounds::new(gpui::point(px(8.0), px(y)), gpui::size(px(200.0), px(36.0)))
    }

    fn track() -> SlideTrack {
        let mut t = SlideTrack {
            origin: Some(gpui::point(px(0.0), px(0.0))),
            ..Default::default()
        };
        t.measure("a".into(), b(0.0));
        t.measure("b".into(), b(100.0));
        t.measure("c".into(), b(200.0));
        t
    }

    #[test]
    fn a_slide_runs_from_the_old_row_to_the_new_and_settles() {
        let mut t = track();
        let t0 = Instant::now();
        t.select(Some("a".into()), t0, || false);
        assert!(!t.is_sliding(), "the first selection doesn't slide");
        t.select(Some("b".into()), t0, || false);
        let first = t.frame(t0).expect("sliding");
        assert_eq!(first.origin.y, px(0.0), "starts at the old row");
        let mid = t.frame(t0 + Duration::from_millis(120)).expect("still sliding");
        assert!(mid.origin.y > px(30.0) && mid.origin.y < px(110.0), "{:?}", mid.origin.y);
        assert!(t.frame(t0 + Duration::from_millis(800)).is_none(), "settled");
        assert!(!t.is_sliding());
    }

    #[test]
    fn a_retarget_mid_slide_starts_from_where_the_chip_is() {
        let mut t = track();
        let t0 = Instant::now();
        t.select(Some("a".into()), t0, || false);
        t.select(Some("b".into()), t0, || false);
        let at = t0 + Duration::from_millis(80);
        let y = t.frame(at).unwrap().origin.y;
        t.select(Some("c".into()), at, || false);
        let restart = t.frame(at).unwrap().origin.y;
        assert!((restart - y).abs() < px(0.01), "no jump: {y:?} -> {restart:?}");
    }

    #[test]
    fn reduce_motion_and_unmeasured_rows_jump() {
        let mut t = track();
        let t0 = Instant::now();
        t.select(Some("a".into()), t0, || false);
        t.select(Some("b".into()), t0, || true);
        assert!(t.frame(t0).is_none(), "Reduce Motion: no slide");
        t.select(Some("never-drawn".into()), t0, || false);
        assert!(t.frame(t0).is_none(), "no row to land on");
        t.select(None, t0, || false);
        assert!(!t.is_sliding());
    }

    #[test]
    fn mix_fades_alpha_too() {
        let clear = gpui::hsla(0.0, 0.0, 1.0, 0.0);
        let white = gpui::hsla(0.0, 0.0, 1.0, 1.0);
        let half: Rgba = mix(clear, white, 0.5).into();
        assert!((half.a - 0.5).abs() < 0.01);
        assert!(half.r > 0.99, "no darkening on the way");
    }
}
