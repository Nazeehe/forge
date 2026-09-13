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
        })
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
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{}", sanitize(line))?;
        f.sync_all()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
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
    fn sanitize_kills_controls() {
        assert_eq!(sanitize("a\nb\rc\td"), "a b c d");
        assert_eq!(sanitize("x\x00y\x1bz"), "xyz");
        assert_eq!(sanitize("ok ✓"), "ok ✓");
    }
}
