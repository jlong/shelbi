//! The desktop's interface settings on Linux: the GNOME interface keys the
//! XDG Settings portal serves (`org.gnome.desktop.interface`), which GTK
//! apps follow under any compositor that runs `xdg-desktop-portal-gtk`
//! (Hyprland on Omarchy does). Two of them have a macOS twin the app
//! already follows:
//!
//! - `enable-animations` (off is System Settings' Reduce Motion,
//!   [`super::motion`]);
//! - `overlay-scrolling` (on is "Show scroll bars: When scrolling",
//!   [`super::scrollers`]).
//!
//! And one from the freedesktop appearance namespace: `contrast` 1 (the
//! desktop asks for high contrast; GNOME's `high-contrast` accessibility
//! key says the same), which the app takes as Reduce Transparency
//! ([`super::blur`]): every window opaque.
//!
//! The first read starts one thread that asks the portal and then listens
//! for its changes. Until it answers (or when there is no portal) the values
//! come from GTK's own `settings.ini` (`gtk-enable-animations`,
//! `gtk-overlay-scrolling`), else the defaults: animations on, overlay
//! scroll bars. Reads never block on D-Bus.

use std::sync::{Mutex, Once};

use futures::channel::mpsc::UnboundedSender;

/// The portal namespace the keys live in.
const NAMESPACE: &str = "org.gnome.desktop.interface";
const ANIMATIONS: &str = "enable-animations";
const OVERLAY: &str = "overlay-scrolling";
/// The freedesktop appearance namespace and its contrast key (a `u32`: 0 no
/// preference, 1 high contrast).
const APPEARANCE: &str = "org.freedesktop.appearance";
const CONTRAST: &str = "contrast";
/// GNOME's accessibility namespace and its high-contrast switch.
const A11Y: &str = "org.gnome.desktop.a11y.interface";
const HIGH_CONTRAST: &str = "high-contrast";

/// The values the app follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Values {
    pub animations: bool,
    pub overlay_scrolling: bool,
    /// The desktop asks for high contrast.
    pub high_contrast: bool,
}

impl Default for Values {
    fn default() -> Self {
        Values {
            animations: true,
            overlay_scrolling: true,
            high_contrast: false,
        }
    }
}

struct State {
    values: Values,
    /// Told whenever `overlay_scrolling` changes ([`observe_overlay`]).
    overlay_watchers: Vec<UnboundedSender<()>>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static START: Once = Once::new();

/// The current values (starting the portal watch on the first call).
pub fn values() -> Values {
    start();
    STATE.lock().ok().and_then(|s| s.as_ref().map(|s| s.values)).unwrap_or_default()
}

/// Send on `changed` whenever the overlay-scrolling setting changes.
pub fn observe_overlay(changed: UnboundedSender<()>) {
    start();
    if let Ok(mut state) = STATE.lock() {
        if let Some(state) = state.as_mut() {
            state.overlay_watchers.push(changed);
        }
    }
}

fn start() {
    START.call_once(|| {
        if let Ok(mut state) = STATE.lock() {
            *state = Some(State {
                values: gtk_ini_values(),
                overlay_watchers: Vec::new(),
            });
        }
        let spawned = std::thread::Builder::new()
            .name("shelbi-desktop-settings".into())
            .spawn(|| futures::executor::block_on(watch_portal()));
        if let Err(e) = spawned {
            tracing::warn!(%e, "couldn't start the desktop settings watch");
        }
    });
}

/// Apply a new value for `key`, telling the overlay watchers when it moved.
fn apply(key: &str, value: bool) {
    let Ok(mut guard) = STATE.lock() else {
        return;
    };
    let Some(state) = guard.as_mut() else {
        return;
    };
    match key {
        ANIMATIONS => state.values.animations = value,
        CONTRAST | HIGH_CONTRAST => state.values.high_contrast = value,
        OVERLAY if state.values.overlay_scrolling != value => {
            state.values.overlay_scrolling = value;
            state.overlay_watchers.retain(|w| w.unbounded_send(()).is_ok());
        }
        _ => {}
    }
}

async fn watch_portal() {
    use futures::StreamExt as _;
    let settings = match ashpd::desktop::settings::Settings::new().await {
        Ok(settings) => settings,
        Err(e) => {
            tracing::info!(%e, "no settings portal: following GTK's settings.ini");
            return;
        }
    };
    for key in [ANIMATIONS, OVERLAY] {
        match settings.read::<bool>(NAMESPACE, key).await {
            Ok(value) => apply(key, value),
            Err(e) => tracing::debug!(key, %e, "the settings portal has no value"),
        }
    }
    if let Ok(contrast) = settings.read::<u32>(APPEARANCE, CONTRAST).await {
        apply(CONTRAST, contrast == 1);
    }
    if let Ok(true) = settings.read::<bool>(A11Y, HIGH_CONTRAST).await {
        apply(HIGH_CONTRAST, true);
    }
    let Ok(mut changes) = settings.receive_setting_changed().await else {
        return;
    };
    while let Some(setting) = changes.next().await {
        let key = setting.key();
        let value = match (setting.namespace(), key) {
            (NAMESPACE, _) | (A11Y, HIGH_CONTRAST) => as_bool(setting.value()),
            (APPEARANCE, CONTRAST) => as_u32(setting.value()).map(|c| c == 1),
            _ => None,
        };
        if let Some(value) = value {
            apply(key, value);
        }
    }
}

/// A boolean, unwrapping the variant a portal may box it in.
fn as_bool(value: &ashpd::zvariant::Value<'_>) -> Option<bool> {
    match value {
        ashpd::zvariant::Value::Bool(b) => Some(*b),
        ashpd::zvariant::Value::Value(inner) => as_bool(inner),
        _ => None,
    }
}

/// A `u32`, unwrapping the variant a portal may box it in.
fn as_u32(value: &ashpd::zvariant::Value<'_>) -> Option<u32> {
    match value {
        ashpd::zvariant::Value::U32(n) => Some(*n),
        ashpd::zvariant::Value::Value(inner) => as_u32(inner),
        _ => None,
    }
}

/// GTK 3's (then 4's) `settings.ini`, over the defaults.
fn gtk_ini_values() -> Values {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")));
    let mut values = Values::default();
    let Some(config) = config else {
        return values;
    };
    for dir in ["gtk-4.0", "gtk-3.0"] {
        if let Ok(text) = std::fs::read_to_string(config.join(dir).join("settings.ini")) {
            values = parse_gtk_ini(&text, values);
        }
    }
    values
}

/// `gtk-enable-animations` and `gtk-overlay-scrolling` from a GTK
/// `settings.ini`, over `base`.
pub fn parse_gtk_ini(text: &str, base: Values) -> Values {
    let mut values = base;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = match value.trim() {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => continue,
        };
        match key.trim() {
            "gtk-enable-animations" => values.animations = value,
            "gtk-overlay-scrolling" => values.overlay_scrolling = value,
            _ => {}
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_ini_turns_animations_and_overlay_scrolling_off() {
        let text = "[Settings]\ngtk-theme-name=Adwaita\ngtk-enable-animations=false\ngtk-overlay-scrolling = 0\n";
        assert_eq!(
            parse_gtk_ini(text, Values::default()),
            Values {
                animations: false,
                overlay_scrolling: false,
                high_contrast: false,
            }
        );
        assert_eq!(parse_gtk_ini("[Settings]\n", Values::default()), Values::default());
    }
}
