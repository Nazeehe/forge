//! Claude adapter: runtime injection and installer surfaces for the claude CLI.

use std::path::{Path, PathBuf};

use super::runtime::RuntimeMechanism;
use super::AgentAdapter;

/// Stateless adapter for the `claude` CLI.
pub struct ClaudeAdapter;

impl AgentAdapter for ClaudeAdapter {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn runtime_mechanism(&self) -> RuntimeMechanism {
        RuntimeMechanism::SystemPromptFile
    }

    fn runtime_injection(&self, runtime_file: &Path) -> Vec<String> {
        vec![
            "--append-system-prompt-file".to_string(),
            runtime_file.display().to_string(),
        ]
    }

    fn hook_install(&self, home: &Path, forge_bin: &str) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{
            Outcome, hook_command, merge_hook_groups, read_json, write_json,
        };
        let harness = self.name();
        let path = home.join(".claude/settings.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        // Stop is the turn-end edge: it parks the session at Stopped,
        // which is what lets queued peer replies deliver. Without it
        // every session wedges at ToolUse after its first tool call.
        // UserPromptSubmit is the turn-start edge: without it a freshly
        // prompted session still looks Stopped while it generates, so
        // injections land mid-turn where Enter is eaten and the text
        // sits as an unsubmitted draft.
        let handler =
            serde_json::json!({"type": "command", "command": hook_command(forge_bin)});
        let added = match merge_hook_groups(
            &mut v,
            &path,
            &["PreToolUse", "PermissionRequest", "Stop", "UserPromptSubmit"],
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
        let path = home.join(".claude/settings.json");
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
        // Schema mirrors `claude mcp add -s user` output byte for byte.
        let path = home.join(".claude.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        obj_mut(&mut v, "mcpServers").insert(
            "forge".to_string(),
            serde_json::json!({
                "type": "stdio",
                "command": forge_bin,
                "args": ["mcp-serve"],
                "env": {},
            }),
        );
        match write_json(&path, &v) {
            Ok(()) => Outcome::installed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }

    fn mcp_uninstall(&self, home: &Path) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{Outcome, obj_mut, read_json, write_json};
        let harness = self.name();
        let path = home.join(".claude.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        let gone = obj_mut(&mut v, "mcpServers").remove("forge").is_some();
        if !gone {
            return Outcome::unchanged(harness, "nothing to remove".to_string());
        }
        match write_json(&path, &v) {
            Ok(()) => Outcome::removed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }

    fn skills_dir(&self, home: &Path) -> Option<PathBuf> {
        Some(home.join(".claude/skills/forge"))
    }
}
