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
//!   window itself moves there. Without global positions (Wayland) the new
//!   window opens where the compositor puts it, and only the tab menu's
//!   "Move to window" moves a tab into another window ([`move_tab_to`]).
//!
//! The project travels as a switch does: the giving window remembers where
//! it was left, the taking window adopts that and attaches the same daemon
//! sessions, and a window left with no tab closes. A project on another
//! machine takes its host with it (`mixed-host-tabs`): the taking window
//! shares the giver's open host (its one link), so it stays connected. The
//! New tab never travels: it is the bar's `+` slot (`new-tab-plus-morph`),
//! pinned past the project tabs, and a tab dropped on a bar lands before it.

use gpui::{point, px, App, Bounds, Context, Point, Window, WindowHandle, WindowId};

use super::*;
use crate::platform::screen;
use crate::views::tab_drag::BarRole;
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
        // Any tab can leave: a remote one takes its host along.
        state.tearable = true;
        state.can_float = screen::global_positions();
        let frame = screen::content_frame(window);
        let at = window.mouse_position();
        state.pointer = frame.origin + point(at.x.as_f32(), at.y.as_f32());
        tab_tearoff::begin(state, cx);
        cx.notify();
    }

    /// Every window's tab bar in screen space, front to back, this window's
    /// (`frame`) among them.
    fn bars_for_drag(&self, me: WindowId, frame: Bounds<f32>, cx: &mut App) -> Vec<BarSpot<WindowId>> {
        let own = self
            .tab_reorder
            .layout
            .borrow()
            .bar
            .map(|bar| BarSpot { window: me, frame, bar: on_screen(bar, &frame) });
        // No shared screen space (Wayland): the other windows' bars can't be
        // placed against the pointer, so only this one's counts.
        if !screen::global_positions() {
            return own.into_iter().collect();
        }
        let mut bars = Vec::new();
        let mut own_listed = false;
        for handle in windows_front_to_back(cx) {
            let id = handle.window_id();
            if id == me {
                bars.extend(own);
                own_listed = true;
                continue;
            }
            let spot = handle
                .update(cx, |w, window, _| {
                    let bar = w.tab_reorder.layout.borrow().bar?;
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

    /// This window's tab drag moved: find the bar it is over and the gap
    /// there, put them in the drag session, have every bar follow it (the
    /// one the tab is over opens its gap, every other closes its own), turn
    /// the ghost into the card or back, and keep the floating ghost under
    /// the pointer.
    pub(super) fn tab_drag_moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let me = window.window_handle().window_id();
        let Some(state) = tab_tearoff::session(cx).filter(|s| s.source == me) else {
            return;
        };
        let tearable = state.tearable;
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
            .map_or(local.x.as_f32(), |b| pointer.x - b.frame.origin.x);
        let gap = match over {
            Some(w) if w == me => (target == Some(me))
                .then(|| {
                    self.tab_reorder
                        .layout
                        .borrow()
                        .gap_at(self.tabs.strip(), local.x.as_f32(), layout_tail(&self.tab_reorder))
                })
                .flatten(),
            // Another window's bar: over its `+`, the gap at the end (where
            // a drop there lands).
            Some(other) => find_window(other, cx).and_then(|h| {
                h.read_with(cx, |w, _| {
                    let order = w.tabs.strip();
                    w.tab_reorder
                        .layout
                        .borrow()
                        .gap_at(order, over_x, layout_tail(&w.tab_reorder))
                        .unwrap_or(order.len())
                })
                .ok()
            }),
            None => None,
        };
        if let Some(state) = tab_tearoff::session_mut(cx) {
            state.pointer = pointer;
            state.over = over;
            state.over_x = over_x;
            state.gap = gap;
            let card = if over.is_none() { 1.0 } else { 0.0 };
            let reduce_motion = state.reduce_motion;
            state.morph.head(card, now, reduce_motion);
        }
        self.tab_reorder.follow(&self.tabs, tab_tearoff::role(me, cx));
        follow_tab_drag(Some(me), cx);
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
                // It lands in the gap the session showed there.
                let landing = Landing {
                    project: state.project.clone(),
                    role: state.role(to),
                    x: state.over_x,
                };
                cx.defer(move |cx| move_tab(landing, me, to, cx));
            }
            (None, Some(state)) if self.tabs.order().len() > 1 => {
                self.tab_reorder.cancel();
                let tear = TearOff {
                    project: state.project.clone(),
                    from: me,
                    at: Some(state.pointer),
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
        // The drag is over: no other bar shows a gap (the one taking the tab
        // opens it again as it lands).
        follow_tab_drag(Some(me), cx);
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
            tab_tearoff::close_float(state, cx);
        }
        follow_tab_drag(Some(me), cx);
        self.tab_reorder.cancel();
        cx.stop_active_drag(window);
        cx.notify();
        true
    }

    /// Tab `id`'s host, open here, for a remote tab moving to another window
    /// (`None` for a project here, or a host not open yet).
    fn host_of_tab(&self, id: &str) -> Option<RemoteWindow> {
        self.host_remote(tab_key(id).host()?).cloned()
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

    /// A tab dropped here from another window: it takes the gap the drag
    /// showed here (`landing.role`'s), and the window comes forward on it.
    fn take_tab(&mut self, landing: &Landing, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_reorder.follow(&self.tabs, landing.role.clone());
        if self.tab_reorder.accept(&mut self.tabs, landing.x) {
            self.tabs_changed(cx);
        }
        window.activate_window();
        self.switch_to(&landing.project, window, cx);
        cx.notify();
    }
}

/// Every Shelbi window's bar but `skip`'s follows the drag session: the one
/// the tab is over opens its gap, the rest show none (a window whose bar
/// changed draws again).
fn follow_tab_drag(skip: Option<WindowId>, cx: &mut App) {
    for handle in shelbi_windows(cx) {
        let id = handle.window_id();
        if Some(id) == skip {
            continue;
        }
        let role = tab_tearoff::role(id, cx);
        let _ = handle.update(cx, |w, _, cx| {
            if w.tab_reorder.follow(&w.tabs, role) {
                cx.notify();
            }
        });
    }
}

/// A tab dropped on another window's bar: its project, the gap it showed
/// there, and the pointer's x in that window.
struct Landing {
    project: String,
    role: BarRole,
    x: f32,
}

/// Move `landing`'s tab from window `from` to window `to`, into the gap it
/// showed there. Where the project was left travels with it.
fn move_tab(landing: Landing, from: WindowId, to: WindowId, cx: &mut App) {
    let project = landing.project.clone();
    let (Some(source), Some(target)) = (find_window(from, cx), find_window(to, cx)) else {
        return;
    };
    let host = source
        .update(cx, |w, _, cx| {
            if w.shows(&project) {
                w.remember_view_state(cx);
            }
            w.host_of_tab(&project)
        })
        .ok()
        .flatten();
    if target
        .update(cx, |w, window, cx| {
            // A remote tab's host comes along, open: the tab stays connected.
            if let Some(host) = host {
                w.adopt_host(host, cx);
            }
            w.take_tab(&landing, window, cx)
        })
        .is_ok()
    {
        let _ = source.update(cx, |w, window, cx| w.give_up_tab(&project, window, cx));
    }
}

/// A tab torn off its window: what [`tear_off`] needs to place its new one.
struct TearOff {
    project: String,
    from: WindowId,
    /// The drop, in screen space (`None`: no drop, Move to Window ▸ New
    /// Window; the window opens where a new one would).
    at: Option<Point<f32>>,
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
    let Ok((mut state, size, source_display, host)) = source.update(cx, |w, window, cx| {
        if w.shows(&t.project) {
            w.remember_view_state(cx);
        }
        let state = WindowState {
            sidebar_width: w.window_state.sidebar_width,
            sidebar_collapsed: w.window_state.sidebar_collapsed,
            task_pane_width: w.window_state.task_pane_width,
            tabs: vec![tab_key(&t.project)],
            ..WindowState::default()
        };
        let host = w.host_of_tab(&t.project);
        (state, window.window_bounds().get_bounds().size, window_display(window, cx), host)
    }) else {
        return;
    };
    // Where there is no screen space to place it in (Wayland), the
    // compositor places it (Hyprland tiles it).
    let at = t.at.filter(|_| screen::global_positions());
    let bounds = match at {
        Some(at) => {
            let origin = at - point(t.first_x + t.grab.x, t.grab.y);
            // Display-relative, on the display the tab was dropped on (screen
            // space is the source's display-relative space when the platform
            // can't say).
            let (origin, display) = match screen::display_at(at, cx) {
                Some((id, top_left)) => {
                    let display = cx.displays().into_iter().find(|d| d.id() == id).map(|d| display_ref(&*d));
                    (origin - top_left, display)
                }
                None => (origin, source_display),
            };
            state.display = display;
            Some(Bounds::new(point(px(origin.x), px(origin.y)), size))
        }
        None => {
            state.display = source_display;
            None
        }
    };
    let taken: Vec<usize> = cx
        .try_global::<Shell>()
        .map(|s| s.windows.iter().map(|w| w.slot).collect())
        .unwrap_or_default();
    let slot = WindowSession::free_slot(&taken);
    // A remote tab opens over its host, as the source had it open (or, not
    // yet open there, opening in the new window's tab).
    let key = tab_key(&t.project);
    let Some(handle) = open_window_on(&key, host, slot, state, bounds, cx) else {
        return;
    };
    let _ = handle.update(cx, |w, window, cx| {
        if let Some(saved) = cx.try_global::<Catalog>().and_then(|c| c.known_view_state(&key)) {
            w.adopt_view_state(saved);
        }
        window.activate_window();
    });
    let _ = source.update(cx, |w, window, cx| w.give_up_tab(&t.project, window, cx));
}

/// The tab menu's Move to Window: move `project`'s tab from window `from`
/// to window `to`, as its last tab (`None`: into a new window of its own).
/// Where the project was left, and a remote tab's open host, travel with it,
/// as they do when the tab is dragged there.
pub(super) fn move_tab_to(project: String, from: WindowId, to: Option<WindowId>, cx: &mut App) {
    let Some(to) = to else {
        tear_off(
            TearOff {
                project,
                from,
                at: None,
                grab: Point::default(),
                first_x: 0.0,
            },
            cx,
        );
        return;
    };
    let (Some(source), Some(target)) = (find_window(from, cx), find_window(to, cx)) else {
        return;
    };
    let host = source
        .update(cx, |w, _, cx| {
            if w.shows(&project) {
                w.remember_view_state(cx);
            }
            w.host_of_tab(&project)
        })
        .ok()
        .flatten();
    if target
        .update(cx, |w, window, cx| {
            if let Some(host) = host {
                w.adopt_host(host, cx);
            }
            if w.tabs.insert(&project) {
                w.tabs_changed(cx);
            }
            window.activate_window();
            w.switch_to(&project, window, cx);
            cx.notify();
        })
        .is_ok()
    {
        let _ = source.update(cx, |w, window, cx| w.give_up_tab(&project, window, cx));
    }
}

/// The `+` slot's width in `reorder`'s bar as it last painted (wider while
/// it is the New tab): the span past the tabs where no gap opens.
fn layout_tail(reorder: &crate::views::tab_drag::TabReorder) -> f32 {
    reorder.layout.borrow().tail_w(tab_bar::PLUS_SLOT_W)
}
