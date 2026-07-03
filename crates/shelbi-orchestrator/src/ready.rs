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

/// A detected usage-limit stall, with the reset-time hint scraped from the
/// pane when claude showed one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimitStall {
    /// The reset-time hint (e.g. `3pm (America/New_York)`), or `None` when the
    /// pane showed no parseable reset wording. Advisory only — folded into the
    /// `paused` event, never parsed into a real timestamp.
    pub reset: Option<String>,
}

/// Detect whether a captured pane shows claude *actually stalled on its
/// usage/session limit* — as opposed to merely containing the words "usage
/// limit" somewhere (source code, docs, this file, a task description, an
/// agent's own chat about the feature).
///
/// ## Why this can't be a substring match
///
/// The obvious approach — a `dialog_signatures` entry like `"usage limit
/// reached"` — false-positives on any worker whose pane happens to show the
/// phrase: someone editing usage-limit code/tests, a docs page describing the
/// message, or an agent reasoning aloud about the feature. That misfire was
/// observed in practice (a worker writing this very detector's tests got its
/// workspace flipped to `blocked reason=dialog:usage-limit`). A false pause is
/// worse than a missed one: it tells the orchestrator to stop dispatching to a
/// perfectly healthy slot.
///
/// ## What we anchor on instead
///
/// Claude Code's real usage-limit stall is an *interactive modal* it renders
/// and blocks on:
///
/// ```text
/// You've hit your usage limit.
/// ❯ 1. Stop and wait for limit to reset
///   2. Upgrade your plan
/// ```
///
/// We require the actionable option `Stop and wait for limit to reset` to
/// appear **as a rendered menu option** — a line whose leading glyphs are
/// claude's menu chrome (the `❯` selection cursor and/or a `N.` option number),
/// not a bare occurrence — *and* the modal's limit wording ("usage limit") to
/// be present on screen. Source code, prose, or chat that mentions the phrase
/// carries neither the menu chrome nor the paired context, so it doesn't match.
/// This trades away catching a hypothetical non-modal/inline limit notice (we'd
/// rather miss that than pause a healthy worker) for robustness against the
/// false positive.
///
/// As a final backstop we require the pane to *not* also show claude's live,
/// ready input box (see [`is_input_ready`]). A genuine usage-limit modal
/// replaces the input box, just as the trust dialog does; so if the ready
/// footer is still on screen the pane is actively working — the textbook case
/// being a worker *editing* usage-limit code/fixtures whose source embeds the
/// modal text (menu glyphs and all) as a string literal. `capture` samples the
/// visible screen only (no scrollback), so the footer's presence is a reliable
/// "this pane is live, not stalled" signal.
pub fn detect_usage_limit(screen: &str) -> Option<UsageLimitStall> {
    // A live, ready input box means the pane is working, not stalled on a
    // modal — so any usage-limit wording present is mere content.
    if is_input_ready(screen) {
        return None;
    }
    let lower = screen.to_ascii_lowercase();
    // The modal's limit wording must be present somewhere on screen. Cheap
    // corroborating token that a prose/code mention of just the option text
    // alone won't satisfy on its own.
    if !lower.contains("usage limit") {
        return None;
    }
    // The actionable option must be rendered as an actual claude menu option,
    // i.e. on a line that begins with menu chrome — never a bare substring.
    let is_stall = screen.lines().any(|line| {
        line.to_ascii_lowercase()
            .contains("stop and wait for limit to reset")
            && is_menu_option_line(line)
    });
    if !is_stall {
        return None;
    }
    Some(UsageLimitStall {
        reset: parse_usage_limit_reset(screen),
    })
}

/// True when `line` is rendered as one of claude's interactive menu options —
/// its leading, non-whitespace chrome is the `❯` selection cursor and/or a
/// `N.` option number (`❯ 1. …`, `  2. …`). This is the discriminator between
/// claude *rendering* the option and a pane merely *containing* the option's
/// text (a string literal in source, a sentence in prose), neither of which
/// carries the menu framing.
fn is_menu_option_line(line: &str) -> bool {
    // Drop leading whitespace and an optional selection cursor.
    let rest = line.trim_start();
    let rest = rest.strip_prefix('❯').map(str::trim_start).unwrap_or(rest);
    // What remains must open with `<digit(s)>.` — the option number.
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    !digits.is_empty() && rest[digits.len()..].starts_with('.')
}

/// Best-effort extraction of the reset-time hint from a captured usage-limit
/// pane, e.g. `Your limit will reset at 3pm (America/New_York)` →
/// `3pm (America/New_York)`, or `resets 10:30am` → `10:30am`. Returns `None`
/// when no reset wording is present (claude doesn't always show one).
///
/// Only meaningful once [`detect_usage_limit`] has confirmed the pane is on a
/// real stall — outside that context a stray "reset" in scrollback would
/// mis-parse. The captured value is advisory: it's folded into the `paused`
/// event so an operator sees when the limit lifts, not parsed into a real
/// timestamp.
pub fn parse_usage_limit_reset(screen: &str) -> Option<String> {
    // `to_ascii_lowercase` preserves byte length, so an index found in the
    // lowered copy addresses the same byte in the original (case-preserved)
    // screen — we search lowered but slice the original.
    let lower = screen.to_ascii_lowercase();
    // Anchor on the reset-*time* wording. Deliberately NOT a bare "reset":
    // the modal's own option text ("Stop and wait for limit to reset") ends
    // in "reset", and anchoring there would capture the next menu option
    // instead of the time. The "reset(s) at" form consumes the " at "
    // connective; "resets" alone covers "resets 3pm". Earliest anchor wins.
    let (idx, klen) = ["resets at", "reset at", "resets"]
        .iter()
        .filter_map(|k| lower.find(k).map(|i| (i, k.len())))
        .min_by_key(|(i, _)| *i)?;
    let rest = screen[idx + klen..].trim_start();
    // The reset clause ends at the first sentence break or newline.
    let end = rest.find(['\n', '.']).unwrap_or(rest.len());
    let captured: String = rest[..end].trim().chars().take(60).collect();
    let captured = captured.trim();
    if captured.is_empty() {
        None
    } else {
        Some(captured.to_string())
    }
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

    // The real usage-limit modal claude renders and blocks on. Note the
    // menu-option chrome (`❯ 1.` / `  2.`) — the load-bearing detail that
    // tells a rendered modal apart from a mere mention of the phrase.
    const USAGE_LIMIT_MODAL_SCREEN: &str = "\
⏱ You've hit your usage limit.

 ❯ 1. Stop and wait for limit to reset
   2. Upgrade your plan

 Your limit will reset at 3pm (America/New_York).";

    #[test]
    fn detect_blocking_dialog_matches_first_signature_by_kind() {
        let sigs = shelbi_core::default_dialog_signatures("claude");

        // Trust prompt → trust kind (matched case-insensitively).
        assert_eq!(
            detect_blocking_dialog(TRUST_DIALOG_SCREEN, &sigs).as_deref(),
            Some("trust")
        );

        // The live, ready input box is not a blocking dialog.
        assert!(detect_blocking_dialog(INPUT_BOX_SCREEN, &sigs).is_none());

        // usage-limit is intentionally NOT a generic dialog signature — it's
        // handled structurally by `detect_usage_limit`, so the substring path
        // never matches it (that's what prevents the false-positive pause).
        assert!(detect_blocking_dialog(USAGE_LIMIT_MODAL_SCREEN, &sigs).is_none());

        // Empty signature list never matches, even on a real dialog.
        assert!(detect_blocking_dialog(TRUST_DIALOG_SCREEN, &[]).is_none());
    }

    #[test]
    fn detect_usage_limit_matches_the_rendered_modal() {
        let stall = detect_usage_limit(USAGE_LIMIT_MODAL_SCREEN).expect("modal must detect");
        assert_eq!(stall.reset.as_deref(), Some("3pm (America/New_York)"));
    }

    #[test]
    fn detect_usage_limit_ignores_mere_mentions_of_the_phrase() {
        // THE REGRESSION: a pane that merely CONTAINS the wording — because a
        // worker is editing usage-limit code, reading docs, or an agent is
        // reasoning about the feature — must NOT be read as a real stall.
        // Each of these carries the phrase but none carries the menu chrome.

        // Source code containing the exact option string as a literal.
        let code = "        DialogSignature::new(\"usage-limit\", \"Stop and wait for limit to reset\"),\n\
                    // matches the usage limit modal claude renders";
        assert!(detect_usage_limit(code).is_none(), "source literal must not detect");

        // Prose / docs describing the behavior.
        let docs = "When Claude Code hits your usage limit, it prints \"usage limit reached\" and \
                    waits until the limit resets at the top of the hour.";
        assert!(detect_usage_limit(docs).is_none(), "docs prose must not detect");

        // An agent's own chat mentioning it while working on this task.
        let chat = "I'll add detection for the usage limit stall — the 'Stop and wait for limit \
                    to reset' option — and show a pause badge.";
        assert!(detect_usage_limit(chat).is_none(), "agent chat must not detect");

        // A live, ready input box (no limit at all).
        assert!(detect_usage_limit(INPUT_BOX_SCREEN).is_none());
    }

    #[test]
    fn detect_usage_limit_ignores_a_worker_editing_the_modal_fixture() {
        // The hardest case: a worker is *editing* usage-limit code whose source
        // embeds the fully-rendered modal — menu cursor glyphs and all — as a
        // string literal (exactly what this test file does). The chrome anchor
        // alone would match it, but a real stall replaces claude's input box,
        // whereas an editing session still shows the live footer. So a pane that
        // has BOTH the modal text and a ready input box is working, not stalled.
        let editing = format!(
            "{USAGE_LIMIT_MODAL_SCREEN}\n\
             ────────────────────────────────────────\n\
             ❯ edit ready.rs\n\
             ────────────────────────────────────────\n\
               ⏵⏵ accept edits on (shift+tab to cycle) · ← for agents"
        );
        // Sanity: the fixture really does carry the menu chrome that would
        // otherwise trip the detector.
        assert!(editing.contains("❯ 1. Stop and wait for limit to reset"));
        assert!(
            detect_usage_limit(&editing).is_none(),
            "a live input footer means the pane is editing, not stalled"
        );
    }

    #[test]
    fn is_menu_option_line_requires_menu_chrome() {
        assert!(is_menu_option_line(" ❯ 1. Stop and wait for limit to reset"));
        assert!(is_menu_option_line("   2. Upgrade your plan"));
        assert!(is_menu_option_line("10. tenth option"));
        // A bare mention with no menu framing is not a menu option.
        assert!(!is_menu_option_line(
            "DialogSignature::new(\"usage-limit\", \"Stop and wait for limit to reset\"),"
        ));
        assert!(!is_menu_option_line("the option: Stop and wait for limit to reset"));
        assert!(!is_menu_option_line("❯ Try \"edit <filepath>\""));
    }

    #[test]
    fn parse_usage_limit_reset_extracts_time_hint() {
        assert_eq!(
            parse_usage_limit_reset(
                "Claude usage limit reached. Your limit will reset at 3pm (America/New_York)."
            )
            .as_deref(),
            Some("3pm (America/New_York)")
        );
        assert_eq!(
            parse_usage_limit_reset("You've hit your usage limit. resets 10:30am\nmore text")
                .as_deref(),
            Some("10:30am")
        );
        assert_eq!(
            parse_usage_limit_reset("limit resets at 9:00").as_deref(),
            Some("9:00")
        );
        // No reset wording → nothing to capture.
        assert!(parse_usage_limit_reset("Stop and wait for limit to reset").is_none());
        // The bare modal footer has "reset" with nothing usable after it.
        assert!(parse_usage_limit_reset("please reset.").is_none());
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
