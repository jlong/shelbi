// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/paths.rs), MIT in Shelbi.

//! Path-shaped helpers: atomic writes, trash integration (sanitization
//! follows with a later DoD box).
//!
//! All settings and document writes go through `atomic_write` (the
//! crate-private helper in this module) so the file watcher never
//! observes a half-written file. All destructive deletes go through
//! [`delete_to_trash`] so the user can recover from the platform's
//! trash / recycle bin / XDG trash dir.

use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fs::FsError;

/// Join a caller-supplied path onto `space_root`, refusing anything that would
/// leave the space.
///
/// **This is the sanctioned way to turn a path from outside the app into a
/// filesystem path.** Any surface that accepts one — the CLI's `--space`
/// commands, the MCP server's tools, anything added later — should resolve it
/// here rather than reaching for [`Path::join`], which is not a boundary: an
/// absolute path replaces the root outright, and `..` walks above it.
///
/// That gap has been found twice, in `cstore-mcp` and `cstore-cli`, each time
/// permitting reads, overwrites and deletes anywhere the user could reach from
/// a command or tool call scoped to a single space. It lives here so the next
/// surface inherits the guard instead of reinventing the hole.
///
/// The check is deliberately lexical rather than `canonicalize`-based, because
/// `canonicalize` requires the path to exist and so cannot guard the calls that
/// create one. It refuses `a/../b`, which no legitimate caller sends, in
/// exchange for being impossible to defeat by ordering.
pub fn resolve_in_space(space_root: &Path, relative: &str) -> Result<PathBuf, FsError> {
    let candidate = Path::new(relative);
    if candidate.is_absolute() {
        return Err(FsError::PathOutsideSpace {
            path: relative.to_string(),
            reason: "path must be relative to the space",
        });
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(FsError::PathOutsideSpace {
                    path: relative.to_string(),
                    reason: "path must stay within the space",
                })
            }
        }
    }
    Ok(space_root.join(candidate))
}

/// Format a Unix-epoch second count as a UTC ISO 8601 string of the
/// form `YYYY-MM-DDTHH:MM:SSZ`. Used for `created_at` and
/// `modified_at` fields exposed across FFI.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm so we don't need
/// a date/time crate dep in `cstore-fs`. Accurate for any Gregorian
/// date the C type system can express.
pub fn iso8601_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let sod = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = sod / 3600;
    let minute = (sod % 3600) / 60;
    let second = sod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Current UTC time as an ISO 8601 string.
pub fn iso8601_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601_utc(secs)
}

/// Howard Hinnant's civil-from-days algorithm. `z` is days since
/// 1970-01-01. Returns `(year, month, day)` in the proleptic Gregorian
/// calendar.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Convert a human-typed name into a filesystem-safe kebab-case slug.
///
/// Preserves Unicode alphanumerics (matches the Swift sanitization
/// behavior — `"Café"` becomes `"café"`, `"日本語"` stays `"日本語"`).
/// Lowercases letters, collapses spaces and other non-alphanumeric
/// punctuation into a single `-`, and removes characters that aren't
/// safe in path components.
pub fn kebab_slug(name: &str) -> String {
    // Step 1: lowercase. Full Unicode, like Swift's `lowercased()` — an
    // ASCII-only pass left `"ÉTUDE"` as `"Étude"`, and a capital is the one
    // thing kebab-case exists to remove.
    let lowercased: String = name.chars().flat_map(char::to_lowercase).collect();

    // Step 2: split-on-space-or-non-alphanumeric, join with `-`.
    let mut out = String::with_capacity(lowercased.len());
    let mut last_was_dash = true; // suppress leading dashes
    for ch in lowercased.chars() {
        if ch == ' ' || ch == '-' || ch == '_' {
            if !last_was_dash {
                out.push('-');
                last_was_dash = true;
            }
        } else if ch.is_alphanumeric() {
            // Includes Unicode letters/digits.
            out.push(ch);
            last_was_dash = false;
        }
        // Other characters (punctuation, symbols) are silently dropped.
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `FileNameStyle::KebabCase` matching the Swift sanitizer:
/// alphanumeric or `-`, returns `"untitled"` when the input
/// sanitizes to empty (Swift fallback).
pub fn sanitize_kebab(name: &str) -> String {
    let s = kebab_slug(name);
    if s.is_empty() {
        "untitled".to_string()
    } else {
        s
    }
}

/// `FileNameStyle::CamelCase`: split on non-alphanumeric, lowercase
/// the first word, uppercase the first letter of every subsequent
/// word. Falls back to `"untitled"` when empty.
pub fn sanitize_camel_case(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    if words.is_empty() {
        return "untitled".to_string();
    }
    let mut out = String::new();
    out.push_str(&words[0].to_lowercase());
    for w in &words[1..] {
        let mut chars = w.chars();
        if let Some(c) = chars.next() {
            for u in c.to_uppercase() {
                out.push(u);
            }
        }
        for c in chars {
            for l in c.to_lowercase() {
                out.push(l);
            }
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// `FileNameStyle::Original`: preserve user casing and spaces; only
/// strip filesystem-unsafe characters (`/`, `:`, NUL), trim
/// whitespace edges, and drop any leading dots so the result isn't
/// hidden. Falls back to `"untitled"` when empty.
pub fn sanitize_preserve_case(name: &str) -> String {
    const UNSAFE: &[char] = &['/', ':', '\0'];
    let mut out: String = name.chars().filter(|c| !UNSAFE.contains(c)).collect();
    out = out.trim().to_string();
    while out.starts_with('.') {
        out = out.trim_start_matches('.').trim().to_string();
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

/// Humanize a kebab/snake folder name for display: replace `-`/`_`
/// with spaces, then capitalize each word.
pub fn humanize_folder_name(name: &str) -> String {
    name.split(['-', '_', ' '])
        .filter(|s| !s.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => {
                    let mut s = c.to_uppercase().collect::<String>();
                    for ch in chars {
                        s.push(ch);
                    }
                    s
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Style for [`sanitize_file_name_with_style`]. Mirrors
/// [`crate::fs::settings::FileNameStyle`] one-for-one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameStyle {
    /// `kebab-case`
    KebabCase,
    /// `camelCase`
    CamelCase,
    /// `Preserve Original Casing`
    Original,
}

/// Apply the given naming style.
pub fn sanitize_file_name_with_style(name: &str, style: NameStyle) -> String {
    match style {
        NameStyle::KebabCase => sanitize_kebab(name),
        NameStyle::CamelCase => sanitize_camel_case(name),
        NameStyle::Original => sanitize_preserve_case(name),
    }
}

/// Return a `.md` filename stem unique inside `folder` — if `base.md`
/// is taken, try `base-1.md`, `base-2.md`, … until one is free. An
/// optional `exclude` URL is treated as "already mine" — useful when
/// renaming.
pub fn unique_document_filename(base: &str, folder: &Path, exclude: Option<&Path>) -> String {
    let candidate = folder.join(format!("{base}.md"));
    if !candidate.exists() || exclude.map(|e| e == candidate).unwrap_or(false) {
        return base.to_string();
    }
    let mut counter = 1usize;
    loop {
        let trial_name = format!("{base}-{counter}");
        let trial = folder.join(format!("{trial_name}.md"));
        if !trial.exists() || exclude.map(|e| e == trial).unwrap_or(false) {
            return trial_name;
        }
        counter += 1;
    }
}

/// Public-API wrapper around the crate-private `atomic_write` helper
/// for callers outside the crate.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<(), FsError> {
    atomic_write(path, content)
}

/// Write `content` to `path` atomically.
///
/// Mechanism:
///
/// 1. Create a `tempfile::NamedTempFile` in `path`'s parent directory
///    (same filesystem, so the final rename is guaranteed to be
///    cross-device-free).
/// 2. Write the full payload to the temp file.
/// 3. `NamedTempFile::persist` calls the platform's atomic rename
///    primitive — `rename(2)` on Unix and `MoveFileExW` with
///    `MOVEFILE_REPLACE_EXISTING` on Windows.
///
/// If the process crashes between (1) and (3) the temp file is left
/// orphaned (under a `tempfile`-randomized name, so it never collides
/// with a future write) and the target file is untouched. Any error
/// before the rename leaves the original file in place.
pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> Result<(), FsError> {
    let parent = path.parent().ok_or_else(|| FsError::Io {
        path: path.to_string_lossy().into_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "atomic_write target has no parent directory",
        ),
    })?;

    if !parent.exists() {
        std::fs::create_dir_all(parent).map_err(|source| FsError::Io {
            path: parent.to_string_lossy().into_owned(),
            source,
        })?;
    }

    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| FsError::Io {
        path: parent.to_string_lossy().into_owned(),
        source,
    })?;
    tmp.write_all(content).map_err(|source| FsError::Io {
        path: tmp.path().to_string_lossy().into_owned(),
        source,
    })?;
    tmp.persist(path).map_err(|err| FsError::Io {
        path: path.to_string_lossy().into_owned(),
        source: err.error,
    })?;
    Ok(())
}

/// Move `path` to the platform's trash / recycle bin.
///
/// Wraps [`trash::delete`], which dispatches per platform:
///
///   * macOS: `NSFileManager.trashItem(at:)` via the Cocoa API.
///   * Windows: `IFileOperation::DeleteItem` (shows up in the Recycle Bin).
///   * Linux: writes into `$XDG_DATA_HOME/Trash` per the
///     [XDG Trash spec](https://specifications.freedesktop.org/trash-spec/).
///
/// The smoke-test invariant the sub-plan calls out is "the file is gone
/// from its original location" — recoverability of the trashed entry is
/// a per-platform / per-user-environment concern that lives outside
/// this crate.
pub fn delete_to_trash(path: &Path) -> Result<(), FsError> {
    trash::delete(path).map_err(|err| FsError::Io {
        path: path.to_string_lossy().into_owned(),
        source: std::io::Error::other(err),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_new_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("settings.yml");
        atomic_write(&target, b"hello\n").expect("write");
        assert_eq!(std::fs::read(&target).unwrap(), b"hello\n");
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("settings.yml");
        std::fs::write(&target, b"old\n").unwrap();
        atomic_write(&target, b"new\n").expect("write");
        assert_eq!(std::fs::read(&target).unwrap(), b"new\n");
    }

    #[test]
    fn atomic_write_creates_parent_directory_if_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("nested").join("settings.yml");
        atomic_write(&target, b"hello\n").expect("write");
        assert_eq!(std::fs::read(&target).unwrap(), b"hello\n");
    }

    #[test]
    fn dropped_temp_file_does_not_touch_existing_target() {
        // Documents the invariant `atomic_write` relies on: a
        // `NamedTempFile` dropped without `persist` (which is what a
        // mid-write process crash looks like to the next process) does
        // not affect any pre-existing file at the eventual target path.
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("settings.yml");
        std::fs::write(&target, b"original\n").unwrap();

        {
            let mut named = tempfile::NamedTempFile::new_in(tmp.path()).expect("named tmp");
            named
                .write_all(b"would_have_replaced\n")
                .expect("partial write");
            // No `.persist(&target)` — drop simulates a crash.
        }

        assert_eq!(std::fs::read(&target).unwrap(), b"original\n");
    }

    #[test]
    fn delete_to_trash_removes_file_from_original_location() {
        // Per the sub-plan: "real trash test in CI is hard; cross-check
        // via a smoke test that the file is gone from its location."
        // That's what we verify here — recoverability of the trashed
        // entry is a platform / user-environment concern.
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("to-delete.md");
        std::fs::write(&target, b"goodbye\n").expect("seed file");
        assert!(target.exists(), "precondition: file exists before delete");

        delete_to_trash(&target).expect("trash");

        assert!(
            !target.exists(),
            "file should be gone from its original location after delete_to_trash"
        );
    }

    #[test]
    fn iso8601_utc_formats_known_unix_epoch_values() {
        // Unix epoch itself.
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
        // 2026-05-01T12:00:00Z = 1777636800.
        assert_eq!(iso8601_utc(1_777_636_800), "2026-05-01T12:00:00Z");
        // Leap-year-safe sanity: 2024-02-29T00:00:00Z = 1709164800.
        assert_eq!(iso8601_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn kebab_slug_lowercases_and_dashes() {
        assert_eq!(kebab_slug("My Project"), "my-project");
        assert_eq!(kebab_slug("  Hello  World!  "), "hello-world");
        assert_eq!(kebab_slug("ADR-001"), "adr-001");
        assert_eq!(kebab_slug("___underscores___"), "underscores");
        assert_eq!(kebab_slug(""), "");
        assert_eq!(kebab_slug("!@#$%"), "");
    }

    #[test]
    fn kebab_slug_lowercases_beyond_ascii() {
        // An ASCII-only lowercase left these capitalised, which is exactly
        // what kebab-case is for removing.
        assert_eq!(kebab_slug("ÉTUDE"), "étude");
        assert_eq!(kebab_slug("Über Alles"), "über-alles");
        assert_eq!(kebab_slug("ΣΟΦΊΑ"), "σοφία");
        // Scripts without case are untouched.
        assert_eq!(kebab_slug("日本語"), "日本語");
    }

    #[test]
    fn delete_to_trash_missing_file_returns_io_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("never-existed.md");
        let err = delete_to_trash(&target).expect_err("expected error");
        assert!(matches!(err, FsError::Io { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_failure_leaves_original_intact() {
        // Force atomic_write to fail by making the parent directory
        // read-only after seeding the original target. Without write
        // access to the parent, NamedTempFile::new_in can't create the
        // staging file, so the rename never happens.
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("locked");
        std::fs::create_dir(&parent).unwrap();
        let target = parent.join("settings.yml");
        std::fs::write(&target, b"original\n").unwrap();

        let mut perms = std::fs::metadata(&parent).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(&parent, perms).unwrap();

        let err = atomic_write(&target, b"new\n")
            .expect_err("atomic_write must fail when parent is read-only");
        assert!(matches!(err, FsError::Io { .. }));

        // Restore write permission so the tempdir teardown can clean up.
        let mut perms = std::fs::metadata(&parent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&parent, perms).unwrap();

        // Original content survives the failed write.
        assert_eq!(std::fs::read(&target).unwrap(), b"original\n");
    }
    #[test]
    fn resolve_in_space_refuses_a_path_that_leaves_the_space() {
        let root = Path::new("/spaces/Notes");
        for escape in [
            "/etc/hosts",
            "../Other/doc.md",
            "../../outside.md",
            "Product/../../../elsewhere.md",
        ] {
            assert!(
                resolve_in_space(root, escape).is_err(),
                "escaped the space: {escape}"
            );
        }
    }

    #[test]
    fn resolve_in_space_joins_an_ordinary_relative_path() {
        let root = Path::new("/spaces/Notes");
        assert_eq!(
            resolve_in_space(root, "Product/roadmap.md").expect("inside"),
            PathBuf::from("/spaces/Notes/Product/roadmap.md")
        );
        assert_eq!(
            resolve_in_space(root, "./index.md").expect("inside"),
            PathBuf::from("/spaces/Notes/index.md")
        );
    }
}
