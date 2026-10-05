//! Recent documents: files opened in Writer this run first, then a
//! bounded scan of Markdown under the session cwd.
//!
//! Bounds (operator-approved E2 design): depth ≤ 2, at most 2,000
//! directory entries visited, hidden dirs, `.git`, `target`,
//! `node_modules` and `vendor` skipped, symlinks never followed,
//! `*.md` / `*.markdown` only, sorted by mtime descending, at most
//! 8 entries. The scan runs when the tab opens or the cwd changes;
//! the session caches the result (never per frame).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Most rows the recent list ever shows.
pub const MAX_RECENT: usize = 8;

/// Most directory entries one scan may visit (AGENTS §3: bounded).
const MAX_SCAN_ENTRIES: usize = 2000;

/// Deepest directory below the cwd a scan descends into.
const MAX_SCAN_DEPTH: u32 = 2;

/// Directories a scan never descends into (hidden dirs are
/// refused by the dot rule below).
const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", "vendor"];

/// One recent row: the path relative to the cwd plus its mtime.
/// Extensions match lower-case only (`*.md`, `*.markdown`).
pub struct RecentEntry {
    pub rel: String,
    pub mtime: SystemTime,
    pub opened_this_run: bool,
}

/// Scan `cwd` for recent Markdown: `opened` (absolute paths,
/// most-recent-first) heads the list, then the bounded walk fills
/// up to [`MAX_RECENT`] by mtime descending.
pub fn scan(cwd: &Path, opened: &[PathBuf]) -> Vec<RecentEntry> {
    scan_with_budget(cwd, opened, MAX_SCAN_ENTRIES)
}

/// [`scan`] with an injectable visit budget (tests pin the bound).
fn scan_with_budget(cwd: &Path, opened: &[PathBuf], budget: usize) -> Vec<RecentEntry> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    // Files opened this run first, most recent first, still on disk.
    for path in opened.iter().take(MAX_RECENT) {
        let Ok(rel) = path.strip_prefix(cwd) else {
            continue;
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        // Vanished files drop out instead of dating to 1970.
        let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
            continue;
        };
        out.push(RecentEntry {
            rel: rel.to_string_lossy().into_owned(),
            mtime,
            opened_this_run: true,
        });
    }
    // Then the bounded walk, skipping anything already listed.
    let mut walked = walk(cwd, budget);
    walked.sort_by(|a, b| b.1.cmp(&a.1));
    for (rel, mtime) in walked {
        if out.len() >= MAX_RECENT {
            break;
        }
        let abs = cwd.join(&rel);
        if !seen.insert(abs) {
            continue;
        }
        out.push(RecentEntry {
            rel,
            mtime,
            opened_this_run: false,
        });
    }
    out
}

/// `(relative path, mtime)` of Markdown files under `cwd`, visiting
/// at most `budget` directory entries. Depth, skips and symlink
/// rules per the module docs; errors read as empty (a scan never
/// fails the tab).
fn walk(cwd: &Path, budget: usize) -> Vec<(String, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![(cwd.to_path_buf(), 0u32)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if visited >= budget {
                return out;
            }
            visited += 1;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // symlink_metadata: never follow symlinks, files or dirs.
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if depth >= MAX_SCAN_DEPTH {
                    continue;
                }
                let n: &str = &name;
                if n.starts_with('.') || SKIP_DIRS.contains(&n) {
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else if meta.is_file()
                && (name.ends_with(".md") || name.ends_with(".markdown"))
            {
                let Ok(rel) = entry.path().strip_prefix(cwd).map(|p| p.to_path_buf()) else {
                    continue;
                };
                out.push((
                    rel.to_string_lossy().into_owned(),
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                ));
            }
        }
    }
    out
}

/// "12 min ago", "yesterday", "3 days ago": relative age of `mtime`
/// against `now`. Future mtimes (clock skew) read as just now.
pub fn relative_age(mtime: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(mtime).unwrap_or_default().as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{mins} min ago")
    } else if secs < 24 * 3600 {
        let hours = secs / 3600;
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{hours} hours ago")
        }
    } else if secs < 2 * 24 * 3600 {
        "yesterday".to_string()
    } else {
        format!("{} days ago", secs / (24 * 3600))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "forge-recent-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &std::path::Path, rel: &str, age_secs: u64) -> std::path::PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x").unwrap();
        let mtime =
            std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(mtime).unwrap();
        path
    }

    #[test]
    fn finds_nested_markdown_to_depth_two() {
        let dir = scratch();
        touch(&dir, "a.md", 300);
        touch(&dir, "sub/b.markdown", 200);
        touch(&dir, "sub/deep/c.md", 100);
        touch(&dir, "sub/deep/deeper/d.md", 50);
        touch(&dir, "note.txt", 10);
        touch(&dir, "UPPER.MD", 10);
        let found: Vec<String> = scan(&dir, &[]).iter().map(|e| e.rel.clone()).collect();
        assert!(found.contains(&"a.md".to_string()), "{found:?}");
        assert!(found.contains(&"sub/b.markdown".to_string()), "{found:?}");
        assert!(found.contains(&"sub/deep/c.md".to_string()), "{found:?}");
        assert!(!found.iter().any(|r| r.contains("deeper")), "{found:?}");
        assert!(!found.iter().any(|r| r.ends_with(".txt")), "{found:?}");
        assert!(!found.iter().any(|r| r.ends_with(".MD")), "{found:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_hidden_and_vendor_dirs() {
        let dir = scratch();
        touch(&dir, "keep.md", 100);
        for skipped in [".hidden/x.md", ".git/y.md", "target/z.md", "node_modules/w.md", "vendor/v.md"] {
            touch(&dir, skipped, 50);
        }
        let found: Vec<String> = scan(&dir, &[]).iter().map(|e| e.rel.clone()).collect();
        assert_eq!(found, vec!["keep.md".to_string()], "{found:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)]
    fn never_follows_symlinks() {
        let dir = scratch();
        touch(&dir, "real/inside.md", 100);
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.join("real/inside.md"), dir.join("file.md")).unwrap();
        let found: Vec<String> = scan(&dir, &[]).iter().map(|e| e.rel.clone()).collect();
        assert_eq!(found, vec!["real/inside.md".to_string()], "{found:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn opened_this_run_come_first() {
        let dir = scratch();
        touch(&dir, "newer.md", 60);
        let older = touch(&dir, "older.md", 600);
        let found = scan(&dir, &[older.clone()]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].rel, "older.md");
        assert!(found[0].opened_this_run);
        assert_eq!(found[1].rel, "newer.md");
        assert!(!found[1].opened_this_run);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn results_cap_at_eight_by_mtime() {
        let dir = scratch();
        for n in 0..12 {
            touch(&dir, &format!("f{n:02}.md"), (n as u64) * 60);
        }
        let found = scan(&dir, &[]);
        assert_eq!(found.len(), 8);
        assert_eq!(found[0].rel, "f00.md", "newest first");
        assert_eq!(found[7].rel, "f07.md");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn visit_budget_bounds_the_walk() {
        let dir = scratch();
        for n in 0..6 {
            touch(&dir, &format!("g{n}.md"), 100);
        }
        // A budget of 3 visits sees at most 3 files, however many exist.
        assert!(scan_with_budget(&dir, &[], 3).len() <= 3);
        // The full budget sees them all.
        assert_eq!(scan_with_budget(&dir, &[], 10_000).len(), 6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_tree_scans_empty() {
        let dir = scratch();
        assert!(scan(&dir, &[]).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn relative_ages_read_naturally() {
        let now = std::time::SystemTime::now();
        let ago = |s: u64| now - std::time::Duration::from_secs(s);
        assert_eq!(relative_age(now, now), "just now");
        assert_eq!(relative_age(ago(30), now), "just now");
        assert_eq!(relative_age(ago(12 * 60), now), "12 min ago");
        assert_eq!(relative_age(ago(60 * 60), now), "1 hour ago");
        assert_eq!(relative_age(ago(5 * 3600), now), "5 hours ago");
        assert_eq!(relative_age(ago(30 * 3600), now), "yesterday");
        assert_eq!(relative_age(ago(3 * 86400), now), "3 days ago");
        // Future mtimes (clock skew) never print a negative age.
        assert_eq!(
            relative_age(now + std::time::Duration::from_secs(60), now),
            "just now"
        );
    }
}
