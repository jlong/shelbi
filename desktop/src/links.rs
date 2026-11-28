//! A link clicked in Markdown (a task body, a document): a
//! `docs:Plans/foo.md` link opens that document in the window's Documentation
//! ([`OpenDocLink`], which the window handles as the palette's docs result:
//! `CommandKind::OpenDoc`); anything else opens in the browser.

use gpui::{App, Window};

/// Open the document at `path` (docs-relative) in this window's Documentation.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = shelbi_links, no_json)]
pub struct OpenDocLink {
    pub path: String,
}

/// Open `url`: a `docs:` link in the Documentation, anything else in the browser.
pub fn open(url: &str, window: &mut Window, cx: &mut App) {
    match shelbi_app::docs_document::link_path(url) {
        Some(path) => window.dispatch_action(Box::new(OpenDocLink { path }), cx),
        None => cx.open_url(url),
    }
}
