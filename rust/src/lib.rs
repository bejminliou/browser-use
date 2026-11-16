//! browser-use: localhost WebSocket bridge + stdio MCP server for the
//! Gecko Orange (Firefox/Zen) extension.
//!
//! Module dependency DAG (never import "up" the list):
//! secret -> hub -> peer -> format -> tools -> mcp -> cli -> ffi(-> secret/hub)
//! Keep imports inside this DAG.

pub mod cli;
pub mod ffi;
pub mod format;
pub mod hub;
pub mod mcp;
pub mod peer;
pub mod secret;
pub mod tools;

/// Crate version reported by the MCP server and `browser-use --version`.
pub const VERSION: &str = "0.1.0";

/// Server name reported by the MCP server and used in log prefixes.
pub const SERVER_NAME: &str = "browser-use";
