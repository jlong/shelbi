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

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn set_blur_radius(_window: &gpui::Window, _radius: u8) {}

    pub fn reduce_transparency() -> bool {
        false
    }
}

/// Blur what shows through `window` by `radius` points (0 clears it).
pub use imp::set_blur_radius;

/// Whether the user asked macOS to reduce transparency (System Settings ▸
/// Accessibility ▸ Display): every window is then opaque.
pub use imp::reduce_transparency;
