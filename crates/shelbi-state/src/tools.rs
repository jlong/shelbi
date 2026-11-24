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
//! On Linux an app is a desktop entry (a `.desktop` file in an XDG
//! `applications` folder, `{ app: /usr/share/applications/code.desktop }`),
//! opened by its `Exec` line ([`desktop_exec_argv`]).
//!
//! Nothing here spawns a process. Detection reads directories (the login
//! `PATH`, `/Applications`, `~/Applications`, or on Linux the XDG
//! applications folders) and is cached
//! ([`detected_tools`]); the launch side resolves a choice to an
//! [`EditorLaunch`] or a [`DiffLaunch`], which `shelbi_orchestrator::tools`
//! turns into the pane command or the app launch.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The `diff_tool:` value for Shelbi's own diff view, what an unset setting
/// means too.
pub const BUILTIN_DIFF: &str = "builtin";
/// The `diff_tool:` value for git's configured difftool (`diff.tool` /
/// `diff.guitool`).
pub const GIT_DIFF: &str = "git";

/// An `editor:` or `diff_tool:` setting: a command line or a macOS app.
///
/// ```yaml
/// editor: hx                              # a CLI command, opened in a pane
/// editor: code --wait                     # flags are fine
/// editor: { app: /Applications/Zed.app }  # an app, opened with `open -a`
/// diff_tool: builtin                      # Shelbi's own view, the default
/// diff_tool: git                          # git's configured difftool
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

    /// Whether this is `diff_tool: builtin`.
    pub fn is_builtin_diff(&self) -> bool {
        self.command().is_some_and(|c| c.trim() == BUILTIN_DIFF)
    }

    /// Whether this is `diff_tool: git`.
    pub fn is_git_diff(&self) -> bool {
        self.command().is_some_and(|c| c.trim() == GIT_DIFF)
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppTools {
    /// `editor:` is an app.
    pub editor: bool,
    /// `diff_tool:` is an app, and no project `review.diff_command` takes
    /// precedence over it.
    pub diff: bool,
    /// View Diff shows Shelbi's own diff view: `diff_tool:` is unset or
    /// `builtin`, and no project `review.diff_command` takes precedence.
    pub builtin_diff: bool,
}

/// The arguments that open `path` at `line` in the terminal or GUI editor
/// `program`, by the spelling each family takes: VS Code's `--goto`, a
/// `path:line` for those that read one, else the `+line` vi, nano and emacs
/// share. An editor that doesn't know `+line` still opens the file.
pub fn editor_args_at_line(program: &str, path: &str, line: Option<u32>) -> Vec<String> {
    let Some(line) = line else { return vec![path.to_string()] };
    let name = program.rsplit('/').next().unwrap_or(program);
    match name {
        "code" | "code-insiders" | "cursor" | "windsurf" | "codium" => {
            vec!["--goto".to_string(), format!("{path}:{line}")]
        }
        "zed" | "subl" | "hx" | "helix" | "mate" => vec![format!("{path}:{line}")],
        _ => vec![format!("+{line}"), path.to_string()],
    }
}

/// Whether `diff_tool:` set to `choice` means Shelbi's own diff view.
pub fn is_builtin_diff(choice: Option<&ToolChoice>) -> bool {
    match choice.cloned().and_then(ToolChoice::normalized) {
        None => true,
        Some(choice) => choice.is_builtin_diff(),
    }
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

/// An app Shelbi knows, on macOS and on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownApp {
    pub name: &'static str,
    pub bundle_id: &'static str,
    /// Where it installs, relative to an applications folder
    /// (`/Applications`, `~/Applications`): every bundle name it ships as.
    pub bundles: &'static [&'static str],
    /// Its Linux desktop entries' file names, in an XDG `applications`
    /// folder: every name a package, Flatpak or JetBrains Toolbox installs
    /// it as.
    pub desktop_ids: &'static [&'static str],
    /// How it diffs, for a diff tool.
    pub diff: Option<AppDiff>,
}

const fn editor_app(
    name: &'static str,
    bundle_id: &'static str,
    bundles: &'static [&'static str],
    desktop_ids: &'static [&'static str],
) -> KnownApp {
    KnownApp { name, bundle_id, bundles, desktop_ids, diff: None }
}

/// VS Code's desktop entries: Microsoft's package, the AUR's, Flathub's,
/// and Code - OSS.
const VSCODE_DESKTOP: &[&str] = &["code.desktop", "visual-studio-code.desktop", "com.visualstudio.code.desktop", "code-oss.desktop"];

/// The editor apps Shelbi lists when installed, in the order shown. (Users
/// pick any other app with Browse.)
pub const EDITOR_APPS: &[KnownApp] = &[
    editor_app("Visual Studio Code", "com.microsoft.VSCode", &["Visual Studio Code.app"], VSCODE_DESKTOP),
    editor_app("Zed", "dev.zed.Zed", &["Zed.app"], &["dev.zed.Zed.desktop", "zed.desktop"]),
    editor_app("Sublime Text", "com.sublimetext.4", &["Sublime Text.app"], &["sublime_text.desktop", "com.sublimetext.three.desktop"]),
    editor_app("BBEdit", "com.barebones.bbedit", &["BBEdit.app"], &[]),
    editor_app("Nova", "com.panic.Nova", &["Nova.app"], &[]),
    editor_app("TextMate", "com.macromates.TextMate", &["TextMate.app"], &[]),
    editor_app("Xcode", "com.apple.dt.Xcode", &["Xcode.app"], &[]),
    editor_app("MacVim", "org.vim.MacVim", &["MacVim.app"], &[]),
    editor_app("Emacs", "org.gnu.Emacs", &["Emacs.app"], &["emacs.desktop"]),
    editor_app("CotEditor", "com.coteditor.CotEditor", &["CotEditor.app"], &[]),
    editor_app("IntelliJ IDEA", "com.jetbrains.intellij", &["IntelliJ IDEA.app", "IntelliJ IDEA CE.app", "IntelliJ IDEA Ultimate.app"], &["jetbrains-idea.desktop", "jetbrains-idea-ce.desktop"]),
    editor_app("WebStorm", "com.jetbrains.WebStorm", &["WebStorm.app"], &["jetbrains-webstorm.desktop"]),
    editor_app("RustRover", "com.jetbrains.rustrover", &["RustRover.app"], &["jetbrains-rustrover.desktop"]),
    editor_app("GoLand", "com.jetbrains.goland", &["GoLand.app"], &["jetbrains-goland.desktop"]),
    editor_app("PyCharm", "com.jetbrains.pycharm", &["PyCharm.app", "PyCharm CE.app", "PyCharm Professional Edition.app"], &["jetbrains-pycharm.desktop", "jetbrains-pycharm-ce.desktop"]),
    editor_app("CLion", "com.jetbrains.CLion", &["CLion.app"], &["jetbrains-clion.desktop"]),
    editor_app("PhpStorm", "com.jetbrains.PhpStorm", &["PhpStorm.app"], &["jetbrains-phpstorm.desktop"]),
    editor_app("RubyMine", "com.jetbrains.rubymine", &["RubyMine.app"], &["jetbrains-rubymine.desktop"]),
];

/// The diff apps Shelbi lists when installed, in the order shown.
pub const DIFF_APPS: &[KnownApp] = &[
    KnownApp {
        name: "FileMerge",
        bundle_id: "com.apple.FileMerge",
        bundles: &["Xcode.app/Contents/Applications/FileMerge.app"],
        desktop_ids: &[],
        // `| cat` holds opendiff until FileMerge closes the comparison (git's
        // own opendiff driver does the same).
        diff: Some(AppDiff { program: "opendiff", in_bundle: &[], cmd: "{helper} \"$LOCAL\" \"$REMOTE\" | cat", style: DiffStyle::Dirs }),
    },
    KnownApp {
        name: "Kaleidoscope",
        bundle_id: "app.kaleidoscope.v4",
        bundles: &["Kaleidoscope.app"],
        desktop_ids: &[],
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
        desktop_ids: &["bcompare.desktop"],
        diff: Some(AppDiff { program: "bcomp", in_bundle: &["Contents/MacOS/bcomp"], cmd: "{helper} \"$LOCAL\" \"$REMOTE\"", style: DiffStyle::Dirs }),
    },
    KnownApp {
        name: "Visual Studio Code",
        bundle_id: "com.microsoft.VSCode",
        bundles: &["Visual Studio Code.app"],
        desktop_ids: VSCODE_DESKTOP,
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
        desktop_ids: &["sublime_merge.desktop"],
        diff: Some(AppDiff { program: "smerge", in_bundle: &["Contents/SharedSupport/bin/smerge"], cmd: "", style: DiffStyle::Worktree }),
    },
    KnownApp {
        name: "P4Merge",
        bundle_id: "com.perforce.p4merge",
        bundles: &["p4merge.app", "P4Merge.app"],
        desktop_ids: &[],
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
        desktop_ids: &[],
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
        desktop_ids: &[],
        diff: Some(AppDiff {
            program: "DeltaWalker",
            in_bundle: &["Contents/MacOS/DeltaWalker"],
            cmd: "{helper} \"$LOCAL\" \"$REMOTE\"",
            style: DiffStyle::Files,
        }),
    },
    KnownApp {
        name: "Meld",
        bundle_id: "org.gnome.Meld",
        bundles: &["Meld.app"],
        desktop_ids: &["org.gnome.Meld.desktop", "meld.desktop"],
        // `meld` stays in the foreground until its window closes.
        diff: Some(AppDiff { program: "meld", in_bundle: &["Contents/MacOS/Meld"], cmd: "{helper} \"$LOCAL\" \"$REMOTE\"", style: DiffStyle::Dirs }),
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
    ("meld", "Meld"),
    ("zeditor", "Zed"),
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
/// path, so `FileMerge.app` inside any Xcode matches, or on a desktop
/// entry's file name), from `table`.
pub fn known_app<'a>(table: &'a [KnownApp], app: &Path) -> Option<&'a KnownApp> {
    let file = app.file_name().and_then(|f| f.to_str());
    table
        .iter()
        .find(|k| k.bundles.iter().any(|b| app.ends_with(b)) || file.is_some_and(|f| k.desktop_ids.contains(&f)))
}

/// An app's display name: the catalog's name for a known bundle, else a
/// desktop entry's `Name`, else the bundle's file name without `.app`
/// (`/Applications/Foo Bar.app` → `Foo Bar`).
pub fn app_display_name(app: &Path) -> String {
    if let Some(k) = known_app(EDITOR_APPS, app).or_else(|| known_app(DIFF_APPS, app)) {
        return k.name.to_string();
    }
    if is_desktop_file_name(app) {
        if let Some(name) = std::fs::read_to_string(app).ok().and_then(|text| desktop_entry_value(&text, "Name")) {
            return name;
        }
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
    /// `diff.tool`), what `diff_tool: git` runs.
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

/// A path named `*.desktop`.
fn is_desktop_file_name(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("desktop"))
}

/// A desktop entry an app launcher would list: a `.desktop` file whose
/// `[Desktop Entry]` is an `Application` with an `Exec`, not `Hidden`.
pub fn is_desktop_entry(path: &Path) -> bool {
    if !is_desktop_file_name(path) {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    desktop_entry_value(&text, "Type").as_deref() == Some("Application")
        && desktop_entry_value(&text, "Exec").is_some()
        && desktop_entry_value(&text, "Hidden").as_deref() != Some("true")
}

/// `key`'s value in a desktop entry's `[Desktop Entry]` group (the
/// unlocalized one: `Name`, not `Name[de]`).
pub fn desktop_entry_value(text: &str, key: &str) -> Option<String> {
    let mut in_entry = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == key {
            let v = v.trim();
            return (!v.is_empty()).then(|| v.to_string());
        }
    }
    None
}

/// The command line a desktop entry's `Exec` runs on `files` (the Desktop
/// Entry spec's quoting and field codes): `%f`/`%u` take the first file,
/// `%F`/`%U` all of them, `%c` the entry's name, `%k` its path, `%i` its
/// icon (`--icon <icon>`), `%%` a `%`; the deprecated codes go. An `Exec`
/// with no file code gets the files appended. `Err` when there is no
/// `Exec` to run.
pub fn desktop_exec_argv(desktop: &Path, files: &[String]) -> Result<Vec<String>, String> {
    let shown = desktop.display();
    let text = std::fs::read_to_string(desktop).map_err(|e| format!("couldn't read {shown}: {e}"))?;
    let exec = desktop_entry_value(&text, "Exec").ok_or_else(|| format!("{shown} has no Exec line to run"))?;
    let name = desktop_entry_value(&text, "Name").unwrap_or_default();
    let icon = desktop_entry_value(&text, "Icon");
    let mut argv = Vec::new();
    let mut took_files = false;
    for word in exec_words(&exec) {
        match word.as_str() {
            "%f" | "%u" => {
                argv.extend(files.first().cloned());
                took_files = true;
            }
            "%F" | "%U" => {
                argv.extend(files.iter().cloned());
                took_files = true;
            }
            "%i" => {
                if let Some(icon) = &icon {
                    argv.extend(["--icon".to_string(), icon.clone()]);
                }
            }
            _ => {
                let mut out = String::new();
                let mut chars = word.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        out.push(c);
                        continue;
                    }
                    match chars.next() {
                        Some('%') => out.push('%'),
                        Some('c') => out.push_str(&name),
                        Some('k') => out.push_str(&desktop.to_string_lossy()),
                        // %d %D %n %N %v %m, and a code mid-word: dropped.
                        _ => {}
                    }
                }
                if !out.is_empty() {
                    argv.push(out);
                }
            }
        }
    }
    if argv.is_empty() {
        return Err(format!("{shown}'s Exec line is empty"));
    }
    if !took_files {
        argv.extend(files.iter().cloned());
    }
    Ok(argv)
}

/// An `Exec` value split into words: double quotes group, and inside them
/// a backslash escapes `"`, `` ` ``, `$` and `\`.
fn exec_words(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            c => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

/// The first of `app`'s bundles (or, on Linux, desktop entries) installed
/// in `app_dirs`.
fn installed_bundle(app: &KnownApp, app_dirs: &[PathBuf]) -> Option<PathBuf> {
    app_dirs.iter().find_map(|dir| {
        app.bundles
            .iter()
            .map(|b| dir.join(b))
            .find(|p| is_app_bundle(p))
            .or_else(|| app.desktop_ids.iter().map(|d| dir.join(d)).find(|p| is_desktop_entry(p)))
    })
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
#[cfg(target_os = "macos")]
pub fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/Applications")];
    if let Some(home) = shelbi_core::home_dir() {
        dirs.push(home.join("Applications"));
    }
    dirs
}

/// The folders desktop entries are found in, the user's first: the
/// `applications` folder of `$XDG_DATA_HOME` (`~/.local/share`), of each of
/// `$XDG_DATA_DIRS` (`/usr/local/share:/usr/share`), and of Flatpak's
/// exports.
#[cfg(not(target_os = "macos"))]
pub fn app_dirs() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    xdg_app_dirs(shelbi_core::home_dir().as_deref(), var("XDG_DATA_HOME"), var("XDG_DATA_DIRS"))
}

/// [`app_dirs`] for a home and the two XDG variables.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn xdg_app_dirs(home: Option<&Path>, data_home: Option<String>, data_dirs: Option<String>) -> Vec<PathBuf> {
    let mut data: Vec<PathBuf> = Vec::new();
    match data_home {
        Some(d) => data.push(PathBuf::from(d)),
        None => data.extend(home.map(|h| h.join(".local/share"))),
    }
    data.extend(home.map(|h| h.join(".local/share/flatpak/exports/share")));
    let dirs = data_dirs.unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    data.extend(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    data.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in data.into_iter().map(|d| d.join("applications")) {
        if !out.contains(&dir) {
            out.push(dir);
        }
    }
    out
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
    if is_desktop_file_name(path) {
        return if is_desktop_entry(path) {
            Ok(ToolChoice::app(path))
        } else {
            Err(format!("{shown} isn't an application's desktop entry (no Type=Application with an Exec)"))
        };
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
    /// `diff.guitool`): what `diff_tool: git` runs.
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
    // Neither names a program to run: `git` is git's configured tool, and the
    // built-in view is no process at all, so a path that needs one (a client
    // that hasn't routed to the view) falls back to git's.
    if choice.is_builtin_diff() || choice.is_git_diff() {
        return Ok(DiffLaunch::GitConfigured);
    }
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
                if is_desktop_file_name(&app) {
                    // A desktop entry's Exec opens a window and returns: git
                    // would delete the sides from under it.
                    return Err(format!(
                        "Shelbi doesn't know how {} compares two folders; pick a diff command instead",
                        app_display_name(&app)
                    ));
                }
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
mod diff_setting_tests {
    use super::*;

    #[test]
    fn unset_and_builtin_are_the_built_in_view() {
        assert!(is_builtin_diff(None));
        assert!(is_builtin_diff(Some(&"builtin".into())));
        assert!(is_builtin_diff(Some(&"  ".into())), "a blank setting is no setting");
        assert!(!is_builtin_diff(Some(&"git".into())));
        assert!(!is_builtin_diff(Some(&"difft".into())));
        assert!(!is_builtin_diff(Some(&ToolChoice::app("/Applications/Kaleidoscope.app"))));
    }

    #[test]
    fn neither_builtin_nor_git_runs_as_a_command() {
        for value in ["builtin", "git"] {
            assert_eq!(diff_launch(Some(&value.into()), &[]), Ok(DiffLaunch::GitConfigured), "{value}");
        }
        // External tools launch as they did.
        assert!(matches!(diff_launch(Some(&"difft".into()), &[]), Ok(DiffLaunch::Pager { .. })));
        assert!(matches!(diff_launch(Some(&"nvim -d".into()), &[]), Ok(DiffLaunch::Difftool { .. })));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_editor_opens_at_a_line_by_its_own_spelling() {
        assert_eq!(editor_args_at_line("vim", "a.rs", Some(7)), ["+7", "a.rs"]);
        assert_eq!(editor_args_at_line("/usr/bin/nvim", "a.rs", Some(7)), ["+7", "a.rs"]);
        assert_eq!(editor_args_at_line("code", "a.rs", Some(7)), ["--goto", "a.rs:7"]);
        assert_eq!(editor_args_at_line("zed", "a.rs", Some(7)), ["a.rs:7"]);
        assert_eq!(editor_args_at_line("vim", "a.rs", None), ["a.rs"]);
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shelbi-tools-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        crate::test_support::write_executable(path, "#!/bin/sh\n");
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
        let cli: Doc = yaml_serde::from_str("editor: code --wait\n").unwrap();
        assert_eq!(cli.editor, ToolChoice::Command("code --wait".into()));
        assert_eq!(yaml_serde::to_string(&cli).unwrap(), "editor: code --wait\n");

        let app: Doc = yaml_serde::from_str("editor: { app: /Applications/Zed.app }\n").unwrap();
        assert_eq!(app.editor, ToolChoice::app("/Applications/Zed.app"));
        let text = yaml_serde::to_string(&app).unwrap();
        assert_eq!(yaml_serde::from_str::<Doc>(&text).unwrap(), app, "{text}");

        let block: Doc = yaml_serde::from_str("editor:\n  app: /Applications/Zed.app\n  future: 1\n").unwrap();
        assert_eq!(block.editor, ToolChoice::app("/Applications/Zed.app"), "unknown keys are ignored");
        assert!(yaml_serde::from_str::<Doc>("editor: [1]\n").is_err());
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

    fn desktop_entry(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("[Desktop Entry]\nType=Application\n{body}")).unwrap();
    }

    #[test]
    fn linux_apps_are_desktop_entries_in_the_xdg_folders() {
        let root = scratch("desktop-entries");
        let user = root.join("home/.local/share/applications");
        let system = root.join("usr/share/applications");
        desktop_entry(&system.join("code.desktop"), "Name=Visual Studio Code\nExec=/usr/bin/code %F\nIcon=vscode\n");
        desktop_entry(&user.join("dev.zed.Zed.desktop"), "Name=Zed\nExec=zeditor %U\nIcon=zed\n");
        desktop_entry(&system.join("org.gnome.Meld.desktop"), "Name=Meld\nExec=meld %F\n");
        // Hidden, or no Exec: not an app a launcher lists.
        desktop_entry(&system.join("sublime_text.desktop"), "Name=Sublime Text\nExec=subl\nHidden=true\n");
        desktop_entry(&system.join("emacs.desktop"), "Name=Emacs\n");

        let found = detect_in(&[], &[user.clone(), system.clone()]);
        let names = |opts: &[ToolOption]| opts.iter().map(|o| o.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&found.editors), ["Visual Studio Code", "Zed"]);
        assert_eq!(names(&found.diff_tools), ["Visual Studio Code", "Meld"]);
        let zed = &found.editors[1];
        assert_eq!(zed.app_path.as_deref(), Some(user.join("dev.zed.Zed.desktop").as_path()));
        assert_eq!(ToolChoice::app(system.join("org.gnome.Meld.desktop")).display_name(), "Meld");

        // Browse takes a desktop entry as an app, named by its Name.
        let other = root.join("other.desktop");
        desktop_entry(&other, "Name=Kate\nExec=kate -b %U\n");
        assert_eq!(choice_from_path(&other), Ok(ToolChoice::app(&other)));
        assert_eq!(ToolChoice::app(&other).display_name(), "Kate");
        std::fs::write(root.join("broken.desktop"), "[Desktop Entry]\nName=x\n").unwrap();
        assert!(choice_from_path(&root.join("broken.desktop")).is_err());

        // A known diff app runs its helper; an unknown one is refused.
        let meld = diff_launch(Some(&ToolChoice::app(system.join("org.gnome.Meld.desktop"))), &[]);
        assert!(meld.is_err(), "meld isn't on the (empty) PATH: {meld:?}");
        let bin = root.join("bin");
        executable(&bin.join("meld"));
        assert_eq!(
            diff_launch(Some(&ToolChoice::app(system.join("org.gnome.Meld.desktop"))), &[bin]),
            Ok(DiffLaunch::Difftool { cmd: "meld \"$LOCAL\" \"$REMOTE\"".into(), dir_diff: true })
        );
        assert!(diff_launch(Some(&ToolChoice::app(&other)), &[]).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_desktop_entry_runs_its_exec_line_on_the_files() {
        let root = scratch("desktop-exec");
        let entry = root.join("code.desktop");
        desktop_entry(&entry, "Name=Code\nExec=/usr/bin/code --new-window %F\nIcon=vscode\n");
        let files = vec!["/w/a".to_string(), "/w/b".to_string()];
        assert_eq!(desktop_exec_argv(&entry, &files).unwrap(), ["/usr/bin/code", "--new-window", "/w/a", "/w/b"]);
        desktop_entry(&entry, "Name=Zed\nExec=zeditor %u\n");
        assert_eq!(desktop_exec_argv(&entry, &files).unwrap(), ["zeditor", "/w/a"]);
        // No file code: the files go last. Quotes group, deprecated codes go.
        desktop_entry(&entry, "Name=Ed\nExec=\"/opt/My Ed/ed\" --title=%c %i %d 100%%\nIcon=ed\n");
        assert_eq!(
            desktop_exec_argv(&entry, &files[..1]).unwrap(),
            ["/opt/My Ed/ed", "--title=Ed", "--icon", "ed", "100%", "/w/a"]
        );
        // Only the [Desktop Entry] group counts.
        std::fs::write(&entry, "[Desktop Action new]\nExec=x\n").unwrap();
        assert!(desktop_exec_argv(&entry, &files).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn xdg_app_dirs_put_the_users_first() {
        let home = Path::new("/home/me");
        let dirs: Vec<String> = xdg_app_dirs(Some(home), None, None).iter().map(|d| d.display().to_string()).collect();
        assert_eq!(
            dirs,
            [
                "/home/me/.local/share/applications",
                "/home/me/.local/share/flatpak/exports/share/applications",
                "/usr/local/share/applications",
                "/usr/share/applications",
                "/var/lib/flatpak/exports/share/applications",
            ]
        );
        let dirs = xdg_app_dirs(Some(home), Some("/d".into()), Some("/a:/usr/share".into()));
        assert_eq!(dirs[0], PathBuf::from("/d/applications"));
        assert_eq!(dirs[2], PathBuf::from("/a/applications"));
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
