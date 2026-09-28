//! Codex adapter: runtime injection, verdict rendering, and installer
//! surfaces for the codex CLI.

use std::path::{Path, PathBuf};

use super::runtime::{toml_basic_string, FORGE_RUNTIME_CONTRACT, RuntimeMechanism};
use super::AgentAdapter;

/// Stateless adapter for the `codex` CLI.
pub struct CodexAdapter;

/// Codex reads CODEX_HOME, defaulting to ~/.codex. Hooks must land where
/// the MCP subprocess looks, so both consult this.
pub(crate) fn codex_home(home: &Path) -> PathBuf {
    std::env::var("CODEX_HOME")
        .ok()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

/// Allowlist the two env vars forge's MCP server needs through codex's
/// scrubbed MCP environment. Line-based (never a TOML rewrite) so codex's
/// comments survive; idempotent. `mcp remove` drops the whole section, so
/// uninstall needs no counterpart.
pub(crate) fn ensure_codex_mcp_env(home: &Path) -> Result<(), String> {
    const WANT: &str = r#"env_vars = ["FORGE_IPC_ENDPOINT", "FORGE_RUN_ID"]"#;
    const HEADER: &str = "[mcp_servers.forge]";
    let path = codex_home(home).join("config.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == HEADER) else {
        return Err(format!("{HEADER} missing in {}", path.display()));
    };
    let end = lines
        .iter()
        .skip(start + 1)
        .position(|l| l.trim().starts_with('['))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());
    if lines[start + 1..end].iter().any(|l| l.trim().starts_with("env_vars")) {
        return Ok(());
    }
    lines.insert(start + 1, WANT);
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    crate::infra::fs_atomic::write_atomic(&path, out.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
}

impl AgentAdapter for CodexAdapter {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn runtime_mechanism(&self) -> RuntimeMechanism {
        RuntimeMechanism::DeveloperInstructions
    }

    fn runtime_injection(&self, _runtime_file: &Path) -> Vec<String> {
        vec![
            "-c".to_string(),
            format!(
                "developer_instructions={}",
                toml_basic_string(FORGE_RUNTIME_CONTRACT)
            ),
        ]
    }

    fn render_decision(
        &self,
        hook: &str,
        decision: crate::hooks::policy::Decision,
        reason: &str,
    ) -> String {
        // Grounded in codex-cli hooks: a bare `permissionDecision:"allow"`
        // (no `updatedInput` rewrite) is rejected as "unsupported
        // permissionDecision:allow", and `ask` is likewise unsupported on
        // PreToolUse. Forge never rewrites input, so allow/ask must be
        // empty stdout (exit 0, no output) and let Codex's own approval
        // flow decide. Deny keeps the explicit deny shape.
        //
        // Codex approvals gate on `PermissionRequest`: allow there skips
        // the approval prompt, deny blocks, and silence declines to decide
        // (the normal approval flow continues).
        use crate::hooks::policy::Decision;
        if hook == "PreToolUse" && matches!(decision, Decision::Allow | Decision::Ask) {
            return String::new();
        }
        if hook == "PermissionRequest" {
            return match decision {
                Decision::Allow => "{\"hookSpecificOutput\":{\"hookEventName\":\"PermissionRequest\",\"decision\":{\"behavior\":\"allow\"}}}\n"
                    .to_string(),
                Decision::Deny => {
                    let message = crate::hooks::policy::escape_reason(reason);
                    let message = if message.is_empty() { "denied".to_string() } else { message };
                    format!(
                        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PermissionRequest\",\"decision\":{{\"behavior\":\"deny\",\"message\":\"{message}\"}}}}}}\n"
                    )
                }
                Decision::Ask => String::new(),
            };
        }
        crate::hooks::policy::decision_line(hook, decision, reason)
    }

    fn hook_install(&self, home: &Path, forge_bin: &str) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{Outcome, hook_command, read_json, write_json};
        let harness = self.name();
        // Grounded in codex-cli 0.154: hooks.json is `{"hooks":
        // {Event: [{matcher, hooks: [{type, command, timeout}]}]}}`.
        // Anything else (including a flat array) fails the whole file
        // with "failed to parse hooks config". Verified live: a
        // UserPromptSubmit entry fires its command with the hook JSON
        // on stdin. Commands run WITHOUT a shell, so no metacharacters.
        // Event names are PascalCase; `matcher: ""` matches everything.
        // PermissionRequest is the approval gate (official hooks docs):
        // it fires when Codex is about to ask approval, allow skips the
        // prompt, deny blocks, and silence defers to the normal flow.
        // It is the only verdict that can auto-approve a Codex call, so
        // forge YOLO rides on it; PreToolUse stays the deny-only block
        // layer (bare allow/ask are rejected there).
        let path = codex_home(home).join("hooks.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        // A non-object `hooks` value is rejected wholesale by codex
        // (this includes the flat array our own pre-5d installer
        // wrote), so reset it rather than preserve breakage.
        if !v.get("hooks").is_some_and(|h| h.is_object()) {
            v["hooks"] = serde_json::Value::Object(Default::default());
        }
        let ours = hook_command(forge_bin);
        let mut added = 0;
        // Stop is the turn-end edge: it parks the session at Stopped,
        // which is what lets queued peer replies deliver. Without it
        // every session wedges at Thinking/ToolUse after boot.
        // UserPromptSubmit is the turn-start edge: without it a freshly
        // prompted session still looks Stopped while it generates, so
        // injections land mid-turn where Enter is eaten and the text
        // sits as an unsubmitted draft.
        for event in [
            "SessionStart",
            "PreToolUse",
            "PermissionRequest",
            "Stop",
            "UserPromptSubmit",
        ] {
            let slot = v["hooks"]
                .as_object_mut()
                .expect("hooks just normalized to object")
                .entry(event.to_string())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()));
            if !slot.is_array() {
                return Outcome::error(
                    harness,
                    format!("{event} is not an array in {}", path.display()),
                );
            }
            let groups = slot.as_array_mut().expect("just checked array");
            let present = groups.iter().any(|g| {
                g.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                    hs.iter().any(|h| {
                        h.get("command").and_then(|c| c.as_str()) == Some(&ours)
                    })
                })
            });
            if !present {
                groups.push(serde_json::json!({
                    "matcher": "",
                    "hooks": [{"type": "command", "command": ours, "timeout": 10}],
                }));
                added += 1;
            }
        }
        if added == 0 {
            return Outcome::unchanged(harness, format!("already in {}", path.display()));
        }
        match write_json(&path, &v) {
            Ok(()) => Outcome::installed(harness, path.display().to_string()),
            Err(e) => Outcome::error(harness, e),
        }
    }

    fn hook_uninstall(&self, home: &Path) -> crate::hooks::install::Outcome {
        use crate::hooks::install::{Outcome, is_ours, read_json, write_json};
        let harness = self.name();
        let path = codex_home(home).join("hooks.json");
        let mut v = match read_json(&path) {
            Ok(v) => v,
            Err(e) => return Outcome::error(harness, e),
        };
        // Drop our handlers wherever they sit: current 3-level groups
        // and the flat pre-5d array shape. Empty groups and events go
        // with them; foreign entries are kept.
        let mut dropped = 0;
        if let Some(obj) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            let mut dead_events = Vec::new();
            for (event, slot) in obj.iter_mut() {
                let Some(groups) = slot.as_array_mut() else {
                    continue;
                };
                let mut dead_groups = Vec::new();
                for (gi, g) in groups.iter_mut().enumerate() {
                    if let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                        let before = hs.len();
                        hs.retain(|h| {
                            !h.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(is_ours)
                        });
                        dropped += before - hs.len();
                        if hs.is_empty() {
                            dead_groups.push(gi);
                        }
                    }
                }
                for gi in dead_groups.into_iter().rev() {
                    groups.remove(gi);
                }
                if groups.is_empty() {
                    dead_events.push(event.clone());
                }
            }
            for event in dead_events {
                obj.remove(&event);
            }
        } else if let Some(arr) = v.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            let before = arr.len();
            arr.retain(|e| {
                !e.get("command").and_then(|c| c.as_str()).is_some_and(is_ours)
            });
            dropped += before - arr.len();
        }
        if dropped == 0 {
            return Outcome::unchanged(harness, "nothing to remove".to_string());
        }
        let hooks_empty = v.get("hooks").is_some_and(|h| {
            h.as_object().is_some_and(|o| o.is_empty())
                || h.as_array().is_some_and(|a| a.is_empty())
        });
        let only_hooks = v.as_object().is_some_and(|o| o.len() == 1 && o.contains_key("hooks"));
        if hooks_empty && only_hooks {
            match std::fs::remove_file(&path) {
                Ok(()) => Outcome::removed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, format!("cannot remove {}: {e}", path.display())),
            }
        } else {
            match write_json(&path, &v) {
                Ok(()) => Outcome::removed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
    }

    fn mcp_install(
        &self,
        home: &Path,
        forge_bin: &str,
    ) -> crate::hooks::install::Outcome {
        use crate::hooks::install::Outcome;
        use crate::hooks::install::mcp::run_codex_mcp_add;
        let harness = self.name();
        // Codex owns config.toml comments, so registration shells out to the
        // CLI itself (verified offline against 0.154). `mcp add` has no
        // env-passthrough flag, so the `env_vars` allowlist below is patched
        // in textually afterwards; without it codex scrubs FORGE_* from MCP
        // servers (verified live: "no comms route" until allowlisted).
        let Some(bin) =
            crate::agents::harness::Harness::from_name(harness).map(|h| h.spec().resolve_binary())
        else {
            return Outcome::error(harness, "codex is not in agents.json".to_string());
        };
        if !run_codex_mcp_add(&bin, "forge", forge_bin) {
            return Outcome::error(harness, format!("`{bin} mcp add forge` failed"));
        }
        match ensure_codex_mcp_env(home) {
            Ok(()) => Outcome::installed(harness, "codex mcp add forge".to_string()),
            Err(e) => Outcome::error(
                harness,
                format!("forge MCP registered but env passthrough failed: {e}"),
            ),
        }
    }

    fn mcp_uninstall(&self, _home: &Path) -> crate::hooks::install::Outcome {
        use crate::hooks::install::Outcome;
        use crate::hooks::install::mcp::run_codex_mcp_remove;
        let harness = self.name();
        let Some(bin) =
            crate::agents::harness::Harness::from_name(harness).map(|h| h.spec().resolve_binary())
        else {
            return Outcome::error(harness, "codex is not in agents.json".to_string());
        };
        if run_codex_mcp_remove(&bin, "forge") {
            Outcome::removed(harness, "codex mcp remove forge".to_string())
        } else {
            Outcome::error(harness, format!("`{bin} mcp remove forge` failed"))
        }
    }

    fn skills_dir(&self, home: &Path) -> Option<PathBuf> {
        Some(home.join(".codex/skills/forge"))
    }
}
