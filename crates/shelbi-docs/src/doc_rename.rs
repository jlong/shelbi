// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreUI/src/doc_rename.rs), MIT in Shelbi.

//! Keeping a document's file name in step with its heading.
//!
//! A document's title is its H1, and by default the file on disk is named
//! after it — retitle the document and the file follows. Three space settings
//! shape this: whether it happens at all, which naming style the file gets,
//! and a list of file names that are never renamed whatever their heading says
//! (`README.md` and friends, whose names other tools depend on).
//!
//! Port of the rename half of Swift's `EditorViewController.saveContent`.
//! The decision is pure so it can be tested without an editor: given the
//! markdown, the current path and the space's policy, it answers where the
//! document should live.
//!
//! One deliberate difference. Swift falls back to `untitled-<timestamp>` when
//! a document has no H1, which renames such a document on *every* save, each
//! time to a new timestamp. Here a document with no heading keeps the name it
//! has — there is nothing to sync it to.

use std::path::{Path, PathBuf};

use crate::fs::paths::{sanitize_file_name_with_style, unique_document_filename, NameStyle};
use crate::fs::settings::{FileNameStyle, SpaceSettings};
use crate::fs::FsError;

/// The built-in "never rename these files" list, used until a space saves its
/// own. Mirrors Swift's `SpaceSettings.defaultRenameIgnoredFiles`.
pub const DEFAULT_IGNORED: &[&str] = &["README.md", "AGENTS.md", "CLAUDE.md", "index.md"];

/// A space's heading→file-name policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Whether renaming happens at all.
    pub enabled: bool,
    /// The naming style a derived file name gets.
    pub style: NameStyle,
    /// File names never renamed, matched case-insensitively.
    pub ignored: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            style: NameStyle::KebabCase,
            ignored: DEFAULT_IGNORED.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl Policy {
    /// Read the policy from a space's settings, falling back to the defaults
    /// for anything the space hasn't set.
    ///
    /// A temporary space is never renamed in: it isn't the user's library, and
    /// its files are usually someone else's checkout.
    pub fn from_settings(settings: Option<&SpaceSettings>, temporary: bool) -> Self {
        let default = Self::default();
        if temporary {
            return Self {
                enabled: false,
                ..default
            };
        }
        let Some(settings) = settings else {
            return default;
        };
        Self {
            enabled: settings.sync_file_name_with_heading.unwrap_or(true),
            style: settings.file_name_style.map(style_of).unwrap_or(default.style),
            ignored: settings
                .rename_ignored_files
                .clone()
                .unwrap_or(default.ignored),
        }
    }

    /// Whether `path`'s file name is on the never-rename list.
    fn is_ignored(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        self.ignored
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
    }
}

/// Map the settings enum onto the path sanitizer's.
fn style_of(style: FileNameStyle) -> NameStyle {
    match style {
        FileNameStyle::KebabCase => NameStyle::KebabCase,
        FileNameStyle::CamelCase => NameStyle::CamelCase,
        FileNameStyle::Original => NameStyle::Original,
    }
}

/// Where `markdown` should be saved, given where it currently lives.
///
/// `None` means "leave it where it is" — the common case, and the answer
/// whenever the policy is off, the file is protected, there is no heading, or
/// the derived name is the one it already has.
///
/// The document never leaves its own folder: the name comes from the heading,
/// the directory from the current path.
pub fn target_path(markdown: &str, current: &Path, policy: &Policy) -> Option<PathBuf> {
    if !policy.enabled || policy.is_ignored(current) {
        return None;
    }
    let title = heading(markdown)?;
    let base = sanitize_file_name_with_style(&title, policy.style);
    if base.is_empty() {
        return None;
    }
    let folder = current.parent()?;
    let stem = unique_document_filename(&base, folder, Some(current));
    let target = folder.join(format!("{stem}.md"));
    (target != current).then_some(target)
}

/// Rename the document at `current` after the H1 in `markdown`, if the policy
/// calls for it, returning where it lives now.
///
/// Shelbi addition: [`target_path`] decides, this carries it out. The rename
/// stays in the document's folder and never overwrites another file
/// ([`target_path`] picks a free name).
pub fn rename_to_heading(
    markdown: &str,
    current: &Path,
    policy: &Policy,
) -> Result<PathBuf, FsError> {
    let Some(target) = target_path(markdown, current, policy) else {
        return Ok(current.to_path_buf());
    };
    std::fs::rename(current, &target).map_err(|source| FsError::Io {
        path: target.to_string_lossy().into_owned(),
        source,
    })?;
    Ok(target)
}

/// The document's H1, skipping a leading frontmatter block.
///
/// Only a leading `---` opens frontmatter; a `---` later in the document is a
/// horizontal rule, and treating it as a delimiter would swallow the body.
fn heading(markdown: &str) -> Option<String> {
    let mut lines = markdown.lines().peekable();
    if lines.peek().map(|l| l.trim()) == Some("---") {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
        }
    }
    for line in lines {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that cleans up after itself — `target_path` probes
    /// the filesystem for name collisions.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cs-rename-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn write(&self, name: &str, body: &str) -> PathBuf {
            let path = self.join(name);
            std::fs::write(&path, body).unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_heading_becomes_the_file_name() {
        let dir = TempDir::new("basic");
        let current = dir.write("old-name.md", "");
        let target = target_path("# My API Design\n\nBody.\n", &current, &Policy::default());
        assert_eq!(target, Some(dir.join("my-api-design.md")));
    }

    #[test]
    fn a_matching_name_is_left_alone() {
        let dir = TempDir::new("match");
        let current = dir.write("my-notes.md", "");
        assert_eq!(
            target_path("# My Notes\n", &current, &Policy::default()),
            None
        );
    }

    #[test]
    fn the_setting_turns_renaming_off() {
        let dir = TempDir::new("off");
        let current = dir.write("old-name.md", "");
        let policy = Policy {
            enabled: false,
            ..Policy::default()
        };
        assert_eq!(target_path("# A New Title\n", &current, &policy), None);
    }

    #[test]
    fn a_temporary_space_never_renames() {
        // It is someone else's checkout, opened to look at.
        let policy = Policy::from_settings(None, true);
        assert!(!policy.enabled);
    }

    #[test]
    fn protected_names_are_never_renamed() {
        let dir = TempDir::new("ignored");
        for name in DEFAULT_IGNORED {
            let current = dir.write(name, "");
            assert_eq!(
                target_path("# Something Else\n", &current, &Policy::default()),
                None,
                "{name}"
            );
        }
    }

    #[test]
    fn the_protected_list_is_matched_case_insensitively() {
        let dir = TempDir::new("case");
        let current = dir.write("readme.md", "");
        assert_eq!(
            target_path("# Something Else\n", &current, &Policy::default()),
            None
        );
    }

    #[test]
    fn each_style_shapes_the_name() {
        let dir = TempDir::new("styles");
        let current = dir.write("old-name.md", "");
        let markdown = "# My API Design\n";
        for (style, expected) in [
            (NameStyle::KebabCase, "my-api-design.md"),
            (NameStyle::CamelCase, "myApiDesign.md"),
            (NameStyle::Original, "My API Design.md"),
        ] {
            let policy = Policy {
                style,
                ..Policy::default()
            };
            assert_eq!(
                target_path(markdown, &current, &policy),
                Some(dir.join(expected)),
                "{style:?}"
            );
        }
    }

    #[test]
    fn a_taken_name_gets_a_suffix() {
        let dir = TempDir::new("collide");
        let current = dir.write("old-name.md", "");
        dir.write("my-notes.md", "someone else");
        assert_eq!(
            target_path("# My Notes\n", &current, &Policy::default()),
            Some(dir.join("my-notes-1.md"))
        );
    }

    #[test]
    fn a_document_without_a_heading_keeps_its_name() {
        // Swift renamed these to `untitled-<timestamp>` on every save, which
        // renamed the file again each time the timer fired.
        let dir = TempDir::new("noheading");
        let current = dir.write("old-name.md", "");
        assert_eq!(target_path("Just a paragraph.\n", &current, &Policy::default()), None);
        assert_eq!(target_path("", &current, &Policy::default()), None);
        assert_eq!(
            target_path("## Not a title\n", &current, &Policy::default()),
            None
        );
    }

    #[test]
    fn frontmatter_is_skipped_when_looking_for_the_heading() {
        let dir = TempDir::new("frontmatter");
        let current = dir.write("old-name.md", "");
        let markdown = "---\ntitle: Decoy\n---\n\n# Real Title\n";
        assert_eq!(
            target_path(markdown, &current, &Policy::default()),
            Some(dir.join("real-title.md"))
        );
    }

    #[test]
    fn a_horizontal_rule_is_not_frontmatter() {
        // Only a leading `---` opens frontmatter; mistaking a rule for one
        // would skip past the heading that follows it.
        let dir = TempDir::new("rule");
        let current = dir.write("old-name.md", "");
        let markdown = "# Real Title\n\n---\n\nBody.\n";
        assert_eq!(
            target_path(markdown, &current, &Policy::default()),
            Some(dir.join("real-title.md"))
        );
    }

    #[test]
    fn the_document_stays_in_its_own_folder() {
        let dir = TempDir::new("folder");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let current = sub.join("old-name.md");
        std::fs::write(&current, "").unwrap();
        assert_eq!(
            target_path("# Moved\n", &current, &Policy::default()),
            Some(sub.join("moved.md"))
        );
    }

    #[test]
    fn settings_override_each_default_independently() {
        let settings = SpaceSettings {
            sync_file_name_with_heading: Some(false),
            ..SpaceSettings::default()
        };
        let policy = Policy::from_settings(Some(&settings), false);
        assert!(!policy.enabled);
        // The untouched fields keep their defaults.
        assert_eq!(policy.style, NameStyle::KebabCase);
        assert_eq!(policy.ignored.len(), DEFAULT_IGNORED.len());
    }

    #[test]
    fn an_empty_ignore_list_protects_nothing() {
        // Clearing the list in Settings must actually clear it, not fall back
        // to the built-in defaults.
        let settings = SpaceSettings {
            rename_ignored_files: Some(Vec::new()),
            ..SpaceSettings::default()
        };
        let policy = Policy::from_settings(Some(&settings), false);
        assert!(policy.ignored.is_empty());

        let dir = TempDir::new("emptyignore");
        let current = dir.write("README.md", "");
        assert_eq!(
            target_path("# Something Else\n", &current, &policy),
            Some(dir.join("something-else.md"))
        );
    }

    // Shelbi additions.

    #[test]
    fn rename_to_heading_moves_the_file() {
        let dir = TempDir::new("apply");
        let current = dir.write("old-name.md", "# New Name\n");
        let moved = rename_to_heading("# New Name\n", &current, &Policy::default()).unwrap();
        assert_eq!(moved, dir.join("new-name.md"));
        assert!(!current.exists());
        assert_eq!(std::fs::read_to_string(&moved).unwrap(), "# New Name\n");
    }

    #[test]
    fn rename_to_heading_leaves_a_protected_file_in_place() {
        let dir = TempDir::new("apply-protected");
        let current = dir.write("README.md", "# Project\n");
        let kept = rename_to_heading("# Project\n", &current, &Policy::default()).unwrap();
        assert_eq!(kept, current);
        assert!(current.exists());
    }

    #[test]
    fn rename_to_heading_follows_sync_file_name_with_heading() {
        let dir = TempDir::new("apply-setting");
        let current = dir.write("old-name.md", "# New Name\n");
        let settings = SpaceSettings {
            sync_file_name_with_heading: Some(false),
            ..SpaceSettings::default()
        };
        let policy = Policy::from_settings(Some(&settings), false);
        let kept = rename_to_heading("# New Name\n", &current, &policy).unwrap();
        assert_eq!(kept, current);
    }
}
