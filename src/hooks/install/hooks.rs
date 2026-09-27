//! Per-harness hook-file install and uninstall.

use super::*;

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
        "gemini" => {
            // Grounded in the official hooks reference: settings.json
            // takes `hooks: {Event: [{matcher, sequential, hooks:
            // [{type, command, timeout}]}]}` with the hook JSON on stdin.
            // SessionStart carries the harness-side conversation id the
            // restore path resumes with; Before/AfterAgent are the turn
            // edges, BeforeTool the deny layer, SessionEnd the exit edge.
            // `timeout` caps a wedged relay on the harness side; the
            // relay itself gives up after 3 s and always exits 0, so
            // 10 s never blocks a session.
            let path = home.join(".gemini/settings.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let handler = serde_json::json!({
                "type": "command",
                "command": hook_command(forge_bin),
                "timeout": 10000,
            });
            let added = match merge_hook_groups(
                &mut v,
                &path,
                &[
                    "SessionStart",
                    "BeforeTool",
                    "AfterTool",
                    "BeforeAgent",
                    "AfterAgent",
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
        "copilot" => {
            // Grounded in the official hooks reference: user hook files
            // live in ~/.copilot/hooks/ (or $COPILOT_HOME/hooks/) as
            // `{version: 1, hooks: {event: [entries]}}`. `exec`+`args`
            // runs the relay directly without a shell (CLI-only, like
            // codex); timeouts are fail-open on the harness side, and
            // the relay itself gives up after 3 s, so 10 s never blocks
            // a session. sessionStart reports the session, the prompt
            // and agent edges mirror the other CLIs' turn tracking, and
            // preToolUse/permissionRequest are the approval gates.
            let path = copilot_home(home).join("hooks/forge.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            if let Some(version) = v.get("version") {
                if version != &serde_json::Value::from(1) {
                    return Outcome::error(
                        harness,
                        format!("unsupported version in {}", path.display()),
                    );
                }
            } else {
                v["version"] = serde_json::Value::from(1);
            }
            if !v.get("hooks").is_some_and(|h| h.is_object()) {
                v["hooks"] = serde_json::Value::Object(Default::default());
            }
            let ours = forge_bin.to_string();
            let mut added = 0;
            for event in [
                "sessionStart",
                "userPromptSubmitted",
                "preToolUse",
                "permissionRequest",
                "agentStop",
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
                let entries = slot.as_array_mut().expect("just checked array");
                let present = entries.iter().any(|e| {
                    e.get("exec").and_then(|c| c.as_str()) == Some(&ours)
                        && e.get("args").and_then(|a| a.as_array()).is_some_and(|args| {
                            args.iter().any(|a| a.as_str() == Some("hook-relay"))
                        })
                });
                if !present {
                    entries.push(serde_json::json!({
                        "type": "command",
                        "exec": forge_bin,
                        "args": ["hook-relay"],
                        "timeoutSec": 10,
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
        // Pi (pi-mono) exposes no native hook surface: its extension
        // bus (pi.on events) is in-process only, so there is nothing to
        // register. Skips are not errors.
        "pi" => Outcome::skipped(
            harness,
            "no native pi hook mechanism; pi hooks stay uninstalled".to_string(),
        ),
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
    }
}

/// Remove hooks this installer owns. Entries mentioning other commands are
/// kept; the codex file goes away only when nothing foreign remains.
/// A copilot hook entry this installer owns: our relay invoked either
/// directly (`exec` carrying hook-relay) or with hook-relay in `args`.
fn is_copilot_ours(entry: &serde_json::Value) -> bool {
    entry
        .get("exec")
        .and_then(|c| c.as_str())
        .is_some_and(is_ours)
        || entry
            .get("args")
            .and_then(|a| a.as_array())
            .is_some_and(|args| {
                args.iter()
                    .any(|a| a.as_str().is_some_and(is_ours))
            })
}

pub fn uninstall_one_hooks(home: &std::path::Path, harness: &str) -> Outcome {
    match harness {
        "claude" | "muse" | "gemini" => {
            let path = match harness {
                "claude" => home.join(".claude/settings.json"),
                "gemini" => home.join(".gemini/settings.json"),
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
        "copilot" => {
            // Our entries live in our own forge.json: drop them wherever
            // they sit, prune emptied events, and remove the file when
            // nothing remains. Anything foreign is kept; sibling files
            // are never touched.
            let path = copilot_home(home).join("hooks/forge.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            let mut dropped = 0;
            if let Some(obj) = v.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                let mut dead_events = Vec::new();
                for (event, slot) in obj.iter_mut() {
                    let Some(entries) = slot.as_array_mut() else {
                        continue;
                    };
                    let before = entries.len();
                    entries.retain(|e| !is_copilot_ours(e));
                    dropped += before - entries.len();
                    if entries.is_empty() {
                        dead_events.push(event.clone());
                    }
                }
                for event in dead_events {
                    obj.remove(&event);
                }
            }
            if dropped == 0 {
                return Outcome::unchanged(harness, "nothing to remove".to_string());
            }
            let hooks_empty = v
                .get("hooks")
                .is_some_and(|h| h.as_object().is_some_and(|o| o.is_empty()));
            if hooks_empty {
                match std::fs::remove_file(&path) {
                    Ok(()) => Outcome::removed(harness, path.display().to_string()),
                    Err(e) => {
                        Outcome::error(harness, format!("cannot remove {}: {e}", path.display()))
                    }
                }
            } else {
                match write_json(&path, &v) {
                    Ok(()) => Outcome::removed(harness, path.display().to_string()),
                    Err(e) => Outcome::error(harness, e),
                }
            }
        }
        "pi" => Outcome::skipped(harness, "pi hooks were never installed".to_string()),
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
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
    }
}

/// Pin COPILOT_HOME at scratch so copilot paths never consult the
/// developer's real config. Same shape as ClearCodexHome: the
/// variable is process-global, so holders serialize on the lock.
struct ClearCopilotHome {
    saved: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ClearCopilotHome {
    fn pin() -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("COPILOT_HOME").ok();
        std::env::remove_var("COPILOT_HOME");
        ClearCopilotHome {
            saved,
            _lock: lock,
        }
    }
}

impl Drop for ClearCopilotHome {
    fn drop(&mut self) {
        if let Some(v) = self.saved.take() {
            std::env::set_var("COPILOT_HOME", v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::install::test_support::*;
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
        // PermissionRequest joins the set: it is Codex's approval gate and
        // the only verdict that can auto-approve a call, so forge YOLO
        // needs it registered (PreToolUse alone can only deny there).
        for event in [
            "SessionStart",
            "PreToolUse",
            "PermissionRequest",
            "Stop",
            "UserPromptSubmit",
        ] {
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
    fn gemini_hooks_merge_and_are_idempotent() {
        // Grounded in the official hooks reference: settings.json takes
        // `hooks: {Event: [{matcher?, sequential?, hooks: [...]}]}` with
        // `{type: "command", command, timeout?}` entries.
        let home = scratch_home();
        let out = install_one_hooks(&home, "gemini", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for event in [
            "SessionStart",
            "BeforeTool",
            "AfterTool",
            "BeforeAgent",
            "AfterAgent",
            "SessionEnd",
        ] {
            let groups = v["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("missing {event}: {text}"));
            assert!(
                groups.iter().any(|g| {
                    g["hooks"].as_array().is_some_and(|hs| {
                        hs.iter().any(|h| {
                            h["command"].as_str() == Some(&hook_command(FORGE_BIN))
                        })
                    })
                }),
                "ours on {event}: {text}"
            );
        }
        // Second install adds no duplicate.
        install_one_hooks(&home, "gemini", FORGE_BIN);
        let text = std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
        assert_eq!(text.matches("hook-relay").count(), 6, "one per event: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_hooks_uninstall_removes_only_ours() {
        let home = scratch_home();
        install_one_hooks(&home, "gemini", FORGE_BIN);
        let out = uninstall_one_hooks(&home, "gemini");
        assert!(out.removed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
        assert!(!text.contains("hook-relay"), "gone: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_hooks_merge_and_are_idempotent() {
        // Grounded in the official hooks reference: user hook files live
        // in ~/.copilot/hooks/ as {version: 1, hooks: {event: [entries]}}
        // with {type: command, exec, args, timeoutSec?} entries.
        let _pin = ClearCopilotHome::pin();
        let home = scratch_home();
        let out = install_one_hooks(&home, "copilot", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let path = home.join(".copilot/hooks/forge.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["version"], serde_json::Value::from(1));
        for event in [
            "sessionStart",
            "userPromptSubmitted",
            "preToolUse",
            "permissionRequest",
            "agentStop",
        ] {
            let entries = v["hooks"][event]
                .as_array()
                .unwrap_or_else(|| panic!("missing {event}: {text}"));
            assert!(
                entries.iter().any(|e| {
                    e["exec"].as_str() == Some(FORGE_BIN)
                        && e["args"][0].as_str() == Some("hook-relay")
                }),
                "ours on {event}: {text}"
            );
        }
        // Second install adds no duplicate.
        install_one_hooks(&home, "copilot", FORGE_BIN);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("hook-relay").count(), 5, "one per event: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_hooks_uninstall_removes_file_and_keeps_foreign() {
        let _pin = ClearCopilotHome::pin();
        let home = scratch_home();
        // A foreign hook file is never ours to touch.
        std::fs::create_dir_all(home.join(".copilot/hooks")).unwrap();
        std::fs::write(
            home.join(".copilot/hooks/other.json"),
            r#"{"version":1,"hooks":{"preToolUse":[{"type":"command","exec":"other"}]}}"#,
        )
        .unwrap();
        install_one_hooks(&home, "copilot", FORGE_BIN);
        let out = uninstall_one_hooks(&home, "copilot");
        assert!(out.removed, "out: {out:?}");
        assert!(
            !home.join(".copilot/hooks/forge.json").exists(),
            "our file goes away"
        );
        let foreign =
            std::fs::read_to_string(home.join(".copilot/hooks/other.json")).unwrap();
        assert!(foreign.contains("\"exec\":\"other\""), "foreign kept: {foreign}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
