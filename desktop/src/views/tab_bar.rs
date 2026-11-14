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
//! The `+` tab opens the project menu under it: the projects that aren't open
//! (the shared [`project_menu`] list, gray rings), a rule, then
//! "Add project". With every project open, only "Add project".
//!
//! Right-clicking a project's tab (never the `+`) opens its context menu at
//! the pointer ([`TabMenu`]): "Close Project", closing that tab's project as
//! its `×` does, and "Close All Other Projects", closing every other tab's
//! after one confirmation (dimmed while it's the only tab). It closes on a
//! press outside it or Esc.
//!
//! The bar stays where it is while the sidebar hides and shows; only the
//! tabs step right to make room for the toggle.

use std::time::Instant;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Bounds, Context, Div, Hsla, MouseButton, MouseDownEvent,
    SharedString, Size, Stateful,
};
use shelbi_app::overlay::palette::ProjectEntry;

use super::project_menu;
use super::sidebar::{self, SidebarEvent, SidebarHost, TOOLBAR_BUTTON, TRAFFIC_LIGHTS_RIGHT};
use crate::assets::{heroicon, icons, ICON_SIZE_MENU};
use crate::elements::placed::{placed, MenuScroll};
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

/// The project menu's width and its gap under the `+` tab.
const MENU_W: f32 = 240.0;
const MENU_GAP: f32 = 4.0;
/// A tab's context menu's gap under the pointer.
const CONTEXT_GAP: f32 = 2.0;

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
    /// The project menu while it shows (opening, open or closing).
    pub menu: Option<ProjectMenuProps<'a>>,
    /// Where the `+` tab lands, for the menu's placement.
    pub plus: &'a Anchor,
    /// The menu is open (the `+` holds its pressed fill).
    pub menu_open: bool,
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

/// The project menu, as one frame draws it.
pub struct ProjectMenuProps<'a> {
    /// The projects without a tab, each with its (gray) dot.
    pub projects: &'a [ProjectEntry],
    /// The keyboard's row: an index into the projects, or `projects.len()`
    /// for "Add project".
    pub highlight: usize,
    /// How shown the menu is: 0 (hidden) to 1, eased.
    pub phase: f32,
    /// Open, not fading out after closing: only an open menu takes the
    /// window's presses to close.
    pub open: bool,
    /// The menu's scroll, keeping the keyboard's row in view.
    pub scroll: MenuScroll,
}

/// The project menu's state, owned by the window: open or closed, when that
/// last changed (for the slide-and-fade), and the keyboard's row.
#[derive(Debug, Default)]
pub struct ProjectMenu {
    open: bool,
    changed: Option<Instant>,
    pub highlight: usize,
    /// The `+` tab's bounds as last painted.
    pub button: Anchor,
    /// The menu's scroll (capped where the window is short).
    pub scroll: MenuScroll,
}

impl ProjectMenu {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open with the keyboard on `highlight`.
    pub fn open(&mut self, highlight: usize, now: Instant) {
        if !self.open {
            self.open = true;
            self.changed = Some(now);
            // Scrolled to the keyboard's row as it opens.
            self.scroll.follow(None);
        }
        self.highlight = highlight;
    }

    pub fn close(&mut self, now: Instant) {
        if self.open {
            self.open = false;
            self.changed = Some(now);
        }
    }

    /// Move the keyboard's row by `delta` among `count` rows, wrapping.
    pub fn step(&mut self, delta: isize, count: usize) {
        if count > 0 {
            self.highlight = (self.highlight as isize + delta).rem_euclid(count as isize) as usize;
        }
    }

    /// How shown the menu is at `now` (0..=1 on Taskpage's curve), `None`
    /// once it has finished closing. Instant under `reduce_motion`.
    pub fn phase(&self, now: Instant, reduce_motion: bool) -> Option<f32> {
        menu_phase(self.open, self.changed, now, reduce_motion)
    }

    /// Whether the menu is mid-transition at `now` (the window keeps
    /// drawing frames).
    pub fn animating(&self, now: Instant, reduce_motion: bool) -> bool {
        menu_animating(self.changed, now, reduce_motion)
    }
}

/// The bar's fill: `slate/700` in the Dark frame, which is the overlay fill
/// (`button/default/background`) laid over the main area's color, so Light
/// gets its own light gray the same way.
pub fn bar_fill(theme: &Theme) -> Hsla {
    theme.canvas.blend(theme.background)
}

/// A 1pt full-height rule between tabs.
fn rule(theme: &Theme) -> Div {
    div().flex_none().w(px(1.0)).h(px(TAB_BAR_H)).bg(theme.divider)
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

    let mut strip: Vec<AnyElement> = vec![rule(theme).into_any_element()];
    for (ix, tab) in props.tabs.iter().enumerate() {
        let hovered = props.hovered == Some(tab.name.as_str());
        let hotkey = (props.command_held && ix < 9).then(|| tab_hotkey(ix as u8 + 1));
        strip.push(render_tab(tab, hovered, hotkey, props.tab_menu_anchor, theme, cx).into_any_element());
        strip.push(rule(theme).into_any_element());
    }
    strip.push(plus_tab(props.plus, props.menu_open, theme, cx).into_any_element());
    strip.push(rule(theme).into_any_element());

    let menu = props
        .menu
        .as_ref()
        .map(|menu| render_menu_layer(menu, props.plus, theme, cx));
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
        .pl(px(LEAD_X))
        .bg(bar_fill(theme))
        .border_b_1()
        .border_color(theme.divider)
        .text_color(theme.foreground)
        .children(toggle)
        .child(
            // The tabs run the bar's full height, over its rule, so the
            // selected one covers it and joins the main area.
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_w_0()
                .h(px(TAB_BAR_H))
                .children(strip),
        )
        .children(menu)
        .children(tab_menu)
        .into_any_element()
}

/// One project's tab: its dot (an `×` while hovered), its name, and its ⌘N
/// while ⌘ is held.
fn render_tab<H: SidebarHost>(
    tab: &Tab,
    hovered: bool,
    hotkey: Option<String>,
    menu_anchor: &Anchor,
    theme: &Theme,
    cx: &mut Context<H>,
) -> Stateful<Div> {
    let selector = format!("tab-{}", tab.name);
    let name = tab.name.clone();
    let hover_name = tab.name.clone();
    let menu_name = tab.name.clone();
    let menu_anchor = menu_anchor.clone();
    let lead = if hovered {
        let close_selector = format!("tab-close-{}", tab.name);
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
            .tooltip(style::tooltip("Close Project"))
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
        let indicator = shelbi_app::overlay::palette::project_indicator(true, tab.working);
        project_menu::dot(SharedString::from(format!("tab-dot-{}", tab.name)), indicator, theme)
    };
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
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
            let badge = format!("tab-hotkey-{}", tab.name);
            style::hotkey(&h, theme).px_0().debug_selector(move || badge)
        }))
        .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
            this.sidebar_event(SidebarEvent::HoverTab(hover_name.clone(), *hovered), window, cx)
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::SwitchTo(name.clone()), window, cx)
        }))
        // Its context menu, at the pointer.
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                menu_anchor.set(Some(Bounds::new(ev.position, Size::default())));
                this.sidebar_event(SidebarEvent::OpenTabMenu(menu_name.clone()), window, cx)
            }),
        )
}

/// A tab's context menu at the pointer, drawn over everything as the
/// project menu is; while open, a press outside it closes it and goes no
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
/// (dimmed while there is no other tab), on the project menu's fill.
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

/// The `+` tab: 52pt square, Heroicons `plus`, opening the project menu;
/// it records where it lands into `anchor` (for the menu's placement).
fn plus_tab<H: SidebarHost>(anchor: &Anchor, open: bool, theme: &Theme, cx: &mut Context<H>) -> Stateful<Div> {
    let probe = {
        let cell = anchor.clone();
        gpui::canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full()
    };
    let loud = theme.text_loud;
    let hover_fill = theme.background_hover;
    let rest = if open { theme.background_active } else { theme.background.opacity(0.0) };
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
        .bg(rest)
        .hover(move |s| s.bg(hover_fill))
        .tooltip(style::tooltip("Open a project"))
        .child(probe)
        .child(
            heroicon(icons::PLUS)
                .text_color(if open { loud } else { theme.icon })
                .group_hover("tab-plus", move |s| s.text_color(loud)),
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.sidebar_event(SidebarEvent::ToggleSwitcher, window, cx)
        }))
}

/// The menu floating [`MENU_GAP`] under the `+` tab, its left edge on the
/// tab's: drawn after everything (`deferred`), so nothing below it moves,
/// capped and scrolling where the window is short. While open, a press
/// outside it (the `+` included, so a second click closes it) closes it and
/// goes no further.
fn render_menu_layer<H: SidebarHost>(
    menu: &ProjectMenuProps<'_>,
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
        .child(render_menu(menu, theme, cx));
    let placed = placed(anchor, body).gap(MENU_GAP);
    let placed = if menu.open {
        placed.on_dismiss(cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
            this.sidebar_event(SidebarEvent::CloseSwitcher, window, cx)
        }))
    } else {
        placed
    };
    deferred(placed).with_priority(1)
}

/// The menu (the Figma `Project switcher`'s open state, 2186:1692): the
/// "Projects" heading over a row per project that isn't open, a rule, then
/// "+ Add project". Only "Add project" when every project is open. The
/// keyboard's row wears the selection fill.
fn render_menu<H: SidebarHost>(menu: &ProjectMenuProps<'_>, theme: &Theme, cx: &mut Context<H>) -> impl IntoElement {
    let rows = menu.projects.iter().enumerate().map(|(ix, p)| {
        let name = p.slug.clone();
        project_menu::project_row(
            SharedString::from(format!("tab-menu-project-{}", p.slug)),
            format!("tab-menu-project-{}", p.slug),
            p,
            menu.highlight == ix,
            theme,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::SwitchTo(name.clone()), window, cx)
        }))
    });
    let add_ix = menu.projects.len();
    let listed = !menu.projects.is_empty();
    // The keyboard's row among the menu's children: after the heading, and
    // "Add project" after the rule.
    let child = if menu.highlight < add_ix {
        1 + menu.highlight
    } else if listed {
        add_ix + 2
    } else {
        0
    };
    menu.scroll.follow(Some(child));
    let add = project_menu::add_row("tab-menu-add", "tab-menu-add", menu.highlight == add_ix, theme)
        .on_click(cx.listener(|this, _, window, cx| {
            this.sidebar_event(SidebarEvent::AddProject, window, cx)
        }));
    div()
        .id("tab-menu")
        .debug_selector(|| "tab-menu".into())
        .flex()
        .flex_col()
        .px_1()
        .py_2()
        .overflow_y_scroll()
        .track_scroll(menu.scroll.handle())
        .rounded(px(NAV_RADIUS))
        .bg(theme.raised.blend(theme.search))
        .text_color(theme.foreground)
        .occlude()
        .when(listed, |d| d.child(project_menu::heading(theme)))
        .children(rows)
        .when(listed, |d| d.child(project_menu::divider(theme)))
        .child(add)
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
    fn the_menu_steps_and_wraps() {
        let mut m = ProjectMenu::default();
        let now = Instant::now();
        m.open(0, now);
        assert!(m.is_open());
        m.step(-1, 3);
        assert_eq!(m.highlight, 2);
        m.step(1, 3);
        assert_eq!(m.highlight, 0);
        m.close(now);
        assert!(!m.is_open());
        assert_eq!(m.phase(now, true), None, "closed at once under Reduce Motion");
    }
}
