//! Acknowledgements (`dt-about-bundled-software`, `tui-acknowledgements`):
//! what a Shelbi binary ships from others, each with its full license text,
//! and the browser both clients show it in (the desktop's window, Figma
//! "Bundled Software · Dark", 2453:8397; the TUI's full-screen view).
//!
//! The two clients are different binaries, so each has its own list: the
//! desktop app ships fonts, icons and gpui besides the crates, the `shelbi`
//! binary (the TUI's) ratatui and crossterm. What they share lives here:
//!
//! - the data's types ([`Crate`], [`Entry`]) and a binary's generated
//!   license table ([`Table`]), which its build script writes with
//!   `build_support/licenses.rs` (`cargo metadata` walked from the binary,
//!   every text read out of the crates' packages; nothing is fetched at
//!   runtime);
//! - the crate entries both binaries ship ([`shared_entries`]), the long
//!   tail's entry ([`long_tail`]) and the list's order ([`listed`]: by name,
//!   the long tail last);
//! - the search and selection ([`Browser`], [`Entry::matches`]);
//! - a license text's layout ([`license_blocks`]): its paragraphs reflowed
//!   as the frame sets them, for each client to draw.

/// A shipped Rust crate: its name, version and license as an SPDX
/// expression (`Cargo.toml`'s `license`, an old `/` spelled `OR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crate {
    pub name: &'static str,
    pub version: &'static str,
    pub license: &'static str,
}

/// One row of Acknowledgements and the detail it opens.
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

/// A binary's generated license table: what its build script wrote
/// (`$OUT_DIR/licenses.rs`, `include!`d beside the binary's list).
#[derive(Debug, Clone, Copy)]
pub struct Table {
    /// The long tail: every shipped crate not listed on its own.
    pub crates: &'static [Crate],
    /// The long tail's text: the crates, then each distinct notice once.
    pub notices: &'static str,
    /// Every shipped crate, Shelbi's own included.
    pub shipped: &'static [Crate],
    /// A featured entry's license file, `(crate, file)` to its version and
    /// text.
    pub crate_text: fn(&str, &str) -> Option<(&'static str, &'static str)>,
}

impl Table {
    /// A featured crate's license file, as the build script read it.
    pub fn text(&self, name: &str, file: &str) -> &'static str {
        (self.crate_text)(name, file).map(|(_, text)| text).unwrap_or_else(|| {
            // The build script panics before this can happen: it reads every
            // featured file its config names.
            panic!("{name}'s {file} isn't in the build's license table")
        })
    }

    /// A shipped crate's version, from the lockfile: the newest, where two
    /// ship.
    pub fn version(&self, name: &str) -> &'static str {
        self.shipped
            .iter()
            .rev()
            .find(|c| c.name == name)
            .map(|c| c.version)
            .unwrap_or_default()
    }
}

/// The crates [`shared_entries`] lists on their own, so a binary's build
/// script leaves them out of its long tail.
pub const SHARED_FEATURED: &[&str] = &["alacritty_terminal", "nucleo-matcher", "option-ext", "portable-pty", "termwiz"];

/// The license files [`shared_entries`] shows, `(crate, file)`, for the
/// build scripts to read.
pub const SHARED_FILES: &[(&str, &str)] = &[
    ("alacritty_terminal", "LICENSE-APACHE"),
    ("nucleo-matcher", "LICENSE"),
    ("option-ext", "LICENSE.txt"),
    ("portable-pty", "LICENSE.md"),
    ("termwiz", "LICENSE.md"),
];

/// The crate entries both binaries ship: the terminal emulator, the
/// palette's matcher, the MPL-licensed helper, the pseudoterminals and the
/// escape-sequence parser.
pub fn shared_entries(table: &Table) -> Vec<Entry> {
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
    vec![
        entry(
            "Alacritty Terminal",
            "Apache 2.0",
            format!("The Alacritty Project · Terminal emulator {}", table.version("alacritty_terminal")),
            "Reads every session's output into the screen its terminal pane draws.",
            "https://github.com/alacritty/alacritty",
            "Apache License 2.0",
            table.text("alacritty_terminal", "LICENSE-APACHE"),
        ),
        entry(
            "nucleo-matcher",
            "MPL 2.0",
            format!("Pascal Kuthe · Fuzzy matcher {}", table.version("nucleo-matcher")),
            "Ranks the command palette's results as you type.",
            "https://github.com/helix-editor/nucleo",
            "Mozilla Public License 2.0",
            table.text("nucleo-matcher", "LICENSE"),
        ),
        entry(
            "option-ext",
            "MPL 2.0",
            format!("Simon Ochsenreither · Rust library {}", table.version("option-ext")),
            "A helper inside the dirs crate, which finds your home and config folders.",
            "https://github.com/soc/option-ext",
            "Mozilla Public License 2.0",
            table.text("option-ext", "LICENSE.txt"),
        ),
        entry(
            "portable-pty",
            "MIT",
            format!("Wez Furlong · Pseudoterminals {}", table.version("portable-pty")),
            "Opens the pseudoterminal each Shelbi session runs its program in.",
            "https://github.com/wezterm/wezterm",
            "MIT License",
            table.text("portable-pty", "LICENSE.md"),
        ),
        entry(
            "termwiz",
            "MIT",
            format!("Wez Furlong · Terminal toolkit {}", table.version("termwiz")),
            "Parses the escape sequences in session output.",
            "https://github.com/wezterm/wezterm",
            "MIT License",
            table.text("termwiz", "LICENSE.md"),
        ),
    ]
}

/// The long tail's one entry: every other crate in `table`, with their
/// licenses and notices. `description` says what they're compiled into;
/// `source` is the lockfile that pins them.
pub fn long_tail(table: &Table, description: &'static str, source: &'static str) -> Entry {
    Entry {
        name: "Rust crates",
        license: "Various licenses",
        attribution: format!("{} open source libraries", table.crates.len()),
        description,
        source,
        license_title: "Licenses and notices",
        text: table.notices,
        crates: table.crates,
    }
}

/// The list as it shows: `featured` by name, then the long tail.
pub fn listed(mut featured: Vec<Entry>, tail: Entry) -> Vec<Entry> {
    featured.sort_by_key(|e| e.name.to_lowercase());
    featured.push(tail);
    featured
}

/// Acknowledgements' list: the search and the selected entry, as indices
/// into its entries. The selection stays on a row the search shows: one it
/// hides moves to the first that matches, and with nothing matching there is
/// none until the search shows rows again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Browser {
    entries: &'static [Entry],
    query: String,
    selected: usize,
}

impl Browser {
    /// Over `entries`, the first selected, nothing searched.
    pub fn new(entries: &'static [Entry]) -> Self {
        Browser {
            entries,
            query: String::new(),
            selected: 0,
        }
    }

    pub fn entries(&self) -> &'static [Entry] {
        self.entries
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows the search shows, in order.
    pub fn visible(&self) -> Vec<usize> {
        let q = self.query.trim().to_lowercase();
        self.entries
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

    /// The selected entry itself.
    pub fn selected_entry(&self) -> Option<&'static Entry> {
        self.selected().map(|ix| &self.entries[ix])
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
        if ix < self.entries.len() {
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

/// One block of a license text, as [`license_blocks`] lays it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A section heading (`PREAMBLE`, `TERMS AND CONDITIONS`).
    Heading(String),
    /// A paragraph: its lines, each to start on a line of its own (a
    /// client wraps a long one to its width).
    Paragraph(Vec<String>),
}

/// A license text's blocks, as the frame sets them: each paragraph of
/// `text` (a run of lines between blank ones) reflowed, a file's hard wrap
/// joined up and its indentation dropped, but a list item, a rule (`-----`)
/// or a copyright line starts a line of its own, and a list of crates
/// (every line `name version · license`) keeps its lines. A short line in
/// capitals (`PREAMBLE`, `TERMS AND CONDITIONS`), alone or over body text,
/// is a section heading.
pub fn license_blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let text = text.replace("\r\n", "\n");
    let mut paragraph: Vec<&str> = Vec::new();
    let flush = |paragraph: &mut Vec<&str>, out: &mut Vec<Block>| {
        if paragraph.is_empty() {
            return;
        }
        let mut body: &[&str] = paragraph;
        let heads_body = body.len() == 1 || body.get(1).is_some_and(|l| l.chars().any(char::is_lowercase));
        if is_section_heading(body[0]) && heads_body {
            out.push(Block::Heading(body[0].to_string()));
            body = &body[1..];
        }
        if !body.is_empty() {
            out.push(Block::Paragraph(reflow(body)));
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

/// `Copyright (c) 2018 …`, `Copyright 2016 …`, `© 2023 Adobe`: the word
/// followed by the sign or a year, so a wrapped sentence that happens to
/// start with it (`copyright notice that is included …`) still joins up.
fn starts_copyright(line: &str) -> bool {
    let lower = line.to_lowercase();
    if lower.starts_with('©') || lower.starts_with("(c) ") {
        return true;
    }
    let Some(rest) = lower.strip_prefix("copyright") else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', ':']);
    rest.starts_with("(c)") || rest.starts_with('©') || rest.starts_with(|c: char| c.is_ascii_digit())
}

/// `PREAMBLE`, `PERMISSION & CONDITIONS`: a short line with letters and no
/// lowercase ones.
fn is_section_heading(line: &str) -> bool {
    line.chars().count() <= 64
        && line.chars().filter(|c| c.is_alphabetic()).count() >= 3
        && !line.chars().any(char::is_lowercase)
        && !is_rule(line)
}

/// A fixture list for tests that need entries without a build's table.
#[cfg(any(test, feature = "test-support"))]
pub fn fixture_entries() -> &'static [Entry] {
    use std::sync::LazyLock;
    static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(|| {
        const TAIL: &[Crate] = &[
            Crate { name: "serde", version: "1.0.0", license: "MIT OR Apache-2.0" },
            Crate { name: "foldhash", version: "0.1.5", license: "Zlib" },
        ];
        let entry = |name, license, license_title| Entry {
            name,
            license,
            attribution: format!("Someone · {name}"),
            description: "What it does.",
            source: "https://example.com",
            license_title,
            text: "Permission is hereby granted.",
            crates: &[],
        };
        let featured = vec![
            entry("Geist", "SIL OFL 1.1", "SIL Open Font License 1.1"),
            entry("Geist Mono", "SIL OFL 1.1", "SIL Open Font License 1.1"),
            entry("nucleo-matcher", "MPL 2.0", "Mozilla Public License 2.0"),
            entry("option-ext", "MPL 2.0", "Mozilla Public License 2.0"),
            entry("Alacritty Terminal", "Apache 2.0", "Apache License 2.0"),
            entry("Source Code Pro", "SIL OFL 1.1", "SIL Open Font License 1.1"),
        ];
        let table = Table {
            crates: TAIL,
            notices: "CRATES\n\nserde 1.0.0 · MIT OR Apache-2.0\nfoldhash 0.1.5 · Zlib",
            shipped: TAIL,
            crate_text: |_, _| None,
        };
        listed(featured, long_tail(&table, "The other crates.", "https://example.com/Cargo.lock"))
    });
    &ENTRIES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> &'static [Entry] {
        fixture_entries()
    }

    fn ix(name: &str) -> usize {
        entries().iter().position(|e| e.name == name).unwrap()
    }

    #[test]
    fn the_long_tail_is_last_and_the_rest_by_name() {
        let names: Vec<&str> = entries().iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            ["Alacritty Terminal", "Geist", "Geist Mono", "nucleo-matcher", "option-ext", "Source Code Pro", "Rust crates"]
        );
        let tail = entries().last().unwrap();
        assert_eq!(tail.attribution, "2 open source libraries");
        assert_eq!(tail.license, "Various licenses");
    }

    #[test]
    fn the_table_reads_the_newest_version() {
        const SHIPPED: &[Crate] = &[
            Crate { name: "crossterm", version: "0.25.0", license: "MIT" },
            Crate { name: "crossterm", version: "0.28.1", license: "MIT" },
        ];
        let table = Table {
            crates: &[],
            notices: "",
            shipped: SHIPPED,
            crate_text: |name, file| (name == "crossterm" && file == "LICENSE").then_some(("0.28.1", "MIT text")),
        };
        assert_eq!(table.version("crossterm"), "0.28.1");
        assert_eq!(table.version("nothing"), "");
        assert_eq!(table.text("crossterm", "LICENSE"), "MIT text");
    }

    #[test]
    fn search_finds_by_name_license_or_crate() {
        let found = |q: &str| -> Vec<&str> {
            entries().iter().filter(|e| e.matches(&q.to_lowercase())).map(|e| e.name).collect()
        };
        assert_eq!(found("").len(), entries().len());
        assert_eq!(found("geist"), ["Geist", "Geist Mono"]);
        assert!(found("ofl").contains(&"Source Code Pro"));
        assert_eq!(found("mpl"), ["nucleo-matcher", "option-ext"]);
        assert_eq!(found("serde"), ["Rust crates"], "a long-tail crate finds its entry");
        assert_eq!(found("zlib"), ["Rust crates"], "and its license");
        assert!(found("zzzz-nothing").is_empty());
    }

    #[test]
    fn the_browser_opens_on_the_first_entry_and_arrows_move_through_the_rows() {
        let mut b = Browser::new(entries());
        assert_eq!(b.selected(), Some(0));
        assert_eq!(b.selected_entry().map(|e| e.name), Some("Alacritty Terminal"));
        b.step(-1);
        assert_eq!(b.selected(), Some(0), "stops at the top");
        b.step(1);
        b.step(1);
        assert_eq!(b.selected(), Some(2));
        b.step(100);
        assert_eq!(b.selected(), Some(entries().len() - 1), "stops at the bottom");
        b.select(entries().len());
        assert_eq!(b.selected(), Some(entries().len() - 1), "out of range: unchanged");
    }

    #[test]
    fn a_search_keeps_a_selection_it_shows_and_moves_one_it_hides() {
        let mut b = Browser::new(entries());
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
        assert!(b.selected_entry().is_none());
        assert!(b.visible().is_empty());
        b.step(1);
        assert_eq!(b.selected(), None, "nothing to move to");
        b.set_query("");
        assert_eq!(b.selected(), Some(ix("option-ext")), "cleared: the selection comes back");
    }

    #[test]
    fn license_text_reflows_but_keeps_items_rules_copyrights_and_lists() {
        let blocks = license_blocks(
            "PREAMBLE\nThe goals of the\nlicense are\n\n(a) one\n    more\n(b) two\n\n-----\nSIL OPEN\n-----\n\n\
             Copyright (c) A\nCopyright (c) B\n\nfoo 1.0 · MIT\nbar 2.0 · MIT OR Apache-2.0",
        );
        let p = |lines: &[&str]| Block::Paragraph(lines.iter().map(|l| l.to_string()).collect());
        assert_eq!(
            blocks,
            [
                Block::Heading("PREAMBLE".into()),
                p(&["The goals of the license are"]),
                p(&["(a) one more", "(b) two"]),
                p(&["-----", "SIL OPEN", "-----"]),
                p(&["Copyright (c) A", "Copyright (c) B"]),
                p(&["foo 1.0 · MIT", "bar 2.0 · MIT OR Apache-2.0"]),
            ]
        );
        // An all-caps paragraph (a disclaimer) is body text, not headings.
        assert_eq!(
            license_blocks("THE SOFTWARE IS PROVIDED\nAS IS, WITHOUT WARRANTY"),
            [p(&["THE SOFTWARE IS PROVIDED AS IS, WITHOUT WARRANTY"])]
        );
        // A sentence wrapped at "copyright" joins up; a notice line doesn't.
        assert_eq!(
            license_blocks("as indicated by a\ncopyright notice that is\nCopyright 2016 A\nCopyright: (c) B"),
            [p(&["as indicated by a copyright notice that is", "Copyright 2016 A", "Copyright: (c) B"])]
        );
        // A heading alone, and Windows line ends.
        assert_eq!(
            license_blocks("TERMS AND CONDITIONS\r\n\r\n1. Definitions.\r\n   More."),
            [Block::Heading("TERMS AND CONDITIONS".into()), p(&["1. Definitions. More."])]
        );
    }
}
