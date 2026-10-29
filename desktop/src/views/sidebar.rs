//! The sidebar: Figma's Navigation component (State=Default, node 2164:4),
//! rendered from `shelbi_app::view::SidebarModel` only.
//!
//! The items come from the shared [`SidebarModel::rows`] layout (the TUI's
//! rows, labels and state badges), arranged the way the component draws
//! them by [`layout`]: the review sections first (Ready for review, then
//! Queued for review), then Workspaces, each section collapsible, and every
//! review and working workspace numbered for its ⌘1 to ⌘9 chord. Top to
//! bottom: the titlebar row (the native traffic lights, then Search and
//! Toggle sidebar), the project switcher (the name and an up-down chevron),
//! the nav (Orchestrator ⌘O, Issues ⌘I, Changelog ⌘L), the sections, then
//! the footer: any status (the orchestrator's failure, the polling
//! indicator, a version mismatch or a stale board), the unread-errors and
//! Settings buttons, and the Zen pill.
//!
//! Where the desktop departs from the TUI (each a Figma decision): the
//! section order; Heroicons and the component's glyphs instead
//! of the TUI's emoji; a numbered chord per row; the issue number on a
//! review's second line; a review's serving location left to the Review
//! view.
//!
//! The width is the shared nav model's, in columns ([`COLUMN_PX`] each),
//! clamped by `shelbi_app::nav::clamp_sidebar_width`.

use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Bounds, Context, Hsla, Pixels,
    Div, ScrollHandle, SharedString, Stateful, Svg, Window,
};
use shelbi_app::nav::{clamp_sidebar_width, View};
use shelbi_app::overlay::palette::ProjectEntry;
use shelbi_app::view::{ReviewState, SidebarModel, SidebarRow, WorkspaceBadge};
use shelbi_state::ZenModeState;

use super::settings_nav::SettingsPage;
use crate::assets::{glyph, glyphs, heroicon, icons, Icon};
use crate::keys::{nav_view_hotkey, row_hotkey};
use crate::refresh::DaemonLink;
use crate::theme::fold::{self, Folds};
use crate::theme::motion::{self, fill_layer, SharedTrack};
use crate::theme::style::{self, Type, Typography, LIST_ROW_H, NAV_RADIUS};
use crate::theme::Theme;
use crate::views::project_menu;

/// Width of one sidebar column in the window, so the shared nav model's
/// widths (default 28, minimum 24) and its clamp apply unchanged: the default
/// 28 columns is the component's 280pt.
pub const COLUMN_PX: f32 = 10.0;

/// The leading column's titlebar row (every Figma frame's): the native
/// traffic lights centered 26pt down and 26pt in, and, right-aligned and
/// centered on them, the sidebar's Search and Toggle sidebar buttons.
pub const TITLEBAR_H: f32 = 52.0;

/// The macOS standard window buttons' frames (14pt squares, measured on
/// macOS 26), so their circles center where Figma draws them.
const TRAFFIC_LIGHT_BUTTON: f32 = 14.0;
/// Where Figma centers the close button: 26pt in and 26pt down.
const TRAFFIC_LIGHT_CENTER: f32 = 26.0;
/// The traffic lights' right edge (three buttons, 9pt apart), for the
/// collapsed sidebar's toggle beside them.
pub const TRAFFIC_LIGHTS_RIGHT: f32 =
    TRAFFIC_LIGHT_CENTER - TRAFFIC_LIGHT_BUTTON / 2.0 + 3.0 * TRAFFIC_LIGHT_BUTTON + 2.0 * 9.0;
/// The gap from the traffic lights' right edge to whatever sits beside them
/// when no sidebar is showing: Figma's Header puts the Back button's frame
/// 12pt past the zoom button.
const TRAFFIC_LIGHTS_GAP: f32 = 12.0;
/// Where a titlebar's content starts beside the traffic lights (the workflow
/// editor's Back, the collapsed sidebar's Show button, the relaunch bar):
/// every no-sidebar titlebar takes its left edge from here.
pub const BESIDE_TRAFFIC_LIGHTS: f32 = TRAFFIC_LIGHTS_RIGHT + TRAFFIC_LIGHTS_GAP;

/// The close button's origin for gpui's `traffic_light_position` (the
/// button's frame, inside a titlebar as tall as the button plus twice the
/// origin's y), so the lights center on the titlebar row.
pub fn traffic_light_position() -> gpui::Point<Pixels> {
    let at = TRAFFIC_LIGHT_CENTER - TRAFFIC_LIGHT_BUTTON / 2.0;
    gpui::point(px(at), px(at))
}

/// The Navigation component's blocks: 12pt in from the sides, 8pt above and
/// below.
const BLOCK_X: f32 = 12.0;
const BLOCK_Y: f32 = 8.0;

/// A nav item's height (Orchestrator, Issues, Changelog).
const NAV_ITEM_H: f32 = 34.0;

/// The project trigger's height.
pub const SWITCHER_H: f32 = 28.0;
/// The project block's minimum height.
const PROJECT_BLOCK_H: f32 = 48.0;
/// The menu's gap below the trigger (the `Project switcher` component's
/// `4`).
pub const MENU_GAP: f32 = 4.0;

/// The footer toolbar's and the titlebar's square buttons.
const TOOLBAR_BUTTON: f32 = 32.0;
/// The Zen pill's height and corner radius, the same in State=Default,
/// State=Workspace and State=Review.
const ZEN_PILL_H: f32 = 24.0;
const ZEN_PILL_RADIUS: f32 = 8.0;

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

// ---------------------------------------------------------------------------
// Layout: the shared rows, as the Navigation component arranges them.
// ---------------------------------------------------------------------------

/// A collapsible section of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Workspaces,
    Ready,
    Queued,
}

impl Section {
    /// Top to bottom, the shared model's (and the TUI's) order.
    pub const ORDER: [Section; 3] = [Section::Workspaces, Section::Ready, Section::Queued];

    /// The section a shared row label heads.
    fn of_label(label: &str) -> Option<Section> {
        match label {
            "Ready for Review" => Some(Section::Ready),
            "Queued for Review" => Some(Section::Queued),
            "Workspaces" => Some(Section::Workspaces),
            _ => None,
        }
    }

    /// The header as the component spells it (sentence case).
    pub fn label(self) -> &'static str {
        match self {
            Section::Ready => "Ready for review",
            Section::Queued => "Queued for review",
            Section::Workspaces => "Workspaces",
        }
    }

    /// How the collapsed state is saved (`ProjectViewState`).
    pub fn key(self) -> &'static str {
        match self {
            Section::Ready => "ready",
            Section::Queued => "queued",
            Section::Workspaces => "workspaces",
        }
    }

    pub fn from_key(key: &str) -> Option<Section> {
        Section::ORDER.into_iter().find(|s| s.key() == key)
    }
}

/// A body that folds open and closed in the sidebar: a section's rows, or
/// a machine group's workspaces.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Body {
    Section(Section),
    Machine(String),
}

/// The sections' and machine groups' folds, owned by the window.
pub type SidebarFolds = Folds<Body>;

/// A section header's decorations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionHead {
    pub section: Section,
    /// The count badge (the review sections; Workspaces draws none).
    pub count: Option<usize>,
    pub collapsed: bool,
}

/// One laid-out sidebar line: a shared row, with what the desktop adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub row: SidebarRow,
    /// On a section header row.
    pub head: Option<SectionHead>,
    /// The row's ⌘1 to ⌘9 chord.
    pub shortcut: Option<u8>,
    /// The section whose body the row is in (not its header, nor the blank
    /// before it).
    pub section: Option<Section>,
    /// The machine group whose body the row is in (an indented workspace).
    pub machine: Option<String>,
}

impl Entry {
    fn plain(row: SidebarRow) -> Entry {
        Entry {
            row,
            head: None,
            shortcut: None,
            section: None,
            machine: None,
        }
    }

    /// The row is in a body folding closed (`collapsed` sections, the
    /// model's collapsed machine groups): drawn while it closes, but no
    /// longer selectable or clickable.
    fn closing(&self, collapsed: &BTreeSet<Section>, model: &SidebarModel) -> bool {
        self.section.is_some_and(|s| collapsed.contains(&s))
            || self
                .machine
                .as_ref()
                .is_some_and(|m| model.collapsed_machines.contains(m))
    }
}

/// The sidebar as the Navigation component lays it out: the nav, then the
/// shared rows' Workspaces, Ready and Queued sections in the shared order,
/// each header carrying its count and collapse state, a collapsed section's
/// rows left out. Working workspaces and reviews are numbered 1 to 9 top to
/// bottom whether or not their section is collapsed, so a chord keeps its row.
///
/// The window selects and activates over this list's selectable rows, so
/// what the keyboard walks is what is drawn.
pub fn layout(model: &SidebarModel, collapsed: &BTreeSet<Section>) -> Vec<Entry> {
    let rows = model.rows();
    let nav_n = rows
        .iter()
        .take_while(|r| matches!(r, SidebarRow::Nav { .. }))
        .count();
    let mut out: Vec<Entry> = rows[..nav_n].iter().cloned().map(Entry::plain).collect();

    // The rest is groups, each opened by a blank: a section (its header and
    // rows) or the cold board's placeholder.
    let mut groups: Vec<Vec<SidebarRow>> = Vec::new();
    for row in &rows[nav_n..] {
        match (row, groups.last_mut()) {
            (SidebarRow::Blank, _) | (_, None) => groups.push(vec![row.clone()]),
            (_, Some(group)) => group.push(row.clone()),
        }
    }
    let section_of = |group: &[SidebarRow]| {
        group.iter().find_map(|r| match r {
            SidebarRow::Section(label) => Section::of_label(label),
            _ => None,
        })
    };

    let mut next = 1u8;
    let mut number = || {
        let n = next;
        next = next.saturating_add(1);
        (n <= 9).then_some(n)
    };
    for group in groups {
        let section = section_of(&group);
        let count = group.iter().filter(|r| matches!(r, SidebarRow::Review { .. })).count();
        let folded = section.is_some_and(|s| collapsed.contains(&s));
        let mut machine: Option<String> = None;
        for row in group {
            let shortcut = match &row {
                SidebarRow::Review { .. } => number(),
                SidebarRow::Workspace { name, .. } if workspace_busy(model, name) => number(),
                _ => None,
            };
            let head = match (&row, section) {
                (SidebarRow::Section(_), Some(section)) => Some(SectionHead {
                    section,
                    count: (section != Section::Workspaces).then_some(count),
                    collapsed: folded,
                }),
                _ => None,
            };
            let in_body = !matches!(row, SidebarRow::Blank | SidebarRow::Section(_));
            match &row {
                SidebarRow::MachineGroup { name, .. } => machine = Some(name.clone()),
                SidebarRow::Workspace { indent: true, .. } => {}
                _ => machine = None,
            }
            let in_machine = match &row {
                SidebarRow::Workspace { indent: true, .. } => machine.clone(),
                _ => None,
            };
            if !(folded && in_body) {
                out.push(Entry {
                    row,
                    head,
                    shortcut,
                    section: section.filter(|_| in_body),
                    machine: in_machine,
                });
            }
        }
    }
    out
}

/// The rows a frame draws at `now`: [`layout`]'s, plus the rows of each
/// section and machine group still folding closed, so they can close
/// rather than vanish. Chords are [`layout`]'s (a row folding away shows
/// none), and the headers say what is collapsed, not what is still drawn.
pub fn drawn(
    model: &SidebarModel,
    collapsed: &BTreeSet<Section>,
    folds: &SidebarFolds,
    now: Instant,
) -> Vec<Entry> {
    let hidden: BTreeSet<Section> = collapsed
        .iter()
        .copied()
        .filter(|s| !folds.is_folding(&Body::Section(*s), true, now))
        .collect();
    let closing: Vec<&String> = model
        .collapsed_machines
        .iter()
        .filter(|m| folds.is_folding(&Body::Machine((*m).clone()), true, now))
        .collect();
    let mut entries = if closing.is_empty() {
        layout(model, &hidden)
    } else {
        let mut open = model.clone();
        for m in closing {
            open.collapsed_machines.remove(m);
        }
        let chords: HashMap<String, u8> = layout(model, &BTreeSet::new())
            .iter()
            .filter_map(|e| e.shortcut.map(|n| (row_selector(&e.row), n)))
            .collect();
        let mut entries = layout(&open, &hidden);
        for e in &mut entries {
            if let SidebarRow::MachineGroup { name, collapsed, .. } = &mut e.row {
                *collapsed = model.collapsed_machines.contains(name);
            }
            e.shortcut = chords.get(&row_selector(&e.row)).copied();
        }
        entries
    };
    for head in entries.iter_mut().filter_map(|e| e.head.as_mut()) {
        head.collapsed = collapsed.contains(&head.section);
    }
    entries
}

/// Whether workspace `name` is working a task (its row names the agent and
/// takes a chord; an idle one reads "Idle").
fn workspace_busy(model: &SidebarModel, name: &str) -> bool {
    model
        .workspaces
        .iter()
        .any(|w| w.name == name && w.current_task.is_some())
}

/// The `ordinal`-th selectable entry's selection-chip key: its selector, so
/// the chip follows the item (not its ordinal) when rows come and go.
pub fn selected_key(entries: &[Entry], ordinal: usize) -> Option<SharedString> {
    entries
        .iter()
        .filter(|e| e.row.is_selectable())
        .nth(ordinal)
        .map(|e| row_selector(&e.row).into())
}

// ---------------------------------------------------------------------------
// The window's side.
// ---------------------------------------------------------------------------

/// What the user did in the sidebar. The host (the window) acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    /// Open the `index`-th selectable row (selecting it first).
    Activate(usize),
    /// The titlebar's Search button: opens the command palette.
    OpenSearch,
    /// Show or hide the project switcher.
    ToggleSwitcher,
    /// Close the switcher (a click outside it).
    CloseSwitcher,
    /// Switch to (open or focus the window of) a project.
    SwitchTo(String),
    /// The switcher's "Add project…".
    AddProject,
    /// Collapse or expand a section (its header).
    ToggleSection(Section),
    /// The Zen pill.
    ToggleZen,
    /// The unread-errors button.
    OpenErrors,
    OpenSettings,
    /// The footer's megaphone: Contact Us.
    SendFeedback,
    /// The titlebar's Toggle sidebar button.
    Collapse,
    /// The settings sidebar's back button.
    LeaveSettings,
    /// A settings sidebar row.
    ShowSettingsPage(SettingsPage),
}

/// The window the sidebar is drawn into.
pub trait SidebarHost: 'static + Sized {
    fn sidebar_event(&mut self, event: SidebarEvent, window: &mut Window, cx: &mut Context<Self>);
}

/// Everything one sidebar frame draws. All of it is in memory: rendering
/// never reads state.
pub struct SidebarProps<'a> {
    pub model: &'a SidebarModel,
    /// The sections the user has collapsed.
    pub collapsed: &'a BTreeSet<Section>,
    /// The sections and machine groups folding open or closed.
    pub folds: &'a SidebarFolds,
    /// The frame's time, for the folds.
    pub now: Instant,
    /// The selected row's selectable-row ordinal over [`layout`]'s rows, or
    /// `None` when what the main area shows has no row.
    pub selection: Option<usize>,
    pub link: Option<DaemonLink>,
    /// The palette-open chord, for the Search button's tooltip (`⌃P`).
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
    /// Every project, most recently launched first, with its dot
    /// ([`crate::projects::Catalog::rows`]; the current one is `Active`).
    pub projects: &'a [ProjectEntry],
    /// The keyboard's row: an index into the projects, or `projects.len()`
    /// for "Add project…".
    pub highlight: usize,
    /// How shown the menu is: 0 (hidden) to 1, eased.
    pub phase: f32,
}

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



/// One row's text: the primary label, the right-aligned label, and a
/// review's second line (`#401` and the branch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowText {
    pub label: String,
    pub right: Option<String>,
    /// A review's issue number (`#401`), when its tracker has one.
    pub number: Option<String>,
    pub branch: Option<String>,
}

/// The text of a non-nav row. Pure, so its labels are tested.
pub fn row_text(row: &SidebarRow, model: &SidebarModel) -> RowText {
    let plain = |label: String| RowText {
        label,
        right: None,
        number: None,
        branch: None,
    };
    match row {
        SidebarRow::Nav { label, .. } => plain(label.clone()),
        SidebarRow::Section(label) => plain(
            Section::of_label(label)
                .map_or_else(|| label.clone(), |s| s.label().to_string()),
        ),
        SidebarRow::ConfigError(message) => plain(format!("! {message}")),
        SidebarRow::Blank => plain(String::new()),
        SidebarRow::Loading => plain("Loading…".into()),
        SidebarRow::MachineGroup {
            name,
            collapsed,
            total,
            active,
        } => RowText {
            label: name.clone(),
            right: collapsed.then(|| format!("({total}, {active} active)")),
            number: None,
            branch: None,
        },
        SidebarRow::Workspace {
            name,
            badge,
            right_label,
            ..
        } => RowText {
            label: name.clone(),
            right: if workspace_busy(model, name) {
                (!right_label.is_empty()).then(|| right_label.clone())
            } else if *badge == WorkspaceBadge::Loading {
                None
            } else {
                Some("Idle".into())
            },
            number: None,
            branch: None,
        },
        SidebarRow::Review {
            task_id,
            title,
            branch,
            ..
        } => RowText {
            label: title.clone(),
            right: None,
            number: model
                .reviews
                .iter()
                .find(|r| &r.task_id == task_id)
                .and_then(|r| r.number)
                .map(|n| format!("#{n}")),
            branch: Some(branch.clone()),
        },
    }
}

/// How a row's state glyph draws: one of the component's glyphs in a
/// color, or (for the states the component doesn't draw) the shared badge's
/// text glyph.
#[derive(Debug, Clone, PartialEq)]
pub enum StateGlyph {
    Drawn(crate::assets::Glyph, Hsla),
    /// The awaiting-input disc: yellow, its `?` cut out over a dark disc.
    Question(Hsla),
    Text(String, Hsla),
}

/// A workspace's state glyph: green play while working, the yellow `?`
/// while it waits for input, a quiet dot while idle.
pub fn workspace_glyph(badge: WorkspaceBadge, theme: &Theme) -> StateGlyph {
    match badge {
        WorkspaceBadge::Working => StateGlyph::Drawn(glyphs::PLAY, theme.icon_green),
        WorkspaceBadge::AwaitingInput => StateGlyph::Question(theme.icon_yellow),
        WorkspaceBadge::Idle => StateGlyph::Drawn(glyphs::DOT, quiet_glyph(theme)),
        WorkspaceBadge::Loading => StateGlyph::Text(badge.glyph().to_string(), theme.muted),
        WorkspaceBadge::AwaitingPermission | WorkspaceBadge::Paused => StateGlyph::Text(
            badge.glyph().to_string(),
            theme.decoration(badge.decoration_color()),
        ),
    }
}

/// A review's state glyph: the cyan check while it is served, a quiet dot
/// while it waits for a slot, a cyan play while it loads onto one.
pub fn review_glyph(state: ReviewState, theme: &Theme) -> StateGlyph {
    match state {
        ReviewState::Serving => StateGlyph::Drawn(glyphs::CHECK, theme.icon_cyan),
        ReviewState::Loading => StateGlyph::Drawn(glyphs::PLAY, theme.icon_cyan),
        ReviewState::Pending => StateGlyph::Drawn(glyphs::DOT, quiet_glyph(theme)),
    }
}

/// The component's quiet glyphs (the idle dot, the section chevron):
/// `theme.gray`'s slate at 60%.
fn quiet_glyph(theme: &Theme) -> Hsla {
    let mut c = theme.gray;
    c.a = 0.6;
    c
}

fn render_glyph(g: StateGlyph) -> AnyElement {
    match g {
        StateGlyph::Drawn(g, color) => glyph(g).text_color(color).into_any_element(),
        StateGlyph::Question(color) => div()
            .relative()
            .flex_none()
            .size(glyphs::QUESTION.size())
            .child(
                div()
                    .absolute()
                    .top(px(1.0))
                    .left(px(1.0))
                    .size(px(10.0))
                    .rounded_full()
                    .bg(gpui::rgb(0x131418)),
            )
            .child(glyph(glyphs::QUESTION).absolute().top_0().left_0().text_color(color))
            .into_any_element(),
        StateGlyph::Text(text, color) => div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(12.0))
            .type_style(Type::XSmall)
            .text_color(color)
            .child(text)
            .into_any_element(),
    }
}

/// The Heroicon for a nav view.
pub fn nav_icon(view: &View) -> Icon {
    match view {
        View::Session(_) => icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS,
        View::Issues => icons::RECTANGLE_STACK,
        View::Changelog => icons::ROCKET_LAUNCH,
        // Palette-only now (no sidebar row): the bolt the Figma Activity
        // frame draws, beside the Changelog's rocket there.
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

/// A 32pt square toolbar button (the titlebar's, the footer's): the icon in
/// gray, a fill fading in and the icon going loud on hover.
fn toolbar_button(id: &'static str, icon: Svg, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let radius = px(8.0);
    let loud = theme.text_loud;
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .group(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(TOOLBAR_BUTTON))
        .rounded(radius)
        .cursor_pointer();
    fill_layer(
        d,
        id,
        radius,
        [theme.background.opacity(0.0), theme.background_hover, theme.background_active],
    )
    .child(
        icon.text_color(theme.icon)
            .group_hover(id, move |s| s.text_color(loud)),
    )
}

/// Render the sidebar.
pub fn render<H: SidebarHost>(
    props: SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let entries = drawn(props.model, props.collapsed, props.folds, props.now);
    let nav_n = entries
        .iter()
        .take_while(|e| matches!(e.row, SidebarRow::Nav { .. }))
        .count();
    // Selectable ordinal for each entry, over [`layout`]'s rows: a row
    // folding away is drawn but has none.
    let mut ordinal = 0usize;
    let ordinals: Vec<Option<usize>> = entries
        .iter()
        .map(|e| {
            (e.row.is_selectable() && !e.closing(props.collapsed, props.model)).then(|| {
                ordinal += 1;
                ordinal - 1
            })
        })
        .collect();

    let nav = entries[..nav_n]
        .iter()
        .zip(&ordinals)
        .map(|(entry, ord)| render_nav_row(&entry.row, *ord, &props, theme, cx))
        .collect::<Vec<_>>();
    // While the selection chip slides it is drawn here, under every row.
    let chip = props.chip.map(|bounds| {
        motion::floating_chip(bounds, |d| {
            style::nav_chip(d.rounded(px(NAV_RADIUS)), theme)
        })
    });
    let rest = render_bodies(&entries[nav_n..], &ordinals[nav_n..], &props, theme, cx);

    div()
        .id("sidebar")
        .debug_selector(|| "sidebar".into())
        .flex()
        .flex_col()
        .w(px(props.width))
        .h_full()
        .flex_none()
        .relative()
        .bg(theme.nested(theme.sidebar))
        .text_color(theme.foreground)
        .type_style(Type::Base)
        .child(motion::origin_probe(&props.track))
        .children(chip)
        .child(render_titlebar(&props.palette_chord, theme, cx))
        .child(render_project(&props, theme, cx))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap_1()
                .px(px(BLOCK_X))
                .py(px(BLOCK_Y))
                .children(nav),
        )
        .child(
            div()
                .id("sidebar-rest")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .px(px(BLOCK_X))
                .pb(px(BLOCK_Y))
                .overflow_y_scroll()
                .track_scroll(&props.scroll)
                .children(rest),
        )
        .child(render_footer(props.model, props.link, theme, cx))
        .into_any_element()
}

/// The leading column with a task panel in the sidebar's place (the
/// Navigation component's State=Workspace and State=Review): the sidebar's
/// titlebar and footer around `panel`.
pub fn render_panel_column<H: SidebarHost>(
    panel: impl IntoElement,
    model: &SidebarModel,
    link: Option<DaemonLink>,
    palette_chord: &str,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .bg(theme.nested(theme.sidebar))
        .text_color(theme.foreground)
        .type_style(Type::Base)
        .child(render_titlebar(palette_chord, theme, cx))
        .child(div().flex().flex_col().flex_1().min_h_0().child(panel))
        .child(render_footer(model, link, theme, cx))
        .into_any_element()
}

/// The titlebar row: room for the traffic lights, then Search (the palette)
/// and Toggle sidebar, right-aligned.
pub(crate) fn render_titlebar<H: SidebarHost>(
    palette_chord: &str,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let search_tip = if palette_chord.is_empty() {
        "Search".to_string()
    } else {
        format!("Search ({palette_chord})")
    };
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .justify_end()
        .gap_1()
        .h(px(TITLEBAR_H))
        .pr(px(8.0))
        .child(
            toolbar_button("sidebar-search", heroicon(icons::MAGNIFYING_GLASS), theme)
                .tooltip(style::tooltip(search_tip))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.sidebar_event(SidebarEvent::OpenSearch, window, cx)
                })),
        )
        .child(
            toolbar_button("sidebar-collapse", heroicon(icons::SIDEBAR), theme)
                .tooltip(style::tooltip("Hide the sidebar"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.sidebar_event(SidebarEvent::Collapse, window, cx)
                })),
        )
}

/// The project block: the trigger (the project's name and an up-down
/// chevron) that opens the switcher's menu below it.
fn render_project<H: SidebarHost>(
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let button = switcher_trigger(
        props.model.project_label.clone(),
        props.switcher_open,
        &props.switcher_button,
        theme,
        cx,
    );
    let menu = props.switcher.as_ref().map(|switcher| {
        let top = (PROJECT_BLOCK_H + SWITCHER_H) / 2.0 + MENU_GAP;
        switcher_menu(switcher, &props.switcher_button, top, props.width, theme, cx)
    });

    div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .justify_center()
        .items_start()
        .min_h(px(PROJECT_BLOCK_H))
        .px(px(BLOCK_X))
        .py(px(BLOCK_Y))
        .child(button)
        .children(menu)
}

/// The project switcher's trigger: the project's `label` and an up-down
/// chevron, holding its pressed fill while the menu is `open`; it records
/// where it lands into `cell` (for the menu's outside click). The settings
/// sidebar's project section draws it too.
pub fn switcher_trigger<H: SidebarHost>(
    label: impl Into<SharedString>,
    open: bool,
    cell: &Rc<Cell<Option<Bounds<Pixels>>>>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> Stateful<Div> {
    let button_probe = {
        let cell = cell.clone();
        gpui::canvas(move |bounds, _, _| cell.set(Some(bounds)), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full()
    };
    let rest = if open {
        theme.background_active
    } else {
        theme.background.opacity(0.0)
    };
    let radius = px(NAV_RADIUS);
    let button = div()
        .id("sidebar-project-title")
        .debug_selector(|| "sidebar-project-title".into())
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .h(px(SWITCHER_H))
        .max_w_full()
        .rounded(radius)
        .cursor_pointer();
    fill_layer(
        button,
        "sidebar-project-title",
        radius,
        [rest, theme.background_hover, theme.background_active],
    )
    .child(button_probe)
    .child(
        div()
            .min_w_0()
            .truncate()
            .type_style(Type::BaseMedium)
            .text_color(theme.foreground)
            .child(label.into()),
    )
    .child(heroicon(icons::CHEVRON_UP_DOWN).text_color(if open { theme.text_loud } else { theme.muted }))
    .on_click(cx.listener(|this, _, window, cx| {
        this.sidebar_event(SidebarEvent::ToggleSwitcher, window, cx)
    }))
}

/// The switcher's menu floating `top` below its block's top, 8px in and
/// as wide as a column `width` wide allows (the component's 236pt in the
/// 252pt column): drawn after everything (`deferred`), so nothing below it
/// moves and nothing paints over it.
pub fn switcher_menu<H: SidebarHost>(
    switcher: &SwitcherProps<'_>,
    button: &Rc<Cell<Option<Bounds<Pixels>>>>,
    top: f32,
    width: f32,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    deferred(
        div()
            .absolute()
            .top(px(top + (1.0 - switcher.phase) * motion::SLIDE_PX))
            .left(px(8.0))
            .min_w(px((width - 16.0).max(0.0)))
            .opacity(switcher.phase)
            .child(render_switcher(switcher, button.clone(), theme, cx)),
    )
    .with_priority(1)
}

/// The switcher's menu, the Figma `Project switcher`'s open state
/// (2186:1692): the "Projects" heading, a row per project (its dot, its
/// name, a check on the current one), a rule, then "+ Add project"
/// ([`project_menu`], the palette's Projects column too). The keyboard's row
/// wears the selection fill. Its `search` tint is laid over the sidebar's
/// color, so it reads as the frame draws it over the sidebar while hiding
/// the rows below it.
fn render_switcher<H: SidebarHost>(
    switcher: &SwitcherProps<'_>,
    button: Rc<Cell<Option<Bounds<Pixels>>>>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let rows = switcher.projects.iter().enumerate().map(|(ix, p)| {
        let name = p.slug.clone();
        project_menu::project_row(
            SharedString::from(format!("sidebar-project-{}", p.slug)),
            format!("sidebar-project-{}", p.slug),
            p,
            switcher.highlight == ix,
            theme,
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::SwitchTo(name.clone()), window, cx)
        }))
    });
    let add_ix = switcher.projects.len();
    let add = project_menu::add_row(
        "sidebar-add-project",
        "sidebar-add-project",
        switcher.highlight == add_ix,
        theme,
    )
    .on_click(cx.listener(|this, _, window, cx| {
        this.sidebar_event(SidebarEvent::AddProject, window, cx)
    }));
    div()
        .id("sidebar-switcher")
        .debug_selector(|| "sidebar-switcher".into())
        .flex()
        .flex_col()
        .px_1()
        .py_2()
        .rounded(px(NAV_RADIUS))
        .bg(theme.raised.blend(theme.search))
        .text_color(theme.foreground)
        .occlude()
        .on_mouse_down_out(cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
            // A press on the button toggles it closed through its click.
            if button.get().is_some_and(|b| b.contains(&event.position)) {
                return;
            }
            this.sidebar_event(SidebarEvent::CloseSwitcher, window, cx)
        }))
        .child(project_menu::heading(theme))
        .when(switcher.projects.is_empty(), |d| {
            d.child(
                div()
                    .px_2()
                    .h(px(project_menu::ROW_H))
                    .flex()
                    .items_center()
                    .type_style(Type::Base)
                    .text_color(theme.muted)
                    .child("Loading projects…"),
            )
        })
        .children(rows)
        .child(project_menu::divider(theme))
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
    let selected = ordinal.is_some() && ordinal == props.selection;
    let selector = row_selector(row);
    let ord = ordinal.unwrap_or_default();
    let painted = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .text_color(theme.foreground)
        .map(|d| selected_row(d, &row_selector(row), selected, props, theme));
    let hotkey = nav_view_hotkey(view).map(|chord| style::hotkey(&chord, theme).into_any_element());
    nav_item(painted, nav_icon(view), label.clone(), hotkey, selected, theme)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::Activate(ord), window, cx)
        }))
        .into_any_element()
}

/// The component's nav item (the sidebar's nav, the task panels' views):
/// 34pt, 6pt in, the icon, the label growing, then `trailing` (the hotkey);
/// `current` turns the icon to the accent. `d` comes painted with its state
/// (the chip and its loud, medium label, the keyboard's fill, the hover fill),
/// so the content lands over the fill.
pub fn nav_item(
    d: gpui::Stateful<gpui::Div>,
    icon: Icon,
    label: impl Into<SharedString>,
    trailing: Option<AnyElement>,
    current: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    d.relative()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .px(px(6.0))
        .h(px(NAV_ITEM_H))
        .rounded(px(NAV_RADIUS))
        .cursor_pointer()
        .child(heroicon(icon).text_color(if current { theme.accent } else { theme.icon }))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .children(trailing)
}

/// Paint a row selected (the chip) or hoverable, and measure it for the
/// chip's slide. While the chip slides, the floating one stands in for the
/// row's own.
fn selected_row(
    d: gpui::Stateful<gpui::Div>,
    key: &str,
    selected: bool,
    props: &SidebarProps<'_>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let inline = props.chip.is_none();
    style::row_state_rounded(
        d,
        key.to_string(),
        selected,
        false,
        inline,
        px(NAV_RADIUS),
        theme,
    )
    .child(motion::probe(&props.track, key.to_string()))
    .when(selected, |d| d.child(selection_marker()))
}

/// A zero-size marker inside the selected row, so a test can find the
/// selection.
fn selection_marker() -> impl IntoElement {
    div()
        .debug_selector(|| "sidebar-selection".into())
        .absolute()
        .size_0()
}

/// A section header: the label (Small/Medium, muted), the count badge, and
/// the chevron, which turns to point right as the section folds closed
/// (`openness`, 1 open). A click anywhere on it collapses or expands the
/// section.
fn render_section_head<H: SidebarHost>(
    base: gpui::Stateful<gpui::Div>,
    head: &SectionHead,
    openness: f32,
    label: String,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let section = head.section;
    let chevron = glyph(glyphs::CHEVRON_DOWN)
        .text_color(quiet_glyph(theme))
        .with_transformation(fold::chevron(openness));
    base.flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .pt_2()
        .pb_1()
        .cursor_pointer()
        .child(
            div()
                .type_style(Type::SmallMedium)
                .text_color(theme.muted)
                .child(label),
        )
        .children(head.count.map(|n| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .h(px(16.0))
                .min_w(px(16.0))
                .px_1()
                .rounded(px(4.0))
                .bg(theme.background)
                .type_style(Type::XSmallMedium)
                .text_color(theme.foreground)
                .child(n.to_string())
        }))
        .child(chevron)
        .on_click(cx.listener(move |this, _, window, cx| {
            this.sidebar_event(SidebarEvent::ToggleSection(section), window, cx)
        }))
        .into_any_element()
}

/// The rows under the nav, each section's body (and each machine group's,
/// inside it) in its own fold.
fn render_bodies<H: SidebarHost>(
    entries: &[Entry],
    ordinals: &[Option<usize>],
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> Vec<AnyElement> {
    // A run of consecutive rows sharing `key`'s body (`None`: no body).
    type Run<T> = (Option<T>, std::ops::Range<usize>);
    fn runs<T: PartialEq>(range: std::ops::Range<usize>, key: impl Fn(usize) -> Option<T>) -> Vec<Run<T>> {
        let mut out: Vec<Run<T>> = Vec::new();
        for i in range {
            let k = key(i);
            match out.last_mut() {
                Some((last, r)) if k.is_some() && *last == k => r.end = i + 1,
                _ => out.push((k, i..i + 1)),
            }
        }
        out
    }
    let mut out = Vec::new();
    for (section, range) in runs(0..entries.len(), |i| entries[i].section) {
        let mut rows = Vec::new();
        for (machine, range) in runs(range, |i| entries[i].machine.clone()) {
            let mut machine_rows: Vec<AnyElement> = range
                .map(|i| render_row(i, &entries[i], ordinals[i], props, theme, cx))
                .collect();
            match machine {
                Some(name) => {
                    let collapsed = props.model.collapsed_machines.contains(&name);
                    rows.push(fold_body(props, Body::Machine(name), collapsed, machine_rows));
                }
                None => rows.append(&mut machine_rows),
            }
        }
        match section {
            Some(section) => {
                let collapsed = props.collapsed.contains(&section);
                out.push(fold_body(props, Body::Section(section), collapsed, rows));
            }
            None => out.append(&mut rows),
        }
    }
    out
}

/// `rows` as body `key`'s fold, as open as it is this frame.
fn fold_body(props: &SidebarProps<'_>, key: Body, collapsed: bool, rows: Vec<AnyElement>) -> AnyElement {
    let openness = props.folds.openness(&key, collapsed, props.now);
    let selector = match &key {
        Body::Section(s) => format!("sidebar-body-{}", s.key()),
        Body::Machine(name) => format!("sidebar-body-machine-{name}"),
    };
    props
        .folds
        .body(key, openness, div().flex().flex_col().children(rows))
        .debug_selector(move || selector)
        .into_any_element()
}

fn render_row<H: SidebarHost>(
    index: usize,
    entry: &Entry,
    ordinal: Option<usize>,
    props: &SidebarProps<'_>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let row = &entry.row;
    let selector = row_selector(row);
    let text = row_text(row, props.model);
    // A row's element is its row's, not its position's: its fade (the
    // spring's state lives under the element) stays with it as rows arrive,
    // leave and reorder above it. The blanks between sections repeat.
    let id: gpui::ElementId = match row {
        SidebarRow::Blank => ("sidebar-row", index).into(),
        _ => SharedString::from(selector.clone()).into(),
    };
    let base = div()
        .id(id)
        .debug_selector({
            let selector = selector.clone();
            move || selector
        })
        .flex_none();
    let selected = ordinal.is_some() && ordinal == props.selection;
    let ord = ordinal.unwrap_or_default();
    // A selectable row's frame: 8pt in, 4pt above and below, the chip's
    // corners, selectable and clickable.
    let item = |base: gpui::Stateful<gpui::Div>| {
        base.relative()
            .flex()
            .px_2()
            .py_1()
            .rounded(px(NAV_RADIUS))
            .cursor_pointer()
            .text_color(theme.foreground)
            .map(|d| selected_row(d, &selector, selected, props, theme))
            .when(ordinal.is_some(), |d| {
                d.on_click(cx.listener(move |this, _, window, cx| {
                    this.sidebar_event(SidebarEvent::Activate(ord), window, cx)
                }))
            })
    };
    let shortcut = entry.shortcut.map(|n| style::hotkey(&row_hotkey(n), theme));
    match row {
        // The space between sections: each section's own 8pt below, the
        // next one's 8pt above. Under the nav, whose block already ends 8pt
        // down, only the section's own.
        SidebarRow::Blank if index == 0 => base.h(px(BLOCK_Y)).into_any_element(),
        SidebarRow::Blank => base.h(px(2.0 * BLOCK_Y)).into_any_element(),
        SidebarRow::Section(_) => match &entry.head {
            Some(head) => {
                let openness = props.folds.openness(&Body::Section(head.section), head.collapsed, props.now);
                render_section_head(base, head, openness, text.label, theme, cx)
            }
            None => base
                .px_2()
                .pt_2()
                .pb_1()
                .type_style(Type::SmallMedium)
                .text_color(theme.muted)
                .child(text.label)
                .into_any_element(),
        },
        SidebarRow::Loading => base
            .px_2()
            .h(px(LIST_ROW_H))
            .flex()
            .items_center()
            .italic()
            .type_style(Type::Small)
            .text_color(theme.muted)
            .child(text.label)
            .into_any_element(),
        SidebarRow::ConfigError(_) => base
            .px_2()
            .py_1()
            .type_style(Type::Small)
            .text_color(theme.error)
            .child(text.label)
            .into_any_element(),
        SidebarRow::Nav { .. } => base.into_any_element(),
        SidebarRow::MachineGroup { name, collapsed, .. } => {
            let openness = props.folds.openness(&Body::Machine(name.clone()), *collapsed, props.now);
            let chevron = glyph(glyphs::CHEVRON_DOWN)
                .text_color(quiet_glyph(theme))
                .with_transformation(fold::chevron(openness))
                .debug_selector({
                    let name = name.clone();
                    move || format!("sidebar-machine-chevron-{name}")
                });
            // The name then its chevron, as a section header draws them.
            item(base)
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .type_style(Type::SmallMedium)
                                .text_color(theme.muted)
                                .child(text.label)
                                .debug_selector({
                                    let name = name.clone();
                                    move || format!("sidebar-machine-name-{name}")
                                }),
                        )
                        .child(chevron),
                )
                .child(div().flex_1())
                .children(text.right.map(|right| {
                    let name = name.clone();
                    div()
                        .flex_none()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child(right)
                        .debug_selector(move || format!("sidebar-machine-count-{name}"))
                }))
                .into_any_element()
        }
        SidebarRow::Workspace {
            name,
            badge,
            indent,
            ..
        } => {
            let busy = workspace_busy(props.model, name);
            item(base)
                .flex_row()
                .items_center()
                .gap_2()
                // A machine's workspaces sit a chevron and a gap in from its name.
                .when(*indent, |d| d.pl(px(8.0 + 12.0 + 8.0)))
                .child(render_glyph(workspace_glyph(*badge, theme)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .map(|d| {
                            if busy || selected {
                                d.type_style(Type::SmallMedium).text_color(theme.text_loud)
                            } else {
                                d.type_style(Type::Small).text_color(theme.foreground)
                            }
                        })
                        .child(text.label),
                )
                .child(div().flex_1())
                .children(text.right.map(|right| {
                    div()
                        .flex_none()
                        .map(|d| {
                            if busy {
                                d.type_style(Type::XSmallSemibold).text_color(theme.foreground)
                            } else {
                                d.type_style(Type::XSmall).text_color(theme.muted)
                            }
                        })
                        .child(right)
                }))
                .children(shortcut)
                .into_any_element()
        }
        SidebarRow::Review { state, .. } => {
            let title = div()
                .flex_1()
                .min_w_0()
                .truncate()
                .type_style(Type::XSmallMedium)
                .text_color(if selected { theme.text_loud } else { theme.foreground })
                .child(text.label);
            let second = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .type_style(Type::XSmall)
                .text_color(theme.muted)
                .children(text.number.map(|n| div().flex_none().child(n)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(text.branch.unwrap_or_default()),
                );
            item(base)
                .flex_row()
                .items_start()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .h(px(16.0))
                        .child(render_glyph(review_glyph(*state, theme))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(title)
                                .children(shortcut),
                        )
                        .child(second),
                )
                .into_any_element()
        }
    }
}

fn render_footer<H: SidebarHost>(
    model: &SidebarModel,
    link: Option<DaemonLink>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    // The version line says something only when it needs a look (the daemon
    // and the CLI disagree, or the board is stale); otherwise it waits in the
    // Settings button's tooltip, as the component draws no version.
    let version = model
        .daemon_version_line
        .clone()
        .filter(|_| model.daemon_version_mismatch)
        .map(|line| div().text_color(theme.error).child(line));
    let banner = board_warning(model.board_banner.clone())
        .map(|banner| div().text_color(theme.warning).child(banner));
    let polling = (link == Some(DaemonLink::Polling)).then(|| {
        div()
            .debug_selector(|| "sidebar-polling".into())
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .text_color(theme.warning)
            .child(heroicon(icons::SIGNAL_SLASH).text_color(theme.warning))
            .child("Daemon unreachable · polling")
    });
    let status = footer_status(model, theme).map(|(line, color)| {
        div()
            .debug_selector(|| "sidebar-status".into())
            .text_color(color)
            .child(line)
    });
    let notes: Vec<AnyElement> = status
        .into_iter()
        .map(IntoElement::into_any_element)
        .chain(polling.map(IntoElement::into_any_element))
        .chain(version.map(IntoElement::into_any_element))
        .chain(banner.map(IntoElement::into_any_element))
        .collect();

    let settings_tip = match &model.daemon_version_line {
        Some(line) => format!("Settings · {line}"),
        None => "Settings".to_string(),
    };
    // Settings and Send Feedback together in the left corner (Figma
    // Navigation's footer Toolbar, 4pt apart), the unread-errors badge after
    // them while there are any, then the flexible space.
    let toolbar = div()
        .debug_selector(|| "sidebar-footer-toolbar".into())
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(
            toolbar_button("sidebar-settings", heroicon(icons::COG_6_TOOTH), theme)
                .tooltip(style::tooltip(settings_tip))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.sidebar_event(SidebarEvent::OpenSettings, window, cx)
                })),
        )
        .child(
            toolbar_button("sidebar-feedback", heroicon(icons::MEGAPHONE), theme)
                .tooltip(style::tooltip("Send Feedback"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.sidebar_event(SidebarEvent::SendFeedback, window, cx)
                })),
        )
        .when(model.unread_errors > 0, |d| {
            d.child(
                div()
                    .id("sidebar-errors")
                    .debug_selector(|| "sidebar-errors".into())
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(TOOLBAR_BUTTON))
                    .rounded(px(8.0))
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
        .child(div().flex_1());

    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap_2()
        .p_2()
        .when(!notes.is_empty(), |d| {
            d.child(
                div()
                    .debug_selector(|| "sidebar-version".into())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_1()
                    .type_style(Type::XSmall)
                    .children(notes),
            )
        })
        .child(toolbar)
        .child(render_zen_pill(model.zen_mode, theme, cx))
}

/// The Zen pill: the footer's full width, 24pt tall, 8pt corners. On, it is
/// the Success tint over the canvas with a centered green "Zen mode on"; off
/// (or paused), which the component doesn't draw, the neutral fill with
/// "Zen mode off" in the same shape. A click toggles Zen.
fn render_zen_pill<H: SidebarHost>(
    zen: ZenModeState,
    theme: &Theme,
    cx: &mut Context<H>,
) -> impl IntoElement {
    let on = matches!(zen, ZenModeState::On);
    let id = "sidebar-zen";
    let d = div()
        .id(id)
        .debug_selector(move || id.into())
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(ZEN_PILL_H))
        .px_2()
        .py_1()
        .rounded(px(ZEN_PILL_RADIUS))
        .overflow_hidden()
        .bg(theme.canvas)
        .cursor_pointer();
    let fills = if on {
        [theme.zen, theme.success_fill_hover, theme.success_fill_active]
    } else {
        [theme.background, theme.background_hover, theme.background_active]
    };
    fill_layer(d, id, px(ZEN_PILL_RADIUS), fills)
        .when(on, |d| d.child(div().debug_selector(|| "sidebar-zen-band".into()).absolute().size_0()))
        .child(
            div()
                .type_style(Type::SmallMedium)
                .text_color(if on { theme.success } else { theme.foreground })
                .child(if on { "Zen mode on" } else { "Zen mode off" }),
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.sidebar_event(SidebarEvent::ToggleZen, window, cx)
        }))
}

/// The footer's status row, worded as the TUI's
/// ([`SidebarModel::footer_status`]): the orchestrator supervisor's failure in
/// red while there is one, else the status line in yellow. `None` hides the
/// row.
/// The shared board banner, kept only when the board needs a look: a stale
/// index (`board 4m stale · quota resets 16:03`) or a cache read with no
/// daemon (`board 4m cached · no daemon`). A warm board's banner is just its
/// age (`board 12s`), a refresh timer the desktop doesn't show; the TUI's
/// footer still does. The feed applies this too (`refresh::sidebar_reader`),
/// so the ticking age never changes the model and repaints an idle window.
pub fn board_warning(banner: Option<String>) -> Option<String> {
    banner.filter(|b| b.contains("stale") || b.contains("no daemon"))
}

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

    fn review(id: &str, state: ReviewState, number: Option<i64>) -> ReviewRow {
        ReviewRow {
            task_id: id.into(),
            title: format!("Task {id}"),
            branch: format!("jlong/{id}"),
            location: (state == ReviewState::Serving).then(|| "hub:4101".into()),
            workspace: None,
            state,
            number,
        }
    }

    /// The component's sample: one served review, two queued, four
    /// workspaces (two working).
    fn model() -> SidebarModel {
        let mut m = SidebarModel::skeleton("demo");
        m.board_loading = false;
        m.workspaces = vec![
            ws("alpha", "hub", Some("T-1"), WorkspaceBadge::Working),
            ws("bravo", "hub", None, WorkspaceBadge::Idle),
            ws("charlie", "hub", Some("T-2"), WorkspaceBadge::AwaitingInput),
            ws("delta", "hub", None, WorkspaceBadge::Idle),
        ];
        m.reviews = vec![
            review("R-1", ReviewState::Serving, Some(401)),
            review("Q-1", ReviewState::Pending, Some(402)),
            review("Q-2", ReviewState::Pending, None),
        ];
        m
    }

    fn selectors(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|e| row_selector(&e.row)).collect()
    }

    #[test]
    fn rows_know_the_body_they_fold_with() {
        let mut m = model();
        m.workspaces.push(ws("echo", "box", Some("T-3"), WorkspaceBadge::Working));
        let entries = layout(&m, &BTreeSet::new());
        let body = |sel: &str| {
            let e = entries.iter().find(|e| row_selector(&e.row) == sel).unwrap();
            (e.section, e.machine.clone())
        };
        assert_eq!(body("sidebar-section-Queued for Review"), (None, None), "a header isn't in its body");
        assert_eq!(body("sidebar-review-Q-1"), (Some(Section::Queued), None));
        assert_eq!(body("sidebar-machine-box"), (Some(Section::Workspaces), None));
        assert_eq!(body("sidebar-ws-echo"), (Some(Section::Workspaces), Some("box".into())));
        assert_eq!(body("sidebar-ws-alpha"), (Some(Section::Workspaces), Some("hub".into())));
    }

    #[test]
    fn a_closing_body_stays_drawn_until_its_fold_ends() {
        let mut m = model();
        m.workspaces.push(ws("echo", "box", Some("T-3"), WorkspaceBadge::Working));
        let t0 = Instant::now();
        let mut folds = SidebarFolds::default();
        let collapsed = BTreeSet::from([Section::Queued]);
        folds.toggle(Body::Section(Section::Queued), true, t0, false);
        m.collapsed_machines.insert("hub".into());
        folds.toggle(Body::Machine("hub".into()), true, t0, false);

        let mid = drawn(&m, &collapsed, &folds, t0 + std::time::Duration::from_millis(50));
        let sel = selectors(&mid);
        assert!(sel.contains(&"sidebar-review-Q-1".to_string()), "closing, still drawn");
        assert!(sel.contains(&"sidebar-ws-alpha".to_string()), "the machine's rows too");
        let head = mid.iter().find_map(|e| e.head.clone().filter(|h| h.section == Section::Queued)).unwrap();
        assert!(head.collapsed, "the header says collapsed");
        let group = mid.iter().find(|e| row_selector(&e.row) == "sidebar-machine-hub").unwrap();
        assert!(matches!(group.row, SidebarRow::MachineGroup { collapsed: true, .. }));
        // The chords are the collapsed layout's: none on the closing
        // machine's rows, so the next machine's row takes ⌘1 (not ⌘3).
        let chord = |sel: &str| mid.iter().find(|e| row_selector(&e.row) == sel).and_then(|e| e.shortcut);
        assert_eq!(chord("sidebar-ws-alpha"), None);
        assert_eq!(chord("sidebar-ws-echo"), Some(1));
        assert_eq!(chord("sidebar-review-Q-1"), Some(3), "a section's rows keep theirs");
        // Only what is collapsed counts as closing.
        let closing: Vec<String> = mid
            .iter()
            .filter(|e| e.closing(&collapsed, &m))
            .map(|e| row_selector(&e.row))
            .collect();
        assert_eq!(
            closing,
            ["sidebar-ws-alpha", "sidebar-ws-bravo", "sidebar-ws-charlie", "sidebar-ws-delta", "sidebar-review-Q-1", "sidebar-review-Q-2"]
        );

        let settled = drawn(&m, &collapsed, &folds, t0 + fold::FOLD);
        assert_eq!(selectors(&settled), selectors(&layout(&m, &collapsed)), "then exactly the layout");
    }

    #[test]
    fn width_follows_the_shared_clamp() {
        // The default 28 columns is the component's 280pt.
        assert_eq!(width_px(28, 1200.0), 280.0);
        // Never below the 24-column minimum...
        assert_eq!(width_px(10, 1200.0), 24.0 * COLUMN_PX);
        // ...and never past half the window.
        assert_eq!(width_px(100, 900.0), 45.0 * COLUMN_PX);
        // A drag maps the pointer to columns under the same rule.
        assert_eq!(drag_width_cols(300.0, 1200.0), 30);
        assert_eq!(drag_width_cols(50.0, 1200.0), 24);
        assert_eq!(drag_width_cols(1000.0, 1200.0), 60);
    }

    #[test]
    fn the_traffic_lights_center_where_figma_draws_them() {
        // macOS's 14pt buttons at (19, 19): the close button's circle centers
        // at (26, 26), on the 52pt titlebar row's middle.
        assert_eq!(traffic_light_position(), gpui::point(px(19.0), px(19.0)));
        assert_eq!(TITLEBAR_H / 2.0, 26.0);
        assert_eq!(TRAFFIC_LIGHTS_RIGHT, 79.0);
        assert_eq!(BESIDE_TRAFFIC_LIGHTS, 91.0, "Figma's 12pt gap past the zoom button");
    }

    #[test]
    fn the_layout_keeps_the_shared_section_order() {
        let mut m = model();
        m.workspaces.push(ws("echo", "box", Some("T-3"), WorkspaceBadge::Working));
        let entries = layout(&m, &BTreeSet::new());
        let names = selectors(&entries);
        let at = |s: &str| names.iter().position(|n| n == s).unwrap_or_else(|| panic!("{s}"));
        assert!(at("sidebar-nav-Changelog") < at("sidebar-section-Workspaces"));
        assert!(at("sidebar-section-Workspaces") < at("sidebar-machine-hub"));
        assert!(at("sidebar-ws-echo") < at("sidebar-section-Ready for Review"));
        assert!(at("sidebar-section-Ready for Review") < at("sidebar-review-R-1"));
        assert!(at("sidebar-review-R-1") < at("sidebar-section-Queued for Review"));
        assert!(at("sidebar-section-Queued for Review") < at("sidebar-review-Q-2"));
        // Row for row the shared model's (the TUI's) order.
        let shared: Vec<String> = m.rows().iter().map(row_selector).collect();
        assert_eq!(names, shared);
        let heads: Vec<Section> = entries.iter().filter_map(|e| e.head.as_ref().map(|h| h.section)).collect();
        assert_eq!(heads, Section::ORDER);
    }

    #[test]
    fn working_workspaces_then_reviews_are_numbered() {
        let entries = layout(&model(), &BTreeSet::new());
        let numbered: Vec<(String, u8)> = entries
            .iter()
            .filter_map(|e| e.shortcut.map(|n| (row_selector(&e.row), n)))
            .collect();
        assert_eq!(
            numbered,
            [
                ("sidebar-ws-alpha".to_string(), 1),
                ("sidebar-ws-charlie".to_string(), 2),
                ("sidebar-review-R-1".to_string(), 3),
                ("sidebar-review-Q-1".to_string(), 4),
                ("sidebar-review-Q-2".to_string(), 5),
            ],
            "idle workspaces take no chord"
        );
    }

    #[test]
    fn only_nine_rows_are_numbered() {
        let mut m = model();
        m.reviews = (0..12).map(|i| review(&format!("Q-{i}"), ReviewState::Pending, None)).collect();
        let entries = layout(&m, &BTreeSet::new());
        let numbers: Vec<u8> = entries.iter().filter_map(|e| e.shortcut).collect();
        assert_eq!(numbers, (1..=9).collect::<Vec<_>>());
    }

    #[test]
    fn headers_count_their_reviews_and_a_collapsed_section_hides_its_rows() {
        let m = model();
        let open = layout(&m, &BTreeSet::new());
        let head = |entries: &[Entry], s: Section| {
            entries.iter().find_map(|e| e.head.clone().filter(|h| h.section == s)).unwrap()
        };
        assert_eq!(head(&open, Section::Ready).count, Some(1));
        assert_eq!(head(&open, Section::Queued).count, Some(2));
        assert_eq!(head(&open, Section::Workspaces).count, None, "the component draws no count");
        assert!(!head(&open, Section::Queued).collapsed);

        let folded = layout(&m, &BTreeSet::from([Section::Queued, Section::Workspaces]));
        let names = selectors(&folded);
        assert!(head(&folded, Section::Queued).collapsed);
        assert_eq!(head(&folded, Section::Queued).count, Some(2), "the count stays");
        assert!(names.contains(&"sidebar-section-Queued for Review".into()));
        assert!(!names.contains(&"sidebar-review-Q-1".into()));
        assert!(!names.contains(&"sidebar-ws-alpha".into()));
        assert!(names.contains(&"sidebar-review-R-1".into()), "Ready stays open");
        // A chord keeps its row while its section is collapsed.
        assert_eq!(
            folded.iter().find(|e| row_selector(&e.row) == "sidebar-review-R-1").unwrap().shortcut,
            Some(3)
        );
    }

    #[test]
    fn the_selection_key_walks_the_laid_out_rows() {
        let entries = layout(&model(), &BTreeSet::new());
        // Selectable: 3 nav, the four workspaces, then R-1, Q-1, Q-2.
        assert_eq!(selected_key(&entries, 0).as_deref(), Some("sidebar-nav-Orchestrator"));
        assert_eq!(selected_key(&entries, 3).as_deref(), Some("sidebar-ws-alpha"));
        assert_eq!(selected_key(&entries, 7).as_deref(), Some("sidebar-review-R-1"));
        assert_eq!(selected_key(&entries, 9).as_deref(), Some("sidebar-review-Q-2"));
        assert_eq!(selected_key(&entries, 99), None);
    }

    #[test]
    fn sections_save_by_key() {
        for s in Section::ORDER {
            assert_eq!(Section::from_key(s.key()), Some(s));
        }
        assert_eq!(Section::from_key("nope"), None);
        assert_eq!(Section::Ready.label(), "Ready for review");
        assert_eq!(Section::Queued.label(), "Queued for review");
    }

    #[test]
    fn the_shared_nav_ends_on_changelog_with_activity_off_it() {
        let shared = shelbi_app::view::default_nav_items();
        assert!(shared.iter().any(|n| n.view == View::Changelog && n.label == "Changelog"));
        assert!(!shared.iter().any(|n| n.view == View::Activity), "Activity is palette-only");
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
    fn row_text_follows_the_component() {
        let m = model();
        let row = |name: &str| {
            m.rows().into_iter().find(|r| row_selector(r) == name).unwrap()
        };
        let alpha = row_text(&row("sidebar-ws-alpha"), &m);
        assert_eq!(alpha.label, "alpha");
        assert_eq!(alpha.right.as_deref(), Some("Developer"));
        assert_eq!(row_text(&row("sidebar-ws-bravo"), &m).right.as_deref(), Some("Idle"));

        let served = row_text(&row("sidebar-review-R-1"), &m);
        assert_eq!(served.label, "Task R-1");
        assert_eq!(served.number.as_deref(), Some("#401"));
        assert_eq!(served.branch.as_deref(), Some("jlong/R-1"));
        assert_eq!(served.right, None, "the serving location is the Review view's");
        assert_eq!(row_text(&row("sidebar-review-Q-2"), &m).number, None, "no tracker number");

        assert_eq!(
            row_text(&row("sidebar-section-Ready for Review"), &m).label,
            "Ready for review"
        );
        assert_eq!(row_text(&row("sidebar-nav-Changelog"), &m).label, "Changelog");

        let collapsed = SidebarRow::MachineGroup {
            name: "devbox".into(),
            collapsed: true,
            total: 2,
            active: 1,
        };
        let text = row_text(&collapsed, &m);
        assert_eq!(text.label, "devbox");
        assert_eq!(text.right.as_deref(), Some("(2, 1 active)"));

        let loading = SidebarRow::Workspace {
            name: "w".into(),
            badge: WorkspaceBadge::Loading,
            right_label: String::new(),
            indent: false,
        };
        assert_eq!(row_text(&loading, &m).right, None, "a row not read yet says nothing");
        assert_eq!(row_text(&SidebarRow::Loading, &m).label, "Loading…");
        assert_eq!(
            row_text(&SidebarRow::ConfigError("bad yaml".into()), &m).label,
            "! bad yaml"
        );
    }

    #[test]
    fn the_state_glyphs_are_the_components() {
        let t = theme();
        assert_eq!(
            workspace_glyph(WorkspaceBadge::Working, &t),
            StateGlyph::Drawn(glyphs::PLAY, t.icon_green)
        );
        assert_eq!(
            workspace_glyph(WorkspaceBadge::AwaitingInput, &t),
            StateGlyph::Question(t.icon_yellow)
        );
        assert!(matches!(
            workspace_glyph(WorkspaceBadge::Idle, &t),
            StateGlyph::Drawn(g, _) if g == glyphs::DOT
        ));
        assert_eq!(
            review_glyph(ReviewState::Serving, &t),
            StateGlyph::Drawn(glyphs::CHECK, t.icon_cyan)
        );
        assert!(matches!(
            review_glyph(ReviewState::Pending, &t),
            StateGlyph::Drawn(g, _) if g == glyphs::DOT
        ));
        // States the component doesn't draw keep the shared glyph.
        assert!(matches!(
            workspace_glyph(WorkspaceBadge::AwaitingPermission, &t),
            StateGlyph::Text(g, _) if g == "⚠"
        ));
    }

    #[test]
    fn nav_icons_are_heroicons() {
        assert_eq!(nav_icon(&View::Session("orch".into())), icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS);
        assert_eq!(nav_icon(&View::Issues), icons::RECTANGLE_STACK);
        assert_eq!(nav_icon(&View::Changelog), icons::ROCKET_LAUNCH);
        assert_eq!(nav_icon(&View::Activity), icons::BOLT);
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
        collapsed: BTreeSet<Section>,
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
            let collapsed = self.collapsed.clone();
            render(
                SidebarProps {
                    model: &self.model,
                    collapsed: &collapsed,
                    folds: &SidebarFolds::default(),
                    now: Instant::now(),
                    selection: Some(self.selection),
                    link: self.link,
                    palette_chord: "⌃P".into(),
                    switcher: None,
                    switcher_button: Default::default(),
                    switcher_open: false,
                    width: 280.0,
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
            number: Some(401),
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
        open_collapsed(cx, model, BTreeSet::new(), selection, link)
    }

    fn open_collapsed(
        cx: &mut TestAppContext,
        model: SidebarModel,
        collapsed: BTreeSet<Section>,
        selection: usize,
        link: Option<DaemonLink>,
    ) -> (gpui::Entity<Fixture>, VisualTestContext) {
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| Fixture {
                    model,
                    collapsed,
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
            "sidebar-search",
            "sidebar-collapse",
            "sidebar-project-title",
            "sidebar-nav-Orchestrator",
            "sidebar-nav-Issues",
            "sidebar-nav-Changelog",
            "sidebar-section-Workspaces",
            "sidebar-machine-hub",
            "sidebar-machine-devbox",
            "sidebar-ws-alpha",
            "sidebar-ws-bravo",
            "sidebar-ws-delta",
            "sidebar-section-Ready for Review",
            "sidebar-review-T-9",
            "sidebar-zen",
            "sidebar-zen-band",
            "sidebar-errors",
            "sidebar-settings",
        ] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        // Nothing the model doesn't have: no queued section, no polling
        // indicator while live, no machines nav row, and a matching version
        // stays in the Settings tooltip.
        for absent in [
            "sidebar-section-Queued for Review",
            "sidebar-polling",
            "sidebar-loading",
            "sidebar-nav-Machines",
            "sidebar-version",
        ] {
            assert!(cx.debug_bounds(absent).is_none(), "{absent} is not drawn");
        }

        // Rows stack in the component's order.
        let y = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap().top();
        let order = [
            "sidebar-project-title",
            "sidebar-nav-Orchestrator",
            "sidebar-nav-Changelog",
            "sidebar-section-Workspaces",
            "sidebar-machine-hub",
            "sidebar-ws-alpha",
            "sidebar-ws-bravo",
            "sidebar-machine-devbox",
            "sidebar-ws-delta",
            "sidebar-section-Ready for Review",
            "sidebar-review-T-9",
            "sidebar-settings",
            "sidebar-zen",
        ];
        for pair in order.windows(2) {
            assert!(y(&mut cx, pair[0]) < y(&mut cx, pair[1]), "{} above {}", pair[0], pair[1]);
        }
    }

    #[gpui::test]
    fn the_component_geometry(cx: &mut TestAppContext) {
        let (_, mut cx) = open(cx, fixture_model(), 0, None);
        let b = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap();
        // The nav: 12pt in, 34pt items, 4pt apart.
        let orch = b(&mut cx, "sidebar-nav-Orchestrator");
        let issues = b(&mut cx, "sidebar-nav-Issues");
        assert_eq!(orch.left().as_f32(), 12.0);
        assert_eq!(orch.size.width.as_f32(), 256.0);
        assert_eq!(orch.size.height.as_f32(), 34.0);
        assert_eq!((issues.top() - orch.bottom()).as_f32(), 4.0);
        // The titlebar buttons: 32pt squares, 8pt from the right edge.
        let toggle = b(&mut cx, "sidebar-collapse");
        assert_eq!(toggle.size.width.as_f32(), 32.0);
        assert_eq!(toggle.right().as_f32(), 272.0);
        // The project trigger: 28pt.
        assert_eq!(b(&mut cx, "sidebar-project-title").size.height.as_f32(), 28.0);
        // A two-line review row: 42pt; a workspace row: 26pt.
        assert_eq!(b(&mut cx, "sidebar-review-T-9").size.height.as_f32(), 42.0);
        assert_eq!(b(&mut cx, "sidebar-ws-bravo").size.height.as_f32(), 26.0);
        // The Zen pill: full width inside 8pt, 24pt tall, 8pt under the
        // footer's buttons and 8pt off the foot.
        let zen = b(&mut cx, "sidebar-zen");
        assert_eq!(zen.left().as_f32(), 8.0);
        assert_eq!(zen.size.width.as_f32(), 264.0);
        assert_eq!(zen.size.height.as_f32(), 24.0);
        let sidebar = b(&mut cx, "sidebar");
        assert_eq!((sidebar.bottom() - zen.bottom()).as_f32(), 8.0);
        let settings = b(&mut cx, "sidebar-settings");
        assert_eq!((zen.top() - settings.bottom()).as_f32(), 8.0);
    }

    #[gpui::test]
    fn a_machine_rows_chevron_follows_its_name(cx: &mut TestAppContext) {
        // A collapsed group, so it shows its count.
        let mut model = fixture_model();
        model.collapsed_machines.insert("devbox".into());
        let (_, mut cx) = open(cx, model, 0, None);
        let b = |cx: &mut VisualTestContext, s: &'static str| cx.debug_bounds(s).unwrap_or_else(|| panic!("{s}"));
        let row = b(&mut cx, "sidebar-machine-devbox");
        let name = b(&mut cx, "sidebar-machine-name-devbox");
        let chevron = b(&mut cx, "sidebar-machine-chevron-devbox");
        let count = b(&mut cx, "sidebar-machine-count-devbox");
        assert_eq!((name.left() - row.left()).as_f32(), 8.0, "the name leads the row");
        assert_eq!((chevron.left() - name.right()).as_f32(), 4.0, "the chevron 4pt past the name");
        assert_eq!(chevron.size.width.as_f32(), 12.0);
        assert_eq!((row.right() - count.right()).as_f32(), 8.0, "the count at the far right");
        assert!(chevron.right() < count.left());
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
        assert!(cx.debug_bounds("sidebar-zen").is_some(), "the pill always shows");
        assert!(cx.debug_bounds("sidebar-zen-band").is_none(), "zen is off");
        assert!(cx.debug_bounds("sidebar-errors").is_none(), "no unread errors");
    }

    #[gpui::test]
    fn a_version_mismatch_shows_in_the_footer(cx: &mut TestAppContext) {
        let mut model = fixture_model();
        model.daemon_version_mismatch = true;
        let (_, mut cx) = open(cx, model, 0, None);
        assert!(cx.debug_bounds("sidebar-version").is_some());
    }

    #[test]
    fn board_warning_drops_the_warm_age_and_keeps_the_warnings() {
        use shelbi_state::{BoardFreshness, BoardSource, ReadPath};
        let freshness = |source, stale| BoardFreshness {
            source,
            stale,
            age_secs: Some(240),
            remaining: None,
            reset: None,
            read_path: ReadPath::Unknown,
        };
        let warning = |f: BoardFreshness| board_warning(f.banner());
        assert_eq!(warning(freshness(BoardSource::Index, false)), None, "warm: a timer");
        assert_eq!(warning(freshness(BoardSource::Local, false)), None);
        assert_eq!(
            warning(freshness(BoardSource::Index, true)).as_deref(),
            Some("board 4m stale")
        );
        assert_eq!(
            warning(freshness(BoardSource::CacheFallback, false)).as_deref(),
            Some("board 4m cached · no daemon")
        );
    }

    #[gpui::test]
    fn the_footer_shows_a_stale_board_but_not_a_warm_one(app: &mut TestAppContext) {
        let mut warm = fixture_model();
        warm.board_banner = Some("board 12s".into());
        let (_, mut cx) = open(app, warm, 0, None);
        assert!(cx.debug_bounds("sidebar-version").is_none(), "no refresh timer");

        let mut stale = fixture_model();
        stale.board_banner = Some("board 4m stale · quota resets 16:03".into());
        let (_, mut cx) = open(app, stale, 0, None);
        assert!(cx.debug_bounds("sidebar-version").is_some(), "the stale board shows");
    }

    #[gpui::test]
    fn the_footer_shows_the_orchestrator_failure(app: &mut TestAppContext) {
        let (_, mut healthy) = open(app, fixture_model(), 0, None);
        assert!(healthy.debug_bounds("sidebar-status").is_none(), "healthy: no status row");

        let mut model = fixture_model();
        model.orchestrator_status = Some("supervisor gave up: spawn refused".into());
        let (_, mut cx) = open(app, model, 0, None);
        let status = cx.debug_bounds("sidebar-status").expect("the failure is drawn");
        let settings = cx.debug_bounds("sidebar-settings").unwrap();
        assert!(status.top() < settings.top(), "above the footer's buttons");
    }

    #[gpui::test]
    fn a_collapsed_section_draws_only_its_header(cx: &mut TestAppContext) {
        let (_, mut cx) = open_collapsed(
            cx,
            fixture_model(),
            BTreeSet::from([Section::Workspaces]),
            0,
            None,
        );
        assert!(cx.debug_bounds("sidebar-section-Workspaces").is_some());
        assert!(cx.debug_bounds("sidebar-ws-alpha").is_none());
        assert!(cx.debug_bounds("sidebar-machine-hub").is_none());
        assert!(cx.debug_bounds("sidebar-review-T-9").is_some());
    }

    #[gpui::test]
    fn clicks_reach_the_host(cx: &mut TestAppContext) {
        let (root, mut cx) = open(cx, fixture_model(), 0, None);
        for selector in [
            "sidebar-ws-alpha",
            "sidebar-nav-Issues",
            "sidebar-settings",
            "sidebar-section-Workspaces",
            "sidebar-zen",
            "sidebar-search",
            "sidebar-collapse",
        ] {
            let at = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_click(at, Modifiers::none());
        }
        let events = root.read_with(&cx, |f, _| f.events.clone());
        assert_eq!(
            events,
            [
                SidebarEvent::Activate(4),
                SidebarEvent::Activate(1),
                SidebarEvent::OpenSettings,
                SidebarEvent::ToggleSection(Section::Workspaces),
                SidebarEvent::ToggleZen,
                SidebarEvent::OpenSearch,
                SidebarEvent::Collapse,
            ]
        );
    }
}
