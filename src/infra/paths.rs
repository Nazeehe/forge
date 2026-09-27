//! Relative-path confinement for untrusted paths.
//!
//! File transfer, walkthrough and project paths all resolve caller-supplied
//! relative paths under a trusted base. Anything absolute, escaping, empty or
//! routed through a symlink is rejected before any I/O happens.

use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    Absolute,
    Empty,
    EscapesBase,
    SymlinkComponent(PathBuf),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Absolute => write!(f, "path must be relative"),
            PathError::Empty => write!(f, "path must not be empty"),
            PathError::EscapesBase => write!(f, "path escapes its base directory"),
            PathError::SymlinkComponent(p) => {
                write!(f, "path routes through symlink: {}", p.display())
            }
        }
    }
}

impl std::error::Error for PathError {}

/// Resolve `rel` under `base`, rejecting absolute paths, escapes above `base`
/// and symlink components. Returns the confined absolute-ish joined path
/// (lexically normalized; `base` itself is trusted, not symlink-checked).
pub fn confine(base: &Path, rel: &Path) -> Result<PathBuf, PathError> {
    if rel.is_absolute() {
        return Err(PathError::Absolute);
    }
    if rel.as_os_str().is_empty() {
        return Err(PathError::Empty);
    }
    let mut out = base.to_path_buf();
    for comp in rel.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => return Err(PathError::Absolute),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(PathError::EscapesBase);
                }
            }
            Component::Normal(c) => out.push(c),
        }
    }
    if out != *base && !out.starts_with(base) {
        return Err(PathError::EscapesBase);
    }
    // Best-effort symlink check on each component below `base`. Missing
    // components are fine (the file may not exist yet); confirmed symlinks
    // are rejected. Later phases additionally open with O_NOFOLLOW, since
    // any check-then-use sequence is inherently racy.
    let mut acc = base.to_path_buf();
    let tail = out.strip_prefix(base).expect("confined path under base");
    for comp in tail.components() {
        if let Component::Normal(c) = comp {
            acc.push(c);
            match std::fs::symlink_metadata(&acc) {
                Ok(md) if md.file_type().is_symlink() => {
                    return Err(PathError::SymlinkComponent(acc));
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-path-test-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accepts_plain_relative() {
        let base = Path::new("/srv/forge");
        assert_eq!(
            confine(base, Path::new("a/b.txt")).unwrap(),
            PathBuf::from("/srv/forge/a/b.txt")
        );
    }

    #[test]
    fn rejects_absolute_and_empty() {
        let base = Path::new("/srv/forge");
        assert_eq!(confine(base, Path::new("/etc/passwd")).unwrap_err(), PathError::Absolute);
        assert_eq!(confine(base, Path::new("")).unwrap_err(), PathError::Empty);
    }

    #[test]
    fn rejects_escape_but_allows_inner_dotdot() {
        let base = Path::new("/srv/forge");
        assert_eq!(confine(base, Path::new("..")).unwrap_err(), PathError::EscapesBase);
        assert_eq!(
            confine(base, Path::new("a/../../x")).unwrap_err(),
            PathError::EscapesBase
        );
        assert_eq!(
            confine(base, Path::new("a/../b.txt")).unwrap(),
            PathBuf::from("/srv/forge/b.txt")
        );
    }

    #[test]
    fn rejects_symlink_components() {
        let base = scratch();
        std::fs::create_dir_all(base.join("real")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        let err = confine(&base, Path::new("link/evil.txt")).unwrap_err();
        assert_eq!(err, PathError::SymlinkComponent(base.join("link")));
        // The real directory itself resolves fine.
        assert_eq!(
            confine(&base, Path::new("real/ok.txt")).unwrap(),
            base.join("real/ok.txt")
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
