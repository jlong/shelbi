//! Turn a drag's pasteboard into files a terminal program can open.
//!
//! First match wins, per drag:
//!
//! 1. **File URLs** (`public.file-url`): the files themselves. A screenshot
//!    thumbnail's drag resolves to its `…/TemporaryItems/NSIRD_screencaptureui_…`
//!    file this way, which is what Ghostty and Terminal.app paste.
//! 2. **Image data** (`public.png`, `public.tiff`, `public.jpeg`), one image
//!    per pasteboard item: written as a PNG into [`drops_dir`]. A browser,
//!    Preview, Mail or Slack image.
//! 3. **File promises** (`NSFilePromiseReceiver`): received into a fresh
//!    directory under [`drops_dir`]; the paths are handed on only once every
//!    promised file has arrived and exists.
//!
//! (`NSFilenamesPboardType`, a Finder drag, never gets here: gpui takes it.)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::ClassType;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSFilePromiseReceiver, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeTIFF,
    NSPasteboardURLReadingFileURLsOnlyKey,
};
use objc2_foundation::{
    NSArray, NSBundle, NSCopying, NSData, NSDictionary, NSError, NSNumber, NSOperationQueue, NSString,
    NSTemporaryDirectory, NSURL,
};

/// The pasteboard type for JPEG data (AppKit has no constant for it).
const JPEG: &str = "public.jpeg";

/// The image formats read as raw data, in preference order (PNG first: it
/// needs no conversion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Tiff,
    Jpeg,
}

impl ImageKind {
    const ALL: [ImageKind; 3] = [ImageKind::Png, ImageKind::Tiff, ImageKind::Jpeg];

    fn pasteboard_type(self) -> Retained<NSString> {
        match self {
            // SAFETY: AppKit's pasteboard type constants are immutable
            // statics.
            ImageKind::Png => unsafe { NSPasteboardTypePNG }.copy(),
            ImageKind::Tiff => unsafe { NSPasteboardTypeTIFF }.copy(),
            ImageKind::Jpeg => NSString::from_str(JPEG),
        }
    }
}

/// The pasteboard types (beyond gpui's `NSFilenamesPboardType`) a terminal
/// window registers for: file promises, image data and file URLs.
pub fn extra_types() -> Retained<NSArray<NSString>> {
    let mut types: Vec<Retained<NSString>> =
        NSFilePromiseReceiver::readableDraggedTypes().to_vec();
    types.extend(ImageKind::ALL.map(ImageKind::pasteboard_type));
    // SAFETY: an immutable AppKit static.
    types.push(unsafe { NSPasteboardTypeFileURL }.copy());
    NSArray::from_retained_slice(&types)
}

/// Whether `pasteboard` carries anything [`materialise`] can turn into a file.
pub fn accepts(pasteboard: &NSPasteboard) -> bool {
    pasteboard.availableTypeFromArray(&extra_types()).is_some()
}

/// Whether `pasteboard` carries Finder's filename list, which gpui reads.
pub fn has_filenames(pasteboard: &NSPasteboard) -> bool {
    #[allow(deprecated)]
    // SAFETY: an immutable AppKit static.
    let filenames = unsafe { objc2_app_kit::NSFilenamesPboardType };
    pasteboard
        .availableTypeFromArray(&NSArray::from_slice(&[filenames]))
        .is_some()
}

/// Read `pasteboard`'s drop into files and hand their paths to `done`, in
/// drop order: at once for file URLs and image data, from a background queue
/// once every promised file exists for a promise. Returns whether there was
/// anything to drop (`done` is not called when there wasn't).
pub fn materialise(
    pasteboard: &NSPasteboard,
    dir: &Path,
    done: impl FnOnce(Vec<PathBuf>) + Send + 'static,
) -> bool {
    let urls = file_urls(pasteboard);
    if !urls.is_empty() {
        done(urls);
        return true;
    }
    let images = image_data(pasteboard);
    if !images.is_empty() {
        let paths: Vec<PathBuf> = images
            .into_iter()
            .filter_map(|(data, kind)| match write_image(&data, kind, dir) {
                Ok(path) => Some(path),
                Err(e) => {
                    tracing::warn!("dropped image not saved: {e}");
                    None
                }
            })
            .collect();
        if paths.is_empty() {
            return false;
        }
        done(paths);
        return true;
    }
    receive_promises(pasteboard, dir, done)
}

/// The file URLs on `pasteboard`, as paths.
fn file_urls(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    let classes: Retained<NSArray<AnyClass>> = class_array(NSURL::class());
    // SAFETY: an immutable AppKit static.
    let key = unsafe { NSPasteboardURLReadingFileURLsOnlyKey };
    let yes = NSNumber::numberWithBool(true);
    let options: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&[key], &[yes.as_ref() as &AnyObject]);
    // SAFETY: the class array holds NSURL, the options dictionary a key
    // AppKit documents for this call.
    let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, Some(&options)) };
    let Some(objects) = objects else {
        return Vec::new();
    };
    objects
        .iter()
        .filter_map(|object| object.downcast::<NSURL>().ok())
        .filter_map(|url| url.filePathURL().and_then(|u| u.path()))
        .map(|path| PathBuf::from(path.to_string()))
        .collect()
}

/// Each pasteboard item's image, in its preferred format.
fn image_data(pasteboard: &NSPasteboard) -> Vec<(Vec<u8>, ImageKind)> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            ImageKind::ALL.into_iter().find_map(|kind| {
                item.dataForType(&kind.pasteboard_type())
                    .map(|data| (data.to_vec(), kind))
            })
        })
        .filter(|(data, _)| !data.is_empty())
        .collect()
}

/// Write image `data` of `kind` into `dir` as a new PNG and return its path
/// (TIFF and JPEG are converted; PNG is written as it came).
pub fn write_image(data: &[u8], kind: ImageKind, dir: &Path) -> std::io::Result<PathBuf> {
    let png = match kind {
        ImageKind::Png => data.to_vec(),
        ImageKind::Tiff | ImageKind::Jpeg => to_png(data).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "not a readable image")
        })?,
    };
    std::fs::create_dir_all(dir)?;
    let path = fresh_path(dir, "png");
    std::fs::write(&path, png)?;
    Ok(path)
}

/// Re-encode an image AppKit can read as PNG.
fn to_png(data: &[u8]) -> Option<Vec<u8>> {
    let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(data))?;
    // SAFETY: an empty properties dictionary is valid for every file type.
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(png.to_vec())
}

/// Receive every file promise on `pasteboard` into a fresh directory under
/// `dir`, then call `done` with the files that arrived.
fn receive_promises(
    pasteboard: &NSPasteboard,
    dir: &Path,
    done: impl FnOnce(Vec<PathBuf>) + Send + 'static,
) -> bool {
    let classes: Retained<NSArray<AnyClass>> = class_array(NSFilePromiseReceiver::class());
    // SAFETY: the class array holds NSFilePromiseReceiver; no options.
    let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) };
    let receivers: Vec<Retained<NSFilePromiseReceiver>> = objects
        .map(|objects| {
            objects
                .iter()
                .filter_map(|o| o.downcast::<NSFilePromiseReceiver>().ok())
                .collect()
        })
        .unwrap_or_default();
    if receivers.is_empty() {
        return false;
    }
    let destination = fresh_path(dir, "");
    if let Err(e) = std::fs::create_dir_all(&destination) {
        tracing::warn!("dropped file not received: {e}");
        return false;
    }
    // A receiver that names no files yet (`fileNames` is nil until the
    // receive for a modern promise) still delivers at least one.
    let expected: Vec<usize> = receivers.iter().map(|r| promised_count(r).max(1)).collect();
    let batch = PromiseBatch::new(expected, done);
    let dest_url = NSURL::fileURLWithPath_isDirectory(
        &NSString::from_str(&destination.to_string_lossy()),
        true,
    );
    let queue = promise_queue();
    for (index, receiver) in receivers.iter().enumerate() {
        let batch = batch.clone();
        let reader = RcBlock::new(move |url: std::ptr::NonNull<NSURL>, error: *mut NSError| {
            // SAFETY: AppKit passes a valid URL for the duration of the call.
            let url = unsafe { url.as_ref() };
            let path = if error.is_null() {
                url.path().map(|p| PathBuf::from(p.to_string()))
            } else {
                // SAFETY: non-null, valid for the call.
                let error = unsafe { &*error };
                tracing::warn!("promised file not received: {}", error.localizedDescription());
                None
            };
            batch.arrive(index, path);
        });
        // SAFETY: an empty options dictionary; the queue outlives the
        // receive (it lives for the app's lifetime).
        unsafe {
            receiver.receivePromisedFilesAtDestination_options_operationQueue_reader(
                &dest_url,
                &NSDictionary::new(),
                &queue,
                &reader,
            );
        }
    }
    true
}

/// How many files `receiver` names. Read untyped: AppKit returns nil before
/// the files are received, which objc2's non-optional binding would panic on.
fn promised_count(receiver: &NSFilePromiseReceiver) -> usize {
    // SAFETY: `fileNames` takes no arguments and returns an NSArray or nil.
    let names: Option<Retained<NSArray<NSString>>> = unsafe { objc2::msg_send![receiver, fileNames] };
    names.map_or(0, |names| names.count())
}

/// The background queue promised files are written on: one for the app's
/// lifetime, so it outlives every receive.
fn promise_queue() -> Retained<NSOperationQueue> {
    thread_local! {
        static QUEUE: Retained<NSOperationQueue> = NSOperationQueue::new();
    }
    QUEUE.with(|q| q.clone())
}

/// Collects a drop's promised files as they arrive (on any thread, in any
/// order) and calls `done` once, with them in receiver order, after the last
/// one. A file that failed, or that does not exist, is left out.
pub struct PromiseBatch {
    state: Mutex<BatchState>,
}

struct BatchState {
    slots: Vec<Vec<PathBuf>>,
    remaining: usize,
    done: Option<Box<dyn FnOnce(Vec<PathBuf>) + Send>>,
}

impl PromiseBatch {
    /// A batch expecting `expected[i]` files from receiver `i`.
    pub fn new(expected: Vec<usize>, done: impl FnOnce(Vec<PathBuf>) + Send + 'static) -> Arc<Self> {
        let remaining = expected.iter().sum();
        Arc::new(Self {
            state: Mutex::new(BatchState {
                slots: vec![Vec::new(); expected.len()],
                remaining,
                done: Some(Box::new(done)),
            }),
        })
    }

    /// Receiver `index` reported one file (`None`: it failed).
    pub fn arrive(&self, index: usize, path: Option<PathBuf>) {
        let finished = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(path) = path {
                if let Some(slot) = state.slots.get_mut(index) {
                    slot.push(path);
                }
            }
            state.remaining = state.remaining.saturating_sub(1);
            if state.remaining > 0 {
                return;
            }
            let paths: Vec<PathBuf> = state
                .slots
                .drain(..)
                .flatten()
                .filter(|p| p.exists())
                .collect();
            state.done.take().map(|done| (done, paths))
        };
        if let Some((done, paths)) = finished {
            if !paths.is_empty() {
                done(paths);
            }
        }
    }
}

/// The per-app directory drops are written to:
/// `~/Library/Caches/<bundle id>/drops` inside the bundled app, a
/// `shelbi-desktop/drops` directory under `NSTemporaryDirectory()` for an
/// unbundled build (no bundle id).
pub fn drops_dir() -> PathBuf {
    let bundle_id = NSBundle::mainBundle().bundleIdentifier().map(|s| s.to_string());
    match (bundle_id, std::env::var_os("HOME")) {
        (Some(id), Some(home)) if !id.is_empty() => {
            PathBuf::from(home).join("Library/Caches").join(id).join("drops")
        }
        _ => PathBuf::from(NSTemporaryDirectory().to_string()).join("shelbi-desktop/drops"),
    }
}

/// A path in `dir` that does not exist yet: `drop-<local time>-<n>` plus
/// `.ext` (no extension when `ext` is empty). No spaces, so it pastes
/// without escapes.
fn fresh_path(dir: &Path, ext: &str) -> PathBuf {
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

/// A one-class `NSArray`, the shape `readObjectsForClasses:` takes.
fn class_array(class: &'static AnyClass) -> Retained<NSArray<AnyClass>> {
    NSArray::from_slice(&[class])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1×1 opaque red PNG.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("shelbi-desktop-drops-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn tiff_of(png: &[u8]) -> Vec<u8> {
        let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(png)).expect("png reads");
        rep.TIFFRepresentation().expect("tiff encodes").to_vec()
    }

    /// A private pasteboard holding one item per `(type, data)` list.
    fn pasteboard(items: &[&[(&str, &[u8])]]) -> Retained<NSPasteboard> {
        use objc2_app_kit::{NSPasteboardItem, NSPasteboardWriting};
        use objc2::runtime::ProtocolObject;
        let pb = NSPasteboard::pasteboardWithUniqueName();
        pb.clearContents();
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
            .iter()
            .map(|entries| {
                let item = NSPasteboardItem::new();
                for (ty, data) in entries.iter() {
                    assert!(item.setData_forType(&NSData::with_bytes(data), &NSString::from_str(ty)));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        assert!(pb.writeObjects(&NSArray::from_retained_slice(&items)));
        pb
    }

    fn run(pb: &NSPasteboard, dir: &Path) -> Option<Vec<PathBuf>> {
        let got = Arc::new(Mutex::new(None));
        let sink = got.clone();
        let accepted = materialise(pb, dir, move |paths| *sink.lock().unwrap() = Some(paths));
        let out = got.lock().unwrap().take();
        assert_eq!(accepted, out.is_some());
        // SAFETY: a private pasteboard this test made and is done with.
        let _: () = unsafe { objc2::msg_send![pb, releaseGlobally] };
        out
    }

    #[test]
    fn png_data_is_written_as_is() {
        let dir = temp_dir("png");
        let path = write_image(PNG_1X1, ImageKind::Png, &dir).unwrap();
        assert_eq!(path.parent(), Some(dir.as_path()));
        assert_eq!(path.extension().unwrap(), "png");
        assert_eq!(std::fs::read(&path).unwrap(), PNG_1X1);
        // A second image gets its own file.
        let again = write_image(PNG_1X1, ImageKind::Png, &dir).unwrap();
        assert_ne!(again, path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tiff_data_is_converted_to_png() {
        let dir = temp_dir("tiff");
        let tiff = tiff_of(PNG_1X1);
        assert!(!tiff.starts_with(PNG_MAGIC));
        let path = write_image(&tiff, ImageKind::Tiff, &dir).unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(PNG_MAGIC));
        assert!(write_image(b"not an image", ImageKind::Jpeg, &dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_items_on_a_pasteboard_become_pngs_in_order() {
        let dir = temp_dir("items");
        let tiff = tiff_of(PNG_1X1);
        let pb = pasteboard(&[
            &[("public.tiff", &tiff)],
            // PNG preferred over TIFF on the same item.
            &[("public.tiff", &tiff), ("public.png", PNG_1X1)],
        ]);
        assert!(accepts(&pb));
        assert!(!has_filenames(&pb));
        let paths = run(&pb, &dir).expect("dropped");
        assert_eq!(paths.len(), 2);
        for path in &paths {
            assert!(path.starts_with(&dir));
            assert!(std::fs::read(path).unwrap().starts_with(PNG_MAGIC));
        }
        assert_eq!(std::fs::read(&paths[1]).unwrap(), PNG_1X1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_urls_win_over_image_data() {
        let dir = temp_dir("urls");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Screenshot 2026-10-14 at 9.05 PM.png");
        std::fs::write(&file, PNG_1X1).unwrap();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let url_string = url.absoluteString().unwrap().to_string();
        let pb = pasteboard(&[&[
            ("public.file-url", url_string.as_bytes()),
            ("public.png", PNG_1X1),
        ]]);
        assert!(accepts(&pb));
        assert_eq!(run(&pb, &dir.join("out")).unwrap(), vec![file]);
        assert!(!dir.join("out").exists(), "no image written for a file URL");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pasteboard_with_nothing_droppable_is_refused() {
        let dir = temp_dir("none");
        let pb = pasteboard(&[&[("public.utf8-plain-text", b"hello")]]);
        assert!(!accepts(&pb));
        assert_eq!(run(&pb, &dir), None);
        assert!(!dir.exists());
    }

    #[test]
    fn promise_batch_waits_for_every_file_and_keeps_receiver_order() {
        let dir = temp_dir("promise");
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.png");
        let b1 = dir.join("b1.png");
        let b2 = dir.join("b2.png");
        for p in [&a, &b1, &b2] {
            std::fs::write(p, PNG_1X1).unwrap();
        }
        let got = Arc::new(Mutex::new(None));
        let sink = got.clone();
        let batch = PromiseBatch::new(vec![1, 2], move |paths| *sink.lock().unwrap() = Some(paths));
        // Receiver 1's files land first, from another thread.
        let b = batch.clone();
        let (b1c, b2c) = (b1.clone(), b2.clone());
        std::thread::spawn(move || {
            b.arrive(1, Some(b2c));
            b.arrive(1, Some(b1c));
        })
        .join()
        .unwrap();
        assert!(got.lock().unwrap().is_none(), "nothing until the last file");
        batch.arrive(0, Some(a.clone()));
        assert_eq!(got.lock().unwrap().take(), Some(vec![a, b2, b1]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promise_batch_leaves_out_failed_and_missing_files() {
        let dir = temp_dir("promise-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let here = dir.join("here.png");
        std::fs::write(&here, PNG_1X1).unwrap();
        let got = Arc::new(Mutex::new(None));
        let sink = got.clone();
        let batch = PromiseBatch::new(vec![1, 1, 1], move |paths| *sink.lock().unwrap() = Some(paths));
        batch.arrive(0, None);
        batch.arrive(1, Some(dir.join("never-written.png")));
        batch.arrive(2, Some(here.clone()));
        assert_eq!(got.lock().unwrap().take(), Some(vec![here]));

        // Every file failed: nothing is pasted at all.
        let called = Arc::new(Mutex::new(false));
        let flag = called.clone();
        let batch = PromiseBatch::new(vec![1], move |_| *flag.lock().unwrap() = true);
        batch.arrive(0, None);
        assert!(!*called.lock().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drops_dir_is_per_app() {
        // `cargo test` is unbundled: the temp-directory fallback.
        let dir = drops_dir();
        assert!(dir.ends_with("shelbi-desktop/drops") || dir.ends_with("dev.shelbi.desktop/drops"));
    }
}
