//! The startup requirements check, toolkit-free: the command-line tools
//! Shelbi depends on (Git, the GitHub CLI, a coding agent and, on macOS,
//! Homebrew), whether each is installed, and the copy both clients show for
//! the ones that aren't. The desktop's requirements window and the TUI's
//! requirements screen both render this one model ([`Prereqs`]) and run the
//! same checks ([`run_check`]).
//!
//! **Where a tool is looked for.** A binary the user located
//! (`config.yaml`'s `tools:`, [`shelbi_state::ToolPaths`]) first, then the
//! login shell's `PATH` ([`SystemProbe::login`]), which is what Shelbi's own
//! commands and agents run under: a Finder-launched app's `PATH` lacks
//! `/opt/homebrew/bin` and `~/.local/bin`, its login shell's doesn't. On
//! macOS `/usr/bin/git` is a stub that offers to install Apple's Command Line
//! Tools when they're missing, so it only counts once they're installed
//! (and is never run before that: running it opens the installer's dialog).
//!
//! **When it runs.** On every launch of either client. A launch with
//! everything installed never shows it: the clients check quietly
//! ([`quick_check`], existence only) and show the screen only when something
//! is missing. On screen, each row checks in turn ([`run_check`] reports each
//! as it starts and finishes) and its version comes from `<bin> --version`.
//! Check again runs every check once more; nothing re-runs on a timer.
//!
//! The checks block (the login-shell capture, a `--version` per tool), so a
//! client runs them off its UI thread and folds each [`CheckEvent`] back in
//! with [`Prereqs::apply`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub use shelbi_state::ToolPaths;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Copy (Figma "00 · Requirements · …", 2840:13076 and its row)

pub const CHECKING_TITLE: &str = "Checking your tools";
pub const CHECKING_BODY: &str = "Shelbi uses a few command-line tools. This only takes a moment.";
pub const MISSING_TITLE: &str = "A few tools are missing";
pub const MISSING_ONE_TITLE: &str = "A tool is missing";

// The Missing screen's line says where the install commands go, once (the
// rows don't repeat it): the Terminal app beside the Mac desktop app, any
// terminal beside the Linux one, another terminal beside the TUI (which is
// using this one). A Linux row may offer a package manager or a guide rather
// than a command, so Linux reads "as shown below".
pub const MISSING_BODY_MAC: &str = "Open the Terminal app, run the commands below, then check again.";
pub const MISSING_ONE_BODY_MAC: &str = "Open the Terminal app, run the command below, then check again.";
pub const MISSING_BODY_LINUX: &str = "Install them from a terminal as shown below, then check again.";
pub const MISSING_ONE_BODY_LINUX: &str = "Install it from a terminal as shown below, then check again.";
pub const MISSING_BODY_TUI_MAC: &str = "Run the commands below in another terminal, then check again.";
pub const MISSING_ONE_BODY_TUI_MAC: &str = "Run the command below in another terminal, then check again.";
pub const MISSING_BODY_TUI_LINUX: &str = "Install them from another terminal as shown below, then check again.";
pub const MISSING_ONE_BODY_TUI_LINUX: &str = "Install it from another terminal as shown below, then check again.";
pub const READY_TITLE: &str = "Your tools are ready";
pub const READY_BODY_MAC: &str = "Shelbi found everything it needs on this Mac.";
pub const READY_BODY_LINUX: &str = "Shelbi found everything it needs on this machine.";

/// A found row's detail while its check runs, and a row not checked yet.
pub const CHECKING_ROW: &str = "Checking…";
pub const WAITING_ROW: &str = "Waiting";

pub const SKIP_FOR_NOW: &str = "Skip for now";
pub const CHECK_AGAIN: &str = "Check again";
pub const CONTINUE: &str = "Continue";
pub const COPY: &str = "Copy";
pub const COPIED: &str = "Copied";
/// A missing row's button that opens its Locate panel, and the panel's
/// file-picker button.
pub const BROWSE: &str = "Browse…";
pub const LOCATE_PROMPT: &str = "Already installed somewhere else?";

pub const SKIP_TITLE: &str = "Continue without these tools?";
pub const SKIP_TITLE_ONE: &str = "Continue without this tool?";
pub const SKIP_BODY: &str = "Shelbi depends on them. Until they're installed, parts of Shelbi won't work:";
pub const SKIP_BODY_ONE: &str = "Shelbi depends on it. Until it's installed, parts of Shelbi won't work:";
pub const SKIP_FOOTER: &str = "You can install them later. Shelbi checks again the next time it starts.";
pub const SKIP_FOOTER_ONE: &str = "You can install it later. Shelbi checks again the next time it starts.";
pub const GO_BACK: &str = "Go back";
pub const SKIP_ANYWAY: &str = "Skip anyway";

/// How long Copy reads "Copied" after a click.
pub const COPIED_FOR: Duration = Duration::from_secs(2);

/// How long a `--version` may take before the check stops waiting.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// The tools

/// Which install commands and hints apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Linux,
}

impl Platform {
    /// The platform this build runs on (anything but macOS reads as Linux).
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Linux
        }
    }
}

/// Which client shows the screen, which decides where the Missing screen says
/// to run the install commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    /// The desktop app: the user opens a terminal for them.
    Desktop,
    /// The TUI: the user is in a terminal, but Shelbi is using it.
    Tui,
}

/// The Missing screen's line under the heading: where to run the install
/// commands, for `count` missing tools.
pub fn missing_body(platform: Platform, client: Client, count: usize) -> &'static str {
    let one = count == 1;
    match (client, platform, one) {
        (Client::Desktop, Platform::Mac, false) => MISSING_BODY_MAC,
        (Client::Desktop, Platform::Mac, true) => MISSING_ONE_BODY_MAC,
        (Client::Desktop, Platform::Linux, false) => MISSING_BODY_LINUX,
        (Client::Desktop, Platform::Linux, true) => MISSING_ONE_BODY_LINUX,
        (Client::Tui, Platform::Mac, false) => MISSING_BODY_TUI_MAC,
        (Client::Tui, Platform::Mac, true) => MISSING_ONE_BODY_TUI_MAC,
        (Client::Tui, Platform::Linux, false) => MISSING_BODY_TUI_LINUX,
        (Client::Tui, Platform::Linux, true) => MISSING_ONE_BODY_TUI_LINUX,
    }
}

/// One row: a tool Shelbi depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Git,
    Homebrew,
    GithubCli,
    /// Claude Code, Codex or OpenCode: any one satisfies it.
    Agent,
}

impl Tool {
    /// The order the checks run in (and the rows show, while nothing that
    /// others depend on is missing).
    pub const CHECK_ORDER: [Tool; 4] = [Tool::Git, Tool::GithubCli, Tool::Agent, Tool::Homebrew];

    /// The row's heading.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Git => "Git",
            Tool::Homebrew => "Homebrew",
            Tool::GithubCli => "GitHub CLI",
            Tool::Agent => "Coding agent",
        }
    }

    /// The binaries that satisfy it, in the order they're looked for.
    pub fn binaries(self) -> &'static [Binary] {
        match self {
            Tool::Git => &[Binary::Git],
            Tool::Homebrew => &[Binary::Brew],
            Tool::GithubCli => &[Binary::Gh],
            Tool::Agent => &[Binary::Claude, Binary::Codex, Binary::OpenCode],
        }
    }

    /// Whether the tool is checked on `platform` (Homebrew is macOS-only).
    pub fn applies(self, platform: Platform) -> bool {
        !(self == Tool::Homebrew && platform == Platform::Linux)
    }

    /// What breaks without it (the skip confirmation's line).
    pub fn consequence(self) -> &'static str {
        match self {
            Tool::Git => "Shelbi can't create workspaces or branches.",
            Tool::Homebrew => "Nothing breaks, but you'll need to install the other tools yourself.",
            Tool::GithubCli => "Shelbi can't open pull requests, sync GitHub issues, or follow CI.",
            Tool::Agent => "Workspaces have no agent to run, so no task can start.",
        }
    }

    /// The label of the button that opens the row's Locate panel: the
    /// same for every row (the panel names the executable).
    pub fn locate_label(self) -> &'static str {
        BROWSE
    }

    /// What the Locate panel asks for.
    pub fn locate_body(self) -> String {
        match self {
            Tool::Agent => {
                "Choose the claude, codex or opencode executable. Shelbi remembers this location.".to_string()
            }
            tool => format!(
                "Choose the {} executable. Shelbi remembers this location.",
                tool.binaries()[0].command()
            ),
        }
    }

    /// The Locate field's placeholder.
    pub fn locate_placeholder(self) -> String {
        match self {
            Tool::Agent => "/path/to/claude".to_string(),
            tool => format!("/path/to/{}", tool.binaries()[0].command()),
        }
    }

    /// Why a missing row is missing and what to do, over its commands. It
    /// never says where to run them: the screen's line does ([`missing_body`]).
    /// `brew_missing`: Homebrew is missing too, so a brew command can't run
    /// yet.
    pub fn missing_detail(self, platform: Platform, brew_missing: bool) -> &'static str {
        match (self, platform) {
            (Tool::Git, Platform::Mac) => {
                "Installs Apple's Command Line Tools, which include Git. Homebrew needs them too."
            }
            (Tool::Git, Platform::Linux) => {
                "Not found on your PATH. Install git with your package manager, then check again."
            }
            (Tool::Homebrew, _) => {
                "Shelbi installs the other tools with Homebrew. Install Git first, then run:"
            }
            (Tool::GithubCli, Platform::Mac) if brew_missing => "Needs Homebrew. Install Homebrew above, then run:",
            (Tool::GithubCli, Platform::Mac) => "Not found on your PATH. Install it with Homebrew, then check again.",
            (Tool::GithubCli, Platform::Linux) => {
                "Not found on your PATH. Install it with your package manager; GitHub's guide covers each one:"
            }
            (Tool::Agent, _) => {
                "Shelbi runs your agents with Claude Code, Codex or OpenCode (2.3 or later). Install any one."
            }
        }
    }

    /// The commands a missing row offers to copy (none for Git on Linux,
    /// whose package manager varies).
    pub fn install_commands(self, platform: Platform) -> Vec<InstallCommand> {
        let one = |binary, text: &str| InstallCommand { binary, label: None, text: text.to_string() };
        let agent = |binary, text: &str| InstallCommand {
            binary,
            label: Some(binary.label()),
            text: text.to_string(),
        };
        match (self, platform) {
            (Tool::Git, Platform::Mac) => vec![one(Binary::Git, "xcode-select --install")],
            (Tool::Git, Platform::Linux) => Vec::new(),
            (Tool::Homebrew, _) => vec![one(Binary::Brew, BREW_INSTALL)],
            (Tool::GithubCli, Platform::Mac) => vec![one(Binary::Gh, "brew install gh")],
            (Tool::GithubCli, Platform::Linux) => vec![one(Binary::Gh, "https://github.com/cli/cli#installation")],
            (Tool::Agent, Platform::Mac) => vec![
                agent(Binary::Claude, "brew install --cask claude-code"),
                agent(Binary::Codex, "brew install --cask codex"),
                agent(Binary::OpenCode, shelbi_core::OPENCODE_INSTALL),
            ],
            (Tool::Agent, Platform::Linux) => vec![
                agent(Binary::Claude, "npm install -g @anthropic-ai/claude-code"),
                agent(Binary::Codex, "npm install -g @openai/codex"),
                agent(Binary::OpenCode, shelbi_core::OPENCODE_INSTALL),
            ],
        }
    }
}

/// Homebrew's official install command.
pub const BREW_INSTALL: &str =
    r#"/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)""#;

/// One executable a tool is satisfied by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binary {
    Git,
    Brew,
    Gh,
    Claude,
    Codex,
    OpenCode,
}

impl Binary {
    /// The executable's name (and its key under `config.yaml`'s `tools:`).
    pub fn command(self) -> &'static str {
        match self {
            Binary::Git => "git",
            Binary::Brew => "brew",
            Binary::Gh => "gh",
            Binary::Claude => "claude",
            Binary::Codex => "codex",
            Binary::OpenCode => "opencode",
        }
    }

    /// The product's name (the coding-agent row's command labels).
    pub fn label(self) -> &'static str {
        match self {
            Binary::Git => "Git",
            Binary::Brew => "Homebrew",
            Binary::Gh => "GitHub CLI",
            Binary::Claude => "Claude Code",
            Binary::Codex => "Codex",
            Binary::OpenCode => "OpenCode",
        }
    }
}

/// A command a missing row offers to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallCommand {
    /// The binary it installs.
    pub binary: Binary,
    /// Its label beside the command (the coding-agent row's "Claude Code",
    /// "Codex", "OpenCode"); `None` for a row's only command.
    pub label: Option<&'static str>,
    /// Exactly what Copy puts on the clipboard.
    pub text: String,
}

// ---------------------------------------------------------------------------
// Row state

/// A tool that's there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub binary: Binary,
    pub path: PathBuf,
    /// From `<bin> --version`; `None` when that failed or wasn't asked for.
    pub version: Option<String>,
    /// The user located it (it's not on the login `PATH`).
    pub located: bool,
}

impl Found {
    /// The row's detail: `<version> · <path>`, or the path alone, with the
    /// home folder as `~`.
    pub fn detail(&self, home: Option<&Path>) -> String {
        let path = display_path(&self.path, home);
        match &self.version {
            Some(v) => format!("{v} · {path}"),
            None => path,
        }
    }

    /// The Locate panel's confirmation: "gh version 2.62.0" (or "gh works"
    /// when its version didn't read).
    pub fn located_message(&self) -> String {
        match &self.version {
            Some(v) => format!("{} version {v}", self.binary.command()),
            None => format!("{} works", self.binary.command()),
        }
    }
}

/// Where a row's check is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowState {
    /// Not checked yet ("Waiting").
    Pending,
    /// Its check is running (the spinner).
    Checking,
    Found(Found),
    Missing,
    /// Found, but too old for Shelbi to drive: an OpenCode below
    /// [`shelbi_core::OPENCODE_MIN_VERSION`] when no other coding agent is
    /// installed. It counts as missing; the row offers the upgrade.
    Unsupported(Found),
}

impl RowState {
    /// Whether the row needs the user: missing, or found but unsupported.
    pub fn is_missing(&self) -> bool {
        matches!(self, RowState::Missing | RowState::Unsupported(_))
    }
}

/// One row's news from a check run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckEvent {
    /// The [`Prereqs::begin_check`] run it belongs to.
    pub run: u64,
    pub tool: Tool,
    pub state: RowState,
}

/// What the screen as a whole shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A check is running: Continue, disabled.
    Checking,
    /// Something's missing: Skip for now and Check again.
    Missing,
    /// Everything's there: Continue.
    Ready,
}

/// The requirements screen.
#[derive(Debug, Clone)]
pub struct Prereqs {
    platform: Platform,
    client: Client,
    /// One per tool that applies, in [`Tool::CHECK_ORDER`].
    rows: Vec<(Tool, RowState)>,
    run: u64,
    /// The row whose Locate panel is open, and what validating its path
    /// said.
    locate: Option<(Tool, Option<Result<Found, String>>)>,
}

impl Prereqs {
    /// Every row Pending.
    pub fn new(platform: Platform, client: Client) -> Self {
        let rows = Tool::CHECK_ORDER
            .into_iter()
            .filter(|t| t.applies(platform))
            .map(|t| (t, RowState::Pending))
            .collect();
        Prereqs {
            platform,
            client,
            rows,
            run: 0,
            locate: None,
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The current check run.
    pub fn run(&self) -> u64 {
        self.run
    }

    /// Start a check run: every row back to Pending, any Locate panel
    /// closed. Returns the run its [`CheckEvent`]s must carry.
    pub fn begin_check(&mut self) -> u64 {
        self.run += 1;
        for (_, state) in &mut self.rows {
            *state = RowState::Pending;
        }
        self.locate = None;
        self.run
    }

    /// Fold in a check's news. A straggler from an earlier run is dropped.
    /// Returns whether anything changed.
    pub fn apply(&mut self, event: CheckEvent) -> bool {
        if event.run != self.run {
            return false;
        }
        match self.rows.iter_mut().find(|(t, _)| *t == event.tool) {
            Some((_, state)) if *state != event.state => {
                *state = event.state;
                true
            }
            _ => false,
        }
    }

    /// `tool`'s state (Pending for a tool that doesn't apply here).
    pub fn state(&self, tool: Tool) -> &RowState {
        static PENDING: RowState = RowState::Pending;
        self.rows.iter().find(|(t, _)| *t == tool).map(|(_, s)| s).unwrap_or(&PENDING)
    }

    fn is_missing(&self, tool: Tool) -> bool {
        self.state(tool).is_missing() && tool.applies(self.platform)
    }

    /// The rows in the order they show: with Git or Homebrew missing,
    /// dependency order (Git, Homebrew, GitHub CLI, Coding agent: Homebrew
    /// needs Git's Command Line Tools and installs the rest); otherwise
    /// Homebrew last. Linux has no Homebrew row.
    pub fn tools(&self) -> Vec<Tool> {
        let order: [Tool; 4] = if self.is_missing(Tool::Git) || self.is_missing(Tool::Homebrew) {
            [Tool::Git, Tool::Homebrew, Tool::GithubCli, Tool::Agent]
        } else {
            Tool::CHECK_ORDER
        };
        order.into_iter().filter(|t| t.applies(self.platform)).collect()
    }

    /// The missing tools, in display order.
    pub fn missing(&self) -> Vec<Tool> {
        self.tools().into_iter().filter(|t| self.is_missing(*t)).collect()
    }

    /// Whether every tool was found.
    pub fn all_found(&self) -> bool {
        self.rows.iter().all(|(_, s)| matches!(s, RowState::Found(_)))
    }

    pub fn phase(&self) -> Phase {
        if self.rows.iter().any(|(_, s)| matches!(s, RowState::Pending | RowState::Checking)) {
            Phase::Checking
        } else if self.all_found() {
            Phase::Ready
        } else {
            Phase::Missing
        }
    }

    /// The heading.
    pub fn title(&self) -> &'static str {
        match self.phase() {
            Phase::Checking => CHECKING_TITLE,
            Phase::Ready => READY_TITLE,
            Phase::Missing if self.missing().len() == 1 => MISSING_ONE_TITLE,
            Phase::Missing => MISSING_TITLE,
        }
    }

    /// The line under the heading.
    pub fn body(&self) -> &'static str {
        match self.phase() {
            Phase::Checking => CHECKING_BODY,
            Phase::Ready if self.platform == Platform::Mac => READY_BODY_MAC,
            Phase::Ready => READY_BODY_LINUX,
            Phase::Missing => missing_body(self.platform, self.client, self.missing().len()),
        }
    }

    /// A missing row's detail (GitHub CLI's says it needs Homebrew when
    /// that's missing too; an unsupported OpenCode's names its version).
    pub fn missing_detail(&self, tool: Tool) -> String {
        match self.state(tool) {
            RowState::Unsupported(found) => unsupported_detail(found),
            _ => tool.missing_detail(self.platform, self.is_missing(Tool::Homebrew)).to_string(),
        }
    }

    /// A missing row's commands. An unsupported OpenCode's row offers its
    /// upgrade first, then the other agents' installs.
    pub fn install_commands(&self, tool: Tool) -> Vec<InstallCommand> {
        let commands = tool.install_commands(self.platform);
        match self.state(tool) {
            RowState::Unsupported(found) => {
                let mut out = vec![InstallCommand {
                    binary: found.binary,
                    label: Some(found.binary.label()),
                    text: shelbi_core::OPENCODE_UPGRADE.to_string(),
                }];
                out.extend(commands.into_iter().filter(|c| c.binary != found.binary));
                out
            }
            _ => commands,
        }
    }

    /// The skip confirmation's list: each missing tool and what breaks
    /// without it, in display order.
    pub fn consequences(&self) -> Vec<(Tool, &'static str)> {
        self.missing().into_iter().map(|t| (t, t.consequence())).collect()
    }

    /// The skip confirmation's heading, body and footer (singular for one
    /// missing tool).
    pub fn skip_copy(&self) -> (&'static str, &'static str, &'static str) {
        if self.missing().len() == 1 {
            (SKIP_TITLE_ONE, SKIP_BODY_ONE, SKIP_FOOTER_ONE)
        } else {
            (SKIP_TITLE, SKIP_BODY, SKIP_FOOTER)
        }
    }

    // ---- Locate --------------------------------------------------------

    /// Open `tool`'s Locate panel (closing any other).
    pub fn start_locate(&mut self, tool: Tool) {
        self.locate = Some((tool, None));
    }

    /// Close the Locate panel.
    pub fn cancel_locate(&mut self) {
        self.locate = None;
    }

    /// The row whose Locate panel is open.
    pub fn locating(&self) -> Option<Tool> {
        self.locate.as_ref().map(|(t, _)| *t)
    }

    /// What validating the Locate panel's path said, once it has.
    pub fn locate_result(&self) -> Option<&Result<Found, String>> {
        self.locate.as_ref().and_then(|(_, r)| r.as_ref())
    }

    /// Validating `tool`'s path came back. A good one marks the row found
    /// (the panel stays open, showing the version, until the next check).
    /// A result for a panel that has since closed or moved is dropped.
    pub fn located(&mut self, tool: Tool, result: Result<Found, String>) {
        match &mut self.locate {
            Some((open, slot)) if *open == tool => {
                if let Ok(found) = &result {
                    if let Some((_, state)) = self.rows.iter_mut().find(|(t, _)| *t == tool) {
                        *state = RowState::Found(found.clone());
                    }
                }
                *slot = Some(result);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Detection

/// What the checks ask of the machine; a test passes a fake.
pub trait Probe: Send + Sync {
    /// The directories searched for a tool, in order.
    fn path_dirs(&self) -> Vec<PathBuf>;
    /// A regular file with an execute bit.
    fn is_executable(&self, path: &Path) -> bool;
    /// `<path> --version`'s output, or why it failed.
    fn version(&self, path: &Path) -> Result<String, String>;
    /// macOS: Apple's Command Line Tools are installed (`xcode-select -p`),
    /// so `/usr/bin/git` is Git rather than the installer's stub.
    fn command_line_tools(&self) -> bool;
}

/// The macOS stub that stands in for Git until the Command Line Tools are
/// installed.
const GIT_STUB: &str = "/usr/bin/git";

/// Whether `path` is the Git stub with no Command Line Tools behind it.
fn is_git_stub(binary: Binary, path: &Path, platform: Platform, probe: &dyn Probe) -> bool {
    binary == Binary::Git && platform == Platform::Mac && path == Path::new(GIT_STUB) && !probe.command_line_tools()
}

/// Where `binary` is: the located path when it's still executable, else the
/// first match on the probe's `PATH` (skipping the macOS Git stub).
pub fn resolve(binary: Binary, located: &ToolPaths, platform: Platform, probe: &dyn Probe) -> Option<(PathBuf, bool)> {
    if let Some(path) = located.get(binary.command()) {
        if probe.is_executable(path) && !is_git_stub(binary, path, platform, probe) {
            return Some((path.clone(), true));
        }
    }
    probe
        .path_dirs()
        .into_iter()
        .filter(|d| d.is_absolute())
        .map(|d| d.join(binary.command()))
        .find(|p| probe.is_executable(p) && !is_git_stub(binary, p, platform, probe))
        .map(|p| (p, false))
}

/// Check one tool: Found with the first binary that's there (its version
/// when `versions`), else Missing. An OpenCode is always asked its version,
/// since one below [`shelbi_core::OPENCODE_MIN_VERSION`] is Unsupported:
/// Shelbi's plugin does nothing on it.
pub fn check_tool(tool: Tool, located: &ToolPaths, platform: Platform, probe: &dyn Probe, versions: bool) -> RowState {
    for &binary in tool.binaries() {
        if let Some((path, was_located)) = resolve(binary, located, platform, probe) {
            let output = if versions || binary == Binary::OpenCode {
                probe.version(&path).ok()
            } else {
                None
            };
            let found = Found {
                binary,
                path,
                version: output.as_deref().and_then(parse_version),
                located: was_located,
            };
            if binary == Binary::OpenCode
                && output.as_deref().and_then(shelbi_core::opencode_version_supported) == Some(false)
            {
                return RowState::Unsupported(found);
            }
            return RowState::Found(found);
        }
    }
    RowState::Missing
}

/// An unsupported OpenCode row's detail, over its upgrade command.
fn unsupported_detail(found: &Found) -> String {
    let (major, minor) = shelbi_core::OPENCODE_MIN_VERSION;
    let name = match &found.version {
        Some(version) => format!("OpenCode {version}"),
        None => "This OpenCode".to_string(),
    };
    format!(
        "{name} isn't supported. Shelbi needs OpenCode {major}.{minor} or later. \
         Upgrade it, or install Claude Code or Codex."
    )
}

/// Check every tool in [`Tool::CHECK_ORDER`], reporting each as it starts
/// (Checking) and as it finishes. Blocking: run it off the UI thread.
pub fn run_check(
    run: u64,
    located: &ToolPaths,
    platform: Platform,
    probe: &dyn Probe,
    mut report: impl FnMut(CheckEvent),
) {
    for tool in Tool::CHECK_ORDER.into_iter().filter(|t| t.applies(platform)) {
        report(CheckEvent {
            run,
            tool,
            state: RowState::Checking,
        });
        let state = check_tool(tool, located, platform, probe, true);
        report(CheckEvent { run, tool, state });
    }
}

/// Whether every tool is there, without running any of them: what a launch
/// checks before deciding to show the screen at all. Blocking.
pub fn quick_check(located: &ToolPaths, platform: Platform, probe: &dyn Probe) -> bool {
    Tool::CHECK_ORDER
        .into_iter()
        .filter(|t| t.applies(platform))
        .all(|t| matches!(check_tool(t, located, platform, probe, false), RowState::Found(_)))
}

/// Validate a path the user located for `tool`: it must be an executable
/// file whose `--version` succeeds. For the coding agent, a file named like
/// `opencode` is OpenCode, one named like `codex` is Codex, anything else
/// Claude Code. Blocking.
pub fn validate_located(
    tool: Tool,
    input: &str,
    home: Option<&Path>,
    platform: Platform,
    probe: &dyn Probe,
) -> Result<Found, String> {
    let input = input.trim();
    let command = |b: Binary| b.command();
    if input.is_empty() {
        return Err(format!("Enter the path to the {} executable.", command(tool.binaries()[0])));
    }
    let path = expand_tilde(Path::new(input), home);
    if !path.is_absolute() {
        return Err("Enter a full path, starting with / or ~.".to_string());
    }
    let binary = match tool {
        Tool::Agent => {
            let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if name.contains("opencode") {
                Binary::OpenCode
            } else if name.contains("codex") {
                Binary::Codex
            } else {
                Binary::Claude
            }
        }
        tool => tool.binaries()[0],
    };
    if !probe.is_executable(&path) {
        return Err(format!("{} isn't an executable file.", display_path(&path, home)));
    }
    if is_git_stub(binary, &path, platform, probe) {
        return Err("That's macOS's stand-in for Git. Install the Command Line Tools first.".to_string());
    }
    let out = probe
        .version(&path)
        .map_err(|e| format!("`{} --version` failed: {e}", display_path(&path, home)))?;
    // An OpenCode too old to drive is refused here too, naming the upgrade.
    if binary == Binary::OpenCode && shelbi_core::opencode_version_supported(&out) == Some(false) {
        let version = parse_version(&out).unwrap_or_else(|| out.trim().to_string());
        return Err(shelbi_core::opencode_unsupported_message(&version));
    }
    Ok(Found {
        binary,
        path,
        version: parse_version(&out),
        located: true,
    })
}

/// Remember a located binary (`config.yaml`'s `tools:`), so the next launch
/// counts it found and Shelbi's commands find it.
pub fn save_located(found: &Found) -> Result<(), String> {
    shelbi_state::tool_paths::save(found.binary.command(), Some(&found.path)).map_err(|e| e.to_string())
}

/// The first version-looking token in `--version` output: `2.39.5` from
/// `git version 2.39.5 (Apple Git-154)`, `2.1.4` from `2.1.4 (Claude
/// Code)`, `0.46.0` from `codex-cli 0.46.0`, `1.2.3-beta.1` as written, a
/// leading `v` dropped.
pub fn parse_version(output: &str) -> Option<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^v?(\d+(?:\.\d+)+(?:[-+][0-9A-Za-z][0-9A-Za-z.\-]*)?)$").expect("version pattern")
    });
    output.split_whitespace().find_map(|token| {
        let token = token.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | ',' | ';' | ':' | '"' | '\''));
        re.captures(token).map(|c| c[1].to_string())
    })
}

/// `path` with the home folder as `~`.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `~/x` → `<home>/x`; anything else unchanged.
pub fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

// ---------------------------------------------------------------------------
// The real machine

/// The machine as the checks see it.
pub struct SystemProbe {
    /// Search the login shell's `PATH` (then this process's), not only this
    /// process's.
    login: bool,
    dirs: OnceLock<Vec<PathBuf>>,
}

impl SystemProbe {
    /// The login shell's `PATH`, then this process's: what Shelbi's own
    /// commands run under. The first [`Probe::path_dirs`] captures the login
    /// environment (cached for the process), which can take a second.
    pub fn login() -> Self {
        SystemProbe {
            login: true,
            dirs: OnceLock::new(),
        }
    }

    /// This process's `PATH` alone: instant, for a terminal launch's quiet
    /// first look (a terminal's `PATH` is the login shell's).
    pub fn process() -> Self {
        SystemProbe {
            login: false,
            dirs: OnceLock::new(),
        }
    }
}

fn split_path(value: &str) -> Vec<PathBuf> {
    std::env::split_paths(value).filter(|d| !d.as_os_str().is_empty()).collect()
}

impl Probe for SystemProbe {
    fn path_dirs(&self) -> Vec<PathBuf> {
        self.dirs
            .get_or_init(|| {
                let mut dirs = if self.login {
                    shelbi_core::login_shell_env().get("PATH").map(|p| split_path(p)).unwrap_or_default()
                } else {
                    Vec::new()
                };
                let process = std::env::var("PATH").map(|p| split_path(&p)).unwrap_or_default();
                for dir in process {
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
                dirs
            })
            .clone()
    }

    fn is_executable(&self, path: &Path) -> bool {
        shelbi_state::tools::is_executable_file(path)
    }

    fn version(&self, path: &Path) -> Result<String, String> {
        let mut cmd = Command::new(path);
        cmd.arg("--version");
        // A script's `#!/usr/bin/env node` needs the login PATH, not a
        // Finder launch's.
        if let Ok(joined) = std::env::join_paths(self.path_dirs()) {
            cmd.env("PATH", joined);
        }
        run_bounded(cmd, VERSION_TIMEOUT)
    }

    fn command_line_tools(&self) -> bool {
        Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Run `cmd` with a deadline: its stdout (else its stderr) when it exits
/// successfully, else why not. The pipes are read on threads, so a child
/// that leaves a grandchild holding them can't wedge the check.
fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<String, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pipe = |r: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        if let Some(mut r) = r {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
            });
        }
        rx
    };
    let out = pipe(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = pipe(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer in {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let grace = Duration::from_millis(200);
    let stdout = out.recv_timeout(grace).unwrap_or_default();
    let stderr = err.recv_timeout(grace).unwrap_or_default();
    let first_line = |s: &str| s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
    if status.success() {
        let text = if stdout.trim().is_empty() { stderr } else { stdout };
        Ok(text.trim().to_string())
    } else {
        let why = first_line(&stderr);
        Err(if why.is_empty() {
            match status.code() {
                Some(code) => format!("exited with status {code}"),
                None => "stopped by a signal".to_string(),
            }
        } else {
            why
        })
    }
}

#[cfg(test)]
pub(crate) fn run_bounded_for_test(cmd: Command, timeout: Duration) -> Result<String, String> {
    run_bounded(cmd, timeout)
}

/// A machine made of a list of executables and their `--version` answers,
/// for a client's tests and the desktop's visual check. Its `PATH` is the
/// executables' directories, in order.
#[derive(Debug, Clone, Default)]
pub struct FakeProbe {
    pub dirs: Vec<PathBuf>,
    pub exes: std::collections::BTreeMap<PathBuf, Result<String, String>>,
    pub command_line_tools: bool,
}

impl FakeProbe {
    /// `(path, --version output)` pairs, Command Line Tools installed.
    pub fn new(exes: &[(&str, &str)]) -> Self {
        let mut probe = FakeProbe {
            command_line_tools: true,
            ..FakeProbe::default()
        };
        for (path, version) in exes {
            probe = probe.with(path, version);
        }
        probe
    }

    /// One more executable (its directory joins the `PATH`).
    pub fn with(mut self, path: &str, version: &str) -> Self {
        let path = PathBuf::from(path);
        if let Some(dir) = path.parent() {
            if !self.dirs.iter().any(|d| d == dir) {
                self.dirs.push(dir.to_path_buf());
            }
        }
        self.exes.insert(path, Ok(version.to_string()));
        self
    }

    /// One more directory searched (with nothing in it).
    pub fn dir(mut self, dir: &str) -> Self {
        self.dirs.push(dir.into());
        self
    }
}

impl Probe for FakeProbe {
    fn path_dirs(&self) -> Vec<PathBuf> {
        self.dirs.clone()
    }
    fn is_executable(&self, path: &Path) -> bool {
        self.exes.contains_key(path)
    }
    fn version(&self, path: &Path) -> Result<String, String> {
        self.exes.get(path).cloned().unwrap_or_else(|| Err("no such file".to_string()))
    }
    fn command_line_tools(&self) -> bool {
        self.command_line_tools
    }
}
