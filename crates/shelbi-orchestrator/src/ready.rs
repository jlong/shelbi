//! Shared "wait for claude to be ready to type" helpers.
//!
//! Both the workspace dispatch path and the review-pane launch face the
//! same hazard: after we `send_line` the launch command, claude takes a
//! variable amount of time to draw its input box, and on first entry to an
//! untrusted directory tree it interposes a "trust this folder" dialog. A
//! fixed sleep either wastes time on a fast host or — worse — types the
//! prompt into the trust dialog / scrollback on a slow/fresh/remote pane,
//! where the first Enter just confirms trust and the prompt is lost.
//!
//! So instead of sleeping we poll the pane until it shows the input box,
//! auto-confirming the trust dialog along the way. Extracted here so the
//! review flow reuses exactly the same detection the dispatch flow uses.

use shelbi_core::{Host, Result, TmuxAddr};

/// How long to wait for claude's input box to appear before giving up and
/// sending the prompt anyway.
pub const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How often to re-capture the pane while waiting for readiness.
pub const READY_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// Poll the pane until claude's input box is on screen and ready to accept
/// the initial prompt. Returns `Ok(true)` once ready, `Ok(false)` on
/// timeout (the caller sends anyway).
///
/// ## Why this exists / what the bug actually was
///
/// The original code slept a fixed 1500ms then typed. That fails on a
/// fresh devbox workspace for a reason that is *not* terminal encoding:
/// investigation on a Linux workspace showed claude emits the `❯` prompt glyph
/// (`e2 9d af`) under both `en_US.UTF-8` and the bare `C` locale, and
/// `tmux capture-pane` preserves those bytes intact. So a single-glyph probe
/// matches fine on Linux — encoding is a red herring.
///
/// The real fragility is twofold:
///  1. `❯` is *ambiguous*: it is also the menu cursor in claude's modal
///     dialogs (`❯ 1. Yes, I trust this folder`), so a probe keyed on the
///     glyph alone can fire on a dialog instead of the input box.
///  2. On first entry to an untrusted directory tree claude shows a "trust
///     this folder" dialog and waits. The hub rarely sees it (its work_dir
///     tree is already trusted), but a fresh devbox does — and a fixed sleep
///     types the task body straight into that dialog, where the first Enter
///     just confirms trust and the prompt is lost.
///
/// So we (a) auto-confirm the trust dialog (shelbi owns these worktrees, so
/// trusting them is implied by the assignment) and (b) key readiness on
/// signals unique to the *input box*, never present in a modal menu.
pub fn wait_for_claude_ready(
    host: &Host,
    addr: &TmuxAddr,
    timeout: std::time::Duration,
) -> Result<bool> {
    let start = std::time::Instant::now();
    let mut trust_dismissed = false;
    while start.elapsed() < timeout {
        // A capture failure here is transient (pane still spinning up); keep
        // polling rather than aborting the whole task start.
        let screen = shelbi_tmux::capture(host, addr).unwrap_or_default();
        if is_input_ready(&screen) {
            return Ok(true);
        }
        if !trust_dismissed && is_trust_dialog(&screen) {
            shelbi_tmux::send_enter(host, addr)?;
            trust_dismissed = true;
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    }
    Ok(false)
}

/// True when the captured pane shows claude's input box ready for typing.
///
/// We match the footer/status line that claude renders *only* once the input
/// box is live — `shift+tab to cycle` is present in every permission mode,
/// and the others cover mode/version wording drift. None of these strings
/// appear in claude's modal dialogs, so this won't fire on the trust prompt.
pub fn is_input_ready(screen: &str) -> bool {
    const READY_MARKERS: &[&str] = &[
        "shift+tab to cycle", // permission-mode footer (all modes)
        "for shortcuts",      // "? for shortcuts" footer (plain mode)
        "auto mode on",
        "accept edits on",
        "plan mode on",
    ];
    READY_MARKERS.iter().any(|m| screen.contains(m))
}

/// True when the captured pane shows claude's "trust this folder" dialog.
pub fn is_trust_dialog(screen: &str) -> bool {
    let s = screen.to_ascii_lowercase();
    s.contains("trust this folder") || s.contains("do you trust")
}

/// Scan a captured pane for the first matching blocking-dialog signature,
/// returning its `kind`. Case-insensitive plain-substring match.
///
/// This is the heartbeat's counterpart to [`is_input_ready`]/[`is_trust_dialog`]:
/// where those gate the *spawn* path, this runs on every hub poll to spot a
/// workspace frozen on an interactive dialog (usage-limit, trust,
/// permission-confirm, …) that no hook or pane-title marker will ever
/// surface — the pane stays alive and its title keeps reading
/// `shelbi:working` while all real progress is stuck behind the modal.
pub fn detect_blocking_dialog(
    screen: &str,
    signatures: &[shelbi_core::DialogSignature],
) -> Option<String> {
    let lower = screen.to_ascii_lowercase();
    signatures
        .iter()
        .find(|sig| lower.contains(&sig.pattern.to_ascii_lowercase()))
        .map(|sig| sig.kind.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real captures observed on a Linux (delta) workspace, used to pin the
    // readiness/trust detection against claude's actual rendered output.
    const TRUST_DIALOG_SCREEN: &str = "\
 Do you trust the files in this folder?

 /work/myapp/.shelbi/wt/bob

 Claude Code may read, edit, and execute files here.

 ❯ 1. Yes, I trust this folder
   2. No, exit

 Enter to confirm · Esc to cancel";

    const INPUT_BOX_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ Try \"edit <filepath> to...\"
────────────────────────────────────────────────────
  ⏵⏵ accept edits on (shift+tab to cycle) · ← for agents";

    #[test]
    fn input_ready_detects_live_input_box_not_trust_dialog() {
        assert!(is_input_ready(INPUT_BOX_SCREEN));
        // The trust dialog also contains `❯`, but must NOT read as ready.
        assert!(!is_input_ready(TRUST_DIALOG_SCREEN));
        // A bare shell prompt before claude has drawn anything.
        assert!(!is_input_ready("➜  bob git:(main) claude"));
        assert!(!is_input_ready(""));
    }

    #[test]
    fn input_ready_matches_each_permission_mode_footer() {
        assert!(is_input_ready("⏵⏵ auto mode on (shift+tab to cycle)"));
        assert!(is_input_ready("⏸ plan mode on (shift+tab to cycle)"));
        assert!(is_input_ready("? for shortcuts"));
    }

    #[test]
    fn trust_dialog_detected_case_insensitively() {
        assert!(is_trust_dialog(TRUST_DIALOG_SCREEN));
        assert!(is_trust_dialog("DO YOU TRUST the files in this folder?"));
        // The live input box is not a trust dialog.
        assert!(!is_trust_dialog(INPUT_BOX_SCREEN));
    }

    #[test]
    fn detect_blocking_dialog_matches_first_signature_by_kind() {
        let sigs = shelbi_core::default_dialog_signatures("claude");

        // The usage-limit modal that froze the board in the incident.
        let usage = "⏱ You've hit your usage limit.\n\
                     ❯ 1. Stop and wait for limit to reset\n  2. Upgrade your plan";
        assert_eq!(
            detect_blocking_dialog(usage, &sigs).as_deref(),
            Some("usage-limit")
        );

        // Trust prompt → trust kind (matched case-insensitively).
        assert_eq!(
            detect_blocking_dialog(TRUST_DIALOG_SCREEN, &sigs).as_deref(),
            Some("trust")
        );

        // The live, ready input box is not a blocking dialog.
        assert!(detect_blocking_dialog(INPUT_BOX_SCREEN, &sigs).is_none());

        // Empty signature list never matches, even on a real dialog.
        assert!(detect_blocking_dialog(usage, &[]).is_none());
    }

    #[test]
    fn detect_blocking_dialog_honors_custom_signature() {
        // The "extensible via config" path: a project-defined signature is
        // matched just like the built-ins.
        let sigs = vec![shelbi_core::DialogSignature::new("codex-approve", "Approve this edit?")];
        assert_eq!(
            detect_blocking_dialog("codex › Approve this edit? (y/n)", &sigs).as_deref(),
            Some("codex-approve")
        );
        assert!(detect_blocking_dialog("nothing to see here", &sigs).is_none());
    }
}
