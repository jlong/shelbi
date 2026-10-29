use super::*;
use shelbi_core::{Column, Issue};

const DEBOUNCE: Duration = Duration::from_millis(800);

fn rev(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
}

fn at(status: &str, secs: i64) -> ExpectedState {
    ExpectedState {
        status: status.into(),
        updated_at: rev(secs).to_rfc3339(),
    }
}

/// Issue `T` as a read has it: in `status` at revision `secs`.
fn read(status: &str, secs: i64, title: &str, body: &str) -> IssueFile {
    IssueFile {
        task: Issue {
            id: "T".into(),
            title: title.into(),
            column: Column::from_status_id(status),
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: rev(0),
            updated_at: rev(secs),
            params: Default::default(),
        },
        body: body.into(),
        tracker_assignees: Vec::new(),
    }
}

const BODY: &str = "# Plan\n\nfirst\nsecond\nthird\n";

/// An editor open on `T` (`todo`, revision 0, title "Title", [`BODY`]).
fn open() -> (IssueEdits, Instant) {
    let mut e = IssueEdits::with_debounce(DEBOUNCE);
    e.open(&Snapshot::of(&read("todo", 0, "Title", BODY)));
    (e, Instant::now())
}

fn ms(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

fn body(e: &IssueEdits) -> &str {
    e.text("T", Field::Body).unwrap()
}

#[test]
fn typing_saves_once_after_the_pause_with_the_latest_text() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "# Plan\n\nfirst!\nsecond\nthird\n", t0);
    e.edit("T", Field::Body, "# Plan\n\nfirst!!\nsecond\nthird\n", ms(t0, 300));
    assert_eq!(e.status("T"), SaveStatus::Unsaved);
    assert!(e.poll(ms(t0, 900)).is_empty(), "the pause runs from the last keystroke");
    let jobs = e.poll(ms(t0, 1100));
    assert_eq!(jobs.len(), 1, "coalesced into one save");
    let job = &jobs[0];
    assert_eq!(job.body.as_deref(), Some("# Plan\n\nfirst!!\nsecond\nthird\n"));
    assert_eq!(job.expect_body.as_deref(), Some(BODY));
    assert_eq!(job.title, None, "an untouched field isn't sent");
    assert_eq!(job.expected, Some(at("todo", 0)));
    assert_eq!(e.status("T"), SaveStatus::Saving);
    assert!(e.poll(ms(t0, 5000)).is_empty(), "nothing more to send while it's out");
    e.finish(job, Ok(Some(at("todo", 5))), ms(t0, 1200));
    assert_eq!(e.status("T"), SaveStatus::Saved);
    assert!(e.poll(ms(t0, 9000)).is_empty());
}

#[test]
fn an_edit_that_stores_the_same_is_never_sent() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "# Plan\n\nfirst\nsecond\nthird\n\n\n", t0);
    e.edit("T", Field::Title, "  Title ", t0);
    assert_eq!(e.status("T"), SaveStatus::Clean);
    assert!(e.poll(ms(t0, 2000)).is_empty());
    e.edit("T", Field::Body, "changed", t0);
    e.edit("T", Field::Body, BODY, ms(t0, 100));
    assert!(e.poll(ms(t0, 2000)).is_empty(), "typed and put back");
    assert!(e.flush("T").is_none());
}

#[test]
fn typing_while_a_save_is_out_saves_next_against_the_landed_revision() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "one\n", t0);
    let first = e.flush("T").expect("a flush sends without the pause");
    e.edit("T", Field::Body, "one two\n", ms(t0, 100));
    assert!(e.flush("T").is_none(), "one write at a time");
    e.finish(&first, Ok(Some(at("todo", 7))), ms(t0, 200));
    assert_eq!(body(&e), "one two\n", "the newer text is kept");
    assert_eq!(e.status("T"), SaveStatus::Unsaved);
    // The flush asked for during the first save goes as soon as it lands.
    let second = e.poll(ms(t0, 250)).pop().expect("sent without waiting for the pause");
    assert_eq!(second.body.as_deref(), Some("one two\n"));
    assert_eq!(second.expect_body.as_deref(), Some("one\n"), "it replaces what landed");
    assert_eq!(second.expected, Some(at("todo", 7)), "the landed revision, not the read's");
}

#[test]
fn reads_that_trail_a_landed_save_change_nothing() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "mine\n", t0);
    let job = e.flush("T").unwrap();
    // A read from before the save arrives while it's out, then the ack.
    e.observe(&[read("todo", 0, "Title", BODY)]);
    e.finish(&job, Ok(Some(at("todo", 7))), ms(t0, 10));
    assert!(e.take_updates().is_empty(), "the old copy doesn't undo the save");
    assert_eq!(body(&e), "mine\n");
    // The board index still serves the old copy after the ack.
    e.observe(&[read("todo", 0, "Title", BODY)]);
    // Even one that claims the landed revision but has the old text (an
    // index entry stamped late).
    e.observe(&[read("todo", 7, "Title", BODY)]);
    assert!(e.take_updates().is_empty());
    assert_eq!(body(&e), "mine\n");
    assert_eq!(e.status("T"), SaveStatus::Saved);
    // The index catches up: nothing to do.
    e.observe(&[read("todo", 7, "Title", "mine\n")]);
    assert!(e.take_updates().is_empty());
    assert_eq!(e.status("T"), SaveStatus::Saved);
}

#[test]
fn a_change_elsewhere_replaces_untouched_fields_and_merges_apart_lines() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "# Plan\n\nFIRST\nsecond\nthird\n", t0);
    // An agent renames the issue and appends a note.
    let theirs = "# Plan\n\nfirst\nsecond\nthird\n\n## Notes\nagent\n";
    e.observe(&[read("todo", 3, "Renamed", theirs)]);
    let updates = e.take_updates();
    assert_eq!(
        updates,
        vec![
            Update { id: "T".into(), field: Field::Title, text: "Renamed".into() },
            Update {
                id: "T".into(),
                field: Field::Body,
                text: "# Plan\n\nFIRST\nsecond\nthird\n\n## Notes\nagent\n".into()
            },
        ]
    );
    let job = e.poll(ms(t0, 1000)).pop().unwrap();
    assert_eq!(job.title, None, "the title is theirs, untouched here");
    assert_eq!(job.body.as_deref(), Some("# Plan\n\nFIRST\nsecond\nthird\n\n## Notes\nagent\n"));
    assert_eq!(job.expect_body.as_deref(), Some(theirs), "rebased on their copy");
    assert_eq!(job.expected, Some(at("todo", 3)));
}

#[test]
fn a_clean_draft_follows_the_store() {
    let (mut e, _) = open();
    e.observe(&[read("doing", 4, "Title", "rewritten by the CLI\n")]);
    assert_eq!(body(&e), "rewritten by the CLI\n");
    assert_eq!(e.take_updates().len(), 1);
    assert_eq!(e.status("T"), SaveStatus::Clean);
}

#[test]
fn the_same_line_changed_both_ways_keeps_both_until_the_user_picks() {
    let (mut e, t0) = open();
    let mine = "# Plan\n\nmy first\nsecond\nthird\n";
    let theirs = "# Plan\n\ntheir first\nsecond\nthird\n";
    e.edit("T", Field::Body, mine, t0);
    e.observe(&[read("todo", 3, "Title", theirs)]);
    assert!(e.take_updates().is_empty(), "the visible text is the draft's");
    assert_eq!(body(&e), mine);
    assert_eq!(e.status("T"), SaveStatus::Conflict(Field::Body));
    assert_eq!(
        e.conflict("T"),
        Some(Conflict { field: Field::Body, mine: mine.into(), theirs: theirs.into() })
    );
    assert!(e.poll(ms(t0, 5000)).is_empty(), "nothing saves over their change");
    assert!(e.flush("T").is_none());

    // Keep mine: it saves over theirs, guarded on theirs.
    let mut keep = e.clone_for_test();
    keep.resolve("T", Field::Body, Resolution::KeepMine);
    let job = keep.poll(ms(t0, 5000)).pop().unwrap();
    assert_eq!(job.body.as_deref(), Some(mine));
    assert_eq!(job.expect_body.as_deref(), Some(theirs));

    // Take theirs: the editor shows it, mine is set aside, nothing to save.
    e.resolve("T", Field::Body, Resolution::TakeTheirs);
    assert_eq!(body(&e), theirs);
    assert_eq!(e.take_updates().pop().unwrap().text, theirs);
    assert_eq!(e.set_aside("T", Field::Body), Some(mine));
    assert!(e.poll(ms(t0, 5000)).is_empty());
    assert_eq!(e.status("T"), SaveStatus::Clean);
}

#[test]
fn a_conflict_the_other_side_withdraws_clears() {
    let (mut e, t0) = open();
    e.edit("T", Field::Title, "Mine", t0);
    e.observe(&[read("todo", 3, "Theirs", BODY)]);
    assert_eq!(e.status("T"), SaveStatus::Conflict(Field::Title));
    e.observe(&[read("todo", 4, "Title", BODY)]);
    assert_eq!(e.status("T"), SaveStatus::Unsaved);
    assert_eq!(e.poll(ms(t0, 1000)).pop().unwrap().title.as_deref(), Some("Mine"));
}

#[test]
fn a_refused_save_waits_for_the_new_copy_then_rebases_and_retries() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "# Plan\n\nFIRST\nsecond\nthird\n", t0);
    let job = e.poll(ms(t0, 900)).pop().unwrap();
    e.finish(&job, Err(ExecError::Conflict("changed".into())), ms(t0, 1000));
    assert_eq!(e.status("T"), SaveStatus::Saving, "quietly waiting for the new copy");
    assert_eq!(body(&e), "# Plan\n\nFIRST\nsecond\nthird\n", "the draft is intact");
    assert!(e.poll(ms(t0, 2000)).is_empty());
    // A read that shows nothing new keeps it waiting.
    e.observe(&[read("todo", 0, "Title", BODY)]);
    assert!(e.poll(ms(t0, 2100)).is_empty());
    // The new copy: someone moved it and edited a line far away.
    let theirs = "# Plan\n\nfirst\nsecond\nthird\nfourth\n";
    e.observe(&[read("doing", 2, "Title", theirs)]);
    let job = e.poll(ms(t0, 2200)).pop().expect("sent again at once");
    assert_eq!(job.body.as_deref(), Some("# Plan\n\nFIRST\nsecond\nthird\nfourth\n"));
    assert_eq!(job.expected, Some(at("doing", 2)));
    assert_eq!(job.expect_body.as_deref(), Some(theirs));
}

#[test]
fn a_refused_save_with_no_new_copy_gives_up_and_says_so() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "mine\n", t0);
    let job = e.flush("T").unwrap();
    e.finish(&job, Err(ExecError::Conflict("changed".into())), t0);
    assert!(e.poll(ms(t0, 14_000)).is_empty());
    e.poll(t0 + STALE_WAIT);
    assert!(matches!(e.status("T"), SaveStatus::Failed(_)), "{:?}", e.status("T"));
    assert_eq!(body(&e), "mine\n");
    assert!(e.retry("T").is_some());
}

#[test]
fn a_failed_save_keeps_the_draft_until_a_retry_or_the_next_edit() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "mine\n", t0);
    let job = e.poll(ms(t0, 900)).pop().unwrap();
    e.finish(&job, Err(ExecError::Backend("gh: 502".into())), ms(t0, 1000));
    assert_eq!(e.status("T"), SaveStatus::Failed("gh: 502".into()));
    assert_eq!(e.status("T").label().as_deref(), Some("Not saved: gh: 502"));
    assert_eq!(body(&e), "mine\n");
    assert!(e.poll(ms(t0, 60_000)).is_empty(), "no retry loop against a failing backend");
    let retry = e.retry("T").expect("retry sends it");
    assert_eq!(retry.body.as_deref(), Some("mine\n"));
    e.finish(&retry, Err(ExecError::Backend("gh: 502".into())), ms(t0, 61_000));
    e.edit("T", Field::Body, "mine, more\n", ms(t0, 62_000));
    assert_eq!(e.poll(ms(t0, 63_000)).pop().unwrap().body.as_deref(), Some("mine, more\n"));
}

#[test]
fn a_blank_title_isnt_sent_but_the_body_still_saves() {
    let (mut e, t0) = open();
    e.edit("T", Field::Title, "   ", t0);
    e.edit("T", Field::Body, "mine\n", t0);
    assert_eq!(
        e.status("T"),
        SaveStatus::Invalid { field: Field::Title, message: "A title can't be blank".into() }
    );
    let job = e.poll(ms(t0, 900)).pop().unwrap();
    assert_eq!(job.title, None);
    assert_eq!(job.body.as_deref(), Some("mine\n"));
    e.finish(&job, Ok(Some(at("todo", 3))), ms(t0, 1000));
    assert!(e.poll(ms(t0, 5000)).is_empty());
    assert_eq!(e.text("T", Field::Title), Some("   "), "the transient edit stays on screen");
    e.edit("T", Field::Title, " New title ", ms(t0, 6000));
    let job = e.poll(ms(t0, 7000)).pop().unwrap();
    assert_eq!(job.title.as_deref(), Some("New title"), "sent trimmed");
    assert_eq!(job.expect_title.as_deref(), Some("Title"));
}

#[test]
fn other_writes_share_the_lane_and_the_landed_revisions() {
    let (mut e, t0) = open();
    // A move goes out; typing meanwhile waits for it.
    let move_ticket = e.begin_write("T", &at("todo", 0)).unwrap();
    e.edit("T", Field::Body, "mine\n", t0);
    assert!(e.poll(ms(t0, 5000)).is_empty(), "the save waits for the move");
    e.end_write(&move_ticket, Some(at("doing", 6)));
    // The board's read still says todo@0: the save expects where the move landed.
    e.observe(&[read("todo", 0, "Title", BODY)]);
    let save = e.poll(ms(t0, 5100)).pop().unwrap();
    assert_eq!(save.expected, Some(at("doing", 6)));
    // A reorder while the save is out is held, not refused.
    assert!(e.begin_write("T", &at("doing", 6)).is_none());
    assert!(e.busy("T"));
    e.finish(&save, Ok(Some(at("doing", 8))), ms(t0, 5200));
    assert!(!e.busy("T"));
    assert_eq!(e.expected("T", at("todo", 0)), at("doing", 8), "seen before own writes only");
    assert_eq!(e.expected("T", at("todo", 7)), at("doing", 8));
    let reorder = e.begin_write("T", &e.expected("T", at("todo", 0))).unwrap();
    e.end_write(&reorder, Some(at("doing", 9)));
    assert_eq!(body(&e), "mine\n");
    assert_eq!(e.status("T"), SaveStatus::Saved);
    // A read that has caught up ends the substitution: a state seen before
    // it is someone else's, and is sent as seen.
    e.observe(&[read("doing", 9, "Title", "mine\n")]);
    assert_eq!(e.expected("T", at("todo", 0)), at("todo", 0));
}

#[test]
fn a_property_write_alone_keeps_its_landed_revision_until_a_read_shows_it() {
    let mut e = IssueEdits::new();
    let t = e.begin_write("X", &at("todo", 0)).unwrap();
    e.end_write(&t, Some(at("doing", 4)));
    assert_eq!(e.expected("X", at("todo", 0)), at("doing", 4));
    assert!(e.watching());
    let mut x = read("doing", 4, "X", "b");
    x.task.id = "X".into();
    e.observe(&[x]);
    assert!(!e.watching(), "nothing left to track");
}

#[test]
fn leaving_the_task_flushes_and_keeps_a_draft_that_didnt_save() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "mine\n", t0);
    let job = e.close("T").expect("closing flushes without the pause");
    assert!(e.has_draft("T"), "kept until it lands");
    e.finish(&job, Err(ExecError::Backend("offline".into())), ms(t0, 100));
    assert!(e.has_draft("T"), "a failed final flush keeps the draft");
    assert_eq!(e.unsaved(), vec![("T".to_string(), SaveStatus::Failed("offline".into()))]);
    // Reopening shows the draft, not the read.
    e.open(&Snapshot::of(&read("todo", 0, "Title", BODY)));
    assert_eq!(body(&e), "mine\n");
    let job = e.retry("T").unwrap();
    e.finish(&job, Ok(Some(at("todo", 3))), ms(t0, 200));
    assert!(e.close("T").is_none());
    assert!(!e.has_draft("T"), "a saved, closed draft is dropped");
    assert!(e.unsaved().is_empty());
}

#[test]
fn a_clean_draft_is_dropped_on_close() {
    let (mut e, _) = open();
    assert!(e.close("T").is_none());
    assert!(!e.has_draft("T"));
    assert!(!e.watching());
}

#[test]
fn a_workflow_change_goes_at_once_with_the_draft_and_drops_if_refused() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "mine\n", t0);
    e.set_workflow(&Snapshot::of(&read("todo", 0, "Title", BODY)), "subtask");
    let job = e.poll(t0).pop().expect("no pause for a property");
    assert_eq!(job.workflow.as_deref(), Some("subtask"));
    assert_eq!(job.body.as_deref(), Some("mine\n"), "typing rides along");
    e.finish(&job, Err(ExecError::Backend("workflow `subtask` does not exist".into())), t0);
    assert_eq!(
        e.status("T"),
        SaveStatus::Failed("workflow `subtask` does not exist".into())
    );
    let retry = e.retry("T").unwrap();
    assert_eq!(retry.workflow, None, "the refused workflow isn't sent again");
    assert_eq!(retry.body.as_deref(), Some("mine\n"));
}

#[test]
fn a_save_job_is_the_daemon_edit_mutation() {
    let (mut e, t0) = open();
    e.edit("T", Field::Title, "New", t0);
    let job = e.flush("T").unwrap();
    assert_eq!(
        job.mutation("p"),
        Mutation::EditIssue {
            project: "p".into(),
            id: "T".into(),
            title: Some("New".into()),
            body: None,
            workflow: None,
            expect_title: Some("Title".into()),
            expect_body: None,
        }
    );
}

#[test]
fn a_late_answer_for_an_old_job_is_ignored() {
    let (mut e, t0) = open();
    e.edit("T", Field::Body, "mine\n", t0);
    let job = e.flush("T").unwrap();
    e.finish(&job, Err(ExecError::Backend("x".into())), t0);
    let again = e.retry("T").unwrap();
    e.finish(&job, Ok(Some(at("todo", 9))), t0);
    assert_eq!(e.status("T"), SaveStatus::Saving, "the retry is still out");
    e.finish(&again, Ok(Some(at("todo", 9))), t0);
    assert_eq!(e.status("T"), SaveStatus::Saved);
}

#[test]
fn shared_stores_are_one_per_project() {
    let a = shared("issue-edit-test-a");
    let b = shared("issue-edit-test-a");
    let c = shared("issue-edit-test-c");
    assert!(Arc::ptr_eq(&a, &b));
    assert!(!Arc::ptr_eq(&a, &c));
}

impl IssueEdits {
    fn clone_for_test(&self) -> IssueEdits {
        IssueEdits {
            drafts: self.drafts.clone(),
            lanes: self.lanes.clone(),
            updates: self.updates.clone(),
            next_seq: self.next_seq,
            debounce: self.debounce,
        }
    }
}
