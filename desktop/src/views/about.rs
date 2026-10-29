//! The About Shelbi window (`dt-about-bundled-software`, Figma "About
//! Shelbi · Dark", 2431:8310) and the two small windows' lifecycle: About
//! and Acknowledgements ([`super::bundled_software`]) each open once, and
//! asking again brings the open one to the front.
//!
//! About is a fixed 400 × 520 window with only the traffic lights (not
//! resizable, so no zoom, and no minimize): the app icon at 112pt (the one
//! `desktop.app_icon` chooses, following a change while open), the name
//! and this build's version, the tagline, the project's links,
//! Acknowledgements, and the copyright and license. Every link opens in the
//! browser except Acknowledgements, which opens its window. ⌘W closes it.
//! The words and links are [`shelbi_app::about`]'s, which the TUI's About
//! overlay reads too.

use gpui::{
    div, img, prelude::*, px, App, Bounds, ClickEvent, Context, FocusHandle, FontWeight, Global,
    SharedString, Stateful, Div, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions,
};

use shelbi_app::about::{self, AboutLink};

use crate::app::CloseWindow;
use crate::app_icon::{self, AppIconSetting};
use crate::platform::version::APP_VERSION;
use crate::theme::style::{Type, Typography};
use crate::theme::{self, Theme};
use crate::views::bundled_software::BundledSoftwareView;
use crate::views::sidebar;

/// The window's size (the frame's).
pub const SIZE: (f32, f32) = (400.0, 520.0);
/// The app icon's size (drawn from a picture twice that, so it stays sharp
/// on a Retina display: [`app_icon::about_image`]).
pub const ICON: f32 = 112.0;

/// The open About and Acknowledgements windows (a closed one's handle
/// fails to update, so it opens anew).
#[derive(Default)]
struct Windows {
    about: Option<WindowHandle<AboutView>>,
    bundled: Option<WindowHandle<BundledSoftwareView>>,
}

impl Global for Windows {}

/// Bring `handle`'s window to the front, if it is still open.
fn raise<V: 'static>(handle: Option<WindowHandle<V>>, cx: &mut App) -> bool {
    handle.is_some_and(|h| h.update(cx, |_, window, _| window.activate_window()).is_ok())
}

/// A small window's options: centered, a transparent titlebar under the
/// traffic lights as the main window's.
pub fn options(title: &str, size: (f32, f32), resizable: bool, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            gpui::size(px(size.0), px(size.1)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_string().into()),
            appears_transparent: true,
            traffic_light_position: Some(sidebar::traffic_light_position()),
        }),
        is_resizable: resizable,
        is_minimizable: resizable,
        window_min_size: resizable.then(|| gpui::size(px(super::bundled_software::MIN_SIZE.0), px(super::bundled_software::MIN_SIZE.1))),
        app_id: Some("co.32pixels.shelbi".into()),
        ..Default::default()
    }
}

/// Shelbi ▸ About Shelbi: the About window, or the open one to the front.
pub fn show_about(cx: &mut App) {
    if !cx.has_global::<Windows>() {
        cx.set_global(Windows::default());
    }
    if raise(cx.global::<Windows>().about, cx) {
        return;
    }
    let opened = cx.open_window(options("About Shelbi", SIZE, false, cx), |window, cx| {
        cx.new(|cx| AboutView::new(window, cx))
    });
    match opened {
        Ok(handle) => cx.global_mut::<Windows>().about = Some(handle),
        Err(e) => tracing::error!(error = %e, "could not open About Shelbi"),
    }
}

/// About's Acknowledgements: its window, or the open one to the front.
pub fn show_bundled_software(cx: &mut App) {
    if !cx.has_global::<Windows>() {
        cx.set_global(Windows::default());
    }
    if raise(cx.global::<Windows>().bundled, cx) {
        return;
    }
    let size = super::bundled_software::SIZE;
    let opened = cx.open_window(options("Acknowledgements", size, true, cx), |window, cx| {
        cx.new(|cx| BundledSoftwareView::new(window, cx))
    });
    match opened {
        Ok(handle) => cx.global_mut::<Windows>().bundled = Some(handle),
        Err(e) => tracing::error!(error = %e, "could not open Acknowledgements"),
    }
}

/// Open `url` in the browser. A test records it instead.
pub fn open_url(url: &str, cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
        tests::OPENED.with(|o| o.borrow_mut().push(url.to_string()));
    }
    #[cfg(not(test))]
    cx.open_url(url);
}

/// A link: `label` in `color`, the hovered-link color under the pointer.
pub fn link(id: impl Into<SharedString>, label: impl Into<SharedString>, color: gpui::Hsla, theme: &Theme) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let hover = theme.link_hover;
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .cursor_pointer()
        .text_color(color)
        .hover(move |s| s.text_color(hover))
        .child(label.into())
}

/// A link that opens its page.
fn url_link(id: &'static str, target: AboutLink, color: gpui::Hsla, theme: &Theme) -> Stateful<Div> {
    let url = target.url();
    link(id, target.label(), color, theme).on_click(move |_: &ClickEvent, _, cx| {
        if let Some(url) = url {
            open_url(url, cx)
        }
    })
}

/// The frames' heading (`Desktop/Heading` as they draw it: 18 / 28,
/// semibold).
pub fn heading_type<E: Styled>(el: E) -> E {
    el.type_style(Type::Heading).text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD)
}

/// The About Shelbi window.
pub struct AboutView {
    focus: FocusHandle,
    _appearance: Subscription,
    /// Settings picked another app icon: show it.
    _icon: Subscription,
}

impl AboutView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        AboutView {
            focus,
            _appearance: theme::follow_system(window, cx),
            _icon: cx.observe_global::<AppIconSetting>(|_, cx| cx.notify()),
        }
    }
}

impl Render for AboutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let identity = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.0))
            .child(heading_type(div()).text_color(theme.text_loud).child(about::NAME))
            .child(
                div()
                    .debug_selector(|| "about-version".into())
                    .type_style(Type::Small)
                    .text_color(theme.foreground)
                    .child(about::version_line(APP_VERSION)),
            );
        let links = div()
            .flex()
            .flex_row()
            .justify_center()
            .gap(px(24.0))
            .type_style(Type::SmallMedium)
            .child(url_link("about-website", AboutLink::Website, theme.link, &theme))
            .child(url_link("about-docs", AboutLink::Documentation, theme.link, &theme))
            .child(url_link("about-github", AboutLink::GitHub, theme.link, &theme));
        let bundled = link("about-bundled", AboutLink::Acknowledgements.label(), theme.link, &theme)
            .type_style(Type::SmallMedium)
            .on_click(|_: &ClickEvent, _, cx| show_bundled_software(cx));
        let credits = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.0))
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .child(about::copyright())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap(px(8.0))
                    .child(url_link("about-license", AboutLink::License, theme.muted, &theme))
                    .child("·")
                    .child(url_link("about-sponsor", AboutLink::Sponsor, theme.muted, &theme)),
            );
        div()
            .id("about")
            .track_focus(&self.focus)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(24.0))
            .p(px(32.0))
            .bg(theme.canvas)
            .font_family(crate::theme::style::UI_FAMILY)
            .child(
                img(app_icon::about_image(app_icon::current(cx)))
                    .debug_selector(|| "about-icon".into())
                    .size(px(ICON))
                    .flex_none(),
            )
            .child(identity)
            .child(
                div()
                    .type_style(Type::Base)
                    .text_color(theme.foreground)
                    .text_center()
                    .child(about::TAGLINE),
            )
            .child(links)
            .child(bundled)
            .child(credits)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    thread_local! {
        /// The URLs [`open_url`] was asked to open, in order.
        pub(crate) static OPENED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(
                Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark),
                cx,
            );
        });
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} isn't drawn"));
        cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    }

    #[gpui::test]
    fn about_shows_this_builds_version_and_its_links_open(cx: &mut TestAppContext) {
        init(cx);
        OPENED.with(|o| o.borrow_mut().clear());
        let window = cx.add_window(AboutView::new);
        let cx = &mut VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        assert!(cx.debug_bounds("about-version").is_some());
        assert_eq!(cx.debug_bounds("about-icon").unwrap().size, gpui::size(px(ICON), px(ICON)));
        for selector in ["about-website", "about-docs", "about-github", "about-license", "about-sponsor"] {
            click(cx, selector);
        }
        let opened = OPENED.with(|o| o.borrow().clone());
        assert_eq!(
            opened,
            [about::WEBSITE_URL, about::DOCS_URL, about::GITHUB_URL, about::LICENSE_URL, about::SPONSOR_URL]
        );
    }

    #[gpui::test]
    fn about_and_bundled_software_open_once_each(cx: &mut TestAppContext) {
        init(cx);
        let before = cx.update(|cx| cx.windows().len());
        cx.update(show_about);
        cx.update(show_about);
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before + 1, "About again raises the open one");

        // Acknowledgements from About's link, then again.
        let about = cx.update(|cx| cx.global::<Windows>().about).unwrap();
        let vcx = &mut VisualTestContext::from_window(about.into(), cx);
        vcx.run_until_parked();
        click(vcx, "about-bundled");
        click(vcx, "about-bundled");
        vcx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before + 2);
        assert!(cx.update(|cx| cx.global::<Windows>().bundled).is_some());

        // ⌘W closes About; About again opens a new one.
        about.update(cx, |_, window, cx| window.dispatch_action(Box::new(CloseWindow), cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before + 1, "⌘W closed About");
        cx.update(show_about);
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before + 2);
    }
}
