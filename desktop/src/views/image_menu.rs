//! The image menu: a right-click on a Changelog image (a figure in the
//! column, `views::markdown`, or the lightbox's image,
//! `views::overlay::lightbox`) opens it at the pointer, over everything.
//! Its one row, "Download Image…", opens the save panel in the downloads
//! folder ([`downloads_dir`]) with the image's file name ([`file_name`]);
//! saving writes the image's
//! bytes unchanged ([`save`]: a copy of a local file, the downloaded bytes of
//! a web image), off the UI thread. A failure shows in the window's toast;
//! Cancel does nothing.
//!
//! It follows the tab bar's context menu: anchored at the pointer, drawn
//! `deferred`, a press outside it closes it and goes no further, Esc closes
//! it. The window owns it (`ShelbiWindow::image_menu`), so the same menu
//! serves the column and the lightbox, and the toast is the window's.
//!
//! A figure that can't draw its image (a placeholder: missing, or a remote
//! window's local file) has no menu. An image whose bytes can't be had here
//! ([`refusal`]) shows the row dimmed, with why on hover.

use std::path::{Path, PathBuf};

use futures::future::LocalBoxFuture;
use futures::AsyncReadExt as _;
use gpui::{deferred, div, prelude::*, AnyElement, App, ClickEvent, MouseDownEvent, SharedString, Window};
use shelbi_app::changelog::{percent_decode, ImageSource};

use crate::elements::placed::placed;
use crate::theme::style::{self, Anchor};
use crate::theme::Theme;

/// The room between the pointer and the menu (the tab menu's).
const CONTEXT_GAP: f32 = 2.0;
/// Its row's label.
pub const DOWNLOAD_LABEL: &str = "Download Image\u{2026}";
/// Its row's id and debug selector.
pub const DOWNLOAD_ROW: &str = "image-menu-download";
/// The menu's debug selector.
pub const MENU: &str = "image-menu";
/// The name offered when the image's own can't be read off its path or URL.
pub const FALLBACK_NAME: &str = "image.png";

/// An open image menu: the image it is for, and where it was asked for.
pub struct ImageMenu {
    pub source: ImageSource,
    /// The pointer where the image was right-clicked.
    pub anchor: Anchor,
}

impl ImageMenu {
    pub fn new(source: ImageSource, at: gpui::Point<gpui::Pixels>) -> Self {
        let anchor = Anchor::default();
        anchor.set(Some(gpui::Bounds::new(at, gpui::Size::default())));
        ImageMenu { source, anchor }
    }
}

/// The save panel: shown in `dir` with `name` filled in, it resolves to the
/// chosen path, `None` on Cancel. The window's is gpui's
/// (`prompt_for_new_path`); tests answer it themselves.
pub type SavePanel = std::rc::Rc<dyn Fn(&Path, &str, &mut App) -> LocalBoxFuture<'static, Result<Option<PathBuf>, String>>>;

/// gpui's save panel (`NSSavePanel` on macOS, the portal's file chooser on
/// Linux).
pub fn system_save_panel() -> SavePanel {
    std::rc::Rc::new(|dir, name, cx| {
        let answer = cx.prompt_for_new_path(dir, Some(name));
        Box::pin(async move {
            match answer.await {
                Ok(Ok(path)) => Ok(path),
                Ok(Err(e)) => Err(e.to_string()),
                // The panel went away without an answer: as good as Cancel.
                Err(_) => Ok(None),
            }
        })
    })
}

/// Where the save panel opens: `~/Downloads` on macOS, the XDG download
/// folder on Linux (`platform::os::downloads_dir`).
pub fn downloads_dir() -> PathBuf {
    crate::platform::os::downloads_dir()
}

/// The name the save panel offers for `source`: the file's own name, or the
/// last part of the URL's path (unescaped, query and fragment dropped);
/// [`FALLBACK_NAME`] when there is none.
pub fn file_name(source: &ImageSource) -> String {
    let name = match source {
        ImageSource::File(path) | ImageSource::Missing(path) => {
            path.file_name().map(|n| n.to_string_lossy().into_owned())
        }
        ImageSource::Web(url) => url_file_name(url),
        ImageSource::Unavailable => None,
    };
    name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| FALLBACK_NAME.to_string())
}

fn url_file_name(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.split(['?', '#']).next()?;
    // The host isn't a name.
    let (_, path) = path.split_once('/')?;
    let last = percent_decode(path.rsplit('/').next()?);
    // An escaped `/` in it isn't a folder to save into.
    Path::new(&last).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Why `source`'s bytes can't be saved from here, or `None` when they can:
/// a local file is copied, a web image downloaded through the same client
/// that loaded it.
pub fn refusal(source: &ImageSource) -> Option<&'static str> {
    match source {
        ImageSource::File(_) | ImageSource::Web(_) => None,
        ImageSource::Missing(_) => Some("The image\u{2019}s file is missing."),
        ImageSource::Unavailable => Some("The image\u{2019}s file is on another machine."),
    }
}

/// Write `source`'s bytes to `dest`, unchanged: a copy of a local file, the
/// body of a web image's download through `http` (the window's client, the
/// one its image loaded through). Run it off the UI thread.
pub async fn save(
    source: ImageSource,
    dest: PathBuf,
    http: std::sync::Arc<dyn gpui::http_client::HttpClient>,
) -> Result<(), String> {
    match source {
        ImageSource::File(from) => {
            // Saved over itself: there is nothing to write (and a copy onto
            // its own file would empty it).
            if same_file(&from, &dest) {
                return Ok(());
            }
            std::fs::copy(&from, &dest).map(|_| ()).map_err(|e| e.to_string())
        }
        ImageSource::Web(url) => {
            let mut response = http
                .get(&url, gpui::http_client::AsyncBody::default(), true)
                .await
                .map_err(|e| e.to_string())?;
            let mut body = Vec::new();
            response.body_mut().read_to_end(&mut body).await.map_err(|e| e.to_string())?;
            if !response.status().is_success() {
                return Err(format!("the server answered {}", response.status()));
            }
            std::fs::write(&dest, body).map_err(|e| e.to_string())
        }
        other => Err(refusal(&other).unwrap_or("the image can\u{2019}t be read").to_string()),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The menu at its anchor, over everything (`deferred`): "Download Image…",
/// or the row dimmed with [`refusal`]'s reason on hover. `download` runs on a
/// click on the row; `dismiss` on a press anywhere outside the menu, which
/// goes no further.
pub fn render(
    menu: &ImageMenu,
    theme: &Theme,
    download: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let row = match refusal(&menu.source) {
        Some(why) => style::disabled_menu_item(DOWNLOAD_ROW, theme).tooltip(style::tooltip(why)),
        None => style::menu_item(DOWNLOAD_ROW, false, theme).on_click(download),
    };
    let body = style::menu_panel(MENU, style::MENU_WIDTH, theme)
        .child(row.child(div().flex_1().min_w_0().truncate().child(SharedString::from(DOWNLOAD_LABEL))));
    let placed = placed(&menu.anchor, body).gap(CONTEXT_GAP).on_dismiss(dismiss);
    // Over the overlay layer (the lightbox) and the tab bar's menu.
    deferred(placed).with_priority(2).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offered_name_is_the_files_or_the_urls() {
        let file = |p: &str| file_name(&ImageSource::File(PathBuf::from(p)));
        assert_eq!(file("/c/assets/e/board after.png"), "board after.png");
        assert_eq!(file("/c/assets/e/shot.JPG"), "shot.JPG");
        let web = |u: &str| file_name(&ImageSource::Web(u.into()));
        assert_eq!(web("https://example.com/img/shot.png"), "shot.png");
        assert_eq!(web("https://example.com/img/my%20shot.png?w=2#top"), "my shot.png");
        assert_eq!(web("http://example.com/a%2Fb.png"), "b.png", "an escaped slash isn't a folder");
        assert_eq!(web("https://example.com/"), FALLBACK_NAME);
        assert_eq!(web("https://example.com"), FALLBACK_NAME);
        assert_eq!(web("https://example.com/img/?x=1"), FALLBACK_NAME);
        assert_eq!(file_name(&ImageSource::Unavailable), FALLBACK_NAME);
    }

    #[test]
    fn only_a_local_file_or_a_web_image_can_be_saved() {
        assert_eq!(refusal(&ImageSource::File("/a.png".into())), None);
        assert_eq!(refusal(&ImageSource::Web("https://e.com/a.png".into())), None);
        assert!(refusal(&ImageSource::Missing("/a.png".into())).is_some());
        assert!(refusal(&ImageSource::Unavailable).unwrap().contains("another machine"));
    }

    #[test]
    fn saving_a_file_over_itself_leaves_it_whole() {
        let dir = std::env::temp_dir().join(format!("shbim-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, b"png bytes").unwrap();
        let http: std::sync::Arc<dyn gpui::http_client::HttpClient> =
            gpui::http_client::FakeHttpClient::with_404_response();
        futures::executor::block_on(save(ImageSource::File(path.clone()), path.clone(), http)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"png bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
