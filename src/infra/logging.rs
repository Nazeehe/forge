//! Bounded, terminal-safe file logging.
//!
//! The application log rotates at roughly 1 MiB. Logging must never crash
//! the app: startup ignores open failures and every write is best-effort.
//! Lines are sanitized so tailing the log cannot inject control sequences.

use std::io;
use std::path::{Path, PathBuf};

/// Default rotation threshold: `forge.log` stays around 1 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

pub struct FileLogger {
    path: PathBuf,
    max_bytes: u64,
    /// Owner-only (`0600`) on create, with mode drift repaired on each
    /// append. For logs that carry private text (comms previews).
    private: bool,
}

impl FileLogger {
    pub fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(FileLogger {
            path: path.to_path_buf(),
            max_bytes,
            private: false,
        })
    }

    /// [`FileLogger::open`], but the file stays owner-only (`0600`).
    pub fn open_private(path: &Path, max_bytes: u64) -> io::Result<Self> {
        let mut log = Self::open(path, max_bytes)?;
        log.private = true;
        Ok(log)
    }

    /// Append one sanitized line (without trailing newline handling by the
    /// caller), rotating first when the file is already over budget.
    pub fn append(&mut self, line: &str) -> io::Result<()> {
        let len = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if len >= self.max_bytes {
            let rotated = self.path.with_extension("log.1");
            let _ = std::fs::remove_file(&rotated);
            std::fs::rename(&self.path, &rotated)?;
        }
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        if self.private {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(0o600);
            let mut f = opts.open(&self.path)?;
            if f.metadata()?.permissions().mode() & 0o777 != 0o600 {
                f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            writeln!(f, "{}", sanitize(line))?;
            return f.sync_all();
        }
        let mut f = opts.open(&self.path)?;
        writeln!(f, "{}", sanitize(line))?;
        f.sync_all()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

static HOOK_TRACE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Turn on hook tracing process-wide (TUI boot and each hook relay). Never
/// set under test, so tests never touch the real `~/.forge/hooks.log`.
pub fn set_hook_trace_path(path: PathBuf) {
    let _ = HOOK_TRACE.set(path);
}

pub fn hook_trace_path() -> Option<PathBuf> {
    HOOK_TRACE.get().cloned()
}

/// Append `<secs>.<millis> <line>` to the bounded hook trace. Best-effort:
/// relays and the TUI both write here, and tracing must never fail a hook.
pub fn hook_trace(path: &Path, line: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    if let Ok(mut log) = FileLogger::open(path, DEFAULT_MAX_BYTES) {
        let _ = log.append(&format!(
            "{}.{:03} {line}",
            now.as_secs(),
            now.subsec_millis()
        ));
    }
}

/// Trace to the process-wide hook log, when one is set.
pub fn hook_trace_global(line: &str) {
    if let Some(path) = HOOK_TRACE.get() {
        hook_trace(path, line);
    }
}

static COMMS_TRACE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Turn on comms tracing process-wide (TUI boot and each comms relay).
/// Never set under test, so tests never touch the real `~/.forge/comms.log`.
pub fn set_comms_trace_path(path: PathBuf) {
    let _ = COMMS_TRACE.set(path);
}

pub fn comms_trace_path() -> Option<PathBuf> {
    COMMS_TRACE.get().cloned()
}

/// Append `<secs>.<millis> <line>` to the bounded comms trace.
/// Best-effort: broker calls and delivery ticks both write here, and
/// tracing must never fail or block a send. Lines are sanitized on
/// write (see [`FileLogger::append`]); keep each call to one line and
/// pre-truncate message bodies with [`truncate`].
pub fn comms_trace(path: &Path, line: &str) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    if let Ok(mut log) = FileLogger::open_private(path, DEFAULT_MAX_BYTES) {
        let _ = log.append(&format!(
            "{}.{:03} {line}",
            now.as_secs(),
            now.subsec_millis()
        ));
    }
}

/// Trace to the process-wide comms log, when one is set.
pub fn comms_trace_global(line: &str) {
    if let Some(path) = COMMS_TRACE.get() {
        comms_trace(path, line);
    }
}

/// Shorten `s` to at most `max_chars` characters at a char boundary,
/// marking the cut so a truncated preview never reads as the full
/// text. Multi-byte chars count as one and never split.
pub fn truncate(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for c in s.chars() {
        if count >= max_chars {
            break;
        }
        out.push(c);
        count += 1;
    }
    let total: usize = s.chars().count();
    if total > max_chars {
        out.push_str(&format!("...+{}more", total - max_chars));
    }
    out
}

/// Strip control characters and anything that could drive a terminal when
/// the log is tailed. Newlines become spaces; other C0/C1 controls are
/// dropped; DEL is dropped.
pub fn sanitize(line: &str) -> String {
    line.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-log-test-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("forge.log")
    }

    #[test]
    fn appends_lines_readable_back() {
        let path = scratch();
        let mut log = FileLogger::open(&path, DEFAULT_MAX_BYTES).unwrap();
        log.append("hello").unwrap();
        log.append("world").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("hello\n"));
        assert!(text.contains("world\n"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rotates_at_budget() {
        let path = scratch();
        let mut log = FileLogger::open(&path, 32).unwrap();
        for i in 0..20 {
            log.append(&format!("line {i:02} padding padding")).unwrap();
        }
        let rotated = path.with_extension("log.1");
        assert!(rotated.is_file(), "expected rotation sidecar");
        let main = std::fs::metadata(&path).unwrap().len();
        assert!(main <= 256, "main log bounded, was {main}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn hook_trace_appends_timestamped_lines() {
        let path = scratch();
        hook_trace(&path, "relay hook=Stop");
        hook_trace(&path, "tui hook=Stop");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let (ts, rest) = lines[0].split_once(' ').unwrap();
        let (secs, millis) = ts.split_once('.').expect("secs.millis stamp");
        assert!(secs.parse::<u64>().unwrap() > 1_700_000_000, "{ts}");
        assert_eq!(millis.len(), 3, "{ts}");
        assert_eq!(rest, "relay hook=Stop");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sanitize_kills_controls() {
        assert_eq!(sanitize("a\nb\rc\td"), "a b c d");
        assert_eq!(sanitize("x\x00y\x1bz"), "xyz");
        assert_eq!(sanitize("ok ✓"), "ok ✓");
    }

    #[test]
    fn comms_trace_is_owner_only() {
        // The trace carries message-body previews: private like audit.log,
        // both on create and when an older world-readable file exists.
        use std::os::unix::fs::PermissionsExt;
        let path = scratch();
        comms_trace(&path, "comms start");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "created mode was {mode:o}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        comms_trace(&path, "comms tool=tell_session -> ok");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "drift repaired, was {mode:o}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn comms_trace_appends_timestamped_lines() {
        let path = scratch();
        comms_trace(&path, "comms tool=tell_session -> ok");
        comms_trace(&path, "comms tool=ask_session -> err");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let (ts, rest) = lines[0].split_once(' ').unwrap();
        let (secs, millis) = ts.split_once('.').expect("secs.millis stamp");
        assert!(secs.parse::<u64>().unwrap() > 1_700_000_000, "{ts}");
        assert_eq!(millis.len(), 3, "{ts}");
        assert_eq!(rest, "comms tool=tell_session -> ok");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn truncate_bounds_at_char_boundary_with_marker() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hello...+6more");
        assert_eq!(truncate("", 5), "");
        // Multi-byte chars count as one char and never split.
        assert_eq!(truncate("ok ✓✓✓", 4), "ok ✓...+2more");
    }
}
