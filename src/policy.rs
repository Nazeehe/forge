//! Deterministic permission policy (Phase 3c).
//!
//! Modes come from `[permission]` config: Off lets the harness ask, YOLO
//! allows everything, AI-Assisted currently asks (classifier deferred to
//! Phase 8), and Safe-Only applies block patterns first, then allow
//! patterns and safe reads, asking otherwise. Block always wins, including
//! when an allow pattern matches the same request. Allow/Deny outcomes are
//! cached; Ask is never cached. Shell-execution keys stay byte-exact while
//! all other keys normalize case and whitespace.

use crate::config::PermissionMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Ask,
}

/// Read-only harness tools that Safe-Only mode allows without asking.
pub const SAFE_READ_TOOLS: &[&str] = &["Read", "Glob", "Grep", "LS"];

/// Tools whose cache key must stay byte-exact: normalizing a shell command
/// could merge distinct executions into one decision.
pub const SHELL_TOOLS: &[&str] = &["Bash", "shell"];

struct CacheEntry {
    decision: Decision,
}

/// Deterministic permission policy: compiled block/allow patterns plus a
/// normalized decision cache. Construction fails closed on invalid
/// patterns so a half-built policy can never decide.
pub struct Policy {
    mode: PermissionMode,
    allow: Vec<regex::Regex>,
    block: Vec<regex::Regex>,
    cache: std::collections::HashMap<String, CacheEntry>,
}

impl Policy {
    /// Current mode, for chrome display.
    pub fn mode(&self) -> PermissionMode {
        self.mode.clone()
    }

    pub fn new(
        mode: PermissionMode,
        allow: &[String],
        block: &[String],
    ) -> Result<Self, regex::Error> {
        let compile = |patterns: &[String]| {
            patterns
                .iter()
                .map(|p| regex::Regex::new(p))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Policy {
            mode,
            allow: compile(allow)?,
            block: compile(block)?,
            cache: std::collections::HashMap::new(),
        })
    }

    /// Decide one hook event, returning the decision and a short reason.
    /// Allow/Deny outcomes populate the cache; Ask never does.
    pub fn decide(&mut self, _hook: &str, body: &str) -> (Decision, &'static str) {
        let tool = tool_name(body);
        let command =
            json_string_field(body.as_bytes(), &["command", "cmd"]).unwrap_or_default();
        let key = cache_key(&tool, &command);
        if let Some(hit) = self.cache.get(&key) {
            return (hit.decision, "cached");
        }
        let (decision, reason) = match self.mode {
            PermissionMode::Yolo => (Decision::Allow, "yolo mode"),
            PermissionMode::Off => (Decision::Ask, "off: harness asks"),
            PermissionMode::AiAssisted => (Decision::Ask, "ai-assisted deferred"),
            PermissionMode::SafeOnly => self.safe_only(&tool, &command),
        };
        if !matches!(decision, Decision::Ask) {
            self.cache.insert(key, CacheEntry { decision });
        }
        (decision, reason)
    }

    fn safe_only(&self, tool: &str, command: &str) -> (Decision, &'static str) {
        let target = format!("{tool}\n{command}");
        if self.block.iter().any(|re| re.is_match(&target)) {
            return (Decision::Deny, "block pattern");
        }
        if self.allow.iter().any(|re| re.is_match(&target)) {
            return (Decision::Allow, "allow pattern");
        }
        if SAFE_READ_TOOLS
            .iter()
            .any(|safe| safe.eq_ignore_ascii_case(tool))
        {
            return (Decision::Allow, "safe read");
        }
        (Decision::Ask, "needs approval")
    }

    #[cfg(test)]
    fn cache_len(&self) -> usize {
        self.cache.len()
    }
}

/// Cache key for one request. Shell executions stay byte-exact; everything
/// else lowercases and collapses whitespace runs.
fn cache_key(tool: &str, command: &str) -> String {
    if SHELL_TOOLS.iter().any(|s| s.eq_ignore_ascii_case(tool)) {
        return format!("{tool}\0{command}");
    }
    let lowered = format!("{tool}\0{command}").to_lowercase();
    let mut collapsed = String::with_capacity(lowered.len());
    let mut gap = false;
    for c in lowered.chars() {
        if c.is_whitespace() {
            gap = true;
        } else {
            if gap && !collapsed.is_empty() {
                collapsed.push(' ');
            }
            gap = false;
            collapsed.push(c);
        }
    }
    collapsed
}

/// Tool name for one hook body, for audit display.
pub fn tool_name(body: &str) -> String {
    json_string_field(body.as_bytes(), &["tool_name", "tool"]).unwrap_or_default()
}

/// Display command for one hook body: the executed command or file target.
pub fn command_of(body: &str) -> String {
    json_string_field(body.as_bytes(), &["command", "cmd", "file_path", "path", "pattern"])
        .unwrap_or_default()
}

/// Harness-aware decision rendering. Codex accepts a bare
/// `permissionDecision:"allow"` only with an `updatedInput` rewrite (which
/// forge never emits) and rejects `ask` on PreToolUse outright ("unsupported
/// permissionDecision:allow"); both must be silent (empty stdout, exit 0) so
/// Codex's own approval flow decides. Only an attributed codex sender goes
/// silent; unattributed senders keep the long-standing explicit shape so
/// existing behavior (and its tests) is unchanged. Deny keeps the explicit
/// deny shape all harnesses accept.
///
/// Codex approvals gate on a second event, `PermissionRequest`: allow there
/// skips the approval prompt, deny blocks, and silence declines to decide
/// (the normal approval flow continues). That envelope is the only verdict
/// that can auto-approve a Codex call, so forge YOLO rides on it; other
/// harnesses keep the legacy shape they already accept there.
pub fn decision_line_for(cli_tool: &str, hook: &str, decision: Decision, reason: &str) -> String {
    if cli_tool.eq_ignore_ascii_case("codex") {
        if hook == "PreToolUse" && matches!(decision, Decision::Allow | Decision::Ask) {
            return String::new();
        }
        if hook == "PermissionRequest" {
            return match decision {
                Decision::Allow => "{\"hookSpecificOutput\":{\"hookEventName\":\"PermissionRequest\",\"decision\":{\"behavior\":\"allow\"}}}\n"
                    .to_string(),
                Decision::Deny => {
                    let message = escape_reason(reason);
                    let message = if message.is_empty() { "denied".to_string() } else { message };
                    format!(
                        "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PermissionRequest\",\"decision\":{{\"behavior\":\"deny\",\"message\":\"{message}\"}}}}}}\n"
                    )
                }
                Decision::Ask => String::new(),
            };
        }
    }
    decision_line(hook, decision, reason)
}

/// Escape one reason string for embedding in a JSON double-quoted value:
/// quotes/backslashes gain a backslash, controls become spaces.
fn escape_reason(reason: &str) -> String {
    let mut safe = String::with_capacity(reason.len());
    for c in reason.chars() {
        match c {
            '"' | '\\' => {
                safe.push('\\');
                safe.push(c);
            }
            c if c.is_control() => safe.push(' '),
            c => safe.push(c),
        }
    }
    safe
}

/// Canonical one-line decision for the relay to print to the harness.
/// PreToolUse answers use the `hookSpecificOutput` shape: newer Claude
/// and muse both reject the legacy top-level `decision` field there
/// ("unsupported legacy PreToolUse output"). Deny always carries a
/// non-empty reason because the validators require one. Every other hook
/// keeps the legacy shape its harness already accepts.
pub fn decision_line(hook: &str, decision: Decision, reason: &str) -> String {
    let name = match decision {
        Decision::Allow => "allow",
        Decision::Deny => "deny",
        Decision::Ask => "ask",
    };
    let mut safe = String::with_capacity(reason.len());
    for c in reason.chars() {
        match c {
            '"' | '\\' => {
                safe.push('\\');
                safe.push(c);
            }
            c if c.is_control() => safe.push(' '),
            c => safe.push(c),
        }
    }
    // A deny with an empty reason is rejected by the validators, which
    // would fail the gate open. Reasons are static non-empty strings
    // today; the fallback keeps that invariant structural.
    let reason_out = if safe.is_empty() && matches!(decision, Decision::Deny) {
        "denied".to_string()
    } else {
        safe
    };
    if hook == "PreToolUse" {
        return format!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"{name}\",\"permissionDecisionReason\":\"{reason_out}\"}}}}\n"
        );
    }
    format!("{{\"decision\":\"{name}\",\"reason\":\"{reason_out}\"}}\n")
}

/// First string value for any of `fields` in a JSON document, reusing the
/// field-scan technique: valid JSON holds no literal controls in strings.
pub(crate) fn json_string_field(haystack: &[u8], fields: &[&str]) -> Option<String> {
    for field in fields {
        let needle = format!("\"{field}\"");
        let mut search = haystack;
        while let Some(pos) = search
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
        {
            let mut rest = &search[pos + needle.len()..];
            rest = skip_gap(rest);
            if rest.first() != Some(&b':') {
                search = &search[pos + 1..];
                continue;
            }
            rest = skip_gap(&rest[1..]);
            if rest.first() != Some(&b'"') {
                search = &search[pos + 1..];
                continue;
            }
            let mut end = 1;
            while end < rest.len() && rest[end] != b'"' {
                if rest[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            if end < rest.len() {
                if let Ok(value) = std::str::from_utf8(&rest[1..end]) {
                    return Some(value.to_string());
                }
            }
            search = &search[pos + 1..];
        }
    }
    None
}

fn skip_gap(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        bytes = &bytes[1..];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_only(allow: &[&str], block: &[&str]) -> Policy {
        Policy::new(
            PermissionMode::SafeOnly,
            &allow.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &block.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap()
    }

    fn pre_tool(tool: &str, input: &str) -> (String, String) {
        (
            "PreToolUse".to_string(),
            format!(r#"{{"tool_name":"{tool}","tool_input":{{"command":"{input}"}}}}"#),
        )
    }

    #[test]
    fn block_wins_over_allow_on_same_request() {
        let mut p = safe_only(&["rm -rf"], &["rm -rf"]);
        let (hook, body) = pre_tool("Bash", "rm -rf /tmp/x");
        let (decision, _) = p.decide(&hook, &body);
        assert_eq!(decision, Decision::Deny);
    }

    #[test]
    fn allow_pattern_permits_without_asking() {
        let mut p = safe_only(&["git status"], &["rm -rf"]);
        let (hook, body) = pre_tool("Bash", "git status");
        let (decision, _) = p.decide(&hook, &body);
        assert_eq!(decision, Decision::Allow);
    }

    #[test]
    fn safe_reads_allowed_everything_else_asks() {
        let mut p = safe_only(&[], &[]);
        let (hook, body) = pre_tool("Read", "/etc/hosts");
        assert_eq!(p.decide(&hook, &body).0, Decision::Allow);
        let (hook, body) = pre_tool("Bash", "ls");
        assert_eq!(p.decide(&hook, &body).0, Decision::Ask);
    }

    #[test]
    fn yolo_allows_and_off_asks() {
        let mut yolo = Policy::new(PermissionMode::Yolo, &[], &[]).unwrap();
        let (hook, body) = pre_tool("Bash", "rm -rf /");
        assert_eq!(yolo.decide(&hook, &body).0, Decision::Allow);
        let mut off = Policy::new(PermissionMode::Off, &[], &[]).unwrap();
        assert_eq!(off.decide(&hook, &body).0, Decision::Ask);
    }

    #[test]
    fn allow_deny_cached_ask_never_cached() {
        let mut p = safe_only(&["git status"], &[]);
        let (hook, body) = pre_tool("Bash", "git status");
        assert_eq!(p.decide(&hook, &body).0, Decision::Allow);
        assert_eq!(p.cache_len(), 1);
        let (hook, body) = pre_tool("Bash", "something-new");
        assert_eq!(p.decide(&hook, &body).0, Decision::Ask);
        assert_eq!(p.cache_len(), 1, "ask leaves no entry");
    }

    #[test]
    fn shell_keys_stay_exact_others_normalize() {
        let mut p = safe_only(&["echo"], &[]);
        let (hook, body) = pre_tool("Bash", "echo A  B");
        let _ = p.decide(&hook, &body);
        assert_eq!(p.cache_len(), 1);
        let (hook, other_case) = pre_tool("Bash", "echo a b");
        let _ = p.decide(&hook, &other_case);
        assert_eq!(p.cache_len(), 2, "shell keys must not merge");
        let (hook, read) = pre_tool("Read", "/TMP/X");
        let _ = p.decide(&hook, &read);
        let (hook, read_lower) = pre_tool("read", "/tmp/x");
        let _ = p.decide(&hook, &read_lower);
        assert_eq!(p.cache_len(), 3, "read keys normalize together");
    }

    #[test]
    fn invalid_pattern_fails_closed_at_construction() {
        assert!(Policy::new(PermissionMode::SafeOnly, &["([".to_string()], &[]).is_err());
    }

    #[test]
    fn command_of_extracts_tool_command() {
        let (_, body) = pre_tool("Bash", "doom");
        assert_eq!(command_of(&body), "doom");
        assert_eq!(tool_name(&body), "Bash");
    }

    #[test]
    fn decision_line_is_canonical_json() {
        // Non-PreToolUse hooks keep the legacy shape byte for byte.
        let line = decision_line("Stop", Decision::Deny, "block pattern");
        assert!(line.ends_with('\n'));
        assert!(line.contains(r#""decision":"deny""#), "line: {line:?}");
        assert!(line.contains("block pattern"));
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["decision"], serde_json::Value::String("deny".to_string()));
    }

    #[test]
    fn decision_line_pre_tool_use_uses_hook_specific_output() {
        for (decision, name) in [
            (Decision::Allow, "allow"),
            (Decision::Deny, "deny"),
            (Decision::Ask, "ask"),
        ] {
            let line = decision_line("PreToolUse", decision, "yolo mode");
            assert!(line.ends_with('\n'), "newline terminated");
            let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert!(v.get("decision").is_none(), "no legacy field: {line:?}");
            let out = &v["hookSpecificOutput"];
            assert_eq!(out["hookEventName"], serde_json::Value::String("PreToolUse".to_string()));
            assert_eq!(
                out["permissionDecision"],
                serde_json::Value::String(name.to_string()),
                "line: {line:?}"
            );
            assert!(
                out["permissionDecisionReason"].as_str().is_some_and(|r| !r.is_empty()),
                "validators reject reason-less denies: {line:?}"
            );
        }
    }

    #[test]
    fn codex_pre_tool_use_allow_and_ask_are_silent() {
        // Grounded in codex-cli hooks: a bare `permissionDecision:"allow"`
        // (no `updatedInput` rewrite) is rejected as "unsupported
        // permissionDecision:allow", and `ask` is likewise unsupported.
        // Forge never rewrites input, so allow/ask must be empty stdout
        // (exit 0, no output) and let Codex's own approval flow decide.
        // Deny keeps the explicit deny shape both harnesses accept.
        let allow = super::decision_line_for("codex", "PreToolUse", Decision::Allow, "yolo mode");
        assert!(allow.is_empty(), "codex allow must be silent: {allow:?}");
        let ask = super::decision_line_for("codex", "PreToolUse", Decision::Ask, "off: harness asks");
        assert!(ask.is_empty(), "codex ask must defer silently: {ask:?}");
        let deny = super::decision_line_for("codex", "PreToolUse", Decision::Deny, "block pattern");
        assert!(deny.contains(r#""permissionDecision":"deny""#), "deny stays explicit: {deny:?}");
        // Claude-style harnesses keep the explicit allow/ask shapes.
        let claude_allow = super::decision_line_for("claude", "PreToolUse", Decision::Allow, "yolo mode");
        assert!(claude_allow.contains(r#""permissionDecision":"allow""#), "claude allow: {claude_allow:?}");
        let claude_ask = super::decision_line_for("claude", "PreToolUse", Decision::Ask, "off: harness asks");
        assert!(claude_ask.contains(r#""permissionDecision":"ask""#), "claude ask: {claude_ask:?}");
    }

    #[test]
    fn codex_permission_request_uses_behavior_envelope() {
        // Official Codex hooks docs: PermissionRequest fires when Codex is
        // about to ask approval. allow skips the prompt, deny blocks, and
        // no decision defers to the normal approval flow. This is the only
        // hook verdict that can approve a Codex call (bare PreToolUse allow
        // is rejected), so forge YOLO rides on it.
        let allow = super::decision_line_for("codex", "PermissionRequest", Decision::Allow, "yolo mode");
        let v: serde_json::Value = serde_json::from_str(allow.trim()).expect("allow is JSON");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert!(v.get("decision").is_none(), "no legacy field: {allow:?}");
        let deny = super::decision_line_for("codex", "PermissionRequest", Decision::Deny, "block pattern");
        let v: serde_json::Value = serde_json::from_str(deny.trim()).expect("deny is JSON");
        assert_eq!(v["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(
            v["hookSpecificOutput"]["decision"]["message"].as_str().is_some_and(|m| !m.is_empty()),
            "deny carries a message: {deny:?}"
        );
        let ask = super::decision_line_for("codex", "PermissionRequest", Decision::Ask, "off: harness asks");
        assert!(ask.is_empty(), "ask declines to decide: {ask:?}");
        // Other harnesses keep the legacy shape they already accept.
        let claude = super::decision_line_for("claude", "PermissionRequest", Decision::Allow, "yolo mode");
        assert!(claude.contains(r#""decision":"allow""#), "claude unchanged: {claude:?}");
    }

    #[test]
    fn decision_line_never_emits_a_reasonless_deny() {
        let line = decision_line("PreToolUse", Decision::Deny, "");
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert!(
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .is_some_and(|r| !r.is_empty()),
            "fallback reason: {line:?}"
        );
    }
}
