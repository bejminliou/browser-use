//! Pure output formatters shared by MCP tools and the CLI.
//!
//! Ported 1:1 from the legacy TypeScript "Formatting" section of
//! `src/tools.ts`. Output strings, ordering, and truncation notes must stay
//! byte-for-byte identical to the legacy bridge (the ADR-preservation
//! guardrail). Only `serde_json` may be imported here (module dependency DAG).

use serde_json::{Map, Value};

// ── Public formatters ────────────────────────────────────────────────────

/// Render a `browser_snapshot` result (full / unchanged / delta).
pub fn format_snapshot(result: &Value) -> String {
    let result = as_object(result);

    let title = non_empty_or(value_string(get(result, "title"), ""), "(untitled)");
    let url = non_empty_or(value_string(get(result, "url"), ""), "(no url)");
    let tab = value_number_or_null(get(result, "tabId"));
    let revision = value_number_or_null(get(result, "revision"));
    let header = format!(
        "Title: {} | URL: {} | Tab: {} | Revision: {}",
        title,
        url,
        optional_number_text(tab),
        optional_number_text(revision),
    );

    // `stringValue(result.kind, "full")`: an empty-but-present kind falls
    // through to the full rendering, exactly like the legacy code.
    let kind = value_string(get(result, "kind"), "full");
    if kind == "unchanged" {
        let base = match get(result, "baseRevision") {
            None | Some(Value::Null) => "?".to_string(),
            Some(value) => js_string(Some(value)),
        };
        return format!("{header}\n\nNo changes since revision {base}.");
    }

    if kind == "delta" {
        return format_delta_snapshot(&header, get(result, "changes"), get(result, "treeChange"));
    }

    let tree = value_string(get(result, "tree"), "");
    let collapsed = value_number_or_null(get(result, "landmarksCollapsed")).unwrap_or(0.0);
    let collapsed_note = if collapsed > 0.0 {
        format!(
            "\nNote: {} navigation/header/footer items were collapsed to keep the snapshot small; \
             pass selector=\"nav\" or selector=\"main\" to inspect them.",
            number_text(collapsed),
        )
    } else {
        String::new()
    };
    let body = if tree.is_empty() {
        "(empty snapshot)".to_string()
    } else {
        tree
    };
    format!("{header}{collapsed_note}\n\n{body}")
}

/// Render a `browser_tabs` result.
pub fn format_tabs(result: &Value) -> String {
    let tabs: Vec<&Map<String, Value>> = array_of(get(as_object(result), "tabs"))
        .iter()
        .filter_map(as_object)
        .collect();
    if tabs.is_empty() {
        return "No open tabs.".to_string();
    }

    let mut lines = vec![format!("Tabs ({}):", tabs.len())];
    for tab in tabs {
        let marker = if matches!(get(Some(tab), "active"), Some(Value::Bool(true))) {
            "*"
        } else {
            " "
        };
        let mut flags: Vec<&str> = Vec::new();
        if matches!(get(Some(tab), "pinned"), Some(Value::Bool(true))) {
            flags.push("pinned");
        }
        if value_string(get(Some(tab), "status"), "") == "loading" {
            flags.push("loading");
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" ({})", flags.join(", "))
        };
        lines.push(format!(
            "{marker} {}{flags}",
            describe_tab_object(Some(tab)),
        ));
    }
    lines.join("\n")
}

/// Render a single tab the way `browser_tabs`/`browser_tabs(new|switch|bind)`
/// describe it: `[id] title — url`.
pub fn describe_tab(tab: &Value) -> String {
    describe_tab_object(as_object(tab))
}

/// Render a `browser_click`/`browser_fill`/... target description.
pub fn describe_element(result: &Value, fallback: Option<&str>) -> String {
    let result = as_object(result);
    let reference = normalize_ref_option(get(result, "ref"));
    let role = value_string(get(result, "role"), "");
    let raw_name = value_string(get(result, "name"), "");
    let name = if raw_name.is_empty() {
        String::new()
    } else {
        // Legacy: `JSON.stringify(clipInline(rawName, 120))`. Slicing a name
        // mid-emoji makes the legacy string end on a lone high surrogate,
        // which modern `JSON.stringify` escapes as `\udXXX`; reproduce that.
        format!(" {}", quoted_clipped_inline(&raw_name, 120))
    };

    let mut parts: Vec<String> = Vec::new();
    if !reference.is_empty() {
        parts.push(reference);
    } else if let Some(fallback) = fallback {
        if is_ref_pattern(fallback) {
            parts.push(normalize_ref_str(fallback));
        }
    }
    if !role.is_empty() {
        parts.push(role);
    }

    let text = format!("{}{}", parts.join(" "), name).trim().to_string();
    if !text.is_empty() {
        return text;
    }
    match fallback {
        Some(fallback) if !fallback.is_empty() => fallback.to_string(),
        _ => "element".to_string(),
    }
}

/// Render a `browser_eval_and_network` `requests` result.
pub fn format_requests(result: &Value) -> String {
    let result = as_object(result);
    let requests: Vec<&Map<String, Value>> = array_of(get(result, "requests"))
        .iter()
        .filter_map(as_object)
        .collect();
    if requests.is_empty() {
        return "No matching requests captured.".to_string();
    }

    let count = value_number(get(result, "count"), requests.len() as f64);
    let mut lines = vec![format!("Requests ({}):", number_text(count))];
    for record in requests.iter().take(200) {
        let record = *record;
        let method = value_string(get(Some(record), "method"), "GET");
        let status = match get(Some(record), "status") {
            None | Some(Value::Null) => "---".to_string(),
            Some(value) => js_string(Some(value)),
        };
        let time = match get(Some(record), "timeMs") {
            None | Some(Value::Null) => String::new(),
            Some(value) => format!(" {}ms", number_text(value_number(Some(value), 0.0))),
        };
        let error = match get(Some(record), "error") {
            Some(Value::String(error)) if !error.is_empty() => format!(" ERROR {error}"),
            _ => String::new(),
        };
        let url = value_string(get(Some(record), "url"), "");
        lines.push(
            format!("{status} {method}{time} {url}{error}")
                .trim()
                .to_string(),
        );
    }
    if requests.len() > 200 {
        lines.push(format!(
            "... {} more requests omitted",
            requests.len() - 200
        ));
    }
    lines.join("\n")
}

/// Render an eval/route result value the way `browser_eval_and_network` does:
/// strings pass through verbatim, everything else is pretty-printed JSON.
///
/// Caveat: `serde_json`'s default map is key-sorted (the `preserve_order`
/// feature is off), so object key order can differ from legacy
/// `JSON.stringify` for objects whose insertion order is not alphabetical.
pub fn format_json(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => {
            let pretty = serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string());
            clip_text(&pretty, 80_000)
        }
    }
}

/// Normalize a ref: `"e12"` -> `"@e12"`; anything else unchanged.
pub fn normalize_ref(value: &Value) -> String {
    match value {
        Value::String(text) => normalize_ref_str(text),
        _ => String::new(),
    }
}

/// Clip long text, appending the standard truncation note.
pub fn clip_text(text: &str, max: usize) -> String {
    if utf16_len(text) <= max {
        return text.to_string();
    }
    format!(
        "{}\n\n[truncated at {} characters for context efficiency]",
        utf16_prefix(text, max),
        max,
    )
}

/// Clip a snapshot tree, appending the selector-hint truncation note.
pub fn clip_snapshot(text: &str, max: usize) -> String {
    if utf16_len(text) <= max {
        return text.to_string();
    }
    let hint = format!(
        "[truncated to {} chars; pass selector=\"...\" or interactive=true to inspect a specific section]",
        format_thousands(max),
    );
    format!("{}\n\n{hint}", utf16_prefix(text, max))
}

/// Clip a one-line inline value with an ellipsis.
pub fn clip_inline(text: &str, max: usize) -> String {
    if utf16_len(text) <= max {
        return text.to_string();
    }
    format!("{}…", utf16_prefix(text, max))
}

// ── formatSnapshot helpers ───────────────────────────────────────────────

fn format_delta_snapshot(
    header: &str,
    changes: Option<&Value>,
    tree_change: Option<&Value>,
) -> String {
    let changes: Vec<&Map<String, Value>> =
        array_of(changes).iter().filter_map(as_object).collect();
    let tree_change = tree_change.and_then(as_object);

    let mut lines: Vec<String> = vec![header.to_string(), String::new(), "Changes:".to_string()];
    if changes.is_empty() {
        lines.push("(no ref changes)".to_string());
    }

    for change in changes {
        let kind = value_string(get(Some(change), "type"), "?");
        let reference = normalize_ref_option(get(Some(change), "ref"));
        let after = get(Some(change), "after").and_then(as_object);

        match kind.as_str() {
            "add" | "remove" => {
                let role = value_string(
                    first_defined(get(Some(change), "role"), get(after, "role")),
                    "",
                );
                let name =
                    quoted_name(first_defined(get(Some(change), "name"), get(after, "name")));
                let sign = if kind == "add" { "+" } else { "-" };
                lines.push(format!("{sign} {reference}{}{name}", role_suffix(&role),));
            }
            "replace" => {
                let before = get(Some(change), "before").and_then(as_object);
                lines.push(format!(
                    "~ {reference} {}{} (was {}{})",
                    value_string(get(after, "role"), "?"),
                    quoted_name(get(after, "name")),
                    value_string(get(before, "role"), "?"),
                    quoted_name(get(before, "name")),
                ));
            }
            _ => {}
        }
    }

    let splice_lines: Vec<&str> = array_of(tree_change.and_then(|change| change.get("lines")))
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !splice_lines.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Tree splice (from line {}, remove {}):",
            number_text(value_number(
                tree_change.and_then(|change| change.get("startLine")),
                0.0,
            )),
            number_text(value_number(
                tree_change.and_then(|change| change.get("deleteCount")),
                0.0,
            )),
        ));
        lines.extend(splice_lines.iter().map(|line| (*line).to_string()));
    }

    lines.join("\n")
}

fn describe_tab_object(tab: Option<&Map<String, Value>>) -> String {
    let id = value_number_or_null(first_defined(get(tab, "id"), get(tab, "tabId")));
    let title = non_empty_or(value_string(get(tab, "title"), ""), "(untitled)");
    let url = value_string(get(tab, "url"), "");
    let id = optional_number_text(id);
    if url.is_empty() {
        format!("[{id}] {title}")
    } else {
        format!("[{id}] {title} — {url}")
    }
}

fn role_suffix(role: &str) -> String {
    if role.is_empty() {
        String::new()
    } else {
        format!(" {role}")
    }
}

/// JSON-stringify a name for `"..."` output; empty/non-string names vanish.
fn quoted_name(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(name)) if !name.is_empty() => format!(" {}", json_quote(name)),
        _ => String::new(),
    }
}

// ── Value-shape helpers (mirror of the legacy "small runtime-shape helpers") ─

fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn get<'a>(object: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Value> {
    object.and_then(|object| object.get(key))
}

fn array_of(value: Option<&Value>) -> &[Value] {
    match value {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

fn first_defined<'a>(first: Option<&'a Value>, second: Option<&'a Value>) -> Option<&'a Value> {
    match first {
        Some(value) if !value.is_null() => Some(value),
        _ => second,
    }
}

fn value_string(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        _ => fallback.to_string(),
    }
}

/// `stringValue(value, "") || fallback`
fn non_empty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

/// `Number(value)`/`Number.isFinite` pair behind the legacy `numberValue`.
fn value_number(value: Option<&Value>, fallback: f64) -> f64 {
    match js_number(value) {
        Some(number) if number.is_finite() => number,
        _ => fallback,
    }
}

/// Legacy `numberOrNull`: `null`/`undefined`/`""` -> `None`, else finite `Number`.
fn value_number_or_null(value: Option<&Value>) -> Option<f64> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.is_empty() => None,
        Some(_) => js_number(value).filter(|number| number.is_finite()),
    }
}

fn optional_number_text(value: Option<f64>) -> String {
    value.map(number_text).unwrap_or_else(|| "?".to_string())
}

/// JavaScript `Number(value)`; `None` means `NaN`.
fn js_number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Null => Some(0.0),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64(),
        Value::String(text) => parse_js_number(text),
        // Number([]) === 0, Number([x]) === Number(x), longer arrays are NaN.
        Value::Array(items) => match items.as_slice() {
            [] => Some(0.0),
            [item] => js_number(Some(item)),
            _ => None,
        },
        Value::Object(_) => None,
    }
}

/// JavaScript `Number(string)` for the subset of spellings the bridge can see.
fn parse_js_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some(0.0);
    }
    match trimmed {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }

    // Non-decimal literals do not accept a leading sign in JavaScript.
    if !trimmed.starts_with(['+', '-']) {
        if let Some(hex) = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
        {
            return u128::from_str_radix(hex, 16).ok().map(|value| value as f64);
        }
        if let Some(binary) = trimmed
            .strip_prefix("0b")
            .or_else(|| trimmed.strip_prefix("0B"))
        {
            return u128::from_str_radix(binary, 2)
                .ok()
                .map(|value| value as f64);
        }
        if let Some(octal) = trimmed
            .strip_prefix("0o")
            .or_else(|| trimmed.strip_prefix("0O"))
        {
            return u128::from_str_radix(octal, 8)
                .ok()
                .map(|value| value as f64);
        }
    }

    // Reject spellings Rust's float parser accepts but JavaScript does not
    // (`inf`, `NaN`, `1_000`, whitespace inside, ...).
    let decimal = trimmed
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'));
    if !decimal {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

/// JavaScript `String(value)`.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::Number(number)) => number_text(number.as_f64().unwrap_or(f64::NAN)),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// JavaScript `String(number)` / template interpolation of a number.
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
        // JavaScript has no "-0" string form.
        return "0".to_string();
    }
    if number.fract() == 0.0 && number.abs() < 1e21 {
        return format!("{number:.0}");
    }
    // JavaScript switches to exponential notation outside [1e-6, 1e21).
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

// ── Ref helpers ──────────────────────────────────────────────────────────

fn normalize_ref_option(value: Option<&Value>) -> String {
    value.map(normalize_ref).unwrap_or_default()
}

fn normalize_ref_str(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else if is_e_digits(value) {
        format!("@{value}")
    } else {
        value.to_string()
    }
}

fn is_e_digits(value: &str) -> bool {
    match value.strip_prefix('e') {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        None => false,
    }
}

/// Legacy `/^@?e\d+$/` test applied to a bare string fallback.
fn is_ref_pattern(value: &str) -> bool {
    is_e_digits(value.strip_prefix('@').unwrap_or(value))
}

/// Legacy `JSON.stringify(text)` for an attribute/name string.
fn json_quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| format!("\"{text}\""))
}

/// `JSON.stringify(clipInline(text, max))`, including the legacy quirk where a
/// cut inside a surrogate pair leaves a lone high surrogate that
/// `JSON.stringify` emits as a `\udXXX` escape sequence.
fn quoted_clipped_inline(text: &str, max: usize) -> String {
    if utf16_len(text) <= max {
        return json_quote(text);
    }
    let (prefix, dangling_high) = split_utf16_prefix(text, max);
    let quoted_prefix = json_quote(&prefix);
    let mut quoted = quoted_prefix[1..quoted_prefix.len() - 1].to_string();
    if let Some(high) = dangling_high {
        quoted.push_str(&format!("\\u{high:04x}"));
    }
    quoted.push('…');
    format!("\"{quoted}\"")
}

/// Split `text` after `max` UTF-16 code units. When the cut lands between the
/// two units of a surrogate pair, the dangling high surrogate is reported so
/// callers can mirror JavaScript behavior instead of losing it.
fn split_utf16_prefix(text: &str, max: usize) -> (String, Option<u16>) {
    let mut prefix = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let width = character.len_utf16();
        if used + width > max {
            if width == 2 && used + 1 == max {
                let code = character as u32;
                let high = 0xD800u16 + ((code - 0x1_0000) >> 10) as u16;
                return (prefix, Some(high));
            }
            return (prefix, None);
        }
        prefix.push(character);
        used += width;
        if used == max {
            return (prefix, None);
        }
    }
    (prefix, None)
}

// ── Text helpers ─────────────────────────────────────────────────────────

/// JavaScript `String.prototype.length` counts UTF-16 code units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// First `max` UTF-16 code units, matching `String.prototype.slice(0, max)`.
///
/// A cut inside a surrogate pair would produce a lone high surrogate in JS;
/// Node/Rust both write that as U+FFFD when emitting UTF-8, so mirror the
/// emitted bytes by appending the replacement character.
fn utf16_prefix(text: &str, max: usize) -> String {
    let mut prefix = String::new();
    let mut used = 0usize;
    for character in text.chars() {
        let width = character.len_utf16();
        if used + width > max {
            if width == 2 && used + 1 == max {
                prefix.push('\u{FFFD}');
            }
            return prefix;
        }
        prefix.push(character);
        used += width;
        if used == max {
            return prefix;
        }
    }
    prefix
}

/// `toLocaleString("en-US")` for the integer character caps used by clip notes.
fn format_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}
