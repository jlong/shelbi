//! A native open panel that starts in a given folder, for Settings' Browse…
//! in the Editor and Diff tool pickers: gpui's `prompt_for_paths` can't name
//! the folder it opens in, and these start in `/Applications` on macOS
//! (`NSOpenPanel`) and `/usr/bin` on Linux (the XDG Desktop Portal's file
//! chooser, `platform::os::BROWSE_START`).
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

/// Show the portal's file chooser in `start`, titled with `message`; the
/// receiver gets the chosen file, or `None` when the user cancels or there
/// is no portal (which is logged). The request runs on a thread of its own:
/// the portal answers when the user does.
#[cfg(not(target_os = "macos"))]
pub fn choose_file(start: &Path, message: &str) -> oneshot::Receiver<Option<PathBuf>> {
    let (tx, rx) = oneshot::channel();
    let (start, message) = (start.to_path_buf(), message.to_string());
    let spawned = std::thread::Builder::new()
        .name("shelbi-desktop-file-chooser".into())
        .spawn(move || {
            let picked = futures::executor::block_on(portal_choose_file(&start, &message));
            let _ = tx.send(picked.unwrap_or_else(|e| {
                tracing::warn!(%e, "the file chooser portal didn't answer");
                None
            }));
        });
    if let Err(e) = spawned {
        tracing::warn!(%e, "couldn't start the file chooser");
    }
    rx
}

#[cfg(not(target_os = "macos"))]
async fn portal_choose_file(start: &Path, message: &str) -> Result<Option<PathBuf>, ashpd::Error> {
    use ashpd::desktop::file_chooser::OpenFileRequest;
    let request = OpenFileRequest::default()
        .title(message)
        .accept_label("Choose")
        .modal(true)
        .multiple(false)
        .directory(false)
        .current_folder(start)?
        .send()
        .await?;
    match request.response() {
        Ok(files) => Ok(files.uris().iter().find_map(|uri| file_uri_path(uri.as_str()))),
        // The user cancelled.
        Err(ashpd::Error::Response(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The local path a `file://` URI names (percent-decoded); `None` for any
/// other URI.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///x` has an empty host; `file://localhost/x` names this one.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    path.starts_with('/')
        .then(|| PathBuf::from(shelbi_app::changelog::percent_decode(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode_to_local_paths() {
        assert_eq!(file_uri_path("file:///usr/bin/nvim"), Some(PathBuf::from("/usr/bin/nvim")));
        assert_eq!(file_uri_path("file:///home/me/My%20Apps/zed"), Some(PathBuf::from("/home/me/My Apps/zed")));
        assert_eq!(file_uri_path("file://localhost/opt/x"), Some(PathBuf::from("/opt/x")));
        assert_eq!(file_uri_path("https://example.com/x"), None);
    }
}
