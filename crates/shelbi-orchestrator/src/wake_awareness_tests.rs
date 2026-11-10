//! The awareness system (v2, `orchestrator.awareness: awareness`) on the Codex
//! bridge: heartbeat lines are the board digest (context, never a turn), there
//! is no keep-alive, and a digest-only batch is injected into an idle thread
//! or carried by the next wake. The v1 tiers are pinned by the wake module's
//! own tests.

use super::*;

const PROJECT: &str = "demo";
const V2: AwarenessMode = AwarenessMode::Awareness;
const V1: AwarenessMode = AwarenessMode::Heartbeat;

const QUIET_DIGEST: &str = "t project=demo heartbeat zen=on zen_eligible=0 idle_workspaces=9\n";
const CAPACITY_DIGEST: &str = "t project=demo heartbeat zen=on zen_eligible=2 idle_workspaces=1\n";
const READY: &str = "t project=demo task=b backlog -> ready to_category=ready\n";
const REMINDER: &str = "t project=demo reminder id=r-1 text=\"check the tap PR\"\n";
const WORKING: &str = "t project=demo workspace=alpha idle -> working\n";

fn scan(mode: AwarenessMode, start: u64, text: &str, board_in_flight: bool) -> QueuedBatch {
    scan_text_batch_in(mode, PROJECT, start, text, board_in_flight).expect("whole lines scan")
}

fn queue(batches: Vec<QueuedBatch>) -> DurableQueue {
    DurableQueue {
        project: PROJECT.into(),
        batches: batches.into(),
    }
}

#[test]
fn v2_heartbeat_lines_are_digests_whatever_the_board_holds() {
    for (line, in_flight) in [
        (QUIET_DIGEST, false),
        (QUIET_DIGEST, true),
        (CAPACITY_DIGEST, false),
    ] {
        let parsed = ParsedLine::parse(line.trim_end());
        assert_eq!(
            line_priority_in(V2, &parsed, in_flight),
            Some(WakePriority::Digest),
            "{line}"
        );
        // v1 is untouched: the same lines keep their keep-alive / heartbeat tiers.
        assert_eq!(
            line_priority_in(V1, &parsed, in_flight),
            line_priority(&parsed, in_flight)
        );
    }
    assert!(WakePriority::Digest.is_digest());
    assert!(!WakePriority::Digest.is_keep_alive());
    assert!(WakePriority::Digest < WakePriority::HeartbeatKeepAlive);

    // Everything else keeps its v1 tier: reminders stay urgent, transitions wake.
    let reminder = ParsedLine::parse(REMINDER.trim_end());
    assert_eq!(line_priority_in(V2, &reminder, false), Some(WakePriority::Awareness));
    let ready = ParsedLine::parse(READY.trim_end());
    assert_eq!(line_priority_in(V2, &ready, false), Some(WakePriority::Ready));
    let working = ParsedLine::parse(WORKING.trim_end());
    assert_eq!(line_priority_in(V2, &working, false), None);
}

#[test]
fn v2_digest_only_batches_never_start_a_turn() {
    // A quiet board on v2 writes nothing at all; a stray digest (or a capacity
    // digest a v1 heartbeat would have woken for) plus quiet lines is context.
    let text = format!("{WORKING}{CAPACITY_DIGEST}{QUIET_DIGEST}");
    let batch = scan(V2, 0, &text, true);
    assert!(batch.digest);
    assert!(!batch.actionable);
    assert!(!batch.keep_alive, "v2 has no keep-alive");
    assert!(batch.is_digest_only());
    assert_eq!(batch.events[1].kind, "heartbeat");

    let mut digests = queue(vec![batch]);
    assert_eq!(digests.next_pending(), None, "a digest never starts a turn");
    assert_eq!(digests.next_injectable(), Some(0), "an idle thread gets it as context");
    assert!(digests.pending_delivery_ids(0).is_empty());

    // On v1 the very same lines wake Codex.
    let v1 = scan(V1, 0, &text, true);
    assert!(v1.actionable);
    assert_eq!(queue(vec![v1]).next_pending(), Some(0));
}

#[test]
fn v2_digest_ahead_of_a_wake_rides_with_it() {
    let digest = scan(V2, 0, QUIET_DIGEST, false);
    let boundary = digest.through;
    let mut queue = queue(vec![digest]);

    // A wake arriving while the digest waits (mid-turn) folds it in: one
    // steered / started batch carrying both, nothing left to inject.
    queue.enqueue(scan(V2, boundary, REMINDER, false));
    assert_eq!(queue.batches.len(), 1);
    let folded = &queue.batches[0];
    assert!(folded.actionable && folded.digest && !folded.is_digest_only());
    assert!(folded.is_urgent());
    assert!(folded.input.contains("[SHELBI_EVENT_BATCH]"));
    assert_eq!(queue.next_pending(), Some(0));
    assert_eq!(queue.next_injectable(), None);
}

#[test]
fn v2_digest_behind_an_unfoldable_wake_waits_for_that_wake() {
    // An attempted digest can't coalesce; the wake behind it still delivers
    // FIFO (it carries the digest's range into the turn), and nothing is
    // injected while a wake is pending.
    let mut digest = scan(V2, 0, QUIET_DIGEST, false);
    digest.attempted = true;
    let boundary = digest.through;
    let mut queue = queue(vec![digest]);
    queue.enqueue(scan(V2, boundary, READY, false));
    assert_eq!(queue.batches.len(), 2);
    assert_eq!(queue.next_pending(), Some(0));
    assert_eq!(queue.next_injectable(), None);
}

#[test]
fn an_injected_digest_neither_blocks_nor_retries_until_a_new_thread() {
    let mut digest = scan(V2, 0, QUIET_DIGEST, false);
    digest.attempted = true;
    digest.status = DeliveryStatus::Injected {
        thread_id: "thread-1".into(),
    };
    let boundary = digest.through;
    let mut queue = queue(vec![digest]);
    assert_eq!(queue.next_pending(), None);
    assert_eq!(queue.next_injectable(), None, "injected once per thread");

    // The next wake is delivered past it; its drain consumes both ranges.
    queue.enqueue(scan(V2, boundary, READY, false));
    assert_eq!(queue.batches.len(), 2, "never folded into an injected batch");
    assert_eq!(queue.next_pending(), Some(1));

    // Idle on the same thread with the cursor behind it: still injected (a
    // delivered batch would go back to pending here).
    queue.reconcile_thread("thread-1", &HashSet::new(), &ThreadPhase::Idle, 0);
    assert!(matches!(queue.batches[0].status, DeliveryStatus::Injected { .. }));

    // A replaced thread never saw it: inject again.
    queue.batches.pop_back();
    queue.reconcile_thread("thread-2", &HashSet::new(), &ThreadPhase::Idle, 0);
    assert_eq!(queue.batches[0].status, DeliveryStatus::Pending);
    assert_eq!(queue.next_injectable(), Some(0));
}

#[test]
fn switching_to_v2_turns_a_pending_keep_alive_into_a_digest() {
    let keep_alive = scan(V1, 0, QUIET_DIGEST, false);
    assert!(keep_alive.keep_alive);
    let mut v1 = queue(vec![keep_alive.clone()]);
    v1.adopt_mode(V1);
    assert_eq!(v1.next_pending(), Some(0), "v1 keeps its keep-alive");

    let mut v2 = queue(vec![keep_alive]);
    v2.adopt_mode(V2);
    assert!(!v2.batches[0].keep_alive && v2.batches[0].digest);
    assert_eq!(v2.next_pending(), None);
    assert_eq!(v2.next_injectable(), Some(0));
}

#[test]
fn inject_calls_append_a_developer_message_without_a_turn() {
    let batch = scan(V2, 7, QUIET_DIGEST, false);
    let call = DeliveryCall::inject_digest("thread-1", PROJECT, &batch);
    assert_eq!(call.method, "thread/inject_items");
    assert_eq!(call.params["threadId"], "thread-1");
    let item = &call.params["items"][0];
    assert_eq!(item["type"], "message");
    assert_eq!(item["role"], "developer");
    let text = item["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("[SHELBI_BOARD_DIGEST]{"), "{text}");
    assert!(text.contains("idle_workspaces=9"));
    assert!(text.contains(&batch.message_id));
    assert!(!text.contains("Run `shelbi orchestrator events drain` now"));

    // The urgent retry is a plain start with the same idempotency key.
    let urgent = scan(V2, 7, REMINDER, false);
    let start = DeliveryCall::start("thread-1", &urgent);
    assert_eq!(start.method, "turn/start");
    assert_eq!(start.params["clientUserMessageId"], urgent.message_id);
    assert!(start.params.get("expectedTurnId").is_none());
}

#[test]
fn only_plain_steer_rejections_count_as_a_race() {
    assert!(steer_may_have_raced(-32600, "no active turn", None));
    assert!(steer_may_have_raced(-32600, "expected turn id is stale", None));
    assert!(!steer_may_have_raced(-32001, "server overloaded", None));
    assert!(!steer_may_have_raced(-32601, "method not found", None));
    let review = json!({"codexErrorInfo": {"activeTurnNotSteerable": {"turnKind": "review"}}});
    assert!(!steer_may_have_raced(-32600, "not steerable", Some(&review)));
}

#[test]
fn urgency_is_the_three_awareness_kinds() {
    for line in [
        REMINDER,
        "t project=demo job id=j-1 kind=command label=\"tests\" status=ok exit=0\n",
        "t project=demo task=t-1 overdue estimate=10m elapsed=20m workspace=alpha\n",
    ] {
        assert!(scan(V2, 0, line, false).is_urgent(), "{line}");
    }
    assert!(!scan(V2, 0, READY, false).is_urgent());
    assert!(!scan(V2, 0, QUIET_DIGEST, false).is_urgent());
}

#[test]
fn zen_policy_is_injected_only_while_zen_is_on() {
    let _lock = crate::test_lock::acquire();
    let temp = tempfile::tempdir().unwrap();
    let previous = std::env::var_os("SHELBI_HOME");
    std::env::set_var("SHELBI_HOME", temp.path());
    fs::create_dir_all(shelbi_state::project_dir(PROJECT).unwrap()).unwrap();

    assert_eq!(zen_policy_text(PROJECT), None, "zen off: nothing to inject");
    shelbi_state::set_zen_mode(PROJECT, shelbi_state::ZenModeState::On, "test").unwrap();
    let bare = zen_policy_text(PROJECT).expect("zen on");
    assert!(bare.starts_with("[SHELBI_ZEN_POLICY] Zen Mode is on for project `demo`."));
    assert!(!bare.contains("Policy summary"), "no zenmode.md yet: {bare}");

    shelbi_state::scaffold_zenmode(PROJECT).unwrap();
    let summary = shelbi_state::read_zenmode_summary(PROJECT).unwrap().unwrap();
    let text = zen_policy_text(PROJECT).unwrap();
    assert!(text.contains(&format!("Policy summary: {summary}")), "{text}");
    assert!(!text.contains('\n'));

    match previous {
        Some(previous) => std::env::set_var("SHELBI_HOME", previous),
        None => std::env::remove_var("SHELBI_HOME"),
    }
}
