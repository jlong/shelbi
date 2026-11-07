use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::diff::parse::parse_patch;

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(0, 0).unwrap()
}

fn comment(author: &str, body: &str) -> PrThreadComment {
    PrThreadComment { id: format!("c-{author}"), author: author.into(), body: body.into(), created_at: at(), pending: false }
}

fn thread(id: &str, path: &str, line: u32, code: Option<&str>) -> PrThread {
    PrThread {
        id: id.into(),
        path: path.into(),
        side: Side::New,
        line: Some(line),
        original_line: Some(line),
        start_line: None,
        code: code.map(str::to_string),
        resolved: false,
        outdated: false,
        file_level: false,
        comments: vec![comment("bob", "this leaks")],
    }
}

fn review(author: &str, state: ReviewState) -> PrReview {
    PrReview { author: author.into(), state, body: String::new() }
}

fn check(name: &str, state: CheckState) -> PrCheck {
    PrCheck { name: name.into(), state, detail: "word".into() }
}

fn text(spans: &[PrSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

fn details() -> PrDetails {
    PrDetails {
        number: 2155,
        title: "Record a task's PR".into(),
        url: "https://github.com/jlong/shelbi/pull/2155".into(),
        body: "Why it matters.".into(),
        author: "jlong".into(),
        head: "jlong/task-pr-metadata".into(),
        base: "main".into(),
        state: PrState::Open,
        draft: false,
        reviews: vec![review("alice", ReviewState::Approved), review("bob", ReviewState::ChangesRequested)],
        checks: vec![
            check("build", CheckState::Passed),
            check("lint", CheckState::Failed),
            check("msrv", CheckState::Pending),
        ],
        threads: vec![thread("T1", "a.rs", 3, Some("let total = amount + 1;"))],
        threads_cut: false,
    }
}

fn view(d: PrDetails) -> PrView {
    let mut view = PrView::default();
    view.set_details(Ok(Arc::new(d)));
    view
}

// ---- The tally and the verdicts ------------------------------------------

#[test]
fn the_tally_counts_each_state_and_names_what_failed() {
    let checks = [
        check("a", CheckState::Passed),
        check("b", CheckState::Passed),
        check("lint", CheckState::Failed),
        check("c", CheckState::Pending),
    ];
    let tally = ChecksTally::of(&checks);
    assert_eq!((tally.passed, tally.failed, tally.pending), (2, 1, 1));
    assert_eq!(tally.failed_names, ["lint"]);
    assert_eq!(tally.failed_label().as_deref(), Some("(lint)"));
}

#[test]
fn a_long_list_of_failures_is_cut_to_a_count() {
    let checks: Vec<PrCheck> = ["a", "b", "c", "d", "e"].iter().map(|n| check(n, CheckState::Failed)).collect();
    assert_eq!(ChecksTally::of(&checks).failed_label().as_deref(), Some("(a, b, c, +2)"));
}

#[test]
fn a_later_comment_doesnt_withdraw_a_verdict_but_a_dismissal_does() {
    let verdicts = Verdicts::of(&[
        review("alice", ReviewState::Approved),
        review("alice", ReviewState::Commented),
        review("bob", ReviewState::ChangesRequested),
        review("bob", ReviewState::Approved),
        review("carol", ReviewState::ChangesRequested),
        review("carol", ReviewState::Dismissed),
        review("dan", ReviewState::Pending),
    ]);
    assert_eq!(verdicts.approved, ["alice", "bob"]);
    assert!(verdicts.changes_requested.is_empty());
}

// ---- What the block says -------------------------------------------------

#[test]
fn the_block_reads_as_the_wireframe_has_it() {
    let view = view(details());
    assert_eq!(view.parts(), [PrPart::Title, PrPart::Status, PrPart::Gap, PrPart::Body(0)]);
    assert_eq!(text(&view.spans(PrPart::Title)), "▾ #2155 Record a task's PR  jlong · jlong/task-pr-metadata → main");
    assert_eq!(
        text(&view.spans(PrPart::Status)),
        "✓ approved by alice · ✗ changes requested by bob · checks ✓1 ✗1 ●1 (lint) · 1 thread open"
    );
    assert_eq!(text(&view.spans(PrPart::Body(0))), "Why it matters.");
}

#[test]
fn folding_leaves_the_first_two_rows() {
    let mut view = view(details());
    view.folded = true;
    assert_eq!(view.parts(), [PrPart::Title, PrPart::Status]);
    assert!(text(&view.spans(PrPart::Title)).starts_with("▸ "));
}

#[test]
fn no_pull_request_means_no_block() {
    let mut view = PrView::default();
    view.set_lookup(Ok(None));
    assert!(!view.shows());
    assert!(view.parts().is_empty());
}

#[test]
fn a_recorded_pr_shows_while_its_details_are_read() {
    let mut view = PrView::default();
    view.set_lookup(Ok(Some(IssuePr::new(2155, None))));
    assert!(view.loading);
    assert_eq!(view.parts(), [PrPart::Title, PrPart::Status]);
    assert_eq!(text(&view.spans(PrPart::Title)), "▾ #2155");
    assert_eq!(text(&view.spans(PrPart::Status)), "Reading the pull request…");
}

#[test]
fn offline_the_block_says_why_the_parts_are_missing() {
    let mut view = PrView::default();
    view.set_lookup(Ok(Some(IssuePr::new(2155, None))));
    view.set_details(Err("Can't reach GitHub".to_string()));
    assert!(!view.loading);
    assert_eq!(text(&view.spans(PrPart::Status)), "PR details aren't here: Can't reach GitHub");
    assert_eq!(text(&view.info_rows(&Changeset::default())[0]), "Not current: Can't reach GitHub");
}

#[test]
fn a_failed_read_keeps_what_was_read_and_says_it_isnt_current() {
    let mut view = view(details());
    view.set_details(Err("GitHub's rate limit is spent until 14:05".to_string()));
    let status = text(&view.spans(PrPart::Status));
    assert!(status.contains("approved by alice"), "{status}");
    assert!(status.ends_with("not current: GitHub's rate limit is spent until 14:05"), "{status}");
    view.set_details(Ok(Arc::new(details())));
    assert!(!text(&view.spans(PrPart::Status)).contains("not current"));
}

#[test]
fn a_lookup_that_fails_has_no_number_to_show() {
    let mut view = PrView::default();
    view.set_lookup(Err("Can't reach GitHub".to_string()));
    assert!(view.shows());
    assert_eq!(text(&view.spans(PrPart::Title)), "▾ Pull request");
}

#[test]
fn a_merged_or_draft_pr_says_so_and_pending_comments_are_counted() {
    let mut d = details();
    d.state = PrState::Merged;
    d.draft = true;
    d.threads[0].comments[0].pending = true;
    d.threads[0].comments.push(PrThreadComment { pending: true, ..comment("me", "and") });
    let view = view(d);
    let title = text(&view.spans(PrPart::Title));
    assert!(title.ends_with(" · draft · merged"), "{title}");
    assert!(text(&view.spans(PrPart::Status)).contains("2 comments pending (R submits)"));
}

#[test]
fn the_panel_has_every_check_and_review_with_its_first_line() {
    let mut d = details();
    d.reviews[0].body = "\nLooks right.\nMore detail.".into();
    d.threads.push(thread("T2", "elsewhere.rs", 1, None));
    d.threads[1].resolved = true;
    d.threads_cut = true;
    let rows: Vec<String> = view(d).info_rows(&Changeset {
        files: parse_patch(PATCH),
        ..Changeset::default()
    })
    .iter()
    .map(|row| text(row))
    .collect();
    let at = |needle: &str| rows.iter().position(|r| r.contains(needle)).unwrap_or_else(|| panic!("{needle}: {rows:?}"));
    assert!(at("✗ lint") < at("● msrv") && at("● msrv") < at("✓ build"), "failures first");
    assert!(rows.contains(&"✓ alice approved: Looks right.".to_string()), "{rows:?}");
    assert!(rows.contains(&"✗ bob requested changes".to_string()), "{rows:?}");
    assert!(rows.iter().any(|r| r.contains("1 open, 1 resolved, 1 on files not in this view (the first 100")), "{rows:?}");
    assert!(at("Description") > at("Threads"));
    assert!(at("Why it matters.") > at("Description"));
}

#[test]
fn a_description_loses_its_comments_and_long_lines_wrap() {
    let long = "word ".repeat(40);
    let lines = body_lines(&format!("\r\n<!-- the template's\nguidance -->\nFirst.\r\n\r\n{}\n\n<!-- open", long.trim()));
    assert_eq!(lines[0], "First.");
    assert_eq!(lines[1], "");
    assert!(lines.len() > 3, "{lines:?}");
    assert!(lines[2..].iter().all(|l| l.chars().count() <= BODY_WIDTH), "{lines:?}");
    assert_eq!(lines[2..].join(" "), long.trim());
    assert!(!lines.join("\n").contains("guidance"));
}

// ---- Where threads go ----------------------------------------------------

const PATCH: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@ one\n ctx\n-let total = count + 1;\n+// a comment line\n+let total = amount + 1;\n end\n";

fn cs() -> Changeset {
    Changeset { files: parse_patch(PATCH), ..Changeset::default() }
}

fn place_one(t: PrThread) -> Option<Placement> {
    let id = t.comment_id();
    place_threads(&[t], &cs()).remove(&id)
}

#[test]
fn a_thread_lands_on_the_line_github_names() {
    let found = place_one(thread("T", "a.rs", 3, Some("let total = amount + 1;"))).unwrap();
    assert_eq!(found.file, 0);
    assert_eq!(found.at, Some((0, 3)), "the fourth line of the hunk");
    assert_eq!(found.lines, Some((3, 3)));
}

#[test]
fn a_thread_follows_its_text_when_the_worktree_has_moved_on() {
    let found = place_one(thread("T", "a.rs", 40, Some("let total = amount + 1;"))).unwrap();
    assert_eq!(found.at, Some((0, 3)));
}

#[test]
fn a_thread_on_the_old_side_is_placed_on_the_old_side() {
    let mut t = thread("T", "a.rs", 2, Some("let total = count + 1;"));
    t.side = Side::Old;
    assert_eq!(place_one(t).unwrap().at, Some((0, 1)));
}

#[test]
fn an_outdated_thread_uses_its_original_line() {
    let mut t = thread("T", "a.rs", 3, Some("let total = amount + 1;"));
    (t.line, t.outdated) = (None, true);
    assert_eq!(place_one(t).unwrap().at, Some((0, 3)));
}

#[test]
fn a_thread_whose_text_is_gone_sits_at_the_top_of_its_file() {
    let found = place_one(thread("T", "a.rs", 3, Some("rewritten"))).unwrap();
    assert_eq!((found.file, found.at, found.lines), (0, None, None));
}

#[test]
fn with_no_text_only_the_exact_line_will_do() {
    assert_eq!(place_one(thread("T", "a.rs", 3, None)).unwrap().at, Some((0, 3)));
    assert_eq!(place_one(thread("T", "a.rs", 90, None)).unwrap().at, None);
}

#[test]
fn a_file_thread_has_no_line() {
    let mut t = thread("T", "a.rs", 3, Some("let total = amount + 1;"));
    t.file_level = true;
    assert_eq!(place_one(t).unwrap().at, None);
}

#[test]
fn a_thread_on_a_file_not_in_the_diff_has_no_place() {
    assert_eq!(place_one(thread("T", "gone.rs", 3, None)), None);
}

#[test]
fn a_thread_reads_as_a_comment_with_replies() {
    let mut t = thread("T", "a.rs", 3, Some("code"));
    t.comments.push(comment("jlong", "fixed in 3f2a1c"));
    let c = t.as_comment().unwrap();
    assert_eq!(c.id, "gh:T");
    assert_eq!(c.author.name, "⎇ bob (GitHub)");
    assert_eq!(c.body, "this leaks");
    assert_eq!(c.replies[0].author.name, "jlong");
    assert_eq!(c.anchor.code, ["code"]);
    assert_eq!(t.tag(), ("[unresolved]", PrTone::Pending));
    t.resolved = true;
    assert_eq!(t.tag(), ("[resolved]", PrTone::Muted));
    assert!(!t.opens_expanded());
}

// ---- gh, stubbed ---------------------------------------------------------

#[derive(Default)]
struct StubGh {
    /// Each call's args, marked `r` or `w`.
    calls: Mutex<Vec<(char, Vec<String>)>>,
    answers: Mutex<Vec<Result<String, String>>>,
}

impl StubGh {
    fn answering(answers: Vec<Result<String, String>>) -> StubGh {
        StubGh { calls: Mutex::default(), answers: Mutex::new(answers) }
    }

    fn answer(&self, kind: char, args: &[String]) -> Result<String, String> {
        self.calls.lock().unwrap().push((kind, args.to_vec()));
        let mut answers = self.answers.lock().unwrap();
        if answers.is_empty() {
            return Ok("{}".to_string());
        }
        answers.remove(0)
    }

    fn calls(&self) -> Vec<(char, Vec<String>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl GhApi for StubGh {
    fn read(&self, args: &[String]) -> Result<String, String> {
        self.answer('r', args)
    }

    fn write(&self, args: &[String]) -> Result<String, String> {
        self.answer('w', args)
    }
}

/// Every `-f key=value` and `-F key=value` of `args`, with its flag.
fn vars(args: &[String]) -> Vec<(String, String)> {
    args.chunks(2)
        .skip(1)
        .filter(|pair| !pair[1].starts_with("query="))
        .map(|pair| (pair[0].clone(), pair[1].clone()))
        .collect()
}

fn query_of(args: &[String]) -> &str {
    args.iter().find_map(|a| a.strip_prefix("query=")).unwrap()
}

fn has(args: &[String], flag: &str, pair: &str) -> bool {
    vars(args).contains(&(flag.to_string(), pair.to_string()))
}

fn repo() -> Repo {
    Repo { owner: "jlong".into(), name: "shelbi".into() }
}

fn pending_answer(review: Option<&str>) -> String {
    let nodes: Vec<_> = review.iter().map(|id| json!({"id": id, "viewerDidAuthor": true})).collect();
    json!({"data": {"repository": {"pullRequest": {"id": "PR1", "reviews": {"nodes": nodes}}}}}).to_string()
}

#[test]
fn fetch_reads_the_pull_request_through_gh_and_parses_it() {
    let answer = json!({"data": {"repository": {"pullRequest": {
        "number": 2155, "title": "T", "url": "u", "state": "OPEN", "isDraft": false,
        "headRefName": "h", "baseRefName": "main", "author": {"login": "jlong"},
        "reviews": {"nodes": [{"author": null, "state": "APPROVED", "body": ""}, {"author": {"login": "x"}, "state": "NEW_KIND", "body": ""}]},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
            {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE"},
            {"__typename": "CheckRun", "name": "skip", "status": "COMPLETED", "conclusion": "SKIPPED"},
            {"__typename": "CheckRun", "name": "run", "status": "IN_PROGRESS", "conclusion": null},
            {"__typename": "StatusContext", "context": "ci/ok", "state": "SUCCESS"},
            {"__typename": "StatusContext", "context": "ci/wait", "state": "PENDING"},
            {"__typename": "StatusContext", "context": "ci/err", "state": "ERROR"}
        ]}}}}]},
        "reviewThreads": {"pageInfo": {"hasNextPage": true}, "nodes": [
            {"id": "T1", "isResolved": true, "isOutdated": true, "path": "a.rs", "line": null, "originalLine": 3,
             "startLine": null, "diffSide": "LEFT", "subjectType": "LINE",
             "comments": {"nodes": [
                {"id": "C1", "author": {"login": "bob"}, "body": "b", "createdAt": "2026-11-05T00:00:00Z", "state": "PENDING",
                 "diffHunk": "@@ -1,2 +1,2 @@\n ctx\n-let total = count + 1;"}
             ]}},
            {"id": "T2", "isResolved": false, "isOutdated": false, "path": "b.rs", "line": null, "originalLine": null,
             "startLine": null, "diffSide": "RIGHT", "subjectType": "FILE",
             "comments": {"nodes": [{"id": "C2", "author": null, "body": "", "createdAt": "2026-11-05T00:00:00Z", "state": "SUBMITTED", "diffHunk": ""}]}},
            {"id": "T3", "isResolved": false, "isOutdated": false, "path": "c.rs", "line": 1, "originalLine": 1,
             "startLine": null, "diffSide": "RIGHT", "subjectType": "LINE", "comments": {"nodes": []}}
        ]}
    }}}})
    .to_string();
    let gh = StubGh::answering(vec![Ok(answer)]);
    let d = fetch(&gh, &repo(), 2155).unwrap();

    let calls = gh.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 'r');
    assert_eq!(&calls[0].1[..2], ["api", "graphql"]);
    assert!(query_of(&calls[0].1).contains("ShelbiDiffPr"), "named for the request log");
    assert!(has(&calls[0].1, "-f", "owner=jlong") && has(&calls[0].1, "-f", "name=shelbi"));
    assert!(has(&calls[0].1, "-F", "number=2155"));

    assert_eq!(d.author, "jlong");
    assert_eq!(d.reviews.len(), 1, "a kind we don't know is left out");
    assert_eq!(d.reviews[0].author, "ghost");
    let states: Vec<(&str, CheckState, &str)> = d.checks.iter().map(|c| (c.name.as_str(), c.state, c.detail.as_str())).collect();
    assert_eq!(
        states,
        [
            ("lint", CheckState::Failed, "failure"),
            ("skip", CheckState::Passed, "skipped"),
            ("run", CheckState::Pending, "in progress"),
            ("ci/ok", CheckState::Passed, "success"),
            ("ci/wait", CheckState::Pending, "pending"),
            ("ci/err", CheckState::Failed, "error"),
        ]
    );
    assert!(d.threads_cut);
    assert_eq!(d.threads.len(), 2, "a thread with no comment isn't kept");
    let t = &d.threads[0];
    assert_eq!((t.side, t.line, t.original_line, t.resolved, t.outdated), (Side::Old, None, Some(3), true, true));
    assert_eq!(t.code.as_deref(), Some("let total = count + 1;"), "the hunk's last line, without its marker");
    assert!(t.comments[0].pending);
    assert!(d.threads[1].file_level);
    assert_eq!(d.threads[1].code, None);
    assert_eq!(d.threads[1].comments[0].author, "ghost");
}

#[test]
fn no_checks_at_all_reads_as_none() {
    let answer = json!({"data": {"repository": {"pullRequest": {
        "number": 1, "title": "T", "url": "u", "body": "", "state": "MERGED",
        "headRefName": "h", "baseRefName": "main", "author": {"login": "j"},
        "reviews": {"nodes": []},
        "commits": {"nodes": [{"commit": {"statusCheckRollup": null}}]},
        "reviewThreads": {"pageInfo": {"hasNextPage": false}, "nodes": []}
    }}}});
    let d = parse_details(&answer.to_string()).unwrap();
    assert_eq!(d.state, PrState::Merged);
    assert!(d.checks.is_empty());
    assert!(text(&view(d).spans(PrPart::Status)).contains("no checks"));
}

#[test]
fn what_gh_says_comes_back_as_the_reason() {
    let gh = StubGh::answering(vec![Err("Can't reach GitHub".to_string())]);
    assert_eq!(fetch(&gh, &repo(), 1).unwrap_err(), "Can't reach GitHub");
    let gh = StubGh::answering(vec![Ok("not json".to_string())]);
    assert!(fetch(&gh, &repo(), 1).unwrap_err().starts_with("GitHub's answer didn't read"));
}

#[test]
fn a_reply_goes_straight_to_its_thread() {
    let gh = StubGh::default();
    // A body that `-F` would read as a file.
    let change = PrWrite::Reply { thread: "T1".into(), body: "@/etc/passwd\nline two".into() };
    assert_eq!(write(&gh, &repo(), 7, &change).unwrap(), "Replied on GitHub");
    let calls = gh.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 'w');
    assert!(query_of(&calls[0].1).contains("addPullRequestReviewThreadReply"));
    assert!(has(&calls[0].1, "-f", "thread=T1"));
    assert!(has(&calls[0].1, "-f", "body=@/etc/passwd\nline two"), "raw, through -f");
}

#[test]
fn resolving_and_reopening_are_different_mutations() {
    let gh = StubGh::default();
    let resolve = PrWrite::Resolve { thread: "T1".into(), resolved: true };
    assert_eq!(write(&gh, &repo(), 7, &resolve).unwrap(), "Resolved the thread on GitHub");
    let reopen = PrWrite::Resolve { thread: "T1".into(), resolved: false };
    assert_eq!(write(&gh, &repo(), 7, &reopen).unwrap(), "Reopened the thread on GitHub");
    let calls = gh.calls();
    assert!(query_of(&calls[0].1).contains("resolveReviewThread(") && !query_of(&calls[0].1).contains("unresolve"));
    assert!(query_of(&calls[1].1).contains("unresolveReviewThread("));
}

fn anchor(start: u32, end: u32, side: Side) -> Anchor {
    Anchor { path: "a.rs".into(), side, start, end, code: Vec::new() }
}

#[test]
fn the_first_github_comment_starts_a_pending_review() {
    let started = json!({"data": {"addPullRequestReview": {"pullRequestReview": {"id": "R9"}}}}).to_string();
    let gh = StubGh::answering(vec![Ok(pending_answer(None)), Ok(started), Ok("{}".to_string())]);
    let change = PrWrite::AddThread { anchor: anchor(3, 3, Side::New), body: "look".into() };
    assert!(write(&gh, &repo(), 7, &change).unwrap().contains("pending review"));
    let calls = gh.calls();
    assert_eq!(calls.iter().map(|c| c.0).collect::<String>(), "rww");
    assert!(has(&calls[0].1, "-F", "number=7"));
    assert!(query_of(&calls[1].1).contains("addPullRequestReview(") && has(&calls[1].1, "-f", "pull=PR1"));
    assert!(!query_of(&calls[1].1).contains("event"), "no event is what leaves it pending");
    let add = &calls[2].1;
    assert!(query_of(add).contains("addPullRequestReviewThread("));
    assert!(has(add, "-f", "review=R9") && has(add, "-f", "path=a.rs") && has(add, "-f", "side=RIGHT"));
    assert!(has(add, "-F", "line=3") && has(add, "-f", "body=look"));
    assert!(!vars(add).iter().any(|(_, v)| v.starts_with("startLine")), "one line has no start");
}

#[test]
fn a_later_comment_joins_the_pending_review_and_a_range_names_its_start() {
    let gh = StubGh::answering(vec![Ok(pending_answer(Some("R1"))), Ok("{}".to_string())]);
    let change = PrWrite::AddThread { anchor: anchor(2, 5, Side::Old), body: "look".into() };
    write(&gh, &repo(), 7, &change).unwrap();
    let calls = gh.calls();
    assert_eq!(calls.iter().map(|c| c.0).collect::<String>(), "rw", "no second review");
    let add = &calls[1].1;
    assert!(has(add, "-f", "review=R1") && has(add, "-f", "side=LEFT") && has(add, "-f", "startSide=LEFT"));
    assert!(has(add, "-F", "line=5") && has(add, "-F", "startLine=2"));
}

#[test]
fn submitting_finishes_the_pending_review_or_makes_a_new_one() {
    let gh = StubGh::answering(vec![Ok(pending_answer(Some("R1"))), Ok("{}".to_string())]);
    let change = PrWrite::SubmitReview { event: ReviewEvent::Approve, body: String::new() };
    assert_eq!(write(&gh, &repo(), 7, &change).unwrap(), "Review submitted: Approve");
    let calls = gh.calls();
    assert!(query_of(&calls[1].1).contains("submitPullRequestReview("));
    assert!(has(&calls[1].1, "-f", "review=R1") && has(&calls[1].1, "-f", "event=APPROVE"));

    let gh = StubGh::answering(vec![Ok(pending_answer(None)), Ok("{}".to_string())]);
    let change = PrWrite::SubmitReview { event: ReviewEvent::RequestChanges, body: "no".into() };
    write(&gh, &repo(), 7, &change).unwrap();
    let calls = gh.calls();
    assert!(query_of(&calls[1].1).contains("addPullRequestReview("));
    assert!(has(&calls[1].1, "-f", "pull=PR1") && has(&calls[1].1, "-f", "event=REQUEST_CHANGES") && has(&calls[1].1, "-f", "body=no"));
}

#[test]
fn a_refused_write_says_why() {
    let gh = StubGh::answering(vec![Err("Can not approve your own pull request".to_string())]);
    let change = PrWrite::Reply { thread: "T1".into(), body: "b".into() };
    assert_eq!(write(&gh, &repo(), 7, &change).unwrap_err(), "Can not approve your own pull request");
}

#[test]
fn a_repository_reads_from_any_url_github_uses() {
    let want = Some(repo());
    assert_eq!(Repo::parse("https://github.com/jlong/shelbi/pull/2155"), want);
    assert_eq!(Repo::parse("git@github.com:jlong/shelbi.git\n"), want);
    assert_eq!(Repo::parse("ssh://git@github.com/jlong/shelbi.git"), want);
    assert_eq!(Repo::parse("https://github.com/jlong/shelbi"), want);
    assert_eq!(Repo::parse("nothing"), None);
    assert_eq!(Repo::parse("https://github.com/jlong"), None);
}

#[test]
fn a_review_title_marks_the_verdict_and_tab_cycles_them() {
    assert_eq!(review_title(ReviewEvent::Approve), "Submit review: Comment [Approve] Request changes");
    assert_eq!(ReviewEvent::Comment.next(), ReviewEvent::Approve);
    assert_eq!(ReviewEvent::RequestChanges.next(), ReviewEvent::Comment);
    assert_eq!(ReviewEvent::Comment.prev(), ReviewEvent::RequestChanges);
}
