//! The system's "Show scroll bars" (System Settings ▸ Appearance): whether
//! scroll bars are overlays, shown only while scrolling ("When scrolling",
//! or "Automatically based on mouse or trackpad" with a trackpad), or
//! always shown ("Always", or Automatic with a mouse plugged in). The
//! terminals' scrollbar follows it under `desktop.terminal_scrollbar: auto`.

/// Whether the system prefers overlay scroll bars: AppKit's
/// `NSScroller.preferredScrollerStyle`, which resolves Automatic against
/// the pointing devices attached (off the main thread, the
/// `AppleShowScrollBars` default, where Automatic counts as overlay).
#[cfg(target_os = "macos")]
pub fn overlay() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSScroller, NSScrollerStyle};
    use objc2_foundation::{ns_string, NSUserDefaults};
    match MainThreadMarker::new() {
        Some(mtm) => NSScroller::preferredScrollerStyle(mtm) == NSScrollerStyle::Overlay,
        None => NSUserDefaults::standardUserDefaults()
            .stringForKey(ns_string!("AppleShowScrollBars"))
            .is_none_or(|v| v.to_string() != "Always"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn overlay() -> bool {
    true
}

/// Call `changed` (on the main thread) whenever the preferred style
/// changes: the setting was changed, or a mouse was plugged in or out under
/// Automatic. The observer lives as long as the app.
#[cfg(target_os = "macos")]
pub fn observe(changed: futures::channel::mpsc::UnboundedSender<()>) {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2_app_kit::NSPreferredScrollerStyleDidChangeNotification;
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSOperationQueue};

    let block = RcBlock::new(move |_: NonNull<NSNotification>| {
        let _ = changed.unbounded_send(());
    });
    // SAFETY: the name is AppKit's own constant, no object filters it, the
    // block is called on the main queue, and what it captures (a channel
    // sender) is `Send`.
    let observer = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSPreferredScrollerStyleDidChangeNotification),
            None,
            Some(&NSOperationQueue::mainQueue()),
            &block,
        )
    };
    // Observed for the app's life.
    std::mem::forget(observer);
}

#[cfg(not(target_os = "macos"))]
pub fn observe(_changed: futures::channel::mpsc::UnboundedSender<()>) {}
