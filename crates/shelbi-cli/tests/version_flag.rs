use std::process::Command;

/// Run the `shelbi` binary with the given args and capture stdout + exit code.
fn run(args: &[&str]) -> (String, bool) {
    let output = Command::new(env!("CARGO_BIN_EXE_shelbi"))
        .args(args)
        .output()
        .expect("shelbi binary must be built for this test");
    (
        String::from_utf8(output.stdout).expect("version output is utf8"),
        output.status.success(),
    )
}

#[test]
fn short_v_prints_version() {
    let (stdout, ok) = run(&["-v"]);
    assert!(ok, "`shelbi -v` should exit 0");
    assert!(
        stdout.starts_with("shelbi "),
        "`shelbi -v` should print the version, got: {stdout:?}"
    );
}

#[test]
fn short_v_matches_long_version() {
    let (short, short_ok) = run(&["-v"]);
    let (long, long_ok) = run(&["--version"]);
    assert!(short_ok && long_ok, "both version forms should exit 0");
    assert_eq!(
        short, long,
        "`shelbi -v` must print the same output as `shelbi --version`"
    );
}
