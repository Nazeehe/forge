//! Writer document: confined open, atomic save, char-offset edits.
//!
//! A missing file opens as an empty document with `dirty == false` (nothing
//! to lose yet); the first edit marks it dirty. Saving re-checks the on-disk
//! hash first and refuses on any external change, including deletion.

use std::ops::Range;
use std::path::{Path, PathBuf};

use super::{byte_range_of, hash_bytes, WriterError, MAX_DOC_BYTES};
use crate::infra::{fs_atomic, paths};

/// A Markdown document open in the Writer view.
#[derive(Clone, Debug)]
pub struct Document {
    /// Path as given, relative to the session cwd.
    pub path_rel: String,
    /// Confined absolute path.
    pub abs_path: PathBuf,
    /// Full text.
    pub text: String,
    /// Bumps on every buffer change (human or accepted proposal).
    pub revision: u64,
    /// Hash of the bytes at load/last save.
    pub disk_hash: u64,
    /// True once the buffer differs from what was loaded or saved.
    pub dirty: bool,
}

impl Document {
    /// Open `rel` under the session `cwd` through the path jail.
    ///
    /// The size is checked before any read (metadata first, then a capped
    /// read as a backstop), and anything that is not a regular file —
    /// directory, FIFO, socket, symlink — is refused before opening, so a
    /// special file can never block the TUI thread in a read.
    pub fn open(cwd: &Path, rel: &str) -> Result<Self, WriterError> {
        if !is_markdown(rel) {
            return Err(WriterError::NotMarkdown(rel.to_string()));
        }
        let abs_path = paths::confine(cwd, Path::new(rel)).map_err(WriterError::Confined)?;
        let meta = match std::fs::symlink_metadata(&abs_path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Document {
                    path_rel: rel.to_string(),
                    abs_path,
                    text: String::new(),
                    revision: 0,
                    disk_hash: hash_bytes(&[]),
                    dirty: false,
                });
            }
            Err(e) => return Err(WriterError::Io(e.to_string())),
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() || !file_type.is_file() {
            return Err(WriterError::NotRegularFile(rel.to_string()));
        }
        if meta.len() > MAX_DOC_BYTES as u64 {
            return Err(WriterError::TooLarge(meta.len()));
        }
        // Backstop for growth between the stat and the read: never buffer
        // more than one byte past the cap.
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&abs_path)
            .map_err(|e| WriterError::Io(e.to_string()))?
            .take(MAX_DOC_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| WriterError::Io(e.to_string()))?;
        if bytes.len() as u64 > MAX_DOC_BYTES as u64 {
            let actual = std::fs::metadata(&abs_path)
                .map(|m| m.len())
                .unwrap_or(bytes.len() as u64);
            return Err(WriterError::TooLarge(actual));
        }
        let disk_hash = hash_bytes(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| WriterError::InvalidUtf8)?;
        Ok(Document {
            path_rel: rel.to_string(),
            abs_path,
            disk_hash,
            text,
            revision: 0,
            dirty: false,
        })
    }

    /// Save atomically; refuses when the disk changed underneath us.
    /// Existing files keep their mode; new files get 0644.
    pub fn save(&mut self) -> Result<(), WriterError> {
        let current = match std::fs::read(&self.abs_path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(WriterError::Io(e.to_string())),
        };
        match &current {
            Some(bytes) if hash_bytes(bytes) != self.disk_hash => {
                return Err(WriterError::ConflictOnSave);
            }
            // Gone from disk: a conflict unless we opened a missing file
            // ourselves (disk_hash of empty) and never saved it yet.
            None if self.disk_hash != hash_bytes(&[]) => {
                return Err(WriterError::ConflictOnSave);
            }
            _ => {}
        }
        let previous_mode = current
            .is_some()
            .then(|| std::fs::metadata(&self.abs_path).ok().map(|m| m.permissions()))
            .flatten();
        fs_atomic::write_atomic(&self.abs_path, self.text.as_bytes())
            .map_err(|e| WriterError::Io(e.to_string()))?;
        // Existing files keep their mode; new files get a deterministic
        // 0644 (user default) instead of whatever the ambient umask yields.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match previous_mode {
                Some(mode) => {
                    let _ = std::fs::set_permissions(&self.abs_path, mode);
                }
                None => {
                    let _ = std::fs::set_permissions(
                        &self.abs_path,
                        std::fs::Permissions::from_mode(0o644),
                    );
                }
            }
        }
        self.disk_hash = hash_bytes(self.text.as_bytes());
        self.dirty = false;
        Ok(())
    }

    /// Replace a char-offset range; bumps revision, marks dirty.
    pub fn apply_edit(&mut self, range: Range<usize>, replacement: &str) -> Result<u64, WriterError> {
        let bytes = byte_range_of(&self.text, range).ok_or(WriterError::RangeOutOfBounds)?;
        self.text.replace_range(bytes, replacement);
        self.revision += 1;
        self.dirty = true;
        Ok(self.revision)
    }
}

/// Accepted Markdown extensions (case-insensitive). Shared with the
/// Save-as path check so Save-as only offers files the opener accepts.
pub(crate) fn is_markdown(rel: &str) -> bool {
    let lower = rel.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".txt")
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "forge-writer-test-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn open_reads_existing_markdown() {
        let dir = scratch();
        std::fs::write(dir.join("notes.md"), "# hi\n").unwrap();
        let doc = Document::open(&dir, "notes.md").unwrap();
        assert_eq!(doc.text, "# hi\n");
        assert_eq!(doc.revision, 0);
        assert!(!doc.dirty);
        assert_eq!(doc.abs_path, dir.join("notes.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_accepts_markdown_extensions() {
        let dir = scratch();
        for name in ["a.md", "b.markdown", "c.txt", "d.MD"] {
            std::fs::write(dir.join(name), "x").unwrap();
            assert!(Document::open(&dir, name).is_ok(), "name: {name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_refuses_non_markdown() {
        let dir = scratch();
        for name in ["a.png", "a.rs", "noext", "a.md.bak"] {
            std::fs::write(dir.join(name), "x").unwrap();
            assert_eq!(
                Document::open(&dir, name).unwrap_err(),
                WriterError::NotMarkdown(name.to_string()),
                "name: {name}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_refuses_jail_breaks() {
        let dir = scratch();
        // Absolute and escaping paths are confined even with an ok extension.
        assert!(matches!(
            Document::open(&dir, "/etc/x.md").unwrap_err(),
            WriterError::Confined(_)
        ));
        assert!(matches!(
            Document::open(&dir, "../escape.md").unwrap_err(),
            WriterError::Confined(_)
        ));
        // The extension gate runs first: no Markdown suffix, no jail check.
        assert_eq!(
            Document::open(&dir, "/etc/passwd").unwrap_err(),
            WriterError::NotMarkdown("/etc/passwd".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_missing_file_is_empty_and_clean() {
        let dir = scratch();
        let doc = Document::open(&dir, "new.md").unwrap();
        assert_eq!(doc.text, "");
        assert!(!doc.dirty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_refuses_oversize_and_bad_utf8() {
        let dir = scratch();
        let big = dir.join("big.md");
        std::fs::write(&big, vec![b'x'; MAX_DOC_BYTES + 1]).unwrap();
        assert_eq!(
            Document::open(&dir, "big.md").unwrap_err(),
            WriterError::TooLarge((MAX_DOC_BYTES + 1) as u64)
        );
        std::fs::write(dir.join("bad.md"), [0xff, 0xfe]).unwrap();
        assert_eq!(
            Document::open(&dir, "bad.md").unwrap_err(),
            WriterError::InvalidUtf8
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_refuses_oversize_without_full_read() {
        let dir = scratch();
        // Sparse file: multi-MiB size with (almost) no disk backing, so a
        // full read would be the only expensive part — the refusal must
        // come from the size check, carrying the real size.
        let path = dir.join("sparse.md");
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)
            .unwrap();
        let size = MAX_DOC_BYTES as u64 + 1024;
        f.set_len(size).unwrap();
        assert_eq!(
            Document::open(&dir, "sparse.md").unwrap_err(),
            WriterError::TooLarge(size)
        );
        // Unreadable file: the size check must precede any read attempt,
        // so refusal is TooLarge rather than an I/O error.
        let locked = dir.join("locked.md");
        let g = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(&locked)
            .unwrap();
        g.set_len(size).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        assert_eq!(
            Document::open(&dir, "locked.md").unwrap_err(),
            WriterError::TooLarge(size)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_refuses_fifo_promptly() {
        let dir = scratch();
        let path = dir.join("pipe.md");
        let cpath = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: mkfifo on a fresh scratch path; return checked below.
        let ret = unsafe { libc::mkfifo(cpath.as_ptr(), 0o644) };
        assert_eq!(ret, 0);
        // A blocking read would hang the TUI thread forever, so run the
        // open off-thread: a hang becomes a test failure, not a freeze.
        let (tx, rx) = std::sync::mpsc::channel();
        let dir2 = dir.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Document::open(&dir2, "pipe.md").unwrap_err());
        });
        let err = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("open blocked on a FIFO");
        assert_eq!(err, WriterError::NotRegularFile("pipe.md".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_refuses_directory() {
        let dir = scratch();
        std::fs::create_dir_all(dir.join("folder.md")).unwrap();
        assert_eq!(
            Document::open(&dir, "folder.md").unwrap_err(),
            WriterError::NotRegularFile("folder.md".to_string())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_save_of_new_doc_is_not_0600() {
        let dir = scratch();
        let mut doc = Document::open(&dir, "fresh.md").unwrap();
        doc.apply_edit(0..0, "hello").unwrap();
        doc.save().unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("fresh.md")).unwrap(), "hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("fresh.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o644, "new doc mode was {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_round_trip_and_revision() {
        let dir = scratch();
        let mut doc = Document::open(&dir, "draft.md").unwrap();
        let rev = doc.apply_edit(0..0, "hello").unwrap();
        assert_eq!(rev, 1);
        assert!(doc.dirty);
        doc.save().unwrap();
        assert!(!doc.dirty);
        assert_eq!(std::fs::read_to_string(dir.join("draft.md")).unwrap(), "hello");
        // Reopen sees the saved text with a fresh revision.
        let doc2 = Document::open(&dir, "draft.md").unwrap();
        assert_eq!(doc2.text, "hello");
        assert_eq!(doc2.revision, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_on_external_change() {
        let dir = scratch();
        std::fs::write(dir.join("shared.md"), "mine").unwrap();
        let mut doc = Document::open(&dir, "shared.md").unwrap();
        doc.apply_edit(0..4, "edited").unwrap();
        // Someone else writes first.
        std::fs::write(dir.join("shared.md"), "theirs").unwrap();
        assert_eq!(doc.save().unwrap_err(), WriterError::ConflictOnSave);
        // Our text did not clobber theirs.
        assert_eq!(std::fs::read_to_string(dir.join("shared.md")).unwrap(), "theirs");
        assert!(doc.dirty, "still dirty so the human can resolve it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_when_deleted_underneath() {
        let dir = scratch();
        std::fs::write(dir.join("gone.md"), "x").unwrap();
        let mut doc = Document::open(&dir, "gone.md").unwrap();
        std::fs::remove_file(dir.join("gone.md")).unwrap();
        assert_eq!(doc.save().unwrap_err(), WriterError::ConflictOnSave);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_keeps_existing_file_mode() {
        let dir = scratch();
        let path = dir.join("mode.md");
        std::fs::write(&path, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        let mut doc = Document::open(&dir, "mode.md").unwrap();
        doc.apply_edit(0..1, "yz").unwrap();
        doc.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "yz");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "mode was {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_edit_replaces_exact_char_range() {
        let dir = scratch();
        std::fs::write(dir.join("e.md"), "héllo world").unwrap();
        let mut doc = Document::open(&dir, "e.md").unwrap();
        // Replace "world" (chars 6..11) with multibyte text.
        let rev = doc.apply_edit(6..11, "Forge✓").unwrap();
        assert_eq!(rev, 1);
        assert_eq!(doc.text, "héllo Forge✓");
        assert_eq!(doc.apply_edit(0..0, "!").unwrap(), 2);
        assert_eq!(doc.text, "!héllo Forge✓");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_edit_rejects_bad_ranges() {
        let dir = scratch();
        let mut doc = Document::open(&dir, "r.md").unwrap();
        assert_eq!(
            doc.apply_edit(0..1, "x").unwrap_err(),
            WriterError::RangeOutOfBounds
        );
        doc.apply_edit(0..0, "abc").unwrap();
        assert_eq!(
            doc.apply_edit(2..5, "x").unwrap_err(),
            WriterError::RangeOutOfBounds
        );
        assert_eq!(
            doc.apply_edit(2..1, "x").unwrap_err(),
            WriterError::RangeOutOfBounds
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
