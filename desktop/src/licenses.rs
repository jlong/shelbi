//! What the app ships from others, for the Bundled Software window
//! (`dt-about-bundled-software`): the fonts, the icon sets and the notable
//! crates, each with its full license text, plus one entry for the long
//! tail of Rust crates.
//!
//! Every text is embedded at build time. The fonts' and icons' come from
//! the license files vendored beside them under `assets/`; the crates' are
//! read out of their packages by the build script (`build_support/licenses.rs`,
//! which also walks `cargo metadata` for the long tail). Nothing is fetched
//! at runtime.
//!
//! The entries are listed by name (the long tail last); [`Entry::matches`]
//! is the window's search.

use std::sync::LazyLock;

/// A shipped Rust crate: its name, version and license as an SPDX
/// expression (`Cargo.toml`'s `license`, an old `/` spelled `OR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crate {
    pub name: &'static str,
    pub version: &'static str,
    pub license: &'static str,
}

/// The build script's output: `CRATES` (the long tail), `CRATE_NOTICES`
/// (its text), `SHIPPED` (every shipped crate) and `crate_text` (a featured
/// crate's license file).
mod generated {
    use super::Crate;
    include!(concat!(env!("OUT_DIR"), "/licenses.rs"));
}

pub use generated::{CRATES, SHIPPED};

/// One row of Bundled Software and the detail it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The row's and the detail's name.
    pub name: &'static str,
    /// The row's second line: the license, short.
    pub license: &'static str,
    /// Who made it and what it is (`Adobe · Terminal typeface`).
    pub attribution: String,
    /// What it does in Shelbi, one line.
    pub description: &'static str,
    /// Where View source goes.
    pub source: &'static str,
    /// The license text's heading.
    pub license_title: &'static str,
    /// The complete license text, as shipped.
    pub text: &'static str,
    /// The crates the entry stands for (the long tail's), searched by name
    /// and license too.
    pub crates: &'static [Crate],
}

impl Entry {
    /// Whether `query` (already lowercased and trimmed) finds this entry:
    /// in its name or its license, or in one of its crates' names or
    /// licenses. An empty query finds everything.
    pub fn matches(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let has = |s: &str| s.to_lowercase().contains(query);
        has(self.name)
            || has(self.license)
            || has(self.license_title)
            || self.crates.iter().any(|c| has(c.name) || has(c.license))
    }
}

/// A featured crate's license file, as the build script read it.
fn crate_text(name: &str, file: &str) -> &'static str {
    generated::crate_text(name, file).map(|(_, text)| text).unwrap_or_else(|| {
        // The build script panics before this can happen: it reads every
        // file `FEATURED_FILES` names.
        panic!("{name}'s {file} isn't in the build's license table")
    })
}

/// A featured crate's version, from the lockfile.
fn crate_version(name: &str) -> &'static str {
    SHIPPED
        .iter()
        .find(|c| c.name == name)
        .map(|c| c.version)
        .unwrap_or_default()
}

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
    let mut list = vec![
        entry(
            "Alacritty Terminal",
            "Apache 2.0",
            format!("The Alacritty Project · Terminal emulator {}", crate_version("alacritty_terminal")),
            "Reads every session's output into the screen its terminal pane draws.",
            "https://github.com/alacritty/alacritty",
            "Apache License 2.0",
            crate_text("alacritty_terminal", "LICENSE-APACHE"),
        ),
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
            format!("Zed Industries · UI framework {}", crate_version("gpui-pre")),
            "The GPU-accelerated UI framework the app is built with.",
            "https://github.com/zed-industries/zed",
            "Apache License 2.0",
            crate_text("gpui-pre", "LICENSE-APACHE"),
        ),
        entry(
            "gpui-kit",
            "Apache 2.0",
            format!("Longbridge · Component library {}", crate_version("gpui-kit")),
            "The text views, inputs and other components the app's views build on.",
            "https://github.com/longbridge/gpui-kit",
            "Apache License 2.0",
            // gpui-kit's package ships no license file; gpui-base, from the
            // same repository and release, ships its Apache License.
            crate_text("gpui-base", "LICENSE-APACHE"),
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
            crate_text("gpui-kit-assets", "LICENSE-LUCIDE"),
        ),
        entry(
            "nucleo-matcher",
            "MPL 2.0",
            format!("Pascal Kuthe · Fuzzy matcher {}", crate_version("nucleo-matcher")),
            "Ranks the command palette's results as you type.",
            "https://github.com/helix-editor/nucleo",
            "Mozilla Public License 2.0",
            crate_text("nucleo-matcher", "LICENSE"),
        ),
        entry(
            "option-ext",
            "MPL 2.0",
            format!("Simon Ochsenreither · Rust library {}", crate_version("option-ext")),
            "A helper inside the dirs crate, which finds your home and config folders.",
            "https://github.com/soc/option-ext",
            "Mozilla Public License 2.0",
            crate_text("option-ext", "LICENSE.txt"),
        ),
        entry(
            "portable-pty",
            "MIT",
            format!("Wez Furlong · Pseudoterminals {}", crate_version("portable-pty")),
            "Opens the pseudoterminal each Shelbi session runs its program in.",
            "https://github.com/wezterm/wezterm",
            "MIT License",
            crate_text("portable-pty", "LICENSE.md"),
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
        entry(
            "termwiz",
            "MIT",
            format!("Wez Furlong · Terminal toolkit {}", crate_version("termwiz")),
            "Parses the escape sequences in session output.",
            "https://github.com/wezterm/wezterm",
            "MIT License",
            crate_text("termwiz", "LICENSE.md"),
        ),
    ];
    list.sort_by_key(|e| e.name.to_lowercase());
    list.push(Entry {
        name: "Rust crates",
        license: "Various licenses",
        attribution: format!("{} open source libraries", CRATES.len()),
        description: "The other Rust crates compiled into the app, each with its version, license and notice.",
        source: "https://github.com/jlong/shelbi/blob/main/desktop/Cargo.lock",
        license_title: "Licenses and notices",
        text: generated::CRATE_NOTICES,
        crates: CRATES,
    });
    list
}

/// Bundled Software's list: the search and the selected entry, as indices
/// into [`entries`]. The selection stays on a row the search shows: one it
/// hides moves to the first that matches, and with nothing matching there is
/// none until the search shows rows again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Browser {
    query: String,
    selected: usize,
}

impl Browser {
    /// The first entry selected, nothing searched.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows the search shows, in order.
    pub fn visible(&self) -> Vec<usize> {
        let q = self.query.trim().to_lowercase();
        entries()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.matches(&q))
            .map(|(i, _)| i)
            .collect()
    }

    /// The selected entry, unless the search hides every row.
    pub fn selected(&self) -> Option<usize> {
        self.visible().contains(&self.selected).then_some(self.selected)
    }

    /// Search for `query`, keeping the selection if it still shows.
    pub fn set_query(&mut self, query: &str) {
        self.query = query.to_string();
        let visible = self.visible();
        if !visible.contains(&self.selected) {
            if let Some(first) = visible.first() {
                self.selected = *first;
            }
        }
    }

    /// Select entry `ix` (a clicked row).
    pub fn select(&mut self, ix: usize) {
        if ix < entries().len() {
            self.selected = ix;
        }
    }

    /// Move the selection `delta` rows through the visible ones (↑ / ↓),
    /// stopping at either end.
    pub fn step(&mut self, delta: isize) {
        let visible = self.visible();
        let Some(at) = visible.iter().position(|i| *i == self.selected) else {
            if let Some(first) = visible.first() {
                self.selected = *first;
            }
            return;
        };
        let to = (at as isize + delta).clamp(0, visible.len() as isize - 1) as usize;
        self.selected = visible[to];
    }
}

/// Symbols Nerd Font Mono's text: the build's MIT license, then what the
/// glyph sets are under.
static NERD_TEXT: LazyLock<String> = LazyLock::new(|| {
    format!("{}\n\n{NERD_GLYPHS}", include_str!("../assets/fonts/nerd-symbols/LICENSE").trim_end())
});

/// A license text as the detail's text view reads it (Markdown, with every
/// character literal): the `title` as a heading, then each paragraph of
/// `text` (a run of lines between blank ones) reflowed to the pane's width,
/// as the frame sets them: a file's hard wrap joins up, its indentation
/// drops, but a list item, a rule (`-----`) or a copyright line starts a
/// line of its own, and a list of crates (every line `name version ·
/// license`) keeps its lines. A short line in capitals (`PREAMBLE`, `TERMS
/// AND CONDITIONS`), alone or over body text, is a section heading.
pub fn license_markdown(title: &str, text: &str) -> String {
    let mut out = format!("# {}", escape(title));
    let text = text.replace("\r\n", "\n");
    let mut paragraph: Vec<&str> = Vec::new();
    let flush = |paragraph: &mut Vec<&str>, out: &mut String| {
        if paragraph.is_empty() {
            return;
        }
        let mut body: &[&str] = paragraph;
        let heads_body = body.len() == 1 || body.get(1).is_some_and(|l| l.chars().any(char::is_lowercase));
        if is_section_heading(body[0]) && heads_body {
            out.push_str("\n\n#### ");
            out.push_str(&escape(body[0]));
            body = &body[1..];
        }
        if !body.is_empty() {
            let lines: Vec<String> = reflow(body).iter().map(|l| escape(l)).collect();
            out.push_str("\n\n");
            // A backslash at a line's end is a hard break.
            out.push_str(&lines.join("\\\n"));
        }
        paragraph.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut paragraph, &mut out);
        } else {
            paragraph.push(line);
        }
    }
    flush(&mut paragraph, &mut out);
    out
}

/// A paragraph's lines joined where the file only wrapped them.
fn reflow(lines: &[&str]) -> Vec<String> {
    if lines.iter().all(|l| l.contains(" · ")) {
        return lines.iter().map(|l| l.to_string()).collect();
    }
    let mut out: Vec<String> = Vec::new();
    let mut after_rule = false;
    for line in lines {
        let rule = is_rule(line);
        let own_line = rule || after_rule || starts_item(line) || starts_copyright(line);
        match out.last_mut() {
            Some(last) if !own_line => {
                last.push(' ');
                last.push_str(line);
            }
            _ => out.push(line.to_string()),
        }
        after_rule = rule;
    }
    out
}

/// `-----`, `=====`: a line drawn with one character.
fn is_rule(line: &str) -> bool {
    line.len() >= 3 && line.chars().all(|c| matches!(c, '-' | '=' | '_' | '*'))
}

/// A list item: `- x`, `* x`, `• x`, `1. x`, `2) x`, `a) x`, `(b) x`,
/// `(iv) x`.
fn starts_item(line: &str) -> bool {
    let mut chars = line.chars();
    if let (Some(c), Some(' ')) = (chars.next(), chars.next()) {
        if matches!(c, '-' | '*' | '•' | '+') {
            return true;
        }
    }
    let rest = line.strip_prefix('(').unwrap_or(line);
    let marker: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
    let after = &rest[marker.len()..];
    let numbered = !marker.is_empty()
        && (marker.chars().all(|c| c.is_ascii_digit()) && marker.len() <= 3
            || marker.chars().count() == 1 && marker.chars().all(|c| c.is_ascii_alphabetic())
            || marker.len() <= 4 && marker.chars().all(|c| matches!(c, 'i' | 'v' | 'x')));
    numbered && (after.starts_with(". ") || after.starts_with(") "))
}

/// `Copyright (c) 2018 …`, `© 2023 Adobe`.
fn starts_copyright(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.starts_with("copyright") || lower.starts_with('©') || lower.starts_with("(c) ")
}

/// `PREAMBLE`, `PERMISSION & CONDITIONS`: a short line with letters and no
/// lowercase ones.
fn is_section_heading(line: &str) -> bool {
    line.chars().count() <= 64
        && line.chars().filter(|c| c.is_alphabetic()).count() >= 3
        && !line.chars().any(char::is_lowercase)
        && !is_rule(line)
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
        for name in ["alacritty_terminal", "gpui-pre", "gpui-kit", "gpui-kit-assets", "nucleo-matcher", "option-ext", "portable-pty", "termwiz"] {
            assert!(SHIPPED.iter().any(|c| c.name == name), "{name} doesn't ship");
        }
        let featured = ["alacritty_terminal", "gpui-pre", "gpui-kit", "nucleo-matcher", "option-ext", "portable-pty", "termwiz"];
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

    #[test]
    fn the_browser_opens_on_the_first_entry_and_arrows_move_through_the_rows() {
        let mut b = Browser::new();
        assert_eq!(b.selected(), Some(0));
        b.step(-1);
        assert_eq!(b.selected(), Some(0), "stops at the top");
        b.step(1);
        b.step(1);
        assert_eq!(b.selected(), Some(2));
        b.step(100);
        assert_eq!(b.selected(), Some(entries().len() - 1), "stops at the bottom");
    }

    #[test]
    fn a_search_keeps_a_selection_it_shows_and_moves_one_it_hides() {
        let ix = |name: &str| entries().iter().position(|e| e.name == name).unwrap();
        let mut b = Browser::new();
        b.select(ix("Geist Mono"));
        b.set_query("geist");
        assert_eq!(b.visible(), [ix("Geist"), ix("Geist Mono")]);
        assert_eq!(b.selected(), Some(ix("Geist Mono")), "still shown, still selected");
        b.set_query("MPL");
        assert_eq!(b.selected(), Some(ix("nucleo-matcher")), "hidden: the first match");
        b.step(1);
        assert_eq!(b.selected(), Some(ix("option-ext")), "the arrows move through the matches");
        b.set_query("zzzz-nothing");
        assert_eq!(b.selected(), None);
        assert!(b.visible().is_empty());
        b.set_query("");
        assert_eq!(b.selected(), Some(ix("option-ext")), "cleared: the selection comes back");
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
