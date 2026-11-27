//! The Wiki page against a test wiki: an in-memory hub that keeps its
//! documents and folders and answers the page's requests as the hub's wiki
//! service does.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use shelbi_app::wiki::{FailureKind, NodeKind, SyncState, WikiNode, WikiReply, WikiRequest, WikiStatus};

use super::*;

/// The test wiki: documents by path, folders, and what was asked.
#[derive(Default)]
pub(crate) struct FakeWiki {
    pub docs: BTreeMap<String, String>,
    pub folders: Vec<String>,
    pub asked: Vec<WikiRequest>,
    pub set_up: bool,
}

impl FakeWiki {
    pub(crate) fn sample() -> Self {
        let mut docs = BTreeMap::new();
        docs.insert("README.md".into(), "# Shelbi docs\n\nStart here.\n".into());
        docs.insert("Plans/awareness.md".into(), "# Awareness v2\n\nBody.\n".into());
        docs.insert(
            "Plans/opencode.md".into(),
            "# OpenCode integration\n\n## Context\n\nOpenCode is a terminal coding agent.\n".into(),
        );
        docs.insert("Plans/Archive/old.md".into(), "# Old plan\n".into());
        FakeWiki {
            docs,
            folders: vec!["Plans".into(), "Plans/Archive".into(), "Research".into()],
            asked: Vec::new(),
            set_up: true,
        }
    }

    fn status(&self) -> WikiStatus {
        if !self.set_up {
            return WikiStatus::not_set_up();
        }
        WikiStatus {
            set_up: true,
            state: SyncState::Synced,
            conflicts: Vec::new(),
            paused: false,
            auto: true,
            source: "branch".into(),
            branch: "docs/planning".into(),
            root: "/tmp/wiki".into(),
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
    fn nodes(&self, folder: &str) -> Vec<WikiNode> {
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

    fn answer(&mut self, req: WikiRequest) -> WikiReply {
        self.asked.push(req.clone());
        let missing = |p: &str| WikiReply::failed(FailureKind::NotFound, format!("no `{p}`"));
        match req {
            WikiRequest::Init => {
                self.set_up = true;
                WikiReply::Status(self.status())
            }
            WikiRequest::Status => WikiReply::Status(self.status()),
            _ if !self.set_up => WikiReply::failed(FailureKind::NotSetUp, "no wiki"),
            WikiRequest::Sync(_) => WikiReply::Status(self.status()),
            WikiRequest::Tree { .. } => WikiReply::Tree(self.nodes("")),
            WikiRequest::Read { path, .. } => match self.docs.get(&path) {
                Some(content) => WikiReply::Doc { path, content: content.clone() },
                None => missing(&path),
            },
            WikiRequest::Write { path, content, .. } => {
                self.docs.insert(path, content);
                WikiReply::Done
            }
            WikiRequest::New { path, .. } if path.ends_with(".md") => {
                let title = path.rsplit('/').next().unwrap().trim_end_matches(".md").to_string();
                self.docs.insert(path, format!("# {title}\n"));
                WikiReply::Done
            }
            WikiRequest::New { path, .. } => {
                self.folders.push(path);
                WikiReply::Done
            }
            WikiRequest::Move { from, to, .. } => {
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
                WikiReply::Done
            }
            WikiRequest::Remove { path, .. } => {
                self.docs.retain(|d, _| d != &path && !d.starts_with(&format!("{path}/")));
                self.folders.retain(|f| f != &path && !f.starts_with(&format!("{path}/")));
                WikiReply::Done
            }
            other => WikiReply::failed(FailureKind::Failed, format!("the fake doesn't do {other:?}")),
        }
    }
}

fn node(path: &str, name: &str, title: &str, kind: NodeKind, children: Vec<WikiNode>) -> WikiNode {
    let kind = match kind {
        NodeKind::Folder => "Folder",
        NodeKind::Document => "Document",
    };
    serde_json::from_value(serde_json::json!({
        "path": path, "name": name, "title": title, "kind": kind,
        "children": serde_json::to_value(children).unwrap(),
    }))
    .expect("a wiki node")
}

/// An `Ask` over `wiki`.
pub(crate) fn ask_of(wiki: &Arc<Mutex<FakeWiki>>) -> Ask {
    let wiki = wiki.clone();
    Arc::new(move |req| wiki.lock().unwrap().answer(req))
}

/// Fold in every answer the page waits on.
pub(crate) fn settle(page: &mut WikiPage) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        page.poll();
        if !page.busy() {
            return;
        }
        assert!(Instant::now() < deadline, "the wiki's work never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn typed(page: &mut WikiPage, text: &str) {
    for c in text.chars() {
        page.handle_tree_key(ch(c));
    }
}

fn opened() -> (Arc<Mutex<FakeWiki>>, WikiPage) {
    let wiki = Arc::new(Mutex::new(FakeWiki::sample()));
    let mut page = WikiPage::new("proj", ask_of(&wiki));
    page.open();
    settle(&mut page);
    (wiki, page)
}

fn titles(page: &WikiPage) -> Vec<String> {
    page.tree.rows().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.title)).collect()
}

fn asked(wiki: &Arc<Mutex<FakeWiki>>) -> Vec<WikiRequest> {
    wiki.lock().unwrap().asked.clone()
}

#[test]
fn opening_reads_the_tree_with_titles_and_tells_the_hub_it_shows() {
    let (wiki, page) = opened();
    assert!(page.tree_open);
    assert_eq!(titles(&page), ["Plans", "Research", "Shelbi docs"]);
    let asked = asked(&wiki);
    assert!(asked.contains(&WikiRequest::Status));
    assert!(asked.contains(&WikiRequest::Tree { path: None }));
    // The active ping is fire-and-forget; give it a moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !wiki.lock().unwrap().asked.contains(&WikiRequest::Sync(SyncOp::Active)) {
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
    assert_eq!(page.handle_tree_key(key(KeyCode::Enter)), WikiStep::FocusDoc);
    settle(&mut page);
    assert_eq!(page.doc_path(), Some("Plans/opencode.md"));
    assert!(page.doc_content().unwrap().contains("terminal coding agent"));
    // The document scrolls; Esc goes back to the tree.
    page.handle_doc_key(ch('j'));
    assert_eq!(page.doc_scroll(), 1);
    page.handle_doc_key(ch('g'));
    assert_eq!(page.doc_scroll(), 0);
    assert_eq!(page.handle_doc_key(key(KeyCode::Esc)), WikiStep::FocusTree);
}

#[test]
fn n_and_capital_n_make_a_document_and_a_folder_in_the_selected_folder() {
    let (wiki, mut page) = opened();
    page.tree.select_path("Plans/awareness.md");
    page.handle_tree_key(ch('n'));
    assert!(page.typing());
    typed(&mut page, "roadmap");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(asked(&wiki).iter().any(|r| matches!(r, WikiRequest::New { path, .. } if path == "Plans/roadmap.md")));
    // The new document is selected and open.
    assert_eq!(page.tree.selected_path(), Some("Plans/roadmap.md"));
    settle(&mut page);
    assert_eq!(page.doc_path(), Some("Plans/roadmap.md"));

    page.tree.select_path("Research");
    page.handle_tree_key(ch('N'));
    typed(&mut page, "Notes");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(wiki.lock().unwrap().folders.contains(&"Research/Notes".to_string()));
    assert_eq!(page.tree.selected_path(), Some("Research/Notes"));

    // Esc drops a prompt without asking anything.
    let before = asked(&wiki).len();
    page.handle_tree_key(ch('n'));
    typed(&mut page, "x");
    page.handle_tree_key(key(KeyCode::Esc));
    assert!(!page.typing());
    settle(&mut page);
    assert_eq!(asked(&wiki).len(), before);
}

#[test]
fn r_renames_and_m_moves_and_the_open_document_follows() {
    let (wiki, mut page) = opened();
    page.tree.select_path("Plans/awareness.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    page.handle_tree_key(ch('r'));
    // The draft is the file name, without `.md`; Ctrl+U clears it.
    page.handle_tree_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    typed(&mut page, "awareness-v2");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    assert!(asked(&wiki).contains(&WikiRequest::Move {
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
    assert!(wiki.lock().unwrap().docs.contains_key("Research/awareness-v2.md"));
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
    let (wiki, mut page) = opened();
    page.tree.select_path("README.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    page.handle_tree_key(ch('d'));
    page.handle_tree_key(ch('n'));
    settle(&mut page);
    assert!(wiki.lock().unwrap().docs.contains_key("README.md"), "anything but y keeps it");
    page.handle_tree_key(ch('d'));
    page.handle_tree_key(ch('y'));
    settle(&mut page);
    assert!(!wiki.lock().unwrap().docs.contains_key("README.md"));
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
    assert_eq!(page.handle_tree_key(key(KeyCode::Esc)), WikiStep::Stay);
    assert_eq!(titles(&page).len(), 6, "Plans stays open where the filter opened it");
    assert_eq!(page.handle_tree_key(key(KeyCode::Esc)), WikiStep::Back);
}

#[test]
fn e_edits_a_copy_and_the_save_goes_through_the_hub() {
    let (wiki, mut page) = opened();
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
    assert!(asked(&wiki).iter().any(|r| matches!(r, WikiRequest::Write { .. })), "{:?}", page.note());
    assert!(asked(&wiki).contains(&WikiRequest::Write {
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
    let (wiki, mut page) = opened();
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
    wiki.lock().unwrap().docs.insert("README.md".into(), "# Theirs\n".into());
    let file = edit.file.clone();
    page.finish_edit(edit, Ok(()));
    settle(&mut page);
    assert!(page.note().unwrap().contains("changed elsewhere"), "{:?}", page.note());
    assert_eq!(wiki.lock().unwrap().docs["README.md"], "# Theirs\n");
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
fn a_wiki_change_reads_the_tree_and_the_open_document_again() {
    let (wiki, mut page) = opened();
    page.tree.select_path("README.md");
    page.handle_tree_key(key(KeyCode::Enter));
    settle(&mut page);
    {
        let mut w = wiki.lock().unwrap();
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
fn a_project_without_a_wiki_offers_to_set_one_up() {
    let wiki = Arc::new(Mutex::new(FakeWiki::sample()));
    wiki.lock().unwrap().set_up = false;
    let mut page = WikiPage::new("proj", ask_of(&wiki));
    page.open();
    settle(&mut page);
    assert_eq!(page.status_line(chrono::Utc::now()).to_string(), "○ Not set up");
    page.handle_tree_key(ch('i'));
    settle(&mut page);
    assert!(asked(&wiki).contains(&WikiRequest::Init));
    assert_eq!(titles(&page), ["Plans", "Research", "Shelbi docs"]);
}

#[test]
fn the_status_line_says_the_state_and_how_long_ago() {
    let (wiki, mut page) = opened();
    assert_eq!(page.status_line(chrono::Utc::now()).to_string(), "● Synced");
    let now = chrono::Utc::now();
    let mut status = wiki.lock().unwrap().status();
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
    assert!(all.contains("WIKI"), "{all}");
    assert!(all.contains("/ Filter"));
    assert!(all.contains("▾ Plans"));
    assert!(all.contains("    OpenCode integration"));
    assert!(all.contains("● Synced"));
    assert!(all.contains("Plans › OpenCode integration"));
    assert!(all.contains("OpenCode is a terminal coding agent."));
    // A click on the README row opens it.
    let y = screen.iter().position(|l| l.contains("Shelbi docs") && !l.contains("›")).unwrap() as u16;
    assert_eq!(page.click_tree(4, y), WikiStep::Stay);
    settle(&mut page);
    assert_eq!(page.doc_path(), Some("README.md"));
    // The back button goes back to the nav.
    assert_eq!(page.click_tree(3, 1), WikiStep::Back);
}
