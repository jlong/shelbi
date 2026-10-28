//! Generates Acknowledgements' crate list ([`licenses`]) for the `shelbi`
//! binary the TUI ships in: every crate it links, walked from the `shelbi`
//! package (`crates/shelbi-cli`), with their license texts. The list built
//! from it is `src/licenses.rs`.

#[path = "../shelbi-app/build_support/licenses.rs"]
mod licenses;

/// The binary's list: the crates `shelbi_app::licenses::shared_entries`
/// shows (`SHARED_FEATURED` / `SHARED_FILES`), then the TUI's own featured
/// crates, Ratatui and Crossterm (`src/licenses.rs`).
const LICENSES: licenses::Config = licenses::Config {
    root: Some("shelbi"),
    subject: "the shelbi binary",
    long_tail_skip: &[
        "alacritty_terminal",
        "nucleo-matcher",
        "option-ext",
        "portable-pty",
        "termwiz",
        "ratatui",
        "crossterm",
    ],
    featured_files: &[
        ("alacritty_terminal", "LICENSE-APACHE"),
        ("nucleo-matcher", "LICENSE"),
        ("option-ext", "LICENSE.txt"),
        ("portable-pty", "LICENSE.md"),
        ("termwiz", "LICENSE.md"),
        ("ratatui", "LICENSE"),
        ("crossterm", "LICENSE"),
    ],
    rerun_if_changed: &[
        "../../Cargo.lock",
        "../../Cargo.toml",
        "../shelbi-cli/Cargo.toml",
        "../shelbi-app/build_support/licenses.rs",
    ],
};

fn main() {
    licenses::generate(&LICENSES);
}
