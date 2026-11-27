//! The wiki's storage library.
//!
//! A Shelbi wiki and a ContextStore space are the same folder: this crate
//! keeps ContextStore's on-disk format exactly (`.contextstore/settings.yml`,
//! `.contextstore/settings.local.yml`, `.csignore`, per-folder settings,
//! Markdown documents identified by path). Most of it is copied from
//! ContextStore's Rust core (`32pixelsCo/ContextStore`, branch
//! `rewrite/rust-core`) and is MIT in Shelbi; each copied file starts with a
//! header naming its source path and commit.
//!
//! - [`domain`]: Space, Folder and Document types, and [`domain::CoreError`].
//! - [`fs`]: settings YAML, folder and document CRUD, the lazy folder tree,
//!   atomic writes, trash.
//! - [`ignore`]: `.csignore` matching.
//! - [`markdown`]: sections by heading, frontmatter, GFM tasks, links.
//! - [`merge`]: three-way Markdown merge.
//! - [`search`]: live grep, substring or regex.
//! - [`model`], [`doc_rename`], [`frontmatter`]: logic ported from
//!   ContextStore's UI: document titles and sort order, renaming a file after
//!   its H1, and frontmatter as an ordered property list.
//!
//! Everything is synchronous and does no networking; hub, CLI and UI
//! behavior live elsewhere.

pub mod doc_rename;
pub mod domain;
pub mod frontmatter;
pub mod fs;
pub mod ignore;
pub mod markdown;
pub mod merge;
pub mod model;
pub mod search;
