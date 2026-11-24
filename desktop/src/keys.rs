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

use gpui::{Keystroke, Modifiers};
use shelbi_app::nav::View;
use shelbi_state::keymap::{format_chord, DisplayStyle, Key, KeyChord, Mods};

use crate::elements::terminal::FontZoom;

// ---------------------------------------------------------------------------
// The platform's key conventions.
//
// Every app chord is written once, in its macOS form (`super-w`), and
// [`platform_chord`] spells it for the platform the app was built for. On
// Linux (Omarchy) the app modifier is Ctrl+Shift, as in Alacritty, Ghostty
// and Kitty: a terminal pane keeps every plain Ctrl key (Ctrl+C, Ctrl+L,
// Ctrl+W ...) and Super stays with the compositor (Hyprland's binds). The
// chord is the same in every pane, terminal or not. Text fields follow the
// platform's text convention instead ([`text_mod`]): plain Ctrl on Linux,
// as in GTK, and ⌘ on macOS.
// ---------------------------------------------------------------------------

/// Whether this build follows the macOS key conventions (⌘ chords) rather
/// than the Linux ones (Ctrl+Shift chords).
pub const MAC: bool = cfg!(target_os = "macos");

/// The modifiers every app chord holds: ⌘ on macOS, Ctrl+Shift on Linux.
pub fn app_mods() -> Mods {
    if MAC {
        Mods::SUPER
    } else {
        Mods::CONTROL | Mods::SHIFT
    }
}

/// The modifier of the sidebar's numbered rows (`row_hotkey`): ⌃ on macOS,
/// where ⌘ holds the tabs. On Linux plain Ctrl is the terminal's and Ctrl+Shift
/// holds the tabs, so the rows are Alt+1 to Alt+9 (GNOME Terminal's and
/// Firefox's tab keys).
pub fn row_mods() -> Mods {
    if MAC {
        Mods::CONTROL
    } else {
        Mods::ALT
    }
}

/// `chord`, written in its macOS form, as this platform spells it. On Linux
/// Cmd becomes Ctrl+Shift (a Shift or Ctrl the chord already holds merges
/// into them; Alt carries over); a chord without Cmd is unchanged.
pub fn platform_chord(chord: KeyChord) -> KeyChord {
    if MAC || !chord.mods.contains(Mods::SUPER) {
        return chord;
    }
    KeyChord {
        mods: chord.mods.without(Mods::SUPER) | Mods::CONTROL | Mods::SHIFT,
        ..chord
    }
}

/// [`platform_chord`] of a macOS chord spelled as `keys.yaml` spells it
/// (`super-,`). The specs are the app's own constants, so they parse.
pub fn app_chord(mac_spec: &str) -> KeyChord {
    platform_chord(KeyChord::parse(mac_spec).unwrap_or_else(|e| panic!("app chord {mac_spec}: {e}")))
}

/// How this platform writes chords: the macOS glyphs (`⇧⌘N`) or the Linux
/// words (`Ctrl+Shift+N`).
pub fn display_style() -> DisplayStyle {
    DisplayStyle::detect()
}

/// [`chord_text`] in this platform's [`display_style`].
pub fn chord_label(chord: &KeyChord) -> String {
    chord_text(chord, display_style())
}

/// [`app_chord`] as this platform writes it (`⌘B`, `Ctrl+Shift+B`): the
/// tooltips and hints that name an app chord.
pub fn app_label(mac_spec: &str) -> String {
    chord_label(&app_chord(mac_spec))
}

/// Whether exactly the app modifier is held (⌘ on macOS, Ctrl+Shift on
/// Linux), nothing else.
pub fn is_app(m: &Modifiers) -> bool {
    if MAC {
        m.platform && !m.control && !m.alt && !m.shift && !m.function
    } else {
        m.control && m.shift && !m.alt && !m.platform && !m.function
    }
}

/// The key of an app chord (`f` for ⌘F / Ctrl+Shift+F), or `None` when
/// `keystroke` holds anything but exactly the app modifier. On Linux a
/// shifted symbol comes back unshifted (`,` for Ctrl+Shift+, which gpui
/// reports as `<`).
pub fn app_key(keystroke: &Keystroke) -> Option<String> {
    let ks = normalized(keystroke);
    is_app(&ks.modifiers).then_some(ks.key)
}

/// The text-editing modifier a text field's commands use (copy, undo, bold,
/// select all): ⌘ on macOS, Ctrl on Linux (GTK's). Exactly that modifier,
/// Shift allowed (⇧⌘Z, Ctrl+Shift+Z).
pub fn text_mod(m: &Modifiers) -> bool {
    if MAC {
        m.platform && !m.control
    } else {
        m.control && !m.platform
    }
}

/// A text field's chord written in its macOS form (`super-b`), as this
/// platform spells it: on Linux Cmd is plain Ctrl (GTK's text keys, Ctrl+B),
/// not the app modifier.
pub fn text_chord(mac_spec: &str) -> KeyChord {
    let chord = KeyChord::parse(mac_spec).unwrap_or_else(|e| panic!("text chord {mac_spec}: {e}"));
    if MAC || !chord.mods.contains(Mods::SUPER) {
        return chord;
    }
    KeyChord {
        mods: chord.mods.without(Mods::SUPER) | Mods::CONTROL,
        ..chord
    }
}

/// [`text_chord`] as this platform writes it (`⌘B`, `Ctrl+B`).
pub fn text_label(mac_spec: &str) -> String {
    chord_label(&text_chord(mac_spec))
}

/// Whether `chord` is a dialog's submit, ⌘↵: on Linux Ctrl+↵ (a text
/// field's convention there, so a multi-line field's Return stays a new
/// line) or the app chord Ctrl+Shift+↵.
pub fn is_submit(chord: &KeyChord) -> bool {
    chord.code == Key::Enter && (chord.mods == app_mods() || (!MAC && chord.mods == Mods::CONTROL))
}

/// The modifier a click on a link holds to open it: ⌘ on macOS, Ctrl on
/// Linux (Ghostty's, Kitty's and VS Code's).
pub fn link_mod(m: &Modifiers) -> bool {
    if MAC {
        m.platform
    } else {
        m.control
    }
}

/// The key a symbol is typed with Shift on a US layout (`<` is Shift+`,`).
/// gpui's Linux keyboard reports Ctrl+Shift+, as `<` with Shift dropped, so a
/// chord is put back to the key it was pressed on.
fn unshifted(c: char) -> Option<char> {
    const PAIRS: &[(char, char)] = &[
        ('!', '1'), ('@', '2'), ('#', '3'), ('$', '4'), ('%', '5'), ('^', '6'), ('&', '7'),
        ('*', '8'), ('(', '9'), (')', '0'), ('_', '-'), ('+', '='), ('<', ','), ('>', '.'),
        ('?', '/'), (':', ';'), ('"', '\''), ('{', '['), ('}', ']'), ('|', '\\'), ('~', '`'),
    ];
    PAIRS.iter().find_map(|(shifted, base)| (*shifted == c).then_some(*base))
}

/// The symbol a key types with Shift on a US layout (`<` for `,`): the
/// inverse of [`unshifted`], for spelling a binding the way gpui's Linux
/// keyboard reports the key.
pub fn shifted(c: char) -> Option<char> {
    ('!'..='~').find(|s| unshifted(*s) == Some(c))
}

/// `keystroke` as macOS reports it, so one chord table reads both platforms.
/// On Linux, a shifted symbol pressed with Ctrl or Alt (gpui reports
/// Ctrl+Shift+1 as Ctrl+`!`) is the key it was pressed on, with Shift
/// (Ctrl+Shift+1). Unchanged on macOS, and for a symbol typed with Shift
/// alone (`?` is typing, not a chord).
pub fn normalized(keystroke: &Keystroke) -> Keystroke {
    normalized_in(keystroke, MAC)
}

/// [`normalized`] under macOS's conventions (`mac`, unchanged) or Linux's.
pub(crate) fn normalized_in(keystroke: &Keystroke, mac: bool) -> Keystroke {
    let m = &keystroke.modifiers;
    if mac || !(m.control || m.alt) || m.shift {
        return keystroke.clone();
    }
    let mut chars = keystroke.key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return keystroke.clone();
    };
    let Some(base) = unshifted(c) else {
        return keystroke.clone();
    };
    let mut out = keystroke.clone();
    out.key = base.to_string();
    out.modifiers.shift = true;
    out
}

/// The window's Focus Sidebar chord that a focused terminal never keeps: on
/// Linux Ctrl+Shift+H, since `keys.yaml`'s Ctrl+H is the terminal's there
/// (Backspace to most programs). `None` on macOS, where `keys.yaml`'s chord
/// is taken from the terminal as it always was.
pub fn focus_sidebar_chord() -> Option<KeyChord> {
    (!MAC).then(|| app_chord("super-h"))
}

/// The app's own palette chord: ⌘K, Ctrl+Shift+K on Linux (where the shared
/// `PLATFORM_PALETTE_CHORD`, Super+K, is the compositor's).
pub fn palette_chord() -> KeyChord {
    platform_chord(shelbi_app::overlay::PLATFORM_PALETTE_CHORD)
}

/// Whether `chord` opens (and closes) the palette: `keys.yaml`'s chords and
/// the reserved ones (`shelbi_app::overlay::opens_palette`), and
/// [`palette_chord`].
pub fn opens_palette(keymaps: &shelbi_state::keymap::Keymaps, chord: &KeyChord) -> bool {
    shelbi_app::overlay::opens_palette(keymaps, chord) || *chord == palette_chord()
}

/// File ▸ New Window as this platform binds it: ⌘⇧N on macOS, Ctrl+Shift+N
/// on Linux. The window takes it before a focused terminal can
/// (`crate::app`), and the menu shows it on the item.
pub fn new_window_chord() -> KeyChord {
    app_chord("shift-super-n")
}

/// Whether `chord` is [`new_window_chord`].
pub fn is_new_window(chord: &KeyChord) -> bool {
    new_window_chord().canonical() == chord.canonical()
}

/// The terminal font zoom chords (View ▸ Bigger / Smaller / Actual Size), as
/// this platform binds them: ⌘+ ⌘- ⌘0 on macOS, Ctrl+Shift+= Ctrl+Shift+-
/// Ctrl+Shift+0 on Linux (Kitty's). The window takes them before a focused
/// terminal can (`crate::app`), and the menu shows them on the items.
pub fn font_zoom_chord(step: FontZoom) -> KeyChord {
    match step {
        FontZoom::Bigger if MAC => app_chord("super-+"),
        FontZoom::Bigger => app_chord("super-="),
        FontZoom::Smaller => app_chord("super--"),
        FontZoom::ActualSize => app_chord("super-0"),
    }
}

/// The font zoom step `chord` asks for. ⌘+ is Shift+= on a US layout, which
/// macOS reports as `+` (gpui drops the Shift), so ⌘= (no Shift) and a
/// Shift that a layout keeps both count as Bigger, as in Terminal and iTerm.
/// On Linux the chords hold Ctrl+Shift, so `=` and `+` are both Bigger there
/// too.
pub fn font_zoom(chord: &KeyChord) -> Option<FontZoom> {
    let app = app_mods();
    let shift = chord.mods == app | Mods::SHIFT && app != app | Mods::SHIFT;
    if chord.mods != app && !shift {
        return None;
    }
    match chord.code {
        Key::Char('+' | '=') => Some(FontZoom::Bigger),
        Key::Char('-') if !shift => Some(FontZoom::Smaller),
        Key::Char('0') if !shift => Some(FontZoom::ActualSize),
        _ => None,
    }
}

/// One of the app's fixed shortcuts: what it does and its chord on this
/// platform. A numbered pair (the tabs, the rows) is listed once, by its
/// first chord, with a label that names the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppShortcut {
    pub label: &'static str,
    pub chord: KeyChord,
}

/// The app's fixed shortcuts on this platform, in the order Settings ▸
/// Keybindings and the `keys.yaml` stub list them. They are built into the
/// app, not read from `keys.yaml` (making them rebindable is a follow-up).
/// Every one is written in its macOS form and spelled for Linux by
/// [`platform_chord`]: none holds Super there (Hyprland's) and none is a
/// plain Ctrl chord (a terminal's).
pub fn app_shortcuts() -> Vec<AppShortcut> {
    use crate::menus::tree::{fixed_chord, AppCommand};
    let mut out = Vec::new();
    let mut push = |label: &'static str, chord: Option<KeyChord>| {
        if let Some(chord) = chord {
            out.push(AppShortcut { label, chord });
        }
    };
    push("Command palette", Some(palette_chord()));
    push("Settings", fixed_chord(AppCommand::Settings));
    push("New window", Some(new_window_chord()));
    push("Close window", fixed_chord(AppCommand::CloseWindow));
    push("Quit Shelbi (agents keep running)", fixed_chord(AppCommand::QuitClient));
    push("Hide Shelbi", fixed_chord(AppCommand::Hide));
    push("Hide others", fixed_chord(AppCommand::HideOthers));
    push("Minimize", fixed_chord(AppCommand::Minimize));
    push("Show or hide the sidebar", fixed_chord(AppCommand::ToggleSidebar));
    push("Focus the sidebar from a terminal", focus_sidebar_chord());
    push("Orchestrator", nav_view_chord(&View::Session("orch".into())));
    push("Issues", nav_view_chord(&View::Issues));
    push("Changelog", nav_view_chord(&View::Changelog));
    push("Project tabs 1 to 9", Some(KeyChord::new(Key::Char('1'), app_mods())));
    push("Sidebar rows 1 to 9", Some(KeyChord::new(Key::Char('1'), row_mods())));
    push("Copy", fixed_chord(AppCommand::Copy));
    push("Paste", fixed_chord(AppCommand::Paste));
    push("Cut", fixed_chord(AppCommand::Cut));
    push("Select all", fixed_chord(AppCommand::SelectAll));
    push("Find in a terminal", Some(app_chord("super-f")));
    push("Terminal font bigger", Some(font_zoom_chord(FontZoom::Bigger)));
    push("Terminal font smaller", Some(font_zoom_chord(FontZoom::Smaller)));
    push("Terminal font actual size", Some(font_zoom_chord(FontZoom::ActualSize)));
    push("Delete the open issue", Some(app_chord("super-backspace")));
    out
}

/// The comment block the `keys.yaml` stub ends with on this platform: the
/// app's fixed shortcuts ([`app_shortcuts`]), which the file doesn't bind.
pub fn keys_stub_shortcuts() -> String {
    let platform = if MAC { "macOS" } else { "Linux" };
    let mut out = format!(
        "#\n# The desktop app's own shortcuts on {platform} are built in, not read from\n\
         # this file:\n"
    );
    let width = app_shortcuts().iter().map(|s| chord_label(&s.chord).chars().count()).max().unwrap_or(0);
    for s in app_shortcuts() {
        let chord = chord_label(&s.chord);
        out.push_str(&format!("#   {chord:<width$}  {}\n", s.label));
    }
    if !MAC {
        out.push_str(
            "# A terminal pane keeps every plain Ctrl key: a Ctrl binding below works\n\
             # outside terminals only.\n",
        );
    }
    out
}

/// What one of the sidebar's own chords opens: the Navigation component's
/// hotkeys, the desktop's fixed bindings (not `keys.yaml`'s, which the TUI
/// shares).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavChord {
    /// ⌘O, ⌘I, ⌘L (Ctrl+Shift+O, I, L on Linux): the Orchestrator, Issues
    /// and Changelog views.
    View(View),
    /// ⌃1 to ⌃9 (Alt+1 to Alt+9 on Linux, [`row_mods`]): the sidebar row the
    /// hint numbers (reviews, then working workspaces), or a task panel's
    /// view while one holds the column.
    Row(u8),
    /// ⌘1 to ⌘9 (Ctrl+Shift+1 to 9 on Linux): the project tab
    /// (`project-tabs`), or the Settings page with that number while Settings
    /// is open.
    Tab(u8),
}

/// The nav views' chords, as `keys.yaml` chord strings in their macOS form
/// ([`platform_chord`] spells them for Linux).
const NAV_VIEW_CHORDS: &[(&str, &str)] = &[("orch", "super-o"), ("tasks", "super-i"), ("changelog", "super-l")];

/// The chord that opens `view` from anywhere in the window. `None` for a
/// view without one (Machines).
pub fn nav_view_chord(view: &View) -> Option<KeyChord> {
    let id = view.as_view_id();
    NAV_VIEW_CHORDS
        .iter()
        .find(|(v, _)| *v == id)
        .and_then(|(_, spec)| KeyChord::parse(spec).ok())
        .map(platform_chord)
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
/// (`back-tab`) is `⇧`, `Tab`. On Linux the modifiers are words (`Ctrl`,
/// `Alt`, `Shift`, `Super`) and Return is `Enter`, as the keys are labeled.
pub fn keycap_keys(chord: &KeyChord) -> Vec<String> {
    keycap_keys_in(chord, MAC)
}

fn keycap_keys_in(chord: &KeyChord, mac: bool) -> Vec<String> {
    let m = chord.mods;
    // A two-key binding's first key, as caps of its own (`G`, `G`).
    let mut keys = chord.leader.map_or_else(Vec::new, |leader| keycap_keys_in(&leader.chord(), mac));
    let glyphs: [(Mods, &str); 4] = if mac {
        [(Mods::CONTROL, "⌃"), (Mods::ALT, "⌥"), (Mods::SHIFT, "⇧"), (Mods::SUPER, "⌘")]
    } else {
        [(Mods::CONTROL, "Ctrl"), (Mods::ALT, "Alt"), (Mods::SHIFT, "Shift"), (Mods::SUPER, "Super")]
    };
    for (bit, glyph) in glyphs {
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
        Key::Enter if mac => "Return".into(),
        Key::Enter => "Enter".into(),
        Key::Esc => "Esc".into(),
        Key::Tab | Key::BackTab => "Tab".into(),
        Key::Backspace if mac => "⌫".into(),
        Key::Backspace => "Backspace".into(),
        Key::Delete if mac => "⌦".into(),
        Key::Delete => "Delete".into(),
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

/// [`nav_view_chord`] as the sidebar and the Go menu show it (`⌘O`,
/// `Ctrl+Shift+O`).
pub fn nav_view_hotkey(view: &View) -> Option<String> {
    nav_view_chord(view).map(|c| chord_label(&c))
}

/// A numbered sidebar row's hint (`⌃3`, `Alt+3` on Linux), a task panel
/// view's and a Settings page's too.
pub fn row_hotkey(n: u8) -> String {
    chord_label(&KeyChord::new(Key::Char((b'0' + n) as char), row_mods()))
}

/// A project tab's hint (`⌘3`, `Ctrl+Shift+3` on Linux), shown while the
/// app modifier is held.
pub fn tab_hotkey(n: u8) -> String {
    chord_label(&KeyChord::new(Key::Char((b'0' + n) as char), app_mods()))
}

/// Whether `chord` is one of the window's own chords, and what it opens.
/// ⌃1 to ⌃9 are the sidebar's rows, ⌘1 to ⌘9 the tabs, ⌘O ⌘I ⌘L the nav
/// views (Alt+1 to 9, Ctrl+Shift+1 to 9 and Ctrl+Shift+O, I, L on Linux).
pub fn nav_chord(chord: &KeyChord) -> Option<NavChord> {
    let digit = match chord.code {
        Key::Char(c @ '1'..='9') if chord.leader.is_none() => Some(c as u8 - b'0'),
        _ => None,
    };
    if chord.mods == row_mods() {
        return digit.map(NavChord::Row);
    }
    if chord.mods != app_mods() {
        return None;
    }
    if let Some(n) = digit {
        return Some(NavChord::Tab(n));
    }
    let canonical = chord.canonical();
    NAV_VIEW_CHORDS.iter().find_map(|(view, spec)| {
        let bound = platform_chord(KeyChord::parse(spec).ok()?);
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
/// Super; the fn key is not modeled. On Linux a shifted symbol held with
/// Ctrl or Alt is its key with Shift ([`normalized`]): Ctrl+Shift+1 is
/// `ctrl-shift-1`, as macOS reports it.
pub fn chord_from_keystroke(keystroke: &Keystroke) -> Option<KeyChord> {
    let keystroke = &normalized(keystroke);
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

/// Test helpers: an app chord as this platform's keystroke.
#[cfg(test)]
pub(crate) mod test_keys {
    use super::*;

    fn spell(token: &str, map: fn(KeyChord) -> KeyChord) -> String {
        let spec = token.replace("cmd-", "super-");
        if !spec.contains("super-") {
            return token.to_string();
        }
        let chord = map(KeyChord::parse(&spec).unwrap_or_else(|e| panic!("{token}: {e}")));
        crate::menus::tree::keystroke(&chord).unwrap_or_else(|| panic!("{token}: no keystroke"))
    }

    /// `mac` (gpui keystrokes, space-separated, `cmd-` for ⌘) with every
    /// ⌘ chord spelled as this platform's app chord: unchanged on macOS,
    /// Ctrl+Shift on Linux (`cmd-w` is `ctrl-shift-w`, `cmd-1` is
    /// `ctrl-shift-!`... as gpui's Linux keyboard reports it).
    pub fn app_spec(mac: &str) -> String {
        mac.split_whitespace().map(|t| spell(t, platform_chord)).collect::<Vec<_>>().join(" ")
    }

    /// [`app_spec`] for a text field's ⌘ chords: plain Ctrl on Linux.
    pub fn text_spec(mac: &str) -> String {
        mac.split_whitespace()
            .map(|t| {
                spell(t, |c| {
                    if MAC || !c.mods.contains(Mods::SUPER) {
                        c
                    } else {
                        KeyChord {
                            mods: c.mods.without(Mods::SUPER) | Mods::CONTROL,
                            ..c
                        }
                    }
                })
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// A sidebar row chord written as macOS's (`ctrl-3`), as this platform's
    /// ([`row_mods`]: `alt-3` on Linux).
    pub fn row_spec(mac: &str) -> String {
        if MAC {
            mac.to_string()
        } else {
            mac.replacen("ctrl-", "alt-", 1)
        }
    }

    /// [`app_spec`] as a gpui keystroke.
    pub fn app_keystroke(mac: &str) -> Keystroke {
        Keystroke::parse(&app_spec(mac)).unwrap()
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
        let caps = |spec: &str| keycap_keys_in(&KeyChord::parse(spec).unwrap(), true).concat();
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
        assert_eq!(keycap_keys_in(&KeyChord::parse("ctrl-shift-p").unwrap(), true), ["⌃", "⇧", "P"]);
    }

    #[test]
    fn linux_keycaps_spell_the_modifiers_as_words() {
        let caps = |spec: &str| keycap_keys_in(&KeyChord::parse(spec).unwrap(), false);
        assert_eq!(caps("ctrl-shift-p"), ["Ctrl", "Shift", "P"]);
        assert_eq!(caps("alt-3"), ["Alt", "3"]);
        assert_eq!(caps("super-k"), ["Super", "K"]);
        assert_eq!(caps("enter"), ["Enter"]);
        assert_eq!(caps("ctrl-shift-backspace"), ["Ctrl", "Shift", "Backspace"]);
    }

    #[test]
    fn linux_spells_app_chords_with_ctrl_shift() {
        // `platform_chord` on Linux, whatever this build's platform.
        let linux = |spec: &str| {
            let c = KeyChord::parse(spec).unwrap();
            if c.mods.contains(Mods::SUPER) {
                KeyChord { mods: c.mods.without(Mods::SUPER) | Mods::CONTROL | Mods::SHIFT, ..c }.canonical()
            } else {
                c.canonical()
            }
        };
        if !MAC {
            for spec in ["super-w", "shift-super-n", "ctrl-super-s", "alt-super-h", "ctrl-p"] {
                assert_eq!(platform_chord(KeyChord::parse(spec).unwrap()).canonical(), linux(spec), "{spec}");
            }
        }
        assert_eq!(linux("super-w"), KeyChord::parse("ctrl-shift-w").unwrap().canonical());
        assert_eq!(linux("shift-super-n"), KeyChord::parse("ctrl-shift-n").unwrap().canonical());
        assert_eq!(linux("ctrl-super-s"), KeyChord::parse("ctrl-shift-s").unwrap().canonical());
        assert_eq!(linux("ctrl-p"), KeyChord::parse("ctrl-p").unwrap().canonical());
    }

    #[test]
    fn linux_keystrokes_come_back_to_the_key_they_were_pressed_on() {
        let ks = |spec: &str| Keystroke::parse(spec).unwrap();
        let norm = |spec: &str| {
            let k = normalized_in(&ks(spec), false);
            (k.key, k.modifiers.control, k.modifiers.shift)
        };
        // gpui reports Ctrl+Shift+1 as Ctrl+`!` (Shift dropped).
        assert_eq!(norm("ctrl-!"), ("1".to_string(), true, true));
        assert_eq!(norm("ctrl-<"), (",".to_string(), true, true));
        assert_eq!(norm("ctrl-+"), ("=".to_string(), true, true));
        assert_eq!(norm("ctrl-_"), ("-".to_string(), true, true));
        assert_eq!(norm("ctrl-*"), ("8".to_string(), true, true));
        // Letters keep their Shift; plain typing is untouched.
        assert_eq!(norm("ctrl-shift-n"), ("n".to_string(), true, true));
        assert_eq!(norm("?").0, "?");
        assert_eq!(norm("ctrl-c"), ("c".to_string(), true, false));
        // macOS reports the key itself: unchanged.
        assert_eq!(normalized_in(&ks("ctrl-!"), true).key, "!");
        // The inverse table round-trips.
        for c in ['1', '0', ',', '.', '/', '=', '-', '[', ']', '\\', ';', '\'', '`'] {
            assert_eq!(shifted(c).and_then(unshifted), Some(c), "{c}");
        }
        assert_eq!(shifted('a'), None);
    }

    fn chord(spec: &str, key_char: Option<&str>) -> String {
        chord_from_keystroke(&ks(spec, key_char))
            .map(|c| c.canonical())
            .unwrap_or_else(|| "<none>".into())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_shift_n_is_new_window() {
        let chord = |spec: &str| chord_from_keystroke(&test_keys::app_keystroke(spec)).unwrap();
        assert!(is_new_window(&chord("cmd-shift-n")));
        assert!(!is_new_window(&chord("ctrl-n")));
        if MAC {
            assert!(!is_new_window(&chord("cmd-n")));
            assert!(!is_new_window(&chord("ctrl-shift-n")));
        } else {
            // Super is Hyprland's.
            assert!(!is_new_window(&chord_from_keystroke(&ks("cmd-shift-n", Some("n"))).unwrap()));
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn ctrl_shift_n_is_new_window_on_linux() {
        let chord = |spec| chord_from_keystroke(&ks(spec, Some("n"))).unwrap();
        assert!(is_new_window(&chord("ctrl-shift-n")));
        assert!(!is_new_window(&chord("ctrl-n")), "the terminal's");
        assert!(!is_new_window(&chord("cmd-shift-n")), "Hyprland's");
    }

    #[test]
    fn cmd_plus_equals_minus_and_zero_are_the_font_zoom() {
        let zoom = |spec: &str, key_char: &str| {
            font_zoom(&chord_from_keystroke(&ks(&test_keys::app_spec(spec), Some(key_char))).unwrap())
        };
        // ⌘+ as macOS reports it on a US layout (Shift dropped), and as a
        // layout that keeps the Shift would.
        assert_eq!(zoom("cmd-+", "+"), Some(FontZoom::Bigger));
        assert_eq!(zoom("cmd-shift-=", "+"), Some(FontZoom::Bigger));
        assert_eq!(zoom("cmd-=", "="), Some(FontZoom::Bigger));
        assert_eq!(zoom("cmd--", "-"), Some(FontZoom::Smaller));
        assert_eq!(zoom("cmd-0", "0"), Some(FontZoom::ActualSize));
        // Other modifiers, or none, are someone else's.
        for (spec, key_char) in [("=", "="), ("-", "-"), ("0", "0"), ("ctrl-0", "0"), ("alt-cmd-0", "0"), ("cmd-9", "9")] {
            assert_eq!(zoom(spec, key_char), None, "{spec}");
        }
        // On macOS ⌃⌘- and ⇧⌘0 are other chords (on Linux the Ctrl and the
        // Shift are the app modifier's).
        if MAC {
            assert_eq!(zoom("ctrl-cmd--", "-"), None);
            assert_eq!(zoom("cmd-shift-0", ")"), None);
        }
        // The menu chords are the zoom chords.
        for step in [FontZoom::Bigger, FontZoom::Smaller, FontZoom::ActualSize] {
            assert_eq!(font_zoom(&font_zoom_chord(step)), Some(step), "{step:?}");
        }
    }

    #[test]
    fn the_sidebar_chords_open_the_nav_views_and_numbered_rows() {
        let chord = |spec: &str, typed| {
            let spec = test_keys::app_spec(&if MAC { spec.to_string() } else { spec.replace("ctrl-", "alt-") });
            chord_from_keystroke(&ks(&spec, Some(typed))).unwrap()
        };
        assert_eq!(nav_chord(&chord("cmd-o", "o")), Some(NavChord::View(View::Session("orch".into()))));
        assert_eq!(nav_chord(&chord("cmd-i", "i")), Some(NavChord::View(View::Issues)));
        assert_eq!(nav_chord(&chord("cmd-l", "l")), Some(NavChord::View(View::Changelog)));
        assert_eq!(nav_chord(&chord("cmd-1", "1")), Some(NavChord::Tab(1)));
        assert_eq!(nav_chord(&chord("cmd-9", "9")), Some(NavChord::Tab(9)));
        assert_eq!(nav_chord(&chord("cmd-0", "0")), None);
        assert_eq!(nav_chord(&chord("ctrl-1", "1")), Some(NavChord::Row(1)));
        assert_eq!(nav_chord(&chord("ctrl-9", "9")), Some(NavChord::Row(9)));
        assert_eq!(nav_chord(&chord("ctrl-0", "0")), None);
        if MAC {
            assert_eq!(nav_chord(&chord("ctrl-cmd-1", "1")), None);
            assert_eq!(nav_chord(&chord("alt-cmd-1", "1")), None, "the doc editor's heading chord");
            assert_eq!(nav_chord(&chord("cmd-shift-o", "O")), None);
        }
        assert_eq!(nav_chord(&chord("o", "o")), None);
        assert_eq!(nav_chord(&chord_from_keystroke(&ks("ctrl-o", Some("o"))).unwrap()), None);
        let label = |mac: &str, linux: &str| Some(if MAC { mac } else { linux }.to_string());
        assert_eq!(nav_view_hotkey(&View::Session("orch".into())), label("⌘O", "Ctrl+Shift+O"));
        assert_eq!(nav_view_hotkey(&View::Issues), label("⌘I", "Ctrl+Shift+I"));
        assert_eq!(nav_view_hotkey(&View::Changelog), label("⌘L", "Ctrl+Shift+L"));
        assert_eq!(nav_view_hotkey(&View::Activity), None, "Activity is palette-only");
        assert_eq!(nav_view_hotkey(&View::Machines), None);
        assert_eq!(nav_view_hotkey(&View::Jobs), None, "Jobs is palette-only: no chord");
        assert_eq!(nav_view_hotkey(&View::Reminders), None, "Reminders is palette-only: no chord");
        assert_eq!(row_hotkey(4), if MAC { "⌃4" } else { "Alt+4" });
        assert_eq!(tab_hotkey(4), if MAC { "⌘4" } else { "Ctrl+Shift+4" });
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

    #[test]
    fn the_app_shortcuts_keep_off_the_terminal_and_the_compositor() {
        let shortcuts = app_shortcuts();
        assert!(shortcuts.len() > 15);
        let mut seen = std::collections::HashMap::new();
        for s in &shortcuts {
            if let Some(other) = seen.insert(s.chord.canonical(), s.label) {
                panic!("{} and {other} share {}", s.label, s.chord.canonical());
            }
            if !MAC {
                assert!(!s.chord.mods.contains(Mods::SUPER), "{}: Super is Hyprland's", s.label);
                assert_ne!(s.chord.mods, Mods::CONTROL, "{}: plain Ctrl is the terminal's", s.label);
                assert!(!s.chord.mods.is_empty(), "{}", s.label);
            }
        }
        // The stub documents every one.
        let stub = keys_stub_shortcuts();
        for s in &shortcuts {
            assert!(stub.contains(s.label), "{}", s.label);
            assert!(stub.lines().all(|l| l.is_empty() || l.starts_with('#')), "comments only");
        }
        // And it parses as an empty keys.yaml.
        let (_, diags) = shelbi_state::keymap::validate_keymaps_yaml(&stub, None);
        assert!(diags.is_empty(), "{diags:?}");
    }
}
