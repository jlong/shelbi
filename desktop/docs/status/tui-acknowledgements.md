# tui-acknowledgements: the bundled software and its licenses in the TUI

## The change

The desktop's Acknowledgements window (Figma "Bundled Software · Dark",
2453:8397) lists the third-party software it ships with each license. The TUI
had nothing: About's Acknowledgements link was hidden. The TUI now has the same
browser as a full-screen view, opened from About's link and the palette, and
Done or Esc returns to where it was opened from.

## Shared (`shelbi-app`)

- `shelbi_app::licenses`: the types (`Crate`, `Entry`, a binary's generated
  `Table`), the crate entries both binaries ship (`shared_entries`: Alacritty
  Terminal, nucleo-matcher, option-ext, portable-pty, termwiz), the long
  tail's entry, the list order, the search / selection `Browser` (now over a
  given entry slice), and a license text's paragraph layout
  (`license_blocks`). The desktop's `license_markdown` is built on the blocks.
- The generator moved from `desktop/build_support/` to
  `crates/shelbi-app/build_support/licenses.rs`, parametrized by a `Config`
  (walk root, long-tail skips, featured files, wording). Both build scripts
  `#[path]`-include it.
- Fixed while here: a wrapped sentence starting with "copyright notice ..."
  was set as a copyright line of its own (Apache's "as indicated by a /
  copyright notice"); a copyright line now needs `(c)`, `©` or a year after
  the word. Both clients get the fix.
- `CommandKind::OpenAcknowledgements` / `Effect::OpenAcknowledgements`, id
  `action:acknowledgements`, title "Acknowledgements", hidden until typed,
  no project needed. Scenario
  `acknowledgements_reach_the_host_with_or_without_a_project`.
- The focused search uses #1935's `input_focus` fill and the hover pill
  #1939's `row_hover` (the settings tables' hover), so the list hovers as
  the tables do; no new theme tokens.

## Which crates the TUI lists

The TUI ships in the `shelbi` binary, a different binary from the desktop
app, so it has its own list: `crates/shelbi-tui/build.rs` walks `cargo
metadata` from the `shelbi` package (`crates/shelbi-cli`, normal
dependencies only, proc-macros excluded, `--filter-platform` for the target),
so the CLI's crates (inquire, clap, ...) are in it and gpui isn't. Featured:
the shared five (nucleo-matcher and option-ext are MPL 2.0, the only licenses
in the tree with obligations beyond MIT / Apache / BSD / Zlib-style notices),
plus Ratatui and Crossterm (what the TUI draws with). Fonts and icon sets are
left out: a terminal draws in the user's font. Everything else is the "Rust
crates" entry with every notice.

## Desktop

- `licenses.rs` builds its list over the shared module; behaviour unchanged.
- `action:acknowledgements` opens the window (`HostCall::OpenAcknowledgements`,
  and windowless from the menu like About). It is placed in the Help menu as
  "Acknowledgements" (the menu coverage test requires every registry command
  to have an item).

## TUI

- `shell/acknowledgements.rs`: title row (Acknowledgements, Done as the
  primary button), the half-block search box, the list (name over license;
  selection and hover pills via the new `sidebar::bleed_separator`), and the
  detail (name, attribution, View source ↗, description, rule, license name
  and text). View source stacks under the description when the detail is
  narrower than 48 columns.
- Keys: ↑/↓ (j/k), Home/End, PgUp/PgDn and Shift+↑/↓ for the license, `/` or
  Ctrl+F / ⌘F for search, Tab through search, list, View source and Done,
  Esc backs out (clear search, leave search, leave). Mouse: hover, click,
  wheel over either pane, × clears the search.
- It stands over everything (Settings included) and takes every key, click
  and paste. Opened from About, Done brings About back focused on the link.

## QA

- `cargo test -p shelbi-tui` (render tests at 100x30, 160x45, 60x20 and tiny
  sizes, light theme, shell round trips), `cargo test -p shelbi-app`, desktop
  `cargo test`.
- Sandboxed eye check of the real binary at all three sizes, dark and light.
