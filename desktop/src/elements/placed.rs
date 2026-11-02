//! A menu placed against the control that opened it, inside the window.
//!
//! Every dropdown, select and ⋮ menu goes through [`placed`]: it opens
//! below its control, flips above when it doesn't fit there and there's
//! more room above, caps its height to the side it took (8pt clear of the
//! window's edge) so it scrolls instead of running off, and shifts sideways
//! to stay inside the window. It measures each frame, after the control has
//! painted, so a window resize or a scroll under the open menu re-places it.
//!
//! A placed menu must be able to shrink in a flex column: it is a scroll
//! container itself (`overflow_y_scroll`, its rows `flex_none`), or it holds
//! one (the font picker's list under its search box). [`MenuScroll`] keeps
//! the keyboard's row in view inside it.
//!
//! An open menu closes on a press anywhere outside it ([`Placed::on_dismiss`]):
//! it lays a window-wide layer under itself that takes that press, so the
//! press does nothing else (it doesn't click the button under it, start a
//! drag, or move focus). A press on the menu's own control lands on that
//! layer too, so a second click on a dropdown's button closes it rather than
//! closing and reopening it. Scrolling still reaches what's under the layer.

use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;

use gpui::{
    div, point, prelude::*, px, size, AnyElement, App, AvailableSpace, Bounds, DispatchPhase, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, LayoutId, MouseDownEvent, Pixels, Point,
    ScrollHandle, Size, Style, Window,
};

use crate::theme::style::Anchor;

/// How far a menu keeps from the window's edges.
pub const MARGIN: f32 = 8.0;

/// Which side of its control a menu opened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
}

/// Which of its control's edges a menu lines up with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Their left edges (a select, a picker).
    Left,
    /// Their right edges, the menu hanging left (a ⋮ button at a row's end).
    Right,
}

/// Where a menu lands: the side it took and its bounds in the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub side: Side,
    pub bounds: Bounds<Pixels>,
}

/// Place a menu `gap` from `trigger` in a window `viewport` big.
/// `measure(h)` lays the menu out at most `h` tall and returns its size.
///
/// Below by default; above when the menu is cut short below (it came back
/// as tall as the room there) and there is more room above. Either way it
/// is capped to its side's room, [`MARGIN`] clear of the window's edge.
/// Sideways it lines up with the trigger by `align`, then shifts to stay
/// [`MARGIN`] inside the window (its left edge wins when it's too wide).
pub fn place(
    trigger: Bounds<Pixels>,
    viewport: Size<Pixels>,
    gap: Pixels,
    align: Align,
    mut measure: impl FnMut(Pixels) -> Size<Pixels>,
) -> Placement {
    let margin = px(MARGIN);
    let room_below = (viewport.height - margin - trigger.bottom() - gap).max(px(0.0));
    let room_above = (trigger.top() - gap - margin).max(px(0.0));
    let mut side = Side::Below;
    let mut menu = measure(room_below);
    if menu.height >= room_below - px(0.5) && room_above > room_below {
        side = Side::Above;
        menu = measure(room_above);
    }
    let y = match side {
        Side::Below => trigger.bottom() + gap,
        Side::Above => trigger.top() - gap - menu.height,
    };
    let x = match align {
        Align::Left => trigger.left(),
        Align::Right => trigger.right() - menu.width,
    };
    let x = x.min(viewport.width - margin - menu.width).max(margin);
    Placement {
        side,
        bounds: Bounds::new(point(x.round(), y.round()), menu),
    }
}

/// `menu` placed against the control `anchor` measures (see the module
/// docs). Put it in a `deferred` (or over everything) so it draws last.
pub fn placed(anchor: &Anchor, menu: impl IntoElement) -> Placed {
    Placed {
        anchor: anchor.clone(),
        fallback: None,
        gap: px(4.0),
        align: Align::Left,
        match_width: false,
        dismiss: None,
        menu: Some(menu.into_any_element()),
    }
}

/// What a press outside an open menu runs (it closes the menu).
pub type Dismiss = Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

pub struct Placed {
    anchor: Anchor,
    fallback: Option<Point<Pixels>>,
    gap: Pixels,
    align: Align,
    match_width: bool,
    dismiss: Option<Dismiss>,
    menu: Option<AnyElement>,
}

/// What [`Placed`] laid out at prepaint: the menu where it goes, its
/// bounds, and the window-wide layer under it while it dismisses.
pub struct PlacedMenu {
    menu: AnyElement,
    bounds: Bounds<Pixels>,
    layer: Option<(Hitbox, Dismiss)>,
}

impl Placed {
    /// The space between the control and the menu (4pt unless set).
    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = px(gap);
        self
    }

    /// Line the menu's right edge up with the control's.
    pub fn align_right(mut self) -> Self {
        self.align = Align::Right;
        self
    }

    /// Make the menu as wide as its control.
    pub fn match_width(mut self) -> Self {
        self.match_width = true;
        self
    }

    /// Close the menu on a press anywhere outside it: `dismiss` runs and the
    /// press goes no further (nothing under it acts on it). A press on the
    /// menu's own control counts as outside, so it toggles the menu closed.
    /// Every open menu sets this; a menu fading out after it closed doesn't,
    /// so it doesn't hold the window's clicks while it fades.
    pub fn on_dismiss(mut self, dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static) -> Self {
        self.dismiss = Some(Rc::new(dismiss));
        self
    }

    /// Where the control is taken to be (window coordinates) while it hasn't
    /// painted; otherwise where this element lands.
    pub fn fallback(mut self, at: Point<Pixels>) -> Self {
        self.fallback = Some(at);
        self
    }
}

impl IntoElement for Placed {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Placed {
    type RequestLayoutState = ();
    /// The menu, laid out and prepainted where it goes.
    type PrepaintState = Option<PlacedMenu>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Takes no room where it sits: the menu is laid out on its own once
        // the control has painted.
        let style = Style {
            position: gpui::Position::Absolute,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let menu = self.menu.take()?;
        // The control prepaints before anything deferred, so its probe has
        // already recorded where it is this frame.
        let trigger = self
            .anchor
            .get()
            .unwrap_or_else(|| Bounds::new(self.fallback.unwrap_or(bounds.origin), Size::default()));
        // The root is capped at the height it's given (`max_h_full` against
        // a definite available height), and the menu shrinks into it.
        let mut root = div()
            .flex()
            .flex_col()
            .max_h_full()
            .when(self.match_width, |d| d.w(trigger.size.width))
            .child(menu)
            .into_any_element();
        let viewport = window.viewport_size();
        let placement = place(trigger, viewport, self.gap, self.align, |h| {
            root.layout_as_root(size(AvailableSpace::MinContent, AvailableSpace::Definite(h)), window, cx)
        });
        // The dismiss layer goes in first, so the menu's own hitboxes sit
        // over it. Drawn deferred, its prepaint has no clip: it covers the
        // whole window.
        let layer = self.dismiss.clone().map(|dismiss| {
            let hitbox = window.insert_hitbox(
                Bounds::new(Point::default(), viewport),
                HitboxBehavior::BlockMouseExceptScroll,
            );
            (hitbox, dismiss)
        });
        root.prepaint_at(placement.bounds.origin, window, cx);
        Some(PlacedMenu {
            menu: root,
            bounds: placement.bounds,
            layer,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        menu: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(placed) = menu else {
            return;
        };
        if let Some((hitbox, dismiss)) = placed.layer.clone() {
            let menu = placed.bounds;
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                // Bubbling, the menu's own rows have had the press; anything
                // registered before this (all that's under the menu) hasn't.
                if phase != DispatchPhase::Bubble
                    || menu.contains(&event.position)
                    || !hitbox.is_hovered_at(event.position, window)
                {
                    return;
                }
                cx.stop_propagation();
                window.prevent_default();
                dismiss(event, window, cx);
            });
        }
        placed.menu.paint(window, cx);
    }
}

/// A menu's scroll, keeping the keyboard's row in view. It scrolls only when
/// that row changes (or the menu opens), so it never fights the wheel.
#[derive(Clone, Default)]
pub struct MenuScroll {
    handle: ScrollHandle,
    row: Rc<Cell<Option<usize>>>,
}

impl std::fmt::Debug for MenuScroll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuScroll").field("row", &self.row.get()).finish()
    }
}

impl MenuScroll {
    /// The handle the menu's scroll container tracks.
    pub fn handle(&self) -> &ScrollHandle {
        &self.handle
    }

    /// Keep child `child` of the scroll container (an index among all its
    /// children, headings and rules included) in view; `None` while the
    /// menu is closed, so the next open scrolls to its row again.
    pub fn follow(&self, child: Option<usize>) {
        if self.row.replace(child) != child {
            if let Some(ix) = child {
                self.handle.scroll_to_item(ix);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{size, TestAppContext, VisualTestContext};

    const VIEWPORT: Size<Pixels> = Size {
        width: px(800.0),
        height: px(600.0),
    };

    fn trigger(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(200.0), px(32.0)))
    }

    /// A menu `w` × `h` at its natural size, laid out like a capped one.
    fn menu(w: f32, h: f32) -> impl FnMut(Pixels) -> Size<Pixels> {
        move |cap| size(px(w), px(h).min(cap))
    }

    #[test]
    fn a_menu_that_fits_opens_below() {
        let p = place(trigger(40.0, 40.0), VIEWPORT, px(4.0), Align::Left, menu(200.0, 300.0));
        assert_eq!(p.side, Side::Below);
        assert_eq!(p.bounds, Bounds::new(point(px(40.0), px(76.0)), size(px(200.0), px(300.0))));
    }

    #[test]
    fn near_the_bottom_it_opens_above() {
        // 520 + 32 + 4 = 556 down, 36 left below, 476 above.
        let p = place(trigger(40.0, 520.0), VIEWPORT, px(4.0), Align::Left, menu(200.0, 300.0));
        assert_eq!(p.side, Side::Above);
        assert_eq!(p.bounds.size.height, px(300.0));
        assert_eq!(p.bounds.bottom(), px(516.0), "4pt over the trigger");
    }

    #[test]
    fn a_menu_too_tall_for_both_sides_takes_the_bigger_and_is_capped() {
        // Below: 600 - 8 - 232 - 4 = 356. Above: 200 - 4 - 8 = 188.
        let p = place(trigger(40.0, 200.0), VIEWPORT, px(4.0), Align::Left, menu(200.0, 900.0));
        assert_eq!(p.side, Side::Below);
        assert_eq!(p.bounds.size.height, px(356.0));
        assert_eq!(p.bounds.bottom(), px(592.0), "8pt clear of the window's edge");
        // Lower down, the room above wins and caps it.
        let p = place(trigger(40.0, 400.0), VIEWPORT, px(4.0), Align::Left, menu(200.0, 900.0));
        assert_eq!(p.side, Side::Above);
        assert_eq!(p.bounds.top(), px(8.0));
        assert_eq!(p.bounds.size.height, px(388.0));
    }

    #[test]
    fn a_menu_cut_short_below_stays_there_when_above_has_less_room() {
        // 600 - 8 - 282 - 4 = 306 below, 250 - 4 - 8 = 238 above.
        let p = place(trigger(40.0, 250.0), VIEWPORT, px(4.0), Align::Left, menu(200.0, 400.0));
        assert_eq!(p.side, Side::Below);
        assert_eq!(p.bounds.size.height, px(306.0));
    }

    #[test]
    fn it_stays_inside_the_window_sideways() {
        let p = place(trigger(700.0, 40.0), VIEWPORT, px(4.0), Align::Left, menu(240.0, 100.0));
        assert_eq!(p.bounds.right(), px(792.0), "shifted left, 8pt in");
        let p = place(trigger(0.0, 40.0), VIEWPORT, px(4.0), Align::Right, menu(240.0, 100.0));
        assert_eq!(p.bounds.left(), px(8.0), "a right-aligned menu shifted right, 8pt in");
        let p = place(trigger(500.0, 40.0), VIEWPORT, px(4.0), Align::Right, menu(240.0, 100.0));
        assert_eq!(p.bounds.right(), px(700.0), "right edges lined up");
    }

    // ---- the element, laid out by gpui ------------------------------------

    struct Host {
        anchor: Anchor,
        trigger_top: f32,
        rows: usize,
        scroll: MenuScroll,
        highlight: usize,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            let rows = (0..self.rows).map(|ix| {
                div()
                    .id(("row", ix))
                    .debug_selector(move || format!("row-{ix}"))
                    .flex_none()
                    .h(px(30.0))
            });
            div()
                .size_full()
                .child(
                    div()
                        .absolute()
                        .top(px(self.trigger_top))
                        .left(px(40.0))
                        .w(px(200.0))
                        .h(px(32.0))
                        .child(crate::theme::style::anchor_probe(&self.anchor)),
                )
                .child(gpui::deferred(placed(
                    &self.anchor,
                    div()
                        .id("menu")
                        .debug_selector(|| "menu".into())
                        .flex()
                        .flex_col()
                        .w(px(180.0))
                        .p(px(4.0))
                        .overflow_y_scroll()
                        .track_scroll(self.scroll.handle())
                        .children(rows),
                )))
                .map(|d| {
                    self.scroll.follow(Some(self.highlight));
                    d
                })
        }
    }

    fn host(cx: &mut TestAppContext, trigger_top: f32, rows: usize) -> (gpui::Entity<Host>, VisualTestContext) {
        let window = cx.add_window(|_, _| Host {
            anchor: Anchor::default(),
            trigger_top,
            rows,
            scroll: MenuScroll::default(),
            highlight: 0,
        });
        let host = window.root(cx).unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.simulate_resize(VIEWPORT);
        vcx.run_until_parked();
        // A second frame: the probe has painted.
        host.update(&mut vcx, |_, cx| cx.notify());
        vcx.run_until_parked();
        (host, vcx)
    }

    #[gpui::test]
    fn the_element_flips_a_menu_above_a_control_near_the_bottom(cx: &mut TestAppContext) {
        let (_, mut cx) = host(cx, 500.0, 8);
        let menu = cx.debug_bounds("menu").unwrap();
        assert_eq!(menu.size.height, px(248.0), "8 rows and 4pt padding, uncapped");
        assert_eq!(menu.bottom(), px(496.0), "above the control");
    }

    #[gpui::test]
    fn the_element_caps_a_menu_too_tall_for_the_window_and_it_scrolls(cx: &mut TestAppContext) {
        let (host, mut cx) = host(cx, 40.0, 40);
        let menu = cx.debug_bounds("menu").unwrap();
        assert_eq!(menu.top(), px(76.0), "below the control");
        assert_eq!(menu.bottom(), px(592.0), "capped 8pt from the window's edge");
        // The keyboard's row at the end scrolls into view.
        host.update(&mut cx, |h, cx| {
            h.highlight = 39;
            cx.notify();
        });
        cx.run_until_parked();
        let last = cx.debug_bounds("row-39").unwrap();
        assert!(last.bottom() <= menu.bottom(), "the last row shows: {last:?} in {menu:?}");
    }

    #[gpui::test]
    fn the_element_re_places_a_menu_when_the_window_shrinks(cx: &mut TestAppContext) {
        let (host, mut cx) = host(cx, 40.0, 10);
        assert_eq!(cx.debug_bounds("menu").unwrap().size.height, px(308.0));
        cx.simulate_resize(size(px(800.0), px(240.0)));
        host.update(&mut cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let menu = cx.debug_bounds("menu").unwrap();
        assert_eq!(menu.bottom(), px(232.0), "re-capped to the smaller window");
    }

    // ---- dismissal ----------------------------------------------------------

    /// A dropdown: a button toggling a menu with one item, beside another
    /// button and over a row that both count their clicks.
    #[derive(Default)]
    struct Dropdown {
        anchor: Anchor,
        open: bool,
        dismissed: usize,
        picked: usize,
        other: usize,
        under: usize,
    }

    impl Render for Dropdown {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let menu = self.open.then(|| {
                let item = div()
                    .id("item")
                    .debug_selector(|| "item".into())
                    .h(px(30.0))
                    .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                        this.picked += 1;
                        this.open = false;
                        cx.notify();
                    }));
                let menu = div()
                    .id("menu")
                    .debug_selector(|| "menu".into())
                    .flex()
                    .flex_col()
                    .w(px(180.0))
                    .p(px(4.0))
                    .occlude()
                    .child(item);
                gpui::deferred(placed(&self.anchor, menu).on_dismiss(cx.listener(
                    |this, _: &MouseDownEvent, _, cx| {
                        this.dismissed += 1;
                        this.open = false;
                        cx.notify();
                    },
                )))
                .with_priority(1)
            });
            div()
                .id("under")
                .debug_selector(|| "under".into())
                .size_full()
                .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, _| this.under += 1))
                .child(
                    div()
                        .id("button")
                        .debug_selector(|| "button".into())
                        .absolute()
                        .top(px(40.0))
                        .left(px(40.0))
                        .w(px(200.0))
                        .h(px(32.0))
                        .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.open = !this.open;
                            cx.notify();
                        }))
                        .child(crate::theme::style::anchor_probe(&self.anchor)),
                )
                .child(
                    div()
                        .id("other")
                        .debug_selector(|| "other".into())
                        .absolute()
                        .top(px(40.0))
                        .left(px(400.0))
                        .w(px(100.0))
                        .h(px(32.0))
                        .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                            cx.stop_propagation();
                            this.other += 1;
                        })),
                )
                .children(menu)
        }
    }

    fn dropdown(cx: &mut TestAppContext) -> (gpui::Entity<Dropdown>, VisualTestContext) {
        let window = cx.add_window(|_, _| Dropdown::default());
        let view = window.root(cx).unwrap();
        let vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.simulate_resize(VIEWPORT);
        vcx.run_until_parked();
        (view, vcx)
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
        cx.simulate_click(at, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    fn click_at(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_click(at, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_second_click_on_the_button_closes_its_menu(cx: &mut TestAppContext) {
        let (view, mut cx) = dropdown(cx);
        click(&mut cx, "button");
        assert!(view.read_with(&cx, |v, _| v.open), "the first click opens it");
        click(&mut cx, "button");
        let (open, dismissed) = view.read_with(&cx, |v, _| (v.open, v.dismissed));
        assert!(!open, "the second click closes it, and doesn't reopen it");
        assert_eq!(dismissed, 1);
        click(&mut cx, "button");
        assert!(view.read_with(&cx, |v, _| v.open), "a third opens it again");
    }

    #[gpui::test]
    fn a_click_outside_the_menu_closes_it_and_does_nothing_else(cx: &mut TestAppContext) {
        let (view, mut cx) = dropdown(cx);
        click(&mut cx, "button");
        // Another control: the menu closes, the control isn't clicked.
        click(&mut cx, "other");
        let (open, dismissed, other) = view.read_with(&cx, |v, _| (v.open, v.dismissed, v.other));
        assert!(!open && dismissed == 1);
        assert_eq!(other, 0, "the press went no further than the menu's layer");
        click(&mut cx, "other");
        assert_eq!(view.read_with(&cx, |v, _| v.other), 1, "closed, the control clicks again");
        // Bare background, far from the button.
        click(&mut cx, "button");
        click_at(&mut cx, point(px(700.0), px(500.0)));
        let (open, dismissed, under) = view.read_with(&cx, |v, _| (v.open, v.dismissed, v.under));
        assert!(!open && dismissed == 2);
        assert_eq!(under, 0, "nothing under the menu took the click");
    }

    #[gpui::test]
    fn a_click_inside_the_menu_picks_without_dismissing(cx: &mut TestAppContext) {
        let (view, mut cx) = dropdown(cx);
        click(&mut cx, "button");
        // The menu's padding: neither its item nor outside it.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let menu = cx.debug_bounds("menu").unwrap();
        click_at(&mut cx, point(menu.left() + px(2.0), menu.top() + px(2.0)));
        let (open, dismissed, under) = view.read_with(&cx, |v, _| (v.open, v.dismissed, v.under));
        assert!(open && dismissed == 0 && under == 0, "a press on the menu itself keeps it open");
        click(&mut cx, "item");
        let (open, dismissed, picked) = view.read_with(&cx, |v, _| (v.open, v.dismissed, v.picked));
        assert_eq!((open, dismissed, picked), (false, 0, 1), "the item picks and closes it");
    }
}
