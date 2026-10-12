//! Scrollback navigation.
//!
//! Shelbi's own scrollback exists for sessions on the **normal** screen. A
//! full-screen program (one on the alternate screen) has no scrollback to
//! show; that is the program's design, not something Shelbi overrides. The
//! normal-screen gate itself lives in [`crate::view::TerminalView`]; this
//! module is the pure offset arithmetic it drives.
//!
//! The offset is measured in lines above the live bottom: `0` is pinned to the
//! bottom (new output is visible), a larger offset scrolls further back into
//! history, clamped to the retained history length. New output or input resets
//! the view to the bottom.

/// A scrollback position: a line offset above the live bottom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scrollback {
    offset: usize,
}

impl Scrollback {
    /// A scrollback pinned to the bottom.
    pub fn new() -> Self {
        Self::default()
    }

    /// A scrollback at a given offset (used to mirror the emulator's display
    /// offset after it shifts on its own, e.g. when output pushes into history
    /// while the view is scrolled back).
    pub fn at(offset: usize) -> Self {
        Self { offset }
    }

    /// The current offset above the bottom (`0` = live bottom).
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Whether the view is scrolled back into history (not at the bottom).
    pub fn is_active(&self) -> bool {
        self.offset > 0
    }

    /// Whether the view is pinned to the live bottom.
    pub fn at_bottom(&self) -> bool {
        self.offset == 0
    }

    /// Scroll up (toward older history) by `lines`, clamped to `max_offset`
    /// (the retained history length). Returns the new offset.
    pub fn scroll_up(&mut self, lines: usize, max_offset: usize) -> usize {
        self.offset = self.offset.saturating_add(lines).min(max_offset);
        self.offset
    }

    /// Scroll down (toward the live bottom) by `lines`. Returns the new offset.
    pub fn scroll_down(&mut self, lines: usize) -> usize {
        self.offset = self.offset.saturating_sub(lines);
        self.offset
    }

    /// Jump to the oldest retained line.
    pub fn to_top(&mut self, max_offset: usize) -> usize {
        self.offset = max_offset;
        self.offset
    }

    /// Jump to the live bottom.
    pub fn to_bottom(&mut self) -> usize {
        self.offset = 0;
        self.offset
    }

    /// Reset to the live bottom. Called when new output arrives or the user
    /// types, so the view follows the session again.
    pub fn reset(&mut self) {
        self.offset = 0;
    }

    /// Re-clamp the offset to `max_offset` after history shrank (e.g. on
    /// resize, or when the alternate screen's empty history replaces a full
    /// one). Returns the new offset.
    pub fn clamp(&mut self, max_offset: usize) -> usize {
        self.offset = self.offset.min(max_offset);
        self.offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_the_bottom() {
        let s = Scrollback::new();
        assert!(s.at_bottom());
        assert!(!s.is_active());
        assert_eq!(s.offset(), 0);
    }

    #[test]
    fn scroll_up_clamps_to_history() {
        let mut s = Scrollback::new();
        assert_eq!(s.scroll_up(5, 100), 5);
        assert!(s.is_active());
        assert_eq!(s.scroll_up(200, 100), 100); // clamped
        assert_eq!(s.scroll_up(10, 100), 100); // stays clamped
    }

    #[test]
    fn scroll_down_saturates_at_the_bottom() {
        let mut s = Scrollback::new();
        s.scroll_up(10, 100);
        assert_eq!(s.scroll_down(3), 7);
        assert_eq!(s.scroll_down(100), 0); // can't go below the bottom
        assert!(s.at_bottom());
    }

    #[test]
    fn top_and_bottom_jumps() {
        let mut s = Scrollback::new();
        assert_eq!(s.to_top(100), 100);
        assert_eq!(s.to_bottom(), 0);
    }

    #[test]
    fn reset_pins_to_bottom() {
        let mut s = Scrollback::new();
        s.scroll_up(50, 100);
        s.reset();
        assert!(s.at_bottom());
    }

    #[test]
    fn clamp_shrinks_with_history() {
        let mut s = Scrollback::new();
        s.scroll_up(80, 100);
        assert_eq!(s.clamp(10), 10);
        assert_eq!(s.clamp(0), 0);
    }
}
