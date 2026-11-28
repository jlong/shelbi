// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-domain/src/lib.rs), MIT in Shelbi.

//! Foundational ContextStore types.
//!
//! Shelbi drops the GitHub connection and auth types (`GitHubConnection`,
//! `GitHubAuthKind`, `CoreError::Auth`): docs syncs with the user's own git
//! setup, never a stored credential.
//!
//! No I/O, no filesystem access, no async. Just data and errors that
//! every other `cstore-*` crate depends on. Splitting these out keeps
//! the workspace's dependency graph acyclic — `cstore-domain` depends
//! on nothing else in the workspace — and gives the rest of the core a
//! stable anchor for its shared types.
//!
//! Path fields are stored as UTF-8 [`String`]s rather than [`PathBuf`]
//! because these types serialize to the on-disk YAML settings and paths
//! containing non-UTF-8 sequences are not supported. Crates that need a
//! [`PathBuf`] convert at their own boundary.
//!
//! [`PathBuf`]: std::path::PathBuf

#![deny(missing_docs)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─── Space ──────────────────────────────────────────────────────────

/// A ContextStore space.
///
/// A directory whose root contains a `.contextstore/settings.yml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Space {
    /// Human-readable name shown in the UI.
    pub name: String,

    /// Absolute path to the space directory on disk, as UTF-8.
    pub path: String,

    /// ISO 8601 timestamp recorded when the space was first created.
    pub created_at: String,
}

// ─── Folder ─────────────────────────────────────────────────────────

/// A folder inside a space.
///
/// Every non-internal subdirectory of a space (or of another folder)
/// is a folder — top-level folders and nested sub-folders are the same
/// type and treated uniformly by discovery, listing, and CRUD APIs.
/// A folder may optionally carry metadata in `.contextstore/settings.yml`
/// (icon, description); folders without that file are still folders,
/// just with no extra metadata. Internal directories like
/// `.contextstore/` and `.git/` are excluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    /// Human-readable folder name.
    pub name: String,

    /// Absolute path to the folder on disk, as UTF-8.
    pub path: String,

    /// Path relative to the enclosing space's root, as UTF-8.
    pub relative_path: String,

    /// Emoji icon shown in the sidebar, if the folder has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Short description of the folder, if the user provided one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ─── Document ───────────────────────────────────────────────────────

/// A markdown document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    /// Absolute path to the document on disk, as UTF-8.
    pub path: String,

    /// Path relative to the enclosing space's root, as UTF-8.
    pub relative_path: String,

    /// Title extracted from the first H1 heading, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// File size in bytes.
    pub size_bytes: u64,

    /// ISO 8601 timestamp of the most recent modification.
    pub modified_at: String,
}

// ─── SpaceRef ───────────────────────────────────────────────────────

/// A parsed reference to a space, optionally with a path within it.
///
/// Models the `@space/path/doc.md` syntax described in
/// `cstore-cli-redesign`. The parsing rules live in `cstore-refs`; this
/// crate only holds the value shape.
///
/// The path field is typed as [`PathBuf`] for the benefit of internal
/// Rust consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceRef {
    /// Space identifier portion of the ref. `None` means "current space".
    pub space: Option<String>,

    /// Path portion of the ref. `None` means "space root".
    pub path: Option<PathBuf>,
}

// ─── Errors ─────────────────────────────────────────────────────────

/// Unified error type for the ContextStore Rust core.
///
/// Per-crate error enums map to this through `From` impls at their own
/// boundaries.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Resource referenced by the operation does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// Input failed validation before any side effect was attempted.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// Underlying filesystem or system I/O failure.
    #[error("io: {0}")]
    Io(String),

    /// Git operation failed (gix error, network, protocol).
    #[error("git: {0}")]
    Git(String),

    /// Merge could not be completed automatically.
    #[error("merge conflict: {0}")]
    MergeConflict(String),

    /// Unexpected internal invariant violation.
    #[error("internal: {0}")]
    Internal(String),
}

/// `Result` alias defaulting to [`CoreError`].
pub type Result<T> = std::result::Result<T, CoreError>;

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_impl_all;

    // Public types must be Send + Sync + Clone so they can be shared
    // and cloned freely across the core's threads and crates.
    assert_impl_all!(Space: Send, Sync, Clone);
    assert_impl_all!(Folder: Send, Sync, Clone);
    assert_impl_all!(Document: Send, Sync, Clone);
    assert_impl_all!(SpaceRef: Send, Sync, Clone);
    assert_impl_all!(CoreError: Send, Sync);

    fn sample_space() -> Space {
        Space {
            name: "Planning".to_string(),
            path: "/Users/jane/Documents/Planning".to_string(),
            created_at: "2026-05-01T12:00:00Z".to_string(),
        }
    }

    fn sample_folder() -> Folder {
        Folder {
            name: "Engineering".to_string(),
            path: "/Users/jane/Documents/Planning/Engineering".to_string(),
            relative_path: "Engineering".to_string(),
            icon: Some("⚙️".to_string()),
            description: Some("Implementation docs and ADRs.".to_string()),
        }
    }

    fn sample_document() -> Document {
        Document {
            path: "/Users/jane/Documents/Planning/Engineering/adr-001.md".to_string(),
            relative_path: "Engineering/adr-001.md".to_string(),
            title: Some("ADR-001: Persistence layer".to_string()),
            size_bytes: 4_321,
            modified_at: "2026-05-15T09:30:00Z".to_string(),
        }
    }

    #[test]
    fn yaml_roundtrip_space() {
        let original = sample_space();
        let serialized = yaml_serde::to_string(&original).expect("yaml serialize");
        let restored: Space = yaml_serde::from_str(&serialized).expect("yaml deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn yaml_roundtrip_folder() {
        let original = sample_folder();
        let serialized = yaml_serde::to_string(&original).expect("yaml serialize");
        let restored: Folder = yaml_serde::from_str(&serialized).expect("yaml deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn yaml_roundtrip_document() {
        let original = sample_document();
        let serialized = yaml_serde::to_string(&original).expect("yaml serialize");
        let restored: Document = yaml_serde::from_str(&serialized).expect("yaml deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn json_roundtrip_space() {
        let original = sample_space();
        let serialized = serde_json::to_string(&original).expect("json serialize");
        let restored: Space = serde_json::from_str(&serialized).expect("json deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn json_roundtrip_folder() {
        let original = sample_folder();
        let serialized = serde_json::to_string(&original).expect("json serialize");
        let restored: Folder = serde_json::from_str(&serialized).expect("json deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn json_roundtrip_document() {
        let original = sample_document();
        let serialized = serde_json::to_string(&original).expect("json serialize");
        let restored: Document = serde_json::from_str(&serialized).expect("json deserialize");
        assert_eq!(original, restored);
    }

    #[test]
    fn core_error_display_contains_kind_and_message() {
        let err = CoreError::NotFound("space.yml".to_string());
        assert_eq!(err.to_string(), "not found: space.yml");
    }
}
