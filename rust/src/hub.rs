//! In-process hub: loopback HTTP + WebSocket server.
//!
//! Owns the single extension WebSocket and multiplexes peer connections. The
//! wire protocol invariants are documented in this module. The public API below
//! is consumed by `peer`, `tools`, `cli` and `ffi`; keep it stable.
//!
//! Security model (identical to the legacy `src/hub.ts`):
//!   * loopback only (never 0.0.0.0)
//!   * extension upgrades must carry a `moz-extension://` or
//!     `chrome-extension://` Origin; web Origins are rejected before the
//!     WebSocket handshake
//!   * peer upgrades reject `http(s)` Origins (native clients send none)
//!   * the first frame must be `{type:'hello', secret}` within 3 seconds
//!   * the secret comparison is constant-time
//!
//! Deliberately removed versus legacy: `/api/status`, `/api/command`, leader
//! election and every launchd/service behavior.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{CACHE_CONTROL, CONNECTION, CONTENT_TYPE, SEC_WEBSOCKET_ACCEPT, UPGRADE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Map, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Message, Utf8Bytes};
use tokio_tungstenite::WebSocketStream;

use crate::secret::secrets_match;

pub const HUB_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 19455;
pub const PROTOCOL_VERSION: &str = "3.0.0";
pub const AUTH_TIMEOUT_MS: u64 = 3_000;
pub const HEARTBEAT_INTERVAL_MS: u64 = 15_000;
pub const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 30_000;
pub const MAX_COMMAND_TIMEOUT_MS: u64 = 600_000;

/// Maximum accepted WebSocket message / frame size (64 MiB, legacy `ws` config).
const MAX_WEBSOCKET_SIZE: usize = 64 * 1024 * 1024;
/// Timeout for the quiet `tab_list` query inside `status(include_tabs)`.
const STATUS_TABS_TIMEOUT_MS: u64 = 4_000;
/// How long `send` waits for a reconnecting extension.
const EXTENSION_RECONNECT_WAIT_MS: u64 = 3_000;
/// Grace period for a WebSocket close handshake we initiated.
const CLOSE_HANDSHAKE_GRACE_MS: u64 = 250;

/// Hub log sink. Called from internal tasks; must not block for long.
pub type LogCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Invoked with a valid UTF-8 HubStatus JSON snapshot (tabs omitted) whenever
/// extension connection or peer count changes. Never called while internal
/// locks are held.
pub type StateCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Hub construction options.
#[derive(Clone, Default)]
pub struct HubOptions {
    pub port: u16,
    pub secret: String,
    pub on_log: Option<LogCallback>,
    pub on_state: Option<StateCallback>,
}

/// Connection info for the single attached extension.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionInfo {
    pub version: Option<String>,
    pub browser: Option<String>,
    pub protocol: Option<String>,
    pub connected_at_ms: Option<u64>,
    /// Legacy-compatible `client` field (`gecko-orange`) kept for the CLI's
    /// `Extension:` status line and the Swift `BrowserUseStatus` model.
    pub client: Option<String>,
    pub user_agent: Option<String>,
    pub remote_address: Option<String>,
}

/// Serializable status snapshot returned by HTTP `/health`, `/status`, the FFI
/// state callback and peer `status_res` messages.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubStatus {
    pub ok: bool,
    /// Legacy wire compatibility: the hub-status marker peers look for.
    pub hub: bool,
    pub service: String,
    pub mode: String,
    pub port: u16,
    pub pid: u32,
    pub protocol: String,
    pub uptime_ms: u64,
    pub extension_connected: bool,
    pub peers: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<ExtensionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs_error: Option<String>,
}

/// Hub error.
#[derive(Debug, Clone)]
pub struct HubError {
    pub message: String,
}

impl HubError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HubError {}

impl From<HubError> for String {
    fn from(error: HubError) -> Self {
        error.message
    }
}

// ── Internal state ───────────────────────────────────────────────────────

/// The single attached extension WebSocket.
struct ExtensionState {
    conn_id: u64,
    tx: mpsc::UnboundedSender<Message>,
    alive: Arc<AtomicBool>,
    info: ExtensionInfo,
}

/// One authenticated MCP/CLI peer (`/peer`).
struct PeerState {
    tx: mpsc::UnboundedSender<Message>,
}

/// One command awaiting an extension reply.
struct PendingCommand {
    action: String,
    quiet: bool,
    started_at: Instant,
    tx: oneshot::Sender<Result<Value, HubError>>,
}

struct SharedState {
    port: u16,
    started_at: Option<Instant>,
    extension: Option<ExtensionState>,
    peers: HashMap<u64, PeerState>,
    peer_seq: u64,
    extension_seq: u64,
    request_seq: u64,
    pending: HashMap<String, PendingCommand>,
}

impl SharedState {
    fn new(port: u16) -> Self {
        Self {
            port,
            started_at: None,
            extension: None,
            peers: HashMap::new(),
            peer_seq: 0,
            extension_seq: 0,
            request_seq: 0,
            pending: HashMap::new(),
        }
    }

    fn uptime_ms(&self) -> u64 {
        self.started_at
            .map(|started| started.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }

    fn extension_is_connected(&self) -> bool {
        self.extension
            .as_ref()
            .map(|extension| !extension.tx.is_closed())
            .unwrap_or(false)
    }

    fn snapshot(&self) -> HubStatus {
        let connected = self.extension_is_connected();
        HubStatus {
            ok: true,
            hub: true,
            service: crate::SERVER_NAME.to_string(),
            mode: "hub".to_string(),
            port: self.port,
            pid: std::process::id(),
            protocol: PROTOCOL_VERSION.to_string(),
            uptime_ms: self.uptime_ms(),
            extension_connected: connected,
            peers: self.peers.len(),
            extension: if connected {
                self.extension
                    .as_ref()
                    .map(|extension| extension.info.clone())
            } else {
                None
            },
            tabs: None,
            tabs_error: None,
        }
    }
}

fn lock_state(state: &StdMutex<SharedState>) -> std::sync::MutexGuard<'_, SharedState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ── HubShared: everything the spawned tasks need ─────────────────────────

struct HubShared {
    secret: String,
    requested_port: u16,
    on_log: Option<LogCallback>,
    on_state: Option<StateCallback>,
    state: StdMutex<SharedState>,
    extension_notify: Notify,
    shutdown: watch::Sender<bool>,
    stopping: AtomicBool,
    started: AtomicBool,
    callback_lock: StdMutex<()>,
    tasks: StdMutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl HubShared {
    fn new(options: HubOptions) -> Arc<Self> {
        assert!(
            !options.secret.is_empty(),
            "Hub requires a non-empty shared secret"
        );
        let (shutdown, _shutdown_rx) = watch::channel(false);
        Arc::new(Self {
            secret: options.secret,
            requested_port: options.port,
            on_log: options.on_log,
            on_state: options.on_state,
            state: StdMutex::new(SharedState::new(options.port)),
            extension_notify: Notify::new(),
            shutdown,
            stopping: AtomicBool::new(false),
            started: AtomicBool::new(false),
            callback_lock: StdMutex::new(()),
            tasks: StdMutex::new(Vec::new()),
        })
    }

    fn log(&self, line: &str) {
        if let Some(on_log) = &self.on_log {
            on_log(line);
        }
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn port(&self) -> u16 {
        lock_state(&self.state).port
    }

    fn is_extension_connected(&self) -> bool {
        lock_state(&self.state).extension_is_connected()
    }

    fn extension_info(&self) -> Option<ExtensionInfo> {
        let state = lock_state(&self.state);
        if state.extension_is_connected() {
            state
                .extension
                .as_ref()
                .map(|extension| extension.info.clone())
        } else {
            None
        }
    }

    fn status_snapshot(&self) -> HubStatus {
        lock_state(&self.state).snapshot()
    }

    fn extension_not_connected_message(&self) -> String {
        format!(
            "Gecko Orange extension is not connected to ws://{HUB_HOST}:{}. \
             Make sure Zen is open and the secret matches ('browser-use secret').",
            self.port()
        )
    }

    fn spawn_tracked(self: &Arc<Self>, future: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(future);
        if let Ok(mut tasks) = self.tasks.lock() {
            tasks.push(handle);
        }
    }

    /// Invoke the state callback with a tabs-omitted status snapshot. The
    /// snapshot is built after releasing the state lock (guardrail 4).
    fn emit_state(self: &Arc<Self>) {
        if self.is_stopping() {
            return;
        }
        let Some(on_state) = self.on_state.clone() else {
            return;
        };
        let snapshot = self.status_snapshot();
        let Ok(json) = serde_json::to_string(&snapshot) else {
            return;
        };

        let _callback_guard = self
            .callback_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_stopping() {
            return;
        }
        on_state(&json);
    }

    async fn status(self: &Arc<Self>, include_tabs: bool) -> HubStatus {
        let mut status = self.status_snapshot();
        if !include_tabs || !status.extension_connected {
            return status;
        }

        match self
            .send_command(
                json!({ "action": "tab_list" }),
                None,
                STATUS_TABS_TIMEOUT_MS,
                true,
            )
            .await
        {
            Ok(result) => {
                if let Some(tabs) = result.get("tabs").and_then(Value::as_array) {
                    status.tabs = Some(Value::Array(tabs.clone()));
                }
            }
            Err(error) => status.tabs_error = Some(error.message),
        }
        status
    }

    async fn wait_for_extension(&self, timeout_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            if self.is_extension_connected() {
                return true;
            }

            let notified = self.extension_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_extension_connected() {
                return true;
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self.is_extension_connected();
            }
            let _ = tokio::time::timeout(remaining, notified.as_mut()).await;
            // Loop: re-check `is_extension_connected` after every wake-up.
        }
    }

    async fn send_command(
        self: &Arc<Self>,
        cmd: Value,
        tab_id: Option<i64>,
        timeout_ms: u64,
        quiet: bool,
    ) -> Result<Value, HubError> {
        if !cmd.is_object() {
            return Err(HubError::new("Command must be an object"));
        }

        if !self.is_extension_connected() {
            self.wait_for_extension(EXTENSION_RECONNECT_WAIT_MS).await;
        }

        let timeout = if timeout_ms == 0 {
            DEFAULT_COMMAND_TIMEOUT_MS
        } else {
            timeout_ms
        };
        let action = command_label(&cmd);
        let tab_label = tab_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "auto".to_string());

        // Prepare the pending entry under the state lock, then release it
        // before touching any other helper that may read the state again
        // (`std::sync::Mutex` is not re-entrant).
        let prepared = {
            let mut state = lock_state(&self.state);
            let sender = match state.extension.as_ref() {
                Some(extension) if !extension.tx.is_closed() => Some(extension.tx.clone()),
                _ => None,
            };
            sender.map(|sender| {
                state.request_seq += 1;
                let id = format!("r{}", state.request_seq);
                let (tx, rx) = oneshot::channel();
                state.pending.insert(
                    id.clone(),
                    PendingCommand {
                        action: action.clone(),
                        quiet,
                        started_at: Instant::now(),
                        tx,
                    },
                );
                (id, sender, rx)
            })
        };
        let Some((id, sender, receiver)) = prepared else {
            return Err(HubError::new(self.extension_not_connected_message()));
        };

        let payload = json!({
            "type": "cmd",
            "id": id,
            "tabId": tab_id,
            "cmd": cmd,
        });
        if sender.send(Message::text(payload.to_string())).is_err() {
            lock_state(&self.state).pending.remove(&id);
            return Err(HubError::new(self.extension_not_connected_message()));
        }

        if !quiet {
            self.log(&format!("cmd {action} tab={tab_label} id={id}"));
        }

        match tokio::time::timeout(Duration::from_millis(timeout), receiver).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => {
                lock_state(&self.state).pending.remove(&id);
                Err(HubError::new(self.extension_not_connected_message()))
            }
            Err(_) => {
                lock_state(&self.state).pending.remove(&id);
                if !quiet {
                    self.log(&format!(
                        "cmd {action} tab={tab_label} id={id} TIMEOUT {timeout}ms"
                    ));
                }
                Err(HubError::new(format!(
                    "Browser command timed out after {timeout}ms ({action})"
                )))
            }
        }
    }

    async fn stop(self: &Arc<Self>) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }

        let _ = self.shutdown.send(true);

        // Any in-flight state callback finishes before this returns; every
        // later callback attempt observes `stopping` and bails out.
        {
            let _callback_guard = self
                .callback_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }

        let (extension, peers, failed) = {
            let mut state = lock_state(&self.state);
            let extension = state.extension.take();
            let peers: Vec<mpsc::UnboundedSender<Message>> =
                state.peers.drain().map(|(_, peer)| peer.tx).collect();
            let failed: Vec<oneshot::Sender<Result<Value, HubError>>> = state
                .pending
                .drain()
                .map(|(_, pending)| pending.tx)
                .collect();
            (extension, peers, failed)
        };

        for waiter in failed {
            let _ = waiter.send(Err(HubError::new("Hub stopped")));
        }
        if let Some(extension) = extension {
            let _ = extension.tx.send(close_message(1001, "Hub shutting down"));
        }
        for peer in peers {
            let _ = peer.send(close_message(1001, "Hub shutting down"));
        }

        // Give the connection tasks a moment to flush their close frames,
        // then abort everything and wait for the tasks to actually stop.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let handles: Vec<tokio::task::JoinHandle<()>> = self
            .tasks
            .lock()
            .map(|mut tasks| std::mem::take(&mut *tasks))
            .unwrap_or_default();
        for handle in &handles {
            handle.abort();
        }
        for handle in handles {
            let _ = handle.await;
        }

        self.log("hub stopped");
    }
}

// ── Public Hub wrapper ───────────────────────────────────────────────────

/// The in-process hub.
pub struct Hub {
    shared: Arc<HubShared>,
}

impl Hub {
    pub fn new(options: HubOptions) -> Self {
        Self {
            shared: HubShared::new(options),
        }
    }

    /// Bind the hub on `127.0.0.1:<port>` and start serving. Returns the bound
    /// port. Fails with an address-in-use kind of error when the port is taken.
    pub async fn start(&self) -> Result<u16, HubError> {
        if self.shared.started.load(Ordering::SeqCst) {
            return Ok(self.shared.port());
        }

        let listener = match TcpListener::bind((HUB_HOST, self.shared.requested_port)).await {
            Ok(listener) => listener,
            Err(error) => {
                let message = format!(
                    "Unable to bind hub on {HUB_HOST}:{}: {error}",
                    self.shared.requested_port
                );
                self.shared.log(&message);
                return Err(HubError::new(message));
            }
        };
        let port = listener
            .local_addr()
            .map(|address| address.port())
            .map_err(|error| HubError::new(format!("Unable to inspect hub socket: {error}")))?;

        {
            let mut state = lock_state(&self.shared.state);
            state.port = port;
            state.started_at = Some(Instant::now());
        }
        self.shared.started.store(true, Ordering::SeqCst);
        self.shared
            .log(&format!("hub listening on ws://{HUB_HOST}:{port}"));

        let accept_shared = Arc::clone(&self.shared);
        self.shared.spawn_tracked(async move {
            accept_loop(listener, accept_shared).await;
        });

        let heartbeat_shared = Arc::clone(&self.shared);
        self.shared.spawn_tracked(async move {
            heartbeat_loop(heartbeat_shared).await;
        });

        Ok(port)
    }

    /// The bound port (valid after `start`).
    pub fn port(&self) -> u16 {
        self.shared.port()
    }

    /// Send one command to the extension. `tab_id == None` sends JSON null.
    /// `timeout_ms == 0` uses `DEFAULT_COMMAND_TIMEOUT_MS`. When `quiet` is
    /// true the per-command log line is suppressed.
    pub async fn send(
        &self,
        cmd: Value,
        tab_id: Option<i64>,
        timeout_ms: u64,
        quiet: bool,
    ) -> Result<Value, HubError> {
        self.shared
            .send_command(cmd, tab_id, timeout_ms, quiet)
            .await
    }

    /// Current status. When `include_tabs` is true and the extension is
    /// connected, issues a quiet `tab_list` command with a 4s timeout.
    pub async fn status(&self, include_tabs: bool) -> HubStatus {
        self.shared.status(include_tabs).await
    }

    /// Snapshot without tabs (safe to compute without holding locks).
    pub fn status_snapshot(&self) -> HubStatus {
        self.shared.status_snapshot()
    }

    /// The actionable "extension is not connected" message used by `send`.
    pub fn extension_not_connected_message(&self) -> String {
        self.shared.extension_not_connected_message()
    }

    pub fn is_extension_connected(&self) -> bool {
        self.shared.is_extension_connected()
    }

    pub fn extension_info(&self) -> Option<ExtensionInfo> {
        self.shared.extension_info()
    }

    /// Stop accepting connections, fail in-flight commands, and stop internal
    /// tasks. No state callback runs after this returns.
    pub async fn stop(&self) {
        self.shared.stop().await;
    }
}

// ── TCP / HTTP server ────────────────────────────────────────────────────

async fn accept_loop(listener: TcpListener, shared: Arc<HubShared>) {
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        let _ = stream.set_nodelay(true);
                        let connection_shared = Arc::clone(&shared);
                        shared.spawn_tracked(async move {
                            serve_http_connection(stream, connection_shared).await;
                        });
                    }
                    Err(error) => {
                        if !shared.is_stopping() {
                            shared.log(&format!("http accept error: {error}"));
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        }
    }
}

async fn serve_http_connection(stream: TcpStream, shared: Arc<HubShared>) {
    let remote_address = stream
        .peer_addr()
        .ok()
        .map(|address| address.ip().to_string());
    let io = TokioIo::new(stream);
    let service_shared = Arc::clone(&shared);
    let service = service_fn(move |request: Request<Incoming>| {
        let request_shared = Arc::clone(&service_shared);
        let remote_address = remote_address.clone();
        async move {
            let response = route_request(request, request_shared, remote_address);
            Ok::<Response<Full<Bytes>>, Infallible>(response)
        }
    });

    let connection = http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades();
    if let Err(error) = connection.await {
        if !shared.is_stopping() {
            shared.log(&format!("http connection error: {error}"));
        }
    }
}

fn route_request(
    request: Request<Incoming>,
    shared: Arc<HubShared>,
    remote_address: Option<String>,
) -> Response<Full<Bytes>> {
    if is_websocket_upgrade(&request) {
        return route_upgrade(request, &shared, remote_address);
    }

    let path = request.uri().path().to_string();
    match (request.method().clone(), path.as_str()) {
        (Method::GET, "/health") | (Method::GET, "/status") => {
            json_response(StatusCode::OK, health_json(&shared))
        }
        (Method::GET, "/") => {
            let port = shared.port();
            let body = format!(
                "browser-use hub\n\
                 ws://{HUB_HOST}:{port}/      extension\n\
                 ws://{HUB_HOST}:{port}/peer  MCP/CLI peers\n"
            );
            text_response(StatusCode::OK, &body)
        }
        _ => text_response(StatusCode::NOT_FOUND, "Not found\n"),
    }
}

fn route_upgrade(
    request: Request<Incoming>,
    shared: &Arc<HubShared>,
    remote_address: Option<String>,
) -> Response<Full<Bytes>> {
    let path = request.uri().path().to_string();
    let kind = match path.as_str() {
        "/" => UpgradeKind::Extension,
        "/peer" => UpgradeKind::Peer,
        _ => return text_response(StatusCode::NOT_FOUND, "Not Found\n"),
    };

    let origin = header_str(&request, "origin").trim().to_string();
    match kind {
        UpgradeKind::Extension => {
            if !is_extension_origin(&origin) {
                shared.log(&format!(
                    "rejected extension upgrade origin={}",
                    if origin.is_empty() { "(none)" } else { &origin }
                ));
                return text_response(StatusCode::FORBIDDEN, "Forbidden\n");
            }
        }
        UpgradeKind::Peer => {
            if is_web_origin(&origin) {
                shared.log(&format!("rejected peer upgrade from web origin={origin}"));
                return text_response(StatusCode::FORBIDDEN, "Forbidden\n");
            }
        }
    }

    let key = header_str(&request, "sec-websocket-key").trim().to_string();
    if key.is_empty() {
        return text_response(StatusCode::BAD_REQUEST, "Bad Request\n");
    }
    let accept = derive_accept_key(key.as_bytes());

    let upgrade_shared = Arc::clone(shared);
    shared.spawn_tracked(async move {
        match hyper::upgrade::on(request).await {
            Ok(upgraded) => {
                let io = TokioIo::new(upgraded);
                let mut config = WebSocketConfig::default();
                config.max_message_size = Some(MAX_WEBSOCKET_SIZE);
                config.max_frame_size = Some(MAX_WEBSOCKET_SIZE);
                let socket = WebSocketStream::from_raw_socket(io, Role::Server, Some(config)).await;
                run_connection(upgrade_shared, socket, kind, remote_address).await;
            }
            Err(error) => {
                upgrade_shared.log(&format!("websocket upgrade failed: {error}"));
            }
        }
    });

    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "upgrade")
        .header(UPGRADE, "websocket")
        .header(SEC_WEBSOCKET_ACCEPT, accept)
        .body(Full::new(Bytes::new()))
        .unwrap_or_else(|_| {
            text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error\n")
        })
}

fn is_websocket_upgrade(request: &Request<Incoming>) -> bool {
    if !request.headers().contains_key("sec-websocket-key") {
        return false;
    }
    let connection = header_str(request, "connection").to_ascii_lowercase();
    let upgrade = header_str(request, "upgrade").to_ascii_lowercase();
    upgrade.contains("websocket") || connection.split(',').any(|token| token.trim() == "upgrade")
}

fn header_str<'a>(request: &'a Request<Incoming>, name: &str) -> &'a str {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

fn health_json(shared: &Arc<HubShared>) -> String {
    let state = lock_state(&shared.state);
    let connected = state.extension_is_connected();
    let peers = state.peers.len();
    let port = state.port;
    let uptime = state.uptime_ms();
    drop(state);
    format!(
        "{{\"ok\":true,\"service\":\"{}\",\"mode\":\"hub\",\"port\":{port},\
         \"pid\":{},\"protocol\":\"{}\",\"uptimeMs\":{uptime},\
         \"extensionConnected\":{connected},\"peers\":{peers}}}",
        crate::SERVER_NAME,
        std::process::id(),
        PROTOCOL_VERSION,
    )
}

fn json_response(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(Full::new(Bytes::from(body)))
        .unwrap_or_else(|_| {
            text_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error\n")
        })
}

fn text_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .header(CONNECTION, "close")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap_or_else(|_| {
            Response::new(Full::new(Bytes::from_static(b"Internal Server Error\n")))
        })
}

// ── WebSocket connection lifecycle ───────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeKind {
    Extension,
    Peer,
}

type ConnectionSocket = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;
type ConnectionSink = SplitSink<ConnectionSocket, Message>;
type ConnectionStream = SplitStream<ConnectionSocket>;

enum AuthOutcome {
    Accepted {
        extension_conn_id: Option<u64>,
        peer_id: Option<u64>,
    },
    Rejected,
}

async fn run_connection(
    shared: Arc<HubShared>,
    socket: ConnectionSocket,
    kind: UpgradeKind,
    remote_address: Option<String>,
) {
    let (mut sink, mut stream): (ConnectionSink, ConnectionStream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let alive = Arc::new(AtomicBool::new(true));

    let mut authenticated = false;
    let mut extension_conn_id: Option<u64> = None;
    let mut peer_id: Option<u64> = None;
    let mut close_info: Option<(u16, String)> = None;
    let mut close_deadline: Option<Instant> = None;
    let auth_deadline = Instant::now() + Duration::from_millis(AUTH_TIMEOUT_MS);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(auth_deadline)), if !authenticated => {
                match kind {
                    UpgradeKind::Extension => shared.log("extension auth timeout"),
                    UpgradeKind::Peer => shared.log("peer auth timeout"),
                }
                send_close_frame(&mut sink, 4001, "Unauthorized").await;
                break;
            }

            _ = async {
                match close_deadline {
                    Some(deadline) => tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await,
                    None => std::future::pending::<()>().await,
                }
            }, if close_deadline.is_some() => break,

            queued = out_rx.recv() => {
                match queued {
                    Some(message) => {
                        let is_close = matches!(message, Message::Close(_));
                        if sink.send(message).await.is_err() {
                            break;
                        }
                        if is_close {
                            close_deadline = Some(Instant::now() + Duration::from_millis(CLOSE_HANDSHAKE_GRACE_MS));
                        }
                    }
                    None => break,
                }
            }

            incoming = stream.next() => {
                let message = match incoming {
                    Some(Ok(message)) => message,
                    Some(Err(_)) | None => break,
                };

                match message {
                    Message::Text(text) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else {
                            continue;
                        };

                        if !authenticated {
                            match authenticate(
                                &shared,
                                kind,
                                &value,
                                &out_tx,
                                &alive,
                                remote_address.as_deref(),
                            )
                            .await {
                                AuthOutcome::Accepted { extension_conn_id: extension, peer_id: peer } => {
                                    authenticated = true;
                                    extension_conn_id = extension;
                                    peer_id = peer;
                                }
                                AuthOutcome::Rejected => {
                                    send_close_frame(&mut sink, 4001, "Unauthorized").await;
                                    break;
                                }
                            }
                            continue;
                        }

                        match kind {
                            UpgradeKind::Extension => {
                                handle_extension_message(
                                    &shared,
                                    extension_conn_id.unwrap_or(0),
                                    &value,
                                    &out_tx,
                                    &alive,
                                );
                            }
                            UpgradeKind::Peer => {
                                handle_peer_message(&shared, &value, &out_tx);
                            }
                        }
                    }
                    Message::Close(frame) => {
                        close_info = frame.as_ref().map(|frame| {
                            (u16::from(frame.code), frame.reason.to_string())
                        });
                        let _ = sink.close().await;
                        break;
                    }
                    Message::Pong(_) => {
                        if kind == UpgradeKind::Extension && authenticated {
                            alive.store(true, Ordering::SeqCst);
                        }
                    }
                    Message::Ping(_) | Message::Binary(_) | Message::Frame(_) => {}
                }
            }
        }
    }

    drop(sink);
    drop(stream);
    drop(out_rx);

    match kind {
        UpgradeKind::Extension => {
            if let Some(conn_id) = extension_conn_id {
                let failed = {
                    let mut state = lock_state(&shared.state);
                    if state.extension.as_ref().map(|extension| extension.conn_id) == Some(conn_id)
                    {
                        state.extension = None;
                        Some(
                            state
                                .pending
                                .drain()
                                .map(|(_, pending)| pending.tx)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        None
                    }
                };
                if let Some(failed) = failed {
                    for waiter in failed {
                        let _ = waiter
                            .send(Err(HubError::new("Extension disconnected before replying")));
                    }
                    let (code, reason) = close_info.unwrap_or((1006, String::new()));
                    shared.log(&format!(
                        "extension disconnected code={code} reason={reason}"
                    ));
                    shared.emit_state();
                }
            }
        }
        UpgradeKind::Peer => {
            if let Some(id) = peer_id {
                let removed = {
                    let mut state = lock_state(&shared.state);
                    state.peers.remove(&id).is_some()
                };
                if removed {
                    let (code, reason) = close_info.unwrap_or((1006, String::new()));
                    shared.log(&format!(
                        "peer disconnected id={id} code={code} reason={reason}"
                    ));
                    shared.emit_state();
                }
            }
        }
    }
}

async fn authenticate(
    shared: &Arc<HubShared>,
    kind: UpgradeKind,
    value: &Value,
    out_tx: &mpsc::UnboundedSender<Message>,
    alive: &Arc<AtomicBool>,
    remote_address: Option<&str>,
) -> AuthOutcome {
    let Some(hello) = value.as_object() else {
        return AuthOutcome::Rejected;
    };
    let is_hello = hello.get("type").and_then(Value::as_str) == Some("hello");
    let received = hello.get("secret").and_then(Value::as_str).unwrap_or("");
    if !is_hello || !secrets_match(&shared.secret, received) {
        match kind {
            UpgradeKind::Extension => shared.log("extension auth rejected"),
            UpgradeKind::Peer => shared.log("peer auth rejected"),
        }
        return AuthOutcome::Rejected;
    }

    match kind {
        UpgradeKind::Extension => {
            let conn_id = register_extension(
                shared,
                out_tx.clone(),
                Arc::clone(alive),
                hello,
                remote_address,
            );
            let port = shared.port();
            send_json_via(
                out_tx,
                &json!({
                    "type": "hello_ok",
                    "hub": crate::SERVER_NAME,
                    "protocol": PROTOCOL_VERSION,
                    "port": port,
                }),
            );
            if let Some(info) = shared.extension_info() {
                shared.log(&format!(
                    "extension connected client={} version={}",
                    info.browser.as_deref().unwrap_or("unknown"),
                    info.version.as_deref().unwrap_or("unknown"),
                ));
            }
            shared.extension_notify.notify_waiters();
            shared.emit_state();
            AuthOutcome::Accepted {
                extension_conn_id: Some(conn_id),
                peer_id: None,
            }
        }
        UpgradeKind::Peer => {
            let id = register_peer(shared, out_tx.clone());
            let port = shared.port();
            send_json_via(
                out_tx,
                &json!({
                    "type": "hello_ok",
                    "hub": crate::SERVER_NAME,
                    "mode": "hub",
                    "protocol": PROTOCOL_VERSION,
                    "port": port,
                }),
            );
            shared.log(&format!(
                "peer connected id={id} client={} version={}",
                hello_string(hello, "client").unwrap_or_else(|| "browser-use".to_string()),
                hello_string(hello, "version").unwrap_or_else(|| "unknown".to_string()),
            ));
            shared.emit_state();
            AuthOutcome::Accepted {
                extension_conn_id: None,
                peer_id: Some(id),
            }
        }
    }
}

fn register_extension(
    shared: &Arc<HubShared>,
    tx: mpsc::UnboundedSender<Message>,
    alive: Arc<AtomicBool>,
    hello: &Map<String, Value>,
    remote_address: Option<&str>,
) -> u64 {
    let client = hello_string(hello, "client");
    let info = ExtensionInfo {
        version: hello_string(hello, "version"),
        browser: client.clone(),
        protocol: Some(
            hello_string(hello, "protocol").unwrap_or_else(|| PROTOCOL_VERSION.to_string()),
        ),
        connected_at_ms: Some(now_ms()),
        client,
        user_agent: hello_string(hello, "userAgent"),
        remote_address: remote_address.map(|address| address.to_string()),
    };

    let (conn_id, previous, failed) = {
        let mut state = lock_state(&shared.state);
        state.extension_seq += 1;
        let conn_id = state.extension_seq;
        let previous = state.extension.replace(ExtensionState {
            conn_id,
            tx,
            alive,
            info,
        });
        let failed: Vec<oneshot::Sender<Result<Value, HubError>>> = state
            .pending
            .drain()
            .map(|(_, pending)| pending.tx)
            .collect();
        (conn_id, previous, failed)
    };

    if let Some(previous) = previous {
        let _ = previous.tx.send(close_message(
            4002,
            "Replaced by a newer extension connection",
        ));
    }
    for waiter in failed {
        let _ = waiter.send(Err(HubError::new("Extension reconnected before replying")));
    }

    conn_id
}

fn register_peer(shared: &Arc<HubShared>, tx: mpsc::UnboundedSender<Message>) -> u64 {
    let mut state = lock_state(&shared.state);
    state.peer_seq += 1;
    let id = state.peer_seq;
    state.peers.insert(id, PeerState { tx });
    id
}

fn handle_extension_message(
    shared: &Arc<HubShared>,
    conn_id: u64,
    value: &Value,
    out_tx: &mpsc::UnboundedSender<Message>,
    alive: &Arc<AtomicBool>,
) {
    let Some(message) = value.as_object() else {
        return;
    };

    let is_current = {
        let state = lock_state(&shared.state);
        state.extension.as_ref().map(|extension| extension.conn_id) == Some(conn_id)
    };
    if !is_current {
        return;
    }

    match message.get("type").and_then(Value::as_str).unwrap_or("") {
        "res" => {
            let id = message.get("id").map(id_string).unwrap_or_default();
            let pending = lock_state(&shared.state).pending.remove(&id);
            let Some(pending) = pending else {
                return;
            };
            let elapsed = pending.started_at.elapsed().as_millis();

            if message.get("ok") == Some(&Value::Bool(false)) {
                let text = string_value(message.get("error"), "Browser command failed");
                if !pending.quiet {
                    shared.log(&format!(
                        "cmd {} id={id} ERROR {elapsed}ms: {text}",
                        pending.action
                    ));
                }
                let _ = pending.tx.send(Err(HubError::new(text)));
            } else {
                if !pending.quiet {
                    shared.log(&format!("cmd {} id={id} ok {elapsed}ms", pending.action));
                }
                let result = message.get("result").cloned().unwrap_or(Value::Null);
                let _ = pending.tx.send(Ok(result));
            }
        }
        "pong" => {
            alive.store(true, Ordering::SeqCst);
        }
        "ping" => {
            send_json_via(out_tx, &json!({ "type": "pong", "ts": now_ms() }));
        }
        "hello_ok" | "event" => {}
        _ => {}
    }
}

fn handle_peer_message(
    shared: &Arc<HubShared>,
    value: &Value,
    out_tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(message) = value.as_object() else {
        return;
    };

    match message.get("type").and_then(Value::as_str).unwrap_or("") {
        "cmd" => {
            let request_id = message.get("id").cloned().unwrap_or(Value::Null);
            let Some(cmd) = message.get("cmd").filter(|cmd| cmd.is_object()).cloned() else {
                send_json_via(
                    out_tx,
                    &json!({
                        "type": "res",
                        "id": request_id,
                        "ok": false,
                        "error": "cmd must be an object",
                    }),
                );
                return;
            };
            let tab_id = number_or_null(message.get("tabId"));
            let timeout = number_or_default(
                message.get("timeoutMs"),
                DEFAULT_COMMAND_TIMEOUT_MS,
                MAX_COMMAND_TIMEOUT_MS,
            );

            let command_shared = Arc::clone(shared);
            let command_tx = out_tx.clone();
            shared.spawn_tracked(async move {
                let reply = match command_shared
                    .send_command(cmd, tab_id, timeout, false)
                    .await
                {
                    Ok(result) => json!({
                        "type": "res",
                        "id": request_id,
                        "ok": true,
                        "result": result,
                    }),
                    Err(error) => json!({
                        "type": "res",
                        "id": request_id,
                        "ok": false,
                        "error": error.message,
                    }),
                };
                let _ = command_tx.send(Message::text(reply.to_string()));
            });
        }
        "status" => {
            let request_id = message.get("id").cloned().unwrap_or(Value::Null);
            let include_tabs = message.get("tabs") != Some(&Value::Bool(false));

            let status_shared = Arc::clone(shared);
            let status_tx = out_tx.clone();
            shared.spawn_tracked(async move {
                let status = status_shared.status(include_tabs).await;
                let mut value = serde_json::to_value(&status).unwrap_or_else(|_| json!({}));
                if let Some(object) = value.as_object_mut() {
                    object.insert("type".to_string(), json!("status_res"));
                    object.insert("id".to_string(), request_id);
                    object.insert("hub".to_string(), json!(true));
                }
                let _ = status_tx.send(Message::text(value.to_string()));
            });
        }
        "ping" => {
            send_json_via(out_tx, &json!({ "type": "pong", "ts": now_ms() }));
        }
        "hello" | "hello_ok" | "pong" => {}
        other => {
            if let Some(id) = message.get("id") {
                send_json_via(
                    out_tx,
                    &json!({
                        "type": "res",
                        "id": id.clone(),
                        "ok": false,
                        "error": format!("Unknown peer message type: {other}"),
                    }),
                );
            }
        }
    }
}

async fn heartbeat_loop(shared: Arc<HubShared>) {
    let mut interval = tokio::time::interval(Duration::from_millis(HEARTBEAT_INTERVAL_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await; // the first tick completes immediately
    let mut shutdown = shared.shutdown.subscribe();

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = interval.tick() => {
                let (ping_tx, dropped) = {
                    let mut state = lock_state(&shared.state);
                    match state.extension.as_mut() {
                        Some(extension) => {
                            if !extension.alive.swap(false, Ordering::SeqCst) {
                                let dropped = state.extension.take();
                                let failed: Vec<oneshot::Sender<Result<Value, HubError>>> =
                                    state.pending.drain().map(|(_, pending)| pending.tx).collect();
                                (None, Some((dropped, failed)))
                            } else {
                                (Some(extension.tx.clone()), None)
                            }
                        }
                        None => (None, None),
                    }
                };

                if let Some((dropped, failed)) = dropped {
                    shared.log("extension heartbeat missed; dropping socket");
                    if let Some(extension) = dropped {
                        let _ = extension
                            .tx
                            .send(close_message(1001, "Heartbeat timeout"));
                    }
                    for waiter in failed {
                        let _ = waiter.send(Err(HubError::new(
                            "Extension disconnected before replying",
                        )));
                    }
                    shared.emit_state();
                } else if let Some(tx) = ping_tx {
                    let ping = json!({ "type": "ping", "ts": now_ms() });
                    let _ = tx.send(Message::text(ping.to_string()));
                }
            }
        }
    }
}

// ── Wire helpers ─────────────────────────────────────────────────────────

fn close_message(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code: CloseCode::from(code),
        reason: Utf8Bytes::from(reason.to_string()),
    }))
}

async fn send_close_frame(sink: &mut ConnectionSink, code: u16, reason: &str) {
    let _ = sink.send(close_message(code, reason)).await;
    let _ = sink.flush().await;
}

fn send_json_via(tx: &mpsc::UnboundedSender<Message>, payload: &Value) -> bool {
    tx.send(Message::text(payload.to_string())).is_ok()
}

fn hello_string(hello: &Map<String, Value>, key: &str) -> Option<String> {
    hello
        .get(key)
        .and_then(Value::as_str)
        .map(|value| safe_string(value))
        .filter(|value| !value.is_empty())
}

/// Legacy `safeString`: single line, capped at 200 UTF-16 code units.
fn safe_string(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len().min(200));
    for character in value.chars() {
        let character = match character {
            '\r' | '\n' | '\t' => ' ',
            other => other,
        };
        if cleaned.encode_utf16().count() >= 200 {
            break;
        }
        cleaned.push(character);
    }
    cleaned
}

fn command_label(cmd: &Value) -> String {
    cmd.get("action")
        .and_then(Value::as_str)
        .map(|action| safe_string(action))
        .filter(|action| !action.is_empty())
        .unwrap_or_else(|| "cmd".to_string())
}

fn id_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        other => other.to_string(),
    }
}

fn string_value(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(Value::String(text)) if !text.is_empty() => text.clone(),
        _ => fallback.to_string(),
    }
}

/// Legacy `numberOrNull`.
fn number_or_null(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Null => None,
        Value::String(text) if text.is_empty() => None,
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|n| n.trunc() as i64)),
        Value::String(text) => text.parse::<f64>().ok().map(|n| n.trunc() as i64),
        _ => None,
    }
}

/// Legacy `numberOrDefault`: finite and positive, capped at `max`.
fn number_or_default(value: Option<&Value>, fallback: u64, max: u64) -> u64 {
    let parsed = match value {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.parse::<f64>().ok(),
        Some(Value::Bool(flag)) => Some(if *flag { 1.0 } else { 0.0 }),
        _ => None,
    };
    match parsed {
        Some(number) if number.is_finite() && number > 0.0 => (number.floor() as u64).min(max),
        _ => fallback,
    }
}

fn is_extension_origin(origin: &str) -> bool {
    let lower = origin.to_ascii_lowercase();
    lower.starts_with("moz-extension://") || lower.starts_with("chrome-extension://")
}

fn is_web_origin(origin: &str) -> bool {
    let lower = origin.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Resolve the hub port from an optional raw `ORANGE_PORT`-style string.
///
/// Invalid values fall back to `DEFAULT_PORT` after a stderr warning; use
/// [`try_resolve_port`] when a hard error is required (the CLI).
pub fn resolve_port(raw: Option<&str>) -> u16 {
    match try_resolve_port(raw) {
        Ok(port) => port,
        Err(message) => {
            eprintln!("{message}");
            DEFAULT_PORT
        }
    }
}

/// Fallible [`resolve_port`]: validates `ORANGE_PORT` exactly like the legacy
/// hub (`1..=65535` integer).
pub fn try_resolve_port(raw: Option<&str>) -> Result<u16, String> {
    let raw = match raw {
        Some(value) => value.to_string(),
        None => std::env::var("ORANGE_PORT").unwrap_or_default(),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(DEFAULT_PORT);
    }
    match trimmed.parse::<u32>() {
        Ok(port) if (1..=65_535).contains(&port) => Ok(port as u16),
        _ => Err(format!(
            "ORANGE_PORT must be an integer between 1 and 65535 (got \"{raw}\")"
        )),
    }
}
