use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn forge() -> (Command, std::path::PathBuf) {
    // Point HOME at a fresh scratch dir so startup (migration, config load)
    // never touches the developer's real ~/.forge or ~/.ccpp.
    let mut c = Command::new(env!("CARGO_BIN_EXE_forge"));
    let home = std::env::temp_dir().join(format!(
        "forge-cli-test-{}-{}",
        std::process::id(),
        HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    c.env("HOME", &home);
    (c, home)
}

#[test]
fn version_flag_prints_version() {
    let (mut c, _home) = forge();
    let out = c.arg("--version").output().expect("run forge");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "forge 1.0.0\n");
}

#[test]
fn help_flag_prints_usage() {
    let (mut c, _home) = forge();
    let out = c.arg("--help").output().expect("run forge");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Usage: forge"), "help was: {text:?}");
}

#[test]
fn no_command_reports_missing_tui() {
    let (mut c, _home) = forge();
    let out = c.output().expect("run forge");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("TUI"), "stderr was: {err:?}");
}

#[test]
fn hook_relay_without_listener_is_silent_success() {
    use std::io::Write;
    let (mut c, _home) = forge();
    let mut child = c
        .arg("hook-relay")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run forge");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(br#"{"hook_event_name":"PreToolUse","tool":"Bash"}"#)
        .unwrap();
    let out = child.wait_with_output().expect("relay exits");
    assert!(out.status.success(), "fail-open exit 0");
    assert!(out.stdout.is_empty(), "silent without listener");
    assert!(out.stderr.is_empty(), "silent without listener");
}

#[test]
fn mcp_serve_answers_initialize_list_and_parse_error() {
    use std::io::Write;
    let (mut c, _home) = forge();
    let mut child = c
        .arg("mcp-serve")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run forge");
    let stdin = child.stdin.as_mut().unwrap();
    stdin
        .write_all(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin
        .write_all(br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#)
        .unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.write_all(b"garbage\n").unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("mcp-serve exits");
    assert!(out.status.success(), "stdio server exits 0, stderr: {:?}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let init = lines.next().unwrap_or("");
    assert!(init.contains(r#""protocolVersion":"2025-03-26""#), "init: {init}");
    let list = lines.next().unwrap_or("");
    assert!(list.contains(r#""name":"ask""#), "list: {list}");
    let err = lines.next().unwrap_or("");
    assert!(err.contains(r#""code":-32700"#), "err: {err}");
}

#[test]
fn first_launch_materializes_config_and_audit_log() {
    let (mut c, home) = forge();
    let out = c.output().expect("run forge");
    assert!(!out.status.success()); // TUI stub still exits nonzero
    let dot = home.join(".forge");
    assert!(dot.join("config.toml").is_file());
    let audit = dot.join("audit.log");
    assert!(audit.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&audit).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "audit mode was {mode:o}");
    }
    let _ = std::fs::remove_dir_all(&dot);
}
