//! `shelbi-desktop --bench-client <session> <cols>x<rows> <control-dir>`: the
//! desktop app's side of `scripts/multiclient-check.sh`.
//!
//! A real window holding one [`TerminalPane`] attached to `<session>` through
//! the app's own connect, its viewport held at exactly `<cols>x<rows>` cells.
//! Once attached it writes `<control-dir>/ready`, then runs the commands the
//! script drops in as `<control-dir>/cmd.<n>` (in order, answering each with
//! `ack.<n>`):
//!
//! - `type <text>`: each character as a key press through gpui's key
//!   dispatch and the pane's input handler, as typing in the window does;
//! - `key <keystroke>`: one key (`enter`, `ctrl-c`, ...);
//! - `dump <grid-file> <view-file>`: the pane's grid (the session-sized
//!   emulator screen, one line per row) and its viewport (that grid placed
//!   into the pane by `shelbi_term::viewport`, letterboxed or clipped: the
//!   cells the element paints);
//! - `quit`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use gpui::{
    div, point, prelude::*, px, size, App, AsyncApp, Bounds, Context, Entity, Focusable,
    Keystroke, Window, WindowBounds, WindowHandle, WindowOptions,
};
use shelbi_term::Size;

use crate::elements::terminal::{TerminalPane, TerminalSettings};
use crate::theme::Theme;

struct ClientRoot {
    pane: Entity<TerminalPane>,
    /// The viewport to hold, in cells.
    grid: Size,
}

impl Render for ClientRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let pane = self.pane.read(cx);
        // Exactly `grid` cells once the cell is measured (a hair over, so
        // rounding never loses a column).
        let fixed = pane.metrics.as_ref().map(|m| {
            (
                m.width * self.grid.cols as f32 + px(0.5),
                m.height * self.grid.rows as f32 + px(0.5),
            )
        });
        let holder = div().bg(theme.canvas).child(self.pane.clone());
        match fixed {
            Some((w, h)) => holder.w(w).h(h),
            None => holder.size_full(),
        }
    }
}

pub fn run(args: &[String]) -> ! {
    let usage = || -> ! {
        eprintln!("usage: shelbi-desktop --bench-client <session> <cols>x<rows> <control-dir>");
        std::process::exit(2)
    };
    let (Some(selector), Some(grid), Some(ctl)) = (args.first(), args.get(1), args.get(2)) else {
        usage()
    };
    let Some((cols, rows)) = grid
        .split_once('x')
        .and_then(|(c, r)| Some((c.parse::<u16>().ok()?, r.parse::<u16>().ok()?)))
    else {
        usage()
    };
    let (selector, grid, ctl) = (selector.clone(), Size::new(cols, rows), PathBuf::from(ctl));
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            crate::theme::init(cx);
            crate::assets::load_fonts(cx);
            TerminalSettings::init(cx);
            let bounds = Bounds {
                origin: point(px(60.0), px(80.0)),
                size: size(px(1400.0), px(900.0)),
            };
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        focus: false,
                        ..Default::default()
                    },
                    move |window, cx| {
                        let pane = cx.new(|cx| TerminalPane::connect(selector, window, cx));
                        pane.read(cx).focus_handle(cx).focus(window, cx);
                        cx.new(|_| ClientRoot { pane, grid })
                    },
                )
                .expect("open the client window");
            cx.spawn(async move |cx| drive(window, grid, ctl, cx).await)
                .detach();
        });
    std::process::exit(0)
}

async fn drive(window: WindowHandle<ClientRoot>, grid: Size, ctl: PathBuf, cx: &mut AsyncApp) {
    // Attached, with the viewport at the asked size.
    loop {
        cx.background_executor().timer(Duration::from_millis(20)).await;
        let ready = window
            .update(cx, |root, _, cx| {
                let pane = root.pane.read(cx);
                pane.is_live() && pane.geometry.as_ref().is_some_and(|g| g.viewer == grid)
            })
            .unwrap_or(false);
        if ready {
            break;
        }
    }
    std::fs::write(ctl.join("ready"), b"").expect("write ready");
    let mut next = 1u64;
    loop {
        let cmd_path = ctl.join(format!("cmd.{next}"));
        let Ok(cmd) = std::fs::read_to_string(&cmd_path) else {
            cx.background_executor().timer(Duration::from_millis(20)).await;
            continue;
        };
        let cmd = cmd.trim_end_matches('\n').to_string();
        let (verb, rest) = cmd.split_once(' ').unwrap_or((cmd.as_str(), ""));
        match verb {
            "type" => {
                for ch in rest.chars() {
                    let key = match ch {
                        ' ' => "space".to_string(),
                        c => c.to_string(),
                    };
                    press(window, &key, cx);
                }
            }
            "key" => press(window, rest, cx),
            "dump" => {
                let mut paths = rest.split_whitespace().map(Path::new);
                if let (Some(grid_file), Some(view_file)) = (paths.next(), paths.next()) {
                    let (grid_text, view_text) = window
                        .update(cx, |root, _, cx| dump(root.pane.read(cx)))
                        .expect("the client window");
                    std::fs::write(grid_file, grid_text).expect("write the grid");
                    std::fs::write(view_file, view_text).expect("write the view");
                }
            }
            "quit" => {
                let _ = std::fs::write(ctl.join(format!("ack.{next}")), b"");
                cx.update(|cx| cx.quit());
                return;
            }
            other => eprintln!("bench-client: unknown command `{other}`"),
        }
        // Let the frame that follows the input land before answering.
        cx.background_executor().timer(Duration::from_millis(50)).await;
        std::fs::write(ctl.join(format!("ack.{next}")), b"").expect("write the ack");
        next += 1;
    }
}

fn press(window: WindowHandle<ClientRoot>, key: &str, cx: &mut AsyncApp) {
    let keystroke = Keystroke::parse(key).unwrap_or_else(|_| panic!("bad key `{key}`"));
    let focus = window
        .update(cx, |root, _, cx| root.pane.read(cx).focus_handle(cx))
        .expect("the client window");
    // Through the window, not the root view: dispatch reaches the root's own
    // listeners, which can't run while it is leased.
    cx.update_window(window.into(), |_, window, cx| {
        window.focus(&focus, cx);
        window.dispatch_keystroke(keystroke, cx);
    })
    .expect("the client window");
}

/// The pane's grid, and its viewport (the grid placed into the pane).
fn dump(pane: &TerminalPane) -> (String, String) {
    let emu = pane.view().emulator();
    let grid = emu.grid();
    let size = emu.size();
    let row_text = |row: u16, cols: std::ops::Range<u16>| -> String {
        cols.filter_map(|col| {
            let cell = &grid[Line(row as i32)][Column(col as usize)];
            (!cell.flags.contains(Flags::WIDE_CHAR_SPACER)).then_some(match cell.c {
                '\0' => ' ',
                c => c,
            })
        })
        .collect()
    };
    let grid_text: Vec<String> = (0..size.rows)
        .map(|r| row_text(r, 0..size.cols).trim_end().to_string())
        .collect();
    let Some(g) = pane.geometry.as_ref() else {
        return (grid_text.join("\n"), String::new());
    };
    let p = g.placement;
    let view_text: Vec<String> = (0..g.viewer.rows)
        .map(|y| {
            let in_rows = y >= p.rows.pad_before && y < p.rows.pad_before + p.rows.len;
            if !in_rows {
                return String::new();
            }
            let row = p.rows.src_start + (y - p.rows.pad_before);
            let mut line = " ".repeat(p.cols.pad_before as usize);
            line.push_str(&row_text(row, p.cols.src_start..p.cols.src_start + p.cols.len));
            line.trim_end().to_string()
        })
        .collect();
    (grid_text.join("\n"), view_text.join("\n"))
}
