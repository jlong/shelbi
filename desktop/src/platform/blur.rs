//! A translucent window's background blur, and the system's Reduce
//! Transparency setting.
//!
//! The blur is the window server's own background blur at a chosen radius,
//! `CGSSetWindowBackgroundBlurRadius`: what Ghostty's `background-blur` and
//! iTerm's blur use. It is private API (in SkyLight, re-exported by
//! CoreGraphics) but has shipped unchanged since OS X 10.x, and it blurs
//! exactly what shows through the window's translucent pixels at any radius.
//! gpui's own `WindowBackgroundAppearance::Blurred` is an
//! `NSVisualEffectView` with one fixed radius, so it can't follow a slider.
//! The symbol is looked up at run time: if a macOS release drops it, the
//! window stays translucent and unblurred, and the app logs it once.
//!
//! On Linux the compositor blurs (see the Linux `imp`'s note).

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{c_char, c_void};
    use std::sync::OnceLock;

    use gpui::Window;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// `CGSConnectionID CGSMainConnectionID(void)`.
    type MainConnection = unsafe extern "C" fn() -> i32;
    /// `CGError CGSSetWindowBackgroundBlurRadius(CGSConnectionID, CGSWindowID, int)`.
    type SetBlurRadius = unsafe extern "C" fn(i32, u32, i32) -> i32;

    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// `RTLD_DEFAULT` on Darwin: search every loaded image.
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

    struct Cgs {
        main_connection: MainConnection,
        set_blur_radius: SetBlurRadius,
    }

    fn cgs() -> Option<&'static Cgs> {
        static CGS: OnceLock<Option<Cgs>> = OnceLock::new();
        CGS.get_or_init(|| {
            // SAFETY: dlsym with NUL-terminated names; the functions have
            // these signatures (the private CoreGraphics headers').
            unsafe {
                let main = dlsym(RTLD_DEFAULT, c"CGSMainConnectionID".as_ptr());
                let set = dlsym(RTLD_DEFAULT, c"CGSSetWindowBackgroundBlurRadius".as_ptr());
                if main.is_null() || set.is_null() {
                    tracing::warn!("window blur unavailable: no CGSSetWindowBackgroundBlurRadius");
                    return None;
                }
                Some(Cgs {
                    main_connection: std::mem::transmute::<*mut c_void, MainConnection>(main),
                    set_blur_radius: std::mem::transmute::<*mut c_void, SetBlurRadius>(set),
                })
            }
        })
        .as_ref()
    }

    /// `window`'s window server number, if it has one (a test window
    /// doesn't).
    fn window_number(window: &Window) -> Option<isize> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return None;
        };
        let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
        // SAFETY: gpui's handle is its live content NSView; `window` and
        // `windowNumber` are plain getters.
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            if ns_window.is_null() {
                return None;
            }
            let number: isize = msg_send![ns_window, windowNumber];
            (number > 0).then_some(number)
        }
    }

    pub fn set_blur_radius(window: &Window, radius: u8) {
        let (Some(cgs), Some(number)) = (cgs(), window_number(window)) else {
            return;
        };
        // SAFETY: the looked-up functions, our own connection and a live
        // window's number.
        let error = unsafe {
            let connection = (cgs.main_connection)();
            (cgs.set_blur_radius)(connection, number as u32, radius as i32)
        };
        if error != 0 {
            tracing::warn!(error, radius, "could not set the window's blur radius");
        }
    }

    pub fn reduce_transparency() -> bool {
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
    }
}

#[cfg(target_os = "macos")]
mod consts {
    /// The app sets the blur's radius itself (Settings' Background blur).
    #[cfg_attr(test, allow(dead_code))]
    pub const RADIUS_ADJUSTABLE: bool = true;

    /// Why the windows are opaque, while [`super::reduce_transparency`]
    /// holds them so.
    pub fn opaque_note() -> &'static str {
        "Reduce Transparency is on (System Settings ▸ Accessibility ▸ Display), so windows are opaque."
    }
}

/// Linux: the compositor blurs, not the app.
///
/// gpui's Linux renderer (`gpui-pre-wgpu`) asks the surface for
/// premultiplied alpha and blends every primitive with premultiplied "over"
/// on both color and alpha (`BlendState::PREMULTIPLIED_ALPHA_BLENDING`), so,
/// unlike the Metal pipelines ([`super::metal_blend`]), a translucent
/// window's alpha is right as drawn: a fill over the glass tints it, and
/// the canvas keeps the window's opacity. Hyprland then blurs whatever shows
/// through a translucent window by its own `decoration:blur` settings (on
/// by default, and per window with `windowrule = noblur`), at a radius the
/// app can't set: there is no Wayland protocol Hyprland implements for it
/// (`org_kde_kwin_blur` is KWin's and asks for a blur, not a radius). So
/// Settings shows no Background blur slider here.
///
/// Translucency needs a compositing Wayland session: under X11 (no
/// `WAYLAND_DISPLAY`, gpui's X11 backend) nothing promises a compositor, so
/// the windows stay opaque, as they do when the desktop asks for high
/// contrast ([`super::desktop_settings`]).
#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn set_blur_radius(_window: &gpui::Window, _radius: u8) {}

    pub fn reduce_transparency() -> bool {
        !translucency_supported() || super::super::desktop_settings::values().high_contrast
    }

    /// A Wayland session: gpui picks its Wayland backend whenever
    /// `WAYLAND_DISPLAY` is set.
    pub fn translucency_supported() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty())
    }
}

#[cfg(not(target_os = "macos"))]
mod consts {
    /// The compositor picks the blur's radius (Hyprland's `decoration:blur`).
    #[cfg_attr(test, allow(dead_code))]
    pub const RADIUS_ADJUSTABLE: bool = false;

    /// Why the windows are opaque, while [`super::reduce_transparency`]
    /// holds them so.
    pub fn opaque_note() -> &'static str {
        if super::imp::translucency_supported() {
            "Your desktop asks for high contrast, so windows are opaque."
        } else {
            "Transparency needs a Wayland session, so windows are opaque."
        }
    }
}

/// Blur what shows through `window` by `radius` points (0 clears it). A
/// no-op off macOS, where the compositor blurs.
pub use imp::set_blur_radius;

/// Whether every window must be opaque: macOS's Reduce Transparency (System
/// Settings ▸ Accessibility ▸ Display); on Linux the desktop's high-contrast
/// setting, or a session that can't composite a translucent window.
pub use imp::reduce_transparency;

pub use consts::opaque_note;

/// Whether the app sets the blur's radius (macOS) or the compositor does
/// (Linux): Settings shows the Background blur slider only for the first.
/// A test's app is the macOS one unless it says otherwise
/// ([`test_radius_adjustable`]), so the views' tests read the same on both.
pub fn radius_adjustable() -> bool {
    #[cfg(test)]
    {
        TEST_ADJUSTABLE.with(|a| a.get())
    }
    #[cfg(not(test))]
    {
        consts::RADIUS_ADJUSTABLE
    }
}

#[cfg(test)]
thread_local! {
    static TEST_ADJUSTABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Have [`radius_adjustable`] answer `adjustable` on this test's thread.
#[cfg(test)]
pub fn test_radius_adjustable(adjustable: bool) {
    TEST_ADJUSTABLE.with(|a| a.set(adjustable));
}
