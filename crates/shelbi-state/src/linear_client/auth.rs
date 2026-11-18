//! Linear API key resolution and storage.
//!
//! **Resolution order — first hit wins:**
//! 1. `LINEAR_API_KEY` in the process environment (CI / headless).
//! 2. The macOS login keychain: the generic password with service
//!    [`KEYCHAIN_SERVICE`] and account [`KEYCHAIN_ACCOUNT`], which
//!    `shelbi tracker linear login` writes. Read with
//!    `security find-generic-password -s shelbi-linear -a default -w`.
//! 3. The project's out-of-repo `~/.shelbi/projects/<name>/tokens.yml`
//!    (`linear: { token: ... }`, `chmod 600`), shared with the GitHub backend.
//! 4. Otherwise [`Error::MissingIssueTrackerAuth`] telling the user to run
//!    `shelbi tracker linear login`.
//!
//! The key is a [`SecretToken`]: `Debug` / `Display` are redacted.

use std::path::Path;
use std::process::{Command, Stdio};

use shelbi_core::{Error, Result};

use crate::issue_auth::{read_token_file, SecretToken, TokenBlock, TokenSource};

/// The environment variable checked first.
pub const LINEAR_API_KEY_ENV: &str = "LINEAR_API_KEY";
/// The keychain service name the key is stored under.
pub const KEYCHAIN_SERVICE: &str = "shelbi-linear";
/// The keychain account name the key is stored under.
pub const KEYCHAIN_ACCOUNT: &str = "default";

/// Resolve the Linear API key: env → keychain → `project`'s `tokens.yml`
/// (skipped when `project` is `None`, e.g. before a project is configured).
pub fn resolve_linear_key(project: Option<&str>) -> Result<SecretToken> {
    let token_file = match project {
        Some(p) => Some(crate::project_dir(p)?.join("tokens.yml")),
        None => None,
    };
    // Under `FakeLinear::install` the developer's own env key must not leak
    // into (or change the outcome of) a test: only the fake keychain counts.
    #[cfg(any(test, feature = "test-support"))]
    if super::fake::fake_keychain_read().is_some() {
        return resolve_linear_key_with(|_| None, keychain_read, token_file.as_deref());
    }
    resolve_linear_key_with(
        |k| std::env::var(k).ok(),
        keychain_read,
        token_file.as_deref(),
    )
}

/// Testable core of [`resolve_linear_key`] with its three sources injected.
pub(crate) fn resolve_linear_key_with(
    env: impl Fn(&str) -> Option<String>,
    keychain: impl FnOnce() -> Option<String>,
    token_file: Option<&Path>,
) -> Result<SecretToken> {
    if let Some(key) = env(LINEAR_API_KEY_ENV).and_then(non_empty) {
        return Ok(SecretToken::new(key, TokenSource::Env(LINEAR_API_KEY_ENV)));
    }
    if let Some(key) = keychain().and_then(non_empty) {
        return Ok(SecretToken::new(key, TokenSource::Keychain));
    }
    if let Some(path) = token_file {
        if let Some(key) = read_token_file(path, TokenBlock::Linear)? {
            return Ok(SecretToken::new(key, TokenSource::File));
        }
    }
    Err(Error::MissingIssueTrackerAuth {
        backend: "linear",
        checked: format!("${LINEAR_API_KEY_ENV}, the macOS keychain (service `{KEYCHAIN_SERVICE}`)"),
        token_file: token_file
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "the project's tokens.yml".to_string()),
        fix: format!("run `shelbi tracker linear login` or set {LINEAR_API_KEY_ENV}"),
    })
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Check a pasted key's shape and return it trimmed. Linear personal API keys
/// are `lin_api_` followed by letters and digits; anything outside
/// `[A-Za-z0-9_-]` is refused, which also keeps the key safe to hand to
/// `security -i` unquoted and to send as an HTTP header.
pub fn validate_key_shape(raw: &str) -> Result<String> {
    let key = raw.trim();
    if key.is_empty() {
        return Err(Error::Other("no Linear API key given".into()));
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(Error::Other(
            "that doesn't look like a Linear API key (expected letters, digits, `_` and `-`, \
             like `lin_api_...`)"
                .into(),
        ));
    }
    Ok(key.to_string())
}

/// What a successful [`login_with_key`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearLogin {
    /// Who the key belongs to.
    pub viewer: super::LinearUser,
    /// Where the key was stored, for the user (`the macOS keychain (...)`).
    pub stored_in: String,
}

/// The `shelbi tracker linear login` flow minus the prompt: check the key's
/// shape, prove it works by asking Linear who it belongs to (`viewer`), and
/// only then store it with [`store_linear_key`]. A rejected key is never
/// stored.
pub fn login_with_key(raw: &str) -> super::LinearResult<LinearLogin> {
    let key = validate_key_shape(raw)?;
    let client = super::LinearClient::new(SecretToken::new(key.clone(), TokenSource::Keychain))
        .caller("login");
    let viewer = client.viewer()?;
    let stored_in = store_linear_key(&key)?;
    Ok(LinearLogin { viewer, stored_in })
}

/// Read the stored key from the macOS keychain; `None` when absent, when
/// `security` fails, or off macOS.
fn keychain_read() -> Option<String> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(fake) = super::fake::fake_keychain_read() {
        return fake;
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    non_empty(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Store `key` in the macOS keychain (replacing any earlier one), returning a
/// description of where it went. The key reaches `security` on **stdin**
/// (`security -i` reads commands from it), never argv. Off macOS this is an
/// error naming the alternatives (`LINEAR_API_KEY`, `tokens.yml`).
pub fn store_linear_key(key: &str) -> Result<String> {
    let key = validate_key_shape(key)?;
    #[cfg(any(test, feature = "test-support"))]
    if super::fake::fake_keychain_write(&key) {
        return Ok(keychain_label());
    }
    if !cfg!(target_os = "macos") {
        return Err(Error::Other(format!(
            "no keychain on this platform: set {LINEAR_API_KEY_ENV}, or add `linear: {{ token: ... }}` \
             to the project's tokens.yml (chmod 600)"
        )));
    }
    keychain_write_with("security", &key)?;
    Ok(keychain_label())
}

fn keychain_label() -> String {
    format!("the macOS keychain (service `{KEYCHAIN_SERVICE}`, account `{KEYCHAIN_ACCOUNT}`)")
}

/// Run `<security> -i` with an `add-generic-password -U` command on stdin.
/// `-U` updates an existing item instead of failing on a duplicate.
fn keychain_write_with(security: impl AsRef<std::ffi::OsStr>, key: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new(security)
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(format!("could not run `security`: {e}")))?;
    let command = format!(
        "add-generic-password -U -s {KEYCHAIN_SERVICE} -a {KEYCHAIN_ACCOUNT} -w {key}\n"
    );
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(command.as_bytes())
            .map_err(|e| Error::Other(format!("could not talk to `security`: {e}")))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::Other(format!("`security` did not finish: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    // `security -i` reports a failed command on stderr (and only sometimes in
    // its exit status); a successful add prints nothing.
    if !out.status.success() || !stderr.trim().is_empty() {
        return Err(Error::Other(format!(
            "could not store the key in the keychain: {}",
            stderr.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn private_file(name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("shelbi-linear-auth-{}-{name}", std::process::id()));
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    #[test]
    fn env_wins_over_keychain_and_file() {
        let file = private_file("env", "linear:\n  token: file-key\n");
        let env = |k: &str| (k == LINEAR_API_KEY_ENV).then(|| " env-key ".to_string());
        let key = resolve_linear_key_with(env, || Some("kc-key".into()), Some(&file)).unwrap();
        assert_eq!((key.expose(), key.source()), ("env-key", TokenSource::Env(LINEAR_API_KEY_ENV)));
    }

    #[test]
    fn keychain_then_file() {
        let file = private_file("kc", "linear:\n  token: file-key\n");
        let key = resolve_linear_key_with(no_env, || Some("kc-key".into()), Some(&file)).unwrap();
        assert_eq!((key.expose(), key.source()), ("kc-key", TokenSource::Keychain));

        let key = resolve_linear_key_with(no_env, || None, Some(&file)).unwrap();
        assert_eq!((key.expose(), key.source()), ("file-key", TokenSource::File));
        // A blank env/keychain value doesn't shadow the file.
        let blank = |_: &str| Some("  ".to_string());
        let key = resolve_linear_key_with(blank, || Some(String::new()), Some(&file)).unwrap();
        assert_eq!(key.source(), TokenSource::File);
    }

    #[test]
    fn a_github_only_tokens_file_does_not_supply_a_linear_key() {
        let file = private_file("gh", "github:\n  token: gh-key\n");
        let err = resolve_linear_key_with(no_env, || None, Some(&file)).unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, Error::MissingIssueTrackerAuth { backend: "linear", .. }));
        assert!(msg.contains("shelbi tracker linear login"), "{msg}");
        assert!(msg.contains("LINEAR_API_KEY"), "{msg}");
        assert!(msg.contains(&file.display().to_string()), "{msg}");
    }

    #[test]
    fn missing_key_without_a_project_still_names_the_fix() {
        let msg = resolve_linear_key_with(no_env, || None, None).unwrap_err().to_string();
        assert!(msg.contains("shelbi tracker linear login"), "{msg}");
    }

    #[test]
    fn key_shape_is_checked() {
        assert_eq!(validate_key_shape("  lin_api_AbC123\n").unwrap(), "lin_api_AbC123");
        assert!(validate_key_shape("").is_err());
        assert!(validate_key_shape("lin_api_x y").is_err());
        assert!(validate_key_shape("lin\"api").is_err());
    }

    #[test]
    fn keychain_write_feeds_the_key_on_stdin_not_argv() {
        // A stand-in `security` that records its argv and stdin.
        let dir = std::env::temp_dir().join(format!("shelbi-linear-sec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("security");
        crate::test_support::write_executable(
            &script,
            format!(
                "#!/bin/sh\necho \"$@\" > '{0}/argv'\ncat > '{0}/stdin'\n",
                dir.display()
            ),
        );
        keychain_write_with(&script, "lin_api_SECRET").unwrap();
        let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
        let stdin = std::fs::read_to_string(dir.join("stdin")).unwrap();
        assert_eq!(argv.trim(), "-i");
        assert!(stdin.contains("add-generic-password -U -s shelbi-linear -a default"), "{stdin}");
        assert!(stdin.contains("-w lin_api_SECRET"), "{stdin}");

        // `security -i` can exit 0 after a failed command; stderr gives it away.
        let failing = dir.join("security-fails");
        crate::test_support::write_executable(
            &failing,
            "#!/bin/sh
cat >/dev/null
echo 'SecKeychainItemCreateFromContent: User interaction is not allowed.' >&2
",
        );
        let err = keychain_write_with(&failing, "lin_api_SECRET").unwrap_err().to_string();
        assert!(err.contains("could not store the key") && !err.contains("SECRET"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
