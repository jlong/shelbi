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
/// timeout. Pane-injection callers abort on timeout so they never type into an
/// unknown startup or modal screen.
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

/// True when Claude's current footer says a turn is actively running. This is
/// deliberately narrower than the dispatch verifier's scrollback heuristics:
/// old `tokens)` text can remain above a genuine limit modal, while these
/// interrupt/stop controls belong to the live busy footer the modal replaces.
pub fn is_claude_busy(screen: &str) -> bool {
    let lower = screen.to_ascii_lowercase();
    lower.contains("esc to interrupt") || lower.contains("ctrl+c to stop")
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
///
/// ## Why the substring match alone isn't enough
///
/// The signatures are plain substrings (`Do you trust the files`,
/// `Enter to confirm`, …), so a worker whose pane merely *shows* that text —
/// editing this detector's code, a runner adapter carrying trust-dialog
/// signature strings, a diff, docs, or chat about the feature — would match and
/// get flipped to `blocked reason=dialog:*`. That misfire was observed live: a
/// workspace actively editing runner-adapter code (containing the trust
/// signatures and `--dangerously-bypass-hook-trust`) with its spinner running
/// was flagged blocked. A false block is worse than a missed one: if the
/// orchestrator "clears" it with a stray Enter, that keystroke corrupts the
/// worker's live edit.
///
/// So, exactly as [`detect_usage_limit`] does, we first veto on the two signals
/// that prove the pane is *working, not stalled*: Claude's live ready input box
/// ([`is_input_ready`]) or its active-turn footer ([`is_claude_busy`], the
/// `esc to interrupt` spinner). A genuine blocking dialog replaces both, and
/// `capture` samples only the visible screen (no scrollback), so these are
/// reliable "this pane isn't behind a modal" evidence. This trades away
/// catching a hypothetical dialog that somehow coexists with a live footer (we
/// would rather miss that than block a healthy worker) for robustness against
/// the false positive.
pub fn detect_blocking_dialog(
    screen: &str,
    signatures: &[shelbi_core::DialogSignature],
) -> Option<String> {
    // A live, ready input box or an active-turn footer means the pane is
    // working — any dialog wording present is mere on-screen content, not a
    // modal the pane is blocked behind.
    if is_input_ready(screen) || is_claude_busy(screen) {
        return None;
    }
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
    /// The structurally paired current banner line. The poller uses this as a
    /// stable incident key so stale visible modal text cannot re-arm a resume
    /// after a confirmed submission or task lifecycle change.
    pub banner: String,
    /// The reset-time hint (e.g. `3pm (America/New_York)`), or `None` when the
    /// pane showed no parseable reset wording. Folded into the `paused` event
    /// as-is; the auto-resume scheduler additionally tries to turn it into a
    /// real instant via [`next_reset_instant`], degrading to a needs-human
    /// warning when it can't.
    pub reset: Option<String>,
}

/// Maximum number of rendered lines that belong to one usage-limit modal,
/// starting at its `You've hit ... limit` banner. The observed variants use
/// four to six lines; the extra room tolerates wrapping without allowing an
/// unrelated reset mention elsewhere in the pane to supply the schedule.
const USAGE_LIMIT_MODAL_MAX_LINES: usize = 12;

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
/// As a final backstop we require the pane to *not* also show Claude's live
/// ready input box ([`is_input_ready`]) or active-turn footer
/// ([`is_claude_busy`]). A genuine usage-limit modal replaces both. This
/// rejects a worker editing a modal fixture and, after a confirmed resume, old
/// modal pixels that remain visible above a current busy footer. `capture`
/// samples the visible screen only (no scrollback), so those live controls are
/// reliable "this pane is working, not stalled" signals.
pub fn detect_usage_limit(screen: &str) -> Option<UsageLimitStall> {
    // A live, ready input box means the pane is working, not stalled on a
    // modal — so any usage-limit wording present is mere content.
    if is_input_ready(screen) || is_claude_busy(screen) {
        return None;
    }
    let lines: Vec<&str> = screen.lines().collect();

    // Select the bottom-most *paired* modal, not independent occurrences of
    // limit wording and menu chrome anywhere on screen. A visible pane can
    // retain an earlier limit banner in conversation above the current one;
    // parsing the full capture would schedule against that stale reset time.
    // Bounding the suffix also prevents prose outside the rendered modal from
    // donating a reset clause to a banner that has none.
    let (banner, modal) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| is_usage_limit_banner_line(line))
        .find_map(|(start, _)| {
            let end = (start + USAGE_LIMIT_MODAL_MAX_LINES).min(lines.len());
            let block = &lines[start..end];
            block
                .iter()
                .any(|line| {
                    line.to_ascii_lowercase()
                        .contains("stop and wait for limit to reset")
                        && is_menu_option_line(line)
                })
                .then_some((*lines.get(start)?, block))
        })?;

    // The session-limit variant carries `resets ...` inline on the banner.
    // The usage-limit variant puts it on a dedicated `Your limit will reset
    // ...` footer. Read only those structural locations: an unrelated line
    // later in the bounded block must not donate a plausible-but-wrong time.
    let reset = parse_usage_limit_reset(banner).or_else(|| {
        modal
            .iter()
            .find(|line| line.to_ascii_lowercase().contains("your limit will reset"))
            .and_then(|line| parse_usage_limit_reset(line))
    });
    Some(UsageLimitStall {
        banner: banner.trim().to_string(),
        reset,
    })
}

/// True when `line` is claude's usage/session-limit banner. Requiring the
/// complete banner wording is what ties reset extraction to the current
/// rendered modal instead of a loose `usage limit` mention in conversation.
fn is_usage_limit_banner_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("you've hit your usage limit")
        || lower.contains("you've hit your session limit")
        || lower.contains("you’ve hit your usage limit")
        || lower.contains("you’ve hit your session limit")
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

/// Best-effort extraction of the reset-time hint from a bounded usage-limit
/// modal, e.g. `Your limit will reset at 3pm (America/New_York)` →
/// `3pm (America/New_York)`, or `resets 10:30am` → `10:30am`. Returns `None`
/// when no reset wording is present (claude doesn't always show one).
///
/// [`detect_usage_limit`] is responsible for selecting the current structural
/// modal before calling this helper. Passing a whole pane capture directly is
/// intentionally unsupported: stale conversation may contain an older reset
/// clause. The captured value is folded into the `paused` event and parsed by
/// [`next_reset_instant`] for scheduling.
pub fn parse_usage_limit_reset(modal: &str) -> Option<String> {
    // `to_ascii_lowercase` preserves byte length, so an index found in the
    // lowered copy addresses the same byte in the original (case-preserved)
    // screen — we search lowered but slice the original.
    let lower = modal.to_ascii_lowercase();
    // Anchor on the reset-*time* wording. Deliberately NOT a bare "reset":
    // the modal's own option text ("Stop and wait for limit to reset") ends
    // in "reset", and anchoring there would capture the next menu option
    // instead of the time. The "reset(s) at" form consumes the " at "
    // connective; "resets" alone covers "resets 3pm". Earliest anchor wins.
    let (idx, klen) = ["resets at", "reset at", "resets"]
        .iter()
        .filter_map(|k| lower.find(k).map(|i| (i, k.len())))
        .min_by_key(|(i, _)| *i)?;
    let rest = modal[idx + klen..].trim_start();
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

/// Turn a scraped reset hint (see [`parse_usage_limit_reset`]) into the
/// concrete UTC instant the limit window resets, e.g.
/// `7:20am (America/New_York)` first observed at 06:00 UTC → today 11:20
/// UTC. This is what the poller's auto-resume schedules against. The
/// reference instant must be when this stall was first observed (persisted
/// across poller restarts), not the time a restarted poller happened to
/// rebuild its in-memory schedule.
///
/// Deliberately conservative — `None` means "don't guess" and the caller
/// surfaces a needs-human warning instead of resuming at a wrong time:
///
/// - The leading token must parse as a wall-clock time (`7:20am`, `3pm`,
///   `9:00`). A bare hour with no meridiem/colon (`resets 3`) is ambiguous →
///   `None`. So is date-first wording (`resets Jul 15 at 3am`) — a multi-day
///   window must not be collapsed onto today's clock.
/// - A parenthesized zone must be a recognizable IANA name
///   (`(America/New_York)`); anything else (`(ET)`) → `None` rather than
///   silently reinterpreting the time in the hub's zone. No parens at all may
///   use the hub's local zone only when `allow_local_implied` is true. Callers
///   must pass false for remote panes, whose timezone may differ from the hub.
///
/// The reset is the *next* occurrence of that wall time: today if still
/// ahead of `reference`, else tomorrow. A reset reconstructed after a poller
/// restart may therefore be before the caller's current time, which correctly
/// makes the persisted schedule immediately due. Ambiguous fall-back times
/// choose the later occurrence (safe-late); nonexistent spring-forward times
/// return `None` rather than guessing.
pub fn next_reset_instant(
    hint: &str,
    reference: chrono::DateTime<chrono::Utc>,
    allow_local_implied: bool,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let time = parse_wall_time(hint)?;
    match parse_hint_zone(hint) {
        HintZone::Named(tz) => next_occurrence(time, &tz, reference),
        HintZone::LocalImplied if allow_local_implied => {
            next_occurrence(time, &chrono::Local, reference)
        }
        HintZone::LocalImplied => None,
        HintZone::Unrecognized => None,
    }
}

/// The timezone a reset hint carries. Split three ways because the two
/// failure-ish shapes must behave differently: no parens at all means claude
/// printed a bare time (assume the hub's zone), while parens holding
/// something we can't map to an IANA zone means we *know* the time is in a
/// zone we can't resolve — guessing would risk a wrong-time resume.
enum HintZone {
    Named(chrono_tz::Tz),
    LocalImplied,
    Unrecognized,
}

fn parse_hint_zone(hint: &str) -> HintZone {
    let Some(start) = hint.find('(') else {
        return HintZone::LocalImplied;
    };
    let Some(len) = hint[start + 1..].find(')') else {
        return HintZone::Unrecognized;
    };
    let end = start + 1 + len;
    if !hint[end + 1..].trim().is_empty() {
        return HintZone::Unrecognized;
    }
    match hint[start + 1..start + 1 + len].trim().parse() {
        Ok(tz) => HintZone::Named(tz),
        Err(_) => HintZone::Unrecognized,
    }
}

/// Parse the complete pre-timezone phrase of a reset hint as a wall-clock
/// time: `7:20am`, `7:20 PM`, `3pm`, `9:00`. Returns `None` for anything
/// ambiguous, including a bare hour, an out-of-range value, or trailing date
/// wording (`7:20am tomorrow`). Consuming the complete phrase is deliberate:
/// silently ignoring a separated `PM` would schedule twelve hours early.
fn parse_wall_time(hint: &str) -> Option<chrono::NaiveTime> {
    let clock_phrase = match hint.find('(') {
        Some(start) => {
            let close = hint[start + 1..].find(')')? + start + 1;
            if !hint[close + 1..].trim().is_empty() {
                return None;
            }
            hint[..start].trim().trim_end_matches(',').trim()
        }
        None => hint.trim().trim_end_matches(',').trim(),
    };
    let mut parts = clock_phrase.split_whitespace();
    let clock = parts.next()?.trim_matches(',').to_ascii_lowercase();
    let separate_meridiem = parts.next().map(str::to_ascii_lowercase);
    if parts.next().is_some() {
        return None;
    }
    let (digits, attached_meridiem) = if let Some(d) = clock.strip_suffix("am") {
        (d, Some(false))
    } else if let Some(d) = clock.strip_suffix("pm") {
        (d, Some(true))
    } else {
        (clock.as_str(), None)
    };
    let meridiem = match (attached_meridiem, separate_meridiem.as_deref()) {
        (Some(value), None) => Some(value),
        (None, Some("am")) => Some(false),
        (None, Some("pm")) => Some(true),
        (None, None) => None,
        _ => return None,
    };
    let (h_str, m_str) = digits.split_once(':').unwrap_or((digits, "0"));
    let hour: u32 = h_str.parse().ok()?;
    let minute: u32 = m_str.parse().ok()?;
    let hour = match meridiem {
        // 12-hour clock: `12am` is midnight, `12pm` is noon.
        Some(_) if !(1..=12).contains(&hour) => return None,
        Some(false) => hour % 12,
        Some(true) => hour % 12 + 12,
        // A bare number with no colon (`resets 3`) is too ambiguous to
        // schedule on; a colon form (`9:00`) reads as 24-hour time.
        None if !digits.contains(':') => return None,
        None => hour,
    };
    chrono::NaiveTime::from_hms_opt(hour, minute, 0)
}

/// The next occurrence of wall time `t` in zone `tz`, strictly after
/// `reference`, as a UTC instant.
fn next_occurrence<Tz: chrono::TimeZone>(
    t: chrono::NaiveTime,
    tz: &Tz,
    reference: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let today = reference.with_timezone(tz).date_naive();
    let mut candidate = resolve_local(tz, today, t)?;
    if candidate <= reference {
        candidate = resolve_local(tz, today.succ_opt()?, t)?;
    }
    Some(candidate)
}

/// Resolve a local date+time in `tz` to UTC conservatively across DST edges:
/// an ambiguous fall-back time takes the later reading so an auto-resume can
/// be late but never an hour early; a nonexistent spring-forward time is
/// rejected rather than shifted to a guessed instant.
fn resolve_local<Tz: chrono::TimeZone>(
    tz: &Tz,
    date: chrono::NaiveDate,
    t: chrono::NaiveTime,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let naive = date.and_time(t);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&chrono::Utc)),
        chrono::LocalResult::Ambiguous(_, later) => Some(later.with_timezone(&chrono::Utc)),
        chrono::LocalResult::None => None,
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
    fn detect_blocking_dialog_ignores_dialog_strings_while_working() {
        // THE REGRESSION (observed live 2026-07-15): a worker editing code that
        // carries the trust/permission signature strings must NOT be flagged
        // blocked while its pane shows it is actively working. The dialog
        // wording is on screen as source content, not as a modal.
        let sigs = shelbi_core::default_dialog_signatures("claude");

        // Active turn: the spinner footer (`esc to interrupt`) vetoes.
        let busy_editing = "\
    DialogSignature::new(\"trust\", \"Do you trust the files\"),
    DialogSignature::new(\"permission\", \"Enter to confirm\"),
    // flag: --dangerously-bypass-hook-trust

· Working… (esc to interrupt)";
        assert!(is_claude_busy(busy_editing));
        assert!(detect_blocking_dialog(busy_editing, &sigs).is_none());

        // Idle at the input box: the ready footer vetoes just the same.
        let ready_editing = format!(
            "{busy_editing}\n  ⏵⏵ accept edits on (shift+tab to cycle)",
        );
        assert!(is_input_ready(&ready_editing));
        assert!(detect_blocking_dialog(&ready_editing, &sigs).is_none());

        // Sanity: the genuine dialog (no live footer) is still detected.
        assert_eq!(
            detect_blocking_dialog(TRUST_DIALOG_SCREEN, &sigs).as_deref(),
            Some("trust")
        );
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
        assert!(
            detect_usage_limit(code).is_none(),
            "source literal must not detect"
        );

        // Prose / docs describing the behavior.
        let docs = "When Claude Code hits your usage limit, it prints \"usage limit reached\" and \
                    waits until the limit resets at the top of the hour.";
        assert!(
            detect_usage_limit(docs).is_none(),
            "docs prose must not detect"
        );

        // An agent's own chat mentioning it while working on this task.
        let chat = "I'll add detection for the usage limit stall — the 'Stop and wait for limit \
                    to reset' option — and show a pause badge.";
        assert!(
            detect_usage_limit(chat).is_none(),
            "agent chat must not detect"
        );

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
    fn detect_usage_limit_ignores_stale_modal_pixels_above_live_busy_footer() {
        let resumed = format!("{SESSION_LIMIT_MODAL_SCREEN}\n\n· Working…\n  esc to interrupt");
        assert!(is_claude_busy(&resumed));
        assert!(
            detect_usage_limit(&resumed).is_none(),
            "a current busy footer means the old modal is no longer blocking"
        );
    }

    #[test]
    fn is_menu_option_line_requires_menu_chrome() {
        assert!(is_menu_option_line(
            " ❯ 1. Stop and wait for limit to reset"
        ));
        assert!(is_menu_option_line("   2. Upgrade your plan"));
        assert!(is_menu_option_line("10. tenth option"));
        // A bare mention with no menu framing is not a menu option.
        assert!(!is_menu_option_line(
            "DialogSignature::new(\"usage-limit\", \"Stop and wait for limit to reset\"),"
        ));
        assert!(!is_menu_option_line(
            "the option: Stop and wait for limit to reset"
        ));
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

    // The session-limit variant of the same modal — different limit wording,
    // same menu chrome. Observed 2026-07-11 (alpha): the pane stalled on
    // "session limit" with an inline `resets <time> (<tz>)` clause.
    const SESSION_LIMIT_MODAL_SCREEN: &str = "\
⏱ You've hit your session limit · resets 7:20am (America/New_York)

 ❯ 1. Stop and wait for limit to reset
   2. Upgrade your plan";

    #[test]
    fn detect_usage_limit_matches_the_session_limit_wording() {
        let stall = detect_usage_limit(SESSION_LIMIT_MODAL_SCREEN).expect("modal must detect");
        assert_eq!(stall.reset.as_deref(), Some("7:20am (America/New_York)"));
    }

    #[test]
    fn detect_usage_limit_parses_only_the_bottom_most_structural_modal() {
        // A pane can retain the previous limit incident above the current
        // modal. The old implementation searched the whole capture and took
        // the earliest `reset(s)` clause, scheduling this pane for 3pm even
        // though the live modal says 7:20am.
        let screen = format!(
            "{USAGE_LIMIT_MODAL_SCREEN}\n\nLimit reset; work continued.\n\n{SESSION_LIMIT_MODAL_SCREEN}"
        );
        let stall = detect_usage_limit(&screen).expect("current modal must detect");
        assert_eq!(stall.reset.as_deref(), Some("7:20am (America/New_York)"));
    }

    #[test]
    fn detect_usage_limit_does_not_borrow_reset_from_an_earlier_modal() {
        let current_without_reset = "\
⏱ You've hit your session limit

 ❯ 1. Stop and wait for limit to reset
   2. Upgrade your plan

 unrelated build cache resets 9:45am (America/New_York)";
        let screen = format!("{USAGE_LIMIT_MODAL_SCREEN}\n\n{current_without_reset}");
        let stall = detect_usage_limit(&screen).expect("current modal must detect");
        assert_eq!(stall.reset, None);
        assert!(stall.banner.contains("session limit"));
    }

    #[test]
    fn parse_wall_time_accepts_clock_forms_and_rejects_ambiguity() {
        let t = |h, m| chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap();
        assert_eq!(parse_wall_time("7:20am (America/New_York)"), Some(t(7, 20)));
        assert_eq!(parse_wall_time("10:30pm"), Some(t(22, 30)));
        assert_eq!(parse_wall_time("3pm (America/New_York)"), Some(t(15, 0)));
        assert_eq!(
            parse_wall_time("7:20 PM (America/New_York)"),
            Some(t(19, 20)),
            "a separated meridiem must not be discarded"
        );
        assert_eq!(parse_wall_time("9:00"), Some(t(9, 0)));
        assert_eq!(parse_wall_time("12am"), Some(t(0, 0)));
        assert_eq!(parse_wall_time("12pm"), Some(t(12, 0)));
        // Ambiguous / not-a-time forms must not guess.
        assert_eq!(parse_wall_time("3"), None, "bare hour is ambiguous");
        assert_eq!(parse_wall_time("Jul 15 at 3am"), None, "date-first wording");
        assert_eq!(
            parse_wall_time("7:20am tomorrow (America/New_York)"),
            None,
            "date qualifiers are ambiguous"
        );
        assert_eq!(
            parse_wall_time("7:20am (America/New_York) on Jul 15"),
            None,
            "trailing wording after the timezone must not be ignored"
        );
        assert_eq!(parse_wall_time("19pm"), None, "out-of-range 12h hour");
        assert_eq!(parse_wall_time(""), None);
    }

    #[test]
    fn next_reset_instant_schedules_the_occurrence_after_the_stall_reference() {
        use chrono::{TimeZone, Utc};
        // 02:28 UTC on 2026-07-11 == 22:28 EDT on 2026-07-10. The observed
        // incident: reset "2:20am (America/New_York)" == 06:20 UTC, later
        // today — the very stall this feature exists for.
        let stalled_at = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        assert_eq!(
            next_reset_instant("2:20am (America/New_York)", stalled_at, false),
            Some(Utc.with_ymd_and_hms(2026, 7, 11, 6, 20, 0).unwrap()),
        );
        // 8pm EDT was before the first observation, so this banner denotes
        // the following day's occurrence.
        assert_eq!(
            next_reset_instant("8pm (America/New_York)", stalled_at, false),
            Some(Utc.with_ymd_and_hms(2026, 7, 12, 0, 0, 0).unwrap()),
        );
    }

    #[test]
    fn next_reset_instant_restart_recovery_uses_the_original_stall_reference() {
        use chrono::{TimeZone, Utc};
        let stalled_at = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        let restarted_at = Utc.with_ymd_and_hms(2026, 7, 11, 10, 30, 0).unwrap();

        // Rebuilding more than four hours after reset still reconstructs the
        // original 06:20 due time. The caller sees it is before `restarted_at`
        // and attempts immediately instead of deferring until tomorrow.
        let reset = next_reset_instant("2:20am (America/New_York)", stalled_at, false)
            .expect("explicit-zone reset must parse");
        assert_eq!(reset, Utc.with_ymd_and_hms(2026, 7, 11, 6, 20, 0).unwrap());
        assert!(reset < restarted_at);
    }

    #[test]
    fn next_reset_instant_applies_explicit_local_implied_policy() {
        use chrono::{TimeZone, Utc};
        let reference = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        // Parens naming a zone we can't resolve: reinterpreting the time in
        // the hub's zone could resume mid-window — refuse instead.
        assert_eq!(next_reset_instant("7:20am (ET)", reference, true), None);
        // Unparseable time with a good zone: same refusal.
        assert_eq!(
            next_reset_instant("soon (America/New_York)", reference, true),
            None
        );
        // A local pane may explicitly opt into the hub's local timezone.
        let local = next_reset_instant("7:20am", reference, true)
            .expect("local-implied time must parse when allowed");
        assert!(
            local > reference && local <= reference + chrono::Duration::hours(25),
            "local occurrence: {local}"
        );
        // A remote pane must refuse the same zone-less hint: the remote and
        // hub timezones are not assumed to match.
        assert_eq!(next_reset_instant("7:20am", reference, false), None);
    }

    #[test]
    fn next_reset_instant_chooses_the_later_fall_back_occurrence() {
        use chrono::{TimeZone, Utc};
        // America/New_York repeats 01:30 on 2026-11-01: 05:30 UTC (EDT), then
        // 06:30 UTC (EST). Safe scheduling takes the later occurrence so it
        // can never resume an hour before the intended reset.
        let reference = Utc.with_ymd_and_hms(2026, 11, 1, 4, 0, 0).unwrap();
        assert_eq!(
            next_reset_instant("1:30am (America/New_York)", reference, false),
            Some(Utc.with_ymd_and_hms(2026, 11, 1, 6, 30, 0).unwrap()),
        );
    }

    #[test]
    fn next_reset_instant_rejects_a_nonexistent_spring_forward_time() {
        use chrono::{TimeZone, Utc};
        // 02:30 never occurs in America/New_York on 2026-03-08. Shifting it
        // to an invented 03:30 reset would be a wrong-time auto-resume.
        let reference = Utc.with_ymd_and_hms(2026, 3, 8, 5, 0, 0).unwrap();
        assert_eq!(
            next_reset_instant("2:30am (America/New_York)", reference, false),
            None,
        );
    }

    #[test]
    fn detect_blocking_dialog_honors_custom_signature() {
        // The "extensible via config" path: a project-defined signature is
        // matched just like the built-ins.
        let sigs = vec![shelbi_core::DialogSignature::new(
            "codex-approve",
            "Approve this edit?",
        )];
        assert_eq!(
            detect_blocking_dialog("codex › Approve this edit? (y/n)", &sigs).as_deref(),
            Some("codex-approve")
        );
        assert!(detect_blocking_dialog("nothing to see here", &sigs).is_none());
    }
}
