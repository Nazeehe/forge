//! Per-harness MCP server registration, including the Codex CLI runner.

use super::*;

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
            let Some(bin) =
                crate::session::harness::Harness::from_name("codex").map(|h| h.spec().resolve_binary())
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
        // Grounded in muse 1.4.0, verified live: the `mcpServers` block in
        // ~/.config/muse/settings.json takes stdio entries {transport,
        // command, args, env, mode}. 1.4 rejects 1.2.1's `mcp_servers` key
        // and disables MCP entirely, so a stale forge entry there goes.
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
        // Grounded in the official MCP docs: the user settings file
        // takes a stdio `mcpServers` map of {command, args, env}.
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
        // Grounded in the official MCP docs: the user config at
        // ~/.copilot/mcp-config.json takes {mcpServers: {name: {type:
        // local, command, args, env, tools}}}. Direct file surgery like
        // claude, so no copilot binary is needed; everything else in
        // the file is preserved.
        "copilot" => {
            let path = home.join(".copilot/mcp-config.json");
            let mut v = match read_json(&path) {
                Ok(v) => v,
                Err(e) => return Outcome::error(harness, e),
            };
            obj_mut(&mut v, "mcpServers").insert(
                "forge".to_string(),
                serde_json::json!({
                    "type": "local",
                    "command": forge_bin,
                    "args": ["mcp-serve"],
                    "env": {},
                    "tools": ["*"],
                }),
            );
            match write_json(&path, &v) {
                Ok(()) => Outcome::installed(harness, path.display().to_string()),
                Err(e) => Outcome::error(harness, e),
            }
        }
        // Pi has no native MCP surface (only a third-party adapter with
        // no documented config contract), so registration stays a skip.
        "pi" => Outcome::skipped(
            harness,
            "no native pi MCP surface; pi MCP stays unregistered".to_string(),
        ),
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
    crate::infra::fs_atomic::write_atomic(&path, out.as_bytes())
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
            let Some(bin) =
                crate::session::harness::Harness::from_name("codex").map(|h| h.spec().resolve_binary())
            else {
                return Outcome::error(harness, "codex is not in agents.json".to_string());
            };
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
        "copilot" => {
            let path = home.join(".copilot/mcp-config.json");
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
        "pi" => Outcome::skipped(harness, "pi MCP was never registered".to_string()),
        other => Outcome::error(harness, format!("unknown harness {other:?}")),
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

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn gemini_mcp_round_trips() {
        let home = scratch_home();
        let out = install_one_mcp(&home, "gemini", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text = std::fs::read_to_string(home.join(".gemini/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["forge"]["command"], serde_json::Value::String(FORGE_BIN.to_string()));
        let out = uninstall_one_mcp(&home, "gemini");
        assert!(out.removed, "out: {out:?}");
        let skills = install_one_skills(&home, "gemini");
        assert!(skills.skipped, "out: {skills:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_mcp_registers_forge_server() {
        // Grounded in the official MCP docs: user config lives in
        // ~/.copilot/mcp-config.json as {mcpServers: {name: {type:
        // local, command, args, ...}}}.
        let home = scratch_home();
        let out = install_one_mcp(&home, "copilot", FORGE_BIN);
        assert!(out.installed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".copilot/mcp-config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let srv = &v["mcpServers"]["forge"];
        assert_eq!(srv["type"], serde_json::Value::String("local".to_string()));
        assert_eq!(srv["command"], serde_json::Value::String(FORGE_BIN.to_string()));
        assert_eq!(srv["args"][0], serde_json::Value::String("mcp-serve".to_string()));
        let out = uninstall_one_mcp(&home, "copilot");
        assert!(out.removed, "out: {out:?}");
        let text =
            std::fs::read_to_string(home.join(".copilot/mcp-config.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(v["mcpServers"].get("forge").is_none(), "gone: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
