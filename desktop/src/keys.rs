//! gpui key events to Shelbi's toolkit-independent [`KeyChord`].
//!
//! The desktop twin of the TUI's crossterm edge (`shelbi_tui::keymap::
//! chord_from_event`): the keymap layer in `shelbi_state::keymap` knows nothing
//! about any toolkit, so every key the window handles is converted here first
//! and then dispatched through the shared `keys.yaml` keymaps. The result
//! matches what the crossterm conversion produces for the same physical key,
//! so one `keys.yaml` binding means the same thing in both clients: the
//! sidebar/global keys in `crate::app` and the board's `kanban:`/`popover:`
//! keys in `crate::views::issues` both go through here.

use gpui::Keystroke;
use shelbi_app::nav::View;
use shelbi_state::keymap::{format_chord, DisplayStyle, Key, KeyChord, Mods};

use crate::elements::terminal::FontZoom;

/// File ▸ New Window, the macOS standard (⌘⇧N), as a `keys.yaml` chord. The
/// window takes it before a focused terminal can (`crate::app`), and the
/// menu shows it on the item.
pub const NEW_WINDOW: &str = "shift-super-n";

/// Whether `chord` is [`NEW_WINDOW`].
pub fn is_new_window(chord: &KeyChord) -> bool {
    KeyChord::parse(NEW_WINDOW).is_ok_and(|c| c.canonical() == chord.canonical())
}

/// The terminal font zoom chords (View ▸ Bigger / Smaller / Actual Size), as
/// `keys.yaml` chords. The window takes them before a focused terminal can
/// (`crate::app`), and the menu shows them on the items.
pub const FONT_BIGGER: &str = "super-+";
pub const FONT_SMALLER: &str = "super--";
pub const FONT_ACTUAL_SIZE: &str = "super-0";

/// The font zoom step `chord` asks for. ⌘+ is Shift+= on a US layout, which
/// macOS reports as `+` (gpui drops the Shift), so ⌘= (no Shift) and a
/// Shift that a layout keeps both count as Bigger, as in Terminal and iTerm.
pub fn font_zoom(chord: &KeyChord) -> Option<FontZoom> {
    let shift = chord.mods == Mods::SUPER | Mods::SHIFT;
    if chord.mods != Mods::SUPER && !shift {
        return None;
    }
    match chord.code {
        Key::Char('+' | '=') => Some(FontZoom::Bigger),
        Key::Char('-') if !shift => Some(FontZoom::Smaller),
        Key::Char('0') if !shift => Some(FontZoom::ActualSize),
        _ => None,
    }
}

/// What one of the sidebar's own chords opens: the Navigation component's
/// hotkeys, the desktop's fixed bindings (not `keys.yaml`'s, which the TUI
/// shares).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavChord {
    /// ⌘O, ⌘I, ⌘L: the Orchestrator, Issues and Changelog (Activity) views.
    View(View),
    /// ⌘1 to ⌘9: the sidebar row the hint numbers (reviews, then working
    /// workspaces).
    Row(u8),
}

/// The nav views' chords, as `keys.yaml` chord strings.
const NAV_VIEW_CHORDS: &[(&str, &str)] = &[("orch", "super-o"), ("tasks", "super-i"), ("activity", "super-l")];

/// The chord that opens `view` from anywhere in the window. `None` for a
/// view without one (Machines).
pub fn nav_view_chord(view: &View) -> Option<KeyChord> {
    let id = view.as_view_id();
    NAV_VIEW_CHORDS
        .iter()
        .find(|(v, _)| *v == id)
        .and_then(|(_, spec)| KeyChord::parse(spec).ok())
}

/// A chord as the window's own text spells it (key hints, footers, the
/// palette): `style`'s spelling with Enter as Geist's `↵`, the glyph the
/// Figma frames draw. The shared Mac spelling's `⏎` isn't in Geist, so it
/// fell back to a heavier, wider symbol face. Native menus keep
/// `format_chord`.
pub fn chord_text(chord: &KeyChord, style: DisplayStyle) -> String {
    format_chord(chord, style).replace('⏎', "↵")
}

/// A chord's keycap as the Keybindings frame (2252:28172) spells it: each
/// modifier glyph, then the key, letters as capitals and named keys as their
/// macOS key names (`⌃`, `P`; `⇧`, `↑`; `Return`; `Page Up`). Shift+Tab
/// (`back-tab`) is `⇧`, `Tab`.
pub fn keycap_keys(chord: &KeyChord) -> Vec<String> {
    let m = chord.mods;
    let mut keys = Vec::new();
    for (bit, glyph) in [(Mods::CONTROL, "⌃"), (Mods::ALT, "⌥"), (Mods::SHIFT, "⇧"), (Mods::SUPER, "⌘")] {
        let back_tab = bit == Mods::SHIFT && chord.code == Key::BackTab;
        if m.contains(bit) || back_tab {
            keys.push(glyph.to_string());
        }
    }
    let key = match chord.code {
        Key::Char(' ') => "Space".to_string(),
        Key::Char(c) => c.to_uppercase().to_string(),
        Key::Up => "↑".into(),
        Key::Down => "↓".into(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::Enter => "Return".into(),
        Key::Esc => "Esc".into(),
        Key::Tab | Key::BackTab => "Tab".into(),
        Key::Backspace => "⌫".into(),
        Key::Delete => "⌦".into(),
        Key::Insert => "Insert".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::PageUp => "Page Up".into(),
        Key::PageDown => "Page Down".into(),
        Key::F(n) => format!("F{n}"),
    };
    keys.push(key);
    keys
}

/// Every spelling a search for `chord` should find it by: its keycap
/// (`⌃P`), the shared Mac and spelled-out forms (`Ctrl+P`) and `keys.yaml`'s
/// (`ctrl-p`), lowercased.
pub fn chord_search_text(chord: &KeyChord) -> String {
    [
        keycap_keys(chord).concat(),
        format_chord(chord, DisplayStyle::Mac),
        format_chord(chord, DisplayStyle::Linux),
        chord.canonical(),
    ]
    .join(" ")
    .to_lowercase()
}

/// [`nav_view_chord`] as the sidebar and the Go menu show it (`⌘O`).
pub fn nav_view_hotkey(view: &View) -> Option<String> {
    nav_view_chord(view).map(|c| chord_text(&c, DisplayStyle::Mac))
}

/// A numbered row's hint (`⌘3`).
pub fn row_hotkey(n: u8) -> String {
    format!("⌘{n}")
}

/// Whether `chord` is one of the sidebar's own chords, and what it opens.
pub fn nav_chord(chord: &KeyChord) -> Option<NavChord> {
    if chord.mods != Mods::SUPER {
        return None;
    }
    if let Key::Char(c @ '1'..='9') = chord.code {
        return Some(NavChord::Row(c as u8 - b'0'));
    }
    let canonical = chord.canonical();
    NAV_VIEW_CHORDS.iter().find_map(|(view, spec)| {
        let bound = KeyChord::parse(spec).ok()?;
        (bound.canonical() == canonical).then(|| NavChord::View(View::from_view_id(view)))
    })
}

/// Translate a gpui [`Keystroke`] into a [`KeyChord`], or `None` for a key
/// outside Shelbi's chord vocabulary (a bare modifier, a media key). Such keys
/// can never be bound, so `None` means "no chord".
///
/// Letters keep the physical key and the Shift bit (`shift-k`, the canonical
/// form `keys.yaml`'s `K` parses to). Shifted punctuation takes the character
/// it types and drops Shift (`shift-/` is `?`), the way a terminal reports it.
/// Option-modified keys keep the physical key (`alt-z`, not `Ω`). Cmd maps to
/// Super; the fn key is not modeled.
pub fn chord_from_keystroke(keystroke: &Keystroke) -> Option<KeyChord> {
    let m = &keystroke.modifiers;
    let mut mods = Mods::NONE;
    if m.control {
        mods |= Mods::CONTROL;
    }
    if m.alt {
        mods |= Mods::ALT;
    }
    if m.shift {
        mods |= Mods::SHIFT;
    }
    if m.platform {
        mods |= Mods::SUPER;
    }

    let key = match keystroke.key.as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "escape" => Key::Esc,
        // Shift+Tab is reported as BackTab (with Shift still held), as
        // crossterm does.
        "tab" if m.shift => Key::BackTab,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Char(' '),
        name => {
            if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=12).contains(&n) {
                    return Some(KeyChord::new(Key::F(n), mods));
                }
                return None;
            }
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if c.is_ascii_alphabetic() {
                Key::Char(c.to_ascii_lowercase())
            } else if m.shift && !m.control && !m.alt && !m.platform {
                // Shifted punctuation: the typed character, without Shift.
                match keystroke.key_char.as_deref().map(single_char) {
                    Some(Some(typed)) => {
                        mods = Mods::NONE;
                        Key::Char(typed)
                    }
                    _ => Key::Char(c),
                }
            } else {
                Key::Char(c)
            }
        }
    };
    Some(KeyChord::new(key, mods))
}

/// The chord an overlay's text field receives for a keystroke. Like
/// [`chord_from_keystroke`], except that a key that types a character (with
/// no Ctrl / Cmd held) arrives as that character: `A` for Shift+A, `é` for a
/// dead-key sequence, so the shared state machines (which type
/// `Key::Char(c)`) insert what the user typed. Keys that type nothing
/// (arrows, Enter, Ctrl / Cmd chords) are the plain chord.
pub fn overlay_chord(keystroke: &Keystroke) -> Option<KeyChord> {
    let m = &keystroke.modifiers;
    if !m.control && !m.platform {
        if let Some(Some(typed)) = keystroke.key_char.as_deref().map(single_char) {
            if !typed.is_control() {
                return Some(KeyChord::new(Key::Char(typed), Mods::NONE));
            }
        }
    }
    chord_from_keystroke(keystroke)
}

/// Pasted `text` as the chords that type it into an overlay. A multi-line
/// field takes a line break as Enter (the reject prompt's newline); a
/// single-line one (the palette query, the add-project fields) drops it, so a
/// paste can't submit. Other control characters are dropped.
pub fn paste_chords(text: &str, multiline: bool) -> Vec<KeyChord> {
    let text = text.replace("\r\n", "\n");
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\r' if multiline => Some(KeyChord::new(Key::Enter, Mods::NONE)),
            '\t' => Some(KeyChord::new(Key::Char(' '), Mods::NONE)),
            c if c.is_control() => None,
            c => Some(KeyChord::new(Key::Char(c), Mods::NONE)),
        })
        .collect()
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::{GlobalAction, Keymaps, SidebarAction};

    /// A keystroke as macOS reports it: `key` is the physical key,
    /// `key_char` what it types.
    fn ks(spec: &str, key_char: Option<&str>) -> Keystroke {
        let mut k = Keystroke::parse(spec).expect(spec);
        k.key_char = key_char.map(str::to_string);
        k
    }

    #[test]
    fn enter_reads_as_geists_return_glyph() {
        let enter = KeyChord::parse("enter").unwrap();
        assert_eq!(chord_text(&enter, DisplayStyle::Mac), "↵");
        let save = KeyChord::parse("super-s").unwrap();
        assert_eq!(chord_text(&save, DisplayStyle::Mac), format_chord(&save, DisplayStyle::Mac));
    }

    #[test]
    fn keycaps_spell_chords_as_the_keybindings_frame_does() {
        let caps = |spec: &str| keycap_keys(&KeyChord::parse(spec).unwrap()).concat();
        for (spec, want) in [
            ("ctrl-p", "⌃P"),
            ("alt-z", "⌥Z"),
            ("k", "K"),
            ("K", "⇧K"),
            ("shift-up", "⇧↑"),
            ("enter", "Return"),
            ("space", "Space"),
            ("esc", "Esc"),
            ("page-up", "Page Up"),
            ("page-down", "Page Down"),
            ("home", "Home"),
            ("G", "⇧G"),
            ("backspace", "⌫"),
            ("tab", "Tab"),
            ("back-tab", "⇧Tab"),
            ("super-shift-n", "⇧⌘N"),
            ("?", "?"),
            ("f5", "F5"),
        ] {
            assert_eq!(caps(spec), want, "{spec}");
        }
        assert_eq!(keycap_keys(&KeyChord::parse("ctrl-shift-p").unwrap()), ["⌃", "⇧", "P"]);
        let text = chord_search_text(&KeyChord::parse("ctrl-p").unwrap());
        for needle in ["⌃p", "ctrl+p", "ctrl-p"] {
            assert!(text.contains(needle), "{needle} in {text}");
        }
    }

    fn chord(spec: &str, key_char: Option<&str>) -> String {
        chord_from_keystroke(&ks(spec, key_char))
            .map(|c| c.canonical())
            .unwrap_or_else(|| "<none>".into())
    }

    #[test]
    fn cmd_shift_n_is_new_window() {
        let chord = |spec| chord_from_keystroke(&ks(spec, Some("n"))).unwrap();
        assert!(is_new_window(&chord("cmd-shift-n")));
        assert!(!is_new_window(&chord("cmd-n")));
        assert!(!is_new_window(&chord("ctrl-shift-n")));
    }

    #[test]
    fn cmd_plus_equals_minus_and_zero_are_the_font_zoom() {
        let zoom = |spec: &str, key_char: &str| {
            font_zoom(&chord_from_keystroke(&ks(spec, Some(key_char))).unwrap())
        };
        // ⌘+ as macOS reports it on a US layout (Shift dropped), and as a
        // layout that keeps the Shift would.
        assert_eq!(zoom("cmd-+", "+"), Some(FontZoom::Bigger));
        assert_eq!(zoom("cmd-shift-=", "+"), Some(FontZoom::Bigger));
        assert_eq!(zoom("cmd-=", "="), Some(FontZoom::Bigger));
        assert_eq!(zoom("cmd--", "-"), Some(FontZoom::Smaller));
        assert_eq!(zoom("cmd-0", "0"), Some(FontZoom::ActualSize));
        // Other modifiers, or none, are someone else's.
        for (spec, key_char) in [
            ("=", "="),
            ("-", "-"),
            ("0", "0"),
            ("ctrl-0", "0"),
            ("alt-cmd-0", "0"),
            ("ctrl-cmd--", "-"),
            ("cmd-shift-0", ")"),
            ("cmd-9", "9"),
        ] {
            assert_eq!(zoom(spec, key_char), None, "{spec}");
        }
        // The menu chords are the zoom chords.
        for (spec, step) in [
            (FONT_BIGGER, FontZoom::Bigger),
            (FONT_SMALLER, FontZoom::Smaller),
            (FONT_ACTUAL_SIZE, FontZoom::ActualSize),
        ] {
            assert_eq!(font_zoom(&KeyChord::parse(spec).unwrap()), Some(step), "{spec}");
        }
    }

    #[test]
    fn the_sidebar_chords_open_the_nav_views_and_numbered_rows() {
        let chord = |spec, typed| chord_from_keystroke(&ks(spec, Some(typed))).unwrap();
        assert_eq!(nav_chord(&chord("cmd-o", "o")), Some(NavChord::View(View::Session("orch".into()))));
        assert_eq!(nav_chord(&chord("cmd-i", "i")), Some(NavChord::View(View::Issues)));
        assert_eq!(nav_chord(&chord("cmd-l", "l")), Some(NavChord::View(View::Activity)));
        assert_eq!(nav_chord(&chord("cmd-1", "1")), Some(NavChord::Row(1)));
        assert_eq!(nav_chord(&chord("cmd-9", "9")), Some(NavChord::Row(9)));
        assert_eq!(nav_chord(&chord("cmd-0", "0")), None);
        assert_eq!(nav_chord(&chord("cmd-shift-o", "O")), None);
        assert_eq!(nav_chord(&chord("ctrl-o", "o")), None);
        assert_eq!(nav_chord(&chord("o", "o")), None);
        assert_eq!(nav_view_hotkey(&View::Session("orch".into())).as_deref(), Some("⌘O"));
        assert_eq!(nav_view_hotkey(&View::Issues).as_deref(), Some("⌘I"));
        assert_eq!(nav_view_hotkey(&View::Activity).as_deref(), Some("⌘L"));
        assert_eq!(nav_view_hotkey(&View::Machines), None);
        assert_eq!(row_hotkey(4), "⌘4");
    }

    #[test]
    fn named_keys_map_to_the_chord_vocabulary() {
        for (spec, want) in [
            ("up", "up"),
            ("down", "down"),
            ("left", "left"),
            ("right", "right"),
            ("enter", "enter"),
            ("escape", "esc"),
            ("tab", "tab"),
            ("backspace", "backspace"),
            ("delete", "delete"),
            ("home", "home"),
            ("end", "end"),
            ("pageup", "page-up"),
            ("pagedown", "page-down"),
            ("space", "space"),
            ("f5", "f5"),
        ] {
            assert_eq!(chord(spec, None), want, "{spec}");
        }
    }

    #[test]
    fn modifiers_carry_over() {
        assert_eq!(chord("ctrl-h", None), "ctrl-h");
        assert_eq!(chord("ctrl-l", None), "ctrl-l");
        assert_eq!(chord("cmd-p", None), "super-p");
        // Option keeps the physical key, not the character it types.
        assert_eq!(chord("alt-z", Some("Ω")), "alt-z");
        assert_eq!(chord("ctrl-shift-z", None), "ctrl-shift-z");
    }

    #[test]
    fn shifted_letters_and_punctuation() {
        // `K` in keys.yaml parses to shift-k; gpui reports key "k" + shift.
        assert_eq!(chord("shift-k", Some("K")), "shift-k");
        // keys.yaml spells the board's move keys `H` / `L`.
        assert_eq!(
            chord_from_keystroke(&ks("shift-h", Some("H"))),
            Some(KeyChord::parse("H").unwrap())
        );
        assert_eq!(chord("shift-up", None), "shift-up");
        // Shifted punctuation is the typed character, as a terminal sends it.
        assert_eq!(chord("shift-/", Some("?")), "?");
        assert_eq!(chord("/", Some("/")), "/");
        // Shift+Tab is BackTab, as crossterm reports it.
        assert_eq!(
            chord_from_keystroke(&ks("shift-tab", None)).map(|c| c.code),
            Some(Key::BackTab)
        );
    }

    #[test]
    fn overlay_chords_type_what_the_key_types() {
        let typed = |spec: &str, key_char: Option<&str>| {
            overlay_chord(&ks(spec, key_char)).map(|c| (c.code, c.mods))
        };
        assert_eq!(typed("shift-a", Some("A")), Some((Key::Char('A'), Mods::NONE)));
        assert_eq!(typed("a", Some("a")), Some((Key::Char('a'), Mods::NONE)));
        assert_eq!(typed("shift-/", Some("?")), Some((Key::Char('?'), Mods::NONE)));
        assert_eq!(typed("space", Some(" ")), Some((Key::Char(' '), Mods::NONE)));
        // Chords stay chords: Ctrl+D submits the reject prompt, Cmd+K is the
        // palette chord, Enter and the arrows type nothing.
        assert_eq!(typed("ctrl-d", None), Some((Key::Char('d'), Mods::CONTROL)));
        assert_eq!(typed("cmd-k", Some("k")), Some((Key::Char('k'), Mods::SUPER)));
        assert_eq!(typed("enter", None), Some((Key::Enter, Mods::NONE)));
        assert_eq!(typed("shift-tab", None).map(|c| c.0), Some(Key::BackTab));
    }

    #[test]
    fn pastes_type_their_text_and_only_multiline_fields_take_newlines() {
        let codes = |text: &str, multiline: bool| -> Vec<Key> {
            paste_chords(text, multiline).into_iter().map(|c| c.code).collect()
        };
        assert_eq!(
            codes("a\r\nb", true),
            [Key::Char('a'), Key::Enter, Key::Char('b')]
        );
        assert_eq!(codes("a\nb", false), [Key::Char('a'), Key::Char('b')]);
        assert_eq!(codes("x\ty", false), [Key::Char('x'), Key::Char(' '), Key::Char('y')]);
    }

    #[test]
    fn keys_outside_the_vocabulary_have_no_chord() {
        assert_eq!(chord("shift", None), "<none>");
        assert_eq!(chord("f20", None), "<none>");
        assert_eq!(chord("capslock", None), "<none>");
    }

    #[test]
    fn converted_chords_dispatch_through_the_shared_keymaps() {
        // The built-in defaults, without reading any `keys.yaml`.
        let keymaps: Keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        let dispatch_global =
            |spec: &str| keymaps.global.dispatch(chord_from_keystroke(&ks(spec, None)).unwrap());
        assert_eq!(dispatch_global("ctrl-h"), Some(GlobalAction::FocusSidebar));
        assert_eq!(dispatch_global("ctrl-l"), Some(GlobalAction::FocusMain));
        assert_eq!(dispatch_global("ctrl-p"), Some(GlobalAction::OpenPalette));
        // Backspace is never mistaken for the Ctrl+H focus move.
        assert_eq!(dispatch_global("backspace"), None);

        let dispatch_sidebar =
            |spec: &str| keymaps.sidebar.dispatch(chord_from_keystroke(&ks(spec, None)).unwrap());
        assert_eq!(dispatch_sidebar("j"), Some(SidebarAction::NavDown));
        assert_eq!(dispatch_sidebar("down"), Some(SidebarAction::NavDown));
        assert_eq!(dispatch_sidebar("k"), Some(SidebarAction::NavUp));
        assert_eq!(dispatch_sidebar("up"), Some(SidebarAction::NavUp));
        assert_eq!(dispatch_sidebar("enter"), Some(SidebarAction::Activate));
        assert_eq!(dispatch_sidebar("space"), Some(SidebarAction::Activate));
    }
}
