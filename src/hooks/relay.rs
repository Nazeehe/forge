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

/// Home-relative path of the live-endpoint file the TUI maintains. Some
/// harnesses (muse) scrub hook-child environments, so neither
/// `FORGE_IPC_ENDPOINT` nor `FORGE_RUN_ID` arrives; the file lets the relay
/// still reach a running TUI. Records sent this way carry no run ID; the loop
/// attributes them by their inherited PTY process-session ID, with the
/// harness session ID/cwd path retained for old relay records (see session.rs).
pub fn endpoint_file_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".forge/endpoint.json")
}

/// Publish this process's listener socket for scrubbed hook children. Called
/// once per TUI boot; records the owner pid so a later instance never
/// accepts another instance's records.
pub fn write_endpoint_file(
    home: &std::path::Path,
    pid: u32,
    sock: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = endpoint_file_path(home).parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = format!(
        "{{\"pid\":{pid},\"sock\":{}}}",
        crate::ipc::mcp::escape_json(&sock.to_string_lossy())
    );
    crate::infra::fs_atomic::write_atomic(&endpoint_file_path(home), text.as_bytes())
}

/// Remove the live-endpoint file at orderly shutdown so orphaned hook
/// children fail open instead of routing into whoever boots next.
pub fn clear_endpoint_file(home: &std::path::Path) {
    let _ = std::fs::remove_file(endpoint_file_path(home));
}

/// Owner pid recorded in `text`, when it names a live process running this
/// same binary. Anything else (missing file, foreign pid, reused pid now
/// running something else) yields None.
fn file_owner_pid(text: &str) -> Option<u32> {
    file_owner(text).ok()
}

/// [`file_owner_pid`] with the rejection reason, for the hook trace.
fn file_owner(text: &str) -> Result<u32, String> {
    let raw = crate::ipc::mcp::top_raw(text, "pid").ok_or("no pid field")?;
    let pid: u32 = raw.parse().map_err(|_| format!("bad pid {raw:?}"))?;
    if pid == 0 {
        return Err("pid 0".to_string());
    }
    if pid == std::process::id() {
        return Err("owner pid is this process".to_string());
    }
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|e| format!("owner {pid} exe unreadable: {e}"))?;
    let own = std::env::current_exe().map_err(|e| format!("own exe unknown: {e}"))?;
    if same_binary(&exe, &own) {
        Ok(pid)
    } else {
        Err(format!("owner {pid} exe {} != {}", exe.display(), own.display()))
    }
}

/// Whether the endpoint owner runs forge. Compared by file name, not path:
/// the TUI may run an installed copy while harness hooks name a dev build,
/// and a rebuild shows the owner's exe as `<path> (deleted)`. Records still
/// carry the owner pid, so the listener rejects any other instance's relays.
fn same_binary(owner_exe: &std::path::Path, own: &std::path::Path) -> bool {
    let owner = owner_exe.to_str().map(|s| s.strip_suffix(" (deleted)").unwrap_or(s));
    let owner_name = owner.and_then(|s| std::path::Path::new(s).file_name());
    owner_name.is_some() && owner_name == own.file_name()
}

/// Endpoint from the live-endpoint file: `(owner pid, socket path)`. The
/// pid check keeps stale files (dead TUI, reused pid) fail-open and lets
/// the loop drop records that another instance's relays send our way.
pub fn file_endpoint() -> Option<(u32, String)> {
    file_endpoint_traced().ok()
}

/// [`file_endpoint`] with the rejection reason, for the hook trace.
fn file_endpoint_traced() -> Result<(u32, String), String> {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .ok_or("HOME unset")?;
    let path = endpoint_file_path(&home);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{} unreadable: {e}", path.display()))?;
    let pid = file_owner(&text)?;
    match crate::ipc::mcp::top_str(&text, "sock") {
        Some(sock) if !sock.is_empty() => Ok((pid, sock)),
        _ => Err("no sock field".to_string()),
    }
}

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
/// sender's run ID (empty when unset), the endpoint-file owner pid (zero
/// for explicit channels), the relay's Unix process-session ID, plus the raw
/// stdin body. PTY children are session leaders, so the latter remains a
/// deterministic attribution key even when a harness scrubs its hook-child
/// environment. Literal CR/LF bytes cannot occur inside valid JSON strings,
/// so stripping them keeps the body intact while guaranteeing one line on
/// the wire.
pub fn record_line(input: &[u8], hook: Option<&str>, run_id: &str, forge_pid: u32) -> Vec<u8> {
    let body: Vec<u8> = input
        .iter()
        .copied()
        .filter(|b| *b != b'\n' && *b != b'\r')
        .collect();
    let hook = hook.unwrap_or("");
    let mut line = Vec::with_capacity(body.len() + hook.len() + run_id.len() + 64);
    line.extend_from_slice(b"{\"v\":1,\"hook\":\"");
    line.extend_from_slice(hook.replace('\\', "\\\\").replace('"', "\\\"").as_bytes());
    line.extend_from_slice(b"\",\"run_id\":\"");
    line.extend_from_slice(run_id.replace('\\', "\\\\").replace('"', "\\\"").as_bytes());
    line.extend_from_slice(b"\",\"forge_pid\":");
    line.extend_from_slice(forge_pid.to_string().as_bytes());
    // SAFETY: getsid with pid zero only queries the calling process.
    let source_sid = unsafe { libc::getsid(0) };
    line.extend_from_slice(b",\"source_sid\":");
    line.extend_from_slice(source_sid.max(0).to_string().as_bytes());
    line.extend_from_slice(b",\"body\":");
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
/// `forge_pid` tags records resolved through the endpoint file so the loop
/// can drop another instance's relays; explicit channels pass zero.
pub fn run(
    stdin_bytes: &[u8],
    endpoint: Option<&str>,
    forge_pid: u32,
    stdout: &mut dyn std::io::Write,
    timeout: std::time::Duration,
) -> i32 {
    deliver(stdin_bytes, endpoint, forge_pid, stdout, timeout);
    0
}

/// [`run`], describing what happened for the hook trace.
pub fn deliver(
    stdin_bytes: &[u8],
    endpoint: Option<&str>,
    forge_pid: u32,
    stdout: &mut dyn std::io::Write,
    timeout: std::time::Duration,
) -> String {
    if stdin_bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return "empty stdin".to_string();
    }
    let Some(path) = endpoint.filter(|p| !p.is_empty()) else {
        return "no endpoint".to_string();
    };
    let hook = hook_name(stdin_bytes);
    // Hook children inherit this from the session pane (session.rs); it
    // attributes the record to its session for activity tracking. Scrubbed
    // harnesses (muse) send none; the loop falls back to harness ID.
    let run_id = std::env::var("FORGE_RUN_ID").unwrap_or_default();
    let record = record_line(stdin_bytes, hook.as_deref(), &run_id, forge_pid);
    let mut conn = match std::os::unix::net::UnixStream::connect(path) {
        Ok(conn) => conn,
        Err(e) => return format!("connect failed: {e}"),
    };
    {
        use std::io::Write;
        if let Err(e) = conn.write_all(&record) {
            return format!("write failed: {e}");
        }
    }
    if !hook.as_deref().is_some_and(is_sync_hook) {
        return "sent".to_string();
    }
    if let Err(e) = conn.set_read_timeout(Some(timeout)) {
        return format!("sent; read timeout unset: {e}");
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
                Ok(_) => return "sent; no decision (closed)".to_string(),
                Err(e) => return format!("sent; no decision: {e}"),
            }
        }
        if reply.ends_with(b"\n") {
            let _ = stdout.write_all(&reply);
            return "sent; decision relayed".to_string();
        }
    }
    "sent; decision too long".to_string()
}

/// One hook-trace line for this relay invocation: everything the TUI
/// needs to attribute it, plus how (and whether) it was delivered.
pub fn trace_line(input: &[u8], run_id: &str, endpoint: &str, sock: &str, outcome: &str) -> String {
    let text = String::from_utf8_lossy(input);
    // SAFETY: getsid/getppid with no pointers only query this process.
    let (sid, ppid) = unsafe { (libc::getsid(0), libc::getppid()) };
    format!(
        "relay pid={} ppid={ppid} sid={sid} hook={} session_id={} run_id={} endpoint={endpoint} sock={sock} -> {outcome}",
        std::process::id(),
        hook_name(input).unwrap_or_else(|| "-".to_string()),
        crate::ipc::mcp::top_str(&text, "session_id").unwrap_or_else(|| "-".to_string()),
        if run_id.is_empty() { "empty" } else { "set" },
    )
}

/// Where the relay writes its trace: `$HOME`, else the passwd entry, since
/// a scrubbed hook environment may carry no HOME at all.
fn trace_home() -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Some(home.into());
    }
    // SAFETY: getpwuid returns a pointer into static storage (or null); the
    // relay is single-threaded and copies the string out immediately.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir);
        Some(std::path::PathBuf::from(dir.to_string_lossy().into_owned()))
    }
}

/// Read stdin fully and relay through the endpoint override,
/// `FORGE_IPC_ENDPOINT`, or the TUI's live-endpoint file (for harnesses
/// like muse that scrub hook-child environments). Always exits zero; use
/// the return as the code.
pub fn run_stdin(endpoint_override: Option<&str>) -> i32 {
    if let Some(home) = trace_home() {
        crate::infra::logging::set_hook_trace_path(crate::infra::branding::hooks_log(&home));
    }
    let mut stdin_bytes = Vec::new();
    {
        use std::io::Read;
        if let Err(e) = std::io::stdin().read_to_end(&mut stdin_bytes) {
            crate::infra::logging::hook_trace_global(&format!("relay stdin read failed: {e}"));
            return 0;
        }
    }
    let env_endpoint = std::env::var("FORGE_IPC_ENDPOINT").ok().filter(|p| !p.is_empty());
    let override_endpoint = endpoint_override.filter(|p| !p.is_empty()).map(str::to_string);
    let (endpoint, forge_pid, source) = if let Some(path) = override_endpoint {
        (Some(path), 0, "override".to_string())
    } else if let Some(path) = env_endpoint {
        (Some(path), 0, "env".to_string())
    } else {
        match file_endpoint_traced() {
            Ok((pid, sock)) => (Some(sock), pid, format!("file:{pid}")),
            Err(why) => (None, 0, format!("none({why})")),
        }
    };
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let outcome = deliver(&stdin_bytes, endpoint.as_deref(), forge_pid, &mut handle, DECISION_TIMEOUT);
    let run_id = std::env::var("FORGE_RUN_ID").unwrap_or_default();
    crate::infra::logging::hook_trace_global(&trace_line(
        &stdin_bytes,
        &run_id,
        &source,
        endpoint.as_deref().unwrap_or("-"),
        &outcome,
    ));
    0
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
        let line = record_line(br#"{"hook_event_name":"Stop"}"#, Some("Stop"), "run-1", 0);
        assert_eq!(
            line.iter().filter(|b| **b == b'\n').count(),
            1,
            "exactly the terminator newline: {line:?}"
        );
        assert!(line.ends_with(b"\n"), "newline terminated");
        let text = String::from_utf8(line).unwrap();
        assert!(text.contains(r#""hook":"Stop""#), "route inside: {text:?}");
        assert!(text.contains(r#""run_id":"run-1""#), "attribution inside: {text:?}");
        assert!(text.contains(r#""forge_pid":0"#), "explicit channel untagged: {text:?}");
        let source_sid = unsafe { libc::getsid(0) };
        assert!(source_sid > 0, "test process has a Unix session");
        assert!(
            text.contains(&format!(r#""source_sid":{source_sid}"#)),
            "PTY process-session attribution rides along: {text:?}"
        );
        assert!(text.contains(r#"hook_event_name"#), "body inside");
        let tagged = record_line(b"{}", Some("Stop"), "", 4242);
        let tagged_text = String::from_utf8(tagged).unwrap();
        assert!(
            tagged_text.contains(r#""forge_pid":4242"#),
            "file owner rides along: {tagged_text:?}"
        );
    }

    #[test]
    fn no_endpoint_is_silent_success() {
        let mut out = Vec::new();
        let code = run(b"{}", None, 0, &mut out, std::time::Duration::from_millis(50));
        assert_eq!(code, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn empty_stdin_sends_nothing() {
        let mut out = Vec::new();
        let code = run(
            b"",
            Some("/nonexistent-forge-test.sock"),
            0,
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
            0,
            &mut out,
            std::time::Duration::from_millis(50),
        );
        assert_eq!(code, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn endpoint_file_round_trips_through_helpers() {
        let home = std::env::temp_dir().join(format!(
            "forge-relay-test-{}-home",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        // Self pid is never accepted back: a relay is never its own TUI.
        write_endpoint_file(&home, std::process::id(), std::path::Path::new("/tmp/x.sock"))
            .unwrap();
        let text = std::fs::read_to_string(endpoint_file_path(&home)).unwrap();
        assert!(text.contains("/tmp/x.sock"), "sock persisted: {text}");
        assert_eq!(file_owner_pid(&text), None, "self pid refused");
        // A dead pid is refused too.
        let dead = 424242u32;
        assert!(
            std::fs::read_link(format!("/proc/{dead}/exe")).is_err(),
            "test needs a dead pid"
        );
        write_endpoint_file(
            &home,
            dead,
            std::path::Path::new("/tmp/y.sock"),
        )
        .unwrap();
        let text = std::fs::read_to_string(endpoint_file_path(&home)).unwrap();
        assert_eq!(file_owner_pid(&text), None, "dead pid refused");
        // Garbage is refused without panicking.
        assert_eq!(file_owner_pid("not json"), None);
        assert_eq!(file_owner_pid(r#"{"pid":"abc","sock":"/tmp/z.sock"}"#), None);
        assert_eq!(file_owner_pid(r#"{"pid":0,"sock":"/tmp/z.sock"}"#), None);
        clear_endpoint_file(&home);
        assert!(!endpoint_file_path(&home).exists(), "cleared");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn file_owner_explains_every_rejection() {
        let own = std::process::id();
        assert!(file_owner("not json").unwrap_err().contains("pid"));
        assert!(file_owner(r#"{"pid":0}"#).unwrap_err().contains("pid 0"));
        assert!(file_owner(&format!(r#"{{"pid":{own}}}"#))
            .unwrap_err()
            .contains("this process"));
        let dead = file_owner(r#"{"pid":424242}"#).unwrap_err();
        assert!(dead.contains("424242") && dead.contains("unreadable"), "{dead}");
        let mut other = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let foreign = file_owner(&format!(r#"{{"pid":{}}}"#, other.id())).unwrap_err();
        let _ = other.kill();
        let _ = other.wait();
        assert!(foreign.contains("sleep") && foreign.contains("!="), "{foreign}");
    }

    #[test]
    fn deliver_reports_its_outcome() {
        let quick = std::time::Duration::from_millis(50);
        let mut out = Vec::new();
        assert_eq!(deliver(b"  ", Some("/x.sock"), 0, &mut out, quick), "empty stdin");
        assert_eq!(deliver(b"{}", None, 0, &mut out, quick), "no endpoint");
        let failed = deliver(
            br#"{"hook_event_name":"Stop"}"#,
            Some("/nonexistent-forge-test.sock"),
            0,
            &mut out,
            quick,
        );
        assert!(failed.starts_with("connect failed"), "{failed}");
    }

    #[test]
    fn trace_line_carries_attribution_facts() {
        let line = trace_line(
            br#"{"hook_event_name":"UserPromptSubmit","session_id":"muse-1"}"#,
            "",
            "file:77",
            "/tmp/f.sock",
            "sent",
        );
        for needle in [
            "relay ",
            "hook=UserPromptSubmit",
            "session_id=muse-1",
            "run_id=empty",
            "endpoint=file:77",
            "sock=/tmp/f.sock",
            "-> sent",
            &format!("pid={}", std::process::id()),
            &format!("sid={}", unsafe { libc::getsid(0) }),
        ] {
            assert!(line.contains(needle), "{needle} missing: {line}");
        }
        assert!(trace_line(b"{}", "r", "env", "", "sent").contains("run_id=set"));
    }

    #[test]
    fn rebuilt_binary_still_owns_the_endpoint() {
        let own = std::path::Path::new("/work/target/debug/forge");
        assert!(same_binary(own, own));
        // `cargo build` replaces the file under a running TUI; the kernel
        // then reports its exe link with a " (deleted)" suffix.
        assert!(
            same_binary(std::path::Path::new("/work/target/debug/forge (deleted)"), own),
            "a rebuild must not orphan scrubbed-env (muse) hooks"
        );
        // Grounded in hooks.log: the TUI ran from ~/.local/bin while muse's
        // hook command named target/debug, so every muse hook was dropped.
        assert!(
            same_binary(std::path::Path::new("/home/u/.local/bin/forge"), own),
            "an installed copy and a dev build are both forge"
        );
        assert!(!same_binary(std::path::Path::new("/usr/bin/sleep"), own));
        assert!(!same_binary(std::path::Path::new("/usr/bin/sleep (deleted)"), own));
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
            0,
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
            0,
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
