//! Bake the install-time shelbi root into the binary.
//!
//! `scripts/install.sh` prompts the operator for a global state directory
//! and exports it as `SHELBI_DEFAULT_ROOT` before invoking `cargo build`.
//! We forward that into the compiled artifact with `cargo:rustc-env=` so
//! the runtime can pick it up via `env!("SHELBI_DEFAULT_ROOT")`. When the
//! env var is absent (developer running `cargo test` directly, CI doing a
//! plain build), we still emit the var — empty — so `env!()` never errors
//! and `root()` falls back to `~/.shelbi`.

fn main() {
    let value = std::env::var("SHELBI_DEFAULT_ROOT").unwrap_or_default();
    println!("cargo:rustc-env=SHELBI_DEFAULT_ROOT={value}");
    // Re-run when the install-time choice changes so subsequent builds
    // bake the new default in.
    println!("cargo:rerun-if-env-changed=SHELBI_DEFAULT_ROOT");
}
