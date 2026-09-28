//! Per-harness MCP server registration, including the Codex CLI runner.

use super::*;

/// Register the forge MCP server for one harness. Behavior lives in the
/// agent adapters; unknown registry names are an error.
pub fn install_one_mcp(home: &std::path::Path, harness: &str, forge_bin: &str) -> Outcome {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return Outcome::error(harness, format!("unknown harness {harness:?}"));
    }
    let mut out = crate::agents::adapter_for(harness).mcp_install(home, forge_bin);
    out.harness = harness.to_string();
    out
}

/// Unregister the forge MCP server for one harness. Behavior lives in the
/// agent adapters; unknown registry names are an error.
pub fn uninstall_one_mcp(home: &std::path::Path, harness: &str) -> Outcome {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return Outcome::error(harness, format!("unknown harness {harness:?}"));
    }
    let mut out = crate::agents::adapter_for(harness).mcp_uninstall(home);
    out.harness = harness.to_string();
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::codex::ensure_codex_mcp_env;
    use crate::hooks::install::test_support::*;
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
    fn muse_mcp_writes_verified_schema() {
        // Grounded in muse 1.4.0: it reads `mcpServers` and rejects the old
        // 1.2.1 `mcp_servers` key ("MCP configuration error ... MCP is
        // disabled for this runtime"), so a stale entry must be migrated away.
        let home = scratch_home();
        std::fs::create_dir_all(home.join(".config/muse")).unwrap();
        std::fs::write(
            home.join(".config/muse/settings.json"),
            r#"{"mcp_servers": {"forge": {"command": "/old/forge", "args": ["mcp-serve"]}}}"#,
        )
        .unwrap();
        let out = install_one_mcp(&home, "muse", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v.get("mcp_servers").is_none(), "1.2.1 key migrated away: {text}");
        let srv = &v["mcpServers"]["forge"];
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
        assert!(srv.get("enabled").is_none(), "not part of the 1.4 entry: {text}");
        assert_eq!(srv["mode"], serde_json::Value::String("optional".to_string()));
        // Hooks and MCP share the file: installing both keeps both.
        install_one_hooks(&home, "muse", FORGE_BIN);
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert!(text.contains("mcpServers"), "mcp kept: {text}");
        assert!(text.contains("hook-relay"), "hooks kept: {text}");
        let out = uninstall_one_mcp(&home, "muse");
        assert!(out.removed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["mcpServers"].get("forge").is_none(), "gone: {text}");
        assert!(text.contains("hook-relay"), "hooks kept: {text}");
        // Uninstall also clears a stale 1.2.1 entry on its own.
        std::fs::write(
            home.join(".config/muse/settings.json"),
            r#"{"mcp_servers": {"forge": {"command": "/old/forge"}, "other": {}}}"#,
        )
        .unwrap();
        assert!(uninstall_one_mcp(&home, "muse").removed);
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["mcp_servers"].get("forge").is_none(), "stale gone: {text}");
        assert!(v["mcp_servers"].get("other").is_some(), "others kept: {text}");
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

}
