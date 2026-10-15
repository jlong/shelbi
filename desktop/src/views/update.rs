//! The new-release notice: a bar atop every project window's main area when
//! `platform::update` found a newer Shelbi, naming the Homebrew command that
//! upgrades the app. Dismissing it hides it in every window until the next
//! launch (the check itself runs at most once a day).

use gpui::{div, prelude::*, AnyElement, Global, IntoElement};

use crate::assets::{heroicon, icons};
use crate::platform::update::UpdateNotice;
use crate::theme::Theme;
use crate::views::bootstrap::{command_box, APP_UPGRADE_COMMAND};

/// The app-wide result of the launch's update check.
#[derive(Debug, Default)]
pub struct UpdateState {
    pub notice: Option<UpdateNotice>,
    pub dismissed: bool,
}

impl Global for UpdateState {}

impl UpdateState {
    /// The notice to show, if any.
    pub fn visible(&self) -> Option<&UpdateNotice> {
        self.notice.as_ref().filter(|_| !self.dismissed)
    }
}

/// Run the check off the UI thread and, when it finds a newer release,
/// publish it to every window.
pub fn spawn_check(cx: &mut gpui::App) {
    cx.set_global(UpdateState::default());
    let check = cx
        .background_executor()
        .spawn(async { crate::platform::update::check_now() });
    cx.spawn(async move |cx| {
        let Some(notice) = check.await else { return };
        tracing::info!(latest = %notice.latest, "a newer Shelbi release is available");
        cx.update(|cx| {
            cx.global_mut::<UpdateState>().notice = Some(notice);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// The bar: what's new, the upgrade command (copyable), and a dismiss button.
pub fn render_update_bar(
    notice: &UpdateNotice,
    theme: &Theme,
    on_dismiss: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id("update-bar")
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_2()
        .bg(theme.selection)
        .text_color(theme.white)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    heroicon(icons::ARROW_DOWN_CIRCLE).text_color(theme.white),
                )
                .child(notice.message())
                .child(div().text_color(theme.muted).child("Upgrade with"))
                .child(command_box(0, APP_UPGRADE_COMMAND, theme)),
        )
        .child(
            div()
                .id("dismiss-update")
                .flex_none()
                .cursor_pointer()
                .child(
                    heroicon(icons::X_MARK).text_color(theme.muted),
                )
                .on_click(move |_, window, cx| on_dismiss(window, cx)),
        )
        .into_any_element()
}
