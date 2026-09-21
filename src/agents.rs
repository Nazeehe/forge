//! Agent CLI registry: what forge needs to know to run an agent CLI,
//! loaded from `~/.forge/agents.json` (packaged default below is
//! materialized there on first launch when missing).
//!
//! No serde derive in the tree, so parsing is manual over
//! `serde_json::Value` — which also makes unknown-field rejection and
//! exact error messages straightforward. A bad file is a hard startup
//! error, never a silent fallback.

use std::path::Path;

/// Packaged default, placed in `~/.forge` when no agents file exists.
pub const DEFAULT_AGENTS_JSON: &str = include_str!("../assets/agents.json");

/// The only schema version this forge understands.
pub const EXPECTED_VERSION: u64 = 1;

/// Substrings that never appear in launch argv: forge gates through
/// hooks where supported and never adds approval bypasses itself, so a
/// config must not smuggle them in either.
const BYPASS_DENYLIST: [&str; 2] = ["yolo", "dangerously"];

/// How a hook SessionStart report is attributed to a live pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
    /// The hook environment names the session (normal case).
    HookEnv,
    /// The CLI scrubs the hook environment: use the hook process's inherited
    /// PTY session, falling back to a recent unbound session in the same cwd
    /// for records from an older relay.
    CwdWindow,
}

/// How the harness conversation id rides on a resume spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WithId {
    /// A flag taking the id: `--resume <id>`.
    Flag(String),
    /// A bare positional: `resume <id>`.
    Positional,
}

/// Resume argv template for one agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeDef {
    /// Subcommand prepended after the binary, if any (`resume`).
    pub subcommand: Option<String>,
    /// How a known conversation id is passed.
    pub with_id: WithId,
    /// Literal argv used when no id is known (`--continue`, `--last`).
    pub without_id: Vec<String>,
}

/// Everything forge needs to run one agent CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDef {
    /// Registry id: the `cli_tool` string, dialog choice, installer key.
    pub name: String,
    /// Default binary name, resolved through PATH at spawn.
    pub binary: String,
    /// Env var holding an explicit binary path; wins when set non-empty.
    pub env_override: String,
    /// Flag taking the model on fresh launches (`--model`).
    pub model_flag: String,
    /// Model used when the session spec names none; empty means the
    /// CLI default.
    pub default_model: String,
    /// Always appended: fresh launches after the model, restores after
    /// the resume tail.
    pub extra_args: Vec<String>,
    pub resume: ResumeDef,
    pub supports_hooks: bool,
    pub attribution: Attribution,
}

static REGISTRY: std::sync::OnceLock<Vec<AgentDef>> = std::sync::OnceLock::new();
static FALLBACK: std::sync::OnceLock<Vec<AgentDef>> = std::sync::OnceLock::new();

/// Process-wide registry: the startup-loaded file, or the packaged
/// default when startup never ran (unit tests). Set once; startup is
/// the only writer.
pub fn registry() -> &'static [AgentDef] {
    if let Some(loaded) = REGISTRY.get() {
        return loaded;
    }
    FALLBACK.get_or_init(|| {
        parse_agents(DEFAULT_AGENTS_JSON).expect("packaged agents.json is valid")
    })
}

/// Read, validate, and install the registry. Hard error on any problem;
/// the caller (startup) refuses to boot without good agent definitions.
pub fn load_registry(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{}: cannot read agents file: {e}", path.display()))?;
    let agents = parse_agents(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    REGISTRY
        .set(agents)
        .map_err(|_| "agent registry already loaded".to_string())
}

/// Parse and validate one agents file. Pure, so tests never touch the
/// process-wide registry.
pub fn parse_agents(text: &str) -> Result<Vec<AgentDef>, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let top = value
        .as_object()
        .ok_or_else(|| "top level must be an object".to_string())?;
    reject_unknown(top, &["version", "agents"], "top level")?;
    let version = top
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "version must be a positive integer".to_string())?;
    if version != EXPECTED_VERSION {
        return Err(format!("unsupported version {version}, expected {EXPECTED_VERSION}"));
    }
    let raw_agents = top
        .get("agents")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "agents must be an array".to_string())?;
    if raw_agents.is_empty() {
        return Err("agents must not be empty".to_string());
    }
    let mut agents = Vec::with_capacity(raw_agents.len());
    for (i, raw) in raw_agents.iter().enumerate() {
        agents.push(parse_agent(raw, i)?);
    }
    let mut names = std::collections::HashSet::new();
    for agent in &agents {
        if !names.insert(agent.name.as_str()) {
            return Err(format!("duplicate agent name {:?}", agent.name));
        }
    }
    Ok(agents)
}

fn parse_agent(raw: &serde_json::Value, i: usize) -> Result<AgentDef, String> {
    let ctx = format!("agents[{i}]");
    let obj = raw
        .as_object()
        .ok_or_else(|| format!("{ctx} must be an object"))?;
    reject_unknown(
        obj,
        &[
            "name",
            "binary",
            "env_override",
            "model_flag",
            "default_model",
            "extra_args",
            "resume",
            "supports_hooks",
            "session_attribution",
        ],
        &ctx,
    )?;
    let name = required_str(obj, &ctx, "name")?;
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c == '/') {
        return Err(format!(
            "{ctx}: name {name:?} must be non-empty with no whitespace or '/'"
        ));
    }
    let binary = required_str(obj, &ctx, "binary")?;
    if binary.is_empty() {
        return Err(format!("{ctx}: binary must be non-empty"));
    }
    let resume = obj
        .get("resume")
        .ok_or_else(|| format!("{ctx}: missing resume"))?;
    Ok(AgentDef {
        name,
        binary,
        env_override: opt_str(obj, &ctx, "env_override")?.unwrap_or_default(),
        model_flag: opt_str(obj, &ctx, "model_flag")?.unwrap_or_else(|| "--model".to_string()),
        default_model: opt_str(obj, &ctx, "default_model")?.unwrap_or_default(),
        extra_args: str_list(obj, &ctx, "extra_args")?.unwrap_or_default(),
        resume: parse_resume(resume, &ctx)?,
        supports_hooks: opt_bool(obj, &ctx, "supports_hooks")?.unwrap_or(false),
        attribution: parse_attribution(opt_str(obj, &ctx, "session_attribution")?)?,
    })
}

fn parse_resume(raw: &serde_json::Value, ctx: &str) -> Result<ResumeDef, String> {
    let ctx = format!("{ctx}.resume");
    let obj = raw
        .as_object()
        .ok_or_else(|| format!("{ctx} must be an object"))?;
    reject_unknown(obj, &["subcommand", "with_id", "without_id"], &ctx)?;
    let subcommand = match obj.get("subcommand") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{ctx}.subcommand must be a non-empty string or null"))?
                .to_string(),
        ),
    };
    let with_id = obj
        .get("with_id")
        .ok_or_else(|| format!("{ctx}: missing with_id"))?;
    let with_id = parse_with_id(with_id, &ctx)?;
    let without_id = obj
        .get("without_id")
        .ok_or_else(|| format!("{ctx}: missing without_id"))?;
    let without_id = without_id
        .as_array()
        .ok_or_else(|| format!("{ctx}.without_id must be an array of strings"))?;
    if without_id.is_empty() {
        return Err(format!("{ctx}.without_id must not be empty"));
    }
    let mut tail = Vec::with_capacity(without_id.len());
    for arg in without_id {
        let arg = arg
            .as_str()
            .ok_or_else(|| format!("{ctx}.without_id must be an array of strings"))?;
        check_bypasses(arg, &ctx, "without_id")?;
        tail.push(arg.to_string());
    }
    Ok(ResumeDef { subcommand, with_id, without_id: tail })
}

fn parse_with_id(raw: &serde_json::Value, ctx: &str) -> Result<WithId, String> {
    let ctx = format!("{ctx}.with_id");
    let obj = raw
        .as_object()
        .ok_or_else(|| format!("{ctx} must be an object"))?;
    reject_unknown(obj, &["flag", "positional"], &ctx)?;
    match (obj.get("flag"), obj.get("positional")) {
        (Some(flag), None) => {
            let flag = flag
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{ctx}.flag must be a non-empty string"))?;
            Ok(WithId::Flag(flag.to_string()))
        }
        (None, Some(pos)) => {
            if pos.as_bool() == Some(true) {
                Ok(WithId::Positional)
            } else {
                Err(format!("{ctx}: positional must be true when present"))
            }
        }
        _ => Err(format!("{ctx}: exactly one of flag or positional is required")),
    }
}

fn parse_attribution(raw: Option<String>) -> Result<Attribution, String> {
    match raw.as_deref().unwrap_or("hook_env") {
        "hook_env" => Ok(Attribution::HookEnv),
        "cwd_window" => Ok(Attribution::CwdWindow),
        other => Err(format!(
            "session_attribution must be hook_env or cwd_window, got {other:?}"
        )),
    }
}

fn check_bypasses(arg: &str, ctx: &str, field: &str) -> Result<(), String> {
    for denied in BYPASS_DENYLIST {
        if arg.contains(denied) {
            return Err(format!(
                "{ctx}: {field:?} must not carry approval bypasses ({denied:?})"
            ));
        }
    }
    Ok(())
}

fn reject_unknown(
    obj: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    ctx: &str,
) -> Result<(), String> {
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(format!("{ctx}: unknown field {key:?}"));
        }
    }
    Ok(())
}

fn required_str(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    field: &str,
) -> Result<String, String> {
    obj.get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{ctx}: missing string {field:?}"))
}

fn opt_str(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    field: &str,
) -> Result<Option<String>, String> {
    match obj.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| format!("{ctx}: {field:?} must be a string")),
    }
}

fn opt_bool(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    field: &str,
) -> Result<Option<bool>, String> {
    match obj.get(field) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| format!("{ctx}: {field:?} must be a boolean")),
    }
}

fn str_list(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    field: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = obj.get(field) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let items = raw
        .as_array()
        .ok_or_else(|| format!("{ctx}: {field:?} must be an array of strings"))?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let arg = item
            .as_str()
            .ok_or_else(|| format!("{ctx}: {field:?} must be an array of strings"))?;
        check_bypasses(arg, ctx, field)?;
        out.push(arg.to_string());
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_default_parses_to_three_known_agents() {
        let agents = parse_agents(DEFAULT_AGENTS_JSON).expect("packaged default valid");
        assert_eq!(
            agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["claude", "codex", "muse"]
        );
        assert_eq!(agents[0].resume.subcommand, None);
        assert_eq!(
            agents[0].resume.with_id,
            WithId::Flag("--resume".to_string())
        );
        assert_eq!(agents[0].resume.without_id, ["--continue"]);
        assert_eq!(agents[1].resume.subcommand.as_deref(), Some("resume"));
        assert_eq!(agents[1].resume.with_id, WithId::Positional);
        assert_eq!(agents[2].attribution, Attribution::CwdWindow);
        assert!(agents.iter().all(|a| a.extra_args.is_empty()));
        assert!(agents.iter().all(|a| a.supports_hooks));
    }

    #[test]
    fn extra_args_parse_and_reject_bypasses() {
        let mut value: serde_json::Value = serde_json::from_str(DEFAULT_AGENTS_JSON).unwrap();
        value["agents"][0]["extra_args"] =
            serde_json::json!(["--verbose", "--max-turns", "10"]);
        let agents = parse_agents(&value.to_string()).unwrap();
        assert_eq!(agents[0].extra_args, ["--verbose", "--max-turns", "10"]);
        value["agents"][0]["extra_args"] =
            serde_json::json!(["--dangerously-skip-permissions"]);
        assert!(parse_agents(&value.to_string()).is_err(), "no bypasses");
    }

    #[test]
    fn load_registry_installs_file_and_rejects_bad_ones() {
        // Default content: installing the global keeps every other test
        // on the same three agents regardless of execution order.
        let dir = std::env::temp_dir().join(format!("forge-agents-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("agents.json");
        std::fs::write(&path, DEFAULT_AGENTS_JSON).unwrap();
        load_registry(&path).expect("default file loads");
        assert_eq!(registry().len(), 3);
        assert_eq!(registry()[0].name, "claude");
        std::fs::write(&path, r#"{"version": 1, "agents": []}"#).unwrap();
        assert!(load_registry(&path).is_err(), "empty registry rejected");
        assert_eq!(registry().len(), 3, "failed load changes nothing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn broken(mutate: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value: serde_json::Value = serde_json::from_str(DEFAULT_AGENTS_JSON).unwrap();
        mutate(&mut value);
        parse_agents(&value.to_string())
            .expect_err("must reject")
            .to_lowercase()
    }

    #[test]
    fn validation_rejects_bad_files() {
        assert!(parse_agents("not json").is_err());
        assert!(parse_agents(r#"{"version": 1}"#).is_err(), "missing agents");
        assert!(parse_agents(r#"{"version": 2, "agents": []}"#).is_err(), "bad version");
        assert!(parse_agents(r#"{"version": 1, "agents": []}"#).is_err(), "empty registry");
        let e = broken(|v| v["agents"][1]["name"] = serde_json::json!("claude"));
        assert!(e.contains("duplicate"), "dup names: {e}");
        let e = broken(|v| v["agents"][0]["bogus"] = serde_json::json!(1));
        assert!(e.contains("unknown field"), "unknown agent field: {e}");
        let e = broken(|v| v["zzz"] = serde_json::json!(1));
        assert!(e.contains("unknown field"), "unknown top field: {e}");
        let e = broken(|v| v["agents"][0]["binary"] = serde_json::json!(""));
        assert!(e.contains("binary"), "empty binary: {e}");
        let e = broken(|v| v["agents"][0]["name"] = serde_json::json!("my agent"));
        assert!(e.contains("name"), "spaced name: {e}");
        let e = broken(|v| {
            v["agents"][0]["resume"]["with_id"] = serde_json::json!({"flag": "--r", "positional": true})
        });
        assert!(e.contains("exactly one"), "both id styles: {e}");
        let e = broken(|v| {
            v["agents"][0]["resume"]["with_id"] = serde_json::json!({})
        });
        assert!(e.contains("exactly one"), "neither id style: {e}");
        let e = broken(|v| v["agents"][0]["resume"]["without_id"] = serde_json::json!([]));
        assert!(e.contains("without_id"), "empty fallback: {e}");
        let e = broken(|v| {
            v["agents"][0]["session_attribution"] = serde_json::json!("telepathy")
        });
        assert!(e.contains("session_attribution"), "bad attribution: {e}");
        let e = broken(|v| {
            v["agents"][0].as_object_mut().unwrap().remove("resume");
        });
        assert!(e.contains("resume"), "missing resume: {e}");
    }
}
