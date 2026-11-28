// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreUI/src/frontmatter.rs), MIT in Shelbi.

//! YAML frontmatter as the editor's **document-properties block**.
//!
//! The Milkdown bundle already renders frontmatter as an editable key-value
//! table (`Editor/src/frontmatter.js`, byte-identical to the Swift app's), and
//! Settings already has the "Show document properties" toggle. Nothing on the
//! Rust side ever fed it: frontmatter was split off on load and re-prepended on
//! save, so it survived round-trips but never appeared, and the toggle did
//! nothing.
//!
//! This is the missing half — the port of `EditorViewController.frontmatterToJSON`
//! and `jsonToFrontmatter`. The JS wants an ordered array of
//! `{key, value, type[, rawYaml]}` objects; edits come back in the same shape and
//! are written out as YAML again.
//!
//! **Order and fidelity.** Entries follow the order the keys appear in the
//! source text, not the parse's map order, so saving doesn't shuffle a
//! another editor's file. Values a table can't represent — nested maps, arrays
//! of maps, multi-line block scalars — are marked `complex` and carry their
//! original YAML block, which is written back verbatim. YAML that doesn't parse
//! at all becomes a single `error` entry holding the whole original text, so a
//! malformed block is shown as-is and saved unchanged rather than being
//! destroyed by a round-trip.

use yaml_serde::Value;

/// What kind of value an entry holds, which decides how it round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A single scalar: written back as `key: value`.
    Scalar,
    /// A flat list of scalars: written back as `key: [a, b]`.
    Array,
    /// Anything a flat table can't hold; written back from `raw_yaml`.
    Complex,
    /// The whole block failed to parse; `raw_yaml` is the original text.
    Error,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Scalar => "scalar",
            Kind::Array => "array",
            Kind::Complex => "complex",
            Kind::Error => "error",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "array" => Kind::Array,
            "complex" => Kind::Complex,
            "error" => Kind::Error,
            _ => Kind::Scalar,
        }
    }
}

/// One row of the properties block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The top-level key.
    pub key: String,
    /// The value as shown: a scalar's text, an array's items joined with
    /// `, `, or a one-line preview of a complex value.
    pub value: String,
    /// How the entry round-trips.
    pub kind: Kind,
    /// The original YAML for this key, kept for `Complex`/`Error` entries so an
    /// unrepresentable value survives a save untouched.
    pub raw_yaml: Option<String>,
}

/// Parse a frontmatter block (without its `---` fences) into display entries.
pub fn parse(yaml: &str) -> Vec<Entry> {
    if yaml.trim().is_empty() {
        return Vec::new();
    }
    let parsed: Option<Value> = yaml_serde::from_str(yaml).ok();
    let Some(Value::Mapping(map)) = parsed else {
        // Either it isn't valid YAML, or it is but isn't a key-value mapping —
        // a list, say. Neither can be shown as a table; keep it verbatim.
        return vec![Entry {
            key: String::new(),
            value: String::new(),
            kind: Kind::Error,
            raw_yaml: Some(yaml.to_string()),
        }];
    };

    let blocks = raw_blocks(yaml);
    let mut entries = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    // Walk the *source text* so entries keep the author's ordering.
    for key in top_level_keys(yaml) {
        if seen.contains(&key) {
            continue;
        }
        let Some(value) = map.get(Value::String(key.clone())) else {
            continue;
        };
        seen.push(key.clone());
        let (value_text, kind) = classify(value);
        entries.push(Entry {
            key: key.clone(),
            value: value_text,
            kind,
            raw_yaml: if kind == Kind::Complex {
                blocks
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, block)| block.clone())
            } else {
                None
            },
        });
    }
    entries
}

/// Render entries back to a frontmatter block, or `None` when there is nothing
/// to write.
pub fn to_yaml(entries: &[Entry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    // A block that never parsed is written back exactly as it came in.
    if let Some(error) = entries.iter().find(|e| e.kind == Kind::Error) {
        return error.raw_yaml.clone();
    }
    let mut lines = Vec::new();
    for entry in entries {
        match entry.kind {
            Kind::Complex => match &entry.raw_yaml {
                Some(raw) => lines.push(raw.clone()),
                // The original block is gone (a complex value the user renamed,
                // say); fall back to the flat form rather than dropping the key.
                None => lines.push(format!("{}: {}", entry.key, entry.value)),
            },
            Kind::Array => {
                if entry.value.trim().is_empty() {
                    lines.push(format!("{}: []", entry.key));
                } else {
                    let items: Vec<&str> = entry.value.split(',').map(str::trim).collect();
                    lines.push(format!("{}: [{}]", entry.key, items.join(", ")));
                }
            }
            Kind::Scalar | Kind::Error => lines.push(format!("{}: {}", entry.key, entry.value)),
        }
    }
    Some(lines.join("\n"))
}

/// The entries as the JSON array `window.setFrontmatter` expects.
pub fn to_json(entries: &[Entry]) -> String {
    let array: Vec<serde_json::Value> = entries
        .iter()
        .map(|entry| {
            let mut object = serde_json::Map::new();
            object.insert("key".into(), entry.key.clone().into());
            object.insert("value".into(), entry.value.clone().into());
            object.insert("type".into(), entry.kind.as_str().into());
            if let Some(raw) = &entry.raw_yaml {
                object.insert("rawYaml".into(), raw.clone().into());
            }
            serde_json::Value::Object(object)
        })
        .collect();
    serde_json::to_string(&array).unwrap_or_else(|_| "[]".to_string())
}

/// Read back the entries the editor posted through `frontmatterChanged`.
pub fn from_json(json: &str) -> Vec<Entry> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let text = |name: &str| object.get(name).and_then(|v| v.as_str());
            Some(Entry {
                key: text("key")?.to_string(),
                value: text("value").unwrap_or_default().to_string(),
                kind: Kind::from_str(text("type").unwrap_or("scalar")),
                raw_yaml: text("rawYaml").map(str::to_string),
            })
        })
        .collect()
}

/// How a parsed value is shown, and how it has to round-trip.
fn classify(value: &Value) -> (String, Kind) {
    match value {
        Value::Sequence(items) => {
            if items.is_empty() {
                return (String::new(), Kind::Array);
            }
            if items
                .iter()
                .any(|i| matches!(i, Value::Mapping(_) | Value::Sequence(_)))
            {
                return (format_complex(value), Kind::Complex);
            }
            let joined: Vec<String> = items.iter().map(format_scalar).collect();
            (joined.join(", "), Kind::Array)
        }
        Value::Mapping(_) => (format_complex(value), Kind::Complex),
        // A multi-line string is a block scalar; its indicators can't survive
        // the flat `key: value` form.
        Value::String(s) if s.trim_matches('\n').contains('\n') => {
            (s.trim_matches('\n').to_string(), Kind::Complex)
        }
        other => (format_scalar(other), Kind::Scalar),
    }
}

fn format_scalar(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => format_complex(other),
    }
}

/// A one-line preview of a value the table can't hold. Only ever displayed —
/// the `raw_yaml` block is what gets written back.
fn format_complex(value: &Value) -> String {
    match value {
        Value::Mapping(map) => {
            let mut pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}: {}", format_scalar(k), format_scalar(v)))
                .collect();
            pairs.sort();
            pairs.join(", ")
        }
        Value::Sequence(items) => items
            .iter()
            .map(format_complex)
            .collect::<Vec<_>>()
            .join("; "),
        other => format_scalar(other),
    }
}

/// The top-level keys in source order. Comments, blank lines, list items and
/// indented continuations are skipped, so only real keys are returned.
fn top_level_keys(yaml: &str) -> Vec<String> {
    yaml.lines()
        .filter(|line| !line.starts_with(' ') && !line.starts_with('\t'))
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
                return None;
            }
            let key = trimmed.split_once(':')?.0.trim();
            (!key.is_empty()).then(|| key.to_string())
        })
        .collect()
}

/// Each top-level key paired with its full YAML block — the key's own line plus
/// every indented or blank line under it — so a value the table can't hold is
/// written back exactly as the author wrote it.
fn raw_blocks(yaml: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in yaml.lines() {
        let top_level = !line.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && !line.trim_start().starts_with('#');
        if top_level && line.contains(':') {
            if let Some((key, lines)) = current.take() {
                blocks.push((key, lines.join("\n")));
            }
            let key = line.split_once(':').map(|(k, _)| k.trim().to_string());
            if let Some(key) = key {
                current = Some((key, vec![line.to_string()]));
            }
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        }
    }
    if let Some((key, lines)) = current {
        blocks.push((key, lines.join("\n")));
    }
    // Trailing blank lines belong to the document, not the block.
    for (_, block) in blocks.iter_mut() {
        *block = block.trim_end().to_string();
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_keep_their_source_order() {
        // Alphabetical order would put `status` first; the author's doesn't.
        let entries = parse("title: Architecture\nstatus: draft\n");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].key, "title");
        assert_eq!(entries[0].value, "Architecture");
        assert_eq!(entries[0].kind, Kind::Scalar);
        assert_eq!(entries[1].key, "status");
        assert_eq!(entries[1].value, "draft");
    }

    #[test]
    fn numbers_and_booleans_render_as_written() {
        let entries = parse("count: 3\ndraft: true\nratio: 1.5\n");
        let values: Vec<&str> = entries.iter().map(|e| e.value.as_str()).collect();
        assert_eq!(values, ["3", "true", "1.5"]);
        assert!(entries.iter().all(|e| e.kind == Kind::Scalar));
    }

    #[test]
    fn a_flat_list_becomes_a_comma_separated_array() {
        let entries = parse("tags: [a, b, c]\n");
        assert_eq!(entries[0].kind, Kind::Array);
        assert_eq!(entries[0].value, "a, b, c");
        assert_eq!(to_yaml(&entries).unwrap(), "tags: [a, b, c]");
    }

    #[test]
    fn an_empty_list_round_trips_as_empty() {
        let entries = parse("tags: []\n");
        assert_eq!(entries[0].kind, Kind::Array);
        assert_eq!(entries[0].value, "");
        assert_eq!(to_yaml(&entries).unwrap(), "tags: []");
    }

    #[test]
    fn a_nested_map_keeps_its_original_block() {
        let yaml = "author:\n  name: Ada\n  email: ada@example.com\ntitle: Notes\n";
        let entries = parse(yaml);
        assert_eq!(entries[0].key, "author");
        assert_eq!(entries[0].kind, Kind::Complex);
        assert_eq!(
            entries[0].raw_yaml.as_deref(),
            Some("author:\n  name: Ada\n  email: ada@example.com")
        );
        // The block, not the flattened preview, is what gets written back.
        let out = to_yaml(&entries).unwrap();
        assert!(out.starts_with("author:\n  name: Ada\n  email: ada@example.com"));
        assert!(out.ends_with("title: Notes"));
    }

    #[test]
    fn a_multi_line_string_is_complex() {
        let yaml = "summary: |\n  line one\n  line two\n";
        let entries = parse(yaml);
        assert_eq!(entries[0].kind, Kind::Complex);
        assert!(entries[0].raw_yaml.as_deref().unwrap().contains("line two"));
    }

    #[test]
    fn unparseable_yaml_survives_untouched() {
        let broken = "title: [unclosed\n  : nonsense\n";
        let entries = parse(broken);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, Kind::Error);
        // Round-tripping a block we couldn't understand must not damage it.
        assert_eq!(to_yaml(&entries).unwrap(), broken);
    }

    #[test]
    fn yaml_that_is_not_a_mapping_is_kept_verbatim() {
        let list = "- one\n- two\n";
        let entries = parse(list);
        assert_eq!(entries[0].kind, Kind::Error);
        assert_eq!(to_yaml(&entries).unwrap(), list);
    }

    #[test]
    fn an_empty_block_has_no_entries_and_writes_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("   \n").is_empty());
        assert_eq!(to_yaml(&[]), None);
    }

    #[test]
    fn comments_are_not_mistaken_for_keys() {
        let entries = parse("# a note: not a key\ntitle: Real\n");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "title");
    }

    #[test]
    fn json_round_trips_through_the_bridge_shape() {
        let entries = parse("title: Architecture\ntags: [a, b]\nauthor:\n  name: Ada\n");
        let json = to_json(&entries);
        assert!(json.contains("\"type\":\"array\""));
        assert!(json.contains("\"rawYaml\""));
        assert_eq!(from_json(&json), entries);
    }

    #[test]
    fn an_edit_from_the_editor_is_written_back() {
        // What the block posts after the user retypes a value.
        let edited = r#"[{"key":"title","value":"Renamed","type":"scalar"}]"#;
        let entries = from_json(edited);
        assert_eq!(to_yaml(&entries).unwrap(), "title: Renamed");
    }

    #[test]
    fn malformed_bridge_json_yields_no_entries() {
        assert!(from_json("not json").is_empty());
        assert!(from_json("{}").is_empty());
        // An entry with no key can't be written back, so it is dropped.
        assert!(from_json(r#"[{"value":"x"}]"#).is_empty());
    }
}
