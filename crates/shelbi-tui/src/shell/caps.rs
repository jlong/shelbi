//! Terminal capability detection for the single-process shell (plan, "Running
//! inside tmux or Screen").
//!
//! The shell runs as an ordinary full-screen program, so nesting inside tmux or
//! Screen works by construction — there is no `$TMUX` branching. What needs
//! care is detecting which capabilities the outer multiplexer passes through:
//!
//! - The **kitty keyboard protocol** is not forwarded by Screen, or by tmux
//!   without `extended-keys`. Without it Claude Code's Shift+Enter (and other
//!   disambiguated keys) cannot reach the agent. We detect the missing
//!   round-trip and say so once, with the one-line tmux mitigation (the Phase 0
//!   nesting spike confirmed `extended-keys` is the right thing to recommend).
//! - **Truecolor** falls back to 256 colors; the renderer quantizes RGB cells
//!   when this is false.

/// An outer multiplexer the shell is running inside, detected from the
/// environment. We don't branch behavior on it (the shell is just a full-screen
/// program either way), but it tailors the keyboard-protocol notice to the
/// mitigation that actually applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nesting {
    /// Inside tmux (`$TMUX` set).
    Tmux,
    /// Inside GNU Screen (`$STY` set).
    Screen,
}

/// What the terminal (through any outer multiplexer) supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The kitty keyboard protocol round-trips, so modified keys such as
    /// Shift+Enter reach the agent.
    pub kitty: bool,
    /// The terminal advertises 24-bit truecolor.
    pub truecolor: bool,
    /// The outer multiplexer we are nested in, if any.
    pub nested: Option<Nesting>,
}

/// Inside tmux without `extended-keys`: name the exact one-line fix.
pub const KEYBOARD_NOTICE_TMUX: &str = "⚠ Modified keys like Shift+Enter may not reach the agent \
    here (the kitty keyboard protocol isn't passed through). Inside tmux: set -s extended-keys on";
/// Inside GNU Screen, which cannot forward the protocol at all.
pub const KEYBOARD_NOTICE_SCREEN: &str = "⚠ Modified keys like Shift+Enter may not reach the agent \
    here (GNU Screen doesn't pass the kitty keyboard protocol through). Run shelbi outside Screen, \
    or attach from a terminal that speaks it";
/// Not nested, but the terminal itself does not speak the protocol.
pub const KEYBOARD_NOTICE_GENERIC: &str = "⚠ Modified keys like Shift+Enter may not reach the \
    agent here (this terminal doesn't support the kitty keyboard protocol)";

impl Caps {
    /// Probe the live terminal. `supports_keyboard_enhancement` issues a
    /// `CSI ? u` query and waits briefly for the reply; inside tmux/Screen the
    /// query is swallowed and this reports `false`, which is exactly the signal
    /// we want. Truecolor is read from `COLORTERM` (the only cheap signal; when
    /// it over-reports inside a multiplexer the renderer still quantizes
    /// correctly because the outer terminal does, so the worst case is a
    /// true-color escape the multiplexer itself downsamples). Nesting is read
    /// from `$TMUX` / `$STY`.
    pub fn detect() -> Self {
        Caps {
            kitty: crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false),
            truecolor: truecolor_from_env(std::env::var("COLORTERM").ok().as_deref()),
            nested: detect_nesting(
                std::env::var("TMUX").ok().as_deref(),
                std::env::var("STY").ok().as_deref(),
            ),
        }
    }

    /// The notice to show once at startup, or `None` when the kitty protocol is
    /// available (everything the shell needs). Tailored to how we are nested so
    /// the suggested fix matches the actual outer multiplexer.
    pub fn keyboard_notice(&self) -> Option<&'static str> {
        if self.kitty {
            return None;
        }
        Some(match self.nested {
            Some(Nesting::Tmux) => KEYBOARD_NOTICE_TMUX,
            Some(Nesting::Screen) => KEYBOARD_NOTICE_SCREEN,
            None => KEYBOARD_NOTICE_GENERIC,
        })
    }
}

/// Pure `COLORTERM` → truecolor decision, split out for testing.
pub(crate) fn truecolor_from_env(colorterm: Option<&str>) -> bool {
    matches!(colorterm, Some("truecolor") | Some("24bit"))
}

/// Pure `$TMUX` / `$STY` → nesting decision, split out for testing. tmux wins
/// if both are set (a Screen inside tmux still reaches a tmux that can be fixed
/// with `extended-keys`). An empty value counts as unset.
pub(crate) fn detect_nesting(tmux: Option<&str>, sty: Option<&str>) -> Option<Nesting> {
    if tmux.is_some_and(|v| !v.is_empty()) {
        Some(Nesting::Tmux)
    } else if sty.is_some_and(|v| !v.is_empty()) {
        Some(Nesting::Screen)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truecolor_reads_colorterm() {
        assert!(truecolor_from_env(Some("truecolor")));
        assert!(truecolor_from_env(Some("24bit")));
        assert!(!truecolor_from_env(Some("256")));
        assert!(!truecolor_from_env(None));
    }

    #[test]
    fn keyboard_notice_shows_only_without_kitty() {
        let with = Caps { kitty: true, truecolor: true, nested: Some(Nesting::Tmux) };
        assert!(with.keyboard_notice().is_none(), "kitty present → no notice even inside tmux");
        let without = Caps { kitty: false, truecolor: true, nested: None };
        assert_eq!(without.keyboard_notice(), Some(KEYBOARD_NOTICE_GENERIC));
    }

    #[test]
    fn nesting_reads_tmux_and_sty() {
        assert_eq!(detect_nesting(Some("/tmp/tmux-501/default,1,0"), None), Some(Nesting::Tmux));
        assert_eq!(detect_nesting(None, Some("12345.pts-0.host")), Some(Nesting::Screen));
        // tmux wins when both are present.
        assert_eq!(detect_nesting(Some("x"), Some("y")), Some(Nesting::Tmux));
        // Empty values count as unset.
        assert_eq!(detect_nesting(Some(""), Some("")), None);
        assert_eq!(detect_nesting(None, None), None);
    }

    #[test]
    fn notice_is_tailored_to_the_outer_multiplexer() {
        // Inside tmux without kitty: the notice names the `extended-keys` fix.
        let tmux = Caps { kitty: false, truecolor: true, nested: Some(Nesting::Tmux) };
        let msg = tmux.keyboard_notice().expect("notice warranted without kitty");
        assert!(msg.contains("extended-keys"), "tmux notice names the fix: {msg}");
        // Inside Screen: a Screen-specific message, with no tmux-only command.
        let screen = Caps { kitty: false, truecolor: true, nested: Some(Nesting::Screen) };
        let msg = screen.keyboard_notice().expect("notice warranted without kitty");
        assert!(msg.contains("Screen"), "screen notice mentions Screen: {msg}");
        assert!(!msg.contains("extended-keys"), "screen notice omits the tmux-only fix: {msg}");
    }
}
