//! About Shelbi: what both clients show about this build and the project
//! (the desktop's About window, Figma "About Shelbi · Dark", 2431:8310; the
//! TUI's About overlay). The words and links live here so the two read
//! the same; each client brings its own version (its crate's) and layout.
//!
//! Top to bottom: the app icon, [`NAME`] and [`version_line`], the
//! [`TAGLINE`], the [`LINKS_ROW`] (Website, Documentation, GitHub),
//! [`AboutLink::Acknowledgements`], then the [`copyright`] line over the
//! [`CREDITS_ROW`] (the license and the sponsor).

/// The app's name, the About screen's heading.
pub const NAME: &str = "Shelbi";
/// The line under the version.
pub const TAGLINE: &str = "An Open Source personal agent orchestrator.";

pub const WEBSITE_URL: &str = "https://shelbi.dev";
/// The docs (About's Documentation, and the desktop's Help menu).
pub const DOCS_URL: &str = "https://shelbi.dev/docs";
pub const GITHUB_URL: &str = "https://github.com/jlong/shelbi";
pub const LICENSE_URL: &str = "https://github.com/jlong/shelbi/blob/main/LICENSE";
pub const SPONSOR_URL: &str = "https://32pixels.co";

/// The license, as its link reads.
pub const LICENSE_NAME: &str = "MIT License";
/// Who the copyright line names.
pub const COPYRIGHT_HOLDER: &str = "John W. Long";
/// The copyright line's year: the year this build was made (`build.rs`
/// reads `SOURCE_DATE_EPOCH`, else the clock).
pub const COPYRIGHT_YEAR: &str = env!("SHELBI_BUILD_YEAR");

/// The copyright line: `© <year> <holder>`.
pub fn copyright() -> String {
    format!("© {COPYRIGHT_YEAR} {COPYRIGHT_HOLDER}")
}

/// The line under the name: `Version <version>`, `version` being the
/// client's own build.
pub fn version_line(version: &str) -> String {
    format!("Version {version}")
}

/// A link on the About screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AboutLink {
    Website,
    Documentation,
    GitHub,
    /// Opens the client's list of the open source software Shelbi bundles,
    /// not a page.
    Acknowledgements,
    License,
    Sponsor,
}

/// The first row of links, left to right.
pub const LINKS_ROW: [AboutLink; 3] = [AboutLink::Website, AboutLink::Documentation, AboutLink::GitHub];
/// The credits' row under the copyright, left to right, `·` between them.
pub const CREDITS_ROW: [AboutLink; 2] = [AboutLink::License, AboutLink::Sponsor];

/// Every link in reading order (the keyboard's order too).
pub const LINKS: [AboutLink; 6] = [
    AboutLink::Website,
    AboutLink::Documentation,
    AboutLink::GitHub,
    AboutLink::Acknowledgements,
    AboutLink::License,
    AboutLink::Sponsor,
];

impl AboutLink {
    /// What the link reads.
    pub fn label(self) -> &'static str {
        match self {
            AboutLink::Website => "Website",
            AboutLink::Documentation => "Documentation",
            AboutLink::GitHub => "GitHub",
            AboutLink::Acknowledgements => "Acknowledgements",
            AboutLink::License => LICENSE_NAME,
            AboutLink::Sponsor => "Sponsored by 32pixels",
        }
    }

    /// The page it opens in the browser; `None` for Acknowledgements, which
    /// opens a screen of the client's own.
    pub fn url(self) -> Option<&'static str> {
        match self {
            AboutLink::Website => Some(WEBSITE_URL),
            AboutLink::Documentation => Some(DOCS_URL),
            AboutLink::GitHub => Some(GITHUB_URL),
            AboutLink::Acknowledgements => None,
            AboutLink::License => Some(LICENSE_URL),
            AboutLink::Sponsor => Some(SPONSOR_URL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copyright_year_is_a_year() {
        let year: u32 = COPYRIGHT_YEAR.parse().unwrap();
        assert!((2026..2200).contains(&year));
        assert_eq!(copyright(), format!("© {year} John W. Long"));
    }

    #[test]
    fn the_rows_are_the_links_in_reading_order() {
        let mut rows: Vec<AboutLink> = LINKS_ROW.to_vec();
        rows.push(AboutLink::Acknowledgements);
        rows.extend(CREDITS_ROW);
        assert_eq!(rows, LINKS);
    }

    #[test]
    fn every_link_but_acknowledgements_opens_an_https_page() {
        for link in LINKS {
            match link.url() {
                Some(url) => assert!(url.starts_with("https://"), "{link:?}"),
                None => assert_eq!(link, AboutLink::Acknowledgements),
            }
        }
        assert_eq!(version_line("0.11.0"), "Version 0.11.0");
    }
}
