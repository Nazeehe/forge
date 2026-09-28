//! Muse adapter: runtime injection and installer surfaces for the muse CLI.

use std::path::Path;

use super::runtime::{FORGE_RUNTIME_CONTRACT, RuntimeMechanism};
use super::AgentAdapter;

/// Stateless adapter for the `muse` CLI.
pub struct MuseAdapter;

/// Drop forge from muse 1.2.1's `mcp_servers` block (and the block itself
/// once empty): muse 1.4 rejects the key. True when anything was removed.
fn remove_stale_muse_mcp(v: &mut serde_json::Value) -> bool {
    let Some(root) = v.as_object_mut() else {
        return false;
    };
    let Some(servers) = root.get_mut("mcp_servers").and_then(|m| m.as_object_mut()) else {
        return false;
    };
    let removed = servers.remove("forge").is_some();
    if servers.is_empty() {
        root.remove("mcp_servers");
    }
    removed
}

impl AgentAdapter for MuseAdapter {
    fn name(&self) -> &'static str {
        "muse"
    }

    fn runtime_mechanism(&self) -> RuntimeMechanism {
        RuntimeMechanism::StartupPromptPositional
    }

    fn runtime_injection(&self, _runtime_file: &Path) -> Vec<String> {
        vec![FORGE_RUNTIME_CONTRACT.to_string()]
    }

    fn hook_install(&self, home: &Path, forge_bin: &str) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{
            Outcome, hook_command, merge_hook_groups, read_json, write_json,
        };
        let harness = self.name();
        // Grounded in muse 1.2.1, verified live: the user `hooks`
        // block in ~/.config/muse/settings.json takes the same
        // Claude-style shape ({Event: [{matcher, hooks}]}), and
        // SessionStart, PreToolUse, UserPromptSubmit, Stop, and
        // SessionEnd all fire with hook_event_name, session_id, and
        // cwd on stdin. PermissionRequest is left out: its blocking
        // semantics are unverified and a wrong verdict would gate
        // every tool call. Hook children run with a cleared
        // environment, so hook-relay resolves the TUI through the
        // endpoint file and the loop attributes by harness session ID.
        let path = home.join(".config/muse/settings.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        // Same turn edges as claude/codex (see above), plus
        // SessionStart (the harness-side conversation ID the restore
        // path resumes with) and SessionEnd (orderly termination).
        // `timeout` caps a wedged relay on the harness side; the
        // relay itself gives up after 3 s and always exits 0, so
        // 10 s never blocks a session.
        let handler = serde_json::json!({
            "type": "command",
            "command": hook_command(forge_bin),
            "timeout": 10,
        });
        let added = match merge_hook_groups(
            &mut v,
            &path,
            &[
                "SessionStart",
                "PreToolUse",
                "UserPromptSubmit",
                "Stop",
                "SessionEnd",
            ],
            &handler,
            forge_bin,
        ) {
            Ok(added) => added,
            Err(e) => return Outcome::error(harness, e),
        };
        if added == 0 {
            return Outcome::unchanged(harness, format!("already in {}", path.display()));
        }
        match write_json(&path, &v) {
            Ok(()) => Outcome::installed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }

    fn hook_uninstall(&self, home: &Path) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{Outcome, drop_hook_groups, read_json, write_json};
        let harness = self.name();
        let path = home.join(".config/muse/settings.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        if drop_hook_groups(&mut v) == 0 {
            return Outcome::unchanged(harness, "nothing to remove".to_string());
        }
        match write_json(&path, &v) {
            Ok(()) => Outcome::removed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }

    fn mcp_install(
        &self,
        home: &Path,
        forge_bin: &str,
    ) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{Outcome, obj_mut, read_json, write_json};
        let harness = self.name();
        // Grounded in muse 1.4.0, verified live: the `mcpServers` block in
        // ~/.config/muse/settings.json takes stdio entries {transport,
        // command, args, env, mode}. 1.4 rejects 1.2.1's `mcp_servers` key
        // and disables MCP entirely, so a stale forge entry there goes.
        // MCP children inherit only PATH+PWD plus the static `env` map,
        // but ${VAR} entries expand from the parent process (verified),
        // so the endpoint and run ride through from forge-spawned panes.
        // Outside forge both expand empty and mcp-serve fail-softs. Mode
        // is optional so a broken forge warns instead of aborting runs.
        let path = home.join(".config/muse/settings.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        remove_stale_muse_mcp(&mut v);
        obj_mut(&mut v, "mcpServers").insert(
            "forge".to_string(),
            serde_json::json!({
                "transport": "stdio",
                "command": forge_bin,
                "args": ["mcp-serve"],
                "env": {
                    "FORGE_IPC_ENDPOINT": "${FORGE_IPC_ENDPOINT}",
                    "FORGE_RUN_ID": "${FORGE_RUN_ID}",
                },
                "mode": "optional",
            }),
        );
        match write_json(&path, &v) {
            Ok(()) => Outcome::installed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }

    fn mcp_uninstall(&self, home: &Path) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{Outcome, read_json, write_json};
        let harness = self.name();
        let path = home.join(".config/muse/settings.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        let stale = remove_stale_muse_mcp(&mut v);
        let gone = v
            .get_mut("mcpServers")
            .and_then(|m| m.as_object_mut())
            .is_some_and(|m| m.remove("forge").is_some());
        if !gone && !stale {
            return Outcome::unchanged(harness, "nothing to remove".to_string());
        }
        match write_json(&path, &v) {
            Ok(()) => Outcome::removed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }
}
