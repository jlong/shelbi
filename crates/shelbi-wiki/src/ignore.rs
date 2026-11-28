// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-ignore/src/lib.rs), MIT in Shelbi.

//! `.csignore` pattern matching for ContextStore spaces.
//!
//! Loads a single `.csignore` file at a space root and exposes
//! [`CsIgnore::is_ignored`], the predicate `cstore-fs` uses to filter
//! directory listings, search results, and MCP responses. Pattern
//! syntax follows `.gitignore` via the [`ignore`] crate, a strict
//! superset of what the Swift `CSIgnore` implementation supported.
//!
//! The crate also publishes the always-ignored set (entries no
//! `.csignore` rule can re-include) and the default content written
//! when a new space is created.
//!
//! # Always-ignored
//!
//! [`always_ignored`] holds the named entries — `.contextstore`,
//! `.git`, `.DS_Store` — that match the Swift `defaultIgnored` set.
//! [`CsIgnore::is_ignored`] additionally rejects every path component
//! whose name starts with `.`, matching `CSIgnore.swift`'s
//! `hasPrefix(".")` rule. Both checks happen before the `.csignore`
//! matcher runs, so a `!` rule cannot un-ignore them.

#![deny(missing_docs)]

use std::path::Path;

use crate::domain::{CoreError, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

const ALWAYS_IGNORED: &[&str] = &[".contextstore", ".git", ".DS_Store"];

const DEFAULT_CONTENT: &str = "\
# ContextStore ignore file
# Patterns follow .gitignore syntax

# AI agent files
AGENTS.md
CLAUDE.md

# Dependencies
node_modules/

# Build output
build/
dist/
.build/";

/// A compiled `.csignore` matcher rooted at a space directory.
pub struct CsIgnore {
    inner: Gitignore,
}

impl CsIgnore {
    /// Load the `.csignore` file from a space root.
    ///
    /// Returns an empty matcher — still applying [`always_ignored`]
    /// and the dot-file rule — when no `.csignore` exists at the
    /// root. I/O errors other than "not found" propagate as
    /// [`CoreError::Io`].
    pub fn load(space_root: &Path) -> Result<Self> {
        let path = space_root.join(".csignore");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(CoreError::Io(format!(
                    "read .csignore at {}: {e}",
                    path.display()
                )));
            }
        };
        Self::build(space_root, &content)
    }

    /// Build a matcher from explicit pattern content.
    ///
    /// Useful for tests and for callers that already have the
    /// `.csignore` body in memory. The matcher root is empty, which
    /// is fine for matching by relative path.
    pub fn from_content(content: &str) -> Result<Self> {
        Self::build(Path::new(""), content)
    }

    fn build(root: &Path, content: &str) -> Result<Self> {
        let mut builder = GitignoreBuilder::new(root);
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            builder
                .add_line(None, trimmed)
                .map_err(|e| CoreError::InvalidInput(format!("csignore: {e}")))?;
        }
        let inner = builder
            .build()
            .map_err(|e| CoreError::InvalidInput(format!("csignore: {e}")))?;
        Ok(Self { inner })
    }

    /// Return `true` when `relative_path` should be hidden from
    /// listings, search, and MCP responses.
    ///
    /// Decision order:
    ///
    /// 1. Any component matching an entry in [`always_ignored`] →
    ///    ignored.
    /// 2. Any component whose name starts with `.` → ignored.
    /// 3. Otherwise, the `.csignore` matcher decides, walking the
    ///    path and every parent so a top-level `node_modules/` rule
    ///    also catches `Foo/node_modules/file.txt`. A `!pattern` line
    ///    can un-ignore a path that earlier `.csignore` lines matched
    ///    but cannot un-ignore rule 1 or rule 2.
    pub fn is_ignored(&self, relative_path: &Path, is_dir: bool) -> bool {
        for component in relative_path.components() {
            let Some(name) = component.as_os_str().to_str() else {
                continue;
            };
            if ALWAYS_IGNORED.contains(&name) {
                return true;
            }
            if name.starts_with('.') {
                return true;
            }
        }
        self.inner
            .matched_path_or_any_parents(relative_path, is_dir)
            .is_ignore()
    }
}

/// Default `.csignore` content created for new spaces.
///
/// Byte-for-byte equivalent to `CSIgnore.swift`'s `defaultContent`.
pub fn default_content() -> &'static str {
    DEFAULT_CONTENT
}

/// Names always ignored regardless of `.csignore` content.
///
/// Anything whose name starts with `.` is also always-ignored even
/// though it is not listed here; this slice is the explicitly-named
/// set carried over from Swift's `defaultIgnored`.
pub fn always_ignored() -> &'static [&'static str] {
    ALWAYS_IGNORED
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn rel(p: &str) -> &Path {
        Path::new(p)
    }

    #[test]
    fn default_content_matches_swift_verbatim() {
        // Byte-for-byte transcription of CSIgnore.swift's
        // `defaultContent` triple-quoted string.
        let expected = "# ContextStore ignore file\n\
            # Patterns follow .gitignore syntax\n\
            \n\
            # AI agent files\n\
            AGENTS.md\n\
            CLAUDE.md\n\
            \n\
            # Dependencies\n\
            node_modules/\n\
            \n\
            # Build output\n\
            build/\n\
            dist/\n\
            .build/";
        assert_eq!(default_content(), expected);
    }

    #[test]
    fn always_ignored_set_matches_swift() {
        assert_eq!(always_ignored(), &[".contextstore", ".git", ".DS_Store"]);
    }

    // ── always-ignored ──────────────────────────────────────────────

    #[test]
    fn contextstore_always_ignored_even_when_negated() {
        let csi = CsIgnore::from_content("!.contextstore").unwrap();
        assert!(csi.is_ignored(rel(".contextstore"), true));
        assert!(csi.is_ignored(rel(".contextstore/settings.yml"), false));
    }

    #[test]
    fn ds_store_always_ignored() {
        let csi = CsIgnore::from_content("").unwrap();
        assert!(csi.is_ignored(rel(".DS_Store"), false));
        assert!(csi.is_ignored(rel("Engineering/.DS_Store"), false));
    }

    #[test]
    fn dotfiles_always_ignored() {
        let csi = CsIgnore::from_content("").unwrap();
        assert!(csi.is_ignored(rel(".env"), false));
        assert!(csi.is_ignored(rel(".csignore"), false));
        assert!(csi.is_ignored(rel(".hidden/doc.md"), false));
    }

    #[test]
    fn dot_prefixed_negation_cannot_unhide() {
        let csi = CsIgnore::from_content("!.env").unwrap();
        assert!(csi.is_ignored(rel(".env"), false));
    }

    // ── pattern semantics ───────────────────────────────────────────

    #[test]
    fn directory_only_pattern_does_not_match_file() {
        let csi = CsIgnore::from_content("node_modules/").unwrap();
        assert!(csi.is_ignored(rel("node_modules"), true));
        assert!(csi.is_ignored(rel("Foo/node_modules"), true));
        assert!(!csi.is_ignored(rel("node_modules"), false));
    }

    #[test]
    fn directory_only_pattern_matches_nested_descendants() {
        let csi = CsIgnore::from_content("node_modules/").unwrap();
        assert!(csi.is_ignored(rel("node_modules/lodash/index.js"), false));
        assert!(csi.is_ignored(rel("Foo/node_modules/lodash/index.js"), false));
    }

    #[test]
    fn star_log_matches_log_files() {
        let csi = CsIgnore::from_content("*.log").unwrap();
        assert!(csi.is_ignored(rel("app.log"), false));
        assert!(csi.is_ignored(rel("errors.log"), false));
        assert!(!csi.is_ignored(rel("log.txt"), false));
    }

    #[test]
    fn negation_unignores_specific_log() {
        let csi = CsIgnore::from_content("*.log\n!important.log").unwrap();
        assert!(csi.is_ignored(rel("app.log"), false));
        assert!(!csi.is_ignored(rel("important.log"), false));
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let csi = CsIgnore::from_content(
            "# leading comment\n\
             \n\
             AGENTS.md\n\
             # trailing comment\n",
        )
        .unwrap();
        assert!(csi.is_ignored(rel("AGENTS.md"), false));
        assert!(!csi.is_ignored(rel("README.md"), false));
    }

    #[test]
    fn trailing_whitespace_trimmed() {
        // "AGENTS.md   " with trailing spaces should still match
        // "AGENTS.md" because the parser trims each line.
        let csi = CsIgnore::from_content("AGENTS.md   \n").unwrap();
        assert!(csi.is_ignored(rel("AGENTS.md"), false));
    }

    // ── load ────────────────────────────────────────────────────────

    #[test]
    fn load_missing_csignore_returns_empty_matcher() {
        let dir = tempfile::tempdir().unwrap();
        let csi = CsIgnore::load(dir.path()).unwrap();
        assert!(!csi.is_ignored(rel("README.md"), false));
        // Always-ignored rules still apply.
        assert!(csi.is_ignored(rel(".DS_Store"), false));
    }

    #[test]
    fn load_reads_csignore_from_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".csignore"), "AGENTS.md\n").unwrap();
        let csi = CsIgnore::load(dir.path()).unwrap();
        assert!(csi.is_ignored(rel("AGENTS.md"), false));
    }
}
