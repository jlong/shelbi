//! The root view: the sidebar | main area split.
//!
//! Both halves are placeholders in the foundation. The sidebar lists one nav
//! row per [`View`] so each `ShowView` reaches the command dispatch (and its
//! not-implemented banner) through a real click. `dt-shell-sidebar` replaces
//! the sidebar with the real one (projects, nav, workspaces, Zen) and the
//! per-view subtasks fill the main area.

use gpui::{div, prelude::*, px, svg, Context, FontWeight, SharedString, Window};
use shelbi_app::{CommandKind, View};

use crate::assets::icons;
use crate::commands::{self, CommandHost};
use crate::theme::Theme;

/// Sidebar width in the foundation. `dt-shell-sidebar` makes it resizable.
const SIDEBAR_WIDTH: f32 = 240.0;

/// Nav and banner icon size.
const ICON_SIZE: f32 = 16.0;

/// The nav rows: the TUI sidebar's views, in its order, each with its
/// Heroicon.
fn nav_rows() -> [(&'static str, &'static str, View); 4] {
    [
        (
            "Orchestrator",
            icons::CHAT_BUBBLE_LEFT_RIGHT,
            View::Session("orch".into()),
        ),
        ("Issues", icons::VIEW_COLUMNS, View::Issues),
        ("Activity", icons::BOLT, View::Activity),
        ("Machines", icons::SERVER_STACK, View::Machines),
    ]
}

/// The window's root view.
pub struct ShelbiWindow {
    /// The nav row last clicked, painted with the selection fill.
    selected: Option<View>,
    /// The "Not implemented yet" banner, when a stub command ran.
    banner: Option<SharedString>,
}

impl ShelbiWindow {
    pub fn new() -> Self {
        Self {
            selected: None,
            banner: None,
        }
    }

    /// Run a command from the UI and repaint.
    fn run(&mut self, kind: CommandKind, cx: &mut Context<Self>) {
        if let CommandKind::ShowView(view) = &kind {
            self.selected = Some(view.clone());
        }
        commands::dispatch_command(&kind, self);
        cx.notify();
    }

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = nav_rows()
            .into_iter()
            .enumerate()
            .map(|(ix, (label, icon, view))| {
                let selected = self.selected.as_ref() == Some(&view);
                let color = if selected {
                    theme.text_selected
                } else {
                    theme.text
                };
                div()
                    .id(("nav", ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| {
                        row.bg(theme.selection_bg)
                            .text_color(theme.text_selected)
                            .font_weight(FontWeight::BOLD)
                    })
                    .when(!selected, |row| row.text_color(theme.text))
                    .child(
                        svg()
                            .path(icon)
                            .size(px(ICON_SIZE))
                            .flex_none()
                            .text_color(color),
                    )
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run(CommandKind::ShowView(view.clone()), cx)
                    }))
            });

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .p_3()
            // Clear the macOS traffic lights, which sit over the sidebar.
            .pt(px(36.0))
            .bg(theme.background)
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_color(theme.accent)
                    .font_weight(FontWeight::BOLD)
                    .child("Shelbi"),
            )
            .children(rows)
    }

    fn render_main(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .bg(theme.main_background)
            .when_some(self.banner.clone(), |main, message| {
                main.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_4()
                        .py_2()
                        .bg(theme.selection_bg)
                        .text_color(theme.status_yellow)
                        .child(message)
                        .child(
                            div()
                                .id("dismiss-banner")
                                .cursor_pointer()
                                .child(
                                    svg()
                                        .path(icons::X_MARK)
                                        .size(px(ICON_SIZE))
                                        .text_color(theme.muted),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.banner = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted)
                    .child("Shelbi desktop foundation: pick a view in the sidebar."),
            )
    }
}

impl Default for ShelbiWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandHost for ShelbiWindow {
    fn not_implemented(&mut self, command_id: &str) {
        self.banner = Some(commands::not_implemented_message(command_id).into());
    }
}

impl Render for ShelbiWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        div()
            .flex()
            .flex_row()
            .size_full()
            .text_sm()
            .child(self.render_sidebar(&theme, cx))
            // The sidebar's right-edge divider (`DIVIDER_DIM`).
            .child(div().w(px(1.0)).h_full().flex_none().bg(theme.divider_dim))
            .child(self.render_main(&theme, cx))
    }
}
