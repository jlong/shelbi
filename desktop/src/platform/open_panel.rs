//! A native open panel (`NSOpenPanel`) that starts in a given folder, for
//! Settings' Browse… in the Editor and Diff tool pickers: gpui's
//! `prompt_for_paths` can't name the folder it opens in, and these start in
//! `/Applications`.
//!
//! It chooses files only. An `.app` is a package, which the panel treats as
//! a file, so apps and executables are both choosable; what a pick becomes
//! is the shared model's call (`shelbi_state::tools::choice_from_path`).

use std::path::{Path, PathBuf};

use futures::channel::oneshot;

/// Show the panel in `start`, titled with `message`; the receiver gets the
/// chosen file, or `None` when the user cancels. Call on the main thread.
#[cfg(target_os = "macos")]
pub fn choose_file(start: &Path, message: &str) -> oneshot::Receiver<Option<PathBuf>> {
    use std::cell::Cell;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSModalResponse, NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::{NSString, NSURL};

    let (tx, rx) = oneshot::channel();
    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("the open panel can only be shown on the main thread");
        let _ = tx.send(None);
        return rx;
    };
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setResolvesAliases(true);
    panel.setDirectoryURL(NSURL::from_directory_path(start).as_deref());
    panel.setMessage(Some(&NSString::from_str(message)));
    panel.setPrompt(Some(&NSString::from_str("Choose")));
    let tx = Cell::new(Some(tx));
    let handler = RcBlock::new({
        let panel = panel.clone();
        move |response: NSModalResponse| {
            let Some(tx) = tx.take() else {
                return;
            };
            let picked = (response == NSModalResponseOK)
                .then(|| panel.URLs().iter().filter(|u| u.isFileURL()).find_map(|u| u.to_file_path()))
                .flatten();
            let _ = tx.send(picked);
        }
    });
    panel.beginWithCompletionHandler(&handler);
    rx
}

#[cfg(not(target_os = "macos"))]
pub fn choose_file(_start: &Path, _message: &str) -> oneshot::Receiver<Option<PathBuf>> {
    let (tx, rx) = oneshot::channel();
    let _ = tx.send(None);
    rx
}
