//! An app bundle's icon as Finder shows it (`NSWorkspace iconForFile:`),
//! drawn at a given pixel size and encoded as a PNG, for Settings' Editor and
//! Diff tool pickers ([`crate::views::tool_select`]).
//!
//! `NSWorkspace` and `NSImage` drawing are safe off the main thread, so the
//! pickers read icons on the background executor.

use std::path::Path;

/// `app`'s icon at `px` × `px` pixels, as PNG bytes; `None` when it can't
/// be drawn.
#[cfg(target_os = "macos")]
pub fn png(app: &Path, px: u32) -> Option<Vec<u8>> {
    use objc2::AllocAnyThread;
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace, NSGraphicsContext,
        NSImageInterpolation, NSWorkspace,
    };
    use objc2_foundation::{NSDictionary, NSPoint, NSRect, NSSize, NSString};

    let path = NSString::from_str(app.to_str()?);
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
    let side = px as isize;
    // SAFETY: a null planes pointer has the rep allocate its own buffer;
    // RGBA, 8 bits a sample, rows and pixel bits computed by AppKit.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            side,
            side,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
    rep.setSize(rect.size);
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    // Draw the icon's best representation for the size into the bitmap.
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    context.setImageInterpolation(NSImageInterpolation::High);
    icon.drawInRect_fromRect_operation_fraction(rect, NSRect::ZERO, NSCompositingOperation::SourceOver, 1.0);
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    // SAFETY: an empty properties dictionary.
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
    Some(data.to_vec())
}

#[cfg(not(target_os = "macos"))]
pub fn png(_app: &Path, _px: u32) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn an_app_icon_is_a_png_at_the_size_asked() {
        let png = png(Path::new("/System/Applications/Calculator.app"), 64).expect("Calculator draws");
        assert_eq!(&png[1..4], b"PNG");
        let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert_eq!((be(16), be(20)), (64, 64));
    }
}
