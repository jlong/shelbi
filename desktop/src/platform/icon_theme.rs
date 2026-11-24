//! Finding an app's icon on Linux the freedesktop way: a desktop entry's
//! `Icon` names it, and the icon theme has it (the Icon Theme spec, kept to
//! what the tool pickers need).
//!
//! An `Icon` that is a path is that file. A name is looked up in the
//! desktop's icon theme (GTK's `gtk-icon-theme-name`), the themes it
//! `Inherits`, then `hicolor` (where apps install their own icons), in every
//! icon folder (`~/.local/share/icons`, `~/.icons`, each `$XDG_DATA_DIRS`'s
//! `icons`, Flatpak's exports), then `/usr/share/pixmaps`. In a theme a
//! scalable SVG wins (it draws sharp at any size); else the smallest PNG at
//! least the size asked for, else the largest there is.
//!
//! Reads files only; no process runs. The pickers call it off the UI thread.

use std::path::{Path, PathBuf};

/// The icon a desktop entry names, as a file: `Icon=` read from `desktop`,
/// then looked up for `px` pixels. `None` when the entry has none, or no
/// theme has it.
pub fn desktop_icon(desktop: &Path, px: u32) -> Option<PathBuf> {
    let text = std::fs::read_to_string(desktop).ok()?;
    let icon = shelbi_state::tools::desktop_entry_value(&text, "Icon")?;
    let path = Path::new(&icon);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let bases = icon_bases();
    let mut themes = user_themes();
    themes.push("hicolor".to_string());
    find_icon(&icon, px, &bases, &themes).or_else(|| pixmap(&icon))
}

/// The folders icon themes live in, the user's first.
fn icon_bases() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut bases = Vec::new();
    match var("XDG_DATA_HOME") {
        Some(d) => bases.push(PathBuf::from(d).join("icons")),
        None => bases.extend(home.as_ref().map(|h| h.join(".local/share/icons"))),
    }
    bases.extend(home.as_ref().map(|h| h.join(".icons")));
    bases.extend(home.as_ref().map(|h| h.join(".local/share/flatpak/exports/share/icons")));
    let dirs = var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    bases.extend(dirs.split(':').filter(|d| !d.is_empty()).map(|d| Path::new(d).join("icons")));
    bases.push(PathBuf::from("/var/lib/flatpak/exports/share/icons"));
    bases.dedup();
    bases
}

/// The desktop's icon theme (GTK 4's, else GTK 3's `settings.ini`), and the
/// themes it inherits, in lookup order.
fn user_themes() -> Vec<String> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")));
    let theme = config.and_then(|config| {
        ["gtk-4.0", "gtk-3.0"].iter().find_map(|dir| {
            let text = std::fs::read_to_string(config.join(dir).join("settings.ini")).ok()?;
            ini_value(&text, "gtk-icon-theme-name")
        })
    });
    let Some(theme) = theme else {
        return Vec::new();
    };
    let bases = icon_bases();
    let mut themes = vec![theme];
    let mut ix = 0;
    while ix < themes.len() && themes.len() < 8 {
        for inherited in inherits(&themes[ix], &bases) {
            if inherited != "hicolor" && !themes.contains(&inherited) {
                themes.push(inherited);
            }
        }
        ix += 1;
    }
    themes
}

/// `key = value` in an ini file's text, unquoted.
fn ini_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        let v = v.trim().trim_matches('"');
        (k.trim() == key && !v.is_empty()).then(|| v.to_string())
    })
}

/// The themes `theme`'s `index.theme` names in `Inherits`.
fn inherits(theme: &str, bases: &[PathBuf]) -> Vec<String> {
    bases
        .iter()
        .find_map(|b| std::fs::read_to_string(b.join(theme).join("index.theme")).ok())
        .and_then(|text| ini_value(&text, "Inherits"))
        .map(|list| list.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
        .unwrap_or_default()
}

/// `name` in the first of `themes` (in any of `bases`) that has it, for
/// `px` pixels.
pub fn find_icon(name: &str, px: u32, bases: &[PathBuf], themes: &[String]) -> Option<PathBuf> {
    themes.iter().find_map(|theme| {
        let found: Vec<(PathBuf, Option<u32>)> = bases
            .iter()
            .map(|b| b.join(theme))
            .filter(|dir| dir.is_dir())
            .flat_map(|dir| candidates(&dir, name))
            .collect();
        best(found, px)
    })
}

/// Every `<name>.svg` / `<name>.png` in a theme folder's size folders, two
/// levels down (`48x48/apps/`, `apps/48/`, `scalable/apps/`), with the
/// size its folders give (`None`: scalable).
fn candidates(theme_dir: &Path, name: &str) -> Vec<(PathBuf, Option<u32>)> {
    let mut found = Vec::new();
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
            .unwrap_or_default()
    };
    for first in subdirs(theme_dir) {
        for second in subdirs(&first) {
            for ext in ["svg", "png"] {
                let file = second.join(format!("{name}.{ext}"));
                if file.is_file() {
                    let size = if ext == "svg" { None } else { folder_size(&first).or_else(|| folder_size(&second)) };
                    found.push((file, size));
                }
            }
        }
    }
    found
}

/// The pixel size a theme's size folder names: `48x48`, `48x48@2` (96), or
/// `48`. `None` for anything else (`scalable`, `apps`).
fn folder_size(dir: &Path) -> Option<u32> {
    let name = dir.file_name()?.to_str()?;
    let (base, scale) = match name.split_once('@') {
        Some((base, scale)) => (base, scale.parse::<u32>().ok()?),
        None => (name, 1),
    };
    let side = base.split_once('x').map_or(base, |(w, _)| w);
    side.parse::<u32>().ok().map(|s| s * scale)
}

/// The best of `found` for `px`: an SVG, else the smallest PNG at least
/// `px`, else the largest.
fn best(found: Vec<(PathBuf, Option<u32>)>, px: u32) -> Option<PathBuf> {
    if let Some((svg, _)) = found.iter().find(|(p, _)| p.extension().is_some_and(|e| e == "svg")) {
        return Some(svg.clone());
    }
    let sized = |s: &Option<u32>| s.unwrap_or(0);
    found
        .iter()
        .filter(|(_, s)| sized(s) >= px)
        .min_by_key(|(_, s)| sized(s))
        .or_else(|| found.iter().max_by_key(|(_, s)| sized(s)))
        .map(|(p, _)| p.clone())
}

/// The legacy `/usr/share/pixmaps/<name>.{svg,png}`.
fn pixmap(name: &str) -> Option<PathBuf> {
    ["svg", "png"]
        .iter()
        .map(|ext| Path::new("/usr/share/pixmaps").join(format!("{name}.{ext}")))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn a_theme_prefers_svg_then_the_smallest_png_big_enough() {
        let root = std::env::temp_dir().join(format!("shelbi-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bases = vec![root.join("user"), root.join("system")];
        touch(&root.join("system/hicolor/32x32/apps/code.png"));
        touch(&root.join("system/hicolor/128x128/apps/code.png"));
        touch(&root.join("system/hicolor/512x512/apps/code.png"));
        touch(&root.join("system/hicolor/16x16/apps/zed.png"));
        touch(&root.join("user/Papirus/64x64/apps/zed.svg"));
        touch(&root.join("system/Yaru/apps/48@2/meld.png"));
        let themes = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        let hicolor = themes(&["hicolor"]);
        assert_eq!(find_icon("code", 64, &bases, &hicolor), Some(root.join("system/hicolor/128x128/apps/code.png")));
        assert_eq!(find_icon("code", 600, &bases, &hicolor), Some(root.join("system/hicolor/512x512/apps/code.png")));
        assert_eq!(find_icon("zed", 64, &bases, &hicolor), Some(root.join("system/hicolor/16x16/apps/zed.png")));
        // The desktop's theme first, then hicolor.
        let papirus = themes(&["Papirus", "hicolor"]);
        assert_eq!(find_icon("zed", 64, &bases, &papirus), Some(root.join("user/Papirus/64x64/apps/zed.svg")));
        assert_eq!(find_icon("code", 32, &bases, &papirus), Some(root.join("system/hicolor/32x32/apps/code.png")));
        assert_eq!(find_icon("meld", 64, &bases, &themes(&["Yaru"])), Some(root.join("system/Yaru/apps/48@2/meld.png")));
        assert_eq!(find_icon("nope", 64, &bases, &papirus), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn size_folders_read_as_pixels() {
        assert_eq!(folder_size(Path::new("48x48")), Some(48));
        assert_eq!(folder_size(Path::new("48x48@2")), Some(96));
        assert_eq!(folder_size(Path::new("64")), Some(64));
        assert_eq!(folder_size(Path::new("scalable")), None);
        assert_eq!(ini_value("[Settings]\ngtk-icon-theme-name = \"Yaru-blue\"\n", "gtk-icon-theme-name").as_deref(), Some("Yaru-blue"));
    }
}
