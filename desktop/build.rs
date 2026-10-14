//! Embed `macos/Info.plist` in the binary's `__TEXT,__info_plist` section.
//!
//! macOS names an unbundled executable's app menu (the bold name in the menu
//! bar) from the main bundle's `CFBundleName`, and for a bare binary the
//! main bundle's Info.plist is this embedded section. Without it `cargo run`
//! shows "shelbi-desktop" in the menu bar. `dt-packaging` builds the real
//! `.app` with its own Info.plist; this one only covers running unbundled.

fn main() {
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
