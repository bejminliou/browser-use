# browser-use

A localhost WebSocket bridge, stdio MCP server, and native macOS menu bar app
for driving Gecko-based browsers (Firefox / Zen).

```text
Browser Use.app ── in-process hub ── ws://127.0.0.1:19455 ── Firefox / Zen
      │                   ▲
      │ links             │ ws /peer (lazy reconnect)
      │ libbrowser_use.a  │
      ▼                   │
  browser-use  ───────────┘
  (CLI + stdio MCP server)
```

- **`Browser Use.app`** hosts the loopback HTTP/WebSocket hub in-process via
  `libbrowser_use.a`. App open = hub on; quit = hub off.
- **`browser-use`** is the bundled CLI and stdio MCP server (`Contents/MacOS/browser-use`),
  lazily reconnecting to the app across restarts.

## Requirements

- macOS 15+, Swift 6 (Xcode 16+)
- Rust 1.80+ (`cargo`)
- [`just`](https://github.com/casey/just)
- Firefox or Zen configured to connect over the local WebSocket protocol (`3.0.0`)

## Build & Install

```bash
just install        # release build -> /Applications/Browser Use.app + ~/.local/bin/browser-use
just build          # release build only -> build/Browser Use.app
just test           # cargo test + release bundle + app sanity checks
```

Dev loop:

```bash
just reload         # debug Rust + Swift build, restart Browser Use.app, wait for /health
just dev-cli        # point ~/.local/bin/browser-use at target/debug/browser-use
```

Other recipes: `just verify`, `just verify-menu`, `just snapshot`, `just icons`, `just stop`, `just clean`.

## Usage

Launch the app (`open "/Applications/Browser Use.app"` or `just reload`), then point your MCP client at the bundled binary:

```bash
"/Applications/Browser Use.app/Contents/MacOS/browser-use"
```

### CLI

```bash
browser-use status                      # hub, browser connection, and open tabs
browser-use secret                      # print/initialize the shared secret
browser-use snap [-i] [-c]              # accessibility snapshot
browser-use click <@ref|selector>       # click an element
browser-use fill <@ref|selector> <text> # fill a field
browser-use read [url]                  # read page as Markdown
browser-use --help                      # full flags & options
```

## Layout

- `rust/` — Core library (`secret`, `hub`, `peer`, `format`, `tools`, `mcp`, `cli`, `ffi`) + C-ABI header (`rust/include/browser_use.h`)
- `macos/` — SwiftPM menu bar app (`Shell/`, `Kit/`, `DevTools/`, `BrowserUse/`) linking `libbrowser_use.a`