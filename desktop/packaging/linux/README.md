# Linux packaging files

What a Linux package of the desktop app installs beside the binary
(`/usr/bin/shelbi-desktop`):

| File | Installs to |
|---|---|
| `dev.shelbi.Desktop.desktop` | `/usr/share/applications/dev.shelbi.Desktop.desktop` |
| `icons/hicolor/<n>x<n>/apps/dev.shelbi.Desktop.png` | `/usr/share/icons/hicolor/<n>x<n>/apps/` |

The name is the app's id (`platform::os::APP_ID`), which every window
reports as its Wayland app id. That is how Hyprland's window rules
(`windowrule = ..., class:^(dev\.shelbi\.Desktop)$`), Walker, the bar and the
window switcher find the window's desktop entry and icon.

The icons are Command Grille, the default app icon, at 16 to 512 px, cut
from `assets/icon/command-grille-512.png` (Lanczos, with ImageMagick):

```sh
for n in 16 24 32 48 64 128 256; do
  magick ../../assets/icon/command-grille-512.png -filter Lanczos -resize ${n}x${n} -strip \
    PNG32:icons/hicolor/${n}x${n}/apps/dev.shelbi.Desktop.png
done
cp ../../assets/icon/command-grille-512.png icons/hicolor/512x512/apps/dev.shelbi.Desktop.png
```

Another icon picked in Settings ▸ General goes into the user's own hicolor
theme (`~/.local/share/icons/hicolor`, `platform::dock_icon`), which is
looked up before these.
