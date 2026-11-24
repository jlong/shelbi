# AUR packages

Shelbi ships two prebuilt packages on the [AUR](https://aur.archlinux.org),
x86_64 only for now:

| Package | Installs | From the release asset |
|---|---|---|
| `shelbi-bin` | `/usr/bin/shelbi`, the system plugins under `/usr/share/shelbi/plugins` | `shelbi_Linux_x86_64.tar.gz` |
| `shelbi-desktop-bin` | `/usr/bin/shelbi-desktop`, `/usr/share/applications/dev.shelbi.Desktop.desktop`, hicolor icons 16 to 512 px | `shelbi-desktop_Linux_x86_64.tar.gz` |

`shelbi-desktop-bin` depends on `shelbi-bin` of the same version, because the
app runs only against a CLI of its own version. So installing the app installs
the CLI, and the app never downloads one on Linux. On Omarchy:

```sh
yay -S shelbi-desktop-bin   # pulls in shelbi-bin
```

The app then shows up in Walker and the Omarchy menu as Shelbi.

## Templates

Each directory holds a `PKGBUILD.in`, not a `PKGBUILD`: the version and the
archive's SHA-256 are filled in per release by
`scripts/release/render-aur-package.sh`, which also writes `.SRCINFO`
(`makepkg --printsrcinfo`, so it runs on Arch). The `aur-publish` job in
`.github/workflows/release.yml` renders both, builds and installs them in an
Arch container, checks the installed app finds the installed CLI
(`shelbi-desktop --print-version`), then pushes `PKGBUILD` and `.SRCINFO` to
each package's AUR repository. See `docs/release.md`.

## Building by hand

From a published release:

```sh
scripts/release/render-aur-package.sh --package shelbi-bin --version 0.15.0 --out /tmp/aur/shelbi-bin
scripts/release/render-aur-package.sh --package shelbi-desktop-bin --version 0.15.0 --out /tmp/aur/shelbi-desktop-bin
(cd /tmp/aur/shelbi-bin && makepkg -si)
(cd /tmp/aur/shelbi-desktop-bin && makepkg -si)
```

`shelbi-bin` goes first: plain `makepkg -s` resolves dependencies from the
official repositories only, so it can't fetch `shelbi-bin` from the AUR (an
AUR helper such as `yay` does).

Before a release exists, package a local build of the app instead:

```sh
desktop/scripts/linux-archive.sh            # desktop/target/dist/shelbi-desktop_Linux_x86_64.tar.gz
scripts/release/render-aur-package.sh --package shelbi-desktop-bin --version "$(sed -n 's/^version = "\(.*\)"/\1/p' desktop/Cargo.toml)" \
  --archive desktop/target/dist/shelbi-desktop_Linux_x86_64.tar.gz --out desktop/target/aur/shelbi-desktop-bin
(cd desktop/target/aur/shelbi-desktop-bin && makepkg -si)
```

`--archive` hashes the local archive and copies it beside the `PKGBUILD`
under the source's file name, so makepkg uses it instead of downloading. The
`shelbi-bin` it needs must be of the same version (the published one for the
current version, rendered as above).
