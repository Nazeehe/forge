//! Per-harness agent skill install and uninstall.

use super::*;

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
- `visual_answer(answer)` answers the operator's question about a
  diagram shape (`<visual-question>` markup in this session's pane).

Cross-session communication MUST go through these Forge tools; NEVER use
your CLI's own messaging — only Forge tools resolve Forge session names
and enforce shared-group routing.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::install::test_support::*;
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
    fn forge_skill_mandates_forge_comms() {
        // Skill-following harnesses weight SKILL.md over tool text: it
        // must carry the same MUST/NEVER override as the MCP instructions.
        let home = scratch_home();
        install_one_skills(&home, "claude");
        let text =
            std::fs::read_to_string(home.join(".claude/skills/forge/SKILL.md")).unwrap();
        assert!(text.contains("MUST go through these Forge tools"), "skill: {text:?}");
        assert!(text.contains("NEVER use"), "skill: {text:?}");
        assert!(text.contains("your CLI's own messaging"), "skill: {text:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
