# The app icon

`shelbi-1024.png` is the Shelbi desktop app's icon and its only source: a
1024 x 1024 RGBA PNG, the "Command Grille" design (a cobalt rounded-square
tile with twin ivory racing stripes and a chrome-rimmed oval grille holding an
ivory `>_` prompt). The tile's macOS mask, padding and shadow are already in
the artwork, over transparent corners, so nothing adds a mask or background on
top of it.

`scripts/bundle.sh` cuts it into an iconset with `sips` and compiles
`Contents/Resources/Shelbi.icns` with `iconutil`, both part of macOS. No step
renders or regenerates the PNG, so bundling always ships exactly the committed
file.

`shelbi-224.png` is the same icon at 224 x 224, for About Shelbi, which draws
it at 112pt: twice the size, so it stays sharp on a Retina display (gpui
samples a texture without mipmaps, so drawing the 1024px master that small
would alias). It is cut from the master with
`sips -z 224 224 shelbi-1024.png --out shelbi-224.png`; recut it whenever the
master changes.

## Where it comes from

The artwork is raster, not vector: it was generated, then finished in Figma
with a native macOS mask, framing and shadow. The selected component is
[App Icon / Command Grille](https://www.figma.com/design/pu3hxHgWoCDPCzJ4u6zTQK/Shelbi?node-id=2366-9174),
next to the other explorations and their 64/32/16px previews on the
[comparison board](https://www.figma.com/design/pu3hxHgWoCDPCzJ4u6zTQK/Shelbi?node-id=2362-40717).
The committed file is that component's 1024px transparent export, SHA-256
`591edeb18e39426e3c194f73e8684d961ea0298de9bd711bce8dc112d5075baf`.

It replaced the earlier pixel "S" icon (an SVG rendered by
`scripts/render-icon.sh`); both were removed, and live on in git history.

## Changing it

1. Export the Figma component in isolation at 1024px, PNG, with transparency
   (no background fill, no extra mask).
2. Replace `shelbi-1024.png` with it and commit it. Update the SHA-256 above,
   and recut `shelbi-224.png` (above).
3. Run `scripts/bundle.sh --no-build` (or a full bundle) and check the icon in
   Finder, the Dock and the app switcher, on light and dark backgrounds.

`bundle.sh` refuses a master that isn't 1024 x 1024 with alpha.
