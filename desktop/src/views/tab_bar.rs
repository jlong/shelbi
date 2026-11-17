//! The project tab bar (`project-tabs`): the Figma `Tabs` across the top of
//! the window (`Orchestrator · Dark`, node 2792:33339), 52pt tall, the
//! native traffic lights on its left.
//!
//! Left to right: one tab per open project ([`crate::project_tabs`]),
//! starting just past the traffic lights, each a green dot and the project's
//! name between 1pt rules, then the `+` tab. The bar's right end is empty:
//! the command palette opens from the sidebar's header row (its Search
//! button), so the bar doesn't compete with the screens' own search fields.
//! While the sidebar is hidden a Toggle sidebar button slides in before the
//! first tab, with the sidebar's own reveal, to bring it back (the sidebar's
//! header row has the toggle otherwise). The selected tab fills with the
//! main area's color, so it reads as
//! joined to the content below; the rest are unfilled. Hovering a tab swaps
//! its dot for an `×` that closes the project. While ⌘ is held each of the
//! first nine tabs shows its ⌘N.
//!
//! The `+` tab opens a New tab (`new-tab-launch`) after the last tab and
//! selects it, or selects the window's New tab when it has one: "New tab"
//! beside a gray ring (no project, so no status), its content the launch
//! screen ([`super::launch`]). It closes with its `×` as a project's tab
//! does, without asking (nothing stops), and drags, reorders and tears off
//! the same way.
//!
//! Right-clicking a project's tab (never the `+` or the New tab) opens its context menu at
//! the pointer ([`TabMenu`]): "Close Project", closing that tab's project as
//! its `×` does, and "Close All Other Projects", closing every other tab's
//! after one confirmation (dimmed while it's the only tab). It closes on a
//! press outside it or Esc.
//!
//! Dragging a tab moves it ([`TabReorder`], [`super::tab_drag`]): a
//! tab-shaped ghost ([`TabGhost`]) follows the pointer, the dragged tab's
//! slot empties (it is "lifted"), and the other tabs slide aside so that
//! empty slot opens in the gap it lands in: a tab's left half is the gap
//! before it, its right half the gap after (judged where the tabs rest, so
//! a tab sliding out from under the pointer doesn't flip it), the bar
//! before the first tab is the start, and its empty space past the `+` the
//! end. Over the `+` there is no gap: the tabs slide back, and a drop there
//! leaves the order alone. The gap follows the pointer while it is within
//! [`tab_tearoff::TEAR_OFF_PX`] of the bar, on it or off it. Dropped, the tab
//! settles from under the ghost into its slot. A press without movement
//! stays a click.
//!
//! Dragged further from every bar, the ghost turns into a card and the tab
//! tears off into a window of its own; over another window's bar it drops
//! into that one ([`tab_tearoff`]). The bar follows its own drags past the
//! window's edges for that, and records where it is for other windows'.
//!
//! The bar stays where it is while the sidebar hides and shows; only the
//! tabs step right to make room for the toggle.
//!
//! The window owns its titlebar drag (`app_owns_titlebar_drag`), so a press
//! on a tab is the tab's. The bar's empty space moves the window instead
//! ([`window_drag::region`]): the traffic lights' room before the first
//! item, and the space past the `+`. A double click there is the system's
//! titlebar action.

use std::time::Instant;

use gpui::{
    canvas, deferred, div, point, prelude::*, px, AnyElement, App, Bounds, Context, DispatchPhase, Div, Hsla,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, SharedString, Size, Stateful, Window,
};
use shelbi_app::overlay::palette::{project_indicator, ProjectIndicator};

use super::project_menu;
use super::sidebar::{self, SidebarEvent, SidebarHost, TOOLBAR_BUTTON, TRAFFIC_LIGHTS_RIGHT};
use super::tab_drag::{SharedLayout, TabFrame};
use super::tab_tearoff;
pub use super::tab_drag::TabReorder;
use crate::assets::{heroicon, icons, ICON_SIZE_MENU};
use crate::elements::placed::placed;
use crate::project_tabs::NEW_TAB;
use crate::elements::window_drag;
use crate::keys::tab_hotkey;
use crate::theme::motion;
use crate::theme::style::{self, Anchor, Type, Typography, NAV_RADIUS};
use crate::theme::Theme;

/// The bar's height, the 1pt rule under it included: the traffic lights
/// center on it (`sidebar::traffic_light_position`).
pub const TAB_BAR_H: f32 = 52.0;

/// The gap from the traffic lights' right edge to the bar's first item.
/// Figma draws its lights ending at 69 and the first tab's rule at 86; the
/// real lights end at [`TRAFFIC_LIGHTS_RIGHT`], so the bar keeps the frame's
/// 17pt past them.
const LIGHTS_GAP: f32 = 17.0;
/// Where the bar's first item starts: the first tab's rule, or, while the
/// sidebar is hidden, the Toggle sidebar button.
pub const LEAD_X: f32 = TRAFFIC_LIGHTS_RIGHT + LIGHTS_GAP;
/// The room the Toggle sidebar button takes while the sidebar is hidden:
/// the button and the 16pt before the first tab's rule.
pub const TOGGLE_ROOM: f32 = TOOLBAR_BUTTON + 16.0;

/// A tab's padding: 16pt before its dot, 20pt after its name, 8pt between.
const TAB_PAD_LEFT: f32 = 16.0;
const TAB_PAD_RIGHT: f32 = 20.0;
const TAB_GAP: f32 = 8.0;
/// The widest a tab grows; a longer name truncates.
const TAB_MAX_W: f32 = 240.0;
/// The 1pt rule between tabs.
const RULE_W: f32 = 1.0;
/// The `+` tab and the rule after it: no drop gap over it.
pub const PLUS_SLOT_W: f32 = TAB_BAR_H + RULE_W;

/// A tab's context menu's width.
const MENU_W: f32 = 240.0;
/// A tab's context menu's gap under the pointer.
const CONTEXT_GAP: f32 = 2.0;

/// What the New tab says.
pub const NEW_TAB_LABEL: &str = "New tab";

/// A tab's dot: an open project's (green, breathing while it works), or the
/// New tab's gray ring (no project, so no status).
pub fn indicator(name: &str, working: bool) -> ProjectIndicator {
    if name == NEW_TAB {
        ProjectIndicator::Unloaded
    } else {
        project_indicator(true, working)
    }
}

/// The name a tab's debug selectors use: the project's, or `new-tab`.
fn selector_name(name: &str) -> &str {
    if name == NEW_TAB {
        "new-tab"
    } else {
        name
    }
}

/// One tab, as a frame draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    /// The project id.
    pub name: String,
    /// What the tab says: the project's display name.
    pub label: SharedString,
    /// The window shows this project.
    pub selected: bool,
    /// One of its workspaces' agents is working: the dot breathes. A tab's
    /// project is always loaded, so it is the pulsing disc or the solid one.
    pub working: bool,
}

/// Everything one bar frame draws.
pub struct TabBarProps<'a> {
    pub tabs: &'a [Tab],
    /// The tab under the pointer (its dot is an `×`).
    pub hovered: Option<&'a str>,
    /// ⌘ is held: the first nine tabs show their ⌘N.
    pub command_held: bool,
    /// The sidebar shows, or is on its way (the Toggle sidebar tooltip says
    /// what a click does).
    pub sidebar_shown: bool,
    /// Where the sidebar stands, 0 hidden to 1 shown, eased
    /// ([`super::sidebar_reveal`]): the Toggle sidebar button shows as far as
    /// the sidebar is hidden.
    pub reveal: f32,
    /// A tab's context menu while it shows (opening, open or closing).
    pub tab_menu: Option<TabMenuProps<'a>>,
    /// Where the pointer was when a tab was last right-clicked: the context
    /// menu's anchor, which the right-click sets.
    pub tab_menu_anchor: &'a Anchor,
    /// The tabs' slide this frame ([`TabReorder::frame`]): each tab's x
    /// offset from where it rests, and the dragged (lifted) tab, whose slot
    /// draws empty.
    pub slide: &'a TabFrame,
    /// Where the tabs rest, measured as they paint (the drop gap is judged
    /// against it), and a drag's start.
    pub layout: &'a SharedLayout,
}

/// The value a tab drag carries.
#[derive(Debug, Clone)]
pub struct TabDrag {
    pub project: String,
    label: SharedString,
    working: bool,
}

/// The tab that follows the pointer during a drag: the tab as the selected
/// one draws it, lifted off the bar with a shadow; torn off, the card
/// ([`tab_tearoff::ghost`]). While the tab isn't over this window's bar the
/// floating ghost window draws it instead ([`tab_tearoff::FloatingGhost`]),
/// and this one draws nothing.
pub struct TabGhost {
    label: SharedString,
    indicator: ProjectIndicator,
    /// The dragged tab's width.
    tab_w: f32,
}

impl Render for TabGhost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().opaque();
        let now = Instant::now();
        let Some(drag) = tab_tearoff::session(cx) else {
            return tab_tearoff::ghost(self.label.clone(), self.indicator, 0.0, self.tab_w, &theme).into_any_element();
        };
        if drag.floats() {
            return div().into_any_element();
        }
        if drag.morph.animating(now) {
            window.request_animation_frame();
        }
        // gpui draws the ghost at the tab's grab point; the card is held
        // elsewhere ([`tab_tearoff::DragState::hold`]).
        let shift = drag.grab - drag.hold(now);
        div()
            .relative()
            .left(px(shift.x))
            .top(px(shift.y))
            .child(tab_tearoff::ghost(
                self.label.clone(),
                self.indicator,
                drag.morph.at(now),
                self.tab_w,
                &theme,
            ))
            .into_any_element()
    }
}

/// The tab drag in this window is live: its ghost is up.
fn drag_live(layout: &SharedLayout, cx: &App) -> bool {
    cx.has_active_drag() && layout.borrow().ghost_alive()
}

/// How one tab draws this frame, beyond the tab itself.
struct TabLook {
    hovered: bool,
    hotkey: Option<String>,
    /// It is the tab being dragged: its slot draws empty.
    lifted: bool,
}

/// A tab's context menu, as one frame draws it.
pub struct TabMenuProps<'a> {
    /// The project whose tab was right-clicked.
    pub project: &'a str,
    /// Another tab is open: "Close All Other Projects" can run.
    pub can_close_others: bool,
    /// The keyboard's row ([`TabMenuRow`]), if the keys have moved onto one.
    pub highlight: Option<TabMenuRow>,
    /// How shown the menu is: 0 (hidden) to 1, eased.
    pub phase: f32,
    /// Open, not fading out: only an open menu takes the window's presses.
    pub open: bool,
}

/// A row of a tab's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabMenuRow {
    CloseProject,
    CloseOthers,
}

impl TabMenuRow {
    pub const ALL: [TabMenuRow; 2] = [TabMenuRow::CloseProject, TabMenuRow::CloseOthers];

    pub fn label(self) -> &'static str {
        match self {
            TabMenuRow::CloseProject => "Close Project",
            TabMenuRow::CloseOthers => "Close All Other Projects",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            TabMenuRow::CloseProject => "tab-context-close",
            TabMenuRow::CloseOthers => "tab-context-close-others",
        }
    }
}

/// A tab's context menu's state, owned by the window: which tab it was
/// opened on (kept while it fades out), open or closed and when that last
/// changed, and the keyboard's row.
#[derive(Debug, Default)]
pub struct TabMenu {
    project: Option<String>,
    open: bool,
    changed: Option<Instant>,
    highlight: Option<TabMenuRow>,
    /// The pointer where the tab was right-clicked (the menu's anchor).
    pub anchor: Anchor,
}

impl TabMenu {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The tab it was opened on, while it shows or fades.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    pub fn highlight(&self) -> Option<TabMenuRow> {
        self.highlight
    }

    /// Open on `project`'s tab, no row lit (as a right-click opens it).
    pub fn open(&mut self, project: &str, now: Instant) {
        if !self.open || self.project.as_deref() != Some(project) {
            self.changed = Some(now);
        }
        self.project = Some(project.to_string());
        self.open = true;
        self.highlight = None;
    }

    pub fn close(&mut self, now: Instant) {
        if self.open {
            self.open = false;
            self.changed = Some(now);
        }
    }

    /// Move the keyboard's row by `delta` over the rows `enabled` allows,
    /// wrapping; from none, ↓ lands on the first and ↑ on the last.
    pub fn step(&mut self, delta: isize, enabled: impl Fn(TabMenuRow) -> bool) {
        let rows: Vec<TabMenuRow> = TabMenuRow::ALL.into_iter().filter(|r| enabled(*r)).collect();
        if rows.is_empty() {
            return;
        }
        let n = rows.len() as isize;
        let at = match self.highlight.and_then(|h| rows.iter().position(|r| *r == h)) {
            Some(ix) => (ix as isize + delta).rem_euclid(n),
            None if delta < 0 => n - 1,
            None => 0,
        };
        self.highlight = Some(rows[at as usize]);
    }

    /// How shown the menu is at `now`, `None` once it has finished closing.
    pub fn phase(&self, now: Instant, reduce_motion: bool) -> Option<f32> {
        menu_phase(self.open, self.changed, now, reduce_motion)
    }

    pub fn animating(&self, now: Instant, reduce_motion: bool) -> bool {
        menu_animating(self.changed, now, reduce_motion)
    }
}

/// A menu's slide-and-fade at `now` (0..=1 on Taskpage's curve), `None`
/// once a closed one has finished fading. Instant under `reduce_motion`.
fn menu_phase(open: bool, changed: Option<Instant>, now: Instant, reduce_motion: bool) -> Option<f32> {
    let t = match changed {
        Some(at) if !reduce_motion => now.saturating_duration_since(at).as_secs_f32() / motion::MENU_IN.as_secs_f32(),
        _ => 1.0,
    };
    let eased = motion::ease_out_expo(t.min(1.0));
    if open {
        Some(eased)
    } else if t < 1.0 {
        Some(1.0 - eased)
    } else {
        None
    }
}

/// Whether a menu that last changed at `changed` is mid-transition at `now`.
fn menu_animating(changed: Option<Instant>, now: Instant, reduce_motion: bool) -> bool {
    !reduce_motion && changed.is_some_and(|at| now.saturating_duration_since(at) < motion::MENU_IN)
}

/// The bar's fill: `slate/700` in the Dark frame, which is the overlay fill
/// (`button/default/background`) laid over the main area's color, so Light
/// gets its own light gray the same way.
pub fn bar_fill(theme: &Theme) -> Hsla {
    theme.canvas.blend(theme.background)
}

/// A 1pt full-height rule between tabs.
fn rule(theme: &Theme) -> Div {
    div().flex_none().w(px(RULE_W)).h(px(TAB_BAR_H)).bg(theme.divider)
}

/// A probe filling its parent, handing it the bounds it painted at.
fn probe(f: impl Fn(Bounds<gpui::Pixels>) + 'static) -> impl IntoElement {
    canvas(move |bounds, _, _| f(bounds), |_, _, _, _| {})
        .absolute()
        .top_0()
        .left_0()
        .size_full()
}

/// Render the bar.
pub fn render<H: SidebarHost>(props: TabBarProps<'_>, theme: &Theme, cx: &mut Context<H>) -> AnyElement {
    // Toggle sidebar, while the sidebar is hidden or on its way: its room
    // opens as the sidebar leaves (the tabs step right) and closes as it
    // comes back, the button fading with it.
    let hidden = 1.0 - props.reveal.clamp(0.0, 1.0);
    let toggle = (hidden > 0.0).then(|| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .w(px(TOGGLE_ROOM * hidden))
            .h(px(TAB_BAR_H))
            .overflow_hidden()
            .opacity(hidden)
            .child(
                sidebar::toggle_button("tab-bar-sidebar", Some(props.sidebar_shown), theme).on_click(cx.listener(
                    |this, _, window, cx| this.sidebar_event(SidebarEvent::Collapse, window, cx),
                )),
            )
    });

    // The strip's first rule, then each tab with the rule after it (its
    // slot), laid out where it rests and drawn at its slide offset.
    let mut strip: Vec<AnyElement> = vec![rule(theme).into_any_element()];
    for (ix, tab) in props.tabs.iter().enumerate() {
        let look = TabLook {
            hovered: props.hovered == Some(tab.name.as_str()),
            hotkey: (props.command_held && ix < 9).then(|| tab_hotkey(ix as u8 + 1)),
            lifted: props.slide.lifted.as_deref() == Some(tab.name.as_str()),
        };
        let measure = {
            let (layout, name) = (props.layout.clone(), tab.name.clone());
            probe(move |b| layout.borrow_mut().set_width(&name, b.size.width.as_f32()))
        };
        // The dragged tab gone from this bar: its slot isn't drawn, rule and
        // all (the tabs after it close up over it).
        let gone = look.lifted && props.slide.away;
        strip.push(
            div()
                .relative()
                .left(px(props.slide.offset(&tab.name)))
                .flex()
                .flex_row()
                .min_w_0()
                .when(gone, |d| d.opacity(0.0))
                .child(render_tab(tab, look, props.tab_menu_anchor, props.layout, theme, cx))
                .child(rule(theme))
                .child(measure)
                .into_any_element(),
        );
    }
    // The `+` and its rule follow the tabs' end: in when a tab leaves, out
    // when another window's tab opens a gap.
    strip.push(
        div()
            .relative()
            .left(px(props.slide.tail_offset()))
            .flex()
            .flex_row()
            .flex_none()
            .child(plus_tab(theme, cx))
            .child(rule(theme))
            .into_any_element(),
    );
    // The bar's empty space past the `+`: the gap after the last tab, and,
    // with no drag, a press there moves the window.
    strip.push(
        window_drag::region("tab-bar-drag-end")
            .debug_selector(|| "tab-bar-drag-end".into())
            .flex_1()
            .h(px(TAB_BAR_H))
            .into_any_element(),
    );
    let strip_origin = {
        let layout = props.layout.clone();
        probe(move |b| layout.borrow_mut().first_x = b.origin.x.as_f32() + RULE_W)
    };
    // The bar's bounds, for a tab dragged over it from another window.
    let bar_probe = {
        let layout = props.layout.clone();
        probe(move |b| {
            layout.borrow_mut().bar = Some(Bounds::new(
                point(b.origin.x.as_f32(), b.origin.y.as_f32()),
                gpui::size(b.size.width.as_f32(), b.size.height.as_f32()),
            ))
        })
    };
    // A tab drag of this window's, followed wherever the pointer goes (past
    // the window's edges too: AppKit keeps sending this window the press's
    // moves and its release), so the window can tell which bar the tab is
    // over, in any window, or that it is torn off (`tab-tearoff`).
    let follow = {
        let host = cx.entity().downgrade();
        let layout = props.layout.clone();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let (moved_host, moved_layout) = (host.clone(), layout.clone());
                window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && drag_live(&moved_layout, cx) {
                        moved_host
                            .update(cx, |this, cx| this.sidebar_event(SidebarEvent::DragTabMoved, window, cx))
                            .ok();
                    }
                });
                let (up_host, up_layout) = (host.clone(), layout.clone());
                window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && ev.button == MouseButton::Left && drag_live(&up_layout, cx) {
                        up_host
                            .update(cx, |this, cx| this.sidebar_event(SidebarEvent::DropTab, window, cx))
                            .ok();
                    }
                });
            },
        )
        .absolute()
        .size_full()
    };

    let tab_menu = props
        .tab_menu
        .as_ref()
        .map(|menu| render_tab_menu_layer(menu, props.tab_menu_anchor, theme, cx));

    div()
        .id("tab-bar")
        .debug_selector(|| "tab-bar".into())
        .relative()
        .flex()
        .flex_row()
        .flex_none()
        .items_start()
        .w_full()
        .h(px(TAB_BAR_H))
        .bg(bar_fill(theme))
        .border_b_1()
        .border_color(theme.divider)
        .text_color(theme.foreground)
        // The traffic lights' room, up to the first item: it moves the
        // window (the lights are native buttons over it).
        .child(
            window_drag::region("tab-bar-drag-lead")
                .debug_selector(|| "tab-bar-drag-lead".into())
                .flex_none()
                .w(px(LEAD_X))
                .h(px(TAB_BAR_H)),
        )
        .children(toggle)
        .child(
            // The tabs run the bar's full height, over its rule, so the
            // selected one covers it and joins the main area.
            div()
                .relative()
                .flex()
                .flex_row()
                .flex_1()
                .min_w_0()
                .h(px(TAB_BAR_H))
                .child(strip_origin)
                .children(strip),
        )
        .child(bar_probe)
        .child(follow)
        .children(tab_menu)
        .into_any_element()
}

/// One project's tab: its dot (an `×` while hovered), its name, and its ⌘N
/// while ⌘ is held; dragged, it moves to another place in the bar.
fn render_tab<H: SidebarHost>(
    tab: &Tab,
    look: TabLook,
    menu_anchor: &Anchor,
    layout: &SharedLayout,
    theme: &Theme,
    cx: &mut Context<H>,
) -> Stateful<Div> {
    let TabLook { hovered, hotkey, lifted } = look;
    let new_tab = tab.name == NEW_TAB;
    let key = selector_name(&tab.name).to_string();
    let selector = format!("tab-{key}");
    let name = tab.name.clone();
    let hover_name = tab.name.clone();
    let menu_name = tab.name.clone();
    let menu_anchor = menu_anchor.clone();
    let drag = TabDrag {
        project: tab.name.clone(),
        label: tab.label.clone(),
        working: tab.working,
    };
    let host = cx.entity().downgrade();
    let layout = layout.clone();
    let lead = if hovered {
        let close_selector = format!("tab-close-{key}");
        let close = tab.name.clone();
        let group = SharedString::from(close_selector.clone());
        let loud = theme.text_loud;
        let hover_fill = theme.background_hover;
        div()
            .id(SharedString::from(close_selector.clone()))
            .debug_selector(move || close_selector.clone())
            .group(group.clone())
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(ICON_SIZE_MENU)
            .rounded(px(4.0))
            .cursor_pointer()
            .hover(move |s| s.bg(hover_fill))
            .tooltip(style::tooltip(if new_tab { "Close Tab" } else { "Close Project" }))
            .child(
                heroicon(icons::X_MARK_MENU)
                    .text_color(theme.gray)
                    .group_hover(group, move |s| s.text_color(loud)),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.sidebar_event(SidebarEvent::CloseTab(close.clone()), window, cx)
            }))
            .into_any_element()
    } else {
        project_menu::dot(SharedString::from(format!("tab-dot-{key}")), indicator(&tab.name, tab.working), theme)
    };
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(TAB_GAP))
        .h(px(TAB_BAR_H))
        .min_w_0()
        .max_w(px(TAB_MAX_W))
        .pl(px(TAB_PAD_LEFT))
        .pr(px(TAB_PAD_RIGHT))
        .overflow_hidden()
        .cursor_pointer()
        .when(tab.selected, |d| d.bg(theme.canvas))
        // Lifted: its slot draws empty (the ghost has it).
        .when(lifted, |d| d.opacity(0.0))
        .child(lead)
        .child(
            div()
                .min_w_0()
                .truncate()
                .type_style(Type::BaseMedium)
                .text_color(theme.foreground)
                .child(tab.label.clone()),
        )
        .children(hotkey.map(|h| {
            let badge = format!("tab-hotkey-{key}");
            style::hotkey(&h, theme).px_0().debug_selector(move || badge)
        }))
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            this.sidebar_event(SidebarEvent::HoverTab(hover_name.clone(), *hovered), window, cx)
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::SwitchTo(name.clone()), window, cx)
        }))
        // Its context menu, at the pointer (a project's: the New tab has
        // no project to close).
        .when(!new_tab, |d| {
            d.on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    menu_anchor.set(Some(Bounds::new(ev.position, Size::default())));
                    this.sidebar_event(SidebarEvent::OpenTabMenu(menu_name.clone()), window, cx)
                }),
            )
        })
        // A drag (past gpui's threshold, so a click stays a click) carries
        // the tab; the ghost starts where the tab sits under the pointer.
        .on_drag(drag, move |drag: &TabDrag, offset, window, cx: &mut App| {
            let (label, indicator) = (drag.label.clone(), indicator(&drag.project, drag.working));
            let tab_w = (layout.borrow().width(&drag.project) - RULE_W).max(0.0);
            let ghost = cx.new(|_| TabGhost { label, indicator, tab_w });
            {
                let mut layout = layout.borrow_mut();
                layout.grab_x = offset.x.as_f32();
                layout.grab_y = offset.y.as_f32();
                let at = window.mouse_position() - offset;
                layout.tab_origin = (at.x.as_f32(), at.y.as_f32());
                layout.ghost = Some(ghost.downgrade());
            }
            let project = drag.project.clone();
            host.update(cx, |this, cx| this.sidebar_event(SidebarEvent::DragTab(project), window, cx))
                .ok();
            ghost
        })
}

/// A tab's context menu at the pointer, drawn over everything
/// (`deferred`); while open, a press outside it closes it and goes no
/// further.
fn render_tab_menu_layer<H: SidebarHost>(
    menu: &TabMenuProps<'_>,
    anchor: &Anchor,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let body = div()
        .relative()
        .top(px((1.0 - menu.phase) * motion::SLIDE_PX))
        .opacity(menu.phase)
        .flex()
        .flex_col()
        .min_h_0()
        .w(px(MENU_W))
        .child(render_tab_menu(menu, theme, cx));
    let placed = placed(anchor, body).gap(CONTEXT_GAP);
    let placed = if menu.open {
        placed.on_dismiss(cx.listener(|this, _: &MouseDownEvent, window, cx| {
            this.sidebar_event(SidebarEvent::CloseTabMenu, window, cx)
        }))
    } else {
        placed
    };
    deferred(placed).with_priority(1)
}

/// The context menu's rows: Close Project, then Close All Other Projects
/// (dimmed while there is no other tab), on the menus' raised fill.
fn render_tab_menu<H: SidebarHost>(menu: &TabMenuProps<'_>, theme: &Theme, cx: &mut Context<H>) -> impl IntoElement {
    let rows = TabMenuRow::ALL.into_iter().map(|row| {
        let id = SharedString::from(row.selector());
        if row == TabMenuRow::CloseOthers && !menu.can_close_others {
            return project_menu::disabled_text_row(id.clone(), id, row.label(), theme).into_any_element();
        }
        let project = menu.project.to_string();
        project_menu::text_row(id.clone(), id, row.label(), menu.highlight == Some(row), theme)
            .on_click(cx.listener(move |this, _, window, cx| {
                let event = match row {
                    TabMenuRow::CloseProject => SidebarEvent::CloseTab(project.clone()),
                    TabMenuRow::CloseOthers => SidebarEvent::CloseOtherTabs(project.clone()),
                };
                this.sidebar_event(event, window, cx)
            }))
            .into_any_element()
    });
    div()
        .id("tab-context-menu")
        .debug_selector(|| "tab-context-menu".into())
        .flex()
        .flex_col()
        .px_1()
        .py_1()
        .rounded(px(NAV_RADIUS))
        .bg(theme.raised.blend(theme.search))
        .text_color(theme.foreground)
        .occlude()
        .children(rows)
}

/// The `+` tab: 52pt square, Heroicons `plus`, opening a New tab (or
/// selecting the window's).
fn plus_tab<H: SidebarHost>(theme: &Theme, cx: &mut Context<H>) -> Stateful<Div> {
    let loud = theme.text_loud;
    let hover_fill = theme.background_hover;
    div()
        .id("tab-plus")
        .debug_selector(|| "tab-plus".into())
        .group("tab-plus")
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(TAB_BAR_H))
        .cursor_pointer()
        .hover(move |s| s.bg(hover_fill))
        .tooltip(style::tooltip("New tab"))
        .child(
            heroicon(icons::PLUS)
                .text_color(theme.icon)
                .group_hover("tab-plus", move |s| s.text_color(loud)),
        )
        .on_click(cx.listener(|this, _, window, cx| this.sidebar_event(SidebarEvent::NewTab, window, cx)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tabs_keep_figmas_gap_past_the_lights() {
        assert_eq!(LEAD_X, 96.0, "the real lights end at 79, Figma's 17pt past them");
    }

    #[test]
    fn the_bar_is_the_overlay_fill_over_the_canvas() {
        let dark = Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark);
        let fill = bar_fill(&dark).to_rgb();
        // Figma's slate/700 (#373e53), within the overlay's rounding.
        let (r, g, b) = (fill.r * 255.0, fill.g * 255.0, fill.b * 255.0);
        assert!((r - 55.0).abs() < 3.0 && (g - 62.0).abs() < 3.0 && (b - 83.0).abs() < 8.0, "{r} {g} {b}");
        let light = Theme::resolve(shelbi_state::ThemePreference::Light, gpui::WindowAppearance::Light);
        assert_ne!(bar_fill(&light), light.canvas, "light gets a gray bar under a white selected tab");
    }

    #[test]
    fn the_tab_menu_steps_over_the_rows_it_can_run() {
        let mut m = TabMenu::default();
        let now = Instant::now();
        m.open("alpha", now);
        assert!(m.is_open());
        assert_eq!(m.project(), Some("alpha"));
        assert_eq!(m.highlight(), None, "a right-click lights no row");
        m.step(1, |_| true);
        assert_eq!(m.highlight(), Some(TabMenuRow::CloseProject));
        m.step(1, |_| true);
        assert_eq!(m.highlight(), Some(TabMenuRow::CloseOthers));
        m.step(1, |_| true);
        assert_eq!(m.highlight(), Some(TabMenuRow::CloseProject), "wraps");
        // The only tab: Close All Other Projects is skipped.
        let only = |r: TabMenuRow| r != TabMenuRow::CloseOthers;
        m.step(1, only);
        assert_eq!(m.highlight(), Some(TabMenuRow::CloseProject));
        m.close(now);
        assert!(!m.is_open());
        assert_eq!(m.project(), Some("alpha"), "kept while it fades");
        assert_eq!(m.phase(now, true), None, "closed at once under Reduce Motion");
    }

    #[test]
    fn the_new_tab_wears_a_gray_ring() {
        assert_eq!(indicator(NEW_TAB, true), ProjectIndicator::Unloaded, "no project, no status");
        assert_eq!(indicator("web", false), ProjectIndicator::LoadedIdle);
        assert_eq!(indicator("web", true), ProjectIndicator::Working);
        assert_eq!(selector_name(NEW_TAB), "new-tab");
    }
}
