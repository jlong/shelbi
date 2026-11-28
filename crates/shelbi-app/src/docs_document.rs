//! A document open for editing in place (`docs-desktop-document`): what the
//! hub has, a save on its way, and what to do when the hub says the
//! document changed.
//!
//! The editor's text is the client's; [`DocSync`] only keeps the hub's side
//! of it. Typing restarts a short debounce ([`SAVE_AFTER`]); once it runs
//! out the text goes to the hub as one whole-document write, one at a time.
//! When the hub announces a change to the open document the client reads it
//! again and hands the text to [`DocSync::read`], which tells our own save's
//! echo from someone else's edit: with no unsaved edits of ours the editor
//! takes it in place ([`Read::Reload`]); with some, it is held
//! ([`Read::Elsewhere`]) and nothing saves until the user picks Reload
//! ([`DocSync::take_theirs`]) or Keep mine ([`DocSync::keep_mine`]).
//!
//! Also the toolbar's "Updated by … · 2m ago" line ([`Updated`]), from the
//! document's last commit or a change seen since.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::docs::{Author, DocsCommit};

/// How long the typing has to pause before the document saves.
pub const SAVE_AFTER: Duration = Duration::from_millis(600);

/// What [`DocSync::read`] makes of the hub's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    /// Nothing to do: the editor already shows it, or it is our own save
    /// coming back.
    Same,
    /// Someone else changed it and we have no unsaved edits: show this.
    Reload(String),
    /// Someone else changed it while we have unsaved edits: the
    /// changed-elsewhere bar asks which to keep.
    Elsewhere,
}

/// The hub's side of one open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSync {
    path: String,
    /// The text the hub has, as far as we know: the last read, or the last
    /// save that landed.
    base: String,
    /// A save on its way: the text it sends.
    sending: Option<String>,
    /// When the editor last changed while it differs from `base`: the
    /// debounce runs from here.
    edited_at: Option<Instant>,
    /// The hub's text changed elsewhere while we had unsaved edits: theirs,
    /// held until the user picks.
    elsewhere: Option<String>,
    /// Why the last save failed.
    error: Option<String>,
}

impl DocSync {
    /// `path` just read from the hub as `text`.
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        DocSync {
            path: path.into(),
            base: text.into(),
            sending: None,
            edited_at: None,
            elsewhere: None,
            error: None,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The text the hub has, as far as we know.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The document moved (a rename in the tree, or its title renamed its
    /// file): the same text, at `path` now.
    pub fn moved_to(&mut self, path: impl Into<String>) {
        self.path = path.into();
    }

    /// Whether `text` (the editor's) has edits the hub doesn't have yet.
    pub fn is_dirty(&self, text: &str) -> bool {
        text != self.base
    }

    /// A save is on its way.
    pub fn is_saving(&self) -> bool {
        self.sending.is_some()
    }

    /// The changed-elsewhere bar shows.
    pub fn changed_elsewhere(&self) -> bool {
        self.elsewhere.is_some()
    }

    /// Why the last save failed, until one lands.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The editor's text is now `text`: (re)start the debounce, or stop it
    /// when the edits were undone back to what the hub has.
    pub fn edited(&mut self, text: &str, now: Instant) {
        self.edited_at = (text != self.base).then_some(now);
    }

    /// When the next save is due, if one is waiting.
    pub fn due_at(&self) -> Option<Instant> {
        if self.sending.is_some() || self.elsewhere.is_some() {
            return None;
        }
        self.edited_at.map(|at| at + SAVE_AFTER)
    }

    /// The save due at `now` for the editor's `text` (or at once with
    /// `flush`: ⌘S, leaving the document): the text to send, which is then
    /// on its way. None while one is already on its way, while the
    /// changed-elsewhere bar waits, or when there is nothing new.
    pub fn take_save(&mut self, text: &str, now: Instant, flush: bool) -> Option<String> {
        if self.sending.is_some() || self.elsewhere.is_some() {
            return None;
        }
        if text == self.base {
            self.edited_at = None;
            return None;
        }
        let at = self.edited_at.get_or_insert(now);
        if !flush && now < *at + SAVE_AFTER {
            return None;
        }
        self.edited_at = None;
        self.sending = Some(text.to_string());
        Some(text.to_string())
    }

    /// The save on its way landed (`Ok`) or was refused. A refused save
    /// waits for the next edit, or a flush, to try again.
    pub fn saved(&mut self, result: Result<(), String>) {
        let Some(sent) = self.sending.take() else {
            return;
        };
        match result {
            Ok(()) => {
                self.base = sent;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The hub's text `theirs`, read after it announced a change to this
    /// document, while the editor shows `ours`.
    pub fn read(&mut self, theirs: String, ours: &str) -> Read {
        if theirs == ours {
            // Whatever happened, both sides agree now.
            if self.sending.as_deref() != Some(theirs.as_str()) {
                self.base = theirs;
            }
            self.elsewhere = None;
            return Read::Same;
        }
        // Our own save coming back (the editor has moved on since), or a
        // read that raced ahead of it: nothing new.
        if self.sending.as_deref() == Some(theirs.as_str()) || theirs == self.base {
            return Read::Same;
        }
        if ours == self.base && self.sending.is_none() {
            self.base = theirs.clone();
            self.edited_at = None;
            return Read::Reload(theirs);
        }
        self.elsewhere = Some(theirs);
        Read::Elsewhere
    }

    /// The bar's Reload: theirs, our edits dropped. The text the editor
    /// takes.
    pub fn take_theirs(&mut self) -> Option<String> {
        let theirs = self.elsewhere.take()?;
        self.base = theirs.clone();
        self.edited_at = None;
        Some(theirs)
    }

    /// The bar's Keep mine: theirs is what the hub has, and ours is an edit
    /// of it, to save at once (a flush). `false` when the bar wasn't up.
    pub fn keep_mine(&mut self, now: Instant) -> bool {
        let Some(theirs) = self.elsewhere.take() else {
            return false;
        };
        self.base = theirs;
        self.edited_at = Some(now);
        true
    }
}

/// Who last changed a document, for the toolbar's line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    You,
    Agent,
    /// A commit's git author, when its hub didn't say (made elsewhere).
    Named(String),
}

/// A document's last change: who and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    pub who: Who,
    pub at: DateTime<Utc>,
}

impl Updated {
    /// From the document's last commit.
    pub fn of_commit(c: &DocsCommit) -> Option<Updated> {
        let at = DateTime::parse_from_rfc3339(&c.date).ok()?.with_timezone(&Utc);
        let who = match c.by {
            Some(Author::Person) => Who::You,
            Some(Author::Agent) => Who::Agent,
            None => Who::Named(c.author.clone()),
        };
        Some(Updated { who, at })
    }

    /// From a change the hub announced (`local`, `agent`, `pull`) at `at`:
    /// `None` for a pull, whose commit says who made it.
    pub fn of_change(source: &str, at: DateTime<Utc>) -> Option<Updated> {
        let who = match source {
            "agent" => Who::Agent,
            "local" => Who::You,
            _ => return None,
        };
        Some(Updated { who, at })
    }

    /// `Updated by you · 2m ago`.
    pub fn line(&self, now: DateTime<Utc>) -> String {
        let who = match &self.who {
            Who::You => "you",
            Who::Agent => "an agent",
            Who::Named(name) if name.trim().is_empty() => "someone",
            Who::Named(name) => name.as_str(),
        };
        format!("Updated by {who} · {}", crate::jobs::age_label(self.at, now))
    }
}

/// The URL scheme of a link to a document from anywhere Markdown is
/// shown (a task body): `docs:Plans/foo.md`. New text writes this one.
pub const LINK_SCHEME: &str = "docs:";

/// The scheme such links had before Documentation was called that
/// (`wiki:Plans/foo.md`). Task bodies written then still carry it, so
/// [`link_path`] reads it as [`LINK_SCHEME`].
pub const LEGACY_LINK_SCHEME: &str = "wiki:";

/// The docs-relative path a `docs:` link points at (`docs:Plans/foo.md`,
/// `docs:/Plans/foo.md`, `docs://Plans/foo.md`, or the same under the legacy
/// `wiki:`), or `None` for any other URL. A `#section` is dropped, and
/// percent-escapes (`%20`) are read.
pub fn link_path(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url.strip_prefix(LINK_SCHEME).or_else(|| url.strip_prefix(LEGACY_LINK_SCHEME))?;
    let rest = rest.trim_start_matches('/');
    let rest = rest.split(['#', '?']).next().unwrap_or("");
    let path = percent_decode(rest);
    let path = path.trim_matches('/').to_string();
    (!path.is_empty()).then_some(path)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, t0: Instant) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn typing_saves_once_it_pauses_and_one_save_at_a_time() {
        let t0 = Instant::now();
        let mut d = DocSync::new("a.md", "# A\n");
        assert_eq!(d.take_save("# A\n", t0, false), None, "nothing new");
        d.edited("# A\nx", t0);
        assert_eq!(d.take_save("# A\nx", at(100, t0), false), None, "still typing");
        d.edited("# A\nxy", at(400, t0));
        assert_eq!(d.due_at(), Some(at(400, t0) + SAVE_AFTER), "each keystroke restarts it");
        assert_eq!(d.take_save("# A\nxy", at(1100, t0), false), Some("# A\nxy".into()));
        assert!(d.is_saving());
        d.edited("# A\nxyz", at(1200, t0));
        assert_eq!(d.take_save("# A\nxyz", at(5000, t0), true), None, "the first is still on its way");
        d.saved(Ok(()));
        assert_eq!(d.base(), "# A\nxy");
        assert!(d.is_dirty("# A\nxyz"));
        assert_eq!(d.take_save("# A\nxyz", at(5000, t0), false), Some("# A\nxyz".into()));
    }

    #[test]
    fn undoing_back_to_the_hubs_text_saves_nothing_and_a_flush_saves_at_once() {
        let t0 = Instant::now();
        let mut d = DocSync::new("a.md", "a");
        d.edited("ab", t0);
        d.edited("a", at(10, t0));
        assert_eq!(d.due_at(), None);
        assert_eq!(d.take_save("a", at(2000, t0), true), None);
        d.edited("abc", at(20, t0));
        assert_eq!(d.take_save("abc", at(30, t0), true), Some("abc".into()), "⌘S doesn't wait");
    }

    #[test]
    fn a_refused_save_is_kept_and_tried_again() {
        let t0 = Instant::now();
        let mut d = DocSync::new("a.md", "a");
        d.edited("ab", t0);
        assert!(d.take_save("ab", at(700, t0), false).is_some());
        d.saved(Err("the file is held".into()));
        assert_eq!(d.error(), Some("the file is held"));
        assert_eq!(d.base(), "a");
        assert_eq!(d.take_save("ab", at(800, t0), true), Some("ab".into()));
        d.saved(Ok(()));
        assert_eq!(d.error(), None);
    }

    #[test]
    fn our_own_saves_echo_changes_nothing() {
        let t0 = Instant::now();
        let mut d = DocSync::new("a.md", "a");
        d.edited("ab", t0);
        d.take_save("ab", at(700, t0), false);
        // The hub announces the write before its answer lands; the editor
        // has moved on.
        assert_eq!(d.read("ab".into(), "abc"), Read::Same);
        // A read that raced ahead of the write.
        assert_eq!(d.read("a".into(), "abc"), Read::Same);
        d.saved(Ok(()));
        assert_eq!(d.read("ab".into(), "abc"), Read::Same);
        assert!(!d.changed_elsewhere());
    }

    #[test]
    fn a_clean_document_takes_an_outside_change_in_place() {
        let mut d = DocSync::new("a.md", "a");
        assert_eq!(d.read("a, by an agent".into(), "a"), Read::Reload("a, by an agent".into()));
        assert_eq!(d.base(), "a, by an agent");
        assert!(!d.is_dirty("a, by an agent"));
    }

    #[test]
    fn a_dirty_document_holds_the_outside_change_for_the_bar() {
        let t0 = Instant::now();
        let mut d = DocSync::new("a.md", "a");
        d.edited("a, mine", t0);
        assert_eq!(d.read("a, theirs".into(), "a, mine"), Read::Elsewhere);
        assert!(d.changed_elsewhere());
        assert_eq!(d.due_at(), None, "nothing saves while the bar asks");
        assert_eq!(d.take_save("a, mine", at(5000, t0), true), None);

        // Reload: theirs.
        let mut reload = d.clone();
        assert_eq!(reload.take_theirs(), Some("a, theirs".into()));
        assert!(!reload.changed_elsewhere());
        assert!(!reload.is_dirty("a, theirs"));

        // Keep mine: saves over theirs at once.
        assert!(d.keep_mine(at(6000, t0)));
        assert_eq!(d.take_save("a, mine", at(6000, t0), true), Some("a, mine".into()));
        assert_eq!(d.base(), "a, theirs");
        d.saved(Ok(()));
        assert_eq!(d.base(), "a, mine");
    }

    #[test]
    fn the_same_text_from_both_sides_clears_the_bar() {
        let t0 = Instant::now();
        let mut d = DocSync::new("a.md", "a");
        d.edited("b", t0);
        assert_eq!(d.read("c".into(), "b"), Read::Elsewhere);
        assert_eq!(d.read("b".into(), "b"), Read::Same);
        assert!(!d.changed_elsewhere());
        assert_eq!(d.base(), "b");
    }

    #[test]
    fn the_updated_line_names_you_an_agent_or_the_commits_author() {
        let now = DateTime::parse_from_rfc3339("2026-11-27T12:10:00Z").unwrap().with_timezone(&Utc);
        let commit = |by, author: &str| DocsCommit {
            sha: "abc".into(),
            author: author.into(),
            date: "2026-11-27T12:00:00Z".into(),
            subject: "Update a.md".into(),
            paths: vec!["a.md".into()],
            by,
        };
        let line = |c: DocsCommit| Updated::of_commit(&c).unwrap().line(now);
        assert_eq!(line(commit(Some(Author::Person), "John")), "Updated by you · 10m ago");
        assert_eq!(line(commit(Some(Author::Agent), "John")), "Updated by an agent · 10m ago");
        assert_eq!(line(commit(None, "Jane Doe")), "Updated by Jane Doe · 10m ago");
        assert_eq!(Updated::of_change("agent", now).unwrap().line(now), "Updated by an agent · just now");
        assert_eq!(Updated::of_change("pull", now), None, "a pull's commit says");
    }

    #[test]
    fn docs_links_name_a_docs_path() {
        assert_eq!(link_path("docs:Plans/foo.md").as_deref(), Some("Plans/foo.md"));
        assert_eq!(link_path("docs:/Plans/foo.md").as_deref(), Some("Plans/foo.md"));
        assert_eq!(link_path("docs://Plans/foo.md#goals").as_deref(), Some("Plans/foo.md"));
        assert_eq!(link_path("docs:Plans/My%20plan.md").as_deref(), Some("Plans/My plan.md"));
        assert_eq!(link_path("docs:"), None);
        // Links written before the rename open the same documents.
        assert_eq!(link_path("wiki:Plans/foo.md").as_deref(), Some("Plans/foo.md"));
        assert_eq!(link_path("wiki://Plans/foo.md#goals").as_deref(), Some("Plans/foo.md"));
        assert_eq!(link_path(" wiki:Plans/My%20plan.md ").as_deref(), Some("Plans/My plan.md"));
        assert_eq!(link_path("wiki:"), None);
        assert_eq!(link_path("https://example.com/wiki:x"), None);
        assert_eq!(link_path("https://example.com/docs:x"), None);
    }
}
