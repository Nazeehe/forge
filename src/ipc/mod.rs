//! IPC: the TUI listener and the MCP stdio server.
//!
//! Short-lived harness processes reach the TUI broker through the Unix
//! socket / loopback TCP listener, or speak newline-delimited JSON-RPC
//! over stdio via `mcp-serve`.

pub mod listener;
pub mod mcp;
