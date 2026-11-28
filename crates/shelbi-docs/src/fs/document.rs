// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/document.rs), MIT in Shelbi.

//! Document CRUD: list, create, read, write, delete.
//!
//! Documents are `.md` files inside a folder. Writes go through the
//! crate-private `atomic_write` helper; deletes go through
//! [`delete_to_trash`] so users can recover via their platform trash.
//! Title extraction (first H1) and full `.csignore`-aware listing land
//! with their own DoD items; this iteration's `list_documents` returns
//! every `.md` in the given directory.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::domain::Document;

use crate::fs::paths::{atomic_write, delete_to_trash, iso8601_utc, unique_document_filename};
use crate::fs::FsError;

/// Write `content` to `path` atomically. Parent directories are created
/// if they don't yet exist.
pub fn write_document(path: &Path, content: &str) -> Result<(), FsError> {
    atomic_write(path, content.as_bytes())
}

/// Read the UTF-8 contents of a document at `path`.
pub fn read_document(path: &Path) -> Result<String, FsError> {
    std::fs::read_to_string(path).map_err(|source| FsError::Io {
        path: path.to_string_lossy().into_owned(),
        source,
    })
}

/// Move a document to the platform trash / recycle bin.
pub fn delete_document(path: &Path) -> Result<(), FsError> {
    delete_to_trash(path)
}

/// Create a new blank document inside `folder` using the space's
/// blank template at `<space_root>/.contextstore/templates/blank.md`.
///
/// If the template doesn't exist, it is materialized with the default
/// content from [`crate::fs::defaults::BLANK_TEMPLATE`]. The new file is
/// named `new-document.md` (or `new-document-N.md` if that's taken).
pub fn create_blank_document(folder: &Path, space_root: &Path) -> Result<PathBuf, FsError> {
    let templates = space_root.join(".contextstore").join("templates");
    let template_file = templates.join("blank.md");
    let content = match std::fs::read_to_string(&template_file) {
        Ok(s) => s,
        Err(_) => {
            std::fs::create_dir_all(&templates).map_err(|source| FsError::Io {
                path: templates.to_string_lossy().into_owned(),
                source,
            })?;
            atomic_write(&template_file, crate::fs::defaults::BLANK_TEMPLATE.as_bytes())?;
            crate::fs::defaults::BLANK_TEMPLATE.to_string()
        }
    };
    let stem = unique_document_filename("new-document", folder, None);
    let target = folder.join(format!("{stem}.md"));
    atomic_write(&target, content.as_bytes())?;
    Ok(target)
}

/// Duplicate `source`, returning the new path. The duplicate is named
/// `<stem> copy.md`, `<stem> copy 2.md`, etc., and its first H1
/// heading is rewritten to match the new name.
pub fn duplicate_document(source: &Path) -> Result<PathBuf, FsError> {
    let parent = source.parent().ok_or_else(|| FsError::Io {
        path: source.to_string_lossy().into_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no parent"),
    })?;
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| FsError::Io {
            path: source.to_string_lossy().into_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no stem"),
        })?;
    let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("md");

    let mut copy_name = format!("{stem} copy");
    let mut counter = 2usize;
    let mut copy_path = parent.join(format!("{copy_name}.{ext}"));
    while copy_path.exists() {
        copy_name = format!("{stem} copy {counter}");
        copy_path = parent.join(format!("{copy_name}.{ext}"));
        counter += 1;
    }

    let original = std::fs::read_to_string(source).map_err(|source_err| FsError::Io {
        path: source.to_string_lossy().into_owned(),
        source: source_err,
    })?;

    let suffix = if counter == 2 {
        " copy".to_string()
    } else {
        format!(" copy {}", counter - 1)
    };
    let mut new_content = String::with_capacity(original.len());
    let mut rewrote_heading = false;
    for line in original.split_inclusive('\n') {
        if !rewrote_heading {
            let line_trimmed = line.trim_start();
            if let Some(rest) = line_trimmed.strip_prefix("# ") {
                let title = rest.trim_end_matches(['\n', '\r']).trim();
                new_content.push_str("# ");
                new_content.push_str(title);
                new_content.push_str(&suffix);
                if line.ends_with('\n') {
                    new_content.push('\n');
                }
                rewrote_heading = true;
                continue;
            }
        }
        new_content.push_str(line);
    }

    atomic_write(&copy_path, new_content.as_bytes())?;
    Ok(copy_path)
}

/// Move `source` into `destination_folder`, keeping its filename.
/// Errors with `AlreadyExists` if a file with the same name already
/// lives at the destination.
pub fn move_document(source: &Path, destination_folder: &Path) -> Result<PathBuf, FsError> {
    let Some(name) = source.file_name() else {
        return Err(FsError::Io {
            path: source.to_string_lossy().into_owned(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no filename"),
        });
    };
    let dest = destination_folder.join(name);
    if dest.exists() {
        return Err(FsError::Io {
            path: dest.to_string_lossy().into_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "a document named \"{}\" already exists in the destination",
                    name.to_string_lossy()
                ),
            ),
        });
    }
    std::fs::rename(source, &dest).map_err(|source_err| FsError::Io {
        path: dest.to_string_lossy().into_owned(),
        source: source_err,
    })?;
    Ok(dest)
}

/// Import (copy) every `.md` file in `sources` into
/// `destination_folder`, producing unique filenames if necessary.
/// Non-markdown files are silently skipped.
pub fn import_documents(
    sources: &[PathBuf],
    destination_folder: &Path,
) -> Result<Vec<PathBuf>, FsError> {
    let mut imported = Vec::new();
    for src in sources {
        let Some(ext) = src.extension().and_then(|s| s.to_str()) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case("md") {
            continue;
        }
        let Some(stem) = src.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let unique = unique_document_filename(stem, destination_folder, None);
        let target = destination_folder.join(format!("{unique}.md"));
        std::fs::copy(src, &target).map_err(|source| FsError::Io {
            path: target.to_string_lossy().into_owned(),
            source,
        })?;
        imported.push(target);
    }
    Ok(imported)
}

/// Rename a document on disk. New name is the `.md`-less stem; the
/// extension is preserved.
pub fn rename_document(source: &Path, new_stem: &str) -> Result<PathBuf, FsError> {
    let parent = source.parent().ok_or_else(|| FsError::Io {
        path: source.to_string_lossy().into_owned(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "source has no parent"),
    })?;
    let ext = source.extension().and_then(|s| s.to_str()).unwrap_or("md");
    let dest = parent.join(format!("{new_stem}.{ext}"));
    if dest == source {
        return Ok(dest);
    }
    if dest.exists() {
        return Err(FsError::Io {
            path: dest.to_string_lossy().into_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("a document named \"{new_stem}.{ext}\" already exists"),
            ),
        });
    }
    std::fs::rename(source, &dest).map_err(|source_err| FsError::Io {
        path: dest.to_string_lossy().into_owned(),
        source: source_err,
    })?;
    Ok(dest)
}

/// List every `.md` file directly inside `folder_path`.
///
/// `folder_path` can be any folder — a space root or any nested
/// folder are treated identically. Returns `relative_path` values that
/// are relative to `folder_path`. Does not recurse into sub-folders,
/// does not apply `.csignore`, and does not extract titles; those land
/// with their dedicated DoD items.
pub fn list_documents(folder_path: &Path) -> Result<Vec<Document>, FsError> {
    let mut docs = Vec::new();
    let entries = match std::fs::read_dir(folder_path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(docs),
        Err(source) => {
            return Err(FsError::Io {
                path: folder_path.to_string_lossy().into_owned(),
                source,
            });
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let metadata = entry.metadata().map_err(|source| FsError::Io {
            path: path.to_string_lossy().into_owned(),
            source,
        })?;
        if !metadata.is_file() {
            continue;
        }

        let relative = path
            .strip_prefix(folder_path)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.clone());

        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| iso8601_utc(d.as_secs() as i64))
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

        docs.push(Document {
            path: path.to_string_lossy().into_owned(),
            relative_path: relative.to_string_lossy().into_owned(),
            title: None,
            size_bytes: metadata.len(),
            modified_at,
        });
    }
    docs.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrips_content() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("doc.md");
        write_document(&target, "# Hello\n\nWorld.\n").expect("write");
        let read = read_document(&target).expect("read");
        assert_eq!(read, "# Hello\n\nWorld.\n");
    }

    #[test]
    fn write_creates_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a/b/c/doc.md");
        write_document(&target, "content").expect("write");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "content");
    }

    #[test]
    fn read_missing_file_returns_io_error() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("nope.md");
        let err = read_document(&target).expect_err("expected error");
        match err {
            FsError::Io { source, .. } => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected FsError::Io, got {other:?}"),
        }
    }

    #[test]
    fn delete_removes_file_from_original_location() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("doc.md");
        write_document(&target, "x").expect("write");
        assert!(target.exists());
        delete_document(&target).expect("delete");
        assert!(!target.exists());
    }

    #[test]
    fn list_documents_returns_only_markdown_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        write_document(&tmp.path().join("z.md"), "z").unwrap();
        write_document(&tmp.path().join("a.md"), "a").unwrap();
        // Non-markdown files are skipped.
        std::fs::write(tmp.path().join("note.txt"), "txt").unwrap();
        // Subdirectories aren't recursed into.
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();
        write_document(&tmp.path().join("subdir/x.md"), "x").unwrap();

        let docs = list_documents(tmp.path()).expect("list");
        let names: Vec<_> = docs.iter().map(|d| d.relative_path.clone()).collect();
        assert_eq!(names, vec!["a.md", "z.md"]);
        for d in &docs {
            assert_eq!(d.title, None);
            assert!(d.size_bytes > 0);
            assert!(d.modified_at.ends_with('Z'));
        }
    }

    #[test]
    fn list_documents_is_uniform_at_any_nesting_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("engineering");
        std::fs::create_dir_all(&nested).unwrap();
        write_document(&nested.join("adr.md"), "adr").unwrap();

        let docs = list_documents(&nested).expect("list");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].relative_path, "adr.md");
    }

    #[test]
    fn list_documents_returns_empty_when_folder_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = list_documents(&tmp.path().join("nope")).expect("list");
        assert!(docs.is_empty());
    }
}
