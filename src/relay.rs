//! Short-lived fail-open hook relay (Phase 3a).
//!
//! Harnesses invoke `forge hook-relay` from their hook handlers. It reads
//! all of stdin as JSON, resolves whether the hook needs a synchronous
//! decision, sends one newline record to the TUI listener, optionally
//! awaits a one-line decision (~3 s), prints it when one arrives, and
//! always exits zero silently so an unavailable Forge never blocks a
//! harness.

/// How long a synchronous hook waits for the TUI decision.
pub const DECISION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Hooks that block the harness until Forge decides. Everything else is
/// fire-and-forget: the record is still delivered, but no reply is read.
pub fn is_sync_hook(name: &str) -> bool {
    matches!(name, "PreToolUse" | "PermissionRequest" | "BeforeTool")
}

/// Extract the hook name from harness stdin JSON without a JSON dependency:
/// valid JSON never contains literal control bytes inside strings, so a
/// plain field scan is exact. Checks Claude's `hook_event_name`, then
/// generic `hook` / `event` fields.
pub fn hook_name(input: &[u8]) -> Option<String> {
    for field in ["hook_event_name", "hook", "event"] {
        let needle = format!("\"{field}\"");
        let mut search = input;
        while let Some(pos) = find_subslice(search, needle.as_bytes()) {
            let mut rest = &search[pos + needle.len()..];
            rest = skip_json_gap(rest);
            if rest.first() != Some(&b':') {
                search = &search[pos + 1..];
                continue;
            }
            rest = skip_json_gap(&rest[1..]);
            if rest.first() != Some(&b'"') {
                search = &search[pos + 1..];
                continue;
            }
            let mut end = 1;
            while end < rest.len() && rest[end] != b'"' {
                // No unescaped quote can hide inside valid JSON strings;
                // backslash escapes never contain a raw quote.
                if rest[end] == b'\\' {
                    end += 1;
                }
                end += 1;
            }
            if end < rest.len() {
                if let Ok(name) = std::str::from_utf8(&rest[1..end]) {
                    return Some(unescape_json_string(name));
                }
            }
            search = &search[pos + 1..];
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn skip_json_gap(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        bytes = &bytes[1..];
    }
    bytes
}

fn unescape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Build the single newline record: a tiny envelope carrying the route, the
/// sender's run ID (empty when unset), plus the raw stdin body. Literal
/// CR/LF bytes cannot occur inside valid JSON strings, so stripping them
/// keeps the body intact while guaranteeing one line on the wire.
pub fn record_line(input: &[u8], hook: Option<&str>, run_id: &str) -> Vec<u8> {
    let body: Vec<u8> = input
        .iter()
        .copied()
        .filter(|b| *b != b'\n' && *b != b'\r')
        .collect();
    let hook = hook.unwrap_or("");
    let mut line = Vec::with_capacity(body.len() + hook.len() + run_id.len() + 48);
    line.extend_from_slice(b"{\"v\":1,\"hook\":\"");
    line.extend_from_slice(hook.replace('\\', "\\\\").replace('"', "\\\"").as_bytes());
    line.extend_from_slice(b"\",\"run_id\":\"");
    line.extend_from_slice(run_id.replace('\\', "\\\\").replace('"', "\\\"").as_bytes());
    line.extend_from_slice(b"\",\"body\":");
    if body.is_empty() {
        line.extend_from_slice(b"null");
    } else {
        line.extend_from_slice(&body);
    }
    line.extend_from_slice(b"}\n");
    line
}

/// Relay one hook event. Returns the process exit code: always zero.
/// `endpoint` is the TUI listener socket path; `None` means unavailable.
pub fn run(
    stdin_bytes: &[u8],
    endpoint: Option<&str>,
    stdout: &mut dyn std::io::Write,
    timeout: std::time::Duration,
) -> i32 {
    if stdin_bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return 0;
    }
    let Some(path) = endpoint.filter(|p| !p.is_empty()) else {
        return 0;
    };
    let hook = hook_name(stdin_bytes);
    // Hook children inherit this from the session pane (session.rs); it
    // attributes the record to its session for activity tracking.
    let run_id = std::env::var("FORGE_RUN_ID").unwrap_or_default();
    let record = record_line(stdin_bytes, hook.as_deref(), &run_id);
    let mut conn = match std::os::unix::net::UnixStream::connect(path) {
        Ok(conn) => conn,
        Err(_) => return 0,
    };
    {
        use std::io::Write;
        if conn.write_all(&record).is_err() {
            return 0;
        }
    }
    if !hook.as_deref().is_some_and(is_sync_hook) {
        return 0;
    }
    if conn.set_read_timeout(Some(timeout)).is_err() {
        return 0;
    }
    {
        use std::io::Read;
        let mut reply = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match conn.read(&mut byte) {
                Ok(1) => {
                    reply.push(byte[0]);
                    if byte[0] == b'\n' || reply.len() > 65536 {
                        break;
                    }
                }
                _ => return 0,
            }
        }
        if reply.ends_with(b"\n") {
            use std::io::Write;
            let _ = stdout.write_all(&reply);
        }
    }
    0
}

/// Read stdin fully and relay through the endpoint override or
/// `FORGE_IPC_ENDPOINT`. Always exits zero; use the return as the code.
pub fn run_stdin(endpoint_override: Option<&str>) -> i32 {
    let mut stdin_bytes = Vec::new();
    {
        use std::io::Read;
        if std::io::stdin().read_to_end(&mut stdin_bytes).is_err() {
            return 0;
        }
    }
    let env_endpoint = std::env::var("FORGE_IPC_ENDPOINT").ok();
    let endpoint = endpoint_override
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .or(env_endpoint);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    run(&stdin_bytes, endpoint.as_deref(), &mut handle, DECISION_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_hooks_need_a_decision() {
        assert!(is_sync_hook("PreToolUse"));
        assert!(is_sync_hook("PermissionRequest"));
        assert!(is_sync_hook("BeforeTool"));
        assert!(!is_sync_hook("PostToolUse"));
        assert!(!is_sync_hook("SessionStart"));
        assert!(!is_sync_hook("Notification"));
        assert!(!is_sync_hook("whatever"));
    }

    #[test]
    fn hook_name_scans_known_fields() {
        assert_eq!(
            hook_name(br#"{"hook_event_name":"PreToolUse","tool":"Bash"}"#),
            Some("PreToolUse".to_string())
        );
        assert_eq!(
            hook_name(br#"{"hook":"Stop"}"#),
            Some("Stop".to_string())
        );
        assert_eq!(hook_name(br#"{"tool":"Bash"}"#), None);
        assert_eq!(hook_name(b"not json"), None);
        assert_eq!(hook_name(b""), None);
    }

    #[test]
    fn record_is_one_line_envelope() {
        let line = record_line(br#"{"hook_event_name":"Stop"}"#, Some("Stop"), "run-1");
        assert_eq!(
            line.iter().filter(|b| **b == b'\n').count(),
            1,
            "exactly the terminator newline: {line:?}"
        );
        assert!(line.ends_with(b"\n"), "newline terminated");
        let text = String::from_utf8(line).unwrap();
        assert!(text.contains(r#""hook":"Stop""#), "route inside: {text:?}");
        assert!(text.contains(r#""run_id":"run-1""#), "attribution inside: {text:?}");
        assert!(text.contains(r#"hook_event_name"#), "body inside");
    }

    #[test]
    fn no_endpoint_is_silent_success() {
        let mut out = Vec::new();
        let code = run(b"{}", None, &mut out, std::time::Duration::from_millis(50));
        assert_eq!(code, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn empty_stdin_sends_nothing() {
        let mut out = Vec::new();
        let code = run(
            b"",
            Some("/nonexistent-forge-test.sock"),
            &mut out,
            std::time::Duration::from_millis(50),
        );
        assert_eq!(code, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn unreachable_listener_is_silent_success() {
        let mut out = Vec::new();
        let code = run(
            br#"{"hook_event_name":"PreToolUse"}"#,
            Some("/nonexistent-forge-test.sock"),
            &mut out,
            std::time::Duration::from_millis(50),
        );
        assert_eq!(code, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn listener_decision_is_relayed() {
        let path = std::env::temp_dir().join(format!(
            "forge-relay-test-{}-decision.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut record = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                use std::io::Read;
                match conn.read(&mut byte) {
                    Ok(1) => {
                        record.push(byte[0]);
                        if byte[0] == b'\n' {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            assert!(record.ends_with(b"\n"), "one newline record");
            let text = String::from_utf8(record).unwrap();
            assert!(text.contains(r#""hook":"PreToolUse""#), "route: {text:?}");
            use std::io::Write;
            conn.write_all(b"{\"decision\":\"allow\"}\n").unwrap();
        });
        let mut out = Vec::new();
        let code = run(
            br#"{"hook_event_name":"PreToolUse","tool":"Bash"}"#,
            Some(path.to_str().unwrap()),
            &mut out,
            std::time::Duration::from_secs(5),
        );
        handle.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(code, 0);
        assert_eq!(out, b"{\"decision\":\"allow\"}\n");
    }

    #[test]
    fn silent_listener_times_out_quietly() {
        let path = std::env::temp_dir().join(format!(
            "forge-relay-test-{}-silent.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let handle = std::thread::spawn(move || {
            let (_conn, _) = listener.accept().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(400));
        });
        let mut out = Vec::new();
        let start = std::time::Instant::now();
        let code = run(
            br#"{"hook_event_name":"PreToolUse"}"#,
            Some(path.to_str().unwrap()),
            &mut out,
            std::time::Duration::from_millis(150),
        );
        handle.join().unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(code, 0);
        assert!(out.is_empty());
        assert!(
            start.elapsed() < std::time::Duration::from_secs(4),
            "returned promptly"
        );
    }
}
