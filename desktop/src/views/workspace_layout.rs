//! The agent | user-shell split the Workspace and Review views share: the
//! pixel math ([`Split`], tested without a window) and the panes' row with its
//! draggable divider ([`render_split`]).
//!
//! The split is kept as the agent's share of the room left after the divider,
//! so it survives window resizes. Both panes keep a minimum width; a container
//! too narrow for two minimums splits evenly instead.

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, Bounds, Context, CursorStyle, MouseButton,
    MouseMoveEvent, Pixels, SharedString, Stateful,
};

use crate::theme::Theme;

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

/// A view's split as it is on screen: the ratio, where the panes' row was
/// laid out last frame (for the drag math), and whether the divider is being
/// dragged.
#[derive(Debug, Default)]
pub struct ShellSplit {
    pub split: Split,
    pub bounds: Option<Bounds<Pixels>>,
    pub dragging: bool,
}

/// The panes' row: `content`, then, when `shell` is given, the divider and
/// the shell's pane. Ids and selectors are `{prefix}-panes`,
/// `{prefix}-split-divider` and `{prefix}-shell-pane`; `state` reaches the
/// view's [`ShellSplit`] for the drag.
pub fn render_split<V: 'static>(
    prefix: &'static str,
    split: &ShellSplit,
    state: fn(&mut V) -> &mut ShellSplit,
    content: AnyElement,
    shell: Option<AnyElement>,
    theme: &Theme,
    cx: &mut Context<V>,
) -> Stateful<gpui::Div> {
    let this = cx.entity().downgrade();
    let open = shell.is_some();
    let widths = split.bounds.map(|b| split.split.widths(b.size.width.as_f32()));
    let content = div()
        .flex()
        .min_w_0()
        .min_h_0()
        .h_full()
        .map(|d| match (open, widths) {
            (false, _) => d.flex_1(),
            (true, Some((agent, _))) => d.flex_none().w(px(agent)),
            (true, None) => d.flex_none().w(relative(split.split.ratio())),
        })
        .child(content);
    let divider_color = if split.dragging { theme.icon } else { theme.divider };
    let name = |suffix: &str| SharedString::from(format!("{prefix}-{suffix}"));
    let shell_side = shell.map(|shell| {
        let divider_name = name("split-divider");
        let divider = div()
            .id(divider_name.clone())
            .debug_selector(move || divider_name.to_string())
            .w(px(DIVIDER_W))
            .h_full()
            .flex_none()
            .flex()
            .justify_center()
            .cursor(CursorStyle::ResizeLeftRight)
            .child(div().w(px(1.0)).h_full().bg(divider_color))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    state(this).dragging = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        let pane_name = name("shell-pane");
        let pane = div()
            .debug_selector(move || pane_name.to_string())
            .flex()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex_1()
            .child(shell);
        [divider.into_any_element(), pane.into_any_element()]
    });
    let stop_drag = move |this: &mut V, _: &gpui::MouseUpEvent, _: &mut gpui::Window, cx: &mut Context<V>| {
        if state(this).dragging {
            state(this).dragging = false;
            cx.notify();
        }
    };
    let panes_name = name("panes");
    div()
        .id(panes_name.clone())
        .debug_selector(move || panes_name.to_string())
        .relative()
        .flex()
        .flex_row()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            let s = state(this);
            if let (true, Some(bounds)) = (s.dragging, s.bounds) {
                let x = (event.position.x - bounds.origin.x).as_f32();
                s.split.drag_to(x, bounds.size.width.as_f32());
                cx.notify();
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(stop_drag))
        .on_mouse_up_out(MouseButton::Left, cx.listener(stop_drag))
        .child(content)
        .children(shell_side.into_iter().flatten())
        // Measures the panes' row for the divider math; repaints once when
        // the width first becomes known or changes.
        .child(
            canvas(
                move |bounds, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        let s = state(this);
                        if s.bounds != Some(bounds) {
                            s.bounds = Some(bounds);
                            cx.notify();
                        }
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
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
