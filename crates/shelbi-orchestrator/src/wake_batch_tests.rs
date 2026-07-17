use super::*;

const PROJECT: &str = "demo";

fn scan_chunks(text: &str, start: u64) -> Vec<QueuedBatch> {
    let mut chunks = Vec::new();
    let head = start + text.len() as u64;
    let mut cursor = start;
    while cursor < head {
        let relative = (cursor - start) as usize;
        let batch = scan_text_batch(PROJECT, cursor, &text[relative..], false)
            .expect("complete line must advance the scanner");
        assert!(batch.through > cursor);
        assert_eq!(text.as_bytes()[(batch.through - start) as usize - 1], b'\n');
        cursor = batch.through;
        chunks.push(batch);
    }
    chunks
}

fn assert_bounded(batch: &QueuedBatch) {
    assert!(batch.events.len() <= EVENT_BATCH_MAX_EVENTS);
    assert!(serialized_input_bytes(&batch.input) <= EVENT_BATCH_MAX_SERIALIZED_INPUT_BYTES);
    assert_eq!(
        batch.message_id,
        stable_message_id(PROJECT, batch.from, batch.through)
    );
}

fn normalized_event(index: usize, raw_bytes: usize) -> NormalizedEvent {
    NormalizedEvent {
        cursor: index as u64 + 1,
        offset: index as u64,
        timestamp: Some("t".into()),
        kind: "task_transition".into(),
        raw: format!("project={PROJECT} note={}", "x".repeat(raw_bytes)),
        metadata: BTreeMap::from([
            ("project".into(), PROJECT.into()),
            ("task".into(), format!("task-{index}")),
        ]),
    }
}

#[test]
fn offline_quiet_chunks_wait_then_flush_fifo_when_action_arrives() {
    // A genuinely non-deliverable owned quiet line: pane death is neither
    // actionable nor keep-alive, so it exercises the buffer/FIFO/directional
    // machinery. (Quiet heartbeats are now keep-alive and deliverable on their
    // own; see the keep-alive tests in the wake module.)
    let quiet_line = "t project=demo workspace=alpha pane_alive=false reason=exit:1\n";
    let quiet = quiet_line.repeat(EVENT_BATCH_MAX_EVENTS + 5);
    let quiet_chunks = scan_chunks(&quiet, 0);
    assert!(quiet_chunks.len() >= 2, "quiet catch-up must be split");
    assert!(quiet_chunks
        .iter()
        .all(|batch| !batch.actionable && !batch.keep_alive));

    let mut queue = DurableQueue {
        project: PROJECT.into(),
        batches: VecDeque::new(),
    };
    for chunk in quiet_chunks {
        assert_bounded(&chunk);
        queue.enqueue(chunk);
    }
    assert_eq!(queue.next_pending(), None, "quiet tail must not wake Codex");

    let action = "t project=demo task=x backlog -> todo to_category=ready\n";
    let action_batch = scan_text_batch(PROJECT, quiet.len() as u64, action, false).unwrap();
    assert!(action_batch.actionable);
    queue.enqueue(action_batch);
    assert!(queue.batches.front().is_some_and(|batch| !batch.actionable));
    assert!(queue.batches.back().is_some_and(|batch| batch.actionable));

    let expected = queue
        .batches
        .iter()
        .map(|batch| batch.message_id.clone())
        .collect::<Vec<_>>();
    let mut delivered = Vec::new();
    while let Some(index) = queue.next_pending() {
        delivered.push(queue.batches[index].message_id.clone());
        queue.batches[index].status = DeliveryStatus::Delivered {
            thread_id: "thread-1".into(),
        };
    }
    assert_eq!(delivered, expected, "action must release every chunk FIFO");
    assert!(queue
        .batches
        .iter()
        .all(|batch| { matches!(batch.status, DeliveryStatus::Delivered { .. }) }));

    let trailing_quiet = scan_text_batch(
        PROJECT,
        (quiet.len() + action.len()) as u64,
        quiet_line,
        false,
    )
    .unwrap();
    assert!(!trailing_quiet.actionable);
    queue.enqueue(trailing_quiet);
    assert_eq!(
        queue.next_pending(),
        None,
        "an earlier action must not release a later quiet tail"
    );

    let mut directional = DurableQueue {
        project: PROJECT.into(),
        batches: VecDeque::new(),
    };
    directional.enqueue(scan_text_batch(PROJECT, 0, action, false).unwrap());
    directional.enqueue(scan_text_batch(PROJECT, action.len() as u64, quiet_line, false).unwrap());
    assert_eq!(
        directional.batches.len(),
        2,
        "coalescing must not fold quiet data into an earlier action"
    );
    let action_index = directional.next_pending().unwrap();
    assert_eq!(action_index, 0);
    directional.batches[action_index].status = DeliveryStatus::Delivered {
        thread_id: "thread-1".into(),
    };
    assert_eq!(directional.next_pending(), None);
}

#[test]
fn scanner_enforces_count_and_serialized_byte_bounds_at_line_boundaries() {
    let count_line = "t project=demo heartbeat zen=on zen_eligible=0 idle_workspaces=9\n";
    let count_text = count_line.repeat(EVENT_BATCH_MAX_EVENTS * 2 + 3);
    let count_chunks = scan_chunks(&count_text, 0);
    assert_eq!(count_chunks[0].events.len(), EVENT_BATCH_MAX_EVENTS);
    assert!(count_chunks.len() >= 3);
    for batch in &count_chunks {
        assert_bounded(batch);
    }

    let byte_line = format!(
        "t project=demo heartbeat zen=on zen_eligible=0 idle_workspaces=9 note={}\n",
        "b".repeat(2_000)
    );
    let byte_text = byte_line.repeat(40);
    let byte_chunks = scan_chunks(&byte_text, 17);
    assert!(
        byte_chunks.len() > 1,
        "serialized byte cap must split before the event-count cap"
    );
    assert!(byte_chunks
        .iter()
        .all(|batch| batch.events.len() < EVENT_BATCH_MAX_EVENTS));
    for batch in &byte_chunks {
        assert_bounded(batch);
    }
}

#[test]
fn coalescing_refuses_combined_count_or_byte_overflow() {
    let mut count_left = QueuedBatch::new(
        PROJECT,
        0,
        40,
        (0..40).map(|index| normalized_event(index, 8)).collect(),
    );
    let count_right = QueuedBatch::new(
        PROJECT,
        40,
        70,
        (40..70).map(|index| normalized_event(index, 8)).collect(),
    );
    let count_id = count_left.message_id.clone();
    assert!(!count_left.try_coalesce(PROJECT, count_right));
    assert_eq!(count_left.message_id, count_id);
    assert_eq!(count_left.through, 40);

    let mut byte_left = QueuedBatch::new(PROJECT, 0, 1, vec![normalized_event(0, 18_000)]);
    let byte_right = QueuedBatch::new(PROJECT, 1, 2, vec![normalized_event(1, 18_000)]);
    assert_bounded(&byte_left);
    assert_bounded(&byte_right);
    let byte_id = byte_left.message_id.clone();
    assert!(!byte_left.try_coalesce(PROJECT, byte_right));
    assert_eq!(byte_left.message_id, byte_id);
    assert_eq!(byte_left.through, 1);

    let mut small_left = QueuedBatch::new(PROJECT, 0, 1, vec![normalized_event(0, 8)]);
    let small_right = QueuedBatch::new(PROJECT, 1, 2, vec![normalized_event(1, 8)]);
    assert!(small_left.try_coalesce(PROJECT, small_right));
    assert_bounded(&small_left);
}

#[test]
fn oversized_single_line_advances_with_bounded_authoritative_marker() {
    let payload = "DO-NOT-COPY-TO-NATIVE-BATCH".repeat(EVENT_BATCH_MAX_SERIALIZED_INPUT_BYTES / 8);
    let line =
        format!("t project=demo task=huge backlog -> todo to_category=ready reason={payload}");
    let text = format!("{line}\n");
    let batch = scan_text_batch(PROJECT, 0, &text, false).unwrap();

    assert_eq!(batch.from, 0);
    assert_eq!(batch.through, text.len() as u64);
    assert_eq!(batch.events.len(), 1);
    assert!(batch.actionable);
    assert_bounded(&batch);
    let marker = &batch.events[0];
    assert_eq!(marker.kind, "oversized_event");
    assert_eq!(marker.offset, 0);
    assert_eq!(marker.cursor, text.len() as u64);
    assert_eq!(marker.metadata["oversized"], "true");
    assert_eq!(marker.metadata["original_bytes"], line.len().to_string());
    assert!(!marker.raw.contains("DO-NOT-COPY"));
    assert!(!batch.input.contains("DO-NOT-COPY"));
    assert_eq!(
        &text[marker.offset as usize..marker.cursor as usize],
        text,
        "marker cursor must retain the exact authoritative log line"
    );
}

#[test]
fn persisted_over_limit_legacy_batch_is_rebuilt_from_the_durable_cursor() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("queue.json");
    let escaped_input = "\"".repeat(EVENT_BATCH_MAX_SERIALIZED_INPUT_BYTES);
    assert_eq!(escaped_input.len(), EVENT_BATCH_MAX_SERIALIZED_INPUT_BYTES);
    assert!(serialized_input_bytes(&escaped_input) > EVENT_BATCH_MAX_SERIALIZED_INPUT_BYTES);
    let legacy = QueuedBatch {
        from: 0,
        through: 1,
        message_id: stable_message_id(PROJECT, 0, 1),
        events: Vec::new(),
        input: escaped_input,
        actionable: true,
        keep_alive: false,
        attempted: false,
        status: DeliveryStatus::Pending,
    };
    save_json_atomic(
        &path,
        &QueueFile {
            version: STATE_VERSION,
            project: PROJECT.into(),
            batches: VecDeque::from([legacy]),
        },
    )
    .unwrap();

    let loaded = DurableQueue::load(&path, PROJECT).unwrap();
    assert!(
        loaded.batches.is_empty(),
        "over-limit persisted input must be rescanned from the durable cursor"
    );
}
