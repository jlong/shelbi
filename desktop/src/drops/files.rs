//! Where drops and pasted images become files, on every platform.
//!
//! A terminal program takes a file by its path, so an image that arrives as
//! data (a screenshot on the clipboard, a promised or dragged image on macOS)
//! is written into [`drops_dir`] first and its path is pasted, as a dropped
//! file's is (`TerminalPane::drop_paths`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat};

/// The per-app directory drops are written to. macOS:
/// `~/Library/Caches/<bundle id>/drops` inside the bundled app (see
/// `pasteboard::drops_dir`). Linux: `$XDG_CACHE_HOME/shelbi-desktop/drops`
/// (`~/.cache/shelbi-desktop/drops` by default), falling back to the
/// temporary directory with no home.
pub fn drops_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        super::pasteboard::drops_dir()
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg_drops_dir(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
    }
}

/// [`drops_dir`] on Linux from `XDG_CACHE_HOME` and `HOME`. An empty or
/// relative `XDG_CACHE_HOME` is ignored, as the XDG spec asks.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn xdg_drops_dir(cache_home: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    let cache = cache_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| PathBuf::from(h).join(".cache")).filter(|p| p.is_absolute()))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("shelbi-desktop").join("drops")
}

/// The image a clipboard item holds, if any (its first).
pub fn clipboard_image(item: &ClipboardItem) -> Option<Image> {
    item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) if !image.bytes().is_empty() => Some(image.clone()),
        _ => None,
    })
}

/// The file extension an image of `format` is written with.
pub fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Ico => "ico",
        ImageFormat::Pnm => "pnm",
    }
}

/// Write `image` into `dir` (created when missing) as a new file, its bytes
/// as they came, and return its path.
pub fn write_image(image: &Image, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = fresh_path(dir, extension(image.format()));
    std::fs::write(&path, image.bytes())?;
    Ok(path)
}

/// A path in `dir` that does not exist yet: `drop-<local time>-<n>` plus
/// `.ext` (no extension when `ext` is empty). No spaces, so it pastes
/// without escapes.
pub fn fresh_path(dir: &Path, ext: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    loop {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut name = format!("drop-{stamp}-{n}");
        if !ext.is_empty() {
            name.push('.');
            name.push_str(ext);
        }
        let path = dir.join(name);
        if !path.exists() {
            return path;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_drops_live_under_the_xdg_cache() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            xdg_drops_dir(os("/x/cache"), os("/home/u")),
            PathBuf::from("/x/cache/shelbi-desktop/drops")
        );
        assert_eq!(
            xdg_drops_dir(None, os("/home/u")),
            PathBuf::from("/home/u/.cache/shelbi-desktop/drops")
        );
        // A relative XDG_CACHE_HOME is ignored.
        assert_eq!(
            xdg_drops_dir(os("rel"), os("/home/u")),
            PathBuf::from("/home/u/.cache/shelbi-desktop/drops")
        );
        assert!(xdg_drops_dir(None, None).ends_with("shelbi-desktop/drops"));
    }

    #[test]
    fn a_clipboard_image_is_written_as_it_came() {
        let dir = std::env::temp_dir().join(format!("shelbi-drops-test-{}", std::process::id()));
        let png = vec![0x89, b'P', b'N', b'G', 1, 2, 3];
        let item = ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, png.clone()));
        let image = clipboard_image(&item).expect("an image");
        let path = write_image(&image, &dir).unwrap();
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(std::fs::read(&path).unwrap(), png);
        let jpeg = write_image(&Image::from_bytes(ImageFormat::Jpeg, vec![1]), &dir).unwrap();
        assert_ne!(jpeg, path);
        assert_eq!(jpeg.extension().and_then(|e| e.to_str()), Some("jpg"));
        let _ = std::fs::remove_dir_all(&dir);
        // Text holds no image.
        assert!(clipboard_image(&ClipboardItem::new_string("hi".into())).is_none());
    }
}
