//! The lightbox: a Changelog image the column scaled down, shown over the
//! window at its own size, or as large as fits the window (with a margin)
//! when that's smaller, keeping its aspect ratio. Its caption (the alt text)
//! and a close button sit under it, on the overlay layer's dimmed scrim.
//!
//! A click on a scaled-down figure (`views::markdown`) opens it: the
//! Changelog emits `ChangelogEvent::OpenImage` and the window shows
//! [`ActiveOverlay::Lightbox`](super::ActiveOverlay). Esc, or a click
//! anywhere (the scrim, the image, the close button), closes it, and the
//! keys go back to the Changelog. It is drawn against the window's size
//! every frame, so a resize re-fits it. A right-click on the image opens the
//! image menu over it (`views::image_menu`), and the lightbox stays.
//!
//! Desktop only: the TUI's Changelog draws an image as its alt text.

use gpui::{div, img, prelude::*, px, size, AnyElement, Context, MouseButton, Pixels, SharedUri, Size};
use shelbi_app::changelog::ImageSource;

use super::{with_overlay, OverlayEvent, OverlayHost};
use crate::assets::icons;
use crate::theme::style::{self, Type, Typography};
use crate::theme::Theme;
use crate::views::floating_pane;

/// The room kept clear on each side of the window.
pub const MARGIN: f32 = 32.0;
/// The footer's height (the caption's band and the close button).
pub const FOOTER_H: f32 = 28.0;
/// The room between the image and its footer.
pub const FOOTER_GAP: f32 = 8.0;

/// An image to show: where it comes from (a file or a web image), its own
/// size in points (one per pixel, as the column draws it), and its caption.
#[derive(Debug, Clone, PartialEq)]
pub struct LightboxImage {
    pub source: ImageSource,
    pub width: f32,
    pub height: f32,
    /// The alt text, trimmed; empty for none.
    pub caption: String,
}

/// The size the image is drawn at in a window `window` big: its own, or
/// scaled down uniformly to fit the window less [`MARGIN`] on each side and
/// the footer under it.
pub fn fit(width: f32, height: f32, window: Size<Pixels>) -> Size<Pixels> {
    if width <= 0.0 || height <= 0.0 {
        return size(px(0.0), px(0.0));
    }
    let room_w = (window.width.as_f32() - 2.0 * MARGIN).max(1.0);
    let room_h = (window.height.as_f32() - 2.0 * MARGIN - FOOTER_GAP - FOOTER_H).max(1.0);
    let scale = (room_w / width).min(room_h / height).min(1.0);
    size(px(width * scale), px(height * scale))
}

/// The lightbox's panel: the image at [`fit`] size over its footer. The
/// overlay layer draws the scrim and centers it.
pub fn render<H: OverlayHost>(
    image: &LightboxImage,
    window: Size<Pixels>,
    theme: &Theme,
    cx: &mut Context<H>,
) -> AnyElement {
    let shown = fit(image.width, image.height, window);
    let picture = match &image.source {
        ImageSource::File(path) => Some(img(path.clone())),
        ImageSource::Web(url) => Some(img(SharedUri::from(url.clone()))),
        ImageSource::Missing(_) | ImageSource::Unavailable => None,
    };
    let frame = div()
        .id("lightbox-image")
        .debug_selector(|| "lightbox-image".into())
        .flex_none()
        .w(shown.width)
        .h(shown.height)
        .rounded_md()
        .overflow_hidden()
        .shadow(style::dialog_shadow(theme))
        .children(picture.map(|p| p.size_full()))
        .on_mouse_down(MouseButton::Left, close_on_press(cx))
        .on_mouse_down(MouseButton::Right, menu_on_press(image.source.clone(), cx));
    let caption = (!image.caption.is_empty()).then(|| {
        style::dialog(
            div()
                .id("lightbox-caption")
                .debug_selector(|| "lightbox-caption".into())
                .h(px(FOOTER_H))
                .px_3()
                .flex()
                .items_center()
                .min_w_0()
                .overflow_hidden()
                .type_style(Type::Small)
                .child(div().truncate().child(image.caption.clone())),
            theme,
        )
    });
    let close_button = style::dialog(
        floating_pane::header_button("lightbox-close", icons::X_MARK, false, theme),
        theme,
    )
    .on_mouse_down(MouseButton::Left, close_on_press(cx));
    let footer = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(FOOTER_GAP))
        .max_w(px((window.width.as_f32() - 2.0 * MARGIN).max(FOOTER_H)))
        .children(caption)
        .child(close_button);
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(FOOTER_GAP))
        .child(frame)
        .child(footer)
        .into_any_element()
}

/// A press that closes the lightbox, and goes no further.
fn close_on_press<H: OverlayHost>(
    cx: &mut Context<H>,
) -> impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static + use<H> {
    let host = cx.entity().downgrade();
    move |_, window, cx| {
        cx.stop_propagation();
        let _ = host.update(cx, |this, cx| with_overlay(this, window, cx, |_| OverlayEvent::Close));
    }
}

/// A right-click on the image: the image menu at the pointer, the lightbox
/// left open under it.
fn menu_on_press<H: OverlayHost>(
    source: ImageSource,
    cx: &mut Context<H>,
) -> impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static + use<H> {
    let host = cx.entity().downgrade();
    move |ev, window, cx| {
        cx.stop_propagation();
        let source = source.clone();
        let _ = host.update(cx, |this, cx| this.open_image_menu(source, ev.position, window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fitted(w: f32, h: f32, window: (f32, f32)) -> (f32, f32) {
        let s = fit(w, h, size(px(window.0), px(window.1)));
        (s.width.as_f32(), s.height.as_f32())
    }

    #[test]
    fn an_image_that_fits_is_drawn_at_its_own_size() {
        assert_eq!(fitted(1200.0, 600.0, (1600.0, 1200.0)), (1200.0, 600.0));
        // Exactly the room.
        let room_h = 800.0 - 2.0 * MARGIN - FOOTER_GAP - FOOTER_H;
        assert_eq!(fitted(1000.0 - 2.0 * MARGIN, room_h, (1000.0, 800.0)), (1000.0 - 2.0 * MARGIN, room_h));
    }

    #[test]
    fn a_bigger_image_scales_down_to_the_window_keeping_its_ratio() {
        // Width-bound: 2000x1000 in 1000x900 → 936 wide.
        let (w, h) = fitted(2000.0, 1000.0, (1000.0, 900.0));
        assert!((w - 936.0).abs() < 0.01, "{w}");
        assert!((h - 468.0).abs() < 0.01, "{h}");
        // Height-bound: a tall phone shot in a wide, short window.
        let (w, h) = fitted(1000.0, 2000.0, (1600.0, 700.0));
        let room_h = 700.0 - 2.0 * MARGIN - FOOTER_GAP - FOOTER_H;
        assert!((h - room_h).abs() < 0.01, "{h}");
        assert!((w - room_h / 2.0).abs() < 0.01, "{w}");
    }

    #[test]
    fn a_tiny_window_or_an_empty_image_never_goes_negative() {
        let (w, h) = fitted(800.0, 400.0, (20.0, 20.0));
        assert!(w > 0.0 && h > 0.0 && w <= 1.0, "{w}x{h}");
        assert_eq!(fitted(0.0, 400.0, (800.0, 600.0)), (0.0, 0.0));
    }
}
