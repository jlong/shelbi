//! Hyprland's IPC, for the one thing the app asks of it: whether Hyprland
//! blurs what shows through the app's own windows (Settings' Background
//! blur on Linux, [`super::blur`]).
//!
//! Hyprland (0.56) has no Wayland protocol or window property that carries
//! a blur radius. `decoration:blur:size` and `passes` are global, and
//! `decoration:blur:enabled` gates every window: with it off, nothing
//! blurs, and no window rule or property turns blur on for one window
//! (`IHyprRenderer::shouldBlur` checks it first). `ext-background-effect-v1`
//! (which Hyprland implements) only marks a region to blur, behind the same
//! switch. What one window can have is `no_blur`, set at run time over the
//! instance socket (`hl.dsp.window.set_prop`, what Omarchy's own
//! transparency toggle uses): so the app turns Hyprland's blur off for its
//! windows at 0 and leaves Hyprland's own blur on above it, and never
//! touches the global settings or another app's windows. It reads
//! `decoration:blur:enabled` (and again on a config reload) so Settings can
//! show Background blur disabled, and say why, while it is off.
//!
//! The app's windows are the clients whose `pid` is this process's: gpui
//! doesn't say which Hyprland window a `gpui::Window` is, and every one of
//! them takes the same setting. A worker thread does the socket work, off
//! the UI thread, and listens on the event socket so a window that opens
//! later gets the setting once Hyprland maps it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;

/// Running under Hyprland: it sets `HYPRLAND_INSTANCE_SIGNATURE` for every
/// client it starts.
pub fn detected() -> bool {
    detected_from(std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").as_deref())
}

fn detected_from(signature: Option<&std::ffi::OsStr>) -> bool {
    signature.is_some_and(|s| !s.is_empty())
}

/// What the app asks Hyprland for its windows.
const UNSET: u8 = u8::MAX;
static WANT_NO_BLUR: AtomicU8 = AtomicU8::new(UNSET);

/// Hyprland's `decoration:blur:enabled`, as last read (on until read, as
/// Hyprland's default is). The worker reads it when it starts, on every
/// sync and on a config reload.
static GLOBAL_BLUR: AtomicBool = AtomicBool::new(true);

/// Told whenever [`GLOBAL_BLUR`] changes ([`observe_global_blur`]).
static WATCHERS: Mutex<Vec<UnboundedSender<()>>> = Mutex::new(Vec::new());

/// Whether Hyprland blurs at all (`decoration:blur:enabled`), as last read.
/// The app only reads it; it never changes the user's global settings.
pub fn global_blur_enabled() -> bool {
    GLOBAL_BLUR.load(Ordering::Relaxed)
}

/// Send on `changed` whenever `decoration:blur:enabled` changes (a config
/// reload: Omarchy's glassy mode, an edit to the user's config), and read it
/// now.
pub fn observe_global_blur(changed: UnboundedSender<()>) {
    if !detected() {
        return;
    }
    if let Ok(mut watchers) = WATCHERS.lock() {
        watchers.push(changed);
    }
    if let Some(worker) = worker() {
        let _ = worker.send(Ask::Sync);
    }
}

/// Have Hyprland blur what shows through the app's windows (`blur`) or not,
/// now and for every window that opens later. Returns at once; a worker
/// does the IPC.
pub fn set_blur(blur: bool) {
    if !detected() {
        return;
    }
    WANT_NO_BLUR.store(u8::from(!blur), Ordering::Relaxed);
    if let Some(worker) = worker() {
        let _ = worker.send(Ask::Sync);
    }
}

/// What the worker is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    /// Re-read the global switch and bring every window to the setting.
    Sync,
    /// The same, after a config reload, which may have reset the windows'
    /// properties: send every window its setting again.
    Reload,
}

/// The worker's queue.
fn worker() -> Option<&'static Sender<Ask>> {
    static WORKER: OnceLock<Option<Sender<Ask>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let dir = instance_dir()?;
            let (tx, rx) = mpsc::channel::<Ask>();
            let events = tx.clone();
            let socket = dir.join(".socket.sock");
            std::thread::Builder::new()
                .name("hyprland-blur".into())
                .spawn(move || {
                    let mut applied = HashMap::new();
                    while let Ok(mut ask) = rx.recv() {
                        // Coalesce a slider's run of moves into one sync.
                        while let Ok(next) = rx.try_recv() {
                            if next == Ask::Reload {
                                ask = Ask::Reload;
                            }
                        }
                        if ask == Ask::Reload {
                            applied.clear();
                        }
                        sync(&socket, &mut applied);
                    }
                })
                .ok()?;
            let event_socket = dir.join(".socket2.sock");
            let _ = std::thread::Builder::new()
                .name("hyprland-events".into())
                .spawn(move || listen(&event_socket, &events));
            Some(tx)
        })
        .as_ref()
}

/// `$XDG_RUNTIME_DIR/hypr/<signature>` (Hyprland's sockets), or the
/// `/tmp/hypr` an older Hyprland used.
fn instance_dir() -> Option<PathBuf> {
    let signature = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty())?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    [runtime.map(|r| r.join("hypr")), Some(PathBuf::from("/tmp/hypr"))]
        .into_iter()
        .flatten()
        .map(|base| base.join(&signature))
        .find(|dir| dir.join(".socket.sock").exists())
}

/// Re-read the global switch (telling the watchers of a change), then bring
/// every one of the app's windows to the wanted `no_blur`. `applied`
/// remembers what each window was given, so a sync only sends changes.
fn sync(socket: &std::path::Path, applied: &mut HashMap<String, bool>) {
    if let Some(on) = request(socket, "j/getoption decoration:blur:enabled").ok().and_then(|r| parse_bool_option(&r)) {
        if GLOBAL_BLUR.swap(on, Ordering::Relaxed) != on {
            if let Ok(mut watchers) = WATCHERS.lock() {
                watchers.retain(|w| w.unbounded_send(()).is_ok());
            }
        }
    }
    let want = WANT_NO_BLUR.load(Ordering::Relaxed);
    if want == UNSET {
        return;
    }
    let no_blur = want == 1;
    let Ok(clients) = request(socket, "j/clients") else {
        return;
    };
    let windows = own_windows(&clients, std::process::id());
    applied.retain(|address, _| windows.contains(address));
    for address in windows {
        if applied.get(&address) == Some(&no_blur) {
            continue;
        }
        if set_no_blur(socket, &address, no_blur) {
            applied.insert(address, no_blur);
        }
    }
}

/// Which dispatch syntax the instance takes: a Lua config's
/// `hl.dsp.window.set_prop`, or hyprlang's `setprop`.
static LEGACY_SYNTAX: AtomicBool = AtomicBool::new(false);

fn set_no_blur(socket: &std::path::Path, address: &str, no_blur: bool) -> bool {
    let legacy = LEGACY_SYNTAX.load(Ordering::Relaxed);
    for syntax in [legacy, !legacy] {
        let command = set_prop_command(address, no_blur, syntax);
        match request(socket, &command) {
            Ok(reply) if reply.trim() == "ok" => {
                LEGACY_SYNTAX.store(syntax, Ordering::Relaxed);
                return true;
            }
            Ok(reply) => tracing::debug!(%reply, %command, "hyprland refused the dispatch"),
            Err(error) => {
                tracing::debug!(%error, "hyprland's socket");
                return false;
            }
        }
    }
    tracing::warn!(address, "could not set no_blur on the window");
    false
}

/// The dispatch giving `address` `no_blur`, or leaving it to Hyprland (and
/// the user's own window rules) with `unset`.
fn set_prop_command(address: &str, no_blur: bool, legacy: bool) -> String {
    match (legacy, no_blur) {
        (false, no_blur) => {
            let value = if no_blur { "true" } else { "unset" };
            format!(
                r#"dispatch hl.dsp.window.set_prop({{ window = "address:{address}", prop = "no_blur", value = "{value}" }})"#
            )
        }
        (true, true) => format!("dispatch setprop address:{address} no_blur 1"),
        (true, false) => format!("dispatch setprop address:{address} no_blur unset"),
    }
}

/// One request on the instance socket: Hyprland replies, then closes.
fn request(socket: &std::path::Path, command: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(command.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply)
}

/// Follow the event socket: a window opening or the config reloading asks
/// the worker to sync. Ends when the socket does (Hyprland exiting).
fn listen(socket: &std::path::Path, worker: &Sender<Ask>) {
    let Ok(stream) = UnixStream::connect(socket) else {
        tracing::debug!("no hyprland event socket: later windows get the blur setting on its next change");
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if let Some(ask) = wants(&line) {
            if worker.send(ask).is_err() {
                return;
            }
        }
    }
}

/// What an event asks of the worker, if anything.
fn wants(event: &str) -> Option<Ask> {
    let name = event.split_once(">>").map_or(event, |(name, _)| name);
    match name {
        "openwindow" => Some(Ask::Sync),
        "configreloaded" => Some(Ask::Reload),
        _ => None,
    }
}

/// The addresses of `pid`'s windows in a `j/clients` reply.
fn own_windows(clients: &str, pid: u32) -> Vec<String> {
    let Ok(serde_json::Value::Array(clients)) = serde_json::from_str(clients) else {
        return Vec::new();
    };
    clients
        .iter()
        .filter(|c| c.get("pid").and_then(serde_json::Value::as_u64) == Some(pid as u64))
        .filter_map(|c| c.get("address")?.as_str().map(str::to_owned))
        .collect()
}

/// A bool option's value in a `j/getoption` reply.
fn parse_bool_option(reply: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(reply).ok()?;
    let bool = value.get("bool").and_then(serde_json::Value::as_bool);
    bool.or_else(|| value.get("int").and_then(serde_json::Value::as_i64).map(|i| i != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_is_its_instance_signature() {
        assert!(detected_from(Some("abc_123".as_ref())));
        assert!(!detected_from(Some("".as_ref())));
        assert!(!detected_from(None));
    }

    #[test]
    fn only_this_process_s_windows_are_ours() {
        let clients = r#"[
            {"address": "0x1", "pid": 42, "class": "dev.shelbi.Desktop"},
            {"address": "0x2", "pid": 7, "class": "Alacritty"},
            {"address": "0x3", "pid": 42, "class": "dev.shelbi.Desktop"}
        ]"#;
        assert_eq!(own_windows(clients, 42), ["0x1", "0x3"]);
        assert!(own_windows(clients, 1).is_empty());
        assert!(own_windows("unknown request", 42).is_empty());
    }

    #[test]
    fn the_dispatch_follows_the_config_s_syntax() {
        assert_eq!(
            set_prop_command("0xab", true, false),
            r#"dispatch hl.dsp.window.set_prop({ window = "address:0xab", prop = "no_blur", value = "true" })"#
        );
        assert_eq!(
            set_prop_command("0xab", false, false),
            r#"dispatch hl.dsp.window.set_prop({ window = "address:0xab", prop = "no_blur", value = "unset" })"#
        );
        assert_eq!(set_prop_command("0xab", true, true), "dispatch setprop address:0xab no_blur 1");
        assert_eq!(set_prop_command("0xab", false, true), "dispatch setprop address:0xab no_blur unset");
    }

    #[test]
    fn the_global_switch_reads_from_getoption() {
        let on = r#"{"option": "decoration:blur:enabled", "bool": true, "set": true }"#;
        let off = r#"{"option": "decoration:blur:enabled", "bool": false, "set": true }"#;
        let legacy = r#"{"option": "decoration:blur:enabled", "int": 0, "set": false }"#;
        assert_eq!(parse_bool_option(on), Some(true));
        assert_eq!(parse_bool_option(off), Some(false));
        assert_eq!(parse_bool_option(legacy), Some(false));
        assert_eq!(parse_bool_option("no such option"), None);
    }

    #[test]
    fn a_new_window_or_a_reload_syncs() {
        assert_eq!(wants("openwindow>>5a1b,1,dev.shelbi.Desktop,Shelbi"), Some(Ask::Sync));
        assert_eq!(wants("configreloaded>>"), Some(Ask::Reload));
        assert_eq!(wants("activewindow>>Alacritty,~"), None);
    }
}
