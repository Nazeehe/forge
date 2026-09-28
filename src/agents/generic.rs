//! Generic fallback adapter for user-added agents.json entries.
//!
//! Unknown agents get MCP server instructions only, never a guessed argv
//! shape: no launch-time transport is emitted.

use super::AgentAdapter;

/// Stateless fallback: no launch extras, no hook/MCP/skills surface.
pub struct GenericAdapter;

impl AgentAdapter for GenericAdapter {
    fn name(&self) -> &'static str {
        "unknown"
    }
}
