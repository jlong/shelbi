//! Bundled Software's generated data (`dt-about-bundled-software`): every
//! Rust crate compiled into the app, with its license texts, read at build
//! time from `cargo metadata` and the crates' own files.
//!
//! - **Which crates ship**: `cargo metadata --filter-platform <target>`
//!   resolves the lockfile for the target being built; the walk starts at
//!   this crate and follows only normal dependencies, and stops at a
//!   proc-macro (its code runs in the compiler, not the app). Build and dev
//!   dependencies never ship, so they are left out too.
//! - **Which text**: a crate's license files are the `LICENSE*`, `LICENCE*`,
//!   `COPYING*`, `UNLICENSE*`, `NOTICE*` and `COPYRIGHT*` files in its
//!   package, each classified by what it says (falling back to its name).
//!   Where the crate offers a choice (`MIT OR Apache-2.0`), the first
//!   license in [`PREFERENCE`] whose text the package ships is the one
//!   Shelbi uses it under, and only that text is shown; an `AND` takes one
//!   from each side. `NOTICE` and `COPYRIGHT` files always come along (the
//!   Apache License asks for a NOTICE to be passed on), and so does a
//!   license file for something the crate's own license doesn't cover (the
//!   Lucide icons' ISC license inside gpui-kit-assets). A package that ships
//!   no text for its license takes it from a crate of the same repository
//!   that does, else gets the standard MIT text with the authors its
//!   manifest names, or a note pointing at its repository for any other
//!   license.
//! - **What comes out**: `$OUT_DIR/licenses.rs`, `include!`d by
//!   `src/licenses.rs`: the long tail ([`LONG_TAIL_SKIP`] and Shelbi's own
//!   crates aside) as `CRATES` and one text, `CRATE_NOTICES`, that lists
//!   them and then each distinct notice once with the crates it covers; the
//!   featured entries' texts (`crate_text`); and every shipped crate
//!   (`SHIPPED`), for the tests.
//!
//! The nested cargo runs `--offline --locked`: every package it reads has
//! already been downloaded for the build that runs this script.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Crates Bundled Software lists on their own (with the license files it
/// shows for them), so the generated long tail leaves them out.
const LONG_TAIL_SKIP: &[&str] = &[
    "alacritty_terminal",
    "gpui-pre",
    "gpui-kit",
    "nucleo-matcher",
    "option-ext",
    "portable-pty",
    "termwiz",
];

/// The license files a featured entry shows, `(crate, file)`. gpui-kit's
/// package ships no license file, so its entry shows gpui-base's (the same
/// repository's Apache License); Lucide's icons ship inside gpui-kit-assets.
const FEATURED_FILES: &[(&str, &str)] = &[
    ("alacritty_terminal", "LICENSE-APACHE"),
    ("gpui-pre", "LICENSE-APACHE"),
    ("gpui-base", "LICENSE-APACHE"),
    ("gpui-kit-assets", "LICENSE-LUCIDE"),
    ("nucleo-matcher", "LICENSE"),
    ("option-ext", "LICENSE.txt"),
    ("portable-pty", "LICENSE.md"),
    ("termwiz", "LICENSE.md"),
];

/// The license families, in the order Shelbi takes a crate under one when
/// it offers a choice: the shortest notice first.
const PREFERENCE: &[Kind] = &[
    Kind::Mit,
    Kind::Apache,
    Kind::Bsd,
    Kind::Isc,
    Kind::Zlib,
    Kind::Unicode,
    Kind::Boost,
    Kind::Unlicense,
    Kind::Cc0,
    Kind::Mpl,
    Kind::Wtfpl,
    Kind::Other,
];

/// The MIT permission notice, for a crate whose package ships no text.
const MIT_BODY: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Mit,
    Apache,
    Bsd,
    Isc,
    Zlib,
    Unicode,
    Boost,
    Unlicense,
    Cc0,
    Mpl,
    Wtfpl,
    Other,
}

impl Kind {
    /// The family an SPDX license id belongs to.
    fn of_spdx(id: &str) -> Kind {
        let id = id.split(" WITH ").next().unwrap_or(id).trim();
        match id {
            "MIT" | "MIT-0" => Kind::Mit,
            "Apache-2.0" => Kind::Apache,
            "BSD-2-Clause" | "BSD-3-Clause" => Kind::Bsd,
            "ISC" | "0BSD" => Kind::Isc,
            "Zlib" => Kind::Zlib,
            "Unicode-3.0" | "Unicode-DFS-2016" => Kind::Unicode,
            "BSL-1.0" => Kind::Boost,
            "Unlicense" => Kind::Unlicense,
            "CC0-1.0" => Kind::Cc0,
            "MPL-2.0" => Kind::Mpl,
            "WTFPL" => Kind::Wtfpl,
            _ => Kind::Other,
        }
    }

    fn rank(self) -> usize {
        PREFERENCE.iter().position(|k| *k == self).unwrap_or(PREFERENCE.len())
    }
}

/// What a license file is.
#[derive(Debug, Default)]
struct FileKinds {
    kinds: BTreeSet<Kind>,
    /// A NOTICE or COPYRIGHT file: always passed on.
    notice: bool,
}

fn classify(name: &str, text: &str) -> FileKinds {
    let upper = name.to_ascii_uppercase();
    let mut kinds = BTreeSet::new();
    let says = |s: &str| text.contains(s);
    if says("Permission is hereby granted, free of charge") {
        kinds.insert(Kind::Mit);
    }
    if says("Apache License") && (says("Version 2.0") || says("VERSION 2.0")) {
        kinds.insert(Kind::Apache);
    }
    if says("Redistribution and use in source and binary forms") {
        kinds.insert(Kind::Bsd);
    }
    if says("Permission to use, copy, modify, and/or distribute this software") {
        kinds.insert(Kind::Isc);
    }
    if says("This software is provided 'as-is'") || says("freely, subject to the following restrictions") {
        kinds.insert(Kind::Zlib);
    }
    if says("UNICODE LICENSE") || says("Unicode, Inc.") || says("UNICODE, INC.") {
        kinds.insert(Kind::Unicode);
    }
    if says("Boost Software License") {
        kinds.insert(Kind::Boost);
    }
    if says("This is free and unencumbered software") {
        kinds.insert(Kind::Unlicense);
    }
    if says("CC0") || says("Creative Commons Legal Code") {
        kinds.insert(Kind::Cc0);
    }
    if says("Mozilla Public License") {
        kinds.insert(Kind::Mpl);
    }
    if says("DO WHAT THE FUCK YOU WANT") {
        kinds.insert(Kind::Wtfpl);
    }
    if kinds.is_empty() {
        for (needle, kind) in [
            ("MIT", Kind::Mit),
            ("APACHE", Kind::Apache),
            ("BSD", Kind::Bsd),
            ("ISC", Kind::Isc),
            ("ZLIB", Kind::Zlib),
            ("UNICODE", Kind::Unicode),
            ("BOOST", Kind::Boost),
            ("UNLICENSE", Kind::Unlicense),
            ("CC0", Kind::Cc0),
            ("MPL", Kind::Mpl),
        ] {
            if upper.contains(needle) {
                kinds.insert(kind);
            }
        }
    }
    FileKinds {
        kinds,
        notice: upper.starts_with("NOTICE") || upper.starts_with("COPYRIGHT"),
    }
}

/// A file's text with Unix line ends and no trailing spaces.
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

/// A shipped crate.
struct Package {
    name: String,
    version: String,
    license: String,
    repository: String,
    authors: Vec<String>,
    dir: PathBuf,
    /// Shelbi's own crates (path dependencies named `shelbi*`).
    own: bool,
}

/// The license expression as written, with the old `/` spelled `OR`.
fn expression(raw: &str) -> String {
    raw.split('/').map(str::trim).collect::<Vec<_>>().join(" OR ")
}

/// The expression's `AND` sides, each its `OR` options.
fn choices(expr: &str) -> Vec<Vec<String>> {
    expr.split(" AND ")
        .map(|side| {
            side.replace(['(', ')'], "")
                .split(" OR ")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .collect()
}

/// The license files in `dir`, sorted by name.
fn license_files(dir: &Path) -> Vec<(String, String)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = read
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let upper = name.to_ascii_uppercase();
            let wanted = ["LICENSE", "LICENCE", "COPYING", "UNLICENSE", "NOTICE", "COPYRIGHT"]
                .iter()
                .any(|p| upper.starts_with(p));
            if !wanted {
                return None;
            }
            let text = std::fs::read_to_string(e.path()).ok()?;
            Some((name, normalize(&text)))
        })
        .collect();
    out.sort();
    out
}

/// A package's license files, each with what it is.
fn classified(dir: &Path) -> Vec<(String, String, FileKinds)> {
    license_files(dir)
        .into_iter()
        .map(|(name, text)| {
            let kinds = classify(&name, &text);
            (name, text, kinds)
        })
        .collect()
}

/// The notice a package is passed on with: the texts of the licenses
/// Shelbi takes it under, then any license file for something its own
/// license doesn't cover (bundled third-party work, an unrecognized
/// license), then its NOTICE and COPYRIGHT files. A license whose text the
/// package doesn't ship comes from a crate of the same repository that
/// does, else (MIT) the standard text with the manifest's authors.
fn notice(p: &Package, all: &[Package]) -> String {
    let files = classified(&p.dir);
    let shipped: BTreeSet<Kind> = files.iter().flat_map(|(_, _, k)| k.kinds.iter().copied()).collect();
    let sides = choices(&p.license);
    let offered: BTreeSet<Kind> = sides.iter().flatten().map(|id| Kind::of_spdx(id)).collect();
    let mut chosen: Vec<(Kind, String)> = Vec::new();
    for side in sides {
        let mut options: Vec<(Kind, String)> = side.into_iter().map(|id| (Kind::of_spdx(&id), id)).collect();
        options.sort_by_key(|(k, _)| k.rank());
        let at = options.iter().position(|(k, _)| shipped.contains(k)).unwrap_or(0);
        if at < options.len() {
            chosen.push(options.swap_remove(at));
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let push = |parts: &mut Vec<String>, text: &String| {
        if !parts.contains(text) {
            parts.push(text.clone());
        }
    };
    for (kind, id) in &chosen {
        let texts: Vec<&String> = files
            .iter()
            .filter(|(_, _, k)| k.kinds.contains(kind))
            .map(|(_, t, _)| t)
            .collect();
        if !texts.is_empty() {
            for t in texts {
                push(&mut parts, t);
            }
            continue;
        }
        let sibling = (!p.repository.is_empty())
            .then(|| {
                all.iter()
                    .filter(|o| o.repository == p.repository && o.dir != p.dir)
                    .find_map(|o| classified(&o.dir).into_iter().find(|(_, _, k)| k.kinds.contains(kind)))
            })
            .flatten();
        if let Some((_, text, _)) = sibling {
            push(&mut parts, &text);
        } else if *kind == Kind::Mit {
            // With no authors named, the repository's (objc2's framework
            // crates share one, so they share one notice).
            let who = if p.authors.is_empty() && !p.repository.is_empty() {
                format!("the authors of {}", p.repository)
            } else if p.authors.is_empty() {
                format!("the {} authors", p.name)
            } else {
                p.authors
                    .iter()
                    .map(|a| a.split(" <").next().unwrap_or(a).trim().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            parts.push(format!("MIT License\n\nCopyright (c) {who}\n\n{MIT_BODY}"));
        } else {
            parts.push(format!(
                "{id}. The package ships no license text; see {}.",
                if p.repository.is_empty() { "its repository" } else { &p.repository }
            ));
        }
    }
    for (_, text, kinds) in &files {
        let foreign = !kinds.notice && kinds.kinds.is_disjoint(&offered);
        if kinds.notice || foreign {
            push(&mut parts, text);
        }
    }
    parts.join("\n\n")
}

/// Run `cargo metadata` and walk what ships.
fn shipped(manifest_dir: &Path, target: &str) -> Vec<Package> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", target])
        .current_dir(manifest_dir)
        .output()
        .expect("run cargo metadata for the bundled-software list");
    assert!(
        out.status.success(),
        "cargo metadata failed for the bundled-software list:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: Value = serde_json::from_slice(&out.stdout).expect("cargo metadata JSON");
    let packages: HashMap<&str, &Value> = meta["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| (p["id"].as_str().expect("id"), p))
        .collect();
    let nodes: HashMap<&str, &Value> = meta["resolve"]["nodes"]
        .as_array()
        .expect("resolve")
        .iter()
        .map(|n| (n["id"].as_str().expect("id"), n))
        .collect();
    let root = meta["resolve"]["root"].as_str().expect("root");
    let proc_macro = |p: &Value| {
        p["targets"].as_array().into_iter().flatten().any(|t| {
            t["kind"].as_array().into_iter().flatten().any(|k| k == "proc-macro")
        })
    };
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    let mut out = Vec::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let p = packages[id];
        if proc_macro(p) {
            continue;
        }
        for dep in nodes[id]["deps"].as_array().into_iter().flatten() {
            let normal = dep["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            if normal {
                stack.push(dep["pkg"].as_str().expect("pkg"));
            }
        }
        if id == root {
            continue;
        }
        let name = p["name"].as_str().unwrap_or_default().to_string();
        let manifest = PathBuf::from(p["manifest_path"].as_str().expect("manifest_path"));
        out.push(Package {
            own: p["source"].is_null() && name.starts_with("shelbi"),
            version: p["version"].as_str().unwrap_or_default().to_string(),
            license: expression(p["license"].as_str().unwrap_or("NOASSERTION")),
            repository: p["repository"].as_str().unwrap_or_default().to_string(),
            authors: p["authors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(String::from))
                .collect(),
            dir: manifest.parent().expect("package dir").to_path_buf(),
            name,
        });
    }
    out.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    out
}

/// The long tail's one text: the crates, then each distinct notice once.
fn crate_notices(tail: &[&Package], all: &[Package]) -> String {
    let mut groups: BTreeMap<String, Vec<&Package>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for p in tail {
        let text = notice(p, all);
        if !groups.contains_key(&text) {
            order.push(text.clone());
        }
        groups.entry(text).or_default().push(p);
    }
    let mut s = String::new();
    let _ = writeln!(
        s,
        "These {} open source Rust crates are compiled into the app, besides the ones listed on their own. \
         Each is listed with its version and its license as an SPDX expression. Where a crate offers a \
         choice of licenses, Shelbi uses it under the one whose notice follows.\n",
        tail.len()
    );
    s.push_str("CRATES\n\n");
    for p in tail {
        let _ = writeln!(s, "{} {} · {}", p.name, p.version, p.license);
    }
    s.push_str("\nNOTICES");
    for text in &order {
        let names: Vec<String> = groups[text].iter().map(|p| format!("{} {}", p.name, p.version)).collect();
        let _ = write!(s, "\n\nApplies to {}.\n\n{}", names.join(", "), text);
    }
    s
}

/// Write `$OUT_DIR/licenses.rs`.
pub fn generate() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build_support/licenses.rs");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let target = std::env::var("TARGET").expect("TARGET");
    let all = shipped(&manifest_dir, &target);

    let tail: Vec<&Package> = all
        .iter()
        .filter(|p| !p.own && !LONG_TAIL_SKIP.contains(&p.name.as_str()))
        .collect();

    let mut src = String::from("// Generated by build_support/licenses.rs. Do not edit.\n\n");
    src.push_str("/// The long tail: every shipped crate not listed on its own.\npub const CRATES: &[Crate] = &[\n");
    for p in &tail {
        let _ = writeln!(
            src,
            "    Crate {{ name: {:?}, version: {:?}, license: {:?} }},",
            p.name, p.version, p.license
        );
    }
    src.push_str("];\n\n");
    let _ = writeln!(src, "/// The long tail's text.\npub const CRATE_NOTICES: &str = {:?};\n", crate_notices(&tail, &all));

    src.push_str("/// Every shipped crate (Shelbi's own included), for the tests.\n#[allow(dead_code)]\npub const SHIPPED: &[Crate] = &[\n");
    for p in &all {
        let _ = writeln!(
            src,
            "    Crate {{ name: {:?}, version: {:?}, license: {:?} }},",
            p.name, p.version, p.license
        );
    }
    src.push_str("];\n\n");

    src.push_str("/// A featured entry's license file: `(crate, file)` to its version and text.\npub fn crate_text(name: &str, file: &str) -> Option<(&'static str, &'static str)> {\n    match (name, file) {\n");
    for (name, file) in FEATURED_FILES {
        let p = all
            .iter()
            .find(|p| p.name == *name)
            .unwrap_or_else(|| panic!("{name} no longer ships: update FEATURED_FILES and src/licenses.rs"));
        let text = std::fs::read_to_string(p.dir.join(file))
            .unwrap_or_else(|e| panic!("{name}'s {file}: {e}"));
        let _ = writeln!(src, "        ({name:?}, {file:?}) => Some(({:?}, {:?})),", p.version, normalize(&text));
    }
    src.push_str("        _ => None,\n    }\n}\n");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("licenses.rs");
    std::fs::write(&out, src).expect("write licenses.rs");
}
