//! The settings sidebar (`dt-settings-sections`): the Navigation component's
//! State=Settings (2252:27532), which stands in the app sidebar's column
//! while Settings is open, as the task panels do (`column_swap`).
//!
//! Top to bottom: the sidebar's titlebar (the traffic lights, Search and
//! Toggle sidebar), the back button beside the Settings title, then each
//! section: its header and its pages' nav rows, numbered ⌘1 to ⌘N. Only the
//! Global section exists (General, Keybindings, Terminal); the frame's
//! project section (the project switcher, Workflows, Agents, Integrations)
//! joins [`SECTIONS`] once those pages exist, after a divider. No footer:
//! the frame draws no Zen pill and no settings gear.
//!
//! [`SettingsNav`] is the toolkit-free part: the page shown and the row the
//! keys moved to.

use gpui::{div, prelude::*, px, AnyElement, Bounds, Context, FontWeight, Pixels};

use super::sidebar::{nav_item, render_titlebar, SidebarEvent, SidebarHost};
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
}

impl SettingsPage {
    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::General => "General",
            SettingsPage::Keybindings => "Keybindings",
            SettingsPage::Terminal => "Terminal",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            SettingsPage::General => icons::WRENCH_SCREWDRIVER,
            SettingsPage::Keybindings => icons::KEYBOARD_KEY,
            SettingsPage::Terminal => icons::COMMAND_LINE,
        }
    }

    /// Its ⌘N while Settings is open. Fixed per page rather than counted:
    /// the frame numbers the project pages 4, 5 and 7.
    pub fn shortcut(self) -> u8 {
        match self {
            SettingsPage::General => 1,
            SettingsPage::Keybindings => 2,
            SettingsPage::Terminal => 3,
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
        }
    }
}

/// A group of pages under a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub header: &'static str,
    pub pages: &'static [SettingsPage],
}

/// The sidebar's sections, in order.
pub const SECTIONS: &[Section] = &[Section {
    header: "Global",
    pages: &[
        SettingsPage::General,
        SettingsPage::Keybindings,
        SettingsPage::Terminal,
    ],
}];

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

/// The settings sidebar. `chip` is the sliding selection while it moves
/// (measured on `track`).
pub fn render<H: SidebarHost>(
    nav: SettingsNav,
    chip: Option<Bounds<Pixels>>,
    track: &SharedTrack,
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
            div()
                .flex()
                .flex_col()
                .flex_none()
                .child(section_header(section.header, theme))
                .child(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .gap(px(4.0))
                        .px(px(12.0))
                        .py(px(8.0))
                        .child(motion::origin_probe(track))
                        .children(chip.map(|b| {
                            motion::floating_chip(b, |d| {
                                style::nav_chip(d.rounded(px(NAV_RADIUS)), theme)
                            })
                        }))
                        .children(rows),
                )
        })
        .collect::<Vec<_>>();
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
        .children(sections)
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
                SettingsPage::Terminal
            ]
        );
        assert_eq!(
            all.iter().map(|p| p.shortcut()).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(SettingsPage::for_shortcut(3), Some(SettingsPage::Terminal));
        assert_eq!(SettingsPage::for_shortcut(4), None);
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
        nav.step(true);
        nav.step(true);
        nav.step(true);
        assert_eq!(
            nav.selected(),
            SettingsPage::Terminal,
            "stops at the bottom"
        );
        assert_eq!(nav.page(), SettingsPage::General, "moving doesn't open");
        assert!(nav.activate());
        assert_eq!(
            (nav.page(), nav.selected()),
            (SettingsPage::Terminal, SettingsPage::Terminal)
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
