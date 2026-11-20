//! The lightbox: a Changelog image the column scaled down, shown over the
//! window at its own size, or as large as fits the window (with a margin)
//! when that's smaller, keeping its aspect ratio. A round close button sits
//! centered on the image's top-left corner, and its caption (the alt text)
//! under it as bare text, on a scrim much darker than the other overlays'.
//!
//! A click on a scaled-down figure (`views::markdown`) opens it: the
//! Changelog emits `ChangelogEvent::OpenImage` and the window shows
//! [`ActiveOverlay::Lightbox`](super::ActiveOverlay). Esc, or a click
//! anywhere (the scrim, the image, the close button), closes it, and the
//! keys go back to the Changelog. It is drawn against the window's size
//! every frame, so a resize re-fits it. A right-click on the image opens the
//! image menu over it (`views::image_menu`), and the lightbox stays.
//!
//! It holds every image on the Changelog page in order ([`Lightbox`]), not
//! only the one clicked: h, k, ← and ↑ step to the one before, l, j, → and
//! ↓ to the one after, across figures, galleries and entries alike, and it
//! stops at either end.
//!
//! Desktop only: the TUI's Changelog draws an image as its alt text.

use gpui::{
    div, img, point, prelude::*, px, size, AnyElement, Bounds, Context, Hsla, MouseButton, Pixels, SharedUri, Size,
};
use shelbi_app::changelog::ImageSource;

use super::{with_overlay, OverlayEvent, OverlayHost};
use crate::assets::{heroicon, icons};
use crate::elements::clamped_text::clamped_text;
use crate::theme::style::{self, Type, Typography, TOOLBAR_ICON_BUTTON};
use crate::theme::Theme;

/// The room kept clear on each side of the window. At least half the close
/// button, so the button, which overhangs the image's corner by that much,
/// stays on screen.
pub const MARGIN: f32 = 32.0;
/// The close button's diameter.
pub const CLOSE_SIZE: f32 = TOOLBAR_ICON_BUTTON;
/// How far the close button reaches past the image's top and left edges:
/// half of it, so its center sits on the corner.
pub const CLOSE_OVERHANG: f32 = CLOSE_SIZE / 2.0;
/// The room between the image and its caption.
pub const CAPTION_GAP: f32 = 12.0;
/// The most lines the caption wraps to before it ends in an ellipsis.
pub const CAPTION_LINES: usize = 2;
/// The caption's type.
const CAPTION_TYPE: Type = Type::Small;

const _: () = assert!(MARGIN >= CLOSE_OVERHANG, "the margin must hold the close button's overhang");

/// The lightbox's backdrop: about 85% black in both themes, far darker than
/// the other overlays' scrim, so the bare caption and the button stand out.
pub fn scrim() -> Hsla {
    gpui::black().opacity(0.85)
}

/// The caption's color: a light gray that reads on [`scrim`] in both
/// themes (the scrim is near black whatever is under it).
pub fn caption_color() -> Hsla {
    gpui::white().opacity(0.88)
}

/// The height kept under the image for a caption: the gap and
/// [`CAPTION_LINES`] lines. None without one.
pub fn caption_room(captioned: bool) -> f32 {
    if captioned {
        CAPTION_GAP + CAPTION_LINES as f32 * CAPTION_TYPE.line_height()
    } else {
        0.0
    }
}

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

impl LightboxImage {
    /// Whether a caption is drawn under the image.
    pub fn captioned(&self) -> bool {
        !self.caption.is_empty()
    }

    /// Its own size, `(width, height)`, when known (a neighbor whose file
    /// hadn't decoded when the lightbox opened isn't, until it does).
    pub fn size(&self) -> Option<(f32, f32)> {
        (self.width > 0.0 && self.height > 0.0).then_some((self.width, self.height))
    }
}

/// What the lightbox shows: the page's images in order and which one is up.
#[derive(Debug, Clone, PartialEq)]
pub struct Lightbox {
    images: Vec<LightboxImage>,
    at: usize,
    /// Where the opened image was drawn on the page (in the window), which
    /// it grows from as the lightbox comes in, and which one that was.
    from: Option<(usize, Bounds<Pixels>)>,
}

impl Lightbox {
    /// `images`, `at` the one shown; `at` past the end shows the last.
    /// Empty, it is `fallback` alone.
    pub fn new(images: Vec<LightboxImage>, at: usize, fallback: LightboxImage) -> Self {
        if images.is_empty() {
            return Self::single(fallback);
        }
        let at = at.min(images.len() - 1);
        Self { images, at, from: None }
    }

    /// Just `image`, nothing to step to.
    pub fn single(image: LightboxImage) -> Self {
        Self { images: vec![image], at: 0, from: None }
    }

    /// The shown image grows from `from` (where the page drew it, in the
    /// window) as the lightbox comes in.
    pub fn growing_from(mut self, from: Bounds<Pixels>) -> Self {
        self.from = Some((self.at, from));
        self
    }

    /// Where the shown image grows from: the page's place for it, while it
    /// is still the one the lightbox opened on.
    pub fn grows_from(&self) -> Option<Bounds<Pixels>> {
        self.from.filter(|&(ix, b)| ix == self.at && b.size.width > px(0.0)).map(|(_, b)| b)
    }

    /// The image shown.
    pub fn current(&self) -> &LightboxImage {
        &self.images[self.at]
    }

    /// Where the shown image sits in [`Self::images`].
    #[cfg(test)]
    pub fn position(&self) -> usize {
        self.at
    }

    /// Every image it steps through, in the page's order.
    #[cfg(test)]
    pub fn images(&self) -> &[LightboxImage] {
        &self.images
    }

    /// Show the image `by` places on (back, when negative), stopping at
    /// either end; whether the shown image changed.
    pub fn step(&mut self, by: isize) -> bool {
        let to = self.at.saturating_add_signed(by).min(self.images.len() - 1);
        let moved = to != self.at;
        self.at = to;
        moved
    }
}

/// Which way a key steps the lightbox: -1 for h, k, ← and ↑, 1 for l, j,
/// → and ↓, with no modifier held. `None` for any other key.
pub fn step_for(chord: &shelbi_state::keymap::KeyChord) -> Option<isize> {
    use shelbi_state::keymap::{Key, Mods};
    if chord.mods != Mods::NONE {
        return None;
    }
    match chord.code {
        Key::Char('h' | 'k') | Key::Left | Key::Up => Some(-1),
        Key::Char('l' | 'j') | Key::Right | Key::Down => Some(1),
        _ => None,
    }
}

/// The size the image is drawn at in a window `window` big: its own, or
/// scaled down uniformly to fit the window less [`MARGIN`] on each side and
/// the [`caption_room`] under it. The margin holds the close button's
/// overhang, so with the image centered the button stays on screen.
pub fn fit(width: f32, height: f32, window: Size<Pixels>, captioned: bool) -> Size<Pixels> {
    if width <= 0.0 || height <= 0.0 {
        return size(px(0.0), px(0.0));
    }
    let room_w = (window.width.as_f32() - 2.0 * MARGIN).max(1.0);
    let room_h = (window.height.as_f32() - 2.0 * MARGIN - caption_room(captioned)).max(1.0);
    let scale = (room_w / width).min(room_h / height).min(1.0);
    size(px(width * scale), px(height * scale))
}

/// Where the close button's top-left sits from the image's top-left, when
/// the image is drawn `shown` big where [`place`] puts it in a window
/// `window` big: [`CLOSE_OVERHANG`] up and left, so it's centered on the
/// corner. Only in a window too small for [`fit`]'s margin to hold the
/// overhang does it move in, as far as keeps it on screen.
pub fn close_offset(shown: Size<Pixels>, window: Size<Pixels>, captioned: bool) -> (f32, f32) {
    let left = (window.width.as_f32() - shown.width.as_f32()) / 2.0;
    let top = (window.height.as_f32() - shown.height.as_f32() - caption_room(captioned)) / 2.0;
    (-CLOSE_OVERHANG.min(left), -CLOSE_OVERHANG.min(top))
}

/// Where the image is drawn in a window `window` big: at [`fit`] size,
/// centered across, and centered down with the [`caption_room`] under it.
pub fn place(width: f32, height: f32, window: Size<Pixels>, captioned: bool) -> Bounds<Pixels> {
    let shown = fit(width, height, window, captioned);
    let left = (window.width - shown.width) / 2.0;
    let top = (window.height - shown.height - px(caption_room(captioned))) / 2.0;
    Bounds::new(point(left, top), shown)
}

/// `from` moved `t` (0..=1) of the way to `to`.
pub fn lerp_bounds(from: Bounds<Pixels>, to: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let mix = |a: Pixels, b: Pixels| a + (b - a) * t;
    Bounds::new(
        point(mix(from.origin.x, to.origin.x), mix(from.origin.y, to.origin.y)),
        size(mix(from.size.width, to.size.width), mix(from.size.height, to.size.height)),
    )
}

/// How opaque the close button and caption are `t` (0..=1, eased) into the
/// entrance: hidden while the image travels, fading in over its second half.
pub fn chrome_opacity(t: f32) -> f32 {
    ((t - 0.5) * 2.0).clamp(0.0, 1.0)
}

/// The lightbox over the window: the image at [`place`], the close button on
/// its corner, its caption under it. The overlay layer draws the scrim
/// ([`scrim`]) under it.
///
/// `entering` is the entrance's progress (0..=1, eased), `None` once it's
/// in: the image grows from its place on the page ([`Lightbox::grows_from`])
/// to its own, the button and caption fading in after it.
pub fn render<H: OverlayHost>(
    lightbox: &Lightbox,
    window: Size<Pixels>,
    entering: Option<f32>,
    theme: &Theme,
    win: &mut gpui::Window,
    cx: &mut Context<H>,
) -> AnyElement {
    let image = lightbox.current();
    let captioned = image.captioned();
    // A neighbor stepped to before its file decoded: the shared image
    // cache's size once it lands (the cache redraws the window then).
    let (width, height) = image
        .size()
        .or_else(|| crate::views::markdown::image_size(&image.source, win, cx))
        .unwrap_or((0.0, 0.0));
    let target = place(width, height, window, captioned);
    let (drawn, chrome) = match (entering, lightbox.grows_from()) {
        (Some(t), Some(from)) => (lerp_bounds(from, target, t), chrome_opacity(t)),
        (Some(t), None) => (target, t),
        (None, _) => (target, 1.0),
    };
    let picture = match &image.source {
        ImageSource::File(path) => Some(img(path.clone())),
        ImageSource::Web(url) => Some(img(SharedUri::from(url.clone()))),
        ImageSource::Missing(_) | ImageSource::Unavailable => None,
    };
    let frame = div()
        .id("lightbox-image")
        .debug_selector(|| "lightbox-image".into())
        .absolute()
        .left(drawn.origin.x)
        .top(drawn.origin.y)
        .w(drawn.size.width)
        .h(drawn.size.height)
        .rounded_md()
        .overflow_hidden()
        .shadow(style::dialog_shadow(theme))
        .children(picture.map(|p| p.size_full()))
        .on_mouse_down(MouseButton::Left, close_on_press(cx))
        .on_mouse_down(MouseButton::Right, menu_on_press(image.source.clone(), cx));
    // The button rides the image's corner as it grows; a sibling of the
    // image, so the image's rounded clip doesn't cut it off.
    let (dx, dy) = close_offset(target.size, window, captioned);
    let close_button = style::floating_icon_button("lightbox-close", heroicon(icons::X_MARK), "Close", theme)
        .absolute()
        .left(drawn.origin.x + px(dx))
        .top(drawn.origin.y + px(dy))
        .opacity(chrome)
        .on_mouse_down(MouseButton::Left, close_on_press(cx));
    let caption = captioned.then(|| {
        div()
            .id("lightbox-caption")
            .debug_selector(|| "lightbox-caption".into())
            .absolute()
            .left(target.origin.x)
            .top(target.bottom() + px(CAPTION_GAP))
            .w(target.size.width)
            .opacity(chrome)
            .type_style(CAPTION_TYPE)
            .text_center()
            .text_color(caption_color())
            .child(clamped_text(image.caption.clone(), CAPTION_LINES))
    });
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(frame)
        .child(close_button)
        .children(caption)
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

    fn win(w: f32, h: f32) -> Size<Pixels> {
        size(px(w), px(h))
    }

    fn fitted(w: f32, h: f32, window: (f32, f32), captioned: bool) -> (f32, f32) {
        let s = fit(w, h, win(window.0, window.1), captioned);
        (s.width.as_f32(), s.height.as_f32())
    }

    /// The close button's bounds in the window (left, top, right, bottom)
    /// for an image `w` x `h` centered over its caption, as the overlay
    /// layer centers it, with the caption at its fullest.
    fn close_rect(w: f32, h: f32, window: (f32, f32), captioned: bool) -> (f32, f32, f32, f32) {
        let shown = fit(w, h, win(window.0, window.1), captioned);
        let left = (window.0 - shown.width.as_f32()) / 2.0;
        let top = (window.1 - shown.height.as_f32() - caption_room(captioned)) / 2.0;
        let (dx, dy) = close_offset(shown, win(window.0, window.1), captioned);
        (left + dx, top + dy, left + dx + CLOSE_SIZE, top + dy + CLOSE_SIZE)
    }

    #[test]
    fn an_image_that_fits_is_drawn_at_its_own_size() {
        assert_eq!(fitted(1200.0, 600.0, (1600.0, 1200.0), true), (1200.0, 600.0));
        // Exactly the room.
        let room_h = 800.0 - 2.0 * MARGIN - caption_room(true);
        assert_eq!(fitted(1000.0 - 2.0 * MARGIN, room_h, (1000.0, 800.0), true), (1000.0 - 2.0 * MARGIN, room_h));
    }

    #[test]
    fn a_bigger_image_scales_down_to_the_window_keeping_its_ratio() {
        // Width-bound: 2000x1000 in 1000x900 → 936 wide.
        let (w, h) = fitted(2000.0, 1000.0, (1000.0, 900.0), true);
        assert!((w - 936.0).abs() < 0.01, "{w}");
        assert!((h - 468.0).abs() < 0.01, "{h}");
        // Height-bound: a tall phone shot in a wide, short window.
        let (w, h) = fitted(1000.0, 2000.0, (1600.0, 700.0), true);
        let room_h = 700.0 - 2.0 * MARGIN - caption_room(true);
        assert!((h - room_h).abs() < 0.01, "{h}");
        assert!((w - room_h / 2.0).abs() < 0.01, "{w}");
    }

    #[test]
    fn the_caption_takes_room_only_when_there_is_one() {
        // Two lines and the gap under the image, kept whether it wraps or not.
        assert_eq!(caption_room(true), CAPTION_GAP + 2.0 * 18.0);
        assert_eq!(caption_room(false), 0.0);
        // Height-bound, the uncaptioned image gets that room back.
        let (_, with) = fitted(1000.0, 2000.0, (1600.0, 700.0), true);
        let (_, without) = fitted(1000.0, 2000.0, (1600.0, 700.0), false);
        assert!((without - (700.0 - 2.0 * MARGIN)).abs() < 0.01, "{without}");
        assert!((without - with - caption_room(true)).abs() < 0.01, "{with} vs {without}");
    }

    #[test]
    fn the_close_button_is_centered_on_the_corner_with_room_for_its_overhang() {
        for captioned in [true, false] {
            for (w, h) in [(1200.0, 600.0), (2000.0, 1000.0), (1000.0, 2000.0), (40.0, 30.0)] {
                for window in [(1600.0, 1200.0), (1000.0, 800.0), (800.0, 600.0), (1600.0, 400.0), (480.0, 320.0)] {
                    let shown = fit(w, h, win(window.0, window.1), captioned);
                    let what = format!("{w}x{h} in {window:?}, captioned {captioned}");
                    // Its center on the image's top-left corner.
                    assert_eq!(
                        close_offset(shown, win(window.0, window.1), captioned),
                        (-CLOSE_OVERHANG, -CLOSE_OVERHANG),
                        "{what}"
                    );
                    // And the fit left room for it: all of it on screen.
                    let (l, t, r, b) = close_rect(w, h, window, captioned);
                    assert!(l >= 0.0 && t >= 0.0 && r <= window.0 && b <= window.1, "{what}: {l},{t}");
                }
            }
        }
    }

    #[test]
    fn in_a_window_too_small_for_the_margin_the_close_button_moves_in_to_stay_on_screen() {
        for captioned in [true, false] {
            for window in [(120.0, 90.0), (64.0, 64.0), (40.0, 200.0), (300.0, 40.0), (32.0, 32.0)] {
                let (l, t, r, b) = close_rect(1200.0, 600.0, window, captioned);
                let what = format!("{window:?}, captioned {captioned}");
                assert!(l >= -0.01 && t >= -0.01, "{what}: {l},{t}");
                assert!(r <= window.0 + 0.01 && b <= window.1 + 0.01, "{what}: {r},{b}");
            }
        }
    }

    #[test]
    fn a_tiny_window_or_an_empty_image_never_goes_negative() {
        let (w, h) = fitted(800.0, 400.0, (20.0, 20.0), true);
        assert!(w > 0.0 && h > 0.0 && w <= 1.0, "{w}x{h}");
        assert_eq!(fitted(0.0, 400.0, (800.0, 600.0), false), (0.0, 0.0));
    }

    #[test]
    fn the_scrim_is_far_darker_than_the_overlay_dim_and_the_caption_light() {
        assert_eq!(scrim().l, 0.0);
        assert!((scrim().a - 0.85).abs() < 0.01);
        use gpui::WindowAppearance;
        use shelbi_app::theme::ThemePreference;
        for theme in [
            Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark),
            Theme::resolve(ThemePreference::Light, WindowAppearance::Light),
        ] {
            assert!(scrim().a > theme.scrim.a + 0.15, "{:?}", theme.scrim);
        }
        assert!(caption_color().l > 0.95 && caption_color().a > 0.8);
    }

    fn shot(n: usize) -> LightboxImage {
        LightboxImage {
            source: ImageSource::File(format!("/nonexistent/{n}.png").into()),
            width: 100.0,
            height: 50.0,
            caption: format!("Shot {n}"),
        }
    }

    #[test]
    fn stepping_moves_through_the_images_and_stops_at_either_end() {
        let mut lb = Lightbox::new((0..3).map(shot).collect(), 1, shot(9));
        assert_eq!(lb.current(), &shot(1));
        assert!(lb.step(1));
        assert_eq!(lb.current(), &shot(2));
        assert!(!lb.step(1), "the last one stays");
        assert_eq!(lb.current(), &shot(2));
        assert!(lb.step(-1) && lb.step(-1));
        assert_eq!(lb.current(), &shot(0));
        assert!(!lb.step(-1), "the first one stays");
        assert_eq!(lb.position(), 0);
        // Alone, nothing to step to.
        let mut one = Lightbox::single(shot(4));
        assert!(!one.step(1) && !one.step(-1));
        assert_eq!(one.current(), &shot(4));
        // No images: the fallback alone; past the end: the last.
        assert_eq!(Lightbox::new(Vec::new(), 0, shot(7)).images(), [shot(7)]);
        assert_eq!(Lightbox::new((0..2).map(shot).collect(), 5, shot(7)).current(), &shot(1));
    }

    #[test]
    fn h_j_k_l_and_the_arrows_step_with_no_modifier() {
        use shelbi_state::keymap::{Key, KeyChord, Mods};
        let plain = |code| KeyChord::new(code, Mods::NONE);
        for code in [Key::Char('h'), Key::Char('k'), Key::Left, Key::Up] {
            assert_eq!(step_for(&plain(code)), Some(-1), "{code:?}");
        }
        for code in [Key::Char('l'), Key::Char('j'), Key::Right, Key::Down] {
            assert_eq!(step_for(&plain(code)), Some(1), "{code:?}");
        }
        for code in [Key::Char(' '), Key::Char('q'), Key::Enter, Key::Esc] {
            assert_eq!(step_for(&plain(code)), None, "{code:?}");
        }
        assert_eq!(step_for(&KeyChord::new(Key::Right, Mods::SUPER)), None);
        assert_eq!(step_for(&KeyChord::new(Key::Char('l'), Mods::CONTROL)), None);
    }

    #[test]
    fn a_neighbor_not_yet_decoded_has_no_size() {
        assert_eq!(shot(0).size(), Some((100.0, 50.0)));
        assert_eq!(LightboxImage { width: 0.0, height: 0.0, ..shot(0) }.size(), None);
    }

    #[test]
    fn the_image_is_placed_centered_over_its_caption_room() {
        let window = win(1000.0, 800.0);
        let b = place(1200.0, 600.0, window, true);
        assert_eq!(b.size, fit(1200.0, 600.0, window, true));
        assert!((b.center().x.as_f32() - 500.0).abs() < 0.01);
        assert!((b.top().as_f32() - (800.0 - b.size.height.as_f32() - caption_room(true)) / 2.0).abs() < 0.01);
        let bare = place(1200.0, 600.0, window, false);
        assert!((bare.center().y.as_f32() - 400.0).abs() < 0.01, "{bare:?}");
    }

    #[test]
    fn the_grow_runs_from_the_page_to_the_place_and_the_chrome_fades_in_after() {
        let from = Bounds::new(point(px(10.0), px(20.0)), size(px(100.0), px(50.0)));
        let to = Bounds::new(point(px(110.0), px(220.0)), size(px(300.0), px(150.0)));
        assert_eq!(lerp_bounds(from, to, 0.0), from);
        assert_eq!(lerp_bounds(from, to, 1.0), to);
        let mid = lerp_bounds(from, to, 0.5);
        assert_eq!((mid.origin.x, mid.size.width), (px(60.0), px(200.0)));
        assert_eq!(chrome_opacity(0.0), 0.0);
        assert_eq!(chrome_opacity(0.5), 0.0);
        assert!((chrome_opacity(0.75) - 0.5).abs() < 1e-6);
        assert_eq!(chrome_opacity(1.0), 1.0);
        // Only the image it opened on grows, and only from a drawn place.
        let mut lb = Lightbox::new((0..2).map(shot).collect(), 0, shot(9)).growing_from(from);
        assert_eq!(lb.grows_from(), Some(from));
        lb.step(1);
        assert_eq!(lb.grows_from(), None);
        lb.step(-1);
        assert_eq!(lb.grows_from(), Some(from));
        assert_eq!(Lightbox::single(shot(0)).grows_from(), None);
        assert_eq!(Lightbox::single(shot(0)).growing_from(Bounds::default()).grows_from(), None);
    }
}
