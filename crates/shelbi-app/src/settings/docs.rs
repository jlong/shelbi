//! Settings ▸ Documentation's shared form (`docs-settings`): where the project's docs
//! lives (the `docs:` block of the project YAML) and where its sync stands,
//! with Sync now.
//!
//! ```text
//!  Documentation
//!  Where this project's docs live, and whether Shelbi syncs it on its own.
//!
//!  Source     (•) Orphan branch  ( ) Repo folder
//!  Branch     [ docs/planning              ]
//!  Auto-sync  [x]
//!  Status     ● Synced · 2m ago   [ Sync now ]
//! ```
//!
//! The block lives beside `orchestrator.awareness:` in the project YAML, so
//! the form loads and saves it through the project's [`WorkflowConfig`]
//! ([`WorkflowConfig::docs`], [`WorkflowConfig::set_docs`]): the same
//! hash-checked, comment-preserving plan the Orchestrator and Properties
//! pages send to the daemon that owns the project, here or on the far
//! machine. The status and Sync now come from that machine's hub docs
//! service ([`crate::docs`]), through an [`Ask`] the client builds for the
//! window's project.
//!
//! Saving works as the Orchestrator and Properties pages' do, once the
//! project has docs: the radio and the checkbox save as they're picked,
//! the Branch / Folder field when it's committed (Enter, or focus leaving
//! it), each value checked first ([`DocsConfig::validate`]): a refused one
//! keeps its text and shows its error under the field. A project with no
//! docs yet (no block, and nothing set up on the hub) gets nothing written
//! as the controls change: they set up the docs the Set up docs button
//! asks for, which writes the block and asks the hub to set it up (as
//! `shelbi docs init` does). Existing projects get nothing until they opt
//! in.
//!
//! The page's work (reads, saves, Sync now, Set up) is queued as
//! [`DocsWork`] for the client to run off the UI thread and hand back with
//! [`DocsForm::done`], tagged with the form's generation so a result for a
//! project no longer shown is dropped.
//!
//! Toolkit-free: the TUI's and the desktop app's Documentation pages both draw
//! [`DocsForm`] with the labels here.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use shelbi_core::docs::{DEFAULT_DOCS_BRANCH, DEFAULT_DOCS_PATH};
use shelbi_core::{DocsConfig, DocsSource, DocsSyncMode};
use shelbi_state::workflow_config::WorkflowConfig;

use crate::settings::orchestrator::SaveStatus;
use crate::settings::store::{self, ConfigStore};
use crate::settings::summary::display_path;
use crate::docs::{SyncOp, SyncState, DocsReply, DocsRequest, DocsStatus};

/// The page's title.
pub const TITLE: &str = "Documentation";
/// The line under the title.
pub const HELP: &str = "Where this project's docs live (its plans, designs and decisions), and whether Shelbi syncs them on its own.";
/// The source radio's label.
pub const SOURCE_LABEL: &str = "Source";
/// The Auto-sync checkbox's label.
pub const AUTO_SYNC_LABEL: &str = "Auto-sync";
/// Its help line.
pub const AUTO_SYNC_HELP: &str =
    "Commit a few seconds after each change, and pull and push on a timer. Off, the docs sync on Sync now only.";
/// The status row's label.
pub const STATUS_LABEL: &str = "Status";
/// The status row's button once the docs are set up.
pub const SYNC_NOW_LABEL: &str = "Sync now";
/// The status row's button before it is.
pub const SET_UP_LABEL: &str = "Set up docs";

/// The sources, in the order the radio lists them.
pub const SOURCES: [DocsSource; 2] = DocsSource::ALL;

/// A source's radio label.
pub fn source_label(source: DocsSource) -> &'static str {
    match source {
        DocsSource::Branch => "Orphan branch",
        DocsSource::Folder => "Repo folder",
    }
}

/// A source's description.
pub fn source_help(source: DocsSource) -> &'static str {
    match source {
        DocsSource::Branch => "A branch of its own, with no history shared with the code, that the hub checks out beside the repo.",
        DocsSource::Folder => "A folder of the main repo. Edits are committed to one rolling pull request.",
    }
}

/// The location field's label for `source`.
pub fn location_label(source: DocsSource) -> &'static str {
    match source {
        DocsSource::Branch => "Branch",
        DocsSource::Folder => "Folder",
    }
}

/// The location field's help for `source`.
pub fn location_help(source: DocsSource) -> &'static str {
    match source {
        DocsSource::Branch => "The orphan branch the docs live on. Created (and pushed) if origin doesn't have it.",
        DocsSource::Folder => "The folder, relative to the repo's root.",
    }
}

/// What an empty location field stands for.
pub fn placeholder(source: DocsSource) -> &'static str {
    match source {
        DocsSource::Branch => DEFAULT_DOCS_BRANCH,
        DocsSource::Folder => DEFAULT_DOCS_PATH,
    }
}

/// `cfg` with its source's location spelled out (the default when it names
/// none): what Set up docs and `shelbi docs init` write, so the block says
/// where the docs are.
pub fn filled(mut cfg: DocsConfig) -> DocsConfig {
    match cfg.source {
        DocsSource::Branch => {
            cfg.branch.get_or_insert_with(|| DEFAULT_DOCS_BRANCH.to_string());
        }
        DocsSource::Folder => {
            cfg.path.get_or_insert_with(|| DEFAULT_DOCS_PATH.to_string());
        }
    }
    cfg
}

/// A source's location as the block has it: its branch or its path.
fn location(cfg: &DocsConfig) -> Option<&str> {
    match cfg.source {
        DocsSource::Branch => cfg.branch.as_deref(),
        DocsSource::Folder => cfg.path.as_deref(),
    }
}

/// Asks the project's hub docs service (blocking): [`crate::docs::ask`]
/// through the window's link, a fake under test.
pub type Ask = Arc<dyn Fn(DocsRequest) -> DocsReply + Send + Sync>;

/// Work the page needs run off the UI thread.
#[derive(Debug, Clone)]
pub enum DocsWork {
    /// Read the project's config (the `docs:` block).
    Load,
    /// Read where the hub's sync stands.
    Status,
    /// Write the block.
    Save(WorkflowConfig),
    /// Commit, pull and push now.
    SyncNow,
    /// Write the block (when it's given) and ask the hub to set the docs up.
    SetUp(Option<WorkflowConfig>),
}

/// Its outcome, handed back to [`DocsForm::done`].
#[derive(Debug, Clone)]
pub enum DocsDone {
    Loaded(Result<WorkflowConfig, String>),
    Status(DocsReply),
    Saved(Result<WorkflowConfig, String>),
    Synced(DocsReply),
    /// The block's write (when one was made), then the hub's answer (none
    /// when the write failed).
    SetUp {
        saved: Option<Result<WorkflowConfig, String>>,
        reply: Option<DocsReply>,
    },
}

impl DocsWork {
    /// Run it (blocking): config through `store`, the docs through `ask`.
    pub fn run(self, store: &dyn ConfigStore, ask: &Ask) -> DocsDone {
        match self {
            DocsWork::Load => DocsDone::Loaded(store.workflows()),
            DocsWork::Status => DocsDone::Status(ask(DocsRequest::Status)),
            DocsWork::Save(config) => DocsDone::Saved(save(store, &config)),
            DocsWork::SyncNow => DocsDone::Synced(ask(DocsRequest::Sync(SyncOp::Now))),
            DocsWork::SetUp(config) => {
                let saved = config.map(|c| save(store, &c));
                let reply = match &saved {
                    Some(Err(_)) => None,
                    _ => Some(ask(DocsRequest::Init)),
                };
                DocsDone::SetUp { saved, reply }
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

/// A set-up docs' status line at `now`: its sync state, how long since it
/// last synced, and Paused or Manual. The Documentation page's status row and the
/// Documentation sidebar's footer read the same.
pub fn status_line(s: &DocsStatus, now: DateTime<Utc>) -> StatusLine {
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

/// The Documentation page's state: the project's config as read, the block shown
/// (being saved, or the setup draft), the location field as typed, the
/// hub's status, and the work in flight.
#[derive(Debug, Clone, Default)]
pub struct DocsForm {
    /// Bumped by [`Self::reset`]: work handed out before it is dropped.
    generation: u64,
    /// `None` until it's read, or while it doesn't read.
    config: Option<WorkflowConfig>,
    unreadable: Option<String>,
    /// The block being written.
    pending: Option<DocsConfig>,
    /// The choices made before the docs are set up (nothing is written).
    draft: Option<DocsConfig>,
    /// The location field as typed, once it's been edited.
    text: Option<String>,
    error: Option<String>,
    status: SaveStatus,
    /// The hub's answer: `None` until it's read; `Err` when it couldn't be.
    hub: Option<Result<DocsStatus, String>>,
    syncing: bool,
    setting_up: bool,
    /// Why the last Sync now or Set up failed.
    sync_error: Option<String>,
    work: Vec<DocsWork>,
}

impl DocsForm {
    /// The form's generation: the client tags the work it takes with it and
    /// hands it back with its result.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Forget everything (another project is shown, or none). Work in
    /// flight is dropped when it lands.
    pub fn reset(&mut self) {
        let generation = self.generation + 1;
        *self = DocsForm {
            generation,
            ..DocsForm::default()
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
        self.queue(DocsWork::Load);
        self.queue(DocsWork::Status);
    }

    /// Read the hub's status again (the hub announced a change).
    pub fn refresh_status(&mut self) {
        self.queue(DocsWork::Status);
    }

    /// Queue `work`; a read already queued isn't queued twice.
    fn queue(&mut self, work: DocsWork) {
        let read = matches!(work, DocsWork::Load | DocsWork::Status);
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
    pub fn take_work(&mut self) -> Vec<DocsWork> {
        std::mem::take(&mut self.work)
    }

    /// A piece of work landed. Returns whether it was taken (a stale one,
    /// from before a [`Self::reset`], is dropped).
    pub fn done(&mut self, generation: u64, done: DocsDone) -> bool {
        if generation != self.generation {
            return false;
        }
        match done {
            DocsDone::Loaded(read) => self.load(read),
            DocsDone::Status(reply) => self.hub = Some(status_of(reply)),
            DocsDone::Saved(result) => {
                self.saved_result(result);
                // The hub starts (or restarts) the service on the new block.
                self.queue(DocsWork::Status);
            }
            DocsDone::Synced(reply) => {
                self.syncing = false;
                match status_of(reply) {
                    Ok(s) => self.hub = Some(Ok(s)),
                    Err(e) => self.sync_error = Some(e),
                }
            }
            DocsDone::SetUp { saved, reply } => {
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

    /// Why the file's `docs:` block doesn't parse, if it doesn't (a change
    /// here replaces it).
    pub fn block_unreadable(&self) -> Option<&str> {
        self.config.as_ref().and_then(WorkflowConfig::docs_unreadable)
    }

    /// The config as last read or saved.
    pub fn config(&self) -> Option<&WorkflowConfig> {
        self.config.as_ref()
    }

    /// The file's block, if it has one.
    fn file_block(&self) -> Option<&DocsConfig> {
        self.config.as_ref().and_then(WorkflowConfig::docs)
    }

    /// The hub's status, once read.
    pub fn hub(&self) -> Option<&Result<DocsStatus, String>> {
        self.hub.as_ref()
    }

    /// Whether the hub has the docs set up.
    pub fn set_up(&self) -> bool {
        matches!(&self.hub, Some(Ok(s)) if s.set_up)
    }

    /// Whether a change saves at once: the project has docs (a block, or
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
    pub fn shown(&self) -> DocsConfig {
        self.pending
            .clone()
            .or_else(|| self.draft.clone())
            .or_else(|| self.file_block().cloned())
            .unwrap_or_default()
    }

    pub fn source(&self) -> DocsSource {
        self.shown().source
    }

    pub fn auto(&self) -> bool {
        self.shown().sync == DocsSyncMode::Auto
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

    /// Pick `source`: saved once the project has docs.
    pub fn choose_source(&mut self, source: DocsSource) {
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
            DocsSyncMode::Auto => DocsSyncMode::Manual,
            DocsSyncMode::Manual => DocsSyncMode::Auto,
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
            DocsSource::Branch => next.branch = value,
            DocsSource::Folder => next.path = value,
        }
        if let Err(e) = next.validate() {
            self.error = Some(sentence(&e.to_string()));
            return;
        }
        self.text = None;
        self.apply(next);
    }

    /// Show `next`: saved when the project has docs, drafted otherwise.
    fn apply(&mut self, next: DocsConfig) {
        if !self.live() {
            self.draft = Some(next);
            return;
        }
        if self.file_block() == Some(&next) {
            return;
        }
        let Some(config) = self.config.as_ref() else { return };
        match config.set_docs(Some(next.clone())) {
            Ok(config) => {
                self.pending = Some(next);
                self.status = SaveStatus::Saving;
                self.work.push(DocsWork::Save(config));
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

    /// Press the status row's button: Sync now, or Set up docs.
    pub fn press(&mut self) {
        match self.button() {
            (_, false) => {}
            (Button::SyncNow, true) => {
                self.syncing = true;
                self.sync_error = None;
                self.work.push(DocsWork::SyncNow);
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
                    Some(c) if c.docs() != Some(&block) => match c.set_docs(Some(block.clone())) {
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
                self.work.push(DocsWork::SetUp(write));
            }
        }
    }

    /// Whether Sync now is running.
    pub fn syncing(&self) -> bool {
        self.syncing
    }

    /// Whether Set up docs are running.
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
                    "Not set up. This project has no docs yet."
                },
            );
        }
        status_line(s, now)
    }

    /// Folder mode's rolling pull request, once one is open.
    pub fn pull_request(&self) -> Option<&str> {
        match &self.hub {
            Some(Ok(s)) => s.pull_request.as_deref(),
            _ => None,
        }
    }

    /// Where the docs are on the project's machine, once set up.
    pub fn root(&self) -> Option<&str> {
        match &self.hub {
            Some(Ok(s)) if s.set_up && !s.root.is_empty() => Some(s.root.as_str()),
            _ => None,
        }
    }
}

/// A status reply's status, or why there's none.
fn status_of(reply: DocsReply) -> Result<DocsStatus, String> {
    match reply {
        DocsReply::Status(s) => Ok(s),
        DocsReply::Failed(f) => Err(f.message),
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
    use crate::docs::{FailureKind, DocsReply};
    use std::sync::Mutex;

    fn status(set_up: bool, state: SyncState) -> DocsStatus {
        DocsStatus {
            set_up,
            state,
            auto: true,
            source: "branch".into(),
            branch: "docs/planning".into(),
            root: "/home/u/.shelbi/projects/p/docs".into(),
            last_synced: Some("2026-11-27T12:00:00Z".into()),
            ..DocsStatus::not_set_up()
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-27T12:02:30Z").unwrap().with_timezone(&Utc)
    }

    /// Run the form's work against `store` and `ask`, handing each back.
    fn drain(form: &mut DocsForm, store: &dyn ConfigStore, ask: &Ask) {
        while form.work_due() {
            let generation = form.generation();
            for work in form.take_work() {
                let done = work.run(store, ask);
                form.done(generation, done);
            }
        }
    }

    /// A hub that answers from `status` and records what it was asked.
    fn hub(status: Arc<Mutex<DocsStatus>>, asked: Arc<Mutex<Vec<DocsRequest>>>) -> Ask {
        Arc::new(move |req: DocsRequest| {
            asked.lock().unwrap().push(req.clone());
            let mut s = status.lock().unwrap();
            if matches!(req, DocsRequest::Init) {
                s.set_up = true;
            }
            DocsReply::Status(s.clone())
        })
    }

    #[test]
    fn the_labels_match_the_wireframe() {
        assert_eq!(SOURCES.map(source_label), ["Orphan branch", "Repo folder"]);
        assert_eq!(location_label(DocsSource::Branch), "Branch");
        assert_eq!(placeholder(DocsSource::Branch), "docs/planning");
        assert_eq!(placeholder(DocsSource::Folder), "docs");
        assert_eq!((AUTO_SYNC_LABEL, STATUS_LABEL, SYNC_NOW_LABEL), ("Auto-sync", "Status", "Sync now"));
    }

    #[test]
    fn the_status_line_reads_the_hub() {
        let mut form = DocsForm::default();
        assert_eq!(form.status_line(now()).text, "Checking…");
        form.done(0, DocsDone::Status(DocsReply::Status(status(true, SyncState::Synced))));
        assert_eq!(form.status_line(now()), StatusLine { tone: Tone::Good, text: "Synced · 2m ago".into() });
        let mut conflict = status(true, SyncState::Conflict);
        conflict.conflicts = vec!["a.md".into(), "b.md".into()];
        form.done(0, DocsDone::Status(DocsReply::Status(conflict)));
        assert_eq!(form.status_line(now()).text, "2 files held for a conflict");
        assert_eq!(form.status_line(now()).tone, Tone::Bad);
        let mut paused = status(true, SyncState::Synced);
        paused.paused = true;
        form.done(0, DocsDone::Status(DocsReply::Status(paused)));
        assert_eq!(form.status_line(now()), StatusLine { tone: Tone::Muted, text: "Synced · 2m ago · Paused".into() });
        form.done(0, DocsDone::Status(DocsReply::failed(FailureKind::Failed, "no daemon")));
        assert_eq!(form.status_line(now()).text, "Couldn't reach the hub: no daemon");
        assert_eq!(form.button(), (Button::SetUp, false), "nothing to set up through a hub out of reach");
        form.done(0, DocsDone::Status(DocsReply::Status(DocsStatus::not_set_up())));
        assert!(form.status_line(now()).text.starts_with("Not set up"));
    }

    #[test]
    fn a_stale_result_is_dropped() {
        let mut form = DocsForm::default();
        let old = form.generation();
        form.reset();
        assert!(!form.done(old, DocsDone::Status(DocsReply::Status(status(true, SyncState::Synced)))));
        assert!(form.hub().is_none());
    }

    #[test]
    fn a_project_without_a_docs_drafts_its_choices_until_set_up() {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = crate::settings::agents::fixture::Home::new("docs", "docs-page", lock);
        let store = store::Disk::store("docs-page");
        let hub_status = Arc::new(Mutex::new(DocsStatus::not_set_up()));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let ask = hub(hub_status.clone(), asked.clone());

        let mut form = DocsForm::default();
        form.visit();
        drain(&mut form, store.as_ref(), &ask);
        assert!(form.loaded() && !form.live());
        assert_eq!(form.button(), (Button::SetUp, true));
        assert_eq!(form.shown(), DocsConfig::default());

        // Changes before setup write nothing.
        form.choose_source(DocsSource::Folder);
        form.set_text("handbook");
        form.commit_text();
        form.toggle_auto();
        assert!(!form.work_due(), "a draft, not a save");
        assert!(shelbi_state::load_project("docs-page").unwrap().docs.is_none());
        assert_eq!(form.text(), "handbook");

        // Set up writes the block and asks the hub.
        form.press();
        assert!(form.setting_up() && !form.editable());
        assert_eq!(form.status_line(now()).text, "Setting up…");
        drain(&mut form, store.as_ref(), &ask);
        let block = shelbi_state::load_project("docs-page").unwrap().docs.unwrap();
        assert_eq!(block.source, DocsSource::Folder);
        assert_eq!(block.path.as_deref(), Some("handbook"));
        assert_eq!(block.sync, DocsSyncMode::Manual);
        assert!(asked.lock().unwrap().contains(&DocsRequest::Init));
        assert!(form.live() && form.set_up());
        assert_eq!(form.button(), (Button::SyncNow, true));
        drop(home);
    }

    #[test]
    fn once_set_up_each_change_saves_and_a_bad_value_is_refused() {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = crate::settings::agents::fixture::Home::new("docs", "docs-live", lock);
        let store = store::Disk::store("docs-live");
        let hub_status = Arc::new(Mutex::new(status(true, SyncState::Synced)));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let ask = hub(hub_status, asked.clone());

        let mut form = DocsForm::default();
        form.visit();
        drain(&mut form, store.as_ref(), &ask);
        assert!(form.live(), "the hub has it set up (a worktree, no block)");
        assert_eq!(form.text(), "", "the default branch shows as the placeholder");

        form.set_text("has space");
        form.commit_text();
        assert!(form.error().unwrap().starts_with("Docs.branch `has space`"), "{:?}", form.error());
        assert!(!form.work_due(), "nothing invalid is written");
        assert_eq!(form.text(), "has space", "a refused value keeps its text");
        form.revert_text();
        assert_eq!(form.text(), "");

        form.set_text(" team-docs ");
        form.commit_text();
        assert!(form.saving() && !form.editable());
        assert_eq!(form.text(), "team-docs", "pending shows at once");
        drain(&mut form, store.as_ref(), &ask);
        assert_eq!(form.status(), &SaveStatus::Saved);
        assert!(form.status_message().unwrap().starts_with("Saved to "));
        let project = shelbi_state::load_project("docs-live").unwrap();
        assert_eq!(project.docs.unwrap().branch.as_deref(), Some("team-docs"));

        form.toggle_auto();
        drain(&mut form, store.as_ref(), &ask);
        assert!(!form.auto());
        assert_eq!(shelbi_state::load_project("docs-live").unwrap().docs.unwrap().sync, DocsSyncMode::Manual);

        form.set_text("/abs");
        form.choose_source(DocsSource::Folder);
        assert_eq!(form.text(), "", "a new source shows its own location");
        drain(&mut form, store.as_ref(), &ask);
        assert_eq!(shelbi_state::load_project("docs-live").unwrap().docs.unwrap().source, DocsSource::Folder);
        form.set_text("../out");
        form.commit_text();
        assert!(form.error().unwrap().contains("inside the repo"));

        asked.lock().unwrap().clear();
        form.press();
        assert!(form.syncing());
        assert_eq!(form.button(), (Button::SyncNow, false), "one sync at a time");
        drain(&mut form, store.as_ref(), &ask);
        assert!(!form.syncing());
        assert_eq!(asked.lock().unwrap().as_slice(), [DocsRequest::Sync(SyncOp::Now)]);
        drop(home);
    }

    #[test]
    fn filled_spells_out_the_default_location() {
        assert_eq!(filled(DocsConfig::default()).branch.as_deref(), Some("docs/planning"));
        let folder = filled(DocsConfig {
            source: DocsSource::Folder,
            ..DocsConfig::default()
        });
        assert_eq!((folder.branch, folder.path.as_deref()), (None, Some("docs")));
    }
}
