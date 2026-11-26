//! Tearing a project tab off its window (`tab-tearoff`), and dragging it
//! between windows.
//!
//! A tab drag starts as [`super::tab_drag`]'s reorder: a tab-shaped ghost
//! and a gap opening in the bar. Dragged more than [`TEAR_OFF_PX`] from every
//! tab bar (measured from the bar's bounds, so running along a bar never
//! does it) the ghost turns into a card ([`CARD_W`] × [`CARD_H`], the tab's
//! name centered), and a drop opens a new window holding just that tab where
//! it was dropped. Brought within [`TEAR_OFF_PX`] of a tab bar, this window's
//! or another's, it is the tab ghost again and that bar opens its drop gap;
//! dropped there it lands in the gap.
//!
//! How it works across windows. gpui keeps a drag per window: another
//! window's `on_drop` never hears it. AppKit, though, keeps sending a press's
//! moves and its release to the window the press began in, wherever the
//! pointer goes. So the source window follows the whole drag: on each move
//! it puts the pointer in screen space ([`crate::platform::screen`]), asks
//! every Shelbi window where its content and tab bar are, picks the bar the
//! tab is over ([`target`]), and tells that window to open its gap
//! ([`super::tab_drag::TabReorder::host`]). The release lands it: in its own
//! bar a reorder, in another window's bar a move there, anywhere else a new
//! window. The handoff is a switch: the window giving the tab up remembers
//! where the project was left (its view state), and the window taking it
//! switches to it, attaching the same daemon sessions, as switching tabs
//! does. Nothing is reopened.
//!
//! A window's ghost can't leave the window, so while the tab isn't over its
//! own bar the ghost is drawn in a small transparent pop-up window that
//! follows the pointer ([`FloatingGhost`]), above every window. That window
//! is borderless ([`crate::platform::screen::NativeWindow::make_overlay`]):
//! no window chrome draws around the ghost, and no display edge or menu bar
//! holds it back from the pointer ([`float_origin`]).
//!
//! Wayland gives an app no global positions: a window can't learn where it
//! or another is on screen, or be placed there. So there
//! ([`crate::platform::screen::global_positions`] false) the drag only knows
//! its own window: no other window's bar takes the tab (the tab menu's
//! "Move to window" does that), no floating ghost opens (the card is drawn
//! in the source window, as far as its edges), and a tab dropped away from
//! its bar opens in a new window where the compositor puts it (Hyprland
//! tiles it). The compositor still sends the source window the press's
//! moves past its edges, so tearing off works the same.
//!
//! The session is the one place the drop gap lives: which bar the tab is
//! over and the gap there ([`DragState::over`], [`DragState::gap`]). Each
//! window's bar follows it ([`BarRole`]), on every move and every frame, so
//! exactly one bar shows a gap (none while torn off), however the tab goes
//! back and forth.
//!
//! The drag's state is the app's, not a window's ([`TabDragSession`]): both
//! ghosts read it, and the bar under the tab is another window's.

use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, size, App, Bounds, Context, Div, Global, Pixels, Point, SharedString, Window,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowId, WindowKind, WindowOptions,
};

use shelbi_app::overlay::palette::ProjectIndicator;

use super::project_menu;
use super::tab_drag::BarRole;
use crate::platform::screen::NativeWindow;
use crate::theme::motion;
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;

/// How far from every tab bar a dragged tab tears off.
pub const TEAR_OFF_PX: f32 = 200.0;
/// The torn-off card: landscape, wider than tall.
pub const CARD_W: f32 = 300.0;
pub const CARD_H: f32 = 200.0;
const _: () = assert!(CARD_W > CARD_H, "the card is landscape");
/// The card's corners (a dialog's: `rounded_lg`).
pub const CARD_RADIUS: f32 = 8.0;
/// How long the ghost takes to turn into the card, or back: a menu's pace,
/// on its curve.
pub const MORPH: Duration = motion::MENU_IN;
/// The tab ghost's height (the bar's).
const TAB_H: f32 = super::tab_bar::TAB_BAR_H;
/// The tab's padding, as the bar draws it.
const TAB_PAD_LEFT: f32 = 16.0;
const TAB_PAD_RIGHT: f32 = 20.0;
const TAB_GAP: f32 = 8.0;
/// The card's padding around its name.
const CARD_PAD: f32 = 16.0;
/// The floating ghost window's room around the ghost, for its shadow.
const FLOAT_PAD: f32 = 40.0;
/// The widest tab ghost.
const TAB_MAX_W: f32 = 240.0;
/// How far below the card's top the pointer holds it: about where it held
/// the tab.
const CARD_GRAB_Y: f32 = TAB_H / 2.0;

/// One window's tab bar, in screen space, as the drag sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BarSpot<Id> {
    pub window: Id,
    /// The window's content.
    pub frame: Bounds<f32>,
    /// Its tab bar.
    pub bar: Bounds<f32>,
}

/// How far screen point `p` is from `b` (0 inside it).
pub fn distance(b: &Bounds<f32>, p: Point<f32>) -> f32 {
    let dx = (b.left() - p.x).max(p.x - b.right()).max(0.0);
    let dy = (b.top() - p.y).max(p.y - b.bottom()).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// The window whose tab bar a tab dragged to screen point `pointer` is over,
/// `bars` front to back: `None` past [`TEAR_OFF_PX`] from them all, where it
/// tears off.
///
/// Over a window, only that window's bar counts (the frontmost window under
/// the pointer: the bars of the windows behind it are hidden). Over no window
/// (the desktop between them), the nearest bar within reach.
pub fn target<Id: Copy>(pointer: Point<f32>, bars: &[BarSpot<Id>]) -> Option<Id> {
    if let Some(over) = bars.iter().find(|b| b.frame.contains(&pointer)) {
        return (distance(&over.bar, pointer) <= TEAR_OFF_PX).then_some(over.window);
    }
    bars.iter()
        .map(|b| (distance(&b.bar, pointer), b.window))
        .filter(|(d, _)| *d <= TEAR_OFF_PX)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, w)| w)
}

/// The ghost's turn between the tab (0) and the card (1): eased over
/// [`MORPH`] from wherever it is when it turns around, so reversing midway
/// doesn't restart it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Morph {
    from: f32,
    to: f32,
    start: Option<Instant>,
}

impl Default for Morph {
    fn default() -> Self {
        Morph { from: 0.0, to: 0.0, start: None }
    }
}

impl Morph {
    /// Where it is at `now`.
    pub fn at(&self, now: Instant) -> f32 {
        match self.start {
            Some(start) => {
                let t = now.saturating_duration_since(start).as_secs_f32() / MORPH.as_secs_f32();
                self.from + (self.to - self.from) * motion::ease_out_expo(t.min(1.0))
            }
            None => self.to,
        }
    }

    /// Head for `to` (0 the tab, 1 the card) from where it is at `now`; at
    /// once under `reduce_motion`. Whether that changed where it is headed.
    pub fn head(&mut self, to: f32, now: Instant, reduce_motion: bool) -> bool {
        if (self.to - to).abs() < f32::EPSILON {
            return false;
        }
        let from = self.at(now);
        *self = if reduce_motion {
            Morph { from: to, to, start: None }
        } else {
            Morph { from, to, start: Some(now) }
        };
        true
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.start.is_some_and(|s| now.saturating_duration_since(s) < MORPH)
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// A tab drag in flight, app-wide.
pub struct DragState {
    /// The window the tab is being dragged out of.
    pub source: WindowId,
    pub project: String,
    pub label: SharedString,
    pub working: bool,
    /// The dragged tab's width and its slot's (the tab and its rule): the
    /// gap another bar opens for it.
    pub tab_w: f32,
    pub slot_w: f32,
    /// Where in the tab the pointer pressed.
    pub grab: Point<f32>,
    /// Where the tab sat in its window, and where that window's first tab
    /// starts: a window torn off from it puts its tab (its first) under the
    /// pointer the same way.
    pub tab_origin: Point<f32>,
    pub first_x: f32,
    /// The tab can leave its window: not a remote window's (its tabs are its
    /// host's projects, which no other window lists).
    pub tearable: bool,
    /// The window whose bar the tab is over (`None`: torn off), and the
    /// pointer's x in that window.
    pub over: Option<WindowId>,
    pub over_x: f32,
    /// The drop gap in that bar (`None`: over none). The one gap any bar
    /// shows ([`Self::role`]), and the one a drop lands in.
    pub gap: Option<usize>,
    /// The tab ↔ card turn.
    pub morph: Morph,
    pub reduce_motion: bool,
    /// The pointer, in screen space.
    pub pointer: Point<f32>,
    /// The floating ghost, once it has opened.
    pub float: Option<WindowHandle<FloatingGhost>>,
    float_native: Option<NativeWindow>,
    /// The floating ghost is opening.
    float_opening: bool,
    /// The platform can place a window at a screen point
    /// ([`crate::platform::screen::global_positions`]): the floating ghost
    /// can follow the pointer. Without (Wayland), the source window draws
    /// the torn-off card itself, within its own edges.
    pub can_float: bool,
}

impl DragState {
    /// The dragged tab's dot: a project's (open, working or not; the New
    /// tab never drags).
    pub fn indicator(&self) -> ProjectIndicator {
        super::tab_bar::indicator(self.working)
    }

    pub fn new(source: WindowId, project: String, label: SharedString, working: bool) -> DragState {
        DragState {
            source,
            project,
            label,
            working,
            tab_w: 0.0,
            slot_w: 0.0,
            grab: Point::default(),
            tab_origin: Point::default(),
            first_x: 0.0,
            tearable: true,
            over: Some(source),
            over_x: 0.0,
            gap: None,
            morph: Morph::default(),
            reduce_motion: false,
            pointer: Point::default(),
            float: None,
            float_native: None,
            float_opening: false,
            can_float: true,
        }
    }

    /// The floating ghost draws the ghost (not the source window's own):
    /// whenever the tab isn't over its own bar.
    pub fn floats(&self) -> bool {
        self.can_float && self.over != Some(self.source)
    }

    /// What the drag asks of `window`'s bar: the source's tab is home over
    /// its own bar or away from it, the bar the tab is over opens the gap,
    /// every other bar shows none.
    pub fn role(&self, window: WindowId) -> BarRole {
        let over = self.over == Some(window);
        if window == self.source {
            BarRole::Own { home: over, gap: self.gap.filter(|_| over) }
        } else if over {
            BarRole::Host { project: self.project.clone(), slot_w: self.slot_w, grab_x: self.grab.x, gap: self.gap }
        } else {
            BarRole::Idle
        }
    }

    /// Where the pointer holds the ghost at `now`: from its top-left, the
    /// tab's grab point turning into the card's (its top center).
    pub fn hold(&self, now: Instant) -> Point<f32> {
        let m = self.morph.at(now);
        point(lerp(self.grab.x, CARD_W / 2.0, m), lerp(self.grab.y, CARD_GRAB_Y, m))
    }
}

/// The app's tab drag, while one is in flight.
#[derive(Default)]
pub struct TabDragSession(pub Option<DragState>);

impl Global for TabDragSession {}

/// The drag in flight, if any.
pub fn session(cx: &App) -> Option<&DragState> {
    cx.try_global::<TabDragSession>().and_then(|s| s.0.as_ref())
}

/// What the drag in flight asks of `window`'s bar ([`DragState::role`]);
/// with none (or gpui's drag over without a drop this window heard), none.
pub fn role(window: WindowId, cx: &App) -> BarRole {
    session(cx)
        .filter(|_| cx.has_active_drag())
        .map_or(BarRole::Idle, |s| s.role(window))
}

/// The drag in flight, to change.
pub fn session_mut(cx: &mut App) -> Option<&mut DragState> {
    if !cx.has_global::<TabDragSession>() {
        return None;
    }
    cx.global_mut::<TabDragSession>().0.as_mut()
}

/// Begin `state`'s drag (any earlier one is over).
pub fn begin(state: DragState, cx: &mut App) {
    if let Some(old) = end(cx) {
        close_float(old, cx);
    }
    cx.set_global(TabDragSession(Some(state)));
}

/// End the drag: its state, for the caller to land (the floating ghost is
/// the caller's to close, [`close_float`]).
pub fn end(cx: &mut App) -> Option<DragState> {
    if !cx.has_global::<TabDragSession>() {
        return None;
    }
    cx.global_mut::<TabDragSession>().0.take()
}

/// Close a finished drag's floating ghost.
pub fn close_float(state: DragState, cx: &mut App) {
    if let Some(float) = state.float {
        cx.defer(move |cx| {
            let _ = float.update(cx, |_, window, _| window.remove_window());
        });
    }
}

/// Where the pointer is in the floating ghost window: the window follows the
/// pointer only, and the ghost moves inside it as it turns (so the turn needs
/// no window moves). Room for the ghost held anywhere along the widest tab,
/// or by the card's top.
fn float_anchor() -> Point<f32> {
    point(TAB_MAX_W + FLOAT_PAD, TAB_H + FLOAT_PAD)
}

/// Where the floating ghost window's top-left goes, in screen space, for the
/// pointer at `pointer`: always the pointer less [`float_anchor`], with no
/// display edge or menu bar clamping it, so the ghost stays under the
/// pointer anywhere on any display.
pub fn float_origin(pointer: Point<f32>) -> Point<f32> {
    pointer - float_anchor()
}

/// The floating ghost window's size: the ghost held at [`float_anchor`]
/// fits, whichever way it is held, with room for its shadow.
fn float_size() -> gpui::Size<Pixels> {
    size(px(2.0 * TAB_MAX_W.max(CARD_W) + 2.0 * FLOAT_PAD), px(TAB_H + CARD_H + 2.0 * FLOAT_PAD))
}

/// Bring the floating ghost in line with the drag: open it the first time
/// the tab leaves its own bar, keep it under the pointer, and repaint it.
/// `display` is the source window's display, where the ghost opens when the
/// platform can't say which display the pointer is on.
pub fn sync_float(display: Option<gpui::DisplayId>, cx: &mut App) {
    let Some(state) = session(cx) else {
        return;
    };
    if !state.floats() && state.float.is_none() {
        return;
    }
    let at = float_origin(state.pointer);
    let (float, native, opening, source, pointer) =
        (state.float, state.float_native.clone(), state.float_opening, state.source, state.pointer);
    if let Some(native) = native {
        native.place_later(at, cx);
    }
    if let Some(float) = float {
        let _ = float.update(cx, |_, _, cx| cx.notify());
        return;
    }
    if opening {
        return;
    }
    if let Some(state) = session_mut(cx) {
        state.float_opening = true;
    }
    // Display-relative, as a window opens: on the pointer's display, else
    // the source's (whose bounds are screen space then).
    let (display_id, origin) = crate::platform::screen::display_at(pointer, cx)
        .map(|(id, origin)| (Some(id), origin))
        .unwrap_or((display, Point::default()));
    let at = at - origin;
    // Opened once this event is done: a window doesn't open inside another's
    // dispatch.
    cx.defer(move |cx| {
        if session(cx).is_none_or(|s| s.source != source) {
            return;
        }
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(at.x), px(at.y)), float_size()))),
                display_id,
                titlebar: None,
                focus: false,
                show: true,
                kind: WindowKind::PopUp,
                is_movable: false,
                is_resizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            |_, cx| cx.new(|_| FloatingGhost),
        );
        let handle = match opened {
            Ok(handle) => handle,
            Err(e) => {
                tracing::warn!(error = %e, "could not open the dragged tab's window");
                return;
            }
        };
        let native = handle
            .update(cx, |_, window, _| NativeWindow::of(window))
            .ok()
            .flatten();
        if let Some(native) = &native {
            native.make_overlay();
        }
        let pointer = match session_mut(cx).filter(|s| s.source == source) {
            Some(state) => {
                state.float = Some(handle);
                state.float_native = native.clone();
                state.float_opening = false;
                state.pointer
            }
            // The drag ended while it opened.
            None => {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
                return;
            }
        };
        // Where the pointer is now: opened titled, AppKit may have kept it
        // below the menu bar.
        if let Some(native) = native {
            native.place_later(float_origin(pointer), cx);
        }
    });
}

/// The ghost as the drag draws it at `morph` (0 the tab, 1 the card): the
/// selected tab's look (the main area's fill, its type) lifted off with a
/// dialog's shadow, growing from the tab's `tab_w` × 52 into the
/// [`CARD_W`] × [`CARD_H`] card with a dialog's corners, its name moving to
/// the middle as the dot fades.
pub fn ghost(label: SharedString, indicator: ProjectIndicator, morph: f32, tab_w: f32, theme: &Theme) -> Div {
    let m = morph.clamp(0.0, 1.0);
    let tab_w = if tab_w > 0.0 { tab_w } else { TAB_MAX_W };
    let card = m > 0.0;
    let dot_room = lerp(1.0, 0.0, m);
    let (pl, pr) = if card {
        (lerp(TAB_PAD_LEFT, CARD_PAD, m), lerp(TAB_PAD_RIGHT, CARD_PAD, m))
    } else {
        (TAB_PAD_LEFT, TAB_PAD_RIGHT)
    };
    div()
        .debug_selector(move || if card { "tab-drag-card".into() } else { "tab-drag-ghost".into() })
        .flex()
        .flex_row()
        .items_center()
        .when(card, |d| d.justify_center())
        .flex_none()
        .w(px(lerp(tab_w, CARD_W, m)))
        .h(px(lerp(TAB_H, CARD_H, m)))
        .pl(px(pl))
        .pr(px(pr))
        .overflow_hidden()
        .rounded(px(CARD_RADIUS * m))
        .bg(theme.canvas)
        .shadow(if card { style::dialog_shadow(theme) } else { style::popover_shadow(theme) })
        .opacity(0.95)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .w(px(lerp(10.0, 0.0, m) + TAB_GAP * dot_room))
                .overflow_hidden()
                .opacity(dot_room)
                .child(project_menu::dot("tab-drag-ghost-dot", indicator, theme)),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .type_style(Type::BaseMedium)
                .text_color(theme.foreground)
                .child(label),
        )
}

/// The dragged tab's ghost in its own pop-up window, while the tab isn't
/// over its own bar: the window is moved to keep the ghost under the
/// pointer, and lets the pointer through.
pub struct FloatingGhost;

impl Render for FloatingGhost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().opaque();
        let now = Instant::now();
        let ghost = session(cx).filter(|s| s.floats()).map(|s| {
            if s.morph.animating(now) {
                window.request_animation_frame();
            }
            let at = float_anchor() - s.hold(now);
            div()
                .absolute()
                .left(px(at.x))
                .top(px(at.y))
                .child(ghost(s.label.clone(), s.indicator(), s.morph.at(now), s.tab_w, &theme))
        });
        div()
            .debug_selector(|| "tab-drag-float".into())
            .relative()
            .size_full()
            .children(ghost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<f32> {
        Bounds::new(point(x, y), size(w, h))
    }

    /// Window 1 at (0,0) 1000×800, window 2 at (1200,0) 800×600; each bar is
    /// its top 52pt.
    fn two() -> Vec<BarSpot<u8>> {
        vec![
            BarSpot { window: 1, frame: b(0.0, 0.0, 1000.0, 800.0), bar: b(0.0, 0.0, 1000.0, 52.0) },
            BarSpot { window: 2, frame: b(1200.0, 0.0, 800.0, 600.0), bar: b(1200.0, 0.0, 800.0, 52.0) },
        ]
    }

    #[test]
    fn distance_is_from_the_bars_bounds() {
        let bar = b(0.0, 0.0, 1000.0, 52.0);
        assert_eq!(distance(&bar, point(500.0, 20.0)), 0.0, "on it");
        assert_eq!(distance(&bar, point(900.0, 252.0)), 200.0, "straight under it");
        assert_eq!(distance(&bar, point(1003.0, 56.0)), 5.0, "off its corner");
    }

    #[test]
    fn along_a_bar_it_stays_a_tab_and_past_200_it_tears_off() {
        let bars = two();
        assert_eq!(target(point(990.0, 30.0), &bars), Some(1), "dragged along the bar");
        assert_eq!(target(point(500.0, 252.0), &bars), Some(1), "200 under it");
        assert_eq!(target(point(500.0, 253.0), &bars), None, "past 200: the card");
        // Back within 200 of it: the tab again.
        assert_eq!(target(point(500.0, 150.0), &bars), Some(1));
    }

    #[test]
    fn over_another_window_its_bar_takes_the_tab() {
        let bars = two();
        assert_eq!(target(point(1500.0, 20.0), &bars), Some(2), "on its bar");
        assert_eq!(target(point(1500.0, 240.0), &bars), Some(2), "within 200 of it");
        assert_eq!(target(point(1500.0, 400.0), &bars), None, "deep in it: the card");
        // On the desktop between them: the nearest bar in reach.
        assert_eq!(target(point(1050.0, 20.0), &bars), Some(1));
        assert_eq!(target(point(1150.0, 20.0), &bars), Some(2));
        assert_eq!(target(point(1100.0, 700.0), &bars), None);
    }

    #[test]
    fn a_window_in_front_hides_the_bars_behind_it() {
        // Window 2 over window 1's lower half: its content is in front.
        let mut bars = two();
        bars.insert(0, BarSpot { window: 3, frame: b(100.0, 100.0, 600.0, 600.0), bar: b(100.0, 100.0, 600.0, 52.0) });
        assert_eq!(target(point(400.0, 120.0), &bars), Some(3));
        // 60 under window 1's bar but inside window 3, 60 over 3's bar's top:
        // window 3's.
        assert_eq!(target(point(400.0, 112.0), &bars), Some(3));
        // Deep in window 3, even within 200 of window 1's bar: torn off.
        assert_eq!(target(point(400.0, 400.0), &bars), None);
    }

    #[test]
    fn the_morph_turns_around_from_where_it_is() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut m = Morph::default();
        assert_eq!(m.at(t0), 0.0);
        assert!(!m.head(0.0, t0, false), "already the tab");
        assert!(m.head(1.0, t0, false));
        let mid = m.at(ms(40));
        assert!(mid > 0.0 && mid < 1.0, "{mid}");
        assert!(m.animating(ms(40)));
        assert!(m.head(0.0, ms(40), false));
        assert_eq!(m.at(ms(40)), mid, "no snap");
        assert!(m.at(ms(100)) < mid);
        assert_eq!(m.at(ms(40) + MORPH), 0.0);
        assert!(!m.animating(ms(40) + MORPH));
        // Reduce Motion: there at once.
        m.head(1.0, t0, true);
        assert_eq!(m.at(t0), 1.0);
        assert!(!m.animating(t0));
    }

    /// Which bars show a gap for the drag in `s`, of windows 1..=3.
    fn gaps(s: &DragState) -> Vec<u64> {
        (1..=3u64)
            .filter(|w| match s.role(WindowId::from(*w)) {
                BarRole::Own { home, .. } => home,
                BarRole::Host { .. } => true,
                BarRole::Idle => false,
            })
            .collect()
    }

    /// Move the drag in `s` to `pointer` over `bars`, as the source does.
    fn move_drag(s: &mut DragState, pointer: Point<f32>, bars: &[BarSpot<WindowId>]) {
        s.over = target(pointer, bars);
        s.gap = s.over.map(|_| 0);
    }

    fn ids(bars: Vec<BarSpot<u8>>) -> Vec<BarSpot<WindowId>> {
        bars.into_iter()
            .map(|b| BarSpot { window: WindowId::from(u64::from(b.window)), frame: b.frame, bar: b.bar })
            .collect()
    }

    #[test]
    fn exactly_one_bar_shows_the_gap_as_the_tab_goes_back_and_forth() {
        let bars = ids(two());
        let mut s = DragState::new(WindowId::from(1u64), "web".into(), "web".into(), false);
        let (on_a, on_b, off) = (point(500.0, 20.0), point(1500.0, 20.0), point(500.0, 600.0));
        move_drag(&mut s, on_a, &bars);
        assert_eq!(gaps(&s), [1], "its own bar");
        for _ in 0..3 {
            move_drag(&mut s, on_b, &bars);
            assert_eq!(gaps(&s), [2], "A's closes as B's opens");
            assert_eq!(s.role(WindowId::from(1u64)), BarRole::Own { home: false, gap: None });
            move_drag(&mut s, on_a, &bars);
            assert_eq!(gaps(&s), [1], "and back");
            move_drag(&mut s, off, &bars);
            assert!(gaps(&s).is_empty(), "torn off: none");
            move_drag(&mut s, on_b, &bars);
            assert_eq!(gaps(&s), [2]);
            move_drag(&mut s, off, &bars);
            assert!(gaps(&s).is_empty());
        }
        // In one move from B's bar straight to A's.
        move_drag(&mut s, on_b, &bars);
        move_drag(&mut s, on_a, &bars);
        assert_eq!(gaps(&s), [1]);
    }

    #[test]
    fn over_overlapping_windows_the_front_bar_shows_the_gap() {
        // Window 3 in front of window 1's lower part.
        let mut raw = two();
        raw.insert(0, BarSpot { window: 3, frame: b(100.0, 100.0, 600.0, 600.0), bar: b(100.0, 100.0, 600.0, 52.0) });
        let bars = ids(raw);
        let mut s = DragState::new(WindowId::from(1u64), "web".into(), "web".into(), false);
        move_drag(&mut s, point(400.0, 120.0), &bars);
        assert_eq!(gaps(&s), [3]);
        // Up onto window 1's bar, above window 3.
        move_drag(&mut s, point(400.0, 30.0), &bars);
        assert_eq!(gaps(&s), [1]);
        move_drag(&mut s, point(400.0, 120.0), &bars);
        assert_eq!(gaps(&s), [3]);
        move_drag(&mut s, point(1500.0, 20.0), &bars);
        assert_eq!(gaps(&s), [2]);
    }

    #[test]
    fn a_hosting_bar_gets_the_tab_and_the_sessions_gap() {
        let mut s = DragState::new(WindowId::from(1u64), "web".into(), "web".into(), false);
        s.slot_w = 120.0;
        s.grab = point(30.0, 20.0);
        s.over = Some(WindowId::from(2u64));
        s.gap = Some(2);
        assert_eq!(
            s.role(WindowId::from(2u64)),
            BarRole::Host { project: "web".into(), slot_w: 120.0, grab_x: 30.0, gap: Some(2) }
        );
        assert_eq!(s.role(WindowId::from(3u64)), BarRole::Idle);
    }

    #[test]
    fn the_floating_ghost_follows_the_pointer_unclamped() {
        let anchor = float_anchor();
        for p in [
            point(500.0, 400.0),
            // On the menu bar, at the very top, and above the primary
            // display (a display arranged over it).
            point(500.0, 10.0),
            point(0.0, 0.0),
            point(300.0, -400.0),
            // A display left of the primary, and far right.
            point(-1200.0, 300.0),
            point(5000.0, 1400.0),
        ] {
            let at = float_origin(p);
            assert_eq!(at + anchor, p, "the pointer is always at the anchor: {p:?}");
        }
        assert!(float_origin(point(500.0, 10.0)).y < 0.0, "past the screen's top, not held below the menu bar");
    }

    #[test]
    fn the_card_is_held_at_its_top_center() {
        let mut s = DragState::new(gpui::WindowId::from(1u64), "web".into(), "web".into(), false);
        s.grab = point(30.0, 20.0);
        let t0 = Instant::now();
        assert_eq!(s.hold(t0), point(30.0, 20.0), "the tab where it was pressed");
        s.morph.head(1.0, t0, true);
        assert_eq!(s.hold(t0), point(CARD_W / 2.0, CARD_GRAB_Y));
    }

    #[test]
    fn the_landscape_card_fits_the_floating_ghost_window_with_room_for_its_shadow() {
        let window = float_size();
        let (w, h) = (window.width.as_f32(), window.height.as_f32());
        let mut s = DragState::new(gpui::WindowId::from(1u64), "web".into(), "web".into(), false);
        s.morph.head(1.0, Instant::now(), true);
        let at = float_anchor() - s.hold(Instant::now());
        assert!(at.x >= FLOAT_PAD && at.y >= FLOAT_PAD, "the card's top-left {at:?}");
        assert!(at.x + CARD_W + FLOAT_PAD <= w, "its right edge in {w}");
        assert!(at.y + CARD_H + FLOAT_PAD <= h, "its bottom edge in {h}");
    }
}
