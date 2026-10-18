//! The sidebar, rendered from `shelbi_app::view::SidebarModel` only.
//!
//! Item for item it is the TUI's sidebar (`crates/shelbi-tui/src/shell/
//! sidebar.rs`): the rows, their order and their labels come from the shared
//! [`SidebarModel::rows`] layout, and the state glyphs from the shared
//! [`WorkspaceBadge`] / [`ReviewState`] decorations, so the two clients can't
//! drift. Top to bottom: the project title (which opens the project switcher)
//! and the search box, the nav (Orchestrator / Issues / Activity), the
//! machine-grouped workspaces with their state badges, the Ready for Review
//! and Queued for Review sections (two-line entries with the `⎇ branch` line),
//! then the footer: the daemon / CLI version row, the polling indicator while
//! the daemon is unreachable, the `ZEN MODE ON` band, the unread-errors
//! button, and the settings and collapse buttons.
//!
//! The one deliberate difference from the TUI: the nav rows' icons are
//! Heroicons rather than the TUI's emoji (the desktop's icon rule).
//!
//! The width is the shared nav model's, in TUI columns ([`COLUMN_PX`] each),
//! clamped by `shelbi_app::nav::clamp_sidebar_width`.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Bounds, Context, FontWeight, Hsla, Pixels,
    ScrollHandle, SharedString, Window,
};
use shelbi_app::nav::{clamp_sidebar_width, View};
use shelbi_app::view::{ReviewState, SidebarModel, SidebarRow, WorkspaceBadge};
use shelbi_state::ZenModeState;

use crate::assets::{heroicon, icons, Icon, ICON_SIZE};
use crate::projects::ProjectEntry;
use crate::refresh::DaemonLink;
use crate::theme::motion::{self, SharedTrack};
use crate::theme::style::{self, IconButtonSize, LIST_ROW_H, NAV_ROW_H};
use crate::theme::Theme;
use crate::theme::style::{Type, Typography};

/// Width of one TUI column in the window, so the shared nav model's widths
/// (default 28, minimum 24) and its clamp apply unchanged.
pub const COLUMN_PX: f32 = 9.0;

/// Room left above the title for the macOS traffic lights.
pub const TRAFFIC_LIGHT_INSET: f32 = 38.0;

/// A section heading's row (Taskpage's `Nav.Heading`: 20px above, 4px
/// below).
const SECTION_H: f32 = 32.0;

/// The sidebar's displayed width in pixels: the user's chosen `width_cols`,
/// clamped to the window (at least 24 columns, at most half the window).
pub fn width_px(width_cols: u16, window_px: f32) -> f32 {
    let window_cols = (window_px / COLUMN_PX).max(0.0) as u16;
    f32::from(clamp_sidebar_width(width_cols, window_cols)) * COLUMN_PX
}

/// The width, in columns, a drag of the right edge to `pointer_x` asks for,
/// clamped to the window by the same rule.
pub fn drag_width_cols(pointer_x: f32, window_px: f32) -> u16 {
    let desired = (pointer_x / COLUMN_PX).round().clamp(0.0, f32::from(u16::MAX)) as u16;
    clamp_sidebar_width(desired, (window_px / COLUMN_PX).max(0.0) as u16)
}

/// What the user did in the sidebar. The host (the window) acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    /// Open the `index`-th selectable row (selecting it first).
    Activate(usize),
    /// The search box: opens the command palette.
    OpenSearch,
    /// Show or hide the project switcher.
    ToggleSwitcher,
    /// Close the switcher (a click outside it).
    CloseSwitcher,
    /// Switch to (open or focus the window of) a project.
    SwitchTo(String),
    /// The switcher's "Add project…".
    AddProject,
    /// The unread-errors button.
    OpenErrors,
    OpenSettings,
    Collapse,
}

/// The window the sidebar is drawn into.
pub trait SidebarHost: 'static + Sized {
    fn sidebar_event(&mut self, event: SidebarEvent, window: &mut Window, cx: &mut Context<Self>);
}

/// Everything one sidebar frame draws. All of it is in memory: rendering
/// never reads state.
pub struct SidebarProps<'a> {
    pub model: &'a SidebarModel,
    /// The selectable-row ordinal (`ClientState::sidebar_selection`).
    pub selection: usize,
    /// Keyboard focus is in the sidebar (brightens the selected nav label).
    pub focused: bool,
    pub link: Option<DaemonLink>,
    /// The palette-open chord shown in the search box (`⌃P`).
    pub palette_chord: String,
    /// The project switcher's menu while it shows (opening, open or
    /// closing).
    pub switcher: Option<SwitcherProps<'a>>,
    /// Where the switcher's button lands, for the menu's outside click.
    pub switcher_button: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The switcher is open (its button holds its pressed fill).
    pub switcher_open: bool,
    pub width: f32,
    /// The rows' scroll, owned by the window so it outlives frames the
    /// sidebar isn't drawn in (a review panel standing in for it).
    pub scroll: ScrollHandle,
    /// The selectable rows, measured for the selection chip's slide.
    pub track: SharedTrack,
    /// The sliding selection chip's bounds while it moves between rows.
    pub chip: Option<Bounds<Pixels>>,
}

/// The switcher menu, as one frame draws it.
pub struct SwitcherProps<'a> {
    /// Every project, most recently launched first.
    pub projects: &'a [ProjectEntry],
    /// The current project's id.
    pub current: &'a str,
    /// The keyboard's row: an index into the projects, or `projects.len()`
    /// for "Add project…".
    pub highlight: usize,
    /// How shown the menu is: 0 (hidden) to 1, eased.
    pub phase: f32,
}

/// The switcher button's height (ContextStore's `SpacePicker`).
pub const SWITCHER_H: f32 = 36.0;
/// The menu's gap below the button.
const MENU_GAP: f32 = 6.0;

/// The project switcher's state, owned by the window: open or closed, when
/// that last changed (for the slide-and-fade), and the keyboard's row.
#[derive(Debug, Default)]
pub struct Switcher {
    open: bool,
    changed: Option<Instant>,
    pub highlight: usize,
    /// The button's bounds as last painted.
    pub button: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Switcher {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open with the keyboard on `highlight` (the current project).
    pub fn open(&mut self, highlight: usize, now: Instant) {
        if !self.open {
            self.open = true;
            self.changed = Some(now);
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
        let t = match self.changed {
            Some(at) if !reduce_motion => {
                now.saturating_duration_since(at).as_secs_f32() / motion::MENU_IN.as_secs_f32()
            }
            _ => 1.0,
        };
        let eased = motion::ease_out_expo(t.min(1.0));
        if self.open {
            Some(eased)
        } else if t < 1.0 {
            Some(1.0 - eased)
        } else {
            None
        }
    }

    /// Whether the menu is mid-transition at `now` (the window keeps
    /// drawing frames).
    pub fn animating(&self, now: Instant, reduce_motion: bool) -> bool {
        !reduce_motion
            && self
                .changed
                .is_some_and(|at| now.saturating_duration_since(at) < motion::MENU_IN)
    }
}

/// One row's text, as the TUI draws it: the badge glyph and its color, the
/// primary label, the right-aligned label, and a review's branch line.
#[derive(Debug, Clone, PartialEq)]
pub struct RowText {
    pub badge: Option<(String, Hsla)>,
    pub label: String,
    pub right: Option<String>,
    pub branch: Option<String>,
}

/// The text of a non-nav row. Pure, so its labels and glyphs are tested
/// against the TUI's.
pub fn row_text(row: &SidebarRow, theme: &Theme) -> RowText {
    let plain = |label: String| RowText {
        badge: None,
        label,
        right: None,
        branch: None,
    };
    match row {
        SidebarRow::Nav { label, .. } | SidebarRow::Section(label) => plain(label.clone()),
        SidebarRow::ConfigError(message) => plain(format!("! {message}")),
        SidebarRow::Blank => plain(String::new()),
        SidebarRow::Loading => plain("Loading…".into()),
        SidebarRow::MachineGroup {
            name,
            collapsed,
            total,
            active,
        } => RowText {
            badge: None,
            label: format!("{} {name}", if *collapsed { "▸" } else { "▾" }),
            right: collapsed.then(|| format!("({total}, {active} active)")),
            branch: None,
        },
        SidebarRow::Workspace {
            name,
            badge,
            right_label,
            ..
        } => RowText {
            badge: Some((
                badge.glyph().to_string(),
                theme.decoration(badge.decoration_color()),
            )),
            label: name.clone(),
            right: (!right_label.is_empty()).then(|| right_label.clone()),
            branch: None,
        },
        SidebarRow::Review {
            title,
            branch,
            location,
            state,
            ..
        } => {
            let dec = state.decoration();
            RowText {
                badge: Some((dec.glyph, theme.decoration(dec.color))),
                label: title.clone(),
                right: location.clone(),
                branch: Some(format!("⎇ {branch}")),
            }
        }
    }
}

/// The Heroicon for a nav view.
pub fn nav_icon(view: &View) -> Icon {
    match view {
        View::Session(_) => icons::CHAT_BUBBLE_LEFT_RIGHT,
        View::Issues => icons::VIEW_COLUMNS,
        View::Activity => icons::BOLT,
        View::Machines => icons::SERVER_STACK,
    }
}

/// A stable debug-selector name for a row, for tests and the inspector.
pub fn row_selector(row: &SidebarRow) -> String {
    match row {
        SidebarRow::Nav { label, .. } => format!("sidebar-nav-{label}"),
        SidebarRow::Section(label) => format!("sidebar-section-{label}"),
        SidebarRow::ConfigError(_) => "sidebar-config-error".into(),
        SidebarRow::Blank => "sidebar-blank".into(),
        SidebarRow::Loading => "sidebar-loading".into(),
        SidebarRow::MachineGroup { name, .. } => format!("sidebar-machine-{name}"),
        SidebarRow::Workspace { name, .. } => format!("sidebar-ws-{name}"),
        SidebarRow::Review { task_id, .. } => format!("sidebar-review-{task_id}"),
    }
}

/// Render the sidebar.
pub fn render<H: SidebarHost>(
    props: SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let rows = props.model.rows();
    let nav_n = rows
        .iter()
        .take_while(|r| matches!(r, SidebarRow::Nav { .. }))
        .count();
    // Selectable ordinal for each row.
    let mut ordinal = 0usize;
    let ordinals: Vec<Option<usize>> = rows
        .iter()
        .map(|r| {
            r.is_selectable().then(|| {
                ordinal += 1;
                ordinal - 1
            })
        })
        .collect();

    let nav = rows[..nav_n]
        .iter()
        .zip(&ordinals)
        .map(|(row, ord)| render_nav_row(row, *ord, &props, theme, cx))
        .collect::<Vec<_>>();
    // While the selection chip slides it is drawn here, under every row.
    let chip = props
        .chip
        .map(|bounds| {
            motion::floating_chip(bounds, |d| {
                style::nav_chip_layer(d, theme)
                    .when(props.focused, |d| d.child(style::selection_indicator(theme)))
            })
        });
    let rest = rows[nav_n..]
        .iter()
        .zip(&ordinals[nav_n..])
        .enumerate()
        .map(|(i, (row, ord))| render_row(i, row, *ord, &props, theme, cx))
        .collect::<Vec<_>>();

    div()
        .id("sidebar")
        .debug_selector(|| "sidebar".into())
        .flex()
        .flex_col()
        .w(px(props.width))
        .h_full()
        .flex_none()
        .relative()
        .bg(theme.sidebar)
        .text_color(theme.foreground)
        .type_style(Type::Base)
        .child(motion::origin_probe(&props.track))
        .children(chip)
        .child(render_header(&props, theme, cx))
        .child(div().flex().flex_col().px_2().pt(px(12.0)).children(nav))
        .child(
            div()
                .id("sidebar-rest")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .px_2()
                .pt_2()
                .overflow_y_scroll()
                .track_scroll(&props.scroll)
                .children(rest),
        )
        .child(render_footer(&props, theme, cx))
        .into_any_element()
}

fn render_header<H: SidebarHost>(
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let open = props.switcher_open;
    let button_probe = {
        let cell = props.switcher_button.clone();
        gpui::canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full()
    };
    let label = props.model.project_label.clone();
    // ContextStore's `SpacePicker`: the badge, the name and the up-down
    // chevron on a quiet fill that strengthens on hover and while open.
    let rest = if open {
        theme.background_active
    } else {
        theme.background
    };
    let button = div()
        .id("sidebar-project-title")
        .debug_selector(|| "sidebar-project-title".into())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(SWITCHER_H))
        .rounded_lg()
        .border_1()
        .border_color(if open { theme.divider_strong } else { theme.divider })
        .cursor_pointer();
    let button = motion::fill_layer(
        button,
        "sidebar-project-title",
        px(7.0),
        [rest, theme.background_hover, theme.background_active],
    )
    .child(button_probe)
    .child(style::project_badge(&label, 24.0))
    .child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .type_style(Type::SmallMedium)
            .text_color(theme.text_loud)
            .child(label.clone()),
    )
    .child(heroicon(icons::CHEVRON_UP_DOWN).text_color(if open { theme.text_loud } else { theme.muted }))
    .on_click(cx.listener(|this, _, window, cx| {
        this.sidebar_event(SidebarEvent::ToggleSwitcher, window, cx)
    }));

    let search = style::search_box("sidebar-search", &props.palette_chord, theme)
        .mt_2()
        .on_click(cx.listener(|this, _, window, cx| {
            this.sidebar_event(SidebarEvent::OpenSearch, window, cx)
        }));

    // The menu floats over the sidebar, 6px under the button and at least
    // its width: drawn after everything (`deferred`), so nothing below it
    // moves and nothing paints over it.
    let menu = props.switcher.as_ref().map(|switcher| {
        let top = TRAFFIC_LIGHT_INSET + SWITCHER_H + MENU_GAP;
        deferred(
            div()
                .absolute()
                .top(px(top + (1.0 - switcher.phase) * motion::SLIDE_PX))
                .left(px(8.0))
                .min_w(px((props.width - 16.0).max(0.0)))
                .opacity(switcher.phase)
                .child(render_switcher(switcher, props.switcher_button.clone(), theme, cx)),
        )
        .with_priority(1)
    });

    div()
        .relative()
        .flex()
        .flex_col()
        .px_2()
        .pt(px(TRAFFIC_LIGHT_INSET))
        .child(button)
        .child(search)
        .children(menu)
}

/// The switcher's menu: a row per project (its badge, its name, a check on
/// the current one), a rule, then "Add project…". The keyboard's row wears
/// the selection fill.
fn render_switcher<H: SidebarHost>(
    switcher: &SwitcherProps<'_>,
    button: Rc<Cell<Option<Bounds<Pixels>>>>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let rows = switcher.projects.iter().enumerate().map(|(ix, p)| {
        let is_current = p.name == switcher.current;
        let name = p.name.clone();
        style::menu_row(
            SharedString::from(format!("sidebar-project-{}", p.name)),
            switcher.highlight == ix,
            theme,
        )
        .child(style::project_badge(&p.label, 24.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(is_current, |d| d.font_weight(FontWeight::MEDIUM).text_color(theme.text_loud))
                .child(p.label.clone()),
        )
        .child(div().w(ICON_SIZE).flex_none().when(is_current, |d| {
            d.child(heroicon(icons::CHECK).text_color(theme.accent))
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::SwitchTo(name.clone()), window, cx)
        }))
    });
    let add_ix = switcher.projects.len();
    let add = style::menu_row("sidebar-add-project", switcher.highlight == add_ix, theme)
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.0))
                .flex_none()
                .child(heroicon(icons::PLUS).text_color(theme.icon)),
        )
        .child(div().flex_1().child("Add project…"))
        .on_click(cx.listener(|this, _, window, cx| {
            this.sidebar_event(SidebarEvent::AddProject, window, cx)
        }));
    style::popover(
        div()
            .id("sidebar-switcher")
            .debug_selector(|| "sidebar-switcher".into())
            .flex()
            .flex_col()
            .p_1()
            .occlude(),
        theme,
    )
    .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
        // A press on the button toggles it closed through its click.
        if button.get().is_some_and(|b| b.contains(&event.position)) {
            return;
        }
        this.sidebar_event(SidebarEvent::CloseSwitcher, window, cx)
    }))
    .when(switcher.projects.is_empty(), |d| {
        d.child(
            div()
                .px_2()
                .h(px(LIST_ROW_H))
                .flex()
                .items_center()
                .text_color(theme.muted)
                .child("Loading projects…"),
        )
    })
    .children(rows)
    .child(div().my_1().mx(px(-4.0)).h(px(1.0)).bg(theme.divider))
    .child(add)
}

fn render_nav_row<H: SidebarHost>(
    row: &SidebarRow,
    ordinal: Option<usize>,
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let SidebarRow::Nav { label, view } = row else {
        return div().into_any_element();
    };
    let selected = ordinal == Some(props.selection);
    let selector = row_selector(row);
    let ord = ordinal.unwrap_or_default();
    // Taskpage's nav item: 40px, the chip inset 2px top and bottom, the
    // content 8px inside it.
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .my(px(2.0))
        .h(px(NAV_ROW_H - 4.0))
        .rounded_lg()
        .cursor_pointer()
        .text_color(theme.foreground)
        .map(|d| selected_row(d, &row_selector(row), selected, props, theme))
        .child(
            heroicon(nav_icon(view)).text_color(if selected { theme.text_loud } else { theme.icon }),
        )
        .child(label.clone())
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::Activate(ord), window, cx)
        }))
        .into_any_element()
}

/// Paint a row selected (the raised chip, the brand pill while the sidebar
/// has keyboard focus) or hoverable, and measure it for the chip's slide.
/// While the chip slides, the floating one stands in for the row's own.
fn selected_row(
    d: gpui::Stateful<gpui::Div>,
    key: &str,
    selected: bool,
    props: &SidebarProps<'_>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let inline = props.chip.is_none();
    style::row_state(d, key.to_string(), selected, selected && props.focused && inline, inline, theme)
        .child(motion::probe(&props.track, key.to_string()))
        .when(selected, |d| d.child(selection_marker()))
}

/// The selection chip's key for the selected row: its selector, so the chip
/// follows the item (not its ordinal) when rows come and go.
pub fn selected_key(model: &SidebarModel, selection: usize) -> Option<SharedString> {
    model
        .rows()
        .iter()
        .filter(|r| r.is_selectable())
        .nth(selection)
        .map(|r| row_selector(r).into())
}

/// A zero-size marker inside the selected row, so a test can find the
/// selection.
fn selection_marker() -> impl IntoElement {
    div()
        .debug_selector(|| "sidebar-selection".into())
        .absolute()
        .size_0()
}

fn render_row<H: SidebarHost>(
    index: usize,
    row: &SidebarRow,
    ordinal: Option<usize>,
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let selector = row_selector(row);
    let text = row_text(row, theme);
    let base = div()
        .id(("sidebar-row", index))
        .debug_selector({
            let selector = selector.clone();
            move || selector
        })
        .flex_none();
    match row {
        SidebarRow::Blank => base.h(px(8.0)).into_any_element(),
        SidebarRow::Section(_) => base
            .px_2()
            .pt(px(12.0))
            .pb(px(4.0))
            .h(px(SECTION_H))
            .flex()
            .items_end()
            .child(style::heading(text.label, theme))
            .into_any_element(),
        SidebarRow::Loading => base
            .px_2()
            .h(px(LIST_ROW_H))
            .flex()
            .items_center()
            .italic()
            .text_color(theme.muted)
            .child(text.label)
            .into_any_element(),
        SidebarRow::ConfigError(_) => base
            .px_2()
            .py_1()
            .text_color(theme.error)
            .child(text.label)
            .into_any_element(),
        SidebarRow::Nav { .. } => base.into_any_element(),
        SidebarRow::MachineGroup { .. }
        | SidebarRow::Workspace { .. }
        | SidebarRow::Review { .. } => {
            let selected = ordinal == Some(props.selection);
            let ord = ordinal.unwrap_or_default();
            let indent = matches!(row, SidebarRow::Workspace { indent: true, .. });
            let serving = matches!(
                row,
                SidebarRow::Review {
                    state: ReviewState::Serving,
                    ..
                }
            );
            let loading_badge = matches!(
                row,
                SidebarRow::Workspace {
                    badge: WorkspaceBadge::Loading,
                    ..
                }
            );
            let line = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(LIST_ROW_H - 4.0))
                .when(indent, |d| d.pl(px(2.0 * COLUMN_PX)))
                .when_some(text.badge, |d, (glyph, color)| {
                    d.child(
                        div()
                            .w(px(12.0))
                            .flex_none()
                            .text_color(color)
                            .when(serving, |b| b.font_weight(FontWeight::SEMIBOLD))
                            .when(loading_badge, |b| b.text_color(theme.muted))
                            .child(glyph),
                    )
                })
                .child(div().flex_1().min_w_0().truncate().child(text.label))
                .when_some(text.right, |d, right| {
                    d.child(
                        div()
                            .flex_none()
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child(right),
                    )
                });
            base.relative()
                .flex()
                .flex_col()
                .px_2()
                .my(px(2.0))
                .rounded_lg()
                .cursor_pointer()
                .type_style(Type::Small)
                .text_color(theme.foreground)
                .map(|d| selected_row(d, &selector, selected, props, theme))
                .child(line)
                .when_some(text.branch, |d, branch| {
                    d.child(
                        div()
                            .pl(px(2.0 * COLUMN_PX))
                            .pb_1p5()
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .truncate()
                            .child(branch),
                    )
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.sidebar_event(SidebarEvent::Activate(ord), window, cx)
                }))
                .into_any_element()
        }
    }
}

fn render_footer<H: SidebarHost>(
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let model = props.model;
    let version = model.daemon_version_line.clone().map(|line| {
        let color = if model.daemon_version_mismatch {
            theme.error
        } else {
            theme.muted
        };
        div().text_color(color).child(line)
    });
    let banner = model.board_banner.clone().map(|banner| {
        let stale = banner.contains("stale") || banner.contains("no daemon");
        div()
            .text_color(if stale { theme.warning } else { theme.muted })
            .child(banner)
    });
    let polling = (props.link == Some(DaemonLink::Polling)).then(|| {
        div()
            .debug_selector(|| "sidebar-polling".into())
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_color(theme.warning)
            .child(
                heroicon(icons::SIGNAL_SLASH).text_color(theme.warning),
            )
            .child("Daemon unreachable · polling")
    });
    let status = footer_status(model, theme).map(|(line, color)| {
        div()
            .debug_selector(|| "sidebar-status".into())
            .text_color(color)
            .child(line)
    });

    let icon_button = |id: &'static str, icon: Icon| {
        style::icon_button(id, icon, IconButtonSize::Large, false, theme)
    };
    let buttons = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .pb_2()
        .when(model.unread_errors > 0, |d| {
            d.child(
                div()
                    .id("sidebar-errors")
                    .debug_selector(|| "sidebar-errors".into())
                    .flex()
                    .items_center()
                    .justify_center()
                    // An icon button's square.
                    .size(px(IconButtonSize::Large.px()))
                    .rounded_xl()
                    .bg(theme.danger_fill)
                    .tooltip(style::tooltip(format!(
                        "{} unread error{}",
                        model.unread_errors,
                        if model.unread_errors == 1 { "" } else { "s" }
                    )))
                    .text_color(theme.error)
                    .type_style(Type::XSmallSemibold)
                    .cursor_pointer()
                    .child("!")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.sidebar_event(SidebarEvent::OpenErrors, window, cx)
                    })),
            )
        })
        .child(div().flex_1())
        .child(
            icon_button("sidebar-settings", icons::COG_6_TOOTH)
                .tooltip(style::tooltip("Settings"))
                .on_click(cx.listener(
                |this, _, window, cx| this.sidebar_event(SidebarEvent::OpenSettings, window, cx),
            )),
        )
        .child(
            icon_button("sidebar-collapse", icons::CHEVRON_DOUBLE_LEFT)
                .tooltip(style::tooltip("Hide the sidebar"))
                .on_click(cx.listener(
                |this, _, window, cx| this.sidebar_event(SidebarEvent::Collapse, window, cx),
            )),
        );

    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .pt_2()
        .type_style(Type::XSmall)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .children(status)
                .children(polling)
                .child(
                    div()
                        .debug_selector(|| "sidebar-version".into())
                        .flex()
                        .flex_col()
                        .children(version)
                        .children(banner),
                ),
        )
        // The zen row is always reserved, so the version line never moves
        // when Zen toggles; it paints only while Zen is on.
        .child(
            div()
                .h(px(24.0))
                .mx_2()
                .flex()
                .items_center()
                .px_2()
                .rounded_lg()
                .when(matches!(model.zen_mode, ZenModeState::On), |d| {
                    d.debug_selector(|| "sidebar-zen-band".into())
                        .bg(theme.zen)
                        .text_color(theme.success)
                        .type_style(Type::SmallMedium)
                        .child("ZEN MODE ON")
                }),
        )
        .child(buttons)
}

/// The footer's status row, worded as the TUI's
/// ([`SidebarModel::footer_status`]): the orchestrator supervisor's failure in
/// red while there is one, else the status line in yellow. `None` hides the
/// row.
fn footer_status(model: &SidebarModel, theme: &Theme) -> Option<(String, Hsla)> {
    let line = model.footer_status();
    if line.is_empty() {
        return None;
    }
    let color = if model.orchestrator_status.is_some() {
        theme.error
    } else {
        theme.warning
    };
    Some((line, color))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::view::{ReviewRow, WorkspaceRow};

    fn theme() -> Theme {
        Theme::resolve(
            shelbi_state::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        )
    }

    #[test]
    fn width_follows_the_shared_clamp() {
        // Default 28 columns in a wide window.
        assert_eq!(width_px(28, 1200.0), 28.0 * COLUMN_PX);
        // Never below the 24-column minimum...
        assert_eq!(width_px(10, 1200.0), 24.0 * COLUMN_PX);
        // ...and never past half the window.
        assert_eq!(width_px(100, 900.0), 50.0 * COLUMN_PX);
        // A drag maps the pointer to columns under the same rule.
        assert_eq!(drag_width_cols(300.0, 1200.0), 33);
        assert_eq!(drag_width_cols(50.0, 1200.0), 24);
        assert_eq!(drag_width_cols(1000.0, 1200.0), 66);
    }

    #[test]
    fn footer_status_shows_the_orchestrator_failure_in_red() {
        let t = theme();
        let mut model = SidebarModel::skeleton("demo");
        assert_eq!(footer_status(&model, &t), None, "no status, no row");
        model.status_line = "Indexing".into();
        assert_eq!(footer_status(&model, &t), Some(("Indexing".into(), t.warning)));
        // The supervisor's failure takes the row, in the TUI's words.
        model.orchestrator_status = Some("supervisor gave up: spawn refused".into());
        assert_eq!(
            footer_status(&model, &t),
            Some(("Orchestrator: supervisor gave up: spawn refused".into(), t.error))
        );
    }

    #[test]
    fn row_text_matches_the_tui_labels_and_glyphs() {
        let t = theme();
        let ws = SidebarRow::Workspace {
            name: "alpha".into(),
            badge: WorkspaceBadge::Working,
            right_label: "Developer".into(),
            indent: false,
        };
        let text = row_text(&ws, &t);
        assert_eq!(text.badge, Some(("⏵".into(), t.icon_green)));
        assert_eq!(text.label, "alpha");
        assert_eq!(text.right.as_deref(), Some("Developer"));

        let idle = SidebarRow::Workspace {
            name: "bravo".into(),
            badge: WorkspaceBadge::Idle,
            right_label: "idle".into(),
            indent: true,
        };
        assert_eq!(row_text(&idle, &t).badge, Some(("·".into(), t.muted)));

        for (badge, glyph) in [
            (WorkspaceBadge::AwaitingInput, "?"),
            (WorkspaceBadge::AwaitingPermission, "⚠"),
            (WorkspaceBadge::Paused, "⏸"),
            (WorkspaceBadge::Loading, "…"),
        ] {
            let row = SidebarRow::Workspace {
                name: "w".into(),
                badge,
                right_label: String::new(),
                indent: false,
            };
            let text = row_text(&row, &t);
            assert_eq!(text.badge.unwrap().0, glyph);
            assert_eq!(text.right, None, "an empty right label draws nothing");
        }

        let serving = SidebarRow::Review {
            task_id: "T-1".into(),
            title: "Fix it".into(),
            branch: "jlong/T-1".into(),
            location: Some("hub:review".into()),
            state: ReviewState::Serving,
        };
        let text = row_text(&serving, &t);
        assert_eq!(text.badge, Some(("✓".into(), t.icon_cyan)));
        assert_eq!(text.right.as_deref(), Some("hub:review"));
        assert_eq!(text.branch.as_deref(), Some("⎇ jlong/T-1"));

        let loading = SidebarRow::Review {
            task_id: "T-2".into(),
            title: "Next".into(),
            branch: "b".into(),
            location: None,
            state: ReviewState::Loading,
        };
        assert_eq!(row_text(&loading, &t).badge.unwrap().0, "▶");

        let collapsed = SidebarRow::MachineGroup {
            name: "devbox".into(),
            collapsed: true,
            total: 2,
            active: 1,
        };
        let text = row_text(&collapsed, &t);
        assert_eq!(text.label, "▸ devbox");
        assert_eq!(text.right.as_deref(), Some("(2, 1 active)"));
        let open = SidebarRow::MachineGroup {
            name: "hub".into(),
            collapsed: false,
            total: 1,
            active: 0,
        };
        assert_eq!(row_text(&open, &t).label, "▾ hub");
        assert_eq!(row_text(&open, &t).right, None);

        assert_eq!(row_text(&SidebarRow::Loading, &t).label, "Loading…");
        assert_eq!(
            row_text(&SidebarRow::ConfigError("bad yaml".into()), &t).label,
            "! bad yaml"
        );
    }

    #[test]
    fn nav_icons_are_heroicons() {
        assert_eq!(
            nav_icon(&View::Session("orch".into())),
            icons::CHAT_BUBBLE_LEFT_RIGHT
        );
        assert_eq!(nav_icon(&View::Issues), icons::VIEW_COLUMNS);
        assert_eq!(nav_icon(&View::Activity), icons::BOLT);
    }

    #[test]
    fn selectors_name_each_row() {
        let rows = {
            let mut m = SidebarModel::skeleton("demo");
            m.board_loading = false;
            m.workspaces = vec![WorkspaceRow {
                name: "alpha".into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                agent: None,
                badge: WorkspaceBadge::Idle,
            }];
            m.reviews = vec![ReviewRow {
                task_id: "T-1".into(),
                title: "t".into(),
                branch: "b".into(),
                location: None,
                workspace: None,
                state: ReviewState::Pending,
            }];
            m.rows()
        };
        let names: Vec<String> = rows.iter().map(row_selector).collect();
        assert!(names.contains(&"sidebar-nav-Issues".into()));
        assert!(names.contains(&"sidebar-ws-alpha".into()));
        assert!(names.contains(&"sidebar-review-T-1".into()));
        assert!(names.contains(&"sidebar-section-Queued for Review".into()));
    }
}

/// The sidebar rendered in a gpui test window from fixture models.
#[cfg(test)]
mod render_tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use shelbi_app::view::{ReviewRow, WorkspaceRow};

    /// A window that draws only the sidebar, recording what it asks for.
    struct Fixture {
        model: SidebarModel,
        selection: usize,
        link: Option<DaemonLink>,
        events: Vec<SidebarEvent>,
    }

    impl SidebarHost for Fixture {
        fn sidebar_event(&mut self, event: SidebarEvent, _: &mut Window, _: &mut Context<Self>) {
            self.events.push(event);
        }
    }

    impl Render for Fixture {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::resolve(
                shelbi_state::ThemePreference::Dark,
                gpui::WindowAppearance::Dark,
            );
            render(
                SidebarProps {
                    model: &self.model,
                    selection: self.selection,
                    focused: true,
                    link: self.link,
                    palette_chord: "⌃P".into(),
                    switcher: None,
                    switcher_button: Default::default(),
                    switcher_open: false,
                    width: 252.0,
                    scroll: gpui::ScrollHandle::new(),
                    track: motion::SlideTrack::shared(),
                    chip: None,
                },
                &theme,
                cx,
            )
        }
    }

    fn ws(name: &str, machine: &str, task: Option<&str>, badge: WorkspaceBadge) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            machine: machine.into(),
            is_remote: false,
            current_task: task.map(str::to_string),
            agent: task.map(|_| "developer".into()),
            badge,
        }
    }

    fn fixture_model() -> SidebarModel {
        let mut model = SidebarModel::skeleton("Demo");
        model.board_loading = false;
        model.workspaces = vec![
            ws("alpha", "hub", Some("T-1"), WorkspaceBadge::Working),
            ws("bravo", "hub", None, WorkspaceBadge::Idle),
            ws("delta", "devbox", None, WorkspaceBadge::Idle),
        ];
        model.reviews = vec![ReviewRow {
            task_id: "T-9".into(),
            title: "Ship it".into(),
            branch: "jlong/T-9".into(),
            location: Some("hub:review".into()),
            workspace: Some("review".into()),
            state: ReviewState::Serving,
        }];
        model.zen_mode = ZenModeState::On;
        model.unread_errors = 2;
        model.daemon_version_line = Some("daemon 0.10.0 · cli 0.10.0".into());
        model
    }

    fn open(
        cx: &mut TestAppContext,
        model: SidebarModel,
        selection: usize,
        link: Option<DaemonLink>,
    ) -> (gpui::Entity<Fixture>, VisualTestContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| Fixture {
                    model,
                    selection,
                    link,
                    events: Vec::new(),
                })
            })
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let root = window.root(&mut vcx).unwrap();
        vcx.run_until_parked();
        (root, vcx)
    }

    #[gpui::test]
    fn renders_every_row_of_the_model(cx: &mut TestAppContext) {
        let (_, mut cx) = open(cx, fixture_model(), 0, Some(DaemonLink::Live));
        for selector in [
            "sidebar-project-title",
            "sidebar-search",
            "sidebar-nav-Orchestrator",
            "sidebar-nav-Issues",
            "sidebar-nav-Activity",
            "sidebar-section-Workspaces",
            "sidebar-machine-hub",
            "sidebar-machine-devbox",
            "sidebar-ws-alpha",
            "sidebar-ws-bravo",
            "sidebar-ws-delta",
            "sidebar-section-Ready for Review",
            "sidebar-review-T-9",
            "sidebar-version",
            "sidebar-zen-band",
            "sidebar-errors",
            "sidebar-settings",
            "sidebar-collapse",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        // Nothing the model doesn't have: no queued section, no polling
        // indicator while live, no machines nav row.
        for absent in [
            "sidebar-section-Queued for Review",
            "sidebar-polling",
            "sidebar-loading",
            "sidebar-nav-Machines",
        ] {
            assert!(cx.debug_bounds(absent).is_none(), "{absent} is not drawn");
        }

        // Rows stack in the model's order.
        let y = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap().top();
        let order = [
            "sidebar-nav-Orchestrator",
            "sidebar-nav-Activity",
            "sidebar-machine-hub",
            "sidebar-ws-alpha",
            "sidebar-ws-bravo",
            "sidebar-machine-devbox",
            "sidebar-ws-delta",
            "sidebar-review-T-9",
        ];
        for pair in order.windows(2) {
            assert!(y(&mut cx, pair[0]) < y(&mut cx, pair[1]), "{} above {}", pair[0], pair[1]);
        }
    }

    #[gpui::test]
    fn the_selection_marks_the_selected_row(cx: &mut TestAppContext) {
        // Selectable rows: 3 nav, hub, alpha, bravo, devbox, delta, T-9.
        let (_, mut cx) = open(cx, fixture_model(), 4, None);
        let marker = cx.debug_bounds("sidebar-selection").expect("a selection");
        let row = cx.debug_bounds("sidebar-ws-alpha").unwrap();
        assert!(row.contains(&marker.origin), "the marker sits in alpha's row");
    }

    #[gpui::test]
    fn a_skeleton_shows_loading_and_polling_shows_the_indicator(cx: &mut TestAppContext) {
        let (_, mut cx) = open(
            cx,
            SidebarModel::skeleton("cold"),
            0,
            Some(DaemonLink::Polling),
        );
        assert!(cx.debug_bounds("sidebar-loading").is_some());
        assert!(cx.debug_bounds("sidebar-polling").is_some());
        assert!(cx.debug_bounds("sidebar-section-Workspaces").is_none());
        assert!(cx.debug_bounds("sidebar-zen-band").is_none(), "zen is off");
        assert!(cx.debug_bounds("sidebar-errors").is_none(), "no unread errors");
    }

    #[gpui::test]
    fn the_footer_shows_the_orchestrator_failure(app: &mut TestAppContext) {
        let (_, mut healthy) = open(app, fixture_model(), 0, None);
        assert!(healthy.debug_bounds("sidebar-status").is_none(), "healthy: no status row");

        let mut model = fixture_model();
        model.orchestrator_status = Some("supervisor gave up: spawn refused".into());
        let (_, mut cx) = open(app, model, 0, None);
        let status = cx.debug_bounds("sidebar-status").expect("the failure is drawn");
        let version = cx.debug_bounds("sidebar-version").unwrap();
        assert!(status.top() < version.top(), "above the version line, as in the TUI");
    }

    #[gpui::test]
    fn clicking_a_row_activates_its_ordinal(cx: &mut TestAppContext) {
        let (root, mut cx) = open(cx, fixture_model(), 0, None);
        let alpha = cx.debug_bounds("sidebar-ws-alpha").unwrap();
        cx.simulate_click(alpha.center(), Modifiers::none());
        let issues = cx.debug_bounds("sidebar-nav-Issues").unwrap();
        cx.simulate_click(issues.center(), Modifiers::none());
        let settings = cx.debug_bounds("sidebar-settings").unwrap();
        cx.simulate_click(settings.center(), Modifiers::none());
        let events = root.read_with(&cx, |f, _| f.events.clone());
        assert_eq!(
            events,
            [
                SidebarEvent::Activate(4),
                SidebarEvent::Activate(1),
                SidebarEvent::OpenSettings
            ]
        );
    }
}
