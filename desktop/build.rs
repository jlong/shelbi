//! Two jobs: embed `macos/Info.plist` in the binary's `__TEXT,__info_plist`
//! section, and generate Acknowledgements' crate list ([`licenses`]). About's
//! copyright year is `shelbi-app`'s (its build script sets it), shared with
//! the TUI.
//!
//! macOS names an unbundled executable's app menu (the bold name in the menu
//! bar) from the main bundle's `CFBundleName`, and for a bare binary the
//! main bundle's Info.plist is this embedded section. Without it `cargo run`
//! shows "shelbi-desktop" in the menu bar. `scripts/bundle.sh` builds the
//! real `.app` with its own Info.plist (`macos/Shelbi.app.plist`), which
//! wins inside the bundle; this one only covers running unbundled.

#[path = "../crates/shelbi-app/build_support/licenses.rs"]
mod licenses;

/// The app's list: the crates `shelbi_app::licenses::shared_entries` shows
/// (`SHARED_FEATURED` / `SHARED_FILES`) and the app's own featured crates
/// (`src/licenses.rs`). gpui-kit's package ships no license file, so its
/// entry shows gpui-base's (the same repository's Apache License); Lucide's
/// icons ship inside gpui-kit-assets.
const LICENSES: licenses::Config = licenses::Config {
    root: None,
    subject: "the app",
    long_tail_skip: &[
        "alacritty_terminal",
        "nucleo-matcher",
        "option-ext",
        "portable-pty",
        "termwiz",
        "gpui-pre",
        "gpui-kit",
    ],
    featured_files: &[
        ("alacritty_terminal", "LICENSE-APACHE"),
        ("nucleo-matcher", "LICENSE"),
        ("option-ext", "LICENSE.txt"),
        ("portable-pty", "LICENSE.md"),
        ("termwiz", "LICENSE.md"),
        ("gpui-pre", "LICENSE-APACHE"),
        ("gpui-base", "LICENSE-APACHE"),
        ("gpui-kit-assets", "LICENSE-LUCIDE"),
    ],
    rerun_if_changed: &["Cargo.lock", "Cargo.toml", "../crates/shelbi-app/build_support/licenses.rs"],
};

fn main() {
    licenses::generate(&LICENSES);
    println!("cargo:rerun-if-changed=macos/Info.plist");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("macos/Info.plist");
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }
}
