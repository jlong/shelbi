//! What the `shelbi` binary ships from others, for the TUI's
//! Acknowledgements (`tui-acknowledgements`): the notable crates, each with
//! its full license text, plus one entry for the long tail of Rust crates.
//!
//! The TUI is a different binary from the desktop app, so this is its own
//! list over the shared types (`shelbi_app::licenses`): the crate entries
//! both binaries ship ([`shared_entries`]), then the TUI's own featured
//! crates, Ratatui and Crossterm. No fonts or icon sets: a terminal draws
//! in the user's own font. The long tail is every other crate the binary
//! links, walked from the `shelbi` package by the build script (`build.rs`,
//! running `crates/shelbi-app/build_support/licenses.rs`); the crates with
//! a notice obligation beyond MIT / Apache's (nucleo-matcher's and
//! option-ext's MPL 2.0) are entries of their own. Every text is embedded
//! at build time; nothing is fetched at runtime.

use std::sync::LazyLock;

use shelbi_app::licenses::{listed, long_tail, shared_entries, Browser, Crate, Entry, Table};

/// The build script's output: `CRATES` (the long tail), `CRATE_NOTICES`
/// (its text), `SHIPPED` (every shipped crate) and `crate_text` (a featured
/// crate's license file).
mod generated {
    use super::Crate;
    include!(concat!(env!("OUT_DIR"), "/licenses.rs"));
}

/// The binary's license table.
const TABLE: Table = Table {
    crates: generated::CRATES,
    notices: generated::CRATE_NOTICES,
    shipped: generated::SHIPPED,
    crate_text: generated::crate_text,
};

/// Every entry, in the order the list shows them.
pub fn entries() -> &'static [Entry] {
    static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(build);
    &ENTRIES
}

/// A browser over [`entries`], as the view opens on.
pub fn browser() -> Browser {
    Browser::new(entries())
}

fn build() -> Vec<Entry> {
    let mut list = shared_entries(&TABLE);
    list.extend([
        Entry {
            name: "Crossterm",
            license: "MIT",
            attribution: format!("Timon Post · Terminal library {}", TABLE.version("crossterm")),
            description: "Reads the keyboard and mouse, and drives the terminal the TUI draws in.",
            source: "https://github.com/crossterm-rs/crossterm",
            license_title: "MIT License",
            text: TABLE.text("crossterm", "LICENSE"),
            crates: &[],
        },
        Entry {
            name: "Ratatui",
            license: "MIT",
            attribution: format!("The Ratatui Developers · Terminal UI library {}", TABLE.version("ratatui")),
            description: "Draws every screen of the TUI.",
            source: "https://github.com/ratatui/ratatui",
            license_title: "MIT License",
            text: TABLE.text("ratatui", "LICENSE"),
            crates: &[],
        },
    ]);
    let tail = long_tail(
        &TABLE,
        "The other Rust crates compiled into the shelbi binary, each with its version, license and notice.",
        "https://github.com/jlong/shelbi/blob/main/Cargo.lock",
    );
    listed(list, tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use generated::{CRATES, SHIPPED};

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

    #[test]
    fn the_list_is_the_shared_crates_then_the_tuis_own_by_name() {
        let names: Vec<&str> = entries().iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            [
                "Alacritty Terminal",
                "Crossterm",
                "nucleo-matcher",
                "option-ext",
                "portable-pty",
                "Ratatui",
                "termwiz",
                "Rust crates",
            ]
        );
    }

    /// The texts are the real ones: each names its license and its
    /// copyright holder as the shipped file does.
    #[test]
    fn the_texts_are_the_shipped_licenses() {
        let text = |name: &str| entries().iter().find(|e| e.name == name).unwrap().text;
        assert!(text("Alacritty Terminal").contains("Apache License"));
        assert!(text("Ratatui").contains("The Ratatui Developers"));
        assert!(text("Ratatui").contains("Permission is hereby granted"));
        assert!(text("Crossterm").contains("Timon"));
        assert!(text("nucleo-matcher").starts_with("Mozilla Public License Version 2.0"));
        assert!(text("option-ext").starts_with("Mozilla Public License Version 2.0"));
        assert!(text("portable-pty").contains("Wez Furlong"));
        assert!(text("termwiz").contains("Wez Furlong"));
        let attribution = |name: &str| entries().iter().find(|e| e.name == name).unwrap().attribution.clone();
        assert!(attribution("Ratatui").ends_with(TABLE.version("ratatui")));
        assert!(!TABLE.version("ratatui").is_empty());
    }

    /// The list is what the `shelbi` binary links: the TUI's crates and the
    /// CLI's (inquire, the wizard's prompts), not the desktop's (gpui), and
    /// the long tail is every shipped crate but the featured ones and
    /// Shelbi's own.
    #[test]
    fn the_list_is_what_the_shelbi_binary_ships() {
        let mut featured = shelbi_app::licenses::SHARED_FEATURED.to_vec();
        featured.extend(["ratatui", "crossterm"]);
        for name in &featured {
            assert!(SHIPPED.iter().any(|c| c.name == *name), "{name} doesn't ship");
        }
        for c in SHIPPED {
            let own = c.name.starts_with("shelbi");
            assert_eq!(CRATES.contains(c), !own && !featured.contains(&c.name), "{} {}", c.name, c.version);
        }
        assert!(SHIPPED.iter().any(|c| c.name == "inquire"), "the CLI's crates ship too");
        assert!(SHIPPED.iter().any(|c| c.name == "shelbi-tui"));
        assert!(!SHIPPED.iter().any(|c| c.name.starts_with("gpui")), "the desktop's don't");
        // A proc-macro runs in the compiler; it isn't shipped code.
        assert!(!SHIPPED.iter().any(|c| c.name == "serde_derive"));
        assert!(CRATES.len() > 100);
    }

    /// The long tail's text lists every crate and carries a notice for
    /// each.
    #[test]
    fn the_long_tail_lists_and_covers_every_crate() {
        let tail = entries().last().unwrap();
        assert_eq!(tail.attribution, format!("{} open source libraries", CRATES.len()));
        let (list, rest) = tail.text.split_once("\nNOTICES").expect("the notices follow the list");
        assert!(list.contains("compiled into the shelbi binary"));
        for c in CRATES {
            assert!(list.contains(&format!("{} {} · {}", c.name, c.version, c.license)), "{} {}", c.name, c.version);
            let id = format!("{} {}", c.name, c.version);
            assert!(
                rest.lines()
                    .filter(|l| l.starts_with("Applies to "))
                    .any(|l| l.trim_start_matches("Applies to ").trim_end_matches('.').split(", ").any(|n| n == id)),
                "{id} has no notice"
            );
        }
    }

    #[test]
    fn search_finds_the_tuis_crates() {
        let mut b = browser();
        assert_eq!(b.selected_entry().map(|e| e.name), Some("Alacritty Terminal"));
        b.set_query("mpl");
        let names: Vec<&str> = b.visible().into_iter().map(|i| entries()[i].name).collect();
        assert_eq!(names, ["nucleo-matcher", "option-ext"]);
        b.set_query("inquire");
        assert_eq!(b.selected_entry().map(|e| e.name), Some("Rust crates"));
    }
}
