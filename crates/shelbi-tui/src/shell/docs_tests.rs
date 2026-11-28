//! The Documentation page against a test docs: an in-memory hub that keeps its
//! documents and folders and answers the page's requests as the hub's docs
//! service does.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use shelbi_app::docs::{FailureKind, NodeKind, SyncState, DocsConflict, DocsNode, DocsReply, DocsRequest, DocsStatus};

use super::*;

/// The test docs: documents by path, folders, and what was asked.
#[derive(Default)]
pub(crate) struct FakeDocs {
    pub docs: BTreeMap<String, String>,
    pub folders: Vec<String>,
    pub asked: Vec<DocsRequest>,
    pub set_up: bool,
    /// Held documents' sides (`ours` is `docs`' text).
    pub conflicts: BTreeMap<String, DocsConflict>,
    /// The docs' root folder the status names.
    pub root: String,
}

impl FakeDocs {
    pub(crate) fn sample() -> Self {
        let mut docs = BTreeMap::new();
        docs.insert("README.md".into(), "# Shelbi docs\n\nStart here.\n".into());
        docs.insert("Plans/awareness.md".into(), "# Awareness v2\n\nBody.\n".into());
        docs.insert(
            "Plans/opencode.md".into(),
            "# OpenCode integration\n\n## Context\n\nOpenCode is a terminal coding agent.\n".into(),
        );
        docs.insert("Plans/Archive/old.md".into(), "# Old plan\n".into());
        FakeDocs {
            docs,
            folders: vec!["Plans".into(), "Plans/Archive".into(), "Research".into()],
            asked: Vec::new(),
            set_up: true,
            conflicts: BTreeMap::new(),
            root: "/tmp/docs".into(),
        }
    }

    /// Hold `path`: `ours` here, `theirs` from elsewhere.
    pub(crate) fn hold(&mut self, path: &str, ours: &str, theirs: &str) {
        self.docs.insert(path.into(), ours.into());
        self.conflicts.insert(
            path.into(),
            DocsConflict {
                path: path.into(),
                base: None,
                ours: Some(ours.into()),
                theirs: Some(theirs.into()),
                detected_at: "2026-11-27T00:00:00Z".into(),
            },
        );
    }

    fn status(&self) -> DocsStatus {
        if !self.set_up {
            return DocsStatus::not_set_up();
        }
        DocsStatus {
            set_up: true,
            state: if self.conflicts.is_empty() { SyncState::Synced } else { SyncState::Conflict },
            conflicts: self.conflicts.keys().cloned().collect(),
            paused: false,
            auto: true,
            source: "branch".into(),
            branch: "docs/planning".into(),
            root: self.root.clone(),
            last_synced: None,
            pull_request: None,
        }
    }

    fn title_of(&self, path: &str) -> String {
        let text = self.docs.get(path).cloned().unwrap_or_default();
        text.lines()
            .find_map(|l| l.strip_prefix("# ").map(str::to_string))
            .unwrap_or_else(|| path.rsplit('/').next().unwrap().trim_end_matches(".md").to_string())
    }

    /// The tree under `folder` (`""` the top), built from JSON so a field
    /// the hub adds later keeps its default here.
    fn nodes(&self, folder: &str) -> Vec<DocsNode> {
        let inside = |p: &str| match folder {
            "" => !p.contains('/'),
            f => p.strip_prefix(&format!("{f}/")).is_some_and(|rest| !rest.contains('/')),
        };
        let mut out = Vec::new();
        for f in self.folders.iter().filter(|f| inside(f)) {
            let name = f.rsplit('/').next().unwrap();
            out.push(node(f, name, name, NodeKind::Folder, self.nodes(f)));
        }
        for d in self.docs.keys().filter(|d| inside(d)) {
            out.push(node(d, d.rsplit('/').next().unwrap(), &self.title_of(d), NodeKind::Document, Vec::new()));
        }
        out
    }

    fn answer(&mut self, req: DocsRequest) -> DocsReply {
        self.asked.push(req.clone());
        let missing = |p: &str| DocsReply::failed(FailureKind::NotFound, format!("no `{p}`"));
        match req {
            DocsRequest::Init => {
                self.set_up = true;
                DocsReply::Status(self.status())
            }
            DocsRequest::Status => DocsReply::Status(self.status()),
            _ if !self.set_up => DocsReply::failed(FailureKind::NotSetUp, "no docs"),
            DocsRequest::Sync(_) => DocsReply::Status(self.status()),
            DocsRequest::Tree { .. } => DocsReply::Tree(self.nodes("")),
            DocsRequest::Read { path, .. } => match self.docs.get(&path) {
                Some(content) => DocsReply::Doc { path, content: content.clone() },
                None => missing(&path),
            },
            DocsRequest::Write { path, content, .. } => {
                self.docs.insert(path, content);
                DocsReply::Done
            }
            DocsRequest::New { path, .. } if path.ends_with(".md") => {
                let title = path.rsplit('/').next().unwrap().trim_end_matches(".md").to_string();
                self.docs.insert(path, format!("# {title}\n"));
                DocsReply::Done
            }
            DocsRequest::New { path, .. } => {
                self.folders.push(path);
                DocsReply::Done
            }
            DocsRequest::Move { from, to, .. } => {
                if let Some(text) = self.docs.remove(&from) {
                    self.docs.insert(to, text);
                } else if let Some(i) = self.folders.iter().position(|f| f == &from) {
                    self.folders[i] = to.clone();
                    let moved: Vec<String> = self.docs.keys().filter(|d| d.starts_with(&format!("{from}/"))).cloned().collect();
                    for d in moved {
                        let text = self.docs.remove(&d).unwrap();
                        self.docs.insert(format!("{to}{}", &d[from.len()..]), text);
                    }
                } else {
                    return missing(&from);
                }
                DocsReply::Done
            }
            DocsRequest::Conflict { path } => match self.conflicts.get(&path) {
                Some(c) => {
                    let mut c = c.clone();
                    c.ours = self.docs.get(&path).cloned();
                    DocsReply::Conflict(c)
                }
                None => DocsReply::failed(FailureKind::Conflict, format!("`{path}` isn't in conflict")),
            },
            DocsRequest::Resolve { path, resolution, .. } => {
                let Some(mut held) = self.conflicts.get(&path).cloned() else {
                    return DocsReply::failed(FailureKind::Conflict, format!("`{path}` isn't in conflict"));
                };
                held.ours = self.docs.get(&path).cloned();
                match shelbi_app::docs_conflict::resolve(&held, &resolution) {
                    Ok(content) => {
                        self.conflicts.remove(&path);
                        match content {
                            Some(c) => self.docs.insert(path, c),
                            None => self.docs.remove(&path),
                        };
                        DocsReply::Done
                    }
                    Err(e) => DocsReply::failed(FailureKind::Conflict, e),
                }
            }
            DocsRequest::Remove { path, .. } => {
                self.docs.retain(|d, _| d != &path && !d.starts_with(&format!("{path}/")));
                self.folders.retain(|f| f != &path && !f.starts_with(&format!("{path}/")));
                DocsReply::Done
            }
            other => DocsReply::failed(FailureKind::Failed, format!("the fake doesn't do {other:?}")),
        }
    }
}

fn node(path: &str, name: &str, title: &str, kind: NodeKind, children: Vec<DocsNode>) -> DocsNode {
    let kind = match kind {
        NodeKind::Folder => "Folder",
        NodeKind::Document => "Document",
    };
    serde_json::from_value(serde_json::json!({
        "path": path, "name": name, "title": title, "kind": kind,
        "children": serde_json::to_value(children).unwrap(),
    }))
    .expect("docs node")
}

/// An `Ask` over `docs`.
pub(crate) fn ask_of(docs: &Arc<Mutex<FakeDocs>>) -> Ask {
    let docs = docs.clone();
    Arc::new(move |req| docs.lock().unwrap().answer(req))
}

/// Fold in every answer the page waits on.
pub(crate) fn settle(page: &mut DocsPage) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        page.poll();
        if !page.busy() {
            return;
        }
        assert!(Instant::now() < deadline, "the docs' work never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn typed(page: &mut DocsPage, text: &str) {
    for c in text.chars() {
        page.handle_tree_key(ch(c));
    }
}

fn opened() -> (Arc<Mutex<FakeDocs>>, DocsPage) {
    let docs = Arc::new(Mutex::new(FakeDocs::sample()));
    let mut page = DocsPage::new("proj", ask_of(&docs));
    page.open();
    settle(&mut page);
    (docs, page)
}

fn titles(page: &DocsPage) -> Vec<String> {
    page.tree.rows().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.title)).collect()
}

fn asked(docs: &Arc<Mutex<FakeDocs>>) -> Vec<DocsRequest> {
    docs.lock().unwrap().asked.clone()
}

#[test]
fn opening_reads_the_tree_with_titles_and_tells_the_hub_it_shows() {
    let (docs, page) = opened();
    assert!(page.tree_open);
    assert_eq!(titles(&page), ["Plans", "Research", "Shelbi docs"]);
    let asked = asked(&docs);
    assert!(asked.contains(&DocsRequest::Status));
    assert!(asked.contains(&DocsRequest::Tree { path: None }));
    // The active ping is fire-and-forget; give it a moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !docs.lock().unwrap().asked.contains(&DocsRequest::Sync(SyncOp::Active)) {
        assert!(Instant::now() < deadline, "no Active ping");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_tree_keys_move_open_and_close() {
    let (_, mut page) = opened();
    page.handle_tree_key(key(KeyCode::Right));
    assert_eq!(titles(&page), ["Plans", "  Archive", "  Awareness v2", "  OpenCode integration", "Research", "Shelbi docs"]);
    page.handle_tree_key(ch('l'));
    assert_eq!(page.tree.selected_path(), Some("Plans/Archive"));
    page.handle_tree_key(ch('j'));
    page.handle_tree_key(key(KeyCode::Down));
    assert_eq!(page.tree.selected_path(), Some("Plans/opencode.md"));
    page.handle_tree_key(ch('k'));
    assert_eq!(page.tree.selected_path(), Some("Plans/awareness.md"));
    page.handle_tree_key(ch('h'));
    assert_eq!(page.tree.selected_path(), Some("Plans"));
    page.handle_tree_key(key(KeyCode::Left));
    assert_eq!(titles(&page), ["Plans", "Research", "Shelbi docs"]);
    // Enter on a folder toggles it.
    page.handle_tree_key(key(KeyCode::Enter));
    assert!(page.tree.is_expanded("Plans"));
    assert_eq!(page.doc_path(), None);
}

#[test]
fn enter_opens_a_document_and_hands_it_the_keys() {
    let (_, mut page) = opened();
    page.tree.select_path("Plans/opencode.md");
    assert_eq!(page.handle_tree_key(key(KeyCode::Enter)), DocsStep::FocusDoc);
    settle(&mut page);
    assert_eq!(page.doc_path(), Some("Plans/opencode.md"));
    assert!(page.doc_content().unwrap().contains("terminal coding agent"));
    // The document scrolls; Esc goes back to the tree.
    page.handle_doc_key(ch('j'));
    assert_eq!(page.doc_scroll(), 1);
    page.handle_doc_key(ch('g'));
    assert_eq!(page.doc_scroll(), 0);
    assert_eq!(page.handle_doc_key(key(KeyCode::Esc)), DocsStep::FocusTree);
}

#[test]
fn n_and_capital_n_make_a_document_and_a_folder_in_the_selected_folder() {
    let (docs, mut page) = opened();
    page.tree.select_path("Plans/awareness.md");
    page.handle_tree_key(ch('n'));
    assert!(page.typing());
    typed(&mut page, "roadmap");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(asked(&docs).iter().any(|r| matches!(r, DocsRequest::New { path, .. } if path == "Plans/roadmap.md")));
    // The new document is selected and open.
    assert_eq!(page.tree.selected_path(), Some("Plans/roadmap.md"));
    settle(&mut page);
    assert_eq!(page.doc_path(), Some("Plans/roadmap.md"));

    page.tree.select_path("Research");
    page.handle_tree_key(ch('N'));
    typed(&mut page, "Notes");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(docs.lock().unwrap().folders.contains(&"Research/Notes".to_string()));
    assert_eq!(page.tree.selected_path(), Some("Research/Notes"));

    // Esc drops a prompt without asking anything.
    let before = asked(&docs).len();
    page.handle_tree_key(ch('n'));
    typed(&mut page, "x");
    page.handle_tree_key(key(KeyCode::Esc));
    assert!(!page.typing());
    settle(&mut page);
    assert_eq!(asked(&docs).len(), before);
}

#[test]
fn r_renames_and_m_moves_and_the_open_document_follows() {
    let (docs, mut page) = opened();
    page.tree.select_path("Plans/awareness.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    page.handle_tree_key(ch('r'));
    // The draft is the file name, without `.md`; Ctrl+U clears it.
    page.handle_tree_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    typed(&mut page, "awareness-v2");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(asked(&docs).contains(&DocsRequest::Move {
        from: "Plans/awareness.md".into(),
        to: "Plans/awareness-v2.md".into(),
        by: Author::Person,
    }));
    assert_eq!(page.tree.selected_path(), Some("Plans/awareness-v2.md"));
    assert_eq!(page.doc_path(), Some("Plans/awareness-v2.md"));

    page.handle_tree_key(ch('m'));
    for _ in 0.."Plans".len() {
        page.handle_tree_key(key(KeyCode::Backspace));
    }
    typed(&mut page, "Research");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(docs.lock().unwrap().docs.contains_key("Research/awareness-v2.md"));
    assert_eq!(page.tree.selected_path(), Some("Research/awareness-v2.md"));
    assert_eq!(page.doc_path(), Some("Research/awareness-v2.md"));

    // A name with a slash isn't a rename.
    page.handle_tree_key(ch('r'));
    typed(&mut page, "/x");
    page.handle_tree_key(key(KeyCode::Enter));
    assert!(page.note().unwrap().contains("not a path"));
}

#[test]
fn d_deletes_after_a_y_and_closes_the_open_document() {
    let (docs, mut page) = opened();
    page.tree.select_path("README.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    page.handle_tree_key(ch('d'));
    page.handle_tree_key(ch('n'));
    settle(&mut page);
    assert!(docs.lock().unwrap().docs.contains_key("README.md"), "anything but y keeps it");
    page.handle_tree_key(ch('d'));
    page.handle_tree_key(ch('y'));
    settle(&mut page);
    assert!(!docs.lock().unwrap().docs.contains_key("README.md"));
    assert_eq!(page.doc_path(), None);
    assert_eq!(titles(&page), ["Plans", "Research"]);
}

#[test]
fn slash_filters_as_you_type_and_esc_clears_it() {
    let (_, mut page) = opened();
    page.handle_tree_key(ch('/'));
    typed(&mut page, "open");
    assert_eq!(titles(&page), ["Plans", "  OpenCode integration"]);
    // Enter keeps the filter and gives the keys back to the tree.
    page.handle_tree_key(key(KeyCode::Enter));
    assert!(!page.typing());
    page.handle_tree_key(ch('j'));
    assert_eq!(page.tree.selected_path(), Some("Plans/opencode.md"));
    // Esc clears the filter first, then goes back to the nav.
    assert_eq!(page.handle_tree_key(key(KeyCode::Esc)), DocsStep::Stay);
    assert_eq!(titles(&page).len(), 6, "Plans stays open where the filter opened it");
    assert_eq!(page.handle_tree_key(key(KeyCode::Esc)), DocsStep::Back);
}

#[test]
fn e_edits_a_copy_and_the_save_goes_through_the_hub() {
    let (docs, mut page) = opened();
    page.tree.select_path("Plans/opencode.md");
    // From the tree, before the document was ever read.
    page.handle_tree_key(ch('e'));
    settle(&mut page);
    let edit = page.take_edit().expect("an edit for the editor");
    assert_eq!(edit.path, "Plans/opencode.md");
    assert_eq!(std::fs::read_to_string(&edit.file).unwrap(), edit.original);
    // The editor changes it.
    std::fs::write(&edit.file, "# OpenCode integration\n\nRewritten.\n").unwrap();
    let file = edit.file.clone();
    page.finish_edit(edit, Ok(()));
    settle(&mut page);
    assert!(asked(&docs).iter().any(|r| matches!(r, DocsRequest::Write { .. })), "{:?}", page.note());
    assert!(asked(&docs).contains(&DocsRequest::Write {
        path: "Plans/opencode.md".into(),
        content: "# OpenCode integration\n\nRewritten.\n".into(),
        section: None,
        by: Author::Person,
    }));
    assert_eq!(page.note(), Some("Saved Plans/opencode.md"));
    assert_eq!(page.doc_content(), Some("# OpenCode integration\n\nRewritten.\n"));
    assert!(!file.exists(), "the copy is cleaned up");
}

#[test]
fn an_unchanged_edit_saves_nothing_and_a_document_changed_elsewhere_keeps_the_copy() {
    let (docs, mut page) = opened();
    page.tree.select_path("README.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    page.handle_doc_key(ch('e'));
    let edit = page.take_edit().expect("an edit");
    page.finish_edit(edit, Ok(()));
    assert_eq!(page.note(), Some("No changes to README.md"));

    page.handle_doc_key(ch('e'));
    let edit = page.take_edit().expect("an edit");
    std::fs::write(&edit.file, "# Mine\n").unwrap();
    // An agent writes it meanwhile.
    docs.lock().unwrap().docs.insert("README.md".into(), "# Theirs\n".into());
    let file = edit.file.clone();
    page.finish_edit(edit, Ok(()));
    settle(&mut page);
    assert!(page.note().unwrap().contains("changed elsewhere"), "{:?}", page.note());
    assert_eq!(docs.lock().unwrap().docs["README.md"], "# Theirs\n");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# Mine\n", "the copy is kept");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());

    // An editor that failed saves nothing either.
    page.handle_doc_key(ch('e'));
    let edit = page.take_edit().expect("an edit");
    let file = edit.file.clone();
    page.finish_edit(edit, Err("vi exited with 1".into()));
    assert!(page.note().unwrap().starts_with("vi exited with 1; nothing was saved"));
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_docs_change_reads_the_tree_and_the_open_document_again() {
    let (docs, mut page) = opened();
    page.tree.select_path("README.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    {
        let mut w = docs.lock().unwrap();
        w.docs.insert("README.md".into(), "# Shelbi docs\n\nEdited by an agent.\n".into());
        w.docs.insert("Added.md".into(), "# Added\n".into());
    }
    // Some other document: the tree, not the open one.
    page.changed(&["Added.md".to_string()]);
    settle(&mut page);
    assert!(titles(&page).contains(&"Added".to_string()));
    assert!(!page.doc_content().unwrap().contains("agent"));
    page.changed(&["README.md".to_string()]);
    settle(&mut page);
    assert!(page.doc_content().unwrap().contains("Edited by an agent."));
}

#[test]
fn a_project_without_a_docs_offers_to_set_one_up() {
    let docs = Arc::new(Mutex::new(FakeDocs::sample()));
    docs.lock().unwrap().set_up = false;
    let mut page = DocsPage::new("proj", ask_of(&docs));
    page.open();
    settle(&mut page);
    assert_eq!(page.status_line(chrono::Utc::now()).to_string(), "○ Not set up");
    page.handle_tree_key(ch('i'));
    settle(&mut page);
    assert!(asked(&docs).contains(&DocsRequest::Init));
    assert_eq!(titles(&page), ["Plans", "Research", "Shelbi docs"]);
}

#[test]
fn the_status_line_says_the_state_and_how_long_ago() {
    let (docs, mut page) = opened();
    assert_eq!(page.status_line(chrono::Utc::now()).to_string(), "● Synced");
    let now = chrono::Utc::now();
    let mut status = docs.lock().unwrap().status();
    status.last_synced = Some((now - chrono::Duration::minutes(2)).to_rfc3339());
    page.status = Some(status.clone());
    assert_eq!(page.status_line(now).to_string(), "● Synced · 2m ago");
    status.state = SyncState::Conflict;
    status.conflicts = vec!["a.md".into()];
    page.status = Some(status.clone());
    assert_eq!(page.status_line(now).to_string(), "● Conflict (1) · 2m ago");
    status.state = SyncState::Error("no network".into());
    status.last_synced = None;
    page.status = Some(status);
    assert_eq!(page.status_line(now).to_string(), "● Error: no network");
}

#[test]
fn the_tree_and_document_draw_and_a_click_opens_a_row() {
    let (_, mut page) = opened();
    page.tree.select_path("Plans/opencode.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
    term.draw(|f| {
        page.render_tree(f, Rect::new(0, 0, 28, 20), true);
        page.render_doc(f.buffer_mut(), Rect::new(28, 0, 52, 20));
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    let row = |y: u16| (0..80).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
    let screen: Vec<String> = (0..20).map(row).collect();
    let all = screen.join("\n");
    assert!(all.contains("DOCS"), "{all}");
    assert!(all.contains("/ Filter"));
    assert!(all.contains("▾ Plans"));
    assert!(all.contains("    OpenCode integration"));
    assert!(all.contains("● Synced"));
    assert!(all.contains("Plans › OpenCode integration"));
    assert!(all.contains("OpenCode is a terminal coding agent."));
    // A click on the README row opens it.
    let y = screen.iter().position(|l| l.contains("Shelbi docs") && !l.contains("›")).unwrap() as u16;
    assert_eq!(page.click_tree(4, y), DocsStep::Stay);
    settle(&mut page);
    assert_eq!(page.doc_path(), Some("README.md"));
    // The back button goes back to the nav.
    assert_eq!(page.click_tree(3, 1), DocsStep::Back);
}

const OURS: &str = "# Auth\n\n## Alpha\n\nAlpha ours.\n\n## Beta\n\nBeta ours.\n";
const THEIRS: &str = "# Auth\n\n## Alpha\n\nAlpha theirs.\n\n## Beta\n\nBeta theirs.\n";

fn held() -> (Arc<Mutex<FakeDocs>>, DocsPage) {
    let docs = Arc::new(Mutex::new(FakeDocs::sample()));
    docs.lock().unwrap().hold("Plans/opencode.md", OURS, THEIRS);
    let mut page = DocsPage::new("proj", ask_of(&docs));
    page.open();
    settle(&mut page);
    (docs, page)
}

fn drawn(page: &mut DocsPage, width: u16, height: u16, doc: bool) -> String {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    term.draw(|f| {
        let area = f.area();
        if doc {
            page.render_doc(f.buffer_mut(), area);
        } else {
            page.render_tree(f, area, true);
        }
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n")
        .collect()
}

#[test]
fn a_held_document_shows_a_notice_in_the_tree_and_on_the_document() {
    let (_, mut page) = held();
    let tree = drawn(&mut page, 60, 20, false);
    assert!(tree.contains("1 document couldn't be merged · c resolves"), "{tree}");
    assert!(tree.contains("Conflict (1)"), "{tree}");
    page.reveal("Plans/opencode.md");
    settle(&mut page);
    let doc = drawn(&mut page, 200, 12, true);
    assert!(doc.contains("This document couldn't be merged"), "{doc}");
    page.reveal("README.md");
    settle(&mut page);
    assert!(!drawn(&mut page, 200, 12, true).contains("couldn't be merged"));
}

#[test]
fn the_inspector_resolves_per_hunk_and_the_document_reloads() {
    let (docs, mut page) = held();
    page.reveal("Plans/opencode.md");
    settle(&mut page);
    assert_eq!(page.handle_doc_key(ch('c')), DocsStep::Stay);
    settle(&mut page);
    assert_eq!(page.inspecting(), Some("Plans/opencode.md"));
    assert_eq!(page.inspector().unwrap().hunks().len(), 2);
    let shown = drawn(&mut page, 100, 30, true);
    assert!(shown.contains("Resolve Plans › Auth · 0 of 2 chosen"), "{shown}");
    assert!(shown.contains("Alpha ours."), "{shown}");
    assert!(shown.contains("Alpha theirs."), "{shown}");

    // Apply waits for every hunk.
    page.handle_doc_key(key(KeyCode::Enter));
    assert!(!asked(&docs).iter().any(|r| matches!(r, DocsRequest::Resolve { .. })));
    page.handle_doc_key(ch('t'));
    page.handle_doc_key(ch('o'));
    assert!(drawn(&mut page, 100, 30, true).contains("2 of 2 chosen"));
    page.handle_doc_key(key(KeyCode::Enter));
    settle(&mut page);
    assert_eq!(page.inspecting(), None, "closed once applied");
    assert_eq!(page.note(), Some("Resolved Plans/opencode.md"));
    let merged = "# Auth\n\n## Alpha\n\nAlpha theirs.\n\n## Beta\n\nBeta ours.\n";
    assert_eq!(docs.lock().unwrap().docs["Plans/opencode.md"], merged);
    assert_eq!(page.doc_content(), Some(merged), "the open document reads the result");
    assert!(!drawn(&mut page, 60, 20, false).contains("couldn't be merged"));
}

#[test]
fn use_all_theirs_and_esc_from_the_tree() {
    let (docs, mut page) = held();
    // From the tree, `c` opens the first held document and gives it the keys.
    assert_eq!(page.handle_tree_key(ch('c')), DocsStep::FocusDoc);
    settle(&mut page);
    assert_eq!(page.inspecting(), Some("Plans/opencode.md"));
    // Esc cancels without resolving.
    page.handle_doc_key(key(KeyCode::Esc));
    assert_eq!(page.inspecting(), None);
    page.handle_tree_key(ch('c'));
    settle(&mut page);
    page.handle_doc_key(ch('T'));
    page.handle_doc_key(key(KeyCode::Enter));
    settle(&mut page);
    assert_eq!(docs.lock().unwrap().docs["Plans/opencode.md"], THEIRS);
    assert!(asked(&docs).iter().any(|r| matches!(
        r,
        DocsRequest::Resolve { resolution: shelbi_app::docs::Resolution::Hunks { choices, .. }, .. }
            if choices.iter().all(|c| *c == shelbi_app::docs_conflict::Side::Theirs)
    )));
    // Nothing left: `c` says so.
    assert_eq!(page.handle_tree_key(ch('c')), DocsStep::Stay);
    assert!(page.note().unwrap().starts_with("Nothing to resolve"));
}

#[test]
fn a_refused_apply_keeps_the_inspector_with_the_reason() {
    let (docs, mut page) = held();
    page.handle_tree_key(ch('c'));
    settle(&mut page);
    // The document changed here since the sides were read.
    docs.lock().unwrap().docs.insert("Plans/opencode.md".into(), "# Auth\n\nNew.\n".into());
    page.handle_doc_key(ch('O'));
    page.handle_doc_key(key(KeyCode::Enter));
    settle(&mut page);
    assert_eq!(page.inspecting(), Some("Plans/opencode.md"));
    let shown = drawn(&mut page, 120, 30, true);
    assert!(shown.contains("changed since its conflict was read"), "{shown}");
    // The hub's change re-reads the sides.
    page.changed(&["Plans/opencode.md".to_string()]);
    settle(&mut page);
    assert!(page.inspector().unwrap().conflict().ours.as_deref().unwrap().contains("New."));
}

#[test]
fn a_kitty_terminal_draws_a_documents_images_relative_to_its_folder() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("Plans/img")).unwrap();
    image::RgbaImage::from_pixel(200, 60, image::Rgba([0, 0, 0, 255]))
        .save(root.path().join("Plans/img/board.png"))
        .unwrap();
    let mut fake = FakeDocs::sample();
    fake.root = root.path().display().to_string();
    fake.docs.insert(
        "Plans/shots.md".into(),
        "# Shots\n\nIntro.\n\n![The board](img/board.png)\n\n![Missing one](img/gone.png)\n\nAfter.\n".into(),
    );
    let docs = Arc::new(Mutex::new(fake));
    let mut page = DocsPage::new("proj", ask_of(&docs));
    page.open();
    settle(&mut page);
    page.open_doc("Plans/shots.md");
    settle(&mut page);

    // Without graphics: the source, as before.
    let text = drawn(&mut page, 60, 30, true);
    assert!(text.contains("![The board](img/board.png)"), "{text}");
    assert!(page.take_placements().is_empty());

    // With them: the picture (200×60 px in 10×20 cells: 20 cols, 3 rows)
    // under the prose, its caption under it; the missing one keeps its source.
    page.set_graphics(Some(crate::kitty::Graphics { cell: (10, 20) }));
    let text = drawn(&mut page, 60, 30, true);
    assert!(!text.contains("![The board]"), "{text}");
    assert!(text.contains("The board"), "{text}");
    assert!(text.contains("![Missing one](img/gone.png)"), "{text}");
    let placements = page.take_placements();
    assert_eq!(placements.len(), 1, "{placements:?}");
    let p = &placements[0];
    assert_eq!((p.cols, p.rows, p.shown), (20, 3, 3));
    assert_eq!(p.path, root.path().join("Plans/img/board.png"));
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[p.y as usize - 2].contains("Intro."), "{text}");
    assert!(lines[p.y as usize + 3].contains("The board"), "{text}");
}
