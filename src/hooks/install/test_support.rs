//! Shared installer test fixtures: scratch homes and env pins.

use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn scratch_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "forge-install-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Pin codex paths to the scratch home even when the developer exports
/// CODEX_HOME. Restores on drop so panics cannot leak env changes.
/// Holding the lock serializes every CODEX_HOME toucher: the variable
/// is process-global, so parallel setters would otherwise cross-read.
pub(super) struct ClearCodexHome {
    saved: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl ClearCodexHome {
    pub(super) fn pin() -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("CODEX_HOME").ok();
        std::env::remove_var("CODEX_HOME");
        ClearCodexHome {
            saved,
            _lock: lock,
        }
    }
}

impl Drop for ClearCodexHome {
    fn drop(&mut self) {
        if let Some(v) = self.saved.take() {
            std::env::set_var("CODEX_HOME", v);
        }
    }
}

pub(super) const FORGE_BIN: &str = "/tmp/forge-under-test";
