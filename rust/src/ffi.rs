//! C-ABI implementation for the Swift menu bar app.
//!
//! Owns the Tokio runtime + `Arc<Hub>`. Every payload crosses the boundary as a
//! NUL-terminated UTF-8 JSON string. `rust/include/browser_use.h` is the
//! authoritative contract.
//!
//! Threading/lifetime invariants:
//!   * `bu_hub_start` builds a multi-thread Tokio runtime, starts the Hub on it
//!     and stores both in [`BuHub`]; it blocks the calling thread until the Hub
//!     is listening (Swift calls it off `@MainActor`).
//!   * `bu_hub_stop` runs `Hub::stop` on that runtime, then shuts the runtime
//!     down and frees the handle. Hub callbacks observe the hub's `stopping`
//!     flag, so no `on_change` invocation can begin after it returns.
//!   * `status`/`command` block on the owned runtime and MUST be called from a
//!     non-async thread (GCD background queue / `Task.detached`); a Tokio
//!     runtime cannot be driven from inside another runtime's async context.
//!
//! INVARIANT: never let a panic/unwind cross the C boundary. Every user input
//! is validated and reported through `*err_out`; panics that do happen are
//! caught and converted into an error string.

// The C ABI functions are intentionally `extern "C"` (not `unsafe fn`): their
// raw-pointer preconditions are documented in `rust/include/browser_use.h`, and
// the Swift call site cannot express Rust's `unsafe`. Each dereference is
// individually justified with a SAFETY comment.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::runtime::Runtime;

use crate::hub::{self, Hub, HubOptions, LogCallback, StateCallback};

/// Invoked on a Tokio worker thread whenever extension connection or peer count
/// changes. `status_json` is valid only for the duration of the callback.
pub type BuStateCallback = Option<extern "C" fn(status_json: *const c_char, ctx: *mut c_void)>;

/// Opaque hub handle owned by Swift. Owns the Tokio runtime that drives the Hub.
pub struct BuHub {
    runtime: Runtime,
    hub: Arc<Hub>,
}

// ── Error / string marshalling helpers ───────────────────────────────────

/// Clear `*err_out` (if the caller gave us a slot) before doing any work, so a
/// stale pointer can never be mistaken for an error on the success path.
fn clear_error(err_out: *mut *mut c_char) {
    if err_out.is_null() {
        return;
    }
    // SAFETY: contract is that `err_out`, when non-NULL, points at a writable
    // `char *` slot owned by the caller.
    unsafe {
        *err_out = std::ptr::null_mut();
    }
}

/// Store an owned, NUL-terminated error message in `*err_out`. Interior NUL
/// bytes (never produced by our messages) are replaced defensively.
fn write_error(err_out: *mut *mut c_char, message: &str) {
    if err_out.is_null() {
        return;
    }
    let sanitized: String = message
        .chars()
        .map(|character| if character == '\0' { ' ' } else { character })
        .collect();
    let cstring = match CString::new(sanitized) {
        Ok(cstring) => cstring,
        Err(_) => return,
    };
    // SAFETY: see `clear_error`; caller frees with `bu_free_string`.
    unsafe {
        *err_out = cstring.into_raw();
    }
}

/// Copy an optional input C string. Returns `Ok(None)` for NULL; invalid UTF-8
/// is a user error (the C ABI is defined as UTF-8 JSON / paths).
fn read_optional_cstring(ptr: *const c_char) -> Result<Option<String>, String> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: callers pass a NUL-terminated C string for the duration of the
    // call (heap contract in browser_use.h).
    let cstr = unsafe { CStr::from_ptr(ptr) };
    match cstr.to_str() {
        Ok(value) => Ok(Some(value.to_string())),
        Err(error) => Err(format!("Invalid UTF-8 in C string: {error}")),
    }
}

fn read_required_cstring(ptr: *const c_char) -> Result<String, String> {
    read_optional_cstring(ptr)?.ok_or_else(|| "Expected a non-null C string argument".to_string())
}

fn json_cstring(json: &str) -> Result<CString, String> {
    CString::new(json.to_string()).map_err(|_| "JSON payload contained a NUL byte".to_string())
}

// ── Runtime lifecycle helpers ────────────────────────────────────────────

fn build_hub_runtime() -> Result<Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .thread_name("browser-use-hub")
        .enable_all()
        .build()
        .map_err(|error| format!("Unable to create the Tokio runtime: {error}"))
}

/// Drop a runtime without ever panicking across the FFI boundary.
///
/// Tokio panics when a runtime is dropped on a thread that is itself inside a
/// Tokio async context ("Cannot drop a runtime in a context where blocking is
/// not allowed"). Callers are supposed to avoid that, but if it happens we hand
/// the shutdown to a fresh OS thread instead of aborting the process.
fn shutdown_runtime(runtime: Runtime) {
    if tokio::runtime::Handle::try_current().is_ok() {
        let _ = std::thread::Builder::new()
            .name("browser-use-runtime-shutdown".to_string())
            .spawn(move || drop(runtime));
        return;
    }
    drop(runtime);
}

/// Owns the runtime while `bu_hub_start` is still assembling the handle. If a
/// panic unwinds through startup, `Drop` shuts the runtime down safely (a plain
/// local would risk a double panic in an async context).
struct RuntimeGuard {
    runtime: Option<Runtime>,
}

impl RuntimeGuard {
    fn new(runtime: Runtime) -> Self {
        Self {
            runtime: Some(runtime),
        }
    }

    fn runtime(&self) -> &Runtime {
        self.runtime.as_ref().expect("runtime guard present")
    }

    fn into_runtime(mut self) -> Runtime {
        self.runtime.take().expect("runtime guard present")
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            shutdown_runtime(runtime);
        }
    }
}

// ── Callbacks ────────────────────────────────────────────────────────────

/// Build the hub log sink: append `line\n` to `log_path` from a shared file
/// handle. Parent directories are created best-effort, and a log file that
/// cannot be opened only disables logging (the hub still starts).
fn open_log_sink(log_path: &str) -> Option<LogCallback> {
    let path = Path::new(log_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!(
                "[browser-use] unable to open hub log {}: {error}",
                path.display()
            );
            return None;
        }
    };
    let file = Arc::new(Mutex::new(file));
    Some(Arc::new(move |line: &str| {
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    }) as LogCallback)
}

/// Invoke the Swift `BuStateCallback` with a NUL-terminated copy of the status
/// JSON. The C string lives only for the duration of the call, as documented in
/// the header; the trampoline must copy it synchronously.
fn invoke_state_callback(
    callback: extern "C" fn(status_json: *const c_char, ctx: *mut c_void),
    ctx: usize,
    status_json: &str,
) {
    let Ok(cstring) = CString::new(status_json) else {
        return;
    };
    let ctx = ctx as *mut c_void;
    let _ = catch_unwind(AssertUnwindSafe(|| callback(cstring.as_ptr(), ctx)));
}

/// Build hub options (`on_log` / `on_state`) from the C arguments.
fn build_callbacks(
    log_path: Option<&str>,
    on_change: BuStateCallback,
    ctx: *mut c_void,
) -> (Option<LogCallback>, Option<StateCallback>) {
    let on_log = match log_path {
        Some(path) => open_log_sink(path),
        None => None,
    };
    let on_state: Option<StateCallback> = on_change.map(|callback| {
        // Keep the closure `Send + Sync`: a C function pointer and a raw
        // context pointer smuggled through `usize` are both trivially sharable.
        let ctx = ctx as usize;
        Arc::new(move |status_json: &str| {
            invoke_state_callback(callback, ctx, status_json);
        }) as StateCallback
    });
    (on_log, on_state)
}

// ── bu_hub_start ─────────────────────────────────────────────────────────

fn start_hub(
    port: u16,
    log_path: *const c_char,
    on_change: BuStateCallback,
    ctx: *mut c_void,
) -> Result<*mut BuHub, String> {
    let log_path = read_optional_cstring(log_path)?;
    // `port == 0` means "resolve ORANGE_PORT / 19455" (header contract).
    let resolved_port = if port == 0 {
        hub::resolve_port(None)
    } else {
        port
    };

    let (on_log, on_state) = build_callbacks(log_path.as_deref(), on_change, ctx);

    let guard = RuntimeGuard::new(build_hub_runtime()?);
    let hub = guard.runtime().block_on(async move {
        let secret = crate::secret::load_secret()
            .await
            .map_err(|error| error.0)?;
        let hub = Hub::new(HubOptions {
            port: resolved_port,
            secret,
            on_log,
            on_state,
        });
        hub.start().await.map_err(|error| error.message)?;
        Ok::<Hub, String>(hub)
    })?;

    let runtime = guard.into_runtime();
    let boxed = Box::new(BuHub {
        runtime,
        hub: Arc::new(hub),
    });
    Ok(Box::into_raw(boxed))
}

/// Start the Tokio runtime and bind the Hub on `127.0.0.1:<port>`.
#[no_mangle]
pub extern "C" fn bu_hub_start(
    port: u16,
    log_path: *const c_char,
    on_change: BuStateCallback,
    ctx: *mut c_void,
    err_out: *mut *mut c_char,
) -> *mut BuHub {
    clear_error(err_out);
    match catch_unwind(AssertUnwindSafe(|| {
        start_hub(port, log_path, on_change, ctx)
    })) {
        Ok(Ok(hub)) => hub,
        Ok(Err(message)) => {
            write_error(err_out, &message);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_error(
                err_out,
                "Internal error while starting the in-process hub (Tokio runtime unavailable)",
            );
            std::ptr::null_mut()
        }
    }
}

// ── bu_hub_stop ──────────────────────────────────────────────────────────

/// Gracefully close sockets, stop the Hub, and shut down/join the Tokio runtime.
/// Guarantees no `on_change` callback runs after this returns. Safe with NULL.
#[no_mangle]
pub extern "C" fn bu_hub_stop(hub: *mut BuHub) {
    if hub.is_null() {
        return;
    }

    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `hub` was produced by `bu_hub_start`; the caller owns it and
        // calls stop exactly once (C contract in browser_use.h).
        let boxed = unsafe { Box::from_raw(hub) };
        let BuHub { runtime, hub } = *boxed;

        // Block until the hub has closed its sockets, failed in-flight
        // commands and awaited every internal task. `Hub::stop` holds the
        // callback lock, so this also waits out any in-flight `on_change`.
        let _ = catch_unwind(AssertUnwindSafe(|| runtime.block_on(hub.stop())));

        // Drop our Arc clone, then join the runtime's worker threads.
        drop(hub);
        shutdown_runtime(runtime);
    }));
}

// ── bu_hub_status_json ───────────────────────────────────────────────────

fn with_hub<T>(
    hub: *mut BuHub,
    operation: impl FnOnce(&BuHub) -> Result<T, String>,
) -> Result<T, String> {
    if hub.is_null() {
        return Err("The in-process hub is not running (null handle).".to_string());
    }
    // SAFETY: see `bu_hub_stop`; callers serialize access to a live handle.
    let hub = unsafe { &*hub };
    operation(hub)
}

fn status_json(hub: *mut BuHub, include_tabs: bool) -> Result<CString, String> {
    with_hub(hub, |hub| {
        let status = hub.runtime.block_on(hub.hub.status(include_tabs));
        let json = serde_json::to_string(&status)
            .map_err(|error| format!("Unable to serialize the hub status: {error}"))?;
        json_cstring(&json)
    })
}

/// Query current HubStatus JSON (tabs included when requested).
#[no_mangle]
pub extern "C" fn bu_hub_status_json(
    hub: *mut BuHub,
    include_tabs: bool,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_error(err_out);
    match catch_unwind(AssertUnwindSafe(|| status_json(hub, include_tabs))) {
        Ok(Ok(cstring)) => cstring.into_raw(),
        Ok(Err(message)) => {
            write_error(err_out, &message);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_error(err_out, "Internal error while reading the hub status");
            std::ptr::null_mut()
        }
    }
}

// ── bu_hub_command_json ──────────────────────────────────────────────────

fn command_json(
    hub: *mut BuHub,
    cmd_json: *const c_char,
    tab_id: i64,
    timeout_ms: u32,
) -> Result<CString, String> {
    if hub.is_null() {
        return Err("The in-process hub is not running (null handle).".to_string());
    }
    let raw = read_required_cstring(cmd_json)?;
    let cmd: serde_json::Value =
        serde_json::from_str(&raw).map_err(|error| format!("Invalid command JSON: {error}"))?;

    // `tab_id < 0` passes JSON null; `timeout_ms == 0` uses the hub default.
    let tab_id = if tab_id < 0 { None } else { Some(tab_id) };
    let timeout_ms = if timeout_ms == 0 {
        hub::DEFAULT_COMMAND_TIMEOUT_MS
    } else {
        u64::from(timeout_ms).min(hub::MAX_COMMAND_TIMEOUT_MS)
    };

    with_hub(hub, |hub| {
        let result = hub
            .runtime
            .block_on(hub.hub.send(cmd, tab_id, timeout_ms, false))
            .map_err(|error| error.message)?;
        let json = serde_json::to_string(&result)
            .map_err(|error| format!("Unable to serialize the command result: {error}"))?;
        json_cstring(&json)
    })
}

/// Execute one command on the extension and return its result JSON.
#[no_mangle]
pub extern "C" fn bu_hub_command_json(
    hub: *mut BuHub,
    cmd_json: *const c_char,
    tab_id: i64,
    timeout_ms: u32,
    err_out: *mut *mut c_char,
) -> *mut c_char {
    clear_error(err_out);
    match catch_unwind(AssertUnwindSafe(|| {
        command_json(hub, cmd_json, tab_id, timeout_ms)
    })) {
        Ok(Ok(cstring)) => cstring.into_raw(),
        Ok(Err(message)) => {
            write_error(err_out, &message);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_error(err_out, "Internal error while running the browser command");
            std::ptr::null_mut()
        }
    }
}

// ── bu_secret_info_json ──────────────────────────────────────────────────

fn secret_info_json() -> Result<CString, String> {
    // `load_secret_info` is `async` for API symmetry with the rest of the
    // crate but only performs blocking std I/O, so a tiny current-thread
    // runtime is enough (and avoids spawning a full multi-thread runtime).
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Unable to create the Tokio runtime: {error}"))?;
    let guard = RuntimeGuard::new(runtime);
    let info = guard
        .runtime()
        .block_on(crate::secret::load_secret_info())
        .map_err(|error| error.0)?;
    let json = serde_json::to_string(&info)
        .map_err(|error| format!("Unable to serialize the secret info: {error}"))?;
    json_cstring(&json)
}

/// Load or atomically create the secret; returns
/// `{"secret":"...","source":"...","setupCommand":"..."}`.
#[no_mangle]
pub extern "C" fn bu_secret_info_json(err_out: *mut *mut c_char) -> *mut c_char {
    clear_error(err_out);
    match catch_unwind(AssertUnwindSafe(secret_info_json)) {
        Ok(Ok(cstring)) => cstring.into_raw(),
        Ok(Err(message)) => {
            write_error(err_out, &message);
            std::ptr::null_mut()
        }
        Err(_) => {
            write_error(err_out, "Internal error while loading the shared secret");
            std::ptr::null_mut()
        }
    }
}

// ── bu_free_string ───────────────────────────────────────────────────────

/// Free any string returned by `bu_*` functions. Safe with NULL.
#[no_mangle]
pub extern "C" fn bu_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: non-null pointers handed to this function must come from
    // `CString::into_raw` in this module; they are freed exactly once.
    let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
        drop(CString::from_raw(ptr));
    }));
}
