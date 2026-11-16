#ifndef BROWSER_USE_H
#define BROWSER_USE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct BuHub BuHub;

/// Invoked on a Tokio worker thread whenever extension connection or peer count changes.
/// `status_json` is a valid NUL-terminated UTF-8 HubStatus JSON (with `tabs` omitted)
/// valid ONLY for the duration of the callback; copy before returning.
typedef void (*BuStateCallback)(const char *status_json, void *ctx);

/// Start the Tokio runtime and bind the Hub on 127.0.0.1:<port> (if port == 0, uses
/// ORANGE_PORT or 19455). Loads/creates ~/.orange-mcp/secret (or ORANGE_SECRET).
/// If `log_path` is non-NULL, hub log lines append to that file.
/// On failure, returns NULL and sets `*err_out` (caller frees via `bu_free_string`).
BuHub *bu_hub_start(
    uint16_t port,
    const char *log_path,
    BuStateCallback on_change,
    void *ctx,
    char **err_out
);

/// Gracefully close sockets, stop the Hub, and shut down/join the Tokio runtime.
/// Guarantees no `on_change` callback runs after this returns. Safe with NULL.
void bu_hub_stop(BuHub *hub);

/// Query current HubStatus JSON. When `include_tabs` is true and the extension is
/// connected, issues a quiet `tab_list` command (4s timeout) and includes `tabs` or
/// `tabsError`. Blocks the calling thread; Swift MUST call off @MainActor.
/// Returns NULL and sets `*err_out` on error; caller frees with `bu_free_string`.
char *bu_hub_status_json(BuHub *hub, bool include_tabs, char **err_out);

/// Execute one command on the extension (e.g. `{"action":"tab_switch"}`).
/// `tab_id < 0` passes `null`; `timeout_ms == 0` uses default 30,000 ms.
/// Blocks the calling thread; Swift MUST call off @MainActor.
/// Returns result JSON string on success, or NULL + `*err_out` on error.
char *bu_hub_command_json(
    BuHub *hub,
    const char *cmd_json,
    int64_t tab_id,
    uint32_t timeout_ms,
    char **err_out
);

/// Load or atomically create ~/.orange-mcp/secret (or read ORANGE_SECRET).
/// Returns JSON `{"secret":"...","source":"...","setupCommand":"..."}`,
/// or NULL + `*err_out` on failure. Free with `bu_free_string`.
char *bu_secret_info_json(char **err_out);

/// Free any string returned by `bu_*` functions. Safe with NULL.
void bu_free_string(char *ptr);

#ifdef __cplusplus
}
#endif

#endif /* BROWSER_USE_H */
