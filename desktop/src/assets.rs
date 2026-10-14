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
];

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
        for path in [
            icons::CHAT_BUBBLE_LEFT_RIGHT,
            icons::VIEW_COLUMNS,
            icons::BOLT,
            icons::SERVER_STACK,
            icons::X_MARK,
        ] {
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
