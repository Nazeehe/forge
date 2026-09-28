//! Per-harness hook-file install and uninstall.

use super::*;

/// Install hooks for one harness by name.
pub fn install_one_hooks(
    home: &std::path::Path,
    harness: &str,
    forge_bin: &str,
) -> Outcome {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return Outcome::error(harness, format!("unknown harness {harness:?}"));
    }
    let mut out = crate::agents::adapter_for(harness).hook_install(home, forge_bin);
    out.harness = harness.to_string();
    out
}

/// Remove hooks this installer owns. Entries mentioning other commands are
/// kept; the codex file goes away only when nothing foreign remains.
pub fn uninstall_one_hooks(home: &std::path::Path, harness: &str) -> Outcome {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return Outcome::error(harness, format!("unknown harness {harness:?}"));
    }
    let mut out = crate::agents::adapter_for(harness).hook_uninstall(home);
    out.harness = harness.to_string();
    out
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
}
