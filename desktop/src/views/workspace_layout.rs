//! The Workspace view's agent | user-shell split, as plain pixel math so it is
//! tested without a window.
//!
//! The split is kept as the agent's share of the room left after the divider,
//! so it survives window resizes. Both panes keep a minimum width; a container
//! too narrow for two minimums splits evenly instead.

/// The divider's width, its grab room included.
pub const DIVIDER_W: f32 = 5.0;
/// The narrowest either pane is dragged to.
pub const MIN_PANE_W: f32 = 240.0;
/// The agent's share when the shell first opens.
pub const DEFAULT_RATIO: f32 = 0.6;

/// The agent's share of the split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Split {
    ratio: f32,
}

impl Default for Split {
    fn default() -> Self {
        Self { ratio: DEFAULT_RATIO }
    }
}

impl Split {
    /// A split giving the agent `ratio` of the room (clamped into `0..=1`).
    #[cfg(test)]
    pub fn new(ratio: f32) -> Self {
        Self {
            ratio: if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { DEFAULT_RATIO },
        }
    }

    /// The agent's share.
    pub fn ratio(&self) -> f32 {
        self.ratio
    }

    /// `(agent, shell)` widths in a container `total` pixels wide. The two
    /// plus [`DIVIDER_W`] fill `total`; each is at least [`MIN_PANE_W`] when
    /// the container has room for both.
    pub fn widths(&self, total: f32) -> (f32, f32) {
        let room = (total - DIVIDER_W).max(0.0);
        let agent = if room < 2.0 * MIN_PANE_W {
            room / 2.0
        } else {
            (room * self.ratio).clamp(MIN_PANE_W, room - MIN_PANE_W)
        };
        (agent, room - agent)
    }

    /// Follow a divider drag: the pointer is `x` pixels from the container's
    /// left edge, in a container `total` wide. The divider centers on the
    /// pointer, as far as the minimum widths allow.
    pub fn drag_to(&mut self, x: f32, total: f32) {
        let room = total - DIVIDER_W;
        if room <= 0.0 || !x.is_finite() {
            return;
        }
        let agent = x - DIVIDER_W / 2.0;
        let agent = if room < 2.0 * MIN_PANE_W {
            room / 2.0
        } else {
            agent.clamp(MIN_PANE_W, room - MIN_PANE_W)
        };
        self.ratio = agent / room;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn the_default_split_gives_the_agent_more_room() {
        let (agent, shell) = Split::default().widths(1005.0);
        assert!(close(agent, 600.0), "{agent}");
        assert!(close(shell, 400.0), "{shell}");
        assert!(close(agent + shell + DIVIDER_W, 1005.0));
    }

    #[test]
    fn both_panes_keep_their_minimum() {
        let (agent, shell) = Split::new(0.95).widths(805.0);
        assert!(close(shell, MIN_PANE_W), "{shell}");
        assert!(close(agent, 800.0 - MIN_PANE_W));
        let (agent, _) = Split::new(0.0).widths(805.0);
        assert!(close(agent, MIN_PANE_W));
    }

    #[test]
    fn a_narrow_container_splits_evenly() {
        let (agent, shell) = Split::new(0.9).widths(305.0);
        assert!(close(agent, 150.0) && close(shell, 150.0));
        let (agent, shell) = Split::default().widths(2.0);
        assert_eq!((agent, shell), (0.0, 0.0), "never negative");
    }

    #[test]
    fn dragging_centers_the_divider_on_the_pointer_within_the_minimums() {
        let mut split = Split::default();
        split.drag_to(502.5, 1005.0);
        assert!(close(split.ratio(), 0.5), "{}", split.ratio());
        let (agent, _) = split.widths(1005.0);
        assert!(close(agent, 500.0));

        split.drag_to(10.0, 1005.0);
        assert!(close(split.widths(1005.0).0, MIN_PANE_W), "clamped at the left");
        split.drag_to(5000.0, 1005.0);
        assert!(close(split.widths(1005.0).1, MIN_PANE_W), "clamped at the right");
    }

    #[test]
    fn the_ratio_survives_a_window_resize() {
        let mut split = Split::default();
        split.drag_to(402.5, 1005.0); // agent 400 of 1000
        let (agent, shell) = split.widths(2005.0);
        assert!(close(agent, 800.0) && close(shell, 1200.0), "{agent} {shell}");
    }

    #[test]
    fn junk_input_leaves_a_sane_split() {
        assert_eq!(Split::new(f32::NAN), Split::default());
        let mut split = Split::default();
        split.drag_to(f32::NAN, 1000.0);
        split.drag_to(100.0, 0.0);
        assert_eq!(split, Split::default());
    }
}
