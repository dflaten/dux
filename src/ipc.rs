//! Local control channel for agents running inside dux.
//!
//! The TUI owns all workspace state, so an agent process cannot safely write
//! SQLite or git worktrees on its own. Instead, agents invoke the `dux` CLI,
//! which connects to a user-only Unix socket under the dux config directory.
//! The TUI receives the request on its event loop, performs the work through
//! its normal workers, and returns a JSON response to the waiting CLI.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub const CONTROL_PROTOCOL: u32 = 1;

/// Maximum accepted request or response size. Prompts can be large, but not
/// unbounded.
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 256 * 1024;

/// How long a CLI request waits for the TUI to finish the operation.
pub const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum number of simultaneously connected control clients.
const MAX_CONTROL_CONNECTIONS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlRequest {
    pub protocol: u32,
    #[serde(flatten)]
    pub command: ControlCommand,
}

impl ControlRequest {
    pub fn split_out(params: SplitOutParams) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL,
            command: ControlCommand::SplitOut(params),
        }
    }

    pub fn related(source_session_id: Option<String>) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL,
            command: ControlCommand::Related { source_session_id },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ControlCommand {
    SplitOut(SplitOutParams),
    Related { source_session_id: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SplitOutParams {
    /// Defaults to the `DUX_SESSION_ID` inherited from the dux-launched agent.
    pub source_session_id: Option<String>,
    /// Target git checkout. Ignored when `project` is also supplied.
    pub repo: Option<String>,
    /// Registered project id or name. Takes precedence over `repo`.
    pub project: Option<String>,
    /// Branch and worktree name. Generated when omitted.
    pub name: Option<String>,
    /// Base branch. Defaults to the target project's leading branch.
    pub base: Option<String>,
    /// Provider override. Defaults to the target project's default provider.
    pub provider: Option<String>,
    /// Optional initial handoff prompt for the child agent.
    pub prompt: Option<String>,
    /// Client-side timeout in milliseconds. Informational for the server.
    pub timeout_ms: Option<u64>,
}

impl SplitOutParams {
    pub fn timeout(&self) -> Duration {
        self.timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_CONTROL_TIMEOUT)
    }
}

impl ControlCommand {
    pub fn timeout(&self) -> Duration {
        match self {
            Self::SplitOut(params) => params.timeout(),
            Self::Related { .. } => DEFAULT_CONTROL_TIMEOUT,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RelatedAgentInfo {
    pub session_id: String,
    pub project_id: String,
    pub project_name: String,
    pub label: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub provider: String,
    pub status: String,
    pub is_origin: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlResponse {
    pub protocol: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_added: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<RelatedAgentInfo>,
}

impl ControlResponse {
    pub fn success() -> Self {
        Self {
            protocol: CONTROL_PROTOCOL,
            ok: true,
            ..Self::default()
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            protocol: CONTROL_PROTOCOL,
            ok: false,
            code: Some(code.into()),
            error: Some(message.into()),
            ..Self::default()
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

impl Default for ControlResponse {
    fn default() -> Self {
        Self {
            protocol: CONTROL_PROTOCOL,
            ok: false,
            code: None,
            error: None,
            message: None,
            session_id: None,
            group_id: None,
            group_label: None,
            project: None,
            project_added: None,
            branch: None,
            worktree: None,
            provider: None,
            status: None,
            related: Vec::new(),
        }
    }
}

/// A request forwarded from the socket listener to the TUI event loop.
pub struct IncomingControlRequest {
    pub request: ControlRequest,
    pub respond: Sender<ControlResponse>,
}

/// Owns the listening socket and the accept thread. Dropping it removes the
/// socket file and stops the accept loop.
pub struct ControlServer {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ControlServer {
    pub fn start(path: PathBuf, tx: Sender<IncomingControlRequest>) -> Result<Self> {
        prepare_socket_path(&path)?;
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("failed to bind control socket {}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure control socket {}", path.display()))?;
        listener
            .set_nonblocking(true)
            .context("failed to configure control socket")?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let active_connections = Arc::new(AtomicUsize::new(0));
        let handle = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        if active_connections.fetch_add(1, Ordering::AcqRel)
                            >= MAX_CONTROL_CONNECTIONS
                        {
                            active_connections.fetch_sub(1, Ordering::AcqRel);
                            let _ = write_response(
                                stream,
                                &ControlResponse::error(
                                    "busy",
                                    "dux is handling too many agent control requests. Try again shortly.",
                                ),
                            );
                            continue;
                        }
                        let tx = tx.clone();
                        let connections = Arc::clone(&active_connections);
                        thread::spawn(move || {
                            handle_connection(stream, tx);
                            connections.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(err) => {
                        crate::logger::debug(&format!("control socket accept failed: {err}"));
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        });

        Ok(Self {
            path,
            shutdown,
            handle: Some(handle),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                let _ = fs::remove_file(&self.path);
            }
            _ => {}
        }
    }
}

fn prepare_socket_path(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.file_type().is_socket() {
        bail!(
            "control socket path {} already exists and is not a socket. Remove it or set DUX_SOCKET to a different path.",
            path.display()
        );
    }
    // The single-instance lock guarantees no other dux owns this directory, so
    // any pre-existing socket is left over from a previous process.
    fs::remove_file(path)
        .with_context(|| format!("failed to remove stale control socket {}", path.display()))?;
    Ok(())
}

fn handle_connection(stream: UnixStream, tx: Sender<IncomingControlRequest>) {
    if let Err(err) = prepare_connection(&stream) {
        let _ = write_response(
            stream,
            &ControlResponse::error(
                "io_error",
                format!("failed to configure the control connection: {err}"),
            ),
        );
        return;
    }

    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(err) => {
            let _ = write_response(
                stream,
                &ControlResponse::error("invalid_request", err.to_string()),
            );
            return;
        }
    };
    if request.protocol != CONTROL_PROTOCOL {
        let _ = write_response(
            stream,
            &ControlResponse::error(
                "protocol_mismatch",
                format!(
                    "This dux CLI speaks control protocol {} but dux speaks {}. Use the DUX_BIN path injected by dux or update dux.",
                    request.protocol, CONTROL_PROTOCOL
                ),
            ),
        );
        return;
    }

    let timeout = request.command.timeout().min(Duration::from_secs(600));
    let (response_tx, response_rx) = mpsc::channel();
    if tx
        .send(IncomingControlRequest {
            request,
            respond: response_tx,
        })
        .is_err()
    {
        let _ = write_response(
            stream,
            &ControlResponse::error("not_running", "dux is shutting down."),
        );
        return;
    }

    match response_rx.recv_timeout(timeout) {
        Ok(response) => {
            let _ = write_response(stream, &response);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = write_response(
                stream,
                &ControlResponse::error(
                    "timeout",
                    "dux did not finish the request in time. The operation may still complete; check dux before retrying.",
                ),
            );
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = write_response(
                stream,
                &ControlResponse::error("not_running", "dux closed the control channel."),
            );
        }
    }
}

/// Put an accepted connection into blocking mode with bounded I/O timeouts.
///
/// The listener is nonblocking so the accept loop can poll for shutdown. On
/// Linux the accepted socket starts blocking, but BSD-derived systems
/// (including macOS) inherit `O_NONBLOCK` from the listener. Without resetting
/// it, the first read can return `WouldBlock` before the client's request
/// arrives, and timeout configuration is not enough to fix that.
fn prepare_connection(stream: &UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    Ok(())
}

fn read_request(stream: &UnixStream) -> Result<ControlRequest> {
    let mut reader = BufReader::new(stream);
    let line = read_limited_line(&mut reader, MAX_CONTROL_MESSAGE_BYTES)?;
    if line.is_empty() {
        bail!("empty control request");
    }
    serde_json::from_str(&line).context("invalid control request JSON")
}

fn read_limited_line(reader: &mut impl BufRead, limit: usize) -> Result<String> {
    let mut bytes = Vec::new();
    let mut limited = reader.take(limit as u64 + 1);
    limited
        .read_until(b'\n', &mut bytes)
        .context("failed to read control message")?;
    if bytes.len() > limit {
        bail!("control message exceeds {limit} bytes");
    }
    if bytes.is_empty() {
        return Ok(String::new());
    }
    if let Some(stripped) = bytes.strip_suffix(b"\n") {
        bytes = stripped.to_vec();
    }
    if let Some(stripped) = bytes.strip_suffix(b"\r") {
        bytes = stripped.to_vec();
    }
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn write_response(mut stream: UnixStream, response: &ControlResponse) -> Result<()> {
    let encoded = serde_json::to_vec(response)?;
    if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
        let fallback = serde_json::to_vec(&ControlResponse::error(
            "response_too_large",
            "control response was too large",
        ))?;
        stream.write_all(&fallback)?;
    } else {
        stream.write_all(&encoded)?;
    }
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Send a request to the running dux TUI and wait for its response.
pub fn send_request(
    socket_path: &Path,
    request: &ControlRequest,
    timeout: Duration,
) -> Result<ControlResponse> {
    let mut stream = UnixStream::connect(socket_path).map_err(|err| {
        anyhow!(
            "could not connect to dux at {}: {err}. Is dux running with agent control enabled?",
            socket_path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(timeout + Duration::from_secs(5)))
        .context("failed to configure control socket timeout")?;
    stream
        .set_write_timeout(Some(Duration::from_secs(15)))
        .context("failed to configure control socket timeout")?;

    let mut encoded = serde_json::to_vec(request).context("failed to encode control request")?;
    encoded.push(b'\n');
    if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
        bail!(
            "control request is too large ({} bytes, limit {}). Shorten the handoff prompt.",
            encoded.len(),
            MAX_CONTROL_MESSAGE_BYTES
        );
    }
    stream
        .write_all(&encoded)
        .context("failed to send control request to dux")?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let line = read_limited_line(&mut reader, MAX_CONTROL_MESSAGE_BYTES)?;
    if line.is_empty() {
        bail!("dux closed the control connection without a response");
    }
    let response: ControlResponse =
        serde_json::from_str(&line).context("dux returned invalid control response JSON")?;
    if response.protocol != CONTROL_PROTOCOL {
        bail!(
            "dux responded with control protocol {}, expected {}. Update dux or the CLI binary.",
            response.protocol,
            CONTROL_PROTOCOL
        );
    }
    Ok(response)
}

/// Read a whole prompt from a file, or stdin when `path` is `-`.
pub fn read_prompt_file(path: &str) -> Result<String> {
    let mut text = String::new();
    if path == "-" {
        std::io::stdin()
            .take(MAX_CONTROL_MESSAGE_BYTES as u64 + 1)
            .read_to_string(&mut text)
            .context("failed to read prompt from stdin")?;
    } else {
        if let Ok(metadata) = fs::metadata(path)
            && metadata.len() > MAX_CONTROL_MESSAGE_BYTES as u64
        {
            bail!(
                "prompt file {path} is too large ({} bytes, limit {})",
                metadata.len(),
                MAX_CONTROL_MESSAGE_BYTES
            );
        }
        text = fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {path}"))?;
    }
    if text.len() > MAX_CONTROL_MESSAGE_BYTES {
        bail!(
            "prompt is too large ({} bytes, limit {})",
            text.len(),
            MAX_CONTROL_MESSAGE_BYTES
        );
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;
    use tempfile::tempdir;

    fn test_server(dir: &Path) -> (ControlServer, Receiver<IncomingControlRequest>) {
        let (tx, rx) = mpsc::channel();
        let server = ControlServer::start(dir.join("dux.sock"), tx).expect("start server");
        (server, rx)
    }

    fn respond_once(rx: &Receiver<IncomingControlRequest>, response: ControlResponse) {
        let incoming = rx.recv_timeout(Duration::from_secs(2)).expect("request");
        incoming.respond.send(response).expect("respond");
    }

    #[test]
    fn split_out_request_round_trips_over_the_socket() {
        let dir = tempdir().unwrap();
        let (server, rx) = test_server(dir.path());
        let socket = server.path().to_path_buf();
        let client = thread::spawn(move || {
            send_request(
                &socket,
                &ControlRequest::split_out(SplitOutParams {
                    repo: Some("/tmp/example".to_string()),
                    name: Some("fix-auth".to_string()),
                    ..Default::default()
                }),
                Duration::from_secs(5),
            )
        });
        respond_once(
            &rx,
            ControlResponse::success()
                .with_message("created")
                .to_owned(),
        );
        let response = client.join().unwrap().unwrap();
        assert!(response.ok);
        assert_eq!(response.message.as_deref(), Some("created"));
    }

    #[test]
    fn malformed_request_returns_an_error_without_reaching_the_app() {
        let dir = tempdir().unwrap();
        let (server, rx) = test_server(dir.path());
        let mut stream = UnixStream::connect(server.path()).unwrap();
        stream.write_all(b"not json\n").unwrap();
        let mut response = String::new();
        BufReader::new(stream)
            .read_to_string(&mut response)
            .unwrap();
        let response: ControlResponse = serde_json::from_str(response.trim()).unwrap();
        assert!(!response.ok);
        assert_eq!(response.code.as_deref(), Some("invalid_request"));
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn stale_socket_is_replaced_but_regular_files_are_rejected() {
        let dir = tempdir().unwrap();
        let socket_path = dir.path().join("dux.sock");
        let stale = UnixListener::bind(&socket_path).unwrap();
        drop(stale);
        let _server = ControlServer::start(socket_path.clone(), mpsc::channel().0).unwrap();
        assert!(socket_path.exists());
        drop(_server);

        let regular = dir.path().join("not-a-socket");
        fs::write(&regular, "keep me").unwrap();
        let err = match ControlServer::start(regular.clone(), mpsc::channel().0) {
            Ok(_) => panic!("regular file should not be accepted as a socket"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not a socket"));
        assert_eq!(fs::read_to_string(regular).unwrap(), "keep me");
    }

    #[test]
    fn socket_is_user_only() {
        let dir = tempdir().unwrap();
        let (server, _rx) = test_server(dir.path());
        let mode = fs::metadata(server.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn prepared_connections_block_until_the_client_sends_data() {
        let (server, client) = UnixStream::pair().expect("socket pair");
        // Simulate BSD/macOS accept(), which inherits O_NONBLOCK from the
        // nonblocking listener.
        server.set_nonblocking(true).expect("set nonblocking");
        prepare_connection(&server).expect("prepare connection");

        let writer = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            (&client)
                .write_all(b"{\"protocol\":1}\n")
                .expect("client write");
        });

        let mut reader = BufReader::new(server);
        let mut line = String::new();
        reader.read_line(&mut line).expect("blocking read");
        assert_eq!(line, "{\"protocol\":1}\n");
        writer.join().expect("writer thread");
    }

    #[test]
    fn oversized_request_is_rejected() {
        let dir = tempdir().unwrap();
        let (server, _rx) = test_server(dir.path());
        let mut stream = UnixStream::connect(server.path()).unwrap();
        let junk = vec![b'{'; MAX_CONTROL_MESSAGE_BYTES + 1];
        let _ = stream.write_all(&junk);
        let mut response = String::new();
        let _ = BufReader::new(stream).read_to_string(&mut response);
        let response: ControlResponse = serde_json::from_str(response.trim()).unwrap();
        assert_eq!(response.code.as_deref(), Some("invalid_request"));
    }
}
