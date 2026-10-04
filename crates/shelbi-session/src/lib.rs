//! # shelbi-session
//!
//! The `shelbi __session` process: **one small detached process per session**
//! that owns one PTY and one headless terminal emulator, and outlives whatever
//! launched it. This is the server side of the remove-tmux session protocol
//! (`shelbi-proto`); the client side is `shelbi-client` / `shelbi-term`.
//!
//! See the "Removing tmux" plan, section "One process per session", and the
//! Phase 0 findings under `docs/removing-tmux/phase0/` (emulator choice, spawn
//! recipe, query responder) that this crate applies.
//!
//! ## Shape
//!
//! - [`spawn::spawn_detached`] — what a *client* calls: launch a `shelbi
//!   __session` process detached (setsid / `systemd-run --user --scope`, stdio to
//!   `/dev/null`) with an explicit environment, so it survives the launcher (and
//!   a launching `ssh`) exiting.
//! - [`session::run`] — the *body* of `shelbi __session`: own the PTY and
//!   emulator, answer terminal queries with no client attached, serve clients on
//!   a Unix socket, and on child exit write `exit.json` + `final.txt`.
//! - [`layout`] — the on-disk directory (`~/.shelbi/sessions/<short-id>/` with
//!   `sock`, `lock`, `meta.json`, `exit.json`, `final.txt`, `raw.log`), and the
//!   short-id hashing that keeps the socket path under the 104-byte limit.
//! - [`lock`] — the lifetime lock (an unheld lock means the session is dead).
//! - [`responder`] — the startup query responder (the session is the *only*
//!   thing that answers cursor/DA/color queries, with no client attached).
//! - [`emulator`] — the headless `alacritty_terminal` emulator, kitty keyboard
//!   protocol enabled, 10,000 lines of scrollback.
//! - [`history`] — the bounded recent-bytes ring and the optional raw output log.
//! - [`transport`] — the Unix-socket frame server: the full session protocol
//!   (frozen core plus every additive capability — `info`, `paste`, `set-meta`,
//!   `detach`, the pushed title/bell/resized events, in-band sequenced resize,
//!   backpressure `resync`, and keepalive).
//! - [`daemon_watchdog`] — a background thread that restarts a crashed hub
//!   daemon while this session's project is open (the service units are retired,
//!   so sessions are the watchers).
//!
//! ## What this crate does *not* do
//!
//! Attach **replay** — reconstructing *full* emulator state (both buffers, saved
//! cursors, modes, history) for a new client — is `rt-replay`. Until it lands,
//! `attach` and the backpressure drop both recover a client with a full-screen
//! text snapshot (the [`Resync`](shelbi_proto::Resync) stand-in), which is
//! correct for a repaint but not a byte-exact state restore.

pub mod daemon_watchdog;
pub mod emulator;
pub mod history;
pub mod layout;
pub mod lock;
pub mod meta;
pub mod responder;
pub mod session;
pub mod spawn;
pub mod transport;

pub use layout::{SessionPaths, MAX_SOCKET_PATH};
pub use meta::{ExitRecord, Meta};
pub use session::{run, RunArgs};
pub use spawn::{spawn_detached, spawn_detached_with_exe, SpawnSpec, SpawnedSession};
