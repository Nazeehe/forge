//! Harness installers (Phase 5b): register forge hooks, MCP servers, and
//! skills with each agent CLI. Everything operates on an explicit home
//! directory so tests run on scratch dirs, never the developer's real
//! config. Codex MCP goes through the `codex` binary itself so user
//! `config.toml` comments survive; everything else is direct file surgery
//! in schemas observed live (see each function).

/// One harness result. Skips (unsupported) are not errors; only `error`
/// fails the subcommand.
#[derive(Debug)]
pub struct Outcome {
    pub harness: String,
    pub installed: bool,
    pub removed: bool,
    pub skipped: bool,
    pub error: Option<String>,
    pub detail: String,
}

impl Outcome {
    fn installed(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: true,
            removed: false,
            skipped: false,
            error: None,
            detail,
        }
    }

    fn removed(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: true,
            skipped: false,
            error: None,
            detail,
        }
    }

    fn skipped(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: false,
            skipped: true,
            error: None,
            detail,
        }
    }

    fn error(harness: &str, error: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: false,
            skipped: false,
            error: Some(error),
            detail: String::new(),
        }
    }

    fn unchanged(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: false,
            skipped: false,
            error: None,
            detail,
        }
    }
}

fn hook_command(forge_bin: &str) -> String {
    format!("{forge_bin} hook-relay")
}

/// Codex reads CODEX_HOME, defaulting to ~/.codex. Hooks must land where
/// the MCP subprocess looks, so both consult this.
fn codex_home(home: &std::path::Path) -> std::path::PathBuf {
    std::env::var("CODEX_HOME")
        .ok()
        .filter(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
}

fn is_ours(cmd: &str) -> bool {
    cmd.contains("hook-relay")
}

/// Read JSON, preserving everything we do not touch. Missing file is an
/// empty object; corrupt files are an error and are never clobbered.
fn read_json(path: &std::path::Path) -> Result<serde_json::Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(serde_json::Value::Object(Default::default()))
        }
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("cannot parse {}: {e}", path.display())),
    }
}

fn write_json(path: &std::path::Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("cannot encode {}: {e}", path.display()))?;
    crate::fs_atomic::write_atomic(path, text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn obj_mut<'v>(
    value: &'v mut serde_json::Value,
    key: &str,
) -> &'v mut serde_json::Map<String, serde_json::Value> {
    if !value.get(key).is_some_and(|v| v.is_object()) {
        value[key] = serde_json::Value::Object(Default::default());
    }
    value[key].as_object_mut().expect("just ensured object")
}

/// Install hooks for one harness by name.
pub fn install_one_hooks(
    home: &std::path::Path,
    harness: &str,
    forge_bin: &str,
) -> Outcome {
    match harness {
        "claude" => {
            let path = home.join(".claude/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let ours = hook_command(forge_bin);
            let mut added = 0;
            for event in ["PreToolUse", "PermissionRequest"] {
                let hooks = obj_mut(&mut v, "hooks");
                let slot = hooks
                    .entry(event.to_string())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if !slot.is_array() {
                    return Outcome::error(
                        harness,
                        format!("{event} is not an array in {}", path.display()),
                    );
                }
                let arr = slot.as_array_mut().expect("just checked array");
                let present = arr.iter().any(|e| {
                    e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                        hs.iter().any(|h| h.get("command").and_then(|c| c.as_str()) == Some(&ours))
                    })
                });
                if !present {
                    arr.push(serde_json::json!({
                        "matcher": "",
                        "hooks": [{"type": "command", "command": ours}],
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
        "codex" => {
            // Best-effort: the hooks engine reads hooks.json (event names and
            // matcher/timeout fields observed in the 0.154 binary), but the
            // discovery path is unconfirmed and firing needs API auth, so the
            // first live session must confirm pickup. The file is inert:
            // codex loads config cleanly with it present.
            let path = codex_home(home).join("hooks.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let ours = hook_command(forge_bin);
            match v.get("hooks") {
                None => {
                    v["hooks"] = serde_json::Value::Array(Vec::new());
                }
                Some(h) if h.is_array() => {}
                Some(_) => {
                    return Outcome::error(
                        harness,
                        format!("{} has a non-array hooks value; refusing to clobber", path.display()),
                    );
                }
            }
            let arr = v["hooks"].as_array().cloned().unwrap_or_default();
            let mut arr = arr;
            let mut added = 0;
            for event in ["pre_tool_use", "session_start"] {
                let present = arr.iter().any(|e| {
                    e.get("command").and_then(|c| c.as_str()) == Some(&ours)
                        && e.get("event_name").and_then(|c| c.as_str()) == Some(event)
                });
                if !present {
                    arr.push(serde_json::json!({
                        "event_name": event,
                        "matcher": ".*",
                        "command": ours,
                        "timeoutSec": 10,
                    }));
                    added += 1;
                }
            }
            if added == 0 {
                return Outcome::unchanged(harness, format!("already in {}", path.display()));
            }
            v["hooks"] = serde_json::Value::Array(arr);
            match write_json(&path, &v) {
                Ok(()) => Outcome::installed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        "muse" => Outcome::skipped(
            harness,
            "muse exposes no shell hook configuration; allow policy lives in opencode.json"
                .to_string(),
        ),
        "gemini" => Outcome::skipped(
            harness,
            "no grounded gemini hook mechanism; gemini hooks stay uninstalled".to_string(),
        ),
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
    }
}

/// Remove hooks this installer owns. Entries mentioning other commands are
/// kept; the codex file goes away only when nothing foreign remains.
pub fn uninstall_one_hooks(home: &std::path::Path, harness: &str) -> Outcome {
    match harness {
        "claude" => {
            let path = home.join(".claude/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let mut dropped = 0;
            if let Some(hooks) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
                    let before = arr.len();
                    arr.retain(|e| {
                        !e.get("hooks")
                            .and_then(|h| h.as_array())
                            .is_some_and(|hs| {
                                hs.iter().any(|h| {
                                    h.get("command")
                                        .and_then(|c| c.as_str())
                                        .is_some_and(is_ours)
                                })
                            })
                    });
                    dropped += before - arr.len();
                }
            }
            if dropped == 0 {
                return Outcome::unchanged(harness, "nothing to remove".to_string());
            }
            match write_json(&path, &v) {
                Ok(()) => Outcome::removed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        "codex" => {
            let path = codex_home(home).join("hooks.json");
            let v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let arr = v
                .get("hooks")
                .and_then(|h| h.as_array())
                .cloned()
                .unwrap_or_default();
            if !arr.iter().any(|e| {
                e.get("command").and_then(|c| c.as_str()).is_some_and(is_ours)
            }) {
                return Outcome::unchanged(harness, "nothing to remove".to_string());
            }
            let kept: Vec<_> = arr
                .into_iter()
                .filter(|e| {
                    !e.get("command").and_then(|c| c.as_str()).is_some_and(is_ours)
                })
                .collect();
            if kept.is_empty() {
                match std::fs::remove_file(&path) {
                    Ok(()) => Outcome::removed(harness, path.display().to_string()),
                    Err(e) => Outcome::error(harness, format!("cannot remove {}: {e}", path.display())),
                }
            } else {
                let mut v = v;
                v["hooks"] = serde_json::Value::Array(kept);
                match write_json(&path, &v) {
                    Ok(()) => Outcome::removed(harness, path.display().to_string()),
                    Err(e) => Outcome::error(harness, e),
                }
            }
        }
        "muse" => Outcome::skipped(harness, "muse has no shell hooks to remove".to_string()),
        "gemini" => Outcome::skipped(harness, "gemini hooks were never installed".to_string()),
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
    }
}

/// Register the forge MCP server for one harness.
pub fn install_one_mcp(home: &std::path::Path, harness: &str, forge_bin: &str) -> Outcome {
    match harness {
        // Schema mirrors `claude mcp add -s user` output byte for byte.
        "claude" => {
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
        // Codex owns config.toml comments, so registration shells out to the
        // CLI itself (verified offline against 0.154).
        "codex" => {
            let bin = crate::harness::Harness::Codex.spec().resolve_binary();
            if run_codex_mcp_add(&bin, "forge", forge_bin) {
                Outcome::installed(harness, "codex mcp add forge".to_string())
            } else {
                Outcome::error(harness, format!("`{bin} mcp add forge` failed"))
            }
        }
        // Best-effort opencode format; pickup confirmed at first live muse run.
        "muse" => {
            let path = home.join(".config/opencode/opencode.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let mcp = obj_mut(&mut v, "mcp");
            let forge = mcp
                .entry("forge".to_string())
                .or_insert_with(|| serde_json::Value::Object(Default::default()));
            if !forge.is_object() {
                return Outcome::error(
                    harness,
                    format!("mcp.forge is not an object in {}", path.display()),
                );
            }
            let fo = forge.as_object_mut().expect("just checked object");
            fo.insert(
                "command".to_string(),
                serde_json::json!([forge_bin, "mcp-serve"]),
            );
            fo.insert("type".to_string(), serde_json::Value::String("local".to_string()));
            fo.insert("enabled".to_string(), serde_json::Value::Bool(true));
            match write_json(&path, &v) {
                Ok(()) => Outcome::installed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        // Best-effort: no gemini config on this machine to observe; the
        // mcpServers map mirrors the documented settings.json shape.
        "gemini" => {
            let path = home.join(".gemini/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            obj_mut(&mut v, "mcpServers").insert(
                "forge".to_string(),
                serde_json::json!({
                    "command": forge_bin,
                    "args": ["mcp-serve"],
                }),
            );
            match write_json(&path, &v) {
                Ok(()) => Outcome::installed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
    }
}

/// Unregister the forge MCP server for one harness.
pub fn uninstall_one_mcp(home: &std::path::Path, harness: &str) -> Outcome {
    match harness {
        "claude" => {
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
        "codex" => {
            let bin = crate::harness::Harness::Codex.spec().resolve_binary();
            if run_codex_mcp_remove(&bin, "forge") {
                Outcome::removed(harness, "codex mcp remove forge".to_string())
            } else {
                Outcome::error(harness, format!("`{bin} mcp remove forge` failed"))
            }
        }
        "muse" => {
            let path = home.join(".config/opencode/opencode.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let gone = v
                .get_mut("mcp")
                .and_then(|m| m.as_object_mut())
                .is_some_and(|m| m.remove("forge").is_some());
            if !gone {
                return Outcome::unchanged(harness, "nothing to remove".to_string());
            }
            match write_json(&path, &v) {
                Ok(()) => Outcome::removed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        "gemini" => {
            let path = home.join(".gemini/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let gone = v
                .get_mut("mcpServers")
                .and_then(|m| m.as_object_mut())
                .is_some_and(|m| m.remove("forge").is_some());
            if !gone {
                return Outcome::unchanged(harness, "nothing to remove".to_string());
            }
            match write_json(&path, &v) {
                Ok(()) => Outcome::removed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
    }
}

/// Minimal agent skill: how to reach peer sessions through forge.
const SKILL_MD: &str = r#"---
name: forge
description: Talk to peer agent sessions through the forge control plane
---

Peer sessions are reachable through the forge MCP server (`forge mcp-serve`
on stdio):

- `list_sessions` shows live peers (names route, run IDs confer authority).
- `ask_session(target, message)` asks a peer; the conversation ID returns at
  once and the answer arrives asynchronously. Only sessions sharing a
  communication group can exchange messages.
- `tell_session(target, message)` informs a peer; acknowledge with
  `ack_message(conversation_id)`.
- `send_response(conversation_id, message)` answers a question addressed to
  this session.
"#;

/// Install the forge skill for one harness. Only harnesses with grounded
/// skills paths are written; the rest report skipped.
pub fn install_one_skills(home: &std::path::Path, harness: &str) -> Outcome {
    let dir = match harness {
        "claude" => home.join(".claude/skills/forge"),
        "codex" => home.join(".codex/skills/forge"),
        _ => {
            return Outcome::skipped(
                harness,
                "no grounded skills path for this harness".to_string(),
            );
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Outcome::error(harness, format!("cannot create {}: {e}", dir.display()));
    }
    match std::fs::write(dir.join("SKILL.md"), SKILL_MD) {
        Ok(()) => Outcome::installed(harness, dir.display().to_string()),
        Err(e) => Outcome::error(harness, format!("cannot write skill: {e}")),
    }
}

/// Remove the forge skill for one harness.
pub fn uninstall_one_skills(home: &std::path::Path, harness: &str) -> Outcome {
    let file = match harness {
        "claude" => home.join(".claude/skills/forge/SKILL.md"),
        "codex" => home.join(".codex/skills/forge/SKILL.md"),
        _ => {
            return Outcome::skipped(harness, "no grounded skills path".to_string());
        }
    };
    match std::fs::remove_file(&file) {
        Ok(()) => {
            let _ = std::fs::remove_dir(file.parent().expect("skill has a dir"));
            Outcome::removed(harness, file.display().to_string())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Outcome::unchanged(harness, "nothing to remove".to_string())
        }
        Err(e) => Outcome::error(harness, format!("cannot remove {}: {e}", file.display())),
    }
}

/// argv for `codex mcp add`, verified against 0.154 offline.
pub fn codex_mcp_argv(codex_bin: &str, name: &str, forge_bin: &str) -> Vec<String> {
    vec![
        codex_bin.to_string(),
        "mcp".to_string(),
        "add".to_string(),
        name.to_string(),
        "--".to_string(),
        forge_bin.to_string(),
        "mcp-serve".to_string(),
    ]
}

fn run_codex_mcp(codex_bin: &str, args: &[&str]) -> bool {
    std::process::Command::new(codex_bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn run_codex_mcp_add(codex_bin: &str, name: &str, forge_bin: &str) -> bool {
    let argv = codex_mcp_argv(codex_bin, name, forge_bin);
    run_codex_mcp(&argv[0], &argv[1..].iter().map(String::as_str).collect::<Vec<_>>())
}

pub fn run_codex_mcp_remove(codex_bin: &str, name: &str) -> bool {
    run_codex_mcp(codex_bin, &["mcp", "remove", name])
}

/// Cross-harness aggregates in registry order.
pub fn install_hooks(home: &std::path::Path, forge_bin: &str) -> Vec<Outcome> {
    ["claude", "codex", "muse"]
        .into_iter()
        .map(|h| install_one_hooks(home, h, forge_bin))
        .collect()
}

pub fn uninstall_hooks(home: &std::path::Path) -> Vec<Outcome> {
    ["claude", "codex", "muse"]
        .into_iter()
        .map(|h| uninstall_one_hooks(home, h))
        .collect()
}

pub fn install_mcp(home: &std::path::Path, forge_bin: &str) -> Vec<Outcome> {
    ["claude", "codex", "muse"]
        .into_iter()
        .map(|h| install_one_mcp(home, h, forge_bin))
        .collect()
}

pub fn uninstall_mcp(home: &std::path::Path) -> Vec<Outcome> {
    ["claude", "codex", "muse"]
        .into_iter()
        .map(|h| uninstall_one_mcp(home, h))
        .collect()
}

pub fn install_skills(home: &std::path::Path) -> Vec<Outcome> {
    ["claude", "codex", "muse"]
        .into_iter()
        .map(|h| install_one_skills(home, h))
        .collect()
}

pub fn uninstall_skills(home: &std::path::Path) -> Vec<Outcome> {
    ["claude", "codex", "muse"]
        .into_iter()
        .map(|h| uninstall_one_skills(home, h))
        .collect()
}

/// Map a per-harness subcommand name to installer keys. `metamate` is the
/// blueprint name for the muse CLI.
fn keys_for(harness: &str) -> Option<&'static str> {
    match harness {
        "codex" => Some("codex"),
        "gemini" => Some("gemini"),
        "metamate" | "muse" => Some("muse"),
        "claude" => Some("claude"),
        _ => None,
    }
}

/// Full per-harness install for `install-codex` and friends.
pub fn install_one(
    home: &std::path::Path,
    harness: &str,
    forge_bin: &str,
) -> Vec<Outcome> {
    let Some(key) = keys_for(harness) else {
        return vec![Outcome::error(harness, format!("unknown harness {harness:?}"))];
    };
    vec![
        install_one_hooks(home, key, forge_bin),
        install_one_mcp(home, key, forge_bin),
        install_one_skills(home, key),
    ]
}

pub fn uninstall_one(home: &std::path::Path, harness: &str) -> Vec<Outcome> {
    let Some(key) = keys_for(harness) else {
        return vec![Outcome::error(harness, format!("unknown harness {harness:?}"))];
    };
    vec![
        uninstall_one_hooks(home, key),
        uninstall_one_mcp(home, key),
        uninstall_one_skills(home, key),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_home() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-install-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Pin codex paths to the scratch home even when the developer exports
    /// CODEX_HOME. Restores on drop so panics cannot leak env changes.
    struct ClearCodexHome {
        saved: Option<String>,
    }

    impl ClearCodexHome {
        fn pin() -> Self {
            let saved = std::env::var("CODEX_HOME").ok();
            std::env::remove_var("CODEX_HOME");
            ClearCodexHome { saved }
        }
    }

    impl Drop for ClearCodexHome {
        fn drop(&mut self) {
            if let Some(v) = self.saved.take() {
                std::env::set_var("CODEX_HOME", v);
            }
        }
    }

    const FORGE_BIN: &str = "/tmp/forge-under-test";

    #[test]
    fn claude_hooks_merge_and_are_idempotent() {
        let home = scratch_home();
        // Pre-existing unrelated settings survive.
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{"model": "sonnet", "hooks": {"PostToolUse": []}}"#,
        )
        .unwrap();
        let out = install_one_hooks(&home, "claude", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert!(text.contains("sonnet"), "kept: {text}");
        assert!(text.contains("PostToolUse"), "kept: {text}");
        assert!(text.contains("hook-relay"), "added: {text}");
        // Second install adds no duplicate.
        install_one_hooks(&home, "claude", FORGE_BIN);
        let text = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert_eq!(text.matches("hook-relay").count(), 2, "pre+permission: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_hooks_uninstall_removes_only_ours() {
        let home = scratch_home();
        install_one_hooks(&home, "claude", FORGE_BIN);
        let out = uninstall_one_hooks(&home, "claude");
        assert!(out.removed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert!(!text.contains("hook-relay"), "gone: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn corrupt_settings_are_never_clobbered() {
        let home = scratch_home();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{oops").unwrap();
        let out = install_one_hooks(&home, "claude", FORGE_BIN);
        assert!(!out.installed, "out: {out:?}");
        assert!(out.error.is_some(), "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert_eq!(text, "{oops");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_mcp_matches_observed_schema() {
        let home = scratch_home();
        let out = install_one_mcp(&home, "claude", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let srv = &v["mcpServers"]["forge"];
        assert_eq!(srv["type"], serde_json::Value::String("stdio".to_string()));
        assert_eq!(srv["command"], serde_json::Value::String(FORGE_BIN.to_string()));
        assert_eq!(srv["args"][0], serde_json::Value::String("mcp-serve".to_string()));
        let out = uninstall_one_mcp(&home, "claude");
        assert!(out.removed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        assert!(!text.contains("forge"), "gone: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_hooks_file_round_trips() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        let out = install_one_hooks(&home, "codex", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".codex/hooks.json")).unwrap();
        assert!(text.contains("hook-relay"), "added: {text}");
        let out = uninstall_one_hooks(&home, "codex");
        assert!(out.removed, "out: {out:?}");
        assert!(!home.join(".codex/hooks.json").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_hooks_uninstall_keeps_foreign_entries() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(
            home.join(".codex/hooks.json"),
            r#"{"hooks": [{"matcher": ".*", "command": "/usr/bin/other-hook"}]}"#,
        )
        .unwrap();
        let out = uninstall_one_hooks(&home, "codex");
        assert!(!out.removed, "out: {out:?}");
        assert!(home.join(".codex/hooks.json").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn muse_hooks_are_reported_unsupported() {
        let home = scratch_home();
        let out = install_one_hooks(&home, "muse", FORGE_BIN);
        assert!(!out.installed, "out: {out:?}");
        assert!(out.skipped, "out: {out:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn muse_mcp_merges_opencode_json() {
        let home = scratch_home();
        let out = install_one_mcp(&home, "muse", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".config/opencode/opencode.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let cmd = &v["mcp"]["forge"]["command"];
        assert_eq!(cmd[0], serde_json::Value::String(FORGE_BIN.to_string()));
        assert_eq!(cmd[1], serde_json::Value::String("mcp-serve".to_string()));
        let out = uninstall_one_mcp(&home, "muse");
        assert!(out.removed, "out: {out:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn skills_round_trip_for_claude_and_codex() {
        let home = scratch_home();
        for h in ["claude", "codex"] {
            let out = install_one_skills(&home, h);
            assert!(out.installed, "out: {out:?}");
        }
        for dir in ["claude", "codex"] {
            let skill = home.join(format!(".{dir}/skills/forge/SKILL.md"));
            let text = std::fs::read_to_string(&skill).unwrap();
            assert!(text.contains("ask_session"), "skill: {text:?}");
        }
        for h in ["claude", "codex"] {
            let out = uninstall_one_skills(&home, h);
            assert!(out.removed, "out: {out:?}");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_mcp_argv_and_fake_binary_round_trip() {
        let argv = codex_mcp_argv("/usr/bin/codex", "forge", "/bin/forge");
        assert_eq!(
            argv,
            vec!["/usr/bin/codex", "mcp", "add", "forge", "--", "/bin/forge", "mcp-serve"]
        );
        // Hermetic: a fake codex that records argv and succeeds.
        let home = scratch_home();
        let fake = home.join("codex");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho \"$@\" > \"$FORGE_FAKE_OUT\"\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake, perms).unwrap();
        }
        let seen = home.join("seen.txt");
        std::env::set_var("FORGE_FAKE_OUT", &seen);
        let ok = run_codex_mcp_add(fake.to_str().unwrap(), "forge", "/bin/forge");
        std::env::remove_var("FORGE_FAKE_OUT");
        assert!(ok, "fake codex succeeds");
        let recorded = std::fs::read_to_string(&seen).unwrap();
        assert!(
            recorded.contains("mcp add forge -- /bin/forge mcp-serve"),
            "argv: {recorded}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_mcp_round_trips_hooks_skip() {
        let home = scratch_home();
        let out = install_one_mcp(&home, "gemini", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["forge"]["command"], serde_json::Value::String(FORGE_BIN.to_string()));
        let out = uninstall_one_mcp(&home, "gemini");
        assert!(out.removed, "out: {out:?}");
        let hooks = install_one_hooks(&home, "gemini", FORGE_BIN);
        assert!(hooks.skipped, "out: {hooks:?}");
        let skills = install_one_skills(&home, "gemini");
        assert!(skills.skipped, "out: {skills:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn per_harness_installer_composes_all_three() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        // codex MCP shells out; point it at a fake binary.
        let fake = home.join("codex");
        std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake, perms).unwrap();
        }
        std::env::set_var("CODEX_BIN", &fake);
        let outs = install_one(&home, "codex", FORGE_BIN);
        std::env::remove_var("CODEX_BIN");
        assert_eq!(outs.len(), 3);
        assert!(outs.iter().all(|o| o.error.is_none()), "outs: {outs:?}");
        let outs = install_one(&home, "metamate", FORGE_BIN);
        assert_eq!(outs.len(), 3);
        assert!(outs.iter().all(|o| o.error.is_none()), "outs: {outs:?}");
        let outs = install_one(&home, "bogus", FORGE_BIN);
        assert!(outs.iter().all(|o| o.error.is_some()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn full_install_reports_every_harness() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        let outs = install_hooks(&home, FORGE_BIN);
        assert_eq!(outs.len(), 3);
        assert!(outs.iter().any(|o| o.harness == "claude" && o.installed));
        assert!(outs.iter().any(|o| o.harness == "codex" && o.installed));
        assert!(outs.iter().any(|o| o.harness == "muse" && o.skipped));
        let _ = std::fs::remove_dir_all(&home);
    }
}
