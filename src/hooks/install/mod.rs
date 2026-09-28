//! Harness installers (Phase 5b): register forge hooks, MCP servers, and
//! skills with each agent CLI. Everything operates on an explicit home
//! directory so tests run on scratch dirs, never the developer's real
//! config. Codex MCP goes through the `codex` binary itself so user
//! `config.toml` comments survive; everything else is direct file surgery
//! in schemas observed live (see each function).

pub mod hooks;
pub mod mcp;
pub mod skills;
#[cfg(test)]
mod test_support;

pub use hooks::{install_one_hooks, uninstall_one_hooks};
use mcp::{install_one_mcp, uninstall_one_mcp};
use skills::{install_one_skills, uninstall_one_skills};

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
    pub(crate) fn installed(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: true,
            removed: false,
            skipped: false,
            error: None,
            detail,
        }
    }

    pub(crate) fn removed(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: true,
            skipped: false,
            error: None,
            detail,
        }
    }

    pub(crate) fn skipped(harness: &str, detail: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: false,
            skipped: true,
            error: None,
            detail,
        }
    }

    pub(crate) fn error(harness: &str, error: String) -> Self {
        Outcome {
            harness: harness.to_string(),
            installed: false,
            removed: false,
            skipped: false,
            error: Some(error),
            detail: String::new(),
        }
    }

    pub(crate) fn unchanged(harness: &str, detail: String) -> Self {
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

pub(crate) fn hook_command(forge_bin: &str) -> String {
    format!("{forge_bin} hook-relay")
}

pub(crate) fn is_ours(cmd: &str) -> bool {
    cmd.contains("hook-relay")
}

/// True when every listed event in a Claude-style hooks document carries
/// at least one forge relay handler. Lenient on the binary path (any
/// `hook-relay` counts): a moved forge binary still reads as set up
/// rather than stacking duplicate entries on the next repair.
pub(crate) fn hook_events_present(v: &serde_json::Value, events: &[&str]) -> bool {
    let Some(hooks) = v.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    events.iter().all(|event| {
        hooks.get(*event).and_then(|s| s.as_array()).is_some_and(|arr| {
            arr.iter().any(|e| {
                e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                    hs.iter().any(|h| {
                        h.get("command").and_then(|c| c.as_str()).is_some_and(is_ours)
                    })
                })
            })
        })
    })
}

/// True when a Claude-style settings document registers the forge MCP
/// server (`mcpServers.forge` object). Presence only: the installers own
/// the entry shape, the check only asks whether it exists.
pub(crate) fn mcp_server_present(v: &serde_json::Value) -> bool {
    v.get("mcpServers")
        .and_then(|m| m.get("forge"))
        .is_some_and(|s| s.is_object())
}

/// Read one JSON config for a setup check. Missing or corrupt files read
/// as `None` (not installed); the check never errors and never writes.
pub(crate) fn read_json_opt(path: &std::path::Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Read JSON, preserving everything we do not touch. Missing file is an
/// empty object; corrupt files are an error and are never clobbered.
pub(crate) fn read_json(path: &std::path::Path) -> Result<serde_json::Value, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(serde_json::Value::Object(Default::default()))
        }
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("cannot parse {}: {e}", path.display())),
    }
}

pub(crate) fn write_json(path: &std::path::Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("cannot encode {}: {e}", path.display()))?;
    crate::infra::fs_atomic::write_atomic(path, text.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub(crate) fn obj_mut<'v>(
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
pub(crate) fn merge_hook_groups(
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
pub(crate) fn drop_hook_groups(v: &mut serde_json::Value) -> usize {
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

/// Cross-harness aggregates in `agents.json` file order.
fn registry_names() -> Vec<&'static str> {
    crate::agents::harness::Harness::all()
        .into_iter()
        .map(|h| h.as_str())
        .collect()
}

pub fn install_hooks(home: &std::path::Path, forge_bin: &str) -> Vec<Outcome> {
    registry_names()
        .into_iter()
        .map(|h| install_one_hooks(home, h, forge_bin))
        .collect()
}

pub fn uninstall_hooks(home: &std::path::Path) -> Vec<Outcome> {
    registry_names()
        .into_iter()
        .map(|h| uninstall_one_hooks(home, h))
        .collect()
}

pub fn install_mcp(home: &std::path::Path, forge_bin: &str) -> Vec<Outcome> {
    registry_names()
        .into_iter()
        .map(|h| install_one_mcp(home, h, forge_bin))
        .collect()
}

pub fn uninstall_mcp(home: &std::path::Path) -> Vec<Outcome> {
    registry_names()
        .into_iter()
        .map(|h| uninstall_one_mcp(home, h))
        .collect()
}

pub fn install_skills(home: &std::path::Path) -> Vec<Outcome> {
    registry_names()
        .into_iter()
        .map(|h| install_one_skills(home, h))
        .collect()
}

pub fn uninstall_skills(home: &std::path::Path) -> Vec<Outcome> {
    registry_names()
        .into_iter()
        .map(|h| uninstall_one_skills(home, h))
        .collect()
}

/// Full per-harness install for `install-<agent>`. Names resolve through
/// the agent registry; unknown names are an error (the `metamate` blueprint
/// alias is dropped: `muse` is the registry name).
pub fn install_one(
    home: &std::path::Path,
    harness: &str,
    forge_bin: &str,
) -> Vec<Outcome> {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return vec![Outcome::error(harness, format!("unknown harness {harness:?}"))];
    }
    vec![
        install_one_hooks(home, harness, forge_bin),
        install_one_mcp(home, harness, forge_bin),
        install_one_skills(home, harness),
    ]
}

/// Ensure one harness is set up before launch: when hooks or MCP are
/// missing (a CLI installed after forge, or wiped config), run the full
/// per-harness install. Returns the install outcomes, or an empty vec
/// when already set up. Unknown harnesses error like [`install_one`].
/// Callers fail open: a repair error never blocks the launch.
pub fn ensure_installed(
    home: &std::path::Path,
    harness: &str,
    forge_bin: &str,
) -> Vec<Outcome> {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return vec![Outcome::error(harness, format!("unknown harness {harness:?}"))];
    }
    if crate::agents::adapter_for(harness).is_setup(home) {
        return Vec::new();
    }
    install_one(home, harness, forge_bin)
}

pub fn uninstall_one(home: &std::path::Path, harness: &str) -> Vec<Outcome> {
    if crate::agents::harness::Harness::from_name(harness).is_none() {
        return vec![Outcome::error(harness, format!("unknown harness {harness:?}"))];
    }
    vec![
        uninstall_one_hooks(home, harness),
        uninstall_one_mcp(home, harness),
        uninstall_one_skills(home, harness),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::test_support::*;

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
        // The `metamate` blueprint alias is dropped: `muse` is the name.
        let outs = install_one(&home, "metamate", FORGE_BIN);
        assert!(outs.iter().all(|o| o.error.is_some()));
        let outs = install_one(&home, "bogus", FORGE_BIN);
        assert!(outs.iter().all(|o| o.error.is_some()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn full_install_reports_every_harness() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        let outs = install_hooks(&home, FORGE_BIN);
        assert_eq!(outs.len(), 4);
        assert!(outs.iter().any(|o| o.harness == "claude" && o.installed));
        assert!(outs.iter().any(|o| o.harness == "codex" && o.installed));
        assert!(outs.iter().any(|o| o.harness == "muse" && o.installed));
        assert!(outs.iter().any(|o| o.harness == "agy" && o.skipped));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_repairs_a_cli_installed_after_forge() {
        // The reported bug: muse installed after forge has no hooks or
        // MCP, and the user should never have to run install-hooks by
        // hand. `ensure_installed` is the create-time repair.
        let home = scratch_home();
        let outs = ensure_installed(&home, "muse", FORGE_BIN);
        assert_eq!(outs.len(), 3, "full per-harness repair: {outs:?}");
        assert!(outs.iter().all(|o| o.error.is_none()), "outs: {outs:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert!(text.contains("hook-relay"), "hooks repaired: {text}");
        assert!(text.contains("mcpServers"), "mcp repaired: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_is_a_noop_when_setup_is_present() {
        let home = scratch_home();
        install_one(&home, "muse", FORGE_BIN);
        let before =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        let outs = ensure_installed(&home, "muse", FORGE_BIN);
        assert!(outs.is_empty(), "already set up repairs nothing: {outs:?}");
        let after =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert_eq!(before, after, "no rewrite means no duplicate entries");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_repairs_partial_setup() {
        // Hooks alone (what OOBE installs) still count as unset-up:
        // the missing MCP server must be added without duplicating hooks.
        let home = scratch_home();
        install_one_hooks(&home, "muse", FORGE_BIN);
        let outs = ensure_installed(&home, "muse", FORGE_BIN);
        assert!(!outs.is_empty(), "partial setup still repairs: {outs:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert_eq!(text.matches("hook-relay").count(), 5, "no dup hooks: {text}");
        assert!(text.contains("mcpServers"), "mcp added: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_repairs_claude_by_file_surgery() {
        let home = scratch_home();
        let outs = ensure_installed(&home, "claude", FORGE_BIN);
        assert!(outs.iter().all(|o| o.error.is_none()), "outs: {outs:?}");
        assert!(!outs.is_empty(), "missing setup repairs");
        let hooks = std::fs::read_to_string(home.join(".claude/settings.json")).unwrap();
        assert!(hooks.contains("hook-relay"), "hooks: {hooks}");
        let mcp = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        assert!(mcp.contains("\"forge\""), "mcp: {mcp}");
        assert!(ensure_installed(&home, "claude", FORGE_BIN).is_empty(), "second run is a noop");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_repairs_codex_through_its_cli() {
        let _pin = ClearCodexHome::pin();
        let home = scratch_home();
        // Codex MCP shells out; point it at a fake binary like the
        // per-harness installer test does.
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
        let outs = ensure_installed(&home, "codex", FORGE_BIN);
        std::env::remove_var("CODEX_BIN");
        std::env::remove_var("CODEX_HOME");
        assert!(outs.iter().all(|o| o.error.is_none()), "outs: {outs:?}");
        assert!(!outs.is_empty(), "missing setup repairs");
        let text = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(text.contains("env_vars"), "passthrough patched: {text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_skips_agents_without_a_setup_surface() {
        // agy has no hooks/MCP surface: there is nothing to repair, so
        // every create-time check must pass without touching the disk.
        let home = scratch_home();
        assert!(ensure_installed(&home, "agy", FORGE_BIN).is_empty());
        let outs = ensure_installed(&home, "bogus", FORGE_BIN);
        assert!(outs.iter().all(|o| o.error.is_some()), "unknown harness: {outs:?}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_never_clobbers_corrupt_config() {
        let home = scratch_home();
        std::fs::create_dir_all(home.join(".config/muse")).unwrap();
        std::fs::write(home.join(".config/muse/settings.json"), "{oops").unwrap();
        let outs = ensure_installed(&home, "muse", FORGE_BIN);
        assert!(!outs.is_empty(), "corrupt counts as unset-up");
        assert!(outs.iter().any(|o| o.error.is_some()), "repair reports the error: {outs:?}");
        let text =
            std::fs::read_to_string(home.join(".config/muse/settings.json")).unwrap();
        assert_eq!(text, "{oops", "corrupt file left intact for the launch to proceed");
        let _ = std::fs::remove_dir_all(&home);
    }
}
