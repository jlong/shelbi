use super::*;

use std::ffi::OsString;

const PROJECT: &str = "demo";
const THREAD_ID: &str = "thread-1";

struct HomeGuard {
    previous: Option<OsString>,
}

impl HomeGuard {
    fn install(root: &Path) -> Self {
        let previous = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", root);
        fs::create_dir_all(shelbi_state::project_dir(PROJECT).unwrap()).unwrap();
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => std::env::set_var("SHELBI_HOME", previous),
            None => std::env::remove_var("SHELBI_HOME"),
        }
    }
}

fn test_project() -> Project {
    Project {
        name: PROJECT.into(),
        default_branch: "main".into(),
        default_workflow: None,
        config_mode: None,
        orchestrator: shelbi_core::OrchestratorSpec {
            runner: "codex".into(),
        },
        agent_runners: BTreeMap::new(),
        github_url: None,
        workspace_poll_interval_secs: 5,
        workspace_permissions_mode: "auto".into(),
        workspace_settings_template: None,
        zen: shelbi_core::ZenConfig::default(),
        heartbeat: shelbi_core::HeartbeatConfig::default(),
        git: shelbi_core::GitConfig::default(),
        repo: "/tmp/demo".into(),
        machines: Vec::new(),
        editor: None,
        workspaces: Vec::new(),
        detected_shapes: Vec::new(),
    }
}

fn queued_batch(from: u64, through: u64) -> QueuedBatch {
    QueuedBatch::new(
        PROJECT,
        from,
        through,
        vec![NormalizedEvent {
            cursor: through,
            offset: from,
            timestamp: Some("t".into()),
            kind: "task_transition".into(),
            raw: format!("t project={PROJECT} task=x a -> b to_category=ready"),
            metadata: BTreeMap::from([
                ("project".into(), PROJECT.into()),
                ("task".into(), "x".into()),
            ]),
        }],
    )
}

fn sleeping_child() -> Child {
    Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn test_bridge(
    root: &Path,
    socket_path: PathBuf,
    rpc: CodexRpcClient,
    phase: ThreadPhase,
    queue: DurableQueue,
    bootstrap_sent: bool,
) -> NativeBridge {
    let queue_path = root.join("event-queue.json");
    queue.save(&queue_path).unwrap();
    let (_event_tx, events) = mpsc::channel();
    let mut runtime = ThreadRuntime::default();
    runtime.set_phase(phase);
    NativeBridge {
        project: test_project(),
        workdir: root.to_path_buf(),
        socket_path: socket_path.clone(),
        server: AppServerProcess {
            child: sleeping_child(),
            socket_path,
        },
        tui_relay: TuiRelay {
            socket_path: root.join("unused-relay.sock"),
            events,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        },
        tui: sleeping_child(),
        rpc: Some(rpc),
        thread_id: THREAD_ID.into(),
        developer_instructions: "owned developer instructions".into(),
        runtime,
        queue,
        queue_path,
        tui_ready: true,
        tui_ready_deadline: Instant::now() + TUI_READY_TIMEOUT,
        protocol_unsupported: false,
        bootstrap_sent,
        bootstrap_prompt: crate::ORCH_BOOTSTRAP_PROMPT.into(),
        bootstrap_blocked_generation: None,
        bootstrap_retry_not_before: Instant::now(),
        bootstrap_message_id: format!("shelbi-bootstrap/{THREAD_ID}/1"),
        blocked_generation: None,
        awaiting_nonsteerable_completion: None,
        retry_not_before: Instant::now(),
        next_scan: Instant::now() + Duration::from_secs(60),
        next_reconnect: Instant::now(),
    }
}

fn read_json(socket: &mut WebSocket<UnixStream>) -> Value {
    let Message::Text(text) = socket.read().unwrap() else {
        panic!("expected JSON text frame");
    };
    serde_json::from_str(text.as_str()).unwrap()
}

fn send_json(socket: &mut WebSocket<UnixStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .unwrap();
}

fn accept_initialized(listener: &UnixListener) -> WebSocket<UnixStream> {
    let (stream, _) = listener.accept().unwrap();
    let mut socket = accept(stream).unwrap();
    let initialize = read_json(&mut socket);
    assert_eq!(initialize["method"], "initialize");
    send_json(
        &mut socket,
        json!({"id": initialize["id"], "result": {"userAgent": "wake-rejection-test"}}),
    );
    assert_eq!(read_json(&mut socket), json!({"method": "initialized"}));
    socket
}

fn send_idle_resume(socket: &mut WebSocket<UnixStream>, expected_method: &str) {
    let resume = read_json(socket);
    assert_eq!(resume["method"], expected_method);
    assert_eq!(resume["params"]["threadId"], THREAD_ID);
    assert_eq!(
        resume["params"]["developerInstructions"],
        "owned developer instructions"
    );
    send_json(
        socket,
        json!({
            "id": resume["id"],
            "result": {
                "thread": {
                    "id": THREAD_ID,
                    "status": {"type": "idle"},
                    "turns": []
                }
            }
        }),
    );
}

#[test]
fn native_first_launch_sends_contextual_prompt_with_stable_message_id() {
    let _lock = crate::test_lock::acquire();
    let temp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install(temp.path());
    let socket_path = temp.path().join("app-server.sock");
    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("skipping: sandbox does not permit Unix sockets");
            return;
        }
        Err(error) => panic!("bind test socket: {error}"),
    };
    let (release_tx, release_rx) = mpsc::channel();

    let server = thread::spawn(move || {
        let mut socket = accept_initialized(&listener);
        let start = read_json(&mut socket);
        assert_eq!(start["method"], "turn/start");
        assert_eq!(
            start["params"]["clientUserMessageId"],
            "shelbi-first-launch/demo/thread-1"
        );
        let prompt = start["params"]["input"][0]["text"].as_str().unwrap();
        assert!(prompt.contains("[SHELBI_FIRST_PROJECT_GREETING]"));
        assert!(prompt.contains("Welcome to demo"));
        assert!(prompt.contains("write it up as a task and dispatch it"));
        send_json(
            &mut socket,
            json!({"id": start["id"], "result": {"turn": {"id": "first-opening"}}}),
        );
        release_rx
            .recv_timeout(RPC_TIMEOUT)
            .expect("client should release the fixture socket");
    });

    let rpc = CodexRpcClient::connect(
        &socket_path,
        "wake-rejection-test",
        env!("CARGO_PKG_VERSION"),
        RPC_TIMEOUT,
    )
    .unwrap();
    let mut bridge = test_bridge(
        temp.path(),
        socket_path,
        rpc,
        ThreadPhase::Idle,
        DurableQueue {
            project: PROJECT.into(),
            batches: VecDeque::new(),
        },
        false,
    );
    bridge.bootstrap_prompt = crate::orchestrator_bootstrap_prompt(
        PROJECT,
        Path::new("/tmp/demo"),
        true,
    );
    bridge.bootstrap_message_id = bootstrap_message_id(PROJECT, THREAD_ID, 7, true);

    bridge.maybe_send_bootstrap().unwrap();
    release_tx.send(()).unwrap();
    server.join().unwrap();
    assert!(
        bridge.bootstrap_sent,
        "bootstrap was not sent: rpc_connected={} phase={:?} generation={} blocked={:?} \
         awaiting_nonsteerable={:?}",
        bridge.rpc.is_some(),
        bridge.runtime.phase,
        bridge.runtime.generation,
        bridge.bootstrap_blocked_generation,
        bridge.awaiting_nonsteerable_completion,
    );
    assert_eq!(
        bridge.runtime.phase,
        ThreadPhase::Active("first-opening".into())
    );
}

#[test]
fn bootstrap_rejection_without_notification_rehydrates_and_retries_exact_thread() {
    let _lock = crate::test_lock::acquire();
    let temp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install(temp.path());
    let socket_path = temp.path().join("app-server.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let (release_tx, release_rx) = mpsc::channel();

    let server = thread::spawn(move || {
        let mut first = accept_initialized(&listener);
        let start = read_json(&mut first);
        assert_eq!(start["method"], "turn/start");
        assert_eq!(start["params"]["threadId"], THREAD_ID);
        assert_eq!(
            start["params"]["input"][0]["text"],
            crate::ORCH_BOOTSTRAP_PROMPT
        );
        assert!(!start["params"]["input"][0]["text"]
            .as_str()
            .unwrap()
            .contains("SHELBI_FIRST_PROJECT_GREETING"));
        send_json(
            &mut first,
            json!({
                "id": start["id"],
                "error": {"code": -32600, "message": "thread is busy"}
            }),
        );

        let mut second = accept_initialized(&listener);
        send_idle_resume(&mut second, "thread/resume");
        let retry = read_json(&mut second);
        assert_eq!(retry["method"], "turn/start");
        assert_eq!(retry["params"]["threadId"], THREAD_ID);
        assert_eq!(
            retry["params"]["clientUserMessageId"],
            format!("shelbi-bootstrap/{THREAD_ID}/1")
        );
        send_json(
            &mut second,
            json!({"id": retry["id"], "result": {"turn": {"id": "bootstrap-retry"}}}),
        );
        let _ = release_rx.recv();
    });

    let rpc = CodexRpcClient::connect(
        &socket_path,
        "wake-rejection-test",
        env!("CARGO_PKG_VERSION"),
        RPC_TIMEOUT,
    )
    .unwrap();
    let mut bridge = test_bridge(
        temp.path(),
        socket_path,
        rpc,
        ThreadPhase::Idle,
        DurableQueue {
            project: PROJECT.into(),
            batches: VecDeque::new(),
        },
        false,
    );

    bridge.maybe_send_bootstrap().unwrap();
    assert!(bridge.rpc.is_none());
    assert!(!bridge.bootstrap_sent);
    assert_eq!(
        bridge.bootstrap_blocked_generation,
        Some(bridge.runtime.generation)
    );

    bridge.next_reconnect = Instant::now();
    bridge.maybe_reconnect();
    assert!(bridge.rpc.is_some());
    assert_eq!(bridge.runtime.phase, ThreadPhase::Idle);
    assert_eq!(bridge.bootstrap_blocked_generation, None);

    bridge.maybe_send_bootstrap().unwrap();
    assert!(bridge.bootstrap_sent);
    assert_eq!(
        bridge.runtime.phase,
        ThreadPhase::Active("bootstrap-retry".into())
    );
    release_tx.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn stale_steer_rejection_without_notification_rehydrates_idle_and_flushes_oldest() {
    let _lock = crate::test_lock::acquire();
    let temp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install(temp.path());
    let socket_path = temp.path().join("app-server.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let oldest = queued_batch(4, 42);
    let oldest_id = oldest.message_id.clone();
    let newer = queued_batch(42, 84);
    let (release_tx, release_rx) = mpsc::channel();

    let expected_oldest_id = oldest_id.clone();
    let server = thread::spawn(move || {
        let mut first = accept_initialized(&listener);
        let steer = read_json(&mut first);
        assert_eq!(steer["method"], "turn/steer");
        assert_eq!(steer["params"]["threadId"], THREAD_ID);
        assert_eq!(steer["params"]["expectedTurnId"], "stale-turn");
        assert_eq!(steer["params"]["clientUserMessageId"], expected_oldest_id);
        send_json(
            &mut first,
            json!({
                "id": steer["id"],
                "error": {"code": -32600, "message": "expected turn id is stale"}
            }),
        );

        let mut second = accept_initialized(&listener);
        send_idle_resume(&mut second, "thread/resume");
        let retry = read_json(&mut second);
        assert_eq!(retry["method"], "turn/start");
        assert_eq!(retry["params"]["threadId"], THREAD_ID);
        assert!(retry["params"].get("expectedTurnId").is_none());
        assert_eq!(retry["params"]["clientUserMessageId"], expected_oldest_id);
        send_json(
            &mut second,
            json!({"id": retry["id"], "result": {"turn": {"id": "event-retry"}}}),
        );
        let _ = release_rx.recv();
    });

    let rpc = CodexRpcClient::connect(
        &socket_path,
        "wake-rejection-test",
        env!("CARGO_PKG_VERSION"),
        RPC_TIMEOUT,
    )
    .unwrap();
    let mut bridge = test_bridge(
        temp.path(),
        socket_path,
        rpc,
        ThreadPhase::Active("stale-turn".into()),
        DurableQueue {
            project: PROJECT.into(),
            batches: VecDeque::from([oldest, newer]),
        },
        true,
    );

    bridge.maybe_deliver_event().unwrap();
    assert!(bridge.rpc.is_none());
    assert_eq!(bridge.queue.next_pending(), Some(0));
    assert_eq!(bridge.blocked_generation, Some(bridge.runtime.generation));

    bridge.next_reconnect = Instant::now();
    bridge.maybe_reconnect();
    assert!(bridge.rpc.is_some());
    assert_eq!(bridge.runtime.phase, ThreadPhase::Idle);
    assert_eq!(bridge.blocked_generation, None);

    bridge.maybe_deliver_event().unwrap();
    assert_eq!(
        bridge.queue.batches[0].status,
        DeliveryStatus::Delivered {
            thread_id: THREAD_ID.into()
        }
    );
    assert_eq!(bridge.queue.batches[0].message_id, oldest_id);
    assert_eq!(bridge.queue.batches[1].status, DeliveryStatus::Pending);
    assert_eq!(
        bridge.runtime.phase,
        ThreadPhase::Active("event-retry".into())
    );
    release_tx.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn nonsteerable_rejection_ignores_stale_completion_then_flushes_fifo() {
    let _lock = crate::test_lock::acquire();
    let temp = tempfile::tempdir().unwrap();
    let _home = HomeGuard::install(temp.path());
    let socket_path = temp.path().join("app-server.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let oldest = queued_batch(4, 42);
    let oldest_id = oldest.message_id.clone();
    let newer = queued_batch(42, 84);
    let newer_id = newer.message_id.clone();
    let (no_retry_tx, no_retry_rx) = mpsc::channel();
    let (first_completion_tx, first_completion_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let expected_oldest_id = oldest_id.clone();
    let expected_newer_id = newer_id.clone();
    let server = thread::spawn(move || {
        let mut first = accept_initialized(&listener);
        let start = read_json(&mut first);
        assert_eq!(start["method"], "turn/start");
        assert_eq!(
            start["params"]["clientUserMessageId"],
            expected_oldest_id
        );
        // A completion for the turn that lost the idle race can already be
        // queued ahead of the review turn's start. It must not clear a gate
        // whose rejected turn id is not known yet.
        send_json(
            &mut first,
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": THREAD_ID,
                    "turn": {"id": "preceding-turn", "status": "completed", "items": []}
                }
            }),
        );
        // This queued start must bind the authoritative review id to the gate,
        // not advance a generic generation gate and authorize another request.
        send_json(
            &mut first,
            json!({
                "method": "turn/started",
                "params": {
                    "threadId": THREAD_ID,
                    "turn": {"id": "review-turn", "status": "inProgress", "items": []}
                }
            }),
        );
        send_json(
            &mut first,
            json!({
                "id": start["id"],
                "error": {
                    "code": -32602,
                    "message": "active turn cannot accept steering",
                    "data": {
                        "codexErrorInfo": {
                            "activeTurnNotSteerable": {"turnKind": "review"}
                        }
                    }
                }
            }),
        );

        let mut second = accept_initialized(&listener);
        let resume = read_json(&mut second);
        assert_eq!(resume["method"], "thread/resume");
        send_json(
            &mut second,
            json!({
                "id": resume["id"],
                "result": {
                    "thread": {
                        "id": THREAD_ID,
                        "status": {"type": "active"},
                        "turns": [{
                            "id": "review-turn",
                            "status": "inProgress",
                            "items": []
                        }]
                    }
                }
            }),
        );

        second
            .get_mut()
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        match second.read() {
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            other => panic!("non-steerable turn was retried before completion: {other:?}"),
        }
        second
            .get_mut()
            .set_read_timeout(Some(RPC_TIMEOUT))
            .unwrap();
        send_json(
            &mut second,
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": THREAD_ID,
                    "turn": {"id": "review-turn", "status": "completed", "items": []}
                }
            }),
        );
        no_retry_tx.send(()).unwrap();

        let oldest_start = read_json(&mut second);
        assert_eq!(oldest_start["method"], "turn/start");
        assert_eq!(
            oldest_start["params"]["clientUserMessageId"],
            expected_oldest_id
        );
        send_json(
            &mut second,
            json!({"id": oldest_start["id"], "result": {"turn": {"id": "oldest-turn"}}}),
        );
        send_json(
            &mut second,
            json!({
                "method": "turn/completed",
                "params": {
                    "threadId": THREAD_ID,
                    "turn": {"id": "oldest-turn", "status": "completed", "items": []}
                }
            }),
        );
        first_completion_tx.send(()).unwrap();

        let newer_start = read_json(&mut second);
        assert_eq!(newer_start["method"], "turn/start");
        assert_eq!(
            newer_start["params"]["clientUserMessageId"],
            expected_newer_id
        );
        send_json(
            &mut second,
            json!({"id": newer_start["id"], "result": {"turn": {"id": "newer-turn"}}}),
        );
        let _ = release_rx.recv();
    });

    let rpc = CodexRpcClient::connect(
        &socket_path,
        "wake-rejection-test",
        env!("CARGO_PKG_VERSION"),
        RPC_TIMEOUT,
    )
    .unwrap();
    let mut bridge = test_bridge(
        temp.path(),
        socket_path,
        rpc,
        ThreadPhase::Idle,
        DurableQueue {
            project: PROJECT.into(),
            batches: VecDeque::from([oldest, newer]),
        },
        true,
    );

    bridge.maybe_deliver_event().unwrap();
    assert!(bridge.rpc.is_none());
    assert_eq!(
        bridge.awaiting_nonsteerable_completion,
        Some(AwaitingNonSteerableCompletion {
            turn_id: Some("review-turn".into()),
            kind: NonSteerableTurnKind::Review,
        })
    );
    assert!(!bridge.protocol_unsupported);

    bridge.next_reconnect = Instant::now();
    bridge.maybe_reconnect();
    assert!(bridge.rpc.is_some());
    assert_eq!(
        bridge.runtime.phase,
        ThreadPhase::Active("review-turn".into())
    );
    bridge.maybe_deliver_event().unwrap();
    no_retry_rx.recv().unwrap();

    bridge.poll_notifications();
    assert_eq!(bridge.awaiting_nonsteerable_completion, None);
    assert_eq!(bridge.runtime.phase, ThreadPhase::Idle);
    bridge.maybe_deliver_event().unwrap();
    assert_eq!(
        bridge.queue.batches[0].status,
        DeliveryStatus::Delivered {
            thread_id: THREAD_ID.into()
        }
    );
    assert_eq!(bridge.queue.batches[1].status, DeliveryStatus::Pending);

    first_completion_rx.recv().unwrap();
    bridge.poll_notifications();
    bridge.maybe_deliver_event().unwrap();
    assert_eq!(
        bridge.queue.batches[1].status,
        DeliveryStatus::Delivered {
            thread_id: THREAD_ID.into()
        }
    );
    assert_eq!(bridge.queue.batches[0].message_id, oldest_id);
    assert_eq!(bridge.queue.batches[1].message_id, newer_id);
    release_tx.send(()).unwrap();
    server.join().unwrap();
}
