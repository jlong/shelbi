//! The diff view's colors, as meanings over the palette's roles: green and
//! red tints for added and deleted lines, a stronger step for the words that
//! changed within one, and a color per syntax class. The Figma file has no diff
//! frame yet, so these are derived rather than taken from `03 Desktop`: from
//! the same text and icon greens and reds the rest of the app uses, and from
//! the terminal's ANSI palette for syntax, which is already tuned to read on
//! both canvases. Everything is a tint, so a translucent window shows through.

use gpui::Hsla;
use shelbi_app::diff::highlight::SyntaxClass;

use super::{rgb_hsla, Theme};

/// The tint behind an added or deleted line.
const LINE_TINT: f32 = 0.13;
/// The tint behind its gutter.
const GUTTER_TINT: f32 = 0.2;
/// The tint behind the words that changed.
const WORD_TINT: f32 = 0.34;

/// Everything the diff body paints with.
#[derive(Debug, Clone, Copy)]
pub struct DiffColors {
    pub text: Hsla,
    pub loud: Hsla,
    pub muted: Hsla,
    pub add: Hsla,
    pub del: Hsla,
    pub add_line: Hsla,
    pub del_line: Hsla,
    pub add_gutter: Hsla,
    pub del_gutter: Hsla,
    pub add_word: Hsla,
    pub del_word: Hsla,
    pub file_header: Hsla,
    pub hunk_header: Hsla,
    pub rule: Hsla,
    /// The cursor's row, and its bar, while the view has the keyboard.
    pub cursor: Hsla,
    pub cursor_bar: Hsla,
    /// The cursor's bar without the keyboard.
    pub cursor_bar_idle: Hsla,
    pub selection: Hsla,
    pub search: Hsla,
    pub search_current: Hsla,
    pub thread: Hsla,
    pub viewed: Hsla,
    pub scroll_thumb: Hsla,
}

impl DiffColors {
    pub fn of(theme: &Theme) -> Self {
        let (add, del) = (theme.success, theme.error);
        DiffColors {
            text: theme.foreground,
            loud: theme.text_loud,
            muted: theme.muted,
            add,
            del,
            add_line: add.opacity(LINE_TINT),
            del_line: del.opacity(LINE_TINT),
            add_gutter: add.opacity(GUTTER_TINT),
            del_gutter: del.opacity(GUTTER_TINT),
            add_word: add.opacity(WORD_TINT),
            del_word: del.opacity(WORD_TINT),
            file_header: theme.background,
            hunk_header: theme.background.opacity(theme.background.a * 0.55),
            rule: theme.divider,
            cursor: theme.background_hover,
            cursor_bar: theme.focus_ring,
            cursor_bar_idle: theme.gray,
            selection: theme.text_selection,
            search: rgb_hsla(theme.terminal.search_match),
            search_current: rgb_hsla(theme.terminal.search_current),
            thread: theme.accent,
            viewed: theme.icon_green,
            scroll_thumb: theme.gray.opacity(0.35),
        }
    }
}

/// The color a syntax class paints, or `None` for the text's own.
pub fn syntax_color(theme: &Theme, class: SyntaxClass) -> Option<Hsla> {
    let ansi = |n: usize| rgb_hsla(theme.terminal.ansi[n]);
    Some(match class {
        SyntaxClass::Comment => theme.muted,
        SyntaxClass::String => ansi(2),
        SyntaxClass::Number | SyntaxClass::Constant | SyntaxClass::Attribute => ansi(3),
        SyntaxClass::Keyword | SyntaxClass::Tag => ansi(5),
        SyntaxClass::Function => ansi(4),
        SyntaxClass::Type => ansi(6),
        SyntaxClass::Heading => theme.text_loud,
        SyntaxClass::Operator | SyntaxClass::Variable | SyntaxClass::Punctuation => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::WindowAppearance;
    use shelbi_app::theme::ThemePreference;

    /// The WCAG ratio of `text` over `tint` over `canvas`.
    fn contrast(text: Hsla, tint: Hsla, canvas: Hsla) -> f32 {
        let lum = |c: gpui::Rgba| {
            let f = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        };
        let over = |top: gpui::Rgba, under: gpui::Rgba| gpui::Rgba {
            r: top.r * top.a + under.r * (1.0 - top.a),
            g: top.g * top.a + under.g * (1.0 - top.a),
            b: top.b * top.a + under.b * (1.0 - top.a),
            a: 1.0,
        };
        let surface = over(tint.into(), canvas.into());
        let text = over(text.into(), surface);
        let (a, b) = (lum(surface), lum(text));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn code_stays_legible_on_every_tint_in_both_themes() {
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            let theme = Theme::resolve(ThemePreference::Auto, system);
            let c = DiffColors::of(&theme);
            let canvas = theme.canvas;
            for tint in [c.add_line, c.del_line, c.add_word, c.del_word] {
                let ratio = contrast(c.text, tint, canvas);
                assert!(ratio >= 4.5, "{system:?}: text on {tint:?} is only {ratio}:1");
            }
            for class in [
                SyntaxClass::String,
                SyntaxClass::Number,
                SyntaxClass::Keyword,
                SyntaxClass::Function,
                SyntaxClass::Type,
            ] {
                let color = syntax_color(&theme, class).unwrap();
                let ratio = contrast(color, c.add_line, canvas);
                assert!(ratio >= 3.0, "{system:?}: {class:?} on an added line is only {ratio}:1");
            }
        }
    }
}
