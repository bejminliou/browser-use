//! The 12 MCP tool schemas and handlers.
//!
//! Ported 1:1 from the legacy TypeScript `src/tools.ts`: every tool validates
//! the agent arguments, maps them to exactly one extension command with the
//! same defaults/omitted fields, and renders the extension's JSON through
//! `crate::format` (formatting is never duplicated here). The ADR-001..004
//! behaviors live in those defaults: the interactive flip when `selector` is
//! set, the 12k/80k snapshot budgets, `maxChars`, landmarks-collapsed notes,
//! batched `check`/`uncheck`, screenshot `[image, text]` blocks and the
//! `timeoutMs` command key for `browser_wait`.
//!
//! `tool_definitions()` returns the `tools/list` payload. The schema JSON is
//! embedded verbatim from the legacy interop surface so names, titles,
//! descriptions and JSON Schemas stay byte-identical.

use serde_json::{json, Map, Value};

use crate::format::{
    clip_inline, clip_snapshot, clip_text, describe_element, describe_tab, format_json,
    format_requests, format_snapshot, format_tabs, normalize_ref,
};
use crate::peer::PeerClient;

/// Per-command extension timeout used by every tool that does not override it.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Unscoped snapshots stay small so agent hosts inline them (~10KB threshold).
const DEFAULT_SNAPSHOT_LIMIT: usize = 12_000;
/// A scoped `selector` opts back into the larger cap.
const SELECTED_SNAPSHOT_LIMIT: usize = 80_000;
/// Whole-payload clip for reader / eval / diff output.
const MAX_CONTENT_CHARS: usize = 80_000;
/// Maximum annotation legend rows rendered for one screenshot.
const MAX_ANNOTATIONS: usize = 200;

/// One successful tool call: MCP content blocks (`{"type":"text",...}` or
/// `{"type":"image",...}`).
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    pub content: Vec<Value>,
}

impl ToolResult {
    /// A single text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![json!({ "type": "text", "text": text.into() })],
        }
    }
}

/// A rejected tool call, surfaced as an MCP `isError: true` result.
#[derive(Debug, Clone)]
pub struct ToolError(pub String);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

/// The `tools/list` payload: a JSON array of `{ name, title, description,
/// inputSchema, execution }` objects, in legacy registration order.
pub fn tool_definitions() -> Value {
    serde_json::from_str(TOOL_DEFINITIONS_JSON).expect("embedded tool definitions are valid JSON")
}

/// Tool names in `tools/list` order (1:1 with the legacy registrations).
pub const TOOL_NAMES: [&str; 12] = [
    "browser_snapshot",
    "browser_click",
    "browser_fill",
    "browser_press",
    "browser_select_or_check",
    "browser_locate",
    "browser_wait",
    "browser_navigate",
    "browser_tabs",
    "browser_read",
    "browser_screenshot",
    "browser_eval_and_network",
];

/// True when `name` is one of the 12 registered tools.
pub fn is_known_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

/// Execute one tool by name against the peer transport.
pub async fn call_tool(
    peer: &PeerClient,
    name: &str,
    args: Value,
) -> Result<ToolResult, ToolError> {
    let args = match args {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        _ => return Err(input_error(name, "arguments must be a JSON object")),
    };

    match name {
        "browser_snapshot" => browser_snapshot(peer, &args).await,
        "browser_click" => browser_click(peer, &args).await,
        "browser_fill" => browser_fill(peer, &args).await,
        "browser_press" => browser_press(peer, &args).await,
        "browser_select_or_check" => browser_select_or_check(peer, &args).await,
        "browser_locate" => browser_locate(peer, &args).await,
        "browser_wait" => browser_wait(peer, &args).await,
        "browser_navigate" => browser_navigate(peer, &args).await,
        "browser_tabs" => browser_tabs(peer, &args).await,
        "browser_read" => browser_read(peer, &args).await,
        "browser_screenshot" => browser_screenshot(peer, &args).await,
        "browser_eval_and_network" => browser_eval_and_network(peer, &args).await,
        other => Err(ToolError(format!("Tool {other} not found"))),
    }
}

// ── Tools ────────────────────────────────────────────────────────────────

async fn browser_snapshot(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    let selector = arg_string(args, "selector");
    let scoped = selector.map(|value| !value.is_empty()).unwrap_or(false);
    let limit = if scoped {
        SELECTED_SNAPSHOT_LIMIT
    } else {
        DEFAULT_SNAPSHOT_LIMIT
    };

    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!("snapshot"));
    // ADR: interactive defaults to true, but flips to false when a selector
    // scopes the snapshot (a scoped tree is usually read for its content).
    cmd.insert(
        "interactive".to_string(),
        json!(arg_bool(args, "interactive").unwrap_or(!scoped)),
    );
    cmd.insert(
        "compact".to_string(),
        json!(arg_bool(args, "compact").unwrap_or(true)),
    );
    cmd.insert(
        "delta".to_string(),
        json!(arg_bool(args, "delta").unwrap_or(false)),
    );
    insert_opt(&mut cmd, "selector", selector.map(|value| json!(value)));
    insert_opt(&mut cmd, "urls", arg_bool(args, "urls").map(Value::Bool));
    // Let the page drop repeated landmark noise before the tree is clipped:
    // leave room for the title/URL header that wraps the tree.
    cmd.insert(
        "maxChars".to_string(),
        json!(std::cmp::max(2_000, limit as i64 - 512)),
    );

    let result = command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
    let formatted = format_snapshot(&result);
    let text = if scoped {
        clip_text(&formatted, limit)
    } else {
        clip_snapshot(&formatted, limit)
    };
    Ok(ToolResult::text(text))
}

async fn browser_click(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    let tool = "browser_click";
    let target = require_string(tool, args, "target")?;
    let action = if arg_bool(args, "rightClick") == Some(true) {
        "contextmenu"
    } else if arg_bool(args, "double") == Some(true) {
        "dblclick"
    } else {
        "click"
    };
    let cmd = json!({
        "action": action,
        "selector": target,
        "force": arg_bool(args, "force") == Some(true),
    });
    let result = command(peer, cmd, tab_id(args), DEFAULT_TIMEOUT_MS).await?;

    let verb = match action {
        "contextmenu" => "Right-clicked",
        "dblclick" => "Double-clicked",
        _ => "Clicked",
    };
    Ok(ToolResult::text(format!(
        "{verb} {}.",
        describe_element(&result, Some(target))
    )))
}

async fn browser_fill(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    let tool = "browser_fill";
    let target = require_string(tool, args, "target")?;
    let value = require_string(tool, args, "value")?;
    let slow = arg_bool(args, "slowType") == Some(true);

    let mut cmd = Map::new();
    cmd.insert(
        "action".to_string(),
        json!(if slow { "type" } else { "fill" }),
    );
    cmd.insert("selector".to_string(), json!(target));
    cmd.insert("value".to_string(), json!(value));
    if slow {
        if let Some(delay) = arg_number(args, "delayMs") {
            cmd.insert("delay".to_string(), json_number(delay));
        }
    }

    let result = command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
    let mut text = format!(
        "{} {} with {}.",
        if slow { "Typed" } else { "Filled" },
        describe_element(&result, Some(target)),
        json_string(&clip_inline(value, 120)),
    );

    if arg_bool(args, "pressEnter") == Some(true) {
        let pressed = command(
            peer,
            json!({ "action": "press", "selector": target, "key": "Enter" }),
            tab_id(args),
            DEFAULT_TIMEOUT_MS,
        )
        .await?;
        text.push_str(&format!(
            " Pressed Enter on {}.",
            describe_element(&pressed, Some(target))
        ));

        // The extension waits for form-submit navigations before replying;
        // report that so the next snapshot is not read against a stale page.
        if pressed.get("navigated") == Some(&Value::Bool(true)) {
            text.push_str(&format!(
                " Waited for navigation (status={}).",
                string_value(pressed.get("navigationStatus"), "complete")
            ));
        }
    }

    Ok(ToolResult::text(text))
}

async fn browser_press(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    let key = require_string("browser_press", args, "key")?;
    let target = arg_string(args, "target");
    let selector = target.unwrap_or("body");
    let result = command(
        peer,
        json!({ "action": "press", "selector": selector, "key": key }),
        tab_id(args),
        DEFAULT_TIMEOUT_MS,
    )
    .await?;

    let place = if target.map(|value| !value.is_empty()).unwrap_or(false) {
        describe_element(&result, target)
    } else {
        "the page".to_string()
    };
    Ok(ToolResult::text(format!(
        "Pressed {} on {place}.",
        json_string(key)
    )))
}

async fn browser_select_or_check(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const ACTIONS: [&str; 6] = ["select", "check", "uncheck", "hover", "scroll", "focus"];
    let tool = "browser_select_or_check";
    let action = require_choice(tool, args, "action", &ACTIONS)?;

    let targets: Vec<&str> = match args.get("target") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) => vec![text.as_str()],
        Some(Value::Array(items)) => {
            let mut targets = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(text) => targets.push(text),
                    None => {
                        return Err(input_error(
                            tool,
                            "field 'target' must be a string or an array of strings",
                        ))
                    }
                }
            }
            targets
        }
        Some(_) => {
            return Err(input_error(
                tool,
                "field 'target' must be a string or an array of strings",
            ))
        }
    };

    let batched = action == "check" || action == "uncheck";
    if action == "select" {
        if targets.is_empty() {
            return Err(ToolError(
                "browser_select_or_check(select) requires target".to_string(),
            ));
        }
        if targets.len() > 1 {
            return Err(ToolError(
                "browser_select_or_check(select) accepts one target".to_string(),
            ));
        }
        if arg_value(args, "value").is_none() {
            return Err(ToolError(
                "browser_select_or_check(select) requires value".to_string(),
            ));
        }
    } else if !batched && action != "scroll" {
        if targets.is_empty() {
            return Err(ToolError(format!(
                "browser_select_or_check({action}) requires target"
            )));
        }
        if targets.len() > 1 {
            return Err(ToolError(format!(
                "browser_select_or_check({action}) accepts one target"
            )));
        }
    }

    // Batch check/uncheck: one tool call, one command per target.
    if batched && targets.len() > 1 {
        let mut checked: Vec<(&str, Value)> = Vec::new();
        let mut failed: Vec<(&str, String)> = Vec::new();
        for target in &targets {
            match command(
                peer,
                json!({ "action": action, "selector": target }),
                tab_id(args),
                DEFAULT_TIMEOUT_MS,
            )
            .await
            {
                Ok(result) => checked.push((target, result)),
                Err(error) => failed.push((target, error.0)),
            }
        }

        let verb = if action == "check" {
            "Checked"
        } else {
            "Unchecked"
        };
        let failed_text = failed
            .iter()
            .map(|(target, error)| format!("{target} ({error})"))
            .collect::<Vec<_>>()
            .join(", ");

        if checked.is_empty() {
            return Err(ToolError(format!(
                "{} 0 of {} elements; failed: {failed_text}",
                verb.to_ascii_lowercase(),
                targets.len()
            )));
        }

        let labels = checked
            .iter()
            .map(|(target, result)| {
                let reference = normalize_ref(result.get("ref").unwrap_or(&Value::Null));
                if reference.is_empty() {
                    (*target).to_string()
                } else {
                    reference
                }
            })
            .collect::<Vec<_>>();

        let mut text = if checked.len() == targets.len() {
            format!("{verb} {} elements: {}", targets.len(), labels.join(", "))
        } else {
            format!(
                "{verb} {} of {} elements: {}",
                checked.len(),
                targets.len(),
                labels.join(", ")
            )
        };
        if !failed.is_empty() {
            text.push_str(&format!("; failed: {failed_text}"));
        }
        return Ok(ToolResult::text(format!("{text}.")));
    }

    let target = targets.first().copied();
    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!(action));
    insert_opt(&mut cmd, "selector", target.map(|value| json!(value)));
    if let Some(value) = args.get("value") {
        if !value.is_null() {
            cmd.insert("value".to_string(), value.clone());
        }
    }
    if action == "scroll" {
        cmd.insert(
            "y".to_string(),
            json_number(arg_number(args, "scrollY").unwrap_or(0.0)),
        );
    }

    let result = command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
    let description = describe_element(&result, target);

    match action.as_str() {
        "select" => {
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            let current = match result.get("value") {
                Some(Value::String(value)) => {
                    format!(" (current value {})", json_string(value))
                }
                _ => String::new(),
            };
            Ok(ToolResult::text(format!(
                "Selected {} in {description}{current}.",
                json_value(&value)
            )))
        }
        "check" => Ok(ToolResult::text(format!(
            "Checked {description}{}.",
            checked_suffix(&result)
        ))),
        "uncheck" => Ok(ToolResult::text(format!(
            "Unchecked {description}{}.",
            checked_suffix(&result)
        ))),
        "hover" => Ok(ToolResult::text(format!("Hovered {description}."))),
        "focus" => Ok(ToolResult::text(format!("Focused {description}."))),
        "scroll" => {
            if target.is_some() {
                return Ok(ToolResult::text(format!(
                    "Scrolled {description} into view."
                )));
            }
            let y = number_value(
                result.get("scrollY"),
                arg_number(args, "scrollY").unwrap_or(0.0),
            );
            let x = match result.get("scrollX") {
                None | Some(Value::Null) => String::new(),
                Some(_) => format!(
                    " (x={})",
                    number_text(number_value(result.get("scrollX"), 0.0))
                ),
            };
            Ok(ToolResult::text(format!(
                "Scrolled to y={}{x}.",
                number_text(y)
            )))
        }
        _ => Err(input_error(
            tool,
            "field 'action' must be one of select, check, uncheck, hover, scroll, focus",
        )),
    }
}

async fn browser_locate(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const BYS: [&str; 7] = [
        "role",
        "text",
        "label",
        "placeholder",
        "alt",
        "title",
        "testid",
    ];
    let tool = "browser_locate";
    let by = require_choice(tool, args, "by", &BYS)?;
    let query = require_string(tool, args, "query")?;

    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!("locate"));
    cmd.insert("by".to_string(), json!(by));
    cmd.insert(
        "exact".to_string(),
        json!(arg_bool(args, "exact").unwrap_or(false)),
    );
    match by.as_str() {
        "role" => match arg_string(args, "role").filter(|value| !value.is_empty()) {
            Some(role) => {
                cmd.insert("role".to_string(), json!(role));
                cmd.insert("name".to_string(), json!(query));
            }
            None => {
                cmd.insert("role".to_string(), json!(query));
            }
        },
        "text" => {
            cmd.insert("text".to_string(), json!(query));
        }
        "label" => {
            cmd.insert("label".to_string(), json!(query));
        }
        "placeholder" => {
            cmd.insert("placeholder".to_string(), json!(query));
        }
        "alt" => {
            cmd.insert("alt".to_string(), json!(query));
        }
        "title" => {
            cmd.insert("title".to_string(), json!(query));
        }
        _ => {
            cmd.insert("testId".to_string(), json!(query));
        }
    }

    let result = command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
    let matches = number_value(result.get("matches"), 1.0);
    let plural = if matches == 1.0 { "" } else { "es" };
    Ok(ToolResult::text(format!(
        "Located {} ({} match{plural}).",
        describe_element(&result, Some(query)),
        number_text(matches)
    )))
}

async fn browser_wait(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const STATES: [&str; 4] = ["visible", "hidden", "attached", "detached"];
    let selector = arg_string(args, "selector").filter(|value| !value.trim().is_empty());
    let text_arg = arg_string(args, "text").filter(|value| !value.trim().is_empty());
    let has_condition = selector.is_some() || text_arg.is_some();
    let timeout_ms = arg_integer(args, "timeoutMs").unwrap_or(10_000).max(0) as u64;

    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!("wait"));
    // Omit absent conditions entirely so the extension can distinguish
    // "pause while the page loads" from "wait for empty selector".
    insert_opt(&mut cmd, "selector", selector.map(|value| json!(value)));
    insert_opt(&mut cmd, "text", text_arg.map(|value| json!(value)));
    cmd.insert("timeoutMs".to_string(), json!(timeout_ms));

    // ADR: `state` is only meaningful (and only sent) when there is a condition.
    let state = arg_choice(args, "state", &STATES);
    if has_condition {
        cmd.insert("state".to_string(), json!(state.unwrap_or("visible")));
    }

    let result = command(peer, Value::Object(cmd), tab_id(args), timeout_ms + 5_000).await?;
    let waited = number_value(result.get("waited"), 0.0);

    if !has_condition {
        return Ok(ToolResult::text(format!(
            "Waited {}ms.",
            number_text(waited)
        )));
    }

    let condition = match selector {
        Some(selector) => format!("selector {}", json_string(selector)),
        None => format!("text {}", json_string(text_arg.unwrap_or(""))),
    };
    Ok(ToolResult::text(format!(
        "Waited {}ms for {condition} (state={}).",
        number_text(waited),
        state.unwrap_or("visible")
    )))
}

async fn browser_navigate(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const ACTIONS: [&str; 4] = ["goto", "back", "forward", "reload"];
    let action = require_choice("browser_navigate", args, "action", &ACTIONS)?;
    let url = arg_string(args, "url");
    if action == "goto" && url.map(|value| value.is_empty()).unwrap_or(true) {
        return Err(ToolError("browser_navigate(goto) requires url".to_string()));
    }

    let mapped = match action.as_str() {
        "goto" => "navigate",
        "back" => "back",
        "forward" => "forward",
        _ => "reload",
    };
    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!(mapped));
    if action == "goto" {
        if let Some(url) = url {
            cmd.insert("url".to_string(), json!(url));
        }
    }

    let requested_tab = tab_id(args);
    let result = command(peer, Value::Object(cmd), requested_tab, 40_000).await?;

    let tab = number_value(
        result.get("tabId"),
        requested_tab.map(|value| value as f64).unwrap_or(0.0),
    );
    let final_url = string_value(result.get("url"), url.unwrap_or(""));
    let status = string_value(result.get("status"), "complete");
    let where_ = if final_url.is_empty() {
        String::new()
    } else {
        format!(" to {final_url}")
    };
    let tab_suffix = if tab != 0.0 {
        format!(", tab {}", number_text(tab))
    } else {
        String::new()
    };
    let verb = match action.as_str() {
        "goto" => "Navigated",
        "back" => "Went back",
        "forward" => "Went forward",
        _ => "Reloaded",
    };
    Ok(ToolResult::text(format!(
        "{verb}{where_} (status={status}{tab_suffix})."
    )))
}

async fn browser_tabs(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const ACTIONS: [&str; 5] = ["list", "new", "switch", "close", "bind"];
    let tool = "browser_tabs";
    let action = require_choice(tool, args, "action", &ACTIONS)?;

    if action == "list" {
        let result = command(
            peer,
            json!({ "action": "tab_list" }),
            None,
            DEFAULT_TIMEOUT_MS,
        )
        .await?;
        return Ok(ToolResult::text(format_tabs(&result)));
    }

    if action == "new" {
        let mut cmd = Map::new();
        cmd.insert("action".to_string(), json!("tab_new"));
        insert_opt(
            &mut cmd,
            "url",
            arg_string(args, "url").map(|value| json!(value)),
        );
        cmd.insert(
            "active".to_string(),
            json!(arg_bool(args, "active") == Some(true)),
        );
        // Legacy sends no message `tabId` for `tab_new` (it always opens in the
        // bound/active context); a stray `tabId` argument is ignored.
        let result = command(peer, Value::Object(cmd), None, DEFAULT_TIMEOUT_MS).await?;
        return Ok(ToolResult::text(format!(
            "Opened {}.",
            describe_tab(&result)
        )));
    }

    let mut tab = tab_id(args);

    // Closing without a tab id closes whichever tab the browser marks active.
    if action == "close" && tab.is_none() {
        let list = command(peer, json!({ "action": "tab_list" }), None, 10_000).await?;
        let active = list.get("tabs").and_then(Value::as_array).and_then(|tabs| {
            tabs.iter()
                .find(|tab| tab.get("active") == Some(&Value::Bool(true)))
        });
        tab = active
            .and_then(|tab| number_or_null(tab.get("id")))
            .map(|value| value as i64);
        if tab.is_none() {
            return Err(ToolError("No active tab to close".to_string()));
        }
    }

    if action == "switch" && tab.is_none() {
        return Err(ToolError("browser_tabs(switch) requires tabId".to_string()));
    }

    if action == "bind" {
        let result = command(
            peer,
            json!({ "action": "tab_bind", "tabId": tab }),
            tab,
            DEFAULT_TIMEOUT_MS,
        )
        .await?;
        let bound = number_or_null(result.get("boundTabId"));
        let text = if bound.is_none() {
            "Unbound the agent tab (using the active tab).".to_string()
        } else {
            format!("Bound to {}.", describe_tab(&result))
        };
        return Ok(ToolResult::text(text));
    }

    if action == "close" {
        let result = command(
            peer,
            json!({ "action": "tab_close", "tabId": tab }),
            tab,
            DEFAULT_TIMEOUT_MS,
        )
        .await?;
        let closed = number_value(
            result.get("closed"),
            tab.map(|value| value as f64).unwrap_or(0.0),
        );
        return Ok(ToolResult::text(format!(
            "Closed tab {}.",
            number_text(closed)
        )));
    }

    let result = command(
        peer,
        json!({ "action": "tab_switch", "tabId": tab }),
        tab,
        DEFAULT_TIMEOUT_MS,
    )
    .await?;
    Ok(ToolResult::text(format!(
        "Switched to {}.",
        describe_tab(&result)
    )))
}

async fn browser_read(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const LLMS: [&str; 2] = ["index", "full"];
    let mut url = arg_string(args, "url").map(|value| value.to_string());
    let llms = arg_choice(args, "llms", &LLMS);

    // llms.txt discovery needs a URL. When only a tab is selected, ask the
    // extension for that tab's current URL first.
    if url.is_none() && llms.is_some() {
        let active = command(
            peer,
            json!({ "action": "url" }),
            tab_id(args),
            DEFAULT_TIMEOUT_MS,
        )
        .await?;
        let discovered = string_value(active.get("url"), "");
        if discovered.is_empty() {
            return Err(ToolError(
                "Could not determine the active tab URL; pass url explicitly".to_string(),
            ));
        }
        url = Some(discovered);
    }

    let outline = arg_bool(args, "outline") == Some(true);
    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!("read"));
    insert_opt(&mut cmd, "url", url.map(|value| json!(value)));
    insert_opt(&mut cmd, "llms", llms.map(|value| json!(value)));
    cmd.insert("outline".to_string(), json!(outline));
    insert_opt(
        &mut cmd,
        "filter",
        arg_string(args, "filter").map(|value| json!(value)),
    );

    let result = command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
    let fallback = string_value(result.get("content"), "");
    let mut body = if outline {
        match result.get("outline") {
            Some(Value::String(outline)) => outline.clone(),
            _ => string_value(result.get("markdown"), &fallback),
        }
    } else {
        string_value(result.get("markdown"), &fallback)
    };

    if body.trim().is_empty() {
        body = "(empty document: the reader extracted no text; the page may still be loading \
                or may not expose readable content)"
            .to_string();
    }
    if result.get("truncated") == Some(&Value::Bool(true)) {
        body.push_str("\n\n[content truncated by reader]");
    }
    Ok(ToolResult::text(clip_text(&body, MAX_CONTENT_CHARS)))
}

async fn browser_screenshot(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const FORMATS: [&str; 2] = ["png", "jpeg"];
    let format = arg_choice(args, "format", &FORMATS).unwrap_or("jpeg");

    let mut cmd = Map::new();
    cmd.insert("action".to_string(), json!("screenshot"));
    cmd.insert(
        "annotate".to_string(),
        json!(arg_bool(args, "annotate").unwrap_or(true)),
    );
    insert_opt(
        &mut cmd,
        "selector",
        arg_string(args, "selector").map(|value| json!(value)),
    );
    cmd.insert(
        "fullPage".to_string(),
        json!(arg_bool(args, "fullPage") == Some(true)),
    );
    cmd.insert("format".to_string(), json!(format));

    let result = command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
    let base64 = string_value(result.get("base64"), "");
    if base64.is_empty() {
        return Err(ToolError(
            "Extension returned an empty screenshot".to_string(),
        ));
    }

    let actual_format = string_value(result.get("format"), format);
    let mime_type = if actual_format == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    let tab = number_or_null(result.get("tabId")).or(tab_id(args).map(|value| value as f64));
    let annotations: Vec<&Map<String, Value>> = result
        .get("annotations")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();
    let tab_suffix = tab
        .map(|tab| format!(", tab {}", number_text(tab)))
        .unwrap_or_default();
    lines.push(format!(
        "Screenshot captured ({actual_format}{tab_suffix})."
    ));

    if !annotations.is_empty() {
        lines.push(String::new());
        lines.push("Annotations:".to_string());
        for entry in annotations.iter().take(MAX_ANNOTATIONS) {
            let reference = normalize_ref(entry.get("ref").unwrap_or(&Value::Null));
            let label = match entry.get("label") {
                Some(Value::String(label)) => label.clone(),
                _ => strip_ref_prefix(&js_string_or_empty(entry.get("ref"))),
            };
            let role = string_value(entry.get("role"), "");
            let name = match entry.get("name") {
                Some(Value::String(name)) if !name.is_empty() => {
                    format!(" {}", json_string(name))
                }
                _ => String::new(),
            };
            let role_suffix = if role.is_empty() {
                String::new()
            } else {
                format!(" {role}")
            };
            lines.push(
                format!("[{label}] {reference}{role_suffix}{name}")
                    .trim()
                    .to_string(),
            );
        }
        if annotations.len() > MAX_ANNOTATIONS {
            lines.push(format!(
                "... {} more annotations omitted",
                annotations.len() - MAX_ANNOTATIONS
            ));
        }
    } else if arg_bool(args, "annotate").unwrap_or(true) {
        lines.push("No visible annotated elements.".to_string());
    }

    let notes: Vec<&str> = result
        .get("notes")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !notes.is_empty() {
        lines.push(String::new());
        for note in notes {
            lines.push(format!("Note: {note}"));
        }
    }

    Ok(ToolResult {
        content: vec![
            json!({ "type": "image", "data": base64, "mimeType": mime_type }),
            json!({ "type": "text", "text": lines.join("\n") }),
        ],
    })
}

async fn browser_eval_and_network(
    peer: &PeerClient,
    args: &Map<String, Value>,
) -> Result<ToolResult, ToolError> {
    const ACTIONS: [&str; 5] = ["evaluate", "requests", "route", "unroute", "diff_snapshot"];
    let tool = "browser_eval_and_network";
    let action = require_choice(tool, args, "action", &ACTIONS)?;

    match action.as_str() {
        "evaluate" => {
            let expression = arg_string(args, "expression");
            let code = arg_string(args, "code");
            if expression.is_none() && code.is_none() {
                return Err(ToolError(
                    "evaluate requires expression or code".to_string(),
                ));
            }

            let mut cmd = Map::new();
            cmd.insert("action".to_string(), json!("evaluate"));
            insert_opt(&mut cmd, "expression", expression.map(|value| json!(value)));
            insert_opt(&mut cmd, "code", code.map(|value| json!(value)));
            let result =
                command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;

            // Make the fallback explicit: page globals and expando props are
            // invisible from the isolated content-script world.
            let world_note = if string_value(result.get("world"), "") == "isolated" {
                " (isolated content-script world; MAIN world eval was blocked by CSP)"
            } else {
                ""
            };
            let rendered = match result.get("result") {
                Some(value) => format_json(value),
                None => "undefined".to_string(),
            };
            Ok(ToolResult::text(format!("Result{world_note}:\n{rendered}")))
        }
        "requests" => {
            let mut cmd = Map::new();
            cmd.insert("action".to_string(), json!("requests"));
            cmd.insert(
                "clear".to_string(),
                json!(arg_bool(args, "clear") == Some(true)),
            );
            insert_opt(
                &mut cmd,
                "filter",
                arg_string(args, "filter").map(|value| json!(value)),
            );
            insert_opt(
                &mut cmd,
                "limit",
                arg_integer(args, "limit").map(|value| json!(value)),
            );
            let result =
                command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
            Ok(ToolResult::text(format_requests(&result)))
        }
        "route" => {
            let url_pattern = arg_string(args, "urlPattern").filter(|value| !value.is_empty());
            let Some(url_pattern) = url_pattern else {
                return Err(ToolError("route requires urlPattern".to_string()));
            };

            let mut cmd = Map::new();
            cmd.insert("action".to_string(), json!("route"));
            cmd.insert("urlPattern".to_string(), json!(url_pattern));
            insert_opt(
                &mut cmd,
                "status",
                arg_integer(args, "status").map(|value| json!(value)),
            );
            insert_opt(
                &mut cmd,
                "body",
                args.get("body").filter(|value| !value.is_null()).cloned(),
            );
            insert_opt(
                &mut cmd,
                "contentType",
                arg_string(args, "contentType").map(|value| json!(value)),
            );
            insert_opt(
                &mut cmd,
                "headers",
                args.get("headers")
                    .filter(|value| !value.is_null())
                    .cloned(),
            );
            cmd.insert(
                "abort".to_string(),
                json!(arg_bool(args, "abort") == Some(true)),
            );
            insert_opt(
                &mut cmd,
                "redirectUrl",
                arg_string(args, "redirectUrl").map(|value| json!(value)),
            );

            let result =
                command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
            let pattern = string_value(
                result
                    .get("route")
                    .and_then(|route| route.get("urlPattern")),
                url_pattern,
            );
            let active = result
                .get("routes")
                .and_then(Value::as_array)
                .map(|routes| routes.len())
                .unwrap_or(0);
            let active_suffix = if active > 0 {
                format!(" ({active} active)")
            } else {
                String::new()
            };
            Ok(ToolResult::text(format!(
                "Route {} configured{active_suffix}.",
                json_string(&pattern)
            )))
        }
        "unroute" => {
            let mut cmd = Map::new();
            cmd.insert("action".to_string(), json!("unroute"));
            insert_opt(
                &mut cmd,
                "urlPattern",
                arg_string(args, "urlPattern").map(|value| json!(value)),
            );
            let result =
                command(peer, Value::Object(cmd), tab_id(args), DEFAULT_TIMEOUT_MS).await?;
            let removed = number_value(result.get("removed"), 0.0);
            let remaining = result
                .get("routes")
                .and_then(Value::as_array)
                .map(|routes| routes.len())
                .unwrap_or(0);
            let remain_suffix = if remaining > 0 {
                format!("; {remaining} remain")
            } else {
                String::new()
            };
            Ok(ToolResult::text(format!(
                "Removed {} route(s){remain_suffix}.",
                number_text(removed)
            )))
        }
        _ => {
            let result = command(
                peer,
                json!({ "action": "diff_snapshot" }),
                tab_id(args),
                DEFAULT_TIMEOUT_MS,
            )
            .await?;
            let diff = string_value(result.get("diff"), "");
            if diff.trim().is_empty() {
                return Ok(ToolResult::text("No snapshot changes."));
            }
            let additions = number_value(result.get("additions"), 0.0);
            let removals = number_value(result.get("removals"), 0.0);
            Ok(ToolResult::text(clip_text(
                &format!(
                    "Snapshot diff ({}+ / {}-):\n\n{diff}",
                    number_text(additions),
                    number_text(removals)
                ),
                MAX_CONTENT_CHARS,
            )))
        }
    }
}

// ── Extension command plumbing ───────────────────────────────────────────

/// Send one command and require an object result (the legacy `command()`).
async fn command(
    peer: &PeerClient,
    cmd: Value,
    tab_id: Option<i64>,
    timeout_ms: u64,
) -> Result<Value, ToolError> {
    let result = peer
        .command(cmd, tab_id, timeout_ms)
        .await
        .map_err(|error| ToolError(error.message))?;
    if result.is_object() {
        Ok(result)
    } else {
        Err(ToolError(
            "Extension returned an invalid command response".to_string(),
        ))
    }
}

// ── Argument helpers ─────────────────────────────────────────────────────

/// Legacy SDK-level argument validation failure (no `Error: ` prefix).
fn input_error(tool: &str, message: impl std::fmt::Display) -> ToolError {
    ToolError(format!(
        "Input validation error: Invalid arguments for tool {tool}: {message}"
    ))
}

fn arg_value<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|value| !value.is_null())
}

fn arg_string<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    arg_value(args, key).and_then(Value::as_str)
}

fn require_string<'a>(
    tool: &str,
    args: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, ToolError> {
    match args.get(key) {
        Some(Value::String(text)) => Ok(text.as_str()),
        Some(Value::Null) | None => {
            Err(input_error(tool, format!("missing required field '{key}'")))
        }
        Some(_) => Err(input_error(tool, format!("field '{key}' must be a string"))),
    }
}

fn arg_bool(args: &Map<String, Value>, key: &str) -> Option<bool> {
    match args.get(key) {
        Some(Value::Bool(flag)) => Some(*flag),
        _ => None,
    }
}

fn arg_number(args: &Map<String, Value>, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        _ => None,
    }
}

fn arg_integer(args: &Map<String, Value>, key: &str) -> Option<i64> {
    match args.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|value| value.trunc() as i64)),
        Some(Value::String(text)) => text
            .trim()
            .parse::<f64>()
            .ok()
            .map(|value| value.trunc() as i64),
        _ => None,
    }
}

fn tab_id(args: &Map<String, Value>) -> Option<i64> {
    arg_integer(args, "tabId")
}

/// Optional enum argument (invalid values fall back to the default).
fn arg_choice<'a>(args: &'a Map<String, Value>, key: &str, allowed: &[&str]) -> Option<&'a str> {
    arg_string(args, key).filter(|value| allowed.contains(value))
}

/// Required enum argument; anything else is a tool-input validation error.
fn require_choice(
    tool: &str,
    args: &Map<String, Value>,
    key: &str,
    allowed: &[&str],
) -> Result<String, ToolError> {
    let value = require_string(tool, args, key)?;
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(input_error(
            tool,
            format!("field '{key}' must be one of {}", allowed.join(", ")),
        ))
    }
}

// ── JSON / number helpers (legacy runtime-shape semantics) ───────────────

fn insert_opt(map: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        map.insert(key.to_string(), value);
    }
}

/// Legacy `JSON.stringify(text)`.
fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}

/// Legacy `JSON.stringify(value)`.
fn json_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

/// JSON number that keeps integer spellings integral (legacy sends `0`, not
/// `0.0`, for scroll offsets and timeouts).
fn json_number(number: f64) -> Value {
    if !number.is_finite() {
        return Value::Null;
    }
    if number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_992.0 {
        return json!(number as i64);
    }
    json!(number)
}

/// Legacy `stringValue(value, fallback)` from `tools.ts`.
fn string_value(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        _ => fallback.to_string(),
    }
}

/// Legacy `numberValue(value, fallback)`.
fn number_value(value: Option<&Value>, fallback: f64) -> f64 {
    match js_number(value) {
        Some(number) if number.is_finite() => number,
        _ => fallback,
    }
}

/// Legacy `numberOrNull(value)`.
fn number_or_null(value: Option<&Value>) -> Option<f64> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.is_empty() => None,
        _ => js_number(value).filter(|number| number.is_finite()),
    }
}

/// JavaScript `Number(value)` for the runtime shapes the bridge can see.
fn js_number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Null => Some(0.0),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Some(0.0)
            } else {
                trimmed.parse::<f64>().ok()
            }
        }
        Value::Array(items) => match items.as_slice() {
            [] => Some(0.0),
            [item] => js_number(Some(item)),
            _ => None,
        },
        Value::Object(_) => None,
    }
}

/// JavaScript `String(number)` / template-literal number interpolation.
fn number_text(number: f64) -> String {
    if number.is_nan() {
        return "NaN".to_string();
    }
    if number == f64::INFINITY {
        return "Infinity".to_string();
    }
    if number == f64::NEG_INFINITY {
        return "-Infinity".to_string();
    }
    if number == 0.0 {
        return "0".to_string();
    }
    if number.fract() == 0.0 && number.abs() < 1e21 {
        return format!("{number:.0}");
    }
    if number.abs() < 1e-6 || number.abs() >= 1e21 {
        let exponential = format!("{number:e}");
        if let Some((mantissa, exponent)) = exponential.split_once('e') {
            if let Ok(exponent) = exponent.parse::<i32>() {
                let sign = if exponent < 0 { '-' } else { '+' };
                return format!("{mantissa}e{sign}{}", exponent.abs());
            }
        }
        return exponential;
    }
    format!("{number}")
}

/// JavaScript `String(value ?? "")` (screenshot annotation label fallback).
fn js_string_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number_text(number.as_f64().unwrap_or(f64::NAN)),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| js_string_or_empty(Some(item)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// Legacy `/^@?e/` replacement used for annotation label fallbacks.
fn strip_ref_prefix(text: &str) -> String {
    if let Some(rest) = text.strip_prefix("@e") {
        rest.to_string()
    } else if let Some(rest) = text.strip_prefix('e') {
        rest.to_string()
    } else {
        text.to_string()
    }
}

/// Legacy `checkedSuffix(result)`.
fn checked_suffix(result: &Value) -> String {
    match result.get("checked") {
        Some(Value::Bool(flag)) => format!(" (checked={flag})"),
        _ => String::new(),
    }
}

// ── Embedded tool definitions ────────────────────────────────────────────

/// The exact `tools/list` surface transcribed from the legacy TypeScript
/// definitions (MCP SDK 1.32 `tools/list` output): names, titles, descriptions
/// and JSON Schemas are byte-identical to `browser-use-legacy/src/tools.ts`.
const TOOL_DEFINITIONS_JSON: &str = r##"[
  {
    "name": "browser_snapshot",
    "title": "Browser snapshot",
    "description": "Get the page accessibility tree with durable @eN refs. Call this before acting; pass delta=true after actions. interactive (default true; false when selector is set) prunes static content, compact (default true) removes noise. Pass interactive: false to read page text, tables, or status values.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "interactive": {
          "description": "Include static text, tables, and status values (default true; false when selector is set)",
          "type": "boolean"
        },
        "compact": {
          "description": "Compact output (default true)",
          "type": "boolean"
        },
        "delta": {
          "description": "Return a delta from the previous snapshot (default false)",
          "type": "boolean"
        },
        "selector": {
          "description": "Scope the snapshot to a CSS selector or @ref. @eN refs reset after navigation; prefer a CSS selector like 'iframe' or 'main' after navigating.",
          "type": "string"
        },
        "urls": {
          "description": "Include link URLs",
          "type": "boolean"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      }
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_click",
    "title": "Browser click",
    "description": "Click an element by @ref from browser_snapshot/browser_locate, or by CSS (or xpath=...). double clicks twice; rightClick opens the context menu; force skips actionability checks.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "target": {
          "type": "string",
          "description": "@eN ref, CSS selector, or xpath=..."
        },
        "double": {
          "description": "Double-click",
          "type": "boolean"
        },
        "rightClick": {
          "description": "Right-click / context menu",
          "type": "boolean"
        },
        "force": {
          "description": "Bypass visibility and hit-test checks",
          "type": "boolean"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "target"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_fill",
    "title": "Browser fill",
    "description": "Fill a text field or contenteditable element. Set slowType for key-by-key typing (optional delayMs), and pressEnter to submit afterwards (waits for the resulting navigation to settle).",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "target": {
          "type": "string",
          "description": "@eN ref, CSS selector, or xpath=..."
        },
        "value": {
          "type": "string",
          "description": "Text to enter"
        },
        "slowType": {
          "description": "Type character by character instead of fill (default false)",
          "type": "boolean"
        },
        "delayMs": {
          "description": "Delay per character when slowType is true",
          "type": "integer",
          "minimum": 0,
          "maximum": 10000
        },
        "pressEnter": {
          "description": "Press Enter after filling (default false)",
          "type": "boolean"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "target",
        "value"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_press",
    "title": "Browser press",
    "description": "Press a key or key chord on an element (default target: body). Examples: Enter, Escape, Control+a, Meta+k, Shift+Tab.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "target": {
          "description": "Element to focus first (default body)",
          "type": "string"
        },
        "key": {
          "type": "string",
          "description": "Key or chord, e.g. Enter, Escape, Control+a, Meta+k"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "key"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_select_or_check",
    "title": "Browser select, check, hover, scroll",
    "description": "Grouped state interactions: select an option, check/uncheck, hover, scroll to a Y offset or element, or focus an element. For check/uncheck, target may be an array to toggle many elements in one call.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "action": {
          "type": "string",
          "enum": [
            "select",
            "check",
            "uncheck",
            "hover",
            "scroll",
            "focus"
          ]
        },
        "target": {
          "description": "@eN ref or CSS/XPath selector (required except page scroll); array batches check/uncheck",
          "anyOf": [
            {
              "type": "string"
            },
            {
              "minItems": 1,
              "type": "array",
              "items": {
                "type": "string"
              }
            }
          ]
        },
        "value": {
          "description": "Option value(s) for select",
          "anyOf": [
            {
              "type": "string"
            },
            {
              "type": "array",
              "items": {
                "type": "string"
              }
            }
          ]
        },
        "scrollY": {
          "description": "Vertical scroll offset for action=scroll",
          "type": "number"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "action"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_locate",
    "title": "Browser locate",
    "description": "Find an element by role, text, label, placeholder, alt text, title, or test id and return its durable @eN ref for immediate clicking/filling. exact defaults to false.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "by": {
          "type": "string",
          "enum": [
            "role",
            "text",
            "label",
            "placeholder",
            "alt",
            "title",
            "testid"
          ]
        },
        "query": {
          "type": "string",
          "description": "Search string"
        },
        "role": {
          "description": "ARIA role when by=role (query becomes the accessible name)",
          "type": "string"
        },
        "exact": {
          "description": "Exact match (default false)",
          "type": "boolean"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "by",
        "query"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_wait",
    "title": "Browser wait",
    "description": "Wait for a selector or text to reach a state, or omit both to pause while the page loads. Defaults: state=visible, timeoutMs=10000.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "selector": {
          "description": "Selector or @ref to wait for",
          "type": "string"
        },
        "text": {
          "description": "Text that must appear",
          "type": "string"
        },
        "state": {
          "type": "string",
          "enum": [
            "visible",
            "hidden",
            "attached",
            "detached"
          ]
        },
        "timeoutMs": {
          "description": "Default 10000",
          "type": "integer",
          "minimum": 0,
          "maximum": 120000
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      }
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_navigate",
    "title": "Browser navigate",
    "description": "Navigate the bound/active tab (goto), or go back, forward, or reload.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "action": {
          "type": "string",
          "enum": [
            "goto",
            "back",
            "forward",
            "reload"
          ]
        },
        "url": {
          "description": "Required for action=goto",
          "type": "string"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "action"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_tabs",
    "title": "Browser tabs",
    "description": "List, open, switch, close, or bind tabs. Binding pins commands that omit tabId; call bind without tabId to unpin.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "action": {
          "type": "string",
          "enum": [
            "list",
            "new",
            "switch",
            "close",
            "bind"
          ]
        },
        "url": {
          "description": "URL for action=new",
          "type": "string"
        },
        "active": {
          "description": "Open action=new in the foreground (default false: background tab)",
          "type": "boolean"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "action"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_read",
    "title": "Browser read",
    "description": "Read a page as clean Markdown. Omit url to convert the active tab; same-origin frames and open shadow roots are expanded and hidden elements are skipped. Use llms=index/full for llms.txt, outline=true for headings, and filter (plain text, or an unwrapped regex such as \"Fluff & Tuff|KONG\") to narrow sections.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "url": {
          "description": "URL to read; omit for the active tab",
          "type": "string"
        },
        "llms": {
          "description": "Read llms.txt or llms-full.txt",
          "type": "string",
          "enum": [
            "index",
            "full"
          ]
        },
        "outline": {
          "description": "Return only the heading outline",
          "type": "boolean"
        },
        "filter": {
          "description": "Keep sections whose heading/text matches; plain text, or an unwrapped regex such as \"foo|bar\"",
          "type": "string"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      }
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_screenshot",
    "title": "Browser screenshot",
    "description": "Capture the viewport, a selector, or the full page. annotate=true (default) overlays numbered Set-of-Mark labels matching @eN refs and returns their legend.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "annotate": {
          "description": "Overlay numbered element boxes (default true)",
          "type": "boolean"
        },
        "selector": {
          "description": "Capture one element",
          "type": "string"
        },
        "fullPage": {
          "description": "Capture the full document (default false)",
          "type": "boolean"
        },
        "format": {
          "description": "Image format (default jpeg)",
          "type": "string",
          "enum": [
            "png",
            "jpeg"
          ]
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      }
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  },
  {
    "name": "browser_eval_and_network",
    "title": "Browser eval and network",
    "description": "Power tool: evaluate page JS, inspect tracked requests, mock/abort routes, clear routes, or diff the last two snapshots.",
    "inputSchema": {
      "$schema": "http://json-schema.org/draft-07/schema#",
      "type": "object",
      "properties": {
        "action": {
          "type": "string",
          "enum": [
            "evaluate",
            "requests",
            "route",
            "unroute",
            "diff_snapshot"
          ]
        },
        "expression": {
          "description": "Expression for evaluate",
          "type": "string"
        },
        "code": {
          "description": "Statement body for evaluate",
          "type": "string"
        },
        "clear": {
          "description": "Clear captured requests",
          "type": "boolean"
        },
        "filter": {
          "description": "URL filter for requests",
          "type": "string"
        },
        "limit": {
          "type": "integer",
          "minimum": 1,
          "maximum": 1000
        },
        "urlPattern": {
          "description": "Route URL pattern (substring or /regex/)",
          "type": "string"
        },
        "status": {
          "description": "Mock response status",
          "type": "integer",
          "minimum": 100,
          "maximum": 599
        },
        "body": {
          "description": "Mock response body"
        },
        "contentType": {
          "type": "string"
        },
        "headers": {
          "type": "object",
          "propertyNames": {
            "type": "string"
          },
          "additionalProperties": {
            "type": "string"
          }
        },
        "abort": {
          "description": "Abort matching requests",
          "type": "boolean"
        },
        "redirectUrl": {
          "type": "string"
        },
        "tabId": {
          "description": "Target tab id (defaults to bound/active tab)",
          "type": "integer",
          "exclusiveMinimum": 0,
          "maximum": 9007199254740991
        }
      },
      "required": [
        "action"
      ]
    },
    "execution": {
      "taskSupport": "forbidden"
    }
  }
]"##;
