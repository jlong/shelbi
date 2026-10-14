//! Embedded assets: the icons the app draws.
//!
//! **Every icon in Shelbi's own UI is a Heroicon** (MIT, Tailwind Labs,
//! vendored under `assets/icons/heroicons/` with its LICENSE, 24px outline
//! set from heroicons v2.2.0). To add one, copy its SVG from
//! `optimized/24/outline/` at that tag into the folder, then add a constant
//! below and a row to [`HEROICONS`]. Don't reach for gpui-kit's bundled icon
//! catalog in Shelbi's own views; it is only the fallback that keeps
//! gpui-kit components' internal glyphs (a checkbox tick, a dropdown caret)
//! resolving.
//!
//! Render one with `gpui::svg().path(icons::X_MARK)`; the SVG's
//! `currentColor` takes the element's `text_color`.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

/// Asset paths of the vendored Heroicons.
pub mod icons {
    pub const CHAT_BUBBLE_LEFT_RIGHT: &str = "icons/heroicons/chat-bubble-left-right.svg";
    pub const VIEW_COLUMNS: &str = "icons/heroicons/view-columns.svg";
    pub const BOLT: &str = "icons/heroicons/bolt.svg";
    pub const SERVER_STACK: &str = "icons/heroicons/server-stack.svg";
    pub const X_MARK: &str = "icons/heroicons/x-mark.svg";
    pub const ARROW_LEFT: &str = "icons/heroicons/arrow-left.svg";
    pub const ARROW_PATH: &str = "icons/heroicons/arrow-path.svg";
    pub const ARROW_TOP_RIGHT_ON_SQUARE: &str = "icons/heroicons/arrow-top-right-on-square.svg";
    pub const CLIPBOARD_DOCUMENT: &str = "icons/heroicons/clipboard-document.svg";
    pub const EXCLAMATION_TRIANGLE: &str = "icons/heroicons/exclamation-triangle.svg";
    pub const FUNNEL: &str = "icons/heroicons/funnel.svg";
    pub const LOCK_CLOSED: &str = "icons/heroicons/lock-closed.svg";
    pub const PENCIL_SQUARE: &str = "icons/heroicons/pencil-square.svg";
    pub const PLAY: &str = "icons/heroicons/play.svg";
    pub const PLUS: &str = "icons/heroicons/plus.svg";
    pub const USER: &str = "icons/heroicons/user.svg";
    pub const MAGNIFYING_GLASS: &str = "icons/heroicons/magnifying-glass.svg";
    pub const COG_6_TOOTH: &str = "icons/heroicons/cog-6-tooth.svg";
    pub const CHEVRON_UP_DOWN: &str = "icons/heroicons/chevron-up-down.svg";
    pub const CHEVRON_DOUBLE_LEFT: &str = "icons/heroicons/chevron-double-left.svg";
    pub const BARS_3: &str = "icons/heroicons/bars-3.svg";
    pub const CHECK: &str = "icons/heroicons/check.svg";
    pub const SIGNAL_SLASH: &str = "icons/heroicons/signal-slash.svg";
    pub const ARROW_DOWN_CIRCLE: &str = "icons/heroicons/arrow-down-circle.svg";
    pub const COMMAND_LINE: &str = "icons/heroicons/command-line.svg";
    pub const FOLDER_OPEN: &str = "icons/heroicons/folder-open.svg";
    pub const GLOBE_ALT: &str = "icons/heroicons/globe-alt.svg";
    pub const DOCUMENT_TEXT: &str = "icons/heroicons/document-text.svg";
}

macro_rules! heroicon {
    ($path:expr, $file:literal) => {
        (
            $path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/heroicons/",
                $file
            )) as &[u8],
        )
    };
}

/// Every vendored Heroicon, by asset path.
const HEROICONS: &[(&str, &[u8])] = &[
    heroicon!(icons::CHAT_BUBBLE_LEFT_RIGHT, "chat-bubble-left-right.svg"),
    heroicon!(icons::VIEW_COLUMNS, "view-columns.svg"),
    heroicon!(icons::BOLT, "bolt.svg"),
    heroicon!(icons::SERVER_STACK, "server-stack.svg"),
    heroicon!(icons::X_MARK, "x-mark.svg"),
    heroicon!(icons::ARROW_LEFT, "arrow-left.svg"),
    heroicon!(icons::ARROW_PATH, "arrow-path.svg"),
    heroicon!(icons::ARROW_TOP_RIGHT_ON_SQUARE, "arrow-top-right-on-square.svg"),
    heroicon!(icons::CLIPBOARD_DOCUMENT, "clipboard-document.svg"),
    heroicon!(icons::EXCLAMATION_TRIANGLE, "exclamation-triangle.svg"),
    heroicon!(icons::FUNNEL, "funnel.svg"),
    heroicon!(icons::LOCK_CLOSED, "lock-closed.svg"),
    heroicon!(icons::PENCIL_SQUARE, "pencil-square.svg"),
    heroicon!(icons::PLAY, "play.svg"),
    heroicon!(icons::PLUS, "plus.svg"),
    heroicon!(icons::USER, "user.svg"),
    heroicon!(icons::MAGNIFYING_GLASS, "magnifying-glass.svg"),
    heroicon!(icons::COG_6_TOOTH, "cog-6-tooth.svg"),
    heroicon!(icons::CHEVRON_UP_DOWN, "chevron-up-down.svg"),
    heroicon!(icons::CHEVRON_DOUBLE_LEFT, "chevron-double-left.svg"),
    heroicon!(icons::BARS_3, "bars-3.svg"),
    heroicon!(icons::CHECK, "check.svg"),
    heroicon!(icons::SIGNAL_SLASH, "signal-slash.svg"),
    heroicon!(icons::ARROW_DOWN_CIRCLE, "arrow-down-circle.svg"),
    heroicon!(icons::COMMAND_LINE, "command-line.svg"),
    heroicon!(icons::FOLDER_OPEN, "folder-open.svg"),
    heroicon!(icons::GLOBE_ALT, "globe-alt.svg"),
    heroicon!(icons::DOCUMENT_TEXT, "document-text.svg"),
];

/// Geist, the UI typeface (SIL Open Font License 1.1, vendored with its
/// license under `assets/fonts/geist/`, from the `geist` npm package 1.7.2).
const GEIST: &[&[u8]] = &[
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Regular.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Medium.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-SemiBold.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Bold.ttf")),
];

/// Register the vendored fonts, so views can set `font_family("Geist")`.
pub fn load_fonts(cx: &mut gpui::App) {
    let fonts = GEIST.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::warn!(error = %e, "could not load the Geist fonts");
    }
}

/// The app's asset source: the Heroicons first, then gpui-kit's assets.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = HEROICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = HEROICONS
            .iter()
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect();
        out.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_heroicon_loads_as_an_svg() {
        for path in HEROICONS.iter().map(|(p, _)| *p) {
            let bytes = Assets.load(path).unwrap().expect(path);
            assert!(bytes.starts_with(b"<svg"), "{path} is an SVG");
        }
    }

    #[test]
    fn heroicons_are_listed() {
        let listed = Assets.list("icons/heroicons/").unwrap();
        assert_eq!(listed.len(), HEROICONS.len());
    }
}
