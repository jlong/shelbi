//! What the app ships from others, for the Acknowledgements window
//! (`dt-about-bundled-software`): the fonts, the icon sets and the notable
//! crates, each with its full license text, plus one entry for the long
//! tail of Rust crates.
//!
//! Every text is embedded at build time. The fonts' and icons' come from
//! the license files vendored beside them under `assets/`; the crates' are
//! read out of their packages by the build script (`build.rs`, running
//! `crates/shelbi-app/build_support/licenses.rs`, which also walks `cargo
//! metadata` for the long tail). Nothing is fetched at runtime.
//!
//! The types, the crate entries the TUI's list shares, the search and the
//! paragraph layout are `shelbi_app::licenses`; this is the app's own list
//! and the license text as its text view reads it ([`license_markdown`]).

use std::sync::LazyLock;

use shelbi_app::licenses::{self as shared, Block, Table};
pub use shelbi_app::licenses::{Browser, Crate, Entry};

/// The build script's output: `CRATES` (the long tail), `CRATE_NOTICES`
/// (its text), `SHIPPED` (every shipped crate) and `crate_text` (a featured
/// crate's license file).
mod generated {
    use super::Crate;
    include!(concat!(env!("OUT_DIR"), "/licenses.rs"));
}

pub use generated::{CRATES, SHIPPED};

/// The app's license table.
const TABLE: Table = Table {
    crates: CRATES,
    notices: generated::CRATE_NOTICES,
    shipped: SHIPPED,
    crate_text: generated::crate_text,
};

/// Symbols Nerd Font Mono's glyph sets keep their upstream licenses; the
/// font's own README lists them (Nerd Fonts' `license-audit.md` has each in
/// full).
const NERD_GLYPHS: &str = "The glyph sets in the font keep their upstream licenses, \
as listed in the Nerd Fonts repository's license-audit.md: Font Awesome (SIL OFL 1.1), \
Material Design Icons (Apache 2.0), Codicons (CC BY 4.0), Octicons (MIT), and others.";

/// Every entry, in the order the list shows them.
pub fn entries() -> &'static [Entry] {
    static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(build);
    &ENTRIES
}

/// A browser over [`entries`], as the window opens on.
pub fn browser() -> Browser {
    Browser::new(entries())
}

fn build() -> Vec<Entry> {
    let entry = |name, license, attribution: String, description, source, license_title, text| Entry {
        name,
        license,
        attribution,
        description,
        source,
        license_title,
        text,
        crates: &[],
    };
    let mut list = shared::shared_entries(&TABLE);
    list.extend([
        entry(
            "Geist",
            "SIL OFL 1.1",
            "Vercel · Interface typeface".into(),
            "The typeface of Shelbi's interface.",
            "https://github.com/vercel/geist-font",
            "SIL Open Font License 1.1",
            include_str!("../assets/fonts/geist/LICENSE.txt"),
        ),
        entry(
            "Geist Mono",
            "SIL OFL 1.1",
            "Vercel · Terminal fallback typeface".into(),
            "The terminal's fallback for characters its font doesn't have.",
            "https://github.com/vercel/geist-font",
            "SIL Open Font License 1.1",
            include_str!("../assets/fonts/geist-mono/LICENSE.txt"),
        ),
        entry(
            "GPUI",
            "Apache 2.0",
            format!("Zed Industries · UI framework {}", TABLE.version("gpui-pre")),
            "The GPU-accelerated UI framework the app is built with.",
            "https://github.com/zed-industries/zed",
            "Apache License 2.0",
            TABLE.text("gpui-pre", "LICENSE-APACHE"),
        ),
        entry(
            "gpui-kit",
            "Apache 2.0",
            format!("Longbridge · Component library {}", TABLE.version("gpui-kit")),
            "The text views, inputs and other components the app's views build on.",
            "https://github.com/longbridge/gpui-kit",
            "Apache License 2.0",
            // gpui-kit's package ships no license file; gpui-base, from the
            // same repository and release, ships its Apache License.
            TABLE.text("gpui-base", "LICENSE-APACHE"),
        ),
        entry(
            "Heroicons",
            "MIT",
            "Tailwind Labs · Icons".into(),
            "The outline icons throughout Shelbi's interface.",
            "https://github.com/tailwindlabs/heroicons",
            "MIT License",
            crate::assets::HEROICONS_LICENSE,
        ),
        entry(
            "Lucide",
            "ISC",
            "Lucide Contributors · Icons".into(),
            "The few icons gpui-kit's components draw for themselves, like a dropdown's caret.",
            "https://github.com/lucide-icons/lucide",
            "ISC License",
            TABLE.text("gpui-kit-assets", "LICENSE-LUCIDE"),
        ),
        entry(
            "Source Code Pro",
            "SIL OFL 1.1",
            "Adobe · Terminal typeface".into(),
            "The default monospace font used in Shelbi's terminal.",
            "https://github.com/adobe-fonts/source-code-pro",
            "SIL Open Font License 1.1",
            include_str!("../assets/fonts/source-code-pro/LICENSE.md"),
        ),
        entry(
            "Symbols Nerd Font Mono",
            "Multiple licenses",
            "Nerd Fonts · Symbol font".into(),
            "The terminal's fallback for the icons shell prompts and agents print.",
            "https://github.com/ryanoasis/nerd-fonts",
            "MIT License, with the glyph sets' own licenses",
            NERD_TEXT.as_str(),
        ),
    ]);
    let tail = shared::long_tail(
        &TABLE,
        "The other Rust crates compiled into the app, each with its version, license and notice.",
        "https://github.com/jlong/shelbi/blob/main/desktop/Cargo.lock",
    );
    shared::listed(list, tail)
}

/// Symbols Nerd Font Mono's text: the build's MIT license, then what the
/// glyph sets are under.
static NERD_TEXT: LazyLock<String> = LazyLock::new(|| {
    format!("{}\n\n{NERD_GLYPHS}", include_str!("../assets/fonts/nerd-symbols/LICENSE").trim_end())
});

/// A license text as the detail's text view reads it (Markdown, with every
/// character literal): the `title` as a heading, then `text`'s blocks
/// ([`shelbi_app::licenses::license_blocks`], the paragraphs reflowed as
/// the frame sets them): a section heading as a `####`, a paragraph's lines
/// a hard break apart.
pub fn license_markdown(title: &str, text: &str) -> String {
    let mut out = format!("# {}", escape(title));
    for block in shared::license_blocks(text) {
        match block {
            Block::Heading(heading) => {
                out.push_str("\n\n#### ");
                out.push_str(&escape(&heading));
            }
            Block::Paragraph(lines) => {
                let lines: Vec<String> = lines.iter().map(|l| escape(l)).collect();
                out.push_str("\n\n");
                // A backslash at a line's end is a hard break.
                out.push_str(&lines.join("\\\n"));
            }
        }
    }
    out
}

/// `s` with every ASCII punctuation character escaped, so Markdown reads it
/// literally (no emphasis, lists, rules, links or HTML).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_has_its_license_text() {
        for e in entries() {
            assert!(!e.name.is_empty());
            assert!(!e.license.is_empty(), "{} has no license", e.name);
            assert!(!e.license_title.is_empty(), "{} has no license title", e.name);
            assert!(e.text.trim().len() > 200, "{}'s license text is missing or truncated", e.name);
            assert!(e.source.starts_with("https://"), "{}'s source", e.name);
            assert!(!e.description.is_empty() && !e.attribution.is_empty());
        }
    }

    /// The texts are the real ones: each names its license and its
    /// copyright holder as the shipped file does.
    #[test]
    fn the_texts_are_the_shipped_licenses() {
        let text = |name: &str| entries().iter().find(|e| e.name == name).unwrap().text;
        assert!(text("Source Code Pro").starts_with("© 2023 Adobe"));
        assert!(text("Source Code Pro").contains("SIL OPEN FONT LICENSE Version 1.1"));
        assert!(text("Geist").contains("Vercel"));
        assert!(text("Geist Mono").contains("SIL OPEN FONT LICENSE"));
        assert!(text("GPUI").contains("Zed Industries") && text("GPUI").contains("END OF TERMS AND CONDITIONS"));
        assert!(text("gpui-kit").contains("Longbridge") && text("gpui-kit").contains("Apache License"));
        assert!(text("Alacritty Terminal").contains("The Alacritty Project"));
        assert!(text("Heroicons").contains("Tailwind Labs"));
        assert!(text("Lucide").contains("ISC License") && text("Lucide").contains("Lucide Icons and Contributors"));
        assert!(text("nucleo-matcher").starts_with("Mozilla Public License Version 2.0"));
        assert!(text("option-ext").starts_with("Mozilla Public License Version 2.0"));
        assert!(text("portable-pty").contains("Wez Furlong"));
        assert!(text("termwiz").contains("Wez Furlong"));
        assert!(text("Symbols Nerd Font Mono").contains("Ryan L McIntyre"));
        assert!(text("Symbols Nerd Font Mono").contains("Font Awesome"));
    }

    /// Every featured crate is one the app links, and the long tail lists
    /// every other shipped crate except Shelbi's own.
    #[test]
    fn the_list_is_what_ships() {
        let mut featured = shared::SHARED_FEATURED.to_vec();
        featured.extend(["gpui-pre", "gpui-kit"]);
        for name in featured.iter().chain(&["gpui-kit-assets"]) {
            assert!(SHIPPED.iter().any(|c| c.name == *name), "{name} doesn't ship");
        }
        for c in SHIPPED {
            let own = c.name.starts_with("shelbi");
            let listed = CRATES.contains(c);
            assert_eq!(
                listed,
                !own && !featured.contains(&c.name),
                "{} {}: listed {listed}",
                c.name,
                c.version
            );
        }
        // A proc-macro runs in the compiler; it isn't shipped code.
        assert!(!SHIPPED.iter().any(|c| c.name == "serde_derive"));
        assert!(SHIPPED.iter().any(|c| c.name == "serde_json"));
        assert!(CRATES.len() > 100);
    }

    /// The long tail's text lists every crate and carries a notice for
    /// each.
    #[test]
    fn the_long_tail_lists_and_covers_every_crate() {
        let notices = generated::CRATE_NOTICES;
        let (list, rest) = notices.split_once("\nNOTICES").expect("the notices follow the list");
        for c in CRATES {
            assert!(
                list.contains(&format!("{} {} · {}", c.name, c.version, c.license)),
                "{} {} isn't listed",
                c.name,
                c.version
            );
            let id = format!("{} {}", c.name, c.version);
            assert!(
                rest.lines()
                    .filter(|l| l.starts_with("Applies to "))
                    .any(|l| l.trim_start_matches("Applies to ").trim_end_matches('.').split(", ").any(|n| n == id)),
                "{id} has no notice"
            );
        }
        assert!(rest.contains("Permission is hereby granted, free of charge"));
    }

    #[test]
    fn the_long_tail_is_last_and_the_rest_by_name() {
        let names: Vec<&str> = entries().iter().map(|e| e.name).collect();
        assert_eq!(names.last(), Some(&"Rust crates"));
        let mut sorted = names[..names.len() - 1].to_vec();
        sorted.sort_by_key(|n| n.to_lowercase());
        assert_eq!(sorted, names[..names.len() - 1]);
        assert_eq!(names[0], "Alacritty Terminal");
    }

    #[test]
    fn search_finds_by_name_license_or_crate() {
        let found = |q: &str| -> Vec<&str> {
            entries().iter().filter(|e| e.matches(&q.to_lowercase())).map(|e| e.name).collect()
        };
        assert_eq!(found("").len(), entries().len());
        assert_eq!(found("geist"), ["Geist", "Geist Mono"]);
        assert!(found("ofl").contains(&"Source Code Pro"));
        assert!(found("mpl").contains(&"nucleo-matcher") && found("mpl").contains(&"option-ext"));
        assert_eq!(found("serde"), ["Rust crates"], "a long-tail crate finds its entry");
        assert!(found("zzzz-nothing").is_empty());
    }

    /// The search and the arrows are `shelbi_app::licenses::Browser`'s
    /// (tested there); over the app's list it opens on the first entry.
    #[test]
    fn the_browser_opens_on_the_first_entry() {
        let mut b = browser();
        assert_eq!(b.selected_entry().map(|e| e.name), Some("Alacritty Terminal"));
        b.set_query("ofl");
        let names: Vec<&str> = b.visible().into_iter().map(|i| entries()[i].name).collect();
        assert_eq!(names, ["Geist", "Geist Mono", "Source Code Pro"]);
    }

    #[test]
    fn license_text_reflows_but_keeps_items_rules_copyrights_and_lists() {
        let md = license_markdown(
            "T",
            "PREAMBLE\nThe goals of the\nlicense are\n\n(a) one\n    more\n(b) two\n\n-----\nSIL OPEN\n-----\n\n\
             Copyright (c) A\nCopyright (c) B\n\nfoo 1.0 · MIT\nbar 2.0 · MIT OR Apache-2.0",
        );
        let blocks: Vec<&str> = md.split("\n\n").collect();
        assert_eq!(
            blocks,
            [
                "# T",
                "#### PREAMBLE",
                "The goals of the license are",
                "\\(a\\) one more\\\n\\(b\\) two",
                "\\-\\-\\-\\-\\-\\\nSIL OPEN\\\n\\-\\-\\-\\-\\-",
                "Copyright \\(c\\) A\\\nCopyright \\(c\\) B",
                "foo 1\\.0 · MIT\\\nbar 2\\.0 · MIT OR Apache\\-2\\.0",
            ]
        );
        // An all-caps paragraph (a disclaimer) is body text, not headings.
        let md = license_markdown("T", "THE SOFTWARE IS PROVIDED\nAS IS, WITHOUT WARRANTY");
        assert_eq!(md, "# T\n\nTHE SOFTWARE IS PROVIDED AS IS\\, WITHOUT WARRANTY");
    }

    #[test]
    fn license_text_reads_literally() {
        let md = license_markdown("MIT License", "Copyright (c) 2020 *Someone*\n\n1) one\n   two\n\n-----\n\nPREAMBLE\n\n<b>x</b> [a](b)");
        assert!(md.starts_with("# MIT License"));
        assert!(md.contains("Copyright \\(c\\) 2020 \\*Someone\\*"));
        assert!(md.contains("1\\) one two"), "wrapped lines join, indentation dropped: {md}");
        assert!(md.contains("\\-\\-\\-\\-\\-"));
        assert!(md.contains("#### PREAMBLE"));
        assert!(md.contains("\\<b\\>x\\<\\/b\\> \\[a\\]\\(b\\)"));
        // Parsed, the text comes back as written.
        let parsed = markdown::to_mdast(&md, &markdown::ParseOptions::gfm()).unwrap().to_string();
        assert!(parsed.contains("Copyright (c) 2020 *Someone*"), "{parsed}");
        assert!(parsed.contains("<b>x</b> [a](b)"), "{parsed}");
    }
}
