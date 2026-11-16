//! Formatter parity tests for `rust/src/format.rs`.
//!
//! Expected strings are transcribed from the legacy TypeScript formatters
//! (`browser-use-legacy/src/tools.ts`, "Formatting" section) and its test
//! suite (`browser-use-legacy/test/tools.test.ts`). These assert byte-for-byte
//! legacy output parity, including truncation notes and ordering.

use serde_json::{json, Value};

use browser_use::format::{
    clip_inline, clip_snapshot, clip_text, describe_element, describe_tab, format_json,
    format_requests, format_snapshot, format_tabs, normalize_ref,
};

// ── format_snapshot ──────────────────────────────────────────────────────

#[test]
fn format_snapshot_full_matches_legacy_header_and_tree() {
    let result = json!({
        "title": "Example",
        "url": "https://example.com",
        "tabId": 12,
        "revision": 3,
        "kind": "full",
        "tree": "- button \"Submit\" [ref=e4]\n- link \"Docs\" [ref=e5]",
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: Example | URL: https://example.com | Tab: 12 | Revision: 3\n\n",
            "- button \"Submit\" [ref=e4]\n",
            "- link \"Docs\" [ref=e5]",
        )
    );
}

#[test]
fn format_snapshot_defaults_missing_fields() {
    assert_eq!(
        format_snapshot(&json!({})),
        "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\n(empty snapshot)"
    );
    assert_eq!(
        format_snapshot(&json!({ "title": "", "url": "", "tree": "" })),
        "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\n(empty snapshot)"
    );
}

#[test]
fn format_snapshot_renders_numeric_strings_like_numbers() {
    let result = json!({
        "title": "T",
        "url": "u",
        "tabId": "12",
        "revision": "3.5",
        "tree": "x",
    });

    assert_eq!(
        format_snapshot(&result),
        "Title: T | URL: u | Tab: 12 | Revision: 3.5\n\nx"
    );
}

#[test]
fn format_snapshot_unchanged_matches_legacy() {
    let result = json!({
        "title": "Example",
        "url": "https://example.com",
        "tabId": 12,
        "baseRevision": 3,
        "revision": 4,
        "kind": "unchanged",
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: Example | URL: https://example.com | Tab: 12 | Revision: 4\n\n",
            "No changes since revision 3.",
        )
    );

    // Missing baseRevision interpolates "?"; baseRevision itself renders like
    // a JavaScript template interpolation (strings pass through).
    let without_base = json!({ "kind": "unchanged" });
    assert_eq!(
        format_snapshot(&without_base),
        "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\nNo changes since revision ?."
    );
    let string_base = json!({ "kind": "unchanged", "baseRevision": "7" });
    assert!(format_snapshot(&string_base).ends_with("No changes since revision 7."));
}

#[test]
fn format_snapshot_delta_matches_legacy() {
    let result = json!({
        "title": "Example",
        "url": "https://example.com",
        "tabId": 12,
        "baseRevision": 3,
        "revision": 4,
        "kind": "delta",
        "changes": [
            { "type": "add", "ref": "e9", "role": "button", "name": "Save" },
            { "type": "remove", "ref": "e4", "role": "button", "name": "Submit" },
            {
                "type": "replace",
                "ref": "e5",
                "before": { "role": "link", "name": "Old" },
                "after": { "role": "link", "name": "New" },
            },
        ],
        "treeChange": {
            "startLine": 4,
            "deleteCount": 1,
            "lines": ["  - button \"Save\" [ref=e9]"],
        },
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: Example | URL: https://example.com | Tab: 12 | Revision: 4\n",
            "\n",
            "Changes:\n",
            "+ @e9 button \"Save\"\n",
            "- @e4 button \"Submit\"\n",
            "~ @e5 link \"New\" (was link \"Old\")\n",
            "\n",
            "Tree splice (from line 4, remove 1):\n",
            "  - button \"Save\" [ref=e9]",
        )
    );
}

#[test]
fn format_snapshot_delta_without_changes() {
    let result = json!({
        "title": "T",
        "url": "u",
        "tabId": 1,
        "revision": 2,
        "kind": "delta",
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: T | URL: u | Tab: 1 | Revision: 2\n\n",
            "Changes:\n",
            "(no ref changes)",
        )
    );
}

#[test]
fn format_snapshot_delta_uses_after_fields_and_normalizes_refs() {
    let result = json!({
        "kind": "delta",
        "changes": [
            { "type": "add", "ref": "e7", "after": { "role": "textbox", "name": "Email" } },
            { "type": "remove", "ref": "@e8", "name": "Delete", "after": { "role": "button" } },
            { "type": "replace", "ref": "e9", "after": { "role": "button" } },
            { "type": "replace", "ref": "e10", "before": { "role": "link", "name": "Old" } },
            { "type": "unknown", "ref": "e11", "role": "ignored" },
        ],
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\n",
            "Changes:\n",
            "+ @e7 textbox \"Email\"\n",
            "- @e8 button \"Delete\"\n",
            "~ @e9 button (was ?)\n",
            "~ @e10 ? (was link \"Old\")",
        )
    );
}

#[test]
fn format_snapshot_delta_splice_defaults_and_filters_non_strings() {
    let result = json!({
        "kind": "delta",
        "treeChange": { "lines": ["a", 1, null] },
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\n",
            "Changes:\n",
            "(no ref changes)\n",
            "\n",
            "Tree splice (from line 0, remove 0):\n",
            "a",
        )
    );
}

#[test]
fn format_snapshot_landmarks_collapsed_note_matches_legacy() {
    let result = json!({
        "title": "Amazon",
        "url": "https://www.amazon.com/s?k=dog",
        "tabId": 3,
        "revision": 8,
        "kind": "full",
        "landmarksCollapsed": 63,
        "tree": "- navigation \"Main menu\"\n  ... 63 more navigation items omitted",
    });

    assert_eq!(
        format_snapshot(&result),
        concat!(
            "Title: Amazon | URL: https://www.amazon.com/s?k=dog | Tab: 3 | Revision: 8\n",
            "Note: 63 navigation/header/footer items were collapsed to keep the snapshot small; ",
            "pass selector=\"nav\" or selector=\"main\" to inspect them.\n",
            "\n",
            "- navigation \"Main menu\"\n",
            "  ... 63 more navigation items omitted",
        )
    );

    // Numeric strings are converted through Number() like the legacy code.
    let as_string = json!({ "landmarksCollapsed": "12", "tree": "t" });
    assert!(format_snapshot(&as_string).starts_with(
        "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\
         Note: 12 navigation/header/footer items were collapsed"
    ));

    // Zero/false-y values produce no note.
    for collapsed in [json!(0), json!(null), json!(""), json!("nope")] {
        let result = json!({ "tree": "t", "landmarksCollapsed": collapsed });
        assert!(
            !format_snapshot(&result).contains("navigation/header/footer"),
            "unexpected note for {collapsed}"
        );
    }
}

#[test]
fn format_snapshot_empty_kind_still_renders_full() {
    let result = json!({ "kind": "", "tree": "tree" });
    assert_eq!(
        format_snapshot(&result),
        "Title: (untitled) | URL: (no url) | Tab: ? | Revision: ?\n\ntree"
    );
}

// ── format_tabs / describe_tab ───────────────────────────────────────────

#[test]
fn format_tabs_matches_legacy_markers_and_flags() {
    let result = json!({
        "tabs": [
            { "id": 1, "active": true, "title": "One", "url": "https://one.example" },
            { "id": 2, "active": false, "title": "Two", "url": "https://two.example", "pinned": true },
            { "tabId": 3, "status": "loading" },
            { "id": 4, "active": true, "title": "Four", "pinned": true, "status": "loading" },
            { "title": "", "url": "" },
        ],
    });

    assert_eq!(
        format_tabs(&result),
        concat!(
            "Tabs (5):\n",
            "* [1] One — https://one.example\n",
            "  [2] Two — https://two.example (pinned)\n",
            "  [3] (untitled) (loading)\n",
            "* [4] Four (pinned, loading)\n",
            "  [?] (untitled)",
        )
    );
}

#[test]
fn format_tabs_empty_and_non_object_entries() {
    assert_eq!(format_tabs(&json!({})), "No open tabs.");
    assert_eq!(format_tabs(&json!({ "tabs": [] })), "No open tabs.");
    assert_eq!(
        format_tabs(&json!({ "tabs": [null, "x", { "id": 1, "title": "One" }] })),
        "Tabs (1):\n  [1] One"
    );
}

#[test]
fn describe_tab_cases() {
    assert_eq!(
        describe_tab(&json!({ "id": 1, "title": "One", "url": "https://one.example" })),
        "[1] One — https://one.example"
    );
    assert_eq!(
        describe_tab(&json!({ "tabId": 7, "title": "Seven" })),
        "[7] Seven"
    );
    assert_eq!(describe_tab(&json!({})), "[?] (untitled)");
    assert_eq!(
        describe_tab(&json!({ "id": "3", "title": "Three", "url": "u" })),
        "[3] Three — u"
    );
}

// ── describe_element ─────────────────────────────────────────────────────

#[test]
fn describe_element_matches_legacy() {
    let cases: Vec<(Value, Option<&str>, &str)> = vec![
        (
            json!({ "ref": "e4", "role": "button", "name": "Submit" }),
            Some("@e4"),
            "@e4 button \"Submit\"",
        ),
        (json!({}), Some("#fallback"), "#fallback"),
        (json!({}), Some("e9"), "@e9"),
        (json!({}), Some("@e12"), "@e12"),
        (json!({}), Some(""), "element"),
        (json!({}), None, "element"),
        (json!({ "ref": "@e3" }), None, "@e3"),
        (json!({ "ref": "" }), Some("#x"), "#x"),
        (json!({ "ref": 12 }), Some("#n"), "#n"),
        (
            json!({ "role": "link", "name": "Docs" }),
            Some("#x"),
            "link \"Docs\"",
        ),
        (
            json!({ "name": "A \"quote\"" }),
            None,
            "\"A \\\"quote\\\"\"",
        ),
        (json!({ "ref": "e1", "role": "", "name": "" }), None, "@e1"),
    ];

    for (result, fallback, expected) in cases {
        assert_eq!(
            describe_element(&result, fallback),
            expected,
            "case: result={result} fallback={fallback:?}"
        );
    }
}

#[test]
fn describe_element_clips_names_at_120_units() {
    let clipped_name = format!("{}…", "a".repeat(120));
    let expected = format!("button {}", serde_json::to_string(&clipped_name).unwrap());
    let result = json!({ "role": "button", "name": "a".repeat(130) });
    assert_eq!(describe_element(&result, None), expected);

    // Exactly 120 units is not clipped.
    let exact = "b".repeat(120);
    let expected = format!("button {}", serde_json::to_string(&exact).unwrap());
    let result = json!({ "role": "button", "name": exact });
    assert_eq!(describe_element(&result, None), expected);
}

#[test]
fn describe_element_escapes_lone_surrogate_like_legacy_json_stringify() {
    // "a"*119 + "😀" is 121 UTF-16 units, so clipInline cuts through the
    // surrogate pair; legacy `JSON.stringify` then emits the dangling high
    // surrogate as a literal `\ud83d` escape before the ellipsis.
    let name = format!("{}😀", "a".repeat(119));
    let result = json!({ "role": "button", "name": name });
    assert_eq!(
        describe_element(&result, None),
        format!("button \"{}\\ud83d…\"", "a".repeat(119))
    );
}

// ── format_requests ──────────────────────────────────────────────────────

#[test]
fn format_requests_matches_legacy_rendering() {
    let result = json!({
        "count": 4,
        "requests": [
            { "method": "GET", "status": 200, "timeMs": 12, "url": "https://a.example" },
            { "status": null, "url": "https://b.example" },
            {
                "method": "POST",
                "status": 500,
                "timeMs": 40,
                "error": "net::ERR_FAILED",
                "url": "https://c.example",
            },
            { "status": "301", "timeMs": "5", "url": "" },
        ],
    });

    assert_eq!(
        format_requests(&result),
        concat!(
            "Requests (4):\n",
            "200 GET 12ms https://a.example\n",
            "--- GET https://b.example\n",
            "500 POST 40ms https://c.example ERROR net::ERR_FAILED\n",
            "301 GET 5ms",
        )
    );
}

#[test]
fn format_requests_defaults_and_empty() {
    assert_eq!(
        format_requests(&json!({})),
        "No matching requests captured."
    );
    assert_eq!(
        format_requests(&json!({ "requests": [] })),
        "No matching requests captured."
    );

    // Missing count falls back to the array length; an explicit 0 is kept.
    let result = json!({ "requests": [{ "url": "u1" }, { "url": "u2" }] });
    assert_eq!(
        format_requests(&result),
        "Requests (2):\n--- GET u1\n--- GET u2"
    );
    let zero = json!({ "count": 0, "requests": [{ "url": "u1" }] });
    assert_eq!(format_requests(&zero), "Requests (0):\n--- GET u1");
}

#[test]
fn format_requests_caps_at_200() {
    let requests: Vec<Value> = (0..201)
        .map(|index| json!({ "status": 200, "url": format!("https://example.com/{index}") }))
        .collect();
    let result = json!({ "count": 201, "requests": requests });

    let text = format_requests(&result);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 202);
    assert_eq!(lines[0], "Requests (201):");
    assert_eq!(lines[1], "200 GET https://example.com/0");
    assert_eq!(lines[200], "200 GET https://example.com/199");
    assert_eq!(lines[201], "... 1 more requests omitted");
    assert!(!text.contains("https://example.com/200"));
}

// ── clip_* ───────────────────────────────────────────────────────────────

#[test]
fn clip_text_matches_legacy_note() {
    assert_eq!(clip_text("abc", 3), "abc");
    assert_eq!(clip_text("abc", 4), "abc");
    assert_eq!(
        clip_text("abcdef", 3),
        "abc\n\n[truncated at 3 characters for context efficiency]"
    );
    assert_eq!(clip_text("abcdef", 12_000), "abcdef");
}

#[test]
fn clip_snapshot_matches_legacy_note_with_thousands_separators() {
    let text = "x".repeat(12_010);
    let expected = format!(
        "{}\n\n[truncated to 12,000 chars; pass selector=\"...\" or interactive=true to inspect a specific section]",
        "x".repeat(12_000)
    );
    assert_eq!(clip_snapshot(&text, 12_000), expected);

    assert_eq!(clip_snapshot("short", 12_000), "short");
    assert!(clip_snapshot(&"y".repeat(80_001), 80_000).contains("[truncated to 80,000 chars;"));
    assert!(clip_snapshot(&"z".repeat(2_001), 2_000).contains("[truncated to 2,000 chars;"));
    assert!(clip_snapshot(&"z".repeat(1_000), 999).contains("[truncated to 999 chars;"));
}

#[test]
fn clip_inline_matches_legacy_ellipsis() {
    assert_eq!(clip_inline("abcdef", 3), "abc…");
    assert_eq!(clip_inline("abc", 3), "abc");
    assert_eq!(clip_inline("", 0), "");
    assert_eq!(clip_inline("abcdef", 0), "…");
}

#[test]
fn clips_count_utf16_code_units_like_javascript() {
    // "a😀b" is 4 UTF-16 code units but only 3 Rust chars.
    assert_eq!(clip_text("a😀b", 4), "a😀b");
    assert_eq!(
        clip_text("a😀b", 3),
        "a😀\n\n[truncated at 3 characters for context efficiency]"
    );
    // Cutting a surrogate pair mid-way emits U+FFFD, exactly like Node
    // encoding the lone surrogate produced by JS slice(0, max).
    assert_eq!(
        clip_text("a😀b", 2),
        "a\u{FFFD}\n\n[truncated at 2 characters for context efficiency]"
    );
    assert_eq!(clip_inline("a😀b", 1), "a…");
    assert_eq!(clip_inline("😀", 1), "\u{FFFD}…");
}

// ── normalize_ref ────────────────────────────────────────────────────────

#[test]
fn normalize_ref_cases() {
    let cases: Vec<(Value, &str)> = vec![
        (json!("e12"), "@e12"),
        (json!("e0"), "@e0"),
        (json!("e007"), "@e007"),
        (json!("E12"), "E12"),
        (json!("@e12"), "@e12"),
        (json!("e"), "e"),
        (json!("e12x"), "e12x"),
        (json!("ee12"), "ee12"),
        (json!(""), ""),
        (json!(12), ""),
        (json!(null), ""),
        (json!(["e12"]), ""),
    ];

    for (value, expected) in cases {
        assert_eq!(normalize_ref(&value), expected, "case: {value}");
    }
}

// ── format_json (bonus helper used by tools.rs) ──────────────────────────

#[test]
fn format_json_matches_legacy() {
    assert_eq!(format_json(&json!("already text")), "already text");
    assert_eq!(format_json(&json!({ "a": 1 })), "{\n  \"a\": 1\n}");
    assert_eq!(
        format_json(&json!([1, "two", true, null])),
        "[\n  1,\n  \"two\",\n  true,\n  null\n]"
    );
    assert_eq!(format_json(&json!(null)), "null");
    assert_eq!(format_json(&json!(1.5)), "1.5");
}
