//! Embedded assets: the icons the app draws.
//!
//! **Every icon in Shelbi's own UI is a Heroicon** (MIT, Tailwind Labs,
//! vendored under `assets/icons/heroicons/` with its LICENSE, from heroicons
//! v2.2.0). Don't reach for gpui-kit's bundled icon catalog in Shelbi's own
//! views; it is only the fallback that keeps gpui-kit components' internal
//! glyphs (a checkbox tick, a dropdown caret) resolving.
//!
//! **An icon renders at exactly one of three sizes**, each drawn from the
//! Heroicons set made for that grid, so an icon is never a scaled copy of a
//! larger drawing:
//!
//! | [`IconSize`] | Pixels | Heroicons set     | Use it for |
//! |--------------|--------|-------------------|------------|
//! | `Standard`   | 24×24  | `24/outline/`     | toolbar and header buttons, the sidebar's view icons, the bootstrap/update screens' status icons |
//! | `Mini`       | 20×20  | `20/solid/`       | inline icons in rows, chips, buttons with a text label, the terminal search bar |
//! | `Micro`      | 16×16  | `16/solid/`       | icons set inside dense text: card badges, activity/machine row markers, link arrows |
//!
//! Render one with [`heroicon`]`(icons::X_MARK, IconSize::Mini)`; the SVG's
//! `currentColor` takes the element's `text_color`. No view calls
//! `svg()` or sets an icon's pixel size itself (a test guards it); pick the
//! size by the icon's context, and grow the row around it rather than
//! shrinking an icon below 16.
//!
//! To add an icon, copy its SVG from all three sets at the v2.2.0 tag
//! (`optimized/24/outline/`, `optimized/20/solid/`, `optimized/16/solid/`)
//! into `assets/icons/heroicons/{24,20,16}/`, then add a row to the
//! `heroicons!` list below.

use std::borrow::Cow;

use gpui::{px, svg, AssetSource, Pixels, Result, SharedString, Styled, Svg};

/// The three sizes an icon renders at. See the module docs for which to use
/// where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSize {
    /// 24×24, from Heroicons' 24px outline set.
    Standard,
    /// 20×20, from Heroicons' 20px solid ("mini") set.
    Mini,
    /// 16×16, from Heroicons' 16px solid ("micro") set.
    Micro,
}

impl IconSize {
    #[cfg(test)]
    pub const ALL: [IconSize; 3] = [IconSize::Standard, IconSize::Mini, IconSize::Micro];

    /// The icon's width and height.
    pub fn px(self) -> Pixels {
        px(self.grid())
    }

    /// The Heroicons grid the size draws from, in pixels.
    const fn grid(self) -> f32 {
        match self {
            IconSize::Standard => 24.0,
            IconSize::Mini => 20.0,
            IconSize::Micro => 16.0,
        }
    }
}

/// One vendored Heroicon: the same symbol drawn once per [`IconSize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    name: &'static str,
    standard: &'static str,
    mini: &'static str,
    micro: &'static str,
}

impl Icon {
    /// The Heroicons name, e.g. `x-mark`.
    #[cfg(test)]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// The asset path of the drawing for `size`.
    pub const fn path(self, size: IconSize) -> &'static str {
        match size {
            IconSize::Standard => self.standard,
            IconSize::Mini => self.mini,
            IconSize::Micro => self.micro,
        }
    }
}

/// `icon` at `size`: the only way Shelbi's views draw an icon.
pub fn heroicon(icon: Icon, size: IconSize) -> Svg {
    svg().path(icon.path(size)).size(size.px()).flex_none()
}

macro_rules! heroicon_file {
    ($set:literal, $file:literal) => {
        (
            concat!("icons/heroicons/", $set, "/", $file, ".svg"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/assets/icons/heroicons/",
                $set,
                "/",
                $file,
                ".svg"
            )) as &[u8],
        )
    };
}

macro_rules! heroicons {
    ($($const:ident = $file:literal;)*) => {
        /// The vendored Heroicons.
        pub mod icons {
            use super::Icon;

            $(
                pub const $const: Icon = Icon {
                    name: $file,
                    standard: concat!("icons/heroicons/24/", $file, ".svg"),
                    mini: concat!("icons/heroicons/20/", $file, ".svg"),
                    micro: concat!("icons/heroicons/16/", $file, ".svg"),
                };
            )*

            /// Every vendored icon.
            #[cfg(test)]
            pub const ALL: &[Icon] = &[$($const),*];
        }

        /// Every vendored drawing, by asset path: three per icon.
        const HEROICONS: &[(&str, &[u8])] = &[$(
            heroicon_file!("24", $file),
            heroicon_file!("20", $file),
            heroicon_file!("16", $file),
        )*];
    };
}

heroicons! {
    CHAT_BUBBLE_LEFT_RIGHT = "chat-bubble-left-right";
    VIEW_COLUMNS = "view-columns";
    BOLT = "bolt";
    SERVER_STACK = "server-stack";
    X_MARK = "x-mark";
    ARROW_LEFT = "arrow-left";
    ARROW_PATH = "arrow-path";
    ARROW_TOP_RIGHT_ON_SQUARE = "arrow-top-right-on-square";
    CLIPBOARD_DOCUMENT = "clipboard-document";
    EXCLAMATION_TRIANGLE = "exclamation-triangle";
    FUNNEL = "funnel";
    LOCK_CLOSED = "lock-closed";
    PENCIL_SQUARE = "pencil-square";
    PLAY = "play";
    PLUS = "plus";
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
}

/// Geist, the UI typeface (SIL Open Font License 1.1, vendored with its
/// license under `assets/fonts/geist/`, from the `geist` npm package 1.7.2).
const GEIST: &[&[u8]] = &[
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Regular.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Medium.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-SemiBold.ttf")),
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts/geist/Geist-Bold.ttf")),
];

/// Geist Mono, the terminal typeface (SIL Open Font License 1.1, vendored
/// with its license under `assets/fonts/geist-mono/`, from the `geist` npm
/// package 1.7.2): regular, bold, and their italics, so the terminal's SGR
/// bold and italic use real faces, not synthesized ones.
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
/// the terminal can use Geist Mono and its symbol fallback on any machine.
///
/// gpui keeps added fonts in its own (font-kit) memory source, which resolves
/// a primary family but not a **fallback**: CoreText resolves a font's
/// cascade list by name among the fonts *it* knows. So the terminal's
/// fallbacks (Geist Mono, the Nerd symbols) are also registered with CoreText
/// for this process.
pub fn load_fonts(cx: &mut gpui::App) {
    let fonts = GEIST
        .iter()
        .chain(GEIST_MONO)
        .chain(std::iter::once(&NERD_SYMBOLS))
        .map(|bytes| Cow::Borrowed(*bytes))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        tracing::warn!(error = %e, "could not load the bundled fonts");
    }
    for bytes in GEIST_MONO.iter().chain(std::iter::once(&NERD_SYMBOLS)) {
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
        if let Some((_, bytes)) = HEROICONS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out: Vec<SharedString> = HEROICONS
            .iter()
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
    fn every_heroicon_loads_as_an_svg_in_all_three_sizes() {
        for icon in icons::ALL {
            for size in IconSize::ALL {
                let path = icon.path(size);
                assert_eq!(
                    path,
                    format!("icons/heroicons/{}/{}.svg", size.grid() as u32, icon.name())
                );
                let bytes = Assets.load(path).unwrap().expect(path);
                let svg = std::str::from_utf8(&bytes).expect(path);
                assert!(svg.starts_with("<svg"), "{path} is an SVG");
                // Drawn on its own grid, from the set made for that size:
                // outline at 24, solid at 20 and 16.
                let grid = size.grid() as u32;
                assert!(
                    svg.contains(&format!(r#"viewBox="0 0 {grid} {grid}""#)),
                    "{path} is drawn on a {grid}px grid"
                );
                let outline = svg.contains(r#"fill="none""#) && svg.contains("stroke=");
                assert_eq!(outline, size == IconSize::Standard, "{path}: outline only at 24px");
            }
        }
    }

    #[test]
    fn heroicons_are_listed() {
        let listed = Assets.list("icons/heroicons/").unwrap();
        assert_eq!(listed.len(), HEROICONS.len());
        assert_eq!(HEROICONS.len(), icons::ALL.len() * IconSize::ALL.len());
    }

    #[test]
    fn icon_sizes_are_24_20_and_16_pixels() {
        assert_eq!(IconSize::Standard.px(), px(24.0));
        assert_eq!(IconSize::Mini.px(), px(20.0));
        assert_eq!(IconSize::Micro.px(), px(16.0));
    }

    /// No view draws an SVG or sizes an icon itself: every icon goes through
    /// [`heroicon`], so it renders at one of the three sizes, from the set
    /// drawn for it.
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
            "draw icons with assets::heroicon(icon, IconSize::…), not svg():\n{}",
            offenders.join("\n")
        );
    }
}
