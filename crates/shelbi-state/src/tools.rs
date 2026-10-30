//! The editor and diff tool settings (`editor:` and `diff_tool:` in
//! `~/.shelbi/config.yaml`): what a choice is, the curated catalog of tools
//! Shelbi knows, which of them are installed, turning a picked path into a
//! choice (Browse), and how a choice launches.
//!
//! The rule both clients follow: a **CLI tool opens as a pane** (a terminal
//! session in the worktree), and an **app opens in the app** (`open -a`). A
//! setting is either form ([`ToolChoice`]): a plain string is a command line,
//! as `editor:` has always been, and `{ app: <path to .app> }` is an app.
//!
//! Nothing here spawns a process. Detection reads directories (the login
//! `PATH`, `/Applications`, `~/Applications`) and is cached
//! ([`detected_tools`]); the launch side resolves a choice to an
//! [`EditorLaunch`] or a [`DiffLaunch`], which `shelbi_orchestrator::tools`
//! turns into the pane command or the app launch.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// An `editor:` or `diff_tool:` setting: a command line or a macOS app.
///
/// ```yaml
/// editor: hx                              # a CLI command, opened in a pane
/// editor: code --wait                     # flags are fine
/// editor: { app: /Applications/Zed.app }  # an app, opened with `open -a`
/// diff_tool: difft
/// diff_tool:
///   app: /Applications/Kaleidoscope.app
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// A command line, run in a pane in the worktree (it may carry flags).
    Command(String),
    /// A macOS app bundle, opened on the worktree (or the diff) with
    /// `open -a` or its own diff helper.
    App {
        /// The `.app` bundle's path.
        app: PathBuf,
    },
}

impl From<&str> for ToolChoice {
    fn from(command: &str) -> Self {
        ToolChoice::Command(command.to_string())
    }
}

impl From<String> for ToolChoice {
    fn from(command: String) -> Self {
        ToolChoice::Command(command)
    }
}

/// Whether a tool is a command or an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// A command found on the login `PATH`, run in a pane.
    Cli,
    /// A macOS app, opened in the app.
    App,
}

impl ToolChoice {
    /// An app choice for the bundle at `app`.
    pub fn app(app: impl Into<PathBuf>) -> Self {
        ToolChoice::App { app: app.into() }
    }

    pub fn kind(&self) -> ToolKind {
        match self {
            ToolChoice::Command(_) => ToolKind::Cli,
            ToolChoice::App { .. } => ToolKind::App,
        }
    }

    pub fn is_app(&self) -> bool {
        self.kind() == ToolKind::App
    }

    /// The command line, for a command.
    pub fn command(&self) -> Option<&str> {
        match self {
            ToolChoice::Command(c) => Some(c),
            ToolChoice::App { .. } => None,
        }
    }

    /// The bundle path, for an app.
    pub fn app_path(&self) -> Option<&Path> {
        match self {
            ToolChoice::Command(_) => None,
            ToolChoice::App { app } => Some(app),
        }
    }

    /// The choice as saved: a command trimmed, and a blank command or app
    /// path is no choice at all (the fallback applies).
    pub fn normalized(self) -> Option<ToolChoice> {
        match self {
            ToolChoice::Command(c) => {
                let c = c.trim();
                (!c.is_empty()).then(|| ToolChoice::Command(c.to_string()))
            }
            ToolChoice::App { app } => (!app.as_os_str().is_empty()).then_some(ToolChoice::App { app }),
        }
    }

    /// The name a label shows (`Edit in <name>`): a known tool's name
    /// (`nvim` → `Neovim`, `Zed.app` → `Zed`), else the program's basename
    /// title-cased, or the app bundle's name.
    pub fn display_name(&self) -> String {
        match self {
            ToolChoice::Command(c) => command_display_name(c),
            ToolChoice::App { app } => app_display_name(app),
        }
    }

    /// What a setting row shows for it: the command line, or the app's name.
    pub fn summary(&self) -> String {
        match self {
            ToolChoice::Command(c) => c.clone(),
            ToolChoice::App { app } => app_display_name(app),
        }
    }
}

/// Which of a workspace's Edit and View Diff entries open in an app instead
/// of a pane. A client's panel reads it to launch the app and keep the
/// current view, rather than switch to a pane.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppTools {
    /// `editor:` is an app.
    pub editor: bool,
    /// `diff_tool:` is an app, and no project `review.diff_command` takes
    /// precedence over it.
    pub diff: bool,
}

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

/// How a diff tool is driven over a branch's changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStyle {
    /// `git difftool` per changed file: the command gets `$LOCAL` and
    /// `$REMOTE` (two files).
    Files,
    /// `git difftool --dir-diff`: the command gets `$LOCAL` and `$REMOTE` as
    /// two directories, the whole change at once.
    Dirs,
    /// `git diff --ext-diff` through a pager, the command as git's
    /// `diff.external` (a tool that prints a diff, like difftastic).
    Pager,
    /// The tool opens the worktree's repository itself (`<helper>
    /// <worktree>`), as Sublime Merge does.
    Worktree,
}

/// A command-line tool Shelbi knows: found on the login `PATH` by
/// `program`, saved as `command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownCli {
    pub name: &'static str,
    /// The executable looked for on the login `PATH`.
    pub program: &'static str,
    /// The command line saved for it and run in the pane.
    pub command: &'static str,
}

/// The command-line editors Shelbi lists when installed, in the order shown.
pub const EDITOR_CLIS: &[KnownCli] = &[
    KnownCli { name: "Neovim", program: "nvim", command: "nvim" },
    KnownCli { name: "Vim", program: "vim", command: "vim" },
    KnownCli { name: "Vi", program: "vi", command: "vi" },
    // `-nw` keeps an Emacs built with a GUI in the terminal pane.
    KnownCli { name: "Emacs", program: "emacs", command: "emacs -nw" },
    KnownCli { name: "Helix", program: "hx", command: "hx" },
    KnownCli { name: "Nano", program: "nano", command: "nano" },
    KnownCli { name: "Micro", program: "micro", command: "micro" },
    KnownCli { name: "Kakoune", program: "kak", command: "kak" },
];

/// The command-line diff tools Shelbi lists when installed, in the order
/// shown. "Git's configured difftool" is no setting at all (`diff_tool:`
/// unset), so it isn't an entry here.
pub const DIFF_CLIS: &[KnownCli] = &[
    KnownCli { name: "Neovim", program: "nvim", command: "nvim -d" },
    KnownCli { name: "Vim", program: "vimdiff", command: "vimdiff" },
    KnownCli { name: "Difftastic", program: "difft", command: "difft" },
];

/// How an app diffs: its command-line helper and how git drives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppDiff {
    /// The helper's name on the login `PATH` (`ksdiff`, `bcomp`).
    pub program: &'static str,
    /// Where the bundle keeps the helper, relative to the `.app`, tried when
    /// it isn't on the `PATH`.
    pub in_bundle: &'static [&'static str],
    /// The difftool command: `{helper}` is the helper's path, `$LOCAL` and
    /// `$REMOTE` the two sides. Unused for [`DiffStyle::Worktree`]. It must
    /// block until the comparison closes (git deletes the sides then).
    pub cmd: &'static str,
    pub style: DiffStyle,
}

/// A macOS app Shelbi knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownApp {
    pub name: &'static str,
    pub bundle_id: &'static str,
    /// Where it installs, relative to an applications folder
    /// (`/Applications`, `~/Applications`): every bundle name it ships as.
    pub bundles: &'static [&'static str],
    /// How it diffs, for a diff tool.
    pub diff: Option<AppDiff>,
}

const fn editor_app(name: &'static str, bundle_id: &'static str, bundles: &'static [&'static str]) -> KnownApp {
    KnownApp { name, bundle_id, bundles, diff: None }
}

/// The editor apps Shelbi lists when installed, in the order shown. (Users
/// pick any other app with Browse.)
pub const EDITOR_APPS: &[KnownApp] = &[
    editor_app("Visual Studio Code", "com.microsoft.VSCode", &["Visual Studio Code.app"]),
    editor_app("Zed", "dev.zed.Zed", &["Zed.app"]),
    editor_app("Sublime Text", "com.sublimetext.4", &["Sublime Text.app"]),
    editor_app("BBEdit", "com.barebones.bbedit", &["BBEdit.app"]),
    editor_app("Nova", "com.panic.Nova", &["Nova.app"]),
    editor_app("TextMate", "com.macromates.TextMate", &["TextMate.app"]),
    editor_app("Xcode", "com.apple.dt.Xcode", &["Xcode.app"]),
    editor_app("MacVim", "org.vim.MacVim", &["MacVim.app"]),
    editor_app("Emacs", "org.gnu.Emacs", &["Emacs.app"]),
    editor_app("CotEditor", "com.coteditor.CotEditor", &["CotEditor.app"]),
    editor_app("IntelliJ IDEA", "com.jetbrains.intellij", &["IntelliJ IDEA.app", "IntelliJ IDEA CE.app", "IntelliJ IDEA Ultimate.app"]),
    editor_app("WebStorm", "com.jetbrains.WebStorm", &["WebStorm.app"]),
    editor_app("RustRover", "com.jetbrains.rustrover", &["RustRover.app"]),
    editor_app("GoLand", "com.jetbrains.goland", &["GoLand.app"]),
    editor_app("PyCharm", "com.jetbrains.pycharm", &["PyCharm.app", "PyCharm CE.app", "PyCharm Professional Edition.app"]),
    editor_app("CLion", "com.jetbrains.CLion", &["CLion.app"]),
    editor_app("PhpStorm", "com.jetbrains.PhpStorm", &["PhpStorm.app"]),
    editor_app("RubyMine", "com.jetbrains.rubymine", &["RubyMine.app"]),
];

/// The diff apps Shelbi lists when installed, in the order shown.
pub const DIFF_APPS: &[KnownApp] = &[
    KnownApp {
        name: "FileMerge",
        bundle_id: "com.apple.FileMerge",
        bundles: &["Xcode.app/Contents/Applications/FileMerge.app"],
        // `| cat` holds opendiff until FileMerge closes the comparison (git's
        // own opendiff driver does the same).
        diff: Some(AppDiff { program: "opendiff", in_bundle: &[], cmd: "{helper} \"$LOCAL\" \"$REMOTE\" | cat", style: DiffStyle::Dirs }),
    },
    KnownApp {
        name: "Kaleidoscope",
        bundle_id: "app.kaleidoscope.v4",
        bundles: &["Kaleidoscope.app"],
        diff: Some(AppDiff {
            program: "ksdiff",
            in_bundle: &["Contents/MacOS/ksdiff", "Contents/Resources/bin/ksdiff"],
            cmd: "{helper} --wait \"$LOCAL\" \"$REMOTE\"",
            style: DiffStyle::Dirs,
        }),
    },
    KnownApp {
        name: "Beyond Compare",
        bundle_id: "com.ScooterSoftware.BeyondCompare",
        bundles: &["Beyond Compare.app"],
        diff: Some(AppDiff { program: "bcomp", in_bundle: &["Contents/MacOS/bcomp"], cmd: "{helper} \"$LOCAL\" \"$REMOTE\"", style: DiffStyle::Dirs }),
    },
    KnownApp {
        name: "Visual Studio Code",
        bundle_id: "com.microsoft.VSCode",
        bundles: &["Visual Studio Code.app"],
        diff: Some(AppDiff {
            program: "code",
            in_bundle: &["Contents/Resources/app/bin/code"],
            cmd: "{helper} --wait --diff \"$LOCAL\" \"$REMOTE\"",
            style: DiffStyle::Files,
        }),
    },
    KnownApp {
        name: "Sublime Merge",
        bundle_id: "com.sublimemerge",
        bundles: &["Sublime Merge.app"],
        diff: Some(AppDiff { program: "smerge", in_bundle: &["Contents/SharedSupport/bin/smerge"], cmd: "", style: DiffStyle::Worktree }),
    },
    KnownApp {
        name: "P4Merge",
        bundle_id: "com.perforce.p4merge",
        bundles: &["p4merge.app", "P4Merge.app"],
        diff: Some(AppDiff {
            program: "launchp4merge",
            in_bundle: &["Contents/Resources/launchp4merge"],
            cmd: "{helper} \"$LOCAL\" \"$REMOTE\"",
            style: DiffStyle::Files,
        }),
    },
    KnownApp {
        name: "Araxis Merge",
        bundle_id: "com.araxis.merge",
        bundles: &["Araxis Merge.app"],
        diff: Some(AppDiff {
            program: "compare",
            in_bundle: &["Contents/Utilities/compare"],
            cmd: "{helper} -wait -2 \"$LOCAL\" \"$REMOTE\"",
            style: DiffStyle::Files,
        }),
    },
    KnownApp {
        name: "DeltaWalker",
        bundle_id: "com.deltopia.DeltaWalker",
        bundles: &["DeltaWalker.app"],
        diff: Some(AppDiff {
            program: "DeltaWalker",
            in_bundle: &["Contents/MacOS/DeltaWalker"],
            cmd: "{helper} \"$LOCAL\" \"$REMOTE\"",
            style: DiffStyle::Files,
        }),
    },
];

/// Display names of programs that aren't catalog entries in their own right
/// (another spelling, or a GUI editor's command-line launcher).
const PROGRAM_NAMES: &[(&str, &str)] = &[
    ("helix", "Helix"),
    ("vimdiff", "Vim"),
    ("nvimdiff", "Neovim"),
    ("code", "Visual Studio Code"),
    ("zed", "Zed"),
    ("subl", "Sublime Text"),
    ("mate", "TextMate"),
    ("bbedit", "BBEdit"),
    ("nova", "Nova"),
    ("mvim", "MacVim"),
    ("idea", "IntelliJ IDEA"),
    ("webstorm", "WebStorm"),
    ("rustrover", "RustRover"),
    ("goland", "GoLand"),
    ("pycharm", "PyCharm"),
    ("clion", "CLion"),
    ("opendiff", "FileMerge"),
    ("ksdiff", "Kaleidoscope"),
    ("bcomp", "Beyond Compare"),
    ("smerge", "Sublime Merge"),
];

/// The basename of a command line's program (`/usr/bin/hx -v` → `hx`).
fn program_basename(command: &str) -> &str {
    let program = command.split_whitespace().next().unwrap_or("");
    program.rsplit(['/', '\\']).next().unwrap_or(program).trim()
}

/// The display name of a command line: a known program's name (`nvim` →
/// `Neovim`, `code --wait` → `Visual Studio Code`), else the program's
/// basename with its first letter upper-cased (`foo` → `Foo`). Flags and
/// directories are dropped; an empty command is an empty name.
pub fn command_display_name(command: &str) -> String {
    let base = program_basename(command);
    let known = EDITOR_CLIS
        .iter()
        .chain(DIFF_CLIS)
        .find(|t| t.program == base)
        .map(|t| t.name)
        .or_else(|| PROGRAM_NAMES.iter().find(|(p, _)| *p == base).map(|(_, n)| *n));
    if let Some(name) = known {
        return name.to_string();
    }
    let mut chars = base.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The catalog app installed as `app` (matched on the bundle's trailing
/// path, so `FileMerge.app` inside any Xcode matches), from `table`.
pub fn known_app<'a>(table: &'a [KnownApp], app: &Path) -> Option<&'a KnownApp> {
    table.iter().find(|k| k.bundles.iter().any(|b| app.ends_with(b)))
}

/// An app's display name: the catalog's name for a known bundle, else the
/// bundle's file name without `.app` (`/Applications/Foo Bar.app` →
/// `Foo Bar`).
pub fn app_display_name(app: &Path) -> String {
    if let Some(k) = known_app(EDITOR_APPS, app).or_else(|| known_app(DIFF_APPS, app)) {
        return k.name.to_string();
    }
    app.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// One installed tool a setting can pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOption {
    pub name: String,
    pub kind: ToolKind,
    /// What picking it saves.
    pub choice: ToolChoice,
    /// The bundle, for an app (a client draws its icon).
    pub app_path: Option<PathBuf>,
}

impl ToolOption {
    fn cli(t: &KnownCli) -> ToolOption {
        ToolOption {
            name: t.name.to_string(),
            kind: ToolKind::Cli,
            choice: ToolChoice::Command(t.command.to_string()),
            app_path: None,
        }
    }

    fn app(name: &str, path: PathBuf) -> ToolOption {
        ToolOption {
            name: name.to_string(),
            kind: ToolKind::App,
            choice: ToolChoice::app(path.clone()),
            app_path: Some(path),
        }
    }

    /// The option for a choice the catalog doesn't list (a browsed app, a
    /// typed command).
    pub fn custom(choice: &ToolChoice) -> ToolOption {
        ToolOption {
            name: choice.display_name(),
            kind: choice.kind(),
            choice: choice.clone(),
            app_path: choice.app_path().map(Path::to_path_buf),
        }
    }
}

/// The editors and diff tools installed on this machine, CLI tools first
/// (catalog order), then apps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectedTools {
    pub editors: Vec<ToolOption>,
    pub diff_tools: Vec<ToolOption>,
    /// The difftool git is configured with (`diff.guitool`, else
    /// `diff.tool`), what an unset `diff_tool:` runs.
    pub git_difftool: Option<String>,
}

/// `program` on `path_dirs`: the first executable file of that name.
pub fn find_on_path(program: &str, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    path_dirs
        .iter()
        .map(|dir| dir.join(program))
        .find(|p| is_executable_file(p))
}

/// A regular file with an execute bit.
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// A directory that is an app bundle: a `.app` with a `Contents/Info.plist`.
pub fn is_app_bundle(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))
        && path.is_dir()
        && path.join("Contents").join("Info.plist").is_file()
}

/// The first of `app`'s bundles installed in `app_dirs`.
fn installed_bundle(app: &KnownApp, app_dirs: &[PathBuf]) -> Option<PathBuf> {
    app_dirs
        .iter()
        .flat_map(|dir| app.bundles.iter().map(move |b| dir.join(b)))
        .find(|p| is_app_bundle(p))
}

/// Which catalog tools are installed: CLIs on `path_dirs`, apps in
/// `app_dirs`. Blocking (directory reads), but quick; [`detected_tools`]
/// caches it.
pub fn detect_in(path_dirs: &[PathBuf], app_dirs: &[PathBuf]) -> DetectedTools {
    let clis = |table: &[KnownCli]| -> Vec<ToolOption> {
        table
            .iter()
            .filter(|t| find_on_path(t.program, path_dirs).is_some())
            .map(ToolOption::cli)
            .collect()
    };
    let apps = |table: &[KnownApp]| -> Vec<ToolOption> {
        table
            .iter()
            .filter_map(|a| installed_bundle(a, app_dirs).map(|p| ToolOption::app(a.name, p)))
            .collect()
    };
    let mut editors = clis(EDITOR_CLIS);
    editors.extend(apps(EDITOR_APPS));
    let mut diff_tools = clis(DIFF_CLIS);
    diff_tools.extend(apps(DIFF_APPS));
    DetectedTools {
        editors,
        diff_tools,
        git_difftool: None,
    }
}

/// The login shell's `PATH` directories (the capture the daemon and the
/// desktop app's bootstrap use: `$SHELL -l -i -c env`, cached per process).
pub fn login_path_dirs() -> Vec<PathBuf> {
    shelbi_core::login_env::login_shell_env()
        .get("PATH")
        .map(|p| std::env::split_paths(p).filter(|d| !d.as_os_str().is_empty()).collect())
        .unwrap_or_default()
}

/// The folders apps are found in: `/Applications` and `~/Applications`.
pub fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// The difftool the user's global git config names: `diff.guitool`, else
/// `diff.tool` (the precedence `git difftool` itself uses for the GUI pick).
fn git_global_difftool(path_dirs: &[PathBuf]) -> Option<String> {
    let git = find_on_path("git", path_dirs).unwrap_or_else(|| PathBuf::from("git"));
    ["diff.guitool", "diff.tool", "merge.guitool", "merge.tool"].iter().find_map(|key| {
        let out = std::process::Command::new(&git)
            .args(["config", "--global", "--get", key])
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (out.status.success() && !value.is_empty()).then_some(value)
    })
}

static DETECTED: Mutex<Option<DetectedTools>> = Mutex::new(None);

/// The installed tools, detected once per process (the login `PATH` and the
/// applications folders) and cached. Blocking the first time (the login-env
/// capture, a directory scan and a `git config` read), so call it off the UI
/// thread; [`refresh_detected_tools`] scans again.
pub fn detected_tools() -> DetectedTools {
    if let Some(cached) = DETECTED.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return cached;
    }
    refresh_detected_tools()
}

/// Detect the installed tools again (an app was installed since), replacing
/// the cache.
pub fn refresh_detected_tools() -> DetectedTools {
    let path = login_path_dirs();
    let mut found = detect_in(&path, &app_dirs());
    found.git_difftool = git_global_difftool(&path);
    *DETECTED.lock().unwrap_or_else(|e| e.into_inner()) = Some(found.clone());
    found
}

// ---------------------------------------------------------------------------
// Browse
// ---------------------------------------------------------------------------

/// The choice for a path the user picked with Browse: an `.app` bundle is an
/// app, an executable file a command (its path, shell-quoted when it needs
/// it). Anything else, or a path that doesn't exist, is an error saying why.
pub fn choice_from_path(path: &Path) -> Result<ToolChoice, String> {
    let shown = path.display();
    if !path.exists() {
        return Err(format!("{shown} doesn't exist"));
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")) {
        return if is_app_bundle(path) {
            Ok(ToolChoice::app(path))
        } else {
            Err(format!("{shown} isn't an app bundle (no Contents/Info.plist)"))
        };
    }
    if path.is_dir() {
        return Err(format!("{shown} is a folder, not an app or a program"));
    }
    if !is_executable_file(path) {
        return Err(format!("{shown} isn't executable"));
    }
    Ok(ToolChoice::Command(shelbi_core::shell_escape(&path.to_string_lossy())))
}

// ---------------------------------------------------------------------------
// Launching
// ---------------------------------------------------------------------------

/// How the editor opens a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorLaunch {
    /// Run this command line in a pane in the worktree.
    Pane(String),
    /// Open the worktree in this app (`open -a <app> <worktree>`).
    App(PathBuf),
}

/// How a setting's editor launches: a command in a pane, an app in the app,
/// and with nothing set `fallback` (`$EDITOR`, then vim) in a pane.
pub fn editor_launch(choice: Option<&ToolChoice>, fallback: &str) -> EditorLaunch {
    match choice.cloned().and_then(ToolChoice::normalized) {
        Some(ToolChoice::Command(c)) => EditorLaunch::Pane(c),
        Some(ToolChoice::App { app }) => EditorLaunch::App(app),
        None => EditorLaunch::Pane(fallback.to_string()),
    }
}

/// How a diff runs over `merge-base(base, HEAD)..HEAD` in the worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLaunch {
    /// `git difftool` with git's own configured tool (`diff.tool` /
    /// `diff.guitool`): what an unset `diff_tool:` runs.
    GitConfigured,
    /// `git difftool --tool=shelbi` with this as `difftool.shelbi.cmd`
    /// (`$LOCAL` / `$REMOTE` are the sides), per file or over directories.
    Difftool { cmd: String, dir_diff: bool },
    /// `git diff --ext-diff` with this as `diff.external`, through a pager.
    Pager { cmd: String },
    /// Run this command line with the worktree as its argument.
    Worktree { cmd: String },
}

/// Fill a catalog diff command's `{helper}`.
fn with_helper(diff: &AppDiff, helper: &str) -> DiffLaunch {
    let cmd = diff.cmd.replace("{helper}", helper);
    match diff.style {
        DiffStyle::Files => DiffLaunch::Difftool { cmd, dir_diff: false },
        DiffStyle::Dirs => DiffLaunch::Difftool { cmd, dir_diff: true },
        DiffStyle::Pager => DiffLaunch::Pager { cmd },
        DiffStyle::Worktree => DiffLaunch::Worktree { cmd: helper.to_string() },
    }
}

/// How a `diff_tool:` setting runs, its helpers looked up on `path_dirs`.
///
/// * Unset: [`DiffLaunch::GitConfigured`].
/// * A command: `difft` (difftastic) through a pager; a diff app's bare
///   helper (`opendiff`, `ksdiff`, …) the way the catalog drives it; a
///   command that names `$LOCAL` / `$REMOTE` as written; any other command
///   per file with the two files appended (`nvim -d`, `vimdiff`).
/// * An app: the catalog's helper for it (on the `PATH`, else inside the
///   bundle; an error naming it when it's in neither). An app the catalog
///   doesn't know gets the two directories through `open -W -a`.
pub fn diff_launch(choice: Option<&ToolChoice>, path_dirs: &[PathBuf]) -> Result<DiffLaunch, String> {
    let Some(choice) = choice.cloned().and_then(ToolChoice::normalized) else {
        return Ok(DiffLaunch::GitConfigured);
    };
    match choice {
        ToolChoice::Command(cmd) => {
            let base = program_basename(&cmd);
            let bare = !cmd.trim().contains(char::is_whitespace);
            if base == "difft" {
                let cmd = if cmd.contains("--color") { cmd } else { format!("{cmd} --color=always") };
                return Ok(DiffLaunch::Pager { cmd });
            }
            if bare {
                if let Some(diff) = DIFF_APPS.iter().filter_map(|a| a.diff.as_ref()).find(|d| d.program == base) {
                    return Ok(with_helper(diff, &cmd));
                }
            }
            if cmd.contains("$LOCAL") || cmd.contains("$REMOTE") {
                return Ok(DiffLaunch::Difftool { cmd, dir_diff: false });
            }
            Ok(DiffLaunch::Difftool {
                cmd: format!("{cmd} \"$LOCAL\" \"$REMOTE\""),
                dir_diff: false,
            })
        }
        ToolChoice::App { app } => {
            let Some(known) = known_app(DIFF_APPS, &app) else {
                return Ok(DiffLaunch::Difftool {
                    cmd: format!(
                        "open -W -a {} \"$LOCAL\" \"$REMOTE\"",
                        shelbi_core::shell_escape(&app.to_string_lossy())
                    ),
                    dir_diff: true,
                });
            };
            let diff = known.diff.as_ref().expect("diff apps carry a diff recipe");
            let helper = find_on_path(diff.program, path_dirs)
                .map(|_| diff.program.to_string())
                .or_else(|| {
                    diff.in_bundle
                        .iter()
                        .map(|rel| app.join(rel))
                        .find(|p| is_executable_file(p))
                        .map(|p| shelbi_core::shell_escape(&p.to_string_lossy()))
                })
                .ok_or_else(|| {
                    format!(
                        "{}'s `{}` command isn't installed; install it from {} or pick another diff tool",
                        known.name, diff.program, known.name
                    )
                })?;
            Ok(with_helper(diff, &helper))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shelbi-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn bundle(path: &Path) {
        std::fs::create_dir_all(path.join("Contents")).unwrap();
        std::fs::write(path.join("Contents/Info.plist"), "<plist/>").unwrap();
    }

    #[test]
    fn both_forms_round_trip_through_yaml() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Doc {
            editor: ToolChoice,
        }
        let cli: Doc = serde_yaml::from_str("editor: code --wait\n").unwrap();
        assert_eq!(cli.editor, ToolChoice::Command("code --wait".into()));
        assert_eq!(serde_yaml::to_string(&cli).unwrap(), "editor: code --wait\n");

        let app: Doc = serde_yaml::from_str("editor: { app: /Applications/Zed.app }\n").unwrap();
        assert_eq!(app.editor, ToolChoice::app("/Applications/Zed.app"));
        let text = serde_yaml::to_string(&app).unwrap();
        assert_eq!(serde_yaml::from_str::<Doc>(&text).unwrap(), app, "{text}");

        let block: Doc = serde_yaml::from_str("editor:\n  app: /Applications/Zed.app\n  future: 1\n").unwrap();
        assert_eq!(block.editor, ToolChoice::app("/Applications/Zed.app"), "unknown keys are ignored");
        assert!(serde_yaml::from_str::<Doc>("editor: [1]\n").is_err());
    }

    #[test]
    fn names_come_from_the_catalog_or_the_program() {
        assert_eq!(ToolChoice::from("nvim").display_name(), "Neovim");
        assert_eq!(ToolChoice::from("/opt/bin/hx").display_name(), "Helix");
        assert_eq!(ToolChoice::from("emacs -nw").display_name(), "Emacs");
        assert_eq!(ToolChoice::from("code --wait").display_name(), "Visual Studio Code");
        assert_eq!(ToolChoice::from("vimdiff").display_name(), "Vim");
        assert_eq!(ToolChoice::from("difft").display_name(), "Difftastic");
        assert_eq!(ToolChoice::from("foo -x").display_name(), "Foo");
        assert_eq!(ToolChoice::from("").display_name(), "");
        assert_eq!(ToolChoice::app("/Applications/Zed.app").display_name(), "Zed");
        assert_eq!(ToolChoice::app("/Users/me/Applications/PyCharm CE.app").display_name(), "PyCharm");
        assert_eq!(
            ToolChoice::app("/Applications/Xcode.app/Contents/Applications/FileMerge.app").display_name(),
            "FileMerge"
        );
        assert_eq!(ToolChoice::app("/Applications/Some Editor.app").display_name(), "Some Editor");
        assert_eq!(ToolChoice::app("/Applications/Some Editor.app").summary(), "Some Editor");
        assert_eq!(ToolChoice::from("code --wait").summary(), "code --wait");
    }

    #[test]
    fn a_blank_choice_normalizes_to_none() {
        assert_eq!(ToolChoice::from("  ").normalized(), None);
        assert_eq!(ToolChoice::from(" hx ").normalized(), Some(ToolChoice::from("hx")));
        assert_eq!(ToolChoice::app("").normalized(), None);
    }

    #[test]
    fn the_catalogs_are_well_formed() {
        for t in EDITOR_CLIS.iter().chain(DIFF_CLIS) {
            assert!(t.command.starts_with(t.program), "{t:?} runs its program");
        }
        for a in EDITOR_APPS.iter().chain(DIFF_APPS) {
            assert!(!a.bundles.is_empty() && a.bundles.iter().all(|b| b.ends_with(".app")), "{a:?}");
            assert!(a.bundle_id.contains('.'), "{a:?}");
        }
        for a in DIFF_APPS {
            let d = a.diff.expect("a diff app says how it diffs");
            if d.style != DiffStyle::Worktree {
                assert!(d.cmd.contains("{helper}") && d.cmd.contains("$LOCAL") && d.cmd.contains("$REMOTE"), "{a:?}");
            }
        }
        assert!(EDITOR_APPS.iter().chain(DIFF_APPS).all(|a| !a.name.contains("Cursor")));
    }

    #[test]
    fn detection_lists_installed_clis_then_apps_in_catalog_order() {
        let root = scratch("detect");
        let bin = root.join("bin");
        for p in ["hx", "nvim", "difft"] {
            executable(&bin.join(p));
        }
        // On the PATH but not executable: not installed.
        std::fs::write(bin.join("vim"), "").unwrap();
        let apps = root.join("Applications");
        let home_apps = root.join("home/Applications");
        bundle(&apps.join("Zed.app"));
        bundle(&home_apps.join("PyCharm CE.app"));
        bundle(&apps.join("Kaleidoscope.app"));
        bundle(&apps.join("Xcode.app"));
        bundle(&apps.join("Xcode.app/Contents/Applications/FileMerge.app"));
        // A folder named like an app, but no bundle: not installed.
        std::fs::create_dir_all(apps.join("Nova.app")).unwrap();

        let found = detect_in(&[bin, root.join("missing")], &[apps.clone(), home_apps.clone()]);
        let names = |opts: &[ToolOption]| opts.iter().map(|o| o.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&found.editors), ["Neovim", "Helix", "Zed", "Xcode", "PyCharm"]);
        assert_eq!(names(&found.diff_tools), ["Neovim", "Difftastic", "FileMerge", "Kaleidoscope"]);

        let nvim = &found.editors[0];
        assert_eq!((nvim.kind, &nvim.choice, &nvim.app_path), (ToolKind::Cli, &ToolChoice::from("nvim"), &None));
        assert_eq!(found.diff_tools[0].choice, ToolChoice::from("nvim -d"));
        let pycharm = found.editors.iter().find(|o| o.name == "PyCharm").unwrap();
        assert_eq!(pycharm.kind, ToolKind::App);
        assert_eq!(pycharm.choice, ToolChoice::app(home_apps.join("PyCharm CE.app")));
        assert_eq!(pycharm.app_path.as_deref(), Some(home_apps.join("PyCharm CE.app").as_path()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn browse_makes_an_app_or_a_command_and_refuses_the_rest() {
        let root = scratch("browse");
        let app = root.join("My Editor.app");
        bundle(&app);
        assert_eq!(choice_from_path(&app), Ok(ToolChoice::app(&app)));
        assert_eq!(ToolChoice::app(&app).display_name(), "My Editor");

        let exe = root.join("my tools/ed");
        executable(&exe);
        let choice = choice_from_path(&exe).unwrap();
        assert_eq!(choice, ToolChoice::Command(shelbi_core::shell_escape(&exe.to_string_lossy())));
        assert!(choice.command().unwrap().starts_with('\''), "a path with a space is quoted");

        let not_bundle = root.join("Fake.app");
        std::fs::create_dir_all(&not_bundle).unwrap();
        assert!(choice_from_path(&not_bundle).unwrap_err().contains("isn't an app bundle"));
        assert!(choice_from_path(&root).unwrap_err().contains("is a folder"));
        let plain = root.join("notes.txt");
        std::fs::write(&plain, "").unwrap();
        assert!(choice_from_path(&plain).unwrap_err().contains("isn't executable"));
        assert!(choice_from_path(&root.join("gone")).unwrap_err().contains("doesn't exist"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_editor_opens_a_command_in_a_pane_and_an_app_in_the_app() {
        assert_eq!(editor_launch(None, "nvim"), EditorLaunch::Pane("nvim".into()));
        assert_eq!(editor_launch(Some(&" ".into()), "vim"), EditorLaunch::Pane("vim".into()));
        assert_eq!(editor_launch(Some(&"code --wait".into()), "vim"), EditorLaunch::Pane("code --wait".into()));
        assert_eq!(
            editor_launch(Some(&ToolChoice::app("/Applications/Zed.app")), "vim"),
            EditorLaunch::App("/Applications/Zed.app".into())
        );
    }

    #[test]
    fn a_diff_command_launches_by_what_it_is() {
        let none: &[PathBuf] = &[];
        assert_eq!(diff_launch(None, none), Ok(DiffLaunch::GitConfigured));
        assert_eq!(
            diff_launch(Some(&"nvim -d".into()), none),
            Ok(DiffLaunch::Difftool { cmd: "nvim -d \"$LOCAL\" \"$REMOTE\"".into(), dir_diff: false })
        );
        assert_eq!(
            diff_launch(Some(&"difft".into()), none),
            Ok(DiffLaunch::Pager { cmd: "difft --color=always".into() })
        );
        assert_eq!(
            diff_launch(Some(&"difft --color=never".into()), none),
            Ok(DiffLaunch::Pager { cmd: "difft --color=never".into() })
        );
        assert_eq!(
            diff_launch(Some(&"opendiff".into()), none),
            Ok(DiffLaunch::Difftool { cmd: "opendiff \"$LOCAL\" \"$REMOTE\" | cat".into(), dir_diff: true })
        );
        assert_eq!(
            diff_launch(Some(&"meld \"$REMOTE\" \"$LOCAL\"".into()), none),
            Ok(DiffLaunch::Difftool { cmd: "meld \"$REMOTE\" \"$LOCAL\"".into(), dir_diff: false })
        );
    }

    #[test]
    fn a_diff_app_finds_its_helper_on_the_path_then_in_the_bundle() {
        let root = scratch("helper");
        let bin = root.join("bin");
        let ks = root.join("Kaleidoscope.app");
        bundle(&ks);
        let ks_choice = ToolChoice::app(&ks);
        let err = diff_launch(Some(&ks_choice), std::slice::from_ref(&bin)).unwrap_err();
        assert!(err.contains("Kaleidoscope's `ksdiff` command isn't installed"), "{err}");

        executable(&ks.join("Contents/MacOS/ksdiff"));
        let in_bundle = shelbi_core::shell_escape(&ks.join("Contents/MacOS/ksdiff").to_string_lossy());
        assert_eq!(
            diff_launch(Some(&ks_choice), std::slice::from_ref(&bin)),
            Ok(DiffLaunch::Difftool { cmd: format!("{in_bundle} --wait \"$LOCAL\" \"$REMOTE\""), dir_diff: true })
        );
        executable(&bin.join("ksdiff"));
        assert_eq!(
            diff_launch(Some(&ks_choice), std::slice::from_ref(&bin)),
            Ok(DiffLaunch::Difftool { cmd: "ksdiff --wait \"$LOCAL\" \"$REMOTE\"".into(), dir_diff: true }),
            "the PATH's helper wins"
        );

        let code = root.join("Visual Studio Code.app");
        bundle(&code);
        executable(&code.join("Contents/Resources/app/bin/code"));
        let launch = diff_launch(Some(&ToolChoice::app(&code)), &[]).unwrap();
        assert!(
            matches!(&launch, DiffLaunch::Difftool { cmd, dir_diff: false } if cmd.ends_with("--wait --diff \"$LOCAL\" \"$REMOTE\"")),
            "{launch:?}"
        );

        executable(&bin.join("smerge"));
        let smerge = root.join("Sublime Merge.app");
        bundle(&smerge);
        assert_eq!(
            diff_launch(Some(&ToolChoice::app(&smerge)), std::slice::from_ref(&bin)),
            Ok(DiffLaunch::Worktree { cmd: "smerge".into() })
        );

        let other = root.join("Other Diff.app");
        bundle(&other);
        assert_eq!(
            diff_launch(Some(&ToolChoice::app(&other)), &[]),
            Ok(DiffLaunch::Difftool {
                cmd: format!("open -W -a {} \"$LOCAL\" \"$REMOTE\"", shelbi_core::shell_escape(&other.to_string_lossy())),
                dir_diff: true,
            }),
            "an app the catalog doesn't know gets both directories"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
