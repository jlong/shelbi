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

#[path = "build_support/licenses.rs"]
mod licenses;

fn main() {
    licenses::generate();
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
