//! Installing the `shelbi` CLI when discovery finds none (the first launch of
//! an app that didn't come with Homebrew's formula).
//!
//! The app downloads the prebuilt CLI from the official `jlong/shelbi`
//! GitHub release of **its own version** (never "latest": the app, the CLI
//! and the daemon must match exactly, `version`), checks the archive against
//! that release's `checksums.txt`, and installs the package the way
//! `scripts/install.sh` lays out a user install: the executable at
//! `~/bin/shelbi` and the system plugin under
//! `~/share/shelbi/plugins/update-shelbi-configuration/` (the CLI looks for
//! it at `<exe>/../../share/shelbi/plugins`). No Homebrew, no administrator
//! rights, no Rust toolchain.
//!
//! The release archive (`.goreleaser.yaml`) is `shelbi_Darwin_<arch>.tar.gz`
//! with `shelbi`, `LICENSE`, `README.md` and `plugins/…` at its root.
//!
//! Safety:
//!
//! - Everything is staged first, in a private directory beside the
//!   destination: downloaded, checksum-verified, unpacked, and the staged
//!   executable run (`--version` must name the app's version) before
//!   anything is published.
//! - Publishing swaps each piece in with a rename, keeping what was there
//!   until the new install has passed its post-install check; a failure puts
//!   it back. The executable is copied to a fresh file before its rename (a
//!   new inode: macOS kills a process whose signed file was rewritten in
//!   place, the reason `install.sh` re-signs after `cp`).
//! - One install at a time: a process-wide guard plus an advisory lock file,
//!   so a retry (or a second copy of the app) never runs a second job; the
//!   loser gets [`InstallErrorKind::Busy`]. Leftovers of an interrupted
//!   install are swept by the next one, under the lock.
//!
//! The download is `/usr/bin/curl` (as the update check is: present on every
//! Mac), the unpack `/usr/bin/tar`. [`Io`] is the seam tests replace.
//! `SHELBI_DESKTOP_CLI_DOWNLOAD_URL` points the download at another base
//! (any URL curl takes, `file://` included) holding the archive and
//! `checksums.txt`, for testing the flow against a local fixture.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::discovery::CLI_NAME;

/// Where the official releases' assets are, by tag.
pub const RELEASE_BASE: &str = "https://github.com/jlong/shelbi/releases/download";
/// The release's checksum file (`.goreleaser.yaml`'s `checksum.name_template`).
pub const CHECKSUMS: &str = "checksums.txt";
/// The system plugin's directory name, under `share/shelbi/plugins`.
pub const PLUGIN: &str = "update-shelbi-configuration";
/// The files the package's plugin must carry
/// (`scripts/release/verify-system-plugin-packages.sh`).
pub const PLUGIN_FILES: [&str; 3] = [
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    "skills/update-shelbi-configuration/SKILL.md",
];

const BASE_ENV: &str = "SHELBI_DESKTOP_CLI_DOWNLOAD_URL";
/// The lock file, in `~/share/shelbi`.
const LOCK_FILE: &str = ".desktop-install.lock";
/// The staging directory's prefix, in `~/share/shelbi`.
const STAGING_PREFIX: &str = ".desktop-install-";
/// A swapped-in piece's temporary and backup names carry these.
const NEW_MARK: &str = ".new-";
const OLD_MARK: &str = ".old-";

/// One install per process (the lock file covers other processes).
static INSTALLING: Mutex<()> = Mutex::new(());

/// What an install fetches and where it puts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The CLI version to install: the app's.
    pub version: String,
    /// `shelbi_Darwin_arm64.tar.gz`.
    pub archive: String,
    pub archive_url: String,
    pub checksums_url: String,
    /// `~/bin`: the executable's directory.
    pub bin_dir: PathBuf,
    /// `~/share/shelbi`: the plugins' parent, the lock and the staging area.
    pub share_dir: PathBuf,
}

impl Plan {
    /// The install of `version` for this Mac's `arch`
    /// (`std::env::consts::ARCH`) under `home`, from `base` (the release's
    /// download directory; `None`: the official release).
    pub fn new(version: &str, home: &Path, arch: &str, base: Option<&str>) -> Result<Plan, InstallError> {
        let archive = archive_name(arch).ok_or_else(|| {
            InstallError::new(
                InstallErrorKind::Unsupported,
                format!("Shelbi doesn't publish a CLI for this Mac's processor ({arch})."),
            )
        })?;
        let base = match base {
            Some(base) => base.trim_end_matches('/').to_string(),
            None => format!("{RELEASE_BASE}/v{version}"),
        };
        Ok(Plan {
            version: version.to_string(),
            archive_url: format!("{base}/{archive}"),
            checksums_url: format!("{base}/{CHECKSUMS}"),
            archive,
            bin_dir: home.join("bin"),
            share_dir: home.join("share/shelbi"),
        })
    }

    /// The plan for the running app on this machine: its version, `$HOME`,
    /// the `SHELBI_DESKTOP_CLI_DOWNLOAD_URL` override.
    pub fn for_app(version: &str, home: Option<&Path>) -> Result<Plan, InstallError> {
        let home = home.ok_or_else(|| {
            InstallError::new(
                InstallErrorKind::Unwritable,
                "HOME isn't set, so there's no folder to install the CLI in.",
            )
        })?;
        let base = std::env::var(BASE_ENV).ok().filter(|b| !b.trim().is_empty());
        Plan::new(version, home, std::env::consts::ARCH, base.as_deref())
    }

    /// Where the CLI ends up: `~/bin/shelbi`.
    pub fn cli_path(&self) -> PathBuf {
        self.bin_dir.join(CLI_NAME)
    }

    /// Where the system plugin ends up.
    pub fn plugin_dir(&self) -> PathBuf {
        self.share_dir.join("plugins").join(PLUGIN)
    }
}

/// The release archive for `arch`, as goreleaser names it.
pub fn archive_name(arch: &str) -> Option<String> {
    let arch = match arch {
        "aarch64" | "arm64" => "arm64",
        "x86_64" => "x86_64",
        _ => return None,
    };
    Some(format!("shelbi_Darwin_{arch}.tar.gz"))
}

/// Where an install is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Downloading,
    /// Checking the archive's checksum and the unpacked package.
    Verifying,
    /// Publishing the package and running the installed CLI.
    Installing,
}

/// Why an install failed. Each kind names its own recovery on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallErrorKind {
    /// No package for this Mac's processor.
    Unsupported,
    /// The download didn't complete (offline, DNS, TLS, a server error).
    Network,
    /// The release has no such asset (a 404): this app's version was never
    /// published (a development build), or the release is incomplete.
    NotPublished,
    /// The archive doesn't match `checksums.txt`, or isn't listed in it.
    Integrity,
    /// A destination can't be written.
    Unwritable,
    /// The archive unpacked to something that isn't the CLI package, or its
    /// executable doesn't report the expected version.
    InvalidPackage,
    /// The installed CLI didn't answer `--version` afterwards (rolled back).
    PostInstallCheck,
    /// Another install is running.
    Busy,
    /// Anything else (an I/O error mid-install).
    Failed,
}

/// A failed install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError {
    pub kind: InstallErrorKind,
    /// One sentence for the screen.
    pub message: String,
    /// Lines of detail (a URL, a path, curl's error).
    pub detail: Vec<String>,
}

impl InstallError {
    pub fn new(kind: InstallErrorKind, message: impl Into<String>) -> Self {
        InstallError {
            kind,
            message: message.into(),
            detail: Vec::new(),
        }
    }

    fn with(mut self, line: impl Into<String>) -> Self {
        self.detail.push(line.into());
        self
    }

    pub fn with_detail(mut self, lines: impl IntoIterator<Item = String>) -> Self {
        self.detail.extend(lines);
        self
    }
}

/// Why a download failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// The server answered 404.
    NotFound,
    /// Anything else, in curl's words.
    Network(String),
}

/// What an install needs from the machine besides the filesystem.
pub trait Io {
    /// Download `url` to `dest`.
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), FetchError>;
    /// `<cli> --version`'s version, when it runs and answers.
    fn cli_version(&self, cli: &Path) -> Option<String>;
}

/// Install `plan`, calling `progress` as each step starts. Blocks (call it off
/// the UI thread). Returns the installed CLI's path.
pub fn install(plan: &Plan, io: &impl Io, progress: &mut dyn FnMut(Step)) -> Result<PathBuf, InstallError> {
    let Ok(_process) = INSTALLING.try_lock() else {
        return Err(busy());
    };
    create_dir(&plan.share_dir)?;
    let lock = File::create(plan.share_dir.join(LOCK_FILE)).map_err(|e| io_error(e, &plan.share_dir))?;
    if lock.try_lock().is_err() {
        return Err(busy());
    }
    sweep_leftovers(plan);

    let staging = Staging::new(&plan.share_dir)?;
    progress(Step::Downloading);
    let checksums = staging.path.join(CHECKSUMS);
    fetch(io, &plan.checksums_url, &checksums)?;
    let archive = staging.path.join(&plan.archive);
    fetch(io, &plan.archive_url, &archive)?;

    progress(Step::Verifying);
    verify_checksum(&archive, &plan.archive, &checksums)?;
    let package = staging.path.join("package");
    unpack(&archive, &package)?;
    let staged_cli = validate_package(&package, &plan.version, io)?;

    progress(Step::Installing);
    publish(plan, &staged_cli, &package.join("plugins").join(PLUGIN), io)
    // `_process`, `lock` and `staging` drop here: the staging directory goes.
}

fn busy() -> InstallError {
    InstallError::new(
        InstallErrorKind::Busy,
        "Another Shelbi window is already installing the CLI. Wait for it to finish, then try again.",
    )
}

fn fetch(io: &impl Io, url: &str, dest: &Path) -> Result<(), InstallError> {
    io.fetch(url, dest).map_err(|e| match e {
        FetchError::NotFound => InstallError::new(
            InstallErrorKind::NotPublished,
            "This version of Shelbi has no CLI download on GitHub.",
        )
        .with(url),
        FetchError::Network(why) => InstallError::new(
            InstallErrorKind::Network,
            "The CLI couldn't be downloaded from GitHub.",
        )
        .with(url)
        .with(why),
    })
}

/// The archive's SHA-256 must be the one `checksums.txt` lists for its name.
fn verify_checksum(archive: &Path, name: &str, checksums: &Path) -> Result<(), InstallError> {
    let listing = fs::read_to_string(checksums).map_err(|e| io_error(e, checksums))?;
    let Some(expected) = expected_checksum(&listing, name) else {
        return Err(InstallError::new(
            InstallErrorKind::Integrity,
            format!("The release's {CHECKSUMS} doesn't list {name}, so the download can't be verified."),
        ));
    };
    let actual = sha256_file(archive).map_err(|e| io_error(e, archive))?;
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(InstallError::new(
            InstallErrorKind::Integrity,
            "The downloaded CLI doesn't match the release's checksum, so it wasn't installed.",
        )
        .with(format!("expected {expected}"))
        .with(format!("got      {actual}")));
    }
    Ok(())
}

/// `name`'s hash in a `sha256sum`-style listing (`<hex>  <name>`, the name
/// possibly marked binary with a leading `*`).
pub fn expected_checksum(listing: &str, name: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let hash = words.next()?;
        let file = words.next()?.trim_start_matches('*');
        (file == name && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// `tar -xzf archive -C dest` (bsdtar refuses `..` and strips a leading `/`
/// without `-P`).
fn unpack(archive: &Path, dest: &Path) -> Result<(), InstallError> {
    create_dir(dest)?;
    let out = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| InstallError::new(InstallErrorKind::Failed, format!("tar didn't run: {e}")))?;
    if !out.status.success() {
        return Err(InstallError::new(
            InstallErrorKind::InvalidPackage,
            "The downloaded CLI archive couldn't be unpacked.",
        )
        .with(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(())
}

/// The unpacked package has the executable (a regular file, which runs and
/// reports `version`) and the complete plugin. Returns the executable.
fn validate_package(package: &Path, version: &str, io: &impl Io) -> Result<PathBuf, InstallError> {
    let invalid = |what: String| {
        InstallError::new(
            InstallErrorKind::InvalidPackage,
            "The downloaded archive isn't a complete Shelbi CLI package.",
        )
        .with(what)
    };
    let cli = package.join(CLI_NAME);
    if !is_regular_file(&cli) {
        return Err(invalid(format!("no `{CLI_NAME}` executable at its root")));
    }
    let plugin = package.join("plugins").join(PLUGIN);
    for file in PLUGIN_FILES {
        if !is_regular_file(&plugin.join(file)) {
            return Err(invalid(format!("missing plugins/{PLUGIN}/{file}")));
        }
    }
    set_executable(&cli).map_err(|e| io_error(e, &cli))?;
    match io.cli_version(&cli) {
        Some(v) if v == version => Ok(cli),
        Some(v) => Err(InstallError::new(
            InstallErrorKind::InvalidPackage,
            format!("The downloaded CLI is version {v}, not this app's {version}."),
        )),
        None => Err(InstallError::new(
            InstallErrorKind::InvalidPackage,
            "The downloaded CLI doesn't run on this Mac.",
        )),
    }
}

/// Swap the plugin, then the executable, in; run the installed CLI; on any
/// failure put back what was there.
fn publish(plan: &Plan, staged_cli: &Path, staged_plugin: &Path, io: &impl Io) -> Result<PathBuf, InstallError> {
    let pid = std::process::id();
    let plugins = plan.share_dir.join("plugins");
    create_dir(&plugins)?;
    let plugin = plan.plugin_dir();
    let plugin_new = plugins.join(format!(".{PLUGIN}{NEW_MARK}{pid}"));
    let plugin_old = plugins.join(format!(".{PLUGIN}{OLD_MARK}{pid}"));
    copy_tree(staged_plugin, &plugin_new).map_err(|e| {
        let _ = fs::remove_dir_all(&plugin_new);
        io_error(e, &plugins)
    })?;
    let had_plugin = plugin.exists();
    if had_plugin {
        fs::rename(&plugin, &plugin_old).map_err(|e| {
            let _ = fs::remove_dir_all(&plugin_new);
            io_error(e, &plugins)
        })?;
    }
    let restore_plugin = || {
        let _ = fs::remove_dir_all(&plugin);
        if had_plugin {
            let _ = fs::rename(&plugin_old, &plugin);
        }
    };
    if let Err(e) = fs::rename(&plugin_new, &plugin) {
        let _ = fs::remove_dir_all(&plugin_new);
        restore_plugin();
        return Err(io_error(e, &plugins));
    }

    let cli = plan.cli_path();
    let cli_new = plan.bin_dir.join(format!(".{CLI_NAME}{NEW_MARK}{pid}"));
    let cli_old = plan.bin_dir.join(format!(".{CLI_NAME}{OLD_MARK}{pid}"));
    let placed = create_dir(&plan.bin_dir).and_then(|()| {
        fs::copy(staged_cli, &cli_new)
            .and_then(|_| set_executable(&cli_new))
            .map_err(|e| io_error(e, &plan.bin_dir))
    });
    if let Err(e) = placed {
        let _ = fs::remove_file(&cli_new);
        restore_plugin();
        return Err(e);
    }
    // A second name for what's there, so the rename below stays atomic and
    // a failed check can put it back.
    let had_cli = fs::symlink_metadata(&cli).is_ok() && fs::hard_link(&cli, &cli_old).is_ok();
    if let Err(e) = fs::rename(&cli_new, &cli) {
        let _ = fs::remove_file(&cli_new);
        let _ = fs::remove_file(&cli_old);
        restore_plugin();
        return Err(io_error(e, &plan.bin_dir));
    }

    if io.cli_version(&cli).as_deref() != Some(plan.version.as_str()) {
        if had_cli {
            let _ = fs::rename(&cli_old, &cli);
        } else {
            let _ = fs::remove_file(&cli);
        }
        restore_plugin();
        return Err(InstallError::new(
            InstallErrorKind::PostInstallCheck,
            format!("The installed CLI didn't answer `{CLI_NAME} --version`, so the install was undone."),
        )
        .with(cli.display().to_string()));
    }
    let _ = fs::remove_file(&cli_old);
    let _ = fs::remove_dir_all(&plugin_old);
    Ok(cli)
}

/// Remove what an interrupted install left (call with the lock held): stale
/// staging directories, and temporaries and backups beside the destinations.
/// A published piece is never touched: each swap is a rename.
fn sweep_leftovers(plan: &Plan) {
    let swept = |dir: &Path, prefixes: &[String]| {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if prefixes.iter().any(|p| name.starts_with(p.as_str())) {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir_all(&path);
                } else {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    };
    swept(&plan.share_dir, &[STAGING_PREFIX.to_string()]);
    swept(
        &plan.share_dir.join("plugins"),
        &[format!(".{PLUGIN}{NEW_MARK}"), format!(".{PLUGIN}{OLD_MARK}")],
    );
    swept(
        &plan.bin_dir,
        &[format!(".{CLI_NAME}{NEW_MARK}"), format!(".{CLI_NAME}{OLD_MARK}")],
    );
}

/// A private staging directory, removed on drop.
struct Staging {
    path: PathBuf,
}

impl Staging {
    fn new(parent: &Path) -> Result<Staging, InstallError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = parent.join(format!("{STAGING_PREFIX}{}-{nanos}", std::process::id()));
        fs::create_dir(&path).map_err(|e| io_error(e, parent))?;
        Ok(Staging { path })
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Copy a directory of regular files (a symlink in the package is refused).
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &dest)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} isn't a regular file", entry.path().display()),
            ));
        }
    }
    Ok(())
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

fn create_dir(dir: &Path) -> Result<(), InstallError> {
    fs::create_dir_all(dir).map_err(|e| io_error(e, dir))
}

/// An I/O error at `path`: unwritable when the filesystem said so.
fn io_error(e: io::Error, path: &Path) -> InstallError {
    let unwritable = matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    ) || e.raw_os_error() == Some(1); // EPERM
    if unwritable {
        InstallError::new(
            InstallErrorKind::Unwritable,
            format!("Shelbi can't write to {}.", path.display()),
        )
        .with(e.to_string())
    } else {
        InstallError::new(InstallErrorKind::Failed, format!("The install failed at {}.", path.display()))
            .with(e.to_string())
    }
}

/// The real downloader and runner.
pub struct SystemIo {
    /// The environment `shelbi --version` runs under.
    pub env: super::env::Env,
}

impl Io for SystemIo {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), FetchError> {
        let mut cmd = Command::new("/usr/bin/curl");
        cmd.args(["--fail", "--silent", "--show-error", "--location"])
            .args(["--connect-timeout", "20", "--max-time", "600", "--retry", "2"])
            .args(["--write-out", "%{http_code}"]);
        // The official release only over HTTPS (redirects included); an
        // override may be a local fixture.
        if url.starts_with("https://") {
            cmd.args(["--proto", "=https", "--proto-redir", "=https"]);
        }
        let out = cmd
            .arg("--output")
            .arg(dest)
            .arg(url)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| FetchError::Network(format!("curl didn't run: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if code == "404" {
            return Err(FetchError::NotFound);
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(FetchError::Network(if stderr.is_empty() {
            format!("curl exited with {} (HTTP {code})", out.status)
        } else {
            stderr
        }))
    }

    fn cli_version(&self, cli: &Path) -> Option<String> {
        super::discovery::cli_version(cli, &self.env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    const VERSION: &str = "9.8.7";

    /// A temporary home, removed on drop.
    struct Home(PathBuf);

    impl Home {
        fn new(tag: &str) -> Home {
            let dir = std::env::temp_dir().join(format!(
                "shb-dt-install-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&dir).unwrap();
            Home(dir)
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            make_writable(&self.0);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn make_writable(dir: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    make_writable(&e.path());
                }
            }
        }
    }

    /// A release on disk: a package with an executable that prints
    /// `shelbi <version>`, archived the way goreleaser does, and its
    /// checksums.
    struct Release {
        archive: PathBuf,
        checksums: PathBuf,
    }

    fn release(home: &Path, version: &str, tweak: impl FnOnce(&Path)) -> Release {
        let pkg = home.join("release-pkg");
        let plugin = pkg.join("plugins").join(PLUGIN);
        for file in PLUGIN_FILES {
            let path = plugin.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("{file} {version}")).unwrap();
        }
        fs::write(pkg.join("LICENSE"), "MIT").unwrap();
        fs::write(pkg.join("README.md"), "# Shelbi").unwrap();
        let cli = pkg.join(CLI_NAME);
        fs::write(&cli, format!("#!/bin/sh\necho \"shelbi {version}\"\n")).unwrap();
        set_executable(&cli).unwrap();
        tweak(&pkg);
        let out = home.join("release");
        fs::create_dir_all(&out).unwrap();
        let archive = out.join("shelbi_Darwin_arm64.tar.gz");
        let status = Command::new("/usr/bin/tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&pkg)
            .args(["shelbi", "LICENSE", "README.md", "plugins"])
            .status()
            .unwrap();
        assert!(status.success());
        let checksums = out.join(CHECKSUMS);
        fs::write(
            &checksums,
            format!(
                "{}  shelbi_Darwin_x86_64.tar.gz\n{}  shelbi_Darwin_arm64.tar.gz\n",
                "0".repeat(64),
                sha256_file(&archive).unwrap()
            ),
        )
        .unwrap();
        let _ = fs::remove_dir_all(&pkg);
        Release { archive, checksums }
    }

    /// Serves URLs from files; runs `--version` for real (the fixture's
    /// script), unless told an installed path is broken.
    struct FakeIo {
        files: BTreeMap<String, Result<PathBuf, FetchError>>,
        fetched: RefCell<Vec<String>>,
        broken: Option<PathBuf>,
        /// Run between the downloads (a test's chance to race).
        during: Cell<Option<fn()>>,
    }

    impl FakeIo {
        fn serving(plan: &Plan, release: &Release) -> FakeIo {
            FakeIo {
                files: [
                    (plan.archive_url.clone(), Ok(release.archive.clone())),
                    (plan.checksums_url.clone(), Ok(release.checksums.clone())),
                ]
                .into(),
                fetched: RefCell::default(),
                broken: None,
                during: Cell::new(None),
            }
        }
    }

    impl Io for FakeIo {
        fn fetch(&self, url: &str, dest: &Path) -> Result<(), FetchError> {
            self.fetched.borrow_mut().push(url.to_string());
            if let Some(f) = self.during.take() {
                f();
            }
            match self.files.get(url) {
                Some(Ok(src)) => fs::copy(src, dest).map(|_| ()).map_err(|e| FetchError::Network(e.to_string())),
                Some(Err(e)) => Err(e.clone()),
                None => Err(FetchError::NotFound),
            }
        }
        fn cli_version(&self, cli: &Path) -> Option<String> {
            if self.broken.as_deref() == Some(cli) {
                return None;
            }
            let env = [("PATH".to_string(), "/usr/bin:/bin".to_string())].into();
            super::super::discovery::cli_version(cli, &env)
        }
    }

    fn plan(home: &Path) -> Plan {
        Plan::new(VERSION, home, "aarch64", None).unwrap()
    }

    fn run(plan: &Plan, io: &FakeIo) -> (Vec<Step>, Result<PathBuf, InstallError>) {
        let mut steps = Vec::new();
        let result = install(plan, io, &mut |s| steps.push(s));
        (steps, result)
    }

    /// Installs share the process-wide guard, so they run one at a time
    /// (with bootstrap's integration tests, which install too).
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        crate::test_env::lock()
    }

    fn version_of(path: &Path) -> Option<String> {
        let env = [("PATH".to_string(), "/usr/bin:/bin".to_string())].into();
        super::super::discovery::cli_version(path, &env)
    }

    fn leftovers(home: &Path) -> Vec<String> {
        let mut out = Vec::new();
        for dir in [home.join("bin"), home.join("share/shelbi"), home.join("share/shelbi/plugins")] {
            if let Ok(entries) = fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.contains(NEW_MARK) || name.contains(OLD_MARK) || name.starts_with(STAGING_PREFIX) {
                        out.push(name);
                    }
                }
            }
        }
        out
    }

    /// The real contract, end to end: the app's own release from GitHub,
    /// into a temporary home. Needs the network, so it only runs when asked
    /// (`cargo test live_release -- --ignored`).
    #[test]
    #[ignore = "downloads the app's release from GitHub"]
    fn live_release_installs_into_a_temporary_home() {
        let _serial = serial();
        let home = Home::new("live");
        let plan = Plan::new(super::super::version::APP_VERSION, &home.0, std::env::consts::ARCH, None).unwrap();
        let io = SystemIo {
            env: [("PATH".to_string(), "/usr/bin:/bin".to_string())].into(),
        };
        let mut steps = Vec::new();
        let cli = install(&plan, &io, &mut |s| steps.push(s)).unwrap();
        assert_eq!(steps, [Step::Downloading, Step::Verifying, Step::Installing]);
        assert_eq!(version_of(&cli).as_deref(), Some(super::super::version::APP_VERSION));
        for file in PLUGIN_FILES {
            assert!(plan.plugin_dir().join(file).is_file(), "{file}");
        }
        assert!(leftovers(&home.0).is_empty(), "{:?}", leftovers(&home.0));
    }

    #[test]
    fn the_plan_pins_the_apps_release_and_the_user_prefix() {
        let p = Plan::new("0.12.0", Path::new("/Users/me"), "aarch64", None).unwrap();
        assert_eq!(
            p.archive_url,
            "https://github.com/jlong/shelbi/releases/download/v0.12.0/shelbi_Darwin_arm64.tar.gz"
        );
        assert_eq!(
            p.checksums_url,
            "https://github.com/jlong/shelbi/releases/download/v0.12.0/checksums.txt"
        );
        assert_eq!(p.cli_path(), PathBuf::from("/Users/me/bin/shelbi"));
        assert_eq!(
            p.plugin_dir(),
            PathBuf::from("/Users/me/share/shelbi/plugins/update-shelbi-configuration")
        );
        assert_eq!(archive_name("x86_64").as_deref(), Some("shelbi_Darwin_x86_64.tar.gz"));
        let err = Plan::new("0.12.0", Path::new("/Users/me"), "riscv64", None).unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::Unsupported);
        let local = Plan::new("0.12.0", Path::new("/h"), "arm64", Some("file:///tmp/rel/")).unwrap();
        assert_eq!(local.archive_url, "file:///tmp/rel/shelbi_Darwin_arm64.tar.gz");
    }

    #[test]
    fn checksum_listings_parse() {
        let hash = "a".repeat(64);
        let listing = format!("{}  other.tar.gz\n{hash} *shelbi_Darwin_arm64.tar.gz\n", "b".repeat(64));
        assert_eq!(expected_checksum(&listing, "shelbi_Darwin_arm64.tar.gz"), Some(hash));
        assert_eq!(expected_checksum(&listing, "missing.tar.gz"), None);
        assert_eq!(expected_checksum("short  shelbi_Darwin_arm64.tar.gz", "shelbi_Darwin_arm64.tar.gz"), None);
    }

    #[test]
    fn a_first_install_publishes_the_cli_and_the_plugin() {
        let _serial = serial();
        let home = Home::new("first");
        let rel = release(&home.0, VERSION, |_| {});
        let plan = plan(&home.0);
        let io = FakeIo::serving(&plan, &rel);
        let (steps, result) = run(&plan, &io);
        assert_eq!(steps, [Step::Downloading, Step::Verifying, Step::Installing]);
        let cli = result.unwrap();
        assert_eq!(cli, home.0.join("bin/shelbi"));
        assert!(super::super::discovery::is_executable_file(&cli));
        assert_eq!(version_of(&cli).as_deref(), Some(VERSION));
        for file in PLUGIN_FILES {
            assert!(plan.plugin_dir().join(file).is_file(), "{file}");
        }
        assert!(leftovers(&home.0).is_empty(), "{:?}", leftovers(&home.0));
        // The checksums come first: nothing is trusted before them.
        assert_eq!(*io.fetched.borrow(), [plan.checksums_url.clone(), plan.archive_url.clone()]);
    }

    #[test]
    fn a_checksum_mismatch_installs_nothing_and_keeps_what_was_there() {
        let _serial = serial();
        let home = Home::new("sum");
        let plan = plan(&home.0);
        // An earlier plugin install (a source install's, say).
        fs::create_dir_all(plan.plugin_dir()).unwrap();
        fs::write(plan.plugin_dir().join("keep"), "old").unwrap();
        let rel = release(&home.0, VERSION, |_| {});
        fs::write(&rel.checksums, format!("{}  shelbi_Darwin_arm64.tar.gz\n", "f".repeat(64))).unwrap();
        let (_, result) = run(&plan, &FakeIo::serving(&plan, &rel));
        assert_eq!(result.unwrap_err().kind, InstallErrorKind::Integrity);
        assert!(!plan.cli_path().exists());
        assert_eq!(fs::read_to_string(plan.plugin_dir().join("keep")).unwrap(), "old");

        fs::write(&rel.checksums, "").unwrap();
        let (_, result) = run(&plan, &FakeIo::serving(&plan, &rel));
        assert_eq!(result.unwrap_err().kind, InstallErrorKind::Integrity, "unlisted");
        assert!(leftovers(&home.0).is_empty());
    }

    #[test]
    fn network_and_missing_release_failures_name_their_cause() {
        let _serial = serial();
        let home = Home::new("net");
        let plan = plan(&home.0);
        let rel = release(&home.0, VERSION, |_| {});
        let mut io = FakeIo::serving(&plan, &rel);
        io.files.insert(
            plan.checksums_url.clone(),
            Err(FetchError::Network("Could not resolve host: github.com".into())),
        );
        let err = run(&plan, &io).1.unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::Network);
        assert!(err.detail.iter().any(|l| l.contains("Could not resolve host")), "{err:?}");

        io.files.insert(plan.checksums_url.clone(), Err(FetchError::NotFound));
        let err = run(&plan, &io).1.unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::NotPublished);
        assert!(err.detail.contains(&plan.checksums_url));
        assert!(!plan.cli_path().exists());
        assert!(leftovers(&home.0).is_empty());
    }

    #[test]
    fn a_package_without_the_plugin_or_with_another_version_is_refused() {
        let _serial = serial();
        let home = Home::new("pkg");
        let plan = plan(&home.0);
        let rel = release(&home.0, VERSION, |pkg| {
            fs::remove_file(pkg.join("plugins").join(PLUGIN).join(PLUGIN_FILES[2])).unwrap();
        });
        let err = run(&plan, &FakeIo::serving(&plan, &rel)).1.unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::InvalidPackage);
        assert!(err.detail[0].contains("SKILL.md"), "{err:?}");

        let rel = release(&home.0, "0.0.1", |_| {});
        let err = run(&plan, &FakeIo::serving(&plan, &rel)).1.unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::InvalidPackage);
        assert!(err.message.contains("0.0.1"), "{err:?}");
        assert!(!plan.cli_path().exists());
    }

    #[test]
    fn an_unwritable_bin_dir_fails_and_rolls_the_plugin_back() {
        use std::os::unix::fs::PermissionsExt;
        let _serial = serial();
        let home = Home::new("ro");
        let plan = plan(&home.0);
        fs::create_dir_all(plan.plugin_dir()).unwrap();
        fs::write(plan.plugin_dir().join("keep"), "old").unwrap();
        fs::create_dir_all(&plan.bin_dir).unwrap();
        fs::set_permissions(&plan.bin_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let rel = release(&home.0, VERSION, |_| {});
        let err = run(&plan, &FakeIo::serving(&plan, &rel)).1.unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::Unwritable, "{err:?}");
        assert!(err.message.contains("/bin"), "{err:?}");
        assert_eq!(fs::read_to_string(plan.plugin_dir().join("keep")).unwrap(), "old");
        assert!(!plan.cli_path().exists());
        // Writable again, the retry installs.
        fs::set_permissions(&plan.bin_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let cli = run(&plan, &FakeIo::serving(&plan, &rel)).1.unwrap();
        assert_eq!(version_of(&cli).as_deref(), Some(VERSION));
        assert!(!plan.plugin_dir().join("keep").exists(), "the plugin is the package's");
        assert!(leftovers(&home.0).is_empty(), "{:?}", leftovers(&home.0));
    }

    #[test]
    fn a_failed_post_install_check_puts_the_previous_files_back() {
        let _serial = serial();
        let home = Home::new("post");
        let plan = plan(&home.0);
        // What was there: an older CLI and plugin.
        let old = release(&home.0, "1.0.0", |_| {});
        let mut io = FakeIo::serving(&plan, &old);
        let mut older = plan.clone();
        older.version = "1.0.0".into();
        run(&older, &io).1.unwrap();

        let rel = release(&home.0, VERSION, |_| {});
        io = FakeIo::serving(&plan, &rel);
        io.broken = Some(plan.cli_path());
        let err = run(&plan, &io).1.unwrap_err();
        assert_eq!(err.kind, InstallErrorKind::PostInstallCheck);
        assert_eq!(version_of(&plan.cli_path()).as_deref(), Some("1.0.0"));
        assert_eq!(
            fs::read_to_string(plan.plugin_dir().join(PLUGIN_FILES[0])).unwrap(),
            format!("{} 1.0.0", PLUGIN_FILES[0])
        );
        assert!(leftovers(&home.0).is_empty(), "{:?}", leftovers(&home.0));
    }

    #[test]
    fn a_second_install_while_one_runs_is_busy_and_an_interrupted_ones_leftovers_go() {
        let _serial = serial();
        let home = Home::new("busy");
        let plan = plan(&home.0);
        let rel = release(&home.0, VERSION, |_| {});
        // Another process holds the lock.
        fs::create_dir_all(&plan.share_dir).unwrap();
        let held = File::create(plan.share_dir.join(LOCK_FILE)).unwrap();
        held.lock().unwrap();
        let io = FakeIo::serving(&plan, &rel);
        assert_eq!(run(&plan, &io).1.unwrap_err().kind, InstallErrorKind::Busy);
        assert!(io.fetched.borrow().is_empty(), "nothing downloaded");
        drop(held);

        // In this process: the guard is held while an install runs.
        let io = FakeIo::serving(&plan, &rel);
        io.during.set(Some(|| {
            assert!(INSTALLING.try_lock().is_err(), "the guard is held mid-install");
        }));
        // An interrupted install's debris.
        fs::create_dir_all(plan.share_dir.join(format!("{STAGING_PREFIX}1-2"))).unwrap();
        fs::create_dir_all(&plan.bin_dir).unwrap();
        fs::write(plan.bin_dir.join(format!(".shelbi{NEW_MARK}1")), "half").unwrap();
        run(&plan, &io).1.unwrap();
        assert!(leftovers(&home.0).is_empty(), "{:?}", leftovers(&home.0));
    }
}
