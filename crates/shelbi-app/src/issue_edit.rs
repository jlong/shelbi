//! Editing an issue in place: the draft and save state both clients' task
//! editors run on (`task-inline-edit-state`).
//!
//! The user edits an issue's title and body with no edit mode and no Save
//! button. [`IssueEdits`] holds, per issue, what the editor shows (the
//! *draft*) against the content it is based on, and decides when to save:
//! after a short pause in typing ([`SAVE_DEBOUNCE`]), or at once when the
//! editor asks ([`IssueEdits::flush`]: a field losing focus, Cmd+S, leaving
//! the task). Edits made while a save is out coalesce into the next one,
//! and a field equal to what is stored is never sent.
//!
//! Every write to one issue goes through one lane, one at a time: the
//! editor's saves and the client's other mutations of it (a move, a
//! reorder, an assignment) alike ([`IssueEdits::begin_write`]). A write
//! that waits for another is held, never refused or dropped. Each write is
//! sent through the daemon's issue mutation path ([`SaveJob::mutation`])
//! with the `(status, revision)` it is based on; the revision a write lands
//! at (the daemon's ack) is what the next one expects, since this client's
//! reads (the daemon's board index) trail its own writes. A read older than
//! a landed write is a lagging copy and changes nothing.
//!
//! A read newer than the draft's base is someone else's change (an agent,
//! the CLI, the TUI, another window). Where the draft hasn't changed a field
//! the read's value replaces it ([`Update`]); where both changed it, the
//! body's two edits are combined when they touch different lines
//! ([`merge::merge3`]), and otherwise the field is a [`Conflict`]: both
//! versions are kept, saving pauses, and the user picks ([`Resolution`]).
//! Each save also carries the title / body it replaces, so the daemon
//! refuses it if that changed meanwhile, even on a tracker whose revisions
//! can't tell ([`shelbi_proto::control::EditSpec::expect_body`]); the draft
//! then waits for a read of the new copy and rebases on it.
//!
//! A failed save keeps the draft, says why ([`SaveStatus`]) and is retried
//! by the next edit, a flush or [`IssueEdits::retry`]. Closing the editor
//! flushes it; a draft that still holds unsaved work stays here, so
//! reopening the task (or returning to the project: [`shared`] keeps one
//! store per project for the life of the process) shows it again.
//!
//! Nothing here performs IO or reads a clock: the host passes `now` and
//! runs the jobs, so every rule is tested without a daemon or a window.

pub mod merge;

pub use merge::Splice;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use shelbi_proto::control::ExpectedState;
use shelbi_state::IssueFile;

use crate::exec::{ExecError, Mutation};

/// How long typing must pause before the draft saves.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);

/// How long a save refused as changed elsewhere waits for a read of the new
/// copy before it says so and stops.
pub const STALE_WAIT: Duration = Duration::from_secs(15);

/// Refusals as changed elsewhere in a row (each followed by a read that
/// showed nothing new) before the draft stops retrying on its own.
const STALE_RETRY_LIMIT: u32 = 3;

/// An editable text field of an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Title,
    Body,
}

impl Field {
    pub const ALL: [Field; 2] = [Field::Title, Field::Body];

    /// Whether `a` and `b` store as the same value: a title is stored
    /// trimmed, a body with its trailing whitespace folded to one newline
    /// (the daemon's edit), so those differences are never saved.
    pub fn same(self, a: &str, b: &str) -> bool {
        match self {
            Field::Title => a.trim() == b.trim(),
            Field::Body => a.trim_end() == b.trim_end(),
        }
    }

    /// The value the store keeps for `text`.
    fn stored(self, text: &str) -> String {
        match self {
            Field::Title => text.trim().to_string(),
            Field::Body => format!("{}\n", text.trim_end()),
        }
    }

    /// Why `text` can't be saved in this field, if it can't.
    pub fn invalid(self, text: &str) -> Option<&'static str> {
        match self {
            Field::Title if text.trim().is_empty() => Some("A title can't be blank"),
            Field::Title if text.contains(['\n', '\r']) => Some("A title is one line"),
            _ => None,
        }
    }

    /// The field's name in user-facing text.
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Body => "description",
        }
    }
}

/// One issue's editable content at one revision, as a read has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: String,
    pub state: ExpectedState,
    pub title: String,
    pub body: String,
}

impl Snapshot {
    pub fn of(tf: &IssueFile) -> Snapshot {
        Snapshot {
            id: tf.task.id.clone(),
            state: ExpectedState {
                status: tf.task.column.as_str().to_string(),
                updated_at: tf.task.updated_at.to_rfc3339(),
            },
            title: tf.task.title.clone(),
            body: tf.body.clone(),
        }
    }

    fn get(&self, field: Field) -> &str {
        match field {
            Field::Title => &self.title,
            Field::Body => &self.body,
        }
    }
}

/// Where an issue's edits stand, for the editor's quiet feedback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveStatus {
    /// Nothing unsaved, and nothing saved since the editor opened.
    Clean,
    /// Typed, waiting for the pause before it saves.
    Unsaved,
    /// A save is out, or a refused one waits for the latest copy to retry.
    Saving,
    /// Everything typed is saved.
    Saved,
    /// A field holds a value that can't be stored (a blank title); it isn't
    /// sent until it is fixed. The other field still saves.
    Invalid { field: Field, message: String },
    /// The last save failed. The draft is kept; the next edit, a flush or
    /// [`IssueEdits::retry`] sends it again.
    Failed(String),
    /// `field` was changed elsewhere where the draft changed it too; saving
    /// waits for [`IssueEdits::resolve`].
    Conflict(Field),
}

impl SaveStatus {
    /// The quiet one-liner an editor shows, if any.
    pub fn label(&self) -> Option<String> {
        match self {
            SaveStatus::Clean => None,
            SaveStatus::Unsaved => Some("Edited".into()),
            SaveStatus::Saving => Some("Saving…".into()),
            SaveStatus::Saved => Some("Saved".into()),
            SaveStatus::Invalid { message, .. } => Some(format!("Not saved: {message}")),
            SaveStatus::Failed(message) => Some(format!("Not saved: {message}")),
            SaveStatus::Conflict(field) => {
                Some(format!("The {} was changed elsewhere", field.label()))
            }
        }
    }

    /// Whether the draft holds work that isn't stored.
    pub fn is_unsaved(&self) -> bool {
        !matches!(self, SaveStatus::Clean | SaveStatus::Saved)
    }
}

/// Both versions of a field changed here and elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub field: Field,
    /// The draft: what the editor shows.
    pub mine: String,
    /// What is stored now.
    pub theirs: String,
}

/// How the user settles a [`Conflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Save the draft over the other change.
    KeepMine,
    /// Take the stored text. The draft's version stays recoverable
    /// ([`IssueEdits::set_aside`]) until the editor closes.
    TakeTheirs,
}

/// The text an editor shows for a field changed under it: a read replaced a
/// field the draft hadn't changed, a rebase merged another change into it,
/// or a conflict was resolved. Apply it with [`merge::Splice::between`] the
/// current text to keep the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    pub id: String,
    pub field: Field,
    pub text: String,
}

/// A save to send: the changed fields, each with the value it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveJob {
    pub id: String,
    seq: u64,
    pub title: Option<String>,
    pub body: Option<String>,
    /// A workflow change ([`IssueEdits::set_workflow`]).
    pub workflow: Option<String>,
    /// The `(status, revision)` the save is based on.
    pub expected: Option<ExpectedState>,
    pub expect_title: Option<String>,
    pub expect_body: Option<String>,
}

impl SaveJob {
    /// The daemon mutation that carries it.
    pub fn mutation(&self, project: &str) -> Mutation {
        Mutation::EditIssue {
            project: project.to_string(),
            id: self.id.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            workflow: self.workflow.clone(),
            expect_title: self.expect_title.clone(),
            expect_body: self.expect_body.clone(),
        }
    }
}

/// A write of an issue other than a save (a move, a reorder, ...) holding
/// its lane, from [`IssueEdits::begin_write`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteTicket {
    id: String,
    seq: u64,
}

/// One field of a draft.
#[derive(Debug, Clone, Default)]
struct FieldDraft {
    /// The stored value the draft is based on.
    base: String,
    /// What the editor shows.
    local: String,
    /// The value of the save in flight.
    sent: Option<String>,
    /// The base a landed save replaced, until a read shows the save: a read
    /// that still has it is a copy from before the save, not a revert.
    superseded: Option<String>,
    /// The stored value, when it conflicts with the draft.
    theirs: Option<String>,
    /// The draft's text a [`Resolution::TakeTheirs`] replaced.
    set_aside: Option<String>,
}

impl FieldDraft {
    fn new(value: &str) -> FieldDraft {
        FieldDraft {
            base: value.to_string(),
            local: value.to_string(),
            ..FieldDraft::default()
        }
    }

    fn dirty(&self, field: Field) -> bool {
        !field.same(&self.local, &self.base)
    }
}

#[derive(Debug, Clone)]
struct Draft {
    title: FieldDraft,
    body: FieldDraft,
    /// The revision the draft's base was read (or landed) at.
    base_state: ExpectedState,
    /// A workflow change to send with the next save.
    workflow: Option<String>,
    /// The workflow change in flight.
    sent_workflow: Option<String>,
    last_edit: Option<Instant>,
    /// Send without waiting for the pause (a flush, a retry, a rebase).
    due: bool,
    failure: Option<String>,
    /// Refused as changed elsewhere at this time: waiting for a read of the
    /// new copy to rebase on.
    awaiting: Option<Instant>,
    stale_refusals: u32,
    saved: bool,
    open: bool,
}

impl Draft {
    fn new(snap: &Snapshot) -> Draft {
        Draft {
            title: FieldDraft::new(&snap.title),
            body: FieldDraft::new(&snap.body),
            base_state: snap.state.clone(),
            workflow: None,
            sent_workflow: None,
            last_edit: None,
            due: false,
            failure: None,
            awaiting: None,
            stale_refusals: 0,
            saved: false,
            open: true,
        }
    }

    fn field(&self, field: Field) -> &FieldDraft {
        match field {
            Field::Title => &self.title,
            Field::Body => &self.body,
        }
    }

    fn field_mut(&mut self, field: Field) -> &mut FieldDraft {
        match field {
            Field::Title => &mut self.title,
            Field::Body => &mut self.body,
        }
    }

    fn conflict(&self) -> Option<Field> {
        Field::ALL.into_iter().find(|f| self.field(*f).theirs.is_some())
    }

    fn dirty(&self) -> bool {
        Field::ALL.into_iter().any(|f| self.field(f).dirty(f)) || self.workflow.is_some()
    }

    /// Nothing here the store doesn't have, and nothing to tell the user.
    fn settled(&self) -> bool {
        !self.dirty()
            && self.conflict().is_none()
            && self.failure.is_none()
            && self.awaiting.is_none()
            && self.sent_workflow.is_none()
            && Field::ALL.into_iter().all(|f| self.field(f).sent.is_none())
    }
}

/// What is known about writes to one issue.
#[derive(Debug, Clone, Default)]
struct Lane {
    /// The write out now (a save or another mutation), by sequence number.
    in_flight: Option<u64>,
    /// The state the write in flight was sent against.
    sent_from: Option<ExpectedState>,
    /// The latest revision known: the newest read, or a landed write.
    known: Option<ExpectedState>,
    /// The read state before this client's first write that no read has
    /// shown yet. A state between it and `known` predates only this
    /// client's own writes.
    own_from: Option<ExpectedState>,
    /// The newest read that arrived while a write was out (folded in once
    /// it lands, when it isn't older than the write).
    held: Option<Snapshot>,
}

impl Lane {
    fn idle(&self) -> bool {
        self.in_flight.is_none() && self.own_from.is_none() && self.held.is_none()
    }

    /// A read at `state` arrived: it shows every write landed at or before
    /// it. False when it is older than a write this client knows landed.
    fn read(&mut self, state: &ExpectedState) -> bool {
        match &self.known {
            Some(known) if !revision_at_or_after(&state.updated_at, &known.updated_at) => false,
            _ => {
                self.known = Some(state.clone());
                self.own_from = None;
                true
            }
        }
    }

    fn landed(&mut self, landed: Option<ExpectedState>) {
        let sent = self.sent_from.take();
        if let Some(landed) = landed {
            if self.own_from.is_none() {
                self.own_from = sent;
            }
            self.known = Some(landed);
        }
    }
}

/// The draft and save state of one project's issues. See the module docs.
#[derive(Debug)]
pub struct IssueEdits {
    drafts: BTreeMap<String, Draft>,
    lanes: BTreeMap<String, Lane>,
    updates: Vec<Update>,
    next_seq: u64,
    debounce: Duration,
}

impl Default for IssueEdits {
    fn default() -> Self {
        IssueEdits::new()
    }
}

/// A project's [`IssueEdits`], shared by its windows and the threads that
/// send its saves.
pub type SharedEdits = Arc<Mutex<IssueEdits>>;

/// The process's one [`IssueEdits`] for `project`. A draft outlives the view
/// that made it: a save finishing after its window moved to another project
/// lands here, and a draft left unsaved is there when the project is shown
/// again.
pub fn shared(project: &str) -> SharedEdits {
    static STORES: OnceLock<Mutex<HashMap<String, SharedEdits>>> = OnceLock::new();
    let stores = STORES.get_or_init(Default::default);
    let mut stores = stores.lock().unwrap_or_else(|e| e.into_inner());
    stores.entry(project.to_string()).or_default().clone()
}

/// Lock a [`SharedEdits`] (a panic elsewhere doesn't lose the drafts).
pub fn lock(edits: &SharedEdits) -> MutexGuard<'_, IssueEdits> {
    edits.lock().unwrap_or_else(|e| e.into_inner())
}

impl IssueEdits {
    pub fn new() -> IssueEdits {
        IssueEdits {
            drafts: BTreeMap::new(),
            lanes: BTreeMap::new(),
            updates: Vec::new(),
            next_seq: 0,
            debounce: SAVE_DEBOUNCE,
        }
    }

    // ---- the editor --------------------------------------------------------

    /// The editor shows `snap`'s issue. Starts a draft from it, or picks up
    /// the draft already here (one left unsaved, or open in another
    /// window): the editor shows [`IssueEdits::text`], not the read.
    pub fn open(&mut self, snap: &Snapshot) {
        match self.drafts.get_mut(&snap.id) {
            Some(draft) => draft.open = true,
            None => {
                self.drafts.insert(snap.id.clone(), Draft::new(snap));
                self.lanes.entry(snap.id.clone()).or_default();
            }
        }
    }

    /// Whether `id` has a draft (open, or kept with unsaved work).
    pub fn has_draft(&self, id: &str) -> bool {
        self.drafts.contains_key(id)
    }

    /// The text the editor shows for `field`.
    pub fn text(&self, id: &str, field: Field) -> Option<&str> {
        self.drafts.get(id).map(|d| d.field(field).local.as_str())
    }

    /// The user changed `field` to `text`. It saves after the pause.
    pub fn edit(&mut self, id: &str, field: Field, text: &str, now: Instant) {
        let Some(draft) = self.drafts.get_mut(id) else {
            return;
        };
        if draft.field(field).local == text {
            return;
        }
        draft.field_mut(field).local = text.to_string();
        draft.last_edit = Some(now);
        // A new edit is a new attempt: a failure no longer stands in its way.
        draft.failure = None;
        draft.stale_refusals = 0;
    }

    /// The user changed `field` outside the editor (in their own editor):
    /// an edit like any other, and the open editor shows it ([`Update`]).
    pub fn edit_from_elsewhere(&mut self, id: &str, field: Field, text: &str, now: Instant) {
        if !self.drafts.contains_key(id) {
            return;
        }
        self.edit(id, field, text, now);
        self.updates.push(Update { id: id.to_string(), field, text: text.to_string() });
    }

    /// Change `snap`'s issue's workflow, with the next save (sent at once,
    /// in its turn in the issue's lane). The daemon validates it; a refusal
    /// drops it and says why.
    pub fn set_workflow(&mut self, snap: &Snapshot, workflow: &str) {
        if !self.drafts.contains_key(&snap.id) {
            self.open(snap);
            if let Some(d) = self.drafts.get_mut(&snap.id) {
                d.open = false;
            }
        }
        if let Some(draft) = self.drafts.get_mut(&snap.id) {
            draft.workflow = Some(workflow.to_string());
            draft.due = true;
            draft.failure = None;
        }
    }

    /// Save `id` now (a field lost focus, Cmd+S, leaving the task): the job
    /// to send, or `None` when there is nothing to send or it must wait (it
    /// then goes as soon as it can, without the pause).
    pub fn flush(&mut self, id: &str) -> Option<SaveJob> {
        let draft = self.drafts.get_mut(id)?;
        draft.due = true;
        draft.failure = None;
        self.take_job(id, None)
    }

    /// The editor closed on `id`: flush it. The draft is dropped once
    /// everything in it is stored; one holding unsaved work stays.
    pub fn close(&mut self, id: &str) -> Option<SaveJob> {
        let job = self.flush(id);
        if let Some(draft) = self.drafts.get_mut(id) {
            draft.open = false;
            for f in Field::ALL {
                draft.field_mut(f).set_aside = None;
            }
        }
        self.collect(id);
        job
    }

    /// Send a failed save again.
    pub fn retry(&mut self, id: &str) -> Option<SaveJob> {
        let draft = self.drafts.get_mut(id)?;
        draft.stale_refusals = 0;
        draft.awaiting = None;
        self.flush(id)
    }

    /// Throw away `id`'s unsaved changes: the editor shows what is stored.
    /// Only on the user's say-so.
    pub fn discard(&mut self, id: &str) {
        let Some(draft) = self.drafts.get_mut(id) else {
            return;
        };
        for f in Field::ALL {
            let fd = draft.field_mut(f);
            let stored = fd.theirs.take().unwrap_or_else(|| fd.base.clone());
            fd.base = stored.clone();
            if fd.local != stored {
                fd.local = stored.clone();
                self.updates.push(Update { id: id.to_string(), field: f, text: stored });
            }
        }
        draft.workflow = None;
        draft.failure = None;
        draft.awaiting = None;
        draft.due = false;
        self.collect(id);
    }

    /// The saves due by `now`: each draft whose typing paused for the
    /// debounce, or that was flushed, and whose lane is free.
    pub fn poll(&mut self, now: Instant) -> Vec<SaveJob> {
        let ids: Vec<String> = self.drafts.keys().cloned().collect();
        let mut jobs = Vec::new();
        for id in ids {
            if let Some(draft) = self.drafts.get_mut(&id) {
                if draft.awaiting.is_some_and(|at| now.duration_since(at) >= STALE_WAIT) {
                    draft.awaiting = None;
                    draft.failure = Some(
                        "it changed elsewhere and the new copy hasn't loaded; try again".into(),
                    );
                }
            }
            jobs.extend(self.take_job(&id, Some(now)));
        }
        jobs
    }

    /// The job for `id`, if one is due. `now` is `None` for a flush (no
    /// pause to wait for).
    fn take_job(&mut self, id: &str, now: Option<Instant>) -> Option<SaveJob> {
        let lane = self.lanes.entry(id.to_string()).or_default();
        let draft = self.drafts.get_mut(id)?;
        if lane.in_flight.is_some()
            || draft.awaiting.is_some()
            || draft.failure.is_some()
            || draft.conflict().is_some()
        {
            return None;
        }
        let fields: Vec<Field> = Field::ALL
            .into_iter()
            .filter(|f| {
                let fd = draft.field(*f);
                fd.dirty(*f) && f.invalid(&fd.local).is_none()
            })
            .collect();
        if fields.is_empty() && draft.workflow.is_none() {
            draft.due = false;
            return None;
        }
        if let Some(now) = now {
            let paused = draft
                .last_edit
                .is_none_or(|at| now.duration_since(at) >= self.debounce);
            if !draft.due && !paused {
                return None;
            }
        }
        draft.due = false;
        self.next_seq += 1;
        let seq = self.next_seq;
        let mut job = SaveJob {
            id: id.to_string(),
            seq,
            title: None,
            body: None,
            workflow: draft.workflow.clone(),
            expected: Some(expected_from(lane, draft.base_state.clone())),
            expect_title: None,
            expect_body: None,
        };
        for f in fields {
            let fd = draft.field_mut(f);
            let value = match f {
                Field::Title => fd.local.trim().to_string(),
                Field::Body => fd.local.clone(),
            };
            fd.sent = Some(value.clone());
            match f {
                Field::Title => {
                    job.title = Some(value);
                    job.expect_title = Some(fd.base.clone());
                }
                Field::Body => {
                    job.body = Some(value);
                    job.expect_body = Some(fd.base.clone());
                }
            }
        }
        draft.sent_workflow = draft.workflow.take();
        lane.in_flight = Some(seq);
        lane.sent_from = job.expected.clone();
        Some(job)
    }

    /// The daemon answered `job`. An ack makes the sent values the draft's
    /// base (what was typed meanwhile stays, and saves next) at the revision
    /// the ack reports. A refusal as changed elsewhere waits for a read of
    /// the new copy, rebases on it and sends again; any other failure keeps
    /// the draft and says why.
    pub fn finish(&mut self, job: &SaveJob, result: Result<Option<ExpectedState>, ExecError>, now: Instant) {
        let Some(lane) = self.lanes.get_mut(&job.id) else {
            return;
        };
        if lane.in_flight != Some(job.seq) {
            return;
        }
        lane.in_flight = None;
        let landed = result.as_ref().ok().cloned().flatten();
        if result.is_ok() {
            lane.landed(landed.clone());
        } else {
            lane.sent_from = None;
        }
        let Some(draft) = self.drafts.get_mut(&job.id) else {
            return;
        };
        let sent_workflow = draft.sent_workflow.take();
        match result {
            Ok(_) => {
                for f in Field::ALL {
                    let fd = draft.field_mut(f);
                    if let Some(sent) = fd.sent.take() {
                        fd.superseded = Some(std::mem::replace(&mut fd.base, f.stored(&sent)));
                    }
                }
                if let Some(landed) = landed {
                    draft.base_state = landed;
                }
                draft.saved = true;
                draft.stale_refusals = 0;
            }
            Err(e) => {
                for f in Field::ALL {
                    draft.field_mut(f).sent = None;
                }
                match e {
                    ExecError::Conflict(_) => {
                        // The workflow change goes again with the retry.
                        if draft.workflow.is_none() {
                            draft.workflow = sent_workflow;
                        }
                        draft.stale_refusals += 1;
                        if draft.stale_refusals > STALE_RETRY_LIMIT {
                            draft.failure = Some("it keeps changing elsewhere; try again".into());
                        } else {
                            draft.awaiting = Some(now);
                        }
                    }
                    ExecError::NotFound(_) => {
                        draft.failure = Some("the issue no longer exists".into());
                    }
                    ExecError::Backend(message) => draft.failure = Some(message),
                }
            }
        }
        if let Some(held) = self.lanes.get_mut(&job.id).and_then(|l| l.held.take()) {
            self.apply_read(&held);
        }
        self.collect(&job.id);
    }

    // ---- the issue's other writes ------------------------------------------

    /// Whether a write of `id` is out.
    pub fn busy(&self, id: &str) -> bool {
        self.lanes.get(id).is_some_and(|l| l.in_flight.is_some())
    }

    /// Take `id`'s lane for a mutation other than a save, sent against
    /// `sent`. `None` while another write is out: the caller holds the
    /// mutation and asks again once [`IssueEdits::busy`] clears.
    pub fn begin_write(&mut self, id: &str, sent: &ExpectedState) -> Option<WriteTicket> {
        let lane = self.lanes.entry(id.to_string()).or_default();
        if lane.in_flight.is_some() {
            return None;
        }
        self.next_seq += 1;
        lane.in_flight = Some(self.next_seq);
        lane.sent_from = Some(sent.clone());
        Some(WriteTicket {
            id: id.to_string(),
            seq: self.next_seq,
        })
    }

    /// The write `ticket` finished: `landed` is the revision an ack
    /// reported, `None` for a failure (or a daemon that doesn't report it).
    pub fn end_write(&mut self, ticket: &WriteTicket, landed: Option<ExpectedState>) {
        let Some(lane) = self.lanes.get_mut(&ticket.id) else {
            return;
        };
        if lane.in_flight != Some(ticket.seq) {
            return;
        }
        lane.in_flight = None;
        lane.landed(landed);
        if let Some(held) = lane.held.take() {
            self.apply_read(&held);
        }
        self.collect(&ticket.id);
    }

    /// The state a write of `id` should expect, given the state the user
    /// acted on (`seen`). When `seen` predates only this client's own landed
    /// writes, the daemon already holds the last one's state, so that is
    /// what to send; anything else (a change made elsewhere) is sent as
    /// seen, so the daemon still refuses it.
    pub fn expected(&self, id: &str, seen: ExpectedState) -> ExpectedState {
        match self.lanes.get(id) {
            Some(lane) => expected_from(lane, seen),
            None => seen,
        }
    }

    // ---- reads ---------------------------------------------------------------

    /// Whether any issue needs [`IssueEdits::observe`] to see reads.
    pub fn watching(&self) -> bool {
        !self.drafts.is_empty() || !self.lanes.is_empty()
    }

    /// Fold a read of the board. Only issues with a draft or a write are
    /// looked at; an issue missing from the read (moved off the board) is
    /// left as it is.
    pub fn observe(&mut self, tasks: &[IssueFile]) {
        let ids: std::collections::BTreeSet<String> =
            self.lanes.keys().chain(self.drafts.keys()).cloned().collect();
        for id in ids {
            if let Some(tf) = tasks.iter().find(|tf| tf.task.id == id) {
                self.apply_read(&Snapshot::of(tf));
            }
        }
    }

    fn apply_read(&mut self, snap: &Snapshot) {
        let id = &snap.id;
        let lane = self.lanes.entry(id.clone()).or_default();
        if lane.in_flight.is_some() {
            // It may or may not have the write that's out: decide once that
            // write answers.
            lane.held = Some(snap.clone());
            return;
        }
        if !lane.read(&snap.state) {
            return;
        }
        let Some(draft) = self.drafts.get_mut(id) else {
            self.collect(id);
            return;
        };
        let newer = snap.state != draft.base_state;
        let mut changed = false;
        for f in Field::ALL {
            let theirs = snap.get(f);
            let fd = draft.field_mut(f);
            if f.same(theirs, &fd.base) {
                fd.superseded = None;
                // A conflict the other side withdrew: back to an ordinary
                // draft against the stored text.
                fd.theirs = None;
                continue;
            }
            if fd.superseded.as_deref().is_some_and(|old| f.same(old, theirs)) {
                // A copy from before this client's own save.
                continue;
            }
            fd.superseded = None;
            changed = true;
            if !fd.dirty(f) && fd.theirs.is_none() {
                // An untouched field takes the new text.
                fd.base = theirs.to_string();
                if fd.local != theirs {
                    fd.local = theirs.to_string();
                    self.updates.push(Update { id: id.clone(), field: f, text: theirs.to_string() });
                }
            } else if f.same(&fd.local, theirs) {
                // Both made the same change.
                fd.base = theirs.to_string();
                fd.theirs = None;
            } else {
                let merged = match f {
                    Field::Body if fd.theirs.is_none() => merge::merge3(&fd.base, &fd.local, theirs),
                    _ => None,
                };
                match merged {
                    Some(merged) => {
                        fd.base = theirs.to_string();
                        if merged != fd.local {
                            fd.local = merged.clone();
                            self.updates.push(Update { id: id.clone(), field: f, text: merged });
                        }
                    }
                    None => fd.theirs = Some(theirs.to_string()),
                }
            }
        }
        if newer {
            draft.base_state = snap.state.clone();
        }
        if draft.awaiting.is_some() && (newer || changed) {
            // The new copy: rebased on it, send again now.
            draft.awaiting = None;
            draft.due = true;
        }
        self.collect(id);
    }

    /// The text changes the editor must apply, since the last call.
    pub fn take_updates(&mut self) -> Vec<Update> {
        std::mem::take(&mut self.updates)
    }

    // ---- state ------------------------------------------------------------

    pub fn status(&self, id: &str) -> SaveStatus {
        let Some(draft) = self.drafts.get(id) else {
            return SaveStatus::Clean;
        };
        if let Some(field) = draft.conflict() {
            return SaveStatus::Conflict(field);
        }
        if let Some(message) = &draft.failure {
            return SaveStatus::Failed(message.clone());
        }
        if let Some((field, message)) = Field::ALL.into_iter().find_map(|f| {
            let fd = draft.field(f);
            fd.dirty(f)
                .then(|| f.invalid(&fd.local))
                .flatten()
                .map(|m| (f, m.to_string()))
        }) {
            return SaveStatus::Invalid { field, message };
        }
        let sending = Field::ALL.into_iter().any(|f| draft.field(f).sent.is_some())
            || draft.sent_workflow.is_some();
        if sending || draft.awaiting.is_some() {
            return SaveStatus::Saving;
        }
        if draft.dirty() {
            return SaveStatus::Unsaved;
        }
        if draft.saved {
            SaveStatus::Saved
        } else {
            SaveStatus::Clean
        }
    }

    /// The conflict `id`'s draft is in, if any.
    pub fn conflict(&self, id: &str) -> Option<Conflict> {
        let draft = self.drafts.get(id)?;
        let field = draft.conflict()?;
        let fd = draft.field(field);
        Some(Conflict {
            field,
            mine: fd.local.clone(),
            theirs: fd.theirs.clone()?,
        })
    }

    /// Settle `field`'s conflict.
    pub fn resolve(&mut self, id: &str, field: Field, resolution: Resolution) {
        let Some(draft) = self.drafts.get_mut(id) else {
            return;
        };
        let fd = draft.field_mut(field);
        let Some(theirs) = fd.theirs.take() else {
            return;
        };
        match resolution {
            Resolution::KeepMine => fd.base = theirs,
            Resolution::TakeTheirs => {
                fd.set_aside = Some(std::mem::replace(&mut fd.local, theirs.clone()));
                fd.base = theirs.clone();
                self.updates.push(Update { id: id.to_string(), field, text: theirs });
            }
        }
        draft.due = true;
    }

    /// The draft text a [`Resolution::TakeTheirs`] replaced, until the
    /// editor closes.
    pub fn set_aside(&self, id: &str, field: Field) -> Option<&str> {
        self.drafts.get(id)?.field(field).set_aside.as_deref()
    }

    /// The issues whose drafts hold work that isn't stored, with why.
    pub fn unsaved(&self) -> Vec<(String, SaveStatus)> {
        self.drafts
            .keys()
            .map(|id| (id.clone(), self.status(id)))
            .filter(|(_, s)| s.is_unsaved())
            .collect()
    }

    /// Drop what `id` no longer needs: a closed draft with nothing unsaved,
    /// and a lane with nothing out or unconfirmed.
    fn collect(&mut self, id: &str) {
        let busy = self.busy(id);
        let drop_draft = self.drafts.get(id).is_some_and(|d| !d.open && d.settled() && !busy);
        if drop_draft {
            self.drafts.remove(id);
        }
        if !self.drafts.contains_key(id) && self.lanes.get(id).is_some_and(Lane::idle) {
            self.lanes.remove(id);
        }
    }

    #[cfg(test)]
    fn with_debounce(debounce: Duration) -> IssueEdits {
        IssueEdits {
            debounce,
            ..IssueEdits::new()
        }
    }
}

fn expected_from(lane: &Lane, seen: ExpectedState) -> ExpectedState {
    match (&lane.own_from, &lane.known) {
        (Some(from), Some(known))
            if revision_at_or_after(&seen.updated_at, &from.updated_at)
                && revision_at_or_after(&known.updated_at, &seen.updated_at) =>
        {
            known.clone()
        }
        _ => seen,
    }
}

/// Is revision `a` (an RFC3339 `updated_at`) the same as or later than `b`?
/// Unparseable revisions compare only as equal.
pub fn revision_at_or_after(a: &str, b: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a), Ok(b)) => a >= b,
        _ => a == b,
    }
}
