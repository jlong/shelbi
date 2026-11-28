// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/lib.rs), MIT in Shelbi.

//! ContextStore filesystem layer.
//!
//! Owns the on-disk layout: settings YAML schemas
//! (`.contextstore/settings.yml`, `.contextstore/settings.local.yml`),
//! project/folder/document discovery, and CRUD for the files inside a space.
//!
//! Shelbi drops ContextStore's `agents.yml`, `.mcp.yml`, app pointer file
//! (`appconfig`) and keyring hash (`hash`) support.
//!
//! No async API — every operation is synchronous. Callers that want to
//! run filesystem work off the main thread run it on a blocking thread.

#![deny(missing_docs)]

pub mod defaults;
pub mod detect;
pub mod document;
pub mod folder;
pub mod paths;
pub mod settings;
pub mod space;
pub mod tree;

/// Errors raised by `cstore-fs`. Each variant maps to a
/// [`crate::domain::CoreError`] at the FFI boundary.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// A settings YAML file failed to parse.
    #[error("settings parse error in {path}: {source}")]
    SettingsParse {
        /// Path of the file whose YAML could not be decoded.
        path: String,
        /// Underlying `yaml_serde` error.
        #[source]
        source: yaml_serde::Error,
    },

    /// A settings struct failed to encode back to YAML.
    #[error("settings encode error: {0}")]
    SettingsEncode(#[source] yaml_serde::Error),

    /// A caller-supplied path pointed outside the space it was resolved
    /// against — an absolute path, or one containing `..`.
    ///
    /// Raised by [`paths::resolve_in_space`], which every surface accepting a
    /// path from outside the app is expected to go through.
    #[error("{reason}: {path}")]
    PathOutsideSpace {
        /// The path exactly as the caller supplied it.
        path: String,
        /// Which rule it broke, phrased for whoever typed it.
        reason: &'static str,
    },

    /// A filesystem I/O error.
    #[error("io error at {path}: {source}")]
    Io {
        /// Path of the file or directory that caused the error.
        path: String,
        /// Underlying `std::io::Error`.
        #[source]
        source: std::io::Error,
    },
}

impl From<FsError> for crate::domain::CoreError {
    fn from(err: FsError) -> Self {
        match err {
            FsError::SettingsParse { path, source } => {
                crate::domain::CoreError::InvalidInput(format!("{path}: {source}"))
            }
            FsError::SettingsEncode(source) => {
                crate::domain::CoreError::Internal(format!("settings encode failed: {source}"))
            }
            FsError::PathOutsideSpace { path, reason } => {
                crate::domain::CoreError::InvalidInput(format!("{reason}: {path}"))
            }
            FsError::Io { path, source } => {
                if source.kind() == std::io::ErrorKind::NotFound {
                    crate::domain::CoreError::NotFound(path)
                } else {
                    crate::domain::CoreError::Io(format!("{path}: {source}"))
                }
            }
        }
    }
}

/// `Result` alias defaulting to [`FsError`].
pub type Result<T> = std::result::Result<T, FsError>;
