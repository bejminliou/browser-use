//! Integration tests for the C-ABI in `rust/src/ffi.rs`.
//!
//! These exercise the real `#[no_mangle] extern "C"` functions (not the safe
//! Rust wrapper): start a hub through `bu_hub_start`, hit `/health` over a raw
//! `std::net::TcpStream`, connect a [`FakeExtension`], observe the `on_change`
//! state callbacks, round-trip `bu_hub_status_json` / `bu_hub_command_json` /
//! `bu_secret_info_json`, then verify `bu_hub_stop` tears everything down.
//!
//! The machine's legacy hub owns port 19455; every test here uses ephemeral
//! ports from [`common::free_port`]. `ORANGE_SECRET` is set for the test
//! process so the FFI never creates/touches `~/.orange-mcp/secret`.

mod common;

use std::ffi::{c_char, c_void, CStr, CString};
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use browser_use::ffi::{
    bu_free_string, bu_hub_command_json, bu_hub_start, bu_hub_status_json, bu_hub_stop,
    bu_secret_info_json, BuHub,
};
use browser_use::hub::PROTOCOL_VERSION;
use common::{free_port, FakeExtension, TEST_SECRET};

/// Upper bound for asynchronous waits (state callbacks, extension replies).
const WAIT: Duration = Duration::from_secs(5);

static SECRET_ENV: Once = Once::new();

/// `std::env` mutation is process-global; serialize the two tests in this
/// binary so one test's `ORANGE_PORT` tweak can never race another test's
/// `ORANGE_SECRET` lookup (the FFI reads the environment in-process).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// The FFI reads the process environment; supply the shared test secret once so
/// no test ever falls back to creating the user's real `~/.orange-mcp/secret`.
fn ensure_test_secret() {
    SECRET_ENV.call_once(|| {
        std::env::set_var("ORANGE_SECRET", TEST_SECRET);
    });
}

// ── C-string helpers ─────────────────────────────────────────────────────

/// Copy a `bu_*` string result and free it with `bu_free_string`.
fn take_cstring(ptr: *mut c_char) -> String {
    assert!(!ptr.is_null(), "expected a non-null C string");
    // SAFETY: every non-null pointer handed here came from `CString::into_raw`
    // inside `ffi.rs` and is freed exactly once here.
    let text = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    bu_free_string(ptr);
    text
}

/// Consume the `(result, err)` pair of a successful `bu_*` call, panicking with
/// the FFI error message otherwise.
fn take_result(result: *mut c_char, err: *mut c_char) -> String {
    if !err.is_null() {
        let message = take_cstring(err);
        if !result.is_null() {
            bu_free_string(result);
        }
        panic!("FFI call failed unexpectedly: {message}");
    }
    assert!(
        !result.is_null(),
        "FFI call returned NULL without setting err_out"
    );
    take_cstring(result)
}

/// Consume the `(result, err)` pair of a call that must fail; returns the
/// `*err_out` message and asserts the result pointer was NULL.
fn take_failure(result: *mut c_char, err: *mut c_char) -> String {
    assert!(
        result.is_null(),
        "expected the FFI call to return NULL on failure"
    );
    assert!(
        !err.is_null(),
        "expected the FFI call to set a non-null *err_out on failure"
    );
    take_cstring(err)
}

/// Consume the `(handle, err)` pair of a `bu_hub_start` call that must fail.
fn take_start_failure(handle: *mut BuHub, err: *mut c_char) -> String {
    assert!(
        handle.is_null(),
        "expected bu_hub_start to return NULL on failure"
    );
    assert!(
        !err.is_null(),
        "expected bu_hub_start to set a non-null *err_out on failure"
    );
    take_cstring(err)
}

fn assert_no_error(err: *mut c_char) {
    if !err.is_null() {
        panic!("unexpected FFI error: {}", take_cstring(err));
    }
}

// ── State callback sink ──────────────────────────────────────────────────

#[derive(Default)]
struct CallbackSink {
    snapshots: Mutex<Vec<Value>>,
    raw_json: Mutex<Vec<String>>,
}

impl CallbackSink {
    fn count(&self) -> usize {
        self.snapshots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Wait (up to `timeout`) for a state snapshot matching `predicate`.
    fn wait_for(&self, timeout: Duration, predicate: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let snapshots = self
                    .snapshots
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(found) = snapshots.iter().find(|snapshot| predicate(snapshot)) {
                    return Some(found.clone());
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait until the *latest* snapshot matches `predicate` (no stale matches).
    fn wait_for_latest(
        &self,
        timeout: Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let snapshots = self
                    .snapshots
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(last) = snapshots.last() {
                    if predicate(last) {
                        return Some(last.clone());
                    }
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The C `BuStateCallback` used by the test: copies the NUL-terminated JSON and
/// records the parsed value.
extern "C" fn capture_state(status_json: *const c_char, ctx: *mut c_void) {
    if status_json.is_null() || ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `Arc<CallbackSink>` raw pointer created below and
    // stays alive until after `bu_hub_stop` returns.
    let sink = unsafe { &*(ctx as *const CallbackSink) };
    let text = unsafe { CStr::from_ptr(status_json) }
        .to_string_lossy()
        .into_owned();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    if let Ok(mut snapshots) = sink.snapshots.lock() {
        snapshots.push(value);
    }
    if let Ok(mut raw) = sink.raw_json.lock() {
        raw.push(text);
    }
}

// ── Raw HTTP helpers (std::net, exactly like a native client) ────────────

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .unwrap_or_else(|error| panic!("connect to hub 127.0.0.1:{port}: {error}"));
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .expect("write raw HTTP request");
    let mut buffer = Vec::new();
    stream
        .read_to_end(&mut buffer)
        .expect("read raw HTTP response");
    String::from_utf8_lossy(&buffer).into_owned()
}

fn http_json(port: u16, path: &str) -> Value {
    let response = http_get(port, path);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "unexpected response for {path}: {response}"
    );
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_else(|| panic!("no HTTP body in response for {path}: {response}"));
    serde_json::from_str::<Value>(body)
        .unwrap_or_else(|error| panic!("invalid JSON from {path}: {error}\n{response}"))
}

fn parse_json(label: &str, text: &str) -> Value {
    serde_json::from_str::<Value>(text)
        .unwrap_or_else(|error| panic!("invalid JSON from {label}: {error}\n{text}"))
}

// ── Main round-trip test ─────────────────────────────────────────────────

#[test]
fn ffi_c_abi_round_trip() {
    let _env_guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    ensure_test_secret();

    // A log path whose parent directories do not exist yet: the FFI must create
    // them and append one `\n`-terminated line per hub log entry.
    let log_dir = tempfile::tempdir().expect("create temp dir");
    let log_path = log_dir.path().join("logs").join("hub.log");
    let log_path_c = CString::new(log_path.to_string_lossy().as_bytes()).expect("log path CString");

    let sink = Arc::new(CallbackSink::default());
    let ctx = Arc::into_raw(Arc::clone(&sink)) as *mut c_void;

    let port = free_port();
    let mut err: *mut c_char = std::ptr::null_mut();
    let handle: *mut BuHub = bu_hub_start(
        port,
        log_path_c.as_ptr(),
        Some(capture_state),
        ctx,
        &mut err,
    );
    assert_no_error(err);
    assert!(!handle.is_null(), "bu_hub_start returned NULL");

    // ── /health + /status over raw HTTP, before any extension is connected ──
    let health = http_json(port, "/health");
    assert_eq!(health["ok"], json!(true));
    assert_eq!(health["service"], json!("browser-use"));
    assert_eq!(health["mode"], json!("hub"));
    assert_eq!(health["protocol"], json!(PROTOCOL_VERSION));
    assert_eq!(
        health["protocol"].as_str(),
        Some("3.0.0"),
        "the wire protocol version must stay 3.0.0"
    );
    assert_eq!(health["port"], json!(port));
    assert_eq!(health["extensionConnected"], json!(false));
    assert_eq!(health["peers"], json!(0));
    assert!(health["pid"].as_u64().unwrap_or(0) > 0);

    let http_status = http_json(port, "/status");
    assert_eq!(http_status["ok"], json!(true));
    assert_eq!(http_status["extensionConnected"], json!(false));

    // Starting the hub does not emit a state callback by itself.
    assert_eq!(
        sink.count(),
        0,
        "bu_hub_start emitted an unexpected state callback"
    );

    // ── bu_hub_status_json(include_tabs = false) ──
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_status_json(handle, false, &mut err);
    let status = parse_json("bu_hub_status_json", &take_result(ptr, err));
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["service"], json!("browser-use"));
    assert_eq!(status["protocol"], json!(PROTOCOL_VERSION));
    assert_eq!(status["port"], json!(port));
    assert_eq!(status["extensionConnected"], json!(false));
    assert_eq!(status["peers"], json!(0));
    assert!(
        status.get("tabs").is_none(),
        "tabs must be omitted when include_tabs=false"
    );
    assert!(
        status.get("tabsError").is_none(),
        "tabsError must be omitted when include_tabs=false"
    );

    // ── Connect a fake extension; the on_change callback must fire ──
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build test Tokio runtime");
    let mut extension = runtime
        .block_on(FakeExtension::connect(port))
        .expect("fake extension failed to connect through the FFI-started hub");
    assert_eq!(extension.hello_ok()["hub"], json!("browser-use"));
    assert_eq!(extension.hello_ok()["protocol"], json!(PROTOCOL_VERSION));

    let connected = sink
        .wait_for(WAIT, |snapshot| {
            snapshot["extensionConnected"] == json!(true)
        })
        .expect("on_change never reported extensionConnected=true");
    assert!(
        connected.get("tabs").is_none(),
        "on_change snapshots must omit tabs"
    );
    assert_eq!(connected["service"], json!("browser-use"));
    assert_eq!(connected["protocol"], json!(PROTOCOL_VERSION));
    assert_eq!(connected["peers"], json!(0));
    assert_eq!(connected["extension"]["client"], json!("gecko-orange"));

    // ── bu_hub_status_json(include_tabs = true) ──
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_status_json(handle, true, &mut err);
    let status = parse_json("bu_hub_status_json(true)", &take_result(ptr, err));
    assert_eq!(status["extensionConnected"], json!(true));
    let tabs = status["tabs"]
        .as_array()
        .unwrap_or_else(|| panic!("tabs must be an array when include_tabs=true: {status}"));
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0]["id"], json!(42));
    assert_eq!(tabs[0]["active"], json!(true));
    assert_eq!(tabs[1]["url"], json!("https://beta.example"));
    assert!(status.get("tabsError").is_none());
    // The quiet `tab_list` really went through the WebSocket.
    let tab_list = runtime.block_on(extension.next_command());
    assert_eq!(tab_list["type"], json!("cmd"));
    assert_eq!(tab_list["cmd"]["action"], json!("tab_list"));

    // ── bu_hub_command_json round-trip with an explicit tab id ──
    let cmd_json = CString::new(json!({"action": "tab_switch", "tabId": 42}).to_string())
        .expect("command JSON");
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(handle, cmd_json.as_ptr(), 42, 0, &mut err);
    let result = parse_json("bu_hub_command_json", &take_result(ptr, err));
    assert_eq!(result["ok"], json!(true));
    assert_eq!(result["action"], json!("tab_switch"));
    assert_eq!(result["tabId"], json!(42));
    let framed = runtime.block_on(extension.next_command());
    assert_eq!(framed["type"], json!("cmd"));
    assert_eq!(framed["cmd"]["action"], json!("tab_switch"));
    assert_eq!(framed["tabId"], json!(42));

    // ── tab_id < 0 becomes JSON null on the wire ──
    let cmd_json = CString::new(json!({"action": "noop"}).to_string()).expect("command JSON");
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(handle, cmd_json.as_ptr(), -1, 0, &mut err);
    let result = parse_json("bu_hub_command_json(tab_id=-1)", &take_result(ptr, err));
    assert!(result["tabId"].is_null());
    let framed = runtime.block_on(extension.next_command());
    assert!(framed["tabId"].is_null(), "tab_id < 0 must send JSON null");
    assert_eq!(framed["cmd"]["action"], json!("noop"));

    // ── extension errors surface through *err_out, not a panic ──
    extension.set_handler(|_| common::FakeReply::Err("boom from extension".to_string()));
    let cmd_json = CString::new(json!({"action": "explode"}).to_string()).expect("command JSON");
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(handle, cmd_json.as_ptr(), -1, 0, &mut err);
    let message = take_failure(ptr, err);
    assert!(
        message.contains("boom from extension"),
        "unexpected command error: {message}"
    );
    extension.set_handler(common::default_reply);

    // ── invalid command JSON is rejected without touching the extension ──
    let invalid = CString::new("{not valid json").expect("invalid JSON CString");
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(handle, invalid.as_ptr(), -1, 0, &mut err);
    let message = take_failure(ptr, err);
    assert!(
        message.contains("Invalid command JSON"),
        "unexpected invalid-JSON error: {message}"
    );

    // ── bu_secret_info_json sees ORANGE_SECRET (environment source) ──
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_secret_info_json(&mut err);
    let info = parse_json("bu_secret_info_json", &take_result(ptr, err));
    assert_eq!(info["secret"], json!(TEST_SECRET));
    assert_eq!(info["source"], json!("environment"));
    let expected_setup = format!(
        "await OrangeBridge.setSecret({})",
        serde_json::to_string(TEST_SECRET).expect("serialize test secret")
    );
    assert_eq!(info["setupCommand"], json!(expected_setup));

    // ── peer connect/disconnect also drives on_change (peer count) ──
    let mut peer = runtime
        .block_on(common::ws_connect(port, "/peer", None))
        .expect("peer websocket upgrade failed");
    let hello = json!({"type": "hello", "secret": TEST_SECRET});
    assert!(runtime.block_on(common::send_json(&mut peer, &hello)));
    let hello_ok = runtime
        .block_on(common::next_json(&mut peer, 2_000))
        .expect("peer never received hello_ok");
    assert_eq!(hello_ok["type"], json!("hello_ok"));
    let peer_snapshot = sink
        .wait_for(WAIT, |snapshot| snapshot["peers"].as_u64() == Some(1))
        .expect("on_change never reported peers=1");
    assert!(peer_snapshot.get("tabs").is_none());
    assert_eq!(peer_snapshot["extensionConnected"], json!(true));
    let _ = runtime.block_on(peer.close(None));
    let _ = sink
        .wait_for(WAIT, |snapshot| snapshot["peers"].as_u64() == Some(0))
        .expect("on_change never reported peers=0 after the peer disconnected");

    // ── the log file has one line per entry, newline-terminated ──
    let log_text = std::fs::read_to_string(&log_path).expect("hub log file was not created");
    assert!(
        log_text.ends_with('\n'),
        "hub log lines must be newline terminated:\n{log_text}"
    );
    assert!(
        log_text.contains(&format!("hub listening on ws://127.0.0.1:{port}")),
        "hub log is missing the listening line:\n{log_text}"
    );
    assert!(
        log_text.contains("extension connected"),
        "hub log is missing the extension line:\n{log_text}"
    );
    assert!(
        log_text.contains("cmd tab_switch"),
        "hub log is missing the command line:\n{log_text}"
    );

    // ── disconnect the extension: on_change must report it ──
    runtime.block_on(extension.close());
    let _ = sink
        .wait_for_latest(WAIT, |snapshot| {
            snapshot["extensionConnected"] == json!(false)
        })
        .expect("on_change never reported extensionConnected=false");

    // Reconnect and stop the hub while an extension is still attached: the
    // app-quit path is exactly this (bu_hub_stop closes the live socket).
    let attached = runtime
        .block_on(FakeExtension::connect(port))
        .expect("second fake extension failed to connect");
    let _ = sink
        .wait_for_latest(WAIT, |snapshot| {
            snapshot["extensionConnected"] == json!(true)
        })
        .expect("on_change never reported the reconnected extension");

    bu_hub_stop(handle);

    // No callback may run after bu_hub_stop returns.
    let callbacks_after_stop = sink.count();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        sink.count(),
        callbacks_after_stop,
        "on_change fired after bu_hub_stop returned"
    );

    // The listener must be gone.
    let address: SocketAddr = format!("127.0.0.1:{port}").parse().expect("socket address");
    let connect = TcpStream::connect_timeout(&address, Duration::from_millis(750));
    assert!(
        connect.is_err(),
        "the hub still accepts TCP connections after bu_hub_stop"
    );
    assert!(
        runtime.block_on(FakeExtension::connect(port)).is_err(),
        "a new extension connection succeeded after bu_hub_stop"
    );

    // The live socket was closed by the hub during stop; dropping the fake
    // extension aborts its client task.
    attached.abort();

    // Reclaim the callback context and shut the test runtime down.
    // SAFETY: the hub (and therefore every callback) has stopped above.
    unsafe {
        drop(Arc::from_raw(ctx as *const CallbackSink));
    }
    drop(runtime);

    // ── port == 0 resolves ORANGE_PORT (header contract) ──
    let resolved_port = free_port();
    std::env::set_var("ORANGE_PORT", resolved_port.to_string());
    let mut err: *mut c_char = std::ptr::null_mut();
    let handle = bu_hub_start(0, std::ptr::null(), None, std::ptr::null_mut(), &mut err);
    std::env::remove_var("ORANGE_PORT");
    assert_no_error(err);
    assert!(
        !handle.is_null(),
        "bu_hub_start(port=0) did not resolve ORANGE_PORT"
    );
    let status = http_json(resolved_port, "/health");
    assert_eq!(status["port"], json!(resolved_port));
    assert_eq!(status["ok"], json!(true));
    bu_hub_stop(handle);
}

// ── Error paths / NULL safety ────────────────────────────────────────────

#[test]
fn ffi_error_paths_and_null_safety() {
    let _env_guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // NULL handle -> NULL result + err_out, for every handle-taking function.
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_status_json(std::ptr::null_mut(), false, &mut err);
    let message = take_failure(ptr, err);
    assert!(
        message.to_ascii_lowercase().contains("hub"),
        "unexpected null-handle status error: {message}"
    );

    let command = CString::new(json!({"action": "tab_list"}).to_string()).expect("command JSON");
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(std::ptr::null_mut(), command.as_ptr(), -1, 0, &mut err);
    let message = take_failure(ptr, err);
    assert!(
        message.to_ascii_lowercase().contains("hub"),
        "unexpected null-handle command error: {message}"
    );

    // NULL-safe teardown helpers.
    bu_hub_stop(std::ptr::null_mut());
    bu_free_string(std::ptr::null_mut());

    // Invalid UTF-8 log path is rejected before anything binds.
    let bad_log_path = CString::new(vec![0xff, b'x']).expect("CString from bytes");
    let mut err: *mut c_char = std::ptr::null_mut();
    let handle = bu_hub_start(
        free_port(),
        bad_log_path.as_ptr(),
        None,
        std::ptr::null_mut(),
        &mut err,
    );
    let message = take_start_failure(handle, err);
    assert!(
        message.contains("Invalid UTF-8"),
        "unexpected log-path error: {message}"
    );

    // NULL command JSON through a live hub.
    ensure_test_secret();
    let port = free_port();
    let mut err: *mut c_char = std::ptr::null_mut();
    let handle = bu_hub_start(port, std::ptr::null(), None, std::ptr::null_mut(), &mut err);
    assert_no_error(err);
    assert!(!handle.is_null(), "bu_hub_start returned NULL");

    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(handle, std::ptr::null(), -1, 0, &mut err);
    let message = take_failure(ptr, err);
    assert!(
        message.to_ascii_lowercase().contains("null"),
        "unexpected null command JSON error: {message}"
    );

    let invalid = CString::new("{oops").expect("invalid JSON CString");
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_hub_command_json(handle, invalid.as_ptr(), -1, 0, &mut err);
    let message = take_failure(ptr, err);
    assert!(
        message.contains("Invalid command JSON"),
        "unexpected invalid-JSON error: {message}"
    );

    bu_hub_stop(handle);

    // Bind failure -> NULL + actionable error, nothing left running.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("occupy ephemeral port");
    let occupied = listener.local_addr().expect("listener address").port();
    let mut err: *mut c_char = std::ptr::null_mut();
    let handle = bu_hub_start(
        occupied,
        std::ptr::null(),
        None,
        std::ptr::null_mut(),
        &mut err,
    );
    let message = take_start_failure(handle, err);
    assert!(
        message.contains("Unable to bind hub"),
        "unexpected bind error: {message}"
    );
    drop(listener);

    // The secret info call also works without a running hub.
    let mut err: *mut c_char = std::ptr::null_mut();
    let ptr = bu_secret_info_json(&mut err);
    let info = parse_json("bu_secret_info_json", &take_result(ptr, err));
    assert_eq!(info["secret"], json!(TEST_SECRET));
    assert_eq!(info["source"], json!("environment"));
}
