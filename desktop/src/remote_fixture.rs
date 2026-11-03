//! A fake far host for the remote window's tests (remote-window P6): a
//! fixture project in a temp Shelbi home, read by the far side's real query
//! handler (`shelbi_orchestrator::source::QueryAnswers`) across an in-process
//! relay (`serve_relay_with` over a socket pair, no `ssh`), plus a fake far
//! daemon on the relay's `control.sock` and a fake far hub on its `hub.sock`.
//!
//! The far daemon answers the hello with this build's version, takes
//! subscriptions, and applies an issue move to the far board index as the
//! real daemon would (the store write, then a `Changed` note to every
//! subscriber), recording each mutation it saw. A project-state write it
//! makes with the real daemon's own function
//! (`shelbi_orchestrator::state_write::apply`) and records too. Nothing reaches the real
//! `~/.shelbi`, the real hub or a network; both "sides" share the temp home,
//! as one process.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shelbi_app::launch::link::HostLink;
use shelbi_app::launch::remote::RemoteSession;
use shelbi_app::launch::{FakeRemote, RemoteConnector, RemoteListing};
use shelbi_app::settings::agents::fixture::Home;
use shelbi_app::source::remote::{ChannelLink, RelayLink};
use shelbi_client::{RelayChannel, RelayServices};
use shelbi_core::{Column, Issue};
use shelbi_proto::control::{
    self, ChangeNote, ClientMsg, MutationError, MutationKind, ServerMsg, StateWrite, CONTROL_PROTOCOL_VERSION,
};
use shelbi_state::IssueFile;

/// The far project.
pub const PROJECT: &str = "far";
/// The far host, as typed.
pub const HOST: &str = "devbox";
/// The far user.
pub const USER: &str = "jlong";

/// One mutation the fake far daemon took: the issue and what it was asked.
pub type Taken = (String, MutationKind);

pub struct FarHost {
    _home: Home,
    dir: PathBuf,
    /// The mutations the far daemon took, in order.
    pub taken: Arc<Mutex<Vec<Taken>>>,
    /// The project-state writes the far daemon made, in order.
    pub writes: Arc<Mutex<Vec<StateWrite>>>,
    /// The far daemon's change subscribers (for a pushed `Changed`).
    subscribers: Arc<Mutex<Vec<UnixStream>>>,
    /// The host's link, already up, over the in-process relay.
    pub link: Arc<HostLink>,
    pub remote: Arc<FakeRemote>,
}

/// An open issue on the far board.
pub fn issue(id: &str, status: &str) -> IssueFile {
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    IssueFile {
        task: Issue {
            id: id.into(),
            title: format!("Far issue {id}"),
            column: Column::from_status_id(status),
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: at,
            updated_at: at,
            params: Default::default(),
        },
        body: format!("Body of {id}"),
        tracker_assignees: Vec::new(),
    }
}

impl FarHost {
    /// The far host, its project's board holding `t-1` (To Do) and `t-2`
    /// (Backlog), its event log a few lines.
    pub fn new(tag: &str) -> FarHost {
        // The real hub's sockets stay out of it.
        for var in ["SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK", "SHELBI_HUB_ADDR"] {
            std::env::remove_var(var);
        }
        let home = Home::new(tag, PROJECT, crate::test_env::lock());
        for (id, status) in [("t-1", "in-progress"), ("t-2", "backlog")] {
            let issue = issue(id, status);
            shelbi_state::save_task(PROJECT, &issue.task, &issue.body).unwrap();
        }
        std::fs::write(
            home.root.join("events.log"),
            format!(
                "2026-11-02T10:00:00Z project={PROJECT} workspace=alpha task=t-1 -> working\n\
                 2026-11-02T10:05:00Z project={PROJECT} workspace=alpha task=t-1 -> awaiting_input\n"
            ),
        )
        .unwrap();

        // Short: sockets live here (macOS caps socket paths).
        let dir = std::env::temp_dir().join(format!("rwf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let taken: Arc<Mutex<Vec<Taken>>> = Arc::default();
        let writes: Arc<Mutex<Vec<StateWrite>>> = Arc::default();
        let subscribers: Arc<Mutex<Vec<UnixStream>>> = Arc::default();
        serve_hub(UnixListener::bind(dir.join("hub.sock")).unwrap());
        serve_daemon(
            UnixListener::bind(dir.join("control.sock")).unwrap(),
            Took {
                taken: taken.clone(),
                writes: writes.clone(),
            },
            subscribers.clone(),
        );

        let channel: Arc<dyn RelayLink> = Arc::new(relay(&dir));
        let link = Arc::new(HostLink::new(HOST, move |_| Ok(channel.clone())));
        link.connect(&|| Ok(())).expect("the in-process relay is up");
        let remote = Arc::new(FakeRemote::new().with_host(HOST, USER, &[(PROJECT, "~/Workspaces/far")]));
        FarHost {
            _home: home,
            dir,
            taken,
            writes,
            subscribers,
            link,
            remote,
        }
    }

    /// The far host's listing, as the Connect dialog's probe reads it.
    pub fn listing(&self) -> RemoteListing {
        self.remote.listing(HOST).unwrap()
    }

    /// The session the Connect dialog would open.
    pub fn session(&self) -> RemoteSession {
        self.remote.find_sessions(HOST).unwrap().1.remove(0)
    }

    /// The far side moves `id` to `status` on its own (another client there),
    /// and its daemon says so.
    pub fn far_move(&self, id: &str, status: &str) {
        move_on_board(id, status);
        push_changed(&self.subscribers, id, status);
    }
}

impl Drop for FarHost {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A relay serving the far side: the query handler, and the far sockets.
fn relay(dir: &std::path::Path) -> ChannelLink {
    let (near, far) = UnixStream::pair().unwrap();
    let far_read = far.try_clone().unwrap();
    let services = RelayServices {
        query: Some(shelbi_orchestrator::source::QueryAnswers::default().handler()),
        hub_socket: Some(dir.join("hub.sock")),
        control_socket: Some(dir.join("control.sock")),
    };
    let root = dir.join("sessions");
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay_with(Box::new(far_read), Box::new(far), &root, services);
    });
    let near_read = near.try_clone().unwrap();
    ChannelLink::new(HOST, RelayChannel::new(Box::new(near_read), Box::new(near)).unwrap())
}

/// The far hub: takes each subscriber's line and keeps it open.
fn serve_hub(listener: UnixListener) {
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
            held.push(stream);
        }
    });
}

/// What the far daemon records.
#[derive(Clone)]
struct Took {
    taken: Arc<Mutex<Vec<Taken>>>,
    writes: Arc<Mutex<Vec<StateWrite>>>,
}

/// The far daemon: one thread per connection.
fn serve_daemon(listener: UnixListener, took: Took, subscribers: Arc<Mutex<Vec<UnixStream>>>) {
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (took, subscribers) = (took.clone(), subscribers.clone());
            std::thread::spawn(move || serve_connection(stream, &took, &subscribers));
        }
    });
}

fn serve_connection(mut stream: UnixStream, took: &Took, subscribers: &Arc<Mutex<Vec<UnixStream>>>) {
    let mut buf = Vec::new();
    while let Some(msg) = read_frame::<ClientMsg>(&mut stream, &mut buf) {
        let reply = match msg {
            ClientMsg::Hello { .. } => Some(ServerMsg::Hello {
                protocol: CONTROL_PROTOCOL_VERSION,
                daemon_version: shelbi_state::CLIENT_VERSION.to_string(),
            }),
            ClientMsg::Subscribe { .. } => {
                subscribers.lock().unwrap().push(stream.try_clone().unwrap());
                None
            }
            ClientMsg::Mutate(req) => {
                if let MutationKind::Move { to, .. } = &req.kind {
                    move_on_board(&req.id, to);
                }
                took.taken.lock().unwrap().push((req.id.clone(), req.kind.clone()));
                let done = ServerMsg::Done {
                    request_id: req.request_id,
                    state: None,
                    id: None,
                };
                let _ = stream.write_all(&control::encode(&done).unwrap());
                if let MutationKind::Move { to, .. } = &req.kind {
                    push_changed(subscribers, &req.id, to);
                }
                None
            }
            ClientMsg::WriteState(req) => {
                let result = shelbi_orchestrator::state_write::apply(&req.project, &req.write);
                took.writes.lock().unwrap().push(req.write);
                Some(match result {
                    Ok(()) => ServerMsg::Done {
                        request_id: req.request_id,
                        state: None,
                        id: None,
                    },
                    Err(message) => ServerMsg::Failed {
                        request_id: req.request_id,
                        error: MutationError::Backend { message },
                    },
                })
            }
            _ => None,
        };
        if let Some(reply) = reply {
            if stream.write_all(&control::encode(&reply).unwrap()).is_err() {
                return;
            }
        }
    }
}

/// The far store's write: `id` now sits in `status` on the far board.
fn move_on_board(id: &str, status: &str) {
    let mut moved = issue(id, status);
    moved.task.updated_at = chrono::Utc::now();
    shelbi_state::save_task(PROJECT, &moved.task, &moved.body).unwrap();
}

/// Where `id` sits on the far board (its task file).
pub fn far_column(id: &str) -> Option<Column> {
    shelbi_state::read_board(PROJECT)
        .ok()?
        .into_issues()
        .into_iter()
        .find(|i| i.task.id == id)
        .map(|i| i.task.column)
}

/// Tell every far subscriber `id` moved to `status`.
fn push_changed(subscribers: &Mutex<Vec<UnixStream>>, id: &str, status: &str) {
    let note = ServerMsg::Changed(ChangeNote {
        project: PROJECT.into(),
        id: id.into(),
        verb: "move".into(),
        status: status.into(),
        updated_at: String::new(),
    });
    let frame = control::encode(&note).unwrap();
    subscribers.lock().unwrap().retain_mut(|s| s.write_all(&frame).is_ok());
}

/// One control frame, or `None` once the peer hangs up.
fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream, buf: &mut Vec<u8>) -> Option<T> {
    loop {
        match control::decode::<T>(buf) {
            Ok((msg, used)) => {
                buf.drain(..used);
                return Some(msg);
            }
            Err(shelbi_proto::ProtoError::Incomplete { .. }) => {
                let mut chunk = [0u8; 4096];
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return None,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            Err(_) => return None,
        }
    }
}
