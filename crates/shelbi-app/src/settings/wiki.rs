//! Settings ▸ Wiki's shared form (`wiki-settings`): where the project's wiki
//! lives (the `wiki:` block of the project YAML) and where its sync stands,
//! with Sync now.
//!
//! ```text
//!  Wiki
//!  Where this project's wiki lives, and whether Shelbi syncs it on its own.
//!
//!  Source     (•) Orphan branch  ( ) Repo folder
//!  Branch     [ docs/planning              ]
//!  Auto-sync  [x]
//!  Status     ● Synced · 2m ago   [ Sync now ]
//! ```
//!
//! The block lives beside `orchestrator.awareness:` in the project YAML, so
//! the form loads and saves it through the project's [`WorkflowConfig`]
//! ([`WorkflowConfig::wiki`], [`WorkflowConfig::set_wiki`]): the same
//! hash-checked, comment-preserving plan the Orchestrator and Properties
//! pages send to the daemon that owns the project, here or on the far
//! machine. The status and Sync now come from that machine's hub wiki
//! service ([`crate::wiki`]), through an [`Ask`] the client builds for the
//! window's project.
//!
//! Saving works as the Orchestrator and Properties pages' do, once the
//! project has a wiki: the radio and the checkbox save as they're picked,
//! the Branch / Folder field when it's committed (Enter, or focus leaving
//! it), each value checked first ([`WikiConfig::validate`]): a refused one
//! keeps its text and shows its error under the field. A project with no
//! wiki yet (no block, and nothing set up on the hub) gets nothing written
//! as the controls change: they set up the wiki the Set up wiki button
//! asks for, which writes the block and asks the hub to set it up (as
//! `shelbi wiki init` does). Existing projects get nothing until they opt
//! in.
//!
//! The page's work (reads, saves, Sync now, Set up) is queued as
//! [`WikiWork`] for the client to run off the UI thread and hand back with
//! [`WikiForm::done`], tagged with the form's generation so a result for a
//! project no longer shown is dropped.
//!
//! Toolkit-free: the TUI's and the desktop app's Wiki pages both draw
//! [`WikiForm`] with the labels here.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use shelbi_core::wiki::{DEFAULT_WIKI_BRANCH, DEFAULT_WIKI_PATH};
use shelbi_core::{WikiConfig, WikiSource, WikiSyncMode};
use shelbi_state::workflow_config::WorkflowConfig;

use crate::settings::orchestrator::SaveStatus;
use crate::settings::store::{self, ConfigStore};
use crate::settings::summary::display_path;
use crate::wiki::{SyncOp, SyncState, WikiReply, WikiRequest, WikiStatus};

/// The page's title.
pub const TITLE: &str = "Wiki";
/// The line under the title.
pub const HELP: &str = "Where this project's wiki lives (its plans, decisions and docs), and whether Shelbi syncs it on its own.";
/// The source radio's label.
pub const SOURCE_LABEL: &str = "Source";
/// The Auto-sync checkbox's label.
pub const AUTO_SYNC_LABEL: &str = "Auto-sync";
/// Its help line.
pub const AUTO_SYNC_HELP: &str =
    "Commit a few seconds after each change, and pull and push on a timer. Off, the wiki syncs on Sync now only.";
/// The status row's label.
pub const STATUS_LABEL: &str = "Status";
/// The status row's button once the wiki is set up.
pub const SYNC_NOW_LABEL: &str = "Sync now";
/// The status row's button before it is.
pub const SET_UP_LABEL: &str = "Set up wiki";

/// The sources, in the order the radio lists them.
pub const SOURCES: [WikiSource; 2] = WikiSource::ALL;

/// A source's radio label.
pub fn source_label(source: WikiSource) -> &'static str {
    match source {
        WikiSource::Branch => "Orphan branch",
        WikiSource::Folder => "Repo folder",
    }
}

/// A source's description.
pub fn source_help(source: WikiSource) -> &'static str {
    match source {
        WikiSource::Branch => "A branch of its own, with no history shared with the code, that the hub checks out beside the repo.",
        WikiSource::Folder => "A folder of the main repo. Edits are committed to one rolling pull request.",
    }
}

/// The location field's label for `source`.
pub fn location_label(source: WikiSource) -> &'static str {
    match source {
        WikiSource::Branch => "Branch",
        WikiSource::Folder => "Folder",
    }
}

/// The location field's help for `source`.
pub fn location_help(source: WikiSource) -> &'static str {
    match source {
        WikiSource::Branch => "The orphan branch the wiki lives on. Created (and pushed) if origin doesn't have it.",
        WikiSource::Folder => "The folder, relative to the repo's root.",
    }
}

/// What an empty location field stands for.
pub fn placeholder(source: WikiSource) -> &'static str {
    match source {
        WikiSource::Branch => DEFAULT_WIKI_BRANCH,
        WikiSource::Folder => DEFAULT_WIKI_PATH,
    }
}

/// `cfg` with its source's location spelled out (the default when it names
/// none): what Set up wiki and `shelbi wiki init` write, so the block says
/// where the wiki is.
pub fn filled(mut cfg: WikiConfig) -> WikiConfig {
    match cfg.source {
        WikiSource::Branch => {
            cfg.branch.get_or_insert_with(|| DEFAULT_WIKI_BRANCH.to_string());
        }
        WikiSource::Folder => {
            cfg.path.get_or_insert_with(|| DEFAULT_WIKI_PATH.to_string());
        }
    }
    cfg
}

/// A source's location as the block has it: its branch or its path.
fn location(cfg: &WikiConfig) -> Option<&str> {
    match cfg.source {
        WikiSource::Branch => cfg.branch.as_deref(),
        WikiSource::Folder => cfg.path.as_deref(),
    }
}

/// Asks the project's hub wiki service (blocking): [`crate::wiki::ask`]
/// through the window's link, a fake under test.
pub type Ask = Arc<dyn Fn(WikiRequest) -> WikiReply + Send + Sync>;

/// Work the page needs run off the UI thread.
#[derive(Debug, Clone)]
pub enum WikiWork {
    /// Read the project's config (the `wiki:` block).
    Load,
    /// Read where the hub's sync stands.
    Status,
    /// Write the block.
    Save(WorkflowConfig),
    /// Commit, pull and push now.
    SyncNow,
    /// Write the block (when it's given) and ask the hub to set the wiki up.
    SetUp(Option<WorkflowConfig>),
}

/// Its outcome, handed back to [`WikiForm::done`].
#[derive(Debug, Clone)]
pub enum WikiDone {
    Loaded(Result<WorkflowConfig, String>),
    Status(WikiReply),
    Saved(Result<WorkflowConfig, String>),
    Synced(WikiReply),
    /// The block's write (when one was made), then the hub's answer (none
    /// when the write failed).
    SetUp {
        saved: Option<Result<WorkflowConfig, String>>,
        reply: Option<WikiReply>,
    },
}

impl WikiWork {
    /// Run it (blocking): config through `store`, the wiki through `ask`.
    pub fn run(self, store: &dyn ConfigStore, ask: &Ask) -> WikiDone {
        match self {
            WikiWork::Load => WikiDone::Loaded(store.workflows()),
            WikiWork::Status => WikiDone::Status(ask(WikiRequest::Status)),
            WikiWork::Save(config) => WikiDone::Saved(save(store, &config)),
            WikiWork::SyncNow => WikiDone::Synced(ask(WikiRequest::Sync(SyncOp::Now))),
            WikiWork::SetUp(config) => {
                let saved = config.map(|c| save(store, &c));
                let reply = match &saved {
                    Some(Err(_)) => None,
                    _ => Some(ask(WikiRequest::Init)),
                };
                WikiDone::SetUp { saved, reply }
            }
        }
    }
}

/// Write `config` through `store` and read it back. Blocking.
pub fn save(store: &dyn ConfigStore, config: &WorkflowConfig) -> Result<WorkflowConfig, String> {
    store::save_workflows(store, config)
        .map(|saved| saved.config)
        .map_err(|e| e.to_string())
}

/// How a status line reads: its color's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Synced.
    Good,
    /// Syncing, pending, checking.
    Busy,
    /// Paused, offline, not set up.
    Muted,
    /// A conflict, an error, the hub out of reach.
    Bad,
}

/// The status row's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub tone: Tone,
    pub text: String,
}

/// The status row's button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    SyncNow,
    SetUp,
}

impl Button {
    pub fn label(self) -> &'static str {
        match self {
            Button::SyncNow => SYNC_NOW_LABEL,
            Button::SetUp => SET_UP_LABEL,
        }
    }
}

/// The Wiki page's state: the project's config as read, the block shown
/// (being saved, or the setup draft), the location field as typed, the
/// hub's status, and the work in flight.
#[derive(Debug, Clone, Default)]
pub struct WikiForm {
    /// Bumped by [`Self::reset`]: work handed out before it is dropped.
    generation: u64,
    /// `None` until it's read, or while it doesn't read.
    config: Option<WorkflowConfig>,
    unreadable: Option<String>,
    /// The block being written.
    pending: Option<WikiConfig>,
    /// The choices made before the wiki is set up (nothing is written).
    draft: Option<WikiConfig>,
    /// The location field as typed, once it's been edited.
    text: Option<String>,
    error: Option<String>,
    status: SaveStatus,
    /// The hub's answer: `None` until it's read; `Err` when it couldn't be.
    hub: Option<Result<WikiStatus, String>>,
    syncing: bool,
    setting_up: bool,
    /// Why the last Sync now or Set up failed.
    sync_error: Option<String>,
    work: Vec<WikiWork>,
}

impl WikiForm {
    /// The form's generation: the client tags the work it takes with it and
    /// hands it back with its result.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Forget everything (another project is shown, or none). Work in
    /// flight is dropped when it lands.
    pub fn reset(&mut self) {
        let generation = self.generation + 1;
        *self = WikiForm {
            generation,
            ..WikiForm::default()
        };
    }

    /// A visit to the page: read the config and the hub's status again (a
    /// hand edit, a sync made elsewhere), and forget the last visit's lines.
    pub fn visit(&mut self) {
        if self.pending.is_none() {
            self.status = SaveStatus::Idle;
        }
        if !self.syncing && !self.setting_up {
            self.sync_error = None;
        }
        self.queue(WikiWork::Load);
        self.queue(WikiWork::Status);
    }

    /// Read the hub's status again (the hub announced a change).
    pub fn refresh_status(&mut self) {
        self.queue(WikiWork::Status);
    }

    /// Queue `work`; a read already queued isn't queued twice.
    fn queue(&mut self, work: WikiWork) {
        let read = matches!(work, WikiWork::Load | WikiWork::Status);
        let queued = self.work.iter().any(|w| std::mem::discriminant(w) == std::mem::discriminant(&work));
        if !(read && queued) {
            self.work.push(work);
        }
    }

    /// Whether the page has work for the client.
    pub fn work_due(&self) -> bool {
        !self.work.is_empty()
    }

    /// The work to run now, each to hand back with [`Self::done`] under
    /// [`Self::generation`].
    pub fn take_work(&mut self) -> Vec<WikiWork> {
        std::mem::take(&mut self.work)
    }

    /// A piece of work landed. Returns whether it was taken (a stale one,
    /// from before a [`Self::reset`], is dropped).
    pub fn done(&mut self, generation: u64, done: WikiDone) -> bool {
        if generation != self.generation {
            return false;
        }
        match done {
            WikiDone::Loaded(read) => self.load(read),
            WikiDone::Status(reply) => self.hub = Some(status_of(reply)),
            WikiDone::Saved(result) => {
                self.saved_result(result);
                // The hub starts (or restarts) the service on the new block.
                self.queue(WikiWork::Status);
            }
            WikiDone::Synced(reply) => {
                self.syncing = false;
                match status_of(reply) {
                    Ok(s) => self.hub = Some(Ok(s)),
                    Err(e) => self.sync_error = Some(e),
                }
            }
            WikiDone::SetUp { saved, reply } => {
                self.setting_up = false;
                if let Some(result) = saved {
                    self.saved_result(result);
                }
                match reply.map(status_of) {
                    Some(Ok(s)) => {
                        self.draft = None;
                        self.hub = Some(Ok(s));
                    }
                    Some(Err(e)) => self.sync_error = Some(e),
                    // The write failed: its status says why.
                    None => {}
                }
            }
        }
        true
    }

    // --- reading -----------------------------------------------------------------

    /// The config was read. Ignored while a save is in flight (its result
    /// is newer). An edited location keeps its text.
    pub fn load(&mut self, read: Result<WorkflowConfig, String>) {
        if self.pending.is_some() {
            return;
        }
        match read {
            Ok(config) => {
                self.config = Some(config);
                self.unreadable = None;
                if self.text.as_deref() == Some(self.saved_text().as_str()) {
                    self.text = None;
                }
            }
            Err(e) => {
                self.config = None;
                self.unreadable = Some(e);
            }
        }
    }

    /// Whether the config has been read (or failed to).
    pub fn loaded(&self) -> bool {
        self.config.is_some() || self.unreadable.is_some()
    }

    /// Why the config doesn't read, if it doesn't.
    pub fn unreadable(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    /// Why the file's `wiki:` block doesn't parse, if it doesn't (a change
    /// here replaces it).
    pub fn block_unreadable(&self) -> Option<&str> {
        self.config.as_ref().and_then(WorkflowConfig::wiki_unreadable)
    }

    /// The config as last read or saved.
    pub fn config(&self) -> Option<&WorkflowConfig> {
        self.config.as_ref()
    }

    /// The file's block, if it has one.
    fn file_block(&self) -> Option<&WikiConfig> {
        self.config.as_ref().and_then(WorkflowConfig::wiki)
    }

    /// The hub's status, once read.
    pub fn hub(&self) -> Option<&Result<WikiStatus, String>> {
        self.hub.as_ref()
    }

    /// Whether the hub has the wiki set up.
    pub fn set_up(&self) -> bool {
        matches!(&self.hub, Some(Ok(s)) if s.set_up)
    }

    /// Whether a change saves at once: the project has a wiki (a block, or
    /// one the hub has set up). Before, changes are the setup's draft.
    pub fn live(&self) -> bool {
        self.file_block().is_some() || self.set_up()
    }

    /// Whether the controls take a change: the config read, and no save or
    /// setup in flight.
    pub fn editable(&self) -> bool {
        self.config.is_some() && self.pending.is_none() && !self.setting_up
    }

    /// Whether a save is in flight.
    pub fn saving(&self) -> bool {
        self.pending.is_some()
    }

    /// The block the controls show: the one being saved, else the setup's
    /// draft, else the file's, else the default.
    pub fn shown(&self) -> WikiConfig {
        self.pending
            .clone()
            .or_else(|| self.draft.clone())
            .or_else(|| self.file_block().cloned())
            .unwrap_or_default()
    }

    pub fn source(&self) -> WikiSource {
        self.shown().source
    }

    pub fn auto(&self) -> bool {
        self.shown().sync == WikiSyncMode::Auto
    }

    /// The location field's text: as typed, else the block's.
    pub fn text(&self) -> String {
        self.text.clone().unwrap_or_else(|| self.saved_text())
    }

    /// The location as the shown block has it (empty for the default).
    fn saved_text(&self) -> String {
        location(&self.shown()).unwrap_or("").to_string()
    }

    /// The location field's error.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the location field differs from the block.
    pub fn dirty(&self) -> bool {
        self.text.as_ref().is_some_and(|t| t.trim() != self.saved_text())
    }

    // --- editing -----------------------------------------------------------------

    /// Pick `source`: saved once the project has a wiki.
    pub fn choose_source(&mut self, source: WikiSource) {
        if !self.editable() || self.source() == source {
            return;
        }
        self.text = None;
        self.error = None;
        let mut next = self.shown();
        next.source = source;
        self.apply(next);
    }

    /// The source `steps` places from the shown one, wrapping (the radio's
    /// arrow keys).
    pub fn step_source(&mut self, steps: isize) {
        let at = SOURCES.iter().position(|s| *s == self.source()).unwrap_or(0) as isize;
        self.choose_source(SOURCES[(at + steps).rem_euclid(SOURCES.len() as isize) as usize]);
    }

    /// Turn Auto-sync on or off.
    pub fn toggle_auto(&mut self) {
        if !self.editable() {
            return;
        }
        let mut next = self.shown();
        next.sync = match next.sync {
            WikiSyncMode::Auto => WikiSyncMode::Manual,
            WikiSyncMode::Manual => WikiSyncMode::Auto,
        };
        self.apply(next);
    }

    /// Type into the location field. Its last error goes until it's
    /// checked again.
    pub fn set_text(&mut self, text: &str) {
        if !self.editable() {
            return;
        }
        self.text = Some(text.to_string());
        self.error = None;
    }

    /// Put the location field back on the block's (Esc).
    pub fn revert_text(&mut self) {
        self.text = None;
        self.error = None;
    }

    /// The location field was committed (Enter, focus leaving it): checked,
    /// then saved (or drafted) when it changed. A refused value keeps its
    /// text and its error.
    pub fn commit_text(&mut self) {
        if !self.editable() || !self.dirty() {
            return;
        }
        let typed = self.text().trim().to_string();
        let value = (!typed.is_empty()).then_some(typed);
        let mut next = self.shown();
        match next.source {
            WikiSource::Branch => next.branch = value,
            WikiSource::Folder => next.path = value,
        }
        if let Err(e) = next.validate() {
            self.error = Some(sentence(&e.to_string()));
            return;
        }
        self.text = None;
        self.apply(next);
    }

    /// Show `next`: saved when the project has a wiki, drafted otherwise.
    fn apply(&mut self, next: WikiConfig) {
        if !self.live() {
            self.draft = Some(next);
            return;
        }
        if self.file_block() == Some(&next) {
            return;
        }
        let Some(config) = self.config.as_ref() else { return };
        match config.set_wiki(Some(next.clone())) {
            Ok(config) => {
                self.pending = Some(next);
                self.status = SaveStatus::Saving;
                self.work.push(WikiWork::Save(config));
            }
            Err(e) => self.status = SaveStatus::Failed(sentence(&e.to_string())),
        }
    }

    /// The save landed: on success the form holds the config as now on
    /// disk; on failure the controls go back to the file's and the status
    /// says why.
    fn saved_result(&mut self, result: Result<WorkflowConfig, String>) {
        self.pending = None;
        match result {
            Ok(config) => {
                self.config = Some(config);
                self.status = SaveStatus::Saved;
            }
            Err(e) => self.status = SaveStatus::Failed(e),
        }
    }

    pub fn status(&self) -> &SaveStatus {
        &self.status
    }

    /// The save's line on the page, if any.
    pub fn status_message(&self) -> Option<String> {
        match &self.status {
            SaveStatus::Idle => None,
            SaveStatus::Saving => Some("Saving…".into()),
            SaveStatus::Saved => Some(match &self.config {
                Some(c) => format!("Saved to {}", display_path(c.project_path())),
                None => "Saved".into(),
            }),
            SaveStatus::Failed(e) => Some(e.clone()),
        }
    }

    // --- the status row ----------------------------------------------------------

    /// The status row's button, and whether it takes a press now.
    pub fn button(&self) -> (Button, bool) {
        if self.set_up() {
            (Button::SyncNow, !self.syncing)
        } else {
            let ready = self.config.is_some() && !self.setting_up && self.pending.is_none() && self.hub.is_some();
            (Button::SetUp, ready && !matches!(self.hub, Some(Err(_))))
        }
    }

    /// Press the status row's button: Sync now, or Set up wiki.
    pub fn press(&mut self) {
        match self.button() {
            (_, false) => {}
            (Button::SyncNow, true) => {
                self.syncing = true;
                self.sync_error = None;
                self.work.push(WikiWork::SyncNow);
            }
            (Button::SetUp, true) => {
                // Commit what's typed first, so the setup takes it.
                if self.dirty() {
                    self.commit_text();
                    if self.error.is_some() {
                        return;
                    }
                }
                let block = filled(self.shown());
                let write = match self.config.as_ref() {
                    Some(c) if c.wiki() != Some(&block) => match c.set_wiki(Some(block.clone())) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            self.error = Some(sentence(&e.to_string()));
                            return;
                        }
                    },
                    _ => None,
                };
                if write.is_some() {
                    self.pending = Some(block);
                    self.status = SaveStatus::Saving;
                }
                self.setting_up = true;
                self.sync_error = None;
                self.work.push(WikiWork::SetUp(write));
            }
        }
    }

    /// Whether Sync now is running.
    pub fn syncing(&self) -> bool {
        self.syncing
    }

    /// Whether Set up wiki is running.
    pub fn setting_up(&self) -> bool {
        self.setting_up
    }

    /// Why the last Sync now or Set up failed.
    pub fn sync_error(&self) -> Option<&str> {
        self.sync_error.as_deref()
    }

    /// The status row's text at `now`.
    pub fn status_line(&self, now: DateTime<Utc>) -> StatusLine {
        let line = |tone, text: &str| StatusLine { tone, text: text.to_string() };
        if self.setting_up {
            return line(Tone::Busy, "Setting up…");
        }
        if self.syncing {
            return line(Tone::Busy, "Syncing…");
        }
        let s = match &self.hub {
            None => return line(Tone::Busy, "Checking…"),
            Some(Err(e)) => return line(Tone::Bad, &format!("Couldn't reach the hub: {e}")),
            Some(Ok(s)) => s,
        };
        if !s.set_up {
            return line(
                Tone::Muted,
                if self.file_block().is_some() {
                    "Not set up yet"
                } else {
                    "Not set up. This project has no wiki yet."
                },
            );
        }
        let (tone, word) = match &s.state {
            SyncState::Synced => (Tone::Good, "Synced".to_string()),
            SyncState::Syncing => (Tone::Busy, "Syncing".to_string()),
            SyncState::Pending => (Tone::Busy, "Changes waiting to sync".to_string()),
            SyncState::Offline => (Tone::Muted, "Offline".to_string()),
            SyncState::Conflict => (
                Tone::Bad,
                match s.conflicts.len() {
                    1 => "1 file held for a conflict".to_string(),
                    n => format!("{n} files held for a conflict"),
                },
            ),
            SyncState::Error(detail) => (Tone::Bad, format!("Sync failed: {detail}")),
        };
        let mut text = word;
        if let Some(at) = s.last_synced.as_deref().and_then(|t| DateTime::parse_from_rfc3339(t).ok()) {
            if matches!(s.state, SyncState::Synced | SyncState::Pending | SyncState::Offline) {
                text.push_str(" · ");
                text.push_str(&crate::jobs::age_label(at.with_timezone(&Utc), now));
            }
        }
        let tone = if s.paused && tone == Tone::Good { Tone::Muted } else { tone };
        if s.paused {
            text.push_str(" · Paused");
        } else if !s.auto {
            text.push_str(" · Manual");
        }
        StatusLine { tone, text }
    }

    /// Folder mode's rolling pull request, once one is open.
    pub fn pull_request(&self) -> Option<&str> {
        match &self.hub {
            Some(Ok(s)) => s.pull_request.as_deref(),
            _ => None,
        }
    }

    /// Where the wiki is on the project's machine, once set up.
    pub fn root(&self) -> Option<&str> {
        match &self.hub {
            Some(Ok(s)) if s.set_up && !s.root.is_empty() => Some(s.root.as_str()),
            _ => None,
        }
    }
}

/// A status reply's status, or why there's none.
fn status_of(reply: WikiReply) -> Result<WikiStatus, String> {
    match reply {
        WikiReply::Status(s) => Ok(s),
        WikiReply::Failed(f) => Err(f.message),
        other => Err(format!("the hub answered something else ({other:?})")),
    }
}

/// `msg` as a sentence: its first letter capitalized.
fn sentence(msg: &str) -> String {
    let mut chars = msg.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::{FailureKind, WikiReply};
    use std::sync::Mutex;

    fn status(set_up: bool, state: SyncState) -> WikiStatus {
        WikiStatus {
            set_up,
            state,
            auto: true,
            source: "branch".into(),
            branch: "docs/planning".into(),
            root: "/home/u/.shelbi/projects/p/wiki".into(),
            last_synced: Some("2026-11-27T12:00:00Z".into()),
            ..WikiStatus::not_set_up()
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-27T12:02:30Z").unwrap().with_timezone(&Utc)
    }

    /// Run the form's work against `store` and `ask`, handing each back.
    fn drain(form: &mut WikiForm, store: &dyn ConfigStore, ask: &Ask) {
        while form.work_due() {
            let generation = form.generation();
            for work in form.take_work() {
                let done = work.run(store, ask);
                form.done(generation, done);
            }
        }
    }

    /// A hub that answers from `status` and records what it was asked.
    fn hub(status: Arc<Mutex<WikiStatus>>, asked: Arc<Mutex<Vec<WikiRequest>>>) -> Ask {
        Arc::new(move |req: WikiRequest| {
            asked.lock().unwrap().push(req.clone());
            let mut s = status.lock().unwrap();
            if matches!(req, WikiRequest::Init) {
                s.set_up = true;
            }
            WikiReply::Status(s.clone())
        })
    }

    #[test]
    fn the_labels_match_the_wireframe() {
        assert_eq!(SOURCES.map(source_label), ["Orphan branch", "Repo folder"]);
        assert_eq!(location_label(WikiSource::Branch), "Branch");
        assert_eq!(placeholder(WikiSource::Branch), "docs/planning");
        assert_eq!(placeholder(WikiSource::Folder), "docs");
        assert_eq!((AUTO_SYNC_LABEL, STATUS_LABEL, SYNC_NOW_LABEL), ("Auto-sync", "Status", "Sync now"));
    }

    #[test]
    fn the_status_line_reads_the_hub() {
        let mut form = WikiForm::default();
        assert_eq!(form.status_line(now()).text, "Checking…");
        form.done(0, WikiDone::Status(WikiReply::Status(status(true, SyncState::Synced))));
        assert_eq!(form.status_line(now()), StatusLine { tone: Tone::Good, text: "Synced · 2m ago".into() });
        let mut conflict = status(true, SyncState::Conflict);
        conflict.conflicts = vec!["a.md".into(), "b.md".into()];
        form.done(0, WikiDone::Status(WikiReply::Status(conflict)));
        assert_eq!(form.status_line(now()).text, "2 files held for a conflict");
        assert_eq!(form.status_line(now()).tone, Tone::Bad);
        let mut paused = status(true, SyncState::Synced);
        paused.paused = true;
        form.done(0, WikiDone::Status(WikiReply::Status(paused)));
        assert_eq!(form.status_line(now()), StatusLine { tone: Tone::Muted, text: "Synced · 2m ago · Paused".into() });
        form.done(0, WikiDone::Status(WikiReply::failed(FailureKind::Failed, "no daemon")));
        assert_eq!(form.status_line(now()).text, "Couldn't reach the hub: no daemon");
        assert_eq!(form.button(), (Button::SetUp, false), "nothing to set up through a hub out of reach");
        form.done(0, WikiDone::Status(WikiReply::Status(WikiStatus::not_set_up())));
        assert!(form.status_line(now()).text.starts_with("Not set up"));
    }

    #[test]
    fn a_stale_result_is_dropped() {
        let mut form = WikiForm::default();
        let old = form.generation();
        form.reset();
        assert!(!form.done(old, WikiDone::Status(WikiReply::Status(status(true, SyncState::Synced)))));
        assert!(form.hub().is_none());
    }

    #[test]
    fn a_project_without_a_wiki_drafts_its_choices_until_set_up() {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = crate::settings::agents::fixture::Home::new("wiki", "wiki-page", lock);
        let store = store::Disk::store("wiki-page");
        let hub_status = Arc::new(Mutex::new(WikiStatus::not_set_up()));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let ask = hub(hub_status.clone(), asked.clone());

        let mut form = WikiForm::default();
        form.visit();
        drain(&mut form, store.as_ref(), &ask);
        assert!(form.loaded() && !form.live());
        assert_eq!(form.button(), (Button::SetUp, true));
        assert_eq!(form.shown(), WikiConfig::default());

        // Changes before setup write nothing.
        form.choose_source(WikiSource::Folder);
        form.set_text("handbook");
        form.commit_text();
        form.toggle_auto();
        assert!(!form.work_due(), "a draft, not a save");
        assert!(shelbi_state::load_project("wiki-page").unwrap().wiki.is_none());
        assert_eq!(form.text(), "handbook");

        // Set up writes the block and asks the hub.
        form.press();
        assert!(form.setting_up() && !form.editable());
        assert_eq!(form.status_line(now()).text, "Setting up…");
        drain(&mut form, store.as_ref(), &ask);
        let block = shelbi_state::load_project("wiki-page").unwrap().wiki.unwrap();
        assert_eq!(block.source, WikiSource::Folder);
        assert_eq!(block.path.as_deref(), Some("handbook"));
        assert_eq!(block.sync, WikiSyncMode::Manual);
        assert!(asked.lock().unwrap().contains(&WikiRequest::Init));
        assert!(form.live() && form.set_up());
        assert_eq!(form.button(), (Button::SyncNow, true));
        drop(home);
    }

    #[test]
    fn once_set_up_each_change_saves_and_a_bad_value_is_refused() {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = crate::settings::agents::fixture::Home::new("wiki", "wiki-live", lock);
        let store = store::Disk::store("wiki-live");
        let hub_status = Arc::new(Mutex::new(status(true, SyncState::Synced)));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let ask = hub(hub_status, asked.clone());

        let mut form = WikiForm::default();
        form.visit();
        drain(&mut form, store.as_ref(), &ask);
        assert!(form.live(), "the hub has it set up (a worktree, no block)");
        assert_eq!(form.text(), "", "the default branch shows as the placeholder");

        form.set_text("has space");
        form.commit_text();
        assert!(form.error().unwrap().starts_with("Wiki.branch `has space`"), "{:?}", form.error());
        assert!(!form.work_due(), "nothing invalid is written");
        assert_eq!(form.text(), "has space", "a refused value keeps its text");
        form.revert_text();
        assert_eq!(form.text(), "");

        form.set_text(" team-wiki ");
        form.commit_text();
        assert!(form.saving() && !form.editable());
        assert_eq!(form.text(), "team-wiki", "pending shows at once");
        drain(&mut form, store.as_ref(), &ask);
        assert_eq!(form.status(), &SaveStatus::Saved);
        assert!(form.status_message().unwrap().starts_with("Saved to "));
        let project = shelbi_state::load_project("wiki-live").unwrap();
        assert_eq!(project.wiki.unwrap().branch.as_deref(), Some("team-wiki"));

        form.toggle_auto();
        drain(&mut form, store.as_ref(), &ask);
        assert!(!form.auto());
        assert_eq!(shelbi_state::load_project("wiki-live").unwrap().wiki.unwrap().sync, WikiSyncMode::Manual);

        form.set_text("/abs");
        form.choose_source(WikiSource::Folder);
        assert_eq!(form.text(), "", "a new source shows its own location");
        drain(&mut form, store.as_ref(), &ask);
        assert_eq!(shelbi_state::load_project("wiki-live").unwrap().wiki.unwrap().source, WikiSource::Folder);
        form.set_text("../out");
        form.commit_text();
        assert!(form.error().unwrap().contains("inside the repo"));

        asked.lock().unwrap().clear();
        form.press();
        assert!(form.syncing());
        assert_eq!(form.button(), (Button::SyncNow, false), "one sync at a time");
        drain(&mut form, store.as_ref(), &ask);
        assert!(!form.syncing());
        assert_eq!(asked.lock().unwrap().as_slice(), [WikiRequest::Sync(SyncOp::Now)]);
        drop(home);
    }

    #[test]
    fn filled_spells_out_the_default_location() {
        assert_eq!(filled(WikiConfig::default()).branch.as_deref(), Some("docs/planning"));
        let folder = filled(WikiConfig {
            source: WikiSource::Folder,
            ..WikiConfig::default()
        });
        assert_eq!((folder.branch, folder.path.as_deref()), (None, Some("docs")));
    }
}
