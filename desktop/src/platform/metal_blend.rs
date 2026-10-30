//! Correct alpha compositing in gpui's Metal pipelines, so a translucent
//! window's fills let the desktop through.
//!
//! gpui-pre 0.3.9 (`gpui_apple::metal_renderer`, `build_pipeline_state` and
//! `build_path_sprite_pipeline_state`) blends color with "over" but sets the
//! destination ALPHA factor to `One`: every primitive adds its alpha to what
//! the framebuffer already holds. An opaque window clears to alpha 1, so it
//! can't tell. A translucent one clears to 0 and paints the canvas at the
//! window's opacity, then a button's 18% fill over 85% glass ends at alpha
//! 1.03, clamped to 1: CoreAnimation treats the pixel as opaque, and the fill
//! hides the desktop instead of tinting it. "Over" needs
//! `OneMinusSourceAlpha` there (0.18 + 0.85 x 0.82 = 0.877).
//!
//! The fix, without forking gpui: [`install`] wraps the Metal device's
//! `newRenderPipelineStateWithDescriptor:error:` (on the device's own class,
//! found at run time) and, for gpui's pipelines only (by the labels it gives
//! them, [`GPUI_PIPELINES`]), turns a blending color attachment's
//! destination alpha factor `One` into `OneMinusSourceAlpha` before the
//! pipeline is built. The color factors are untouched, and over an opaque
//! window's alpha of 1 the corrected factor still gives 1, so an opaque
//! window draws exactly as before. Install before the first window opens:
//! gpui builds each window's pipelines when it creates the window's renderer.
//!
//! If a gpui upgrade renames its pipelines, nothing matches and the hook
//! changes nothing (fills over glass read solid again); the `--visual-check`
//! blend check fails on it. Coupled to gpui-pre 0.3.9's labels.

/// gpui's pipelines whose alpha factor is `One` (`metal_renderer.rs`'s
/// `build_pipeline_state` and `build_path_sprite_pipeline_state` callers).
/// `paths_rasterization` already blends alpha with `OneMinusSourceAlpha`.
pub const GPUI_PIPELINES: &[&str] = &[
    "path_sprites",
    "shadows",
    "quads",
    "underlines",
    "monochrome_sprites",
    "polychrome_sprites",
    "surfaces",
];

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::sync::OnceLock;

    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::{msg_send, sel};

    use super::GPUI_PIPELINES;

    /// `MTLBlendFactorOne` and `MTLBlendFactorOneMinusSourceAlpha`.
    const BLEND_ONE: usize = 1;
    const BLEND_ONE_MINUS_SOURCE_ALPHA: usize = 5;

    type NewPipeline = unsafe extern "C" fn(&AnyObject, Sel, *mut AnyObject, *mut *mut AnyObject) -> *mut AnyObject;

    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        /// Every device (an `NSArray`, +1).
        fn MTLCopyAllDevices() -> *mut AnyObject;
        /// The default device (+1).
        fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
    }

    /// The replaced methods and their original implementations, as
    /// `(Method address, IMP)`: a device class can inherit the method, so
    /// the replacement finds its original by the method it was reached
    /// through.
    static ORIGINALS: OnceLock<Vec<(usize, NewPipeline)>> = OnceLock::new();

    fn selector() -> Sel {
        sel!(newRenderPipelineStateWithDescriptor:error:)
    }

    pub fn install() {
        // SAFETY: the Metal device constructors; each object returned +1 is
        // released here, and the array is walked with NSArray's getters.
        let classes = unsafe {
            let mut devices: Vec<*mut AnyObject> = Vec::new();
            let all = MTLCopyAllDevices();
            if !all.is_null() {
                let count: usize = msg_send![all, count];
                for i in 0..count {
                    let device: *mut AnyObject = msg_send![all, objectAtIndex: i];
                    let _: *mut AnyObject = msg_send![device, retain];
                    devices.push(device);
                }
                let _: () = msg_send![all, release];
            }
            let default = MTLCreateSystemDefaultDevice();
            if !default.is_null() {
                devices.push(default);
            }
            let classes: Vec<&'static AnyClass> = devices.iter().map(|&d| (*d).class()).collect();
            for device in devices {
                let _: () = msg_send![device, release];
            }
            classes
        };
        let mut originals: Vec<(usize, NewPipeline)> = Vec::new();
        for class in classes {
            let Some(method) = class.instance_method(selector()) else {
                continue;
            };
            let key = method as *const _ as usize;
            if originals.iter().any(|(m, _)| *m == key) {
                continue;
            }
            // SAFETY: the method has exactly this signature
            // (`- (id<MTLRenderPipelineState>)newRenderPipelineStateWithDescriptor:error:`).
            originals.push((key, unsafe { std::mem::transmute::<Imp, NewPipeline>(method.implementation()) }));
        }
        if originals.is_empty() {
            tracing::warn!("metal blend fix unavailable: no device answers {:?}", selector());
            return;
        }
        let methods: Vec<usize> = originals.iter().map(|(m, _)| *m).collect();
        if ORIGINALS.set(originals).is_err() {
            return;
        }
        for method in methods {
            // SAFETY: a live `Method` of a loaded class (classes are never
            // unloaded), replaced with a function of its signature, after
            // its original is stored above.
            unsafe {
                let method = &*(method as *const objc2::runtime::Method);
                method.set_implementation(std::mem::transmute::<NewPipeline, Imp>(new_pipeline));
            }
        }
    }

    /// Whether [`install`] replaced the method `class` answers with.
    #[cfg(test)]
    pub fn installed_on(class: &AnyClass) -> bool {
        class
            .instance_method(selector())
            .is_some_and(|m| m.implementation() as usize == new_pipeline as NewPipeline as usize)
    }

    /// A device class's `newRenderPipelineStateWithDescriptor:error:`.
    unsafe extern "C" fn new_pipeline(
        this: &AnyObject,
        cmd: Sel,
        descriptor: *mut AnyObject,
        error: *mut *mut AnyObject,
    ) -> *mut AnyObject {
        // SAFETY: `descriptor` is the caller's `MTLRenderPipelineDescriptor`.
        unsafe { correct_alpha(descriptor) };
        let method = this.class().instance_method(cmd).map(|m| m as *const _ as usize);
        let original = ORIGINALS
            .get()
            .and_then(|o| o.iter().find(|(m, _)| Some(*m) == method))
            .map(|(_, imp)| *imp);
        match original {
            // SAFETY: the method's own implementation, with its arguments.
            Some(original) => unsafe { original(this, cmd, descriptor, error) },
            None => std::ptr::null_mut(),
        }
    }

    /// On a gpui pipeline's descriptor, attachment 0's destination alpha
    /// factor `One` becomes `OneMinusSourceAlpha`.
    unsafe fn correct_alpha(descriptor: *mut AnyObject) {
        if descriptor.is_null() {
            return;
        }
        // SAFETY: `MTLRenderPipelineDescriptor`'s and its color
        // attachment's documented getters and setter.
        unsafe {
            let label: *mut AnyObject = msg_send![descriptor, label];
            if label.is_null() {
                return;
            }
            let utf8: *const std::ffi::c_char = msg_send![label, UTF8String];
            if utf8.is_null() {
                return;
            }
            let label = CStr::from_ptr(utf8).to_string_lossy();
            if !GPUI_PIPELINES.contains(&label.as_ref()) {
                return;
            }
            let attachments: *mut AnyObject = msg_send![descriptor, colorAttachments];
            let attachment: *mut AnyObject = msg_send![attachments, objectAtIndexedSubscript: 0usize];
            if attachment.is_null() {
                return;
            }
            let blending: Bool = msg_send![attachment, isBlendingEnabled];
            let factor: usize = msg_send![attachment, destinationAlphaBlendFactor];
            if blending.as_bool() && factor == BLEND_ONE {
                let _: () = msg_send![attachment, setDestinationAlphaBlendFactor: BLEND_ONE_MINUS_SOURCE_ALPHA];
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn install_wraps_every_devices_pipeline_constructor() {
            install();
            install(); // once
            // SAFETY: as in `install`.
            let class = unsafe {
                let device = MTLCreateSystemDefaultDevice();
                if device.is_null() {
                    return; // no GPU (a VM): nothing to fix
                }
                let class = (*device).class();
                let _: () = msg_send![device, release];
                class
            };
            assert!(installed_on(class), "{class:?}");
        }
    }
}

/// Wrap the Metal devices' pipeline constructor (once, before the first
/// window opens).
#[cfg(target_os = "macos")]
pub fn install() {
    imp::install();
}

#[cfg(not(target_os = "macos"))]
pub fn install() {}
