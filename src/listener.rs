//! TUI IPC listener (Phase 3b): Unix socket + loopback TCP.
//!
//! Short-lived harnesses (`hook-relay`, later MCP) connect, send one header
//! line, and — for synchronous hooks — wait for a one-line decision. The
//! first line is read byte-at-a-time with no buffered prefetch, because
//! binary traffic may follow it. Connections beyond the per-transport cap
//! are closed immediately. Hook records arrive on the main loop as
//! `AppEvent::HookRequest`; policy replies every verdict immediately
//! later, so an undecided synchronous hook simply waits out the relay's
//! own timeout and fails open.

/// Connections accepted per transport before newcomers are turned away.
pub const MAX_CONNS: usize = 32;

/// Longest accepted header line; longer means a broken or hostile client.
pub const MAX_LINE: usize = 65536;

/// Pending hook queue bound; beyond it newcomers are dropped (their relays
/// fail open on timeout) rather than growing memory without limit.
pub const MAX_PENDING_HOOKS: usize = 128;

/// How long a connection handler waits for the loop's decision before
/// giving up. The relay enforces its own shorter deadline; this only bounds
/// handler threads once policy exists.
pub const REPLY_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// First-line routing: hook records and comms calls go to the loop;
/// framed visual/file traffic is deferred to its own phase.
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Hook { hook: String },
    Comms,
    Other,
}

/// One hook record awaiting a decision. Policy answers through `reply`;
/// dropping it fails the relay open.
/// Asynchronous hooks never wait: the handler closes right after delivery.
#[derive(Debug)]
pub struct HookRequest {
    pub hook: String,
    pub body: String,
    /// Envelope sender for session attribution; empty when unset.
    pub run_id: String,
    pub sync: bool,
    pub reply: std::sync::mpsc::Sender<String>,
    /// Set by the connection handler when the caller stops waiting: the
    /// verdict went out as a timeout-deny; a later settle reply to the
    /// same request lands nowhere. Shared (not copied) with the queued
    /// request.
    pub timed_out: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// One comms tool call for the broker (4c). Always synchronous: the loop
/// answers at once and the handler relays the one-line verdict.
#[derive(Debug)]
pub struct CommsRequest {
    pub run_id: String,
    pub tool: String,
    pub args: String,
    pub reply: std::sync::mpsc::Sender<String>,
    /// Set by the connection handler when the caller stops waiting: the
    /// caller already holds a timeout verdict, so `apply` must not run
    /// the send for nobody. Shared (not copied) with the handler.
    pub timed_out: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// One external bot call for the broker. Identity travels as the
/// operator-issued credential (never a run ID); the loop answers at
/// once and the handler relays the one-line verdict.
#[derive(Debug)]
pub struct BotRequest {
    pub name: String,
    pub token: String,
    pub tool: String,
    pub args: String,
    pub reply: std::sync::mpsc::Sender<String>,
}

/// Parsed bot envelope parts: credential plus the tool call. Missing
/// args default downstream; a missing tool routes to `not_found`.
#[derive(Debug, PartialEq, Eq)]
pub struct BotParts {
    pub name: String,
    pub token: String,
    pub tool: String,
    pub args: String,
}

/// Parse the bot object out of a comms envelope. `None` when no bot
/// object rides along (a harness call, not a bot call).
pub fn bot_parts(text: &str) -> Option<BotParts> {
    let bot = crate::mcp::top_raw(text, "bot")?;
    let name = crate::mcp::top_str(bot, "name").filter(|s| !s.is_empty())?;
    // The credential forwards verbatim: absent reads as failed auth at
    // the broker, never as a second parse error (no oracle either way).
    let token = crate::mcp::top_str(bot, "token").unwrap_or_default();
    let tool = crate::mcp::top_str(text, "tool").unwrap_or_default();
    let args = crate::mcp::top_raw(text, "args").unwrap_or("{}").to_string();
    Some(BotParts {
        name,
        token,
        tool,
        args,
    })
}

/// Whether a pinned instance routes here: pins are exact process IDs
/// and presence is enforced upstream, so only equality routes. Zero
/// never matches, not even itself.
pub fn instance_pinned_ok(pinned: u32, own: u32) -> bool {
    pinned != 0 && pinned == own
}

/// Classify one header line. Comms envelopes match first on their parsed
/// top-level `kind` so a hook body mentioning comms can never misroute;
/// hook envelopes then match as before.
pub fn classify(line: &[u8]) -> Route {
    if let Ok(text) = std::str::from_utf8(line) {
        if crate::mcp::top_str(text, "kind").as_deref() == Some("comms") {
            return Route::Comms;
        }
    }
    if let Some(hook) = crate::relay::hook_name(line) {
        // `hook_name` also matches nested fields; require the envelope shape
        // so a JSON-RPC body mentioning hooks cannot misroute.
        if line.windows(6).any(|w| w == b"\"body\"") {
            return Route::Hook { hook };
        }
    }
    Route::Other
}

/// Read one `\n`-terminated header line byte-at-a-time: no buffered
/// prefetch, so binary bytes after the newline stay in the stream for the
/// next reader. Errors on EOF before newline or lines beyond `cap`.
pub fn read_record_line(
    stream: &mut impl std::io::Read,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) => {
                line.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(line);
                }
                if line.len() > cap {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "header line too long",
                    ));
                }
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof before newline",
                ));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Bind a Unix socket with owner-only permissions regardless of umask. A
/// leftover file from a dead process is removed and retried once.
pub fn bind_unix(path: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixListener> {
    let bound = std::os::unix::net::UnixListener::bind(path).or_else(|_| {
        let _ = std::fs::remove_file(path);
        std::os::unix::net::UnixListener::bind(path)
    })?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(bound)
}

/// Accept-loop guard: removing the socket file on drop. Handler threads are
/// detached; process exit reaps them.
pub struct ListenerGuard {
    sock_path: Option<std::path::PathBuf>,
}

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        if let Some(path) = self.sock_path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Spawn the Unix-socket accept loop with a connection cap. Each hook record
/// arrives on the loop as `AppEvent::HookRequest`.
pub fn spawn_unix(
    path: &std::path::Path,
    tx: std::sync::mpsc::Sender<crate::event::AppEvent>,
    cap: usize,
) -> std::io::Result<ListenerGuard> {
    let listener = bind_unix(path)?;
    accept_loop(listener, tx, cap);
    Ok(ListenerGuard {
        sock_path: Some(path.to_path_buf()),
    })
}

/// Spawn the loopback-TCP accept loop on a dynamic port (for reverse
/// forwarding): same protocol and cap as the Unix socket.
pub fn spawn_tcp(
    tx: std::sync::mpsc::Sender<crate::event::AppEvent>,
    cap: usize,
) -> std::io::Result<(ListenerGuard, u16)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    accept_loop(listener, tx, cap);
    Ok((ListenerGuard { sock_path: None }, port))
}

/// Spawn both transports with the production cap and a pid-namespaced
/// socket path. Never fatal to the TUI: hooks fail open without it.
pub fn spawn_all(
    tx: std::sync::mpsc::Sender<crate::event::AppEvent>,
) -> std::io::Result<Spawned> {
    let path = std::env::temp_dir().join(format!("forge.{}.sock", std::process::id()));
    let guard = spawn_unix(&path, tx.clone(), MAX_CONNS)?;
    let (_tcp_guard, tcp_port) = spawn_tcp(tx, MAX_CONNS)?;
    // The TCP guard needs no cleanup; keep it alive with the return value.
    Ok(Spawned {
        _guard: guard,
        _tcp_guard,
        sock_path: path,
        tcp_port,
    })
}

/// Handles to a live listener pair.
pub struct Spawned {
    _guard: ListenerGuard,
    _tcp_guard: ListenerGuard,
    /// Unix socket path, also advertised as `FORGE_IPC_ENDPOINT`.
    pub sock_path: std::path::PathBuf,
    /// Dynamic loopback TCP port for reverse forwarding.
    pub tcp_port: u16,
}

/// Something that can hand out accepted streams from a moved-in listener.
trait Acceptor: Send + Sync + 'static {
    type Stream: std::io::Read + std::io::Write + Send + 'static;
    fn accept(&self) -> std::io::Result<Self::Stream>;
}

impl Acceptor for std::os::unix::net::UnixListener {
    type Stream = std::os::unix::net::UnixStream;
    fn accept(&self) -> std::io::Result<Self::Stream> {
        self.accept().map(|(conn, _)| conn)
    }
}

impl Acceptor for std::net::TcpListener {
    type Stream = std::net::TcpStream;
    fn accept(&self) -> std::io::Result<Self::Stream> {
        self.accept().map(|(conn, _)| conn)
    }
}

fn accept_loop<A: Acceptor>(
    listener: A,
    tx: std::sync::mpsc::Sender<crate::event::AppEvent>,
    cap: usize,
) {
    let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::spawn(move || loop {
        let conn = match listener.accept() {
            Ok(conn) => conn,
            Err(_) => continue,
        };
        let n = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if n > cap {
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            drop(conn);
            continue;
        }
        let tx = tx.clone();
        let active = std::sync::Arc::clone(&active);
        std::thread::spawn(move || {
            handle_conn(conn, &tx);
            active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    });
}

fn handle_conn<S: std::io::Read + std::io::Write>(
    mut conn: S,
    tx: &std::sync::mpsc::Sender<crate::event::AppEvent>,
) {
    let line = match read_record_line(&mut conn, MAX_LINE) {
        Ok(line) => line,
        Err(_) => return,
    };
    match classify(&line) {
        Route::Hook { hook } => handle_hook(conn, &line, hook, tx),
        Route::Comms => handle_comms(conn, &line, tx),
        Route::Other => {}
    }
}

fn handle_hook<S: std::io::Read + std::io::Write>(
    mut conn: S,
    line: &[u8],
    hook: String,
    tx: &std::sync::mpsc::Sender<crate::event::AppEvent>,
) {
    let body = String::from_utf8_lossy(line).into_owned();
    let run_id = crate::mcp::top_str(&body, "run_id").unwrap_or_default();
    // Records resolved through the endpoint file carry their owner's pid;
    // another live instance's relays must not land in this loop.
    let forge_pid: u32 = crate::mcp::top_raw(&body, "forge_pid")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(0);
    if forge_pid != 0 && forge_pid != std::process::id() {
        return;
    }
    let sync = crate::relay::is_sync_hook(&hook);
    // The timeout-deny below still needs the name after the move.
    let hook_for_timeout = hook.clone();
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if tx
        .send(crate::event::AppEvent::HookRequest(HookRequest {
            hook,
            body,
            run_id,
            sync,
            reply: reply_tx,
            timed_out: std::sync::Arc::clone(&timed_out),
        }))
        .is_err()
    {
        return;
    }
    if !sync {
        return;
    }
    let decision = match reply_rx.recv_timeout(REPLY_WAIT) {
        Ok(decision) => decision,
        // Operator timeout (listener alive, TUI silent): fail CLOSED so a
        // wedged loop cannot silently allow. Infra failure (no listener
        // at all) stays fail-open upstream. The queued request is flagged
        // so any late reply is known-steered-nowhere.
        Err(_) => {
            timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
            crate::policy::decision_line(
                &hook_for_timeout,
                crate::policy::Decision::Deny,
                "operator timeout; denied",
            )
        }
    };
    {
        use std::io::Write;
        let mut bytes = decision.into_bytes();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let _ = conn.write_all(&bytes);
    }
}

/// Deliver one comms call to the broker and relay its one-line verdict.
/// Malformed records are dropped: the caller's own timeout reports them.
fn handle_comms<S: std::io::Read + std::io::Write>(
    mut conn: S,
    line: &[u8],
    tx: &std::sync::mpsc::Sender<crate::event::AppEvent>,
) {
    let text = String::from_utf8_lossy(line).into_owned();
    if crate::mcp::top_raw(&text, "bot").is_some() {
        return handle_bot(conn, &text, tx);
    }
    let (Some(run_id), Some(tool)) = (
        crate::mcp::top_str(&text, "run_id"),
        crate::mcp::top_str(&text, "tool"),
    ) else {
        return;
    };
    let args = crate::mcp::top_raw(&text, "args").unwrap_or("{}").to_string();
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let timed_out = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if tx
        .send(crate::event::AppEvent::CommsRequest(CommsRequest {
            run_id,
            tool,
            args,
            reply: reply_tx,
            timed_out: std::sync::Arc::clone(&timed_out),
        }))
        .is_err()
    {
        return;
    }
    if let Ok(verdict) = reply_rx.recv_timeout(REPLY_WAIT) {
        use std::io::Write;
        let mut bytes = verdict.into_bytes();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let _ = conn.write_all(&bytes);
    } else {
        // The caller already holds its own timeout verdict; flag the
        // queued request so a late `apply` skips the send instead of
        // running it for nobody (same shape as hook timeout-deny).
        timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Deliver one bot call to the broker and relay its one-line verdict.
/// A foreign instance pin is refused with `unavailable` before any
/// event exists; malformed envelopes fail closed the same way.
/// One fail-closed refusal line without touching the loop.
fn refuse_bot<S: std::io::Write>(
    conn: &mut S,
    code: crate::bot::ErrorCode,
    message: &str,
) {
    let mut line = String::from("{\"ok\":false,\"error\":");
    line.push_str(&crate::bot::BotError::new(code, message).to_json());
    line.push_str("}\n");
    let _ = conn.write_all(line.as_bytes());
}

fn handle_bot<S: std::io::Read + std::io::Write>(
    mut conn: S,
    text: &str,
    tx: &std::sync::mpsc::Sender<crate::event::AppEvent>,
) {
    let Some(parts) = bot_parts(text) else {
        refuse_bot(
            &mut conn,
            crate::bot::ErrorCode::InvalidArguments,
            "bot envelope needs bot.name and a tool",
        );
        return;
    };
    // Every bot request pins its instance: a missing, zero, or
    // unparsable pin is malformed, a well-formed foreign pin routes
    // nowhere and refuses as unavailable.
    let pinned = crate::mcp::top_raw(text, "forge_pid")
        .map(|r| r.trim().trim_matches('"').to_string())
        .and_then(|r| r.parse::<u32>().ok())
        .filter(|p| *p > 0);
    let Some(pinned) = pinned else {
        refuse_bot(
            &mut conn,
            crate::bot::ErrorCode::InvalidArguments,
            "bot envelope needs a positive forge_pid",
        );
        return;
    };
    if !instance_pinned_ok(pinned, std::process::id()) {
        refuse_bot(
            &mut conn,
            crate::bot::ErrorCode::Unavailable,
            "wrong forge instance",
        );
        return;
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if tx
        .send(crate::event::AppEvent::BotRequest(BotRequest {
            name: parts.name,
            token: parts.token,
            tool: parts.tool,
            args: parts.args,
            reply: reply_tx,
        }))
        .is_err()
    {
        return;
    }
    if let Ok(verdict) = reply_rx.recv_timeout(REPLY_WAIT) {
        use std::io::Write;
        let mut bytes = verdict.into_bytes();
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        let _ = conn.write_all(&bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn first_line_leaves_trailer_unread() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        b.write_all(b"line1\nBINARY\x00trailer").unwrap();
        drop(b);
        let line = read_record_line(&mut a, MAX_LINE).unwrap();
        assert_eq!(line, b"line1\n");
        let mut rest = Vec::new();
        a.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b"BINARY\x00trailer");
    }

    #[test]
    fn overlong_line_is_rejected() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        b.write_all(&vec![b'x'; MAX_LINE + 1]).unwrap();
        drop(b);
        assert!(read_record_line(&mut a, MAX_LINE).is_err());
    }

    #[test]
    fn eof_without_newline_is_rejected() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        b.write_all(b"partial").unwrap();
        drop(b);
        assert!(read_record_line(&mut a, MAX_LINE).is_err());
    }

    #[test]
    fn header_routes_hooks_and_defers_the_rest() {
        match classify(b"{\"v\":1,\"hook\":\"PreToolUse\",\"body\":{}}\n") {
            Route::Hook { hook } => assert_eq!(hook, "PreToolUse"),
            Route::Other | Route::Comms => panic!("hook misrouted"),
        }
        assert!(matches!(
            classify(b"{\"jsonrpc\":\"2.0\",\"method\":\"x\"}\n"),
            Route::Other
        ));
        assert!(matches!(classify(b"garbage\n"), Route::Other));
    }

    #[test]
    fn unix_socket_is_owner_only() {
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-mode.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let _listener = bind_unix(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "socket mode was {mode:o}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hook_record_reaches_the_loop_with_a_reply_path() {
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-hook.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let _guard = spawn_unix(&path, tx, 8).unwrap();
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"v\":1,\"hook\":\"PreToolUse\",\"run_id\":\"run-9\",\"body\":{\"tool\":\"Bash\"}}\n")
            .unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("hook event arrives");
        match event {
            crate::event::AppEvent::HookRequest(req) => {
                assert_eq!(req.hook, "PreToolUse");
                assert_eq!(req.run_id, "run-9", "envelope attributes the sender");
                assert!(req.sync, "PreToolUse waits for a decision");
                assert!(req.body.contains("Bash"), "body carried: {:?}", req.body);
                req.reply.send("{\"decision\":\"allow\"}\n".to_string()).unwrap();
            }
            other => panic!("wrong event: {other:?}"),
        }
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        loop {
            match conn.read(&mut byte) {
                Ok(1) => {
                    out.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                _ => break,
            }
        }
        assert_eq!(out, b"{\"decision\":\"allow\"}\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn foreign_instance_records_never_reach_the_loop() {
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-pid.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let _guard = spawn_unix(&path, tx, 8).unwrap();
        // Another instance's relay: dropped before an event exists.
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":424242,\"body\":{}}\n")
            .unwrap();
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "foreign record dropped"
        );
        // Own pid (or untagged legacy records) still arrive.
        let mut conn = UnixStream::connect(&path).unwrap();
        let own = std::process::id();
        let line = format!(
            "{{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"forge_pid\":{own},\"body\":{{}}}}\n"
        );
        conn.write_all(line.as_bytes()).unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("own record arrives");
        assert!(
            matches!(event, crate::event::AppEvent::HookRequest(_)),
            "own record kept"
        );
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"v\":1,\"hook\":\"Stop\",\"run_id\":\"\",\"body\":{}}\n")
            .unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("untagged record arrives");
        assert!(
            matches!(event, crate::event::AppEvent::HookRequest(_)),
            "legacy record kept"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unanswered_sync_hook_fails_closed_with_deny() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-timeout.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let _guard = spawn_unix(&path, tx, 8).unwrap();
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"v\":1,\"hook\":\"PreToolUse\",\"body\":{\"tool\":\"Bash\"}}\n")
            .unwrap();
        // Never answer: the handler must deny on its own after REPLY_WAIT
        // instead of leaving the caller hanging (or silently allowing).
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("hook event arrives");
        let crate::event::AppEvent::HookRequest(req) = event else {
            panic!("wrong event");
        };
        conn.set_read_timeout(Some(super::REPLY_WAIT + std::time::Duration::from_secs(5)))
            .unwrap();
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            use std::io::Read;
            match conn.read(&mut byte) {
                Ok(1) => {
                    out.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                _ => break,
            }
        }
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(r#""permissionDecision":"deny""#),
            "fail closed: {text:?}"
        );
        assert!(
            text.contains("operator timeout"),
            "reason rides along: {text:?}"
        );
        assert!(
            req.timed_out.load(std::sync::atomic::Ordering::SeqCst),
            "queued request flagged stale"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tcp_hook_record_reaches_the_loop() {
        use std::io::Write;
        use std::net::TcpStream;
        let (tx, rx) = std::sync::mpsc::channel();
        let (_guard, port) = spawn_tcp(tx, 8).unwrap();
        let mut conn = TcpStream::connect(("127.0.0.1", port)).unwrap();
        conn.write_all(b"{\"v\":1,\"hook\":\"Stop\",\"body\":{}}\n").unwrap();
        match rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("hook event arrives over tcp")
        {
            crate::event::AppEvent::HookRequest(req) => {
                assert_eq!(req.hook, "Stop");
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn async_hooks_close_without_waiting() {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-async.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let _guard = spawn_unix(&path, tx, 8).unwrap();
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"v\":1,\"hook\":\"Stop\",\"body\":{}}\n").unwrap();
        match rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("async event still delivered")
        {
            crate::event::AppEvent::HookRequest(req) => {
                assert!(!req.sync, "Stop never waits");
            }
            other => panic!("wrong event: {other:?}"),
        }
        conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 1];
        use std::io::Read;
        assert!(matches!(conn.read(&mut buf), Ok(0)), "handler closed promptly");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn classify_routes_comms_envelopes() {
        assert!(matches!(
            classify(b"{\"v\":1,\"kind\":\"comms\",\"run_id\":\"x\",\"tool\":\"ask\",\"args\":{}}\n"),
            Route::Comms
        ));
        // A hook body mentioning comms must not misroute.
        assert!(matches!(
            classify(b"{\"v\":1,\"hook\":\"PreToolUse\",\"body\":{\"tool\":\"Bash\",\"text\":\"kind comms\"}}\n"),
            Route::Hook { .. }
        ));
        assert!(matches!(classify(b"garbage\n"), Route::Other));
    }

    #[test]
    fn comms_record_reaches_the_loop_with_a_reply_path() {
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-comms.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        let _guard = spawn_unix(&path, tx, 8).unwrap();
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.write_all(b"{\"v\":1,\"kind\":\"comms\",\"run_id\":\"abc\",\"tool\":\"list_sessions\",\"args\":{}}\n")
            .unwrap();
        let event = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("comms event arrives");
        match event {
            crate::event::AppEvent::CommsRequest(req) => {
                assert_eq!(req.run_id, "abc");
                assert_eq!(req.tool, "list_sessions");
                assert_eq!(req.args, "{}");
                req.reply.send("{\"ok\":true,\"result\":{}}\n".to_string()).unwrap();
            }
            other => panic!("wrong event: {other:?}"),
        }
        let mut out = Vec::new();
        let mut byte = [0u8; 1];
        conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        loop {
            match conn.read(&mut byte) {
                Ok(1) => {
                    out.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                _ => break,
            }
        }
        assert_eq!(out, b"{\"ok\":true,\"result\":{}}\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bot_envelope_parses_credential_and_call() {
        let line = r#"{"v":1,"kind":"comms","run_id":"","forge_pid":0,"tool":"bot_poll","args":{"cursor":3},"bot":{"name":"skippy","token":"tok-1"}}"#;
        let parts = bot_parts(line).expect("bot envelope parses");
        assert_eq!(parts.name, "skippy");
        assert_eq!(parts.token, "tok-1");
        assert_eq!(parts.tool, "bot_poll");
        assert!(parts.args.contains("cursor"), "args: {:?}", parts.args);
        assert!(
            bot_parts(r#"{"v":1,"kind":"comms","run_id":"abc","tool":"list_sessions","args":{}}"#)
                .is_none(),
            "harness envelopes carry no bot parts"
        );
        assert!(
            bot_parts(r#"{"v":1,"kind":"comms","bot":{"token":"tok-only"}}"#).is_none(),
            "nameless envelopes parse nothing"
        );
        assert!(
            bot_parts(r#"{"v":1,"kind":"comms","bot":{"name":"","token":"tok-only"}}"#).is_none(),
            "empty names parse nothing"
        );
    }

    #[test]
    fn instance_pin_requires_an_exact_match() {
        assert!(instance_pinned_ok(4242, 4242), "own pin routes here");
        assert!(!instance_pinned_ok(424241, 424242), "foreign pin refused");
        assert!(!instance_pinned_ok(0, 0), "absence never matches, even itself");
    }

    /// In-memory stand-in for a socket: writes are captured, reads hit
    /// immediate EOF. Zero syscalls, so these tests run anywhere —
    /// including sandboxes that deny socket IO outright.
    #[derive(Clone, Default)]
    struct MemConn {
        written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl std::io::Read for MemConn {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }

    impl std::io::Write for MemConn {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Poll captured bytes until the fragment lands or time runs out.
    fn await_text(reader: &MemConn, fragment: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let text =
                String::from_utf8_lossy(&reader.written.lock().unwrap()).into_owned();
            if text.contains(fragment) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no verdict relayed: {text:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn bot_record_reaches_the_loop_with_a_reply_path() {
        let conn = MemConn::default();
        let reader = conn.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let own = std::process::id();
        let line = format!(
            "{{\"v\":1,\"kind\":\"comms\",\"run_id\":\"\",\"forge_pid\":{own},\"tool\":\"bot_poll\",\"args\":{{\"cursor\":1}},\"bot\":{{\"name\":\"skippy\",\"token\":\"tok-1\"}}}}"
        );
        std::thread::spawn(move || handle_bot(conn, &line, &tx));
        match rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("bot event arrives")
        {
            crate::event::AppEvent::BotRequest(req) => {
                assert_eq!(req.name, "skippy");
                assert_eq!(req.token, "tok-1");
                assert_eq!(req.tool, "bot_poll");
                assert!(req.args.contains("cursor"), "args: {:?}", req.args);
                req.reply.send("{\"ok\":true,\"result\":{}}\n".to_string()).unwrap();
            }
            other => panic!("wrong event: {other:?}"),
        }
        await_text(&reader, "\"ok\":true");
    }

    #[test]
    fn foreign_instance_bot_record_is_refused_without_an_event() {
        // Refusals write synchronously without touching the loop: no
        // thread needed, fully deterministic.
        let conn = MemConn::default();
        let reader = conn.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let line = "{\"v\":1,\"kind\":\"comms\",\"forge_pid\":424242,\"tool\":\"bot_poll\",\"args\":{},\"bot\":{\"name\":\"skippy\",\"token\":\"tok-1\"}}";
        handle_bot(conn, line, &tx);
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
            "foreign pin emits no event"
        );
        await_text(&reader, "\"unavailable\"");
    }

    #[test]
    fn bot_envelope_without_a_usable_pin_is_refused() {
        for line in [
            "{\"v\":1,\"kind\":\"comms\",\"tool\":\"bot_poll\",\"args\":{},\"bot\":{\"name\":\"skippy\",\"token\":\"tok-1\"}}",
            "{\"v\":1,\"kind\":\"comms\",\"forge_pid\":\"soon\",\"tool\":\"bot_poll\",\"args\":{},\"bot\":{\"name\":\"skippy\",\"token\":\"tok-1\"}}",
            "{\"v\":1,\"kind\":\"comms\",\"forge_pid\":0,\"tool\":\"bot_poll\",\"args\":{},\"bot\":{\"name\":\"skippy\",\"token\":\"tok-1\"}}",
        ] {
            let conn = MemConn::default();
            let reader = conn.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            handle_bot(conn, line, &tx);
            assert!(
                rx.recv_timeout(std::time::Duration::from_millis(300)).is_err(),
                "no event for unusable pin: {line}"
            );
            let text =
                String::from_utf8_lossy(&reader.written.lock().unwrap()).into_owned();
            assert!(
                text.contains("\"invalid_arguments\""),
                "fail closed: {text:?}"
            );
        }
    }

    #[test]
    fn connections_beyond_cap_are_turned_away() {
        let path = std::env::temp_dir().join(format!(
            "forge-listen-test-{}-cap.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, _rx) = std::sync::mpsc::channel();
        let _guard = spawn_unix(&path, tx, 2).unwrap();
        let held: Vec<UnixStream> = (0..2).map(|_| UnixStream::connect(&path).unwrap()).collect();
        // Give the accept loop a beat to count both holders.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut extra = UnixStream::connect(&path).unwrap();
            extra
                .set_read_timeout(Some(std::time::Duration::from_millis(200)))
                .unwrap();
            let mut buf = [0u8; 1];
            use std::io::Read;
            match extra.read(&mut buf) {
                // Closed promptly: over cap.
                Ok(0) => break,
                _ => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "extra connection was admitted over cap"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        }
        drop(held);
        let _ = std::fs::remove_file(&path);
    }
}
