//! The AppKit half of [`super`]: widening a gpui window's dragged types and
//! answering the drags gpui's own methods would refuse.
//!
//! Coupled to gpui-pre 0.3.9's `gpui_macos::window` internals: the window
//! class is named `GPUIWindow`, it (not its view) is the drag destination, it
//! registers `NSFilenamesPboardType` only, and its content view's height is
//! what gpui flips drag locations by. If an upgrade renames the class,
//! [`install`] logs and leaves gpui alone (Finder drops keep working, the
//! rest stay refused).

use std::sync::OnceLock;

use gpui::{px, Pixels, Point, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{msg_send, sel};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::{NSArray, NSCopying, NSPoint, NSRect, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::pasteboard;

/// `NSDragOperation` (an `NSUInteger` bit set).
type DragOperation = usize;
const DRAG_NONE: DragOperation = 0;
const DRAG_COPY: DragOperation = 1;
const DRAG_GENERIC: DragOperation = 4;
const DRAG_MOVE: DragOperation = 16;

type DragFn = unsafe extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> DragOperation;
type PerformFn = unsafe extern "C" fn(&AnyObject, Sel, *mut AnyObject) -> Bool;

/// gpui's own implementations, called for a Finder drag.
struct Originals {
    entered: DragFn,
    updated: DragFn,
    perform: PerformFn,
}

static ORIGINALS: OnceLock<Originals> = OnceLock::new();

/// The gpui window class whose drag methods are replaced.
const WINDOW_CLASS: &std::ffi::CStr = c"GPUIWindow";

/// Replace `GPUIWindow`'s drag destination methods (once).
pub fn install() {
    let Some(class) = AnyClass::get(WINDOW_CLASS) else {
        tracing::warn!("image drops unavailable: no {WINDOW_CLASS:?} class in this gpui");
        return;
    };
    let entered = class.instance_method(sel!(draggingEntered:));
    let updated = class.instance_method(sel!(draggingUpdated:));
    let perform = class.instance_method(sel!(performDragOperation:));
    let (Some(entered), Some(updated), Some(perform)) = (entered, updated, perform) else {
        tracing::warn!("image drops unavailable: {WINDOW_CLASS:?} has no drag methods");
        return;
    };
    // SAFETY: gpui declared these methods with exactly these signatures
    // (`extern "C" fn(&Object, Sel, id) -> NSDragOperation` / `-> BOOL`).
    let originals = unsafe {
        Originals {
            entered: std::mem::transmute::<Imp, DragFn>(entered.implementation()),
            updated: std::mem::transmute::<Imp, DragFn>(updated.implementation()),
            perform: std::mem::transmute::<Imp, PerformFn>(perform.implementation()),
        }
    };
    if ORIGINALS.set(originals).is_err() {
        return;
    }
    // SAFETY: each replacement has the signature of the method it replaces,
    // and the originals are stored above before any can run.
    unsafe {
        entered.set_implementation(std::mem::transmute::<DragFn, Imp>(dragging_entered));
        updated.set_implementation(std::mem::transmute::<DragFn, Imp>(dragging_updated));
        perform.set_implementation(std::mem::transmute::<PerformFn, Imp>(perform_drag_operation));
    }
}

/// Register `window`'s `NSWindow` for the extra drag types (on top of the
/// `NSFilenamesPboardType` gpui registered).
pub fn accept_in(window: &Window) {
    let Some(ns_window) = ns_window_ptr(window) else {
        return;
    };
    #[allow(deprecated)]
    // SAFETY: an immutable AppKit static.
    let filenames = unsafe { objc2_app_kit::NSFilenamesPboardType }.copy();
    let mut types: Vec<Retained<NSString>> = vec![filenames];
    types.extend(pasteboard::extra_types().to_vec());
    let types = NSArray::from_retained_slice(&types);
    // SAFETY: a live NSWindow (gpui's window handle) and an NSArray of
    // pasteboard types.
    unsafe {
        let _: () = msg_send![ns_window, registerForDraggedTypes: &*types];
    }
}

/// The address of `window`'s `NSWindow`, if it has one (a test window
/// doesn't).
pub fn ns_window_of(window: &Window) -> Option<usize> {
    ns_window_ptr(window).map(|w| w as usize)
}

fn ns_window_ptr(window: &Window) -> Option<*mut AnyObject> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: gpui's handle is its live content NSView.
    let ns_window: *mut AnyObject = unsafe { msg_send![view, window] };
    (!ns_window.is_null()).then_some(ns_window)
}

/// The drag's pasteboard.
///
/// SAFETY: `info` is the `NSDraggingInfo` AppKit passed the method.
unsafe fn dragging_pasteboard<'a>(info: *mut AnyObject) -> Option<&'a NSPasteboard> {
    let pasteboard: *mut NSPasteboard = unsafe { msg_send![info, draggingPasteboard] };
    unsafe { pasteboard.as_ref() }
}

/// What to answer for a drag gpui doesn't handle: copy when the source
/// allows it (as gpui answers a Finder drag), else whatever it does allow.
///
/// SAFETY: as [`dragging_pasteboard`].
unsafe fn accepted_operation(info: *mut AnyObject) -> DragOperation {
    let Some(pasteboard) = (unsafe { dragging_pasteboard(info) }) else {
        return DRAG_NONE;
    };
    if !pasteboard::accepts(pasteboard) {
        return DRAG_NONE;
    }
    let mask: DragOperation = unsafe { msg_send![info, draggingSourceOperationMask] };
    [DRAG_COPY, DRAG_GENERIC, DRAG_MOVE]
        .into_iter()
        .find(|op| mask & op != 0)
        .unwrap_or(DRAG_NONE)
}

/// Whether gpui's own method takes this drag (a Finder drag carries its
/// filename list).
///
/// SAFETY: as [`dragging_pasteboard`].
unsafe fn gpui_takes(info: *mut AnyObject) -> bool {
    unsafe { dragging_pasteboard(info) }.is_some_and(pasteboard::has_filenames)
}

/// Run a hook body, refusing the drag instead of aborting the app if it
/// panics (these are `extern "C"`: a panic can't unwind into AppKit).
fn guarded<R>(refuse: R, body: impl FnOnce() -> R) -> R {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or_else(|_| {
        tracing::error!("drop refused: the drag handler panicked");
        refuse
    })
}

extern "C" fn dragging_entered(this: &AnyObject, sel: Sel, info: *mut AnyObject) -> DragOperation {
    // SAFETY: AppKit's arguments, forwarded unchanged.
    guarded(DRAG_NONE, || unsafe {
        match ORIGINALS.get() {
            Some(o) if gpui_takes(info) => (o.entered)(this, sel, info),
            _ => accepted_operation(info),
        }
    })
}

extern "C" fn dragging_updated(this: &AnyObject, sel: Sel, info: *mut AnyObject) -> DragOperation {
    // SAFETY: as `dragging_entered`.
    guarded(DRAG_NONE, || unsafe {
        match ORIGINALS.get() {
            Some(o) if gpui_takes(info) => (o.updated)(this, sel, info),
            _ => accepted_operation(info),
        }
    })
}

extern "C" fn perform_drag_operation(this: &AnyObject, sel: Sel, info: *mut AnyObject) -> Bool {
    // SAFETY: as `dragging_entered`.
    guarded(Bool::NO, || unsafe {
        if let Some(o) = ORIGINALS.get().filter(|_| gpui_takes(info)) {
            return (o.perform)(this, sel, info);
        }
        let Some(pasteboard) = dragging_pasteboard(info) else {
            return Bool::NO;
        };
        let ns_window = this as *const AnyObject as usize;
        let position = drop_position(this, info);
        let accepted = pasteboard::materialise(pasteboard, &pasteboard::drops_dir(), move |paths| {
            super::land(ns_window, position, paths)
        });
        Bool::new(accepted)
    })
}

/// The drop point in gpui's coordinates: AppKit's window coordinates have
/// their origin at the bottom left; gpui flips them by the content view's
/// height (`convert_mouse_position`).
///
/// SAFETY: `this` is the destination NSWindow, `info` its dragging info.
unsafe fn drop_position(this: &AnyObject, info: *mut AnyObject) -> Point<Pixels> {
    let location: NSPoint = unsafe { msg_send![info, draggingLocation] };
    let content: *mut AnyObject = unsafe { msg_send![this, contentView] };
    let height = if content.is_null() {
        0.0
    } else {
        let frame: NSRect = unsafe { msg_send![content, frame] };
        frame.size.height
    };
    gpui::point(px(location.x as f32), px((height - location.y) as f32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::rc::Retained;
    use objc2::runtime::NSObject;
    use objc2::{define_class, AllocAnyThread, DefinedClass};
    use objc2_foundation::{NSData, NSSize};

    /// What a fake `NSDraggingInfo` reports.
    struct InfoIvars {
        pasteboard: Retained<NSPasteboard>,
        mask: DragOperation,
        location: NSPoint,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "ShelbiTestDraggingInfo"]
        #[ivars = InfoIvars]
        struct FakeDraggingInfo;

        impl FakeDraggingInfo {
            #[unsafe(method(draggingPasteboard))]
            fn dragging_pasteboard(&self) -> *mut NSPasteboard {
                Retained::as_ptr(&self.ivars().pasteboard) as *mut NSPasteboard
            }

            #[unsafe(method(draggingSourceOperationMask))]
            fn source_mask(&self) -> DragOperation {
                self.ivars().mask
            }

            #[unsafe(method(draggingLocation))]
            fn location(&self) -> NSPoint {
                self.ivars().location
            }
        }
    );

    impl FakeDraggingInfo {
        fn new(pasteboard: Retained<NSPasteboard>, mask: DragOperation, location: NSPoint) -> Retained<Self> {
            let this = Self::alloc().set_ivars(InfoIvars { pasteboard, mask, location });
            unsafe { msg_send![super(this), init] }
        }
    }

    define_class!(
        /// A stand-in destination window: its own content view, 600pt tall.
        #[unsafe(super(NSObject))]
        #[name = "ShelbiTestDragWindow"]
        struct FakeWindow;

        impl FakeWindow {
            #[unsafe(method(contentView))]
            fn content_view(&self) -> *mut AnyObject {
                self as *const Self as *mut AnyObject
            }

            #[unsafe(method(frame))]
            fn frame(&self) -> NSRect {
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
            }
        }
    );

    impl FakeWindow {
        fn new() -> Retained<Self> {
            unsafe { msg_send![Self::alloc(), init] }
        }
    }

    /// A 1×1 PNG (as in `pasteboard::tests`).
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d, 0xb0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn pasteboard_with(ty: &str, data: &[u8]) -> Retained<NSPasteboard> {
        let pb = NSPasteboard::pasteboardWithUniqueName();
        pb.clearContents();
        assert!(pb.setData_forType(Some(&NSData::with_bytes(data)), &NSString::from_str(ty)));
        pb
    }

    fn info_ptr(info: &FakeDraggingInfo) -> *mut AnyObject {
        info as *const FakeDraggingInfo as *mut AnyObject
    }

    #[test]
    fn install_replaces_gpui_window_drag_methods() {
        install();
        let class = AnyClass::get(WINDOW_CLASS).expect("gpui's window class");
        let imp = |s: Sel| class.instance_method(s).unwrap().implementation() as usize;
        assert_eq!(imp(sel!(draggingEntered:)), dragging_entered as DragFn as usize);
        assert_eq!(imp(sel!(draggingUpdated:)), dragging_updated as DragFn as usize);
        assert_eq!(imp(sel!(performDragOperation:)), perform_drag_operation as PerformFn as usize);
        let o = ORIGINALS.get().unwrap();
        assert_ne!(o.entered as usize, dragging_entered as DragFn as usize, "kept gpui's own");
        // Idempotent: a second install keeps the originals (not our hooks).
        install();
        assert_ne!(ORIGINALS.get().unwrap().entered as usize, dragging_entered as DragFn as usize);
    }

    #[test]
    fn an_image_drag_is_accepted_as_a_copy_and_text_is_refused() {
        let window = FakeWindow::new();
        let this: &AnyObject = &window;
        let at = NSPoint::new(10.0, 10.0);
        let image = FakeDraggingInfo::new(pasteboard_with("public.png", PNG_1X1), DRAG_COPY | DRAG_MOVE, at);
        assert_eq!(dragging_entered(this, sel!(draggingEntered:), info_ptr(&image)), DRAG_COPY);
        assert_eq!(dragging_updated(this, sel!(draggingUpdated:), info_ptr(&image)), DRAG_COPY);
        // A source that only allows a move gets a move.
        let move_only = FakeDraggingInfo::new(pasteboard_with("public.png", PNG_1X1), DRAG_MOVE, at);
        assert_eq!(dragging_entered(this, sel!(draggingEntered:), info_ptr(&move_only)), DRAG_MOVE);
        let text = FakeDraggingInfo::new(pasteboard_with("public.utf8-plain-text", b"hi"), DRAG_COPY, at);
        assert_eq!(dragging_entered(this, sel!(draggingEntered:), info_ptr(&text)), DRAG_NONE);
        assert!(!perform_drag_operation(this, sel!(performDragOperation:), info_ptr(&text)).as_bool());
    }

    #[test]
    fn an_image_drop_is_written_and_landed_at_the_flipped_point() {
        let rx = super::super::test_receiver();
        let window = FakeWindow::new();
        let this: &AnyObject = &window;
        let info = FakeDraggingInfo::new(
            pasteboard_with("public.png", PNG_1X1),
            DRAG_COPY,
            NSPoint::new(120.0, 100.0),
        );
        assert!(perform_drag_operation(this, sel!(performDragOperation:), info_ptr(&info)).as_bool());
        let landed = rx.try_recv().expect("landed");
        assert_eq!(landed.ns_window, this as *const AnyObject as usize);
        // AppKit's y=100 from the bottom of a 600pt content view.
        assert_eq!(landed.position, gpui::point(px(120.0), px(500.0)));
        assert_eq!(landed.paths.len(), 1);
        let path = &landed.paths[0];
        assert!(path.starts_with(pasteboard::drops_dir()));
        assert_eq!(std::fs::read(path).unwrap(), PNG_1X1);
        let _ = std::fs::remove_file(path);
    }
}
