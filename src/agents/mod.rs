//! Agent adapter layer: registry data plus per-agent behavior.
//!
//! `agents.json` stays the data (binary, flags, resume); adapters hold the
//! behavior needing code (runtime injection, hook/MCP/skills install,
//! verdict rendering). Adding an agent is one `agents.json` entry plus one
//! adapter file. [`adapter_for`] / [`all`] is the only place agent name
//! strings are matched; core code never matches on agent names.

use std::path::{Path, PathBuf};

pub mod agy;
pub mod claude;
pub mod codex;
pub mod generic;
pub mod harness;
pub mod muse;
pub mod registry;
pub mod runtime;

/// Per-agent behavior needing code. Defaults are the safe fallback
/// (no launch transport, skipped installers, no skills dir, the shared
/// explicit verdict shape); packaged adapters override what they support.
pub trait AgentAdapter: Sync {
    /// Registry id (`cli_tool` string).
    fn name(&self) -> &'static str;

    /// Strongest supported injection transport for the runtime contract.
    fn runtime_mechanism(&self) -> runtime::RuntimeMechanism {
        runtime::RuntimeMechanism::McpInstructionsOnly
    }

    /// Extra argv (after the base launch argv) carrying the contract.
    /// Empty for [`runtime::RuntimeMechanism::McpInstructionsOnly`].
    fn runtime_injection(&self, _runtime_file: &Path) -> Vec<String> {
        Vec::new()
    }

    /// Extra argv for a *resume* launch carrying the contract. Empty
    /// unless the agent has a verified resume-safe transport. Codex
    /// stays empty (`-c developer_instructions` on `codex resume` is
    /// unverified); muse and agy stay empty (their contract is a
    /// positional prompt, which on resume would arrive as a new user
    /// turn instead of instructions). Claude overrides: it rebuilds
    /// its system prompt on every launch, resume included.
    fn resume_runtime_injection(&self, _runtime_file: &Path) -> Vec<String> {
        Vec::new()
    }

    /// Install harness hooks. Skipped by default (no grounded surface).
    /// Callers label the outcome with the registry name; the default
    /// uses the adapter name only as a fallback.
    fn hook_install(
        &self,
        _home: &Path,
        _forge_bin: &str,
    ) -> crate::hooks::install::Outcome {
        crate::hooks::install::Outcome::skipped(
            self.name(),
            "no grounded hook surface for this agent".to_string(),
        )
    }

    /// Remove harness hooks. Skipped by default.
    fn hook_uninstall(&self, _home: &Path) -> crate::hooks::install::Outcome {
        crate::hooks::install::Outcome::skipped(
            self.name(),
            "no grounded hook surface for this agent".to_string(),
        )
    }

    /// Register the forge MCP server. Skipped by default.
    fn mcp_install(
        &self,
        _home: &Path,
        _forge_bin: &str,
    ) -> crate::hooks::install::Outcome {
        crate::hooks::install::Outcome::skipped(
            self.name(),
            "no grounded MCP surface for this agent".to_string(),
        )
    }

    /// Remove the forge MCP server. Skipped by default.
    fn mcp_uninstall(&self, _home: &Path) -> crate::hooks::install::Outcome {
        crate::hooks::install::Outcome::skipped(
            self.name(),
            "no grounded MCP surface for this agent".to_string(),
        )
    }

    /// Directory holding the forge skill, if the harness has a grounded
    /// skills path. `None` by default.
    fn skills_dir(&self, _home: &Path) -> Option<PathBuf> {
        None
    }

    /// True when forge hooks are registered for this agent. Read-only:
    /// missing or corrupt config reads as not installed, never an error.
    /// Default true: agents without a hook surface need no repair.
    fn hooks_installed(&self, _home: &Path) -> bool {
        true
    }

    /// True when the forge MCP server is registered for this agent.
    /// Same fail-closed read as [`AgentAdapter::hooks_installed`].
    fn mcp_installed(&self, _home: &Path) -> bool {
        true
    }

    /// True when the agent needs no repair before launch: hooks and MCP
    /// both present (or both surfaceless). The create-time check.
    fn is_setup(&self, home: &Path) -> bool {
        self.hooks_installed(home) && self.mcp_installed(home)
    }

    /// One-line verdict for the relay to print to the harness. Defaults to
    /// the shared explicit shape; codex overrides (see `codex.rs` in step 3).
    fn render_decision(
        &self,
        hook: &str,
        decision: crate::hooks::policy::Decision,
        reason: &str,
    ) -> String {
        crate::hooks::policy::decision_line(hook, decision, reason)
    }
}

static CLAUDE: claude::ClaudeAdapter = claude::ClaudeAdapter;
static CODEX: codex::CodexAdapter = codex::CodexAdapter;
static MUSE: muse::MuseAdapter = muse::MuseAdapter;
static AGY: agy::AgyAdapter = agy::AgyAdapter;
static GENERIC: generic::GenericAdapter = generic::GenericAdapter;

/// The only place agent name strings are matched. Unknown names resolve
/// to the generic fallback (MCP instructions only, never a guessed shape).
pub fn adapter_for(name: &str) -> &'static dyn AgentAdapter {
    match name {
        "claude" => &CLAUDE,
        "codex" => &CODEX,
        "muse" => &MUSE,
        "agy" => &AGY,
        _ => &GENERIC,
    }
}

/// Every registered agent's adapter, in `agents.json` file order (the
/// single source of order; drives OOBE and installer aggregates).
/// User-added registry entries resolve to the generic fallback.
pub fn all() -> Vec<&'static dyn AgentAdapter> {
    harness::Harness::all()
        .into_iter()
        .map(|h| adapter_for(h.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_names_match_registry_order() {
        let names: Vec<_> = all().iter().map(|a| a.name()).collect();
        assert_eq!(names, ["claude", "codex", "muse", "agy"]);
    }

    #[test]
    fn unknown_agents_fall_back_to_generic() {
        let adapter = adapter_for("something-custom");
        assert_eq!(adapter.name(), "unknown");
        assert_eq!(
            adapter.runtime_mechanism(),
            runtime::RuntimeMechanism::McpInstructionsOnly
        );
        assert!(adapter
            .runtime_injection(std::path::Path::new("/home/tester/.forge/runtime.md"))
            .is_empty());
    }

    #[test]
    fn packaged_injections_match_verified_transports() {
        let file = Path::new("/home/tester/.forge/runtime.md");
        assert_eq!(
            adapter_for("claude").runtime_injection(file),
            vec![
                "--append-system-prompt-file".to_string(),
                "/home/tester/.forge/runtime.md".to_string(),
            ]
        );
        let codex = adapter_for("codex").runtime_injection(file);
        assert_eq!(codex.len(), 2);
        assert_eq!(codex[0], "-c");
        assert!(codex[1].starts_with("developer_instructions=\""));
        for name in ["muse", "agy"] {
            let extras = adapter_for(name).runtime_injection(file);
            assert!(
                extras.len() == 1 && extras[0].contains("Forge"),
                "{name} must carry the runtime contract: {extras:?}"
            );
        }
    }
}
