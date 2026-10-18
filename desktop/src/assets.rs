//! Embedded assets: the icons the app draws.
//!
//! **Every icon in Shelbi's own UI is a Heroicon** (MIT, Tailwind Labs,
//! vendored under `assets/icons/heroicons/` with its LICENSE, from heroicons
//! v2.2.0). Don't reach for gpui-kit's bundled icon catalog in Shelbi's own
//! views; it is only the fallback that keeps gpui-kit components' internal
//! glyphs (a checkbox tick, a dropdown caret) resolving.
//!
//! **Every icon is the Heroicons outline drawing at 20×20** ([`ICON_SIZE`]):
//! toolbar and back buttons, the sidebar, panel rows, chips, card badges,
//! activity and machine rows, the terminal search bar, the bootstrap and
//! update screens. One look everywhere, no per-view sizing. Heroicons has no
//! 20px outline set, so this is the `24/outline` drawing (a 1.5px stroke on a
//! 24 grid) scaled to 20px, a 1.25px stroke: how the outline set reads at
//! mini size on the web.
//!
//! Render one with [`heroicon`]`(icons::X_MARK)`; the SVG's `currentColor`
//! stroke takes the element's `text_color`. No view calls `svg()` or sizes an
//! icon itself (a test guards it). Lay a row out around the 20px icon; an
//! icon button that needs a bigger hit area pads the icon rather than
//! growing it.
//!
//! To add an icon, copy its SVG from `optimized/24/outline/` at the v2.2.0
//! tag into `assets/icons/heroicons/`, then add a row to the `heroicons!`
//! list below.
//!
//! One exception: a few drawings John made in the Figma file where
//! Heroicons has none (the palette's Zen toggle, error log and finish
//! flag). They live under `assets/icons/figma/` (see its README), on the
//! Figma's 20pt grid, and render through the same [`heroicon`] helper.
//!
//! The other exception is the **status glyphs** ([`status_glyph`]): the Figma
//! file's own `status-*` drawings (a dashed circle for backlog, an open one
//! for to do, a circle filling up through in progress and review to a solid
//! done), 16×16 on a 16 grid, vendored under `assets/icons/status/` in
//! `currentColor`. Heroicons has no status set. Views draw them through
//! `theme::style::status_icon`, which tints each glyph.
//!
//! The last is the **checkbox mark** ([`check_mark`]): Settings' checked
//! box draws the Figma file's `Icon / check` (node 2169:675), the Heroicons
//! check at 12pt with a 1.5pt stroke, vendored as Figma exports it under
//! `assets/icons/checkbox/` in `currentColor`.

use std::borrow::Cow;

use gpui::{px, svg, AssetSource, Pixels, Result, SharedString, Styled, Svg};

/// The width and height every icon renders at.
pub const ICON_SIZE: Pixels = px(20.0);

/// One vendored Heroicon (its outline drawing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    name: &'static str,
    path: &'static str,
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
}

/// `icon`, outline, at [`ICON_SIZE`]: the only way Shelbi's views draw an
/// icon.
pub fn heroicon(icon: Icon) -> Svg {
    svg().path(icon.path()).size(ICON_SIZE).flex_none()
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
        $($const:ident = $file:literal;)*
        @figma { $($fconst:ident = $ffile:literal;)* }
    ) => {
        /// The vendored icons.
        pub mod icons {
            use super::Icon;

            $(
                pub const $const: Icon = Icon {
                    name: $file,
                    path: concat!("icons/heroicons/", $file, ".svg"),
                };
            )*
            $(
                pub const $fconst: Icon = Icon {
                    name: $ffile,
                    path: concat!("icons/figma/", $ffile, ".svg"),
                };
            )*

            /// Every vendored icon.
            #[cfg(test)]
            pub const ALL: &[Icon] = &[$($const,)* $($fconst,)*];
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
        ];
    };
}

heroicons! {
    CHAT_BUBBLE_LEFT_RIGHT = "chat-bubble-left-right";
    VIEW_COLUMNS = "view-columns";
    BOLT = "bolt";
    SERVER_STACK = "server-stack";
    X_MARK = "x-mark";
    ARROW_LEFT = "arrow-left";
    ARROW_RIGHT = "arrow-right";
    ARROW_PATH = "arrow-path";
    ARROW_TOP_RIGHT_ON_SQUARE = "arrow-top-right-on-square";
    CLIPBOARD_DOCUMENT = "clipboard-document";
    EXCLAMATION_TRIANGLE = "exclamation-triangle";
    FUNNEL = "funnel";
    LOCK_CLOSED = "lock-closed";
    PENCIL_SQUARE = "pencil-square";
    PLAY = "play";
    PLUS = "plus";
    MINUS = "minus";
    USER = "user";
    MAGNIFYING_GLASS = "magnifying-glass";
    COG_6_TOOTH = "cog-6-tooth";
    CHEVRON_UP_DOWN = "chevron-up-down";
    CHEVRON_DOUBLE_LEFT = "chevron-double-left";
    BARS_3 = "bars-3";
    CHECK = "check";
    SIGNAL_SLASH = "signal-slash";
    ARROW_DOWN_CIRCLE = "arrow-down-circle";
    COMMAND_LINE = "command-line";
    FOLDER_OPEN = "folder-open";
    GLOBE_ALT = "globe-alt";
    ARROW_DOWN = "arrow-down";
    CHEVRON_UP = "chevron-up";
    CHEVRON_DOWN = "chevron-down";
    ARROWS_RIGHT_LEFT = "arrows-right-left";
    SHARE = "share";
    CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS = "chat-bubble-oval-left-ellipsis";
    RECTANGLE_STACK = "rectangle-stack";
    @figma {
        TOGGLE_OFF = "toggle-off";
        TOGGLE_ON = "toggle-on";
        PAGE_CURL = "page-curl";
        FLAG_FINISH = "flag-finish";
    }
}

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
    fn heroicons_are_listed() {
        let listed = Assets.list("icons/heroicons/").unwrap().len()
            + Assets.list("icons/figma/").unwrap().len();
        assert_eq!(listed, HEROICONS.len());
        assert_eq!(HEROICONS.len(), icons::ALL.len());
    }

    #[test]
    fn every_icon_draws_at_20_pixels() {
        assert_eq!(ICON_SIZE, px(20.0));
        for icon in icons::ALL {
            let mut svg = heroicon(*icon);
            let style = svg.style();
            assert_eq!(style.size.width, Some(ICON_SIZE.into()), "{}", icon.name());
            assert_eq!(style.size.height, Some(ICON_SIZE.into()), "{}", icon.name());
            assert_eq!(style.flex_shrink, Some(0.0), "{} never squeezes", icon.name());
        }
    }

    /// No view draws an SVG or sizes an icon itself: every icon goes through
    /// [`heroicon`], so it renders as the outline drawing at 20px.
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
                if line.contains("svg()") || line.contains("icons/heroicons/") {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "draw icons with assets::heroicon(icon), not svg():\n{}",
            offenders.join("\n")
        );
    }
}
