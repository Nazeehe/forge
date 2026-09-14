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

/// Merge forge hook-relay groups into a Claude-style settings document:
/// top-level `hooks` object keyed by event, each an array of
/// `{matcher, hooks: [...]}` groups. Everything else is preserved.
/// Returns the number of events gained an entry.
fn merge_hook_groups(
    v: &mut serde_json::Value,
    path: &std::path::Path,
    events: &[&str],
    handler: &serde_json::Value,
    forge_bin: &str,
) -> Result<usize, String> {
    let ours = hook_command(forge_bin);
    let mut added = 0;
    for event in events {
        let hooks = obj_mut(v, "hooks");
        let slot = hooks
            .entry(event.to_string())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()));
        if !slot.is_array() {
            return Err(format!("{event} is not an array in {}", path.display()));
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
                "hooks": [handler],
            }));
            added += 1;
        }
    }
    Ok(added)
}

/// Drop every hook group that invokes this installer's relay, across all
/// events. Foreign entries are kept. Returns the drop count.
fn drop_hook_groups(v: &mut serde_json::Value) -> usize {
    let mut dropped = 0;
    if let Some(hooks) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for arr in hooks.values_mut().filter_map(|v| v.as_array_mut()) {
            let before = arr.len();
            arr.retain(|e| {
                !e.get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hs| {
                        hs.iter().any(|h| {
                            h.get("command").and_then(|c| c.as_str()).is_some_and(is_ours)
                        })
                    })
            });
            dropped += before - arr.len();
        }
    }
    dropped
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
            // Stop is the turn-end edge: it parks the session at Stopped,
            // which is what lets queued peer replies deliver. Without it
            // every session wedges at ToolUse after its first tool call.
            // UserPromptSubmit is the turn-start edge: without it a freshly
            // prompted session still looks Stopped while it generates, so
            // injections land mid-turn where Enter is eaten and the text
            // sits as an unsubmitted draft.
            let handler = serde_json::json!({"type": "command", "command": hook_command(forge_bin)});
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
        "codex" => {
            // Grounded in codex-cli 0.154: hooks.json is `{"hooks":
            // {Event: [{matcher, hooks: [{type, command, timeout}]}]}}`.
            // Anything else (including a flat array) fails the whole file
            // with "failed to parse hooks config". Verified live: a
            // UserPromptSubmit entry fires its command with the hook JSON
            // on stdin. Commands run WITHOUT a shell, so no metacharacters.
            // Event names are PascalCase; `matcher: ""` matches everything.
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
            for event in ["SessionStart", "PreToolUse", "Stop", "UserPromptSubmit"] {
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
        "muse" => {
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
        "claude" | "muse" => {
            let path = match harness {
                "claude" => home.join(".claude/settings.json"),
                _ => home.join(".config/muse/settings.json"),
            };
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
        "codex" => {
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
        // CLI itself (verified offline against 0.154). `mcp add` has no
        // env-passthrough flag, so the `env_vars` allowlist below is patched
        // in textually afterwards; without it codex scrubs FORGE_* from MCP
        // servers (verified live: "no comms route" until allowlisted).
        "codex" => {
            let bin = crate::harness::Harness::Codex.spec().resolve_binary();
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
        // Grounded in muse 1.2.1, verified live: the `mcp_servers`
        // block in ~/.config/muse/settings.json takes stdio entries
        // {transport, command, args, env} plus `enabled` and `mode`.
        // MCP children inherit only PATH+PWD plus the static `env` map,
        // but ${VAR} entries expand from the parent process (verified),
        // so the endpoint and run ride through from forge-spawned panes.
        // Outside forge both expand empty and mcp-serve fail-softs. Mode
        // is optional so a broken forge warns instead of aborting runs.
        "muse" => {
            let path = home.join(".config/muse/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            obj_mut(&mut v, "mcp_servers").insert(
                "forge".to_string(),
                serde_json::json!({
                    "transport": "stdio",
                    "command": forge_bin,
                    "args": ["mcp-serve"],
                    "env": {
                        "FORGE_IPC_ENDPOINT": "${FORGE_IPC_ENDPOINT}",
                        "FORGE_RUN_ID": "${FORGE_RUN_ID}",
                    },
                    "enabled": true,
                    "mode": "optional",
                }),
            );
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

/// Allowlist the two env vars forge's MCP server needs through codex's
/// scrubbed MCP environment. Line-based (never a TOML rewrite) so codex's
/// comments survive; idempotent. `mcp remove` drops the whole section, so
/// uninstall needs no counterpart.
fn ensure_codex_mcp_env(home: &std::path::Path) -> Result<(), String> {
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
    crate::fs_atomic::write_atomic(&path, out.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(())
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
            let path = home.join(".config/muse/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let gone = v
                .get_mut("mcp_servers")
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
  `ack_message(conversation_id)`. Either party can keep talking on the
  returned `conversation_id` via
  `tell_session(target, message, conversation_id)`.
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
    /// Holding the lock serializes every CODEX_HOME toucher: the variable
    /// is process-global, so parallel setters would otherwise cross-read.
    struct ClearCodexHome {
        saved: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl ClearCodexHome {
        fn pin() -> Self {
            static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = std::env::var("CODEX_HOME").ok();
            std::env::remove_var("CODEX_HOME");
            ClearCodexHome {
                saved,
                _lock: lock,
            }
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
        assert_eq!(text.matches("hook-relay").count(), 4, "pre+permission+stop+submit: {text}");
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
        // Pin the live-grounded schema (codex-cli 0.154 rejects anything
        // else with "failed to parse hooks config"): an object keyed by
        // PascalCase event, matcher groups, typed command handlers.
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["hooks"].is_object(), "3-level map: {text}");
        for event in ["SessionStart", "PreToolUse", "Stop", "UserPromptSubmit"] {
            let groups = v["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("{event} groups: {text}"));
            assert_eq!(groups.len(), 1, "{event}: {text}");
            assert_eq!(groups[0]["matcher"], serde_json::Value::String(String::new()));
            let hs = groups[0]["hooks"]
                .as_array()
                .unwrap_or_else(|| panic!("{event} handlers: {text}"));
            assert_eq!(hs.len(), 1);
            assert_eq!(hs[0]["type"], serde_json::Value::String("command".to_string()));
            assert!(
                hs[0]["command"].as_str().is_some_and(|c| c.contains("hook-relay")),
                "added: {text}"
            );
            assert!(hs[0]["timeout"].is_number(), "numeric timeout: {text}");
        }
        // Second install adds no duplicate.
        let out = install_one_hooks(&home, "codex", FORGE_BIN);
        assert!(!out.installed, "idempotent: {out:?}");
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
            r#"{"hooks": {"SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "/usr/bin/other-hook", "timeout": 5}]}]}}"#,
        )
        .unwrap();
        let out = install_one_hooks(&home, "codex", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let out = uninstall_one_hooks(&home, "codex");
        assert!(out.removed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".codex/hooks.json")).unwrap();
        assert!(text.contains("/usr/bin/other-hook"), "foreign kept: {text}");
        assert!(!text.contains("hook-relay"), "ours gone: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_hooks_install_migrates_legacy_flat_array() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        // The pre-5d installer wrote a flat array codex rejects wholesale.
        std::fs::write(
            home.join(".codex/hooks.json"),
            r#"{"hooks": [{"event_name": "session_start", "matcher": ".*", "command": "/tmp/forge-under-test hook-relay", "timeoutSec": 10}]}"#,
        )
        .unwrap();
        let out = install_one_hooks(&home, "codex", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".codex/hooks.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["hooks"].is_object(), "migrated to map: {text}");
        assert!(v["hooks"]["SessionStart"].is_array(), "event present: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn muse_hooks_merge_and_are_idempotent() {
        let home = scratch_home();
        // Pre-existing model/effort settings survive; schema matches the
        // shape verified live against muse 1.2.1.
        std::fs::create_dir_all(home.join(".config/muse")).unwrap();
        std::fs::write(
            home.join(".config/muse/settings.json"),
            r#"{"schema_version": 1, "model": "muse-spark-1.3-contributor", "hooks": {"PostToolUse": []}}"#,
        )
        .unwrap();
        let out = install_one_hooks(&home, "muse", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert!(text.contains("muse-spark-1.3-contributor"), "kept: {text}");
        assert!(text.contains("PostToolUse"), "kept: {text}");
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for event in [
            "SessionStart",
            "PreToolUse",
            "UserPromptSubmit",
            "Stop",
            "SessionEnd",
        ] {
            let groups = v["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("event present: {text}"));
            assert!(
                groups.iter().any(|g| {
                    g["matcher"] == serde_json::Value::String(String::new())
                        && g["hooks"][0]["type"] == serde_json::Value::String("command".to_string())
                        && g["hooks"][0]["command"]
                            == serde_json::Value::String(format!("{FORGE_BIN} hook-relay"))
                        && g["hooks"][0]["timeout"] == serde_json::Value::from(10)
                }),
                "relay group on {event}: {text}"
            );
        }
        // Second install adds no duplicate.
        install_one_hooks(&home, "muse", FORGE_BIN);
        let text = std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert_eq!(text.matches("hook-relay").count(), 5, "one per event: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn muse_hooks_uninstall_removes_only_ours() {
        let home = scratch_home();
        install_one_hooks(&home, "muse", FORGE_BIN);
        let out = uninstall_one_hooks(&home, "muse");
        assert!(out.removed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert!(!text.contains("hook-relay"), "gone: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn muse_mcp_writes_verified_schema() {
        let home = scratch_home();
        let out = install_one_mcp(&home, "muse", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let srv = &v["mcp_servers"]["forge"];
        assert_eq!(srv["transport"], serde_json::Value::String("stdio".to_string()));
        assert_eq!(srv["command"], serde_json::Value::String(FORGE_BIN.to_string()));
        assert_eq!(srv["args"][0], serde_json::Value::String("mcp-serve".to_string()));
        // ${VAR} entries expand from the parent process at spawn (verified
        // live); static values would freeze a dead endpoint into config.
        assert_eq!(
            srv["env"]["FORGE_IPC_ENDPOINT"],
            serde_json::Value::String("${FORGE_IPC_ENDPOINT}".to_string())
        );
        assert_eq!(
            srv["env"]["FORGE_RUN_ID"],
            serde_json::Value::String("${FORGE_RUN_ID}".to_string())
        );
        assert_eq!(srv["enabled"], serde_json::Value::Bool(true));
        assert_eq!(srv["mode"], serde_json::Value::String("optional".to_string()));
        // Hooks and MCP share the file: installing both keeps both.
        install_one_hooks(&home, "muse", FORGE_BIN);
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert!(text.contains("mcp_servers"), "mcp kept: {text}");
        assert!(text.contains("hook-relay"), "hooks kept: {text}");
        let out = uninstall_one_mcp(&home, "muse");
        assert!(out.removed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["mcp_servers"].get("forge").is_none(), "gone: {text}");
        assert!(text.contains("hook-relay"), "hooks kept: {text}");
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
    fn codex_mcp_env_passthrough_is_idempotent_and_comment_safe() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        let dir = home.join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# codex owns this file\n[mcp_servers.forge]\ncommand = \"/bin/forge\"\nargs = [\"mcp-serve\"]\n\n[other]\nkeep = true\n",
        )
        .unwrap();
        // Pin codex_home() at this scratch dir via CODEX_HOME.
        std::env::set_var("CODEX_HOME", &dir);
        let out = ensure_codex_mcp_env(&home);
        std::env::remove_var("CODEX_HOME");
        assert!(out.is_ok(), "out: {out:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# codex owns this file"), "comments kept: {text}");
        assert!(
            text.contains(r#"env_vars = ["FORGE_IPC_ENDPOINT", "FORGE_RUN_ID"]"#),
            "allowlisted: {text}"
        );
        assert!(text.contains("[other]"), "neighbor kept: {text}");
        // Second run adds no duplicate.
        std::env::set_var("CODEX_HOME", &dir);
        let out = ensure_codex_mcp_env(&home);
        std::env::remove_var("CODEX_HOME");
        assert!(out.is_ok(), "out: {out:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("env_vars").count(), 1, "once: {text}");
        let _ = std::fs::remove_dir_all(&home);
        // Missing section errors instead of inventing config. Same test
        // body: CODEX_HOME is process-global, so concurrent setters race.
        let home = scratch_home();
        let dir = home.join(".codex");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "# empty\n").unwrap();
        std::env::set_var("CODEX_HOME", &dir);
        let out = ensure_codex_mcp_env(&home);
        std::env::remove_var("CODEX_HOME");
        assert!(out.is_err(), "out: {out:?}");
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
        // codex MCP shells out; point it at a fake binary. The fake mimics
        // `mcp add` by writing the section; the installer must then patch
        // the env passthrough into it.
        let fake = home.join("codex");
        std::fs::write(
            &fake,
            "#!/bin/sh\nmkdir -p \"$CODEX_HOME\"\nprintf '[mcp_servers.forge]\\ncommand = \"fake\"\\nargs = [\"mcp-serve\"]\\n' >> \"$CODEX_HOME/config.toml\"\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake, perms).unwrap();
        }
        let codex_dir = home.join(".codex");
        std::env::set_var("CODEX_HOME", &codex_dir);
        std::env::set_var("CODEX_BIN", &fake);
        let outs = install_one(&home, "codex", FORGE_BIN);
        std::env::remove_var("CODEX_BIN");
        std::env::remove_var("CODEX_HOME");
        assert_eq!(outs.len(), 3);
        assert!(outs.iter().all(|o| o.error.is_none()), "outs: {outs:?}");
        let text = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(text.contains("env_vars"), "passthrough patched: {text}");
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
        assert!(outs.iter().any(|o| o.harness == "muse" && o.installed));
        let _ = std::fs::remove_dir_all(&home);
    }
}
