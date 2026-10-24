# dt-about-bundled-software: About Shelbi and Bundled Software

## The change

Shelbi ▸ About Shelbi used to show a system alert ("Desktop 0.11.0, CLI
…"). It now opens John's About window (Figma "About Shelbi · Dark",
2431:8310), and that window's **Bundled Software** link opens a second
window listing what the app ships from others, each with its full license
(Figma "Bundled Software · Dark", 2453:8397).

```
 ● ● ●                                   ● ● ●  Bundled Software                     [Done]
            [icon 112pt]                 ┌──────────────────────┬──────────────────────────────┐
               Shelbi                    │ [🔍 Search bundled…] │ Source Code Pro  [View source↗]│
           Version 0.11.0                │ Alacritty Terminal   │ Adobe · Terminal typeface    │
 An Open Source personal agent…          │   Apache 2.0         │ The default monospace font…  │
   Website  Documentation  GitHub        │ …                    │ ──────────────────────────── │
          Bundled Software               │▌Source Code Pro     ▐│ SIL Open Font License 1.1    │
        © 2026 John W. Long              │ …                    │ © 2023 Adobe … (scrolls)     │
  MIT License · Sponsored by 32pixels    │ Software, fonts, and…│                              │
```

### About Shelbi (`src/views/about.rs`)

- A 400 × 520 window, **not resizable** (so macOS greys the zoom button)
  and **not minimizable**, with a transparent titlebar under the traffic
  lights, placed where the main window puts them. ⌘W closes it.
- The app icon at 112pt from `assets/icon/shelbi-224.png`, a 2x rendition
  cut from the committed 1024px master with `sips` (gpui samples textures
  without mipmaps, so drawing the master that small would alias). The
  master's artwork already sits 64px in from each edge, so at 112pt it
  lands exactly where the frame's 98pt masked artwork does (7pt in).
- The version is `CARGO_PKG_VERSION` (the frame's 0.10.0 is a
  placeholder), in `Desktop/Small/Regular`.
- Links in `text/link` (the credits' two in `text/muted`), the
  hovered-link color (`text/bold`) under the pointer, all opening in the
  browser: Website `https://shelbi.dev`, Documentation
  `https://shelbi.dev/docs` (the Help menu's `DOCS_URL`), GitHub
  `https://github.com/jlong/shelbi`, MIT License
  `https://github.com/jlong/shelbi/blob/main/LICENSE`, Sponsored by
  32pixels `https://32pixels.co`.
- **© year: the build's year**, not a literal 2026: `build.rs` sets
  `SHELBI_BUILD_YEAR` from `SOURCE_DATE_EPOCH` when set (a reproducible
  build), else the clock. It reads 2026 today, as the frame does, and a
  release built in a later year says that year. (Caveat: a local build
  only re-runs the build script when its inputs change, so a dev build can
  keep last year's until then; release builds are fresh.)

### Bundled Software (`src/views/bundled_software.rs`)

- 1000 × 760, resizable down to 720 × 480, minimizable. The titlebar row
  (52pt, the toolbar's `surface/depth-10` band) holds the traffic lights,
  the title (`Desktop/Base/Semibold`) and **Done** (Primary, Small), which
  closes the window as ⌘W does.
- Left, 280pt on the same band: the Searchbox at Base with its clear
  button (`style::search_box_base` + `style::search_clear`), filtering by
  name or license (and by the long tail's crate names and licenses, so
  `serde` finds "Rust crates"); the rows (48pt: name in
  `Desktop/Small/Medium`, license under it in `Desktop/X-Small/Regular`
  muted), filling the column to its bottom padding (the frame's footer
  caption was dropped in `dt-bundled-drop-footer`). ↑ / ↓ move the
  selection through the shown rows (scrolling it into view), a click
  selects, ⌘F goes to the search, Esc in the search clears it or hands the
  keys back. It opens on the first entry.
- Right, on the canvas: the name (the frames' heading, 18 / 28 semibold),
  the attribution line, **View source** (Default, Small, the
  `arrow-top-right-on-square` right icon), the one-line description, a
  rule, then the license: its name (`Desktop/Base/Semibold`, loud) and the
  complete text in `Desktop/Small/Regular`, paragraphs a blank line apart,
  all-caps section titles (`PREAMBLE`) in `Desktop/Small/Medium`. It is a
  gpui-kit text view, so it is selectable (drag, ⌘A) and copyable (⌘C),
  and it scrolls virtualized, which the long tail's ~460 KB needs.
- The search with nothing matching shows "Nothing matches the search." in
  the detail pane (no frame draws it).

### Both windows

- Single instance each: `about::show_about` / `show_bundled_software`
  keep the window handles in a global, and asking again activates the open
  one (a closed window's handle fails to update, so a new one opens).
- About moved out of `ShelbiWindow`: `AppCommand::About` is now one of the
  window-less app commands (`menus::run_app_command`), so it works whichever
  window is key, including About itself.
- They follow the theme (light from the same layout in the Light tokens)
  and the window translucency setting like the main windows.
- Closing the last *project* window with About open no longer quits right
  away (the app quits when its last window of any kind closes); the
  project window's slot is still reopened at the next launch, as when it
  was the last window.

## The list, and how it was verified

`src/licenses.rs` holds the entries. The frame's 12 were the designer's
sample; each was checked against what the app actually ships:

| Entry | Ships as | Verified by |
|---|---|---|
| Alacritty Terminal | `vendor/alacritty_terminal` 0.26.0 (Apache-2.0) | path dependency; its `LICENSE-APACHE` |
| Geist, Geist Mono | `assets/fonts/geist*/` (OFL 1.1) | `assets.rs` embeds them; their `LICENSE.txt` |
| GPUI | `gpui-pre` 0.3.9 (Apache-2.0) | `cargo metadata`; its `LICENSE-APACHE` |
| gpui-kit | `gpui-kit` 0.7.3 (Apache-2.0) | its package ships **no** license file, so the entry shows `gpui-base`'s (same repository and release) |
| Heroicons | `assets/icons/heroicons*` (MIT) | `assets.rs`; `assets/icons/heroicons/LICENSE` |
| **Lucide** (added) | inside `gpui-kit-assets` (ISC) | `assets.rs` falls back to `gpui_kit::assets::Assets`, which embeds gpui-kit's 101 component icons, Lucide's; its `LICENSE-LUCIDE` |
| nucleo-matcher | 0.3.1 (MPL-2.0) | `cargo tree -i`: via `shelbi-palette` |
| option-ext | 0.2.0 (MPL-2.0) | `cargo tree -i -e normal,no-proc-macro`: via `dirs` 5 (`shelbi-state`, `zed-font-kit`), not only the proc-macro path |
| portable-pty | 0.9.0 (MIT) | via `shelbi-session` (the client links it) |
| Source Code Pro | `assets/fonts/source-code-pro/` (OFL 1.1) | `assets.rs`; its `LICENSE.md` |
| Symbols Nerd Font Mono | `assets/fonts/nerd-symbols/` (MIT build, glyph sets under their own) | its `LICENSE` and README |
| termwiz | 0.23.3 (MIT) | via `shelbi-term` |
| **Rust crates** (added, last) | the other 465 shipped crates | generated, below |

Nothing in the sample was dropped: all 12 ship. The other icon folders
(`heroicons-14/16/18`, `checkbox/`) are derived Heroicons drawings, and
`figma/`, `status/`, `glyphs/` and the app icon are John's own, so they
need no entry. Shelbi's own crates (`shelbi-*`) aren't listed.

**Texts are embedded at build time**: the fonts' and icons' with
`include_str!` from `assets/`, the crates' by the build script out of their
packages (the cargo registry, or `vendor/`). Nothing is fetched at runtime.
`the_texts_are_the_shipped_licenses` checks each names its license and
copyright holder as the file does.

### The long tail (`build_support/licenses.rs`)

A small build script (no `cargo-about`: it would be a new tool in the
build, and the walk is ~40 lines), run from `build.rs`:

1. `cargo metadata --format-version 1 --locked --offline --filter-platform
   $TARGET` (the nested cargo needs no network: every package it reads was
   downloaded for the build running it; it adds ~2s when the build script
   re-runs, which is only when `Cargo.lock`, `Cargo.toml` or the script
   change).
2. Walk from `shelbi-desktop` along **normal** dependencies only, stopping
   at proc-macros: build and dev dependencies and proc-macros run at build
   time and don't ship. 483 crates ship (default features; the
   `visual-check` and `bench` tools aren't shipped).
3. Each crate's license files (`LICENSE*`, `LICENCE*`, `COPYING*`,
   `UNLICENSE*`, `NOTICE*`, `COPYRIGHT*`) are classified by what they say.
   For a choice (`MIT OR Apache-2.0`) the first of MIT, Apache-2.0, BSD,
   ISC, Zlib, Unicode, BSL, Unlicense, CC0, MPL, WTFPL whose text the
   package ships is the license Shelbi takes it under, and that text is the
   notice; an `AND` takes one from each side. NOTICE and COPYRIGHT files are
   always included (Apache asks for NOTICE), and so is a license file for
   something the crate's own license doesn't cover (bundled third-party
   work). A package that ships no text for its license takes it from a
   shipped crate of the same repository; failing that, MIT gets the
   standard MIT text with the manifest's authors (or "the authors of
   <repository>"), anything else a pointer to the repository (none today).
4. Output, `$OUT_DIR/licenses.rs`: `CRATES` (465, after the featured ones
   and Shelbi's own), `CRATE_NOTICES` (an intro, the crates as `name
   version · SPDX`, then each distinct notice once under "Applies to …",
   about 240 notices, ~460 KB), `SHIPPED` (all 483, for the tests) and the
   featured entries' files.

Tests (`licenses::tests`): every entry has a license, title and a
non-trivial text; `SHIPPED` minus Shelbi's own and the featured crates is
exactly `CRATES`; every listed crate appears in the list with its SPDX and
under some notice; no proc-macro ships; the featured crates all ship;
search and selection (`Browser`); the Markdown escaping reads back
literally and the reflow keeps list items, rules, copyright lines and the
crate list on their own lines.

## Verification

- `cargo build`, `cargo clippy --all-targets -- -D warnings`,
  `cargo clippy --all-targets --features visual-check -- -D warnings`:
  clean.
- `cargo test`: 767 passed, 0 failed (the full suite). The new tests are `licenses::tests` (10),
  `views::about::tests` (4: links open their URLs, both windows open once
  and ⌘W closes, the icon rendition is 2x with alpha, the year), and
  `views::bundled_software::tests` (4: opens on the first entry and the
  arrows and a click move it, the search narrows and clears, the detail's
  license is the full text when selected-all (the copy is the shipped
  text, unescaped) and View source opens the repository, Done and ⌘W
  close).
- `--visual-figma` (`SHELBI_VISUAL_ONLY=about` renders just these):
  `about-{dark,light}`, `bundled-software-{dark,light}` (Source Code Pro
  selected, as the frame), and `state-bundled-first` / `-search` (`mit`) /
  `-crates` / `-no-match`. The frames are in `design-refs/desktop-figma/about/`
  as `about.png` and `bundled-software.png`; copied as `about-dark.png` and
  `bundled-software-dark.png` into a ref dir for `figma-diff.sh`:

| Screen | Mismatch | What differs |
|---|---|---|
| about-dark | 2.05% | text antialiasing; "Version 0.11.0" for the frame's 0.10.0; the icon's edge (the frame masks the artwork itself, the app draws the finished master) |
| bundled-software-dark | 9.04% | content, not layout: Lucide's extra row shifts the rows below Heroicons by 48pt; the real OFL file has `-----` rules around its title and `PREAMBLE` above its paragraph, which the frame's sample text dropped; the title at 91pt (the app's real-traffic-light inset, `BESIDE_TRAFFIC_LIGHTS`) where the frame draws it at 80; the frame's static scrollbar thumb (the app's shows while scrolling) |

The header, search box, rows and their selected fill, the detail's heading,
attribution, View source, description, rule, license title and first
paragraphs all land on the frame's pixels.

## Decisions (not asked)

- **The selected row is the `row_selected` fill**, not the sidebar's
  `nav_selected` chip: the frame binds it to the 18% slate overlay
  (`#434859` over the pane), which is `row_selected` in Dark; the
  Navigation chip is the canvas color, darker than the pane. Light uses
  `row_selected`'s Light value.
- **The pane band is `toolbar`** (`surface/depth-10`, the Issues Header's
  tint), measured from the frame (`#2c3140` over the canvas), for both the
  titlebar row and the list column.
- **Headings at 18 / 28 semibold**: both frames' `Desktop/Heading` is
  18 / 28 semibold, while `Type::Heading` (other screens) is 20 / 28 bold;
  the two windows use `about::heading_type` rather than changing the shared
  style under the other screens.
- **The scrollbar is gpui-kit's**, which the text view draws over its
  virtualized list: an overlay scrollbar (shown while scrolling, then
  fading) in the app's palette (`scrollbar_thumb` = muted). The terminal's
  own overlay scrollbar is part of the terminal element and has no plain
  scroll container to sit on; a non-virtualized text view could host one,
  but the long tail's ~460 KB needs the virtualized list.
- **License texts reflow** to the pane, as the frame sets them: a file's
  hard wrap joins up, indentation drops, list items, rules and copyright
  lines keep their own lines, and the crate list keeps every line. Every
  character is escaped, so nothing in a license reads as Markdown.
- **Description and attribution copy** is mine where the frame has only
  Source Code Pro's ("Adobe · Terminal typeface"; "The default monospace
  font used in Shelbi's terminal."). Crate attributions carry the version.
- **View source** goes to the homepage or repository: for the long tail,
  the lockfile on GitHub.
- **No footer**: the frame's "Software, fonts, and icons" caption under
  the rows added nothing and was dropped (`dt-bundled-drop-footer`); the
  list runs to the column's bottom padding and scrolls there.
- **Symbols Nerd Font Mono** shows the font's MIT license and a paragraph
  naming the glyph sets' licenses (from its README); their full texts live
  in Nerd Fonts' `license-audit.md`, not vendored here.
