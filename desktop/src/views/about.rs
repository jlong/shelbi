//! The About Shelbi window (`dt-about-bundled-software`, Figma "About
//! Shelbi · Dark", 2431:8310) and the two small windows' lifecycle: About
//! and Bundled Software ([`super::bundled_software`]) each open once, and
//! asking again brings the open one to the front.
//!
//! About is a fixed 400 × 520 window with only the traffic lights (not
//! resizable, so no zoom, and no minimize): the app icon at 112pt, the name
//! and this build's version, the tagline, the project's links, Bundled
//! Software, and the copyright and license. Every link opens in the
//! browser except Bundled Software, which opens its window. ⌘W closes it.

use std::sync::{Arc, LazyLock};

use gpui::{
    div, img, prelude::*, px, App, Bounds, ClickEvent, Context, FocusHandle, FontWeight, Global,
    Image, ImageFormat, SharedString, Stateful, Div, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions,
};

use crate::app::CloseWindow;
use crate::menus::tree::DOCS_URL;
use crate::platform::version::APP_VERSION;
use crate::theme::style::{Type, Typography};
use crate::theme::{self, Theme};
use crate::views::bundled_software::BundledSoftwareView;
use crate::views::sidebar;

/// The window's size (the frame's).
pub const SIZE: (f32, f32) = (400.0, 520.0);
/// The app icon's size.
pub const ICON: f32 = 112.0;

pub const WEBSITE_URL: &str = "https://shelbi.dev";
pub const GITHUB_URL: &str = "https://github.com/jlong/shelbi";
pub const LICENSE_URL: &str = "https://github.com/jlong/shelbi/blob/main/LICENSE";
pub const SPONSOR_URL: &str = "https://32pixels.co";

/// The copyright line's year: the year this build was made (the build
/// script reads `SOURCE_DATE_EPOCH`, else the clock).
pub const COPYRIGHT_YEAR: &str = env!("SHELBI_BUILD_YEAR");

/// The app icon at twice its 112pt, so it draws sharp on a Retina display:
/// `assets/icon/shelbi-224.png`, cut from the committed 1024px master (see
/// that folder's README).
static ICON_IMAGE: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../assets/icon/shelbi-224.png").to_vec(),
    ))
});

/// The open About and Bundled Software windows (a closed one's handle
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

/// About's Bundled Software: its window, or the open one to the front.
pub fn show_bundled_software(cx: &mut App) {
    if !cx.has_global::<Windows>() {
        cx.set_global(Windows::default());
    }
    if raise(cx.global::<Windows>().bundled, cx) {
        return;
    }
    let size = super::bundled_software::SIZE;
    let opened = cx.open_window(options("Bundled Software", size, true, cx), |window, cx| {
        cx.new(|cx| BundledSoftwareView::new(window, cx))
    });
    match opened {
        Ok(handle) => cx.global_mut::<Windows>().bundled = Some(handle),
        Err(e) => tracing::error!(error = %e, "could not open Bundled Software"),
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

/// A link that opens `url`.
fn url_link(id: &'static str, label: &'static str, url: &'static str, color: gpui::Hsla, theme: &Theme) -> Stateful<Div> {
    link(id, label, color, theme).on_click(move |_: &ClickEvent, _, cx| open_url(url, cx))
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
}

impl AboutView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        AboutView {
            focus,
            _appearance: theme::follow_system(window, cx),
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
            .child(heading_type(div()).text_color(theme.text_loud).child("Shelbi"))
            .child(
                div()
                    .debug_selector(|| "about-version".into())
                    .type_style(Type::Small)
                    .text_color(theme.foreground)
                    .child(format!("Version {APP_VERSION}")),
            );
        let links = div()
            .flex()
            .flex_row()
            .justify_center()
            .gap(px(24.0))
            .type_style(Type::SmallMedium)
            .child(url_link("about-website", "Website", WEBSITE_URL, theme.link, &theme))
            .child(url_link("about-docs", "Documentation", DOCS_URL, theme.link, &theme))
            .child(url_link("about-github", "GitHub", GITHUB_URL, theme.link, &theme));
        let bundled = link("about-bundled", "Bundled Software", theme.link, &theme)
            .type_style(Type::SmallMedium)
            .on_click(|_: &ClickEvent, _, cx| show_bundled_software(cx));
        let credits = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.0))
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .child(format!("© {COPYRIGHT_YEAR} John W. Long"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .gap(px(8.0))
                    .child(url_link("about-license", "MIT License", LICENSE_URL, theme.muted, &theme))
                    .child("·")
                    .child(url_link("about-sponsor", "Sponsored by 32pixels", SPONSOR_URL, theme.muted, &theme)),
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
                img(ICON_IMAGE.clone())
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
                    .child("An Open Source personal agent orchestrator."),
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

    /// The rendition is the master at twice the icon's size, with its
    /// alpha (a PNG's IHDR: width, height, then RGBA's color type 6).
    #[test]
    fn the_icon_rendition_is_2x_with_alpha() {
        let png = include_bytes!("../../assets/icon/shelbi-224.png");
        let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!((be(16), be(20)), (2 * ICON as u32, 2 * ICON as u32));
        assert_eq!(png[25], 6, "RGBA");
    }

    #[test]
    fn the_copyright_year_is_a_year() {
        let year: u32 = COPYRIGHT_YEAR.parse().unwrap();
        assert!((2026..2200).contains(&year));
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
        assert_eq!(opened, [WEBSITE_URL, DOCS_URL, GITHUB_URL, LICENSE_URL, SPONSOR_URL]);
    }

    #[gpui::test]
    fn about_and_bundled_software_open_once_each(cx: &mut TestAppContext) {
        init(cx);
        let before = cx.update(|cx| cx.windows().len());
        cx.update(show_about);
        cx.update(show_about);
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| cx.windows().len()), before + 1, "About again raises the open one");

        // Bundled Software from About's link, then again.
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
