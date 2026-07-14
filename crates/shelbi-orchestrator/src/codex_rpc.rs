//! Small blocking client for Codex app-server's JSON-RPC-over-WebSocket API.
//!
//! Codex's `unix://` app-server transport is a regular WebSocket HTTP upgrade
//! carried over a Unix domain socket. This client deliberately owns one
//! request at a time: that matches Shelbi's wake-delivery actor and keeps
//! response correlation straightforward while still retaining notifications
//! received before the matching response.

use std::collections::VecDeque;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use thiserror::Error;
use tungstenite::{client, Message, WebSocket};

/// A server notification retained while a request is awaiting its response.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexRpcNotification {
    pub method: String,
    /// `None` preserves the distinction between an omitted `params` member and
    /// an explicit JSON `null` value.
    pub params: Option<Value>,
}

/// Errors from the Codex app-server connection or protocol.
#[derive(Debug, Error)]
pub enum CodexRpcError {
    #[error("failed to connect to Codex app-server socket: {0}")]
    Connect(#[source] io::Error),
    #[error("failed to configure Codex app-server socket: {0}")]
    Socket(#[source] io::Error),
    #[error("Codex app-server WebSocket handshake failed: {0}")]
    Handshake(String),
    #[error("Codex app-server WebSocket error: {0}")]
    WebSocket(#[from] tungstenite::Error),
    #[error("invalid JSON from Codex app-server: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Codex app-server request `{method}` timed out after {timeout:?}")]
    Timeout { method: String, timeout: Duration },
    #[error("Codex app-server rejected `{method}` ({code}): {message}")]
    Remote {
        method: String,
        code: i64,
        message: String,
        data: Option<Value>,
    },
    #[error("Codex app-server protocol error: {0}")]
    Protocol(String),
    #[error("Codex app-server connection is closed")]
    Closed,
    #[error("Codex app-server connection cannot be reused after a timed-out request")]
    Desynchronized,
}

pub type CodexRpcResult<T> = Result<T, CodexRpcError>;

/// A synchronous, single-request-at-a-time Codex app-server client.
pub struct CodexRpcClient {
    socket: WebSocket<UnixStream>,
    next_request_id: i64,
    notifications: VecDeque<CodexRpcNotification>,
    io_timeout: Duration,
    closed: bool,
    desynchronized: bool,
}

impl CodexRpcClient {
    /// Connect, perform the WebSocket upgrade, and complete Codex's
    /// `initialize` / `initialized` handshake.
    pub fn connect(
        socket_path: impl AsRef<Path>,
        client_name: &str,
        client_version: &str,
        timeout: Duration,
    ) -> CodexRpcResult<Self> {
        let stream = UnixStream::connect(socket_path).map_err(CodexRpcError::Connect)?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(CodexRpcError::Socket)?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(CodexRpcError::Socket)?;

        // A synthetic host is sufficient because the HTTP upgrade is sent over
        // the already-connected Unix stream. `client` does not add an Origin
        // header, which is also what Codex's local transport expects.
        let (socket, _) = client("ws://localhost/", stream)
            .map_err(|error| CodexRpcError::Handshake(error.to_string()))?;
        let mut rpc = Self {
            socket,
            next_request_id: 1,
            notifications: VecDeque::new(),
            io_timeout: timeout,
            closed: false,
            desynchronized: false,
        };

        rpc.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": client_name,
                    "title": "Shelbi",
                    "version": client_version,
                },
                "capabilities": {
                    "experimentalApi": true,
                },
            }),
            timeout,
        )?;
        rpc.notify("initialized", None)?;
        Ok(rpc)
    }

    /// Send a request and wait up to `timeout` for its matching response.
    ///
    /// Server notifications received first are queued and can be obtained with
    /// [`Self::drain_notifications`] or [`Self::poll`]. A timeout poisons this
    /// connection because a late response would otherwise be indistinguishable
    /// from a response to later work; callers should reconnect and replay from
    /// Shelbi's durable cursor.
    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> CodexRpcResult<Value> {
        self.ensure_usable()?;
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| CodexRpcError::Protocol("request id space exhausted".into()))?;

        self.send_json(json!({
            "method": method,
            "id": request_id,
            "params": params,
        }))?;

        let deadline = Instant::now() + timeout;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                self.desynchronized = true;
                return Err(CodexRpcError::Timeout {
                    method: method.to_string(),
                    timeout,
                });
            };
            self.socket
                .get_mut()
                .set_read_timeout(Some(remaining))
                .map_err(CodexRpcError::Socket)?;

            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    self.desynchronized = true;
                    return Err(CodexRpcError::Timeout {
                        method: method.to_string(),
                        timeout,
                    });
                }
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    self.closed = true;
                    return Err(CodexRpcError::Closed);
                }
                Err(error) => return Err(error.into()),
            };

            match self.process_message(message)? {
                Incoming::Notification(notification) => {
                    self.notifications.push_back(notification);
                }
                Incoming::Response { id, result, error } => {
                    if id != json!(request_id) {
                        return Err(CodexRpcError::Protocol(format!(
                            "received response id {id} while waiting for {request_id}"
                        )));
                    }
                    self.restore_read_timeout()?;
                    if let Some(error) = error {
                        return Err(CodexRpcError::Remote {
                            method: method.to_string(),
                            code: error.code,
                            message: error.message,
                            data: error.data,
                        });
                    }
                    return result.ok_or_else(|| {
                        CodexRpcError::Protocol(format!(
                            "response to `{method}` contained neither result nor error"
                        ))
                    });
                }
                Incoming::Ignore => {}
                Incoming::Closed => {
                    self.closed = true;
                    return Err(CodexRpcError::Closed);
                }
            }
        }
    }

    /// Send a client notification. `None` omits the `params` member.
    pub fn notify(&mut self, method: &str, params: Option<Value>) -> CodexRpcResult<()> {
        self.ensure_usable()?;
        let mut message = Map::new();
        message.insert("method".into(), Value::String(method.to_string()));
        if let Some(params) = params {
            message.insert("params".into(), params);
        }
        self.send_json(Value::Object(message))
    }

    /// Read all currently available messages without waiting and return every
    /// queued notification, including notifications collected by `request`.
    pub fn poll(&mut self) -> CodexRpcResult<Vec<CodexRpcNotification>> {
        self.ensure_usable()?;
        self.socket
            .get_mut()
            .set_nonblocking(true)
            .map_err(CodexRpcError::Socket)?;

        let poll_result = self.poll_nonblocking();
        let restore_result = self.socket.get_mut().set_nonblocking(false);
        if let Err(error) = restore_result {
            return Err(CodexRpcError::Socket(error));
        }
        self.restore_read_timeout()?;
        poll_result?;
        Ok(self.drain_notifications())
    }

    /// Drain notifications already observed by earlier requests or polls.
    pub fn drain_notifications(&mut self) -> Vec<CodexRpcNotification> {
        self.notifications.drain(..).collect()
    }

    /// Initiate the WebSocket closing handshake.
    #[cfg(test)]
    pub fn close(&mut self) -> CodexRpcResult<()> {
        if self.closed {
            return Ok(());
        }
        match self.socket.close(None) {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                self.closed = true;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn ensure_usable(&self) -> CodexRpcResult<()> {
        if self.closed {
            Err(CodexRpcError::Closed)
        } else if self.desynchronized {
            Err(CodexRpcError::Desynchronized)
        } else {
            Ok(())
        }
    }

    fn send_json(&mut self, value: Value) -> CodexRpcResult<()> {
        self.socket.send(Message::Text(value.to_string().into()))?;
        Ok(())
    }

    fn restore_read_timeout(&self) -> CodexRpcResult<()> {
        self.socket
            .get_ref()
            .set_read_timeout(Some(self.io_timeout))
            .map_err(CodexRpcError::Socket)
    }

    fn poll_nonblocking(&mut self) -> CodexRpcResult<()> {
        loop {
            let message = match self.socket.read() {
                Ok(message) => message,
                Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(());
                }
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    self.closed = true;
                    return Ok(());
                }
                Err(error) => return Err(error.into()),
            };

            match self.process_message(message)? {
                Incoming::Notification(notification) => {
                    self.notifications.push_back(notification);
                }
                Incoming::Response { id, .. } => {
                    return Err(CodexRpcError::Protocol(format!(
                        "received unsolicited response id {id}"
                    )));
                }
                Incoming::Ignore => {}
                Incoming::Closed => {
                    self.closed = true;
                    return Ok(());
                }
            }
        }
    }

    fn process_message(&mut self, message: Message) -> CodexRpcResult<Incoming> {
        match message {
            Message::Text(text) => parse_incoming(text.as_str()),
            Message::Ping(_) => {
                // Tungstenite queues the protocol-mandated Pong while reading.
                // Flushing here ensures it is sent even when Shelbi has no
                // application request ready to write.
                self.socket.flush()?;
                Ok(Incoming::Ignore)
            }
            Message::Pong(_) | Message::Frame(_) => Ok(Incoming::Ignore),
            Message::Close(_) => Ok(Incoming::Closed),
            Message::Binary(_) => Err(CodexRpcError::Protocol(
                "received binary data on the JSON text protocol".into(),
            )),
        }
    }
}

#[derive(Debug)]
struct RemoteError {
    code: i64,
    message: String,
    data: Option<Value>,
}

enum Incoming {
    Notification(CodexRpcNotification),
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<RemoteError>,
    },
    Ignore,
    Closed,
}

fn parse_incoming(text: &str) -> CodexRpcResult<Incoming> {
    let value: Value = serde_json::from_str(text)?;
    let object = value
        .as_object()
        .ok_or_else(|| CodexRpcError::Protocol("top-level message was not an object".into()))?;

    if let Some(method) = object.get("method").and_then(Value::as_str) {
        if object.contains_key("id") {
            // Approval and tool-input requests are broadcast to every
            // subscriber. The visible TUI owns those interactions. Ignoring
            // the bridge's copy keeps its request/response stream aligned
            // without racing the user's approval response.
            return Ok(Incoming::Ignore);
        }
        return Ok(Incoming::Notification(CodexRpcNotification {
            method: method.to_string(),
            params: object.get("params").cloned(),
        }));
    }

    let id = object
        .get("id")
        .cloned()
        .ok_or_else(|| CodexRpcError::Protocol("message had neither method nor id".into()))?;
    let result = object.get("result").cloned();
    let error = match object.get("error") {
        Some(Value::Object(error)) => Some(RemoteError {
            code: error.get("code").and_then(Value::as_i64).ok_or_else(|| {
                CodexRpcError::Protocol("error response had no integer code".into())
            })?,
            message: error
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CodexRpcError::Protocol("error response had no string message".into())
                })?
                .to_string(),
            data: error.get("data").cloned(),
        }),
        Some(_) => {
            return Err(CodexRpcError::Protocol(
                "error response member was not an object".into(),
            ))
        }
        None => None,
    };

    if result.is_some() && error.is_some() {
        return Err(CodexRpcError::Protocol(
            "response contained both result and error".into(),
        ));
    }
    Ok(Incoming::Response { id, result, error })
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::thread;

    use tempfile::tempdir;
    use tungstenite::{accept, Message};

    use super::*;

    #[test]
    fn initializes_requests_and_collects_notifications() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("app-server.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (send_poll_notification, receive_poll_notification) = mpsc::channel();
        let (poll_notification_sent, wait_until_sent) = mpsc::channel();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = accept(stream).unwrap();

            let initialize = read_json(&mut websocket);
            assert_eq!(initialize["method"], "initialize");
            assert_eq!(initialize["id"], 1);
            assert_eq!(initialize["params"]["clientInfo"]["name"], "shelbi-test");
            assert_eq!(
                initialize["params"]["clientInfo"]["version"],
                env!("CARGO_PKG_VERSION")
            );
            assert_eq!(
                initialize["params"]["capabilities"]["experimentalApi"],
                true
            );
            send_json(
                &mut websocket,
                json!({"method": "server/early", "params": {"phase": "initialize"}}),
            );
            send_json(
                &mut websocket,
                json!({"id": 1, "result": {"userAgent": "codex-test/0.144.1"}}),
            );

            let initialized = read_json(&mut websocket);
            assert_eq!(initialized, json!({"method": "initialized"}));

            let request = read_json(&mut websocket);
            assert_eq!(request["method"], "turn/start");
            assert_eq!(request["id"], 2);
            send_json(
                &mut websocket,
                json!({"method": "turn/started", "params": {"threadId": "thread-1"}}),
            );
            send_json(
                &mut websocket,
                json!({"id": 2, "result": {"turn": {"id": "turn-1"}}}),
            );

            receive_poll_notification.recv().unwrap();
            send_json(
                &mut websocket,
                json!({"method": "turn/completed", "params": {"threadId": "thread-1"}}),
            );
            poll_notification_sent.send(()).unwrap();

            match websocket.read() {
                Ok(Message::Close(_))
                | Err(tungstenite::Error::ConnectionClosed)
                | Err(tungstenite::Error::AlreadyClosed) => {}
                other => panic!("expected close handshake, got {other:?}"),
            }
        });

        let mut client = CodexRpcClient::connect(
            &socket_path,
            "shelbi-test",
            env!("CARGO_PKG_VERSION"),
            Duration::from_secs(1),
        )
        .unwrap();
        let response = client
            .request(
                "turn/start",
                json!({"threadId": "thread-1", "input": []}),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(response["turn"]["id"], "turn-1");
        assert_eq!(
            client.drain_notifications(),
            vec![
                CodexRpcNotification {
                    method: "server/early".into(),
                    params: Some(json!({"phase": "initialize"})),
                },
                CodexRpcNotification {
                    method: "turn/started".into(),
                    params: Some(json!({"threadId": "thread-1"})),
                },
            ]
        );

        send_poll_notification.send(()).unwrap();
        wait_until_sent.recv().unwrap();
        assert_eq!(
            client.poll().unwrap(),
            vec![CodexRpcNotification {
                method: "turn/completed".into(),
                params: Some(json!({"threadId": "thread-1"})),
            }]
        );
        client.close().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn parses_remote_error_data() {
        let error = parse_incoming(
            r#"{"id":7,"error":{"code":-32600,"message":"no active turn to steer","data":{"retry":true}}}"#,
        )
        .unwrap();
        let Incoming::Response {
            id,
            result,
            error: Some(error),
        } = error
        else {
            panic!("expected an error response");
        };
        assert_eq!(id, json!(7));
        assert!(result.is_none());
        assert_eq!(error.code, -32600);
        assert_eq!(error.message, "no active turn to steer");
        assert_eq!(error.data, Some(json!({"retry": true})));
    }

    #[test]
    fn ignores_broadcast_approval_requests_without_desynchronizing() {
        let approval = parse_incoming(
            r#"{"method":"item/commandExecution/requestApproval","id":91,"params":{"threadId":"thread-1"}}"#,
        )
        .unwrap();
        assert!(matches!(approval, Incoming::Ignore));

        let response = parse_incoming(r#"{"id":7,"result":{"turnId":"turn-1"}}"#).unwrap();
        assert!(matches!(
            response,
            Incoming::Response {
                id,
                result: Some(_),
                error: None,
            } if id == json!(7)
        ));
    }

    #[test]
    fn approval_request_during_rpc_is_ignored_and_connection_remains_usable() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("app-server.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (release_server, wait_for_release) = mpsc::channel();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = accept(stream).unwrap();
            let initialize = read_json(&mut websocket);
            send_json(
                &mut websocket,
                json!({"id": initialize["id"], "result": {"userAgent": "codex-test"}}),
            );
            assert_eq!(read_json(&mut websocket), json!({"method": "initialized"}));

            let first = read_json(&mut websocket);
            assert_eq!(first["method"], "turn/start");
            send_json(
                &mut websocket,
                json!({
                    "method": "item/commandExecution/requestApproval",
                    "id": 91,
                    "params": {"threadId": "thread-1", "turnId": "turn-1"}
                }),
            );
            send_json(
                &mut websocket,
                json!({"id": first["id"], "result": {"turn": {"id": "turn-1"}}}),
            );

            let second = read_json(&mut websocket);
            assert_eq!(second["method"], "thread/read");
            send_json(
                &mut websocket,
                json!({"id": second["id"], "result": {"thread": {"id": "thread-1"}}}),
            );
            wait_for_release.recv().unwrap();
        });

        let mut client = CodexRpcClient::connect(
            &socket_path,
            "shelbi-test",
            env!("CARGO_PKG_VERSION"),
            Duration::from_secs(1),
        )
        .unwrap();
        let first = client
            .request(
                "turn/start",
                json!({"threadId": "thread-1", "input": []}),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(first["turn"]["id"], "turn-1");
        assert!(client.drain_notifications().is_empty());

        let second = client
            .request(
                "thread/read",
                json!({"threadId": "thread-1"}),
                Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(second["thread"]["id"], "thread-1");
        release_server.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn timeout_retains_notifications_and_requires_reconnect() {
        let temp = tempdir().unwrap();
        let socket_path = temp.path().join("app-server.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let (release_server, wait_for_release) = mpsc::channel();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut websocket = accept(stream).unwrap();
            let initialize = read_json(&mut websocket);
            send_json(
                &mut websocket,
                json!({"id": initialize["id"], "result": {"userAgent": "codex-test"}}),
            );
            assert_eq!(read_json(&mut websocket), json!({"method": "initialized"}));
            let request = read_json(&mut websocket);
            send_json(
                &mut websocket,
                json!({"method": "thread/status/changed", "params": {"status": {"type": "idle"}}}),
            );
            wait_for_release.recv().unwrap();
            send_json(
                &mut websocket,
                json!({"id": request["id"], "result": {"late": true}}),
            );
        });

        let mut client = CodexRpcClient::connect(
            &socket_path,
            "shelbi-test",
            env!("CARGO_PKG_VERSION"),
            Duration::from_secs(1),
        )
        .unwrap();
        let error = client
            .request("turn/start", json!({}), Duration::from_millis(25))
            .unwrap_err();
        match error {
            CodexRpcError::Timeout { method, timeout } => {
                assert_eq!(method, "turn/start");
                assert_eq!(timeout, Duration::from_millis(25));
            }
            other => panic!("expected timeout, got {other:?}"),
        }
        assert_eq!(
            client.drain_notifications(),
            vec![CodexRpcNotification {
                method: "thread/status/changed".into(),
                params: Some(json!({"status": {"type": "idle"}})),
            }]
        );
        assert!(matches!(
            client.request("thread/read", json!({}), Duration::from_secs(1)),
            Err(CodexRpcError::Desynchronized)
        ));

        release_server.send(()).unwrap();
        server.join().unwrap();
    }

    fn read_json(websocket: &mut WebSocket<UnixStream>) -> Value {
        match websocket.read().unwrap() {
            Message::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
            other => panic!("expected text message, got {other:?}"),
        }
    }

    fn send_json(websocket: &mut WebSocket<UnixStream>, value: Value) {
        websocket
            .send(Message::Text(value.to_string().into()))
            .unwrap();
    }
}
