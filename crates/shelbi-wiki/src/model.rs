// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreUI/src/model.rs and ContextStoreUI/src/ignore.rs), MIT in Shelbi.

//! Document titles and the order documents are listed in.
//!
//! The logic half of ContextStore's document list (`load_documents_in`,
//! `document_title`, `pin_priority` in its UI model, and `visible_documents`
//! from its `.csignore` filter), without the AppKit model around it, the HTML
//! documents only its WebKit view shows, or the locale-formatted row subtitle.
//!
//! - A document's **title** is its first `# ` heading after any frontmatter;
//!   the file name (without `.md`) is the fallback.
//! - Documents are **sorted** into pin buckets, then by title,
//!   case-insensitively: the document named after its folder first, then
//!   README, then index/overview, then everything else.
//!
//! Folders sort by the space's `folderOrder`, then by name; that lives with
//! the tree in [`crate::fs::tree`].

use std::path::{Path, PathBuf};

use crate::domain::Document;
use crate::fs::document;
use crate::fs::FsError;
use crate::ignore::CsIgnore;

/// A document as a list shows it: where it is, its title, and its pin bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEntry {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// The first `# ` heading, else the file name stem.
    pub title: String,
    /// Pin bucket for sorting (0 = pinned to top … 3 = normal). See
    /// [`pin_priority`].
    pub pin: u8,
}

impl DocEntry {
    /// The entry for the document at `path` with contents `content`, listed in
    /// a folder (or space) called `folder_name`.
    pub fn new(path: PathBuf, content: &str, folder_name: &str) -> Self {
        let stem = file_stem(&path);
        let title = document_title(content).unwrap_or_else(|| stem.clone());
        let pin = pin_priority(&title, &stem, folder_name);
        Self { path, title, pin }
    }
}

/// The `.md` documents directly in `folder`, minus those the space's
/// `.csignore` hides, titled and sorted.
///
/// `folder_name` is what the list pins against: the folder's own name, or
/// the space's name at its root (as ContextStore's Home list does). A
/// document that can't be read is listed under its file name.
pub fn list_documents(
    folder: &Path,
    space_root: &Path,
    folder_name: &str,
) -> Result<Vec<DocEntry>, FsError> {
    let docs = visible_documents(document::list_documents(folder)?, space_root);
    let mut entries: Vec<DocEntry> = docs
        .iter()
        .map(|d| {
            let path = PathBuf::from(&d.path);
            let content = document::read_document(&path).unwrap_or_default();
            DocEntry::new(path, &content, folder_name)
        })
        .collect();
    sort_documents(&mut entries);
    Ok(entries)
}

/// Pin bucket, then case-insensitive title.
pub fn sort_documents(entries: &mut [DocEntry]) {
    entries.sort_by(|a, b| {
        a.pin
            .cmp(&b.pin)
            .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
    });
}

/// Drop the documents the space's `.csignore` hides.
///
/// An unreadable or malformed `.csignore` must not empty the list; showing
/// an extra file beats showing none.
pub fn visible_documents(docs: Vec<Document>, space_root: &Path) -> Vec<Document> {
    let Ok(ignore) = CsIgnore::load(space_root) else {
        return docs;
    };
    docs.into_iter()
        .filter(|doc| {
            let path = Path::new(&doc.path);
            match path.strip_prefix(space_root) {
                Ok(relative) => !ignore.is_ignored(relative, false),
                Err(_) => true,
            }
        })
        .collect()
}

/// A document's display title: its first `# ` heading, skipping any YAML
/// frontmatter. `None` sends the caller to the file name.
pub fn document_title(content: &str) -> Option<String> {
    strip_frontmatter(content)
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("# ").map(|rest| rest.trim().to_string()))
}

/// The title of `path` given its `content`: [`document_title`], else the
/// file name without its extension.
pub fn title_or_file_name(content: &str, path: &Path) -> String {
    document_title(content).unwrap_or_else(|| file_stem(path))
}

/// Sort bucket matching `DocumentListView.pinPriority`: the doc named after the
/// project pins first, then README, then index/overview, then everything else.
pub fn pin_priority(display_name: &str, filename_stem: &str, project_name: &str) -> u8 {
    let dn = display_name.to_lowercase();
    let stem = filename_stem.to_lowercase();
    let project = project_name.to_lowercase();
    // Swift matched readme/index against the H1 title *and* the filename
    // independently; the port only looked at the filename, so `notes.md` with
    // an `# Index` heading never pinned.
    if !project.is_empty() && (dn == project || stem == project) {
        0
    } else if dn == "readme" || stem == "readme" {
        1
    } else if dn == "index" || stem == "index" || dn == "overview" || stem == "overview" {
        2
    } else {
        3
    }
}

/// Drop a leading `---`-delimited YAML frontmatter block, returning the body.
/// Anything that doesn't open with `---` on the first line is returned whole.
fn strip_frontmatter(content: &str) -> &str {
    let rest = match content.strip_prefix("---\n") {
        Some(rest) => rest,
        None => return content,
    };
    match rest.find("\n---") {
        // Past the closing fence and its newline, if there is one.
        Some(end) => rest[end + 4..].trim_start_matches('\n'),
        None => content,
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_the_first_h1_after_any_frontmatter() {
        assert_eq!(document_title("# Hello World\n\nBody").as_deref(), Some("Hello World"));
        assert_eq!(
            document_title("---\ntitle: x\n---\n\n# Real Title\nBody").as_deref(),
            Some("Real Title")
        );
        assert_eq!(document_title("## Subheading\nlead"), None);
        assert_eq!(document_title(""), None);
    }

    #[test]
    fn pin_priority_buckets() {
        // Project-named doc pins first, by display name OR filename stem,
        // case-insensitively.
        assert_eq!(pin_priority("Engineering", "engineering", "engineering"), 0);
        assert_eq!(pin_priority("Whatever", "Engineering", "engineering"), 0);
        assert_eq!(pin_priority("README", "readme", "engineering"), 1);
        assert_eq!(pin_priority("Index", "index", "engineering"), 2);
        assert_eq!(pin_priority("Overview", "overview", "engineering"), 2);
        assert_eq!(pin_priority("Notes", "notes", "engineering"), 3);
    }

    #[test]
    fn pin_priority_empty_project_never_pins_to_zero() {
        // An empty project name must not make an empty display/stem pin first.
        assert_eq!(pin_priority("", "", ""), 3);
        assert_eq!(pin_priority("readme", "readme", ""), 1);
    }

    // Shelbi additions: the listing itself, which ContextStore tested through
    // its AppKit model.

    #[test]
    fn the_file_name_is_the_fallback_title() {
        let path = Path::new("/space/Plans/zen-mode.md");
        assert_eq!(title_or_file_name("No heading here.\n", path), "zen-mode");
        assert_eq!(title_or_file_name("# Zen Mode\n", path), "Zen Mode");
    }

    #[test]
    fn documents_list_pinned_then_by_title() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let folder = root.join("Plans");
        std::fs::create_dir_all(&folder).unwrap();
        for (name, body) in [
            ("zebra.md", "# alpha\n"),
            ("beta.md", "# Beta\n"),
            ("overview.md", "# Overview\n"),
            ("README.md", "# Read me first\n"),
            ("plans.md", "# The plans\n"),
            ("notes.md", "No heading\n"),
            ("skip.txt", "# Not markdown\n"),
        ] {
            std::fs::write(folder.join(name), body).unwrap();
        }
        let titles: Vec<String> = list_documents(&folder, root, "Plans")
            .unwrap()
            .into_iter()
            .map(|e| e.title)
            .collect();
        assert_eq!(
            titles,
            ["The plans", "Read me first", "Overview", "alpha", "Beta", "notes"]
        );
    }

    #[test]
    fn csignore_hides_documents_from_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".csignore"), "CLAUDE.md\n").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "# Agent notes\n").unwrap();
        std::fs::write(root.join("README.md"), "# Readme\n").unwrap();
        let entries = list_documents(root, root, "Space").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Readme");
    }
}
