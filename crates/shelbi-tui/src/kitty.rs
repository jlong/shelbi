//! Markdown images drawn inline with the kitty graphics protocol, where the
//! outer terminal speaks it (kitty, Ghostty, WezTerm): the Changelog's entries
//! and the Documentation's pages. Everywhere else (and for an image that
//! can't be drawn: a missing file, a remote window's entry, a web image) the
//! renderers keep their placeholders exactly as before.
//!
//! How a picture gets on screen:
//!
//! 1. **Layout.** A Markdown line holding nothing but images
//!    ([`shelbi_app::changelog::image_line`]) becomes a figure per image: rows
//!    reserved for the picture, sized from the file's header ([`dims`]) to fit
//!    the column ([`fit`], at most [`MAX_ROWS`] tall, never scaled up), and its
//!    alt text as a muted caption under it ([`figure`]). The rows are part of
//!    the page's lines, so scrolling and wrapping account for them.
//! 2. **Draw.** The page paints the figure's visible rows with a sentinel cell
//!    ([`reserve`]) and hands back a [`Placement`]. Once everything (toast,
//!    overlay, status band) has drawn over the frame, [`clip_to_intact`] keeps
//!    only the longest run of rows still holding the sentinel, so the picture
//!    never covers what was drawn over it.
//! 3. **Emit.** [`Images::frame`] turns the placements into escape sequences
//!    written after ratatui's flush, inside the same synchronized update:
//!    each image is decoded, scaled and re-encoded as PNG off the UI thread
//!    ([`Images::poll`] reports when one lands), transmitted once by id, and
//!    placed (or moved) by image + placement id. A clipped edge is a source
//!    rectangle, so the top and bottom of the viewport cut cleanly. A
//!    placement not wanted this frame is deleted, so nothing lingers over
//!    another page; [`wipe`] deletes every image when the terminal is handed
//!    away (an editor, a remote session, exit, a panic).
//!
//! Responses are suppressed (`q=2`) on everything but the startup probe
//! (`shell::caps`), so the terminal never answers into the input stream.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use shelbi_app::changelog::ImageSource;

use crate::theme::palette;

/// The tallest a figure gets, in rows.
pub(crate) const MAX_ROWS: u16 = 20;
/// A terminal cell's size in pixels when the terminal doesn't report its
/// window's (only the ratio matters for the layout).
pub(crate) const DEFAULT_CELL: (u16, u16) = (10, 20);
/// Kitty image ids are shared by every program drawing in the window: ours
/// start here, well away from the small ids a shell's tools pick.
const ID_BASE: u32 = 0x5348_0000;
/// Base64 bytes per transmission chunk (the protocol's limit).
const CHUNK: usize = 4096;
/// Decoded images kept for the session before the unused ones are dropped.
const CACHE_LIMIT: usize = 64;

/// The terminal draws kitty graphics, with cells this many pixels across and
/// down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Graphics {
    pub cell: (u16, u16),
}

impl Graphics {
    /// The cell size from the terminal's window size (`TIOCGWINSZ`), or
    /// [`DEFAULT_CELL`] when it reports no pixels.
    pub(crate) fn query() -> Self {
        let cell = crossterm::terminal::window_size()
            .ok()
            .and_then(|ws| cell_size(ws.columns, ws.rows, ws.width, ws.height))
            .unwrap_or(DEFAULT_CELL);
        Graphics { cell }
    }
}

/// A cell's pixel size from the window's cells and pixels; `None` when the
/// terminal leaves the pixels out (zero).
pub(crate) fn cell_size(cols: u16, rows: u16, width: u16, height: u16) -> Option<(u16, u16)> {
    let (cw, ch) = (width.checked_div(cols)?, height.checked_div(rows)?);
    (cw > 0 && ch > 0).then_some((cw, ch))
}

/// The cells an image of `px` pixels takes in a column `max_cols` wide:
/// its own size where it fits, else scaled down (aspect kept) to the column
/// and to `max_rows`. `None` for an empty image or no room.
pub(crate) fn fit(px: (u32, u32), cell: (u16, u16), max_cols: u16, max_rows: u16) -> Option<(u16, u16)> {
    let (w, h) = (f64::from(px.0), f64::from(px.1));
    let (cw, ch) = (f64::from(cell.0.max(1)), f64::from(cell.1.max(1)));
    if w <= 0.0 || h <= 0.0 || max_cols == 0 || max_rows == 0 {
        return None;
    }
    let scale = 1f64
        .min(f64::from(max_cols) * cw / w)
        .min(f64::from(max_rows) * ch / h);
    let cols = ((w * scale / cw).round() as u16).clamp(1, max_cols);
    let rows = ((h * scale / ch).round() as u16).clamp(1, max_rows);
    Some((cols, rows))
}

/// An image a page laid out as a figure: its rows start at `line` (of the
/// lines it was laid out with), `rows` tall and `cols` wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Figure {
    pub line: usize,
    pub path: PathBuf,
    pub cols: u16,
    pub rows: u16,
}

/// How a renderer lays out image-only lines as figures: the terminal's
/// graphics, and where an image's url points.
pub(crate) struct Figures<'a> {
    pub gfx: Graphics,
    pub resolve: &'a dyn Fn(&str) -> ImageSource,
}

/// The lines a figure takes in a column `width` wide (its rows, blank, then
/// the alt text as a muted caption) and the figure, at line 0. `None` when the
/// image can't be drawn here (not a readable PNG, JPEG or GIF on this
/// machine): the caller keeps its placeholder.
pub(crate) fn figure(alt: &str, source: &ImageSource, width: usize, gfx: Graphics) -> Option<(Vec<Line<'static>>, Figure)> {
    let ImageSource::File(path) = source else {
        return None;
    };
    if undrawable(path) {
        return None;
    }
    let px = dims(path)?;
    let (cols, rows) = fit(px, gfx.cell, u16::try_from(width).unwrap_or(u16::MAX), MAX_ROWS)?;
    let mut lines = vec![Line::default(); rows as usize];
    let alt = alt.trim();
    if !alt.is_empty() {
        let style = Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC);
        lines.extend(crate::markdown::render_note(alt, width).into_iter().map(|l| l.style(style)));
    }
    Some((lines, Figure { line: 0, path: path.clone(), cols, rows }))
}

/// The pixel size in an image file's header (PNG, JPEG or GIF), cached by
/// path for the session: what a layout sizes a figure from, read without
/// decoding the picture. `None` when it isn't one of those or can't be read,
/// or when decoding it failed ([`forget_dims`]).
pub(crate) fn dims(path: &Path) -> Option<(u32, u32)> {
    type Sizes = HashMap<PathBuf, Option<(u32, u32)>>;
    static CACHE: OnceLock<Mutex<Sizes>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok()?.get(path) {
        return *hit;
    }
    let read = image::ImageReader::open(path)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.into_dimensions().ok())
        .filter(|(w, h)| *w > 0 && *h > 0);
    cache.lock().ok()?.insert(path.to_path_buf(), read);
    read
}

/// Shown images that couldn't be decoded: laid out again, they keep their
/// placeholders.
static UNDRAWABLE: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();

fn undrawable(path: &Path) -> bool {
    UNDRAWABLE
        .get()
        .and_then(|s| s.lock().ok().map(|s| s.contains(path)))
        .unwrap_or(false)
}

/// A figure's visible slice in a viewport `height` rows tall scrolled
/// `scroll` lines down: the viewport row its first shown row lands on, the
/// image rows cut above it, and how many show. `None` when none do.
pub(crate) fn visible(line: usize, rows: u16, scroll: usize, height: usize) -> Option<(u16, u16, u16)> {
    let start = line.max(scroll);
    let end = (line + rows as usize).min(scroll + height);
    (start < end).then(|| ((start - scroll) as u16, (start - line) as u16, (end - start) as u16))
}

/// Where a figure is drawn this frame: its visible rows' top-left cell, its
/// full size in cells, and the slice of its rows that shows (`skip` cut above,
/// `shown` showing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    pub path: PathBuf,
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub rows: u16,
    pub skip: u16,
    pub shown: u16,
}

impl Placement {
    fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.cols, self.shown)
    }
}

/// Lay out `fig` (its lines starting at document line `top_line`) in a
/// viewport whose first row is document line `scroll`: reserve its visible
/// cells in `buf` and return where it goes, clamped to `vp`. `None` when it's
/// off screen, or the image couldn't be decoded.
pub(crate) fn place(buf: &mut Buffer, fig: &Figure, top_line: usize, scroll: usize, vp: Rect, x: u16) -> Option<Placement> {
    if undrawable(&fig.path) {
        return None;
    }
    let (dy, skip, shown) = visible(top_line + fig.line, fig.rows, scroll, vp.height as usize)?;
    let cols = fig.cols.min(vp.right().saturating_sub(x));
    if cols == 0 {
        return None;
    }
    let p = Placement { path: fig.path.clone(), x, y: vp.y + dy, cols: fig.cols, rows: fig.rows, skip, shown };
    reserve(buf, Rect::new(x, p.y, cols, shown));
    // A column narrower than the figure (a resize mid-layout) clips it.
    (cols == fig.cols).then_some(p)
}

/// The cell a figure's rows are painted with: blank, but marked (an
/// underline color with no underline), so [`clip_to_intact`] can tell a row
/// nothing was drawn over from one a popup or a toast covered.
fn sentinel() -> Cell {
    let mut cell = Cell::default();
    cell.set_symbol(" ").set_style(Style::default().underline_color(Color::Indexed(53)));
    cell
}

/// Paint `area` with the figure sentinel.
pub(crate) fn reserve(buf: &mut Buffer, area: Rect) {
    let cell = sentinel();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            buf[(x, y)] = cell.clone();
        }
    }
}

/// The placements as the finished frame leaves them: each cut to its longest
/// run of rows still holding the sentinel in every cell (what was drawn over
/// the rest, a toast, the status band, a dimmed backdrop, stays on top), and
/// dropped when no row is left.
pub(crate) fn clip_to_intact(buf: &Buffer, placements: Vec<Placement>) -> Vec<Placement> {
    let cell = sentinel();
    let intact = |x0: u16, w: u16, y: u16| (x0..x0 + w).all(|x| buf.cell((x, y)) == Some(&cell));
    placements
        .into_iter()
        .filter_map(|p| {
            let rect = p.rect();
            let (mut best, mut run) = ((0u16, 0u16), None::<u16>);
            for row in 0..rect.height {
                if intact(rect.x, rect.width, rect.y + row) {
                    let start = *run.get_or_insert(row);
                    if row + 1 - start > best.1 {
                        best = (start, row + 1 - start);
                    }
                } else {
                    run = None;
                }
            }
            (best.1 > 0).then(|| Placement { y: p.y + best.0, skip: p.skip + best.0, shown: best.1, ..p })
        })
        .collect()
}

// --- escapes ---------------------------------------------------------------

/// The graphics command `keys;payload` as an APC string.
fn apc(out: &mut Vec<u8>, keys: &str, payload: &str) {
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(keys.as_bytes());
    if !payload.is_empty() {
        out.push(b';');
        out.extend_from_slice(payload.as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
}

/// Transmit `png` (base64) as image `id`, in chunks.
fn transmit(out: &mut Vec<u8>, id: u32, png: &str) {
    let chunks: Vec<&str> = png
        .as_bytes()
        .chunks(CHUNK)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect();
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i < last);
        let keys = if i == 0 {
            format!("a=t,t=d,f=100,i={id},q=2,m={more}")
        } else {
            format!("m={more},q=2")
        };
        apc(out, &keys, chunk);
    }
}

/// What a placement looks like on screen, compared frame to frame.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    x: u16,
    y: u16,
    cols: u16,
    shown: u16,
    /// The source rectangle's top and height in the image's pixels.
    src: (u32, u32),
}

impl Placed {
    fn of(p: &Placement, height_px: u32) -> Self {
        let rows = u32::from(p.rows.max(1));
        let top = (u32::from(p.skip) * height_px / rows).min(height_px.saturating_sub(1));
        let h = (u32::from(p.shown) * height_px / rows).clamp(1, height_px - top);
        Placed { x: p.x, y: p.y, cols: p.cols, shown: p.shown, src: (top, h) }
    }
}

/// Place image `id` (`width_px` wide) as placement `pid`: the cursor saved,
/// moved to the cell, the image scaled into its cells, and the cursor put
/// back, so ratatui's own cursor is left where it was.
fn put(out: &mut Vec<u8>, id: u32, pid: u32, p: &Placed, width_px: u32) {
    out.extend_from_slice(format!("\x1b7\x1b[{};{}H", p.y + 1, p.x + 1).as_bytes());
    let keys = format!(
        "a=p,i={id},p={pid},c={},r={},x=0,y={},w={width_px},h={},C=1,q=2",
        p.cols, p.shown, p.src.0, p.src.1
    );
    apc(out, &keys, "");
    out.extend_from_slice(b"\x1b8");
}

fn delete(out: &mut Vec<u8>, id: u32, pid: u32) {
    apc(out, &format!("a=d,d=i,i={id},p={pid},q=2"), "");
}

/// Every placement deleted and every image's data freed.
pub(crate) const DELETE_ALL: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

/// Whether this process ever put an image up (so [`wipe`] has anything to do).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Bumped by each [`wipe`], so the tracker knows the terminal holds nothing.
static WIPES: AtomicU64 = AtomicU64::new(0);

/// Take every image off the screen and out of the terminal's memory: before
/// the terminal is handed to something else (an editor, a remote session)
/// or restored on exit. A no-op when no image was ever shown. The tracker
/// sends everything afresh next frame.
pub(crate) fn wipe(out: &mut impl Write) {
    if ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = out.write_all(DELETE_ALL);
        let _ = out.flush();
        WIPES.fetch_add(1, Ordering::SeqCst);
    }
}

// --- the tracker -----------------------------------------------------------

/// An image at the size a figure draws it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    cols: u16,
    rows: u16,
}

/// A decoded image, scaled for its figure and encoded as base64 PNG.
#[derive(Debug)]
struct Encoded {
    png: String,
    px: (u32, u32),
}

#[derive(Debug)]
enum Entry {
    Loading,
    Ready { id: u32, image: Arc<Encoded>, sent: bool },
    Failed,
}

/// The shell's images: whether the terminal draws them, the decoded ones,
/// and what's on screen.
pub(crate) struct Images {
    gfx: Option<Graphics>,
    entries: HashMap<Key, Entry>,
    /// On screen: `(image id, placement id)` → where.
    placed: HashMap<(u32, u32), Placed>,
    next_id: u32,
    wipes: u64,
    tx: Sender<(Key, Option<Encoded>)>,
    rx: Receiver<(Key, Option<Encoded>)>,
    /// Decode off the UI thread (`false` in tests that want it inline).
    background: bool,
}

impl Default for Images {
    fn default() -> Self {
        Self::new()
    }
}

impl Images {
    pub(crate) fn new() -> Self {
        let (tx, rx) = channel();
        Images {
            gfx: None,
            entries: HashMap::new(),
            placed: HashMap::new(),
            next_id: ID_BASE,
            wipes: WIPES.load(Ordering::SeqCst),
            tx,
            rx,
            background: true,
        }
    }

    /// The terminal's graphics, when it draws them.
    pub(crate) fn graphics(&self) -> Option<Graphics> {
        self.gfx
    }

    /// The probe found kitty graphics (or the cell size changed with a resize).
    pub(crate) fn enable(&mut self, gfx: Graphics) {
        self.gfx = Some(gfx);
    }

    /// Read the cell size again (after a resize or a font change), when
    /// images are on.
    pub(crate) fn refresh_cell_size(&mut self) {
        if self.gfx.is_some() {
            self.gfx = Some(Graphics::query());
        }
    }

    /// The screen was cleared (a full repaint): place everything again.
    pub(crate) fn forget_placements(&mut self) {
        self.placed.clear();
    }

    /// Take in finished decodes. `(landed, failed)`: whether any image
    /// arrived (so the frame should be drawn again), and whether any failed
    /// (so the pages should lay out again, a placeholder in its place).
    pub(crate) fn poll(&mut self) -> (bool, bool) {
        let (mut landed, mut failed) = (false, false);
        while let Ok((key, encoded)) = self.rx.try_recv() {
            landed = true;
            let entry = match encoded {
                Some(image) => {
                    let id = self.next_id;
                    self.next_id = self.next_id.wrapping_add(1).max(ID_BASE);
                    Entry::Ready { id, image: Arc::new(image), sent: false }
                }
                None => {
                    failed = true;
                    if let Ok(mut set) = UNDRAWABLE.get_or_init(Default::default).lock() {
                        set.insert(key.path.clone());
                    }
                    Entry::Failed
                }
            };
            self.entries.insert(key, entry);
        }
        (landed, failed)
    }

    /// The escapes that bring the screen to `wanted`: decodes started for
    /// new images, images sent once, placements put up or moved, and the
    /// ones no longer wanted deleted. Empty when nothing changed.
    pub(crate) fn frame(&mut self, wanted: &[Placement]) -> Vec<u8> {
        let mut out = Vec::new();
        let wipes = WIPES.load(Ordering::SeqCst);
        if wipes != self.wipes {
            self.wipes = wipes;
            self.placed.clear();
            for e in self.entries.values_mut() {
                if let Entry::Ready { sent, .. } = e {
                    *sent = false;
                }
            }
        }
        let gfx = self.gfx.unwrap_or(Graphics { cell: DEFAULT_CELL });
        if self.entries.len() > CACHE_LIMIT {
            let keep: Vec<Key> = wanted.iter().map(|p| Key { path: p.path.clone(), cols: p.cols, rows: p.rows }).collect();
            self.entries.retain(|k, _| keep.contains(k));
        }
        let mut now: HashMap<(u32, u32), Placed> = HashMap::new();
        let mut uses: HashMap<u32, u32> = HashMap::new();
        for p in wanted {
            let key = Key { path: p.path.clone(), cols: p.cols, rows: p.rows };
            if !self.entries.contains_key(&key) {
                self.entries.insert(key.clone(), Entry::Loading);
                self.load(key.clone(), gfx);
            }
            let Some(Entry::Ready { id, image, sent }) = self.entries.get_mut(&key) else {
                continue;
            };
            if !*sent {
                transmit(&mut out, *id, &image.png);
                *sent = true;
                ACTIVE.store(true, Ordering::SeqCst);
            }
            let pid = uses.entry(*id).or_insert(0);
            *pid += 1;
            let placed = Placed::of(p, image.px.1);
            if self.placed.get(&(*id, *pid)) != Some(&placed) {
                put(&mut out, *id, *pid, &placed, image.px.0);
            }
            now.insert((*id, *pid), placed);
        }
        let mut gone: Vec<(u32, u32)> = self.placed.keys().filter(|k| !now.contains_key(k)).copied().collect();
        gone.sort_unstable();
        for (id, pid) in gone {
            delete(&mut out, id, pid);
        }
        self.placed = now;
        out
    }

    /// Decode, scale and encode `key`'s image for cells of `gfx.cell`, off
    /// the UI thread; the result arrives through [`Images::poll`].
    fn load(&self, key: Key, gfx: Graphics) {
        let tx = self.tx.clone();
        let target = (
            u32::from(key.cols) * u32::from(gfx.cell.0),
            u32::from(key.rows) * u32::from(gfx.cell.1),
        );
        let job = move || {
            let encoded = encode(&key.path, target);
            let _ = tx.send((key, encoded));
        };
        if self.background {
            std::thread::spawn(job);
        } else {
            job();
        }
    }

    #[cfg(test)]
    pub(crate) fn inline_for_tests() -> Self {
        Images { background: false, ..Images::new() }
    }
}

/// The image at `path` scaled down to `target` pixels (never up) and encoded
/// as base64 PNG. GIFs give their first frame.
fn encode(path: &Path, target: (u32, u32)) -> Option<Encoded> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let img = if target.0 > 0 && target.1 > 0 && (img.width() > target.0 || img.height() > target.1) {
        img.resize_exact(target.0.min(img.width()), target.1.min(img.height()), image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img.to_rgba8())
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(Encoded { png: base64(&png), px: (img.width(), img.height()) })
}

/// Standard base64 with padding.
pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[((n >> 18) & 63) as usize] as char);
        s.push(T[((n >> 12) & 63) as usize] as char);
        s.push(if c.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        s.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

#[cfg(test)]
#[path = "kitty_tests.rs"]
mod tests;
