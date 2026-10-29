//! Editing a task's title and Markdown body in place, WYSIWYG, toolkit-free
//! (`dt-task-wysiwyg-editor`).
//!
//! The Markdown source is the model. [`blocks::Doc`] lays it out as rows of
//! formatted text with the syntax hidden, and [`Editor`] edits the source
//! through that view: the caret moves through the text the user sees,
//! typing lands where it's shown, and each command (Enter, Backspace, bold,
//! a heading, a checkbox) is one edit of the source and one undo step. Text
//! an edit doesn't touch keeps its exact bytes, so opening, reading or
//! clicking into a task never changes it, and any Markdown the editor doesn't
//! render (tables, HTML, footnotes) shows as the source it is rather than
//! being dropped or rewritten.
//!
//! A client draws the rows ([`blocks::Row`]: display text, styles, the
//! source each display offset stands for), turns pointer and keys into these
//! operations, and hands the text to the issue's draft
//! ([`crate::issue_edit`]) to save.

pub mod blocks;
pub mod editor;
mod history;
pub mod inline;

pub use blocks::{Chunk, Doc, Gap, Item, Marker, Row, RowKind};
pub use editor::{
    next_boundary, prev_boundary, word_left, word_right, BlockKind, Editor, LinkTarget,
};
pub use inline::{MarkKind, Style};

#[cfg(test)]
mod tests;
