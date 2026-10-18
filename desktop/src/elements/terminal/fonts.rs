//! The terminal fonts Settings offers: the bundled faces first, then the
//! installed monospaced families.
//!
//! The installed families come from CoreText ([`installed`]): every family
//! on the system, and which of them are fixed-pitch (`kCTFontMonoSpaceTrait`
//! on any of the family's faces). Enumerating every face takes a moment, so
//! it runs off the UI thread, once per process. Ordering and the search
//! filter ([`choices`]) are plain functions, tested without CoreText.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use super::metrics::{BUNDLED_FALLBACK_FAMILY, PRIMARY_FAMILY, SYMBOLS_FAMILY};

/// The bundled terminal faces, in the order the picker lists them: Source
/// Code Pro (the default), then Geist Mono.
pub const BUNDLED: [&str; 2] = [PRIMARY_FAMILY, BUNDLED_FALLBACK_FAMILY];

/// The font families on the system.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstalledFonts {
    /// Every family, monospaced or not (a saved family is "not installed"
    /// only when it isn't here).
    pub all: BTreeSet<String>,
    /// The fixed-pitch families, sorted.
    pub monospace: BTreeSet<String>,
}

impl InstalledFonts {
    /// Whether `family` can be drawn: installed, or bundled with the app.
    pub fn has(&self, family: &str) -> bool {
        BUNDLED.contains(&family) || self.all.contains(family)
    }
}

/// The installed families, read from CoreText the first time and kept for
/// the process. Blocking: call it off the UI thread.
pub fn installed() -> &'static InstalledFonts {
    static INSTALLED: OnceLock<InstalledFonts> = OnceLock::new();
    INSTALLED.get_or_init(coretext::installed)
}

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// A family, bundled or installed.
    Family(String),
    /// The rule between the bundled faces and the installed ones.
    Divider,
}

/// The picker's rows for `query`: the bundled faces, a divider, then the
/// installed monospaced families (minus the bundled ones and the Nerd
/// symbols, which draw no letters). A query keeps the families whose name
/// contains it, ignoring case; the divider only shows with families on both
/// sides of it.
pub fn choices(monospace: &BTreeSet<String>, query: &str) -> Vec<Choice> {
    let query = query.trim().to_lowercase();
    let matches = |family: &str| query.is_empty() || family.to_lowercase().contains(&query);
    let bundled: Vec<Choice> = BUNDLED
        .iter()
        .filter(|f| matches(f))
        .map(|f| Choice::Family(f.to_string()))
        .collect();
    let installed: Vec<Choice> = monospace
        .iter()
        .filter(|f| !BUNDLED.contains(&f.as_str()) && f.as_str() != SYMBOLS_FAMILY)
        .filter(|f| matches(f))
        .map(|f| Choice::Family(f.clone()))
        .collect();
    let divider = (!bundled.is_empty() && !installed.is_empty()).then_some(Choice::Divider);
    bundled.into_iter().chain(divider).chain(installed).collect()
}

/// The families among `choices`, in order (the rows the keyboard moves
/// through).
pub fn families(choices: &[Choice]) -> Vec<&str> {
    choices
        .iter()
        .filter_map(|c| match c {
            Choice::Family(f) => Some(f.as_str()),
            Choice::Divider => None,
        })
        .collect()
}

#[cfg(target_os = "macos")]
mod coretext {
    use std::ffi::{c_char, c_void, CStr};

    use super::InstalledFonts;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;
    type CFTypeID = usize;

    const K_CF_NUMBER_SINT32_TYPE: CFIndex = 3;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_CT_FONT_MONOSPACE_TRAIT: u32 = 1 << 10;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: CFTypeRef);
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        fn CFArrayGetCount(array: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, index: CFIndex) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFDictionaryGetTypeID() -> CFTypeID;
        fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, out: *mut c_void) -> bool;
        fn CFNumberGetTypeID() -> CFTypeID;
        fn CFStringGetTypeID() -> CFTypeID;
        fn CFStringGetLength(s: CFTypeRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, encoding: u32) -> bool;
    }

    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        static kCTFontFamilyNameAttribute: CFTypeRef;
        static kCTFontTraitsAttribute: CFTypeRef;
        static kCTFontSymbolicTrait: CFTypeRef;
        fn CTFontCollectionCreateFromAvailableFonts(options: CFTypeRef) -> CFTypeRef;
        fn CTFontCollectionCreateMatchingFontDescriptors(collection: CFTypeRef) -> CFTypeRef;
        fn CTFontDescriptorCopyAttribute(descriptor: CFTypeRef, attribute: CFTypeRef) -> CFTypeRef;
    }

    /// A CFString's text, or `None` for null or another type.
    ///
    /// SAFETY: `s` is null or a live CF object.
    unsafe fn string(s: CFTypeRef) -> Option<String> {
        unsafe {
            if s.is_null() || CFGetTypeID(s) != CFStringGetTypeID() {
                return None;
            }
            let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(s), K_CF_STRING_ENCODING_UTF8) + 1;
            let mut buf = vec![0 as c_char; size.max(1) as usize];
            if !CFStringGetCString(s, buf.as_mut_ptr(), size, K_CF_STRING_ENCODING_UTF8) {
                return None;
            }
            Some(CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
        }
    }

    /// Whether a face's traits dictionary says fixed-pitch.
    ///
    /// SAFETY: `traits` is null or a live CF object.
    unsafe fn is_monospace(traits: CFTypeRef) -> bool {
        unsafe {
            if traits.is_null() || CFGetTypeID(traits) != CFDictionaryGetTypeID() {
                return false;
            }
            let symbolic = CFDictionaryGetValue(traits, kCTFontSymbolicTrait);
            if symbolic.is_null() || CFGetTypeID(symbolic) != CFNumberGetTypeID() {
                return false;
            }
            let mut value: i32 = 0;
            CFNumberGetValue(symbolic, K_CF_NUMBER_SINT32_TYPE, (&mut value as *mut i32).cast());
            (value as u32) & K_CT_FONT_MONOSPACE_TRAIT != 0
        }
    }

    /// Every face CoreText can see (installed, plus the bundled faces this
    /// process registered), by family. A face missing its family name or
    /// traits is skipped rather than trusted (gpui reads them leniently for
    /// the same reason). Hidden system families (`.SF NS`) are left out.
    pub fn installed() -> InstalledFonts {
        let mut fonts = InstalledFonts::default();
        // SAFETY: each Create/Copy result is released once; the array's
        // values are borrowed (Get rule) and outlive their use.
        unsafe {
            let collection = CTFontCollectionCreateFromAvailableFonts(std::ptr::null());
            if collection.is_null() {
                return fonts;
            }
            let descriptors = CTFontCollectionCreateMatchingFontDescriptors(collection);
            CFRelease(collection);
            if descriptors.is_null() {
                return fonts;
            }
            for ix in 0..CFArrayGetCount(descriptors) {
                let descriptor = CFArrayGetValueAtIndex(descriptors, ix);
                let name = CTFontDescriptorCopyAttribute(descriptor, kCTFontFamilyNameAttribute);
                let family = string(name);
                if !name.is_null() {
                    CFRelease(name);
                }
                let Some(family) = family.filter(|f| !f.is_empty() && !f.starts_with('.')) else {
                    continue;
                };
                let traits = CTFontDescriptorCopyAttribute(descriptor, kCTFontTraitsAttribute);
                let mono = is_monospace(traits);
                if !traits.is_null() {
                    CFRelease(traits);
                }
                if mono {
                    fonts.monospace.insert(family.clone());
                }
                fonts.all.insert(family);
            }
            CFRelease(descriptors);
        }
        fonts
    }
}

#[cfg(not(target_os = "macos"))]
mod coretext {
    pub fn installed() -> super::InstalledFonts {
        super::InstalledFonts::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn names(choices: &[Choice]) -> Vec<String> {
        choices
            .iter()
            .map(|c| match c {
                Choice::Family(f) => f.clone(),
                Choice::Divider => "---".into(),
            })
            .collect()
    }

    #[test]
    fn bundled_first_then_a_divider_then_installed_monospace() {
        let mono = set(&["Menlo", "Geist Mono", "Berkeley Mono", "Symbols Nerd Font Mono", "Source Code Pro"]);
        assert_eq!(
            names(&choices(&mono, "")),
            ["Source Code Pro", "Geist Mono", "---", "Berkeley Mono", "Menlo"]
        );
        assert_eq!(
            families(&choices(&mono, "")),
            ["Source Code Pro", "Geist Mono", "Berkeley Mono", "Menlo"]
        );
    }

    #[test]
    fn a_query_filters_ignoring_case_and_drops_a_lonely_divider() {
        let mono = set(&["Menlo", "Monaco", "SF Mono"]);
        assert_eq!(names(&choices(&mono, "  MEN ")), ["Menlo"]);
        assert_eq!(names(&choices(&mono, "mono")), ["Geist Mono", "---", "SF Mono"]);
        assert_eq!(names(&choices(&mono, "source")), ["Source Code Pro"]);
        assert!(choices(&mono, "zzz").is_empty());
        assert_eq!(names(&choices(&BTreeSet::new(), "")), ["Source Code Pro", "Geist Mono"]);
    }

    #[test]
    fn bundled_faces_count_as_installed() {
        let fonts = InstalledFonts {
            all: set(&["Helvetica"]),
            monospace: BTreeSet::new(),
        };
        assert!(fonts.has("Source Code Pro") && fonts.has("Geist Mono") && fonts.has("Helvetica"));
        assert!(!fonts.has("Berkeley Mono"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn coretext_finds_menlo_monospaced_and_helvetica_not() {
        let fonts = installed();
        assert!(fonts.monospace.contains("Menlo"), "{:?}", fonts.monospace);
        assert!(fonts.all.contains("Helvetica"));
        assert!(!fonts.monospace.contains("Helvetica"));
        assert!(fonts.all.iter().all(|f| !f.starts_with('.')));
    }
}
