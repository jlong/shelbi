//! Three jobs: embed `macos/Info.plist` in the binary's `__TEXT,__info_plist`
//! section, generate Bundled Software's crate list ([`licenses`]), and set
//! `SHELBI_BUILD_YEAR`, the year About's copyright line shows
//! (`SOURCE_DATE_EPOCH`'s year when set, for a reproducible build, else
//! the clock's).
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
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rustc-env=SHELBI_BUILD_YEAR={}", build_year());
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

/// The UTC year of `SOURCE_DATE_EPOCH`, else of now.
fn build_year() -> i64 {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64)
        });
    // Days since 1970-01-01 to a civil year (Howard Hinnant's algorithm).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + i64::from(month <= 2)
}
