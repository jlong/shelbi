// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-markdown/src/lib.rs), MIT in Shelbi.

//! Markdown parsing and section addressing for ContextStore.
//!
//! Three sub-modules cover the surface every caller in `cstore-fs`,
//! `cstore-mcp`, and `cstore-merge` needs:
//!
//!   * [`sections`] — heading addressing, frontmatter handling, and
//!     section read/write/append/prepend/remove.
//!   * [`tasks`] — GFM checkbox listing and toggling.
//!   * [`compose`] — whole-document writes with `cstore`'s rules
//!     (keep frontmatter and H1), new-document bodies, `ls --stats`
//!     counts.
//!   * [`links`] — both `[text](href)` and wikilink (`[[name]]`)
//!     extraction.
//!
//! Heavy lifting goes through [`comrak`] (CommonMark + GFM) with a
//! single shared [`comrak_options`] config so the parser configuration
//! never drifts between this crate and `cstore-merge`.

#![deny(missing_docs)]

use comrak::Options;

pub mod compose;
pub mod links;
pub mod sections;
pub mod tasks;

/// Errors raised by markdown operations.
///
/// Mapped to [`crate::domain::CoreError`] at the FFI boundary by
/// `cstore-core`.
#[derive(Debug, thiserror::Error)]
pub enum MarkdownError {
    /// The caller asked for a heading or section that isn't present.
    #[error("section not found: {0}")]
    SectionNotFound(String),

    /// Multiple sections matched the heading; the caller must
    /// disambiguate (e.g., include a deeper path).
    #[error("ambiguous heading {heading:?}: matched {count} sections")]
    AmbiguousSection {
        /// The heading string the caller passed.
        heading: String,
        /// Number of sections that matched.
        count: usize,
    },

    /// The caller asked for a task by substring but multiple tasks
    /// matched it.
    #[error("ambiguous task {needle:?}: matched {count} tasks")]
    AmbiguousTask {
        /// The substring the caller passed.
        needle: String,
        /// Number of tasks that matched.
        count: usize,
    },

    /// No task matched the given substring.
    #[error("task not found: {0}")]
    TaskNotFound(String),
}

impl From<MarkdownError> for crate::domain::CoreError {
    fn from(err: MarkdownError) -> Self {
        match err {
            MarkdownError::SectionNotFound(name) => {
                crate::domain::CoreError::NotFound(format!("section: {name}"))
            }
            MarkdownError::AmbiguousSection { heading, count } => {
                crate::domain::CoreError::InvalidInput(format!(
                    "heading {heading:?} matches {count} sections"
                ))
            }
            MarkdownError::AmbiguousTask { needle, count } => {
                crate::domain::CoreError::InvalidInput(format!(
                    "task substring {needle:?} matches {count} tasks"
                ))
            }
            MarkdownError::TaskNotFound(needle) => {
                crate::domain::CoreError::NotFound(format!("task: {needle}"))
            }
        }
    }
}

/// `Result` alias defaulting to [`MarkdownError`].
pub type Result<T> = std::result::Result<T, MarkdownError>;

/// Shared comrak options used by every parse call in this crate.
///
/// Future callers in `cstore-merge` will re-use this so block-level
/// structural diffs see the same AST shapes we do. Enables:
///
///   * `tasklist` — GFM-style `- [ ] foo` / `- [x] foo` tasks.
///   * `wikilinks_title_after_pipe` — `[[name|label]]` wiki-style
///     links.
///   * YAML frontmatter detection between `---` fences.
pub fn comrak_options() -> Options<'static> {
    let mut options = Options::default();
    options.extension.tasklist = true;
    options.extension.wikilinks_title_after_pipe = true;
    options.extension.front_matter_delimiter = Some("---".to_string());
    options
}
