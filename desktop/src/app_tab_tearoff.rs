//! The window's side of a tab torn off or dragged between windows
//! (`tab-tearoff`; the drag itself is [`crate::views::tab_tearoff`]'s).
//!
//! The window a tab is dragged out of follows the drag (its tab bar's
//! window-wide listener sends [`SidebarEvent::DragTabMoved`] and
//! [`SidebarEvent::DropTab`] wherever the pointer is), finds the bar under
//! the tab in every local window, and has that window open its gap. The drop
//! lands it:
//!
//! - over its own bar: a reorder, as before;
//! - over another window's bar: that window takes the tab at the gap and
//!   switches to it ([`move_tab`]);
//! - torn off: a new window opens holding just that tab, its tab under the
//!   pointer ([`tear_off`]), unless it was the window's only tab, when the
//!   window itself moves there.
//!
//! The project travels as a switch does: the giving window remembers where
//! it was left, the taking window adopts that and attaches the same daemon
//! sessions, and a window left with no tab closes. A New tab travels the
//! same way: torn off, its window shows the launch screen; dropped on a bar
//! whose window has a New tab already, that one is selected.

use gpui::{point, px, App, Bounds, Context, Point, Window, WindowHandle, WindowId};

use super::*;
use crate::platform::screen;
use crate::views::tab_tearoff::{self, BarSpot, DragState};

/// The Shelbi windows front to back: the platform's stack where it keeps
/// one, else the key window first, then the rest as they opened.
fn windows_front_to_back(cx: &App) -> Vec<WindowHandle<ShelbiWindow>> {
    let mut ours = shelbi_windows(cx);
    match cx.window_stack() {
        Some(stack) => ours.sort_by_key(|h| {
            stack
                .iter()
                .position(|s| s.window_id() == h.window_id())
                .unwrap_or(usize::MAX)
        }),
        None => {
            let active = cx.active_window().map(|a| a.window_id());
            if let Some(at) = ours.iter().position(|h| Some(h.window_id()) == active) {
                let key = ours.remove(at);
                ours.insert(0, key);
            }
        }
    }
    ours
}

fn find_window(id: WindowId, cx: &App) -> Option<WindowHandle<ShelbiWindow>> {
    shelbi_windows(cx).into_iter().find(|h| h.window_id() == id)
}

/// Run `f` on the Shelbi window `id`, if it is still open.
fn with_window(
    id: WindowId,
    cx: &mut App,
    f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>),
) {
    if let Some(handle) = find_window(id, cx) {
        let _ = handle.update(cx, f);
    }
}

/// `b` (window-local) moved into screen space by its window's `frame`.
fn on_screen(b: Bounds<f32>, frame: &Bounds<f32>) -> Bounds<f32> {
    Bounds::new(b.origin + frame.origin, b.size)
}

impl ShelbiWindow {
    /// A drag of `project`'s tab began here: the reorder starts, and so does
    /// the app's drag, which the tab may take to another window.
    pub(super) fn tab_drag_started(&mut self, project: String, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_reorder.start(&project);
        let me = window.window_handle().window_id();
        let (label, working) = self
            .tab_props(cx)
            .into_iter()
            .find(|t| t.name == project)
            .map(|t| (t.label, t.working))
            .unwrap_or_else(|| (project.clone().into(), false));
        let mut state = DragState::new(me, project.clone(), label, working);
        {
            let layout = self.tab_reorder.layout.borrow();
            state.slot_w = layout.width(&project);
            state.tab_w = (state.slot_w - 1.0).max(0.0);
            state.grab = point(layout.grab_x, layout.grab_y);
            state.tab_origin = point(layout.tab_origin.0, layout.tab_origin.1);
            state.first_x = layout.first_x;
        }
        state.reduce_motion = (self.reduce_motion)();
        state.tearable = self.remote.is_none();
        let frame = screen::content_frame(window);
        let at = window.mouse_position();
        state.pointer = frame.origin + point(at.x.as_f32(), at.y.as_f32());
        tab_tearoff::begin(state, cx);
        cx.notify();
    }

    /// Every local window's tab bar in screen space, front to back, this
    /// window's (`frame`) among them. A remote window's tab stays in its
    /// window, so it sees only its own bar.
    fn bars_for_drag(&self, me: WindowId, frame: Bounds<f32>, cx: &mut App) -> Vec<BarSpot<WindowId>> {
        let own = self
            .tab_reorder
            .layout
            .borrow()
            .bar
            .map(|bar| BarSpot { window: me, frame, bar: on_screen(bar, &frame) });
        let mut bars = Vec::new();
        let mut own_listed = false;
        for handle in windows_front_to_back(cx) {
            let id = handle.window_id();
            if id == me {
                bars.extend(own);
                own_listed = true;
                continue;
            }
            if self.remote.is_some() {
                continue;
            }
            let spot = handle
                .update(cx, |w, window, _| {
                    let bar = w.tab_reorder.layout.borrow().bar.filter(|_| w.remote.is_none())?;
                    let frame = screen::content_frame(window);
                    Some(BarSpot { window: id, frame, bar: on_screen(bar, &frame) })
                })
                .ok()
                .flatten();
            bars.extend(spot);
        }
        if !own_listed {
            // A window the shell doesn't list (a test's): the key one.
            if let Some(own) = own {
                bars.insert(0, own);
            }
        }
        bars
    }

    /// This window's tab drag moved: find the bar it is over, open that
    /// bar's gap (closing the last one's), turn the ghost into the card or
    /// back, and keep the floating ghost under the pointer.
    pub(super) fn tab_drag_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = window.window_handle().window_id();
        let Some(state) = tab_tearoff::session(cx).filter(|s| s.source == me) else {
            return;
        };
        let (project, slot_w, grab_x, tearable, was_over) =
            (state.project.clone(), state.slot_w, state.grab.x, state.tearable, state.over);
        let now = (self.clock)();
        let frame = screen::content_frame(window);
        let local = window.mouse_position();
        let pointer = frame.origin + point(local.x.as_f32(), local.y.as_f32());
        let bars = self.bars_for_drag(me, frame, cx);
        let target = tab_tearoff::target(pointer, &bars);
        // A tab that can't leave stays this bar's, a tab ghost: within reach
        // the gap opens, past it none does.
        let over = if tearable { target } else { Some(me) };
        let over_x = bars
            .iter()
            .find(|b| Some(b.window) == over)
            .map_or(0.0, |b| pointer.x - b.frame.origin.x);
        if let Some(state) = tab_tearoff::session_mut(cx) {
            state.pointer = pointer;
            state.over = over;
            state.over_x = over_x;
            let card = if over.is_none() { 1.0 } else { 0.0 };
            let reduce_motion = state.reduce_motion;
            state.morph.head(card, now, reduce_motion);
        }

        if was_over != over {
            if let Some(left) = was_over.filter(|w| *w != me) {
                with_window(left, cx, |w, _, cx| {
                    w.tab_reorder.unhost();
                    cx.notify();
                });
            }
        }
        if let Some(other) = over.filter(|w| *w != me) {
            let entered = was_over != over;
            with_window(other, cx, |w, _, cx| {
                if entered {
                    w.tab_reorder.host(&w.tabs, &project, slot_w, grab_x);
                }
                let gap = w
                    .tab_reorder
                    .layout
                    .borrow()
                    .gap_at(w.tabs.order(), over_x, tab_bar::PLUS_SLOT_W);
                w.tab_reorder.over(gap);
                cx.notify();
            });
        }
        let gap = (target == Some(me))
            .then(|| {
                self.tab_reorder
                    .layout
                    .borrow()
                    .gap_at(self.tabs.order(), local.x.as_f32(), tab_bar::PLUS_SLOT_W)
            })
            .flatten();
        self.tab_reorder.over(gap);
        let display = window.display(cx).map(|d| d.id());
        tab_tearoff::sync_float(display, cx);
        cx.notify();
    }

    /// This window's tab drag was released: land it where it is.
    pub(super) fn tab_drag_dropped(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = window.window_handle().window_id();
        let pointer_x = window.mouse_position().x.as_f32();
        let state = match tab_tearoff::session(cx) {
            Some(s) if s.source == me => tab_tearoff::end(cx),
            _ => None,
        };
        let over = state.as_ref().map_or(Some(me), |s| s.over);
        match (over, &state) {
            (Some(o), _) if o == me => {
                if self.tab_reorder.drop(&mut self.tabs, pointer_x) {
                    self.tabs_changed(cx);
                }
            }
            (Some(to), Some(state)) => {
                self.tab_reorder.cancel();
                let (project, x) = (state.project.clone(), state.over_x);
                cx.defer(move |cx| move_tab(project, me, to, x, cx));
            }
            (None, Some(state)) if self.tabs.order().len() > 1 => {
                self.tab_reorder.cancel();
                let tear = TearOff {
                    project: state.project.clone(),
                    from: me,
                    at: state.pointer,
                    grab: state.grab,
                    first_x: state.first_x,
                };
                cx.defer(move |cx| tear_off(tear, cx));
            }
            (None, Some(state)) => {
                // Its only tab, and nowhere to go: the window goes where it
                // was dropped, its tab under the pointer.
                self.tab_reorder.cancel();
                screen::move_window(window, state.pointer - state.tab_origin - state.grab, cx);
            }
            _ => self.tab_reorder.cancel(),
        }
        if let Some(state) = state {
            tab_tearoff::close_float(state, cx);
        }
        cx.notify();
    }

    /// Esc during this window's tab drag: it ends where it began. Whether
    /// there was one.
    pub(super) fn cancel_tab_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let me = window.window_handle().window_id();
        let ours = tab_tearoff::session(cx).is_some_and(|s| s.source == me);
        if !ours && !(cx.has_active_drag() && self.tab_reorder.ghost_alive()) {
            return false;
        }
        if let Some(state) = tab_tearoff::end(cx) {
            if let Some(other) = state.over.filter(|w| *w != me) {
                with_window(other, cx, |w, _, cx| {
                    w.tab_reorder.unhost();
                    cx.notify();
                });
            }
            tab_tearoff::close_float(state, cx);
        }
        self.tab_reorder.cancel();
        cx.stop_active_drag(window);
        cx.notify();
        true
    }

    /// `project`'s tab went to another window: it goes here, and a window
    /// showing it moves to its neighbor tab, or closes with none left.
    fn give_up_tab(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) {
        let neighbor = self.tabs.neighbor(project).map(str::to_string);
        let shown = self.shows(project);
        if self.tabs.remove(project) {
            self.tabs_changed(cx);
        }
        if !shown {
            return;
        }
        match neighbor {
            Some(next) => self.switch_to(&next, window, cx),
            // No empty window is left behind.
            None => window.remove_window(),
        }
    }

    /// A tab dropped here from another window, with the pointer at window x
    /// `x`: it takes the gap it was over, and the window comes forward on it.
    fn take_tab(&mut self, project: &str, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_reorder.accept(&mut self.tabs, x) {
            self.tabs_changed(cx);
        }
        window.activate_window();
        self.switch_to(project, window, cx);
        cx.notify();
    }
}

/// Move `project`'s tab from window `from` to window `to` (dropped at window
/// x `x` there). Where the project was left travels with it.
fn move_tab(project: String, from: WindowId, to: WindowId, x: f32, cx: &mut App) {
    let (Some(source), Some(target)) = (find_window(from, cx), find_window(to, cx)) else {
        return;
    };
    let _ = source.update(cx, |w, _, cx| {
        if w.shows(&project) {
            w.remember_view_state(cx);
        }
    });
    if target
        .update(cx, |w, window, cx| w.take_tab(&project, x, window, cx))
        .is_ok()
    {
        let _ = source.update(cx, |w, window, cx| w.give_up_tab(&project, window, cx));
    }
}

/// A tab torn off its window: what [`tear_off`] needs to place its new one.
struct TearOff {
    project: String,
    from: WindowId,
    /// The drop, in screen space.
    at: Point<f32>,
    /// Where in the tab the pointer held it.
    grab: Point<f32>,
    /// Where the source window's first tab starts (the new window's only
    /// tab starts there too: it has the same sidebar).
    first_x: f32,
}

/// Open a window holding just the torn-off tab, its tab under the drop
/// point (as near as the display allows), the size of the window it came
/// from, with that window's sidebar; the source gives the tab up.
fn tear_off(t: TearOff, cx: &mut App) {
    let Some(source) = find_window(t.from, cx) else {
        return;
    };
    let Ok((mut state, size, source_display)) = source.update(cx, |w, window, cx| {
        if w.shows(&t.project) {
            w.remember_view_state(cx);
        }
        let state = WindowState {
            sidebar_width: w.window_state.sidebar_width,
            sidebar_collapsed: w.window_state.sidebar_collapsed,
            task_pane_width: w.window_state.task_pane_width,
            tabs: if t.project == NEW_TAB { Vec::new() } else { vec![t.project.clone()] },
            ..WindowState::default()
        };
        (state, window.window_bounds().get_bounds().size, window_display(window, cx))
    }) else {
        return;
    };
    let origin = t.at - point(t.first_x + t.grab.x, t.grab.y);
    // Display-relative, on the display the tab was dropped on (screen space
    // is the source's display-relative space when the platform can't say).
    let (origin, display) = match screen::display_at(t.at, cx) {
        Some((id, top_left)) => {
            let display = cx.displays().into_iter().find(|d| d.id() == id).map(|d| display_ref(&*d));
            (origin - top_left, display)
        }
        None => (origin, source_display),
    };
    state.display = display;
    let bounds = Bounds::new(point(px(origin.x), px(origin.y)), size);
    let taken: Vec<usize> = cx
        .try_global::<Shell>()
        .map(|s| s.windows.iter().map(|w| w.slot).collect())
        .unwrap_or_default();
    let slot = WindowSession::free_slot(&taken);
    if t.project == NEW_TAB {
        if let Some(handle) = open_new_tab_window(slot, state, bounds, cx) {
            let _ = handle.update(cx, |_, window, _| window.activate_window());
            let _ = source.update(cx, |w, window, cx| w.give_up_tab(&t.project, window, cx));
        }
        return;
    }
    let Some(handle) = open_window(&t.project, slot, state, Some(bounds), cx) else {
        return;
    };
    let key = ProjectKey::local(&t.project);
    let _ = handle.update(cx, |w, window, cx| {
        if let Some(saved) = cx.try_global::<Catalog>().and_then(|c| c.known_view_state(&key)) {
            w.adopt_view_state(saved);
        }
        window.activate_window();
    });
    let _ = source.update(cx, |w, window, cx| w.give_up_tab(&t.project, window, cx));
}
