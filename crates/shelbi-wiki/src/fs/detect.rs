// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/detect.rs), MIT in Shelbi.

//! Folder detection: classify what kind of space (if any) lives at a
//! given directory.
//!
//! Used by the macOS app's "Connect Existing Space" flow to inspect a
//! folder the user picked and tell them what they're looking at: a
//! tracked space, an untracked but valid space, a git repo without
//! ContextStore settings, a code repo we shouldn't touch, or just a
//! plain folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs::settings::{load_space_settings, space_settings_path};

/// Result of inspecting a candidate space folder. Mirrors the Swift
/// `FolderDetectionResult` model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderDetection {
    /// Absolute path of the folder.
    pub folder_path: PathBuf,
    /// True when `.git/` exists at `folder_path`.
    pub has_git: bool,
    /// True when `.contextstore/settings.yml` exists at `folder_path`.
    pub has_settings: bool,
    /// Space name from `settings.yml` if present.
    pub space_name: Option<String>,
    /// Count of direct-child subdirectories that themselves have
    /// `.contextstore/settings.yml` (i.e. projects under the space).
    pub folder_count: usize,
    /// Count of `.md` files inside the folder (recursive, skipping
    /// `.contextstore/`).
    pub document_count: usize,
}

/// Inspect `folder_path` and report what's there.
pub fn detect_folder_state(folder_path: &Path) -> FolderDetection {
    let has_git = folder_path.join(".git").is_dir();
    let has_settings = space_settings_path(folder_path).is_file();
    let space_name = if has_settings {
        load_space_settings(folder_path).ok().map(|s| s.name)
    } else {
        None
    };

    let mut folder_count = 0usize;
    let mut document_count = 0usize;
    if let Ok(entries) = std::fs::read_dir(folder_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if space_settings_path(&path).is_file() {
                    folder_count += 1;
                }
                document_count += count_documents_recursive(&path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                document_count += 1;
            }
        }
    }

    FolderDetection {
        folder_path: folder_path.to_path_buf(),
        has_git,
        has_settings,
        space_name,
        folder_count,
        document_count,
    }
}

/// Count `.md` files under `dir` recursively, skipping any directory
/// whose name starts with `.` and the special `.contextstore/`
/// subtree.
pub fn count_documents_recursive(dir: &Path) -> usize {
    fn walk(dir: &Path, total: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                walk(&path, total);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
            {
                *total += 1;
            }
        }
    }
    let mut total = 0usize;
    walk(dir, &mut total);
    total
}

/// True when `dir` looks like a source-code repository (Cargo, npm,
/// Xcode, Gradle, …). Used to discourage users from accidentally
/// connecting a build directory as a ContextStore space.
pub fn is_code_repository(dir: &Path) -> bool {
    const MANIFEST_NAMES: &[&str] = &[
        "package.json",
        "Cargo.toml",
        "go.mod",
        "Makefile",
        "CMakeLists.txt",
        "Gemfile",
        "requirements.txt",
        "pyproject.toml",
        "build.gradle",
        "pom.xml",
        "Package.swift",
        "project.yml",
    ];
    for manifest in MANIFEST_NAMES {
        if dir.join(manifest).is_file() {
            return true;
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.ends_with(".xcodeproj") || name.ends_with(".sln") {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_space(root: &Path, name: &str) {
        let dir = root.join(".contextstore");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.yml"),
            format!("name: {name}\ncreatedAt: '2026-05-01T12:00:00Z'\n"),
        )
        .unwrap();
    }

    #[test]
    fn detects_an_empty_directory_as_plain() {
        let tmp = tempfile::tempdir().unwrap();
        let r = detect_folder_state(tmp.path());
        assert!(!r.has_git && !r.has_settings);
        assert_eq!(r.space_name, None);
        assert_eq!(r.folder_count, 0);
        assert_eq!(r.document_count, 0);
    }

    #[test]
    fn detects_tracked_space_with_projects_and_docs() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        // A project (subdir with settings.yml).
        let proj = tmp.path().join("Engineering");
        seed_space(&proj, "Engineering");
        std::fs::write(proj.join("adr.md"), "# adr").unwrap();
        // A plain folder containing markdown — counts toward documents.
        std::fs::create_dir(tmp.path().join("Notes")).unwrap();
        std::fs::write(tmp.path().join("Notes/scratch.md"), "# x").unwrap();
        // A markdown file at the root.
        std::fs::write(tmp.path().join("README.md"), "# r").unwrap();

        let r = detect_folder_state(tmp.path());
        assert!(r.has_settings);
        assert_eq!(r.space_name.as_deref(), Some("Space"));
        assert_eq!(r.folder_count, 1, "Engineering should be a project");
        assert_eq!(r.document_count, 3);
    }

    #[test]
    fn is_code_repository_matches_manifests() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_code_repository(tmp.path()));
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        assert!(is_code_repository(tmp.path()));
    }
}
