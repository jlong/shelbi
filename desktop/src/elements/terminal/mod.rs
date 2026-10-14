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
//!      -> TerminalElement::paint: quads + glyphs + cursor
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
//! ## Shared with the TUI
//!
//! Key encoding (`shelbi_term::input`), the mouse gesture rules
//! (`shelbi_term::gesture`: a drag selects, a click reaches a reporting
//! program, Shift keeps a gesture Shelbi's, Alt hands it to the program, the
//! wheel scrolls scrollback unless the program owns the mouse), scrollback,
//! selection, and search (`shelbi_term::view`) are the TUI's code, not copies.
//! What is here is the gpui side: keystroke classification ([`keys`]), cell
//! geometry ([`metrics`]), and painting ([`layout`], [`element`]).

mod element;
pub mod keys;
pub mod layout;
pub mod metrics;
pub mod session;

use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    div, prelude::*, Bounds, ClipboardItem, Context, EntityInputHandler, FocusHandle, Focusable,
    KeyDownEvent, MouseButton as GpuiMouseButton, Pixels, Point, ScrollDelta, SharedString,
    Subscription, Task, UTF16Selection, Window,
};
use shelbi_client::SessionEvent;
use shelbi_term::gesture::{MouseGesture, MouseOutcome};
use shelbi_term::input::{self, Modifiers, MouseAction, MouseButton, MouseEvent};
use shelbi_term::render::Damage;
use shelbi_term::view::TerminalView;
use shelbi_term::viewport::{self, Placement};
use shelbi_term::Size;

pub use element::TerminalElement;
use keys::KeyAction;
use layout::RowCache;
use metrics::CellMetrics;
use session::SessionIo;

use crate::theme::Theme;

/// How long a viewport size must hold before it is reported to the session,
/// so dragging the window edge sends one resize, not one per frame. The
/// session debounces on its side too.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(60);

/// Most output bytes fed to the emulator per UI turn before yielding, so a
/// flood (`yes`) still lets frames paint and input through.
const MAX_FEED_PER_TURN: usize = 256 * 1024;

/// Cursor blink half-period.
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// A blinking cursor stops (solid) this long after the last input or output,
/// so an idle pane runs no timer.
const BLINK_IDLE_STOP: Duration = Duration::from_secs(10);

/// Option acts as Meta for character keys (Option+B sends `ESC b`), which is
/// what Claude Code's Option chords expect. `dt-terminal-complete` makes this
/// a setting.
const OPTION_AS_ALT: bool = true;

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
/// are painting (`RUST_LOG=shelbi_desktop=debug`). An idle pane paints no
/// frames, so it logs nothing.
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
}

impl FrameStats {
    pub fn record_frame(&mut self, took: Duration, rows_rebuilt: usize) {
        let now = Instant::now();
        let start = *self.window_start.get_or_insert(now);
        self.last_rebuilt = rows_rebuilt;
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
                ..FrameStats::default()
            };
        }
    }
}

/// One session in a terminal element.
pub struct TerminalPane {
    selector: String,
    title: Option<String>,
    status: PaneStatus,
    view: TerminalView,
    /// The PTY's size (the most recently active client's).
    session_size: Size,
    io: Option<Box<dyn SessionIo>>,
    gesture: MouseGesture,
    /// A left-button gesture began over this pane (so its drag and release
    /// are ours even outside the bounds).
    left_down: bool,
    /// Fractional wheel lines carried between events (trackpads send pixels).
    wheel_carry: f32,
    focus_handle: FocusHandle,
    /// Focus as last reported to the program (focus in/out events).
    focused: bool,
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
    /// IME composition in progress, drawn at the cursor.
    pub(crate) marked_text: Option<String>,
    /// Cursor blink phase (`true` = drawn).
    pub(crate) blink_on: bool,
    blink_task: Option<Task<()>>,
    last_activity: Instant,
    pub(crate) stats: FrameStats,
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
        } = connected;
        let mut pane = Self::new(name.clone(), window, cx);
        pane.attached(name, size, link, cx);
        pane._pump = Some(cx.spawn_in(window, async move |this, cx| {
            Self::pump(this, events, cx).await;
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

    /// The view, mutably (to start a search).
    #[cfg(feature = "visual-check")]
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

    fn new(selector: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus_handle, window, |this, window, cx| {
                this.focus_changed(window, cx)
            }),
            cx.on_focus_out(&focus_handle, window, |this, _, window, cx| {
                this.focus_changed(window, cx)
            }),
            cx.observe_window_activation(window, |this, window, cx| this.focus_changed(window, cx)),
        ];
        let size = Size::new(80, 24);
        Self {
            selector,
            title: None,
            status: PaneStatus::Connecting,
            view: TerminalView::new(size),
            session_size: size,
            io: None,
            gesture: MouseGesture::new(1),
            left_down: false,
            wheel_carry: 0.0,
            focus_handle,
            focused: false,
            rows: RowCache::default(),
            damage: Damage::Full,
            metrics: None,
            geometry: None,
            reported_size: None,
            pending_size: None,
            resize_task: None,
            marked_text: None,
            blink_on: true,
            blink_task: None,
            last_activity: Instant::now(),
            stats: FrameStats::default(),
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
        self.title = Some(name);
        self.session_size = size;
        self.view = TerminalView::new(size);
        self.damage = Damage::Full;
        self.status = PaneStatus::Live;
        // Claim our size at once if we've already been laid out.
        if let Some(viewer) = self.geometry.as_ref().map(|g| g.viewer) {
            io.resize(viewer);
            self.reported_size = Some(viewer);
        }
        self.io = Some(io);
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
        for ev in batch {
            match ev {
                SessionEvent::Resync { replay, .. } => {
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
            self.stats.feed_time += start.elapsed();
            self.activity(cx);
        }
        let damage = self.view.take_damage();
        if !damage.is_empty() {
            self.damage.merge(damage);
            cx.notify();
        }
    }

    /// Bring the frame's state up to date and report a new viewport size.
    /// Called from the element's prepaint.
    pub(crate) fn begin_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        viewer: Size,
        cx: &mut Context<Self>,
    ) {
        let placement = viewport::fit(self.session_size, viewer);
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
        self.note_viewer_size(viewer, cx);
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

    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    pub(crate) fn is_live(&self) -> bool {
        self.status == PaneStatus::Live
    }

    pub(crate) fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus_handle
    }

    fn note_viewer_size(&mut self, viewer: Size, cx: &mut Context<Self>) {
        if self.reported_size == Some(viewer) {
            self.pending_size = None;
            self.resize_task = None;
            return;
        }
        if self.pending_size == Some(viewer) {
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
        match keys::classify(&event.keystroke, OPTION_AS_ALT) {
            KeyAction::Send(key, mods) => {
                let bytes = input::encode_key(key, mods, self.view.emulator().key_encoding());
                self.send_input(bytes, cx);
                cx.stop_propagation();
            }
            // Typed text reaches `replace_text_in_range` through the input
            // handler (IME and dead keys compose there).
            KeyAction::Text => {}
            KeyAction::Copy => {
                if let Some(text) = self.selection_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
                cx.stop_propagation();
            }
            KeyAction::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.paste(text, cx);
                }
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
                } else {
                    let enc = self.view.emulator().key_encoding();
                    self.send_input(input::encode_key(scroll.key(), Modifiers::SHIFT, enc), cx);
                }
                cx.stop_propagation();
            }
            KeyAction::Ignore => {}
        }
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
        if let (Some(io), Some(bytes)) = (
            self.io.as_ref(),
            input::encode_focus(focused, self.view.emulator().focus_reporting()),
        ) {
            io.input(bytes);
        }
        self.activity(cx);
        cx.notify();
    }

    // ---- mouse (called from the element's window listeners) ----

    /// The viewer cell under `position` and the placement to map it with.
    fn pointer_cell(&self, position: Point<Pixels>) -> Option<(u16, u16, Placement)> {
        let metrics = self.metrics.as_ref()?;
        let g = self.geometry.as_ref()?;
        let (col, row) = metrics.cell_at(position, g.bounds.origin, g.viewer);
        Some((col, row, g.placement))
    }

    pub(crate) fn mouse_down(
        &mut self,
        button: GpuiMouseButton,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some(button) = map_button(button) else {
            return;
        };
        if button == MouseButton::Left {
            self.left_down = true;
        }
        self.mouse(MouseAction::Press, button, position, modifiers, cx);
    }

    pub(crate) fn mouse_drag(
        &mut self,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.left_down {
            self.mouse(
                MouseAction::Drag,
                MouseButton::Left,
                position,
                modifiers,
                cx,
            );
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
        let Some(button) = map_button(button) else {
            return;
        };
        if button == MouseButton::Left {
            if !std::mem::take(&mut self.left_down) {
                return;
            }
        } else if !hovered {
            return;
        }
        self.mouse(MouseAction::Release, button, position, modifiers, cx);
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
            self.mouse(MouseAction::Press, button, position, modifiers, cx);
        }
    }

    fn mouse(
        &mut self,
        action: MouseAction,
        button: MouseButton,
        position: Point<Pixels>,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some((col, row, placement)) = self.pointer_cell(position) else {
            return;
        };
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
        match self.gesture.handle(&mut self.view, &ev, &placement) {
            MouseOutcome::Forward(bytes) => {
                if let Some(io) = self.io.as_ref() {
                    io.input(bytes);
                }
            }
            MouseOutcome::Handled => cx.notify(),
            MouseOutcome::Copy(text) => {
                // A finished drag copies, as in the TUI.
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

impl Render for TerminalPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let notice = match &self.status {
            PaneStatus::Live => None,
            PaneStatus::Connecting => Some(format!("Connecting to {}…", self.selector)),
            PaneStatus::Exited(how) => Some(format!("{} {how}", self.title())),
            PaneStatus::Failed(why) => Some(why.clone()),
        };
        div()
            .id("terminal-pane")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .relative()
            .size_full()
            .bg(theme.canvas)
            .child(TerminalElement::new(cx.entity()))
            .when_some(notice, |pane, notice| {
                pane.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .px_3()
                        .py_1()
                        .bg(theme.selection)
                        .text_color(theme.muted)
                        .text_sm()
                        .child(notice),
                )
            })
    }
}

impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked_text
            .as_deref()
            .map_or(0, |t| t.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_text
            .as_deref()
            .map(|t| 0..t.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        self.send_input(text.as_bytes().to_vec(), cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The IME's candidate window sits under the cursor cell.
        let metrics = self.metrics.as_ref()?;
        let g = self.geometry.as_ref()?;
        let cursor = self.view.emulator().cursor()?;
        let origin = element::grid_origin(g, metrics);
        Some(metrics.cell_bounds(origin, cursor.col, cursor.row, 1))
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
