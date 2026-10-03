//! `shelbi machine setup|status`: find or install a compatible `shelbi`
//! binary on a remote machine (Phase 5 of the remove-tmux plan — see
//! `docs/removing-tmux/README.md` and the plan's "Remote machines" section).
//!
//! The hub needs each remote to carry a `shelbi` binary it can drive over
//! `ssh <host> shelbi relay` (wired by the sibling `rt-relay` subtask). This
//! module answers two questions without touching dispatch:
//!
//! * **Finding the binary** — probe the remote through its *interactive login
//!   shell* (`$SHELL -l -i -c 'command -v shelbi'`), because a plain SSH
//!   command does not load the user's PATH. A compatible binary found there is
//!   used as-is; otherwise the resolved path is `~/.shelbi/bin/shelbi`. The
//!   answer is recorded per machine ([`shelbi_state::machine_state`]).
//! * **Installing** — detect the remote's OS/arch, fetch the matching release
//!   artifact for the hub's version, verify its checksum, and install it to
//!   `~/.shelbi/bin/shelbi`. A package-manager install that is too old is never
//!   overwritten; the managed copy sits alongside it and the output says so.
//!
//! ## Seams
//!
//! Every remote interaction goes through [`RemoteExec`] and every hub-side
//! download through [`HubFetch`], so the whole flow is unit-testable with fakes
//! and needs no real remote or network. The real implementations ([`SshExec`],
//! [`CurlHubFetch`]) shell out to the host's `ssh`/`curl`/`shasum`, matching
//! the rest of Shelbi's "drive the host's own tools" design.

use std::io;
use std::time::Duration;

use shelbi_core::Host;
use shelbi_state::machine_state::{MachineRecord, SOURCE_PATH, SOURCE_SHELBI_BIN};

// ===========================================================================
// Platform + release-artifact selection (pure)
// ===========================================================================

/// The operating systems Shelbi publishes release artifacts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Darwin,
    Linux,
}

/// The CPU architectures Shelbi publishes release artifacts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Arm64,
}

/// A resolved remote platform. Not every `(os, arch)` pair has a published
/// artifact (there is no Linux/arm64 build today); see [`Platform::artifact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: Os,
    pub arch: Arch,
}

/// Why a remote's reported platform has no installable artifact. Carries the
/// raw `uname` tokens so the error message can name exactly what was seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// `uname -s` was not `Darwin` or `Linux`.
    UnsupportedOs(String),
    /// `uname -m` was not an x86_64/arm64 synonym.
    UnsupportedArch(String),
    /// A recognized OS and arch, but no published artifact for the pair
    /// (Linux/arm64 today).
    UnsupportedCombo { os: Os, arch: Arch },
}

impl Os {
    /// The token Shelbi's release artifacts use (`shelbi_<Os>_<arch>.tar.gz`).
    fn artifact_token(self) -> &'static str {
        match self {
            Os::Darwin => "Darwin",
            Os::Linux => "Linux",
        }
    }
}

impl Arch {
    fn artifact_token(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Arm64 => "arm64",
        }
    }
}

impl std::fmt::Display for Os {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.artifact_token())
    }
}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.artifact_token())
    }
}

/// Map raw `uname -s` / `uname -m` output to a [`Platform`]. Case- and
/// whitespace-insensitive; accepts the common arch synonyms (`amd64` for
/// x86_64, `aarch64` for arm64) so detection works across Linux distros and
/// macOS. Errors name the exact token so the CLI can print what it saw.
pub fn detect_platform(uname_s: &str, uname_m: &str) -> Result<Platform, PlatformError> {
    let s = uname_s.trim();
    let m = uname_m.trim();
    let os = match s.to_ascii_lowercase().as_str() {
        "darwin" => Os::Darwin,
        "linux" => Os::Linux,
        _ => return Err(PlatformError::UnsupportedOs(s.to_string())),
    };
    let arch = match m.to_ascii_lowercase().as_str() {
        "x86_64" | "amd64" => Arch::X86_64,
        "arm64" | "aarch64" => Arch::Arm64,
        _ => return Err(PlatformError::UnsupportedArch(m.to_string())),
    };
    Ok(Platform { os, arch })
}

impl Platform {
    /// The release-artifact filename for this platform, or a
    /// [`PlatformError::UnsupportedCombo`] when the pair has no published
    /// build. The name matches `.goreleaser.yaml`'s template:
    /// `shelbi_<Darwin|Linux>_<x86_64|arm64>.tar.gz`.
    pub fn artifact(self) -> Result<String, PlatformError> {
        // The only recognized-but-unbuilt pair today.
        if let (Os::Linux, Arch::Arm64) = (self.os, self.arch) {
            return Err(PlatformError::UnsupportedCombo {
                os: self.os,
                arch: self.arch,
            });
        }
        Ok(format!(
            "shelbi_{}_{}.tar.gz",
            self.os.artifact_token(),
            self.arch.artifact_token()
        ))
    }
}

/// The GitHub `owner/repo` the release artifacts live in. Overridable via
/// `$SHELBI_RELEASE_REPO` for forks and tests.
fn release_repo() -> String {
    std::env::var("SHELBI_RELEASE_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "jlong/shelbi".to_string())
}

/// Base URL for a version's release assets. `$SHELBI_RELEASE_BASE_URL`, when
/// set, replaces the whole base (used by the no-network fallback tests and by
/// anyone mirroring releases); otherwise it is the GitHub releases download
/// path for tag `v<version>`.
pub fn release_base_url(version: &Version) -> String {
    if let Ok(base) = std::env::var("SHELBI_RELEASE_BASE_URL") {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            return base.to_string();
        }
    }
    format!(
        "https://github.com/{}/releases/download/v{}",
        release_repo(),
        version
    )
}

// ===========================================================================
// Version parse + hub-compatibility policy (pure)
// ===========================================================================

/// A parsed `major.minor.patch` version. Pre-release / build metadata (`-rc1`,
/// `+meta`) is ignored for the compatibility decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The hub's own version — every Shelbi crate ships with one workspace
/// version, so the orchestrator's `CARGO_PKG_VERSION` is the hub's.
pub fn hub_version() -> Version {
    parse_version(env!("CARGO_PKG_VERSION")).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
    })
}

/// Parse a semver-ish string (`0.9.0`, `v0.9`, `0.9.0-rc1`). Returns `None`
/// when there is no leading `major.minor` to read. A missing patch defaults to
/// `0`.
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    // Cut off pre-release / build metadata.
    let core = s
        .split(['-', '+', ' ', '\t', '\n'])
        .next()
        .unwrap_or("")
        .trim();
    let mut parts = core.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    Some(Version {
        major,
        minor,
        patch,
    })
}

/// Parse the version out of `shelbi --version` output (`shelbi 0.9.0\n`).
/// Scans tokens for the first that parses as a version so a differently
/// formatted banner still resolves.
pub fn parse_version_output(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(parse_version)
}

/// How a remote binary's version relates to the hub's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    /// Same `major.minor` as the hub — usable.
    Compatible,
    /// An older `major.minor` than the hub. The "too old" case the plan calls
    /// out: never overwrite it, install alongside.
    TooOld,
    /// A newer `major.minor` than the hub. Not used, to avoid a remote
    /// speaking a protocol the hub doesn't.
    TooNew,
}

impl Compat {
    pub fn is_compatible(self) -> bool {
        matches!(self, Compat::Compatible)
    }
}

/// Decide whether a remote `shelbi` is compatible with the hub.
///
/// The rule is **same `major.minor`** (patch may differ). The daemon version
/// handshake ([`shelbi_state`]'s `hub_version`) demands an *exact* match
/// because the daemon and CLI share the same on-disk state shape on one host; a
/// remote relay only has to speak the frozen *wire* protocol, which is stable
/// across patch releases within a `major.minor`. Keeping the remote rule at
/// `major.minor` therefore tolerates harmless patch drift while still refusing
/// a binary old (or new) enough to have a different protocol.
pub fn classify_compat(remote: &Version, hub: &Version) -> Compat {
    use std::cmp::Ordering;
    match (remote.major, remote.minor).cmp(&(hub.major, hub.minor)) {
        Ordering::Equal => Compat::Compatible,
        Ordering::Less => Compat::TooOld,
        Ordering::Greater => Compat::TooNew,
    }
}

// ===========================================================================
// Remote-exec seam
// ===========================================================================

/// Result of running a script on a remote: the shell's exit code plus its
/// captured streams. `success` is `code == Some(0)`.
#[derive(Debug, Clone)]
pub struct ExecOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// The seam for everything run on a remote machine. One method runs a POSIX
/// shell script (`sh -c`), the other feeds it bytes on stdin (used to stream a
/// hub-fetched tarball to a remote with no outbound network). An `Err` means
/// the script could not be run *at all* — the transport failed or timed out,
/// i.e. the host is unreachable — as distinct from an `Ok` whose `code` is
/// non-zero (the script ran and reported a failure).
pub trait RemoteExec {
    /// A human label for the target (hostname), for messages.
    fn label(&self) -> &str;
    fn run_script(&self, script: &str) -> io::Result<ExecOutput>;
    fn run_script_with_stdin(&self, script: &str, stdin: &[u8]) -> io::Result<ExecOutput>;
}

/// Default wall-clock bound for a single remote probe/install step. Long
/// enough for an install's download, short enough that a wedged
/// (Tailscale-SSH-style) host fails fast into the Unreachable state instead of
/// hanging. Overridable via `$SHELBI_MACHINE_SSH_TIMEOUT_SECS`.
const DEFAULT_SSH_DEADLINE: Duration = Duration::from_secs(120);

fn ssh_deadline() -> Duration {
    std::env::var("SHELBI_MACHINE_SSH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_SSH_DEADLINE)
}

/// Real [`RemoteExec`] over the host's `ssh`, via [`shelbi_ssh`].
pub struct SshExec {
    host: Host,
    label: String,
    deadline: Duration,
}

impl SshExec {
    /// Build an exec for a machine's [`Host`], labelled for messages.
    pub fn new(host: Host, label: impl Into<String>) -> Self {
        Self {
            host,
            label: label.into(),
            deadline: ssh_deadline(),
        }
    }
}

impl RemoteExec for SshExec {
    fn label(&self) -> &str {
        &self.label
    }

    fn run_script(&self, script: &str) -> io::Result<ExecOutput> {
        // A deadline is the only reliable bound on a Tailscale-SSH web-auth
        // wedge (see `shelbi_ssh::run_with_deadline`): on timeout it returns
        // `TimedOut`, which we surface as Unreachable rather than "no binary".
        let out =
            shelbi_ssh::run_with_deadline(&self.host, ["sh", "-c", script], self.deadline)?;
        Ok(ExecOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn run_script_with_stdin(&self, script: &str, stdin: &[u8]) -> io::Result<ExecOutput> {
        use std::io::Write;
        use std::process::Stdio;
        // No wall-clock deadline here: the stdin payload is a release tarball
        // and a slow link must not look like an unreachable host. Transport
        // death still surfaces as a non-zero exit, which the caller classifies.
        let mut cmd = shelbi_ssh::build_command(&self.host, ["sh", "-c", script]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        // Capture the write error rather than `?`-ing it: a remote that exits
        // early (refused auth, read-only home) closes the pipe, and we want its
        // stderr, not a bare BrokenPipe.
        {
            let mut si = child.stdin.take().expect("stdin piped");
            let _ = si.write_all(stdin);
        }
        let out = child.wait_with_output()?;
        Ok(ExecOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

// ===========================================================================
// Hub-side fetch seam (no-network-on-remote fallback)
// ===========================================================================

/// Why a hub-side fetch failed.
#[derive(Debug, Clone)]
pub enum HubFetchError {
    /// The hub itself has no outbound network (or the asset is missing).
    NoNetwork(String),
    /// The downloaded artifact's checksum did not match.
    Checksum(String),
    /// Anything else (no `curl`, no sha tool, ...).
    Other(String),
}

/// The seam for downloading-and-verifying a release artifact **on the hub**,
/// used only when the remote has no outbound network and the tarball must be
/// copied over SSH. Returns the verified raw `.tar.gz` bytes.
pub trait HubFetch {
    fn fetch_verified(
        &self,
        artifact_url: &str,
        checksums_url: &str,
        artifact_name: &str,
    ) -> Result<Vec<u8>, HubFetchError>;
}

/// Real [`HubFetch`] shelling out to `curl` + `shasum`/`sha256sum` on the hub.
pub struct CurlHubFetch;

impl HubFetch for CurlHubFetch {
    fn fetch_verified(
        &self,
        artifact_url: &str,
        checksums_url: &str,
        artifact_name: &str,
    ) -> Result<Vec<u8>, HubFetchError> {
        let tarball = curl_bytes(artifact_url)?;
        let checksums = String::from_utf8_lossy(&curl_bytes(checksums_url)?).into_owned();
        let expected = expected_sha(&checksums, artifact_name).ok_or_else(|| {
            HubFetchError::Checksum(format!(
                "no checksum entry for {artifact_name} in {checksums_url}"
            ))
        })?;
        let got = sha256_hex(&tarball)?;
        if got != expected {
            return Err(HubFetchError::Checksum(format!(
                "checksum mismatch for {artifact_name}: expected {expected}, got {got}"
            )));
        }
        Ok(tarball)
    }
}

/// `curl -fsSL <url>` on the hub, bytes or a [`HubFetchError`].
fn curl_bytes(url: &str) -> Result<Vec<u8>, HubFetchError> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "10", url])
        .output()
        .map_err(|e| HubFetchError::Other(format!("spawning curl: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(HubFetchError::NoNetwork(format!(
            "curl {url} failed ({}): {}",
            out.status,
            stderr.trim()
        )));
    }
    Ok(out.stdout)
}

/// Compute a tarball's sha256 on the hub via `shasum`/`sha256sum`.
fn sha256_hex(bytes: &[u8]) -> Result<String, HubFetchError> {
    use std::io::Write;
    for (prog, args) in [("sha256sum", &["-"][..]), ("shasum", &["-a", "256", "-"][..])] {
        let mut child = match std::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(bytes);
        }
        if let Ok(out) = child.wait_with_output() {
            if out.status.success() {
                if let Some(hex) = String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .next()
                {
                    return Ok(hex.to_string());
                }
            }
        }
    }
    Err(HubFetchError::Other(
        "no sha256 tool on the hub (need `sha256sum` or `shasum`)".to_string(),
    ))
}

/// The expected hex sha for `artifact_name` from a `checksums.txt` body
/// (`<hex>  <filename>` lines).
fn expected_sha(checksums: &str, artifact_name: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut it = line.split_whitespace();
        let hex = it.next()?;
        let name = it.next()?;
        (name == artifact_name).then(|| hex.to_string())
    })
}

// ===========================================================================
// Probe (find the binary + platform)
// ===========================================================================

/// Live reachability + resolution of a machine's `shelbi` binaries.
#[derive(Debug, Clone)]
pub enum MachineProbe {
    /// The transport failed or timed out — the host is unreachable. Its own
    /// state, never reported as a missing binary.
    Unreachable { detail: String },
    /// SSH connected but authentication/authorization was refused (locked-down
    /// host). Distinct from Unreachable so the message can point at access.
    AuthDenied { detail: String },
    /// SSH ran our probe. Carries the platform (or why it is unusable) and the
    /// binaries found on the PATH and under `~/.shelbi/bin`.
    Reachable(ReachableProbe),
}

/// What a successful probe found on a reachable machine.
#[derive(Debug, Clone)]
pub struct ReachableProbe {
    pub platform: Result<Platform, PlatformError>,
    /// The `shelbi` found on the user's interactive-login PATH, if any.
    pub path_binary: Option<ResolvedBinary>,
    /// `~/.shelbi/bin/shelbi`, if present and executable.
    pub shelbi_bin: Option<ResolvedBinary>,
}

/// A `shelbi` binary found on a remote: where it is and what version it
/// reports. `version` is `None` when `--version` produced nothing parseable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinary {
    pub path: String,
    pub version: Option<Version>,
}

impl ResolvedBinary {
    /// Compatibility against the hub, or `None` when the version is unknown.
    pub fn compat(&self, hub: &Version) -> Option<Compat> {
        self.version.as_ref().map(|v| classify_compat(v, hub))
    }

    pub fn is_compatible(&self, hub: &Version) -> bool {
        self.compat(hub).is_some_and(Compat::is_compatible)
    }
}

// Marker strings framing each field in the probe output. Unique so an
// interactive shell's banners/prompts around the answer are discarded.
const M_OS: (&str, &str) = ("\u{1}OSB\u{1}", "\u{1}OSE\u{1}");
const M_OS_SEP: &str = "\u{1}OSM\u{1}";
const M_PATHBIN: (&str, &str) = ("\u{1}PBB\u{1}", "\u{1}PBE\u{1}");
const M_PATHVER: (&str, &str) = ("\u{1}PVB\u{1}", "\u{1}PVE\u{1}");
const M_HOMEBIN: (&str, &str) = ("\u{1}HBB\u{1}", "\u{1}HBE\u{1}");
const M_HOMEVER: (&str, &str) = ("\u{1}HVB\u{1}", "\u{1}HVE\u{1}");

/// Substrings ssh/sh emit when the host could not be reached (connect-level
/// failure, not an auth refusal), matched case-insensitively.
const UNREACHABLE_MARKERS: &[&str] = &[
    "connection refused",
    "connection timed out",
    "could not resolve",
    "name or service not known",
    "no route to host",
    "network is unreachable",
    "connection closed",
    "operation timed out",
    "host is down",
    "broken pipe",
];

/// Substrings that mean SSH reached the host but refused access (locked-down).
const AUTH_MARKERS: &[&str] = &[
    "permission denied",
    "publickey",
    "authentication failed",
    "too many authentication failures",
];

/// The one-shot probe script: prints the platform and the two candidate
/// binaries (+ their versions) framed in unique markers. It is pure text so it
/// can be snapshot-tested.
pub fn probe_script() -> String {
    let (osb, ose) = M_OS;
    let osm = M_OS_SEP;
    let (pbb, pbe) = M_PATHBIN;
    let (pvb, pve) = M_PATHVER;
    let (hbb, hbe) = M_HOMEBIN;
    let (hvb, hve) = M_HOMEVER;
    // `$SHELL -l -i -c 'command -v shelbi'` loads the user's PATH the way the
    // plan requires; `</dev/null` keeps an interactive shell from blocking on
    // input, and stderr is dropped so shell banners never reach stdout.
    format!(
        r#"printf '{osb}%s{osm}%s{ose}\n' "$(uname -s)" "$(uname -m)"
SH=${{SHELL:-/bin/sh}}
P=$("$SH" -l -i -c 'command -v shelbi' 2>/dev/null </dev/null)
printf '{pbb}%s{pbe}\n' "$P"
if [ -n "$P" ]; then
  printf '{pvb}%s{pve}\n' "$("$P" --version 2>/dev/null)"
fi
HB="$HOME/.shelbi/bin/shelbi"
if [ -x "$HB" ]; then
  printf '{hbb}%s{hbe}\n' "$HB"
  printf '{hvb}%s{hve}\n' "$("$HB" --version 2>/dev/null)"
fi
"#
    )
}

/// Slice the text between the first `begin` and the next `end` after it.
fn between<'a>(text: &'a str, begin: &str, end: &str) -> Option<&'a str> {
    let start = text.find(begin)? + begin.len();
    let rest = &text[start..];
    let stop = rest.find(end)?;
    Some(&rest[..stop])
}

/// Parse the probe script's stdout into a [`ReachableProbe`]. Pure, so the
/// marker framing and login-shell-noise stripping are directly testable.
pub fn parse_probe_output(stdout: &str, hub: &Version) -> ReachableProbe {
    let _ = hub; // compatibility is derived lazily by callers via ResolvedBinary
    let platform = match between(stdout, M_OS.0, M_OS.1) {
        Some(os_block) => {
            let mut halves = os_block.splitn(2, M_OS_SEP);
            let s = halves.next().unwrap_or("").trim();
            let m = halves.next().unwrap_or("").trim();
            detect_platform(s, m)
        }
        // No OS marker at all: treat as an unusable OS so setup refuses
        // rather than guessing.
        None => Err(PlatformError::UnsupportedOs(String::new())),
    };

    let path_binary = between(stdout, M_PATHBIN.0, M_PATHBIN.1)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| ResolvedBinary {
            path: p.to_string(),
            version: between(stdout, M_PATHVER.0, M_PATHVER.1).and_then(parse_version_output),
        });

    let shelbi_bin = between(stdout, M_HOMEBIN.0, M_HOMEBIN.1)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| ResolvedBinary {
            path: p.to_string(),
            version: between(stdout, M_HOMEVER.0, M_HOMEVER.1).and_then(parse_version_output),
        });

    ReachableProbe {
        platform,
        path_binary,
        shelbi_bin,
    }
}

/// Classify a failed probe `ExecOutput` as unreachable vs auth-denied vs a
/// ran-but-failed script. Returns `None` when it is not a transport/auth
/// failure (the script ran). Pure for testability.
pub fn classify_probe_failure(out: &ExecOutput) -> Option<MachineProbe> {
    if out.success() {
        return None;
    }
    let text = format!("{}\n{}", out.stdout, out.stderr).to_ascii_lowercase();
    if AUTH_MARKERS.iter().any(|m| text.contains(m)) {
        return Some(MachineProbe::AuthDenied {
            detail: first_nonempty_line(&out.stderr),
        });
    }
    if out.code == Some(255) || UNREACHABLE_MARKERS.iter().any(|m| text.contains(m)) {
        return Some(MachineProbe::Unreachable {
            detail: first_nonempty_line(&out.stderr),
        });
    }
    None
}

fn first_nonempty_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no diagnostic)")
        .to_string()
}

/// Run the probe against a remote and classify the result. `Err` from the exec
/// (timeout) becomes [`MachineProbe::Unreachable`].
pub fn probe_machine(exec: &dyn RemoteExec, hub: &Version) -> MachineProbe {
    match exec.run_script(&probe_script()) {
        Err(e) => MachineProbe::Unreachable {
            detail: e.to_string(),
        },
        Ok(out) => {
            if let Some(failure) = classify_probe_failure(&out) {
                return failure;
            }
            MachineProbe::Reachable(parse_probe_output(&out.stdout, hub))
        }
    }
}

// ===========================================================================
// Status view
// ===========================================================================

/// The resolved `shelbi` a machine would run: a compatible PATH binary if one
/// exists, else the managed `~/.shelbi/bin/shelbi` (whether or not it is
/// installed yet). This is what `shelbi machine status` reports.
#[derive(Debug, Clone)]
pub struct EffectiveBinary {
    pub path: String,
    pub version: Option<Version>,
    pub compatible: bool,
    pub source: &'static str,
    /// True when the chosen binary is not actually present yet (the managed
    /// path, no install done). Status shows this as "not installed".
    pub needs_install: bool,
}

impl ReachableProbe {
    /// The binary this machine would run against `hub`. Prefers a compatible
    /// PATH binary; otherwise a compatible managed copy; otherwise names the
    /// managed path as the install target.
    pub fn effective(&self, hub: &Version) -> EffectiveBinary {
        if let Some(b) = &self.path_binary {
            if b.is_compatible(hub) {
                return EffectiveBinary {
                    path: b.path.clone(),
                    version: b.version,
                    compatible: true,
                    source: SOURCE_PATH,
                    needs_install: false,
                };
            }
        }
        if let Some(b) = &self.shelbi_bin {
            return EffectiveBinary {
                path: b.path.clone(),
                version: b.version,
                compatible: b.is_compatible(hub),
                source: SOURCE_SHELBI_BIN,
                needs_install: false,
            };
        }
        // Nothing usable yet: the managed path is where setup will install.
        EffectiveBinary {
            path: MANAGED_BIN_DISPLAY.to_string(),
            version: None,
            compatible: false,
            source: SOURCE_SHELBI_BIN,
            needs_install: true,
        }
    }
}

/// Display form of the managed install path (the remote's `$HOME` is not known
/// hub-side, so show it tilde-relative).
pub const MANAGED_BIN_DISPLAY: &str = "~/.shelbi/bin/shelbi";

/// Build the durable [`MachineRecord`] for a resolved effective binary. Only
/// written when the binary actually exists (a resolved version is present).
pub fn record_for(effective: &EffectiveBinary) -> Option<MachineRecord> {
    let version = effective.version?;
    Some(MachineRecord {
        path: effective.path.clone(),
        version: version.to_string(),
        compatible: effective.compatible,
        source: effective.source.to_string(),
        checked_at: chrono::Utc::now(),
    })
}

// ===========================================================================
// Setup (install)
// ===========================================================================

/// What `shelbi machine setup` did.
#[derive(Debug, Clone)]
pub enum SetupAction {
    /// A compatible binary was already in place; nothing changed (idempotent).
    AlreadyCompatible {
        path: String,
        version: Version,
        source: &'static str,
    },
    /// The matching release was installed to `~/.shelbi/bin/shelbi`.
    Installed {
        path: String,
        version: Version,
        /// Set when a too-old binary was found on the PATH and left untouched;
        /// the managed copy was installed alongside it.
        alongside: Option<ResolvedBinary>,
        /// Whether the hub-fetch-and-copy fallback was used (remote had no
        /// outbound network).
        via_hub_copy: bool,
    },
}

/// A setup failure, each carrying a message that already includes the manual
/// steps a human can follow instead.
#[derive(Debug, Clone)]
pub enum SetupError {
    Unreachable(String),
    AuthDenied(String),
    UnsupportedPlatform(PlatformError),
    ReadOnlyHome(String),
    /// Neither the remote nor the hub could fetch the artifact.
    NoNetwork(String),
    InstallFailed(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for SetupError {}

impl PlatformError {
    /// Human message naming what was seen and the manual path forward.
    pub fn message(&self) -> String {
        match self {
            PlatformError::UnsupportedOs(os) => format!(
                "unsupported OS `{}` — Shelbi publishes binaries for macOS (Darwin) and Linux only.\n\
                 Manual steps: build `shelbi` from source on the machine and put it on your PATH, \
                 or install it to ~/.shelbi/bin/shelbi.",
                if os.is_empty() { "unknown" } else { os }
            ),
            PlatformError::UnsupportedArch(arch) => format!(
                "unsupported architecture `{arch}` — Shelbi publishes x86_64 and arm64 binaries.\n\
                 Manual steps: build `shelbi` from source for this architecture and put it on your \
                 PATH, or install it to ~/.shelbi/bin/shelbi."
            ),
            PlatformError::UnsupportedCombo { os, arch } => format!(
                "no published Shelbi binary for {os}/{arch} yet.\n\
                 Manual steps: build `shelbi` from source on the machine and put it on your PATH, \
                 or install it to ~/.shelbi/bin/shelbi."
            ),
        }
    }
}

impl SetupError {
    pub fn message(&self) -> String {
        match self {
            SetupError::Unreachable(d) => format!(
                "machine is unreachable: {d}\n\
                 Manual steps: check the machine is up and `ssh <host>` works from the hub \
                 (SSH config, Tailscale, VPN), then re-run `shelbi machine setup`."
            ),
            SetupError::AuthDenied(d) => format!(
                "SSH refused access: {d}\n\
                 Manual steps: fix key/agent auth so `ssh <host>` logs in non-interactively, \
                 then re-run `shelbi machine setup`."
            ),
            SetupError::UnsupportedPlatform(p) => p.message(),
            SetupError::ReadOnlyHome(d) => format!(
                "cannot write ~/.shelbi/bin on the machine: {d}\n\
                 Manual steps: make the home directory writable (or set a writable $HOME), \
                 or install `shelbi` to a writable location on the PATH yourself, then re-run."
            ),
            SetupError::NoNetwork(d) => format!(
                "could not download the release artifact from the machine or the hub: {d}\n\
                 Manual steps: on a networked box, download the matching `shelbi` release, copy \
                 it to the machine's ~/.shelbi/bin/shelbi, and `chmod +x` it; then re-run \
                 `shelbi machine status` to confirm."
            ),
            SetupError::InstallFailed(d) => format!(
                "install failed on the machine: {d}\n\
                 Manual steps: download the matching `shelbi` release by hand, place it at \
                 ~/.shelbi/bin/shelbi, `chmod +x` it, and re-run `shelbi machine status`."
            ),
        }
    }
}

// Sentinel exit codes the install script uses so failure classification is
// deterministic (and testable through the fake exec).
const EXIT_HOME_UNWRITABLE: i32 = 10;
const EXIT_DOWNLOAD_FAILED: i32 = 20;
const EXIT_CHECKSUM_FAILED: i32 = 30;
const EXIT_EXTRACT_FAILED: i32 = 40;

const M_INSTALLED: (&str, &str) = ("\u{1}INSB\u{1}", "\u{1}INSE\u{1}");

/// The remote install script: download the artifact + checksums for the hub's
/// version, verify sha256, extract the `shelbi` binary, and atomically install
/// it to `~/.shelbi/bin/shelbi`. Pure text (snapshot-testable). Exit codes are
/// the `EXIT_*` sentinels above.
pub fn install_script(artifact: &str, base_url: &str) -> String {
    let (ib, ie) = M_INSTALLED;
    format!(
        r#"set -u
ART='{artifact}'
BASE='{base_url}'
BIN_DIR="$HOME/.shelbi/bin"
TARGET="$BIN_DIR/shelbi"
mkdir -p "$BIN_DIR" 2>/dev/null || exit {home}
TOUCH="$BIN_DIR/.shelbi-setup.$$"
( : > "$TOUCH" ) 2>/dev/null || exit {home}
rm -f "$TOUCH"
WORK=$(mktemp -d 2>/dev/null) || exit {home}
trap 'rm -rf "$WORK"' EXIT
dl() {{
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout 10 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    return 1
  fi
}}
dl "$BASE/$ART" "$WORK/$ART" || exit {dl}
dl "$BASE/checksums.txt" "$WORK/checksums.txt" || exit {dl}
EXP=$(awk -v a="$ART" '$2==a {{print $1}}' "$WORK/checksums.txt" | head -n1)
[ -n "$EXP" ] || exit {sum}
if command -v sha256sum >/dev/null 2>&1; then
  GOT=$(sha256sum "$WORK/$ART" | awk '{{print $1}}')
elif command -v shasum >/dev/null 2>&1; then
  GOT=$(shasum -a 256 "$WORK/$ART" | awk '{{print $1}}')
else
  exit {sum}
fi
[ "$EXP" = "$GOT" ] || exit {sum}
tar -xzf "$WORK/$ART" -C "$WORK" shelbi 2>/dev/null || tar -xzf "$WORK/$ART" -C "$WORK" 2>/dev/null || exit {ex}
[ -f "$WORK/shelbi" ] || exit {ex}
chmod +x "$WORK/shelbi" 2>/dev/null || exit {ex}
mv -f "$WORK/shelbi" "$TARGET" 2>/dev/null || exit {ex}
printf '{ib}%s{ie}\n' "$("$TARGET" --version 2>/dev/null)"
"#,
        home = EXIT_HOME_UNWRITABLE,
        dl = EXIT_DOWNLOAD_FAILED,
        sum = EXIT_CHECKSUM_FAILED,
        ex = EXIT_EXTRACT_FAILED,
    )
}

/// The extract-only script for the hub-copy fallback: the verified tarball
/// arrives on stdin (checksum already checked hub-side), is extracted, and the
/// binary installed to `~/.shelbi/bin/shelbi`.
pub fn install_from_stdin_script() -> String {
    let (ib, ie) = M_INSTALLED;
    format!(
        r#"set -u
BIN_DIR="$HOME/.shelbi/bin"
TARGET="$BIN_DIR/shelbi"
mkdir -p "$BIN_DIR" 2>/dev/null || exit {home}
TOUCH="$BIN_DIR/.shelbi-setup.$$"
( : > "$TOUCH" ) 2>/dev/null || exit {home}
rm -f "$TOUCH"
WORK=$(mktemp -d 2>/dev/null) || exit {home}
trap 'rm -rf "$WORK"' EXIT
cat > "$WORK/shelbi.tar.gz"
tar -xzf "$WORK/shelbi.tar.gz" -C "$WORK" shelbi 2>/dev/null || tar -xzf "$WORK/shelbi.tar.gz" -C "$WORK" 2>/dev/null || exit {ex}
[ -f "$WORK/shelbi" ] || exit {ex}
chmod +x "$WORK/shelbi" 2>/dev/null || exit {ex}
mv -f "$WORK/shelbi" "$TARGET" 2>/dev/null || exit {ex}
printf '{ib}%s{ie}\n' "$("$TARGET" --version 2>/dev/null)"
"#,
        home = EXIT_HOME_UNWRITABLE,
        ex = EXIT_EXTRACT_FAILED,
    )
}

/// Parse the installed version out of an install script's stdout.
fn parse_installed_version(stdout: &str) -> Option<Version> {
    between(stdout, M_INSTALLED.0, M_INSTALLED.1).and_then(parse_version_output)
}

/// Map an install script's non-zero exit to a classified error, or `None` if
/// it succeeded. Pure. A download failure returns `Some(NoNetwork(..))` so the
/// caller knows to try the hub fallback.
fn classify_install_exit(out: &ExecOutput) -> Option<SetupError> {
    if out.success() {
        return None;
    }
    let detail = first_nonempty_line(&out.stderr);
    Some(match out.code {
        Some(c) if c == EXIT_HOME_UNWRITABLE => SetupError::ReadOnlyHome(detail),
        Some(c) if c == EXIT_DOWNLOAD_FAILED => SetupError::NoNetwork(detail),
        Some(c) if c == EXIT_CHECKSUM_FAILED => {
            SetupError::InstallFailed(format!("release checksum verification failed: {detail}"))
        }
        Some(c) if c == EXIT_EXTRACT_FAILED => {
            SetupError::InstallFailed(format!("could not unpack the release: {detail}"))
        }
        other => SetupError::InstallFailed(format!(
            "exit {}: {detail}",
            other.map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
        )),
    })
}

/// Run `shelbi machine setup` against a remote.
///
/// Idempotent: if a compatible `shelbi` is already on the PATH, or a compatible
/// managed copy is already in `~/.shelbi/bin`, nothing is installed. Otherwise
/// the matching release is fetched and installed to `~/.shelbi/bin/shelbi` —
/// alongside (never over) a too-old package-manager install on the PATH. When
/// the remote has no outbound network, the artifact is fetched and verified on
/// the hub and copied over SSH.
pub fn setup(
    exec: &dyn RemoteExec,
    fetch: &dyn HubFetch,
    hub: &Version,
) -> Result<SetupAction, SetupError> {
    let probe = probe_machine(exec, hub);
    let reachable = match probe {
        MachineProbe::Unreachable { detail } => return Err(SetupError::Unreachable(detail)),
        MachineProbe::AuthDenied { detail } => return Err(SetupError::AuthDenied(detail)),
        MachineProbe::Reachable(r) => r,
    };

    let platform = reachable
        .platform
        .map_err(SetupError::UnsupportedPlatform)?;
    let artifact = platform.artifact().map_err(SetupError::UnsupportedPlatform)?;

    // Idempotency / already-compatible short-circuits.
    if let Some(b) = &reachable.path_binary {
        if let (true, Some(v)) = (b.is_compatible(hub), b.version) {
            return Ok(SetupAction::AlreadyCompatible {
                path: b.path.clone(),
                version: v,
                source: SOURCE_PATH,
            });
        }
    }
    if let Some(b) = &reachable.shelbi_bin {
        if let (true, Some(v)) = (b.is_compatible(hub), b.version) {
            return Ok(SetupAction::AlreadyCompatible {
                path: b.path.clone(),
                version: v,
                source: SOURCE_SHELBI_BIN,
            });
        }
    }

    // A too-old PATH binary is left untouched; we install alongside it.
    let alongside = reachable
        .path_binary
        .as_ref()
        .filter(|b| matches!(b.compat(hub), Some(Compat::TooOld)))
        .cloned();

    let base_url = release_base_url(hub);
    let script = install_script(&artifact, &base_url);
    let out = exec
        .run_script(&script)
        .map_err(|e| SetupError::Unreachable(e.to_string()))?;

    match classify_install_exit(&out) {
        None => {
            let version = parse_installed_version(&out.stdout).unwrap_or(*hub);
            Ok(SetupAction::Installed {
                path: MANAGED_BIN_DISPLAY.to_string(),
                version,
                alongside,
                via_hub_copy: false,
            })
        }
        // Remote has no outbound network: fetch+verify on the hub, copy over SSH.
        Some(SetupError::NoNetwork(remote_detail)) => {
            let artifact_url = format!("{base_url}/{artifact}");
            let checksums_url = format!("{base_url}/checksums.txt");
            let bytes = fetch
                .fetch_verified(&artifact_url, &checksums_url, &artifact)
                .map_err(|e| match e {
                    HubFetchError::NoNetwork(d) => SetupError::NoNetwork(format!(
                        "remote: {remote_detail}; hub also could not fetch: {d}"
                    )),
                    HubFetchError::Checksum(d) => SetupError::InstallFailed(d),
                    HubFetchError::Other(d) => SetupError::InstallFailed(d),
                })?;
            let copy = exec
                .run_script_with_stdin(&install_from_stdin_script(), &bytes)
                .map_err(|e| SetupError::Unreachable(e.to_string()))?;
            if let Some(err) = classify_install_exit(&copy) {
                return Err(err);
            }
            let version = parse_installed_version(&copy.stdout).unwrap_or(*hub);
            Ok(SetupAction::Installed {
                path: MANAGED_BIN_DISPLAY.to_string(),
                version,
                alongside,
                via_hub_copy: true,
            })
        }
        Some(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    // ---- platform / artifact ------------------------------------------

    #[test]
    fn detect_platform_maps_uname_synonyms() {
        assert_eq!(
            detect_platform("Darwin", "arm64").unwrap(),
            Platform {
                os: Os::Darwin,
                arch: Arch::Arm64
            }
        );
        assert_eq!(
            detect_platform("Linux\n", " x86_64 ").unwrap(),
            Platform {
                os: Os::Linux,
                arch: Arch::X86_64
            }
        );
        // synonyms
        assert_eq!(detect_platform("linux", "amd64").unwrap().arch, Arch::X86_64);
        assert_eq!(detect_platform("Linux", "aarch64").unwrap().arch, Arch::Arm64);
    }

    #[test]
    fn detect_platform_rejects_unknowns() {
        assert_eq!(
            detect_platform("FreeBSD", "x86_64"),
            Err(PlatformError::UnsupportedOs("FreeBSD".into()))
        );
        assert_eq!(
            detect_platform("Linux", "riscv64"),
            Err(PlatformError::UnsupportedArch("riscv64".into()))
        );
    }

    #[test]
    fn artifact_names_match_goreleaser() {
        let cases = [
            (Os::Darwin, Arch::Arm64, "shelbi_Darwin_arm64.tar.gz"),
            (Os::Darwin, Arch::X86_64, "shelbi_Darwin_x86_64.tar.gz"),
            (Os::Linux, Arch::X86_64, "shelbi_Linux_x86_64.tar.gz"),
        ];
        for (os, arch, want) in cases {
            assert_eq!(Platform { os, arch }.artifact().unwrap(), want);
        }
    }

    #[test]
    fn linux_arm64_has_no_artifact() {
        let p = Platform {
            os: Os::Linux,
            arch: Arch::Arm64,
        };
        assert_eq!(
            p.artifact(),
            Err(PlatformError::UnsupportedCombo {
                os: Os::Linux,
                arch: Arch::Arm64
            })
        );
    }

    // ---- version / compat ---------------------------------------------

    #[test]
    fn parse_version_handles_prefix_and_metadata() {
        assert_eq!(parse_version("0.9.0").unwrap(), v(0, 9, 0));
        assert_eq!(parse_version("v1.2.3").unwrap(), v(1, 2, 3));
        assert_eq!(parse_version("0.9").unwrap(), v(0, 9, 0));
        assert_eq!(parse_version("0.9.0-rc1").unwrap(), v(0, 9, 0));
        assert_eq!(parse_version("0.10.4+meta").unwrap(), v(0, 10, 4));
        assert!(parse_version("not-a-version").is_none());
    }

    #[test]
    fn parse_version_output_reads_shelbi_banner() {
        assert_eq!(parse_version_output("shelbi 0.9.0\n").unwrap(), v(0, 9, 0));
        assert_eq!(parse_version_output("shelbi 1.0.0").unwrap(), v(1, 0, 0));
        assert!(parse_version_output("command not found").is_none());
    }

    #[test]
    fn compat_is_major_minor() {
        let hub = v(0, 9, 0);
        assert_eq!(classify_compat(&v(0, 9, 5), &hub), Compat::Compatible);
        assert_eq!(classify_compat(&v(0, 9, 0), &hub), Compat::Compatible);
        assert_eq!(classify_compat(&v(0, 8, 9), &hub), Compat::TooOld);
        assert_eq!(classify_compat(&v(0, 10, 0), &hub), Compat::TooNew);
        assert_eq!(classify_compat(&v(1, 0, 0), &hub), Compat::TooNew);
    }

    // ---- probe parsing -------------------------------------------------

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    /// Build a probe-script stdout as the remote would print it, optionally
    /// surrounded by interactive-shell banner noise.
    fn probe_stdout(
        uname_s: &str,
        uname_m: &str,
        path_bin: Option<(&str, &str)>,
        home_bin: Option<(&str, &str)>,
    ) -> String {
        let mut s = String::new();
        s.push_str("Welcome to the machine!\n"); // banner noise before
        s.push_str(&format!("{}{}{}{}{}\n", M_OS.0, uname_s, M_OS_SEP, uname_m, M_OS.1));
        if let Some((p, ver)) = path_bin {
            s.push_str(&format!("{}{}{}\n", M_PATHBIN.0, p, M_PATHBIN.1));
            s.push_str(&format!("{}{}{}\n", M_PATHVER.0, ver, M_PATHVER.1));
        } else {
            s.push_str(&format!("{}{}\n", M_PATHBIN.0, M_PATHBIN.1));
        }
        if let Some((p, ver)) = home_bin {
            s.push_str(&format!("{}{}{}\n", M_HOMEBIN.0, p, M_HOMEBIN.1));
            s.push_str(&format!("{}{}{}\n", M_HOMEVER.0, ver, M_HOMEVER.1));
        }
        s.push_str("you have mail\n"); // banner noise after
        s
    }

    #[test]
    fn parse_probe_finds_path_binary_despite_banner_noise() {
        let hub = v(0, 9, 0);
        let out = probe_stdout(
            "Darwin",
            "arm64",
            Some(("/opt/homebrew/bin/shelbi", "shelbi 0.9.0")),
            None,
        );
        let r = parse_probe_output(&out, &hub);
        assert_eq!(r.platform.unwrap().arch, Arch::Arm64);
        let pb = r.path_binary.unwrap();
        assert_eq!(pb.path, "/opt/homebrew/bin/shelbi");
        assert_eq!(pb.version.unwrap(), v(0, 9, 0));
        assert!(r.shelbi_bin.is_none());
    }

    #[test]
    fn parse_probe_no_binary_when_path_empty() {
        let hub = v(0, 9, 0);
        let out = probe_stdout("Linux", "x86_64", None, None);
        let r = parse_probe_output(&out, &hub);
        assert!(r.path_binary.is_none());
        assert!(r.shelbi_bin.is_none());
    }

    #[test]
    fn parse_probe_reads_managed_copy() {
        let hub = v(0, 9, 0);
        let out = probe_stdout(
            "Linux",
            "x86_64",
            None,
            Some(("/home/u/.shelbi/bin/shelbi", "shelbi 0.9.1")),
        );
        let r = parse_probe_output(&out, &hub);
        let hb = r.shelbi_bin.unwrap();
        assert_eq!(hb.path, "/home/u/.shelbi/bin/shelbi");
        assert!(hb.is_compatible(&hub));
    }

    // ---- failure classification ---------------------------------------

    fn out(code: i32, stdout: &str, stderr: &str) -> ExecOutput {
        ExecOutput {
            code: Some(code),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn probe_failure_classification() {
        assert!(matches!(
            classify_probe_failure(&out(255, "", "ssh: connect to host x port 22: Connection refused")),
            Some(MachineProbe::Unreachable { .. })
        ));
        assert!(matches!(
            classify_probe_failure(&out(255, "", "Permission denied (publickey).")),
            Some(MachineProbe::AuthDenied { .. })
        ));
        // A clean run is not a failure.
        assert!(classify_probe_failure(&out(0, "ok", "")).is_none());
    }

    #[test]
    fn install_exit_classification() {
        assert!(matches!(
            classify_install_exit(&out(EXIT_HOME_UNWRITABLE, "", "Read-only file system")),
            Some(SetupError::ReadOnlyHome(_))
        ));
        assert!(matches!(
            classify_install_exit(&out(EXIT_DOWNLOAD_FAILED, "", "Could not resolve host")),
            Some(SetupError::NoNetwork(_))
        ));
        assert!(matches!(
            classify_install_exit(&out(EXIT_CHECKSUM_FAILED, "", "")),
            Some(SetupError::InstallFailed(_))
        ));
        assert!(classify_install_exit(&out(0, "", "")).is_none());
    }

    // ---- setup orchestration via a fake exec --------------------------

    /// A scripted fake: each `run_script` call pops the next canned reply. The
    /// stdin variant shares the same queue.
    struct FakeExec {
        replies: RefCell<Vec<io::Result<ExecOutput>>>,
        stdin_seen: RefCell<Vec<Vec<u8>>>,
    }
    impl FakeExec {
        fn new(replies: Vec<io::Result<ExecOutput>>) -> Self {
            Self {
                replies: RefCell::new(replies.into_iter().rev().collect()),
                stdin_seen: RefCell::new(Vec::new()),
            }
        }
    }
    impl RemoteExec for FakeExec {
        fn label(&self) -> &str {
            "fake"
        }
        fn run_script(&self, _s: &str) -> io::Result<ExecOutput> {
            self.replies
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| Ok(out(0, "", "")))
        }
        fn run_script_with_stdin(&self, _s: &str, stdin: &[u8]) -> io::Result<ExecOutput> {
            self.stdin_seen.borrow_mut().push(stdin.to_vec());
            self.replies
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| Ok(out(0, "", "")))
        }
    }

    struct FakeFetch {
        result: RefCell<Option<Result<Vec<u8>, HubFetchError>>>,
    }
    impl HubFetch for FakeFetch {
        fn fetch_verified(&self, _a: &str, _c: &str, _n: &str) -> Result<Vec<u8>, HubFetchError> {
            self.result
                .borrow_mut()
                .take()
                .unwrap_or(Err(HubFetchError::Other("no canned result".into())))
        }
    }
    fn no_fetch() -> FakeFetch {
        FakeFetch {
            result: RefCell::new(None),
        }
    }

    #[test]
    fn setup_unreachable_errors_with_manual_steps() {
        let hub = v(0, 9, 0);
        let exec = FakeExec::new(vec![Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "did not finish",
        ))]);
        let err = setup(&exec, &no_fetch(), &hub).unwrap_err();
        assert!(matches!(err, SetupError::Unreachable(_)));
        assert!(err.message().contains("Manual steps"));
    }

    #[test]
    fn setup_is_idempotent_when_path_binary_compatible() {
        let hub = v(0, 9, 0);
        let probe = probe_stdout("Linux", "x86_64", Some(("/usr/bin/shelbi", "shelbi 0.9.2")), None);
        let exec = FakeExec::new(vec![Ok(out(0, &probe, ""))]);
        let action = setup(&exec, &no_fetch(), &hub).unwrap();
        match action {
            SetupAction::AlreadyCompatible { source, path, .. } => {
                assert_eq!(source, SOURCE_PATH);
                assert_eq!(path, "/usr/bin/shelbi");
            }
            other => panic!("expected AlreadyCompatible, got {other:?}"),
        }
    }

    #[test]
    fn setup_installs_alongside_too_old_pkg_binary() {
        let hub = v(0, 9, 0);
        // probe: an old 0.8.0 on PATH, nothing in ~/.shelbi/bin
        let probe = probe_stdout("Linux", "x86_64", Some(("/usr/bin/shelbi", "shelbi 0.8.0")), None);
        let installed = format!("{}shelbi 0.9.0{}\n", M_INSTALLED.0, M_INSTALLED.1);
        let exec = FakeExec::new(vec![Ok(out(0, &probe, "")), Ok(out(0, &installed, ""))]);
        let action = setup(&exec, &no_fetch(), &hub).unwrap();
        match action {
            SetupAction::Installed {
                alongside,
                via_hub_copy,
                version,
                ..
            } => {
                assert!(!via_hub_copy);
                assert_eq!(version, v(0, 9, 0));
                let old = alongside.expect("the too-old pkg binary is reported");
                assert_eq!(old.path, "/usr/bin/shelbi");
                assert_eq!(old.version.unwrap(), v(0, 8, 0));
            }
            other => panic!("expected Installed, got {other:?}"),
        }
    }

    #[test]
    fn setup_read_only_home_errors() {
        let hub = v(0, 9, 0);
        let probe = probe_stdout("Linux", "x86_64", None, None);
        let exec = FakeExec::new(vec![
            Ok(out(0, &probe, "")),
            Ok(out(EXIT_HOME_UNWRITABLE, "", "mkdir: Read-only file system")),
        ]);
        let err = setup(&exec, &no_fetch(), &hub).unwrap_err();
        assert!(matches!(err, SetupError::ReadOnlyHome(_)));
        assert!(err.message().contains("~/.shelbi/bin"));
    }

    #[test]
    fn setup_unsupported_arch_errors_before_install() {
        let hub = v(0, 9, 0);
        let probe = probe_stdout("Linux", "riscv64", None, None);
        let exec = FakeExec::new(vec![Ok(out(0, &probe, ""))]);
        let err = setup(&exec, &no_fetch(), &hub).unwrap_err();
        assert!(matches!(
            err,
            SetupError::UnsupportedPlatform(PlatformError::UnsupportedArch(_))
        ));
    }

    #[test]
    fn setup_falls_back_to_hub_copy_when_remote_has_no_network() {
        let hub = v(0, 9, 0);
        let probe = probe_stdout("Linux", "x86_64", None, None);
        let installed = format!("{}shelbi 0.9.0{}\n", M_INSTALLED.0, M_INSTALLED.1);
        let exec = FakeExec::new(vec![
            Ok(out(0, &probe, "")),                                  // probe
            Ok(out(EXIT_DOWNLOAD_FAILED, "", "curl: (6) could not resolve")), // remote download fails
            Ok(out(0, &installed, "")),                              // stdin copy succeeds
        ]);
        let fetch = FakeFetch {
            result: RefCell::new(Some(Ok(b"fake-tarball-bytes".to_vec()))),
        };
        let action = setup(&exec, &fetch, &hub).unwrap();
        match action {
            SetupAction::Installed { via_hub_copy, .. } => assert!(via_hub_copy),
            other => panic!("expected Installed via hub copy, got {other:?}"),
        }
        assert_eq!(exec.stdin_seen.borrow().len(), 1);
        assert_eq!(exec.stdin_seen.borrow()[0], b"fake-tarball-bytes");
    }

    #[test]
    fn setup_no_network_anywhere_errors() {
        let hub = v(0, 9, 0);
        let probe = probe_stdout("Linux", "x86_64", None, None);
        let exec = FakeExec::new(vec![
            Ok(out(0, &probe, "")),
            Ok(out(EXIT_DOWNLOAD_FAILED, "", "curl: (6) could not resolve")),
        ]);
        let fetch = FakeFetch {
            result: RefCell::new(Some(Err(HubFetchError::NoNetwork("hub offline".into())))),
        };
        let err = setup(&exec, &fetch, &hub).unwrap_err();
        assert!(matches!(err, SetupError::NoNetwork(_)));
    }

    // ---- hub-fetch helpers --------------------------------------------

    #[test]
    fn expected_sha_reads_checksums_line() {
        let body = "abc123  shelbi_Linux_x86_64.tar.gz\ndef456  shelbi_Darwin_arm64.tar.gz\n";
        assert_eq!(
            expected_sha(body, "shelbi_Darwin_arm64.tar.gz").as_deref(),
            Some("def456")
        );
        assert_eq!(expected_sha(body, "shelbi_nope.tar.gz"), None);
    }

    #[test]
    fn install_script_embeds_sentinels_and_artifact() {
        let s = install_script("shelbi_Linux_x86_64.tar.gz", "https://example/v0.9.0");
        assert!(s.contains("shelbi_Linux_x86_64.tar.gz"));
        assert!(s.contains("https://example/v0.9.0"));
        assert!(s.contains(&format!("exit {EXIT_HOME_UNWRITABLE}")));
        assert!(s.contains(&format!("exit {EXIT_DOWNLOAD_FAILED}")));
    }

    // ---- end-to-end PATH probe against a real stub login shell --------

    /// Acceptance: the probe finds a `shelbi` that is only on the PATH the
    /// user's rc file (`.zshrc`/`.bashrc`) sets up — i.e. it goes through
    /// `$SHELL -l -i -c`, not the bare non-login environment.
    ///
    /// Rather than depend on a specific real shell, this builds a tiny POSIX
    /// stub that stands in for the login shell: it sources a fake rc file
    /// (which prepends a dir to PATH, exactly as `.zshrc`/`.bashrc` would) and
    /// then runs the `-c` command. The base environment the probe script runs
    /// in deliberately does NOT contain that dir, so a pass proves the PATH
    /// came from the login-shell step.
    #[cfg(unix)]
    #[test]
    fn probe_finds_shelbi_only_on_login_shell_path() {
        use std::os::unix::fs::PermissionsExt;

        let hub = v(0, 9, 0);
        let dir = std::env::temp_dir().join(format!(
            "shelbi-probe-stub-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bindir = dir.join("rcpath");
        std::fs::create_dir_all(&bindir).unwrap();

        let write_exec = |path: &std::path::Path, body: &str| {
            std::fs::write(path, body).unwrap();
            let mut perm = std::fs::metadata(path).unwrap().permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(path, perm).unwrap();
        };

        // Stub `shelbi` reachable only via the rc-file PATH.
        let stub_shelbi = bindir.join("shelbi");
        write_exec(
            &stub_shelbi,
            "#!/bin/sh\n[ \"$1\" = \"--version\" ] && echo 'shelbi 0.9.3'\nexit 0\n",
        );

        // Fake rc file that extends PATH, like a real .zshrc/.bashrc.
        let rc = dir.join("fake_rc");
        std::fs::write(&rc, format!("export PATH=\"{}:$PATH\"\n", bindir.display())).unwrap();

        // Stub login shell: source the rc file, then run the `-c` command.
        let stub_shell = dir.join("login_shell");
        write_exec(
            &stub_shell,
            "#!/bin/sh\n. \"$FAKE_RC\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-c\" ]; then shift; exec /bin/sh -c \"$1\"; fi\n  shift\ndone\n",
        );

        // Run the probe script with SHELL pointed at the stub, and a PATH that
        // does NOT include `bindir`, so only the login-shell step can find it.
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(probe_script())
            .env("SHELL", &stub_shell)
            .env("FAKE_RC", &rc)
            .env("HOME", &dir) // no ~/.shelbi/bin here
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(out.status.success(), "probe script failed: {out:?}");

        let stdout = String::from_utf8_lossy(&out.stdout);
        let probe = parse_probe_output(&stdout, &hub);
        let found = probe
            .path_binary
            .expect("probe must find the rc-file-PATH shelbi");
        assert_eq!(found.path, stub_shelbi.to_string_lossy());
        assert_eq!(found.version.unwrap(), v(0, 9, 3));
        assert!(probe.shelbi_bin.is_none(), "no managed copy was installed");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
