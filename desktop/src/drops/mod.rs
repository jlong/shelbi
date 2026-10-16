//! Image and file-promise drops onto terminal panes
//! (`dt-drop-images-terminal`).
//!
//! A terminal pane takes a file drop through gpui's own path: an
//! `on_drop::<ExternalPaths>` listener ([`TerminalPane::drop_paths`]) that
//! pastes the dropped paths. gpui-pre 0.3.9's macOS window registers only
//! `NSFilenamesPboardType`, though, so only a Finder drag ever reaches it: a
//! screenshot thumbnail (a file promise), an image out of a browser, Preview,
//! Photos, Mail or Slack (image data or a URL, no filename) are refused at the
//! window before gpui sees them.
//!
//! This module widens that without forking gpui:
//!
//! - every window that holds a terminal pane is re-registered for the extra
//!   types ([`accept_in`], called from the pane's constructor);
//! - gpui's `GPUIWindow` class has its `draggingEntered:`,
//!   `draggingUpdated:` and `performDragOperation:` replaced (once, at
//!   [`install`]). A drag that carries `NSFilenamesPboardType` still goes to
//!   gpui's own methods, untouched. Any other accepted drag is answered here;
//!   on the drop its items are turned into files ([`pasteboard`]) and the
//!   resulting paths are handed back to gpui as an ordinary file drop at the
//!   drop point (`Entered` + `Submit` + `Exited`), so gpui's hit test picks the
//!   pane under the cursor and `drop_paths` pastes them exactly as it pastes a
//!   Finder drop. A drop outside every pane lands on no `on_drop` listener and
//!   does nothing.
//!
//! The route and its gpui coupling are documented in `docs/gpui.md` (file
//! drops).
//!
//! [`TerminalPane::drop_paths`]: crate::elements::terminal::TerminalPane

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) mod pasteboard;

use std::path::PathBuf;
use std::sync::OnceLock;

use gpui::{App, AsyncApp, ExternalPaths, FileDropEvent, Pixels, PlatformInput, Point, Window};

/// A drop whose items are files now, waiting to be handed to gpui.
struct Landed {
    /// The `NSWindow` it was dropped on (its address).
    ns_window: usize,
    /// The drop point, in the window's logical coordinates (gpui's).
    position: Point<Pixels>,
    paths: Vec<PathBuf>,
}

/// Where materialised drops go to reach the foreground (a promised file
/// resolves on a background queue).
static LANDED: OnceLock<async_channel::Sender<Landed>> = OnceLock::new();

/// Take over gpui's drag destination methods and start the task that hands
/// materialised drops to their window. Once, at startup.
pub fn install(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded::<Landed>();
    if LANDED.set(tx).is_err() {
        return;
    }
    #[cfg(target_os = "macos")]
    macos::install();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Ok(landed) = rx.recv().await {
            deliver(landed, cx);
        }
    })
    .detach();
}

/// Let `window` accept image and file-promise drags (idempotent; a window
/// with no native handle, such as a test window, is skipped).
pub fn accept_in(window: &Window) {
    #[cfg(target_os = "macos")]
    macos::accept_in(window);
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// The receiving end of [`LANDED`], for a test that drives the AppKit hooks
/// without the app (`install` never runs there).
#[cfg(test)]
fn test_receiver() -> async_channel::Receiver<Landed> {
    static RX: OnceLock<async_channel::Receiver<Landed>> = OnceLock::new();
    RX.get_or_init(|| {
        let (tx, rx) = async_channel::unbounded();
        let _ = LANDED.set(tx);
        rx
    })
    .clone()
}

/// Queue `paths` as a drop on `ns_window` at `position` (from any thread).
fn land(ns_window: usize, position: Point<Pixels>, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Some(tx) = LANDED.get() {
        let _ = tx.try_send(Landed {
            ns_window,
            position,
            paths,
        });
    }
}

/// Replay `landed` into its window as the file drop gpui would have built.
fn deliver(landed: Landed, cx: &mut AsyncApp) {
    let Landed {
        ns_window,
        position,
        paths,
    } = landed;
    for handle in cx.update(|cx| cx.windows()) {
        let delivered = handle.update(cx, |_, window, cx| {
            #[cfg(target_os = "macos")]
            if macos::ns_window_of(window) != Some(ns_window) {
                return false;
            }
            replay_drop(window, position, paths.clone(), cx);
            true
        });
        if delivered.unwrap_or(false) {
            return;
        }
    }
}

/// The event sequence gpui's macOS window sends for a Finder drop: entering
/// with the paths makes them the active drag, the submit is a mouse-up that
/// fires the `on_drop` listener under `position`, and the exit clears the
/// drag if nothing took it.
pub(crate) fn replay_drop(
    window: &mut Window,
    position: Point<Pixels>,
    paths: Vec<PathBuf>,
    cx: &mut App,
) {
    let paths = ExternalPaths(paths.into());
    window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Entered { position, paths }), cx);
    window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Submit { position }), cx);
    window.dispatch_event(PlatformInput::FileDrop(FileDropEvent::Exited), cx);
}
