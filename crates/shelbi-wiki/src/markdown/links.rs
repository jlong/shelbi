// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-markdown/src/links.rs), MIT in Shelbi.

//! Link extraction — both standard markdown `[text](href)` and
//! wiki-style `[[name]]` / `[[name|label]]` links.
//!
//! Image links (`![alt](href)`) are intentionally excluded — those are
//! display, not navigation.

use comrak::nodes::NodeValue;
use comrak::Arena;

use crate::markdown::comrak_options;

/// A link extracted from a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The link target — a URL, a wiki page name, etc.
    pub href: String,

    /// The link label as it appears in the source. For
    /// `[Anchor](http://example.com)` this is `"Anchor"`; for `[[name]]`
    /// it is `"name"` (or the alias portion of `[[name|alias]]`).
    pub text: String,

    /// 1-based line index of the link's start in the source document.
    pub line: usize,
}

/// Extract every navigation link from `content`, in source order.
///
/// Includes standard markdown links and wiki-links (`[[name]]` and
/// `[[name|alias]]`). Image links are skipped.
pub fn extract(content: &str) -> Vec<Link> {
    let arena = Arena::new();
    let options = comrak_options();
    let root = comrak::parse_document(&arena, content, &options);

    let mut links = Vec::new();
    for node in root.descendants() {
        let data = node.data.borrow();
        match &data.value {
            NodeValue::Link(link) => {
                links.push(Link {
                    href: link.url.clone(),
                    text: inline_text(node),
                    line: data.sourcepos.start.line,
                });
            }
            NodeValue::WikiLink(wiki) => {
                let text = inline_text(node);
                // comrak's WikiLink puts the target page name in `url`.
                // Per the sub-plan we report the alias when one is
                // present (`[[name|alias]]`), else the target itself.
                let display = if text.is_empty() {
                    wiki.url.clone()
                } else {
                    text
                };
                links.push(Link {
                    href: wiki.url.clone(),
                    text: display,
                    line: data.sourcepos.start.line,
                });
            }
            _ => {}
        }
    }
    links
}

fn inline_text<'a>(
    node: &'a comrak::arena_tree::Node<'a, std::cell::RefCell<comrak::nodes::Ast>>,
) -> String {
    let mut out = String::new();
    for child in node.descendants().skip(1) {
        let data = child.data.borrow();
        match &data.value {
            NodeValue::Text(t) => out.push_str(t),
            NodeValue::Code(c) => out.push_str(&c.literal),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_returns_standard_markdown_links() {
        let md = "See [Rust](https://rust-lang.org) for details.\n";
        let links = extract(md);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "https://rust-lang.org");
        assert_eq!(links[0].text, "Rust");
        assert_eq!(links[0].line, 1);
    }

    #[test]
    fn extract_returns_wikilinks_bare() {
        let md = "Refer to [[planning]] for details.\n";
        let links = extract(md);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "planning");
        assert_eq!(links[0].text, "planning");
    }

    #[test]
    fn extract_returns_wikilinks_with_alias() {
        let md = "Refer to [[planning|the planning space]] for details.\n";
        let links = extract(md);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "planning");
        assert_eq!(links[0].text, "the planning space");
    }

    #[test]
    fn extract_skips_image_links() {
        let md = "![alt text](image.png)\n\n[real link](https://example.com)\n";
        let links = extract(md);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "https://example.com");
    }

    #[test]
    fn extract_returns_empty_when_no_links_present() {
        assert!(extract("just plain text\n").is_empty());
    }

    #[test]
    fn extract_handles_mixed_markdown_and_wiki_links_in_source_order() {
        let md = "[a](http://a.com) and [[wiki]] and [b](http://b.com)\n";
        let links = extract(md);
        let hrefs: Vec<&str> = links.iter().map(|l| l.href.as_str()).collect();
        assert_eq!(hrefs, vec!["http://a.com", "wiki", "http://b.com"]);
    }
}
