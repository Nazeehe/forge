//! Agy adapter: runtime injection for the agy CLI.

use std::path::Path;

use super::runtime::{FORGE_RUNTIME_CONTRACT, RuntimeMechanism};
use super::AgentAdapter;

/// Stateless adapter for the `agy` CLI. Agy exposes no hook surface
/// (see `supports_hooks` in the registry); it still carries the runtime
/// contract as a startup prompt.
pub struct AgyAdapter;

impl AgentAdapter for AgyAdapter {
    fn name(&self) -> &'static str {
        "agy"
    }

    fn runtime_mechanism(&self) -> RuntimeMechanism {
        RuntimeMechanism::StartupPromptPositional
    }

    fn runtime_injection(&self, _runtime_file: &Path) -> Vec<String> {
        vec![FORGE_RUNTIME_CONTRACT.to_string()]
    }
}
