//! Round-trip suite for the YAML library: every shipped default Shelbi writes,
//! and user-shaped configs parsed and written back, against the exact bytes
//! recorded in `tests/fixtures/yaml_round_trip/golden/`.
//!
//! What it covers:
//!
//! - **Shipped defaults**: the default, task and subtask workflows and the
//!   statuses catalog as `shelbi init` scaffolds them, the user `config.yaml`,
//!   and every bundled agent's `agent.yaml`. Each is recorded as written, and
//!   parsed back into its type and into a `Value` and re-serialized.
//! - **User-shaped configs** (`inputs/`): global and hub project files (flow
//!   collections, comments, `null`s, quoted globs), a workflow with shell
//!   `run:` lines, custom statuses, the user and hub configs, an agent
//!   manifest, a workspace status, `keys.yaml`, task and question
//!   frontmatter, plus the repo's own `examples/` project and session. Each is parsed into
//!   its type (when it has one) and into a `Value`, then re-serialized. A
//!   project is also split into its shared and local halves and decorated as
//!   `project.yaml` is.
//! - **Comment-preserving edits**: `yaml_edit::patch` falls back to the
//!   serializer for any block it can't patch as text, so a few edits record
//!   its output.
//! - **Scalars and shapes**: strings that look like other types or need
//!   quoting, block scalars, numbers at the edges, enums (YAML tags), and
//!   empty and non-string-keyed collections, so a change in quoting, tag or
//!   number formatting shows up as a diff of one line.
//!
//! The goldens were recorded under `serde_yaml` 0.9.34 before Shelbi moved
//! to `yaml_serde`, so a pass means the replacement reads and writes these
//! files byte for byte as the old crate did. To re-record after a deliberate
//! change, run
//! `SHELBI_WRITE_YAML_GOLDEN=1 cargo test -p shelbi-state --test yaml_round_trip`
//! and review the fixture diff: every changed line is a change to a file
//! Shelbi writes into a user's project.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use yaml_serde::Value;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/yaml_round_trip");
const WRITE_ENV: &str = "SHELBI_WRITE_YAML_GOLDEN";

/// The recorded outputs, by golden file name.
#[derive(Default)]
struct Cases {
    outputs: BTreeMap<String, String>,
    /// Golden names whose bytes are pure serializer output, so parsing them
    /// and writing them again must give the same bytes.
    fixed_points: Vec<String>,
}

impl Cases {
    /// Record `text` under `name`, as-is.
    fn text(&mut self, name: impl Into<String>, text: String) {
        let name = name.into();
        assert!(self.outputs.insert(name.clone(), text).is_none(), "duplicate case {name}");
    }

    /// Record serializer output under `name`.
    fn yaml(&mut self, name: impl Into<String>, text: String) {
        let name = name.into();
        self.fixed_points.push(name.clone());
        self.text(name, text);
    }
}

fn to_yaml<T: Serialize + ?Sized>(value: &T) -> String {
    yaml_serde::to_string(value).expect("serializes")
}

fn parse<T: DeserializeOwned>(what: &str, text: &str) -> T {
    yaml_serde::from_str(text).unwrap_or_else(|e| panic!("{what} doesn't parse: {e}"))
}

/// `text` parsed into `T` and written back.
fn typed<T: DeserializeOwned + Serialize>(what: &str, text: &str) -> String {
    to_yaml(&parse::<T>(what, text))
}

/// A parse-and-write-back function for one type.
type Writer = fn(&str, &str) -> String;

/// `text` parsed into a `Value` and written back.
fn value(what: &str, text: &str) -> String {
    to_yaml(&parse::<Value>(what, text))
}

/// The YAML between a frontmatter file's `---` fences.
fn frontmatter(text: &str) -> &str {
    let rest = text.strip_prefix("---\n").expect("frontmatter opens with ---");
    let end = rest.find("\n---\n").expect("frontmatter closes with ---");
    &rest[..=end]
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn input(name: &str) -> String {
    read(&Path::new(FIXTURES).join("inputs").join(name))
}

fn shipped(cases: &mut Cases) {
    use shelbi_core::scaffold;

    let workflows = [
        ("default", scaffold::default_workflow_yaml().unwrap()),
        ("task", scaffold::task_workflow_yaml().unwrap()),
        ("subtask", scaffold::subtask_workflow_yaml().unwrap()),
    ];
    for (name, text) in workflows {
        cases.text(format!("shipped-workflow-{name}.yaml"), text.clone());
        cases.yaml(
            format!("shipped-workflow-{name}.typed.yaml"),
            typed::<shelbi_core::Workflow>(name, &text),
        );
        cases.yaml(format!("shipped-workflow-{name}.value.yaml"), value(name, &text));
    }
    cases.yaml(
        "shipped-workflow-new.yaml",
        to_yaml(&shelbi_core::Workflow {
            name: "design".into(),
            ..shelbi_core::default_workflow()
        }),
    );

    let statuses = scaffold::default_statuses_yaml().unwrap();
    cases.text("shipped-statuses.yaml", statuses.clone());
    cases.yaml(
        "shipped-statuses.typed.yaml",
        typed::<shelbi_core::ProjectStatuses>("statuses", &statuses),
    );
    cases.yaml("shipped-statuses.value.yaml", value("statuses", &statuses));

    cases.yaml(
        "shipped-config.typed.yaml",
        typed::<shelbi_state::UserConfig>("config.yaml", scaffold::CONFIG_YAML),
    );
    cases.yaml("shipped-config.value.yaml", value("config.yaml", scaffold::CONFIG_YAML));

    for agent in shelbi_state::DEFAULT_AGENTS {
        let text = shelbi_state::bundled_agent_manifest_yaml(agent);
        let name = agent.name;
        cases.text(format!("shipped-agent-{name}.yaml"), text.clone());
        cases.yaml(
            format!("shipped-agent-{name}.typed.yaml"),
            typed::<shelbi_core::agent_manifest::AgentManifest>(name, &text),
        );
        cases.yaml(format!("shipped-agent-{name}.value.yaml"), value(name, &text));
    }
}

/// A project file: as its type, as a `Value`, split into its shared and
/// local halves, and decorated the way a new `project.yaml` is.
fn project(cases: &mut Cases, stem: &str, text: &str) {
    let project: shelbi_core::Project = parse(stem, text);
    let active = to_yaml(&project);
    cases.yaml(format!("{stem}.typed.yaml"), active.clone());
    cases.yaml(format!("{stem}.value.yaml"), value(stem, text));
    cases.yaml(format!("{stem}.shared.yaml"), project.to_shared_yaml_string().unwrap());
    cases.yaml(format!("{stem}.local.yaml"), project.to_local_yaml_string().unwrap());
    cases.text(
        format!("{stem}.decorated.yaml"),
        shelbi_core::scaffold::decorate_project_yaml(&active),
    );
}

fn user_shaped(cases: &mut Cases) {
    for stem in ["project-global", "project-hub"] {
        project(cases, stem, &input(&format!("{stem}.yaml")));
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    project(cases, "example-myapp", &read(&examples.join("myapp.yaml")));
    let session = read(&examples.join("daily.yaml"));
    cases.yaml("example-daily.typed.yaml", typed::<shelbi_core::Session>("daily", &session));
    cases.yaml("example-daily.value.yaml", value("daily", &session));

    let typed_inputs: [(&str, Writer); 6] = [
        ("workflow-app", typed::<shelbi_core::Workflow>),
        ("statuses-custom", typed::<shelbi_core::ProjectStatuses>),
        ("config-user", typed::<shelbi_state::UserConfig>),
        ("hub-config", typed::<shelbi_state::HubConfig>),
        ("agent-developer", typed::<shelbi_core::agent_manifest::AgentManifest>),
        ("workspace-status", typed::<shelbi_state::WorkspaceStatus>),
    ];
    for (stem, typed) in typed_inputs {
        let text = input(&format!("{stem}.yaml"));
        cases.yaml(format!("{stem}.typed.yaml"), typed(stem, &text));
        cases.yaml(format!("{stem}.value.yaml"), value(stem, &text));
    }
    for stem in ["keys", "value-edge-cases"] {
        let text = input(&format!("{stem}.yaml"));
        cases.yaml(format!("{stem}.value.yaml"), value(stem, &text));
    }

    for stem in ["task-simple", "task-depends"] {
        let text = input(&format!("{stem}.md"));
        let file = shelbi_state::parse_task_file(&text).unwrap();
        cases.text(format!("{stem}.md"), shelbi_state::render_task_file(&file).unwrap());
        cases.yaml(format!("{stem}.value.yaml"), value(stem, frontmatter(&text)));
    }
    let question = input("question.md");
    cases.yaml(
        "question.typed.yaml",
        typed::<shelbi_state::questions::QuestionMeta>("question", frontmatter(&question)),
    );
    cases.yaml("question.value.yaml", value("question", frontmatter(&question)));
}

fn set(value: &mut Value, path: &[&str], new: Value) {
    let (last, parents) = path.split_last().unwrap();
    let mut at = value;
    for key in parents {
        at = at.get_mut(*key).unwrap_or_else(|| panic!("no {key}"));
    }
    at.as_mapping_mut().unwrap().insert(Value::from(*last), new);
}

fn patched(cases: &mut Cases, name: &str, text: &str, edit: impl FnOnce(&mut Value)) {
    let mut target = shelbi_state::yaml_edit::parse(text).unwrap();
    edit(&mut target);
    let out = shelbi_state::yaml_edit::patch(text, &target).unwrap();
    assert_eq!(parse::<Value>(name, &out.text), target, "{name}: patch must mean the target");
    cases.text(format!("patch-{name}.yaml"), out.text);
}

fn edits(cases: &mut Cases) {
    let hub = input("project-hub.yaml");
    patched(cases, "project-hub-scalars", &hub, |v| {
        set(v, &["heartbeat"], Value::from("5m"));
        set(v, &["editor"], Value::from("zed"));
        set(v, &["git", "merge_strategy"], Value::from("rebase"));
    });
    patched(cases, "project-hub-new-blocks", &hub, |v| {
        let machine: Value = parse(
            "machine",
            "name: gpu\nkind: ssh\nhost: 'gpu.local'\nwork_dir: /srv/widget\ntags: [linux, cuda]\n",
        );
        v["machines"].as_sequence_mut().unwrap().push(machine);
        set(
            v,
            &["zen", "checks", "local"],
            parse("checks", "- cargo test --workspace\n- 'echo \"done: ok\"'\n"),
        );
        set(v, &["review"], parse("review", "serve:\n  command: npm run dev -- --port {{port}}\n"));
    });
    let global = input("project-global.yaml");
    patched(cases, "project-global-flow", &global, |v| {
        let ws: Value = parse("workspace", "{name: medal, machine: hub, tags: [review]}");
        v["workspaces"].as_sequence_mut().unwrap().push(ws);
        set(v, &["editor"], Value::Null);
    });
    let workflow = input("workflow-app.yaml");
    patched(cases, "workflow-app", &workflow, |v| {
        let edge: Value = parse("edge", "from: todo\nto: in-progress\nactions: [push_branch]\n");
        v["transitions"].as_sequence_mut().unwrap().push(edge);
        set(v, &["description"], Value::from("Rust app track: build, test; squash-merge."));
    });
}

/// Strings that need (or look like they need) quoting, block scalars, and
/// the characters a YAML emitter escapes.
const SCALARS: &[&str] = &[
    "plain", "yes", "no", "on", "off", "y", "n", "true", "False", "null", "Null", "~", "", " ",
    "1", "-1", "1.0", "1e3", "0x1F", "0o17", "010", "+1", "1_000", ".inf", ".nan", "2026-11-09",
    "2026-11-09T13:42:11Z", "1.2.3", "a: b", "a:b", "a #b", "a#b", "- x", "-", "--", "---", "...",
    "*glob", "&anchor", "!tag", "|", ">", "%x", "@scope", "`tick`", "{a}", "[a]", "a, b", ",",
    "?", "? x", "it's", "say \"hi\"", "C:\\path", "{{github_user}}/{{id}}",
    "https://example.com/a?b=c#d", "[ -d t ] && rm -rf t; true", "  lead", "trail  ", "a\tb",
    "one\ntwo", "ends\n", "\nstarts", "a\r\nb", "two\n\nparas\n", "keep\n\n", "café",
    "日本語", "🚀 ship", "em — dash", "a\u{a0}b", "\u{feff}x", "bell\u{7}", "nul\u{0}",
    "this is a deliberately long line of plain text that runs well past eighty columns so a folding emitter would wrap it",
];

fn scalars(cases: &mut Cases) {
    let mut table = String::new();
    for s in SCALARS {
        let top = to_yaml(s);
        let nested = to_yaml(&BTreeMap::from([("k", *s)]));
        let listed = to_yaml(&[*s]);
        assert_eq!(parse::<String>("scalar", &top), *s, "top-level {s:?} must read back");
        assert_eq!(
            parse::<BTreeMap<String, String>>("scalar", &nested)["k"],
            *s,
            "mapped {s:?} must read back"
        );
        table.push_str(&format!("#### {s:?}\n{top}#\n{nested}#\n{listed}"));
    }
    cases.text("scalars.txt", table);
}

#[derive(Serialize)]
#[allow(dead_code)]
enum Variant {
    Unit,
    Newtype(String),
    Tuple(u8, String),
    Struct { a: u8, b: Option<String> },
}

#[derive(Serialize)]
struct Shapes {
    u64_max: u64,
    i64_min: i64,
    i128: i128,
    floats: Vec<f64>,
    f32: f32,
    bools: (bool, bool),
    none: Option<String>,
    some: Option<String>,
    unit: (),
    chr: char,
    variants: Vec<Variant>,
    variant_field: Variant,
    int_keys: BTreeMap<i32, String>,
    bool_keys: BTreeMap<bool, String>,
    empty_seq: Vec<String>,
    empty_map: BTreeMap<String, String>,
    nested: Vec<Vec<BTreeMap<String, Vec<u8>>>>,
    tuple: (String, u8, Option<u8>),
    bytes: Vec<u8>,
}

fn shapes(cases: &mut Cases) {
    let shapes = Shapes {
        u64_max: u64::MAX,
        i64_min: i64::MIN,
        i128: -170_141_183_460_469_231_731_687_303_715_884_105_728,
        floats: vec![0.0, -0.0, 0.5, 1.0, 1e20, 1e-7, 123456.789, f64::MAX, f64::MIN_POSITIVE, f64::NAN, f64::INFINITY, f64::NEG_INFINITY],
        f32: 0.1,
        bools: (true, false),
        none: None,
        some: Some("x".into()),
        unit: (),
        chr: '#',
        variants: vec![
            Variant::Unit,
            Variant::Newtype("n".into()),
            Variant::Tuple(1, "t".into()),
            Variant::Struct { a: 2, b: None },
        ],
        variant_field: Variant::Struct { a: 3, b: Some("yes".into()) },
        int_keys: BTreeMap::from([(-1, "minus".into()), (2, "two".into())]),
        bool_keys: BTreeMap::from([(false, "f".into()), (true, "t".into())]),
        empty_seq: vec![],
        empty_map: BTreeMap::new(),
        nested: vec![vec![], vec![BTreeMap::from([("k".into(), vec![1, 2])]), BTreeMap::new()]],
        tuple: ("a".into(), 1, None),
        bytes: b"hi".to_vec(),
    };
    cases.text("shapes.yaml", to_yaml(&shapes));
    // A bare unit variant at the top level, and a top-level sequence.
    cases.yaml("shapes-top-level-unit.yaml", to_yaml(&Variant::Unit));
    cases.yaml("shapes-top-level-seq.yaml", to_yaml(&["a", "b"]));
}

fn all_cases() -> Cases {
    let mut cases = Cases::default();
    shipped(&mut cases);
    user_shaped(&mut cases);
    edits(&mut cases);
    scalars(&mut cases);
    shapes(&mut cases);
    cases
}

fn golden_dir() -> PathBuf {
    Path::new(FIXTURES).join("golden")
}

#[test]
fn yaml_output_matches_goldens() {
    let cases = all_cases();
    let dir = golden_dir();
    let on_disk: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();

    if std::env::var_os(WRITE_ENV).is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        for name in on_disk.iter().filter(|n| !cases.outputs.contains_key(*n)) {
            std::fs::remove_file(dir.join(name)).unwrap();
        }
        for (name, text) in &cases.outputs {
            std::fs::write(dir.join(name), text).unwrap();
        }
        return;
    }

    let mut problems = Vec::new();
    for (name, text) in &cases.outputs {
        match std::fs::read_to_string(dir.join(name)) {
            Ok(golden) if golden == *text => {}
            Ok(golden) => problems.push(format!(
                "{name} differs\n--- golden\n{golden}\n--- now\n{text}"
            )),
            Err(e) => problems.push(format!("{name} has no golden ({e})")),
        }
    }
    for name in on_disk.iter().filter(|n| !cases.outputs.contains_key(*n)) {
        problems.push(format!("{name} is a golden with no case"));
    }
    assert!(
        problems.is_empty(),
        "{} YAML golden(s) don't match; if the change is deliberate, re-record with \
         {WRITE_ENV}=1 and review the diff:\n\n{}",
        problems.len(),
        problems.join("\n\n")
    );
}

/// Serializer output reads back to the same tree and writes the same bytes
/// again, so a file Shelbi writes doesn't change when Shelbi rewrites it.
#[test]
fn serializer_output_is_a_fixed_point() {
    let cases = all_cases();
    for name in &cases.fixed_points {
        let text = &cases.outputs[name];
        assert_eq!(value(name, text), *text, "{name}: rewriting changed the bytes");
    }
}
