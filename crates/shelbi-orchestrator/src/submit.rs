//! Verified-submit: the one way text goes into a worker pane.
//!
//! Every injection path — task dispatch, supervision re-injection after an
//! auto-restart, `shelbi send`, and any future nudge/resume mechanism — has
//! the same failure mode: the text lands in claude's input box but the
//! trailing Enter is consumed as part of the bracketed paste, so the prompt
//! sits un-submitted until a human presses Enter (observed 2026-07-12 on
//! alpha for a mid-task orchestrator note; earlier for dispatch and the
//! post-restart re-injection). The dispatch path grew a submit-verification
//! probe fix by fix; this module is that probe extracted into a shared
//! primitive so no injection path has to reimplement (or forget) it.
//!
//! The shape of a verified send:
//!
//! 1. Snapshot a [`PaneBaseline`] BEFORE delivering anything — was the pane
//!    already mid-turn, did the title already carry `shelbi:working`? Both
//!    poison the corresponding submit signals for THIS delivery.
//! 2. Deliver the text WITHOUT its Enter ([`shelbi_tmux::send_text`]), let
//!    the pane settle, then send Enter as a separate key event
//!    ([`deliver_text`]) — an Enter riding the same instant as the paste is
//!    exactly the keystroke that gets eaten.
//! 3. Poll for proof of submission ([`verify_submitted`]): title marker
//!    flipping to `shelbi:working`, busy spinner/footer in the pane body, or
//!    the input box no longer holding the text. If nothing lands and the
//!    text is visibly parked in the box, retry Enter once and poll again.
//!
//! Claude and Codex expose enough stable UI evidence for Shelbi to verify and
//! retry a submit. Custom runners still use the same text → settle → separate-
//! Enter delivery sequence, but return [`SubmitStatus::DeliveredUnverified`]
//! instead of being inspected with a foreign screen parser.
//!
//! The result is a [`SubmitStatus`] the caller maps to its own events.log
//! vocabulary (`dispatch … status=confirmed`, `send … status=submitted`),
//! so a stuck Claude prompt is surfaced instead of silently waiting for a
//! human keypress.
//!
//! `shelbi message` is intentionally outside this module's scope. It injects
//! no terminal text or Enter: it appends a durable JSON record that runner
//! hooks consume and acknowledge by `msg_id`. Sending that body here as well
//! would duplicate delivery and weaken the restart-safe message channel.

use shelbi_core::{AgentRunnerSpec, Error, Host, Result, TmuxAddr};
use shelbi_state::PaneMarker;

/// Verification capability for the runner receiving a pane injection.
///
/// Delivery is shared for every runner. Claude and Codex each use their own
/// composer/busy parser; applying either parser to a custom TUI could turn a
/// successful send into a false failure and make the retry Enter submit
/// unrelated input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitProfile {
    ClaudeUi,
    CodexUi,
    DeliveryOnly,
}

impl SubmitProfile {
    pub fn for_runner(runner: &AgentRunnerSpec) -> Self {
        if shelbi_agent::is_claude_runner(&runner.command) {
            Self::ClaudeUi
        } else if shelbi_agent::is_codex_runner(&runner.command) {
            Self::CodexUi
        } else {
            Self::DeliveryOnly
        }
    }

    /// Whether Shelbi knows how to locate and interpret this runner's live
    /// input UI. Callers use this to avoid Claude-only readiness probes too.
    pub fn has_ui_verifier(self) -> bool {
        self != Self::DeliveryOnly
    }

    /// Whether startup readiness may use Claude's bordered-composer parser.
    /// Codex has submit verification but a different startup UI.
    pub fn uses_claude_ui(self) -> bool {
        self == Self::ClaudeUi
    }
}

/// How long to wait, per attempt, for proof the text got submitted (pane
/// title flips to `shelbi:working` OR the pane content shows claude is busy
/// processing OR the input box no longer holds the text). Submit lands
/// almost immediately when the hook fires; the window covers the slow path
/// (busy SSH, sluggish tmux server, a model that takes a few seconds to
/// start streaming). Deliberately longer than the old 5s: a genuine
/// submission whose busy footer was slow to render read as a stall and
/// produced a false `enter-stalled` (observed 2026-07-02 on charlie, whose
/// prompt had submitted fine). With the dispatch aborting on an unconfirmed
/// submit, a false negative is worse than before — so we give a real
/// submission ample room to prove itself.
pub const PROMPT_SUBMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// How often to re-check the pane while waiting for the submit signal.
const PROMPT_SUBMIT_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Scrollback depth captured when checking for the busy signal — enough that
/// a captured pane whose spinner/footer has scrolled a little still shows it.
const PROMPT_SUBMIT_SCROLLBACK: usize = 200;

/// Pause between delivering the text and sending its Enter. A bracketed
/// paste and an Enter arriving in the same input flush is the race that eats
/// the Enter (claude treats it as part of the paste): two nudges sent with
/// the identical command worked and a third left the text parked in the box,
/// same day, same pane. The settle gives claude time to finish consuming the
/// paste so the Enter arrives as an unambiguous, separate keypress.
const SUBMIT_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

/// Codex rotates these dim placeholders while its textarea is empty. Plain
/// tmux capture loses that styling, so autonomous wake eligibility also
/// requires the empty-composer-only shortcuts footer; either signal by itself
/// is ambiguous and must defer delivery. Keep the former single placeholder
/// for compatibility with older Codex versions still used by some projects.
const CODEX_EMPTY_COMPOSER_PLACEHOLDERS: &[&str] = &[
    "Explain this codebase",
    "Summarize recent commits",
    "Implement {feature}",
    "Find and fix a bug in @filename",
    "Write tests for @filename",
    "Improve documentation in @filename",
    "Run /review on my current changes",
    "Use /skills to list available skills",
    "Ask Codex to do anything",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexComposerState {
    Empty,
    Occupied,
    Unknown,
}

/// What the pane looked like BEFORE this delivery — captured so submit
/// signals that were already true can't be mistaken for proof that THIS
/// text landed.
#[derive(Debug, Clone, Copy)]
pub struct PaneBaseline {
    profile: SubmitProfile,
    /// The pane already showed claude mid-turn (busy spinner / token
    /// footer). True on a `--continue` resume, where the replayed
    /// conversation carries old token footers into the scrollback, and on a
    /// message sent to a genuinely busy worker. Either way the
    /// busy-scrollback signal is not proof this delivery submitted, so
    /// verification suppresses it.
    pub busy: bool,
    /// Strong evidence the pane is busy *right now*, suitable for deciding
    /// that visible post-Enter text is Claude's accepted mid-turn queue. This
    /// is intentionally narrower than `busy`: old `tokens)` lines in
    /// scrollback suppress a stale verification signal but must not turn a
    /// genuinely stuck idle send into a false `queued` success.
    pub actively_busy: bool,
    /// The pane title already carried `shelbi:working`. A busy worker's
    /// title keeps the marker from ITS current turn, so seeing it after our
    /// delivery proves nothing; verification then leans on the input box.
    pub title_working: bool,
    /// Conservative reading of Codex's live composer. Capture failure and UI
    /// shapes that do not positively identify an empty textarea stay Unknown.
    codex_composer: CodexComposerState,
}

impl PaneBaseline {
    /// Snapshot the pane's pre-delivery state. Capture failures degrade to
    /// "not busy / no marker" — the conservative direction: a signal that
    /// might be stale is only ever *suppressed* when the baseline says so,
    /// and an SSH hiccup here shouldn't mute real signals.
    pub fn capture(host: &Host, addr: &TmuxAddr, profile: SubmitProfile) -> Self {
        // Delivery-only runners have no pane chrome Shelbi may interpret.
        // Avoid three pointless captures (especially expensive over SSH) and
        // make the capability boundary explicit before any UI inspection.
        if !profile.has_ui_verifier() {
            return PaneBaseline::fresh(profile);
        }
        let screen =
            shelbi_tmux::capture_history(host, addr, PROMPT_SUBMIT_SCROLLBACK).unwrap_or_default();
        // Preserve failure separately from a successful empty capture. Wake
        // injection may never interpret an unavailable composer as empty.
        let visible_screen = shelbi_tmux::capture(host, addr).ok();
        let title = shelbi_tmux::pane_title(host, addr).unwrap_or_default();
        Self::from_capture(profile, &screen, visible_screen.as_deref(), &title)
    }

    #[cfg(test)]
    fn from_snapshots(
        profile: SubmitProfile,
        screen: &str,
        visible_screen: &str,
        title: &str,
    ) -> Self {
        Self::from_capture(profile, screen, Some(visible_screen), title)
    }

    fn from_capture(
        profile: SubmitProfile,
        screen: &str,
        visible_screen: Option<&str>,
        title: &str,
    ) -> Self {
        if !profile.has_ui_verifier() {
            return PaneBaseline {
                profile,
                busy: false,
                actively_busy: false,
                title_working: false,
                codex_composer: CodexComposerState::Unknown,
            };
        }
        let visible = visible_screen.unwrap_or_default();
        let title_working = profile == SubmitProfile::ClaudeUi
            && matches!(
                shelbi_state::parse_pane_title_marker(title),
                Some(PaneMarker::Working)
            );
        let (busy, actively_busy) = match profile {
            SubmitProfile::ClaudeUi => (
                claude_is_processing(screen),
                title_working || claude_is_actively_processing(visible),
            ),
            SubmitProfile::CodexUi => (codex_is_processing(screen), codex_is_processing(visible)),
            SubmitProfile::DeliveryOnly => (false, false),
        };
        PaneBaseline {
            profile,
            busy,
            actively_busy,
            title_working,
            codex_composer: if profile == SubmitProfile::CodexUi {
                codex_composer_state(visible_screen)
            } else {
                CodexComposerState::Unknown
            },
        }
    }

    /// The baseline of a pane that was just created: no scrollback, no
    /// title marker. Used by the launch-seed dispatch path, where the pane
    /// was killed and recreated moments ago — any busy signal is genuinely
    /// this dispatch.
    pub fn fresh(profile: SubmitProfile) -> Self {
        PaneBaseline {
            profile,
            busy: false,
            actively_busy: false,
            title_working: false,
            codex_composer: CodexComposerState::Unknown,
        }
    }

    /// Whether the runner was visibly processing a turn at capture time.
    /// Wake schedulers use this to defer delivery until the pane is idle.
    pub fn is_actively_busy(&self) -> bool {
        self.actively_busy
    }

    /// Only a positively identified empty, idle Codex composer may receive an
    /// autonomous board wake. Unknown is deliberately not equivalent to idle.
    pub(crate) fn is_codex_wake_ready(&self) -> bool {
        self.profile == SubmitProfile::CodexUi
            && self.codex_composer == CodexComposerState::Empty
            && !self.actively_busy
    }
}

/// The verdict of a verified send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitStatus {
    /// A positive submit signal was observed. `detail` is the events.log
    /// token describing which window confirmed it: `busy_observed` (first
    /// wait) or `retry_enter` (after the retry Enter).
    Submitted { detail: &'static str },
    /// Text and Enter were delivered through the shared, race-resistant
    /// sequence, but this runner has no supported UI verifier. This is an
    /// explicit capability fallback, not proof that the worker consumed the
    /// message and not a reason to run Claude's retry heuristic.
    DeliveredUnverified { detail: &'static str },
    /// The caller's live authorization guard changed after delivery but
    /// before a retry Enter could submit text still parked in the input box.
    EligibilityRevoked,
    /// No submit signal after the retry Enter, and the text is *visibly
    /// still parked* in the input box. On an idle pane this is a stuck
    /// delivery. A caller with strong evidence the pane was actively mid-turn
    /// may treat this as Claude's visible queued-input state (the retry Enter
    /// was delivered as a clean separate keypress, so the queued text submits
    /// when the turn ends).
    StillInBox,
    /// No submit signal and no visible text either — we couldn't prove the
    /// delivery landed or that it's stuck (e.g. the input box never
    /// rendered, or a modal is covering it). Treated like a stall by every
    /// caller: never assume success.
    Unconfirmed,
}

/// Deliver `text` to the pane's input and submit it: text first (no Enter),
/// a short settle, then Enter as its own key event. This is the delivery
/// half of a verified send; pair it with [`verify_submitted`] — or call
/// [`send_verified`], which does both.
pub fn deliver_text(host: &Host, addr: &TmuxAddr, text: &str) -> Result<()> {
    // An empty Enter is not a message and Claude leaves its empty box looking
    // "cleared", which would otherwise be indistinguishable from a successful
    // submission. Reject it before touching the pane instead of recording a
    // false positive.
    if text.trim().is_empty() {
        return Err(Error::Other(
            "verified-submit refuses an empty message".to_string(),
        ));
    }
    deliver_text_with(
        || shelbi_tmux::send_text(host, addr, text),
        || std::thread::sleep(SUBMIT_SETTLE),
        || shelbi_tmux::send_enter(host, addr),
    )
}

/// Testable sequencing core for [`deliver_text`]. Keeping the settle between
/// two independent operations is load-bearing: collapsing these closures back
/// into a single `send-keys TEXT Enter` recreates the bracketed-paste race.
fn deliver_text_with(
    send_text: impl FnOnce() -> Result<()>,
    settle: impl FnOnce(),
    send_enter: impl FnOnce() -> Result<()>,
) -> Result<()> {
    send_text()?;
    settle();
    send_enter()
}

/// Deliver `text` and verify it was submitted. `baseline` must have been
/// captured BEFORE this call ([`PaneBaseline::capture`]) — the caller keeps
/// it because the right reading of [`SubmitStatus::StillInBox`] depends on
/// whether the pane was busy at baseline (queued input vs. stuck prompt).
pub fn send_verified(
    host: &Host,
    addr: &TmuxAddr,
    text: &str,
    baseline: &PaneBaseline,
) -> Result<SubmitStatus> {
    send_verified_guarded(host, addr, text, baseline, || true)
}

/// Guarded form of [`send_verified`] for lifecycle-sensitive injections.
/// The guard is checked before delivery and again before the verifier's only
/// retry Enter. This lets a caller revoke a stale task's authorization while
/// retaining the shared text/settle/Enter and submission-verification path.
pub fn send_verified_guarded(
    host: &Host,
    addr: &TmuxAddr,
    text: &str,
    baseline: &PaneBaseline,
    may_submit: impl Fn() -> bool,
) -> Result<SubmitStatus> {
    let may_submit = &may_submit;
    send_verified_guarded_with_guards(host, addr, text, baseline, may_submit, may_submit)
}

/// Guarded submit with distinct authorization for the first delivery and the
/// verifier's retry Enter. Autonomous wake delivery requires an empty composer
/// immediately before typing, but after Shelbi types its own prompt the
/// composer is intentionally occupied; reusing that empty-composer predicate
/// for the retry would revoke every legitimate dropped-Enter recovery.
pub(crate) fn send_verified_guarded_with_guards(
    host: &Host,
    addr: &TmuxAddr,
    text: &str,
    baseline: &PaneBaseline,
    may_deliver: impl Fn() -> bool,
    may_retry_enter: impl Fn() -> bool,
) -> Result<SubmitStatus> {
    guarded_delivery_with(
        may_deliver,
        || deliver_text(host, addr, text),
        || verify_submitted_guarded(host, addr, text, baseline, may_retry_enter),
    )
}

/// Testable boundary around the final pre-delivery authorization check. A
/// false guard must return before either the text or its Enter can touch tmux.
fn guarded_delivery_with(
    may_deliver: impl FnOnce() -> bool,
    deliver: impl FnOnce() -> Result<()>,
    verify: impl FnOnce() -> SubmitStatus,
) -> Result<SubmitStatus> {
    if !may_deliver() {
        return Ok(SubmitStatus::EligibilityRevoked);
    }
    deliver()?;
    Ok(verify())
}

/// Wait for the text-submitted signal; if it doesn't arrive and the text is
/// still parked in the input box, resend Enter once and wait again.
///
/// Submission is confirmed by any of the signals in
/// [`wait_for_prompt_submitted`]. The newest — the text no longer sitting in
/// claude's input box — is what keeps a genuine submit whose busy footer we
/// never caught (the earliest spinner matches no busy marker) from reading
/// as a lost prompt. The one retry Enter is gated on the text *still* being
/// parked in the box — either echoed verbatim or collapsed into a
/// `[Pasted text …]` chip (the auto-restart case, where the first Enter
/// after the paste was dropped): re-Entering an already-cleared box is
/// pointless, and re-Entering a box the user has since started typing into
/// could fire a partial message.
pub fn verify_submitted(
    host: &Host,
    addr: &TmuxAddr,
    text: &str,
    baseline: &PaneBaseline,
) -> SubmitStatus {
    verify_submitted_guarded(host, addr, text, baseline, || true)
}

fn verify_submitted_guarded(
    host: &Host,
    addr: &TmuxAddr,
    text: &str,
    baseline: &PaneBaseline,
    may_submit: impl Fn() -> bool,
) -> SubmitStatus {
    if !baseline.profile.has_ui_verifier() {
        return SubmitStatus::DeliveredUnverified {
            detail: "verification_unsupported",
        };
    }
    verify_submitted_with_profile(
        text,
        || wait_for_prompt_submitted(host, addr, text, baseline, PROMPT_SUBMIT_WAIT),
        || shelbi_tmux::capture(host, addr).unwrap_or_default(),
        || {
            if !may_submit() {
                return false;
            }
            if let Err(e) = shelbi_tmux::send_enter(host, addr) {
                eprintln!(
                    "shelbi: retry Enter to {} after stalled submit failed: {e}",
                    addr.target(),
                );
            }
            true
        },
        baseline.profile,
    )
}

/// State-machine core for [`verify_submitted`]. The injected operations make
/// the retry bound and verdicts deterministic in unit tests without waiting
/// through real tmux deadlines.
fn verify_submitted_with_profile(
    text: &str,
    mut wait_for_submit: impl FnMut() -> bool,
    mut capture: impl FnMut() -> String,
    mut retry_enter: impl FnMut() -> bool,
    profile: SubmitProfile,
) -> SubmitStatus {
    if wait_for_submit() {
        return SubmitStatus::Submitted {
            detail: "busy_observed",
        };
    }
    // No positive signal in the first window. Nudge with one retry Enter
    // only if the text is genuinely still parked in the input box. If it's
    // cleared (submitted; busy signal just missed) or we can't see the box,
    // there's nothing a retry would fix.
    if profile_input_holds_unsubmitted(&capture(), text, profile) {
        // First Enter likely raced claude's focus. Exactly one retry is
        // allowed; after that we surface StillInBox instead of spamming keys.
        if !retry_enter() {
            return SubmitStatus::EligibilityRevoked;
        }
        if wait_for_submit() {
            return SubmitStatus::Submitted {
                detail: "retry_enter",
            };
        }
        if profile_input_holds_unsubmitted(&capture(), text, profile) {
            return SubmitStatus::StillInBox;
        }
    }
    SubmitStatus::Unconfirmed
}

#[cfg(test)]
fn verify_submitted_with(
    text: &str,
    wait_for_submit: impl FnMut() -> bool,
    capture: impl FnMut() -> String,
    retry_enter: impl FnMut() -> bool,
) -> SubmitStatus {
    verify_submitted_with_profile(
        text,
        wait_for_submit,
        capture,
        retry_enter,
        SubmitProfile::ClaudeUi,
    )
}

/// Poll the pane until we have proof the text got submitted, or `timeout`
/// elapses. Capture failures during the poll are transient (the SSH socket
/// can hiccup); we just ignore them and keep polling.
///
/// Three independent signals — any one is sufficient:
///
/// 1. **Pane title flips to `shelbi:working`.** The workspace's
///    `UserPromptSubmit` hook writes it via OSC on every submit, so when the
///    title shows it, Enter definitely landed. Two caveats: claude's own
///    OSC 2 writes (a live activity summary) typically clobber the marker
///    within tens of milliseconds — so we can't rely on this as the only
///    signal — and a pane that was ALREADY `shelbi:working` at baseline
///    (message sent mid-turn) proves nothing, so the signal is suppressed
///    then. Requiring `working` specifically (not any `shelbi:*` marker)
///    matters for sends to existing panes: an idle worker's title still
///    carries the `shelbi:idle` its Stop hook wrote after the previous turn.
///
/// 2. **Pane content shows claude is actively processing.** When the text
///    has been submitted and claude is working, the pane renders a spinner
///    line like `· Booping… (10s · ↑ 2k tokens)` and an `esc to interrupt`
///    footer — none of which appear in the empty-input state. Suppressed
///    when the baseline was already busy: on a `--continue` resume the
///    replayed scrollback carries old token footers, and on a mid-turn
///    message the pane is busy with the PREVIOUS prompt — either way a busy
///    match is not proof THIS text landed.
///
/// 3. **The input box no longer holds our text.** After we type + Enter, a
///    cleared box is direct proof it was consumed. This closes the
///    false-positive gap: claude's *earliest* spinner (the first second or
///    two, before any tokens stream) matches none of the busy markers in
///    (2), so a text that submitted and started working could otherwise
///    slip past both (1) and (2) and get a spurious `enter-stalled`.
///    "Cleared" excludes a collapsed `[Pasted text …]` chip
///    ([`input_box_cleared`] / [`input_holds_unsubmitted_prompt`]): a chip
///    is an un-submitted prompt whose body claude never echoes, so counting
///    it as cleared is precisely the auto-restart false positive this
///    guards against. On a busy pane this is also how queued input reads:
///    Enter on a mid-turn pane queues the message and clears the box.
fn wait_for_prompt_submitted(
    host: &Host,
    addr: &TmuxAddr,
    text: &str,
    baseline: &PaneBaseline,
    timeout: std::time::Duration,
) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if baseline.profile == SubmitProfile::ClaudeUi && !baseline.title_working {
            let title = shelbi_tmux::pane_title(host, addr).unwrap_or_default();
            if title_signals_submit(&title) {
                return true;
            }
        }
        // Title-marker missed (probably clobbered by claude's own OSC, or
        // suppressed as stale). Fall back to the pane body + a little
        // scrollback — claude's busy spinner / "esc to interrupt" line is a
        // much more durable signal that Enter landed, and the scrollback
        // keeps it visible even if a burst of output has scrolled the
        // footer.
        let screen =
            shelbi_tmux::capture_history(host, addr, PROMPT_SUBMIT_SCROLLBACK).unwrap_or_default();
        if screen_shows_submitted_profile(&screen, text, baseline.busy, baseline.profile) {
            return true;
        }
        std::thread::sleep(PROMPT_SUBMIT_POLL);
    }
    false
}

/// True when a freshly-read pane title proves a submit: it parses to the
/// `shelbi:working` marker the `UserPromptSubmit` hook writes. `shelbi:idle`
/// / `shelbi:review` / `shelbi:blocked` do NOT qualify — they're what a
/// worker's title reads *between* turns, so they'd instantly false-confirm a
/// send to any idle worker.
fn title_signals_submit(title: &str) -> bool {
    matches!(
        shelbi_state::parse_pane_title_marker(title),
        Some(PaneMarker::Working)
    )
}

/// Decide, from a single captured pane screen, whether the just-delivered
/// text has been submitted. Encodes signals (2) and (3) from
/// [`wait_for_prompt_submitted`]; signal (1) (the pane title marker) is
/// polled separately because it reads the title, not the body.
///
/// - Signal (2): claude is actively processing ([`claude_is_processing`]).
///   Suppressed when `stale_busy` is set — the pane already looked busy
///   before delivery, so a busy match is NOT proof THIS text landed.
///   Counting it was the resume false-confirm: the dispatch reported
///   `busy_observed` while the resume prompt sat un-submitted at Ctx 0.
/// - Signal (3): the input box no longer holds the text
///   ([`input_box_cleared`]) — direct proof it was consumed. This one is
///   safe on resume: before delivery the box was empty (readiness passed),
///   so a cleared box after delivery can only mean the text we typed was
///   taken.
fn screen_shows_submitted_profile(
    screen: &str,
    text: &str,
    stale_busy: bool,
    profile: SubmitProfile,
) -> bool {
    let processing = match profile {
        SubmitProfile::ClaudeUi => claude_is_processing(screen),
        SubmitProfile::CodexUi => codex_is_processing(screen),
        SubmitProfile::DeliveryOnly => false,
    };
    if !stale_busy && processing {
        return true;
    }
    profile_input_box_cleared(screen, text, profile)
}

#[cfg(test)]
fn screen_shows_submitted(screen: &str, text: &str, stale_busy: bool) -> bool {
    screen_shows_submitted_profile(screen, text, stale_busy, SubmitProfile::ClaudeUi)
}

/// Minimum number of non-whitespace characters a captured input-box line must
/// share with the delivered text before we count it as "the text is still
/// sitting in the box." Short coincidental overlaps (a lone `git`, a bare
/// `2.`) must not qualify, or claude's dim placeholder — or an unrelated
/// line — could read as an un-submitted prompt.
const PROMPT_ECHO_MIN_MATCH: usize = 24;

/// Extract the lines currently shown inside claude's live input box — the
/// region between the last two horizontal-rule lines at the bottom of the
/// pane — with the leading prompt glyph stripped. Returns `None` when no
/// input box is on screen (a modal dialog, or a capture taken before claude
/// drew its box).
///
/// tmux capture uses `-J`, so tmux's own soft-wraps are already rejoined; the
/// lines we get back are claude's own rendered rows.
fn input_box_lines(screen: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = screen.lines().collect();
    let rules: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_input_box_rule(line))
        .map(|(i, _)| i)
        .collect();
    if rules.len() < 2 {
        return None;
    }
    let top = rules[rules.len() - 2];
    let bottom = rules[rules.len() - 1];
    Some(
        lines[top + 1..bottom]
            .iter()
            .map(|l| strip_input_glyph(l).trim().to_string())
            .collect(),
    )
}

/// A plain horizontal border that can fence Claude's live input box.
/// `─── text ───` title rules contain letters and are deliberately
/// excluded.
fn is_input_box_rule(line: &str) -> bool {
    const BORDER: &[char] = &['─', '╭', '╮', '╰', '╯'];
    let trimmed = line.trim();
    trimmed.chars().count() >= 3 && trimmed.chars().all(|c| BORDER.contains(&c))
}

/// Index of the top border of the last live input box in `screen`.
fn input_box_top(screen: &str) -> Option<usize> {
    let rules = screen
        .lines()
        .enumerate()
        .filter_map(|(index, line)| is_input_box_rule(line).then_some(index))
        .collect::<Vec<_>>();
    (rules.len() >= 2).then(|| rules[rules.len() - 2])
}

/// Strip claude's leading input-prompt glyph (`❯` or a plain `>`) plus any
/// following space from a captured input-box line.
fn strip_input_glyph(line: &str) -> &str {
    let t = line.trim_start();
    for g in ['❯', '>'] {
        if let Some(rest) = t.strip_prefix(g) {
            return rest.trim_start();
        }
    }
    t
}

/// Squeeze every whitespace character out of `s`. Comparing prompt text to a
/// captured input box has to survive claude's own soft-wrapping and
/// indentation, which we don't control — dropping all whitespace makes a
/// wrapped row of the prompt a clean substring of the whitespace-free prompt.
fn squeeze_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// True when the pane shows claude's input box *still holding the delivered
/// text* — the genuine "un-submitted prompt" state that warrants a retry
/// Enter / an `enter-stalled` warning.
///
/// We look only at the live input box, never the scrollback: claude keeps a
/// rendered copy of the prompt as the user message above the box even after a
/// successful submit, so a whole-screen text match would false-positive. We
/// then check whether any single box line reproduces a long-enough slice of
/// the text. Matching per-line (rather than the box as a whole) tolerates
/// claude scrolling a tall prompt so only its head or tail is visible, and a
/// truncated middle — any one verbatim prompt line is proof enough.
fn input_holds_prompt(screen: &str, text: &str) -> bool {
    let Some(box_lines) = input_box_lines(screen) else {
        return false;
    };
    let prompt_norm = squeeze_ws(text);
    if prompt_norm.is_empty() {
        return false;
    }
    let prompt_len = prompt_norm.chars().count();
    box_lines.iter().any(|line| {
        let norm = squeeze_ws(line);
        let line_len = norm.chars().count();
        if prompt_len < PROMPT_ECHO_MIN_MATCH {
            // `shelbi send` messages are often short ("please re-run the
            // test"). Requiring the dispatch-era 24-character overlap would
            // make every such message look like a cleared box even while it
            // was visibly stuck. For a short payload, require the whole
            // payload to be the complete live box line instead. A substring
            // check makes messages such as `Try` or `edit` match Claude's dim
            // empty-box placeholder (`Try "edit <filepath> to..."`) and turns
            // a successful clear into a false stuck verdict.
            norm == prompt_norm
        } else {
            // Long task prompts may be clipped or wrapped so only one
            // sufficiently distinctive row is visible. Accept either
            // direction: the row can be a slice of the prompt, or it can
            // contain our entire short rendered paragraph plus UI text.
            line_len >= PROMPT_ECHO_MIN_MATCH
                && (prompt_norm.contains(&norm) || norm.contains(&prompt_norm))
        }
    })
}

/// True when the input box holds a *collapsed paste chip* — claude renders a
/// large multi-line paste (like the re-injected task prompt: dozens of lines)
/// not by echoing its body but as a single `[Pasted text #1 +45 lines]`
/// placeholder. The pasted prompt is sitting un-submitted in the box, but none
/// of its text is on screen for [`input_holds_prompt`]'s per-line match to
/// catch — so without this the chip reads as a *cleared* box and the dispatch
/// false-confirms a prompt that never went in. This is exactly the auto-restart
/// failure: the pane came up as `❯ [Pasted text #1 +45 lines]`, the Enter that
/// should have submitted it was dropped, and the confirmation could not tell
/// the un-submitted chip apart from a cleared box.
fn input_holds_pasted_chip(screen: &str) -> bool {
    input_box_lines(screen)
        .map(|lines| lines.iter().any(|l| is_pasted_chip(l)))
        .unwrap_or(false)
}

/// True for claude's collapsed-paste placeholder line, e.g.
/// `[Pasted text #1 +45 lines]`. Matched structurally (bracketed, "Pasted
/// text" prefix) rather than by exact wording so a minor label drift across
/// claude versions still registers as "a paste is parked here."
fn is_pasted_chip(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("[Pasted text") && t.ends_with(']')
}

/// True when the input box still holds an un-submitted prompt — either the
/// text is echoed verbatim ([`input_holds_prompt`]) OR claude collapsed a
/// large multi-line paste into a `[Pasted text …]` chip
/// ([`input_holds_pasted_chip`]). Both mean Enter has not landed; the chip
/// case is the one the auto-restart bug hit.
pub fn input_holds_unsubmitted_prompt(screen: &str, text: &str) -> bool {
    input_holds_prompt(screen, text) || input_holds_pasted_chip(screen)
}

/// True when claude's input box is on screen but *not* holding the text —
/// empty, or showing only its dim placeholder. After we've typed and Enter'd,
/// a cleared box is direct proof it was consumed (submitted). The
/// `input_box_lines(..).is_some()` guard keeps a capture that missed the box
/// entirely from reading as "cleared." A collapsed paste chip is NOT cleared —
/// it's an un-submitted prompt ([`input_holds_unsubmitted_prompt`]).
fn input_box_cleared(screen: &str, text: &str) -> bool {
    input_box_lines(screen).is_some() && !input_holds_unsubmitted_prompt(screen, text)
}

fn profile_input_holds_unsubmitted(screen: &str, text: &str, profile: SubmitProfile) -> bool {
    match profile {
        SubmitProfile::ClaudeUi => input_holds_unsubmitted_prompt(screen, text),
        SubmitProfile::CodexUi => codex_input_holds_prompt(screen, text),
        SubmitProfile::DeliveryOnly => false,
    }
}

fn profile_input_box_cleared(screen: &str, text: &str, profile: SubmitProfile) -> bool {
    match profile {
        SubmitProfile::ClaudeUi => input_box_cleared(screen, text),
        SubmitProfile::CodexUi => {
            codex_input_line(screen).is_some() && !codex_input_holds_prompt(screen, text)
        }
        SubmitProfile::DeliveryOnly => false,
    }
}

/// Capture the minimum live Codex UI needed by the last-moment autonomous
/// wake guard. Failure is unsafe: unlike submit verification, this path may
/// not degrade an unavailable capture into an apparently empty composer.
pub(crate) fn codex_wake_ready(host: &Host, addr: &TmuxAddr) -> bool {
    shelbi_tmux::capture(host, addr)
        .ok()
        .is_some_and(|screen| codex_screen_wake_ready(&screen))
}

fn codex_screen_wake_ready(screen: &str) -> bool {
    !codex_is_processing(screen) && codex_composer_state(Some(screen)) == CodexComposerState::Empty
}

/// Wake-specific authorization for the verifier's one retry Enter. The first
/// Enter may have been dropped after Shelbi typed the wake, so the composer is
/// expected to be occupied here. Retry only when a fresh successful capture
/// shows exactly Shelbi's prompt and no active turn; appended user text,
/// partial/ambiguous UI, and capture failure all revoke the keypress.
pub(crate) fn codex_wake_retry_ready(host: &Host, addr: &TmuxAddr, text: &str) -> bool {
    let screen = shelbi_tmux::capture(host, addr).ok();
    codex_wake_retry_ready_from_capture(screen.as_deref(), text)
}

fn codex_wake_retry_ready_from_capture(screen: Option<&str>, text: &str) -> bool {
    let Some(screen) = screen else {
        return false;
    };
    if codex_is_processing(screen) {
        return false;
    }
    let Some(line) = codex_wake_retry_composer_line(screen) else {
        return false;
    };
    let line = squeeze_ws(line);
    let text = squeeze_ws(text);
    !text.is_empty() && line == text
}

/// Locate an unchanged single-row wake in Codex's live draft composer. The
/// normal draft-mode context footer anchors the region; a matching history row
/// under a modal is not enough. Requiring every row between the prompt and
/// footer to be blank also rejects hard-newline user text and attachments.
fn codex_wake_retry_composer_line(screen: &str) -> Option<&str> {
    const TAIL_LINES: usize = 8;

    let lines = screen.lines().collect::<Vec<_>>();
    let visible_end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|index| index + 1)?;
    let footer = visible_end.checked_sub(1)?;
    let footer_lower = lines[footer].to_ascii_lowercase();
    if !(footer_lower.contains("context left") || footer_lower.contains("context used")) {
        return None;
    }

    let tail_start = visible_end.saturating_sub(TAIL_LINES);
    let candidates = (tail_start..footer)
        .filter_map(|index| codex_composer_line(lines[index]).map(|line| (index, line)))
        .collect::<Vec<_>>();
    let [(composer, line)] = candidates.as_slice() else {
        return None;
    };
    if lines[composer + 1..footer]
        .iter()
        .any(|line| !line.trim().is_empty())
    {
        return None;
    }
    Some(line)
}

/// Read the live Codex composer conservatively from a plain tmux capture.
///
/// Codex's placeholder is dim only while its textarea is empty, but ordinary
/// capture strips that style. Its `? for shortcuts` footer supplies a second,
/// semantic signal: Codex shows that hint in its empty-composer mode and hides
/// it once a draft exists. We require both in the bottom UI region. A bare
/// prompt glyph, a placeholder without the footer, multiple candidate rows,
/// a missing composer, or capture failure is Unknown rather than Empty.
fn codex_composer_state(screen: Option<&str>) -> CodexComposerState {
    const TAIL_LINES: usize = 8;
    const COMPOSER_ROWS_BEFORE_FOOTER: usize = 4;

    let Some(screen) = screen else {
        return CodexComposerState::Unknown;
    };
    let lines = screen.lines().collect::<Vec<_>>();
    let Some(visible_end) = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map(|index| index + 1)
    else {
        return CodexComposerState::Unknown;
    };
    let tail_start = visible_end.saturating_sub(TAIL_LINES);
    // The footer must be the last non-empty row. If a dialog or popup is
    // rendered below it, the underlying composer is not the live input target.
    let footer = visible_end
        .checked_sub(1)
        .filter(|index| lines[*index].contains("? for shortcuts"));

    if let Some(footer) = footer {
        let composer_start = footer
            .saturating_sub(COMPOSER_ROWS_BEFORE_FOOTER)
            .max(tail_start);
        let candidates = (composer_start..footer)
            .filter_map(|index| codex_composer_line(lines[index]).map(|line| (index, line)))
            .collect::<Vec<_>>();
        if candidates.len() != 1 {
            return CodexComposerState::Unknown;
        }
        let text = candidates[0].1.trim();
        return if codex_is_empty_composer_placeholder(text) {
            CodexComposerState::Empty
        } else if text.is_empty() {
            CodexComposerState::Unknown
        } else {
            CodexComposerState::Occupied
        };
    }

    // Without the empty-mode footer, a non-empty row is still positive
    // evidence of a draft. The placeholder or a bare glyph alone is
    // ambiguous, because capture may have missed or truncated the footer.
    let candidate = lines[tail_start..visible_end]
        .iter()
        .rev()
        .find_map(|line| codex_composer_line(line));
    match candidate.map(str::trim) {
        Some(text) if !text.is_empty() && !codex_is_empty_composer_placeholder(text) => {
            CodexComposerState::Occupied
        }
        _ => CodexComposerState::Unknown,
    }
}

fn codex_is_empty_composer_placeholder(text: &str) -> bool {
    CODEX_EMPTY_COMPOSER_PLACEHOLDERS.contains(&text)
}

fn codex_composer_line(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix('›').map(str::trim_start)
}

/// Codex renders its live composer as a bottom-of-screen `› …` row rather
/// than Claude's bordered box. Restrict the match to the tail so a submitted
/// `› prompt` in conversation history cannot be mistaken for parked input.
fn codex_input_line(screen: &str) -> Option<&str> {
    let lines = screen.lines().collect::<Vec<_>>();
    let visible_end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |index| index + 1);
    lines[..visible_end]
        .iter()
        .rev()
        .take(8)
        .find_map(|line| codex_composer_line(line))
}

fn codex_input_holds_prompt(screen: &str, text: &str) -> bool {
    let Some(line) = codex_input_line(screen) else {
        return false;
    };
    let line = squeeze_ws(line);
    let text = squeeze_ws(text);
    if line.starts_with("[PastedContent") || line.starts_with("[Pastedtext") {
        return true;
    }
    !text.is_empty()
        && if text.chars().count() < PROMPT_ECHO_MIN_MATCH {
            line == text
        } else {
            line.chars().count() >= PROMPT_ECHO_MIN_MATCH
                && (text.contains(&line) || line.contains(&text))
        }
}

/// Stable Codex current-turn chrome. Unlike Claude's historical token footer,
/// this interrupt hint is present only while a turn is running and disappears
/// when the live composer becomes available again.
fn codex_is_processing(screen: &str) -> bool {
    let lower = screen.to_ascii_lowercase();
    lower.contains("esc to interrupt") || lower.contains("ctrl+c to stop")
}

/// True when the captured pane shows claude is actively processing a
/// prompt — the prompt-submitted state, as distinct from an empty input
/// box waiting for the user to type something.
///
/// Why these markers are the right ones: each appears ONLY after a
/// prompt has been submitted and claude has started work, and NONE of
/// them appear on the empty-input / ready-for-typing screen. So a match
/// here is sufficient to conclude Enter landed. We avoid keying on the
/// prompt body text (claude's history scrollback contains it in both
/// "submitted" and "still in input" states, depending on how the pane
/// wrapped) and avoid keying on the static input footer (`shift+tab to
/// cycle`, `for shortcuts`) — those persist across both states.
pub(crate) fn claude_is_processing(screen: &str) -> bool {
    // Lowercase compare so "ESC to interrupt" / "esc to interrupt" both
    // match — Claude's footer phrasing has drifted across versions.
    // NB: do NOT add "esc to cancel" here — the trust-this-folder dialog
    // uses that exact string, and we'd otherwise read the dialog as
    // "prompt submitted" before the user has cleared it.
    let lower = screen.to_ascii_lowercase();
    const BUSY_MARKERS: &[&str] = &[
        "esc to interrupt", // claude's "currently working" footer
        "ctrl+c to stop",   // some older versions
        // Claude's spinner line ends with `(<duration> · ↑ <n> tokens)` or
        // `(<duration> · ↓ <n> tokens)` once tokens have streamed. Either
        // direction is proof a prompt got submitted and claude is mid-turn.
        "tokens)",
    ];
    BUSY_MARKERS.iter().any(|m| lower.contains(m))
}

/// Strong current-turn signal used only for busy-pane queue classification.
/// A normal busy Claude pane can omit the interrupt footer and have its
/// `shelbi:working` title overwritten, leaving only the live spinner row. We
/// accept that row when it is immediately above the live input box and has
/// Claude's streaming-token grammar. A completed `⏺ Done. (... tokens)` row
/// is not a spinner and therefore stays false.
fn claude_is_actively_processing(screen: &str) -> bool {
    claude_is_interruptible(screen) || claude_has_live_spinner(screen)
}

fn claude_has_live_spinner(screen: &str) -> bool {
    let Some(top) = input_box_top(screen) else {
        return false;
    };
    let lines = screen.lines().collect::<Vec<_>>();
    let Some(line) = lines[..top]
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
    else {
        return false;
    };
    let line = line.trim();
    let spinner_glyph = line.chars().next().is_some_and(|glyph| {
        matches!(
            glyph,
            '·' | '✳' | '✻' | '✶' | '✽' | '✺' | '✹' | '✸' | '✷' | '✵'
        )
    });
    spinner_glyph
        && line.contains('…')
        && (line.contains(" · ↑ ") || line.contains(" · ↓ "))
        && line.ends_with("tokens)")
}

/// Interrupt footer signal. Unlike [`claude_is_processing`], this excludes a
/// bare `tokens)` match because completed-turn footers remain in idle history.
fn claude_is_interruptible(screen: &str) -> bool {
    let lower = screen.to_ascii_lowercase();
    lower.contains("esc to interrupt") || lower.contains("ctrl+c to stop")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    fn runner(command: &str) -> AgentRunnerSpec {
        AgentRunnerSpec {
            command: command.into(),
            flags: Vec::new(),
            prompt_injection: None,
            dialog_signatures: Vec::new(),
        }
    }

    // Captured from a workspace pane that had just submitted its prompt and
    // was mid-turn — used to pin the busy-state heuristic against
    // claude's actual rendered output. The point of this whole helper is
    // that nothing here mentions `shelbi:` anywhere: claude's own OSC 2
    // writes have already clobbered the workspace's `shelbi:working` title
    // marker, so the pane-title probe would have missed this state.
    const BUSY_SCREEN_SPINNER: &str = "\
✻ Brewed for 1m 1s · 2 shells, 1 monitor still running

· Booping… (7m 16s · ↑ 19.8k tokens)
─────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────
  Model: Opus 4.7 | Ctx Used: 17.0% | Cost: $4.69
  ⏵⏵ auto mode on (shift+tab to cycle)";

    const BUSY_SCREEN_ESC_FOOTER: &str = "\
⏺ Update(crates/shelbi-orchestrator/src/review.rs)
  ⎿  Added 1 line

✳ Working on the fix...
─────────────────────────────────────────────────────
❯
─────────────────────────────────────────────────────
  esc to interrupt · ctrl+c twice to exit";

    // The readiness detection (input-box vs trust dialog) lives in
    // `crate::ready`, but the `claude_is_processing` tests below still use
    // these two real captures as negative cases — a live/empty input box
    // and a trust dialog are both NOT the "mid-turn processing" state.
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
    fn delivery_sends_text_settles_then_sends_enter_as_separate_operations() {
        let calls = RefCell::new(Vec::new());
        deliver_text_with(
            || {
                calls.borrow_mut().push("text");
                Ok(())
            },
            || calls.borrow_mut().push("settle"),
            || {
                calls.borrow_mut().push("enter");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(*calls.borrow(), ["text", "settle", "enter"]);
    }

    #[test]
    fn delivery_rejects_empty_text_before_touching_the_pane() {
        let addr = TmuxAddr {
            session: "does-not-exist".into(),
            window: "agent".into(),
        };
        let error = deliver_text(&Host::Local, &addr, "  \n\t")
            .expect_err("empty text must not false-confirm a bare Enter");
        assert!(error.to_string().contains("refuses an empty message"));
    }

    #[test]
    fn runner_profile_capability_gates_supported_ui_parsing() {
        assert_eq!(
            SubmitProfile::for_runner(&runner("/opt/bin/claude")),
            SubmitProfile::ClaudeUi
        );
        assert_eq!(
            SubmitProfile::for_runner(&runner("codex")),
            SubmitProfile::CodexUi
        );
        assert_eq!(
            SubmitProfile::for_runner(&runner("custom-agent")),
            SubmitProfile::DeliveryOnly
        );
    }

    #[test]
    fn non_claude_verification_returns_immediately_without_ui_assumptions() {
        let baseline = PaneBaseline::fresh(SubmitProfile::DeliveryOnly);
        let addr = TmuxAddr {
            session: "does-not-exist".into(),
            window: "agent".into(),
        };
        assert_eq!(
            verify_submitted(&Host::Local, &addr, "hello", &baseline),
            SubmitStatus::DeliveredUnverified {
                detail: "verification_unsupported"
            }
        );
    }

    #[test]
    fn codex_profile_detects_busy_idle_and_parked_input() {
        let busy = "• Working (4s)\n  esc to interrupt";
        let idle = "› Explain this codebase\n\n  ? for shortcuts";
        let parked = "› [shelbi board wake] Project events are pending. Drain them now.\n\n  ? for shortcuts";
        assert!(codex_is_processing(busy));
        assert!(!codex_is_processing(idle));
        assert!(profile_input_box_cleared(
            idle,
            "[shelbi board wake] Project events are pending. Drain them now.",
            SubmitProfile::CodexUi,
        ));
        assert!(profile_input_holds_unsubmitted(
            parked,
            "[shelbi board wake] Project events are pending. Drain them now.",
            SubmitProfile::CodexUi,
        ));
        assert_eq!(codex_composer_state(Some(idle)), CodexComposerState::Empty);
        assert!(codex_screen_wake_ready(idle));
        assert!(!codex_screen_wake_ready(busy));
    }

    #[test]
    fn codex_composer_treats_drafts_and_uncertain_captures_conservatively() {
        let draft = "› How do I recover this unsent draft?\n\n  gpt-5 · 100% context left";
        assert_eq!(
            codex_composer_state(Some(draft)),
            CodexComposerState::Occupied
        );
        assert!(!codex_screen_wake_ready(draft));

        for placeholder in CODEX_EMPTY_COMPOSER_PLACEHOLDERS {
            let screen = format!("› {placeholder}\n\n  ? for shortcuts");
            assert_eq!(
                codex_composer_state(Some(&screen)),
                CodexComposerState::Empty,
                "placeholder: {placeholder}"
            );
        }

        for screen in [
            "",
            "Do you approve this command?\n  Enter to confirm · Esc to cancel",
            "›\n\n  ? for shortcuts",
            "› Ask Codex to do anything",
            "› first candidate\n› Ask Codex to do anything\n\n  ? for shortcuts",
            "› Ask Codex to do anything\n\n  ? for shortcuts\nApprove this command?",
        ] {
            assert_eq!(
                codex_composer_state(Some(screen)),
                CodexComposerState::Unknown,
                "screen: {screen:?}"
            );
            assert!(!codex_screen_wake_ready(screen));
        }
        assert_eq!(
            codex_composer_state(None),
            CodexComposerState::Unknown,
            "capture failure must not look empty"
        );

        let unavailable = PaneBaseline::from_capture(SubmitProfile::CodexUi, "", None, "");
        assert!(!unavailable.is_codex_wake_ready());
    }

    #[test]
    fn codex_baseline_requires_empty_composer_and_no_active_turn() {
        let idle = "› Explain this codebase\n\n  ? for shortcuts";
        let ready = PaneBaseline::from_snapshots(SubmitProfile::CodexUi, idle, idle, "");
        assert!(ready.is_codex_wake_ready());

        let active_screen =
            "• Working (4s)\n  esc to interrupt\n› Explain this codebase\n\n  ? for shortcuts";
        let active =
            PaneBaseline::from_snapshots(SubmitProfile::CodexUi, active_screen, active_screen, "");
        assert!(active.is_actively_busy());
        assert!(!active.is_codex_wake_ready());
    }

    #[test]
    fn empty_codex_composer_allows_text_and_enter_delivery() {
        let idle = "› Explain this codebase\n\n  ? for shortcuts";
        let calls = RefCell::new(Vec::new());
        let status = guarded_delivery_with(
            || codex_screen_wake_ready(idle),
            || {
                deliver_text_with(
                    || {
                        calls.borrow_mut().push("text");
                        Ok(())
                    },
                    || calls.borrow_mut().push("settle"),
                    || {
                        calls.borrow_mut().push("enter");
                        Ok(())
                    },
                )
            },
            || SubmitStatus::Submitted {
                detail: "busy_observed",
            },
        )
        .unwrap();
        assert_eq!(
            status,
            SubmitStatus::Submitted {
                detail: "busy_observed"
            }
        );
        assert_eq!(*calls.borrow(), ["text", "settle", "enter"]);
    }

    #[test]
    fn occupied_or_unknown_codex_composer_is_never_touched() {
        for screen in [
            Some("› an unsent user draft\n\n  gpt-5 · 100% context left"),
            Some("Do you approve this command?\n  Enter to confirm · Esc to cancel"),
            None,
        ] {
            let calls = RefCell::new(Vec::new());
            let status = guarded_delivery_with(
                || screen.is_some_and(codex_screen_wake_ready),
                || {
                    deliver_text_with(
                        || {
                            calls.borrow_mut().push("text");
                            Ok(())
                        },
                        || calls.borrow_mut().push("settle"),
                        || {
                            calls.borrow_mut().push("enter");
                            Ok(())
                        },
                    )
                },
                || panic!("revoked delivery must not start verification"),
            )
            .unwrap();
            assert_eq!(status, SubmitStatus::EligibilityRevoked);
            assert!(calls.borrow().is_empty(), "screen was touched: {screen:?}");
        }
    }

    #[test]
    fn codex_draft_appearing_after_baseline_revokes_delivery() {
        let idle = "› Explain this codebase\n\n  ? for shortcuts";
        let draft = "› user started typing after baseline\n\n  gpt-5 · 100% context left";
        let baseline = PaneBaseline::from_snapshots(SubmitProfile::CodexUi, idle, idle, "");
        assert!(baseline.is_codex_wake_ready());

        let calls = RefCell::new(Vec::new());
        let status = guarded_delivery_with(
            || codex_screen_wake_ready(draft),
            || {
                deliver_text_with(
                    || {
                        calls.borrow_mut().push("text");
                        Ok(())
                    },
                    || calls.borrow_mut().push("settle"),
                    || {
                        calls.borrow_mut().push("enter");
                        Ok(())
                    },
                )
            },
            || panic!("revoked delivery must not start verification"),
        )
        .unwrap();
        assert_eq!(status, SubmitStatus::EligibilityRevoked);
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn codex_dropped_submit_retries_enter_once() {
        let waits = Cell::new(0);
        let retries = Cell::new(0);
        let text = "[shelbi board wake] Project events are pending. Drain them now.";
        let parked = format!("› {text}\n\n  ? for shortcuts");
        let status = verify_submitted_with_profile(
            text,
            || {
                waits.set(waits.get() + 1);
                waits.get() == 2
            },
            || parked.clone(),
            || {
                retries.set(retries.get() + 1);
                true
            },
            SubmitProfile::CodexUi,
        );
        assert_eq!(
            status,
            SubmitStatus::Submitted {
                detail: "retry_enter"
            }
        );
        assert_eq!(retries.get(), 1);
    }

    #[test]
    fn codex_wake_retry_requires_exact_own_prompt_and_successful_capture() {
        let text = "[shelbi board wake] Project events are pending. Drain them now.";
        let exact = format!("› {text}\n\n  gpt-5 · 100% context left");
        assert!(codex_wake_retry_ready_from_capture(Some(&exact), text));

        for screen in [
            Some(format!("› {text} user draft\n\n  gpt-5 · 100% context left")),
            Some(format!(
                "› {text}\n  user draft\n\n  gpt-5 · 100% context left"
            )),
            Some("› Project events are pending.\n\n  gpt-5 · 100% context left".into()),
            Some(format!("• Working\n  esc to interrupt\n› {text}")),
            Some(format!(
                "› {text}\n\nApprove this command?\nEnter to confirm · Esc to cancel"
            )),
            None,
        ] {
            assert!(
                !codex_wake_retry_ready_from_capture(screen.as_deref(), text),
                "retry unexpectedly authorized for {screen:?}"
            );
        }
    }

    #[test]
    fn codex_wake_retry_does_not_enter_a_prompt_with_appended_user_text() {
        let text = "[shelbi board wake] Project events are pending. Drain them now.";
        for combined in [
            format!("› {text} user draft\n\n  gpt-5 · 100% context left"),
            format!("› {text}\n  user draft\n\n  gpt-5 · 100% context left"),
        ] {
            let retries = Cell::new(0);
            let status = verify_submitted_with_profile(
                text,
                || false,
                || combined.clone(),
                || {
                    if !codex_wake_retry_ready_from_capture(Some(&combined), text) {
                        return false;
                    }
                    retries.set(retries.get() + 1);
                    true
                },
                SubmitProfile::CodexUi,
            );
            assert_eq!(status, SubmitStatus::EligibilityRevoked);
            assert_eq!(retries.get(), 0, "combined composer received Enter");
        }
    }

    fn visible_short_message(text: &str) -> String {
        format!(
            "────────────────────────────────────────────────────\n\
             ❯ {text}\n\
             ────────────────────────────────────────────────────\n\
               ? for shortcuts"
        )
    }

    #[test]
    fn verifier_confirms_first_attempt_without_retry_across_twenty_trials() {
        for _ in 0..20 {
            let retries = Cell::new(0);
            let status = verify_submitted_with(
                "idle pane note",
                || true,
                || panic!("confirmed delivery must not need a screen capture"),
                || {
                    retries.set(retries.get() + 1);
                    true
                },
            );
            assert_eq!(
                status,
                SubmitStatus::Submitted {
                    detail: "busy_observed"
                }
            );
            assert_eq!(retries.get(), 0);
        }
    }

    #[test]
    fn verified_submit_drives_terminal_fixture_across_twenty_trials() {
        // A tiny terminal fixture that behaves like an idle Claude input box:
        // draw the box, block reading one line, then replace it with a busy
        // footer once Enter is received. This exercises the real tmux text +
        // separate Enter calls, not merely the state-machine closures above.
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake-claude.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             stty -echo\n\
             printf '\\033[2J\\033[H────────────────────────────────────────\\n❯\\n────────────────────────────────────────\\n  ? for shortcuts\\n'\n\
             IFS= read -r line\n\
             printf '\\033[2J\\033[H✳ Working on message\\n────────────────────────────────────────\\n❯\\n────────────────────────────────────────\\n  esc to interrupt\\n'\n\
             sleep 2\n",
        )
        .unwrap();

        for trial in 0..20 {
            let session = format!("shelbi-submit-test-{}-{trial}", std::process::id());
            let started = std::process::Command::new("tmux")
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    &session,
                    "-n",
                    "agent",
                    "sh",
                    script.to_str().unwrap(),
                ])
                .status();
            let Ok(started) = started else {
                // tmux is optional in development/test containers.
                return;
            };
            if !started.success() {
                // The workspace sandbox denies tmux socket access. The full
                // CI/local run (where tmux can create a server) executes all
                // twenty trials; match the repo's existing optional-tmux
                // convention when no server can be created.
                return;
            }

            let addr = TmuxAddr {
                session: session.clone(),
                window: "agent".into(),
            };
            let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < ready_deadline {
                if shelbi_tmux::capture(&Host::Local, &addr)
                    .unwrap_or_default()
                    .contains("? for shortcuts")
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }

            let baseline = PaneBaseline::capture(&Host::Local, &addr, SubmitProfile::ClaudeUi);
            let result = send_verified(
                &Host::Local,
                &addr,
                &format!("idle message trial {trial}"),
                &baseline,
            );
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", &session])
                .status();
            assert_eq!(
                result.unwrap(),
                SubmitStatus::Submitted {
                    detail: "busy_observed"
                },
                "trial {trial} did not submit"
            );
        }
    }

    /// Opt-in acceptance path against the actual Claude Code TUI. Unlike the
    /// lightweight terminal fixture above, this test has no conditional
    /// return: once explicitly selected it fails when tmux, Claude, auth,
    /// hooks, or any one of the twenty submissions is unavailable.
    ///
    /// Run serially so another live test cannot reuse the tmux server while
    /// this one is sampling titles and screens:
    ///
    /// `cargo test -p shelbi-orchestrator live_claude_idle_twenty_and_busy_queue -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore = "requires an authenticated live Claude CLI and tmux; see test docs for the exact command"]
    fn live_claude_idle_twenty_and_busy_queue() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let tmux = Command::new("tmux")
            .arg("-V")
            .status()
            .expect("live acceptance requires tmux on PATH");
        assert!(tmux.success(), "tmux -V failed");
        let claude = Command::new("claude")
            .arg("--version")
            .status()
            .expect("live acceptance requires claude on PATH");
        assert!(claude.success(), "claude --version failed");

        let tmp = tempfile::tempdir().expect("create live-Claude workdir");
        let hooks = tmp.path().join(".shelbi/hooks");
        let settings_dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::create_dir_all(&settings_dir).unwrap();
        let working = hooks.join("pane-working");
        let idle = hooks.join("pane-idle");
        std::fs::write(
            &working,
            "#!/bin/sh\nprintf '\\033]2;shelbi:working\\007'\nprintf 'working\\n' >> .shelbi/live-working.log\n",
        )
        .unwrap();
        std::fs::write(
            &idle,
            "#!/bin/sh\nprintf '\\033]2;shelbi:idle\\007'\nprintf 'idle\\n' >> .shelbi/live-idle.log\n",
        )
        .unwrap();
        for hook in [&working, &idle] {
            let mut permissions = std::fs::metadata(hook).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(hook, permissions).unwrap();
        }
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{
  "hooks": {
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": ".shelbi/hooks/pane-working"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": ".shelbi/hooks/pane-idle"}]}]
  }
}"#,
        )
        .unwrap();

        let session = format!("shelbi-live-submit-{}", std::process::id());
        struct SessionGuard(String);
        impl Drop for SessionGuard {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", &self.0])
                    .status();
            }
        }
        let guard = SessionGuard(session.clone());
        let model = std::env::var("SHELBI_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "haiku".into());
        let started = Command::new("tmux")
            .args([
                "new-session",
                "-d",
                "-s",
                &session,
                "-n",
                "agent",
                "-x",
                "120",
                "-y",
                "40",
                "-c",
                tmp.path().to_str().unwrap(),
                "claude",
                "--model",
                &model,
                "--effort",
                "low",
                "--permission-mode",
                "auto",
                "--setting-sources",
                "project",
                "--disable-slash-commands",
                "--no-chrome",
            ])
            .status()
            .expect("start live Claude tmux session");
        assert!(started.success(), "tmux could not start live Claude");

        let addr = TmuxAddr {
            session: session.clone(),
            window: "agent".into(),
        };
        assert!(
            crate::ready::wait_for_claude_ready(
                &Host::Local,
                &addr,
                std::time::Duration::from_secs(60),
            )
            .expect("probe live Claude readiness"),
            "Claude did not reach its input box; capture:\n{}",
            shelbi_tmux::capture(&Host::Local, &addr).unwrap_or_default(),
        );

        let wait_for_stops = |expected_stops: usize, label: &str, timeout_secs: u64| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
            while std::time::Instant::now() < deadline {
                let stops = std::fs::read_to_string(tmp.path().join(".shelbi/live-idle.log"))
                    .unwrap_or_default()
                    .lines()
                    .count();
                if stops >= expected_stops {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            panic!(
                "live Claude {label} never reached {expected_stops} completed turns; capture:\n{}",
                shelbi_tmux::capture(&Host::Local, &addr).unwrap_or_default(),
            );
        };

        for trial in 0..20 {
            let baseline = PaneBaseline::capture(&Host::Local, &addr, SubmitProfile::ClaudeUi);
            assert!(
                !baseline.actively_busy,
                "idle trial {trial} started from a busy pane"
            );
            let status = send_verified(
                &Host::Local,
                &addr,
                &format!("Reply with only OK-{trial}."),
                &baseline,
            )
            .unwrap();
            assert!(
                matches!(status, SubmitStatus::Submitted { .. }),
                "idle trial {trial} was not verified: {status:?}"
            );
            wait_for_stops(trial + 1, &format!("idle trial {trial}"), 90);
        }

        let baseline = PaneBaseline::capture(&Host::Local, &addr, SubmitProfile::ClaudeUi);
        let status = send_verified(
            &Host::Local,
            &addr,
            "Use Bash to run `sleep 45`, then reply with DONE.",
            &baseline,
        )
        .unwrap();
        assert!(matches!(status, SubmitStatus::Submitted { .. }));

        let busy_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let busy_baseline = loop {
            let candidate = PaneBaseline::capture(&Host::Local, &addr, SubmitProfile::ClaudeUi);
            if candidate.actively_busy {
                break candidate;
            }
            assert!(
                std::time::Instant::now() < busy_deadline,
                "Claude never entered a live busy state; capture:\n{}",
                shelbi_tmux::capture(&Host::Local, &addr).unwrap_or_default(),
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let queued = send_verified(
            &Host::Local,
            &addr,
            "After the current turn, reply with QUEUED-OK.",
            &busy_baseline,
        )
        .unwrap();
        assert!(
            matches!(
                queued,
                SubmitStatus::Submitted { .. } | SubmitStatus::StillInBox
            ),
            "busy-pane note was neither submitted nor visibly queued: {queued:?}"
        );
        // The sleep turn is completion 21. The queued note must then start a
        // real follow-up turn and reach completion 22 without another keypress.
        // This pins the accepted busy-pane contract end to end, rather than
        // merely accepting text that remains visible forever.
        wait_for_stops(22, "busy queued follow-up", 120);

        eprintln!("live verified-submit acceptance passed: idle=20/20 busy=queued-and-processed");
        drop(guard);
    }

    #[test]
    fn verifier_retries_enter_once_then_confirms() {
        let waits = RefCell::new(VecDeque::from([false, true]));
        let retries = Cell::new(0);
        let status = verify_submitted_with(
            "retry tests",
            || waits.borrow_mut().pop_front().unwrap(),
            || visible_short_message("retry tests"),
            || {
                retries.set(retries.get() + 1);
                true
            },
        );
        assert_eq!(
            status,
            SubmitStatus::Submitted {
                detail: "retry_enter"
            }
        );
        assert_eq!(retries.get(), 1);
        assert!(waits.borrow().is_empty());
    }

    #[test]
    fn verifier_bounds_retry_and_reports_visible_stuck_input() {
        let waits = RefCell::new(VecDeque::from([false, false]));
        let retries = Cell::new(0);
        let status = verify_submitted_with(
            "retry tests",
            || waits.borrow_mut().pop_front().unwrap(),
            || visible_short_message("retry tests"),
            || {
                retries.set(retries.get() + 1);
                true
            },
        );
        assert_eq!(status, SubmitStatus::StillInBox);
        assert_eq!(retries.get(), 1, "retry must be bounded to one Enter");
    }

    #[test]
    fn verifier_does_not_press_enter_when_input_cannot_be_identified() {
        let retries = Cell::new(0);
        let status = verify_submitted_with(
            "retry tests",
            || false,
            || TRUST_DIALOG_SCREEN.to_string(),
            || {
                retries.set(retries.get() + 1);
                true
            },
        );
        assert_eq!(status, SubmitStatus::Unconfirmed);
        assert_eq!(retries.get(), 0);
    }

    #[test]
    fn verifier_withholds_retry_when_authorization_is_revoked() {
        let retries = Cell::new(0);
        let status = verify_submitted_with(
            "retry tests",
            || false,
            || visible_short_message("retry tests"),
            || {
                retries.set(retries.get() + 1);
                false
            },
        );
        assert_eq!(status, SubmitStatus::EligibilityRevoked);
        assert_eq!(retries.get(), 1);
    }

    #[test]
    fn claude_is_processing_detects_busy_pane_when_title_marker_lost() {
        // Both fixtures are post-submit screens where claude is mid-turn.
        // Neither has a `shelbi:` title marker (claude's own OSC 2 writes
        // have already overwritten it), so the title-based probe alone
        // would mis-fire a `stalled` abort on a prompt that actually
        // landed. The content fallback catches both.
        assert!(claude_is_processing(BUSY_SCREEN_SPINNER));
        assert!(claude_is_processing(BUSY_SCREEN_ESC_FOOTER));
    }

    #[test]
    fn claude_is_processing_does_not_fire_on_empty_input_or_trust_dialog() {
        // The empty-input ready screen — what the pane looks like
        // BEFORE the prompt is typed. Must not match, otherwise the
        // probe declares success before we've even sent Enter.
        assert!(!claude_is_processing(INPUT_BOX_SCREEN));
        // Trust dialog before claude has accepted the first prompt —
        // the prompt would've been typed INTO this dialog instead of an
        // input box, and we want the probe to keep waiting (and the
        // trust-dismiss path to dismiss it) rather than spuriously
        // signal "submitted."
        assert!(!claude_is_processing(TRUST_DIALOG_SCREEN));
        assert!(!claude_is_processing(""));
        assert!(!claude_is_processing("➜  bob git:(main) claude"));
    }

    #[test]
    fn claude_is_processing_matches_case_insensitively() {
        // Claude's footer text has rendered both "ESC to interrupt" and
        // "esc to interrupt" across versions; we lower-case the screen
        // before matching so neither slips through.
        assert!(claude_is_processing("ESC to interrupt"));
        // The token-counter parenthetical matches in either streaming
        // direction (↑ user-prompt, ↓ tool-output).
        assert!(claude_is_processing("(12s · ↑ 1.2k tokens)"));
        assert!(claude_is_processing("(45s · ↓ 8k tokens)"));
    }

    #[test]
    fn claude_is_processing_does_not_false_positive_on_trust_dialog_footer() {
        // The trust-this-folder dialog footer reads "Enter to confirm ·
        // Esc to cancel" — that "esc to" prefix is the same one claude
        // uses in its busy footer ("esc to interrupt"). We deliberately
        // do NOT include "esc to cancel" in the busy markers because
        // the trust dialog must never read as "claude submitted my
        // prompt and is working" — the prompt was typed INTO the
        // dialog, not into claude's input. Pin that behavior so a
        // future "be more inclusive" tweak can't quietly regress it.
        assert!(!claude_is_processing("Enter to confirm · Esc to cancel"));
    }

    // A pane whose prompt is still sitting UN-submitted in the input box:
    // claude echoed the typed text but Enter never landed, so the box (the
    // region between the last two ──── rules) reproduces the prompt, wrapped
    // across a couple of rows the way claude renders it.
    const STALLED_INPUT_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ # dispatch: enter-stalled false positive — submit
  signal detector reports a stall on submitted prompts
────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)";

    fn stalled_prompt() -> String {
        // Contains the two lines the box shows above, contiguously (the box
        // wraps them, but the source prompt has them on one logical line).
        "# dispatch: enter-stalled false positive — submit \
         signal detector reports a stall on submitted prompts\n\n\
         Fix the detector."
            .to_string()
    }

    #[test]
    fn input_holds_prompt_true_when_box_still_shows_prompt() {
        // The genuine-stall case: the prompt is visibly parked in the input
        // box, so we must still be willing to warn.
        assert!(input_holds_prompt(STALLED_INPUT_SCREEN, &stalled_prompt()));
        assert!(!input_box_cleared(STALLED_INPUT_SCREEN, &stalled_prompt()));
    }

    #[test]
    fn input_holds_prompt_false_when_box_empty_or_placeholder() {
        // A submitted prompt leaves the box empty (busy pane) or showing only
        // claude's dim placeholder (idle-after-submit) — neither is the
        // prompt, so no warning. This is the false-positive the fix closes.
        let prompt = stalled_prompt();
        assert!(!input_holds_prompt(BUSY_SCREEN_SPINNER, &prompt));
        assert!(!input_holds_prompt(INPUT_BOX_SCREEN, &prompt));
        // ...and both read as a *cleared* box, our positive submit signal.
        assert!(input_box_cleared(BUSY_SCREEN_SPINNER, &prompt));
        assert!(input_box_cleared(INPUT_BOX_SCREEN, &prompt));
    }

    #[test]
    fn input_box_helpers_handle_missing_box() {
        // No rules on screen (a modal dialog, or a pre-render capture): we
        // can't locate the box, so we neither claim the prompt is stuck nor
        // claim it cleared — both stay false, keeping us from crying wolf.
        assert!(!input_holds_prompt(TRUST_DIALOG_SCREEN, &stalled_prompt()));
        assert!(!input_box_cleared(TRUST_DIALOG_SCREEN, &stalled_prompt()));
        assert!(!input_holds_prompt("", &stalled_prompt()));
        assert!(!input_box_cleared("", &stalled_prompt()));
    }

    #[test]
    fn input_holds_prompt_ignores_short_coincidental_overlap() {
        // A one-line box that only shares a short token with the prompt must
        // not trip the match — that's how the placeholder and unrelated
        // half-typed lines are kept from reading as the dispatched prompt.
        let screen = "\
────────────────────────────────────────────────────
❯ Fix the detector.
────────────────────────────────────────────────────
  ? for shortcuts";
        assert!(!input_holds_prompt(screen, &stalled_prompt()));
    }

    #[test]
    fn input_holds_short_send_only_on_full_payload_match() {
        let screen = "\
────────────────────────────────────────────────────
❯ retry tests
────────────────────────────────────────────────────
  ? for shortcuts";
        assert!(input_holds_prompt(screen, "retry tests"));
        assert!(input_holds_unsubmitted_prompt(screen, "retry tests"));
        assert!(!input_box_cleared(screen, "retry tests"));
        // A shared short word is not enough. The full delivered text must be
        // visible, which avoids treating an unrelated half-typed line as ours.
        assert!(!input_holds_prompt(screen, "please retry tests now"));
    }

    #[test]
    fn short_send_does_not_match_claudes_empty_box_placeholder() {
        for text in ["Try", "edit", "filepath"] {
            assert!(!input_holds_prompt(INPUT_BOX_SCREEN, text), "text={text}");
            assert!(input_box_cleared(INPUT_BOX_SCREEN, text), "text={text}");

            let visibly_held = visible_short_message(text);
            assert!(input_holds_prompt(&visibly_held, text), "text={text}");
            assert!(!input_box_cleared(&visibly_held, text), "text={text}");
        }
    }

    // The exact state the auto-restart bug left the pane in: claude relaunched,
    // the multi-line task prompt was pasted, but the trailing Enter was dropped
    // — so the prompt sits un-submitted, collapsed into a paste chip. Its body
    // is never echoed, so `input_holds_prompt`'s text match sees nothing and
    // (before the fix) the box read as "cleared" → false submit confirmation.
    const PASTED_CHIP_SCREEN: &str = "\
╭─── Claude Code v2.1.183 ──────────────────────────╮
│            Welcome back John!                      │
╰───────────────────────────────────────────────────╯

────────────────────────────────────────────────────
❯ [Pasted text #1 +45 lines]
────────────────────────────────────────────────────
  Ctx Used: 0.0% · Cost: $0.00";

    #[test]
    fn is_pasted_chip_matches_collapsed_paste_placeholder() {
        assert!(is_pasted_chip("[Pasted text #1 +45 lines]"));
        assert!(is_pasted_chip("  [Pasted text #12 +3 lines]  "));
        // Not a chip: the dim placeholder, an echoed prompt line, empty.
        assert!(!is_pasted_chip("Try \"edit <filepath> to...\""));
        assert!(!is_pasted_chip("# dispatch: enter-stalled false positive"));
        assert!(!is_pasted_chip(""));
    }

    #[test]
    fn pasted_chip_reads_as_unsubmitted_not_cleared() {
        // The fix: a collapsed paste chip is an UN-submitted prompt. It must
        // NOT read as a cleared box (that was the false submit signal that let
        // the restarted worker sit idle at `❯ [Pasted text #1 +45 lines]`).
        let prompt = stalled_prompt();
        assert!(input_holds_pasted_chip(PASTED_CHIP_SCREEN));
        assert!(input_holds_unsubmitted_prompt(PASTED_CHIP_SCREEN, &prompt));
        assert!(!input_box_cleared(PASTED_CHIP_SCREEN, &prompt));
        // The chip body is never echoed, so the plain text match still misses
        // it — which is exactly why the dedicated chip detector is needed.
        assert!(!input_holds_prompt(PASTED_CHIP_SCREEN, &prompt));
    }

    #[test]
    fn dim_placeholder_is_not_mistaken_for_a_paste_chip() {
        // Regression guard: claude's dim "Try …" placeholder on a genuinely
        // empty box must stay a *cleared* box (a real submit signal). Only the
        // bracketed paste chip flips a box to un-submitted.
        let prompt = stalled_prompt();
        assert!(!input_holds_pasted_chip(INPUT_BOX_SCREEN));
        assert!(!input_holds_unsubmitted_prompt(INPUT_BOX_SCREEN, &prompt));
        assert!(input_box_cleared(INPUT_BOX_SCREEN, &prompt));
        // A busy/mid-turn pane has no chip either — still cleared.
        assert!(!input_holds_pasted_chip(BUSY_SCREEN_SPINNER));
        assert!(input_box_cleared(BUSY_SCREEN_SPINNER, &prompt));
    }

    // The exact state the resume false-confirm bug left the pane in: a claude
    // `--continue` resume replayed the prior conversation into the scrollback,
    // leaving a token-usage footer (`… ↑ 19.8k tokens)`) above the box — the
    // very string `claude_is_processing` keys on — while the resume prompt we
    // just pasted sits UN-submitted in the input box (its Enter was dropped).
    // The board showed `in_progress` at Ctx 0 until a human pressed Enter
    // (observed 2026-07-09 on bravo after a `task resume`).
    const RESUMED_STALE_SCREEN: &str = "\
⏺ Read(src/main.rs)
  ⎿  Read 42 lines

⏺ Done. (7m 16s · ↑ 19.8k tokens)
────────────────────────────────────────────────────
❯ # dispatch: enter-stalled false positive — submit
  signal detector reports a stall on submitted prompts
────────────────────────────────────────────────────
  ⏵⏵ auto mode on (shift+tab to cycle)";

    #[test]
    fn resume_replay_scrollback_is_the_false_positive_without_stale_guard() {
        // Sanity: the fixture really does carry BOTH the stale token footer
        // (which trips `claude_is_processing`) AND the un-submitted prompt in
        // the box — so without the guard, `screen_shows_submitted` reads the
        // replay as "this prompt submitted." That is the bug.
        let prompt = stalled_prompt();
        assert!(claude_is_processing(RESUMED_STALE_SCREEN));
        assert!(input_holds_prompt(RESUMED_STALE_SCREEN, &prompt));
        assert!(!input_box_cleared(RESUMED_STALE_SCREEN, &prompt));
        // stale_busy = false (the pre-fix behavior / the launch-seed path):
        // the busy signal fires on replayed scrollback → false confirm.
        assert!(screen_shows_submitted(RESUMED_STALE_SCREEN, &prompt, false));
    }

    #[test]
    fn stale_busy_guard_suppresses_replayed_busy_signal_on_resume() {
        // The fix: when the pane already looked busy before we delivered the
        // prompt (a resume replay), the busy-scrollback signal is not proof
        // THIS prompt landed. With the prompt still parked in the box, the
        // guard must report "not submitted" so the retry-Enter path fires and
        // the dispatch isn't falsely confirmed.
        let prompt = stalled_prompt();
        assert!(!screen_shows_submitted(RESUMED_STALE_SCREEN, &prompt, true));
    }

    #[test]
    fn stale_busy_guard_still_confirms_once_the_box_clears() {
        // A resume prompt that genuinely submitted clears the box, even though
        // the replayed token footer is still in the scrollback. The box-cleared
        // signal (3) survives the stale-busy guard, so the real submit is still
        // confirmed — the guard only mutes the unreliable busy signal (2).
        // This is also the mid-turn message case: a note sent to a busy worker
        // gets queued (Enter on a busy pane queues the input and clears the
        // box), and the cleared box is what confirms the queueing took.
        let prompt = stalled_prompt();
        assert!(input_box_cleared(BUSY_SCREEN_SPINNER, &prompt));
        assert!(screen_shows_submitted(BUSY_SCREEN_SPINNER, &prompt, true));
    }

    #[test]
    fn fresh_dispatch_still_confirms_via_busy_signal() {
        // The launch-seed / fresh-start path passes stale_busy = false (the
        // pane was just recreated, no replay), so a genuinely busy pane whose
        // box we can't cleanly read is still confirmed via the busy signal —
        // no regression to the non-resume path.
        let prompt = stalled_prompt();
        assert!(screen_shows_submitted(
            BUSY_SCREEN_ESC_FOOTER,
            &prompt,
            false
        ));
    }

    #[test]
    fn title_signal_requires_the_working_marker() {
        // `shelbi:working` is the only marker the UserPromptSubmit hook
        // writes, so it's the only title that proves a submit. An idle
        // worker's title still carries the `shelbi:idle` its Stop hook wrote
        // after the previous turn — counting it would instantly
        // false-confirm every `shelbi send` to an idle pane.
        assert!(title_signals_submit("shelbi:working"));
        assert!(title_signals_submit("claude · shelbi:working"));
        assert!(!title_signals_submit("shelbi:idle"));
        assert!(!title_signals_submit("shelbi:review"));
        assert!(!title_signals_submit("shelbi:blocked"));
        assert!(!title_signals_submit("✳ Simmering…"));
        assert!(!title_signals_submit(""));
    }

    #[test]
    fn fresh_baseline_trusts_every_signal() {
        // The launch-seed dispatch path uses a just-recreated pane: nothing
        // on it predates the dispatch, so neither suppression may engage.
        let b = PaneBaseline::fresh(SubmitProfile::ClaudeUi);
        assert!(!b.busy);
        assert!(!b.actively_busy);
        assert!(!b.title_working);
    }

    #[test]
    fn active_queue_classification_accepts_live_spinner_not_old_tokens() {
        assert!(claude_is_actively_processing(BUSY_SCREEN_ESC_FOOTER));
        assert!(claude_is_actively_processing(BUSY_SCREEN_SPINNER));
        assert!(!claude_is_actively_processing(RESUMED_STALE_SCREEN));
        assert!(!claude_is_actively_processing(INPUT_BOX_SCREEN));

        let spinner = PaneBaseline::from_snapshots(
            SubmitProfile::ClaudeUi,
            BUSY_SCREEN_SPINNER,
            BUSY_SCREEN_SPINNER,
            "✳ Simmering…",
        );
        assert!(spinner.busy);
        assert!(spinner.actively_busy);

        let stale = PaneBaseline::from_snapshots(
            SubmitProfile::ClaudeUi,
            RESUMED_STALE_SCREEN,
            RESUMED_STALE_SCREEN,
            "shelbi:idle",
        );
        assert!(stale.busy, "old token footer still suppresses stale proof");
        assert!(!stale.actively_busy, "old token footer is not a live queue");
    }
}
