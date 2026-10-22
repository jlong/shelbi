//! The settings sidebar (`dt-settings-sections`): the Navigation component's
//! State=Settings (2252:27532), which stands in the app sidebar's column
//! while Settings is open, as the task panels do (`column_swap`).
//!
//! Top to bottom: the sidebar's titlebar (the traffic lights, Search and
//! Toggle sidebar), the back button beside the Settings title, then each
//! section: its header and its pages' nav rows, each with its ⌘N. The Global
//! section (General, Keybindings, Terminal), then, after a divider, the
//! project section (`dt-wf-settings-index`, frame 2301:5824): the project
//! switcher, the same trigger and menu as the app sidebar's (switching
//! reopens Settings on the new project), over the project's pages: Workflows
//! and Agents (`dt-agents-settings-page`, frame 2377:41040). The frames'
//! Workspaces and Integrations join [`SECTIONS`] with their pages. No footer: the frame draws no Zen pill and
//! no settings gear.
//!
//! [`SettingsNav`] is the toolkit-free part: the page shown and the row the
//! keys moved to.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{div, prelude::*, px, AnyElement, Bounds, Context, FontWeight, Pixels, SharedString};

use super::sidebar::{
    nav_item, render_titlebar, switcher_menu, switcher_trigger, SidebarEvent, SidebarHost, SwitcherProps,
    MENU_GAP, SWITCHER_H,
};
use super::task_panel;
use crate::assets::{icons, Icon};
use crate::keys::row_hotkey;
use crate::theme::motion::{self, SharedTrack};
use crate::theme::style::{self, Type, Typography, NAV_RADIUS};
use crate::theme::Theme;

/// The Settings title's size.
const TITLE_SIZE: f32 = 18.0;

/// A page of Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsPage {
    /// Theme, the Shelbi CLI, and notifications.
    #[default]
    General,
    /// `keys.yaml`.
    Keybindings,
    /// The terminals' font, size, and Option key.
    Terminal,
    /// The project's workflows and status catalog.
    Workflows,
    /// The project's agents and their shared instructions.
    Agents,
}

impl SettingsPage {
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Keybindings => "Keybindings",
            SettingsPage::Terminal => "Terminal",
            SettingsPage::Workflows => "Workflows",
            SettingsPage::Agents => "Agents",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            SettingsPage::General => icons::WRENCH_SCREWDRIVER,
            SettingsPage::Keybindings => icons::KEYBOARD_KEY,
            SettingsPage::Terminal => icons::COMMAND_LINE,
            SettingsPage::Workflows => icons::VIEW_COLUMNS,
            SettingsPage::Agents => icons::RECTANGLE_STACK,
        }
    }

    /// Its ⌘N while Settings is open. Fixed per page rather than counted:
    /// the frames number Workflows 4 and Agents 6 (and give Workspaces
    /// Terminal's 3; 5 is left to whichever page takes it).
    pub fn shortcut(self) -> u8 {
        match self {
            SettingsPage::General => 1,
            SettingsPage::Keybindings => 2,
            SettingsPage::Terminal => 3,
            SettingsPage::Workflows => 4,
            SettingsPage::Agents => 6,
        }
    }

    /// The page ⌘`n` opens.
    pub fn for_shortcut(n: u8) -> Option<SettingsPage> {
        pages().find(|p| p.shortcut() == n)
    }

    /// Its nav row's selector.
    pub fn selector(self) -> &'static str {
        match self {
            SettingsPage::General => "settings-nav-general",
            SettingsPage::Keybindings => "settings-nav-keybindings",
            SettingsPage::Terminal => "settings-nav-terminal",
            SettingsPage::Workflows => "settings-nav-workflows",
            SettingsPage::Agents => "settings-nav-agents",
        }
    }
}

/// What heads a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    /// A title.
    Title(&'static str),
    /// The project switcher, after a divider: the section's pages edit the
    /// shown project.
    Project,
}

/// A group of pages under a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub header: Header,
    pub pages: &'static [SettingsPage],
}

/// The sidebar's sections, in order.
pub const SECTIONS: &[Section] = &[
    Section {
        header: Header::Title("Global"),
        pages: &[
            SettingsPage::General,
            SettingsPage::Keybindings,
            SettingsPage::Terminal,
        ],
    },
    Section {
        header: Header::Project,
        pages: &[SettingsPage::Workflows, SettingsPage::Agents],
    },
];

/// The project section's switcher, as one frame draws it.
pub struct ProjectSwitcher<'a> {
    /// The shown project's name.
    pub label: SharedString,
    /// The menu while it shows (opening, open or closing).
    pub menu: Option<SwitcherProps<'a>>,
    /// Where the trigger lands, for the menu's outside click (the app
    /// sidebar's: one switcher per window).
    pub button: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The menu is open (the trigger holds its pressed fill).
    pub open: bool,
    /// The column's width, which the menu spans.
    pub width: f32,
}

/// Every page, in the order the sidebar draws (and the keys walk) them.
pub fn pages() -> impl Iterator<Item = SettingsPage> {
    SECTIONS.iter().flat_map(|s| s.pages.iter().copied())
}

/// Where the settings sidebar stands: the page shown, and the row the keys
/// moved the selection to (Enter opens it). The page outlives a visit, so
/// Settings reopens on the page last used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettingsNav {
    page: SettingsPage,
    cursor: Option<SettingsPage>,
}

impl SettingsNav {
    /// The page shown.
    pub fn page(&self) -> SettingsPage {
        self.page
    }

    /// The selected row: the keys' row, else the page's.
    pub fn selected(&self) -> SettingsPage {
        self.cursor.unwrap_or(self.page)
    }

    /// Show `page`; the selection goes back to its row.
    pub fn show(&mut self, page: SettingsPage) {
        self.page = page;
        self.cursor = None;
    }

    /// Settings opened: on `page` when one was asked for, else the last.
    pub fn open(&mut self, page: Option<SettingsPage>) {
        self.show(page.unwrap_or(self.page));
    }

    /// Move the selection a row down or up, stopping at the ends.
    pub fn step(&mut self, down: bool) {
        let all: Vec<SettingsPage> = pages().collect();
        let at = all.iter().position(|p| *p == self.selected()).unwrap_or(0);
        let next = if down {
            (at + 1).min(all.len() - 1)
        } else {
            at.saturating_sub(1)
        };
        self.cursor = Some(all[next]);
    }

    /// Open the selected row. Returns whether the page changed.
    pub fn activate(&mut self) -> bool {
        let page = self.selected();
        let changed = page != self.page;
        self.show(page);
        changed
    }
}

/// The section header: Base/Medium, 20pt in, 8pt above and 4pt below.
fn section_header(label: &'static str, theme: &Theme) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(4.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .h(px(28.0))
                .px(px(8.0))
                .type_style(Type::BaseMedium)
                .text_color(theme.foreground)
                .child(label),
        )
}

/// The project section's head (frame 2301:5825): the 1pt divider 12pt in
/// with 8pt above and below, then the switcher's trigger 12pt in, 8pt
/// above and 4pt below, its menu opening under it.
fn project_header<H: SidebarHost>(project: &ProjectSwitcher<'_>, theme: &Theme, cx: &mut Context<H>) -> impl IntoElement {
    let trigger = switcher_trigger(project.label.clone(), project.open, &project.button, theme, cx);
    let menu = project.menu.as_ref().map(|menu| {
        let top = SWITCHER_TOP + SWITCHER_H + MENU_GAP;
        switcher_menu(menu, &project.button, top, project.width, theme, cx)
    });
    div()
        .flex()
        .flex_col()
        .flex_none()
        .child(
            div()
                .flex_none()
                .px(px(12.0))
                .py(px(8.0))
                .child(div().h(px(1.0)).bg(theme.divider)),
        )
        .child(
            div()
                .debug_selector(|| "settings-project".into())
                .relative()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .px(px(12.0))
                .pt(px(SWITCHER_TOP))
                .pb(px(4.0))
                .child(trigger)
                .children(menu),
        )
}

/// The switcher trigger's inset from its row's top.
const SWITCHER_TOP: f32 = 8.0;

/// The settings sidebar. `chip` is the sliding selection while it moves
/// (measured on `track`); `project` is the project section's switcher.
pub fn render<H: SidebarHost>(
    nav: SettingsNav,
    chip: Option<Bounds<Pixels>>,
    track: &SharedTrack,
    project: ProjectSwitcher<'_>,
    palette_chord: &str,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let back = task_panel::back_button("settings-back", false, theme)
        .tooltip(style::tooltip("Back (Esc)"))
        .on_click(cx.listener(|this, _, window, cx| {
            this.sidebar_event(SidebarEvent::LeaveSettings, window, cx)
        }));
    let title = div()
        .flex()
        .flex_row()
        .flex_1()
        .min_w_0()
        .items_center()
        .h(px(24.0))
        .px(px(4.0))
        // The frame's Desktop/Heading reads 18 / 28 semibold here; the
        // app's `Type::Heading` (20 / 28 bold) is left to its other titles.
        .type_style(Type::Heading)
        .text_size(px(TITLE_SIZE))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.foreground)
        .child("Settings");
    let selected = nav.selected();
    let inline = chip.is_none();
    let sections = SECTIONS
        .iter()
        .map(|section| {
            let rows: Vec<_> = section
                .pages
                .iter()
                .map(|&page| {
                    let id = page.selector();
                    let on = page == selected;
                    let painted = style::row_state_rounded(
                        div()
                            .id(id)
                            .debug_selector(move || id.into())
                            .text_color(theme.foreground),
                        id,
                        on,
                        false,
                        inline,
                        px(NAV_RADIUS),
                        theme,
                    )
                    .child(motion::probe(track, id))
                    .when(on, |d| {
                        d.child(
                            div()
                                .debug_selector(|| "settings-nav-selection".into())
                                .absolute()
                                .size_0(),
                        )
                    });
                    // The frame's hotkeys sit at the row's padding, no inset of their own.
                    let hotkey = style::hotkey(&row_hotkey(page.shortcut()), theme)
                        .px_0()
                        .into_any_element();
                    nav_item(painted, page.icon(), page.label(), Some(hotkey), on, theme).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.sidebar_event(SidebarEvent::ShowSettingsPage(page), window, cx)
                        }),
                    )
                })
                .collect();
            let header = match section.header {
                Header::Title(title) => section_header(title, theme).into_any_element(),
                Header::Project => project_header(&project, theme, cx).into_any_element(),
            };
            div()
                .flex()
                .flex_col()
                .flex_none()
                .child(header)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .gap(px(4.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .children(rows),
                )
        })
        .collect::<Vec<_>>();
    // One layer under every section's rows, so the chip slides between
    // sections as within one.
    let sections = div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .child(motion::origin_probe(track))
        .children(chip.map(|b| {
            motion::floating_chip(b, |d| style::nav_chip(d.rounded(px(NAV_RADIUS)), theme))
        }))
        .children(sections);
    div()
        .id("settings-sidebar")
        .debug_selector(|| "settings-sidebar".into())
        .flex()
        .flex_col()
        .size_full()
        .bg(theme.nested(theme.sidebar))
        .text_color(theme.foreground)
        .type_style(Type::Base)
        .child(render_titlebar(palette_chord, theme, cx))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(12.0))
                .px(px(16.0))
                .py(px(8.0))
                .child(back)
                .child(title),
        )
        .child(sections)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pages_are_numbered_in_order() {
        let all: Vec<_> = pages().collect();
        assert_eq!(
            all,
            [
                SettingsPage::General,
                SettingsPage::Keybindings,
                SettingsPage::Terminal,
                SettingsPage::Workflows,
                SettingsPage::Agents
            ]
        );
        assert_eq!(
            all.iter().map(|p| p.shortcut()).collect::<Vec<_>>(),
            [1, 2, 3, 4, 6]
        );
        assert_eq!(SettingsPage::for_shortcut(3), Some(SettingsPage::Terminal));
        assert_eq!(SettingsPage::for_shortcut(4), Some(SettingsPage::Workflows));
        assert_eq!(SettingsPage::for_shortcut(5), None);
        assert_eq!(SettingsPage::for_shortcut(6), Some(SettingsPage::Agents));
        assert_eq!(
            SECTIONS.iter().map(|s| (s.header, s.pages.len())).collect::<Vec<_>>(),
            [(Header::Title("Global"), 3), (Header::Project, 2)]
        );
    }

    #[test]
    fn the_keys_move_the_selection_and_enter_opens_it() {
        let mut nav = SettingsNav::default();
        assert_eq!(
            (nav.page(), nav.selected()),
            (SettingsPage::General, SettingsPage::General)
        );
        nav.step(false);
        assert_eq!(nav.selected(), SettingsPage::General, "stops at the top");
        for _ in 0..5 {
            nav.step(true);
        }
        assert_eq!(
            nav.selected(),
            SettingsPage::Agents,
            "crosses into the project section and stops at the bottom"
        );
        assert_eq!(nav.page(), SettingsPage::General, "moving doesn't open");
        assert!(nav.activate());
        assert_eq!(
            (nav.page(), nav.selected()),
            (SettingsPage::Agents, SettingsPage::Agents)
        );
        assert!(!nav.activate(), "already shown");
    }

    #[test]
    fn settings_reopens_on_the_last_page_unless_one_is_asked_for() {
        let mut nav = SettingsNav::default();
        nav.show(SettingsPage::Keybindings);
        nav.step(true);
        nav.open(None);
        assert_eq!(
            (nav.page(), nav.selected()),
            (SettingsPage::Keybindings, SettingsPage::Keybindings)
        );
        nav.open(Some(SettingsPage::Terminal));
        assert_eq!(nav.page(), SettingsPage::Terminal);
    }
}
