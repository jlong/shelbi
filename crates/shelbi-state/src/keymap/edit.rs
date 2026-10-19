//! Editing `keys.yaml` in place: set or clear one action's chords, or every
//! override at once, keeping the rest of the file as the user wrote it.
//!
//! The desktop's Settings ▸ Keybindings page saves through here, and the TUI
//! reads the result like any hand-written `keys.yaml` (the same
//! `defaults.<mode>.<action>` / `projects.<name>.<mode>.<action>` layout the
//! [`loader`](super::loader) merges).
//!
//! Every edit is made twice. Structurally, on the parsed YAML tree: that is
//! the edit's meaning. Then on the text, line by line, so comments, blank
//! lines, key order and quoting survive. The text edit is kept only when it
//! parses back to exactly the structural result; anything the line editor
//! doesn't follow (flow mappings like `defaults: {global: ...}`, anchors,
//! odd indentation) falls back to re-serializing the tree, which is correct
//! but drops comments. A file that doesn't parse is never rewritten.

use serde_yaml::{Mapping, Value};

use super::actions::Action;
use super::chord::KeyChord;
use super::key::{Key, Mods};
use super::loader::KEYS_FILENAME;

/// Which layer of `keys.yaml` an override lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// `defaults.<mode>.<action>`: every project.
    Defaults,
    /// `projects.<name>.<mode>.<action>`: one project, over the defaults.
    Project(&'a str),
}

/// Why `keys.yaml` couldn't be edited.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The file doesn't parse as YAML. It is left alone; the loader reports
    /// the same parse error.
    #[error("keys.yaml doesn't parse: {0}")]
    Parse(String),
    /// The file parses, but the override's place in it holds something
    /// other than a mapping (`defaults: [..]`), so there's nowhere to put it
    /// without losing what's there.
    #[error("keys.yaml has `{0}` that isn't a mapping")]
    Shape(String),
    #[error("couldn't write keys.yaml: {0}")]
    Serialize(String),
}

/// The layer that sets `action` for `project`: the project's block when it
/// has an entry for it, else `defaults` when that has one, else `None` (the
/// built-in chords apply). A YAML `null` entry falls through, as it does
/// for the loader.
pub fn override_scope<'a>(text: &str, project: Option<&'a str>, action: Action) -> Option<Scope<'a>> {
    let root = parse(text).ok()?;
    let sets = |path: &[&str]| {
        lookup(&root, path)
            .and_then(|m| m.get(Value::String(action.key_name().into())))
            .is_some_and(|v| !v.is_null())
    };
    if let Some(name) = project {
        if sets(&["projects", name, action.mode()]) {
            return Some(Scope::Project(name));
        }
    }
    sets(&["defaults", action.mode()]).then_some(Scope::Defaults)
}

/// Bind `action` to exactly `chords` (an empty list unbinds it) for
/// `project`, and return the new file. The override goes where the action's
/// current one is ([`override_scope`]), else into `defaults`. Chords equal
/// to the action's built-in ones clear its overrides instead, so the file
/// only ever holds what differs.
pub fn set_chords(
    text: &str,
    project: Option<&str>,
    action: Action,
    chords: &[KeyChord],
) -> Result<String, EditError> {
    if chords == builtin_chords(action) {
        return clear(text, project, action);
    }
    let scope = override_scope(text, project, action).unwrap_or(Scope::Defaults);
    set_override(text, scope, action, Some(chords))
}

/// Clear every override of `action` that applies to `project` (its
/// `defaults` entry and the project's), so the built-in chords apply.
pub fn clear(text: &str, project: Option<&str>, action: Action) -> Result<String, EditError> {
    let mut text = set_override(text, Scope::Defaults, action, None)?;
    if let Some(name) = project {
        text = set_override(&text, Scope::Project(name), action, None)?;
    }
    Ok(text)
}

/// Clear every action's overrides for `project`: what Reset all does.
/// Entries the loader doesn't know (another client's mode, a typo) and other
/// projects' blocks are kept.
pub fn clear_all(text: &str, project: Option<&str>) -> Result<String, EditError> {
    let mut text = text.to_string();
    for action in Action::all() {
        text = clear(&text, project, action)?;
    }
    Ok(text)
}

/// Write (`Some`) or remove (`None`) one override in `scope`. Removing the
/// last entry of a block removes the emptied block too.
pub fn set_override(
    text: &str,
    scope: Scope<'_>,
    action: Action,
    chords: Option<&[KeyChord]>,
) -> Result<String, EditError> {
    let path: Vec<&str> = match scope {
        Scope::Defaults => vec!["defaults", action.mode()],
        Scope::Project(name) => vec!["projects", name, action.mode()],
    };
    let key = action.key_name();
    let root = parse(text)?;
    let mut expected = root.clone();
    match chords {
        Some(chords) => insert(&mut expected, &path, key, chords_value(chords))?,
        None => remove(&mut expected, &path, key),
    }
    if expected == root {
        return Ok(text.to_string());
    }
    let edited = match chords {
        Some(chords) => lines::set(text, &path, key, &chords_yaml(chords)),
        None => lines::remove(text, &path, key),
    };
    if let Some(edited) = edited {
        if parse(&edited).is_ok_and(|v| v == expected) {
            return Ok(edited);
        }
    }
    if expected.as_mapping().is_some_and(Mapping::is_empty) {
        return Ok(String::new());
    }
    serde_yaml::to_string(&expected).map_err(|e| EditError::Serialize(e.to_string()))
}

/// Read `$SHELBI_HOME/keys.yaml`; a missing file is empty.
pub fn read_keys_file() -> crate::Result<String> {
    let path = crate::shelbi_home()?.join(KEYS_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Apply `edit` to `keys.yaml` as it is on disk now and write the result
/// atomically (creating the file and `$SHELBI_HOME` as needed). Returns the
/// file's new text. A failed edit writes nothing.
pub fn update_keys_file(edit: impl FnOnce(&str) -> Result<String, EditError>) -> Result<String, String> {
    let home = crate::shelbi_home().map_err(|e| e.to_string())?;
    let current = read_keys_file().map_err(|e| format!("couldn't read keys.yaml: {e}"))?;
    let text = edit(&current).map_err(|e| e.to_string())?;
    if text != current {
        std::fs::create_dir_all(&home).map_err(|e| format!("couldn't write keys.yaml: {e}"))?;
        crate::atomic_write(&home.join(KEYS_FILENAME), text.as_bytes())
            .map_err(|e| format!("couldn't write keys.yaml: {e}"))?;
    }
    Ok(text)
}

/// The action's built-in chords, parsed.
pub fn builtin_chords(action: Action) -> Vec<KeyChord> {
    action
        .default_chords()
        .iter()
        .filter_map(|s| KeyChord::parse(s).ok())
        .collect()
}

/// A chord as `keys.yaml` spells it: the canonical form, except that a
/// shifted letter is its capital (`K`, as the built-in defaults write it;
/// it parses to the same `shift-k`).
pub fn chord_spec(chord: &KeyChord) -> String {
    match chord.code {
        Key::Char(c) if c.is_ascii_alphabetic() && chord.mods == Mods::SHIFT => {
            c.to_ascii_uppercase().to_string()
        }
        _ => chord.canonical(),
    }
}

/// A chord list as a `keys.yaml` value: one chord as a scalar, several (or
/// none) as a flow list.
pub fn chords_yaml(chords: &[KeyChord]) -> String {
    match chords {
        [one] => yaml_scalar(&chord_spec(one)),
        _ => {
            let items: Vec<String> = chords.iter().map(|c| yaml_scalar(&chord_spec(c))).collect();
            format!("[{}]", items.join(", "))
        }
    }
}

fn chords_value(chords: &[KeyChord]) -> Value {
    match chords {
        [one] => Value::String(chord_spec(one)),
        _ => Value::Sequence(chords.iter().map(|c| Value::String(chord_spec(c))).collect()),
    }
}

/// `s` as a YAML scalar: plain when it can only read as that string, else
/// single-quoted (`'?'`, `'1'`, `'-'`).
fn yaml_scalar(s: &str) -> String {
    const WORDS: &[&str] = &["true", "false", "null", "yes", "no", "on", "off", "y", "n"];
    let plain = s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '='))
        && !WORDS.contains(&s.to_ascii_lowercase().as_str());
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

/// The file's YAML tree; an empty or comment-only file is an empty mapping.
fn parse(text: &str) -> Result<Value, EditError> {
    match serde_yaml::from_str::<Value>(text) {
        Ok(Value::Null) => Ok(Value::Mapping(Mapping::new())),
        Ok(v) => Ok(v),
        Err(e) => Err(EditError::Parse(e.to_string())),
    }
}

fn lookup<'v>(root: &'v Value, path: &[&str]) -> Option<&'v Mapping> {
    let mut map = root.as_mapping()?;
    for seg in path {
        map = map.get(Value::String((*seg).into()))?.as_mapping()?;
    }
    Some(map)
}

fn insert(root: &mut Value, path: &[&str], key: &str, value: Value) -> Result<(), EditError> {
    let mut map = root
        .as_mapping_mut()
        .ok_or_else(|| EditError::Shape("the top level".into()))?;
    for (depth, seg) in path.iter().enumerate() {
        let k = Value::String((*seg).into());
        let entry = map.entry(k).or_insert_with(|| Value::Mapping(Mapping::new()));
        // A bare `defaults:` (null) is an empty block.
        if entry.is_null() {
            *entry = Value::Mapping(Mapping::new());
        }
        map = entry
            .as_mapping_mut()
            .ok_or_else(|| EditError::Shape(path[..=depth].join(".")))?;
    }
    map.insert(Value::String(key.into()), value);
    Ok(())
}

/// Remove `path.key`, then every block on `path` it left empty.
fn remove(root: &mut Value, path: &[&str], key: &str) {
    fn go(map: &mut Mapping, path: &[&str], key: &str) {
        match path.split_first() {
            None => {
                map.shift_remove(Value::String(key.into()));
            }
            Some((seg, rest)) => {
                let k = Value::String((*seg).into());
                let Some(child) = map.get_mut(&k).and_then(Value::as_mapping_mut) else {
                    return;
                };
                let had = child.len();
                go(child, rest, key);
                if child.is_empty() && had > 0 {
                    map.shift_remove(&k);
                }
            }
        }
    }
    if let Some(map) = root.as_mapping_mut() {
        go(map, path, key);
    }
}

/// The line-level editor for block-style YAML mappings. Each function
/// returns `None` for a shape it doesn't follow; the caller then falls back
/// to the structural edit. Its output is always checked by re-parsing, so
/// it only has to be right for the shapes it accepts.
mod lines {
    /// Set `path.key: value`, creating missing blocks at the end of their
    /// parent.
    pub fn set(text: &str, path: &[&str], key: &str, value: &str) -> Option<String> {
        let mut doc = Doc::new(text);
        let unit = doc.indent_unit();
        let mut body = (0, doc.lines.len());
        let mut parent: Option<usize> = None;
        for (depth, seg) in path.iter().enumerate() {
            match doc.find(body, parent, seg) {
                Some(at) => {
                    if !doc.opens_block(at) {
                        return None;
                    }
                    parent = Some(indent(&doc.lines[at]));
                    body = (at + 1, doc.span_end(at));
                }
                None => {
                    let mut ind = doc.child_indent(body, parent, unit);
                    let mut new = Vec::new();
                    for seg in &path[depth..] {
                        new.push(format!("{}{}:", " ".repeat(ind), yaml_key(seg)));
                        ind += unit;
                    }
                    new.push(format!("{}{}: {value}", " ".repeat(ind), yaml_key(key)));
                    doc.insert(body.1, new);
                    return Some(doc.text());
                }
            }
        }
        match doc.find(body, parent, key) {
            Some(at) => {
                let ind = indent(&doc.lines[at]);
                let comment = trailing_comment(&doc.lines[at]).map(str::to_string);
                let mut line = format!("{}{}: {value}", " ".repeat(ind), yaml_key(key));
                if let Some(comment) = comment {
                    line.push_str("  ");
                    line.push_str(&comment);
                }
                let end = doc.span_end(at);
                doc.lines.splice(at..end, [line]);
            }
            None => {
                let ind = doc.child_indent(body, parent, unit);
                doc.insert(body.1, vec![format!("{}{}: {value}", " ".repeat(ind), yaml_key(key))]);
            }
        }
        Some(doc.text())
    }

    /// Remove `path.key` and every block it leaves empty. A missing entry
    /// is no edit.
    pub fn remove(text: &str, path: &[&str], key: &str) -> Option<String> {
        let mut doc = Doc::new(text);
        // The key lines of `path`'s blocks, outermost first.
        let mut opened = Vec::new();
        let mut body = (0, doc.lines.len());
        let mut parent: Option<usize> = None;
        for seg in path {
            let at = doc.find(body, parent, seg)?;
            if !doc.opens_block(at) {
                return None;
            }
            opened.push(at);
            parent = Some(indent(&doc.lines[at]));
            body = (at + 1, doc.span_end(at));
        }
        let at = doc.find(body, parent, key)?;
        let end = doc.span_end(at);
        doc.lines.drain(at..end);
        // Innermost first: a block with no entries left goes too.
        for &block in opened.iter().rev() {
            let end = doc.span_end(block);
            if doc.lines[block + 1..end].iter().any(|l| is_content(l)) {
                break;
            }
            doc.lines.drain(block..end);
        }
        Some(doc.text())
    }

    struct Doc {
        lines: Vec<String>,
    }

    impl Doc {
        fn new(text: &str) -> Doc {
            Doc {
                lines: text.lines().map(str::to_string).collect(),
            }
        }

        fn text(&self) -> String {
            let mut out = self.lines.join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            out
        }

        /// The file's indentation step: its first indented entry's, else 2.
        fn indent_unit(&self) -> usize {
            self.lines
                .iter()
                .filter(|l| is_content(l))
                .map(|l| indent(l))
                .find(|&i| i > 0)
                .unwrap_or(2)
        }

        /// The line of `key`'s entry among the mapping entries in `body`
        /// (those at the body's first indentation, deeper than `parent`).
        fn find(&self, body: (usize, usize), parent: Option<usize>, key: &str) -> Option<usize> {
            let mut level = None;
            for at in body.0..body.1 {
                let line = &self.lines[at];
                if !is_content(line) {
                    continue;
                }
                let ind = indent(line);
                if parent.is_some_and(|p| ind <= p) {
                    return None;
                }
                let level = *level.get_or_insert(ind);
                if ind == level && key_of(line).is_some_and(|(k, _)| k == key) {
                    return Some(at);
                }
            }
            None
        }

        /// Whether the entry at `at` opens a block (`defaults:`, maybe with
        /// a comment), rather than holding an inline value.
        fn opens_block(&self, at: usize) -> bool {
            let line = &self.lines[at];
            key_of(line).is_some_and(|(_, rest)| {
                let rest = rest.trim();
                rest.is_empty() || rest.starts_with('#')
            })
        }

        /// One past the last line of the entry at `at`: its deeper lines
        /// (and a block list at its own indentation), without trailing
        /// blank or comment lines.
        fn span_end(&self, at: usize) -> usize {
            let ind = indent(&self.lines[at]);
            let mut end = at + 1;
            let mut j = at + 1;
            while j < self.lines.len() {
                let line = &self.lines[j];
                if is_content(line) {
                    let li = indent(line);
                    let item = line.trim_start().starts_with('-');
                    if li > ind || (li == ind && item) {
                        end = j + 1;
                    } else {
                        break;
                    }
                }
                j += 1;
            }
            end
        }

        /// The indentation a new entry in `body` takes: its entries', else
        /// one step in from `parent`.
        fn child_indent(&self, body: (usize, usize), parent: Option<usize>, unit: usize) -> usize {
            self.lines[body.0..body.1]
                .iter()
                .find(|l| is_content(l))
                .map(|l| indent(l))
                .unwrap_or_else(|| parent.map_or(0, |p| p + unit))
        }

        fn insert(&mut self, at: usize, new: Vec<String>) {
            // At the end of the file, after its trailing blank lines' worth
            // of separation stays as it was.
            self.lines.splice(at..at, new);
        }
    }

    fn indent(line: &str) -> usize {
        line.len() - line.trim_start_matches(' ').len()
    }

    fn is_content(line: &str) -> bool {
        let t = line.trim_start();
        !t.is_empty() && !t.starts_with('#')
    }

    /// A mapping entry's key (unquoted) and what follows its colon.
    fn key_of(line: &str) -> Option<(String, &str)> {
        let t = line.trim_start();
        if let Some(q @ ('"' | '\'')) = t.chars().next() {
            let close = t[1..].find(q)? + 1;
            let rest = t[close + 1..].strip_prefix(':')?;
            if !(rest.is_empty() || rest.starts_with(' ')) {
                return None;
            }
            return Some((t[1..close].to_string(), rest));
        }
        if t.starts_with(['-', '{', '[', '?', '&', '*', '!', '|', '>']) {
            return None;
        }
        let mut search = 0;
        loop {
            let colon = search + t[search..].find(':')?;
            let rest = &t[colon + 1..];
            if rest.is_empty() || rest.starts_with(' ') {
                return Some((t[..colon].trim_end().to_string(), rest));
            }
            search = colon + 1;
        }
    }

    /// The `# ...` comment ending an entry's line, if any (a `#` after
    /// whitespace, outside quotes).
    fn trailing_comment(line: &str) -> Option<&str> {
        let (_, rest) = key_of(line)?;
        let start = line.len() - rest.len();
        let mut quote = None;
        let mut prev = ' ';
        for (i, c) in rest.char_indices() {
            match (quote, c) {
                (None, '\'' | '"') => quote = Some(c),
                (Some(q), c) if c == q => quote = None,
                (None, '#') if prev.is_whitespace() => return Some(&line[start + i..]),
                _ => {}
            }
            prev = c;
        }
        None
    }

    /// A mapping key as written: plain unless it needs quotes (a project
    /// named `my project: two`).
    fn yaml_key(key: &str) -> String {
        let plain = key.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if plain {
            key.to_string()
        } else {
            format!("'{}'", key.replace('\'', "''"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{validate_keymaps_yaml, GlobalAction, KanbanAction, SidebarAction};

    fn chords(specs: &[&str]) -> Vec<KeyChord> {
        specs.iter().map(|s| KeyChord::parse(s).unwrap()).collect()
    }

    const NAV_UP: Action = Action::Sidebar(SidebarAction::NavUp);
    const REFRESH: Action = Action::Kanban(KanbanAction::Refresh);
    const PALETTE: Action = Action::Global(GlobalAction::OpenPalette);

    /// The merged chords `text` gives `action`, through the real loader.
    fn loaded(text: &str, project: Option<&str>, action: Action) -> Vec<KeyChord> {
        let (keymaps, diags) = validate_keymaps_yaml(text, project);
        assert!(diags.is_empty(), "{diags:?}\n{text}");
        keymaps.chords(action)
    }

    #[test]
    fn a_new_file_gets_the_override_in_the_documented_layout() {
        let out = set_chords("", None, NAV_UP, &chords(&["w", "up"])).unwrap();
        assert_eq!(out, "defaults:\n  sidebar:\n    nav_up: [w, up]\n");
        assert_eq!(loaded(&out, None, NAV_UP), chords(&["w", "up"]));
    }

    #[test]
    fn comments_and_unrelated_entries_survive_an_edit() {
        let text = "\
# My bindings.
defaults:
    # Vim all the way.
    sidebar:
        nav_up: [k, up]   # keep k
        refresh: r

    kanban:
        refresh: f5

# Work project.
projects:
    work:
        kanban:
            open_workspace: O
";
        let out = set_chords(text, None, NAV_UP, &chords(&["w"])).unwrap();
        assert_eq!(
            out,
            text.replace("nav_up: [k, up]   # keep k", "nav_up: w  # keep k")
        );
        // A mode with no block yet goes at the end of `defaults`, at the
        // file's own indentation.
        let out = set_chords(&out, None, PALETTE, &chords(&["super-k"])).unwrap();
        assert!(
            out.contains("        refresh: f5\n    global:\n        open_palette: super-k\n\n# Work project."),
            "{out}"
        );
        assert_eq!(loaded(&out, None, PALETTE), chords(&["super-k"]));
        assert_eq!(loaded(&out, Some("work"), Action::Kanban(KanbanAction::OpenWorkspace)), chords(&["O"]));
    }

    #[test]
    fn a_project_override_is_edited_in_its_project() {
        let text = "projects:\n  work:\n    kanban:\n      refresh: f5\n";
        assert_eq!(override_scope(text, Some("work"), REFRESH), Some(Scope::Project("work")));
        assert_eq!(override_scope(text, Some("home"), REFRESH), None);
        let out = set_chords(text, Some("work"), REFRESH, &chords(&["f6"])).unwrap();
        assert_eq!(out, "projects:\n  work:\n    kanban:\n      refresh: f6\n");
        // Another project's edit goes to the defaults.
        let out = set_chords(&out, Some("home"), REFRESH, &chords(&["f7"])).unwrap();
        assert_eq!(loaded(&out, Some("home"), REFRESH), chords(&["f7"]));
        assert_eq!(loaded(&out, Some("work"), REFRESH), chords(&["f6"]));
    }

    #[test]
    fn the_builtin_chords_clear_the_override_and_its_emptied_blocks() {
        let text = "# top\ndefaults:\n  sidebar:\n    nav_up: w\n  kanban:\n    refresh: f5\n";
        let out = set_chords(text, None, NAV_UP, &chords(&["k", "up"])).unwrap();
        assert_eq!(out, "# top\ndefaults:\n  kanban:\n    refresh: f5\n");
        let out = clear(&out, None, REFRESH).unwrap();
        assert_eq!(out, "# top\n");
        assert_eq!(loaded(&out, None, REFRESH), builtin_chords(REFRESH));
    }

    #[test]
    fn clear_all_keeps_other_projects_and_unknown_entries() {
        let text = "\
defaults:
  sidebar:
    nav_up: w
  future_mode:
    thing: x
projects:
  work:
    sidebar:
      refresh: f5
  home:
    sidebar:
      refresh: f6
";
        let out = clear_all(text, Some("work")).unwrap();
        assert_eq!(
            out,
            "defaults:\n  future_mode:\n    thing: x\nprojects:\n  home:\n    sidebar:\n      refresh: f6\n"
        );
    }

    #[test]
    fn an_empty_list_unbinds_and_awkward_chords_are_quoted() {
        let out = set_chords("", None, NAV_UP, &[]).unwrap();
        assert_eq!(out, "defaults:\n  sidebar:\n    nav_up: []\n");
        assert!(loaded(&out, None, NAV_UP).is_empty());
        let awkward = chords(&["?", "-", "1", "y", "ctrl-[", "#", "'", "shift-k", "super-+"]);
        let out = set_chords("", None, NAV_UP, &awkward).unwrap();
        assert_eq!(
            out,
            "defaults:\n  sidebar:\n    nav_up: ['?', '-', '1', 'y', 'ctrl-[', '#', '''', K, super-+]\n"
        );
        assert_eq!(loaded(&out, None, NAV_UP), awkward);
    }

    #[test]
    fn a_block_list_value_is_replaced_whole() {
        let text = "defaults:\n  sidebar:\n    nav_up:\n    - k\n    - up\n    refresh: r\n";
        let out = set_chords(text, None, NAV_UP, &chords(&["w"])).unwrap();
        assert_eq!(out, "defaults:\n  sidebar:\n    nav_up: w\n    refresh: r\n");
    }

    #[test]
    fn a_flow_style_file_falls_back_to_a_structural_rewrite() {
        let text = "defaults: {sidebar: {nav_up: w}} # flow\n";
        let out = set_chords(text, None, REFRESH, &chords(&["f5"])).unwrap();
        assert_eq!(loaded(&out, None, NAV_UP), chords(&["w"]));
        assert_eq!(loaded(&out, None, REFRESH), chords(&["f5"]));
    }

    #[test]
    fn a_broken_file_is_never_rewritten() {
        let text = "defaults:\n  sidebar: [unclosed\n";
        assert!(matches!(set_chords(text, None, NAV_UP, &chords(&["w"])), Err(EditError::Parse(_))));
        let text = "defaults: [1, 2]\n";
        assert!(matches!(set_chords(text, None, NAV_UP, &chords(&["w"])), Err(EditError::Shape(_))));
    }

    #[test]
    fn a_comment_only_file_takes_the_override_after_its_comments() {
        let stub = "# Shelbi key bindings.\n# More.\n";
        let out = set_chords(stub, None, NAV_UP, &chords(&["w"])).unwrap();
        assert_eq!(out, format!("{stub}defaults:\n  sidebar:\n    nav_up: w\n"));
        assert_eq!(clear(&out, None, NAV_UP).unwrap(), stub);
    }

    #[test]
    fn the_file_on_disk_is_read_edited_and_written_back() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("shelbi-keys-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("SHELBI_HOME", &home);

        // No home yet, no file: empty, and the first edit creates both.
        assert_eq!(read_keys_file().unwrap(), "");
        let out = update_keys_file(|t| set_chords(t, None, NAV_UP, &chords(&["w"]))).unwrap();
        assert_eq!(std::fs::read_to_string(home.join(KEYS_FILENAME)).unwrap(), out);
        // A hand edit since is kept under the next change.
        std::fs::write(home.join(KEYS_FILENAME), format!("# mine\n{out}")).unwrap();
        update_keys_file(|t| set_chords(t, None, REFRESH, &chords(&["f5"]))).unwrap();
        let text = read_keys_file().unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        let (keymaps, _) = crate::keymap::load_keymaps(None);
        assert_eq!(keymaps.chords(NAV_UP), chords(&["w"]));
        assert_eq!(keymaps.chords(REFRESH), chords(&["f5"]));
        // A failed edit writes nothing.
        assert!(update_keys_file(|_| Err(EditError::Parse("no".into()))).is_err());
        assert_eq!(read_keys_file().unwrap(), text);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn every_action_round_trips_through_the_loader() {
        let mut text = String::new();
        for action in Action::all() {
            text = set_chords(&text, Some("p"), action, &chords(&["f12"])).unwrap();
        }
        // f12 everywhere collides within each mode; check the file instead.
        let root: Value = serde_yaml::from_str(&text).unwrap();
        for action in Action::all() {
            let v = lookup(&root, &["defaults", action.mode()])
                .and_then(|m| m.get(Value::String(action.key_name().into())));
            assert_eq!(v, Some(&Value::String("f12".into())), "{action:?}");
        }
        assert_eq!(clear_all(&text, Some("p")).unwrap(), "");
    }
}
