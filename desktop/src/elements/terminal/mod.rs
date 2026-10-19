//! The terminal: a gpui view ([`TerminalPane`]) that owns one session's
//! client-side [`TerminalView`], and the custom element
//! ([`TerminalElement`]) that paints its cell grid.
//!
//! ## Data flow
//!
//! ```text
//!  session socket --reader thread--> forwarder thread --async channel-->
//!    pump task (foreground): feed TerminalView, take damage, notify
//!      -> TerminalElement::prepaint: rebuild damaged rows, shape runs
//!      -> TerminalElement::paint: quads + glyphs + cursor + overlays
//!  keys / IME / mouse --> shelbi_term::input + gesture --> writer thread --> socket
//! ```
//!
//! Nothing here blocks the UI thread on IO: the connect runs on the
//! background executor, output arrives over a channel, and writes queue to a
//! writer thread ([`session`]). Repaint is driven by the emulator's damage:
//! the pump notifies only when a batch of output changed the screen, and no
//! timer runs while the session is idle (a blinking cursor runs one, while
//! focused, for a few seconds after the last activity).
//!
//! ## What the pane does
//!
//! - **Scrollback**: the wheel (Shift+wheel always), Shift+PageUp/PageDown/
//!   Home/End, and the scrollbar ([`scrollbar`]) scroll Shelbi's scrollback on
//!   the normal screen. New output while scrolled up doesn't move the view; a
//!   "Jump to bottom" button shows how far up it is. On the alternate screen
//!   the wheel reaches the program as arrow keys (`less`, `man`).
//! - **Selection**: a drag selects cells, a double-click a word, a
//!   triple-click a line, an Alt-drag a block (Shift keeps any of these
//!   Shelbi's while a program reports the mouse). A finished selection is
//!   copied, as in the TUI; Cmd+C copies too. The selection stays on its text
//!   as output scrolls and goes away when that text changes.
//! - **Paste**: Cmd+V, bracketed when the program asked.
//! - **Find**: Cmd+F opens a find bar ([`search_bar`]): search as you type,
//!   Enter / Shift+Enter (or Cmd+G / Cmd+Shift+G) step to older / newer
//!   matches, Escape closes.
//! - **IME and dead keys**: text arrives through the pane's input handler, so
//!   composition (marked text, drawn at the cursor), commit, and cancel come
//!   from the platform; with Option typing characters (the default,
//!   [`settings`]: `desktop.option_as_meta` is `false`, `true`, `left`, or
//!   `right`) Option+E, E composes `é`. Option with a named key (Enter,
//!   arrows, Backspace, Delete) is always Meta.
//! - **Links**: with Cmd held, a URL or a `path[:line[:col]]` under the
//!   pointer is underlined; Cmd+click opens it ([`open`]).
//! - **Mouse forwarding**: clicks, drags, and motion reach a program that
//!   reports them, in whichever mode it asked for (`shelbi_term::gesture`).
//! - **Fonts**: family and size from the settings, applied live; Source Code
//!   Pro (bundled) by default, Geist Mono (bundled) as the first fallback.
//! - **Colors**: the theme's terminal palette with the user's ANSI colors
//!   over it ([`colors`]), applied live.
//! - **Box drawing**: the common line and rounded-corner characters (`─ │ ╭
//!   ┼`, light and heavy) are drawn as lines snapped to device pixels
//!   ([`boxdraw`]), not taken from the font, so a box's border is one crisp
//!   pixel and its cells join without seams; the block elements (`█ ▐ ▛`)
//!   are filled the same way, so a run of them is solid.
//! - **Accessibility**: the pane is an AccessKit `Terminal` node whose
//!   children are the visible rows as text, with the cursor as the text
//!   selection, so VoiceOver can read the screen.
//!
//! ## Shared with the TUI
//!
//! Key encoding (`shelbi_term::input`), the mouse gesture rules
//! (`shelbi_term::gesture`), scrollback, selection, word boundaries, search
//! stepping, and link detection (`shelbi_term::view`, `selection`, `search`,
//! `links`) are the TUI's code, not copies. What is here is the gpui side:
//! keystroke classification ([`keys`]), cell geometry ([`metrics`]),
//! painting ([`layout`], [`highlights`], [`element`]), and the overlays.

pub mod a11y;
pub mod boxdraw;
pub mod colors;
mod element;
pub mod fonts;
pub mod highlights;
pub mod keys;
pub mod layout;
pub mod metrics;
mod open;
pub mod scrollbar;
pub mod search_bar;
pub mod session;
pub mod settings;

use std::collections::HashSet;
use std::future::Future;
use std::ops::Range;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    div, prelude::*, px, App, Bounds, ClipboardItem, Context, EntityInputHandler, ExternalPaths,
    FocusHandle, Focusable, Global, KeyDownEvent, MouseButton as GpuiMouseButton, Pixels, Point,
    Role, ScrollDelta, SharedString, Subscription, Task, UTF16Selection, Window, WindowId,
};
use shelbi_client::SessionEvent;
use shelbi_term::gesture::{MouseGesture, MouseOutcome};
use shelbi_term::input::{self, Modifiers, MouseAction, MouseButton, MouseEvent};
use shelbi_term::links::Link;
use shelbi_term::render::Damage;
use shelbi_term::selection::Point as SelPoint;
use shelbi_term::view::TerminalView;
use shelbi_term::viewport::Placement;
use shelbi_term::Size;

pub use element::TerminalElement;
use keys::KeyAction;
use layout::RowCache;
use metrics::CellMetrics;
use search_bar::{BarKey, SearchBar};
use session::SessionIo;
pub use settings::{FontZoom, TerminalSettings};

use crate::assets::{heroicon, icons, Icon};
use crate::theme::style::{Type, Typography};
use crate::theme::Theme;

/// How long a viewport size must hold before it is reported to the session,
/// so dragging the window edge sends one resize, not one per frame. The
/// session debounces on its side too.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(60);

/// The windows whose layout is in motion (the sidebar sliding in or out,
/// `dt-sidebar-toggle-animation`): their terminals are drawn at each frame's
/// size but hold their PTY resizes, and report the size they land at once,
/// when the motion settles, instead of one per frame.
#[derive(Debug, Default)]
pub struct ResizeHold(HashSet<WindowId>);

impl Global for ResizeHold {}

impl ResizeHold {
    /// Hold (or release) `window`'s resizes. Cheap when nothing changes.
    pub fn set(window: WindowId, held: bool, cx: &mut App) {
        if Self::holds_id(window, cx) == held {
            return;
        }
        let hold = cx.default_global::<Self>();
        if held {
            hold.0.insert(window);
        } else {
            hold.0.remove(&window);
        }
    }

    /// `window`'s layout is in motion.
    pub fn holds(window: &Window, cx: &App) -> bool {
        Self::holds_id(window.window_handle().window_id(), cx)
    }

    fn holds_id(window: WindowId, cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|h| h.0.contains(&window))
    }
}

/// Most output bytes fed to the emulator per UI turn before yielding, so a
/// flood (`yes`) still lets frames paint and input through.
const MAX_FEED_PER_TURN: usize = 256 * 1024;

/// Cursor blink half-period.
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// A blinking cursor stops (solid) this long after the last input or output,
/// so an idle pane runs no timer.
const BLINK_IDLE_STOP: Duration = Duration::from_secs(10);

/// While the find bar is open, matches are re-scanned at most this often as
/// output arrives (a scan covers the whole history).
const SEARCH_REFRESH: Duration = Duration::from_millis(200);

/// While a selection drag is held above or below the grid, scrollback moves
/// one line this often (and the selection follows).
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// How the pane's session is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneStatus {
    /// Looking up and attaching to the session.
    Connecting,
    /// Attached and streaming.
    Live,
    /// The session's program exited (the message says how).
    Exited(String),
    /// The connect failed or the stream ended unexpectedly.
    Failed(String),
}

/// What a pane tells the view that holds it about its session, so a view can
/// supervise it (the Chat view restarts a pane whose session died) and the
/// shell can show the program's title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// The program set its title (empty = reset).
    Title(String),
    /// The session's program exited.
    Exited(shelbi_proto::Exited),
    /// The connection ended without an exit: the session process went away
    /// (killed, or replaced by a relaunch).
    Closed,
}

impl gpui::EventEmitter<PaneEvent> for TerminalPane {}

/// Where the grid was painted last frame, for mapping pointer positions.
#[derive(Debug, Clone)]
pub(crate) struct Geometry {
    /// The element's bounds: viewer cell (0, 0) is its top-left.
    pub bounds: Bounds<Pixels>,
    /// The viewer's size in cells.
    pub viewer: Size,
    /// The session grid inside the viewer (letterbox or clip).
    pub placement: Placement,
}

/// Per-frame timing, logged at `debug` every couple of seconds while frames
/// are painting (`RUST_LOG=shelbi_desktop=debug`), and each frame at `trace`
/// (`shelbi_desktop::terminal::frame`). An idle pane paints no frames, so it
/// logs nothing.
#[derive(Debug, Default)]
pub(crate) struct FrameStats {
    pub frames: u32,
    pub busy: Duration,
    pub max: Duration,
    pub rows_rebuilt: usize,
    pub bytes_fed: usize,
    pub feed_time: Duration,
    pub window_start: Option<Instant>,
    /// Rows laid out by the most recent frame (not reset with the window).
    pub last_rebuilt: usize,
    /// Since the pane was made (not reset with the window): output bytes
    /// fed, and resyncs (the session's attach replay, or its
    /// recovery of a client that fell behind).
    pub total_fed: u64,
    pub resyncs: u32,
    pub total_feed_time: Duration,
    /// When output was last fed.
    pub last_fed_at: Option<Instant>,
    /// When output was first fed, and when the first frame after that
    /// painted (how long a newly shown session takes to appear).
    pub first_fed_at: Option<Instant>,
    pub first_content_at: Option<Instant>,
}

impl FrameStats {
    pub fn record_frame(&mut self, took: Duration, rows_rebuilt: usize) {
        // One event per painted frame (`RUST_LOG=shelbi_desktop::terminal::frame=trace`):
        // what an idle check counts.
        tracing::trace!(
            target: "shelbi_desktop::terminal::frame",
            us = took.as_micros() as u64,
            rows = rows_rebuilt,
            "frame"
        );
        let now = Instant::now();
        let start = *self.window_start.get_or_insert(now);
        self.last_rebuilt = rows_rebuilt;
        if self.first_fed_at.is_some() && self.first_content_at.is_none() {
            self.first_content_at = Some(now);
        }
        self.frames += 1;
        self.busy += took;
        self.max = self.max.max(took);
        self.rows_rebuilt += rows_rebuilt;
        let elapsed = now - start;
        if elapsed >= Duration::from_secs(2) {
            tracing::debug!(
                target: "shelbi_desktop::terminal::stats",
                fps = format!("{:.1}", self.frames as f64 / elapsed.as_secs_f64()),
                frame_avg_us = (self.busy / self.frames).as_micros() as u64,
                frame_max_us = self.max.as_micros() as u64,
                rows_per_frame = self.rows_rebuilt / self.frames as usize,
                feed_kib_s = format!("{:.0}", self.bytes_fed as f64 / 1024.0 / elapsed.as_secs_f64()),
                feed_ms = self.feed_time.as_millis() as u64,
                "terminal frames"
            );
            *self = FrameStats {
                last_rebuilt: rows_rebuilt,
                total_fed: self.total_fed,
                resyncs: self.resyncs,
                total_feed_time: self.total_feed_time,
                last_fed_at: self.last_fed_at,
                first_fed_at: self.first_fed_at,
                first_content_at: self.first_content_at,
                ..FrameStats::default()
            };
        }
    }
}

/// An IME composition in progress: the marked text and the part of it the
/// input method has selected (UTF-16 offsets, as the platform speaks them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Composition {
    pub text: String,
    pub selected: Range<usize>,
}

/// A scrollbar drag: where on the thumb it was grabbed.
#[derive(Debug, Clone, Copy)]
struct ScrollbarDrag {
    grab: f32,
}

/// One session in a terminal element.
pub struct TerminalPane {
    selector: String,
    title: Option<String>,
    status: PaneStatus,
    view: TerminalView,
    /// The PTY's size (the most recently active client's).
    session_size: Size,
    /// The session program's working directory, for clicked relative paths.
    cwd: Option<PathBuf>,
    io: Option<Box<dyn SessionIo>>,
    gesture: MouseGesture,
    /// A left-button gesture began over this pane (so its drag and release
    /// are ours even outside the bounds).
    left_down: bool,
    /// The middle or right button held since a press over this pane.
    other_down: Option<MouseButton>,
    /// The cell motion was last reported at, so a drag or move within one
    /// cell isn't reported again.
    last_motion_cell: Option<(u16, u16)>,
    /// Fractional wheel lines carried between events (trackpads send pixels).
    wheel_carry: f32,
    scrollbar_drag: Option<ScrollbarDrag>,
    pub(crate) scrollbar_hovered: bool,
    /// Scrolling while a selection drag is held past the top (`true`) or
    /// bottom edge.
    autoscroll: Option<(bool, Task<()>)>,
    focus_handle: FocusHandle,
    /// Focus as last reported to the program (focus in/out events).
    focused: bool,
    pub(crate) settings: TerminalSettings,
    pub(crate) rows: RowCache,
    /// Damage accumulated since the last frame.
    pub(crate) damage: Damage,
    pub(crate) metrics: Option<CellMetrics>,
    pub(crate) geometry: Option<Geometry>,
    /// The size last reported to the session, and one waiting out the
    /// debounce.
    reported_size: Option<Size>,
    pending_size: Option<Size>,
    resize_task: Option<Task<()>>,
    /// IME composition in progress, drawn at the cursor (or in the find bar
    /// while it is open).
    pub(crate) composition: Option<Composition>,
    /// The open find bar.
    search: Option<SearchBar>,
    search_refresh: Option<Task<()>>,
    /// Cmd is held (links are live).
    cmd_down: bool,
    /// The last pointer position over the pane.
    last_pointer: Option<Point<Pixels>>,
    /// The link under the pointer while Cmd is held.
    pub(crate) hovered_link: Option<Link>,
    /// Cursor blink phase (`true` = drawn).
    pub(crate) blink_on: bool,
    blink_task: Option<Task<()>>,
    last_activity: Instant,
    pub(crate) stats: FrameStats,
    #[cfg(feature = "visual-check")]
    link_sink: Option<std::rc::Rc<std::cell::RefCell<Vec<shelbi_term::links::LinkTarget>>>>,
    _pump: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl TerminalPane {
    /// A pane attached to the live session `selector` names (see
    /// [`session::connect`]); it shows "Connecting" until the attach lands.
    pub fn connect(selector: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut pane = Self::new(selector.clone(), window, cx);
        let background = cx.background_executor().clone();
        pane._pump = Some(cx.spawn_in(window, async move |this, cx| {
            let connected = background
                .spawn(async move { session::connect(&selector) })
                .await;
            let events = match connected {
                Ok(c) => {
                    let events = c.events;
                    let attached = this.update(cx, |pane, cx| {
                        pane.cwd = c.cwd;
                        pane.attached(c.name, c.size, c.link, cx);
                    });
                    if attached.is_err() {
                        return;
                    }
                    events
                }
                Err(message) => {
                    let _ = this.update(cx, |pane, cx| {
                        pane.status = PaneStatus::Failed(message);
                        cx.notify();
                    });
                    return;
                }
            };
            Self::pump(this, events, cx).await;
        }));
        pane
    }

    /// A pane over a session the caller already attached to (and wired with
    /// [`session::wire`]): it is live at once and starts pumping the attach
    /// replay. The Chat view attaches through `shelbi_app::session` (its
    /// relaunch-tolerant connect) and hands the result here.
    pub fn attached_to(
        connected: session::Connected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session::Connected {
            link,
            events,
            size,
            name,
            cwd,
        } = connected;
        let mut pane = Self::new(name.clone(), window, cx);
        pane.cwd = cwd;
        pane.attached(name, size, link, cx);
        pane._pump = Some(cx.spawn_in(window, async move |this, cx| {
            Self::pump(this, events, cx).await;
        }));
        pane
    }

    /// A read-only pane with no session behind it, showing `render`'s
    /// output for its size, drawn again whenever its size changes (Settings'
    /// preview). Keys and pastes go nowhere.
    pub fn canned(
        name: String,
        render: fn(Size) -> Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (events, rx) = futures::channel::mpsc::unbounded();
        let mut pane = Self::new(name.clone(), window, cx);
        pane.attached(name, Size::new(80, 24), Box::new(session::Canned { events, render }), cx);
        pane._pump = Some(cx.spawn_in(window, async move |this, cx| {
            Self::pump(this, rx, cx).await;
        }));
        pane
    }

    /// The visible screen as text, one line per row (tests).
    #[cfg(test)]
    pub fn terminal_text(&self) -> String {
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::index::{Column, Line};
        let grid = self.view.emulator().grid();
        (0..grid.screen_lines())
            .map(|row| {
                (0..grid.columns())
                    .map(|col| grid[Line(row as i32)][Column(col)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A pane already attached to `io` at `size`, with no session behind it:
    /// the visual check drives it with [`TerminalPane::feed`].
    #[cfg(feature = "visual-check")]
    pub fn detached(
        name: String,
        size: Size,
        io: Box<dyn SessionIo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self::new(name.clone(), window, cx);
        pane.attached(name, size, io, cx);
        pane
    }

    /// Feed `bytes` as session output (the visual check's stand-in for a
    /// session).
    #[cfg(feature = "visual-check")]
    pub fn feed(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.apply_events(
            vec![SessionEvent::Output {
                seq: 0,
                data: bytes.to_vec(),
            }],
            cx,
        );
    }

    /// A resize event, as the session sends when the PTY's size changes.
    #[cfg(feature = "visual-check")]
    pub fn session_resized(&mut self, size: Size, cx: &mut Context<Self>) {
        self.apply_events(
            vec![SessionEvent::Resized {
                seq: 0,
                cols: size.cols,
                rows: size.rows,
            }],
            cx,
        );
    }

    /// The view, for the visual check's assertions.
    #[cfg(feature = "visual-check")]
    pub fn terminal_view(&self) -> &TerminalView {
        &self.view
    }

    /// The view, mutably (to start a search, or scroll in a test).
    #[cfg(any(test, feature = "visual-check"))]
    pub fn terminal_view_mut(&mut self) -> &mut TerminalView {
        &mut self.view
    }

    /// Rows laid out by the last frame.
    #[cfg(feature = "visual-check")]
    pub fn stats_last_rebuilt(&self) -> usize {
        self.stats.last_rebuilt
    }

    /// The focus handle.
    #[cfg(feature = "visual-check")]
    pub fn focus(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// The find bar's query and status, when it is open.
    #[cfg(feature = "visual-check")]
    pub fn search_state(&self) -> Option<(String, String)> {
        self.search
            .as_ref()
            .map(|bar| (bar.query().to_string(), bar.status(self.view.search_position())))
    }

    /// Open the find bar on `query`, as typing it would (`--visual-figma`'s
    /// find state).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_find(&mut self, query: &str, cx: &mut Context<Self>) {
        self.search = Some(SearchBar::new(Some(query)));
        self.composition = None;
        self.run_search(cx);
        cx.notify();
    }

    /// Report a debounced resize now (gpui timers don't fire offscreen).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_flush_resize(&mut self) {
        if let (Some(io), Some(size)) = (self.io.as_ref(), self.pending_size.take()) {
            io.resize(size);
            self.reported_size = Some(size);
            self.resize_task = None;
        }
    }

    /// The link under the pointer (Cmd held).
    #[cfg(feature = "visual-check")]
    pub fn hovered_link(&self) -> Option<&Link> {
        self.hovered_link.as_ref()
    }

    /// Set the session's working directory (the visual check's links).
    #[cfg(feature = "visual-check")]
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = Some(cwd);
    }

    /// Record Cmd+clicked links here instead of opening them (the visual
    /// check mustn't launch a browser or an editor).
    #[cfg(feature = "visual-check")]
    pub fn set_link_sink(&mut self, sink: std::rc::Rc<std::cell::RefCell<Vec<shelbi_term::links::LinkTarget>>>) {
        self.link_sink = Some(sink);
    }

    /// The scrollbar thumb's bounds (window coordinates), when it shows.
    #[cfg(feature = "visual-check")]
    pub fn scrollbar_thumb_bounds(&self) -> Option<Bounds<Pixels>> {
        let track = self.scrollbar_track()?;
        let thumb = self.scrollbar_thumb()?;
        Some(Bounds {
            origin: gpui::point(track.right() - px(scrollbar::WIDTH + scrollbar::INSET), track.top() + px(thumb.top)),
            size: gpui::size(px(scrollbar::WIDTH), px(thumb.height)),
        })
    }

    /// Where the input method's candidate window would go for `range`.
    #[cfg(feature = "visual-check")]
    pub fn candidate_bounds(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let bounds = self.geometry.as_ref()?.bounds;
        self.bounds_for_range(range, bounds, window, cx)
    }

    /// The pane's geometry (element bounds, viewer cells).
    #[cfg(feature = "visual-check")]
    pub fn element_bounds(&self) -> Option<Bounds<Pixels>> {
        self.geometry.as_ref().map(|g| g.bounds)
    }

    /// The last frame's element bounds, viewer grid, cell height, and
    /// placement (tests).
    #[cfg(test)]
    pub(crate) fn last_frame(&self) -> Option<(Bounds<Pixels>, Size, Pixels, Placement)> {
        let (g, m) = (self.geometry.as_ref()?, self.metrics.as_ref()?);
        Some((g.bounds, g.viewer, m.height, g.placement))
    }

    /// The text of each row the last frame's window showed, top to bottom,
    /// trailing blanks trimmed (tests and the visual check).
    #[cfg(any(test, feature = "visual-check"))]
    pub(crate) fn shown_rows(&self) -> Vec<String> {
        use alacritty_terminal::index::{Column, Line};
        let Some(g) = self.geometry.as_ref() else {
            return Vec::new();
        };
        let grid = self.view.emulator().grid();
        let top = self.view.first_shown_line(&g.placement);
        (0..g.placement.rows.len as i32)
            .map(|row| {
                let line = &grid[Line(top + row)];
                (0..g.placement.cols.len as usize)
                    .map(|c| match line[Column(c)].c {
                        '\0' => ' ',
                        c => c,
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn new(selector: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // gpui's window takes only Finder drops; let this one take image and
        // file-promise drags too (`crate::drops`), so they reach `drop_paths`.
        crate::drops::accept_in(window);
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus_handle, window, |this, window, cx| {
                this.focus_changed(window, cx)
            }),
            cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
                this.focus_changed(window, cx)
            }),
            cx.observe_window_activation(window, |this, window, cx| this.focus_changed(window, cx)),
            cx.observe_global::<TerminalSettings>(|this, cx| this.settings_changed(cx)),
        ];
        let size = Size::new(80, 24);
        Self {
            selector,
            title: None,
            status: PaneStatus::Connecting,
            view: TerminalView::new(size),
            session_size: size,
            cwd: None,
            io: None,
            gesture: MouseGesture::new(1),
            left_down: false,
            other_down: None,
            last_motion_cell: None,
            wheel_carry: 0.0,
            scrollbar_drag: None,
            scrollbar_hovered: false,
            autoscroll: None,
            focus_handle,
            focused: false,
            settings: TerminalSettings::current(cx),
            rows: RowCache::default(),
            damage: Damage::Full,
            metrics: None,
            geometry: None,
            reported_size: None,
            pending_size: None,
            resize_task: None,
            composition: None,
            search: None,
            search_refresh: None,
            cmd_down: false,
            last_pointer: None,
            hovered_link: None,
            blink_on: true,
            blink_task: None,
            last_activity: Instant::now(),
            stats: FrameStats::default(),
            #[cfg(feature = "visual-check")]
            link_sink: None,
            _pump: None,
            _subscriptions: subscriptions,
        }
    }

    /// The session's title: the program's own (OSC 0/2), else the session
    /// name, else what the pane was asked to open.
    pub fn title(&self) -> SharedString {
        self.title
            .clone()
            .unwrap_or_else(|| self.selector.clone())
            .into()
    }

    fn attached(
        &mut self,
        name: String,
        size: Size,
        io: Box<dyn SessionIo>,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(target: "shelbi_desktop::terminal", session = %name, ?size, "attached");
        self.title = Some(name);
        self.session_size = size;
        self.view = TerminalView::new(size);
        self.damage = Damage::Full;
        self.status = PaneStatus::Live;
        // Report our size at once if we've already been laid out, and take
        // the session's size if we have focus (a reattach after a restart).
        if let Some(viewer) = self.geometry.as_ref().map(|g| g.viewer) {
            io.resize(viewer);
            self.reported_size = Some(viewer);
        }
        if self.focused {
            io.claim();
        }
        self.io = Some(io);
        cx.notify();
    }

    /// Make the PTY follow this pane's size: report a size still waiting out
    /// the debounce, then claim the session's sizing (the session applies the
    /// most recently active client's size). Sent when the pane takes focus,
    /// so a taller client that typed last (the TUI in a bigger terminal, a
    /// second window) stops sizing the session the user is now looking at.
    fn claim_size(&mut self) {
        let Some(io) = self.io.as_ref() else {
            return;
        };
        if let Some(size) = self.pending_size.take() {
            io.resize(size);
            self.reported_size = Some(size);
            self.resize_task = None;
        }
        io.claim();
    }

    /// The font, the Option key or the ANSI colors changed: re-measure the
    /// cell on the next frame (which reports the new grid size). New colors
    /// lay the rows out again by themselves (the row cache keys on the
    /// palette).
    fn settings_changed(&mut self, cx: &mut Context<Self>) {
        let settings = TerminalSettings::current(cx);
        if settings == self.settings {
            return;
        }
        let font_changed = settings.font_family != self.settings.font_family
            || settings.font_size != self.settings.font_size;
        self.settings = settings;
        if font_changed {
            self.metrics = None;
            self.damage = Damage::Full;
        }
        cx.notify();
    }

    /// Drain the session's events into the view, a bounded batch per UI turn.
    async fn pump(
        this: gpui::WeakEntity<Self>,
        mut events: futures::channel::mpsc::UnboundedReceiver<SessionEvent>,
        cx: &mut gpui::AsyncWindowContext,
    ) {
        while let Some(first) = events.next().await {
            let mut batch = vec![first];
            let mut bytes = event_len(&batch[0]);
            while bytes < MAX_FEED_PER_TURN {
                match events.try_recv() {
                    Ok(ev) => {
                        bytes += event_len(&ev);
                        batch.push(ev);
                    }
                    Err(_) => break,
                }
            }
            if this
                .update(cx, |pane, cx| pane.apply_events(batch, cx))
                .is_err()
            {
                return;
            }
            YieldNow::default().await;
        }
        let _ = this.update(cx, |pane, cx| {
            if pane.status == PaneStatus::Live {
                pane.status = PaneStatus::Failed("the session closed the connection".into());
                cx.emit(PaneEvent::Closed);
            }
            pane.io = None;
            cx.notify();
        });
    }

    fn apply_events(&mut self, batch: Vec<SessionEvent>, cx: &mut Context<Self>) {
        let start = Instant::now();
        let mut fed = 0;
        let had_selection = self.view.selection().is_some();
        for ev in batch {
            match ev {
                SessionEvent::Resync { replay, .. } => {
                    self.stats.resyncs += 1;
                    fed += replay.len();
                    self.view.feed(&replay);
                }
                SessionEvent::Output { data, .. } => {
                    fed += data.len();
                    self.view.feed(&data);
                }
                SessionEvent::Resized { cols, rows, .. }
                | SessionEvent::SizeChanged { cols, rows } => {
                    self.session_size = Size::new(cols.max(1), rows.max(1));
                    self.view.resize(self.session_size);
                }
                SessionEvent::Title(t) => {
                    cx.emit(PaneEvent::Title(t.clone()));
                    self.title = if t.is_empty() { None } else { Some(t) };
                    cx.notify();
                }
                SessionEvent::Bell => {}
                SessionEvent::Exited(e) => {
                    self.status = PaneStatus::Exited(match (e.code, e.signal) {
                        (Some(c), _) => format!("exited with code {c}"),
                        (_, Some(s)) => format!("killed by signal {s}"),
                        _ => "exited".into(),
                    });
                    cx.emit(PaneEvent::Exited(e));
                    cx.notify();
                }
            }
        }
        if fed > 0 {
            self.stats.bytes_fed += fed;
            self.stats.total_fed += fed as u64;
            self.stats.total_feed_time += start.elapsed();
            self.stats.last_fed_at = Some(Instant::now());
            self.stats.first_fed_at.get_or_insert(start);
            self.stats.feed_time += start.elapsed();
            self.activity(cx);
            self.schedule_search_refresh(cx);
            // The text under the pointer may have changed.
            self.hovered_link = self.link_under_pointer();
        }
        let damage = self.view.take_damage();
        if !damage.is_empty() {
            self.damage.merge(damage);
            cx.notify();
        } else if had_selection && self.view.selection().is_none() {
            // The selection was dropped (its text changed) with no damage on
            // screen (it was scrolled away): still repaint.
            cx.notify();
        }
    }

    /// Bring the frame's state up to date and report a new viewport size
    /// (kept until the motion ends while the window's layout is `held`,
    /// [`ResizeHold`]). Called from the element's prepaint.
    pub(crate) fn begin_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        viewer: Size,
        held: bool,
        cx: &mut Context<Self>,
    ) {
        let placement = self.view.placement(self.session_size, viewer);
        self.geometry = Some(Geometry {
            bounds,
            viewer,
            placement,
        });
        // Local changes (a scrollback move, a resize) damage the whole screen.
        // Output damage was already taken by the pump; what is left otherwise
        // is alacritty's always-damaged cursor row, and the cursor is painted
        // as an overlay, never baked into a row, so it needs no rebuild.
        if let Damage::Full = self.view.take_damage() {
            self.damage = Damage::Full;
        }
        self.note_viewer_size(viewer, held, cx);
    }

    /// The shown rows, rebuilt where damaged, and how many were rebuilt.
    pub(crate) fn layout_rows(
        &mut self,
        placement: Placement,
        palette: &layout::Palette,
    ) -> (Vec<std::sync::Arc<layout::RowLayout>>, usize) {
        let damage = std::mem::replace(&mut self.damage, Damage::Rows(Vec::new()));
        let rows = self.rows.update(
            self.view.emulator(),
            placement.rows.src_start,
            placement.rows.len,
            placement.cols.len,
            palette,
            &damage,
        );
        (rows, self.rows.take_rebuilt())
    }

    pub(crate) fn view(&self) -> &TerminalView {
        &self.view
    }

    /// Output bytes fed since the pane was made (the bench).
    #[cfg_attr(not(feature = "bench"), allow(dead_code))]
    pub(crate) fn total_fed(&self) -> u64 {
        self.stats.total_fed
    }

    /// When output was last fed (the bench).
    #[cfg_attr(not(feature = "bench"), allow(dead_code))]
    pub(crate) fn last_fed_at(&self) -> Option<Instant> {
        self.stats.last_fed_at
    }

    /// When output was first fed and when the first frame after that painted
    /// (the bench).
    #[cfg_attr(not(feature = "bench"), allow(dead_code))]
    pub(crate) fn first_content(&self) -> (Option<Instant>, Option<Instant>) {
        (self.stats.first_fed_at, self.stats.first_content_at)
    }

    /// Time spent feeding the emulator since the pane was made (the bench).
    #[cfg_attr(not(feature = "bench"), allow(dead_code))]
    pub(crate) fn feed_time_total(&self) -> Duration {
        self.stats.total_feed_time
    }

    /// Resyncs received since the pane was made (the bench).
    #[cfg_attr(not(feature = "bench"), allow(dead_code))]
    pub(crate) fn resyncs(&self) -> u32 {
        self.stats.resyncs
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(crate) fn is_live(&self) -> bool {
        self.status == PaneStatus::Live
    }

    pub(crate) fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus_handle
    }

    pub(crate) fn search_open(&self) -> bool {
        self.search.is_some()
    }

    fn note_viewer_size(&mut self, viewer: Size, held: bool, cx: &mut Context<Self>) {
        if self.reported_size == Some(viewer) {
            self.pending_size = None;
            self.resize_task = None;
            return;
        }
        if self.pending_size == Some(viewer) && (held || self.resize_task.is_some()) {
            return;
        }
        let Some(io) = self.io.as_ref() else {
            return;
        };
        if self.reported_size.is_none() {
            io.resize(viewer);
            self.reported_size = Some(viewer);
            return;
        }
        if held {
            // The window's layout is in motion: keep the latest size, and
            // report the one it settles at.
            self.pending_size = Some(viewer);
            self.resize_task = None;
            return;
        }
        if self.pending_size.is_some() && self.resize_task.is_none() {
            // A hold just lifted: the motion is over, report its size now.
            io.resize(viewer);
            self.pending_size = None;
            self.reported_size = Some(viewer);
            return;
        }
        self.pending_size = Some(viewer);
        let timer = cx.background_executor().timer(RESIZE_DEBOUNCE);
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |pane, _| {
                if let (Some(io), Some(size)) = (pane.io.as_ref(), pane.pending_size.take()) {
                    io.resize(size);
                    pane.reported_size = Some(size);
                }
            });
        }));
    }

    // ---- input ----

    fn send_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.is_empty() {
            return;
        }
        self.view.on_user_input();
        self.view.clear_selection();
        if let Some(io) = self.io.as_ref() {
            io.input(bytes);
        }
        self.activity(cx);
        cx.notify();
    }

    /// Files dropped on the pane: paste their paths, as a terminal pastes a
    /// dropped file (`rt-drag-and-drop-paste-into-a-review-or-workspace-panel`),
    /// and take keyboard focus so the user can keep typing after the drop.
    /// A Finder drop arrives straight from gpui; an image or file-promise drop
    /// is first written to a file by `crate::drops`, then replayed here.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let text = input::dropped_paths_text(paths.paths());
        if text.is_empty() || self.io.is_none() {
            return;
        }
        window.focus(&self.focus_handle, cx);
        self.paste(text, cx);
    }

    fn paste(&mut self, text: String, cx: &mut Context<Self>) {
        self.view.on_user_input();
        self.view.clear_selection();
        if let Some(io) = self.io.as_ref() {
            io.paste(text, self.view.emulator().bracketed_paste());
        }
        self.activity(cx);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        // The find bar takes its keys first. A composition in progress owns
        // Enter, Escape, and Backspace (the input method handles them).
        if self.search.is_some() && self.composition.is_none() && !m.control && !m.alt {
            if let Some(key) = BarKey::from_key(&ks.key, m.shift, m.platform) {
                self.bar_key(key, cx);
                cx.stop_propagation();
                return;
            }
        }
        let meta = m.alt && keys::option_is_meta(self.settings.option_as_meta, keys::held_option_sides());
        match keys::classify(ks, meta) {
            KeyAction::Send(key, mods) => {
                if self.search.is_none() {
                    let bytes = input::encode_key(key, mods, self.view.emulator().key_encoding());
                    self.send_input(bytes, cx);
                }
                // While the find bar is open, keys don't reach the session.
                cx.stop_propagation();
            }
            // Typed text reaches `replace_text_in_range` through the input
            // handler (IME and dead keys compose there).
            KeyAction::Text => {}
            // ⌘C / ⌘V normally arrive as the Edit menu's actions (bound in
            // this element's `Terminal` key context); this is the fallback.
            KeyAction::Copy => {
                self.copy(cx);
                cx.stop_propagation();
            }
            KeyAction::Paste => {
                self.paste_clipboard(cx);
                cx.stop_propagation();
            }
            KeyAction::Find => {
                self.open_search(cx);
                cx.stop_propagation();
            }
            KeyAction::Scroll(scroll) => {
                if self.view.scrollback_enabled() {
                    match scroll {
                        keys::ScrollKey::PageUp => self.view.page_up(),
                        keys::ScrollKey::PageDown => self.view.page_down(),
                        keys::ScrollKey::Top => self.view.scroll_to_top(),
                        keys::ScrollKey::Bottom => self.view.scroll_to_bottom(),
                    }
                    cx.notify();
                } else if self.search.is_none() {
                    let enc = self.view.emulator().key_encoding();
                    self.send_input(input::encode_key(scroll.key(), Modifiers::SHIFT, enc), cx);
                }
                cx.stop_propagation();
            }
            KeyAction::Ignore => {}
        }
    }

    /// Edit ▸ Copy: the selection to the clipboard.
    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// Edit ▸ Paste: the clipboard's text to the find bar while it is open,
    /// otherwise to the program.
    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            match self.search.as_mut() {
                Some(bar) => {
                    if bar.insert(&text) {
                        self.run_search(cx);
                    }
                }
                None => self.paste(text, cx),
            }
        }
    }

    /// Edit ▸ Select All: everything retained, scrollback included (the
    /// alternate screen has none, so just its grid).
    fn select_all(&mut self, cx: &mut Context<Self>) {
        let size = self.view.emulator().size();
        let history = self.view.emulator().history_len() as i32;
        self.view.begin_selection(SelPoint { line: -history, col: 0 });
        self.view.update_selection(SelPoint {
            line: i32::from(size.rows) - 1,
            col: size.cols.saturating_sub(1),
        });
        cx.notify();
    }

    /// The current non-empty selection's text.
    fn selection_text(&self) -> Option<String> {
        if self.view.selection().is_none_or(|s| s.is_empty()) {
            return None;
        }
        self.view.selection_text().filter(|t| !t.is_empty())
    }

    fn focus_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.focus_handle.is_focused(window) && window.is_window_active();
        if focused == self.focused {
            return;
        }
        self.focused = focused;
        if focused {
            self.claim_size();
        }
        if let (Some(io), Some(bytes)) = (
            self.io.as_ref(),
            input::encode_focus(focused, self.view.emulator().focus_reporting()),
        ) {
            io.input(bytes);
        }
        if !focused && self.cmd_down {
            self.cmd_down = false;
            self.hovered_link = None;
        }
        self.activity(cx);
        cx.notify();
    }

    // ---- find bar ----

    /// Open the find bar (or keep it open), seeded with the selection when
    /// it is one line of text.
    fn open_search(&mut self, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let seed = self.selection_text().filter(|t| !t.contains('\n'));
            self.search = Some(SearchBar::new(seed.as_deref()));
            self.composition = None;
            self.run_search(cx);
        }
        cx.notify();
    }

    fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search = None;
        self.search_refresh = None;
        self.composition = None;
        self.view.clear_search();
        cx.notify();
    }

    /// Search for the bar's query from the newest visible match.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_ref() else {
            return;
        };
        if bar.query().is_empty() {
            self.view.clear_search();
        } else {
            self.view.start_search(bar.query(), bar.case_insensitive());
        }
        cx.notify();
    }

    /// Step to the next older (`older`) or newer match.
    fn step_search(&mut self, older: bool, cx: &mut Context<Self>) {
        if older {
            self.view.search_prev();
        } else {
            self.view.search_next();
        }
        cx.notify();
    }

    fn bar_key(&mut self, key: BarKey, cx: &mut Context<Self>) {
        let Some(bar) = self.search.as_mut() else {
            return;
        };
        match key {
            BarKey::Older => self.step_search(true, cx),
            BarKey::Newer => self.step_search(false, cx),
            BarKey::Backspace => {
                if bar.backspace() {
                    self.run_search(cx);
                }
            }
            BarKey::Clear => {
                if bar.clear() {
                    self.run_search(cx);
                }
            }
            BarKey::Close => self.close_search(cx),
        }
    }

    /// New output while the find bar is open: re-scan soon (debounced).
    fn schedule_search_refresh(&mut self, cx: &mut Context<Self>) {
        if self.search.is_none() || self.search_refresh.is_some() {
            return;
        }
        let timer = cx.background_executor().timer(SEARCH_REFRESH);
        self.search_refresh = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |pane, cx| {
                pane.search_refresh = None;
                if pane.search.as_ref().is_some_and(|b| !b.query().is_empty()) {
                    pane.view.refresh_search();
                    cx.notify();
                }
            });
        }));
    }

    // ---- mouse (called from the element's window listeners) ----

    /// The viewer cell under `position` and the placement to map it with.
    fn pointer_cell(&self, position: Point<Pixels>) -> Option<(u16, u16, Placement)> {
        let metrics = self.metrics.as_ref()?;
        let g = self.geometry.as_ref()?;
        let (col, row) = metrics.cell_at(position, g.bounds.origin, g.viewer);
        Some((col, row, g.placement))
    }

    /// The link under `position`, if any.
    fn link_at(&self, position: Point<Pixels>) -> Option<Link> {
        let g = self.geometry.as_ref()?;
        if !g.bounds.contains(&position) {
            return None;
        }
        let (col, row, placement) = self.pointer_cell(position)?;
        let point = self.view.viewer_point_to_grid(&placement, col, row)?;
        self.view.link_at(point)
    }

    /// The link under the pointer while Cmd is held.
    fn link_under_pointer(&self) -> Option<Link> {
        if !self.cmd_down {
            return None;
        }
        self.link_at(self.last_pointer?)
    }

    fn update_hovered_link(&mut self, cx: &mut Context<Self>) {
        let link = self.link_under_pointer();
        if link != self.hovered_link {
            self.hovered_link = link;
            cx.notify();
        }
    }

    pub(crate) fn modifiers_changed(&mut self, modifiers: gpui::Modifiers, cx: &mut Context<Self>) {
        if modifiers.platform != self.cmd_down {
            self.cmd_down = modifiers.platform;
            self.update_hovered_link(cx);
        }
    }

    /// The scrollbar's track (the element's right edge) when the scrollbar
    /// shows: on the normal screen, with history to scroll.
    fn scrollbar_track(&self) -> Option<Bounds<Pixels>> {
        let g = self.geometry.as_ref()?;
        let emu = self.view.emulator();
        if !self.view.scrollback_enabled() || emu.history_len() == 0 {
            return None;
        }
        Some(Bounds {
            origin: gpui::point(g.bounds.right() - px(scrollbar::HIT_WIDTH), g.bounds.top()),
            size: gpui::size(px(scrollbar::HIT_WIDTH), g.bounds.size.height),
        })
    }

    /// The thumb for the current scroll position, relative to the track top.
    pub(crate) fn scrollbar_thumb(&self) -> Option<scrollbar::Thumb> {
        let track = self.scrollbar_track()?;
        let emu = self.view.emulator();
        scrollbar::thumb(
            emu.history_len(),
            self.view.scroll_offset(),
            emu.size().rows as usize,
            f32::from(track.size.height),
        )
    }

    pub(crate) fn scrollbar_active(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    /// Scroll to exactly `offset` lines above the live bottom.
    fn scroll_to_offset(&mut self, offset: usize, cx: &mut Context<Self>) {
        let current = self.view.scroll_offset();
        if offset > current {
            self.view.scroll_up(offset - current);
        } else if offset < current {
            self.view.scroll_down(current - offset);
        }
        cx.notify();
    }

    /// A press on the scrollbar: grab the thumb, or jump to the press on the
    /// track. Returns whether the scrollbar took it.
    fn scrollbar_press(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(track) = self.scrollbar_track() else {
            return false;
        };
        if !track.contains(&position) {
            return false;
        }
        let Some(thumb) = self.scrollbar_thumb() else {
            return false;
        };
        let y = f32::from(position.y - track.top());
        let grab = if y >= thumb.top && y <= thumb.top + thumb.height {
            y - thumb.top
        } else {
            let emu = self.view.emulator();
            let offset = scrollbar::offset_for_track_click(
                y,
                emu.history_len(),
                emu.size().rows as usize,
                f32::from(track.size.height),
            );
            self.scroll_to_offset(offset, cx);
            thumb.height / 2.0
        };
        self.scrollbar_drag = Some(ScrollbarDrag { grab });
        cx.notify();
        true
    }

    fn scrollbar_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(drag), Some(track)) = (self.scrollbar_drag, self.scrollbar_track()) else {
            return;
        };
        let emu = self.view.emulator();
        let offset = scrollbar::offset_for_thumb_top(
            f32::from(position.y - track.top()) - drag.grab,
            emu.history_len(),
            emu.size().rows as usize,
            f32::from(track.size.height),
        );
        self.scroll_to_offset(offset, cx);
    }

    pub(crate) fn mouse_down(
        &mut self,
        button: GpuiMouseButton,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.last_pointer = Some(position);
        if button == GpuiMouseButton::Left {
            if self.scrollbar_press(position, cx) {
                return;
            }
            if modifiers.platform {
                if let Some(link) = self.link_at(position) {
                    #[cfg(feature = "visual-check")]
                    if let Some(sink) = self.link_sink.as_ref() {
                        sink.borrow_mut().push(link.target);
                        return;
                    }
                    open::open(&link.target, self.cwd.clone(), cx);
                    return;
                }
            }
        }
        let Some(button) = map_button(button) else {
            return;
        };
        if button == MouseButton::Left {
            self.left_down = true;
        } else {
            self.other_down = Some(button);
        }
        self.last_motion_cell = None;
        let clicks = clicks.min(u8::MAX as usize) as u8;
        self.mouse(MouseAction::Press, button, position, modifiers, clicks, cx);
    }

    /// The pointer moved: a drag of the held button, a scrollbar drag, or a
    /// plain move (link hover, and motion for a program that tracks it).
    pub(crate) fn mouse_move(
        &mut self,
        position: Point<Pixels>,
        pressed: Option<GpuiMouseButton>,
        modifiers: gpui::Modifiers,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        self.last_pointer = Some(position);
        if self.scrollbar_drag.is_some() {
            self.scrollbar_move(position, cx);
            return;
        }
        let over_scrollbar = hovered && self.scrollbar_track().is_some_and(|t| t.contains(&position));
        if over_scrollbar != self.scrollbar_hovered {
            self.scrollbar_hovered = over_scrollbar;
            cx.notify();
        }
        match pressed.and_then(map_button) {
            Some(MouseButton::Left) if self.left_down => {
                self.mouse(MouseAction::Drag, MouseButton::Left, position, modifiers, 1, cx);
                self.update_autoscroll(position, modifiers, cx);
            }
            Some(b) if self.other_down == Some(b) => {
                self.mouse(MouseAction::Drag, b, position, modifiers, 1, cx);
            }
            None if hovered => {
                if self.cmd_down != modifiers.platform {
                    self.cmd_down = modifiers.platform;
                }
                self.update_hovered_link(cx);
                self.mouse(MouseAction::Move, MouseButton::Left, position, modifiers, 1, cx);
            }
            _ => {}
        }
    }

    pub(crate) fn mouse_up(
        &mut self,
        button: GpuiMouseButton,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if button == GpuiMouseButton::Left && self.scrollbar_drag.take().is_some() {
            cx.notify();
            return;
        }
        let Some(button) = map_button(button) else {
            return;
        };
        if button == MouseButton::Left {
            self.autoscroll = None;
            if !std::mem::take(&mut self.left_down) {
                return;
            }
        } else if self.other_down.take() != Some(button) && !hovered {
            return;
        }
        self.last_motion_cell = None;
        self.mouse(MouseAction::Release, button, position, modifiers, 1, cx);
    }

    /// Start or stop scrolling while a selection drag is held past the top
    /// or bottom of the grid (normal screen only; the selection extends as
    /// the view moves under the pointer).
    fn update_autoscroll(&mut self, position: Point<Pixels>, modifiers: gpui::Modifiers, cx: &mut Context<Self>) {
        let Some(g) = self.geometry.as_ref() else {
            return;
        };
        let up = if position.y < g.bounds.top() {
            Some(true)
        } else if position.y >= g.bounds.bottom() {
            Some(false)
        } else {
            None
        };
        let Some(up) = up.filter(|_| self.gesture.is_selecting() && self.view.scrollback_enabled()) else {
            self.autoscroll = None;
            return;
        };
        if self.autoscroll.as_ref().is_some_and(|(dir, _)| *dir == up) {
            return;
        }
        let executor = cx.background_executor().clone();
        let task = cx.spawn(async move |this, cx| loop {
            executor.timer(AUTOSCROLL_INTERVAL).await;
            let keep = this
                .update(cx, |pane, cx| {
                    if !pane.left_down || !pane.gesture.is_selecting() {
                        pane.autoscroll = None;
                        return false;
                    }
                    if up {
                        pane.view.scroll_up(1);
                    } else {
                        pane.view.scroll_down(1);
                    }
                    // Extend the selection to the edge row now under the
                    // pointer.
                    if let Some(at) = pane.last_pointer {
                        pane.last_motion_cell = None;
                        pane.mouse(MouseAction::Drag, MouseButton::Left, at, modifiers, 1, cx);
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !keep {
                return;
            }
        });
        self.autoscroll = Some((up, task));
    }

    pub(crate) fn scroll_wheel(
        &mut self,
        delta: ScrollDelta,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(metrics) = self.metrics.as_ref() else {
            return;
        };
        let lines = match delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / f32::from(metrics.height),
        };
        self.wheel_carry += lines;
        let whole = self.wheel_carry.trunc();
        self.wheel_carry -= whole;
        let button = if whole > 0.0 {
            MouseButton::WheelUp
        } else {
            MouseButton::WheelDown
        };
        for _ in 0..(whole.abs() as usize).min(100) {
            self.mouse(MouseAction::Press, button, position, modifiers, 1, cx);
        }
    }

    fn mouse(
        &mut self,
        action: MouseAction,
        button: MouseButton,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        clicks: u8,
        cx: &mut Context<Self>,
    ) {
        let Some((col, row, placement)) = self.pointer_cell(position) else {
            return;
        };
        if matches!(action, MouseAction::Drag | MouseAction::Move) {
            // Motion is reported per cell, not per pixel.
            if self.last_motion_cell == Some((col, row)) {
                return;
            }
            self.last_motion_cell = Some((col, row));
        }
        let mods = Modifiers {
            shift: modifiers.shift,
            alt: modifiers.alt,
            ctrl: modifiers.control,
            sup: modifiers.platform,
        };
        let ev = MouseEvent {
            action,
            button,
            col,
            row,
            mods,
        };
        match self.gesture.handle_clicks(&mut self.view, &ev, &placement, clicks) {
            MouseOutcome::Forward(bytes) => {
                if let Some(io) = self.io.as_ref() {
                    io.input(bytes);
                }
            }
            MouseOutcome::Handled => cx.notify(),
            MouseOutcome::Copy(text) => {
                // A finished selection copies, as in the TUI.
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.notify();
            }
            MouseOutcome::Ignored => {}
        }
    }

    // ---- cursor blink ----

    /// Note input or output: the cursor shows solid and (if the program asked
    /// for a blinking cursor and we're focused) blinks for a while.
    fn activity(&mut self, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        self.blink_on = true;
        let wants_blink = self.focused && self.view.emulator().cursor().is_some_and(|c| c.blinking);
        if !wants_blink {
            self.blink_task = None;
            return;
        }
        if self.blink_task.is_some() {
            return;
        }
        let executor = cx.background_executor().clone();
        self.blink_task = Some(cx.spawn(async move |this, cx| loop {
            executor.timer(BLINK_INTERVAL).await;
            let keep = this
                .update(cx, |pane, cx| {
                    if pane.last_activity.elapsed() >= BLINK_IDLE_STOP || !pane.focused {
                        pane.blink_on = true;
                        pane.blink_task = None;
                        cx.notify();
                        return false;
                    }
                    pane.blink_on = !pane.blink_on;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !keep {
                return;
            }
        }));
    }

    // ---- accessibility ----

    /// The visible rows as AccessKit text runs under the pane's `Terminal`
    /// node, with the cursor as the text selection. Built only while an
    /// assistive technology is connected.
    fn a11y_children(&self, window: &Window) -> impl FnOnce(&mut gpui::A11ySubtreeBuilder) + 'static {
        use gpui::accesskit::{Node, Rect, TextPosition, TextSelection};
        let text = self.view.screen_text();
        let scale = window.scale_factor() as f64;
        let rows: Vec<Option<Rect>> = (0..text.rows.len())
            .map(|row| {
                let (m, g) = (self.metrics.as_ref()?, self.geometry.as_ref()?);
                let origin = element::grid_origin(g, m);
                // Rows outside a clipped window have no bounds on screen.
                let shown = (row as u16).checked_sub(g.placement.rows.src_start)?;
                if shown >= g.placement.rows.len {
                    return None;
                }
                let b = m.cell_bounds(origin, 0, shown, g.placement.cols.len);
                Some(Rect {
                    x0: f64::from(f32::from(b.left())) * scale,
                    y0: f64::from(f32::from(b.top())) * scale,
                    x1: f64::from(f32::from(b.right())) * scale,
                    y1: f64::from(f32::from(b.bottom())) * scale,
                })
            })
            .collect();
        move |builder| {
            let last = text.rows.len().saturating_sub(1);
            let mut row_runs = Vec::with_capacity(text.rows.len());
            for (i, row) in text.rows.iter().enumerate() {
                let runs = a11y::row_runs(row, i < last);
                let mut ids = Vec::with_capacity(runs.len());
                for (j, run) in runs.iter().enumerate() {
                    let id = builder.synthetic_node_id((i, j));
                    let mut node = Node::new(Role::TextRun);
                    node.set_value(run.value.clone());
                    node.set_character_lengths(run.character_lengths.clone());
                    node.set_word_starts(run.word_starts.clone());
                    if let Some(Some(bounds)) = rows.get(i) {
                        node.set_bounds(*bounds);
                    }
                    builder.push_child(id, node);
                    ids.push(id);
                }
                row_runs.push((runs, ids));
            }
            if let Some((row, index)) = text.cursor {
                if let Some((runs, ids)) = row_runs.get(row as usize) {
                    let (run, character_index) = a11y::caret(runs, index);
                    let caret = TextPosition {
                        node: ids[run],
                        character_index,
                    };
                    builder.parent_node().set_text_selection(TextSelection {
                        anchor: caret,
                        focus: caret,
                    });
                }
            }
        }
    }
}

fn event_len(ev: &SessionEvent) -> usize {
    match ev {
        SessionEvent::Output { data, .. } => data.len(),
        SessionEvent::Resync { replay, .. } => replay.len(),
        _ => 0,
    }
}

fn map_button(b: GpuiMouseButton) -> Option<MouseButton> {
    match b {
        GpuiMouseButton::Left => Some(MouseButton::Left),
        GpuiMouseButton::Middle => Some(MouseButton::Middle),
        GpuiMouseButton::Right => Some(MouseButton::Right),
        GpuiMouseButton::Navigate(_) => None,
    }
}

/// Yield once to the executor, so a long output flood interleaves with frames
/// and input instead of starving them.
#[derive(Default)]
struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalPane {
    /// The find bar overlay.
    fn render_search_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let bar = self.search.as_ref()?;
        let (text, marked_at) = bar.display();
        let status = bar.status(self.view.search_position());
        let (typed, marked) = match marked_at {
            Some(at) => (text[..at].to_string(), Some(text[at..].to_string())),
            None => (text, None),
        };
        let icon_button = |id: &'static str, icon: Icon, older: Option<bool>, cx: &mut Context<Self>| {
            div()
                .id(id)
                .debug_selector(move || format!("terminal-search-{id}"))
                .p_0p5()
                .rounded(px(6.0))
                .cursor_pointer()
                .hover(|s| s.bg(theme.background_hover))
                .child(heroicon(icon).text_color(theme.icon))
                .on_click(cx.listener(move |this, _, _, cx| match older {
                    Some(older) => this.step_search(older, cx),
                    None => this.close_search(cx),
                }))
        };
        Some(
            div()
                .id("terminal-search")
                .debug_selector(|| "terminal-search".into())
                .occlude()
                .absolute()
                .top_2()
                .right(px(scrollbar::HIT_WIDTH + 8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .w(px(340.0))
                .px_2()
                .py_1()
                .map(|d| crate::theme::style::popover(d, theme))
                .type_style(Type::Base)
                .child(heroicon(icons::MAGNIFYING_GLASS).text_color(theme.icon))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(theme.text_loud)
                        .when(typed.is_empty() && marked.is_none(), |d| {
                            d.child(div().text_color(theme.muted).child("Find"))
                        })
                        .child(typed)
                        .when_some(marked, |d, m| d.child(div().underline().child(m)))
                        .child(div().w(px(2.0)).h(px(16.0)).rounded_full().bg(theme.focus_ring)),
                )
                .child(div().flex_none().type_style(Type::XSmall).text_color(theme.muted).child(status))
                .child(icon_button("older", icons::CHEVRON_UP, Some(true), cx))
                .child(icon_button("newer", icons::CHEVRON_DOWN, Some(false), cx))
                .child(icon_button("close", icons::X_MARK, None, cx)),
        )
    }

    /// The "Jump to bottom" button while scrolled up.
    fn render_jump_to_bottom(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let offset = self.view.scroll_offset();
        if offset == 0 {
            return None;
        }
        let label = if offset == 1 {
            "1 line below".to_string()
        } else {
            format!("{offset} lines below")
        };
        Some(
            div()
                .id("terminal-jump-to-bottom")
                .debug_selector(|| "terminal-jump-to-bottom".into())
                .occlude()
                .absolute()
                .bottom_3()
                .right(px(scrollbar::HIT_WIDTH + 8.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .map(|d| crate::theme::style::popover(d, theme))
                .type_style(Type::XSmallMedium)
                .text_color(theme.text_loud)
                .cursor_pointer()
                .hover(|s| s.bg(theme.background))
                .child(heroicon(icons::ARROW_DOWN).text_color(theme.icon))
                .child(label)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.view.scroll_to_bottom();
                    window.focus(&this.focus_handle, cx);
                    cx.notify();
                })),
        )
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let notice = match &self.status {
            PaneStatus::Live => None,
            PaneStatus::Connecting => Some(format!("Connecting to {}…", self.selector)),
            PaneStatus::Exited(how) => Some(format!("{} {how}", self.title())),
            PaneStatus::Failed(why) => Some(why.clone()),
        };
        let a11y = window.is_a11y_active().then(|| self.a11y_children(window));
        let search_bar = self.render_search_bar(&theme, cx);
        let jump = self.render_jump_to_bottom(&theme, cx);
        let has_selection = self.view.selection().is_some_and(|s| !s.is_empty());
        div()
            .id("terminal-pane")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .role(Role::Terminal)
            .aria_label(self.title())
            .when_some(a11y, |d, children| d.a11y_synthetic_children(children))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_drop(cx.listener(Self::drop_paths))
            .on_modifiers_changed(cx.listener(|this, e: &gpui::ModifiersChangedEvent, _, cx| {
                this.modifiers_changed(e.modifiers, cx)
            }))
            // The Edit menu: Copy only with a selection (gray otherwise);
            // there is nothing to cut in a terminal.
            .when(has_selection, |d| {
                d.on_action(cx.listener(|this, _: &crate::menus::Copy, _, cx| this.copy(cx)))
            })
            .on_action(cx.listener(|this, _: &crate::menus::Paste, _, cx| this.paste_clipboard(cx)))
            .on_action(cx.listener(|this, _: &crate::menus::SelectAll, _, cx| this.select_all(cx)))
            .relative()
            .size_full()
            .bg(theme.canvas)
            .child(TerminalElement::new(cx.entity()))
            .children(search_bar)
            .children(jump)
            .when_some(notice, |pane, notice| {
                pane.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .px_3()
                        .py_1p5()
                        .bg(theme.sidebar)
                        .border_t_1()
                        .border_color(theme.divider)
                        .text_color(theme.muted)
                        .type_style(Type::Small)
                        .child(notice),
                )
            })
    }
}

/// The UTF-16 length of `s`.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The byte range of UTF-16 range `range` in `s` (clamped).
fn utf16_to_byte_range(s: &str, range: Range<usize>) -> Range<usize> {
    let byte_at = |target: usize| {
        let mut units = 0;
        for (i, c) in s.char_indices() {
            if units >= target {
                return i;
            }
            units += c.len_utf16();
        }
        s.len()
    };
    let start = byte_at(range.start);
    start..byte_at(range.end).max(start)
}

impl EntityInputHandler for TerminalPane {
    /// The input method reads back the composition (the only "document" a
    /// terminal has to offer it).
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.composition.as_ref().map_or("", |c| c.text.as_str());
        let len = utf16_len(text);
        let range = range.start.min(len)..range.end.min(len);
        *adjusted_range = Some(range.clone());
        Some(text[utf16_to_byte_range(text, range)].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let range = match &self.composition {
            Some(c) => c.selected.clone(),
            None => 0..0,
        };
        Some(UTF16Selection {
            range,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.composition.as_ref().map(|c| 0..utf16_len(&c.text))
    }

    /// The input method abandoned the composition (cancel): drop it, send
    /// nothing.
    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.composition = None;
        if let Some(bar) = self.search.as_mut() {
            bar.set_marked(None);
        }
        cx.notify();
    }

    /// Committed text (a typed key, a finished composition, a dead-key
    /// result): to the find bar while it is open, else to the session.
    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition = None;
        match self.search.as_mut() {
            Some(bar) => {
                if bar.insert(text) {
                    self.run_search(cx);
                }
            }
            None => self.send_input(text.as_bytes().to_vec(), cx),
        }
        cx.notify();
    }

    /// A composition in progress (or a dead key waiting for its letter):
    /// shown at the cursor, sent nowhere until committed.
    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composition = (!new_text.is_empty()).then(|| {
            let len = utf16_len(new_text);
            let selected = new_selected_range
                .map(|r| r.start.min(len)..r.end.min(len))
                .unwrap_or(len..len);
            Composition {
                text: new_text.to_string(),
                selected,
            }
        });
        if let Some(bar) = self.search.as_mut() {
            bar.set_marked(Some(new_text));
        }
        cx.notify();
    }

    /// Where the candidate window goes: under the composition's selected
    /// part, at the cursor cell (or the find bar while it is open).
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let metrics = self.metrics.as_ref()?;
        if self.search.is_some() {
            // The bar sits at the top right; anchor the candidates there.
            let right = element_bounds.right() - px(scrollbar::HIT_WIDTH + 8.0);
            return Some(Bounds {
                origin: gpui::point(right - px(300.0), element_bounds.top() + px(8.0)),
                size: gpui::size(metrics.width, metrics.height + px(8.0)),
            });
        }
        let g = self.geometry.as_ref()?;
        let cursor = self.view.emulator().cursor()?;
        let origin = element::grid_origin(g, metrics);
        let text = self.composition.as_ref().map_or("", |c| c.text.as_str());
        let prefix = &text[utf16_to_byte_range(text, 0..range_utf16.start)];
        let cells = unicode_width::UnicodeWidthStr::width(prefix) as u16;
        let row = cursor.row.checked_sub(g.placement.rows.src_start)?;
        Some(metrics.cell_bounds(origin, cursor.col.saturating_add(cells), row, 1))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_ranges_map_to_byte_ranges() {
        let s = "aé漢👍z";
        // UTF-16 units: a(1) é(1) 漢(1) 👍(2) z(1).
        assert_eq!(utf16_len(s), 6);
        assert_eq!(&s[utf16_to_byte_range(s, 0..2)], "aé");
        assert_eq!(&s[utf16_to_byte_range(s, 2..5)], "漢👍");
        assert_eq!(&s[utf16_to_byte_range(s, 5..6)], "z");
        assert_eq!(&s[utf16_to_byte_range(s, 4..99)], "z", "clamped, from inside a pair");
        let past_the_end = 9..12;
        assert_eq!(utf16_to_byte_range(s, past_the_end), s.len()..s.len());
    }
}
