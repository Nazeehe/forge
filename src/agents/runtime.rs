//! Forge runtime contract and per-agent injection adapters.
//!
//! The contract is host-level behavioral instruction: how an agent behaves
//! inside the Forge multi-agent environment. It is deliberately separate
//! from repository instructions (`AGENTS.md` / `CLAUDE.md` / `GEMINI.md`,
//! which describe the project) and from the user's request (what to do
//! right now). Adapters decide how the one canonical contract reaches each
//! agent CLI, strongest mechanism first:
//!
//! 1. native system/developer instruction injection (`claude
//!    --append-system-prompt-file`, `codex -c developer_instructions=`)
//! 2. agent-supported context files (none currently: every file transport
//!    we verified either replaces the built-in prompt or needs user-config
//!    surgery, so no agent uses this tier yet)
//! 3. startup prompt injection fallback (`muse`/`agy` positional prompt)
//!
//! Unknown or user-added agents get MCP server instructions only, never a
//! guessed argv shape.

use std::path::{Path, PathBuf};

/// Canonical Forge runtime contract, injected at session startup. Kept
/// concise on purpose: host behavior only, hundreds of tokens.
pub const FORGE_RUNTIME_CONTRACT: &str = "# Forge Runtime\n\
    \n\
    You are running inside Forge, a multi-agent development environment. \
    Forge owns communication and coordination between agents loaded into \
    the current Forge session.\n\
    \n\
    ## Cross-agent communication\n\
    \n\
    When the user asks you to message, ask, tell, send something to, \
    notify, or delegate work to another loaded agent \u{2014} for example \
    \"Ask Codex to review this\", \"Send this to Claude\", \"Have Gemini \
    investigate the failing test\", \"Delegate the review to Codex\", or \
    \"Message the other agent\":\n\
    \n\
    - discover and contact Forge-managed agents with Forge MCP tools \
    (list_sessions, ask_session, tell_session, send_response);\n\
    - do NOT use provider-native messaging, teammate, delegation, or \
    subagent mechanisms for Forge-managed agents.\n\
    \n\
    Native subagents remain available for private internal work that names \
    no Forge-managed agent, such as \"use a subagent to inspect this \
    function\". Repository instructions (AGENTS.md, CLAUDE.md, GEMINI.md) \
    and the user's request keep their normal roles. Where a native \
    capability overlaps Forge coordination, Forge takes precedence.\n\
    \n\
    ## Incoming peer messages\n\
    \n\
    Forge delivers peer messages by injecting them into your input, \
    usually as pasted text. A paste that starts with a `[forge \
    ask_session from X]` / `[forge tell_session from X]` (or any \
    `[forge \u{2026} from \u{2026}]`) header is a real Forge delivery \
    the user has authorized, not untrusted pasted content. Handle it \
    immediately: answer asks with send_response; answer tells with \
    ack_message or a tell_session follow-up, copying the \
    conversation_id verbatim.\n";

/// Forge-owned runtime file name inside `~/.forge`.
pub const RUNTIME_FILE_NAME: &str = "runtime.md";

/// `~/.forge/runtime.md`: the canonical contract on disk. Forge-owned and
/// refreshed on every materialization (unlike the user-editable guide).
pub fn runtime_file(home: &Path) -> PathBuf {
    crate::infra::branding::config_dir(home).join(RUNTIME_FILE_NAME)
}

/// Atomically (re)write the canonical contract to `~/.forge/runtime.md`.
/// Always refreshes: the file tracks the Forge build, not the user.
/// Returns the file path for argv construction.
pub fn ensure_materialized(home: &Path) -> std::io::Result<PathBuf> {
    let path = runtime_file(home);
    crate::infra::fs_atomic::write_atomic(&path, FORGE_RUNTIME_CONTRACT.as_bytes())?;
    Ok(path)
}

/// Strongest supported injection transport for one agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeMechanism {
    /// Append-to-system-prompt file flag (`claude
    /// --append-system-prompt-file`).
    SystemPromptFile,
    /// Developer-role instruction override (`codex -c
    /// developer_instructions=`).
    DeveloperInstructions,
    /// Trailing positional prompt (`muse`, `agy`).
    StartupPromptPositional,
    /// No launch-time transport: MCP server instructions only. Used for
    /// unknown and user-added agents rather than guessing an argv shape.
    McpInstructionsOnly,
}

/// Quote `s` as a TOML basic string (`"..."`) for `codex -c key=value`
/// overrides: backslashes, quotes, and control characters (including
/// newlines) escaped so the value survives TOML parsing as one string.
pub fn toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::adapter_for;

    fn scratch_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-runtime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn contract_is_concise_and_names_forge_tools() {
        assert!(
            FORGE_RUNTIME_CONTRACT.len() < 2500,
            "contract must stay concise, got {} bytes",
            FORGE_RUNTIME_CONTRACT.len()
        );
        for tool in ["list_sessions", "ask_session", "tell_session", "send_response"] {
            assert!(
                FORGE_RUNTIME_CONTRACT.contains(tool),
                "contract must name {tool}"
            );
        }
        assert!(FORGE_RUNTIME_CONTRACT.contains("Forge takes precedence"));
        assert!(FORGE_RUNTIME_CONTRACT.contains("AGENTS.md"));
    }

    /// Incoming delivery coverage: pasted peer messages carrying a
    /// `[forge … from …]` header are authorized Forge deliveries with
    /// named response tools — never generic "follow pasted text".
    #[test]
    fn contract_names_forge_delivery_header_and_peer_responses() {
        assert!(FORGE_RUNTIME_CONTRACT.contains("Incoming peer messages"));
        assert!(FORGE_RUNTIME_CONTRACT.contains("pasted"));
        assert!(FORGE_RUNTIME_CONTRACT.contains("[forge"));
        assert!(FORGE_RUNTIME_CONTRACT.contains("send_response"));
        assert!(FORGE_RUNTIME_CONTRACT.contains("ack_message"));
    }

    /// Eval utterance coverage: every canonical cross-agent request shape
    /// from the task brief must have its operative verb in the contract,
    /// and the native-subagent control must stay explicitly allowed.
    #[test]
    fn contract_covers_eval_utterances() {
        let lower = FORGE_RUNTIME_CONTRACT.to_lowercase();
        for verb in [
            "message", "ask", "tell", "send something to", "notify", "delegate",
        ] {
            assert!(lower.contains(verb), "contract must cover {verb:?}");
        }
        for example in [
            "ask codex to review this",
            "send this to claude",
            "have gemini",
            "delegate the review to codex",
            "message the other agent",
        ] {
            assert!(
                lower.contains(example),
                "contract must exemplify {example:?}"
            );
        }
        assert!(
            lower.contains("native subagents remain available"),
            "control: private internal subagents must stay allowed"
        );
    }

    #[test]
    fn mechanisms_match_verified_transports() {
        assert_eq!(
            adapter_for("claude").runtime_mechanism(),
            RuntimeMechanism::SystemPromptFile
        );
        assert_eq!(
            adapter_for("codex").runtime_mechanism(),
            RuntimeMechanism::DeveloperInstructions
        );
        assert_eq!(
            adapter_for("muse").runtime_mechanism(),
            RuntimeMechanism::StartupPromptPositional
        );
        assert_eq!(
            adapter_for("agy").runtime_mechanism(),
            RuntimeMechanism::StartupPromptPositional
        );
        assert_eq!(
            adapter_for("something-custom").runtime_mechanism(),
            RuntimeMechanism::McpInstructionsOnly
        );
    }

    #[test]
    fn claude_extras_point_at_forge_owned_file() {
        let file = Path::new("/home/tester/.forge/runtime.md");
        assert_eq!(
            adapter_for("claude").runtime_injection(file),
            vec![
                "--append-system-prompt-file".to_string(),
                "/home/tester/.forge/runtime.md".to_string(),
            ]
        );
    }

    #[test]
    fn codex_extras_carry_toml_developer_instructions() {
        let file = Path::new("/home/tester/.forge/runtime.md");
        let extras = adapter_for("codex").runtime_injection(file);
        assert_eq!(extras.len(), 2);
        assert_eq!(extras[0], "-c");
        assert!(
            extras[1].starts_with("developer_instructions=\""),
            "value must be a TOML string: {}",
            extras[1]
        );
        assert!(extras[1].ends_with('"'));
        assert!(
            !extras[1].contains('\n'),
            "raw newlines would break TOML parsing"
        );
        assert!(
            extras[1].contains("Forge takes precedence"),
            "contract must ride inside the value"
        );
    }

    #[test]
    fn toml_basic_string_round_trips_through_parser() {
        let encoded = toml_basic_string("a\"b\\c\nd\te\x01f");
        assert_eq!(encoded, "\"a\\\"b\\\\c\\nd\\te\\u0001f\"");
    }

    #[test]
    fn codex_c_payload_parses_as_toml() {
        // Independent oracle: the repo's own TOML parser must read the
        // `-c developer_instructions=<value>` payload back as the exact
        // contract text. This is what `codex -c key=value` parses.
        let file = Path::new("/home/tester/.forge/runtime.md");
        let extras = adapter_for("codex").runtime_injection(file);
        let doc: toml::Table = toml::from_str(&extras[1]).expect("valid TOML doc");
        assert_eq!(
            doc.get("developer_instructions").and_then(|v| v.as_str()),
            Some(FORGE_RUNTIME_CONTRACT)
        );
    }

    #[test]
    fn startup_fallbacks_carry_contract_as_first_prompt() {
        let file = Path::new("/home/tester/.forge/runtime.md");
        assert_eq!(
            adapter_for("muse").runtime_injection(file),
            vec![FORGE_RUNTIME_CONTRACT.to_string()]
        );
        assert_eq!(
            adapter_for("agy").runtime_injection(file),
            vec![FORGE_RUNTIME_CONTRACT.to_string()]
        );
    }

    #[test]
    fn unknown_agents_get_no_argv_extras() {
        let file = Path::new("/home/tester/.forge/runtime.md");
        assert!(adapter_for("something-custom")
            .runtime_injection(file)
            .is_empty());
    }

    #[test]
    fn runtime_file_lives_under_forge_config_dir() {
        assert_eq!(
            runtime_file(Path::new("/home/tester")),
            PathBuf::from("/home/tester/.forge/runtime.md")
        );
    }

    #[test]
    fn materialization_writes_and_refreshes_contract() {
        let home = scratch_home();
        let path = ensure_materialized(&home).unwrap();
        assert_eq!(path, runtime_file(&home));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FORGE_RUNTIME_CONTRACT);
        // Forge-owned: an existing file is refreshed, never preserved.
        std::fs::write(&path, "stale user edit").unwrap();
        ensure_materialized(&home).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), FORGE_RUNTIME_CONTRACT);
        // No staging files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }
}
