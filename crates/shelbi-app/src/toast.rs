//! Small floating notices ("toasts"), shared by both clients.
//!
//! A toast is a short line of text that floats over the main area for a few
//! seconds and goes away by itself. It never takes focus or input: a client
//! only paints it. One shows at a time; a newer toast replaces the older one
//! (a failed launch replaces its `Launching …` toast with the error).
//!
//! The first use is the "Edit in <editor>" entry of the workspace and review
//! panels when the editor is an app: launching one takes a moment before its
//! window appears, so [`Toast::launching_editor`] says it's on its way and
//! [`Toast::editor_launch_failed`] says why it didn't come. An editor that
//! runs in a pane (Vim) shows no toast: the pane itself is the feedback, and
//! a toast must never draw over it.
//!
//! Nothing here reads a clock: the caller passes `now`, so the lifetimes are
//! tested without sleeping.

use std::time::{Duration, Instant};

/// How long an informational toast stays up.
pub const TOAST_LIFETIME: Duration = Duration::from_millis(2500);

/// How long an error toast stays up: longer, so its reason can be read.
pub const ERROR_TOAST_LIFETIME: Duration = Duration::from_secs(5);

/// What a toast says, for its tint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    Info,
    Error,
}

/// One toast: its text, tone, and how long it stays up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub text: String,
    pub tone: ToastTone,
    pub lifetime: Duration,
}

impl Toast {
    /// An informational toast with the standard lifetime.
    pub fn info(text: impl Into<String>) -> Self {
        Toast {
            text: text.into(),
            tone: ToastTone::Info,
            lifetime: TOAST_LIFETIME,
        }
    }

    /// An error toast with the longer lifetime. The text is kept to one line.
    pub fn error(text: impl Into<String>) -> Self {
        Toast {
            text: one_line(&text.into()),
            tone: ToastTone::Error,
            lifetime: ERROR_TOAST_LIFETIME,
        }
    }

    /// `Launching <editor>…`, the moment Edit in <editor> opens an app.
    /// `editor_name` is the display name the nav label uses (`Zed`).
    pub fn launching_editor(editor_name: &str) -> Self {
        Toast::info(format!("Launching {editor_name}\u{2026}"))
    }

    /// `Couldn't launch <editor>: <reason>`, replacing the launching toast
    /// when the app didn't start.
    pub fn editor_launch_failed(editor_name: &str, reason: &str) -> Self {
        let reason = one_line(reason);
        let reason = reason.trim_end_matches('.');
        if reason.is_empty() {
            Toast::error(format!("Couldn't launch {editor_name}"))
        } else {
            Toast::error(format!("Couldn't launch {editor_name}: {reason}"))
        }
    }
}

/// `s` with its runs of whitespace (newlines included) collapsed to single
/// spaces: a toast is one line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The toast on screen, if any, and when it went up. A client holds one and
/// paints [`ToastSlot::current`]; [`ToastSlot::remaining`] says when to look
/// again so an expired toast is cleared without input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToastSlot {
    shown: Option<(Toast, Instant)>,
    /// Bumped by every [`ToastSlot::show`]: a timer armed for one toast
    /// checks it so it never clears a newer one.
    generation: u64,
}

impl ToastSlot {
    /// Put `toast` up at `now`, replacing whatever showed. Returns its
    /// generation.
    pub fn show(&mut self, toast: Toast, now: Instant) -> u64 {
        self.shown = Some((toast, now));
        self.generation += 1;
        self.generation
    }

    /// The toast to paint at `now`: `None` once its lifetime has passed.
    pub fn current(&self, now: Instant) -> Option<&Toast> {
        self.shown
            .as_ref()
            .filter(|(t, at)| now.saturating_duration_since(*at) < t.lifetime)
            .map(|(t, _)| t)
    }

    /// How long until the toast showing at `now` expires (`None` when none
    /// shows).
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let (t, at) = self.shown.as_ref()?;
        t.lifetime.checked_sub(now.saturating_duration_since(*at)).filter(|d| !d.is_zero())
    }

    /// Drop the toast if it has expired at `now`. `true` when one was
    /// dropped (the client repaints).
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.shown.is_some() && self.current(now).is_none() {
            self.shown = None;
            return true;
        }
        false
    }

    /// Drop the toast showing generation `generation` (a timer armed for it
    /// fired). A newer toast is left alone. `true` when one was dropped.
    pub fn dismiss_generation(&mut self, generation: u64) -> bool {
        if self.generation == generation && self.shown.is_some() {
            self.shown = None;
            return true;
        }
        false
    }

    /// Drop the toast now, whatever it is (the main area is handed to a
    /// terminal editor, which a toast must never cover). `true` when one
    /// was showing.
    pub fn dismiss(&mut self) -> bool {
        self.shown.take().is_some()
    }

    /// The current generation (the last [`ToastSlot::show`]'s).
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launching_names_the_editor_by_its_display_name() {
        let t = Toast::launching_editor("VS Code");
        assert_eq!(t.text, "Launching VS Code\u{2026}");
        assert_eq!(t.tone, ToastTone::Info);
        assert_eq!(t.lifetime, TOAST_LIFETIME);
        // About 2 to 3 seconds.
        assert!(TOAST_LIFETIME >= Duration::from_secs(2) && TOAST_LIFETIME <= Duration::from_secs(3));
    }

    #[test]
    fn failure_names_the_editor_and_the_reason_on_one_line() {
        let t = Toast::editor_launch_failed("Zed", "Unable to find application named 'Zed'\n");
        assert_eq!(t.text, "Couldn't launch Zed: Unable to find application named 'Zed'");
        assert_eq!(t.tone, ToastTone::Error);
        assert_eq!(t.lifetime, ERROR_TOAST_LIFETIME);

        let t = Toast::editor_launch_failed("Zed", "first line\n  second line.");
        assert_eq!(t.text, "Couldn't launch Zed: first line second line");

        let t = Toast::editor_launch_failed("Zed", "  ");
        assert_eq!(t.text, "Couldn't launch Zed");
    }

    #[test]
    fn a_toast_shows_for_its_lifetime_then_expires() {
        let t0 = Instant::now();
        let mut slot = ToastSlot::default();
        assert!(slot.current(t0).is_none());
        assert_eq!(slot.remaining(t0), None);
        slot.show(Toast::launching_editor("Zed"), t0);

        let mid = t0 + TOAST_LIFETIME / 2;
        assert_eq!(slot.current(mid).map(|t| t.text.as_str()), Some("Launching Zed\u{2026}"));
        assert_eq!(slot.remaining(mid), Some(TOAST_LIFETIME - TOAST_LIFETIME / 2));
        assert!(!slot.expire(mid), "still showing");

        let end = t0 + TOAST_LIFETIME;
        assert!(slot.current(end).is_none(), "gone at its lifetime");
        assert_eq!(slot.remaining(end), None);
        assert!(slot.expire(end));
        assert!(!slot.expire(end), "already gone");
    }

    #[test]
    fn a_failure_replaces_the_launching_toast() {
        let t0 = Instant::now();
        let mut slot = ToastSlot::default();
        let launching = slot.show(Toast::launching_editor("Zed"), t0);
        let at = t0 + Duration::from_millis(1500);
        let failed = slot.show(Toast::editor_launch_failed("Zed", "no such app"), at);
        assert_ne!(launching, failed);
        assert_eq!(
            slot.current(at).map(|t| (t.text.as_str(), t.tone)),
            Some(("Couldn't launch Zed: no such app", ToastTone::Error))
        );
        // It runs its own (longer) lifetime from when it went up, past the
        // launching toast's.
        assert!(slot.current(t0 + TOAST_LIFETIME).is_some());
        assert!(slot.current(at + ERROR_TOAST_LIFETIME).is_none());
        // The launching toast's timer firing late leaves the error up.
        assert!(!slot.dismiss_generation(launching));
        assert!(slot.current(at).is_some());
        assert!(slot.dismiss_generation(failed));
        assert!(slot.current(at).is_none());
    }

    #[test]
    fn dismiss_drops_whatever_shows() {
        let t0 = Instant::now();
        let mut slot = ToastSlot::default();
        assert!(!slot.dismiss());
        slot.show(Toast::launching_editor("Zed"), t0);
        assert!(slot.dismiss());
        assert!(slot.current(t0).is_none());
    }
}
