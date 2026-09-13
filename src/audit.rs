//! Append-only permission audit (Phase 3c).
//!
//! Every decided hook lands one JSON-escaped line in `audit.log`. Owner-only
//! mode is enforced on every write, repairing drift instead of trusting
//! whatever the filesystem currently says. Fields are escaped losslessly;
//! hostile display encoding stays the view layer's job at read time.

/// Append one decision line, enforcing owner-only mode on every write
/// (repairing drift). Never panics on hostile content: fields are escaped
/// losslessly and the line always ends with exactly one newline.
pub fn append(
    path: &std::path::Path,
    hook: &str,
    tool: &str,
    decision: &str,
    reason: &str,
    now_secs: u64,
) -> std::io::Result<()> {
    ensure_private(path)?;
    let line = format!(
        "{{\"ts\":{now_secs},\"hook\":\"{}\",\"tool\":\"{}\",\"decision\":\"{decision}\",\"reason\":\"{}\"}}\n",
        escape(hook),
        escape(tool),
        escape(reason),
    );
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(line.as_bytes())?;
        file.sync_all()?;
    }
    ensure_private(path)?;
    Ok(())
}

/// Enforce owner-only mode, creating parent dirs as needed. Repairs drift
/// instead of trusting current permissions.
fn ensure_private(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    if !path.exists() {
        std::fs::write(path, b"")?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// Bidi overrides and invisible format chars: encoded so a hostile tool
/// name can never spoof a human reading the log. Mirrors the display
/// encoder's flagged sets; CJK/emoji stay readable.
fn is_flagged(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{200B}'
            | '\u{200C}'
            | '\u{200D}'
            | '\u{FEFF}'
            | '\u{00AD}'
            | '\u{2060}'
    )
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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
            c if ('\u{80}'..='\u{9f}').contains(&c) => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if is_flagged(c) => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static AUDIT_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_log() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "forge-audit-test-{}-{}",
            std::process::id(),
            AUDIT_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn append_is_single_line_and_private() {
        let path = scratch_log();
        append(&path, "PreToolUse", "Bash", "deny", "block pattern", 1700000000).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains(r#""decision":"deny""#), "line: {text:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "audit mode was {mode:o}");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn hostile_fields_stay_on_one_line_losslessly() {
        let path = scratch_log();
        append(
            &path,
            "PreToolUse\x1b[2J",
            "Bash\u{202E}",
            "ask",
            "needs\napproval",
            1700000001,
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1, "no raw newline escapes");
        assert!(!text.contains('\x1b'), "no raw escape survives");
        assert!(text.contains(r#"\u001b"#), "escapes stay encoded");
        assert!(text.contains(r#"\u202e"#), "bidi stays encoded");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn drifted_permissions_are_repaired() {
        let path = scratch_log();
        std::fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        append(&path, "Stop", "", "ask", "off", 1700000002).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "drift repaired, was {mode:o}");
        }
        let _ = std::fs::remove_file(&path);
    }
}
