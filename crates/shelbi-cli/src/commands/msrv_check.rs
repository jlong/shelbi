//! `shelbi msrv-check` — build the workspace on the declared minimum supported
//! Rust version (MSRV), mirroring CI's `msrv` job, so an MSRV break is caught
//! *before* handoff instead of surfacing in CI after the review already passed.
//!
//! The zen probe's stable-toolchain checks (`cargo build`, `cargo clippy`,
//! `cargo test`) never pin an old rustc, so a lockfile change that pulls in a
//! dependency requiring a newer Rust than the project's declared `rust-version`
//! sails through the probe and review and only fails in CI's load-isolated
//! `msrv` job. This command closes that gap: it reads `rust-version` from the
//! workspace `Cargo.toml` in the current directory and runs the same
//! `cargo +<rust-version> check --workspace --all-targets --locked` CI runs.
//!
//! It is meant to be listed as a `zen.checks.local` entry on Rust tracks, so it
//! must **never hard-fail for reasons unrelated to the code**. When the MSRV
//! toolchain can't be provisioned — `rustup` isn't installed, or the toolchain
//! is missing and the one-time install fails (offline, etc.) — it prints a
//! clear warning and exits 0 (a skip), rather than blocking every promotion on
//! a machine that simply can't run the check. A genuine MSRV break (the check
//! ran and `cargo` reported an error) propagates `cargo`'s non-zero exit.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Result};

/// Opt-out escape hatch. Set to `1`/`true` to make `shelbi msrv-check` a no-op
/// (exit 0) — for a host that intentionally doesn't provision the MSRV
/// toolchain and doesn't want the one-time install attempt or the skip warning.
const SKIP_ENV: &str = "SHELBI_SKIP_MSRV_CHECK";

/// Set to `1`/`true` to skip the one-time `rustup toolchain install` when the
/// MSRV toolchain is missing: the check is skipped (exit 0) instead. Lets a
/// host keep the check active where the toolchain is already provisioned
/// without ever triggering an unattended download.
const NO_INSTALL_ENV: &str = "SHELBI_MSRV_NO_INSTALL";

/// How far up from the current directory to look for a `Cargo.toml` carrying a
/// concrete `rust-version`. Bounded so a stray invocation outside a Rust tree
/// walks a few parents and then skips cleanly rather than climbing to `/`.
const MANIFEST_SEARCH_DEPTH: usize = 16;

pub fn run() -> Result<()> {
    if env_flag(SKIP_ENV) {
        eprintln!("shelbi msrv-check: skipped ({SKIP_ENV} is set).");
        return Ok(());
    }

    let cwd = std::env::current_dir().map_err(|e| anyhow!("resolving current directory: {e}"))?;

    // No declared MSRV in reach → nothing to check. This is the "not a Rust
    // project (or no `rust-version` pinned)" case: a clean skip, not a failure.
    let Some((manifest, version)) = find_rust_version(&cwd) else {
        eprintln!(
            "shelbi msrv-check: no `rust-version` found in a Cargo.toml at or above {} — nothing \
             to check (skipping).",
            cwd.display()
        );
        return Ok(());
    };
    eprintln!(
        "shelbi msrv-check: declared MSRV is Rust {version} (from {}).",
        manifest.display()
    );

    // Provisioning failures (no rustup, missing-and-uninstallable toolchain)
    // are skips, not check failures — the machine can't run the check, and a
    // hard failure here would block every promotion behind it.
    if !rustup_available() {
        eprintln!(
            "shelbi msrv-check: `rustup` not found on PATH — can't pin the MSRV toolchain, \
             skipping. (CI's `msrv` job still enforces it.)"
        );
        return Ok(());
    }

    if !toolchain_installed(&version) {
        if env_flag(NO_INSTALL_ENV) {
            eprintln!(
                "shelbi msrv-check: Rust {version} toolchain is not installed and {NO_INSTALL_ENV} \
                 is set — skipping. Install it with `rustup toolchain install {version}` to enable \
                 the check."
            );
            return Ok(());
        }
        eprintln!(
            "shelbi msrv-check: Rust {version} toolchain not installed — installing it once (to \
             match CI's `msrv` job). This may take a minute."
        );
        if !install_toolchain(&version) {
            eprintln!(
                "shelbi msrv-check: could not install the Rust {version} toolchain (offline, or \
                 rustup declined) — skipping. (CI's `msrv` job still enforces it.)"
            );
            return Ok(());
        }
    }

    // The check itself. Matches CI's `cargo check --workspace --locked
    // --all-targets`, pinned to the MSRV toolchain via `cargo +<version>`.
    eprintln!("shelbi msrv-check: running `cargo +{version} check --workspace --all-targets --locked`");
    let status = Command::new("cargo")
        .arg(format!("+{version}"))
        .args(["check", "--workspace", "--all-targets", "--locked"])
        .status()
        .map_err(|e| anyhow!("launching `cargo +{version} check`: {e}"))?;

    // Propagate cargo's own exit code so a genuine MSRV break fails the check
    // (and, through it, the probe's local-checks gate) exactly as CI would.
    std::process::exit(status.code().unwrap_or(1));
}

/// Read `0`/`1`/`true`/`false` (case-insensitive) truthiness from an env var.
/// Any set, non-empty, non-false value is truthy so a bare `VAR=1` works.
fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !v.is_empty() && v != "0" && v != "false" && v != "no"
        }
        Err(_) => false,
    }
}

/// Walk up from `start`, returning the first `Cargo.toml` that declares a
/// concrete `rust-version` string along with the parsed version. A member
/// crate's `rust-version.workspace = true` is *not* a concrete value, so the
/// walk continues up to the workspace root that owns the real one.
fn find_rust_version(start: &Path) -> Option<(PathBuf, String)> {
    let mut dir = Some(start);
    for _ in 0..MANIFEST_SEARCH_DEPTH {
        let here = dir?;
        let manifest = here.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if let Some(version) = parse_rust_version(&text) {
                return Some((manifest, version));
            }
        }
        dir = here.parent();
    }
    None
}

/// Extract a concrete `rust-version = "X.Y[.Z]"` value from a `Cargo.toml`.
///
/// Deliberately line-based rather than a full TOML parse: the workspace has no
/// `toml` dependency (and adding one risks the very MSRV bump this command
/// guards against). The key appears once as a real value — under `[package]` or
/// `[workspace.package]` — so a scan for a `rust-version = "..."` line is
/// sufficient and robust. The dotted-key inheritance form
/// (`rust-version.workspace = true`) carries no literal value and is skipped.
fn parse_rust_version(toml: &str) -> Option<String> {
    for raw in toml.lines() {
        // Drop any trailing comment (`rust-version = "1.88" # min`), then trim.
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix("rust-version") else {
            continue;
        };
        let rest = rest.trim_start();
        // `rust-version.workspace = true` (inheritance) — no literal here.
        if rest.starts_with('.') {
            continue;
        }
        // `rust-versions`/`rust-version2`/… — a different key sharing the prefix.
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim();
        let value = rest
            .strip_prefix('"')
            .and_then(|s| s.split('"').next())
            .or_else(|| rest.strip_prefix('\'').and_then(|s| s.split('\'').next()))?
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Whether `rustup` can be invoked. A spawn error (not on PATH) is the only
/// signal we need — a non-zero exit from `rustup --version` would still mean
/// rustup is present.
fn rustup_available() -> bool {
    Command::new("rustup")
        .arg("--version")
        .output()
        .is_ok()
}

/// Whether a toolchain matching `version` is already installed, parsed from
/// `rustup toolchain list`. A spawn/exec failure conservatively reports "not
/// installed" so the caller attempts an install (which will surface the real
/// error) rather than silently running the check on the wrong toolchain.
fn toolchain_installed(version: &str) -> bool {
    match Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
    {
        Ok(out) if out.status.success() => {
            toolchain_listed(&String::from_utf8_lossy(&out.stdout), version)
        }
        _ => false,
    }
}

/// Does `rustup toolchain list` output contain a toolchain matching `version`?
///
/// `rustup` lists toolchains as `<channel>-<target>` (e.g.
/// `1.88.0-aarch64-apple-darwin (default)`), so a declared `1.88` matches the
/// installed `1.88.0-…` by channel prefix. An exact-channel match (the declared
/// version already carries a patch) and a `version.`/`version-` prefix are both
/// accepted; the whitespace split drops the trailing `(default)`/`(active)`.
fn toolchain_listed(list_output: &str, version: &str) -> bool {
    list_output.lines().any(|line| {
        let Some(name) = line.split_whitespace().next() else {
            return false;
        };
        name == version
            || name.starts_with(&format!("{version}-"))
            || name.starts_with(&format!("{version}."))
    })
}

/// Install the MSRV toolchain with a minimal profile (no docs/clippy/rustfmt —
/// `cargo check` needs none of them). Returns whether the install succeeded.
fn install_toolchain(version: &str) -> bool {
    Command::new("rustup")
        .args(["toolchain", "install", version, "--profile", "minimal"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workspace_package_rust_version() {
        let toml = "\
[workspace]
members = [\"crates/*\"]

[workspace.package]
edition = \"2021\"
rust-version = \"1.88\"
";
        assert_eq!(parse_rust_version(toml).as_deref(), Some("1.88"));
    }

    #[test]
    fn parses_plain_package_rust_version() {
        let toml = "[package]\nname = \"x\"\nrust-version = \"1.74.0\"\n";
        assert_eq!(parse_rust_version(toml).as_deref(), Some("1.74.0"));
    }

    #[test]
    fn ignores_workspace_inheritance_form() {
        // A member crate inheriting the workspace MSRV carries no literal value;
        // the scan must not mistake `rust-version.workspace = true` for "1".
        let toml = "[package]\nname = \"member\"\nrust-version.workspace = true\n";
        assert_eq!(parse_rust_version(toml), None);
    }

    #[test]
    fn tolerates_trailing_comment_and_whitespace() {
        let toml = "  rust-version   =   \"1.90\"   # minimum we support\n";
        assert_eq!(parse_rust_version(toml).as_deref(), Some("1.90"));
    }

    #[test]
    fn does_not_match_a_different_key_sharing_the_prefix() {
        let toml = "rust-versions = [\"1.88\"]\nrust-version-note = \"x\"\n";
        assert_eq!(parse_rust_version(toml), None);
    }

    #[test]
    fn no_rust_version_returns_none() {
        assert_eq!(parse_rust_version("[package]\nname = \"x\"\n"), None);
    }

    #[test]
    fn toolchain_listed_matches_installed_patch_release() {
        let list = "stable-aarch64-apple-darwin (active, default)\n\
                    1.88.0-aarch64-apple-darwin\n\
                    1.97.1-aarch64-apple-darwin\n";
        // Declared `1.88` is satisfied by the installed `1.88.0-…`.
        assert!(toolchain_listed(list, "1.88"));
        // `1.8` must NOT match `1.88.0` (prefix guarded by the `-`/`.` boundary).
        assert!(!toolchain_listed(list, "1.8"));
        // A declared full version matches its own `-target` line.
        assert!(toolchain_listed(list, "1.97.1"));
        // An uninstalled version isn't listed.
        assert!(!toolchain_listed(list, "1.90"));
    }

    #[test]
    fn toolchain_listed_matches_exact_channel_name() {
        // Defensive: if rustup ever lists a bare channel with no target suffix.
        assert!(toolchain_listed("1.88\n", "1.88"));
    }

    #[test]
    fn env_flag_truthiness() {
        let key = "SHELBI_MSRV_CHECK_TEST_FLAG";
        std::env::remove_var(key);
        assert!(!env_flag(key));
        std::env::set_var(key, "1");
        assert!(env_flag(key));
        std::env::set_var(key, "true");
        assert!(env_flag(key));
        std::env::set_var(key, "0");
        assert!(!env_flag(key));
        std::env::set_var(key, "false");
        assert!(!env_flag(key));
        std::env::set_var(key, "");
        assert!(!env_flag(key));
        std::env::remove_var(key);
    }

    #[test]
    fn find_rust_version_walks_up_to_the_workspace_root() {
        let base = std::env::temp_dir().join(format!(
            "shelbi-msrv-find-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let member = base.join("crates").join("member");
        std::fs::create_dir_all(&member).unwrap();
        // Workspace root owns the concrete MSRV; the member only inherits it.
        std::fs::write(
            base.join("Cargo.toml"),
            "[workspace.package]\nrust-version = \"1.88\"\n",
        )
        .unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nrust-version.workspace = true\n",
        )
        .unwrap();

        let (manifest, version) = find_rust_version(&member).expect("walks up to the root");
        assert_eq!(version, "1.88");
        assert_eq!(manifest, base.join("Cargo.toml"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
