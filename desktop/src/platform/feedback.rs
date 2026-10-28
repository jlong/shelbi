//! Contact Us's sending, for the desktop (`dt-feedback-dialog`): the shared
//! [`shelbi_app::feedback`] (the request, its answer, the remembered name
//! and email, the curl transport) sent as [`Client::Desktop`] at the app's
//! version. [`FeedbackGlobal`] is the seam a test sets so nothing reaches
//! the real endpoint.

pub use shelbi_app::feedback::*;

use super::version::APP_VERSION;

/// The [`FeedbackService`] the dialog sends through, as a gpui global when a
/// test sets one; otherwise the dialog uses [`live`].
#[derive(Clone)]
pub struct FeedbackGlobal(pub FeedbackService);

impl gpui::Global for FeedbackGlobal {}

/// The real endpoint (or `SHELBI_FEEDBACK_URL`), as this app.
#[cfg_attr(test, allow(dead_code))]
pub fn live() -> FeedbackService {
    FeedbackService::live(Client::Desktop, APP_VERSION)
}

/// No network, no file: what a test's dialog gets when the test set no
/// service.
#[cfg(test)]
pub fn offline() -> FeedbackService {
    FeedbackService::offline(Client::Desktop)
}
