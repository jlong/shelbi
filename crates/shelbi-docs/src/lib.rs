//! The docs' storage library.
//!
//! A Shelbi docs and a ContextStore space are the same folder: this crate
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
//! Shelbi's own modules sit beside the copied ones:
//!
//! - [`git`]: the git CLI, run non-interactively under a deadline.
//! - [`sync`]: the docs' git sync (worktree setup, commit, pull and merge,
//!   conflict hold, push), over the git CLI rather than libgit2.
//! - [`service`]: the hub's per-project docs service: the single writer of
//!   the docs' files, its filesystem watcher, and the commit and pull clocks.
//!
//! Everything is synchronous (std threads, no async runtime); the CLI, the
//! hub protocol and the UIs live elsewhere.

pub mod doc_rename;
pub mod domain;
pub mod frontmatter;
pub mod fs;
pub mod git;
pub mod ignore;
pub mod markdown;
pub mod merge;
pub mod model;
pub mod search;
pub mod service;
pub mod sync;
