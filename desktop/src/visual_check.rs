//! `shelbi-desktop --visual-check <dir>` (feature `visual-check`): exercise
//! the terminal element end to end without a screen.
//!
//! It opens an offscreen window through gpui's `VisualTestAppContext` (the
//! real macOS platform and Metal renderer, but frames rendered to a texture
//! and read back, so it works on a locked or headless machine and needs no
//! screen-recording permission), attaches a [`TerminalPane`] to a fake session
//! that records what the pane sends, and then:
//!
//! - feeds a showcase (SGR attributes, the ANSI sixteen, the 256-color cube and
//!   ramp, true color, CJK, emoji, combining marks, box drawing) and saves
//!   `showcase.png`, plus frames with a selection, a search, and an IME
//!   composition;
//! - drives gpui key events through the real dispatch path (key listeners,
//!   then the input handler) and checks the bytes that reach the fake session;
//! - drives mouse events and checks the ownership rules (wheel and drag are
//!   Shelbi's until the program reports the mouse; Shift reclaims them);
//! - resizes the window and checks the debounced size report;
//! - checks the fonts (Source Code Pro bundled, every showcase glyph found in some
//!   font, a live font change re-measuring the grid), the find bar, word /
//!   line / block selection, the scrollbar and "Jump to bottom", output not
//!   yanking a scrolled-up view, Cmd-hover and Cmd+click links, the
//!   alternate screen (wheel as arrows, selection), the mouse modes, the
//!   Option key setting, and IME composition, cancel, and dead keys;
//! - times frames: an idle redraw, and a `yes`-style flood fed in chunks.
//!
//! It prints one line per check and exits non-zero if any failed.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, point, prelude::*, px, size, AnyWindowHandle, AppContext, ClipboardItem, Context, Entity,
    Modifiers, MouseButton, Pixels, Point, ScrollDelta, ScrollWheelEvent, TouchPhase,
    VisualTestAppContext, Window, WindowHandle,
};
use shelbi_term::Size;

use crate::elements::terminal::session::SessionIo;
use crate::elements::terminal::{TerminalPane, TerminalSettings};
use crate::theme::Theme;
use gpui::{MouseDownEvent, MouseUpEvent};
use shelbi_app::theme::ThemePreference;
use shelbi_term::links::LinkTarget;

/// What the pane sent to its (fake) session.
#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Input(Vec<u8>),
    Paste(String, bool),
    Resize(Size),
}

#[derive(Clone, Default)]
struct Recorder(Rc<RefCell<Vec<Sent>>>);

impl Recorder {
    fn take(&self) -> Vec<Sent> {
        std::mem::take(&mut self.0.borrow_mut())
    }

    fn input(&self) -> Vec<u8> {
        self.take()
            .into_iter()
            .filter_map(|s| match s {
                Sent::Input(b) => Some(b),
                _ => None,
            })
            .flatten()
            .collect()
    }
}

impl SessionIo for Recorder {
    fn input(&self, bytes: Vec<u8>) {
        self.0.borrow_mut().push(Sent::Input(bytes));
    }

    fn paste(&self, text: String, bracketed: bool) {
        self.0.borrow_mut().push(Sent::Paste(text, bracketed));
    }

    fn resize(&self, size: Size) {
        self.0.borrow_mut().push(Sent::Resize(size));
    }
}

/// The window's root: the pane, filling the window or held at a set size
/// (the resize check changes it, as a split or a sidebar drag would).
struct Harness {
    pane: Entity<TerminalPane>,
    size: Option<gpui::Size<Pixels>>,
}

impl Render for Harness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let pane = div().child(self.pane.clone());
        match self.size {
            Some(s) => pane.w(s.width).h(s.height),
            None => pane.size_full(),
        }
    }
}

struct Check {
    cx: VisualTestAppContext,
    handle: WindowHandle<Harness>,
    window: AnyWindowHandle,
    io: Recorder,
    out: PathBuf,
    failures: usize,
}

/// Run the visual check, saving frames under `out`.
pub fn run(out: PathBuf) -> ! {
    std::fs::create_dir_all(&out).expect("create the output directory");
    let platform = gpui_platform::current_platform(false);
    let mut cx = VisualTestAppContext::with_asset_source(platform, Arc::new(crate::assets::Assets));
    cx.set_global(theme());
    cx.update(|cx| {
        crate::assets::load_fonts(cx);
        cx.set_global(TerminalSettings::default());
    });
    let io = Recorder::default();
    let pane_io = io.clone();
    let handle = cx
        .open_offscreen_window(size(px(1000.0), px(600.0)), move |window, cx| {
            let pane = cx.new(|cx| {
                TerminalPane::detached(
                    "visual-check".into(),
                    Size::new(80, 24),
                    Box::new(pane_io),
                    window,
                    cx,
                )
            });
            cx.new(|_| Harness { pane, size: None })
        })
        .expect("open the offscreen window");
    let window: AnyWindowHandle = handle.into();
    let mut check = Check {
        cx,
        handle,
        window,
        io,
        out,
        failures: 0,
    };
    check.all();
    let failed = check.failures;
    println!(
        "\n{} check(s) failed; frames saved under {}",
        failed,
        check.out.display()
    );
    std::process::exit(if failed == 0 { 0 } else { 1 })
}

impl Check {
    /// Run `f` against the pane.
    fn with_pane<R>(
        &mut self,
        f: impl FnOnce(&mut TerminalPane, &mut Window, &mut Context<TerminalPane>) -> R,
    ) -> Result<R, ()> {
        self.handle
            .update(&mut self.cx, |h, window, cx| {
                h.pane.update(cx, |pane, cx| f(pane, window, cx))
            })
            .map_err(|_| ())
    }

    fn all(&mut self) {
        self.with_pane(|pane, window, cx| window.focus(pane.focus(), cx))
            .unwrap();
        let first = self.draw();
        self.size_reports(first);
        self.showcase();
        self.fonts();
        self.keys();
        self.option_key();
        self.ime();
        self.mouse();
        self.mouse_modes();
        self.selection_modes();
        self.selection_autoscroll();
        self.selection_survives_output();
        self.scrolling();
        self.find_bar();
        self.links();
        self.alt_screen();
        self.taller_session();
        self.resize();
        self.timing();
    }

    // ---- plumbing ----

    fn expect(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) {
        if ok {
            println!("ok    {name}");
        } else {
            self.failures += 1;
            println!("FAIL  {name}: {detail}");
        }
    }

    /// Run a frame (layout, prepaint, paint) and return how long it took.
    fn draw(&mut self) -> Duration {
        self.cx.run_until_parked();
        self.cx
            .update_window(self.window, |_, window, cx| {
                let start = Instant::now();
                window.draw(cx).clear(cx);
                start.elapsed()
            })
            .unwrap()
    }

    fn save(&mut self, name: &str) {
        self.draw();
        let image = self
            .cx
            .capture_screenshot(self.window)
            .expect("render to image");
        let path = self.out.join(name);
        image.save(&path).expect("save the frame");
        println!("saved {}", path.display());
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.with_pane(|pane, _, cx| pane.feed(bytes, cx)).unwrap();
        self.draw();
    }

    fn keys_sent(&mut self, keystrokes: &str) -> Vec<u8> {
        self.io.take();
        self.cx.simulate_keystrokes(self.window, keystrokes);
        self.io.input()
    }

    fn cell(&mut self) -> (f32, f32) {
        self.with_pane(|pane, _, _| {
            let m = pane.metrics.as_ref().expect("measured");
            (f32::from(m.width), f32::from(m.height))
        })
        .unwrap()
    }

    /// The center of viewer cell (`col`, `row`).
    fn at(&mut self, col: u16, row: u16) -> Point<Pixels> {
        let (w, h) = self.cell();
        point(px((col as f32 + 0.5) * w), px((row as f32 + 0.5) * h))
    }

    fn reset_screen(&mut self) {
        // Leave modes the previous check set, then clear screen and history.
        self.feed(b"\x1b[?1000l\x1b[?1006l\x1b[?2004l\x1b[?1l\x1b[<u\x1b[0m\x1b[H\x1b[2J\x1b[3J");
    }

    // ---- checks ----

    fn size_reports(&mut self, first_frame: Duration) {
        let sent = self.io.take();
        let (w, h) = self.cell();
        let expected = Size::from_pixels(1000.0, 600.0, w, h);
        self.expect(
            "the first frame reports the pane's grid size to the session at once",
            sent.contains(&Sent::Resize(expected)),
            format!("sent {sent:?}, expected a resize to {expected:?}"),
        );
        println!(
            "      cell {w:.2}x{h:.2}px, grid {}x{}, first frame {first_frame:?}",
            expected.cols, expected.rows
        );
        // Lock the emulator to that size, as the session's echo would.
        self.with_pane(|pane, _, cx| pane.session_resized(expected, cx))
            .unwrap();
    }

    fn showcase(&mut self) {
        let mut s = String::new();
        s.push_str("\x1b[1mShelbi terminal element\x1b[0m: visual check\r\n\r\n");
        s.push_str("bold \x1b[1mBold\x1b[0m  italic \x1b[3mItalic\x1b[0m  bold-italic \x1b[1;3mBoth\x1b[0m  ");
        s.push_str("underline \x1b[4mUnder\x1b[0m  curly \x1b[4:3mCurly\x1b[0m  strike \x1b[9mStrike\x1b[0m  ");
        s.push_str("dim \x1b[2mDim\x1b[0m  inverse \x1b[7mInverse\x1b[0m\r\n\r\n");
        s.push_str("ansi fg  ");
        for i in 30..=37 {
            s.push_str(&format!("\x1b[{i}m{i} "));
        }
        for i in 90..=97 {
            s.push_str(&format!("\x1b[{i}m{i} "));
        }
        s.push_str("\x1b[0m\r\nansi bg  ");
        for i in 40..=47 {
            s.push_str(&format!("\x1b[{i}m   "));
        }
        for i in 100..=107 {
            s.push_str(&format!("\x1b[{i}m   "));
        }
        s.push_str("\x1b[0m\r\n256      ");
        for i in 16..=231u16 {
            s.push_str(&format!("\x1b[48;5;{i}m "));
            if (i - 16) % 72 == 71 {
                s.push_str("\x1b[0m\r\n         ");
            }
        }
        for i in 232..=255 {
            s.push_str(&format!("\x1b[48;5;{i}m "));
        }
        s.push_str("\x1b[0m\r\ntruecolor ");
        for i in 0..96 {
            let r = (i * 255 / 95) as u8;
            let b = 255 - r;
            s.push_str(&format!("\x1b[48;2;{r};64;{b}m "));
        }
        s.push_str("\x1b[0m\r\n\r\n");
        s.push_str("cjk 漢字 かな カナ 한국어 | emoji 👍 🎉 🚀 ✅ 👨‍👩‍👧 | combining cafe\u{301} nai\u{308}ve\r\n");
        s.push_str("box ┌────┬────┐ ▏▎▍▌▋▊▉█ ░▒▓ ⠿ ← → ↑ ↓\r\n");
        s.push_str("    └────┴────┘ \x1b[38;2;255;160;0mtrue-color text\x1b[0m \x1b[38;5;45m256-color text\x1b[0m\r\n\r\n");
        s.push_str("$ ");
        self.feed(s.as_bytes());
        self.save("showcase.png");

        // The view's text matches what was fed (wide and combining cells
        // included).
        let line = (0..30)
            .map(|r| self.row_text(r))
            .find(|t| t.starts_with("cjk"))
            .unwrap_or_default();
        self.expect(
            "wide characters, emoji, and combining marks land in their cells",
            line.starts_with("cjk 漢字 かな カナ 한국어 | emoji 👍 🎉 🚀 ✅"),
            format!("the cjk row reads {line:?}"),
        );

        // Search highlights, then a selection.
        self.with_pane(|pane, _, cx| {
            pane.terminal_view_mut().start_search("text", false);
            cx.notify();
        })
        .unwrap();
        self.save("search.png");
        self.with_pane(|pane, _, cx| {
            pane.terminal_view_mut().clear_search();
            cx.notify();
        })
        .unwrap();
    }

    fn row_text(&mut self, row: u16) -> String {
        self.with_pane(|pane, _, _| {
            use shelbi_term::selection::{Point as GridPoint, Selection};
            let view = pane.terminal_view();
            let cols = view.emulator().size().cols;
            let mut sel = Selection::start(GridPoint::new(row as i32, 0));
            sel.update(GridPoint::new(row as i32, cols - 1));
            sel.extract_text(view.emulator().grid())
        })
        .unwrap()
    }

    fn keys(&mut self) {
        self.reset_screen();
        let cases: &[(&str, &[u8], &str)] = &[
            ("a", b"a", "plain text goes through the input handler"),
            ("shift-a", b"A", "shifted text"),
            ("enter", b"\r", "Enter"),
            (
                "shift-enter",
                b"\r",
                "Shift+Enter without the kitty protocol is a bare CR",
            ),
            ("alt-enter", b"\x1b\r", "Option+Enter is Meta+Enter"),
            ("alt-backspace", b"\x1b\x7f", "Option+Backspace is Meta (delete a word), whatever the Option setting"),
            ("alt-left alt-right", b"\x1b[1;3D\x1b[1;3C", "Option+arrows are Meta (word motion)"),
            ("alt-delete", b"\x1b[3;3~", "Option+Delete is Meta"),
            ("tab", b"\t", "Tab"),
            ("shift-tab", b"\x1b[Z", "Shift+Tab is backtab"),
            ("backspace", b"\x7f", "Backspace is DEL"),
            ("ctrl-h", b"\x08", "Ctrl+H is BS"),
            ("ctrl-c", b"\x03", "Ctrl+C"),
            ("escape", b"\x1b", "Escape"),
            ("up down left right", b"\x1b[A\x1b[B\x1b[D\x1b[C", "arrows"),
            ("ctrl-left", b"\x1b[1;5D", "Ctrl+Left"),
            ("f1 f5 f12", b"\x1bOP\x1b[15~\x1b[24~", "function keys"),
            (
                "home end pageup pagedown delete",
                b"\x1b[H\x1b[F\x1b[5~\x1b[6~\x1b[3~",
                "editing keys",
            ),
            ("cmd-w", b"", "Cmd chords are the app's"),
            (
                "cmd-+ cmd-= cmd-- cmd-0",
                b"",
                "the font zoom chords (⌘+ ⌘= ⌘− ⌘0) are the app's, never the program's",
            ),
        ];
        for (keys, want, what) in cases {
            let got = self.keys_sent(keys);
            self.expect(
                &format!("key {keys:?}: {what}"),
                got == *want,
                format!("sent {got:?}, want {want:?}"),
            );
        }

        // Application cursor keys and the kitty protocol, as the program
        // enables them.
        self.feed(b"\x1b[?1h");
        let got = self.keys_sent("up");
        self.expect(
            "Up under DECCKM is ESC O A",
            got == b"\x1bOA",
            format!("{got:?}"),
        );
        self.feed(b"\x1b[?1l\x1b[>5u");
        let got = self.keys_sent("shift-enter");
        self.expect(
            "Shift+Enter under the kitty protocol is CSI 13;2u",
            got == b"\x1b[13;2u",
            format!("{got:?}"),
        );
        let got = self.keys_sent("tab");
        self.expect(
            "Tab under the kitty protocol is CSI 9u",
            got == b"\x1b[9u",
            format!("{got:?}"),
        );
        let got = self.keys_sent("enter");
        self.expect(
            "plain Enter under the kitty protocol stays CR",
            got == b"\r",
            format!("{got:?}"),
        );
        self.feed(b"\x1b[<u");

        // Unicode text through the input handler.
        self.io.take();
        self.cx.simulate_input(self.window, "é漢👍");
        let got = self.io.input();
        self.expect(
            "non-ASCII text is sent as UTF-8",
            got == "é漢👍".as_bytes(),
            format!("{got:?}"),
        );

        // Paste: plain, then bracketed (the session brackets it; the pane says
        // whether the program asked).
        self.cx
            .write_to_clipboard(ClipboardItem::new_string("pasted\ntext".into()));
        self.io.take();
        self.cx.simulate_keystrokes(self.window, "cmd-v");
        let sent = self.io.take();
        self.expect(
            "Cmd+V pastes the clipboard as a Paste request",
            sent == vec![Sent::Paste("pasted\ntext".into(), false)],
            format!("{sent:?}"),
        );
        self.feed(b"\x1b[?2004h");
        self.cx.simulate_keystrokes(self.window, "cmd-v");
        let sent = self.io.take();
        self.expect(
            "Cmd+V under bracketed paste is marked bracketed",
            sent == vec![Sent::Paste("pasted\ntext".into(), true)],
            format!("{sent:?}"),
        );
        self.feed(b"\x1b[?2004l");
    }

    fn ime(&mut self) {
        use gpui::EntityInputHandler;
        self.reset_screen();
        self.feed(b"$ ");
        self.io.take();
        // A Japanese composition, its first clause selected by the IME.
        self.with_pane(|pane, window, cx| {
            pane.replace_and_mark_text_in_range(None, "にほんご", Some(0..2), window, cx);
        })
        .unwrap();
        self.save("ime-composing.png");
        let during = self.io.take();
        self.expect(
            "composition sends nothing until committed",
            during.is_empty(),
            format!("{during:?}"),
        );
        let (marked, read_back, candidates, cursor_x) = self
            .with_pane(|pane, window, cx| {
                let marked = pane.marked_text_range(window, cx);
                let mut adjusted = None;
                let read_back = pane.text_for_range(0..2, &mut adjusted, window, cx);
                let candidates = pane.candidate_bounds(2..4, window, cx);
                let m = pane.metrics.clone().unwrap();
                (marked, read_back, candidates, m.width * 2.0)
            })
            .unwrap();
        // "にほ" is four cells wide, so the second clause starts 4 cells past
        // the cursor (at column 2).
        let (w, _) = self.cell();
        self.expect(
            "the input method reads the composition back and places candidates under its clause",
            marked == Some(0..4)
                && read_back.as_deref() == Some("にほ")
                && candidates.is_some_and(|b| (f32::from(b.origin.x - cursor_x) - 4.0 * w).abs() < 0.5),
            format!("marked {marked:?}, read {read_back:?}, candidates {candidates:?}"),
        );
        self.with_pane(|pane, window, cx| pane.replace_text_in_range(None, "日本語", window, cx))
            .unwrap();
        let got = self.io.input();
        self.expect(
            "committing a composition sends its text",
            got == "日本語".as_bytes(),
            format!("{got:?}"),
        );

        // Cancel: the input method unmarks; nothing is sent.
        self.with_pane(|pane, window, cx| {
            pane.replace_and_mark_text_in_range(None, "か", None, window, cx);
            pane.unmark_text(window, cx);
        })
        .unwrap();
        let sent = self.io.take();
        let composing = self
            .with_pane(|pane, window, cx| pane.marked_text_range(window, cx))
            .unwrap();
        self.expect(
            "a cancelled composition sends nothing and clears",
            sent.is_empty() && composing.is_none(),
            format!("{sent:?} {composing:?}"),
        );

        // A dead key: Option+E marks an accent, the next E commits é.
        self.with_pane(|pane, window, cx| {
            pane.replace_and_mark_text_in_range(None, "´", Some(1..1), window, cx);
        })
        .unwrap();
        self.save("dead-key.png");
        let during = self.io.take();
        self.with_pane(|pane, window, cx| pane.replace_text_in_range(None, "é", window, cx))
            .unwrap();
        let got = self.io.input();
        self.expect(
            "a dead key sequence sends the composed character",
            during.is_empty() && got == "é".as_bytes(),
            format!("during {during:?}, sent {got:?}"),
        );
    }

    fn wheel(&mut self, at: Point<Pixels>, lines: f32, modifiers: Modifiers) {
        // The platform reports the pointer's position before a wheel event;
        // hover (and so the wheel's target) follows it.
        self.cx
            .simulate_mouse_move(self.window, at, None, modifiers);
        self.cx.simulate_event(
            self.window,
            ScrollWheelEvent {
                position: at,
                delta: ScrollDelta::Lines(point(0.0, lines)),
                modifiers,
                touch_phase: TouchPhase::Moved,
            },
        );
    }

    fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>, modifiers: Modifiers) {
        self.cx
            .simulate_mouse_down(self.window, from, MouseButton::Left, modifiers);
        for i in 1..=4 {
            let t = i as f32 / 4.0;
            let p = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
            self.cx
                .simulate_mouse_move(self.window, p, MouseButton::Left, modifiers);
        }
        self.cx
            .simulate_mouse_up(self.window, to, MouseButton::Left, modifiers);
        self.draw();
    }

    fn scroll_offset(&mut self) -> usize {
        self.with_pane(|pane, _, _| pane.terminal_view().scroll_offset())
            .unwrap()
    }

    fn mouse(&mut self) {
        self.reset_screen();
        let mut history = String::new();
        for i in 0..200 {
            history.push_str(&format!("history line {i}\r\n"));
        }
        history.push_str("select me please");
        self.feed(history.as_bytes());
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let middle = self.at(10, 10);

        // No program mouse mode: the wheel is Shelbi's scrollback.
        self.io.take();
        self.wheel(middle, 3.0, Modifiers::default());
        let offset = self.scroll_offset();
        self.expect(
            "wheel scrolls scrollback when no program reports the mouse",
            offset == 3,
            format!("offset {offset}"),
        );
        self.save("scrolled.png");
        self.wheel(middle, -3.0, Modifiers::default());
        let offset = self.scroll_offset();
        self.expect(
            "wheel down returns to the live bottom",
            offset == 0,
            format!("offset {offset}"),
        );
        let sent = self.io.take();
        self.expect(
            "a Shelbi-owned wheel sends nothing",
            sent.is_empty(),
            format!("{sent:?}"),
        );

        // A drag selects and copies (the TUI's rule), and paints a highlight.
        let (_, h) = self.cell();
        let (w, _) = self.cell();
        let rows = (600.0 / h).floor() as u16;
        let last = rows - 1;
        let from = point(px(0.5 * w), px((last as f32 + 0.5) * h));
        let to = point(px(8.5 * w), px((last as f32 + 0.5) * h));
        self.drag(from, to, Modifiers::default());
        self.save("selection.png");
        let clip = self.cx.read_from_clipboard().and_then(|c| c.text());
        self.expect(
            "a drag selects and copies the text",
            clip.as_deref() == Some("select me"),
            format!("clipboard {clip:?}"),
        );

        // The program asks for the mouse (click reporting, SGR).
        self.feed(b"\x1b[?1000h\x1b[?1006h");
        self.io.take();
        let cell = self.at(4, 2);
        self.cx
            .simulate_click(self.window, cell, Modifiers::default());
        let got = self.io.input();
        self.expect(
            "a click reaches a reporting program as an SGR press and release",
            got == b"\x1b[<0;5;3M\x1b[<0;5;3m",
            format!("{:?}", String::from_utf8_lossy(&got)),
        );
        self.wheel(cell, 1.0, Modifiers::default());
        let got = self.io.input();
        self.expect(
            "the wheel reaches a reporting program",
            got == b"\x1b[<64;5;3M",
            format!("{:?}", String::from_utf8_lossy(&got)),
        );
        self.wheel(cell, 2.0, shift);
        let got = self.io.input();
        let offset = self.scroll_offset();
        self.expect(
            "Shift+wheel is Shelbi's scrollback even when the program reports",
            got.is_empty() && offset == 2,
            format!("sent {got:?}, offset {offset}"),
        );
        self.wheel(cell, -2.0, shift);
        self.drag(from, to, shift);
        let got = self.io.input();
        let clip = self.cx.read_from_clipboard().and_then(|c| c.text());
        self.expect(
            "Shift+drag selects even when the program reports",
            got.is_empty() && clip.as_deref() == Some("select me"),
            format!("sent {got:?}, clipboard {clip:?}"),
        );
        self.feed(b"\x1b[?1000l\x1b[?1006l");
    }

    fn fonts(&mut self) {
        // The bundled Source Code Pro is the default, with no system install.
        let family = self
            .with_pane(|pane, _, _| pane.metrics.as_ref().map(|m| m.font.family.to_string()))
            .unwrap();
        self.expect(
            "the terminal font is the bundled Source Code Pro",
            family.as_deref() == Some("Source Code Pro"),
            format!("{family:?}"),
        );

        // Every glyph of the fallback showcase comes from some font: no
        // `.notdef` (glyph 0, the missing-glyph box).
        self.reset_screen();
        let sample = "Source Code Pro → 漢字かな한국어 👍🎉 ✓ │ ⏺ ✻ ⎿ \u{e0a0} \u{e0b0} \u{f121} \u{f07b} \u{e7a8}";
        self.feed(format!("{sample}\r\n").as_bytes());
        self.save("fallback.png");
        let missing = self
            .handle
            .update(&mut self.cx, |_, window, _| {
                use crate::elements::terminal::metrics::{terminal_font, SYMBOLS_FAMILY};
                let text_system = window.text_system().clone();
                let glyphs = |ch: char, font: gpui::Font| -> Vec<u32> {
                    let text: gpui::SharedString = ch.to_string().into();
                    let run = gpui::TextRun {
                        len: text.len(),
                        font,
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let line = text_system.shape_line(text, px(14.0), &[run], None);
                    line.runs.iter().flat_map(|r| r.glyphs.iter()).map(|g| g.id.0).collect()
                };
                let mut missing = Vec::new();
                let mut private_use = Vec::new();
                for ch in sample.chars().filter(|c| *c != ' ') {
                    let got = glyphs(ch, terminal_font("Source Code Pro"));
                    // A glyph no font has comes back as `.notdef` (glyph 0).
                    if got.is_empty() || got.contains(&0) {
                        missing.push(format!("{ch}: no glyph"));
                    }
                    if ('\u{e000}'..='\u{f8ff}').contains(&ch) {
                        private_use.push((ch, got));
                    }
                }
                // LastResort draws every character of a block with the same
                // box glyph; the bundled symbols font has one per icon. So
                // distinct icons sharing a glyph are boxes.
                println!("      private-use glyphs: {private_use:?}");
                let mut ids: Vec<&Vec<u32>> = private_use.iter().map(|(_, g)| g).collect();
                ids.sort();
                ids.dedup();
                if ids.len() < private_use.len() {
                    missing.push(format!("Nerd Font icons drawn as LastResort boxes ({SYMBOLS_FAMILY} not in the cascade)"));
                }
                missing
            })
            .unwrap();
        self.expect(
            "CJK, emoji, symbols, and Nerd Font icons all find a font (no boxes)",
            missing.is_empty(),
            format!("missing glyphs for {missing:?}"),
        );

        // A font change applies live: the cell is re-measured and the new
        // grid size reported.
        let (w0, h0) = self.cell();
        self.io.take();
        self.cx.update(|cx| {
            TerminalSettings::set(
                TerminalSettings {
                    font_size: 18.0,
                    ..TerminalSettings::default()
                },
                cx,
            )
        });
        self.draw();
        self.cx.advance_clock(Duration::from_millis(100));
        self.draw();
        let (w1, h1) = self.cell();
        let sent = self.io.take();
        let want = Size::from_pixels(1000.0, 600.0, w1, h1);
        self.expect(
            "a font size change re-measures the cell and reports the new grid",
            w1 > w0 && h1 > h0 && sent.contains(&Sent::Resize(want)),
            format!("cell {w0}x{h0} -> {w1}x{h1}, sent {sent:?}"),
        );
        self.save("font-18.png");
        self.cx.update(|cx| TerminalSettings::set(TerminalSettings::default(), cx));
        self.draw();
        self.cx.advance_clock(Duration::from_millis(100));
        self.draw();
        let (w, h) = self.cell();
        let back = Size::from_pixels(1000.0, 600.0, w, h);
        self.with_pane(|pane, _, cx| pane.session_resized(back, cx))
            .unwrap();
        self.io.take();
    }

    fn option_key(&mut self) {
        self.reset_screen();
        // The default: Option types characters, so Option+E reaches the input
        // handler (a dead key on a real keyboard), nothing is encoded.
        let got = self.keys_sent("alt-b");
        self.expect(
            "with Option typing characters, Option+B sends no Meta sequence",
            !got.starts_with(b"\x1b"),
            format!("{got:?}"),
        );
        self.cx.update(|cx| {
            TerminalSettings::set(
                TerminalSettings {
                    option_as_meta: shelbi_state::OptionAsMeta::Both,
                    ..TerminalSettings::default()
                },
                cx,
            )
        });
        self.draw();
        let got = self.keys_sent("alt-b");
        self.expect(
            "with Option as Meta (a live setting change), Option+B is ESC b",
            got == b"\x1bb",
            format!("{got:?}"),
        );
        self.cx.update(|cx| TerminalSettings::set(TerminalSettings::default(), cx));
        self.draw();
    }

    fn press(&mut self, at: Point<Pixels>, clicks: usize, modifiers: Modifiers) {
        self.cx.simulate_mouse_move(self.window, at, None, modifiers);
        self.cx.simulate_event(
            self.window,
            MouseDownEvent {
                button: MouseButton::Left,
                position: at,
                modifiers,
                click_count: clicks,
                first_mouse: false,
            },
        );
        self.cx.simulate_event(
            self.window,
            MouseUpEvent {
                button: MouseButton::Left,
                position: at,
                modifiers,
                click_count: clicks,
            },
        );
        self.draw();
    }

    fn modifiers(&mut self, modifiers: Modifiers) {
        self.cx.simulate_event(
            self.window,
            gpui::ModifiersChangedEvent {
                modifiers,
                ..Default::default()
            },
        );
    }

    fn clipboard(&mut self) -> Option<String> {
        self.cx.read_from_clipboard().and_then(|c| c.text())
    }

    fn mouse_modes(&mut self) {
        self.reset_screen();
        self.feed(b"0123456789\r\n0123456789");
        let from = self.at(1, 0);
        let to = self.at(4, 0);
        // Click reporting only (1000): a press and release, no motion.
        self.feed(b"\x1b[?1000h\x1b[?1006h");
        self.io.take();
        self.drag(from, to, Modifiers {
            alt: true,
            ..Default::default()
        });
        let got = String::from_utf8_lossy(&self.io.input()).to_string();
        self.expect(
            "under click-only reporting an Alt-drag forwards press and release, no motion",
            got == "\x1b[<8;2;1M\x1b[<8;5;1m",
            format!("{got:?}"),
        );
        // Button-event tracking (1002): the drag's motion too, once per cell.
        self.feed(b"\x1b[?1002h");
        self.drag(from, to, Modifiers {
            alt: true,
            ..Default::default()
        });
        let got = String::from_utf8_lossy(&self.io.input()).to_string();
        self.expect(
            "under button-event tracking the drag's motion is forwarded, once per cell",
            got.starts_with("\x1b[<8;2;1M") && got.contains("\x1b[<40;5;1M") && got.ends_with("\x1b[<8;5;1m"),
            format!("{got:?}"),
        );
        // Any-event tracking (1003): plain motion with no button.
        self.feed(b"\x1b[?1003h");
        self.io.take();
        let at = self.at(6, 1);
        self.cx
            .simulate_mouse_move(self.window, at, None, Modifiers::default());
        let got = String::from_utf8_lossy(&self.io.input()).to_string();
        self.expect(
            "under any-event tracking plain motion is forwarded (button 35)",
            got == "\x1b[<35;7;2M",
            format!("{got:?}"),
        );
        // Legacy X10 encoding when the program didn't ask for SGR. (Each of
        // 1000/1002/1003 replaces the others, as in xterm.)
        self.feed(b"\x1b[?1006l\x1b[?1003l\x1b[?1000h");
        self.io.take();
        let cell = self.at(2, 1);
        self.cx.simulate_click(self.window, cell, Modifiers::default());
        let got = self.io.input();
        self.expect(
            "without SGR a click is X10-encoded",
            got == [0x1b, b'[', b'M', 32, 35, 34, 0x1b, b'[', b'M', 35, 35, 34],
            format!("{got:?}"),
        );
        self.feed(b"\x1b[?1000l");
    }

    fn selection_modes(&mut self) {
        self.reset_screen();
        self.feed(b"cargo test src/main.rs:42 ok\r\nsecond line here\r\nabcdefgh\r\nijklmnop\r\nqrstuvwx");
        // Double-click: a word (`:` ends it).
        let at = self.at(14, 0);
        self.press(at, 1, Modifiers::default());
        let at = self.at(14, 0);
        self.press(at, 2, Modifiers::default());
        let clip = self.clipboard();
        self.expect(
            "a double-click selects and copies the word",
            clip.as_deref() == Some("src/main.rs"),
            format!("{clip:?}"),
        );
        self.save("select-word.png");
        // Triple-click: the line.
        let at = self.at(3, 1);
        self.press(at, 3, Modifiers::default());
        let clip = self.clipboard();
        self.expect(
            "a triple-click selects and copies the line",
            clip.as_deref() == Some("second line here"),
            format!("{clip:?}"),
        );
        // Alt-drag: a block.
        let alt = Modifiers {
            alt: true,
            ..Default::default()
        };
        let (from, to) = (self.at(2, 2), self.at(4, 4));
        self.drag(from, to, alt);
        self.save("select-block.png");
        let clip = self.clipboard();
        self.expect(
            "an Alt-drag selects and copies a block",
            clip.as_deref() == Some("cde\nklm\nstu"),
            format!("{clip:?}"),
        );
        // Cmd+C copies the selection again.
        self.cx
            .write_to_clipboard(ClipboardItem::new_string("other".into()));
        self.cx.simulate_keystrokes(self.window, "cmd-c");
        let clip = self.clipboard();
        self.expect(
            "Cmd+C copies the selection",
            clip.as_deref() == Some("cde\nklm\nstu"),
            format!("{clip:?}"),
        );
    }

    fn selection_autoscroll(&mut self) {
        self.reset_screen();
        let mut history = String::new();
        for i in 0..100 {
            history.push_str(&format!("auto line {i}\r\n"));
        }
        self.feed(history.as_bytes());
        let from = self.at(2, 3);
        let above = point(from.x, px(-20.0));
        self.cx
            .simulate_mouse_down(self.window, from, MouseButton::Left, Modifiers::default());
        let inside = self.at(2, 1);
        self.cx
            .simulate_mouse_move(self.window, inside, MouseButton::Left, Modifiers::default());
        self.cx
            .simulate_mouse_move(self.window, above, MouseButton::Left, Modifiers::default());
        self.cx.advance_clock(Duration::from_millis(520));
        self.draw();
        let offset = self.scroll_offset();
        self.cx
            .simulate_mouse_up(self.window, above, MouseButton::Left, Modifiers::default());
        self.draw();
        let (offset_after, selected) = self
            .with_pane(|pane, _, _| {
                let v = pane.terminal_view();
                (v.scroll_offset(), v.selection_text().unwrap_or_default())
            })
            .unwrap();
        let lines = selected.lines().count();
        self.expect(
            "holding a selection drag above the grid scrolls back and extends the selection",
            offset >= 5 && offset_after == offset && lines > 5,
            format!("offset {offset} -> {offset_after}, {lines} lines selected"),
        );
    }

    fn selection_survives_output(&mut self) {
        self.reset_screen();
        self.feed(b"keep this text\r\n");
        let (from, to) = (self.at(0, 0), self.at(3, 0));
        self.drag(from, to, Modifiers::default());
        let mut more = String::new();
        for i in 0..80 {
            more.push_str(&format!("more output {i}\r\n"));
        }
        self.feed(more.as_bytes());
        let text = self
            .with_pane(|pane, _, _| pane.terminal_view().selection_text())
            .unwrap();
        self.expect(
            "a selection survives output that scrolls it into history",
            text.as_deref() == Some("keep"),
            format!("{text:?}"),
        );
        self.feed(b"\x1b[H\x1b[2J\x1b[3J");
        let gone = self
            .with_pane(|pane, _, _| pane.terminal_view().selection().is_none())
            .unwrap();
        self.expect(
            "the selection goes when the text under it is erased",
            gone,
            "still selected",
        );
    }

    fn scrolling(&mut self) {
        self.reset_screen();
        let mut history = String::new();
        for i in 0..300 {
            history.push_str(&format!("scroll line {i}\r\n"));
        }
        self.feed(history.as_bytes());
        let middle = self.at(10, 10);
        self.wheel(middle, 20.0, Modifiers::default());
        let offset = self.scroll_offset();
        let top_before = self.row_text_on_screen(0);
        self.feed(b"new output 1\r\nnew output 2\r\nnew output 3\r\n");
        let top_after = self.row_text_on_screen(0);
        let offset_after = self.scroll_offset();
        self.expect(
            "new output while scrolled up doesn't move the view",
            top_before == top_after && offset_after == offset + 3,
            format!("top {top_before:?} -> {top_after:?}, offset {offset} -> {offset_after}"),
        );
        self.save("scrolled-jump.png");

        // The scrollbar shows the position; dragging its thumb to the top
        // reaches the oldest line.
        let thumb = self
            .with_pane(|pane, _, _| pane.scrollbar_thumb_bounds())
            .unwrap();
        self.expect("the scrollbar shows with history", thumb.is_some(), "no thumb");
        if let Some(thumb) = thumb {
            let grab = thumb.center();
            let top = point(grab.x, px(-50.0));
            self.cx
                .simulate_mouse_down(self.window, grab, MouseButton::Left, Modifiers::default());
            self.cx
                .simulate_mouse_move(self.window, top, MouseButton::Left, Modifiers::default());
            self.cx
                .simulate_mouse_up(self.window, top, MouseButton::Left, Modifiers::default());
            self.draw();
            let (offset, history) = self
                .with_pane(|pane, _, _| {
                    let v = pane.terminal_view();
                    (v.scroll_offset(), v.emulator().history_len())
                })
                .unwrap();
            self.expect(
                "dragging the scrollbar thumb to the top scrolls to the oldest line",
                offset == history,
                format!("offset {offset} of {history}"),
            );
            // A click low on the track jumps most of the way back down.
            let bounds = self
                .with_pane(|pane, _, _| pane.element_bounds())
                .unwrap()
                .unwrap();
            let low = point(thumb.center().x, bounds.bottom() - px(5.0));
            self.cx
                .simulate_click(self.window, low, Modifiers::default());
            let offset = self.scroll_offset();
            self.expect(
                "a click on the track jumps toward it",
                offset < 20,
                format!("offset {offset}"),
            );
        }

        // "Jump to bottom".
        self.wheel(middle, 15.0, Modifiers::default());
        self.save("jump-to-bottom.png");
        let bounds = self
            .with_pane(|pane, _, _| pane.element_bounds())
            .unwrap()
            .unwrap();
        let button = point(bounds.right() - px(40.0), bounds.bottom() - px(24.0));
        self.cx
            .simulate_click(self.window, button, Modifiers::default());
        let offset = self.scroll_offset();
        self.expect(
            "the Jump to bottom button returns to the live bottom",
            offset == 0,
            format!("offset {offset}"),
        );
        // Typing also returns to the bottom.
        self.wheel(middle, 5.0, Modifiers::default());
        self.keys_sent("x");
        let offset = self.scroll_offset();
        self.expect("typing returns to the live bottom", offset == 0, format!("offset {offset}"));
    }

    fn row_text_on_screen(&mut self, row: u16) -> String {
        self.with_pane(|pane, _, _| {
            let v = pane.terminal_view();
            v.screen_text().rows.get(row as usize).cloned().unwrap_or_default()
        })
        .unwrap()
    }

    fn find_bar(&mut self) {
        self.reset_screen();
        let mut text = String::new();
        for i in 0..120 {
            if i % 30 == 5 {
                text.push_str(&format!("line {i} has a Needle in it\r\n"));
            } else {
                text.push_str(&format!("line {i}\r\n"));
            }
        }
        text.push_str("the last needle");
        self.feed(text.as_bytes());
        self.io.take();
        self.cx.simulate_keystrokes(self.window, "cmd-f");
        self.cx.simulate_input(self.window, "needle");
        self.draw();
        let state = self.with_pane(|pane, _, _| pane.search_state()).unwrap();
        let sent = self.io.take();
        self.expect(
            "Cmd+F opens the find bar; typing searches (smart case) and reaches only the bar",
            state == Some(("needle".into(), "5 of 5".into())) && sent.is_empty(),
            format!("{state:?}, sent {sent:?}"),
        );
        self.save("find-bar.png");
        self.cx.simulate_keystrokes(self.window, "enter enter");
        let state = self.with_pane(|pane, _, _| pane.search_state()).unwrap();
        let offset = self.scroll_offset();
        self.expect(
            "Enter steps to older matches and scrolls them into view",
            state.as_ref().map(|s| s.1.as_str()) == Some("3 of 5") && offset > 0,
            format!("{state:?}, offset {offset}"),
        );
        self.cx.simulate_keystrokes(self.window, "shift-enter");
        let state = self.with_pane(|pane, _, _| pane.search_state()).unwrap();
        self.expect(
            "Shift+Enter steps to the newer match",
            state.as_ref().map(|s| s.1.as_str()) == Some("4 of 5"),
            format!("{state:?}"),
        );
        self.cx.simulate_keystrokes(self.window, "cmd-g");
        let state = self.with_pane(|pane, _, _| pane.search_state()).unwrap();
        self.expect(
            "Cmd+G steps older too",
            state.as_ref().map(|s| s.1.as_str()) == Some("3 of 5"),
            format!("{state:?}"),
        );
        self.cx.simulate_input(self.window, "X");
        let state = self.with_pane(|pane, _, _| pane.search_state()).unwrap();
        self.expect(
            "an uppercase letter makes the search case-sensitive",
            state.as_ref().map(|s| s.1.as_str()) == Some("No matches"),
            format!("{state:?}"),
        );
        self.cx.simulate_keystrokes(self.window, "backspace");
        self.save("find-bar-stepped.png");
        self.cx.simulate_keystrokes(self.window, "escape");
        let (state, matches) = self
            .with_pane(|pane, _, _| (pane.search_state(), pane.terminal_view().search_matches().len()))
            .unwrap();
        let sent = self.io.take();
        self.expect(
            "Escape closes the bar and clears the highlights, sending nothing",
            state.is_none() && matches == 0 && sent.is_empty(),
            format!("{state:?}, {matches} matches, sent {sent:?}"),
        );
        let got = self.keys_sent("a");
        self.expect("after closing, keys reach the session again", got == b"a", format!("{got:?}"));
    }

    fn links(&mut self) {
        self.reset_screen();
        self.feed(b"see https://example.com/docs and src/main.rs:12:3 here");
        let sink = Rc::new(RefCell::new(Vec::new()));
        let pane_sink = sink.clone();
        self.with_pane(|pane, _, _| {
            pane.set_link_sink(pane_sink);
            pane.set_cwd(PathBuf::from("/tmp"));
        })
        .unwrap();
        let cmd = Modifiers {
            platform: true,
            ..Default::default()
        };
        let url_at = self.at(8, 0);
        self.cx
            .simulate_mouse_move(self.window, url_at, None, Modifiers::default());
        let hovered = self
            .with_pane(|pane, _, _| pane.hovered_link().cloned())
            .unwrap();
        self.expect("without Cmd nothing is underlined", hovered.is_none(), format!("{hovered:?}"));
        self.modifiers(cmd);
        self.draw();
        let hovered = self
            .with_pane(|pane, _, _| pane.hovered_link().map(|l| l.target.clone()))
            .unwrap();
        self.expect(
            "holding Cmd over a URL underlines it",
            hovered == Some(LinkTarget::Url("https://example.com/docs".into())),
            format!("{hovered:?}"),
        );
        self.save("link-hover.png");
        self.cx.simulate_click(self.window, url_at, cmd);
        let path_at = self.at(36, 0);
        self.cx.simulate_click(self.window, path_at, cmd);
        let opened = sink.borrow().clone();
        self.expect(
            "Cmd+click opens the URL, and the path at its line and column",
            opened
                == vec![
                    LinkTarget::Url("https://example.com/docs".into()),
                    LinkTarget::Path {
                        path: "src/main.rs".into(),
                        line: Some(12),
                        column: Some(3),
                    },
                ],
            format!("{opened:?}"),
        );
        let sent = self.io.take();
        self.expect(
            "a Cmd+click doesn't reach the session or start a selection",
            sent.is_empty(),
            format!("{sent:?}"),
        );
        self.modifiers(Modifiers::default());
        self.draw();
        let hovered = self
            .with_pane(|pane, _, _| pane.hovered_link().cloned())
            .unwrap();
        self.expect("releasing Cmd removes the underline", hovered.is_none(), format!("{hovered:?}"));
    }

    fn alt_screen(&mut self) {
        self.reset_screen();
        // `less`: the alternate screen, application cursor keys.
        self.feed(b"\x1b[?1049h\x1b[?1h\x1b[Hpager line one\r\npager line two");
        self.io.take();
        let middle = self.at(5, 5);
        self.wheel(middle, 2.0, Modifiers::default());
        let got = self.io.input();
        self.expect(
            "on the alternate screen the wheel sends arrow keys (alternate scroll)",
            got == b"\x1bOA\x1bOA",
            format!("{got:?}"),
        );
        let (from, to) = (self.at(0, 1), self.at(9, 1));
        self.drag(from, to, Modifiers::default());
        let clip = self.clipboard();
        self.expect(
            "selection works on the alternate screen (copying out of a pager)",
            clip.as_deref() == Some("pager line"),
            format!("{clip:?}"),
        );
        self.feed(b"\x1b[?1l\x1b[?1049l");
    }

    /// dt-terminal-bottom-clipped: a session sized by a taller client (twice
    /// the pane's rows) shows the bottom of its content, with the cursor,
    /// a selection, and a search match on the rows they belong to.
    fn taller_session(&mut self) {
        let (w, h) = self.cell();
        let viewer = Size::from_pixels(1000.0, 600.0, w, h);
        let tall = Size::new(viewer.cols, viewer.rows * 2);
        self.with_pane(|pane, _, cx| pane.session_resized(tall, cx))
            .unwrap();
        self.reset_screen();
        let mut s = String::new();
        for i in 0..tall.rows - 3 {
            s.push_str(&format!("line {i}\r\n"));
        }
        s.push_str("needle in the output\r\n> prompt here\r\n\x1b[44m-- status line --\x1b[K\x1b[0m");
        s.push_str(&format!("\x1b[{};15H", tall.rows - 1));
        self.feed(s.as_bytes());
        let rows = self.with_pane(|pane, _, _| pane.shown_rows()).unwrap();
        self.expect(
            "a taller session shows its bottom rows, not its top",
            rows.len() == viewer.rows as usize
                && rows[rows.len() - 3..] == ["needle in the output", "> prompt here", "-- status line --"],
            format!("shown {:?}", &rows[rows.len().saturating_sub(3)..]),
        );
        // Select "prompt" on the pane's second-to-last row: the copy is the
        // text under the pointer.
        let row = viewer.rows - 2;
        let (from, to) = (self.at(2, row), self.at(7, row));
        self.drag(from, to, Modifiers::default());
        let clip = self.clipboard();
        self.expect(
            "a drag on a clipped window selects the row under the pointer",
            clip.as_deref() == Some("prompt"),
            format!("copied {clip:?}"),
        );
        self.with_pane(|pane, _, _| {
            pane.terminal_view_mut().start_search("needle", false);
        })
        .unwrap();
        self.save("taller-session.png");
        self.with_pane(|pane, _, _| pane.terminal_view_mut().clear_search())
            .unwrap();
        self.with_pane(|pane, _, cx| pane.session_resized(viewer, cx))
            .unwrap();
        self.reset_screen();
    }

    fn resize(&mut self) {
        self.io.take();
        self.handle
            .update(&mut self.cx, |h, _, cx| {
                h.size = Some(size(px(800.0), px(400.0)));
                cx.notify();
            })
            .unwrap();
        self.draw();
        let viewer = self
            .with_pane(|pane, _, _| pane.geometry.as_ref().map(|g| g.viewer))
            .unwrap();
        println!("      after the resize the pane's viewer is {viewer:?}");
        let early = self.io.take();
        self.cx.advance_clock(Duration::from_millis(100));
        self.draw();
        let late = self.io.take();
        let (w, h) = self.cell();
        let want = Size::from_pixels(800.0, 400.0, w, h);
        self.expect(
            "a window resize reports the new grid once, after the debounce",
            early.is_empty() && late == vec![Sent::Resize(want)],
            format!("before {early:?}, after {late:?}, want {want:?}"),
        );
        // Until the session echoes the new size the old grid is clipped; then
        // it fills the pane.
        self.save("resize-before-echo.png");
        self.with_pane(|pane, _, cx| pane.session_resized(want, cx))
            .unwrap();
        self.save("resize-after-echo.png");
    }

    fn timing(&mut self) {
        // An idle redraw: nothing changed, so nothing is rebuilt.
        self.draw();
        let idle: Vec<Duration> = (0..20).map(|_| self.draw()).collect();
        let rebuilt = self
            .with_pane(|pane, _, _| pane.stats_last_rebuilt())
            .unwrap();
        println!(
            "      idle redraw: {} (rows rebuilt on the last: {rebuilt})",
            summarize(&idle)
        );
        self.expect(
            "an idle redraw rebuilds no rows",
            rebuilt == 0,
            format!("{rebuilt} rows"),
        );

        // A `yes` flood: 1,000,000 lines in 64 KiB chunks, a frame per chunk.
        let flood: Vec<u8> = b"y\r\n".repeat(1_000_000);
        let mut feeds = Vec::new();
        let mut frames = Vec::new();
        let started = Instant::now();
        for chunk in flood.chunks(64 * 1024) {
            let t = Instant::now();
            self.with_pane(|pane, _, cx| pane.feed(chunk, cx)).unwrap();
            feeds.push(t.elapsed());
            frames.push(self.draw());
        }
        let total = started.elapsed();
        println!(
            "      yes flood: {} bytes in {} chunks, {:?} total ({:.0} MiB/s)",
            flood.len(),
            frames.len(),
            total,
            flood.len() as f64 / 1024.0 / 1024.0 / total.as_secs_f64()
        );
        println!("      feed per 64 KiB chunk: {}", summarize(&feeds));
        println!(
            "      frame (layout+prepaint+paint) under the flood: {}",
            summarize(&frames)
        );
        self.save("flood.png");
        let gpu = {
            let t = Instant::now();
            let _ = self.cx.capture_screenshot(self.window);
            t.elapsed()
        };
        println!("      one Metal render + readback: {gpu:?}");
    }
}

fn summarize(samples: &[Duration]) -> String {
    let mut v: Vec<Duration> = samples.to_vec();
    v.sort();
    let avg = v.iter().sum::<Duration>() / v.len().max(1) as u32;
    let p50 = v[v.len() / 2];
    let p95 = v[(v.len() * 95 / 100).min(v.len() - 1)];
    let max = *v.last().unwrap();
    format!(
        "avg {avg:?}, p50 {p50:?}, p95 {p95:?}, max {max:?} (n={})",
        v.len()
    )
}

/// Parse `--visual-check <dir>` (and the other `--visual-*` modes' dir).
///
/// Every visual run renders in both themes: with `SHELBI_VISUAL_THEME`
/// unset, this runs the same command twice as child processes, once per
/// theme (`dark`, then `light`), and exits with the combined result; each
/// child renders into `<dir>/<theme>/` and paints with [`theme`]. Set the
/// variable to render one theme only.
pub fn out_dir(arg: Option<String>) -> PathBuf {
    let (dir, theme) = per_theme(arg);
    dir.join(theme)
}

/// [`out_dir`]'s re-run, for a mode that names its frames by theme itself
/// (`--visual-figma`'s `<screen>-<theme>.png`): the directory as given and
/// the theme this run paints.
pub fn per_theme(arg: Option<String>) -> (PathBuf, &'static str) {
    let dir = arg
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("visual-check").to_path_buf());
    if let Some(theme) = theme_name() {
        return (dir, theme);
    }
    let mut failed = false;
    for theme in ["dark", "light"] {
        println!("==== {theme} theme ====");
        let status = std::process::Command::new(std::env::current_exe().expect("own path"))
            .args(std::env::args_os().skip(1))
            .env(THEME_VAR, theme)
            .status()
            .expect("run the themed visual check");
        failed |= !status.success();
    }
    std::process::exit(if failed { 1 } else { 0 })
}

const THEME_VAR: &str = "SHELBI_VISUAL_THEME";

fn theme_name() -> Option<&'static str> {
    match std::env::var(THEME_VAR).ok()?.as_str() {
        "light" => Some("light"),
        _ => Some("dark"),
    }
}

/// Install the run's theme with gpui-kit's widgets in step, as the app does
/// (the views' harnesses; the terminal check needs only [`theme`]).
pub fn install_theme(cx: &mut gpui::App) {
    gpui_kit::init(cx);
    crate::theme::install(theme(), cx);
}

/// The theme a visual run paints with: `SHELBI_VISUAL_THEME` (dark unless
/// `light`), at `SHELBI_VISUAL_CONTRAST` percent contrast (0 unless set),
/// its surfaces at `SHELBI_VISUAL_OPACITY` percent (opaque unless set: a
/// translucent window's frames, for QA).
pub fn theme() -> Theme {
    let contrast = std::env::var("SHELBI_VISUAL_CONTRAST")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map_or(0, crate::theme::contrast::clamped);
    let theme = if theme_name() == Some("light") {
        Theme::resolve_with(ThemePreference::Light, contrast, gpui::WindowAppearance::Light)
    } else {
        Theme::resolve_with(ThemePreference::Dark, contrast, gpui::WindowAppearance::Dark)
    };
    let opacity = std::env::var("SHELBI_VISUAL_OPACITY")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map(|v| crate::theme::window::Translucency::clamped(v, 0).alpha());
    theme.with_opacity(opacity.unwrap_or(1.0))
}
