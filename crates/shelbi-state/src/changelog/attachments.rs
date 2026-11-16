//! Files carried by a changelog entry: images shown inline and any other file
//! (a log, a PDF, a zip, a video) listed under it.
//!
//! An entry's files live beside the entries, in
//! `changelog/assets/<entry-id>/`, one directory per entry. They are copied in
//! (the source is never moved) and recorded in the entry's frontmatter, each
//! path relative to the changelog directory:
//!
//! ```text
//! attachments:
//! - path: assets/2026-11-16-170000-board-scrolls/jobs-sheet.png
//!   name: jobs-sheet.png
//!   size: 48213
//!   kind: image
//! ```
//!
//! An image is also referenced from the description with ordinary Markdown,
//! `![alt](assets/<entry-id>/<name>)`, appended at its end, so the entry file
//! stays readable Markdown and the description says where the image sits. A
//! Markdown image the author writes by hand with a relative `assets/...` path
//! resolves the same way, whether or not the frontmatter lists it.
//!
//! A stored name is the source's file name made safe for a path and a
//! Markdown link (ASCII letters, digits, `.`, `_` and `-`; anything else
//! becomes `-`). A name already taken in the entry's directory gets a number:
//! `shot.png`, then `shot-2.png`, `shot-3.png`, …, so attaching never replaces
//! a file. The caller reports each file's final path ([`Attachment::path`],
//! and [`StoredAttachment::absolute`]).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shelbi_core::{Error, Result};

use super::{entry_id, find_in, parse_entry, read_entries_in, ChangelogEntry};
use crate::{atomic_write, ensure_dir};

/// The directory under the changelog directory holding each entry's files.
pub const ASSETS_DIR: &str = "assets";

/// The largest file an entry takes.
pub const MAX_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;

/// The image extensions an `--image` (and an inline image) can be.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// Whether a file is shown inline or listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    /// Shown inline, where the description references it.
    Image,
    /// Listed under the entry, opened with the system opener.
    File,
}

/// One file recorded in an entry's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Relative to the changelog directory: `assets/<entry-id>/<name>`.
    pub path: String,
    /// The stored file name.
    pub name: String,
    /// Bytes, when it was attached.
    #[serde(default)]
    pub size: u64,
    #[serde(default = "file_kind")]
    pub kind: AttachmentKind,
}

fn file_kind() -> AttachmentKind {
    AttachmentKind::File
}

impl Attachment {
    /// The file on this machine, under the changelog directory `dir`.
    pub fn absolute(&self, dir: &Path) -> PathBuf {
        dir.join(&self.path)
    }

    /// The Markdown that shows it inline: `![alt](assets/…)`.
    pub fn image_markdown(&self, alt: &str) -> String {
        format!("![{}]({})", escape_alt(alt), self.path)
    }
}

/// `1.2 MB`, `12 KB`, `512 B`: a size for a list.
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", (b / KB).max(1.0))
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.1} GB", b / (KB * KB * KB))
    }
}

/// A file to attach, as asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttachment {
    pub source: PathBuf,
    pub kind: AttachmentKind,
    /// An image's alt text (its caption). The file's stem when `None`.
    pub alt: Option<String>,
}

/// A [`NewAttachment`] that has been checked: it exists, is a file, is under
/// [`MAX_ATTACHMENT_BYTES`], and an image is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAttachment {
    source: PathBuf,
    kind: AttachmentKind,
    alt: String,
    size: u64,
}

/// Check every file before any is copied (and before `add` writes the entry),
/// so a bad one fails the command without leaving half its work behind.
pub fn prepare_attachments(new: &[NewAttachment]) -> Result<Vec<PreparedAttachment>> {
    new.iter().map(prepare).collect()
}

fn prepare(new: &NewAttachment) -> Result<PreparedAttachment> {
    let shown = new.source.display();
    let meta = fs::metadata(&new.source).map_err(|e| Error::Other(format!("{shown}: {e}")))?;
    if !meta.is_file() {
        return Err(Error::Other(format!("{shown}: not a file")));
    }
    if meta.len() > MAX_ATTACHMENT_BYTES {
        return Err(Error::Other(format!(
            "{shown} is {}; a changelog attachment can be at most {}",
            format_size(meta.len()),
            format_size(MAX_ATTACHMENT_BYTES)
        )));
    }
    if new.kind == AttachmentKind::Image && !is_image(&new.source) {
        return Err(Error::Other(format!(
            "{shown} isn't an image ({}); attach it with --attach instead",
            IMAGE_EXTENSIONS.join(", ")
        )));
    }
    let alt = new
        .alt
        .as_deref()
        .map(|a| a.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|a| !a.is_empty())
        .unwrap_or_else(|| {
            new.source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    Ok(PreparedAttachment {
        source: new.source.clone(),
        kind: new.kind,
        alt,
        size: meta.len(),
    })
}

/// Whether `path` is an image a client can draw: by extension, or by its
/// first bytes (PNG, JPEG, GIF, WebP) when the extension says nothing.
pub fn is_image(path: &Path) -> bool {
    if has_image_extension(&path.to_string_lossy()) {
        return true;
    }
    let mut head = [0u8; 12];
    let n = fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    image_magic(&head[..n])
}

/// Whether a path or URL names an image by its extension.
pub fn has_image_extension(path: &str) -> bool {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

fn image_magic(head: &[u8]) -> bool {
    head.starts_with(b"\x89PNG\r\n\x1a\n")
        || head.starts_with(&[0xff, 0xd8, 0xff])
        || head.starts_with(b"GIF87a")
        || head.starts_with(b"GIF89a")
        || (head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP")
}

/// One file attached by [`attach_files`]: what the frontmatter records, and
/// where the file now is on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAttachment {
    pub attachment: Attachment,
    pub absolute: PathBuf,
}

/// `changelog/assets/<entry-id>/` for the entry stored at `entry_path`.
pub fn entry_assets_dir(entry_path: &Path) -> PathBuf {
    let dir = entry_path.parent().unwrap_or(Path::new("."));
    let id = entry_id(entry_path).unwrap_or_default();
    dir.join(ASSETS_DIR).join(id)
}

/// Attach `files` (from [`prepare_attachments`]) to the entry stored at
/// `entry.path`: copy each into the entry's assets directory under a free
/// name, record it in the frontmatter, and append a Markdown reference for
/// each image to the description. Returns the entry as written and the files
/// in the order given.
///
/// The frontmatter is rewritten keeping every other key (including ones this
/// version doesn't know); the description keeps its text.
pub fn attach_files(
    entry: &ChangelogEntry,
    files: &[PreparedAttachment],
) -> Result<(ChangelogEntry, Vec<StoredAttachment>)> {
    // The entry as it is on disk now, not as the caller last read it: another
    // writer may have attached files since.
    let text = fs::read_to_string(&entry.path).map_err(|e| crate::annotate_io_error(&entry.path, e))?;
    let entry = parse_entry(&entry.id, &entry.path, &text)?;
    let assets = entry_assets_dir(&entry.path);
    let dir = entry.dir().to_path_buf();
    let id = entry_id(&entry.path).unwrap_or_default();
    // Names the frontmatter already holds count as taken even when the file is
    // gone, so two records never share a path.
    let mut taken: Vec<String> = entry.attachments.iter().map(|a| a.name.clone()).collect();
    let mut stored: Vec<StoredAttachment> = Vec::with_capacity(files.len());
    for file in files {
        let name = match copy_in(&assets, &file.source, &taken) {
            Ok(name) => name,
            Err(e) => {
                // Leave nothing behind that the frontmatter won't list.
                for s in &stored {
                    let _ = fs::remove_file(&s.absolute);
                }
                return Err(e);
            }
        };
        taken.push(name.clone());
        let attachment = Attachment {
            path: format!("{ASSETS_DIR}/{id}/{name}"),
            name,
            size: file.size,
            kind: file.kind,
        };
        stored.push(StoredAttachment {
            absolute: attachment.absolute(&dir),
            attachment,
        });
    }
    let images: Vec<String> = stored
        .iter()
        .zip(files)
        .filter(|(s, _)| s.attachment.kind == AttachmentKind::Image)
        .map(|(s, f)| s.attachment.image_markdown(&f.alt))
        .collect();
    let mut all = entry.attachments.clone();
    all.extend(stored.iter().map(|s| s.attachment.clone()));
    let written = rewrite_entry(&entry.path, &all, &images);
    if let Err(e) = written {
        for s in &stored {
            let _ = fs::remove_file(&s.absolute);
        }
        return Err(e);
    }
    let text = fs::read_to_string(&entry.path).map_err(|e| crate::annotate_io_error(&entry.path, e))?;
    let updated = parse_entry(&entry.id, &entry.path, &text)?;
    Ok((updated, stored))
}

/// [`attach_files`] to `project`'s entry `id` (or a unique prefix of one).
pub fn attach_to_entry(
    project: &str,
    id: &str,
    files: &[PreparedAttachment],
) -> Result<(ChangelogEntry, Vec<StoredAttachment>)> {
    attach_to_entry_in(&super::changelog_dir(project)?, id, files)
}

/// [`attach_to_entry`] under the changelog directory `dir`.
pub fn attach_to_entry_in(
    dir: &Path,
    id: &str,
    files: &[PreparedAttachment],
) -> Result<(ChangelogEntry, Vec<StoredAttachment>)> {
    let entry = find_in(read_entries_in(dir)?.entries, id)?;
    attach_files(&entry, files)
}

/// Copy `source` into `assets` under its safe name, numbered past any name in
/// `taken` or already in the directory. Never replaces a file: the name is
/// claimed with an exclusive create.
fn copy_in(assets: &Path, source: &Path, taken: &[String]) -> Result<String> {
    ensure_dir(assets)?;
    let base = safe_name(
        &source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], &base[i..]),
        _ => (base.as_str(), ""),
    };
    for n in 1..10_000 {
        let name = if n == 1 {
            base.clone()
        } else {
            format!("{stem}-{n}{ext}")
        };
        if taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let path = assets.join(&name);
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut out) => {
                let copied = fs::File::open(source)
                    .and_then(|mut src| std::io::copy(&mut src, &mut out))
                    .and_then(|_| out.flush());
                if let Err(e) = copied {
                    let _ = fs::remove_file(&path);
                    return Err(Error::Other(format!("{}: {e}", source.display())));
                }
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(crate::annotate_io_error(&path, e).into()),
        }
    }
    Err(Error::Other(format!(
        "no free name for {base} in {}",
        assets.display()
    )))
}

/// A file name safe in a path and a Markdown link: ASCII letters, digits,
/// `.`, `_` and `-`, with any other run of characters as one `-`. A dotfile
/// loses its dot (`.env` → `env`), a name with nothing left before its
/// extension is `file` (`ü.png` → `file.png`), and an empty one `file`.
pub fn safe_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    // `a-.png` reads better as `a.png`.
    let out = out.trim_matches('-').replace("-.", ".");
    match out.strip_prefix('.') {
        Some(rest) if name.starts_with('.') && !rest.is_empty() && !rest.contains('.') => rest.to_string(),
        Some(_) => format!("file{}", out.trim_end_matches('.')),
        None if out.is_empty() => "file".to_string(),
        None => out,
    }
}

/// Alt text that can't close the `[…]` it sits in.
fn escape_alt(alt: &str) -> String {
    let mut out = String::with_capacity(alt.len());
    for c in alt.chars() {
        if matches!(c, '\\' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Rewrite the entry file at `path` with `attachments` in its frontmatter and
/// `images` appended to its description, each its own paragraph. Every other
/// frontmatter key is kept as it was.
fn rewrite_entry(path: &Path, attachments: &[Attachment], images: &[String]) -> Result<()> {
    let text = fs::read_to_string(path).map_err(|e| crate::annotate_io_error(path, e))?;
    let (front, body) = crate::split_frontmatter(&text)
        .ok_or_else(|| Error::Other(format!("{}: missing frontmatter", path.display())))?;
    let mut value: yaml_serde::Value = yaml_serde::from_str(front)?;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| Error::Other(format!("{}: frontmatter isn't a mapping", path.display())))?;
    let key = yaml_serde::Value::String("attachments".into());
    if attachments.is_empty() {
        map.remove(&key);
    } else {
        map.insert(key, yaml_serde::to_value(attachments)?);
    }
    let mut body = body.trim_end().to_string();
    for image in images {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(image);
    }
    let out = crate::render_frontmatter(&value, &body)?;
    atomic_write(path, out.as_bytes())?;
    Ok(())
}

/// The attachments a reader would miss in the rendered description: every
/// file, and each image the description doesn't reference (its reference was
/// edited out, or it was recorded by hand).
pub fn listed_attachments<'a>(attachments: &'a [Attachment], body: &str) -> Vec<&'a Attachment> {
    attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::File || !body.contains(&a.path))
        .collect()
}
