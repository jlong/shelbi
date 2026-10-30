# dt-app-icon-setting: the App icon setting and the layered Command Grille

## The change

Shelbi gets a new icon, **Command Grille** (concept 02 of the Figma frame
"Shelbi · Layered macOS icons · Round 2", `2379:20`): a layered macOS 26
icon with a cobalt base, two light racing stripes, a floating glass oval
grille and a white `>_` prompt. It is the app bundle's icon and the default.
The previous raster icon stays as **Retro**.

Settings ▸ General gets an App icon row under Background blur:

```
App icon
┌────────┐  ┌────────┐
│  [CG]  │  │ [Retro]│      the picked tile ringed in check_fill + glow
└────────┘  └────────┘      (names in tooltips, since dt-icon-picker-no-shift)
Shown in the Dock while Shelbi runs and in About. Finder, and the Dock
before launch, show the app's own icon, Command Grille.
```

- A click shows the icon in the Dock at once (`NSApplication.
  applicationIconImage`), repaints About and every window's tiles, and is
  saved as `desktop.app_icon: command-grille | retro`. Unset means Command
  Grille and stays unwritten until the user picks; a pick is written
  whichever it is, so the choice outlives a change of default. Any other
  value reads as unset.
- At launch `app_icon::init` reads it and sets the Dock icon before the
  first window.
- The desktop's About draws the chosen icon at 112pt; the TUI's About draws
  its half-block table for the chosen icon (read from `config.yaml` when it
  opens).

## How it works

- `shelbi_state::AppIcon` (`user_config.rs`), with `ALL`, `label()` and
  `from_config`; lenient deserialization like `terminal_scrollbar`.
- `desktop/src/app_icon.rs`: the `AppIconSetting` global, `init` / `set` /
  `current`, and the 224px (About) and 128px (tiles) renditions.
- `desktop/src/platform/dock_icon.rs`: sets the Dock icon. Command Grille in
  a bundle that names its icon (`CFBundleIconName`) sets `nil`, so the Dock
  draws the bundle's own layered icon with its live glass instead of a flat
  picture; Retro, and Command Grille in an unbundled `cargo run`, set the
  512px PNG. Under test it records instead.
- `elements::controls::icon_choice` / `icon_choices`: the tile. The
  Components page draws no image picker, so it borrows the drawn controls'
  tokens (the segmented track's `search` well and hover fills, the
  checkbox's `check_fill` ring and `check_glow`), which is what keeps it
  right in both themes and at every contrast and translucency.
- The bundle: `assets/icon/Shelbi.icon` (Icon Composer document, four SVG
  layers exported from the Figma Source component `2380:20`) compiled by
  `scripts/app-icon.sh` (Xcode 26 actool) into `assets/icon/compiled/`
  (`Assets.car`: layered for macOS 26, flattened 16 to 1024px renditions for
  macOS 15; plus `Shelbi.icns`), committed. `bundle.sh` copies them (no
  Xcode 26 on the release runner) and refuses a source edited since its
  compile (`compiled/Shelbi.icon.sha256`). The plist gains
  `CFBundleIconName`. So the bundle is an Icon Composer icon, not a
  flattened `.icns`, and macOS 26 no longer puts it in the gray plate at 16
  and 32pt that the old raster icon got.
- The app's own pictures of Command Grille (512/224/128px) are the compiled
  icon as macOS draws it (`scripts/render-app-icon.swift` through a
  throwaway registered app), so they match the Dock and Finder.
- `scripts/tui-about-icon.py` now writes a table per icon
  (`COMMAND_GRILLE`, `RETRO`); Retro's is byte-identical to the old one.

## Decisions

- Background as a layer, not Icon Composer's fill: the Figma gradient has
  three stops and the fill takes two.
- The gradient is rewritten in bounding-box units: actool drew Figma's
  `userSpaceOnUse` export as no fill (a white tile).
- Glass on the grille and prompt groups only (the Figma preview puts glass on
  the inset and a shadow under the prompt); stripes and base flat.
- Compiled output committed rather than compiled in `bundle.sh`: the release
  job's `macos-15` runner defaults to Xcode 16, whose actool can't read
  `.icon`.
- The Dock picture is 512px (the Dock's largest magnified size at 2x), not
  1024, to halve what the binary embeds.
- The Retro choice affects only the running app. Changing the bundle's
  on-disk icon (`NSWorkspace.setIcon`) would write into the signed bundle.

## Verified

- Desktop `cargo clippy --all-targets -D warnings` clean; `cargo test`
  794 passed (new: the tiles pick live and save, restore from config, follow
  another window; `set` drives the Dock; rendition sizes).
- Root: `shelbi-state` (round-trip, lenient, default) and `shelbi-tui`
  (both tables, About draws the chosen one) pass; `cargo check --workspace
  --locked`; no `Cargo.lock` change in either workspace.
- `--visual-views` now renders `settings-app-icon-{command-grille,retro}.png`
  and `about-{command-grille,retro}.png`; checked in dark, light and 100%
  contrast. (The offscreen capture composites a translucent window over
  black, so translucent frames show only that the tiles stay legible.)
- `bundle.sh --no-build` (around a debug binary, ad-hoc): `Assets.car` +
  `Shelbi.icns` in Resources, both plist keys; editing `icon.json` without
  recompiling fails the bundle. Finder's rendering at 512, 32 and 16 is the
  layered icon, with no plate.
- Not captured: the live Dock. It auto-hides on the dev Mac, and the screen
  was being shared, so revealing it was stopped. Check by hand (QA list).
