//! Embedded assets: the icons the app draws.
//!
//! **Every icon in Shelbi's own UI is a Heroicon** (MIT, Tailwind Labs,
//! vendored under `assets/icons/heroicons/` with its LICENSE, from heroicons
//! v2.2.0). Don't reach for gpui-kit's bundled icon catalog in Shelbi's own
//! views; it is only the fallback that keeps gpui-kit components' internal
//! glyphs (a checkbox tick, a dropdown caret) resolving.
//!
//! **Every icon is the Heroicons outline drawing at 20×20** ([`ICON_SIZE`])
//! unless the Figma file draws it at 14 ([`ICON_SIZE_SMALL`]) or 18
//! ([`ICON_SIZE_BUTTON`]; see below):
//! toolbar and back buttons, the sidebar, panel rows, chips, card badges,
//! activity and machine rows, the terminal search bar, the bootstrap and
//! update screens. One look everywhere, no per-view sizing. Heroicons has no
//! 20px outline set, so this is the `24/outline` drawing (a 1.5px stroke on a
//! 24 grid) scaled to 20px, a 1.25px stroke: how the outline set reads at
//! mini size on the web.
//!
//! Render one with [`heroicon`]`(icons::X_MARK)`; the SVG's `currentColor`
//! stroke takes the element's `text_color`. An icon carries its size, so no
//! view calls `svg()` or sizes an icon itself (a test guards it, and another
//! that every icon draws at one of the Figma file's sizes, 20, 14, 18 or 16). Lay a row out around the 20px icon; an
//! icon button that needs a bigger hit area pads the icon rather than
//! growing it.
//!
//! To add an icon, copy its SVG from `optimized/24/outline/` at the v2.2.0
//! tag into `assets/icons/heroicons/`, then add a row to the `heroicons!`
//! list below.
//!
//! One exception: a few drawings John made in the Figma file where
//! Heroicons has none (the palette's Zen toggle, error log and finish
//! flag, the sidebar's toggle). They live under `assets/icons/figma/` (see its README), on the
//! Figma's 20pt grid, and render through the same [`heroicon`] helper.
//!
//! Where the Figma file draws an icon at **14pt** (the Activity pickers'
//! funnel and user), the `@small` list names it: the same Heroicons drawing
//! under `assets/icons/heroicons-14/` with its stroke widened so it is 1.5pt
//! at 14pt, as Figma renders it (see that folder's README). Its constant
//! ends in `_SMALL` (`icons::FUNNEL_SMALL`) and [`heroicon`] draws it at
//! [`ICON_SIZE_SMALL`].
//!
//! Where the Figma file draws an icon at **18pt** (the X-Small Button's left
//! icon: the issue detail's actions), the `@button` list names it the same
//! way: the drawing under `assets/icons/heroicons-18/` with its stroke
//! widened so it is 1.25pt at 18pt, as Figma draws it (see that folder's
//! README). Its constant ends in `_BUTTON` (`icons::USER_BUTTON`) and
//! [`heroicon`] draws it at [`ICON_SIZE_BUTTON`]. The Start button's filled
//! play is among them (John's drawing, on Figma's 18pt grid, since Heroicons
//! has none).
//!
//! Where the Figma file draws an icon at **16pt** (the issue detail's close
//! mark), the `@close` list names it: the drawing under
//! `assets/icons/heroicons-16/` with its stroke widened so it is 1.5pt at
//! 16pt. Its constant ends in `_CLOSE` and [`heroicon`] draws it at
//! [`ICON_SIZE_CLOSE`].
//!
//! The other exception is the **status glyphs** ([`status_glyph`]): the Figma
//! file's own `status-*` drawings (a dashed circle for backlog, an open one
//! for to do, a circle filling up through in progress and review to a solid
//! done), 16×16 on a 16 grid, vendored under `assets/icons/status/` in
//! `currentColor`. Heroicons has no status set. Views draw them through
//! `theme::style::status_icon`, which tints each glyph.
//!
//! And the sidebar's 12pt [`glyphs`] (the Navigation component's state
//! glyphs for working, awaiting input, idle and ready for review, and its
//! section chevron), under `assets/icons/glyphs/`: drawn at Figma's 12pt
//! with [`glyph`], since [`heroicon`] draws only at 20pt and 14pt.
//!
//! The last is the **checkbox mark** ([`check_mark`]): Settings' checked
//! box draws the Figma file's `Icon / check` (node 2169:675), the Heroicons
//! check at 12pt with a 1.5pt stroke, vendored as Figma exports it under
//! `assets/icons/checkbox/` in `currentColor`.

use std::borrow::Cow;

use gpui::{px, svg, AssetSource, Pixels, Result, SharedString, Styled, Svg};

/// The width and height an icon renders at, unless it is a small one.
pub const ICON_SIZE: Pixels = px(20.0);

/// The size the Figma file draws its small icons at (the Activity pickers).
pub const ICON_SIZE_SMALL: Pixels = px(14.0);

/// The X-Small Button's left icon size (the issue detail's actions).
pub const ICON_SIZE_BUTTON: Pixels = px(18.0);

/// A modal's close mark (the issue detail's).
pub const ICON_SIZE_CLOSE: Pixels = px(16.0);

/// Every size an icon renders at: the sizes the Figma file uses.
#[cfg(test)]
pub const ICON_SIZES: [Pixels; 4] = [ICON_SIZE, ICON_SIZE_SMALL, ICON_SIZE_BUTTON, ICON_SIZE_CLOSE];

/// One vendored Heroicon (its outline drawing) and the size it draws at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    name: &'static str,
    path: &'static str,
    size: Pixels,
}

impl Icon {
    /// The Heroicons name, e.g. `x-mark`.
    #[cfg(test)]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The drawing's asset path.
    pub const fn path(self) -> &'static str {
        self.path
    }

    /// The width and height it draws at.
    pub const fn size(self) -> Pixels {
        self.size
    }
}

/// `icon`, outline, at its size ([`ICON_SIZE`], [`ICON_SIZE_SMALL`] for a
/// small one, [`ICON_SIZE_BUTTON`] for a button's, [`ICON_SIZE_CLOSE`] for
/// a close mark): the only way Shelbi's
/// views draw an icon.
pub fn heroicon(icon: Icon) -> Svg {
    svg().path(icon.path()).size(icon.size).flex_none()
}

/// The width and height a status glyph renders at.
pub const STATUS_GLYPH_SIZE: Pixels = px(16.0);

/// A workflow status's glyph, as the Figma file draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusGlyph {
    /// `status-backlog`: a dashed circle.
    Backlog,
    /// `status-todo`: an open circle.
    Todo,
    /// `status-2-of-4`: the circle half filled.
    InProgress,
    /// `status-3-of-4`: three quarters filled.
    Review,
    /// `status-complete`: filled.
    Done,
    /// Not drawn in Figma: the open circle struck through.
    Canceled,
}

impl StatusGlyph {
    /// Every glyph.
    #[cfg(test)]
    pub const ALL: [StatusGlyph; 6] = [
        StatusGlyph::Backlog,
        StatusGlyph::Todo,
        StatusGlyph::InProgress,
        StatusGlyph::Review,
        StatusGlyph::Done,
        StatusGlyph::Canceled,
    ];

    /// The drawing's asset path.
    pub const fn path(self) -> &'static str {
        match self {
            StatusGlyph::Backlog => "icons/status/backlog.svg",
            StatusGlyph::Todo => "icons/status/todo.svg",
            StatusGlyph::InProgress => "icons/status/in-progress.svg",
            StatusGlyph::Review => "icons/status/review.svg",
            StatusGlyph::Done => "icons/status/done.svg",
            StatusGlyph::Canceled => "icons/status/canceled.svg",
        }
    }
}

/// `glyph` at [`STATUS_GLYPH_SIZE`], in the element's `text_color`.
pub fn status_glyph(glyph: StatusGlyph) -> Svg {
    svg().path(glyph.path()).size(STATUS_GLYPH_SIZE).flex_none()
}

/// The size Settings ▸ Workflows draws a status's glyph at (its `Status
/// icon`, 2292:1159, the drawing scaled to its 20pt frame).
pub const STATUS_GLYPH_LARGE_SIZE: Pixels = px(20.0);

/// `glyph` at [`STATUS_GLYPH_LARGE_SIZE`].
pub fn status_glyph_large(glyph: StatusGlyph) -> Svg {
    svg().path(glyph.path()).size(STATUS_GLYPH_LARGE_SIZE).flex_none()
}

macro_rules! status_glyphs {
    ($($file:literal),*) => {
        /// The status glyphs, by asset path.
        const STATUS_GLYPHS: &[(&str, &[u8])] = &[$(
            (
                concat!("icons/status/", $file, ".svg"),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/status/",
                    $file,
                    ".svg"
                )) as &[u8],
            ),
        )*];
    };
}

status_glyphs!("backlog", "todo", "in-progress", "review", "done", "canceled");

/// The width and height the checkbox mark renders at.
pub const CHECK_MARK_SIZE: Pixels = px(12.0);

/// The checkbox mark's drawing, by asset path.
const CHECK_MARK: (&str, &[u8]) = (
    "icons/checkbox/check.svg",
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/checkbox/check.svg")),
);

/// A checked box's mark at [`CHECK_MARK_SIZE`], in the element's
/// `text_color`.
pub fn check_mark() -> Svg {
    svg().path(CHECK_MARK.0).size(CHECK_MARK_SIZE).flex_none()
}

macro_rules! heroicons {
    (
        $($(#[$meta:meta])* $const:ident = $file:literal;)*
        @figma { $($fconst:ident = $ffile:literal;)* }
        @small { $($sconst:ident = $sfile:literal;)* }
        @button { $($bconst:ident = $bfile:literal;)* }
        @close { $($cconst:ident = $cfile:literal;)* }
    ) => {
        /// The vendored icons.
        pub mod icons {
            use super::{Icon, ICON_SIZE, ICON_SIZE_BUTTON, ICON_SIZE_CLOSE, ICON_SIZE_SMALL};

            $(
                $(#[$meta])*
                pub const $const: Icon = Icon {
                    name: $file,
                    path: concat!("icons/heroicons/", $file, ".svg"),
                    size: ICON_SIZE,
                };
            )*
            $(
                pub const $fconst: Icon = Icon {
                    name: $ffile,
                    path: concat!("icons/figma/", $ffile, ".svg"),
                    size: ICON_SIZE,
                };
            )*
            $(
                pub const $sconst: Icon = Icon {
                    name: $sfile,
                    path: concat!("icons/heroicons-14/", $sfile, ".svg"),
                    size: ICON_SIZE_SMALL,
                };
            )*
            $(
                pub const $bconst: Icon = Icon {
                    name: $bfile,
                    path: concat!("icons/heroicons-18/", $bfile, ".svg"),
                    size: ICON_SIZE_BUTTON,
                };
            )*
            $(
                pub const $cconst: Icon = Icon {
                    name: $cfile,
                    path: concat!("icons/heroicons-16/", $cfile, ".svg"),
                    size: ICON_SIZE_CLOSE,
                };
            )*

            /// Every vendored icon.
            #[cfg(test)]
            pub const ALL: &[Icon] = &[$($const,)* $($fconst,)* $($sconst,)* $($bconst,)* $($cconst,)*];
        }

        /// Every vendored drawing, by asset path.
        const HEROICONS: &[(&str, &[u8])] = &[
            $((
                concat!("icons/heroicons/", $file, ".svg"),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/heroicons/",
                    $file,
                    ".svg"
                )) as &[u8],
            ),)*
            $((
                concat!("icons/figma/", $ffile, ".svg"),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/figma/",
                    $ffile,
                    ".svg"
                )) as &[u8],
            ),)*
            $((
                concat!("icons/heroicons-14/", $sfile, ".svg"),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/heroicons-14/",
                    $sfile,
                    ".svg"
                )) as &[u8],
            ),)*
            $((
                concat!("icons/heroicons-18/", $bfile, ".svg"),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/heroicons-18/",
                    $bfile,
                    ".svg"
                )) as &[u8],
            ),)*
            $((
                concat!("icons/heroicons-16/", $cfile, ".svg"),
                include_bytes!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/assets/icons/heroicons-16/",
                    $cfile,
                    ".svg"
                )) as &[u8],
            ),)*
        ];
    };
}

heroicons! {
    BOLT = "bolt";
    SERVER_STACK = "server-stack";
    X_MARK = "x-mark";
    ARROW_LEFT = "arrow-left";
    /// The drawing `ARROW_RIGHT_BUTTON` widens; the Add Project wizard's
    /// workflow cards draw it at 12pt between statuses.
    ARROW_RIGHT = "arrow-right";
    ARROW_PATH = "arrow-path";
    ARROW_UTURN_LEFT = "arrow-uturn-left";
    ARROW_TOP_RIGHT_ON_SQUARE = "arrow-top-right-on-square";
    /// The drawing `ARROW_UP_RIGHT_BUTTON` widens; no view draws it at 20pt.
    #[allow(dead_code)]
    ARROW_UP_RIGHT = "arrow-up-right";
    CLIPBOARD_DOCUMENT = "clipboard-document";
    EXCLAMATION_TRIANGLE = "exclamation-triangle";
    /// The drawing `FUNNEL_SMALL` widens; no view draws the funnel at 20pt.
    #[allow(dead_code)]
    FUNNEL = "funnel";
    LOCK_CLOSED = "lock-closed";
    PENCIL_SQUARE = "pencil-square";
    PLUS = "plus";
    MINUS = "minus";
    /// The drawing `USER_SMALL` and `USER_BUTTON` widen; no view draws the
    /// user at 20pt.
    #[allow(dead_code)]
    USER = "user";
    MAGNIFYING_GLASS = "magnifying-glass";
    COG_6_TOOTH = "cog-6-tooth";
    CHEVRON_UP_DOWN = "chevron-up-down";
    CHECK = "check";
    SIGNAL_SLASH = "signal-slash";
    ARROW_DOWN_CIRCLE = "arrow-down-circle";
    COMMAND_LINE = "command-line";
    FOLDER = "folder";
    GLOBE_ALT = "globe-alt";
    ARROW_DOWN = "arrow-down";
    CHEVRON_UP = "chevron-up";
    CHEVRON_DOWN = "chevron-down";
    /// The agent editor's Workflow assignments rows (they open the
    /// workflow).
    CHEVRON_RIGHT = "chevron-right";
    ARROWS_RIGHT_LEFT = "arrows-right-left";
    CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS = "chat-bubble-oval-left-ellipsis";
    RECTANGLE_STACK = "rectangle-stack";
    WRENCH_SCREWDRIVER = "wrench-screwdriver";
    VIEW_COLUMNS = "view-columns";
    BARS_2 = "bars-2";
    DOCUMENT_TEXT = "document-text";
    /// The drawing `TRASH_BUTTON` widens; no view draws it at 20pt.
    #[allow(dead_code)]
    TRASH = "trash";
    /// The drawing `ELLIPSIS_VERTICAL_BUTTON` widens; no view draws it at
    /// 20pt.
    #[allow(dead_code)]
    ELLIPSIS_VERTICAL = "ellipsis-vertical";
    /// Settings ▸ Agents' Edit instructions (`PENCIL_BUTTON` is its
    /// widened drawing for the 24pt row buttons).
    PENCIL = "pencil";
    @figma {
        TOGGLE_OFF = "toggle-off";
        TOGGLE_ON = "toggle-on";
        PAGE_CURL = "page-curl";
        FLAG_FINISH = "flag-finish";
        SIDEBAR = "sidebar";
        KEYBOARD_KEY = "keyboard-key";
    }
    @small {
        FUNNEL_SMALL = "funnel";
        USER_SMALL = "user";
        COMMAND_LINE_SMALL = "command-line";
    }
    @button {
        ARROW_LEFT_BUTTON = "arrow-left";
        ARROW_RIGHT_BUTTON = "arrow-right";
        PLAY_SOLID_BUTTON = "play-solid";
        USER_BUTTON = "user";
        PENCIL_SQUARE_BUTTON = "pencil-square";
        ARROW_UP_RIGHT_BUTTON = "arrow-up-right";
        TRASH_BUTTON = "trash";
        ELLIPSIS_VERTICAL_BUTTON = "ellipsis-vertical";
        PENCIL_BUTTON = "pencil";
        PLUS_BUTTON = "plus";
        X_MARK_BUTTON = "x-mark";
    }
    @close {
        X_MARK_CLOSE = "x-mark";
    }
}

/// One of the sidebar's Figma glyphs: its drawing and the size it renders
/// at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    name: &'static str,
    size: u16,
}

impl Glyph {
    #[cfg(test)]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The width and height it renders at.
    pub fn size(self) -> Pixels {
        px(f32::from(self.size))
    }

    fn path(self) -> String {
        format!("icons/glyphs/{}.svg", self.name)
    }
}

/// `glyph` at its Figma size, in the element's `text_color` (a one-color
/// drawing, like a Heroicon).
pub fn glyph(glyph: Glyph) -> Svg {
    svg().path(glyph.path()).size(glyph.size()).flex_none()
}

/// The sidebar's 12pt Figma glyphs (see the module docs), and the workflow
/// editor's 10pt Discard mark.
pub mod glyphs {
    use super::Glyph;

    /// A workspace whose agent is working (green).
    pub const PLAY: Glyph = Glyph { name: "play", size: 12 };
    /// A review being served (cyan).
    pub const CHECK: Glyph = Glyph { name: "check", size: 12 };
    /// An idle workspace, a queued review.
    pub const DOT: Glyph = Glyph { name: "dot", size: 12 };
    /// A workspace awaiting input: a yellow disc with the `?` cut out, over
    /// a dark disc so the mark reads on either theme.
    pub const QUESTION: Glyph = Glyph { name: "question", size: 12 };
    /// A section header's collapse chevron.
    pub const CHEVRON_DOWN: Glyph = Glyph { name: "chevron-down", size: 12 };
    /// The workflow editor's Discard changes link (the Header's `Discard
    /// Link`, 2342:39062): John's 10pt drawing, a 1pt stroke.
    pub const DISCARD: Glyph = Glyph { name: "discard", size: 10 };

    #[cfg(test)]
    pub const ALL: &[Glyph] = &[PLAY, CHECK, DOT, QUESTION, CHEVRON_DOWN, DISCARD];
}

/// Every glyph drawing, by asset path.
const GLYPHS: &[(&str, &[u8])] = &[
    ("icons/glyphs/play.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/glyphs/play.svg"))),
    ("icons/glyphs/check.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/glyphs/check.svg"))),
    ("icons/glyphs/dot.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/glyphs/dot.svg"))),
    ("icons/glyphs/question.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/glyphs/question.svg"))),
    ("icons/glyphs/chevron-down.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/glyphs/chevron-down.svg"))),
    ("icons/glyphs/discard.svg", include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/glyphs/discard.svg"))),
];

/// The Heroicons license, for Acknowledgements (`crate::licenses`): the
/// vendored set's own `LICENSE`, kept here with the rest of its paths.
pub const HEROICONS_LICENSE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icons/heroicons/LICENSE"));

/// Geist, the UI typeface (SIL Open Font License 1.1, vendored with its
/// license under `assets/fonts/geist/`, from the `geist` npm package 1.7.2).
const GEIST: &[&[u8]] = &[
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Regular.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Medium.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-SemiBold.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Bold.ttf")),
];

/// Source Code Pro, the terminal typeface (SIL Open Font License 1.1,
/// vendored with its license under `assets/fonts/source-code-pro/`, from
/// Adobe's release branch): regular, bold, and their italics, so the
/// terminal's SGR bold and italic use real faces, not synthesized ones.
const SOURCE_CODE_PRO: &[&[u8]] = &[
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/source-code-pro/SourceCodePro-Regular.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/source-code-pro/SourceCodePro-Bold.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/source-code-pro/SourceCodePro-It.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/source-code-pro/SourceCodePro-BoldIt.ttf")),
];

/// Geist Mono, the terminal's first fallback (SIL Open Font License 1.1,
/// vendored with its license under `assets/fonts/geist-mono/`, from the
/// `geist` npm package 1.7.2): regular, bold, and their italics.
const GEIST_MONO: &[&[u8]] = &[
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist-mono/GeistMono-Regular.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist-mono/GeistMono-Bold.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist-mono/GeistMono-Italic.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist-mono/GeistMono-BoldItalic.ttf")),
];

/// Symbols Nerd Font Mono (Nerd Fonts v3.4.0, MIT for the build, the glyph
/// sets under their own licenses; see `assets/fonts/nerd-symbols/`): the
/// terminal's fallback for the private-use icons prompts and agents print,
/// which no system font covers. CJK and emoji fall back through the system
/// (CoreText's cascade).
const NERD_SYMBOLS: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/fonts/nerd-symbols/SymbolsNerdFontMono-Regular.ttf"
));

/// Register the vendored fonts, so views can set `font_family("Geist")` and
/// the terminal can use Source Code Pro, Geist Mono and the symbol fallback
/// on any machine.
///
/// gpui keeps added fonts in its own (font-kit) memory source, which resolves
/// a primary family but not a **fallback**: CoreText resolves a font's
/// cascade list by name among the fonts *it* knows. So the terminal faces
/// (Source Code Pro is a fallback behind a configured family, then Geist
/// Mono, the Nerd symbols) are also registered with CoreText for this
/// process.
pub fn load_fonts(cx: &mut gpui::App) {
    let fonts = GEIST
        .iter()
        .chain(SOURCE_CODE_PRO)
        .chain(GEIST_MONO)
        .chain(std::iter::once(&NERD_SYMBOLS))
        .map(|bytes| Cow::Borrowed(*bytes))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::warn!(error = %e, "could not load the bundled fonts");
    }
    for bytes in SOURCE_CODE_PRO
        .iter()
        .chain(GEIST_MONO)
        .chain(std::iter::once(&NERD_SYMBOLS))
    {
        if let Err(e) = coretext::register_font(bytes) {
            tracing::warn!(error = %e, "could not register a bundled font for fallback");
        }
    }
}

/// Process-scoped CoreText registration of embedded font data.
#[cfg(target_os = "macos")]
mod coretext {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const c_void,
            size: usize,
            release: Option<unsafe extern "C" fn(*mut c_void, *const c_void, usize)>,
        ) -> CFTypeRef;
        fn CGFontCreateWithDataProvider(provider: CFTypeRef) -> CFTypeRef;
    }

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerRegisterGraphicsFont(font: CFTypeRef, error: *mut CFTypeRef) -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
    }

    /// Register `bytes` (a whole font file, alive for the process) with
    /// CoreText for this process, so a cascade list can name its family.
    pub fn register_font(bytes: &'static [u8]) -> Result<(), String> {
        // SAFETY: `bytes` is 'static, so the provider (which doesn't copy and
        // has no release callback) never outlives it; each created CF object
        // is released once; a failed registration's error is released too.
        unsafe {
            let provider = CGDataProviderCreateWithData(
                std::ptr::null_mut(),
                bytes.as_ptr().cast(),
                bytes.len(),
                None,
            );
            if provider.is_null() {
                return Err("CGDataProviderCreateWithData failed".into());
            }
            let font = CGFontCreateWithDataProvider(provider);
            CFRelease(provider);
            if font.is_null() {
                return Err("not a font CoreGraphics can read".into());
            }
            let mut error: CFTypeRef = std::ptr::null();
            let ok = CTFontManagerRegisterGraphicsFont(font, &mut error);
            CFRelease(font);
            if !error.is_null() {
                CFRelease(error);
            }
            // A font already registered (a second window's init) is fine.
            let _ = ok;
            Ok(())
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod coretext {
    pub fn register_font(_bytes: &'static [u8]) -> Result<(), String> {
        Ok(())
    }
}

/// The app's asset source: the Heroicons first, then gpui-kit's assets.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = HEROICONS
            .iter()
            .chain(STATUS_GLYPHS)
            .chain([&CHECK_MARK])
            .chain(GLYPHS)
            .find(|(p, _)| *p == path)
        {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = HEROICONS
            .iter()
            .chain(STATUS_GLYPHS)
            .chain([&CHECK_MARK])
            .chain(GLYPHS)
            .filter(|(p, _)| p.starts_with(path))
            .map(|(p, _)| SharedString::from(*p))
            .collect();
        out.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_heroicon_loads_as_an_outline_drawing_on_a_24_grid() {
        for icon in icons::ALL {
            let path = icon.path();
            let bytes = Assets.load(path).unwrap().expect(path);
            let svg = std::str::from_utf8(&bytes).expect(path);
            assert!(svg.starts_with("<svg"), "{path} is an SVG");
            if path == format!("icons/figma/{}.svg", icon.name()) {
                assert!(svg.contains(r#"viewBox="0 0 20 20""#), "{path} is on the Figma's 20pt grid");
                assert!(svg.contains("currentColor"), "{path} paints in currentColor");
                continue;
            }
            if path == format!("icons/heroicons-14/{}.svg", icon.name()) {
                // The 20pt drawing with only its stroke widened, 1.5pt at 14.
                let full = Assets
                    .load(&format!("icons/heroicons/{}.svg", icon.name()))
                    .unwrap()
                    .expect("a small icon's 24/outline drawing is vendored too");
                let full = std::str::from_utf8(&full).unwrap();
                assert_eq!(
                    svg,
                    full.replace(r#"stroke-width="1.5""#, r#"stroke-width="2.5714""#),
                    "{path} is the outline drawing, stroke widened to 1.5pt at 14pt"
                );
                continue;
            }
            if path == "icons/heroicons-18/play-solid.svg" {
                // John's own drawing (Heroicons has no filled play), as the
                // Figma file draws it on its 18pt grid.
                assert!(svg.contains(r#"viewBox="0 0 18 18""#), "{path} is on Figma's 18pt grid");
                assert!(svg.contains(r#"fill="currentColor""#), "{path} paints in currentColor");
                continue;
            }
            if path == format!("icons/heroicons-16/{}.svg", icon.name()) {
                // The 20pt drawing with only its stroke widened, 1.5pt at 16.
                let full = Assets
                    .load(&format!("icons/heroicons/{}.svg", icon.name()))
                    .unwrap()
                    .expect("a close mark's 24/outline drawing is vendored too");
                let full = std::str::from_utf8(&full).unwrap();
                assert_eq!(
                    svg,
                    full.replace(r#"stroke-width="1.5""#, r#"stroke-width="2.25""#),
                    "{path} is the outline drawing, stroke widened to 1.5pt at 16pt"
                );
                continue;
            }
            if path == format!("icons/heroicons-18/{}.svg", icon.name()) {
                // The 20pt drawing with only its stroke widened, 1.25pt at 18.
                let full = Assets
                    .load(&format!("icons/heroicons/{}.svg", icon.name()))
                    .unwrap()
                    .expect("a button icon's 24/outline drawing is vendored too");
                let full = std::str::from_utf8(&full).unwrap();
                assert_eq!(
                    svg,
                    full.replace(r#"stroke-width="1.5""#, r#"stroke-width="1.6667""#),
                    "{path} is the outline drawing, stroke widened to 1.25pt at 18pt"
                );
                continue;
            }
            assert_eq!(path, format!("icons/heroicons/{}.svg", icon.name()));
            assert!(svg.contains(r#"viewBox="0 0 24 24""#), "{path} is drawn on the 24px grid");
            assert!(
                svg.contains(r#"fill="none""#) && svg.contains(r#"stroke="currentColor""#),
                "{path} is the outline drawing, stroked in currentColor"
            );
        }
    }

    #[test]
    fn every_status_glyph_loads_on_a_16_grid_in_current_color() {
        assert_eq!(STATUS_GLYPHS.len(), StatusGlyph::ALL.len());
        assert_eq!(Assets.list("icons/status/").unwrap().len(), STATUS_GLYPHS.len());
        for glyph in StatusGlyph::ALL {
            let path = glyph.path();
            let bytes = Assets.load(path).unwrap().expect(path);
            let svg = std::str::from_utf8(&bytes).expect(path);
            assert!(svg.starts_with("<svg"), "{path} is an SVG");
            assert!(svg.contains(r#"viewBox="0 0 16 16""#), "{path} is drawn on the 16px grid");
            assert!(svg.contains("currentColor"), "{path} takes the element's color");
            let mut el = status_glyph(glyph);
            assert_eq!(el.style().size.width, Some(STATUS_GLYPH_SIZE.into()), "{path}");
        }
    }

    #[test]
    fn the_check_mark_loads_on_a_12_grid_in_current_color() {
        assert_eq!(Assets.list("icons/checkbox/").unwrap(), vec![SharedString::from(CHECK_MARK.0)]);
        let bytes = Assets.load(CHECK_MARK.0).unwrap().expect(CHECK_MARK.0);
        let svg = std::str::from_utf8(&bytes).unwrap();
        assert!(svg.contains(r#"viewBox="0 0 12 12""#), "drawn on Figma's 12pt grid");
        assert!(svg.contains(r#"stroke="currentColor""#), "takes the element's color");
        let mut el = check_mark();
        assert_eq!(el.style().size.width, Some(CHECK_MARK_SIZE.into()));
        assert_eq!(el.style().size.height, Some(CHECK_MARK_SIZE.into()));
    }

    #[test]
    fn every_glyph_loads_in_current_color() {
        assert_eq!(GLYPHS.len(), glyphs::ALL.len());
        for g in glyphs::ALL {
            let path = g.path();
            let bytes = Assets.load(&path).unwrap().expect(&path);
            let svg = std::str::from_utf8(&bytes).expect(&path);
            assert!(svg.starts_with("<svg"), "{path} is an SVG");
            let grid = g.size().as_f32() as u32;
            assert!(svg.contains(&format!(r#"viewBox="0 0 {grid} {grid}""#)), "{path} is drawn at its size");
            assert!(svg.contains("currentColor"), "{path} takes the element's color");
            let mut el = glyph(*g);
            assert_eq!(el.style().size.width, Some(g.size().into()), "{}", g.name());
        }
    }

    #[test]
    fn heroicons_are_listed() {
        let listed = Assets.list("icons/heroicons/").unwrap().len()
            + Assets.list("icons/figma/").unwrap().len()
            + Assets.list("icons/heroicons-14/").unwrap().len()
            + Assets.list("icons/heroicons-18/").unwrap().len()
            + Assets.list("icons/heroicons-16/").unwrap().len();
        assert_eq!(listed, HEROICONS.len());
        assert_eq!(HEROICONS.len(), icons::ALL.len());
    }

    #[test]
    fn every_icon_draws_at_a_figma_size() {
        assert_eq!(
            ICON_SIZES,
            [px(20.0), px(14.0), px(18.0), px(16.0)],
            "the sizes the Figma file draws icons at"
        );
        for icon in icons::ALL {
            let size = if icon.path().starts_with("icons/heroicons-14/") {
                ICON_SIZE_SMALL
            } else if icon.path().starts_with("icons/heroicons-18/") {
                ICON_SIZE_BUTTON
            } else if icon.path().starts_with("icons/heroicons-16/") {
                ICON_SIZE_CLOSE
            } else {
                ICON_SIZE
            };
            assert_eq!(icon.size, size, "{}", icon.path());
            let mut svg = heroicon(*icon);
            let style = svg.style();
            assert_eq!(style.size.width, Some(size.into()), "{}", icon.path());
            assert_eq!(style.size.height, Some(size.into()), "{}", icon.path());
            assert_eq!(style.flex_shrink, Some(0.0), "{} never squeezes", icon.path());
        }
    }

    /// No view draws an SVG or sizes an icon itself: every icon goes through
    /// [`heroicon`], so it renders as the outline drawing at its size.
    #[test]
    fn only_the_icon_helper_draws_icons() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let this = src.join("assets.rs");
        let mut files = vec![src];
        let mut offenders = Vec::new();
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path == this {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                if line.contains("svg()") || line.contains("icons/heroicons") || line.contains("icons/glyphs/") {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "draw icons with assets::heroicon(icon) or assets::glyph(g), not svg():\n{}",
            offenders.join("\n")
        );
    }
}
