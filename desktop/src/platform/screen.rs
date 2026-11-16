//! Screen geometry for a tab dragged out of its window (`tab-tearoff`).
//!
//! gpui gives a window's bounds relative to the display it is on, so two
//! windows on two displays can't be compared through it. A tab drag that
//! crosses windows needs one space for all of them: "screen space" here is
//! AppKit's global space flipped to grow down, its origin the primary
//! display's top-left (CoreGraphics' global display space). Everything the
//! drag compares (the pointer, each window's content, each display) is put
//! in it here.
//!
//! Off macOS, and for gpui's test windows (they have no native window),
//! screen space falls back to the window's own gpui bounds: right for one
//! display, which is all a test has.
//!
//! A [`NativeWindow`] moves a window by its top-left corner. AppKit tells
//! gpui of a move as it happens, and gpui's handler needs the app, so a move
//! made while the app is borrowed (any event or update) can't reach it: the
//! caller runs it from a foreground task ([`NativeWindow::place_later`]).

use gpui::{point, size, App, Bounds, Point, Window};

/// `window`'s content, in screen space.
pub fn content_frame(window: &Window) -> Bounds<f32> {
    imp::content_frame(window).unwrap_or_else(|| {
        let b = window.bounds();
        Bounds::new(
            point(b.origin.x.as_f32(), b.origin.y.as_f32()),
            size(b.size.width.as_f32(), b.size.height.as_f32()),
        )
    })
}

/// The display under screen point `at`: its gpui display and its top-left
/// in screen space (what a window's display-relative bounds count from).
/// `None` when the platform can't say (off macOS, a test), or `at` is on no
/// display.
pub fn display_at(at: Point<f32>, cx: &App) -> Option<(gpui::DisplayId, Point<f32>)> {
    let (number, top_left) = imp::display_at(at)?;
    let display = cx.displays().into_iter().find(|d| u64::from(d.id()) == number)?;
    Some((display.id(), top_left))
}

/// A window the drag moves by hand: the tab's floating ghost, or a window
/// whose only tab was torn off.
#[derive(Clone)]
pub struct NativeWindow(imp::Handle);

impl NativeWindow {
    /// `window`'s native window, `None` for a test window.
    pub fn of(window: &Window) -> Option<NativeWindow> {
        imp::handle(window).map(NativeWindow)
    }

    /// Move the window's top-left corner to screen point `at`, from a
    /// foreground task (see the module's note).
    pub fn place_later(&self, at: Point<f32>, cx: &App) {
        let handle = self.0.clone();
        cx.foreground_executor()
            .spawn(async move { imp::place(&handle, at) })
            .detach();
    }

    /// Let the pointer through to whatever is under the window (the floating
    /// ghost never takes a click), and draw no window shadow (the ghost
    /// draws its own).
    pub fn make_overlay(&self) {
        imp::make_overlay(&self.0);
    }
}

/// Where tests' window moves went, as [`move_window`] recorded them.
#[cfg(test)]
pub mod test_log {
    use std::cell::RefCell;

    use gpui::Point;

    thread_local! {
        static MOVES: RefCell<Vec<Point<f32>>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn push(at: Point<f32>) {
        MOVES.with_borrow_mut(|m| m.push(at));
    }

    /// The moves since the last `take`, and clear them.
    pub fn take() -> Vec<Point<f32>> {
        MOVES.with_borrow_mut(std::mem::take)
    }
}

/// Move `window`'s top-left corner to screen point `at` (a test window
/// records it in [`test_log`]).
pub fn move_window(window: &Window, at: Point<f32>, cx: &App) {
    if let Some(native) = NativeWindow::of(window) {
        native.place_later(at, cx);
    } else {
        #[cfg(test)]
        test_log::push(at);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use gpui::{point, size, Bounds, Point, Window};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::{NSPoint, NSRect, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub type Handle = Retained<AnyObject>;

    /// `window`'s gpui content view and its NSWindow.
    fn view_and_window(window: &Window) -> Option<(*mut AnyObject, *mut AnyObject)> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return None;
        };
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: gpui's handle is its live content NSView; `window` is a
        // plain getter.
        let ns_window: *mut AnyObject = unsafe { msg_send![view, window] };
        (!ns_window.is_null()).then_some((view, ns_window))
    }

    /// The primary display's height: AppKit's global space grows up from its
    /// bottom edge.
    fn primary_height() -> Option<f64> {
        let screens = screens()?;
        // SAFETY: `NSScreen.screens` is an NSArray of NSScreens; index 0 is
        // the primary (menu bar) display.
        unsafe {
            let count: usize = msg_send![&*screens, count];
            if count == 0 {
                return None;
            }
            let primary: *mut AnyObject = msg_send![&*screens, objectAtIndex: 0usize];
            let frame: NSRect = msg_send![primary, frame];
            Some(frame.size.height)
        }
    }

    fn screens() -> Option<Retained<AnyObject>> {
        let cls: &AnyClass = class!(NSScreen);
        // SAFETY: a class getter returning an autoreleased NSArray.
        unsafe { Retained::retain(msg_send![cls, screens]) }
    }

    /// AppKit global rect (y up) → screen space (y down).
    fn flip(rect: NSRect, primary_h: f64) -> Bounds<f32> {
        Bounds::new(
            point(rect.origin.x as f32, (primary_h - rect.origin.y - rect.size.height) as f32),
            size(rect.size.width as f32, rect.size.height as f32),
        )
    }

    pub fn content_frame(window: &Window) -> Option<Bounds<f32>> {
        let (view, ns_window) = view_and_window(window)?;
        let primary_h = primary_height()?;
        // SAFETY: plain geometry getters and conversions on live objects.
        let rect: NSRect = unsafe {
            let bounds: NSRect = msg_send![view, bounds];
            let in_window: NSRect = msg_send![view, convertRect: bounds, toView: std::ptr::null_mut::<AnyObject>()];
            msg_send![ns_window, convertRectToScreen: in_window]
        };
        Some(flip(rect, primary_h))
    }

    pub fn display_at(at: Point<f32>) -> Option<(u64, Point<f32>)> {
        let screens = screens()?;
        let primary_h = primary_height()?;
        let key = NSString::from_str("NSScreenNumber");
        // SAFETY: NSScreen getters; `deviceDescription` is an NSDictionary
        // whose NSScreenNumber is an NSNumber (the CGDirectDisplayID).
        unsafe {
            let count: usize = msg_send![&*screens, count];
            for ix in 0..count {
                let screen: *mut AnyObject = msg_send![&*screens, objectAtIndex: ix];
                let frame = flip(msg_send![screen, frame], primary_h);
                if !frame.contains(&at) {
                    continue;
                }
                let description: *mut AnyObject = msg_send![screen, deviceDescription];
                let number: *mut AnyObject = msg_send![description, objectForKey: &*key];
                if number.is_null() {
                    return None;
                }
                let id: u32 = msg_send![number, unsignedIntValue];
                return Some((u64::from(id), frame.origin));
            }
        }
        None
    }

    pub fn handle(window: &Window) -> Option<Handle> {
        let (_, ns_window) = view_and_window(window)?;
        // SAFETY: a live NSWindow, retained for the moves to come.
        unsafe { Retained::retain(ns_window) }
    }

    pub fn place(window: &Handle, at: Point<f32>) {
        let Some(primary_h) = primary_height() else {
            return;
        };
        let top_left = NSPoint::new(at.x as f64, primary_h - at.y as f64);
        // SAFETY: `setFrameTopLeftPoint:` on a retained NSWindow.
        unsafe {
            let _: () = msg_send![&**window, setFrameTopLeftPoint: top_left];
        }
    }

    pub fn make_overlay(window: &Handle) {
        // SAFETY: plain NSWindow setters.
        unsafe {
            let _: () = msg_send![&**window, setIgnoresMouseEvents: Bool::YES];
            let _: () = msg_send![&**window, setHasShadow: Bool::NO];
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use gpui::{Bounds, Point, Window};

    pub type Handle = ();

    pub fn content_frame(_window: &Window) -> Option<Bounds<f32>> {
        None
    }

    pub fn display_at(_at: Point<f32>) -> Option<(u64, Point<f32>)> {
        None
    }

    pub fn handle(_window: &Window) -> Option<Handle> {
        None
    }

    pub fn place(_window: &Handle, _at: Point<f32>) {}

    pub fn make_overlay(_window: &Handle) {}
}
