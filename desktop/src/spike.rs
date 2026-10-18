//! `shelbi-desktop --spike <session>`: one window, one terminal element,
//! attached to an existing session. A developer tool for working on the
//! terminal element in isolation (`dt-spike-terminal`); make a session with
//! `shelbi session new` and find its name with `shelbi session ls`.

use gpui::{
    div, prelude::*, px, size, App, Bounds, Context, Entity, Focusable, Subscription, Window,
    WindowBounds, WindowOptions,
};

use crate::elements::terminal::TerminalPane;
use crate::theme::Theme;

/// The spike window's root view: the pane, full size.
pub struct SpikeWindow {
    pane: Entity<TerminalPane>,
    /// Re-resolves the theme when the system switches light/dark.
    _appearance: Subscription,
}

impl Render for SpikeWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&format!("Shelbi spike: {}", self.pane.read(cx).title()));
        let theme = *cx.global::<Theme>();
        div().size_full().bg(theme.canvas).child(self.pane.clone())
    }
}

/// Open the spike window attached to `selector`.
pub fn open(selector: String, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1000.0), px(700.0)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        |window, cx| {
            let pane = cx.new(|cx| TerminalPane::connect(selector, window, cx));
            // Refresh the window title when the session's title changes.
            cx.new(|cx| {
                cx.observe(&pane, |_, _, cx| cx.notify()).detach();
                pane.read(cx).focus_handle(cx).focus(window, cx);
                SpikeWindow {
                    pane,
                    _appearance: crate::theme::follow_system(window, cx),
                }
            })
        },
    )
    .expect("failed to open the spike window");
    cx.activate(true);
}
