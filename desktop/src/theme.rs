//! Visual tokens for the desktop app.
//!
//! The token names mirror `crates/shelbi-tui/src/theme.rs` one for one, so
//! the two clients read as one product and a token can be found in either by
//! the same name. Values are the TUI's dark values (the TUI has only a dark
//! theme today), mapped to gpui colors. `dt-theme-tokens` moves the shared
//! names into one place and adds a light theme to both clients; until then,
//! when a TUI token changes, change it here too.
//!
//! Two TUI tokens are ANSI names, not RGB (`DIVIDER_ACTIVE` is
//! `Color::Cyan`, `PROJECT_STATUS_NEUTRAL` is `Color::DarkGray`); a window
//! has no terminal palette to resolve them against, so they map to the
//! nearest brand token, noted on each field.

use gpui::{rgb, Global, Hsla};

/// The resolved token set. Installed as a gpui [`Global`] at startup and read
/// by views with `cx.global::<Theme>()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// `BACKGROUND` (#000000): the sidebar's solid fill.
    pub background: Hsla,
    /// The main area's fill. Desktop-only: in the TUI the main area is the
    /// user's terminal default background, which a window doesn't have.
    /// #1c1c1c is the Ghostty default the TUI's tokens were tuned against.
    pub main_background: Hsla,
    /// `TEXT` (#bababa): normal sidebar text.
    pub text: Hsla,
    /// `TEXT_SELECTED` (#ffffff): the selected nav label.
    pub text_selected: Hsla,
    /// `MUTED` (#7a7a7a): section headers, secondary labels.
    pub muted: Hsla,
    /// `ACCENT` (#00a6b2): the project name, the ready-for-review check.
    pub accent: Hsla,
    /// `BUSY_GREEN` (#5acd25): a working workspace's badge.
    pub busy_green: Hsla,
    /// `SELECTION_BG` (#3f3f3f): the selected / focused row fill.
    pub selection_bg: Hsla,
    /// `SEARCH_BG` (#292929): the sidebar search box fill.
    pub search_bg: Hsla,
    /// `PALETTE_BG` (#292929): the command palette panel.
    pub palette_bg: Hsla,
    /// `PALETTE_FG` (#c6c6c6): palette primary text.
    pub palette_fg: Hsla,
    /// `PALETTE_MUTED` (#7a7a7a): palette secondary text.
    pub palette_muted: Hsla,
    /// `PALETTE_GREEN` (#5acd25): the palette cursor, a loaded project.
    pub palette_green: Hsla,
    /// `ACCENT_CYAN` (#00a6b2): the review panel's status header.
    pub accent_cyan: Hsla,
    /// `FG_SECONDARY` (#bababa): review panel secondary text.
    pub fg_secondary: Hsla,
    /// `ACTION_RED` (#e04f52): Reject.
    pub action_red: Hsla,
    /// `STATUS_YELLOW` (#dbc300): the workspace "IN PROGRESS" label.
    pub status_yellow: Hsla,
    /// `DIVIDER_DIM` (#414141): the sidebar's right-edge divider at rest.
    pub divider_dim: Hsla,
    /// `DIVIDER_ACTIVE`: ANSI cyan in the TUI; the brand accent (#00a6b2)
    /// here.
    pub divider_active: Hsla,
    /// `WORKFLOW_BADGE_BG` (#3a4258): a card's workflow-name chip.
    pub workflow_badge_bg: Hsla,
    /// `WORKFLOW_BADGE_FG` (#dcdfe8): the workflow chip's text.
    pub workflow_badge_fg: Hsla,
    /// `PROJECT_STATUS_GREEN` (= `PALETTE_GREEN`): a loaded project's ring.
    pub project_status_green: Hsla,
    /// `PROJECT_STATUS_NEUTRAL`: ANSI dark gray in the TUI; `MUTED`
    /// (#7a7a7a) here.
    pub project_status_neutral: Hsla,
}

impl Global for Theme {}

impl Theme {
    /// The dark theme: the TUI's values.
    pub fn dark() -> Self {
        Self {
            background: hex(0x000000),
            main_background: hex(0x1c1c1c),
            text: hex(0xbababa),
            text_selected: hex(0xffffff),
            muted: hex(0x7a7a7a),
            accent: hex(0x00a6b2),
            busy_green: hex(0x5acd25),
            selection_bg: hex(0x3f3f3f),
            search_bg: hex(0x292929),
            palette_bg: hex(0x292929),
            palette_fg: hex(0xc6c6c6),
            palette_muted: hex(0x7a7a7a),
            palette_green: hex(0x5acd25),
            accent_cyan: hex(0x00a6b2),
            fg_secondary: hex(0xbababa),
            action_red: hex(0xe04f52),
            status_yellow: hex(0xdbc300),
            divider_dim: hex(0x414141),
            divider_active: hex(0x00a6b2),
            workflow_badge_bg: hex(0x3a4258),
            workflow_badge_fg: hex(0xdcdfe8),
            project_status_green: hex(0x5acd25),
            project_status_neutral: hex(0x7a7a7a),
        }
    }
}

/// A `0xRRGGBB` literal as a gpui color.
fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_tokens_carry_the_tui_values() {
        let t = Theme::dark();
        // Spot-check one token per family against the TUI's RGB literals.
        let rgba = |c: Hsla| {
            let c = c.to_rgb();
            (
                (c.r * 255.0).round() as u8,
                (c.g * 255.0).round() as u8,
                (c.b * 255.0).round() as u8,
            )
        };
        assert_eq!(rgba(t.background), (0, 0, 0));
        assert_eq!(rgba(t.text), (186, 186, 186));
        assert_eq!(rgba(t.accent), (0, 166, 178));
        assert_eq!(rgba(t.selection_bg), (63, 63, 63));
        assert_eq!(rgba(t.divider_dim), (65, 65, 65));
        assert_eq!(rgba(t.workflow_badge_bg), (58, 66, 88));
        assert_eq!(rgba(t.status_yellow), (0xdb, 0xc3, 0x00));
    }
}
