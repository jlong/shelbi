//! Asking the user for an SSH password, passphrase or confirmation in a
//! system dialog, through OpenSSH's askpass mechanism.
//!
//! A connection the user starts (Connect to remote, opening a remote
//! workspace) runs `ssh` without `BatchMode` and with:
//!
//! - `SSH_ASKPASS=<this executable>` and `SSH_ASKPASS_REQUIRE=force`
//!   (OpenSSH 8.4+), so every prompt ssh has goes to the helper, whether or
//!   not there is a terminal or a `DISPLAY`;
//! - [`HELPER_ENV`]`=1`, which tells this executable, when ssh runs it as
//!   `<exe> "<prompt>"`, to be the helper ([`helper_requested`]) rather than
//!   Shelbi (both `shelbi` and the desktop app check it first thing in
//!   `main`; it takes exactly that shape, with `SSH_ASKPASS_REQUIRE` set too,
//!   so a `shelbi` that merely inherited the variable runs as itself);
//! - [`HOST_ENV`], the host as the user typed it, so the dialog says who is
//!   asking.
//!
//! The helper shows ssh's own prompt text in a native macOS dialog
//! (`osascript`'s `display dialog`): a hidden text field for a password,
//! passphrase or one-time code, Yes/No buttons for a yes/no question (a new
//! host key; No is the default, so Return refuses), an OK button for a
//! notice (touch your security key). A keyboard-interactive prompt is the
//! server's own text, so the dialog leads with a line Shelbi writes naming
//! the host and quotes the prompt as that host's ([`dialog_text`]): a
//! hostile host can't pass its prompt off as Shelbi's or the Mac's. Its
//! answer goes from `osascript`'s stdout straight to ssh: Shelbi never reads,
//! logs or stores it, and it is never on a command line. Cancel (or No)
//! exits non-zero, which ssh reads as a refusal.
//!
//! The dialog is shown only where one can be: macOS, `osascript` present,
//! and a GUI login session (`launchctl managername` is `Aqua`, not an SSH
//! or background session). Elsewhere [`Askpass::available`] is `None` and
//! callers keep `BatchMode=yes`, so ssh never prompts. [`MODE_ENV`]`=never`
//! turns the dialog off. Background `ssh` (the hub's polling) never uses
//! any of this.
//!
//! A step with a deadline must not time out while the user is typing:
//! [`run_until`] pauses its clock while a helper is showing a dialog, which
//! the helper reports by holding a connection to a [`PromptWatch`] socket
//! ([`NOTIFY_ENV`]) open for as long as the dialog is up.

use std::ffi::OsString;
use std::io::{ErrorKind, Read};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Set (to `1`) in the environment of an `ssh` that may ask: when ssh runs
/// this executable as its askpass program, this says to be the helper.
pub const HELPER_ENV: &str = "SHELBI_ASKPASS_HELPER";

/// The host the user is signing in to, as they typed it: the dialog names
/// it so the remote's prompt reads as the remote's.
pub const HOST_ENV: &str = "SHELBI_ASKPASS_HOST";

/// The [`PromptWatch`] socket the helper reports an open dialog to.
pub const NOTIFY_ENV: &str = "SHELBI_ASKPASS_NOTIFY";

/// The user's switch: `never` turns the dialog off (ssh then runs with
/// `BatchMode=yes`, as with no GUI).
pub const MODE_ENV: &str = "SHELBI_ASKPASS";

/// The dialog's title.
pub const DIALOG_TITLE: &str = "Shelbi";

const OSASCRIPT: &str = "/usr/bin/osascript";

/// What ssh is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// A password, key passphrase or one-time code: a hidden text field.
    Secret,
    /// A yes/no question (a host key to accept, an agent confirmation):
    /// Yes and No buttons, no text field.
    Confirm,
    /// Information only, such as "Confirm user presence for key …" while a
    /// security key waits for a touch: an OK button. ssh closes it itself.
    Notice,
}

impl PromptKind {
    /// The word the dialog script reads.
    fn script_word(self) -> &'static str {
        match self {
            PromptKind::Secret => "secret",
            PromptKind::Confirm => "confirm",
            PromptKind::Notice => "notice",
        }
    }
}

/// What kind of answer `prompt` wants. `askpass_prompt` is ssh's
/// `SSH_ASKPASS_PROMPT` hint: `confirm` for a yes/no confirmation, `none`
/// for a notice; unset for anything that wants typed text. A host-key
/// question comes without the hint, so a prompt ending in a `(yes/no…)?`
/// choice is a confirmation too.
pub fn classify_prompt(prompt: &str, askpass_prompt: Option<&str>) -> PromptKind {
    match askpass_prompt.map(str::trim) {
        Some(hint) if hint.eq_ignore_ascii_case("confirm") => return PromptKind::Confirm,
        Some(hint) if hint.eq_ignore_ascii_case("none") => return PromptKind::Notice,
        _ => {}
    }
    let text = prompt.trim_end().to_ascii_lowercase();
    if text.ends_with('?') && text.contains("(yes/no") {
        PromptKind::Confirm
    } else {
        PromptKind::Secret
    }
}

/// The askpass setup an `ssh` the user started runs with. See the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Askpass {
    helper: PathBuf,
    host: Option<String>,
}

impl Askpass {
    /// Askpass through `helper`, an executable that answers
    /// [`helper_requested`] first thing.
    pub fn new(helper: impl Into<PathBuf>) -> Askpass {
        Askpass {
            helper: helper.into(),
            host: None,
        }
    }

    /// This setup for a sign-in to `host` (as the user typed it), which the
    /// dialog names ([`HOST_ENV`]).
    pub fn with_host(mut self, host: impl Into<String>) -> Askpass {
        self.host = Some(host.into());
        self
    }

    /// The host the dialog names, if any.
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// This executable as the helper, when a dialog can be shown here.
    /// `None` off macOS, without `osascript`, outside a GUI login session, or
    /// with [`MODE_ENV`]`=never`.
    pub fn available() -> Option<Askpass> {
        if !dialog_possible() {
            return None;
        }
        std::env::current_exe().ok().map(Askpass::new)
    }

    /// The helper executable.
    pub fn helper(&self) -> &Path {
        &self.helper
    }

    /// The environment ssh runs with so it asks through the helper.
    pub fn env(&self) -> Vec<(&'static str, OsString)> {
        let mut env = vec![
            ("SSH_ASKPASS", self.helper.clone().into_os_string()),
            ("SSH_ASKPASS_REQUIRE", "force".into()),
            (HELPER_ENV, "1".into()),
        ];
        if let Some(host) = &self.host {
            env.push((HOST_ENV, host.into()));
        }
        env
    }

    /// Set [`Askpass::env`] on `cmd`.
    pub fn apply(&self, cmd: &mut Command) {
        for (key, value) in self.env() {
            cmd.env(key, value);
        }
    }
}

/// Whether a system dialog can be shown from this process. See
/// [`Askpass::available`].
pub fn dialog_possible() -> bool {
    if std::env::var(MODE_ENV).is_ok_and(|m| m.trim().eq_ignore_ascii_case("never")) {
        return false;
    }
    platform_dialog_possible()
}

#[cfg(target_os = "macos")]
fn platform_dialog_possible() -> bool {
    Path::new(OSASCRIPT).exists() && gui_session()
}

#[cfg(not(target_os = "macos"))]
fn platform_dialog_possible() -> bool {
    false
}

/// Whether this process is in a live GUI login session: `launchctl
/// managername` says `Aqua` (an SSH login or a background job says
/// otherwise, and a dialog there would never be seen).
#[cfg(target_os = "macos")]
fn gui_session() -> bool {
    match Command::new("launchctl")
        .arg("managername")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "Aqua",
        _ => false,
    }
}

/// The line a client shows when ssh wouldn't let the user in. `asked` is
/// whether ssh could ask the user (the dialog was available): then the
/// reason is the whole story (a wrong password, a Cancel); otherwise the
/// host most likely wanted a password or passphrase Shelbi had no way to
/// ask for, and the fix is a key or an agent.
pub fn auth_failed_message(host: &str, reason: &str, asked: bool) -> String {
    let reason = reason.trim().trim_end_matches('.');
    if asked {
        format!("Couldn't sign in to {host}: {reason}.")
    } else {
        format!(
            "Couldn't sign in to {host}: {reason}. If it wants a password or a key passphrase, \
             Shelbi can't ask for one here: add an SSH key for it, or load yours into the agent \
             with `ssh-add`."
        )
    }
}

// ===========================================================================
// The helper
// ===========================================================================

/// When this process was started by ssh as its askpass program, show the
/// dialog and return the exit code to exit with. `None` otherwise. Call it
/// first thing in `main`. See [`helper_prompt`] for what counts.
pub fn helper_requested() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let prompt = helper_prompt(
        std::env::var_os(HELPER_ENV).as_deref(),
        std::env::var_os("SSH_ASKPASS_REQUIRE").as_deref(),
        &args,
    )?;
    Some(run_helper(&prompt))
}

/// The prompt to ask, when this process has ssh's askpass shape: `helper`
/// ([`HELPER_ENV`]) is `1`, `require` (`SSH_ASKPASS_REQUIRE`, which ssh's
/// askpass child inherits from [`Askpass::env`]) is set, and `args` (after
/// the program name) is exactly one argument, the prompt. Anything else is
/// a process that merely inherited the variables (an ssh `ProxyCommand` or
/// `LocalCommand` child, a `SendEnv`): `None`, so it runs as Shelbi.
pub fn helper_prompt(
    helper: Option<&std::ffi::OsStr>,
    require: Option<&std::ffi::OsStr>,
    args: &[OsString],
) -> Option<String> {
    if helper != Some("1".as_ref()) || require.is_none_or(|r| r.is_empty()) {
        return None;
    }
    match args {
        [prompt] => Some(prompt.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// The text the dialog shows for ssh's `prompt` on a sign-in to `host`. A
/// keyboard-interactive prompt is whatever the server sends, so with a host
/// the dialog leads with a line Shelbi writes and quotes the prompt as the
/// host's:
///
/// ```text
/// SSH sign-in to devbox
///
/// devbox asks:
/// (jlong@devbox) Verification code:
/// ```
///
/// Without a host (an `ssh-askpass` run by hand) it is just the prompt.
/// Control characters in the host are dropped so it stays one line.
pub fn dialog_text(host: Option<&str>, prompt: &str) -> String {
    let prompt = prompt.trim_end();
    let host: String = host
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let host = host.trim();
    if host.is_empty() {
        return prompt.to_string();
    }
    format!("SSH sign-in to {host}\n\n{host} asks:\n{prompt}")
}

/// Show `prompt` in the dialog its kind calls for, with the answer on
/// stdout. `0` on OK / Yes; non-zero on Cancel / No, or when no dialog can
/// be shown.
pub fn run_helper(prompt: &str) -> i32 {
    let hint = std::env::var("SSH_ASKPASS_PROMPT").ok();
    let kind = classify_prompt(prompt, hint.as_deref());
    if !cfg!(target_os = "macos") || !Path::new(OSASCRIPT).exists() {
        eprintln!("shelbi: can't show a password dialog on this system");
        return 1;
    }
    // Held open while the dialog is up, so a waiting step's clock pauses.
    let _prompting = std::env::var_os(NOTIFY_ENV).and_then(|p| UnixStream::connect(p).ok());
    let icon = dialog_icon()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut cmd = Command::new(OSASCRIPT);
    for line in DIALOG_SCRIPT.lines() {
        cmd.arg("-e").arg(line);
    }
    let text = dialog_text(std::env::var(HOST_ENV).ok().as_deref(), prompt);
    // The kind leads, so a prompt can never be read as an osascript option.
    cmd.arg(kind.script_word()).arg(icon).arg(text);
    // The answer goes from osascript's stdout straight to ssh, never through
    // this process. Its "User canceled" chatter on stderr is noise.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return 1;
    };
    let parent = parent_pid();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(1),
            Ok(None) => {}
            Err(_) => return 1,
        }
        // ssh gave up on us (killed, or its caller abandoned the step): take
        // the dialog down rather than leave it orphaned on screen.
        if parent_pid() != parent {
            let _ = child.kill();
            let _ = child.wait();
            return 1;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn parent_pid() -> u32 {
    // SAFETY: getppid has no preconditions and cannot fail.
    unsafe { libc::getppid() as u32 }
}

/// The app icon for the dialog: the bundle's own when this is Shelbi.app,
/// else an installed Shelbi.app's, else none (the dialog's default).
fn dialog_icon() -> Option<PathBuf> {
    const ICON: &str = "Contents/Resources/Shelbi.icns";
    let bundled = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()?
            .parent()
            .map(|contents| contents.join("Resources/Shelbi.icns"))
    });
    let home = std::env::var_os("HOME").map(PathBuf::from);
    bundled
        .into_iter()
        .chain(std::iter::once(
            Path::new("/Applications/Shelbi.app").join(ICON),
        ))
        .chain(home.map(|h| h.join("Applications/Shelbi.app").join(ICON)))
        .find(|p| p.is_file())
}

/// The dialog, as AppleScript run by `osascript -e …`. Its arguments are the
/// [`PromptKind`] word, the icon path (empty for none) and the text
/// ([`dialog_text`]: ssh's prompt, under a line naming the host). `activate`
/// brings the dialog to the front. Cancel / No is the cancel button, which
/// ends `osascript` with an error (a non-zero exit). A confirmation defaults
/// to No: Return refuses an unverified host key, and accepting one takes a
/// click on Yes, as ssh's own prompt takes typing `yes`.
const DIALOG_SCRIPT: &str = r#"on run argv
set dialogKind to item 1 of argv
set iconPath to item 2 of argv
set promptText to item 3 of argv
if iconPath is "" then
set theIcon to note
else
set theIcon to (POSIX file iconPath) as alias
end if
activate
if dialogKind is "confirm" then
display dialog promptText with title "Shelbi" buttons {"No", "Yes"} default button "No" cancel button "No" with icon theIcon
return "yes"
else if dialogKind is "notice" then
display dialog promptText with title "Shelbi" buttons {"OK"} default button "OK" with icon theIcon
return ""
else
set answerRecord to display dialog promptText with title "Shelbi" default answer "" with hidden answer buttons {"Cancel", "OK"} default button "OK" cancel button "Cancel" with icon theIcon
return text returned of answerRecord
end if
end run"#;

// ===========================================================================
// Pausing a deadline while a dialog is open
// ===========================================================================

/// A socket a helper connects to while its dialog is up, so the step that
/// started ssh can tell the user is being asked. Removed on drop.
pub struct PromptWatch {
    listener: UnixListener,
    path: PathBuf,
    open: Vec<UnixStream>,
}

impl PromptWatch {
    /// A fresh watch socket in the temp dir.
    pub fn new() -> std::io::Result<PromptWatch> {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "shelbi-askpass-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(PromptWatch {
            listener,
            path,
            open: Vec::new(),
        })
    }

    /// The socket's path ([`NOTIFY_ENV`]'s value).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Point `cmd`'s helpers at this watch.
    pub fn apply(&self, cmd: &mut Command) {
        cmd.env(NOTIFY_ENV, &self.path);
    }

    /// Whether a helper has a dialog open right now.
    pub fn prompting(&mut self) -> bool {
        while let Ok((stream, _)) = self.listener.accept() {
            if stream.set_nonblocking(true).is_ok() {
                self.open.push(stream);
            }
        }
        // A helper never writes: EOF (or an error) means its dialog closed.
        self.open.retain_mut(|stream| {
            let mut byte = [0u8; 1];
            matches!(stream.read(&mut byte), Err(e) if e.kind() == ErrorKind::WouldBlock)
        });
        !self.open.is_empty()
    }
}

impl Drop for PromptWatch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Run `cmd` (stdin null, stdout and stderr captured) until it exits,
/// killing it once it has run for `deadline` — not counting time a dialog
/// was open. `askpass` is the setup to ask through, `None` to run `cmd` as
/// configured. A deadline that runs out is `ErrorKind::TimedOut`.
pub fn run_until(
    cmd: &mut Command,
    askpass: Option<&Askpass>,
    deadline: Duration,
) -> std::io::Result<Output> {
    let mut watch = match askpass {
        Some(askpass) => {
            askpass.apply(cmd);
            let watch = PromptWatch::new().ok();
            if let Some(watch) = &watch {
                watch.apply(cmd);
            }
            watch
        }
        None => None,
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let mut counted = Duration::ZERO;
    let mut last = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let now = Instant::now();
        let prompting = watch.as_mut().is_some_and(PromptWatch::prompting);
        if !prompting {
            counted += now - last;
        }
        last = now;
        if counted >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                ErrorKind::TimedOut,
                format!("no answer after {}s", deadline.as_secs()),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Read a child's pipe to the end on its own thread, so a chatty one never
/// blocks on a full pipe.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_passphrases_and_codes_are_secret() {
        for prompt in [
            "jlong@devbox's password: ",
            "Enter passphrase for key '/Users/jlong/.ssh/id_ed25519': ",
            "(jlong@devbox) Verification code: ",
            "Password:",
            "",
        ] {
            assert_eq!(
                classify_prompt(prompt, None),
                PromptKind::Secret,
                "{prompt:?}"
            );
        }
    }

    #[test]
    fn a_host_key_question_is_a_confirmation() {
        let prompt = "The authenticity of host 'devbox (10.0.0.4)' can't be established.\n\
                      ED25519 key fingerprint is SHA256:abc.\n\
                      Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(classify_prompt(prompt, None), PromptKind::Confirm);
        assert_eq!(
            classify_prompt(
                "Are you sure you want to continue connecting (yes/no)?",
                None
            ),
            PromptKind::Confirm
        );
        // A secret prompt that merely mentions yes/no isn't a question.
        assert_eq!(
            classify_prompt("Type (yes/no) then your password:", None),
            PromptKind::Secret
        );
    }

    #[test]
    fn sshs_hint_decides_when_given() {
        assert_eq!(
            classify_prompt("Allow use of key id_ed25519?", Some("confirm")),
            PromptKind::Confirm
        );
        assert_eq!(
            classify_prompt(
                "Confirm user presence for key ED25519-SK SHA256:abc",
                Some("none")
            ),
            PromptKind::Notice
        );
        assert_eq!(
            classify_prompt("jlong@devbox's password: ", Some("")),
            PromptKind::Secret
        );
    }

    #[test]
    fn askpass_env_forces_the_helper_and_carries_no_secret() {
        let askpass = Askpass::new("/opt/Shelbi.app/Contents/MacOS/shelbi-desktop");
        let env = askpass.env();
        assert_eq!(
            env,
            vec![
                (
                    "SSH_ASKPASS",
                    OsString::from("/opt/Shelbi.app/Contents/MacOS/shelbi-desktop")
                ),
                ("SSH_ASKPASS_REQUIRE", OsString::from("force")),
                (HELPER_ENV, OsString::from("1")),
            ]
        );
        let mut cmd = Command::new("ssh");
        askpass.apply(&mut cmd);
        assert_eq!(cmd.get_envs().count(), 3);
        assert_eq!(cmd.get_args().count(), 0, "nothing on the command line");

        let for_devbox = askpass.with_host("jlong@devbox");
        assert_eq!(for_devbox.host(), Some("jlong@devbox"));
        assert_eq!(
            for_devbox.env().last(),
            Some(&(HOST_ENV, OsString::from("jlong@devbox"))),
            "the host the dialog names travels alongside"
        );
    }

    #[test]
    fn the_dialog_names_the_host_and_quotes_its_prompt() {
        assert_eq!(
            dialog_text(Some("devbox"), "(jlong@devbox) Verification code: "),
            "SSH sign-in to devbox\n\ndevbox asks:\n(jlong@devbox) Verification code:"
        );
        // A server can't push its text above Shelbi's line.
        let phish = dialog_text(Some("jlong@devbox"), "Enter your Mac password to continue:");
        assert!(phish.starts_with("SSH sign-in to jlong@devbox\n\njlong@devbox asks:\n"), "{phish}");
        assert!(phish.ends_with("Enter your Mac password to continue:"));
        // The host stays one line.
        assert_eq!(
            dialog_text(Some("dev\nbox"), "Password:"),
            "SSH sign-in to devbox\n\ndevbox asks:\nPassword:"
        );
        // No host: just the prompt.
        assert_eq!(dialog_text(None, "Password: "), "Password:");
        assert_eq!(dialog_text(Some("  "), "Password:"), "Password:");
    }

    #[test]
    fn only_sshs_askpass_shape_runs_the_helper() {
        let one = |s: &str| vec![OsString::from(s)];
        let yes = Some(std::ffi::OsStr::new("1"));
        let force = Some(std::ffi::OsStr::new("force"));
        assert_eq!(
            helper_prompt(yes, force, &one("jlong@devbox's password: ")),
            Some("jlong@devbox's password: ".to_string())
        );
        assert_eq!(
            helper_prompt(yes, Some("prefer".as_ref()), &one("")),
            Some(String::new()),
            "ssh may ask with an empty prompt"
        );
        // An inherited variable with no arguments (a ProxyCommand child
        // running plain `shelbi`) is Shelbi, not an empty dialog.
        assert_eq!(helper_prompt(yes, force, &[]), None);
        // Nor with more than one.
        assert_eq!(helper_prompt(yes, force, &[OsString::from("remote"), OsString::from("list")]), None);
        // ssh's own variable must be there too.
        assert_eq!(helper_prompt(yes, None, &one("Password:")), None);
        assert_eq!(helper_prompt(yes, Some("".as_ref()), &one("Password:")), None);
        // And ours must say so.
        assert_eq!(helper_prompt(None, force, &one("Password:")), None);
        assert_eq!(helper_prompt(Some("0".as_ref()), force, &one("Password:")), None);
    }

    #[test]
    fn the_dialog_script_passes_the_prompt_as_data() {
        // The prompt is an argument, never spliced into the script, and the
        // kind word leads so a prompt can't be read as an osascript option.
        assert!(!DIALOG_SCRIPT.contains("{prompt}"));
        assert!(DIALOG_SCRIPT.contains("with hidden answer"));
        assert!(DIALOG_SCRIPT.contains(r#"buttons {"No", "Yes"}"#));
        assert!(DIALOG_SCRIPT.contains("display dialog promptText"));
        for kind in [PromptKind::Secret, PromptKind::Confirm, PromptKind::Notice] {
            assert!(!kind.script_word().starts_with('-'));
        }
        assert!(DIALOG_SCRIPT.contains(r#"dialogKind is "confirm""#));
        assert!(DIALOG_SCRIPT.contains(r#"dialogKind is "notice""#));
    }

    #[test]
    fn the_host_key_dialog_defaults_to_no() {
        let confirm = DIALOG_SCRIPT
            .lines()
            .skip_while(|l| !l.contains(r#"dialogKind is "confirm""#))
            .nth(1)
            .expect("the confirm branch");
        assert!(confirm.contains(r#"default button "No""#), "{confirm}");
        assert!(!confirm.contains(r#"default button "Yes""#), "Return must refuse: {confirm}");
        assert!(confirm.contains(r#"cancel button "No""#));
        assert!(confirm.contains("promptText"), "ssh's whole prompt, fingerprint and all");
    }

    #[test]
    fn the_auth_message_says_what_to_do_when_shelbi_couldnt_ask() {
        assert_eq!(
            auth_failed_message("devbox", "Permission denied (publickey,password).", true),
            "Couldn't sign in to devbox: Permission denied (publickey,password)."
        );
        let batch = auth_failed_message("devbox", "Permission denied (publickey,password).", false);
        assert!(batch
            .starts_with("Couldn't sign in to devbox: Permission denied (publickey,password). "));
        assert!(batch.contains("password"), "{batch}");
        assert!(batch.contains("ssh-add"), "{batch}");
    }

    #[test]
    fn never_turns_the_dialog_off() {
        // Only the off switch is checked here: whether this machine has a GUI
        // session isn't the test's to know.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var(MODE_ENV, "never");
        assert!(!dialog_possible());
        assert!(Askpass::available().is_none());
        std::env::remove_var(MODE_ENV);
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_watch_sees_a_dialog_open_and_close() {
        let mut watch = PromptWatch::new().unwrap();
        assert!(!watch.prompting());
        let helper = UnixStream::connect(watch.path()).unwrap();
        assert!(
            watch.prompting(),
            "a connected helper is a dialog on screen"
        );
        drop(helper);
        // The close reaches the listener's side promptly, but not instantly.
        let until = Instant::now() + Duration::from_secs(2);
        while watch.prompting() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!watch.prompting());
        let path = watch.path().to_path_buf();
        drop(watch);
        assert!(!path.exists(), "the socket is removed");
    }

    #[test]
    fn the_deadline_pauses_while_a_dialog_is_open() {
        // A stand-in "ssh" that plays a helper: it holds the watch socket open
        // (a dialog on screen) for longer than the whole deadline, then exits.
        let mut cmd = Command::new("/usr/bin/env");
        cmd.args([
            "python3",
            "-c",
            "import os,socket,time\n\
             s=socket.socket(socket.AF_UNIX); s.connect(os.environ['SHELBI_ASKPASS_NOTIFY'])\n\
             time.sleep(3); s.close(); print('in')",
        ]);
        if Command::new("python3").arg("-V").output().is_err() {
            return;
        }
        let out = run_until(
            &mut cmd,
            Some(&Askpass::new("/bin/false")),
            Duration::from_millis(1500),
        )
        .expect("the time spent asking isn't counted");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "in");

        // Without a dialog the same wait runs out.
        let mut slow = Command::new("/bin/sleep");
        slow.arg("5");
        let err = run_until(&mut slow, None, Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::TimedOut);
    }
}
