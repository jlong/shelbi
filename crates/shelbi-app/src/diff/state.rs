//! The diff view's state machine: a cursor over [`RowTable`] rows, the modes,
//! and what each [`DiffAction`] does. No IO and no toolkit: a client feeds it
//! keys ([`DiffState::key`]) and its viewport, draws the rows in view, and
//! carries out the [`DiffEffect`]s that come back.
//!
//! The modes follow Skim's. NORMAL and VISUAL read keys through the `diff`
//! keymap, with vim's counts and two-key bindings. SEARCH, FIND, COMMENT and
//! the palette take text, so they read keys directly and aren't rebindable.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::Utc;
use shelbi_state::keymap::{DiffAction, Key, KeyChord, Mods, ModeKeymap};

use super::comments::{
    agent_message, anchor_for, format_comment, format_comments, place, Author, Comment, CommentDoc, CommentOp,
    CommentReply, Placement,
};
use super::draft::{CommentDraft, DraftStep, DraftTarget};
use super::git::BlameLine;
use super::highlight::FileHighlights;
use super::model::{Changeset, CommitScope, DiffLine, FileDiff, LineKind, Side};
use super::palette::{DiffPalette, PaletteMode, PaletteStep, PaletteTarget};
use super::rows::{build_rows, Folds, HunkFilter, Row, RowInput, RowTable, ThreadPart, Threads, ViewMode};
use super::search::{matches_on, search, SearchMatch};
use super::store::{ViewedDoc, ViewedOp};

/// What a client (or the session's workers) has to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffEffect {
    /// Copy this to the clipboard.
    Yank(String),
    /// Open the worktree's file at this line in the user's editor.
    OpenInEditor { path: String, line: Option<u32> },
    /// Send this message to the target's agent. Once it's delivered,
    /// [`DiffState::mark_sent`] these threads.
    SendToAgent { message: String, ids: Vec<String> },
    /// Read the changeset again.
    Refresh,
    /// Read another commit's changes.
    LoadScope(CommitScope),
    /// Read `path`'s blame at `rev` (the worktree when `None`).
    LoadBlame { path: String, rev: Option<String> },
    /// Save this change to the task's comments.
    SaveComment(CommentOp),
    /// Save this change to the task's viewed marks.
    SaveViewed(ViewedOp),
}

/// Which character find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}

/// The mode, and the state only it has.
#[derive(Debug, Clone)]
pub enum Mode {
    Normal,
    /// Selecting lines from this row to the cursor.
    Visual { anchor: usize },
    /// Typing a search. Enter runs it, Esc goes back to the search before it.
    Search { query: String },
    /// Waiting for the character of an `f`, `F`, `t` or `T`.
    Find { kind: FindKind, count: usize },
    Comment(Box<CommentDraft>),
    Palette(DiffPalette),
    /// The keys. A client draws them from the live keymap.
    Help { scroll: usize },
}

impl Mode {
    /// The mode's name, as the status line shows it.
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Visual { .. } => "VISUAL",
            Mode::Search { .. } => "SEARCH",
            Mode::Find { .. } => "FIND",
            Mode::Comment(_) => "COMMENT",
            Mode::Palette(_) => "PALETTE",
            Mode::Help { .. } => "HELP",
        }
    }
}

/// What the cursor is on, so it can be found again after the rows change.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spot {
    File { path: String },
    Hunk { path: String, old_start: u32 },
    Line { path: String, old_start: u32, side: Side, number: u32, text: String },
    Thread { id: String, path: String },
    Nothing,
}

/// The state.
pub struct DiffState {
    changeset: Arc<Changeset>,
    comments: CommentDoc,
    viewed: ViewedDoc,
    places: HashMap<String, Placement>,
    threads: Threads,
    expanded: HashSet<String>,
    table: RowTable,
    view: ViewMode,
    filter: HunkFilter,
    folds: Folds,
    /// The cursor's row.
    cursor: usize,
    /// The cursor's byte offset in its line (`f` and friends move it).
    col: usize,
    /// The first row in view, and how many rows are.
    top: usize,
    height: usize,
    mode: Mode,
    /// A two-key binding's first key, held.
    pending: Option<KeyChord>,
    /// The count typed so far.
    count: Option<usize>,
    /// The search run last, with its matches in reading order.
    query: String,
    matches: Vec<SearchMatch>,
    current_match: Option<usize>,
    last_find: Option<(FindKind, char)>,
    blame_on: bool,
    blame: HashMap<String, Arc<Vec<BlameLine>>>,
    highlights: HashMap<u32, Arc<FileHighlights>>,
    file_list: bool,
    /// One line for the status bar, until the next key.
    flash: Option<String>,
    /// `D` was pressed once: the next `D` clears.
    confirm_clear: bool,
    author: Author,
    loading: bool,
    error: Option<String>,
}

impl DiffState {
    /// An empty view, loading. `author` signs new comments.
    pub fn new(author: Author) -> Self {
        DiffState {
            changeset: Arc::new(Changeset::default()),
            comments: CommentDoc::default(),
            viewed: ViewedDoc::default(),
            places: HashMap::new(),
            threads: Threads::default(),
            expanded: HashSet::new(),
            table: RowTable::default(),
            view: ViewMode::default(),
            filter: HunkFilter::default(),
            folds: Folds::default(),
            cursor: 0,
            col: 0,
            top: 0,
            height: 24,
            mode: Mode::Normal,
            pending: None,
            count: None,
            query: String::new(),
            matches: Vec::new(),
            current_match: None,
            last_find: None,
            blame_on: false,
            blame: HashMap::new(),
            highlights: HashMap::new(),
            file_list: true,
            flash: None,
            confirm_clear: false,
            author,
            loading: true,
            error: None,
        }
    }

    // ---- What a client reads ------------------------------------------------

    pub fn changeset(&self) -> &Arc<Changeset> {
        &self.changeset
    }

    pub fn table(&self) -> &RowTable {
        &self.table
    }

    /// The rows in view.
    pub fn visible_rows(&self) -> &[Row] {
        self.table.slice(self.top, self.height)
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The cursor's byte offset in its line.
    pub fn col(&self) -> usize {
        self.col
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn view(&self) -> ViewMode {
        self.view
    }

    pub fn filter(&self) -> HunkFilter {
        self.filter
    }

    pub fn folds(&self) -> &Folds {
        &self.folds
    }

    pub fn file_list_open(&self) -> bool {
        self.file_list
    }

    pub fn blame_on(&self) -> bool {
        self.blame_on
    }

    /// A client's blame gutter for `path`, once it's read.
    pub fn blame_for(&self, path: &str) -> Option<&[BlameLine]> {
        self.blame.get(path).map(|b| b.as_slice())
    }

    pub fn highlights(&self, file: u32) -> Option<&FileHighlights> {
        self.highlights.get(&file).map(|h| &**h)
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn flash(&self) -> Option<&str> {
        self.flash.as_deref()
    }

    /// The first key of a two-key binding, held (a client can show it).
    pub fn pending(&self) -> Option<KeyChord> {
        self.pending
    }

    pub fn count(&self) -> Option<usize> {
        self.count
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The matches of the last search.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// The current match's place in them, from 1.
    pub fn current_match(&self) -> Option<usize> {
        self.current_match.map(|c| c + 1)
    }

    /// The matches on a line, for a client to mark.
    pub fn matches_on(&self, file: u32, hunk: u32, line: u32) -> &[SearchMatch] {
        matches_on(&self.matches, file, hunk, line)
    }

    /// The selected rows, in VISUAL.
    pub fn selection(&self) -> Option<(usize, usize)> {
        match self.mode {
            Mode::Visual { anchor } => Some((anchor.min(self.cursor), anchor.max(self.cursor))),
            _ => None,
        }
    }

    /// The file the cursor is in.
    pub fn current_file(&self) -> Option<usize> {
        self.table.rows.get(self.cursor).map(|r| r.file() as usize)
    }

    pub fn file(&self, index: u32) -> Option<&FileDiff> {
        self.changeset.files.get(index as usize)
    }

    /// A line of a row, when it has one.
    pub fn line(&self, file: u32, hunk: u32, line: u32) -> Option<&DiffLine> {
        self.changeset
            .files
            .get(file as usize)?
            .hunks
            .get(hunk as usize)?
            .lines
            .get(line as usize)
    }

    /// Whether `file`'s changes have been viewed since it last changed.
    pub fn is_viewed(&self, file: &FileDiff) -> bool {
        self.viewed.is_viewed(&file.path, file.fingerprint)
    }

    /// Every thread, in document order.
    pub fn comments(&self) -> &[Comment] {
        &self.comments.comments
    }

    /// The thread a [`Row::Thread`] shows.
    pub fn thread(&self, index: u32) -> Option<&Comment> {
        let (comment, _) = self.threads.placed.get(index as usize)?;
        self.comments.comments.get(*comment)
    }

    /// Whether a thread is out of date (its lines are gone).
    pub fn thread_outdated(&self, index: u32) -> bool {
        self.threads.placed.get(index as usize).is_some_and(|(_, p)| p.at.is_none())
    }

    /// Whether a thread's replies are showing.
    pub fn thread_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// The status bar, in the order the wireframe has it.
    pub fn status(&self) -> String {
        let mut parts = vec![self.mode.label().to_string(), self.view.label().to_string(), self.filter.label().to_string()];
        let files = self.changeset.files.len();
        if files > 0 {
            parts.push(format!("{}/{files}", self.current_file().map_or(0, |f| f + 1)));
        }
        let comments = self.comments.comments.len();
        if comments > 0 {
            let noun = if comments == 1 { "comment" } else { "comments" };
            parts.push(format!("{comments} {noun} (gY send to agent)"));
        }
        if !self.query.is_empty() {
            match self.current_match() {
                Some(at) => parts.push(format!("/{} {at}/{}", self.query, self.matches.len())),
                None => parts.push(format!("/{} 0/{}", self.query, self.matches.len())),
            }
        }
        if let Some(flash) = &self.flash {
            parts.push(flash.clone());
        }
        parts.push("? help".to_string());
        parts.join(" · ")
    }

    // ---- What a client and the session tell it ------------------------------

    /// How many rows the client shows.
    pub fn set_viewport(&mut self, height: usize) {
        self.height = height.max(1);
        self.keep_cursor_in_view();
    }

    /// Scroll the view without moving past the rows. The cursor stays in view.
    pub fn scroll_by(&mut self, rows: isize) {
        let max = self.max_top();
        self.top = self.top.saturating_add_signed(rows).min(max);
        let bottom = self.top + self.height.saturating_sub(1);
        self.cursor = self.cursor.clamp(self.top, bottom.min(self.table.len().saturating_sub(1)));
    }

    /// Put the cursor on a row (a click).
    pub fn click_row(&mut self, row: usize) {
        if row < self.table.len() {
            self.cursor = row;
            self.col = 0;
            self.keep_cursor_in_view();
        }
    }

    /// Jump to a file (a click in the file list, the palette).
    pub fn goto_file(&mut self, file: usize) {
        self.reveal_file(file);
        if let Some(row) = self.table.file_rows.get(file) {
            self.cursor = *row as usize;
            self.col = 0;
            self.top = self.cursor.min(self.max_top());
        }
    }

    /// Show the file list, or not.
    pub fn set_file_list(&mut self, open: bool) {
        self.file_list = open;
    }

    /// Mark `file`'s changes viewed, or not (the check in a file list).
    pub fn set_viewed(&mut self, file: usize, viewed: bool) -> Vec<DiffEffect> {
        let Some(f) = self.changeset.files.get(file) else { return Vec::new() };
        let op = if viewed {
            ViewedOp::Mark { path: f.path.clone(), fingerprint: f.fingerprint }
        } else {
            ViewedOp::Unmark { path: f.path.clone() }
        };
        self.viewed.apply(&op);
        vec![DiffEffect::SaveViewed(op)]
    }

    /// A new changeset arrived: the rows change under the cursor, which stays
    /// on the same line if it's still there.
    pub fn set_changeset(&mut self, changeset: Arc<Changeset>) {
        let spot = self.spot();
        let (top_offset, had_rows) = (self.cursor.saturating_sub(self.top), !self.table.is_empty());
        let same_files = self.changeset.files.len() == changeset.files.len();
        self.changeset = changeset;
        self.highlights.clear();
        self.blame.clear();
        self.loading = false;
        self.error = None;
        self.places = place(&self.comments, &self.changeset);
        if !self.query.is_empty() {
            self.matches = search(&self.changeset, &self.query);
            self.current_match = None;
        }
        self.rebuild();
        self.restore(&spot);
        if had_rows && same_files {
            self.top = self.cursor.saturating_sub(top_offset).min(self.max_top());
        }
        self.keep_cursor_in_view();
        // A selection's anchor is a row of the old table.
        if matches!(self.mode, Mode::Visual { .. }) {
            self.mode = Mode::Normal;
        }
    }

    /// Reading failed, with why. What's on show stays.
    pub fn set_error(&mut self, error: String) {
        self.loading = false;
        self.error = Some(error);
    }

    /// The task's comments as stored (a save's answer, another client's).
    pub fn set_comments(&mut self, doc: CommentDoc) {
        if doc == self.comments {
            return;
        }
        let spot = self.spot();
        self.comments = doc;
        self.places = place(&self.comments, &self.changeset);
        self.rebuild();
        self.restore(&spot);
        self.keep_cursor_in_view();
        // A selection's anchor is a row of the old table.
        if matches!(self.mode, Mode::Visual { .. }) {
            self.mode = Mode::Normal;
        }
    }

    /// The task's viewed marks as stored.
    pub fn set_viewed_doc(&mut self, doc: ViewedDoc) {
        self.viewed = doc;
    }

    pub fn set_blame(&mut self, path: String, blame: Vec<BlameLine>) {
        self.blame.insert(path, Arc::new(blame));
    }

    /// A file's highlights arrived.
    pub fn set_highlights(&mut self, file: u32, highlights: Arc<FileHighlights>) {
        self.highlights.insert(file, highlights);
    }

    /// `ids` were delivered to the agent.
    pub fn mark_sent(&mut self, ids: Vec<String>) -> Vec<DiffEffect> {
        let op = CommentOp::MarkSent { ids };
        self.comments.apply(&op);
        vec![DiffEffect::SaveComment(op)]
    }

    // ---- Keys ----------------------------------------------------------------

    /// A key. The answers are for the client to carry out, in order.
    pub fn key(&mut self, chord: KeyChord, keymap: &ModeKeymap<DiffAction>) -> Vec<DiffEffect> {
        let chord = normalize_key(chord);
        self.flash = None;
        let was_confirming = std::mem::take(&mut self.confirm_clear);
        match &mut self.mode {
            Mode::Normal | Mode::Visual { .. } => {}
            Mode::Search { .. } => return self.key_search(chord),
            Mode::Find { .. } => return self.key_find(chord),
            Mode::Comment(_) => return self.key_comment(chord),
            Mode::Palette(_) => return self.key_palette(chord),
            Mode::Help { scroll } => {
                match chord.code {
                    Key::Char('j') | Key::Down => *scroll += 1,
                    Key::Char('k') | Key::Up => *scroll = scroll.saturating_sub(1),
                    _ => self.mode = Mode::Normal,
                }
                return Vec::new();
            }
        }
        // Esc lets go of what's held: a first key, a count, a selection, a
        // search's marks.
        if chord.code == Key::Esc && chord.mods == Mods::NONE {
            if self.pending.take().is_none() && self.count.take().is_none() {
                if matches!(self.mode, Mode::Visual { .. }) {
                    self.mode = Mode::Normal;
                } else {
                    self.clear_search();
                }
            }
            return Vec::new();
        }
        let bound = match self.pending.take() {
            Some(first) => keymap.dispatch(chord.after(first)),
            None => {
                if self.take_count_digit(chord, keymap) {
                    return Vec::new();
                }
                if keymap.is_leader(chord) {
                    self.pending = Some(chord);
                    return Vec::new();
                }
                keymap.dispatch(chord)
            }
        };
        let count = self.count.take();
        let Some(action) = bound else { return Vec::new() };
        if action == DiffAction::ClearComments {
            self.confirm_clear = was_confirming;
        }
        self.apply(action, count)
    }

    /// A digit starts or adds to a count, unless the keymap binds it.
    fn take_count_digit(&mut self, chord: KeyChord, keymap: &ModeKeymap<DiffAction>) -> bool {
        let Key::Char(c) = chord.code else { return false };
        let Some(digit) = c.to_digit(10) else { return false };
        if chord.mods != Mods::NONE || keymap.dispatch(chord).is_some() || (digit == 0 && self.count.is_none()) {
            return false;
        }
        self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(digit as usize).min(100_000));
        true
    }

    fn key_search(&mut self, chord: KeyChord) -> Vec<DiffEffect> {
        let Mode::Search { query } = &mut self.mode else { return Vec::new() };
        let plain = !chord.mods.intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER);
        match chord.code {
            Key::Esc => self.mode = Mode::Normal,
            Key::Enter => {
                let query = std::mem::take(query);
                self.mode = Mode::Normal;
                self.run_search(query);
            }
            Key::Backspace => {
                query.pop();
            }
            Key::Char(c) if plain => query.push(with_shift(c, chord.mods)),
            _ => {}
        }
        Vec::new()
    }

    /// Run `query` and go to its first match after the cursor.
    pub fn run_search(&mut self, query: String) {
        self.query = query;
        self.matches = search(&self.changeset, &self.query);
        self.current_match = None;
        if self.query.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            self.flash = Some(format!("Nothing matches {}", self.query));
            return;
        }
        self.goto_next_match(true, 1);
    }

    fn clear_search(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current_match = None;
    }

    fn key_find(&mut self, chord: KeyChord) -> Vec<DiffEffect> {
        let Mode::Find { kind, count } = self.mode else { return Vec::new() };
        self.mode = Mode::Normal;
        if let (Key::Char(c), false) = (chord.code, chord.mods.intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER)) {
            let c = with_shift(c, chord.mods);
            self.last_find = Some((kind, c));
            self.find(kind, c, count);
        }
        Vec::new()
    }

    fn key_comment(&mut self, chord: KeyChord) -> Vec<DiffEffect> {
        let Mode::Comment(draft) = &mut self.mode else { return Vec::new() };
        match draft.key(typed(chord)) {
            DraftStep::Editing => Vec::new(),
            DraftStep::Cancel => {
                self.mode = Mode::Normal;
                Vec::new()
            }
            DraftStep::Save => {
                let (target, text) = (draft.target.clone(), draft.text());
                self.mode = Mode::Normal;
                self.save_draft(target, text)
            }
        }
    }

    fn save_draft(&mut self, target: DraftTarget, text: String) -> Vec<DiffEffect> {
        if text.is_empty() {
            return Vec::new();
        }
        let id = super::comments::fresh_id();
        let (op, focus) = match target {
            DraftTarget::New(anchor) => {
                let comment = Comment {
                    id: id.clone(),
                    anchor,
                    author: self.author.clone(),
                    body: text,
                    created_at: Utc::now(),
                    replies: Vec::new(),
                    sent: false,
                };
                (CommentOp::Add(comment), Some(id))
            }
            DraftTarget::Edit { id, reply } => (CommentOp::Edit { id, reply, body: text }, None),
            DraftTarget::Reply { id: thread } => {
                self.expanded.insert(thread.clone());
                let reply = CommentReply { id, author: self.author.clone(), body: text, created_at: Utc::now() };
                (CommentOp::Reply { id: thread.clone(), reply }, Some(thread))
            }
        };
        self.apply_comment_op(&op);
        if let Some(id) = focus {
            if let Some(row) = self.thread_row(&id) {
                self.cursor = row;
                self.keep_cursor_in_view();
            }
        }
        vec![DiffEffect::SaveComment(op)]
    }

    /// Apply `op` here. The rows change with it.
    fn apply_comment_op(&mut self, op: &CommentOp) {
        let spot = self.spot();
        self.comments.apply(op);
        self.places = place(&self.comments, &self.changeset);
        self.rebuild();
        self.restore(&spot);
        self.keep_cursor_in_view();
    }

    fn key_palette(&mut self, chord: KeyChord) -> Vec<DiffEffect> {
        let Mode::Palette(palette) = &mut self.mode else { return Vec::new() };
        match palette.key(typed(chord), &self.changeset) {
            PaletteStep::Stay => Vec::new(),
            PaletteStep::Close => {
                self.mode = Mode::Normal;
                Vec::new()
            }
            PaletteStep::Choose(target) => {
                self.mode = Mode::Normal;
                match target {
                    PaletteTarget::File(file) => {
                        self.goto_file(file);
                        Vec::new()
                    }
                    // The palette's Refresh is a refresh, wherever the cursor is.
                    PaletteTarget::Action(DiffAction::ReplyOrRefresh) => vec![DiffEffect::Refresh],
                    PaletteTarget::Action(action) => self.apply(action, None),
                    PaletteTarget::Scope(scope) => self.choose_scope(scope),
                }
            }
        }
    }

    /// Read another commit's changes (`Commits: all ▾`). Comments stay, shown
    /// where their lines are in it.
    pub fn choose_scope(&mut self, scope: CommitScope) -> Vec<DiffEffect> {
        if self.changeset.scope == scope && !self.loading {
            return Vec::new();
        }
        self.loading = true;
        vec![DiffEffect::LoadScope(scope)]
    }

    // ---- Actions -------------------------------------------------------------

    /// Do `action`, `count` times where that means something. Exposed so a
    /// menu or a button does what the key does.
    pub fn apply(&mut self, action: DiffAction, count: Option<usize>) -> Vec<DiffEffect> {
        let n = count.unwrap_or(1).max(1);
        match action {
            DiffAction::CursorDown => self.move_cursor(n as isize),
            DiffAction::CursorUp => self.move_cursor(-(n as isize)),
            DiffAction::PrevFile => self.step_file(-(n as isize)),
            DiffAction::NextFile => self.step_file(n as isize),
            DiffAction::PageDown => self.page(self.height.saturating_mul(n) as isize),
            DiffAction::PageUp => self.page(-(self.height.saturating_mul(n) as isize)),
            DiffAction::HalfPageDown => self.page((self.height / 2).max(1).saturating_mul(n) as isize),
            DiffAction::HalfPageUp => self.page(-((self.height / 2).max(1).saturating_mul(n) as isize)),
            DiffAction::Top => {
                self.cursor = 0;
                self.col = 0;
                self.top = 0;
            }
            DiffAction::Bottom => {
                self.cursor = self.table.len().saturating_sub(1);
                self.col = 0;
                self.keep_cursor_in_view();
            }
            DiffAction::CenterViewport => {
                self.top = self.cursor.saturating_sub(self.height / 2).min(self.max_top());
            }
            DiffAction::CenterCursor => {
                let middle = self.top + self.height / 2;
                self.cursor = middle.min(self.table.len().saturating_sub(1));
                self.col = 0;
            }
            DiffAction::NextChange => self.jump(n, true, Self::is_change_start),
            DiffAction::PrevChange => self.jump(n, false, Self::is_change_start),
            DiffAction::NextComment => self.jump(n, true, Self::is_thread_head),
            DiffAction::PrevComment => self.jump(n, false, Self::is_thread_head),
            DiffAction::NextEmptyLine => self.jump(n, true, Self::is_empty_line),
            DiffAction::PrevEmptyLine => self.jump(n, false, Self::is_empty_line),
            DiffAction::FindForward => self.mode = Mode::Find { kind: FindKind::Forward, count: n },
            DiffAction::FindBackward => self.mode = Mode::Find { kind: FindKind::Backward, count: n },
            DiffAction::TillForward => self.mode = Mode::Find { kind: FindKind::TillForward, count: n },
            DiffAction::TillBackward => self.mode = Mode::Find { kind: FindKind::TillBackward, count: n },
            DiffAction::RepeatFind => {
                if let Some((kind, c)) = self.last_find {
                    self.find(kind, c, n);
                }
            }
            DiffAction::Search => self.mode = Mode::Search { query: String::new() },
            DiffAction::NextMatch => self.goto_next_match(true, n),
            DiffAction::PrevMatch => self.goto_next_match(false, n),
            DiffAction::FilePalette => self.mode = Mode::Palette(DiffPalette::new(PaletteMode::Files)),
            DiffAction::CommandPalette => self.mode = Mode::Palette(DiffPalette::new(PaletteMode::Commands)),
            DiffAction::Help => self.mode = Mode::Help { scroll: 0 },
            DiffAction::ToggleSplit => {
                self.view = match self.view {
                    ViewMode::Unified => ViewMode::Split,
                    ViewMode::Split => ViewMode::Unified,
                };
                self.reflow();
            }
            DiffAction::NextHunkFilter => {
                self.filter = self.filter.next();
                self.reflow();
            }
            DiffAction::PrevHunkFilter => {
                self.filter = self.filter.prev();
                self.reflow();
            }
            DiffAction::ToggleBlame => return self.toggle_blame(),
            DiffAction::ToggleHunkFold => self.fold_hunk(None),
            DiffAction::CloseHunkFold => self.fold_hunk(Some(true)),
            DiffAction::OpenHunkFold => self.fold_hunk(Some(false)),
            DiffAction::CloseFileFold => self.fold_file(true),
            DiffAction::OpenFileFold => self.fold_file(false),
            DiffAction::CloseAllFolds => {
                self.folds.files = self.changeset.files.iter().map(|f| f.path.clone()).collect();
                self.reflow();
            }
            DiffAction::OpenAllFolds => {
                self.folds.clear();
                self.reflow();
            }
            DiffAction::VisualSelect => {
                self.mode = match self.mode {
                    Mode::Visual { .. } => Mode::Normal,
                    _ => Mode::Visual { anchor: self.cursor },
                };
            }
            DiffAction::Comment => return self.comment(),
            DiffAction::ReplyOrRefresh => return self.reply_or_refresh(),
            DiffAction::ToggleThread => self.toggle_thread(),
            DiffAction::DeleteComment => return self.delete_comment(),
            DiffAction::ClearComments => return self.clear_comments(),
            DiffAction::Yank => return self.yank(),
            DiffAction::YankAll => {
                if self.comments.comments.is_empty() {
                    self.flash = Some("No comments to yank".to_string());
                    return Vec::new();
                }
                return vec![DiffEffect::Yank(format_comments(&self.comments.comments))];
            }
            DiffAction::SendToAgent => match agent_message(&self.comments) {
                Some((message, ids)) => return vec![DiffEffect::SendToAgent { message, ids }],
                None => self.flash = Some("No comments to send".to_string()),
            },
            DiffAction::OpenInEditor => return self.open_in_editor(),
            DiffAction::ToggleViewed => {
                let Some(file) = self.current_file() else { return Vec::new() };
                let viewed = !self.is_viewed(&self.changeset.files[file].clone());
                return self.set_viewed(file, viewed);
            }
            DiffAction::PickCommit => self.mode = Mode::Palette(DiffPalette::new(PaletteMode::Commits)),
            DiffAction::ToggleFileList => self.file_list = !self.file_list,
        }
        Vec::new()
    }

    fn max_top(&self) -> usize {
        self.table.len().saturating_sub(self.height)
    }

    fn keep_cursor_in_view(&mut self) {
        let last = self.table.len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + self.height {
            self.top = self.cursor + 1 - self.height;
        }
        self.top = self.top.min(self.max_top());
    }

    fn move_cursor(&mut self, by: isize) {
        let last = self.table.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
        self.col = 0;
        self.keep_cursor_in_view();
    }

    /// Move the cursor and the view together.
    fn page(&mut self, by: isize) {
        let last = self.table.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(by).min(last);
        self.col = 0;
        self.top = self.top.saturating_add_signed(by).min(self.max_top());
        self.keep_cursor_in_view();
    }

    fn step_file(&mut self, by: isize) {
        let Some(file) = self.current_file() else { return };
        let last = self.changeset.files.len().saturating_sub(1);
        self.goto_file(file.saturating_add_signed(by).min(last));
    }

    /// The first (or next) row where `test` holds, repeated `n` times. A jump
    /// past the last one stays put.
    fn jump(&mut self, n: usize, forward: bool, test: fn(&DiffState, usize) -> bool) {
        for _ in 0..n {
            let found = if forward {
                (self.cursor + 1..self.table.len()).find(|r| test(self, *r))
            } else {
                (0..self.cursor).rev().find(|r| test(self, *r))
            };
            match found {
                Some(row) => {
                    self.cursor = row;
                    self.col = 0;
                }
                None => break,
            }
        }
        self.keep_cursor_in_view();
    }

    /// A changed line's kind in a row, unified or split.
    fn row_changed(&self, row: &Row) -> bool {
        match *row {
            Row::Line { file, hunk, line } => {
                self.line(file, hunk, line).is_some_and(|l| l.kind != LineKind::Context)
            }
            Row::Split { file, hunk, old, new } => [old, new]
                .into_iter()
                .flatten()
                .any(|at| self.line(file, hunk, at).is_some_and(|l| l.kind != LineKind::Context)),
            _ => false,
        }
    }

    /// The first row of a run of changed lines. A thread between two changed
    /// lines doesn't start a new run.
    fn is_change_start(&self, at: usize) -> bool {
        let Some(row) = self.table.rows.get(at) else { return false };
        if !self.row_changed(row) {
            return false;
        }
        let mut before = at;
        while before > 0 {
            before -= 1;
            let prior = &self.table.rows[before];
            if matches!(prior, Row::Thread { .. }) {
                continue;
            }
            return !self.row_changed(prior);
        }
        true
    }

    fn is_thread_head(&self, at: usize) -> bool {
        matches!(self.table.rows.get(at), Some(Row::Thread { part: ThreadPart::Head, .. }))
    }

    fn is_empty_line(&self, at: usize) -> bool {
        match self.table.rows.get(at) {
            Some(row @ (Row::Line { .. } | Row::Split { .. })) => {
                self.row_line(row).is_some_and(|l| l.text.trim().is_empty())
            }
            _ => false,
        }
    }

    /// The line a row is about: its only line, or a split row's new side.
    fn row_line(&self, row: &Row) -> Option<&DiffLine> {
        match *row {
            Row::Line { file, hunk, line } => self.line(file, hunk, line),
            Row::Split { file, hunk, old, new } => self.line(file, hunk, new.or(old)?),
            _ => None,
        }
    }

    /// Move to the `count`th `c` from the cursor, in the line under it. Nothing
    /// to find moves nothing. A till already against its character skips it, so
    /// `;` keeps going.
    fn find(&mut self, kind: FindKind, c: char, count: usize) {
        let Some(row) = self.table.rows.get(self.cursor).copied() else { return };
        let Some(text) = self.row_line(&row).map(|l| l.text.clone()) else { return };
        let mut at = self.col.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        let forward = matches!(kind, FindKind::Forward | FindKind::TillForward);
        let till = matches!(kind, FindKind::TillForward | FindKind::TillBackward);
        let mut from = at;
        if till {
            if forward && text[char_after(&text, at, 1)..].starts_with(c) {
                from = char_after(&text, at, 1);
            }
            if !forward && text[..at].ends_with(c) {
                from = char_before(&text, at);
            }
        }
        for _ in 0..count {
            let hit = if forward {
                let start = char_after(&text, from, 1);
                text[start..].find(c).map(|o| start + o)
            } else {
                text[..from].rfind(c)
            };
            let Some(hit) = hit else { return };
            from = hit;
        }
        self.col = match (forward, till) {
            (_, false) => from,
            (true, true) => char_before(&text, from),
            (false, true) => char_after(&text, from, 1),
        };
    }

    fn goto_next_match(&mut self, forward: bool, n: usize) {
        if self.matches.is_empty() {
            self.flash = Some(if self.query.is_empty() {
                "Nothing to search for: press /".to_string()
            } else {
                format!("Nothing matches {}", self.query)
            });
            return;
        }
        let total = self.matches.len();
        // With no current match, the first step lands on the nearest one.
        let (from, steps) = match self.current_match {
            Some(current) => (current, n),
            None => (self.first_match_from_cursor(forward), n - 1),
        };
        let steps = steps % total;
        self.current_match = Some(if forward { from + steps } else { from + total - steps } % total);
        self.goto_current_match();
    }

    /// The match to start from when there's no current one: the first at or
    /// after the cursor going forward, the last before it going back.
    fn first_match_from_cursor(&self, forward: bool) -> usize {
        let total = self.matches.len();
        let here = self.table.rows.get(self.cursor).map_or((0, 0, 0), |row| match *row {
            Row::Line { file, hunk, line } => (file, hunk, line),
            Row::Split { file, hunk, old, new } => (file, hunk, new.or(old).unwrap_or(0)),
            Row::Hunk { file, hunk, .. } => (file, hunk, 0),
            other => (other.file(), 0, 0),
        });
        let after = self.matches.partition_point(|m| (m.file, m.hunk, m.line) < here);
        if forward {
            after % total
        } else {
            (after + total - 1) % total
        }
    }

    /// Open whatever hides the current match and put the cursor on it.
    fn goto_current_match(&mut self) {
        let Some(m) = self.current_match.and_then(|c| self.matches.get(c)).copied() else { return };
        let path = self.changeset.files[m.file as usize].path.clone();
        let hunk = &self.changeset.files[m.file as usize].hunks[m.hunk as usize];
        let kind = hunk.lines[m.line as usize].kind;
        let (hidden_by_filter, hunk_folded) = (!self.filter.shows(kind), self.folds.hunk_folded(&path, hunk));
        let file_folded = self.folds.file_folded(&path);
        if hidden_by_filter || hunk_folded || file_folded {
            if hidden_by_filter {
                self.filter = HunkFilter::All;
            }
            let hunk = hunk.clone();
            self.folds.set_hunk(&path, &hunk, false);
            self.folds.set_file(&path, false);
            self.rebuild();
        }
        let row = self.table.rows.iter().position(|r| match *r {
            Row::Line { file, hunk, line } => (file, hunk, line) == (m.file, m.hunk, m.line),
            Row::Split { file, hunk, old, new } => {
                (file, hunk) == (m.file, m.hunk) && (old == Some(m.line) || new == Some(m.line))
            }
            _ => false,
        });
        if let Some(row) = row {
            self.cursor = row;
            self.col = m.start as usize;
            self.keep_cursor_in_view();
        }
    }

    fn reveal_file(&mut self, file: usize) {
        let Some(path) = self.changeset.files.get(file).map(|f| f.path.clone()) else { return };
        if self.folds.file_folded(&path) {
            self.folds.set_file(&path, false);
            self.reflow();
        }
    }

    /// The hunk under the cursor.
    fn cursor_hunk(&self) -> Option<(u32, u32)> {
        let row = self.table.rows.get(self.cursor)?;
        Some((row.file(), row.hunk()?))
    }

    fn fold_hunk(&mut self, to: Option<bool>) {
        let Some((file, hunk)) = self.cursor_hunk() else {
            // On a file's header there's no hunk: `za` folds the file.
            if to.is_none() {
                if let Some(Row::File { folded, .. }) = self.table.rows.get(self.cursor).copied() {
                    self.fold_file(!folded);
                }
            }
            return;
        };
        let f = &self.changeset.files[file as usize];
        let (path, hunk) = (f.path.clone(), f.hunks[hunk as usize].clone());
        let folded = to.unwrap_or(!self.folds.hunk_folded(&path, &hunk));
        self.folds.set_hunk(&path, &hunk, folded);
        self.reflow();
    }

    fn fold_file(&mut self, folded: bool) {
        let Some(file) = self.current_file() else { return };
        let path = self.changeset.files[file].path.clone();
        self.folds.set_file(&path, folded);
        self.reflow();
    }

    fn toggle_blame(&mut self) -> Vec<DiffEffect> {
        self.blame_on = !self.blame_on;
        if !self.blame_on {
            return Vec::new();
        }
        self.blame_request().into_iter().collect()
    }

    /// The read for the current file's blame, if it isn't here yet.
    pub fn blame_request(&self) -> Option<DiffEffect> {
        if !self.blame_on {
            return None;
        }
        let file = self.changeset.files.get(self.current_file()?)?;
        if self.blame.contains_key(&file.path) || file.status == super::model::FileStatus::Deleted || file.untracked {
            return None;
        }
        let rev = match &self.changeset.scope {
            CommitScope::Commit(sha) => Some(sha.clone()),
            // A branch's worktree, as it is now: uncommitted lines read so.
            _ => None,
        };
        Some(DiffEffect::LoadBlame { path: file.path.clone(), rev })
    }

    /// The thread the cursor is on: its row, or the line it hangs from.
    fn thread_at_cursor(&self) -> Option<(usize, Option<usize>)> {
        match self.table.rows.get(self.cursor)? {
            Row::Thread { thread, part, .. } => {
                let (comment, _) = self.threads.placed.get(*thread as usize)?;
                let reply = match part {
                    ThreadPart::Reply(r) => Some(*r as usize),
                    _ => None,
                };
                Some((*comment, reply))
            }
            row @ (Row::Line { .. } | Row::Split { .. }) => {
                let (f, h, lines) = match *row {
                    Row::Line { file, hunk, line } => (file, hunk, [Some(line), None]),
                    Row::Split { file, hunk, old, new } => (file, hunk, [new, old]),
                    _ => unreachable!(),
                };
                lines.into_iter().flatten().find_map(|l| {
                    self.threads.placed.iter().find_map(|(comment, place)| {
                        (place.file == f && place.at == Some((h, l))).then_some((*comment, None))
                    })
                })
            }
            _ => None,
        }
    }

    fn comment(&mut self) -> Vec<DiffEffect> {
        if let Mode::Visual { anchor } = self.mode {
            let start = anchor.min(self.cursor);
            let end = anchor.max(self.cursor);
            self.mode = Mode::Normal;
            return self.comment_on_rows(start, end);
        }
        if let Some((comment, reply)) = self.thread_at_cursor() {
            let c = &self.comments.comments[comment];
            let (id, reply, text) = match reply {
                Some(r) => (c.id.clone(), Some(c.replies[r].id.clone()), c.replies[r].body.clone()),
                None => (c.id.clone(), None, c.body.clone()),
            };
            self.mode = Mode::Comment(Box::new(CommentDraft::new(DraftTarget::Edit { id, reply }, &text)));
            return Vec::new();
        }
        self.comment_on_rows(self.cursor, self.cursor)
    }

    /// Start a new thread on rows `start..=end`, staying in the first's file.
    fn comment_on_rows(&mut self, start: usize, end: usize) -> Vec<DiffEffect> {
        let Some(first) = self.table.rows.get(start) else { return Vec::new() };
        let file = first.file();
        let mut new_side = Vec::new();
        let mut old_side = Vec::new();
        for row in &self.table.rows[start..=end.min(self.table.len() - 1)] {
            if row.file() != file {
                break;
            }
            match *row {
                Row::Line { file, hunk, line } => {
                    if let Some(l) = self.line(file, hunk, line) {
                        new_side.extend(l.new);
                        old_side.extend(l.old);
                    }
                }
                Row::Split { file, hunk, old, new } => {
                    new_side.extend(new.and_then(|n| self.line(file, hunk, n)).and_then(|l| l.new));
                    old_side.extend(old.and_then(|o| self.line(file, hunk, o)).and_then(|l| l.old));
                }
                _ => {}
            }
        }
        // The new side, unless the rows are nothing but deleted lines.
        let (side, numbers) = match (new_side.is_empty(), old_side.is_empty()) {
            (false, _) => (Side::New, new_side),
            (true, false) => (Side::Old, old_side),
            (true, true) => {
                self.flash = Some("Move onto a line to comment".to_string());
                return Vec::new();
            }
        };
        let (first_line, last_line) = (numbers[0], numbers[numbers.len() - 1]);
        let Some(anchor) = anchor_for(&self.changeset.files[file as usize], side, first_line, last_line) else {
            self.flash = Some("A comment's lines have to stay within one hunk".to_string());
            return Vec::new();
        };
        self.mode = Mode::Comment(Box::new(CommentDraft::new(DraftTarget::New(anchor), "")));
        Vec::new()
    }

    fn reply_or_refresh(&mut self) -> Vec<DiffEffect> {
        match self.thread_at_cursor() {
            Some((comment, _)) => {
                let id = self.comments.comments[comment].id.clone();
                self.mode = Mode::Comment(Box::new(CommentDraft::new(DraftTarget::Reply { id }, "")));
                Vec::new()
            }
            None => vec![DiffEffect::Refresh],
        }
    }

    fn toggle_thread(&mut self) {
        let Some((comment, _)) = self.thread_at_cursor() else { return };
        let id = self.comments.comments[comment].id.clone();
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        self.reflow();
    }

    fn delete_comment(&mut self) -> Vec<DiffEffect> {
        let Some((comment, reply)) = self.thread_at_cursor() else { return Vec::new() };
        let c = &self.comments.comments[comment];
        let op = CommentOp::Delete { id: c.id.clone(), reply: reply.map(|r| c.replies[r].id.clone()) };
        self.apply_comment_op(&op);
        vec![DiffEffect::SaveComment(op)]
    }

    /// `D` clears everything, so it takes two: the first says what the second
    /// will do.
    fn clear_comments(&mut self) -> Vec<DiffEffect> {
        let count = self.comments.comments.len();
        if count == 0 {
            self.flash = Some("No comments to clear".to_string());
            return Vec::new();
        }
        if !self.confirm_clear {
            self.confirm_clear = true;
            self.flash = Some(format!("Press D again to clear {count} comments"));
            return Vec::new();
        }
        self.confirm_clear = false;
        let op = CommentOp::Clear;
        self.apply_comment_op(&op);
        vec![DiffEffect::SaveComment(op)]
    }

    fn yank(&mut self) -> Vec<DiffEffect> {
        if let Mode::Visual { anchor } = self.mode {
            let (start, end) = (anchor.min(self.cursor), anchor.max(self.cursor));
            let lines: Vec<String> = self.table.rows[start..=end.min(self.table.len().saturating_sub(1))]
                .iter()
                .filter_map(|r| self.row_line(r).map(|l| l.text.clone()))
                .collect();
            self.mode = Mode::Normal;
            return vec![DiffEffect::Yank(lines.join("\n"))];
        }
        match self.thread_at_cursor() {
            Some((comment, _)) => vec![DiffEffect::Yank(format_comment(&self.comments.comments[comment]))],
            None => {
                self.flash = Some("No comment here to yank".to_string());
                Vec::new()
            }
        }
    }

    fn open_in_editor(&mut self) -> Vec<DiffEffect> {
        let Some(row) = self.table.rows.get(self.cursor).copied() else { return Vec::new() };
        let file = &self.changeset.files[row.file() as usize];
        if file.status == super::model::FileStatus::Deleted {
            self.flash = Some("This file is gone from the worktree".to_string());
            return Vec::new();
        }
        let line = self.row_line(&row).and_then(|l| l.new.or(l.old)).or_else(|| {
            row.hunk().map(|h| file.hunks[h as usize].new_start)
        });
        vec![DiffEffect::OpenInEditor { path: file.path.clone(), line }]
    }

    // ---- Rows ----------------------------------------------------------------

    fn thread_row(&self, id: &str) -> Option<usize> {
        let thread = self.threads.placed.iter().position(|(c, _)| self.comments.comments[*c].id == id)? as u32;
        self.table
            .rows
            .iter()
            .position(|r| matches!(r, Row::Thread { thread: t, part: ThreadPart::Head, .. } if *t == thread))
    }

    /// Rebuild keeping the cursor where it is in the text.
    fn reflow(&mut self) {
        let spot = self.spot();
        self.rebuild();
        self.restore(&spot);
        self.keep_cursor_in_view();
    }

    fn rebuild(&mut self) {
        let show_outdated = self.changeset.scope == CommitScope::All;
        self.threads = Threads::new(&self.comments.comments, &self.places, self.expanded.clone(), show_outdated);
        self.table = build_rows(&RowInput {
            changeset: &self.changeset,
            mode: self.view,
            filter: self.filter,
            folds: &self.folds,
            threads: &self.threads,
        });
    }

    fn spot(&self) -> Spot {
        let Some(row) = self.table.rows.get(self.cursor) else { return Spot::Nothing };
        let Some(file) = self.changeset.files.get(row.file() as usize) else { return Spot::Nothing };
        let path = file.path.clone();
        match *row {
            Row::File { .. } | Row::Notice { .. } => Spot::File { path },
            Row::Hunk { hunk, .. } => Spot::Hunk { path, old_start: file.hunks[hunk as usize].old_start },
            Row::Line { .. } | Row::Split { .. } => {
                let hunk = row.hunk().unwrap_or(0) as usize;
                let old_start = file.hunks[hunk].old_start;
                match self.row_line(row) {
                    Some(line) => {
                        let side = line.side();
                        let number = line.number(side).unwrap_or(0);
                        Spot::Line { path, old_start, side, number, text: line.text.clone() }
                    }
                    None => Spot::Hunk { path, old_start },
                }
            }
            Row::Thread { thread, .. } => match self.thread(thread) {
                Some(comment) => Spot::Thread { id: comment.id.clone(), path },
                None => Spot::File { path },
            },
        }
    }

    /// Put the cursor back on `spot`, or on what's left of it: the hunk, the
    /// file, or the same place in the list.
    fn restore(&mut self, spot: &Spot) {
        let (path, found) = match spot {
            Spot::Nothing => return,
            Spot::Thread { id, path } => (path, self.thread_row(id)),
            Spot::File { path } => (path, None),
            Spot::Hunk { path, old_start } => (path, self.hunk_row(path, *old_start)),
            Spot::Line { path, old_start, side, number, text } => {
                let line = self.line_row(path, *side, *number, text);
                (path, line.or_else(|| self.hunk_row(path, *old_start)))
            }
        };
        if let Some(row) = found {
            self.cursor = row;
            return;
        }
        if let Some(file) = self.changeset.file_index(path) {
            self.cursor = self.table.file_rows.get(file).map_or(0, |r| *r as usize);
            return;
        }
        self.cursor = self.cursor.min(self.table.len().saturating_sub(1));
    }

    fn file_range(&self, path: &str) -> Option<std::ops::Range<usize>> {
        let file = self.changeset.file_index(path)?;
        let start = *self.table.file_rows.get(file)? as usize;
        let end = self.table.file_rows.get(file + 1).map_or(self.table.len(), |r| *r as usize);
        Some(start..end)
    }

    fn hunk_row(&self, path: &str, old_start: u32) -> Option<usize> {
        let file = self.changeset.file_index(path)?;
        let range = self.file_range(path)?;
        self.table.rows[range.clone()]
            .iter()
            .position(|r| match *r {
                Row::Hunk { hunk, .. } => self.changeset.files[file].hunks[hunk as usize].old_start == old_start,
                _ => false,
            })
            .map(|at| range.start + at)
    }

    /// The row of the line `number` on `side` with this text, or the nearest
    /// with this text (lines above it came or went), or whatever's now at
    /// `number`.
    fn line_row(&self, path: &str, side: Side, number: u32, text: &str) -> Option<usize> {
        let range = self.file_range(path)?;
        let mut same_text: Option<(u32, usize)> = None;
        let mut same_number = None;
        for at in range {
            let lines: [Option<&DiffLine>; 2] = match self.table.rows[at] {
                Row::Line { file, hunk, line } => [self.line(file, hunk, line), None],
                Row::Split { file, hunk, old, new } => [
                    old.and_then(|o| self.line(file, hunk, o)),
                    new.and_then(|n| self.line(file, hunk, n)),
                ],
                _ => continue,
            };
            for line in lines.into_iter().flatten() {
                let Some(here) = line.number(side) else { continue };
                if here == number {
                    same_number = same_number.or(Some(at));
                }
                let distance = here.abs_diff(number);
                if line.text == text && same_text.is_none_or(|(d, _)| distance < d) {
                    same_text = Some((distance, at));
                }
            }
        }
        same_text.map(|(_, at)| at).or(same_number)
    }
}

/// `Char('G')` with no Shift, or `Shift` with a symbol, reads as the keymap
/// spells them: terminals and window systems disagree. Letters become lowercase
/// plus Shift; a shifted symbol (`?`, `{`) is just the symbol.
pub fn normalize_key(chord: KeyChord) -> KeyChord {
    match chord.code {
        Key::Char(c) if c.is_ascii_uppercase() => {
            KeyChord::new(Key::Char(c.to_ascii_lowercase()), chord.mods | Mods::SHIFT)
        }
        Key::Char(c) if !c.is_alphanumeric() && chord.mods.contains(Mods::SHIFT) => {
            KeyChord::new(Key::Char(c), chord.mods.without(Mods::SHIFT))
        }
        _ => chord,
    }
}

/// A [`normalize_key`]d chord as text entry reads it: the capital, no Shift.
fn typed(chord: KeyChord) -> KeyChord {
    match chord.code {
        Key::Char(c) => KeyChord::new(Key::Char(with_shift(c, chord.mods)), chord.mods.without(Mods::SHIFT)),
        _ => chord,
    }
}

/// The character a [`normalize_key`]d chord types.
fn with_shift(c: char, mods: Mods) -> char {
    if mods.contains(Mods::SHIFT) {
        c.to_ascii_uppercase()
    } else {
        c
    }
}

/// The byte offset `n` characters after `at`.
fn char_after(text: &str, at: usize, n: usize) -> usize {
    text[at..].char_indices().nth(n).map_or(text.len(), |(o, _)| at + o)
}

/// The byte offset one character before `at`.
fn char_before(text: &str, at: usize) -> usize {
    text[..at].char_indices().next_back().map_or(0, |(o, _)| o)
}
