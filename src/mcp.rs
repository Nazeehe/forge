//! `forge mcp-serve`: newline-delimited JSON-RPC 2.0 over stdio,
//! advertising MCP `2025-03-26`. Each harness child runs one; `tools/call`
//! bodies travel to the TUI broker over the IPC socket and the one-line
//! verdict comes back the same way. Tool failures surface as MCP `isError`
//! results; only malformed protocol becomes a JSON-RPC error.

pub const PROTOCOL_VERSION: &str = "2025-03-26";
pub const SERVER_NAME: &str = "forge";
pub const SERVER_VERSION: &str = "1.0.0";

/// Tool failures travel as MCP `isError` results so the harness sees them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub content_json: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(content_json: &str) -> Self {
        ToolResult {
            content_json: content_json.to_string(),
            is_error: false,
        }
    }

    pub fn fail(message: &str) -> Self {
        ToolResult {
            content_json: format!(r#"{{"error":{}}}"#, escape_json(message)),
            is_error: true,
        }
    }
}

/// Static operating instructions plus dynamic project/memory/group context
/// assembled by the caller (4c); the broker owns the live values.
pub struct ServerCtx {
    pub instructions_extra: String,
}

fn ctx_with(extra: &str) -> ServerCtx {
    ServerCtx {
        instructions_extra: extra.to_string(),
    }
}

/// How `tools/call` reaches the TUI broker.
pub struct CallCtx {
    pub endpoint: Option<String>,
    pub run_id: String,
    pub timeout: std::time::Duration,
}

/// Decode a JSON string interior (surrounding quotes already off):
/// the standard escapes plus `\uXXXX`. Lone surrogates and malformed
/// escapes stay literal so tool args never fail on syntax trivia.
pub(crate) fn decode_json_string(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('b') => out.push('\u{08}'),
            Some('f') => out.push('\u{0C}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(decoded) => out.push(decoded),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Escape a string as a JSON string literal, quotes included.
pub fn escape_json(s: &str) -> String {
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
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

struct Span {
    start: usize,
    end: usize,
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Scanner<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'u' => {
                            if self.i + 4 > self.b.len() {
                                return None;
                            }
                            let hex = std::str::from_utf8(&self.b[self.i..self.i + 4]).ok()?;
                            let cp = u32::from_str_radix(hex, 16).ok()?;
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            self.i += 4;
                        }
                        _ => return None,
                    }
                }
                _ => {
                    // Raw UTF-8 bytes pass through; invalid sequences fail.
                    let rest = std::str::from_utf8(&self.b[self.i - 1..]).ok()?;
                    let ch = rest.chars().next()?;
                    out.push(ch);
                    self.i += ch.len_utf8() - 1;
                }
            }
        }
    }

    /// Skip one JSON value; returns its byte span.
    fn value(&mut self) -> Option<Span> {
        self.ws();
        let start = self.i;
        let c = *self.b.get(self.i)?;
        match c {
            b'"' => {
                self.string()?;
            }
            b'{' => {
                self.i += 1;
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                } else {
                    loop {
                        self.string()?;
                        self.ws();
                        if self.b.get(self.i) != Some(&b':') {
                            return None;
                        }
                        self.i += 1;
                        self.value()?;
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => {
                                self.i += 1;
                                self.ws();
                            }
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return None,
                        }
                    }
                }
            }
            b'[' => {
                self.i += 1;
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                } else {
                    loop {
                        self.value()?;
                        self.ws();
                        match self.b.get(self.i) {
                            Some(b',') => {
                                self.i += 1;
                                self.ws();
                            }
                            Some(b']') => {
                                self.i += 1;
                                break;
                            }
                            _ => return None,
                        }
                    }
                }
            }
            b't' => {
                if self.b.get(self.i..self.i + 4) != Some(b"true".as_slice()) {
                    return None;
                }
                self.i += 4;
            }
            b'f' => {
                if self.b.get(self.i..self.i + 5) != Some(b"false".as_slice()) {
                    return None;
                }
                self.i += 5;
            }
            b'n' => {
                if self.b.get(self.i..self.i + 4) != Some(b"null".as_slice()) {
                    return None;
                }
                self.i += 4;
            }
            b'-' | b'0'..=b'9' => {
                while self.i < self.b.len()
                    && matches!(self.b[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    self.i += 1;
                }
            }
            _ => return None,
        }
        Some(Span { start, end: self.i })
    }
}

/// Parse a top-level JSON object into (key, raw-value-span) pairs.
fn object_fields(line: &str) -> Option<Vec<(String, Span)>> {
    let mut s = Scanner {
        b: line.as_bytes(),
        i: 0,
    };
    s.ws();
    if s.b.get(s.i) != Some(&b'{') {
        return None;
    }
    s.i += 1;
    let mut fields = Vec::new();
    s.ws();
    if s.b.get(s.i) == Some(&b'}') {
        return Some(fields);
    }
    loop {
        s.ws();
        let key = s.string()?;
        s.ws();
        if s.b.get(s.i) != Some(&b':') {
            return None;
        }
        s.i += 1;
        let span = s.value()?;
        fields.push((key, span));
        s.ws();
        match s.b.get(s.i) {
            Some(b',') => {
                s.i += 1;
            }
            Some(b'}') => {
                s.i += 1;
                s.ws();
                if s.i != s.b.len() {
                    return None;
                }
                return Some(fields);
            }
            _ => return None,
        }
    }
}

/// Raw JSON span of a top-level object field, for pass-through values.
pub(crate) fn top_raw<'t>(line: &'t str, name: &str) -> Option<&'t str> {
    let trimmed = line.trim();
    let fields = object_fields(trimmed)?;
    let (_, span) = fields.iter().find(|(k, _)| k == name)?;
    Some(&trimmed[span.start..span.end])
}

/// Decoded string value of a top-level object field.
pub(crate) fn top_str(line: &str, name: &str) -> Option<String> {
    let raw = top_raw(line, name)?;
    let mut s = Scanner {
        b: raw.as_bytes(),
        i: 0,
    };
    s.string()
}

fn field<'t>(fields: &'t [(String, Span)], line: &'t str, name: &str) -> Option<&'t str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, span)| &line[span.start..span.end])
}

fn field_str(fields: &[(String, Span)], line: &str, name: &str) -> Option<String> {
    let raw = field(fields, line, name)?;
    let mut s = Scanner {
        b: raw.as_bytes(),
        i: 0,
    };
    s.string()
}

fn proto_error(id_raw: Option<&str>, code: i32, message: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","id":{},"error":{{"code":{code},"message":{}}}}}"#,
        id_raw.unwrap_or("null"),
        escape_json(message),
    )
}

fn result_envelope(id_raw: &str, result_json: &str) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":{id_raw},"result":{result_json}}}"#)
}

fn tool_envelope(id_raw: &str, tool: &ToolResult) -> String {
    let content = escape_json(&tool.content_json);
    let result = format!(
        r#"{{"content":[{{"type":"text","text":{content}}}],"isError":{}}}"#,
        if tool.is_error { "true" } else { "false" },
    );
    result_envelope(id_raw, &result)
}

fn instructions(srv: &ServerCtx) -> String {
    let base = "You run inside forge, a terminal control plane for AI coding agents, as \
        a named member session. Pane lines shaped like `[forge ask_session from X]` \
        or `[forge tell_session from X]` are live peer requests, not logs: answer \
        asks at once with send_response, tells with a tell_session follow-up or \
        ack_message, copying the enclosed conversation_id verbatim — never in chat \
        alone. Use ask to question a peer session, send_response to answer, tell to \
        inform, ack to confirm, list_sessions to discover peers. Sessions only \
        communicate when they share a group; address peers by name, authority comes \
        from run IDs. Cross-session communication MUST go through these Forge tools; \
        NEVER use your CLI's own messaging (session-message commands, subagents, \
        shell signals, or files as a message bus) — only Forge tools resolve Forge \
        session names, enforce shared-group routing, and carry run-ID authority. \
        A `[forge visual ...]` question with `<visual-question>` markup \
        is the operator asking about a diagram shape: answer it at once with \
        visual_answer. Use screenshot to capture a named desktop app window \
        (Hyprland; hidden windows need focus:true). Use walkthrough_start to tour the operator through a file, \
        walkthrough_answer for their waiting tour questions, walkthrough_end to \
        close the tour. Use compact_session to compact context, schedule_prompt \
        for delayed self-injection, start_session to spawn local agent sessions, \
        and set/clear_session_status for your sticky sidebar status.";
    if srv.instructions_extra.is_empty() {
        base.to_string()
    } else {
        format!("{base} {}", srv.instructions_extra)
    }
}

struct ToolDef {
    name: &'static str,
    description: &'static str,
    schema: &'static str,
}

/// Phase 4 serves the five comms tools on every platform, plus the
/// walkthrough trio for agent-led file tours. Unix terminal tools join
/// this list in Phase 7.
fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "ask_session",
            description: "Use this, not your CLI's own messaging, to ask a Forge peer session a question; first call list_sessions to resolve the live peer name. Returns a conversation ID immediately and injects the question when the target is idle. Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"target":{"type":"string"},"message":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["target","message"]}"#,
        },
        ToolDef {
            name: "send_response",
            description: "Use this, not your CLI's own messaging, to answer a conversation addressed to this session: use it the moment your pane shows [forge ask_session from ...], copying its conversation_id verbatim. Only an ask_session takes a response. Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"conversation_id":{"type":"string"},"message":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["conversation_id","message"]}"#,
        },
        ToolDef {
            name: "tell_session",
            description: "Use this, not your CLI's own messaging, to tell a Forge peer session something; the peer acknowledges asynchronously. To follow up on a received tell, pass its conversation_id plus its sender as target (a follow-up on the same conversation). Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"target":{"type":"string"},"message":{"type":"string"},"conversation_id":{"type":"string"},"idempotency_key":{"type":"string"}}}"#,
        },
        ToolDef {
            name: "ack_message",
            description: "Use this, not your CLI's own messaging, to acknowledge a tell addressed to this session: confirms receipt of a tell that needs no reply content; it is not an answer. Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"conversation_id":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["conversation_id"]}"#,
        },
        ToolDef {
            name: "list_sessions",
            description: "Use this, not your CLI's own session listing, to discover Forge peers. List live peer sessions visible to this session: call it when asked what sessions are alive, and before contacting a peer, to resolve live names and groups. The `you` field names the calling session.",
            schema: r#"{"type":"object","properties":{}}"#,
        },
        ToolDef {
            name: "walkthrough_start",
            description: "Open a file tour for the operator: steps is one start:end:explanation header per step, file resolves against the session cwd. The overlay opens on the tour at once. Explanations render as Markdown and may span lines: indent continuation lines with one space (it is stripped, so Markdown nesting survives) and use blank lines for paragraph breaks. Write each explanation like a coworker walking through their code out loud: context, what these lines do, and why it matters — never a bare label.",
            schema: r#"{"type":"object","properties":{"file":{"type":"string"},"steps":{"type":"string"},"title":{"type":"string"}},"required":["file","steps"]}"#,
        },
        ToolDef {
            name: "walkthrough_answer",
            description: "Answer the operator's latest waiting walkthrough question; it renders as Markdown under the question. Errors when nothing is waiting.",
            schema: r#"{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}"#,
        },
        ToolDef {
            name: "walkthrough_end",
            description: "Close the tour with an optional summary line.",
            schema: r#"{"type":"object","properties":{"summary":{"type":"string"}}}"#,
        },
        ToolDef {
            name: "visual_answer",
            description: "Answer the operator's latest waiting visual question about a diagram shape: use it the moment a [forge visual ...] question with <visual-question> markup lands in your pane. It renders as plain text under the diagram. Errors when nothing is waiting.",
            schema: r#"{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}"#,
        },
        #[cfg(all(target_os = "linux", feature = "visual"))]
        ToolDef {
            name: "screenshot",
            description: "Capture a named desktop app window on this Linux desktop (Hyprland): matches the app against window class then title, captures its region, and returns the PNG path plus dimensions. Hidden windows need focus:true to bring them forward first; without it they are an error, never a surprise focus steal. Errors when nothing matches, several match, or no compositor answers.",
            schema: r#"{"type":"object","properties":{"app":{"type":"string"},"focus":{"type":"boolean"}},"required":["app"]}"#,
        },
        ToolDef {
            name: "compact_session",
            description: "Queue /compact for a session; omit target to compact yourself, name a peer in a shared group otherwise. Delivers when the target is idle. Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"target":{"type":"string"},"idempotency_key":{"type":"string"}}}"#,
        },
        ToolDef {
            name: "schedule_prompt",
            description: "Arm a self-injection timer: the prompt lands in your own queue after delay_seconds (0 to 86400) and delivers when idle. clear_context starts it with /clear. Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"prompt":{"type":"string"},"delay_seconds":{"type":"number"},"clear_context":{"type":"boolean"},"idempotency_key":{"type":"string"}},"required":["prompt"]}"#,
        },
        ToolDef {
            name: "cancel_scheduled_prompt",
            description: "Cancel an armed self-injection timer by timer_id. Optional idempotency_key (generate once per send, reuse on retry) makes timeout retries replay instead of duplicating.",
            schema: r#"{"type":"object","properties":{"timer_id":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["timer_id"]}"#,
        },
        ToolDef {
            name: "start_session",
            description: "Create a local agent session: harness is claude, codex, or muse; path defaults to your cwd; an opening prompt queues as the first idle injection. Remote flavors are unsupported.",
            schema: r#"{"type":"object","properties":{"path":{"type":"string"},"name":{"type":"string"},"harness":{"type":"string"},"prompt":{"type":"string"},"communication_group":{"type":"string"}}}"#,
        },
        ToolDef {
            name: "set_session_status",
            description: "Replace your sticky sidebar status; kind is info, progress, success, warning, blocked, or question, message at most 80 characters with no newlines.",
            schema: r#"{"type":"object","properties":{"kind":{"type":"string"},"message":{"type":"string"}},"required":["kind","message"]}"#,
        },
        ToolDef {
            name: "clear_session_status",
            description: "Clear your sticky sidebar status.",
            schema: r#"{"type":"object","properties":{}}"#,
        },
        ToolDef {
            name: "walkthrough_add_step",
            description: "Insert one step into the open tour: start_line, end_line, and explanation (Markdown, may span lines); position is 1-based like the displayed counter and appends when absent.",
            schema: r#"{"type":"object","properties":{"start_line":{"type":"integer"},"end_line":{"type":"integer"},"explanation":{"type":"string"},"file_path":{"type":"string"},"position":{"type":"integer"}},"required":["start_line","end_line","explanation"]}"#,
        },
        ToolDef {
            name: "walkthrough_update",
            description: "Update one step of the open tour: step_index is 1-based like the displayed counter; absent fields keep their values.",
            schema: r#"{"type":"object","properties":{"step_index":{"type":"integer"},"start_line":{"type":"integer"},"end_line":{"type":"integer"},"explanation":{"type":"string"}},"required":["step_index"]}"#,
        },
        #[cfg(feature = "visual")]
        ToolDef {
            name: "visual_show",
            description: "Render a diagram into your Visual tab: content is the diagram source, format is mermaid (only), title and alt label it for the operator.",
            schema: r#"{"type":"object","properties":{"content":{"type":"string"},"format":{"type":"string"},"title":{"type":"string"},"alt":{"type":"string"}},"required":["content"]}"#,
        },
    ]
}

fn tools_list_json() -> String {
    let defs = tool_defs();
    let mut out = String::from(r#"{"tools":["#);
    for (i, d) in defs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"name":{},"description":{},"inputSchema":{}}}"#,
            escape_json(d.name),
            escape_json(d.description),
            d.schema,
        ));
    }
    out.push_str("]}");
    out
}

/// Handle one newline-delimited JSON-RPC value. Returns the response line,
/// or `None` for notifications. `dispatch` executes `tools/call` bodies.
pub fn handle_line(
    line: &str,
    srv: &ServerCtx,
    dispatch: &dyn Fn(&str, &str, &CallCtx) -> ToolResult,
    cc: &CallCtx,
) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let fields = match object_fields(trimmed) {
        Some(f) => f,
        None => {
            if trimmed.starts_with('[') {
                return Some(proto_error(None, -32600, "Invalid Request: batches unsupported"));
            }
            return Some(proto_error(None, -32700, "Parse error"));
        }
    };
    if let Some(v) = field(&fields, trimmed, "jsonrpc") {
        if v != r#""2.0""# {
            let id = field(&fields, trimmed, "id");
            return Some(proto_error(id, -32600, "Invalid Request: jsonrpc must be 2.0"));
        }
    }
    let id_raw = field(&fields, trimmed, "id");
    let Some(method) = field_str(&fields, trimmed, "method") else {
        // No method: a bare notification when id is also absent, else invalid.
        if id_raw.is_none() {
            return None;
        }
        return Some(proto_error(id_raw, -32600, "Invalid Request: missing method"));
    };
    if method.starts_with("notifications/") {
        return None;
    }
    let Some(id) = id_raw else {
        return None; // requests without id are notifications
    };
    match method.as_str() {
        "initialize" => {
            let result = format!(
                r#"{{"protocolVersion":"{PROTOCOL_VERSION}","capabilities":{{"tools":{{"listChanged":false}}}},"serverInfo":{{"name":"{SERVER_NAME}","version":"{SERVER_VERSION}"}},"instructions":{}}}"#,
                escape_json(&instructions(srv)),
            );
            Some(result_envelope(id, &result))
        }
        "tools/list" => Some(result_envelope(id, &tools_list_json())),
        "tools/call" => {
            let params_raw = field(&fields, trimmed, "params").unwrap_or("{}");
            let params = object_fields(params_raw);
            let name = params
                .as_ref()
                .and_then(|p| field_str(p, params_raw, "name"));
            let Some(name) = name else {
                return Some(proto_error(Some(id), -32602, "Invalid params: missing name"));
            };
            if !tool_defs().iter().any(|d| d.name == name) {
                return Some(proto_error(
                    Some(id),
                    -32602,
                    "Invalid params: unknown tool",
                ));
            }
            let args_raw = params
                .as_ref()
                .and_then(|p| field(p, params_raw, "arguments"))
                .unwrap_or("{}");
            Some(tool_envelope(id, &dispatch(&name, args_raw, cc)))
        }
        _ => Some(proto_error(Some(id), -32601, "Method not found")),
    }
}

/// Resolve the broker endpoint: an explicit flag wins, otherwise the
/// inherited environment. A per-session route file supersedes stale
/// inheritance only for remote sessions (Phase 8); locally the inherited
/// endpoint is always fresh because panes spawn after the listener.
pub fn resolve_endpoint(explicit: Option<&str>) -> Option<String> {
    if let Some(p) = explicit.filter(|p| !p.is_empty()) {
        return Some(p.to_string());
    }
    std::env::var("FORGE_IPC_ENDPOINT")
        .ok()
        .filter(|p| !p.is_empty())
}

/// One `tools/call` over the IPC socket: send the comms record, await the
/// broker's one-line verdict. Transport trouble is a tool failure, never a
/// protocol error, so the harness always gets an answer.
pub fn call_via_ipc(
    endpoint: &str,
    tool: &str,
    args_raw: &str,
    run_id: &str,
    timeout: std::time::Duration,
) -> Result<ToolResult, String> {
    let args = if args_raw.trim().is_empty() {
        "null"
    } else {
        args_raw
    };
    let record = format!(
        "{{\"v\":1,\"kind\":\"comms\",\"run_id\":{},\"tool\":{},\"args\":{args}}}\n",
        escape_json(run_id),
        escape_json(tool),
    );
    let mut conn = std::os::unix::net::UnixStream::connect(endpoint)
        .map_err(|_| "cannot reach comms route".to_string())?;
    {
        use std::io::Write;
        conn.write_all(record.as_bytes())
            .map_err(|_| "cannot reach comms route".to_string())?;
    }
    conn.set_read_timeout(Some(timeout))
        .map_err(|_| "cannot reach comms route".to_string())?;
    let mut line = Vec::new();
    {
        use std::io::Read;
        let mut byte = [0u8; 1];
        loop {
            match conn.read(&mut byte) {
                Ok(1) => {
                    line.push(byte[0]);
                    if byte[0] == b'\n' || line.len() > 1_048_576 {
                        break;
                    }
                }
                _ => return Err("comms reply timed out".to_string()),
            }
        }
    }
    let text = String::from_utf8_lossy(&line).into_owned();
    let fields =
        object_fields(text.trim()).ok_or_else(|| "comms reply timed out".to_string())?;
    let ok = field(&fields, &text, "ok").is_some_and(|v| v == "true");
    if ok {
        let result = field(&fields, &text, "result").unwrap_or("null");
        Ok(ToolResult::ok(result))
    } else {
        let err = field(&fields, &text, "error")
            .and_then(|raw| {
                let mut s = Scanner {
                    b: raw.as_bytes(),
                    i: 0,
                };
                s.string()
            })
            .unwrap_or_else(|| "broker error".to_string());
        Ok(ToolResult::fail(&err))
    }
}

/// Serve JSON-RPC on stdio until EOF. Fail-soft like the hook relay: a dead
/// broker surfaces as `isError` results, and output trouble just ends us.
pub fn serve_stdio(srv: &ServerCtx, cc: &CallCtx) -> i32 {
    use std::io::{BufRead, Write};
    let dispatch = |tool: &str, args: &str, cc: &CallCtx| match cc.endpoint.as_deref() {
        Some(endpoint) => match call_via_ipc(endpoint, tool, args, &cc.run_id, cc.timeout) {
            Ok(result) => result,
            Err(e) => ToolResult::fail(&e),
        },
        None => ToolResult::fail("no comms route: TUI is not listening"),
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if let Some(res) = handle_line(&line, srv, &dispatch, cc) {
            if writeln!(out, "{res}").is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ServerCtx {
        ServerCtx {
            instructions_extra: String::new(),
        }
    }

    fn call_ctx() -> CallCtx {
        CallCtx {
            endpoint: None,
            run_id: "deadbeef".to_string(),
            timeout: std::time::Duration::from_millis(200),
        }
    }

    fn stub(_tool: &str, _args: &str, _cc: &CallCtx) -> ToolResult {
        ToolResult::ok(r#"{"conversation":"abc"}"#)
    }

    #[test]
    fn initialize_advertises_mcp_2025_03_26() {
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("initialize answers");
        assert!(res.contains(r#""protocolVersion":"2025-03-26""#), "res: {res}");
        assert!(res.contains(r#""name":"forge""#), "res: {res}");
        assert!(res.contains(r#""tools""#), "res: {res}");
        assert!(res.contains(r#""instructions":""#), "res: {res}");
    }

    #[test]
    fn tools_list_advertises_every_tool() {
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list","params":{}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("tools/list answers");
        for tool in [
            "ask_session",
            "send_response",
            "tell_session",
            "ack_message",
            "list_sessions",
            "walkthrough_start",
            "walkthrough_answer",
            "walkthrough_end",
            "visual_answer",
            #[cfg(all(target_os = "linux", feature = "visual"))]
            "screenshot",
            "compact_session",
            "schedule_prompt",
            "cancel_scheduled_prompt",
            "start_session",
            "set_session_status",
            "clear_session_status",
            "walkthrough_add_step",
            "walkthrough_update",
            #[cfg(feature = "visual")]
            "visual_show",
        ] {
            assert!(res.contains(&format!(r#""name":"{tool}""#)), "res: {res}");
        }
        // Every send honors idempotency_key, so every send schema
        // must advertise it: agents cannot retry safely with a key
        // they were never told exists. Each tool owns the slice up
        // to the next tool's name, so a neighbor's key cannot mask
        // a missing one.
        for tool in [
            "ask_session",
            "send_response",
            "tell_session",
            "ack_message",
            "compact_session",
            "schedule_prompt",
            "cancel_scheduled_prompt",
        ] {
            let marker = format!(r#""name":"{tool}""#);
            let at = res.find(&marker).expect("tool listed");
            let rest = &res[at + marker.len()..];
            let end = rest.find(r#""name":""#).unwrap_or(rest.len());
            let window = &rest[..end];
            assert!(
                window.contains("idempotency_key"),
                "{tool} advertises idempotency_key: {window}"
            );
        }
    }

    #[test]
    fn comms_descriptions_name_reply_triggers() {
        // Pane-driven CLIs only reach for tools whose descriptions name
        // the trigger: the [forge ...] pane line, the answering tool,
        // and the discovery step. Each comms description must carry its
        // trigger words so models prioritize them over chat-only answers.
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list","params":{}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("tools/list answers");
        let window = |tool: &str| -> String {
            let marker = format!(r#""name":"{tool}""#);
            let at = res.find(&marker).expect("tool listed");
            let rest = &res[at + marker.len()..];
            let end = rest.find(r#""name":""#).unwrap_or(rest.len());
            rest[..end].to_string()
        };
        assert!(
            window("ask_session").contains("list_sessions"),
            "ask names discovery: {}",
            window("ask_session")
        );
        assert!(
            window("send_response").contains("ask_session"),
            "response names its trigger: {}",
            window("send_response")
        );
        let tell = window("tell_session");
        assert!(
            tell.contains("conversation_id") && tell.contains("follow"),
            "tell names follow-ups: {tell}"
        );
        assert!(
            window("ack_message").contains("receipt"),
            "ack names receipt: {}",
            window("ack_message")
        );
        let list = window("list_sessions");
        assert!(
            list.contains("alive") && list.contains("`you`"),
            "list names liveness and identity: {list}"
        );
        let init = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("initialize answers");
        assert!(
            init.contains("[forge "),
            "instructions map pane lines to tools: {init}"
        );
    }

    #[test]
    fn forge_comms_override_native_messaging() {
        // Forge hosts peer agent sessions side by side: cross-session
        // traffic MUST go through Forge tools, never the CLI's own
        // messaging. The global instructions carry the MUST/NEVER rule
        // (always in context); each comms description repeats a short
        // clause so models considering that tool see the override.
        let init = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("initialize answers");
        assert!(init.contains("MUST go through these Forge tools"), "init: {init}");
        assert!(init.contains("NEVER use your CLI's own messaging"), "init: {init}");
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":"a","method":"tools/list","params":{}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("tools/list answers");
        let window = |tool: &str| -> String {
            let marker = format!(r#""name":"{tool}""#);
            let at = res.find(&marker).expect("tool listed");
            let rest = &res[at + marker.len()..];
            let end = rest.find(r#""name":""#).unwrap_or(rest.len());
            rest[..end].to_string()
        };
        for tool in [
            "ask_session",
            "send_response",
            "tell_session",
            "ack_message",
            "list_sessions",
        ] {
            assert!(
                window(tool).contains("not your CLI's own"),
                "{tool} overrides native messaging: {}",
                window(tool)
            );
        }
    }

    #[test]
    fn decode_json_string_handles_escapes_and_keeps_garbage_literal() {
        assert_eq!(decode_json_string("a\\nb"), "a\nb");
        assert_eq!(decode_json_string("\\\"q\\\" \\\\ \\/"), "\"q\" \\ /");
        assert_eq!(decode_json_string("\\u0041"), "A");
        assert_eq!(decode_json_string("plain"), "plain");
        assert_eq!(decode_json_string("trail\\"), "trail\\");
        assert_eq!(decode_json_string("\\ud83d"), "\\ud83d");
        assert_eq!(decode_json_string("\\q"), "\\q");
    }

    #[test]
    fn tools_call_returns_stub_result_envelope() {
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"ask_session","arguments":{"target":"b","text":"hi"}}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("tools/call answers");
        assert!(res.contains(r#""id":7"#), "res: {res}");
        assert!(res.contains(r#""isError":false"#), "res: {res}");
        assert!(res.contains("conversation"), "res: {res}");
    }

    #[test]
    fn tool_failure_becomes_is_error_not_protocol_error() {
        let fail = |_: &str, _: &str, _: &CallCtx| ToolResult::fail("no such group");
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"tell_session","arguments":{}}}"#,
            &ctx(),
            &fail,
            &call_ctx(),
        )
        .expect("tool failure still answers");
        assert!(res.contains(r#""isError":true"#), "res: {res}");
        assert!(res.contains("no such group"), "res: {res}");
        assert!(!res.contains(r#""error":{"code""#), "res: {res}");
    }

    #[test]
    fn garbage_line_is_a_parse_error_with_null_id() {
        let res = handle_line("this is not json", &ctx(), &stub, &call_ctx())
            .expect("parse errors answer");
        assert!(res.contains(r#""id":null"#), "res: {res}");
        assert!(res.contains(r#""code":-32700"#), "res: {res}");
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/bogus","params":{}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("unknown method answers");
        assert!(res.contains(r#""code":-32601"#), "res: {res}");
        assert!(res.contains(r#""id":3"#), "res: {res}");
    }

    #[test]
    fn tools_call_without_name_is_invalid_params() {
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"arguments":{}}}"#,
            &ctx(),
            &stub,
            &call_ctx(),
        )
        .expect("missing name answers");
        assert!(res.contains(r#""code":-32602"#), "res: {res}");
    }

    #[test]
    fn initialized_notification_gets_no_reply() {
        assert_eq!(
            handle_line(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                &ctx(),
                &stub,
                &call_ctx()
            ),
            None
        );
    }

    #[test]
    fn hostile_context_is_escaped_not_injected() {
        let evil = ctx_with("quote\" newline\n control\x01 end");
        let res = handle_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            &evil,
            &stub,
            &call_ctx(),
        )
        .expect("initialize answers");
        assert!(res.contains(r#"quote\" newline\n control\u0001 end"#), "res: {res}");
    }

    #[test]
    fn bridge_sends_comms_record_and_maps_reply() {
        let path = std::env::temp_dir().join(format!(
            "forge-mcp-test-{}-bridge.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen2 = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut conn, _) = listener.accept().unwrap();
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while conn.read(&mut byte).unwrap_or(0) == 1 {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            *seen2.lock().unwrap() = line;
            conn.write_all(b"{\"ok\":true,\"result\":{\"a\":1}}\n").unwrap();
        });
        let out = call_via_ipc(
            path.to_str().unwrap(),
            "ask_session",
            r#"{"target":"b"}"#,
            "abc123",
            std::time::Duration::from_secs(5),
        )
        .expect("bridge round-trips");
        assert_eq!(out.content_json, r#"{"a":1}"#);
        assert!(!out.is_error);
        let raw = String::from_utf8(seen.lock().unwrap().clone()).unwrap();
        assert!(raw.contains(r#""kind":"comms""#), "record: {raw}");
        assert!(raw.contains(r#""tool":"ask_session""#), "record: {raw}");
        assert!(raw.contains(r#""run_id":"abc123""#), "record: {raw}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bridge_timeout_is_a_tool_failure() {
        let path = std::env::temp_dir().join(format!(
            "forge-mcp-test-{}-hang.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || {
            let (_conn, _) = listener.accept().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(10));
        });
        let err = call_via_ipc(
            path.to_str().unwrap(),
            "ask_session",
            "{}",
            "abc123",
            std::time::Duration::from_millis(100),
        )
        .expect_err("hung broker must fail the call");
        assert!(err.contains("timeout") || err.contains("timed out"), "err: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn endpoint_resolution_prefers_explicit_over_env() {
        std::env::set_var("FORGE_IPC_ENDPOINT", "/tmp/from-env.sock");
        assert_eq!(
            resolve_endpoint(Some("/tmp/explicit.sock")),
            Some("/tmp/explicit.sock".to_string())
        );
        assert_eq!(
            resolve_endpoint(None),
            Some("/tmp/from-env.sock".to_string())
        );
        std::env::remove_var("FORGE_IPC_ENDPOINT");
        assert_eq!(resolve_endpoint(None), None);
    }
}
