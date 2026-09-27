//! Atomic, durable file writes.
//!
//! Important files are written to a temp sibling, fsynced, renamed over the
//! target, then the directory is fsynced — so a crash can never leave a torn
//! file behind. Private files additionally force `0600` before the rename so
//! secrets are never briefly world-readable.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically with default permissions.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_staged(path, bytes, false)
}

/// Write `bytes` to `path` atomically and force owner-only (`0600`) mode.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_staged(path, bytes, true)
}

fn write_staged(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let stage = stage_path(path);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&stage)?;
        if private {
            // Applied while the staging file is still empty, so secrets are
            // never briefly readable through wider default permissions.
            #[cfg(unix)]
            std::fs::set_permissions(
                &stage,
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&stage, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn stage_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".tmp-{}-{}-{name}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-atomic-test-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip_and_overwrite() {
        let dir = scratch_dir();
        let f = dir.join("data.txt");
        write_atomic(&f, b"one").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"one");
        write_atomic(&f, b"two-longer").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"two-longer");
        // No staging files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_forces_owner_only() {
        let dir = scratch_dir();
        let f = dir.join("secret.json");
        write_private(&f, b"{\"k\":\"v\"}").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"{\"k\":\"v\"}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "mode was {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn io_errors_propagate_as_err() {
        let dir = scratch_dir();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        // Parent is a file, so directory creation must fail — no panic, an Err.
        assert!(write_atomic(&blocker.join("f.txt"), b"x").is_err());
        assert!(write_private(&blocker.join("g.txt"), b"x").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_parent_is_created() {
        let dir = scratch_dir();
        let f = dir.join("sub").join("deep").join("f.txt");
        write_atomic(&f, b"x").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
