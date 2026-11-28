//! A link clicked in Markdown (a task body, a wiki document): a
//! `wiki:Plans/foo.md` link opens that document in the window's Wiki
//! ([`OpenWikiLink`], which the window handles as the palette's wiki result:
//! `CommandKind::OpenWikiDoc`); anything else opens in the browser.

use gpui::{App, Window};

/// Open the wiki document at `path` (wiki-relative) in this window's Wiki.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = shelbi_links, no_json)]
pub struct OpenWikiLink {
    pub path: String,
}

/// Open `url`: a `wiki:` link in the Wiki, anything else in the browser.
pub fn open(url: &str, window: &mut Window, cx: &mut App) {
    match shelbi_app::wiki_doc::link_path(url) {
        Some(path) => window.dispatch_action(Box::new(OpenWikiLink { path }), cx),
        None => cx.open_url(url),
    }
}
