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
use crate::elements::terminal::TerminalPane;
use crate::theme::Theme;
use shelbi_app::theme::ThemePreference;

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
    cx.set_global(Theme::resolve(
        ThemePreference::Dark,
        gpui::WindowAppearance::Dark,
    ));
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
        self.keys();
        self.ime();
        self.mouse();
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
            ("tab", b"\t", "Tab"),
            ("shift-tab", b"\x1b[Z", "Shift+Tab is backtab"),
            ("backspace", b"\x7f", "Backspace is DEL"),
            ("ctrl-h", b"\x08", "Ctrl+H is BS"),
            ("ctrl-c", b"\x03", "Ctrl+C"),
            ("alt-b", b"\x1bb", "Option+B is Meta+B"),
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
        self.with_pane(|pane, window, cx| {
            pane.replace_and_mark_text_in_range(None, "にほ", None, window, cx);
        })
        .unwrap();
        self.save("ime-composing.png");
        let during = self.io.take();
        self.expect(
            "composition sends nothing until committed",
            during.is_empty(),
            format!("{during:?}"),
        );
        self.with_pane(|pane, window, cx| pane.replace_text_in_range(None, "日本", window, cx))
            .unwrap();
        let got = self.io.input();
        self.expect(
            "committing a composition sends its text",
            got == "日本".as_bytes(),
            format!("{got:?}"),
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

/// Parse `--visual-check <dir>`.
pub fn out_dir(arg: Option<String>) -> PathBuf {
    arg.map(PathBuf::from)
        .unwrap_or_else(|| Path::new("visual-check").to_path_buf())
}
