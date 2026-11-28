// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/folder.rs), MIT in Shelbi.

//! Folder CRUD inside a space — uniform across nesting depth.
//!
//! Every non-internal subdirectory is a folder: top-level folders that
//! the user sees in the sidebar and any nested sub-folders are the same
//! kind of thing and go through the same APIs. A folder optionally
//! carries metadata in `.contextstore/settings.yml` (icon, description);
//! folders without that file are still folders, just with no
//! per-folder metadata.
//!
//! Internal directories — `.contextstore` (settings/templates) and
//! `.git` (the version-controlled repo) — are never reported as
//! folders.

use std::path::{Path, PathBuf};

use crate::domain::Folder;

use crate::fs::paths::{atomic_write, kebab_slug, sanitize_preserve_case};
use crate::fs::settings::{
    ProjectSettings as FolderMetadata, ProjectSettingsFile, ProjectSettingsOnDisk,
};
use crate::fs::FsError;

/// Path of a folder's optional metadata file:
/// `<folder>/.contextstore/settings.yml`.
fn folder_metadata_path(folder_root: &Path) -> PathBuf {
    folder_root.join(".contextstore").join("settings.yml")
}

/// Directory names that are part of ContextStore's own internal layout
/// (or a hosting VCS) and are never surfaced as user-visible folders.
const INTERNAL_DIR_NAMES: &[&str] = &[".contextstore", ".git"];

fn is_internal_dir(name: &str) -> bool {
    INTERNAL_DIR_NAMES.contains(&name)
}

/// Create a folder under `parent`.
///
/// `parent` is any directory the caller wants to nest this folder
/// under — the space root, another folder, or anywhere in between.
/// Writes optional metadata to `.contextstore/settings.yml` only when
/// the caller actually supplies metadata (icon/description), keeping
/// the on-disk shape free of `.contextstore/` clutter for plain
/// folders.
///
/// The on-disk folder name is [`crate::fs::paths::kebab_slug`] of `name`.
pub fn create_folder(parent: &Path, name: &str, icon: Option<&str>) -> Result<Folder, FsError> {
    let slug = kebab_slug(name);
    if slug.is_empty() {
        return Err(FsError::Io {
            path: parent.to_string_lossy().into_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "folder name is empty after sanitization",
            ),
        });
    }
    let folder_root = parent.join(&slug);
    std::fs::create_dir_all(&folder_root).map_err(|source| FsError::Io {
        path: folder_root.to_string_lossy().into_owned(),
        source,
    })?;

    if icon.is_some() {
        std::fs::create_dir_all(folder_root.join(".contextstore")).map_err(|source| {
            FsError::Io {
                path: folder_root.to_string_lossy().into_owned(),
                source,
            }
        })?;
        let settings = ProjectSettingsFile::current(FolderMetadata {
            name: name.to_string(),
            icon: icon.map(str::to_string),
            description: None,
        });
        let yaml = yaml_serde::to_string(&settings).map_err(FsError::SettingsEncode)?;
        atomic_write(&folder_metadata_path(&folder_root), yaml.as_bytes())?;
    }

    Ok(Folder {
        name: name.to_string(),
        path: folder_root.to_string_lossy().into_owned(),
        relative_path: slug,
        icon: icon.map(str::to_string),
        description: None,
    })
}

/// List every direct-child folder of `parent`.
///
/// `parent` may be a space root or any folder. Every non-hidden
/// subdirectory is a folder; the only directories filtered out are
/// the internal ones (`.contextstore`, `.git`).
///
/// When `<child>/.contextstore/settings.yml` is present, metadata
/// (name, icon, description) comes from there; otherwise the folder
/// name is derived from the on-disk directory name.
pub fn list_folders(parent: &Path) -> Result<Vec<Folder>, FsError> {
    let mut folders = Vec::new();
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(folders),
        Err(source) => {
            return Err(FsError::Io {
                path: parent.to_string_lossy().into_owned(),
                source,
            });
        }
    };

    for entry in entries.flatten() {
        let folder_root = entry.path();
        if !folder_root.is_dir() {
            continue;
        }
        let dir_name = match folder_root.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue, // skip non-UTF-8 names
        };
        if is_internal_dir(dir_name) {
            continue;
        }

        let (display_name, icon, description) = read_optional_metadata(&folder_root)?
            .map(|m| (m.name, m.icon, m.description))
            .unwrap_or_else(|| (dir_name.to_string(), None, None));

        let relative = folder_root
            .strip_prefix(parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| folder_root.clone());

        folders.push(Folder {
            name: display_name,
            path: folder_root.to_string_lossy().into_owned(),
            relative_path: relative.to_string_lossy().into_owned(),
            icon,
            description,
        });
    }
    folders.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(folders)
}

/// Create a sub-folder with the given display name, preserving casing
/// (the Swift `FileSystemManager.createFolder(name:in:)` behavior used
/// for nested folders the user types directly).
pub fn create_subfolder_preserving_case(parent: &Path, name: &str) -> Result<PathBuf, FsError> {
    let sanitized = sanitize_preserve_case(name);
    let path = parent.join(&sanitized);
    std::fs::create_dir_all(&path).map_err(|source| FsError::Io {
        path: path.to_string_lossy().into_owned(),
        source,
    })?;
    Ok(path)
}

/// Rename `folder_path` to a sibling named `new_name`.
///
/// `preserve_case = true` runs the same Unicode-preserving sanitization
/// the Swift app uses for nested folders the user typed; `false`
/// applies the kebab-slug used for top-level project folders.
///
/// Returns the new path. Errors with `AlreadyExists` if the
/// destination is occupied and the rename is not a pure case change.
pub fn rename_folder(
    folder_path: &Path,
    new_name: &str,
    preserve_case: bool,
) -> Result<PathBuf, FsError> {
    let parent = folder_path.parent().ok_or_else(|| FsError::Io {
        path: folder_path.to_string_lossy().into_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "folder has no parent"),
    })?;
    let sanitized = if preserve_case {
        sanitize_preserve_case(new_name)
    } else {
        let s = kebab_slug(new_name);
        if s.is_empty() {
            "untitled".to_string()
        } else {
            s
        }
    };
    let current = folder_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if sanitized == current {
        return Ok(folder_path.to_path_buf());
    }
    let dest = parent.join(&sanitized);
    let case_only = current.eq_ignore_ascii_case(&sanitized);
    if !case_only && dest.exists() {
        return Err(FsError::Io {
            path: dest.to_string_lossy().into_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("a folder named \"{sanitized}\" already exists"),
            ),
        });
    }
    std::fs::rename(folder_path, &dest).map_err(|source| FsError::Io {
        path: dest.to_string_lossy().into_owned(),
        source,
    })?;
    Ok(dest)
}

/// Move `folder_path` into `destination_parent`, preserving its
/// directory name. Errors with `AlreadyExists` if a sibling with the
/// same name already exists at the destination.
pub fn move_folder(folder_path: &Path, destination_parent: &Path) -> Result<PathBuf, FsError> {
    let Some(name) = folder_path.file_name() else {
        return Err(FsError::Io {
            path: folder_path.to_string_lossy().into_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "folder has no name"),
        });
    };
    let dest = destination_parent.join(name);
    if dest.exists() {
        return Err(FsError::Io {
            path: dest.to_string_lossy().into_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "a folder named \"{}\" already exists at the destination",
                    name.to_string_lossy()
                ),
            ),
        });
    }
    std::fs::rename(folder_path, &dest).map_err(|source| FsError::Io {
        path: dest.to_string_lossy().into_owned(),
        source,
    })?;
    Ok(dest)
}

/// Delete the folder by moving it to the platform trash / recycle
/// bin. See [`crate::fs::paths::delete_to_trash`].
pub fn delete_folder(folder_path: &Path) -> Result<(), FsError> {
    crate::fs::paths::delete_to_trash(folder_path)
}

/// A folder's emoji icon from its `.contextstore/settings.yml`, when it
/// sets one. Shelbi addition (the tree's folder rows); a missing or
/// unreadable file has none.
pub fn folder_icon(folder_root: &Path) -> Option<String> {
    read_optional_metadata(folder_root)
        .ok()
        .flatten()
        .and_then(|m| m.icon)
        .map(|icon| icon.trim().to_string())
        .filter(|icon| !icon.is_empty())
}

fn read_optional_metadata(folder_root: &Path) -> Result<Option<FolderMetadata>, FsError> {
    let path = folder_metadata_path(folder_root);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(FsError::Io {
                path: path.to_string_lossy().into_owned(),
                source,
            });
        }
    };
    // Empty / `{}` settings.yml: the Swift app writes this shape for
    // projects with no overrides. Treat it the same as a missing file
    // so the dir-name fallback in `list_folders` fires.
    if is_empty_yaml_object(&raw) {
        return Ok(None);
    }
    // Accept both the canonical wrapped envelope and the legacy flat
    // form some Swift versions wrote — see ProjectSettingsOnDisk.
    let parsed: ProjectSettingsOnDisk =
        yaml_serde::from_str(&raw).map_err(|source| FsError::SettingsParse {
            path: path.to_string_lossy().into_owned(),
            source,
        })?;
    Ok(Some(parsed.into_settings()))
}

/// True when `raw` parses to either an empty document or an empty
/// mapping. Whitespace and YAML comment lines are ignored. The check
/// is intentionally cheap (no full parse) so non-empty content still
/// hits the strict decoder and surfaces real parse errors.
fn is_empty_yaml_object(raw: &str) -> bool {
    let stripped: String = raw
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = stripped.trim();
    trimmed.is_empty() || trimmed == "{}"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_space(root: &Path) {
        let dir = root.join(".contextstore");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.yml"),
            "name: Planning\ncreatedAt: '2026-05-01T12:00:00Z'\n",
        )
        .unwrap();
    }

    #[test]
    fn create_folder_with_icon_writes_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path());

        let folder = create_folder(tmp.path(), "Engineering", Some("⚙️")).expect("create_folder");

        assert_eq!(folder.name, "Engineering");
        assert_eq!(folder.icon.as_deref(), Some("⚙️"));
        assert_eq!(folder.relative_path, "engineering");

        let metadata_yml = folder_metadata_path(Path::new(&folder.path));
        assert!(metadata_yml.is_file());
        let yaml = std::fs::read_to_string(&metadata_yml).unwrap();
        assert!(yaml.contains("Engineering"));
        assert!(yaml.contains("⚙️"));
    }

    #[test]
    fn create_folder_without_metadata_skips_settings_yml() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path());

        let folder = create_folder(tmp.path(), "Notes", None).expect("create_folder");

        assert!(Path::new(&folder.path).is_dir());
        // No metadata file should have been written.
        assert!(!folder_metadata_path(Path::new(&folder.path)).exists());
    }

    #[test]
    fn list_folders_returns_every_subdirectory_excluding_internal() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path());

        // Folder with metadata.
        create_folder(tmp.path(), "Engineering", Some("⚙️")).unwrap();
        // Folder without metadata — still a folder.
        create_folder(tmp.path(), "Notes", None).unwrap();
        // Plain manually-created subdirectory — also a folder.
        std::fs::create_dir(tmp.path().join("inbox")).unwrap();
        // `.git` and `.contextstore` are internal and must be excluded.
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        // (.contextstore was created by seed_space)

        let folders = list_folders(tmp.path()).expect("list_folders");
        let names: Vec<&str> = folders.iter().map(|f| f.name.as_str()).collect();
        // "Engineering" was created with metadata so its display name is
        // the user-typed name; "inbox" and "notes" have no metadata, so
        // their display name falls back to the on-disk directory name
        // (which for "Notes" came out as "notes" via the kebab slug).
        assert_eq!(names, vec!["Engineering", "inbox", "notes"]);
        // The folder with metadata picks up the icon.
        assert_eq!(folders[0].icon.as_deref(), Some("⚙️"));
        // Folders without metadata fall back to the directory name and
        // carry no icon/description.
        assert!(folders[1].icon.is_none());
        assert!(folders[2].icon.is_none());
    }

    #[test]
    fn list_folders_is_uniform_at_any_nesting_depth() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path());

        let top = create_folder(tmp.path(), "Engineering", None).unwrap();
        create_folder(Path::new(&top.path), "ADRs", Some("📐")).unwrap();
        create_folder(Path::new(&top.path), "Notes", None).unwrap();

        // list_folders on a sub-folder works exactly like on the space.
        // "ADRs" has metadata so keeps its display name; "Notes" doesn't,
        // so it falls back to the kebab-slugged directory name "notes".
        let nested = list_folders(Path::new(&top.path)).expect("list nested");
        let names: Vec<&str> = nested.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["ADRs", "notes"]);
    }

    #[test]
    fn list_folders_returns_empty_when_parent_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_folders(&tmp.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn create_folder_rejects_empty_name() {
        let tmp = tempfile::tempdir().unwrap();
        let err = create_folder(tmp.path(), "!!!", None).expect_err("expected error");
        assert!(matches!(err, FsError::Io { .. }));
    }

    #[test]
    fn list_folders_tolerates_empty_object_settings_yml_swift_writes() {
        // The Swift app writes `{}` to a project's `.contextstore/settings.yml`
        // when the project has no overrides. The Rust parser must tolerate
        // this — anyone cloning a Swift-authored space on Linux/Windows
        // gets these files committed in their repo. Falls back to the
        // dir basename as the display name.
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path());

        let folder = tmp.path().join("LinkedIn");
        std::fs::create_dir_all(folder.join(".contextstore")).unwrap();
        std::fs::write(folder.join(".contextstore/settings.yml"), "{}\n").unwrap();

        let folders = list_folders(tmp.path()).expect("list");
        let linkedin = folders
            .iter()
            .find(|f| f.relative_path == "LinkedIn")
            .expect("LinkedIn folder discovered");
        assert_eq!(linkedin.name, "LinkedIn");
        assert!(linkedin.icon.is_none());
        assert!(linkedin.description.is_none());
    }

    #[test]
    fn is_empty_yaml_object_recognizes_blank_braces_and_comments() {
        assert!(is_empty_yaml_object(""));
        assert!(is_empty_yaml_object("\n\n"));
        assert!(is_empty_yaml_object("   "));
        assert!(is_empty_yaml_object("{}"));
        assert!(is_empty_yaml_object("{}\n"));
        assert!(is_empty_yaml_object("# nothing here\n{}\n"));
        assert!(!is_empty_yaml_object("name: foo\n"));
        assert!(!is_empty_yaml_object("project:\n  name: foo\n"));
    }
}
