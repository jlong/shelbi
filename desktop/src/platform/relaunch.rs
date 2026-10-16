//! Relaunching the app after the CLI was upgraded under it.
//!
//! From the `.app` bundle, `open -n` starts a fresh instance of the bundle on
//! disk (which `brew upgrade` has replaced); an unbundled `cargo run` binary
//! re-runs itself. The caller quits the old process right after.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Start a new instance of the app.
pub fn relaunch() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = match bundle_of(&exe) {
        Some(app) => {
            let mut open = Command::new("/usr/bin/open");
            open.arg("-n").arg(app);
            open
        }
        None => Command::new(&exe),
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The `.app` bundle holding `exe` (`X.app/Contents/MacOS/<exe>`), if any.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    is_bundle.then(|| app.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_enclosing_app_bundle() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Shelbi.app/Contents/MacOS/shelbi-desktop"
            )),
            Some(PathBuf::from("/Applications/Shelbi.app"))
        );
        assert_eq!(
            bundle_of(Path::new("/repo/desktop/target/debug/shelbi-desktop")),
            None
        );
    }
}
