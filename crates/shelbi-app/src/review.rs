//! The review interface's shared model: what the review panel shows, its
//! navigation state machine, and the load/queue flow that opens a review.
//!
//! Both clients render it. The TUI's panel (`shelbi-tui/src/review_panel.rs`)
//! drives its rows, selection and activation through the pure functions here
//! ([`panel_rows`], [`step_selection`], [`activate_row`]); the desktop's
//! Review view (`dt-review-view`) holds a [`ReviewPanelState`] over them. The
//! load flow ([`ReviewLoader`]) is the TUI's `rt-tui-review-load-queued`
//! sequence as data: resolve whether the task is serving or queued, open it,
//! or ask which free slot to load it onto.
//!
//! Nothing here does IO: the host reads [`ReviewInfo`] and [`ReviewTarget`]
//! off its UI thread (`shelbi_orchestrator::review_session`) and feeds the
//! results back in.

pub mod diff_load;

use shelbi_core::IssuePr;
use shelbi_state::keymap::{Key, KeyChord, Mods};
use shelbi_state::AppTools;

use crate::nav::View;
use crate::view::{ReviewRow, ReviewState, WorkspaceRow};
use crate::ReviewRole;

/// The panel's status header, beside the back button (the TUI's wording).
pub const READY_FOR_REVIEW: &str = "Ready for review";

/// The note a close or Back gets while a merge is in flight.
pub const MERGE_IN_PROGRESS: &str = "merge in progress, wait for it to finish";

// ---------------------------------------------------------------------------
// What a review shows
// ---------------------------------------------------------------------------

/// A review's durable inputs, read once when it opens (blocking: the board,
/// the project config, the task's workflow).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewInfo {
    pub task_id: String,
    /// The review slot (workspace) the task is loaded on.
    pub slot: String,
    pub title: String,
    /// The task's markdown body.
    pub body: String,
    /// Absolute path of the review worktree.
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …).
    pub editor_name: String,
    /// Which of Edit and View Diff open an app instead of a pane.
    pub apps: AppTools,
    /// The review URL (the workflow's `review:` template with `$PORT` /
    /// `$SLOT` substituted), when the workflow declares one.
    pub review_url: Option<String>,
    /// Whether the workflow declares a `ready:` health check for the server.
    pub has_health_check: bool,
    /// The machine the slot runs on when it is remote (reached over SSH).
    pub remote_machine: Option<String>,
    /// The task's pull request ([`crate::board::task_pr`]).
    pub pr: Option<IssuePr>,
}

/// Where a review slot's server health check stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthCheck {
    /// The workflow declares no `ready:` probe: a live review agent is
    /// serving on its own.
    NotDeclared,
    /// Declared, and the server hasn't passed it yet.
    Waiting,
    /// Declared and passed (the slot's loaded marker names the task).
    Passed,
}

impl HealthCheck {
    /// The health check's state for a slot in `state`.
    pub fn of(declared: bool, state: ReviewState) -> HealthCheck {
        match (declared, state) {
            (false, _) => HealthCheck::NotDeclared,
            (true, ReviewState::Serving) => HealthCheck::Passed,
            (true, _) => HealthCheck::Waiting,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HealthCheck::NotDeclared => "No health check",
            HealthCheck::Waiting => "Waiting for the health check",
            HealthCheck::Passed => "Health check passed",
        }
    }
}

/// The review sub-state's name, as the orchestrator's instructions spell it:
/// `pending-load`, `loading`, `serving`.
pub fn sub_state_label(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Pending => "pending-load",
        ReviewState::Loading => "loading",
        ReviewState::Serving => "serving",
    }
}

/// What the sub-state means, in a sentence.
pub fn sub_state_detail(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Pending => "Waiting for a free review slot",
        ReviewState::Loading => "The review agent is checking out and building",
        ReviewState::Serving => "The server is up",
    }
}

/// A review slot's server, live: its sub-state, health check and location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewServer {
    pub state: ReviewState,
    pub health: HealthCheck,
    /// `machine:port`, once serving (the sidebar row's location badge).
    pub location: Option<String>,
}

/// A review view-switcher entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSwitch {
    /// Chat with the reviewer agent (default).
    Chat,
    /// The diff view.
    Diff,
    /// Open the worktree in the user's editor.
    Editor,
    /// Open the review URL in a browser (only when one exists).
    Browser,
}

impl ReviewSwitch {
    /// The entry's label (the TUI panel's wording).
    pub fn label(self, editor_name: &str) -> String {
        match self {
            ReviewSwitch::Chat => "Reviewer".to_string(),
            ReviewSwitch::Diff => "View Diff".to_string(),
            ReviewSwitch::Editor => format!("Edit in {editor_name}"),
            ReviewSwitch::Browser => "Open Browser".to_string(),
        }
    }
}

/// The review interface's panel for one ready-for-review task: its durable
/// [`ReviewInfo`] with the live server state from the task's sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPanelModel {
    pub task_id: String,
    pub slot: String,
    pub title: String,
    /// The task's markdown body (the full description popover).
    pub body: String,
    /// The body as plain prose, for the panel's short preview.
    pub description: String,
    pub worktree: String,
    pub status_label: String,
    pub editor_name: String,
    /// Which of Edit and View Diff open an app instead of a pane.
    pub apps: AppTools,
    pub review_url: Option<String>,
    pub remote_machine: Option<String>,
    /// The task's pull request, when it has one.
    pub pr: Option<IssuePr>,
    /// The view-switcher nav items, in order.
    pub switch_items: Vec<ReviewSwitch>,
    pub server: ReviewServer,
}

impl ReviewPanelModel {
    /// The panel for `info`, with the live state of `row` (the task's sidebar
    /// row). Without a row yet the slot reads as loading: the task resolved
    /// onto a slot, but nothing has confirmed its server.
    ///
    /// The switch is the TUI's (Reviewer / View Diff / Edit in <editor> /
    /// Open Browser when a URL exists), less Editor on a remote slot, and Diff
    /// too unless it's Shelbi's own view: the daemon only spawns those panes
    /// for a local one, while the built-in view reads any machine's worktree.
    pub fn build(info: &ReviewInfo, row: Option<&ReviewRow>) -> ReviewPanelModel {
        let remote = info.remote_machine.is_some();
        let mut switch_items = vec![ReviewSwitch::Chat];
        if !remote || info.apps.builtin_diff {
            switch_items.push(ReviewSwitch::Diff);
        }
        if !remote {
            switch_items.push(ReviewSwitch::Editor);
        }
        if info.review_url.is_some() {
            switch_items.push(ReviewSwitch::Browser);
        }
        let state = row.map(|r| r.state).unwrap_or(ReviewState::Loading);
        ReviewPanelModel {
            task_id: info.task_id.clone(),
            slot: info.slot.clone(),
            title: info.title.clone(),
            body: info.body.clone(),
            description: description_text(&info.body),
            worktree: info.worktree.clone(),
            status_label: READY_FOR_REVIEW.to_string(),
            editor_name: info.editor_name.clone(),
            apps: info.apps,
            review_url: info.review_url.clone(),
            remote_machine: info.remote_machine.clone(),
            // The row keeps up with the board while the panel stays open.
            pr: row.and_then(|r| r.pr.clone()).or_else(|| info.pr.clone()),
            switch_items,
            server: ReviewServer {
                state,
                health: HealthCheck::of(info.has_health_check, state),
                location: row.and_then(|r| r.location.clone()),
            },
        }
    }

    pub fn has_review_url(&self) -> bool {
        self.review_url.is_some()
    }

    /// Where the pull request opens, when the panel has one to open.
    pub fn pr_url(&self) -> Option<&str> {
        self.pr.as_ref().and_then(|pr| pr.url.as_deref())
    }

    pub fn has_body(&self) -> bool {
        !self.body.trim().is_empty()
    }

    pub fn is_remote(&self) -> bool {
        self.remote_machine.is_some()
    }

    /// The worktree's last two path components (`wt/review`), the part that
    /// tells review worktrees apart.
    pub fn folder_label(&self) -> String {
        let parts: Vec<&str> = self.worktree.split('/').filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [] => String::new(),
            [one] => (*one).to_string(),
            [.., parent, leaf] => format!("{parent}/{leaf}"),
        }
    }
}

/// A markdown body as one paragraph of plain prose, for the panels' clamped
/// description preview (the TUI wraps it into three lines, the desktop clamps
/// it to five).
///
/// The body is parsed as CommonMark with GitHub's tables, strikethrough and
/// task lists, and only its text is kept:
///
/// - Inline formatting reads as its text: emphasis, strong and strikethrough
///   lose their markers, a link shows its text (never its URL), inline code its
///   contents, an image its alt text. Inline HTML tags are dropped, and escapes
///   and entities are decoded.
/// - Paragraphs, list items, task items and blockquote lines join with single
///   spaces, and whitespace collapses.
/// - Headings (section labels, not prose), fenced and indented code blocks
///   (often ASCII mockups), tables, HTML blocks and thematic breaks are dropped,
///   as are task-list checkboxes.
///
/// Because it is a real parser, intraword `_` and a lone `*` aren't emphasis,
/// so identifiers like `snake_case` and `a*b` survive intact.
pub fn description_text(body: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, Tag};

    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut text = String::new();
    // How deep inside a dropped block (heading, code, table, HTML) the parser
    // is. Start and end events always pair, so counting every nested start
    // inside one finds its end.
    let mut dropped = 0usize;
    for event in Parser::new_ext(body, options) {
        if dropped > 0 {
            match event {
                Event::Start(_) => dropped += 1,
                Event::End(_) => {
                    dropped -= 1;
                    if dropped == 0 {
                        text.push(' ');
                    }
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(
                Tag::Heading { .. } | Tag::CodeBlock(_) | Tag::Table(_) | Tag::HtmlBlock,
            ) => {
                text.push(' ');
                dropped = 1;
            }
            // Inline tags join their text to the words around them; every
            // block boundary is a word break.
            Event::Start(tag) if is_inline(&tag) => {}
            Event::End(end) if is_inline_end(end) => {}
            Event::Start(_) | Event::End(_) => text.push(' '),
            Event::Text(t) | Event::Code(t) => text.push_str(&t),
            Event::SoftBreak | Event::HardBreak | Event::Rule => text.push(' '),
            // Inline HTML tags, task-list checkboxes, and the extensions this
            // parser doesn't enable (math, footnotes).
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_inline(tag: &pulldown_cmark::Tag<'_>) -> bool {
    use pulldown_cmark::Tag;
    matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
    )
}

fn is_inline_end(end: pulldown_cmark::TagEnd) -> bool {
    use pulldown_cmark::TagEnd;
    matches!(
        end,
        TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link | TagEnd::Image
    )
}

// ---------------------------------------------------------------------------
// The panel's navigation
// ---------------------------------------------------------------------------

/// Which content view is shown. `Browser` opens the system browser instead,
/// so it is never the active one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Chat,
    Diff,
    Editor,
}

impl ActiveView {
    /// The daemon-spawned content role it shows (`None` for the agent chat).
    pub fn role(self) -> Option<ReviewRole> {
        match self {
            ActiveView::Chat => None,
            ActiveView::Diff => Some(ReviewRole::Diff),
            ActiveView::Editor => Some(ReviewRole::Editor),
        }
    }
}

/// One selectable panel row, top to bottom. The task info (title,
/// description) is not a row; its `More` link is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRow {
    /// The back button at the top.
    Back,
    /// The `More` link: the full task description. Only with a body.
    More,
    /// The task's pull request: open it in the browser. Only with a link.
    Pr,
    /// The worktree folder: reveal it in the file manager.
    Folder,
    Switch(ReviewSwitch),
    /// The user shell beside the content, on or off (the desktop's; the TUI
    /// asks for no shell row).
    Shell,
    Approve,
    Reject,
}

/// What a panel interaction asks the host to do. The panel never performs
/// side effects itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEffect {
    None,
    /// Leave the review loaded and go back to the previous view.
    Back,
    ShowChat,
    ShowDiff,
    ShowEditor,
    /// Open the worktree (the editor) or the diff in the app chosen for
    /// that role; the content view stays as it is.
    OpenApp(ReviewRole),
    OpenBrowser,
    /// Open the task's pull request in the browser.
    OpenPr,
    RevealFolder,
    ShowDescription,
    /// Accept: the gated review → done merge.
    Approve,
    /// Ask for the reason to reject with.
    RejectPrompt,
    /// Tear the review down (the TUI's `q`): the daemon ends the editor,
    /// diff and server sessions and frees the slot's port.
    Close,
    /// Open or close the user shell beside the content (the desktop's).
    ToggleShell,
}

/// The panel's rows: Back, `More` (only with a body), the pull request (only
/// with a link), the folder (when it can be revealed), the switch entries, then
/// Approve and Reject.
pub fn panel_rows(has_body: bool, has_pr: bool, show_folder: bool, switches: &[ReviewSwitch]) -> Vec<PanelRow> {
    panel_rows_with_shell(has_body, has_pr, show_folder, switches, false)
}

/// [`panel_rows`] with the user-shell row after the switch entries when
/// `shell` (the desktop's).
pub fn panel_rows_with_shell(
    has_body: bool,
    has_pr: bool,
    show_folder: bool,
    switches: &[ReviewSwitch],
    shell: bool,
) -> Vec<PanelRow> {
    let mut rows = vec![PanelRow::Back];
    if has_body {
        rows.push(PanelRow::More);
    }
    if has_pr {
        rows.push(PanelRow::Pr);
    }
    if show_folder {
        rows.push(PanelRow::Folder);
    }
    rows.extend(switches.iter().map(|s| PanelRow::Switch(*s)));
    if shell {
        rows.push(PanelRow::Shell);
    }
    rows.push(PanelRow::Approve);
    rows.push(PanelRow::Reject);
    rows
}

/// Move a selection over `len` rows by one, wrapping at both ends.
pub fn step_selection(selected: usize, len: usize, delta: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let idx = selected.min(len - 1);
    if delta < 0 {
        if idx == 0 {
            len - 1
        } else {
            idx - 1
        }
    } else {
        (idx + 1) % len
    }
}

/// What activating `row` does, and the content view it makes active. While a
/// gated merge is in flight Approve and Reject are declined, so a replayed or
/// double press never starts a second one. Edit and View Diff with an app
/// chosen (`apps`) launch it and leave the content view as it is.
pub fn activate_row(row: PanelRow, has_body: bool, merging: bool, apps: AppTools) -> (PanelEffect, Option<ActiveView>) {
    match row {
        PanelRow::Back => (PanelEffect::Back, None),
        PanelRow::More => (description_effect(has_body), None),
        PanelRow::Pr => (PanelEffect::OpenPr, None),
        PanelRow::Folder => (PanelEffect::RevealFolder, None),
        PanelRow::Switch(ReviewSwitch::Chat) => (PanelEffect::ShowChat, Some(ActiveView::Chat)),
        PanelRow::Switch(ReviewSwitch::Diff) if apps.diff => (PanelEffect::OpenApp(ReviewRole::Diff), None),
        PanelRow::Switch(ReviewSwitch::Editor) if apps.editor => (PanelEffect::OpenApp(ReviewRole::Editor), None),
        PanelRow::Switch(ReviewSwitch::Diff) => (PanelEffect::ShowDiff, Some(ActiveView::Diff)),
        PanelRow::Switch(ReviewSwitch::Editor) => {
            (PanelEffect::ShowEditor, Some(ActiveView::Editor))
        }
        PanelRow::Switch(ReviewSwitch::Browser) => (PanelEffect::OpenBrowser, None),
        PanelRow::Shell => (PanelEffect::ToggleShell, None),
        PanelRow::Approve | PanelRow::Reject if merging => (PanelEffect::None, None),
        PanelRow::Approve => (PanelEffect::Approve, None),
        PanelRow::Reject => (PanelEffect::RejectPrompt, None),
    }
}

/// The description popover opens only when there is a body to show.
pub fn description_effect(has_body: bool) -> PanelEffect {
    if has_body {
        PanelEffect::ShowDescription
    } else {
        PanelEffect::None
    }
}

/// The view to return to from a review that has ended (decided, closed, gone),
/// given the `view` it was opened over: that one, unless it was the review's
/// own `slot`'s page, which would only look like the review just left, or a
/// workspace the project no longer has. Those give way to the Orchestrator.
pub fn ended_review_return(view: View, slot: Option<&str>, workspaces: &[WorkspaceRow]) -> View {
    let name = match &view {
        View::Session(name) if name != "orch" => name.as_str(),
        _ => return view,
    };
    let undeclared = !workspaces.is_empty() && !workspaces.iter().any(|w| w.name == name);
    if slot == Some(name) || undeclared {
        View::Session("orch".into())
    } else {
        view
    }
}

/// A decision on a review. The daemon carries it out (the gated merge for an
/// approve) while the client gets on with something else: see
/// [`ReviewDecisions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

impl Decision {
    /// While it runs, on the client's progress line.
    pub fn working(self, task: &str) -> String {
        match self {
            Decision::Approve => format!("Merging {task}…"),
            Decision::Reject => format!("Rejecting {task}…"),
        }
    }

    /// When it lands.
    pub fn done(self, task: &str) -> String {
        match self {
            Decision::Approve => format!("Approved {task}."),
            Decision::Reject => format!("Rejected {task}."),
        }
    }

    /// Why it didn't land, on the review's own panel.
    pub fn failed(self, error: &str) -> String {
        match self {
            Decision::Approve => format!("approve failed: {error}"),
            Decision::Reject => format!("reject failed: {error}"),
        }
    }

    /// Why it didn't land, away from the review: names the task.
    pub fn failed_on(self, task: &str, error: &str) -> String {
        match self {
            Decision::Approve => format!("Couldn't approve {task}: {error}"),
            Decision::Reject => format!("Couldn't reject {task}: {error}"),
        }
    }

    /// The task's review row while it runs.
    pub fn row_note(self) -> &'static str {
        match self {
            Decision::Approve => "Merging…",
            Decision::Reject => "Rejecting…",
        }
    }
}

/// One decision the client is tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionNotice {
    pub task: String,
    pub decision: Decision,
    /// The daemon's refusal, once it has answered with one. `None` while it
    /// runs.
    pub failure: Option<String>,
}

/// The review decisions a client has sent and not yet seen through. A client
/// leaves the review the moment a decision is accepted, so what it says about
/// the decision lives here, outside the panel: the progress line, the review
/// row's note, and a failure that waits for the user to reopen the review
/// (which stays loaded on its slot) or dismiss it. Oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewDecisions {
    notices: Vec<DecisionNotice>,
}

impl ReviewDecisions {
    /// Start `decision` on `task`, replacing a failure it left. `false` while
    /// one is already running on `task`: a review takes one decision at a time.
    pub fn begin(&mut self, task: &str, decision: Decision) -> bool {
        if self.running(task).is_some() {
            return false;
        }
        self.notices.retain(|n| n.task != task);
        self.notices.push(DecisionNotice {
            task: task.to_string(),
            decision,
            failure: None,
        });
        true
    }

    /// The decision running on `task`.
    pub fn running(&self, task: &str) -> Option<Decision> {
        self.notices
            .iter()
            .find(|n| n.task == task && n.failure.is_none())
            .map(|n| n.decision)
    }

    /// The decision running on `task` went through. `Some` only the first time
    /// it's told, so a client that hears it from both the board and the
    /// daemon's reply acts once.
    pub fn landed(&mut self, task: &str) -> Option<Decision> {
        let decision = self.running(task)?;
        self.notices.retain(|n| n.task != task);
        Some(decision)
    }

    /// The decision running on `task` was refused: it stays, as a failure.
    /// `None` when none was running.
    pub fn failed(&mut self, task: &str, error: &str) -> Option<Decision> {
        let notice = self.notices.iter_mut().find(|n| n.task == task && n.failure.is_none())?;
        notice.failure = Some(error.to_string());
        Some(notice.decision)
    }

    /// Hand over the failure `task` left, for the review's panel to show once
    /// it's open again: the line for its status, and the notice is gone.
    pub fn take_failure(&mut self, task: &str) -> Option<String> {
        let index = self.notices.iter().position(|n| n.task == task && n.failure.is_some())?;
        let notice = self.notices.remove(index);
        notice.failure.map(|error| notice.decision.failed(&error))
    }

    /// The user has seen the failure on `task`.
    pub fn dismiss(&mut self, task: &str) {
        self.notices.retain(|n| n.task != task || n.failure.is_none());
    }

    /// Every decision tracked, oldest first.
    pub fn notices(&self) -> &[DecisionNotice] {
        &self.notices
    }

    /// The project went away; a reply still on its way finds nothing here.
    pub fn clear(&mut self) {
        self.notices.clear();
    }
}

/// The panel's interactive state over a [`ReviewPanelModel`]: the rows, the
/// selection, the active content view, and the merge gate. A review opened
/// again while its gated merge runs is inert (every switch, Close and a second
/// decision are dropped, in the TUI's review interface too), except for Back,
/// so it never holds the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPanelState {
    rows: Vec<PanelRow>,
    selected: usize,
    active: ActiveView,
    has_body: bool,
    merging: bool,
    status_line: Option<String>,
    /// The user-shell row is offered (the desktop's).
    shell: bool,
    apps: AppTools,
}

impl ReviewPanelState {
    /// The panel for `model`, with the Chat switch selected and active (the
    /// default content view).
    pub fn new(model: &ReviewPanelModel) -> Self {
        Self::with_shell(model, false)
    }

    /// [`Self::new`], with the user-shell row when `shell` (the desktop's).
    pub fn with_shell(model: &ReviewPanelModel, shell: bool) -> Self {
        let rows = Self::rows_for(model, shell);
        let selected = rows
            .iter()
            .position(|r| *r == PanelRow::Switch(ReviewSwitch::Chat))
            .unwrap_or(0);
        ReviewPanelState {
            rows,
            selected,
            active: ActiveView::Chat,
            has_body: model.has_body(),
            merging: false,
            status_line: None,
            shell,
            apps: model.apps,
        }
    }

    fn rows_for(model: &ReviewPanelModel, shell: bool) -> Vec<PanelRow> {
        panel_rows_with_shell(model.has_body(), model.pr_url().is_some(), !model.is_remote(), &model.switch_items, shell)
    }

    /// Follow a rebuilt model (a refreshed row): the selection stays on the
    /// same row when it still exists.
    pub fn sync(&mut self, model: &ReviewPanelModel) {
        let current = self.selected_row();
        self.rows = Self::rows_for(model, self.shell);
        self.has_body = model.has_body();
        self.apps = model.apps;
        self.selected = current
            .and_then(|row| self.rows.iter().position(|r| *r == row))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    pub fn rows(&self) -> &[PanelRow] {
        &self.rows
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn selected_row(&self) -> Option<PanelRow> {
        self.rows.get(self.selected).copied()
    }

    pub fn active(&self) -> ActiveView {
        self.active
    }

    /// Mark `view` active (a switch the host fell back from, e.g. a refused
    /// diff back to Chat).
    pub fn set_active(&mut self, view: ActiveView) {
        self.active = view;
    }

    pub fn is_merging(&self) -> bool {
        self.merging
    }

    /// Start or end the merge gate. Starting it clears the status line.
    pub fn set_merging(&mut self, on: bool) {
        self.merging = on;
        if on {
            self.status_line = None;
        }
    }

    pub fn status_line(&self) -> Option<&str> {
        self.status_line.as_deref()
    }

    pub fn set_status(&mut self, line: Option<String>) {
        self.status_line = line;
    }

    /// Move the selection up (`-1`) or down (`+1`). Inert while merging.
    pub fn nav(&mut self, delta: i32) {
        if !self.merging {
            self.selected = step_selection(self.selected, self.rows.len(), delta);
        }
    }

    /// Activate the selected row (Enter / Space).
    pub fn activate(&mut self) -> PanelEffect {
        match self.selected_row() {
            Some(row) => self.activate_index(self.selected, row),
            None => PanelEffect::None,
        }
    }

    /// Select and activate `row` (a click). A row the panel doesn't have
    /// does nothing.
    pub fn click(&mut self, row: PanelRow) -> PanelEffect {
        match self.rows.iter().position(|r| *r == row) {
            Some(idx) => self.activate_index(idx, row),
            None => PanelEffect::None,
        }
    }

    fn activate_index(&mut self, idx: usize, row: PanelRow) -> PanelEffect {
        if self.merging && row != PanelRow::Back {
            return PanelEffect::None;
        }
        self.selected = idx;
        let (effect, view) = activate_row(row, self.has_body, self.merging, self.apps);
        if let Some(view) = view {
            self.active = view;
        }
        effect
    }

    /// Ask to tear the review down. While a merge is in flight it is refused
    /// with a note on the status line.
    pub fn request_close(&mut self) -> PanelEffect {
        if self.merging {
            self.status_line = Some(MERGE_IN_PROGRESS.to_string());
            PanelEffect::None
        } else {
            PanelEffect::Close
        }
    }

    /// One key while the panel has focus: `↑`/`k` and `↓`/`j` move, Enter and
    /// Space activate, `m` opens the description, Esc goes back (leaving the
    /// review loaded) and `q` closes it. Chords with Ctrl, Alt or Cmd are not
    /// the panel's.
    pub fn handle_key(&mut self, chord: KeyChord) -> PanelEffect {
        if chord.mods.intersects(Mods::CONTROL | Mods::ALT | Mods::SUPER) {
            return PanelEffect::None;
        }
        match chord.code {
            Key::Char('q') => self.request_close(),
            Key::Esc => PanelEffect::Back,
            _ if self.merging => PanelEffect::None,
            Key::Up | Key::Char('k') => {
                self.nav(-1);
                PanelEffect::None
            }
            Key::Down | Key::Char('j') => {
                self.nav(1);
                PanelEffect::None
            }
            Key::Enter | Key::Char(' ') => self.activate(),
            Key::Char('m') => description_effect(self.has_body),
            _ => PanelEffect::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Opening a review: resolve, load
// ---------------------------------------------------------------------------

/// How a review task resolved (the host reads it off its UI thread from
/// `shelbi_orchestrator::review_session::review_open_target` and the review
/// slots).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewTarget {
    /// Loaded on a review slot: open it.
    Serving(ReviewInfo),
    /// In the review column but on no slot yet. `free_slots` are the review
    /// slots nothing occupies; `total_slots` tells "every slot is busy" from
    /// "no review workspace is configured".
    Queued {
        title: String,
        free_slots: Vec<String>,
        total_slots: usize,
    },
}

/// What the host should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStep {
    None,
    /// Resolve `task` off the UI thread, then call [`ReviewLoader::resolved`].
    Resolve(String),
    /// Open the review view.
    Open(Box<ReviewInfo>),
    /// Ask which free slot to load `task` onto (the "Load for review" dialog).
    AskSlot {
        task: String,
        title: String,
        free_slots: Vec<String>,
    },
    /// No review workspace is configured: report it.
    NoSlots { task: String, title: String },
    /// Every review slot is busy: report it and load nothing. The task stays
    /// queued (pending-load), and the daemon's auto-loader puts it on the
    /// next slot that frees.
    Busy { task: String, title: String },
    /// Load `task` onto `slot` through the daemon, then call
    /// [`ReviewLoader::loaded`].
    Load { task: String, slot: String },
    /// Report a failure.
    Failed(String),
}

/// The review-open flow, the TUI's (`rt-tui-review-load-queued`) as data:
/// resolve whether the task is serving or queued; open a serving one; ask
/// which free slot to load a queued one onto, or report that none is free;
/// load it through the daemon; then open it when the daemon announces the slot
/// serving (`ReviewOpened`). Only a load this client started opens anything:
/// a background `ReviewOpened` (another client's load, the poller's resume or
/// auto-load) never takes the view. One resolve and one load at a time; a
/// result for a task the flow has moved past is dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewLoader {
    /// The task a resolve is running for.
    resolving: Option<String>,
    /// The task a daemon load is running for.
    loading: Option<String>,
    /// The task this client loaded, waiting for its `ReviewOpened`.
    pending: Option<String>,
}

impl ReviewLoader {
    /// The user opened `task` (a sidebar review row, the palette). Opening
    /// another task gives up waiting on an earlier load's `ReviewOpened`.
    pub fn open(&mut self, task: &str) -> LoadStep {
        if self.pending.as_deref() != Some(task) {
            self.pending = None;
        }
        self.resolving = Some(task.to_string());
        LoadStep::Resolve(task.to_string())
    }

    /// A resolve for `task` finished.
    pub fn resolved(&mut self, task: &str, result: Result<ReviewTarget, String>) -> LoadStep {
        if self.resolving.as_deref() != Some(task) {
            return LoadStep::None;
        }
        self.resolving = None;
        match result {
            Ok(ReviewTarget::Serving(info)) => {
                if self.pending.as_deref() == Some(task) {
                    self.pending = None;
                }
                LoadStep::Open(Box::new(info))
            }
            Ok(ReviewTarget::Queued {
                title,
                free_slots,
                total_slots,
            }) => {
                let task = task.to_string();
                if !free_slots.is_empty() {
                    LoadStep::AskSlot {
                        task,
                        title,
                        free_slots,
                    }
                } else if total_slots == 0 {
                    LoadStep::NoSlots { task, title }
                } else {
                    LoadStep::Busy { task, title }
                }
            }
            Err(e) => {
                if self.pending.as_deref() == Some(task) {
                    self.pending = None;
                }
                LoadStep::Failed(format!("Couldn't open the review: {e}"))
            }
        }
    }

    /// The user picked `slot` for `task` in the "Load for review" dialog. The
    /// task is pending from here, so a `ReviewOpened` that lands before the
    /// load's reply still opens it.
    pub fn slot_chosen(&mut self, task: &str, slot: &str) -> LoadStep {
        self.loading = Some(task.to_string());
        self.pending = Some(task.to_string());
        LoadStep::Load {
            task: task.to_string(),
            slot: slot.to_string(),
        }
    }

    /// A load of `task` finished. Success waits for the daemon's
    /// `ReviewOpened` (published once the server passes its health check);
    /// failure reports.
    pub fn loaded(&mut self, task: &str, result: Result<(), String>) -> LoadStep {
        if self.loading.as_deref() != Some(task) {
            return LoadStep::None;
        }
        self.loading = None;
        match result {
            Ok(()) => LoadStep::None,
            Err(e) => {
                if self.pending.as_deref() == Some(task) {
                    self.pending = None;
                }
                LoadStep::Failed(format!("Couldn't load the review: {e}"))
            }
        }
    }

    /// The daemon announced `task`'s slot serving. Only the task this client
    /// loaded opens; anything else is a background event and changes nothing.
    pub fn review_opened(&mut self, task: &str) -> LoadStep {
        if self.pending.as_deref() != Some(task) {
            return LoadStep::None;
        }
        self.pending = None;
        self.resolving = Some(task.to_string());
        LoadStep::Resolve(task.to_string())
    }

    /// The user gave up opening `task` before it opened (the desktop's panel,
    /// shown the moment the review is asked for, was left): a resolve still
    /// running for it, or a `ReviewOpened` for a load it started, opens
    /// nothing. A daemon load already sent runs on (its slot stays loaded),
    /// and a failure of it still reports.
    pub fn cancel(&mut self, task: &str) {
        if self.resolving.as_deref() == Some(task) {
            self.resolving = None;
        }
        if self.pending.as_deref() == Some(task) {
            self.pending = None;
        }
    }

    /// The task this client loaded and is waiting to open.
    pub fn pending(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    /// The task a load is running for.
    pub fn loading(&self) -> Option<&str> {
        self.loading.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(remote: bool, url: bool) -> ReviewInfo {
        ReviewInfo {
            task_id: "t-1".into(),
            slot: "review-1".into(),
            title: "Cold-start cache".into(),
            body: "## Summary\n\n- Warm the `cache` at boot.\n> quoted snake_case".into(),
            worktree: "/Users/j/proj/.shelbi/wt/review-1".into(),
            editor_name: "Vim".into(),
            apps: AppTools::default(),
            review_url: url.then(|| "http://localhost:4101".to_string()),
            has_health_check: true,
            remote_machine: remote.then(|| "gpu".to_string()),
            pr: None,
        }
    }

    fn row(state: ReviewState) -> ReviewRow {
        ReviewRow {
            task_id: "t-1".into(),
            title: "Cold-start cache".into(),
            branch: "jlong/t-1".into(),
            location: (state == ReviewState::Serving).then(|| "hub:4101".to_string()),
            workspace: Some("review-1".into()),
            state,
            number: None,
            pr: None,
        }
    }

    fn chord(code: Key) -> KeyChord {
        KeyChord::new(code, Mods::NONE)
    }

    #[test]
    fn the_model_carries_the_switch_the_url_and_the_live_server_state() {
        let m = ReviewPanelModel::build(&info(false, true), Some(&row(ReviewState::Serving)));
        assert_eq!(m.status_label, READY_FOR_REVIEW);
        assert_eq!(
            m.switch_items,
            [ReviewSwitch::Chat, ReviewSwitch::Diff, ReviewSwitch::Editor, ReviewSwitch::Browser]
        );
        assert_eq!(m.server.state, ReviewState::Serving);
        assert_eq!(m.server.health, HealthCheck::Passed);
        assert_eq!(m.server.location.as_deref(), Some("hub:4101"));
        assert_eq!(m.review_url.as_deref(), Some("http://localhost:4101"));
        assert_eq!(m.folder_label(), "wt/review-1");
        assert_eq!(m.description, "Warm the cache at boot. quoted snake_case");
        assert_eq!(
            m.switch_items.iter().map(|s| s.label(&m.editor_name)).collect::<Vec<_>>(),
            ["Reviewer", "View Diff", "Edit in Vim", "Open Browser"]
        );

        let loading = ReviewPanelModel::build(&info(false, false), Some(&row(ReviewState::Loading)));
        assert!(!loading.switch_items.contains(&ReviewSwitch::Browser), "no URL, no Browser");
        assert_eq!(loading.server.health, HealthCheck::Waiting);
        assert_eq!(loading.server.location, None);
        assert_eq!(sub_state_label(loading.server.state), "loading");

        // Before the sidebar has the row, the slot reads as loading.
        let fresh = ReviewPanelModel::build(&info(false, false), None);
        assert_eq!(fresh.server.state, ReviewState::Loading);

        let undeclared = ReviewPanelModel::build(
            &ReviewInfo {
                has_health_check: false,
                ..info(false, false)
            },
            Some(&row(ReviewState::Serving)),
        );
        assert_eq!(undeclared.server.health, HealthCheck::NotDeclared);
    }

    #[test]
    fn a_remote_slot_has_no_diff_editor_or_folder() {
        let m = ReviewPanelModel::build(&info(true, true), None);
        assert_eq!(m.switch_items, [ReviewSwitch::Chat, ReviewSwitch::Browser]);
        let p = ReviewPanelState::new(&m);
        assert!(!p.rows().contains(&PanelRow::Folder));
    }

    #[test]
    fn a_remote_slot_can_still_show_shelbis_own_diff() {
        let mut i = info(true, true);
        i.apps = AppTools { builtin_diff: true, ..Default::default() };
        let m = ReviewPanelModel::build(&i, None);
        assert_eq!(m.switch_items, [ReviewSwitch::Chat, ReviewSwitch::Diff, ReviewSwitch::Browser]);
    }

    #[test]
    fn the_shell_row_follows_the_switch_only_when_asked_for() {
        let m = ReviewPanelModel::build(&info(false, true), Some(&row(ReviewState::Serving)));
        assert!(!ReviewPanelState::new(&m).rows().contains(&PanelRow::Shell), "the TUI's has none");
        let mut p = ReviewPanelState::with_shell(&m, true);
        let rows = p.rows().to_vec();
        let shell = rows.iter().position(|r| *r == PanelRow::Shell).expect("a shell row");
        assert_eq!(rows[shell - 1], PanelRow::Switch(ReviewSwitch::Browser));
        assert_eq!(rows[shell + 1], PanelRow::Approve);
        assert_eq!(p.click(PanelRow::Shell), PanelEffect::ToggleShell);
        assert_eq!(p.active(), ActiveView::Chat, "the shell is not a content view");
        // A rebuilt model keeps the row.
        p.sync(&m);
        assert!(p.rows().contains(&PanelRow::Shell));
    }

    #[test]
    fn sub_states_use_the_orchestrators_names() {
        assert_eq!(sub_state_label(ReviewState::Pending), "pending-load");
        assert_eq!(sub_state_label(ReviewState::Loading), "loading");
        assert_eq!(sub_state_label(ReviewState::Serving), "serving");
    }

    #[test]
    fn the_rows_follow_the_tui_panel_order() {
        let m = ReviewPanelModel::build(&info(false, true), None);
        let p = ReviewPanelState::new(&m);
        assert_eq!(
            p.rows(),
            [
                PanelRow::Back,
                PanelRow::More,
                PanelRow::Folder,
                PanelRow::Switch(ReviewSwitch::Chat),
                PanelRow::Switch(ReviewSwitch::Diff),
                PanelRow::Switch(ReviewSwitch::Editor),
                PanelRow::Switch(ReviewSwitch::Browser),
                PanelRow::Approve,
                PanelRow::Reject,
            ]
        );
        assert_eq!(p.selected_row(), Some(PanelRow::Switch(ReviewSwitch::Chat)));
        assert_eq!(p.active(), ActiveView::Chat);

        let bare = ReviewPanelModel::build(
            &ReviewInfo {
                body: String::new(),
                ..info(false, false)
            },
            None,
        );
        assert!(!ReviewPanelState::new(&bare).rows().contains(&PanelRow::More), "no body, no More");
    }

    #[test]
    fn keys_move_wrap_and_switch_the_content_view() {
        let m = ReviewPanelModel::build(&info(false, true), None);
        let mut p = ReviewPanelState::new(&m);
        assert_eq!(p.handle_key(chord(Key::Char('j'))), PanelEffect::None);
        assert_eq!(p.handle_key(chord(Key::Enter)), PanelEffect::ShowDiff);
        assert_eq!(p.active(), ActiveView::Diff);
        assert_eq!(p.handle_key(chord(Key::Down)), PanelEffect::None);
        assert_eq!(p.handle_key(chord(Key::Char(' '))), PanelEffect::ShowEditor);
        assert_eq!(p.active(), ActiveView::Editor);
        assert_eq!(p.handle_key(chord(Key::Char('j'))), PanelEffect::None);
        assert_eq!(p.handle_key(chord(Key::Enter)), PanelEffect::OpenBrowser);
        assert_eq!(p.active(), ActiveView::Editor, "the browser is never the active view");
        // Up from Back wraps to Reject.
        for _ in 0..p.selected() {
            p.handle_key(chord(Key::Char('k')));
        }
        assert_eq!(p.selected_row(), Some(PanelRow::Back));
        p.handle_key(chord(Key::Up));
        assert_eq!(p.selected_row(), Some(PanelRow::Reject));
        assert_eq!(p.handle_key(chord(Key::Enter)), PanelEffect::RejectPrompt);
        assert_eq!(p.handle_key(chord(Key::Char('m'))), PanelEffect::ShowDescription);
        assert_eq!(p.handle_key(chord(Key::Esc)), PanelEffect::Back);
        assert_eq!(p.handle_key(chord(Key::Char('q'))), PanelEffect::Close);
        let ctrl_j = KeyChord::new(Key::Char('j'), Mods::CONTROL);
        let before = p.selected();
        assert_eq!(p.handle_key(ctrl_j), PanelEffect::None);
        assert_eq!(p.selected(), before, "a Ctrl chord isn't the panel's");
    }

    #[test]
    fn clicks_select_and_activate() {
        let m = ReviewPanelModel::build(&info(false, false), None);
        let mut p = ReviewPanelState::new(&m);
        assert_eq!(p.click(PanelRow::Approve), PanelEffect::Approve);
        assert_eq!(p.selected_row(), Some(PanelRow::Approve));
        assert_eq!(p.click(PanelRow::Folder), PanelEffect::RevealFolder);
        assert_eq!(p.click(PanelRow::Back), PanelEffect::Back);
        assert_eq!(
            p.click(PanelRow::Switch(ReviewSwitch::Browser)),
            PanelEffect::None,
            "a row the panel doesn't have"
        );
    }

    #[test]
    fn the_merge_gate_makes_the_panel_inert() {
        let m = ReviewPanelModel::build(&info(false, true), None);
        let mut p = ReviewPanelState::new(&m);
        p.set_status(Some("old".into()));
        p.set_merging(true);
        assert_eq!(p.status_line(), None, "a merge clears the status line");
        let before = p.clone();
        for row in [
            PanelRow::Approve,
            PanelRow::Reject,
            PanelRow::Switch(ReviewSwitch::Diff),
            PanelRow::Folder,
        ] {
            assert_eq!(p.click(row), PanelEffect::None, "{row:?} while merging");
        }
        for code in [Key::Enter, Key::Char('j'), Key::Char('m')] {
            assert_eq!(p.handle_key(chord(code)), PanelEffect::None, "{code:?} while merging");
        }
        assert_eq!(p.handle_key(chord(Key::Esc)), PanelEffect::Back, "Back never held");
        assert_eq!(p.selected(), before.selected());
        assert_eq!(p.active(), before.active());
        assert_eq!(p.handle_key(chord(Key::Char('q'))), PanelEffect::None);
        assert_eq!(p.status_line(), Some(MERGE_IN_PROGRESS));
        assert_eq!(p.click(PanelRow::Back), PanelEffect::Back, "Back never held");
        p.set_merging(false);
        assert_eq!(p.click(PanelRow::Approve), PanelEffect::Approve);
    }

    #[test]
    fn the_pure_activation_declines_only_a_second_decision_while_merging() {
        // The TUI's panel keeps this exact table.
        assert_eq!(activate_row(PanelRow::Approve, true, true, AppTools::default()).0, PanelEffect::None);
        assert_eq!(activate_row(PanelRow::Reject, true, true, AppTools::default()).0, PanelEffect::None);
        assert_eq!(
            activate_row(PanelRow::Switch(ReviewSwitch::Chat), true, true, AppTools::default()),
            (PanelEffect::ShowChat, Some(ActiveView::Chat))
        );
        assert_eq!(activate_row(PanelRow::More, false, false, AppTools::default()).0, PanelEffect::None);
        assert_eq!(step_selection(0, 0, 1), 0);
        assert_eq!(step_selection(9, 3, 1), 0, "a stale index clamps first");
    }

    #[test]
    fn sync_keeps_the_selected_row() {
        let m = ReviewPanelModel::build(&info(false, false), None);
        let mut p = ReviewPanelState::new(&m);
        p.click(PanelRow::Approve);
        let with_url = ReviewPanelModel::build(&info(false, true), None);
        p.sync(&with_url);
        assert_eq!(p.selected_row(), Some(PanelRow::Approve));
    }

    // -- the load flow -------------------------------------------------------

    fn queued(free: &[&str], total: usize) -> Result<ReviewTarget, String> {
        Ok(ReviewTarget::Queued {
            title: "Cold-start cache".into(),
            free_slots: free.iter().map(|s| s.to_string()).collect(),
            total_slots: total,
        })
    }

    fn serving() -> Result<ReviewTarget, String> {
        Ok(ReviewTarget::Serving(info(false, false)))
    }

    #[test]
    fn a_serving_task_opens_straight_away() {
        let mut l = ReviewLoader::default();
        assert_eq!(l.open("t-1"), LoadStep::Resolve("t-1".into()));
        assert_eq!(l.resolved("t-1", serving()), LoadStep::Open(Box::new(info(false, false))));
        assert_eq!(l.resolved("t-1", serving()), LoadStep::None, "one result per resolve");
    }

    #[test]
    fn a_queued_task_asks_for_a_slot_loads_and_opens_on_its_review_opened() {
        let mut l = ReviewLoader::default();
        l.open("t-1");
        assert_eq!(
            l.resolved("t-1", queued(&["review-2"], 2)),
            LoadStep::AskSlot {
                task: "t-1".into(),
                title: "Cold-start cache".into(),
                free_slots: vec!["review-2".into()],
            }
        );
        assert_eq!(
            l.slot_chosen("t-1", "review-2"),
            LoadStep::Load {
                task: "t-1".into(),
                slot: "review-2".into()
            }
        );
        assert_eq!(l.loading(), Some("t-1"));
        // The load's reply opens nothing: the daemon's event does.
        assert_eq!(l.loaded("t-1", Ok(())), LoadStep::None);
        assert_eq!(l.pending(), Some("t-1"));
        assert_eq!(l.review_opened("t-1"), LoadStep::Resolve("t-1".into()));
        assert!(matches!(l.resolved("t-1", serving()), LoadStep::Open(_)));
        assert_eq!(l.pending(), None);
        assert_eq!(l.review_opened("t-1"), LoadStep::None, "a second event opens nothing");
    }

    #[test]
    fn the_review_opened_may_land_before_the_loads_reply() {
        // The daemon publishes `ReviewOpened` and then replies `Done`.
        let mut l = ReviewLoader::default();
        l.slot_chosen("t-1", "review-2");
        assert_eq!(l.review_opened("t-1"), LoadStep::Resolve("t-1".into()));
        assert_eq!(l.loaded("t-1", Ok(())), LoadStep::None);
        assert!(matches!(l.resolved("t-1", serving()), LoadStep::Open(_)));
    }

    #[test]
    fn a_background_review_opened_never_takes_the_view() {
        let mut l = ReviewLoader::default();
        assert_eq!(l.review_opened("t-9"), LoadStep::None, "another client's load");
        l.slot_chosen("t-1", "review-2");
        assert_eq!(l.review_opened("t-9"), LoadStep::None, "a poller resume of another task");
        assert_eq!(l.pending(), Some("t-1"));
    }

    #[test]
    fn a_failed_load_reports_and_stops_waiting() {
        let mut l = ReviewLoader::default();
        l.slot_chosen("t-1", "review-2");
        assert_eq!(
            l.loaded("t-1", Err("slot busy".into())),
            LoadStep::Failed("Couldn't load the review: slot busy".into())
        );
        assert_eq!(l.pending(), None);
        assert_eq!(l.review_opened("t-1"), LoadStep::None);
    }

    #[test]
    fn every_slot_busy_reports_and_loads_nothing() {
        let mut l = ReviewLoader::default();
        l.open("t-1");
        assert_eq!(
            l.resolved("t-1", queued(&[], 2)),
            LoadStep::Busy {
                task: "t-1".into(),
                title: "Cold-start cache".into()
            }
        );
        assert_eq!(l.loading(), None);
        assert_eq!(l.pending(), None, "nothing waits: the daemon's auto-loader takes it");
        // The auto-loader's slot coming up is a background event.
        assert_eq!(l.review_opened("t-1"), LoadStep::None);
    }

    #[test]
    fn no_review_workspace_reports() {
        let mut l = ReviewLoader::default();
        l.open("t-1");
        assert!(matches!(l.resolved("t-1", queued(&[], 0)), LoadStep::NoSlots { .. }));
    }

    #[test]
    fn opening_another_review_stops_waiting_for_a_load() {
        let mut l = ReviewLoader::default();
        l.slot_chosen("t-1", "review-2");
        l.open("t-2");
        assert_eq!(l.pending(), None);
        assert_eq!(l.review_opened("t-1"), LoadStep::None);
    }

    #[test]
    fn a_stale_resolve_is_dropped() {
        let mut l = ReviewLoader::default();
        l.open("t-1");
        l.open("t-2");
        assert_eq!(l.resolved("t-1", serving()), LoadStep::None);
        assert!(matches!(l.resolved("t-2", serving()), LoadStep::Open(_)));
        l.open("t-3");
        assert_eq!(
            l.resolved("t-3", Err("`t-3` is not a review task".into())),
            LoadStep::Failed("Couldn't open the review: `t-3` is not a review task".into())
        );
    }

    #[test]
    fn a_cancelled_open_opens_nothing() {
        // Left mid-resolve: its answer is dropped.
        let mut l = ReviewLoader::default();
        l.open("t-1");
        l.cancel("t-1");
        assert_eq!(l.resolved("t-1", serving()), LoadStep::None);

        // Left while its load runs: the load's reply still lands (a failure
        // still reports), but its `ReviewOpened` opens nothing.
        l.open("t-1");
        l.resolved("t-1", queued(&["review-1"], 1));
        l.slot_chosen("t-1", "review-1");
        l.cancel("t-1");
        assert_eq!(l.loading(), Some("t-1"));
        assert_eq!(l.pending(), None);
        assert_eq!(l.loaded("t-1", Ok(())), LoadStep::None);
        assert_eq!(l.review_opened("t-1"), LoadStep::None);

        // Cancelling another task leaves this one alone.
        l.open("t-2");
        l.cancel("t-1");
        assert!(matches!(l.resolved("t-2", serving()), LoadStep::Open(_)));
    }

    #[test]
    fn an_app_editor_or_diff_launches_and_keeps_the_content_view() {
        let mut i = info(false, false);
        i.editor_name = "Zed".into();
        i.apps = AppTools { editor: true, diff: true, ..Default::default() };
        let model = ReviewPanelModel::build(&i, None);
        assert_eq!(model.switch_items.iter().map(|s| s.label(&model.editor_name)).collect::<Vec<_>>(), ["Reviewer", "View Diff", "Edit in Zed"]);
        let mut p = ReviewPanelState::new(&model);
        assert_eq!(p.click(PanelRow::Switch(ReviewSwitch::Editor)), PanelEffect::OpenApp(ReviewRole::Editor));
        assert_eq!(p.click(PanelRow::Switch(ReviewSwitch::Diff)), PanelEffect::OpenApp(ReviewRole::Diff));
        assert_eq!(p.active(), ActiveView::Chat, "the chat stays shown");
        assert_eq!(
            activate_row(PanelRow::Switch(ReviewSwitch::Editor), false, false, AppTools::default()),
            (PanelEffect::ShowEditor, Some(ActiveView::Editor)),
            "a CLI editor is a pane"
        );
    }

    #[test]
    fn the_pr_row_sits_under_the_summary_and_opens_the_page() {
        let mut with_pr = info(false, false);
        with_pr.body = "A body.".into();
        with_pr.pr = Some(IssuePr::new(7, Some("https://h/pull/7".into())));
        let m = ReviewPanelModel::build(&with_pr, None);
        assert_eq!(m.pr_url(), Some("https://h/pull/7"));
        let mut p = ReviewPanelState::new(&m);
        assert_eq!(&p.rows()[..3], [PanelRow::Back, PanelRow::More, PanelRow::Pr]);
        assert_eq!(p.click(PanelRow::Pr), PanelEffect::OpenPr);

        // A number with nowhere to go isn't a row, and no PR isn't either.
        with_pr.pr = Some(IssuePr::new(7, None));
        let m = ReviewPanelModel::build(&with_pr, None);
        assert!(!ReviewPanelState::new(&m).rows().contains(&PanelRow::Pr));
        with_pr.pr = None;
        let m = ReviewPanelModel::build(&with_pr, None);
        assert!(!ReviewPanelState::new(&m).rows().contains(&PanelRow::Pr));
    }

    #[test]
    fn the_panel_pr_follows_the_row_that_keeps_up_with_the_board() {
        let mut opened = info(false, false);
        opened.pr = Some(IssuePr::new(7, Some("https://h/pull/7".into())));
        let mut fresh = row(ReviewState::Serving);
        fresh.pr = Some(IssuePr {
            ci: Some(shelbi_core::PrCi::Failing),
            ..IssuePr::new(7, Some("https://h/pull/7".into()))
        });
        let m = ReviewPanelModel::build(&opened, Some(&fresh));
        assert_eq!(m.pr.and_then(|p| p.ci), Some(shelbi_core::PrCi::Failing));
        // A row with none says nothing about it: what opened stands.
        let m = ReviewPanelModel::build(&opened, Some(&row(ReviewState::Serving)));
        assert_eq!(m.pr.map(|p| p.number), Some(7));
    }

    #[test]
    fn description_text_strips_block_markdown() {
        assert_eq!(description_text(""), "");
        assert_eq!(description_text("# Title\n\n1. one\n2) two\n* three"), "one two three");
        assert_eq!(description_text("- [x] done\n- [ ] open\n[x] prose"), "done open [x] prose");
        // Inline formatting reads as its text.
        assert_eq!(
            description_text("Fix the **retry** loop in [fetch](https://x/y) so that _flaky_ hosts\nback off."),
            "Fix the retry loop in fetch so that flaky hosts back off."
        );
        assert_eq!(description_text("*one* __two__ ~~three~~ ***four***"), "one two three four");
        assert_eq!(description_text("Run `cargo test` now"), "Run cargo test now");
        assert_eq!(description_text("See <https://x.dev/a> and [ref][r].\n\n[r]: https://y"), "See https://x.dev/a and ref.");
        assert_eq!(description_text("Logo ![the logo](a.png) and ![](b.png) here"), "Logo the logo and here");
        // HTML tags are dropped, their text kept; HTML blocks go entirely.
        assert_eq!(description_text("Press <kbd>Enter</kbd> to go"), "Press Enter to go");
        assert_eq!(description_text("Before\n\n<div align=\"center\">\n<img src=\"x\">\n</div>\n\nAfter"), "Before After");
        // Escapes and entities are decoded.
        assert_eq!(description_text(r"Not \*emphasis\* \[x\] a &amp; b"), "Not *emphasis* [x] a & b");
        // Identifiers that aren't emphasis survive.
        assert_eq!(
            description_text("Rename snake_case_name, MAX_DESC_LINES and a*b"),
            "Rename snake_case_name, MAX_DESC_LINES and a*b"
        );
        // Headings (ATX and setext) are dropped.
        assert_eq!(description_text("## Current Behavior\nIt breaks.\n\nSetext\n======\nMore."), "It breaks. More.");
        // Code blocks (fenced and indented), tables and rules are dropped.
        let body = "Intro.\n\n```\n┌──────┐\n│ mock │\n└──────┘\n```\n\n    indented code\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\nOutro.";
        assert_eq!(description_text(body), "Intro. Outro.");
        // Blockquotes and nested lists join; a checkbox doesn't show, even in a
        // quote or a loose list.
        assert_eq!(
            description_text("> quoted **line**\n> - [x] boxed\n\n- loose\n\n  - [ ] nested\n"),
            "quoted line boxed loose nested"
        );
        // Paragraphs and soft or hard breaks become single spaces.
        assert_eq!(description_text("one  \ntwo\\\nthree\n\n\nfour"), "one two three four");
    }

    // --- decisions in flight -------------------------------------------------

    #[test]
    fn an_ended_review_returns_to_a_view_that_still_makes_sense() {
        let row = WorkspaceRow {
            name: "dev".into(),
            ..WorkspaceRow::default()
        };
        let workspaces = std::slice::from_ref(&row);
        let session = |name: &str| View::Session(name.into());
        let go = |view, slot| ended_review_return(view, slot, workspaces);
        assert_eq!(go(View::Issues, Some("dev")), View::Issues);
        assert_eq!(go(session("orch"), Some("dev")), session("orch"));
        assert_eq!(go(session("dev"), Some("review-1")), session("dev"));
        assert_eq!(go(session("dev"), Some("dev")), session("orch"), "its own slot's page");
        assert_eq!(go(session("gone"), None), session("orch"), "a workspace no longer declared");
        assert_eq!(
            ended_review_return(session("gone"), None, &[]),
            session("gone"),
            "nothing read yet says nothing"
        );
    }

    #[test]
    fn a_review_takes_one_decision_at_a_time() {
        let mut d = ReviewDecisions::default();
        assert!(d.begin("a", Decision::Approve));
        assert!(!d.begin("a", Decision::Reject), "one is running");
        assert_eq!(d.running("a"), Some(Decision::Approve), "the first stands");
        assert!(d.begin("b", Decision::Reject), "another review isn't held up");
        assert_eq!(d.notices().len(), 2);
    }

    #[test]
    fn a_decision_lands_once() {
        let mut d = ReviewDecisions::default();
        d.begin("a", Decision::Approve);
        assert_eq!(d.landed("a"), Some(Decision::Approve));
        assert_eq!(d.landed("a"), None, "the reply follows the board in saying so");
        assert!(d.notices().is_empty());
        assert!(d.begin("a", Decision::Approve), "free again");
    }

    #[test]
    fn a_failure_waits_and_can_be_retried() {
        let mut d = ReviewDecisions::default();
        d.begin("a", Decision::Reject);
        assert_eq!(d.failed("a", "refused"), Some(Decision::Reject));
        assert_eq!(d.failed("a", "again"), None, "nothing is running");
        assert_eq!(d.running("a"), None);
        assert_eq!(d.landed("a"), None, "a failure didn't land");
        assert_eq!(d.notices()[0].failure.as_deref(), Some("refused"));

        assert!(d.begin("a", Decision::Approve), "a failure doesn't hold the review");
        assert_eq!(d.notices().len(), 1, "and goes when it's tried again");
        assert_eq!(d.notices()[0].failure, None);
    }

    #[test]
    fn a_failure_moves_to_the_panel_or_is_dismissed() {
        let mut d = ReviewDecisions::default();
        d.begin("a", Decision::Approve);
        assert_eq!(d.take_failure("a"), None, "still running");
        d.dismiss("a");
        assert_eq!(d.running("a"), Some(Decision::Approve), "dismissing leaves a running one");

        d.failed("a", "refused");
        assert_eq!(d.take_failure("a").as_deref(), Some("approve failed: refused"));
        assert!(d.notices().is_empty());

        d.begin("a", Decision::Approve);
        d.failed("a", "refused");
        d.dismiss("a");
        assert!(d.notices().is_empty());
    }

    #[test]
    fn a_decisions_wording() {
        assert_eq!(Decision::Approve.working("t"), "Merging t…");
        assert_eq!(Decision::Reject.working("t"), "Rejecting t…");
        assert_eq!(Decision::Approve.done("t"), "Approved t.");
        assert_eq!(Decision::Reject.done("t"), "Rejected t.");
        assert_eq!(Decision::Approve.failed_on("t", "no"), "Couldn't approve t: no");
        assert_eq!(Decision::Reject.failed_on("t", "no"), "Couldn't reject t: no");
        assert_eq!(Decision::Approve.row_note(), "Merging…");
        assert_eq!(Decision::Reject.row_note(), "Rejecting…");
    }
}
