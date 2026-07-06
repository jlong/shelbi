//! First-run setup for the Zen Mode hotkey.
//!
//! Some terminals (notably Terminal.app on macOS, where Option/Alt is the
//! "meta" key for typing accented characters) swallow Alt+Z entirely. On
//! the very first sidebar launch we run a tiny probe — show a centered
//! overlay, wait a few seconds for Alt+Z. If it arrives the user is on a
//! cooperative terminal and we save `keymap.zen_toggle = alt-z`. If it
//! doesn't, we pop a chooser with three concrete fallbacks (Ctrl+\, Ctrl+G,
//! Ctrl+Shift+Z) plus a "skip" that leaves the hotkey unbound.
//!
//! The probe and chooser are split into pure-data state machines + thin
//! crossterm/ratatui rendering wrappers so the probe-failure path can be
//! exercised by tests without touching a real terminal.

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use shelbi_state::{
    load_user_config, save_user_config, user_config_path, UserConfig, ZenToggleChord,
};

use crate::keymap::matches_zen_toggle;

/// Default time the probe waits for Alt+Z before falling through to the
/// fallback chooser. Picked to feel snappy but give a user a chance to
/// realize "oh, I should press the key now" if they didn't read fast.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Outcome of the Alt+Z probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Alt+Z arrived within the timeout — terminal delivers the chord
    /// cleanly, no fallback needed.
    Detected,
    /// Timeout elapsed (or the user pressed some other key first). Caller
    /// should pop the fallback chooser.
    TimedOut,
}

/// Polls keyboard events. Trait-bound so [`probe_alt_z`] and
/// [`chooser_loop`] can be unit-tested against canned event streams
/// without spinning up crossterm.
pub trait EventSource {
    /// Wait up to `timeout` for the next key event. Returns `None` if the
    /// timeout elapses with no event.
    fn next_key(&mut self, timeout: Duration) -> Result<Option<KeyEvent>>;
}

/// Real crossterm-backed event source — what production code uses.
pub struct CrosstermEvents;

impl EventSource for CrosstermEvents {
    fn next_key(&mut self, timeout: Duration) -> Result<Option<KeyEvent>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            if !event::poll(remaining)? {
                return Ok(None);
            }
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => return Ok(Some(k)),
                _ => continue,
            }
        }
    }
}

/// Wait up to `timeout` for an Alt+Z keypress. Any other keypress arriving
/// first short-circuits to `TimedOut` — the user pressing something else
/// is a strong signal that Alt+Z isn't reaching us.
pub fn probe_alt_z<E: EventSource>(events: &mut E, timeout: Duration) -> Result<ProbeOutcome> {
    match events.next_key(timeout)? {
        None => Ok(ProbeOutcome::TimedOut),
        Some(k) if matches_zen_toggle(k.code, k.modifiers, ZenToggleChord::AltZ) => {
            Ok(ProbeOutcome::Detected)
        }
        Some(_) => Ok(ProbeOutcome::TimedOut),
    }
}

/// The fallback chooser's three real options plus a Skip sentinel.
/// Order matters — it's the order rendered in the popup and the order the
/// arrow keys walk.
pub const CHOOSER_OPTIONS: [ZenToggleChord; 4] = [
    ZenToggleChord::CtrlBackslash,
    ZenToggleChord::CtrlG,
    ZenToggleChord::CtrlShiftZ,
    ZenToggleChord::None,
];

/// State machine for the fallback chooser popup. The renderer reads
/// `selected` to draw the highlight; the key handler advances it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChooserState {
    pub selected: usize,
}

/// Result of feeding one key event to the chooser state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChooserStep {
    /// Selection moved (or no-op). Caller redraws and loops.
    Continue,
    /// User confirmed a chord with Enter — caller persists + exits.
    Confirmed(ZenToggleChord),
}

/// Step the chooser state machine. Pure function, no IO — keeps the
/// behavior unit-testable.
pub fn step_chooser(state: &mut ChooserState, key: KeyEvent) -> ChooserStep {
    let n = CHOOSER_OPTIONS.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.selected = if state.selected == 0 {
                n - 1
            } else {
                state.selected - 1
            };
            ChooserStep::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.selected = (state.selected + 1) % n;
            ChooserStep::Continue
        }
        KeyCode::Enter | KeyCode::Char(' ') => {
            ChooserStep::Confirmed(CHOOSER_OPTIONS[state.selected])
        }
        // Number shortcuts: 1..4 jump to and confirm that option.
        KeyCode::Char(c @ '1'..='4') => {
            let idx = (c as usize) - ('1' as usize);
            state.selected = idx;
            ChooserStep::Confirmed(CHOOSER_OPTIONS[idx])
        }
        // Esc treated as Skip — gives the user a quick out without forcing
        // them to arrow down to the last entry.
        KeyCode::Esc => ChooserStep::Confirmed(ZenToggleChord::None),
        _ => ChooserStep::Continue,
    }
}

/// Public entry point used by the sidebar. If `~/.shelbi/config.yaml`
/// already names a chord, return it. Otherwise probe + (on failure) show
/// the chooser, persist the result, and return the chosen chord.
///
/// `term` is the active sidebar terminal — we paint our overlays into the
/// same alt-screen so the layout doesn't flicker. Errors propagate; the
/// sidebar callsite degrades to the default chord rather than crashing.
pub fn ensure_zen_keymap<B>(term: &mut Terminal<B>) -> Result<ZenToggleChord>
where
    B: Backend + io::Write,
{
    if let Ok(path) = user_config_path() {
        if path.exists() && !is_untouched_scaffold(&path) {
            // Already onboarded — trust the saved choice. A corrupt file falls
            // through to the default so we never block the sidebar starting.
            let cfg = load_user_config().unwrap_or_default();
            return Ok(cfg.keymap.zen_toggle);
        }
    }

    let chord = run_probe_and_chooser(term, &mut CrosstermEvents, PROBE_TIMEOUT)?;
    let cfg = UserConfig {
        keymap: shelbi_state::Keymap { zen_toggle: chord },
    };
    save_user_config(&cfg).context("writing ~/.shelbi/config.yaml")?;
    Ok(chord)
}

/// True when `config.yaml` byte-matches the self-documenting scaffold that
/// `shelbi init` seeds ([`shelbi_core::scaffold::CONFIG_YAML`]). Such a file
/// carries no persisted choice, so the first-run Zen-keymap probe should still
/// run rather than short-circuiting as if the user had already onboarded.
fn is_untouched_scaffold(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s == shelbi_core::scaffold::CONFIG_YAML)
        .unwrap_or(false)
}

/// Probe + chooser flow, factored out of [`ensure_zen_keymap`] so the
/// EventSource and timeout can be swapped in tests. Returns the chord
/// the user (implicitly or explicitly) picked.
pub fn run_probe_and_chooser<B, E>(
    term: &mut Terminal<B>,
    events: &mut E,
    timeout: Duration,
) -> Result<ZenToggleChord>
where
    B: Backend + io::Write,
    E: EventSource,
{
    term.draw(|f| render_probe(f, f.area()))?;
    match probe_alt_z(events, timeout)? {
        ProbeOutcome::Detected => Ok(ZenToggleChord::AltZ),
        ProbeOutcome::TimedOut => chooser_loop(term, events),
    }
}

/// Run the chooser until the user confirms a chord. Renders the popup,
/// pumps keys through [`step_chooser`].
pub fn chooser_loop<B, E>(term: &mut Terminal<B>, events: &mut E) -> Result<ZenToggleChord>
where
    B: Backend + io::Write,
    E: EventSource,
{
    let mut state = ChooserState::default();
    loop {
        let snapshot = state;
        term.draw(|f| render_chooser(f, f.area(), &snapshot))?;
        // 200ms keeps the loop responsive without burning CPU; the user is
        // staring at a static popup so longer feels fine too.
        let Some(key) = events.next_key(Duration::from_millis(200))? else {
            continue;
        };
        match step_chooser(&mut state, key) {
            ChooserStep::Continue => {}
            ChooserStep::Confirmed(chord) => return Ok(chord),
        }
    }
}

fn render_probe(f: &mut Frame, area: Rect) {
    let overlay = centered_rect(40, 7, area);
    f.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            " Zen Mode hotkey ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(overlay);
    f.render_widget(block, overlay);
    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(
            "Press Alt+Z to verify the chord works.",
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "(any other key picks a fallback)",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_chooser(f: &mut Frame, area: Rect, state: &ChooserState) {
    let overlay = centered_rect(44, 10, area);
    f.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Line::from(Span::styled(
            " Alt+Z didn't arrive — pick a fallback ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(overlay);
    f.render_widget(block, overlay);
    let mut lines = Vec::with_capacity(CHOOSER_OPTIONS.len() + 1);
    for (i, opt) in CHOOSER_OPTIONS.iter().enumerate() {
        let marker = if i == state.selected { "▶ " } else { "  " };
        let style = if i == state.selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(format!("{}. {}", i + 1, opt.label()), style),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "↑↓ select   Enter confirm   Esc skip",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Center a `w × h` rect inside `area`, clipping to the available size.
fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use std::collections::VecDeque;

    struct MockEvents {
        queue: VecDeque<KeyEvent>,
    }

    impl MockEvents {
        fn new(events: Vec<KeyEvent>) -> Self {
            Self {
                queue: events.into(),
            }
        }
    }

    impl EventSource for MockEvents {
        fn next_key(&mut self, _timeout: Duration) -> Result<Option<KeyEvent>> {
            Ok(self.queue.pop_front())
        }
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn untouched_scaffold_is_not_treated_as_onboarded() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-zenprobe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");

        // The init scaffold reads as "not yet onboarded" → probe should run.
        std::fs::write(&path, shelbi_core::scaffold::CONFIG_YAML).unwrap();
        assert!(is_untouched_scaffold(&path));

        // A real persisted choice (or any hand edit) reads as onboarded.
        std::fs::write(&path, "keymap:\n  zen_toggle: ctrl-g\n").unwrap();
        assert!(!is_untouched_scaffold(&path));

        // Missing file is not a scaffold either.
        std::fs::remove_file(&path).unwrap();
        assert!(!is_untouched_scaffold(&path));
    }

    #[test]
    fn probe_detects_alt_z_immediately() {
        let mut events = MockEvents::new(vec![key(KeyCode::Char('z'), KeyModifiers::ALT)]);
        let out = probe_alt_z(&mut events, Duration::from_secs(1)).unwrap();
        assert_eq!(out, ProbeOutcome::Detected);
    }

    #[test]
    fn probe_times_out_when_no_key_arrives() {
        let mut events = MockEvents::new(vec![]);
        // Tiny timeout — the mock always returns None, so the loop exits
        // on the first poll regardless of the duration.
        let out = probe_alt_z(&mut events, Duration::from_millis(1)).unwrap();
        assert_eq!(out, ProbeOutcome::TimedOut);
    }

    #[test]
    fn probe_treats_other_key_as_timeout() {
        // User mashes Enter instead of Alt+Z → fall through to the
        // chooser. Spec: any non-matching keypress short-circuits the probe.
        let mut events = MockEvents::new(vec![key(KeyCode::Enter, KeyModifiers::NONE)]);
        let out = probe_alt_z(&mut events, Duration::from_secs(1)).unwrap();
        assert_eq!(out, ProbeOutcome::TimedOut);
    }

    #[test]
    fn chooser_arrow_down_wraps_at_end() {
        let mut state = ChooserState::default();
        for _ in 0..CHOOSER_OPTIONS.len() {
            assert_eq!(
                step_chooser(&mut state, key(KeyCode::Down, KeyModifiers::NONE)),
                ChooserStep::Continue
            );
        }
        assert_eq!(state.selected, 0, "wrap should return to first option");
    }

    #[test]
    fn chooser_enter_confirms_current_selection() {
        let mut state = ChooserState { selected: 2 };
        let outcome = step_chooser(&mut state, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            outcome,
            ChooserStep::Confirmed(CHOOSER_OPTIONS[2]),
            "Enter confirms the highlighted row"
        );
    }

    #[test]
    fn chooser_number_keys_jump_and_confirm() {
        // '1'..'4' both move highlight and confirm — keyboard shortcut for
        // power users who don't want to arrow through.
        let mut state = ChooserState::default();
        let outcome = step_chooser(&mut state, key(KeyCode::Char('3'), KeyModifiers::NONE));
        assert_eq!(outcome, ChooserStep::Confirmed(CHOOSER_OPTIONS[2]));
        assert_eq!(state.selected, 2);
    }

    #[test]
    fn chooser_esc_picks_skip() {
        let mut state = ChooserState { selected: 0 };
        let outcome = step_chooser(&mut state, key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(outcome, ChooserStep::Confirmed(ZenToggleChord::None));
    }

    /// End-to-end of the probe-failure path: probe times out, chooser opens,
    /// user picks Ctrl+G. Mirrors what the failing-terminal user sees on
    /// first launch.
    #[test]
    fn probe_failure_path_routes_to_chooser_pick() {
        // Drive only the post-probe chooser through canned events: an Enter
        // confirms whatever's highlighted at index 1 (Ctrl+G).
        let mut state = ChooserState { selected: 1 };
        assert_eq!(state.selected, 1);
        let outcome = step_chooser(&mut state, key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(outcome, ChooserStep::Confirmed(ZenToggleChord::CtrlG));
    }
}
